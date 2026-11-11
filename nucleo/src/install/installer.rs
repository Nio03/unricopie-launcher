//! Instalar un juego de principio a fin: planear (qué versión y qué archivo, sin bajar nada),
//! bajar comprobando el SHA-256 de GitHub, descomprimir en una carpeta temporal, comprobar que
//! el ejecutable está, dejar el recibo y solo entonces mover todo a su sitio. Si algo falla,
//! no queda nada a medias en la biblioteca.
//!
//! Formatos: zip, tar.gz, tar.xz, 7z, ejecutables sueltos y AppImage. Los instaladores (msi,
//! exe de instalación) y los dmg se rechazan al planear, antes de bajar nada. Para actualizar
//! un juego ya instalado está `update`.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::data::{DataPath, Places};
use super::receipt::{self, PackageFile, Receipt, Stamp};
use super::recipe::{AssetRule, PackageKind, Platform, Recipe, Step};
use super::release::{self, Channel, GhRelease};
use crate::archive::{self, Limits};
use crate::net::{self, Downloaded, Expected};
use crate::path::RelPath;
use crate::{Error, Result};

/// Dónde viven los juegos, las descargas y las versiones anteriores de cada juego.
#[derive(Debug, Clone)]
pub struct Library {
    pub games: PathBuf,
    pub downloads: PathBuf,
    /// `<previous>/<carpeta del juego>/<id>`: la instalación tal como estaba antes de cada
    /// actualización, para volver atrás.
    pub previous: PathBuf,
    /// `<copies>/<juego>`: la copia del jugador, comprobada y guardada una vez (`copy::import`).
    /// Fuera de la carpeta del juego, para que las actualizaciones no la dupliquen.
    pub copies: PathBuf,
}

impl Library {
    pub fn new(root: impl AsRef<Path>) -> Library {
        let root = root.as_ref();
        Library {
            games: root.join("games"),
            downloads: root.join("downloads"),
            previous: root.join("previous"),
            copies: root.join("copies"),
        }
    }

    /// `<copies>/<id de la ficha>`, más el juego si es una variante, como `game_dir`.
    pub fn copy_dir(&self, recipe: &Recipe, variant: Option<&str>) -> PathBuf {
        let game = self.game_dir(recipe, variant);
        self.copies.join(game.file_name().unwrap_or_default())
    }

    /// `<games>/<id de la ficha>`, más el juego si es una variante. Los dos ya vienen
    /// validados (minúsculas, dígitos y guiones).
    pub fn game_dir(&self, recipe: &Recipe, variant: Option<&str>) -> PathBuf {
        match variant {
            Some(g) => self.games.join(format!("{}--{g}", recipe.slug())),
            None => self.games.join(recipe.slug()),
        }
    }
}

/// Lo que se va a hacer, antes de bajar nada. Es lo que muestra la página de descargas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub entry: String,
    pub game: Option<String>,
    pub repo: String,
    pub platform: Platform,
    pub channel: Channel,
    pub version: String,
    pub asset: String,
    pub url: String,
    pub size: Option<u64>,
    /// El SHA-256 que publica GitHub, si lo publica.
    pub sha256: Option<String>,
    pub kind: PackageKind,
    pub run: Option<String>,
    pub after_download: Vec<Step>,
    pub after_extract: Vec<Step>,
    pub dest: PathBuf,
    /// Las carpetas de fuera de `dest` donde guarda partidas y ajustes (`data-dir`, y lo que
    /// deja fuera aunque sea portable), ya buscadas en este equipo. Al actualizar se guarda una
    /// copia con la versión anterior.
    pub data_dirs: Vec<PathBuf>,
}

/// Planea con las releases ya leídas (para probar sin red y para no pedirlas dos veces).
#[allow(clippy::too_many_arguments)]
pub fn plan_from(
    recipe: &Recipe,
    variant: Option<&str>,
    host: Platform,
    channel: Channel,
    stable_tag: Option<&str>,
    releases: &[GhRelease],
    library: &Library,
) -> Result<Plan> {
    let res = recipe.resolve(variant, host)?;
    let rel = release::pick(releases, channel, stable_tag)
        .ok_or_else(|| Error::NoRelease { repo: recipe.repo.clone(), channel })?;
    let (asset, url, size, sha256) = match &res.asset {
        AssetRule::Pattern(p) => {
            let a = release::find_asset(rel, p, &res.exclude)?;
            (a.name.clone(), a.browser_download_url.clone(), Some(a.size), a.sha256().map(str::to_string))
        }
        AssetRule::Url(template) => {
            let tag = rel.tag_name.as_str();
            let version = recipe.release.tag_prefix.as_deref().and_then(|p| tag.strip_prefix(p)).unwrap_or(tag);
            let url = template.replace("{version}", version);
            let name = url.rsplit('/').next().unwrap_or_default().to_string();
            (name, url, None, None)
        }
    };
    if !url.starts_with("https://") || asset.is_empty() {
        return Err(Error::BadRecipe(format!("dirección de descarga no válida: {url}")));
    }
    supported(res.kind, &asset)?;
    let dest = library.game_dir(recipe, variant);
    let data_dirs = Places::current().outside_dirs(&data_paths(&res), &dest, &[&library.games, &library.previous, &library.downloads]);
    let run = match res.run.as_deref() {
        Some("{asset}") => Some(asset.clone()),
        Some(r) => Some(RelPath::new(r)?.as_str().to_string()),
        None => None,
    };
    Ok(Plan {
        entry: recipe.entry.clone(),
        game: variant.map(str::to_string),
        repo: recipe.repo.clone(),
        platform: res.platform,
        channel,
        version: rel.tag_name.clone(),
        asset,
        url,
        size,
        sha256,
        kind: res.kind,
        run,
        after_download: res.after_download,
        after_extract: res.after_extract,
        data_dirs,
        dest,
    })
}

/// Todo lo que el juego puede usar fuera de su carpeta: lo de `data-dir` y, si se puede hacer
/// portable, lo que deja fuera aun así. Se guardan las dos cosas, esté portable o no: si se
/// cambió de modo, en las dos hay partidas.
fn data_paths(res: &super::recipe::Resolved<'_>) -> Vec<DataPath> {
    let mut all = res.data_dirs.clone();
    if let Some(p) = res.portable {
        all.extend(p.data_dirs());
    }
    all
}

/// Planea leyendo las releases de GitHub.
pub fn plan(
    agent: &ureq::Agent,
    recipe: &Recipe,
    variant: Option<&str>,
    host: Platform,
    channel: Channel,
    stable_tag: Option<&str>,
    library: &Library,
) -> Result<Plan> {
    let releases = release::list(agent, &recipe.repo)?;
    plan_from(recipe, variant, host, channel, stable_tag, &releases, library)
}

fn supported(kind: PackageKind, asset: &str) -> Result<()> {
    match kind {
        PackageKind::Archive if archive::Kind::from_name(asset).is_some() => Ok(()),
        PackageKind::Portable | PackageKind::Appimage => Ok(()),
        // Se planea en cualquier equipo; solo se abre en macOS (`dmg`).
        PackageKind::Dmg => Ok(()),
        _ => Err(Error::UnsupportedPackage { asset: asset.to_string(), kind: format!("{kind:?}") }),
    }
}

/// Baja e instala. `progress` recibe los bytes bajados y el total.
pub fn install(agent: &ureq::Agent, library: &Library, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Receipt> {
    if plan.dest.exists() {
        return Err(Error::AlreadyInstalled(plan.dest.clone()));
    }
    let (file, got) = fetch(agent, library, plan, progress)?;
    let result = install_downloaded(library, plan, &file, &got);
    let _ = fs::remove_file(&file);
    result
}

/// Baja el archivo del plan a la carpeta de descargas, comprobando el tamaño y el SHA-256.
pub(crate) fn fetch(agent: &ureq::Agent, library: &Library, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(PathBuf, Downloaded)> {
    let file = library.downloads.join(RelPath::new(&plan.asset)?.file_name());
    let expected = plan.sha256.clone().map(Expected::Sha256);
    // GitHub dice el tamaño exacto: más que eso es que algo va mal.
    let max = plan.size.unwrap_or(16 << 30);
    let got = net::download(agent, &plan.url, &file, max, expected.as_ref(), progress)?;
    Ok((file, got))
}

/// La parte de la instalación que no usa la red: lo bajado ya está en `file`.
pub fn install_downloaded(library: &Library, plan: &Plan, file: &Path, got: &Downloaded) -> Result<Receipt> {
    if plan.dest.exists() {
        return Err(Error::AlreadyInstalled(plan.dest.clone()));
    }
    let p = prepare(library, plan, file, got)?;
    p.receipt.write(&p.root)?;
    fs::rename(&p.root, &plan.dest).map_err(|e| Error::io(&plan.dest, e))?;
    Ok(p.receipt)
}

/// Una carpeta temporal dentro de la biblioteca que se borra al soltarla, salga bien o mal.
/// Al estar en el mismo disco que los juegos, moverla a su sitio es un `rename`.
pub(crate) struct TmpDir(PathBuf);

impl TmpDir {
    pub(crate) fn new(parent: &Path) -> Result<TmpDir> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = parent.join(format!(".tmp-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        Ok(TmpDir(dir))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Lo bajado, ya descomprimido y comprobado en una carpeta temporal, con su recibo (sin
/// escribir todavía). Falta solo moverlo a su sitio.
pub(crate) struct Prepared {
    pub(crate) root: PathBuf,
    pub(crate) receipt: Receipt,
    _tmp: TmpDir,
}

/// Comprueba la suma, descomprime en una carpeta temporal, busca el ejecutable y arma el
/// recibo con la lista de archivos del paquete. No toca nada fuera de la carpeta temporal.
pub(crate) fn prepare(library: &Library, plan: &Plan, file: &Path, got: &Downloaded) -> Result<Prepared> {
    if let Some(want) = &plan.sha256 {
        if !want.eq_ignore_ascii_case(&got.sha256) {
            return Err(Error::ChecksumMismatch { algo: "SHA-256", expected: want.clone(), got: got.sha256.clone() });
        }
    }
    let tmp = TmpDir::new(&library.games)?;
    let content = tmp.path().join("contenido");
    fs::create_dir_all(&content).map_err(|e| Error::io(&content, e))?;
    let (root, files) = match plan.kind {
        PackageKind::Archive => {
            let kind = archive::Kind::from_name(&plan.asset).ok_or_else(|| Error::UnsupportedPackage { asset: plan.asset.clone(), kind: "Archive".into() })?;
            let extracted = archive::extract(file, kind, &content, Limits::default())?;
            let root = content_root(&content, plan.run.as_deref())?;
            let files = package_files(&extracted, &content, &root)?;
            (root, files)
        }
        PackageKind::Portable | PackageKind::Appimage => {
            let name = RelPath::new(&plan.asset)?;
            let dest = name.under(&content);
            fs::copy(file, &dest).map_err(|e| Error::io(&dest, e))?;
            if plan.after_download.contains(&Step::ChmodX) || plan.kind == PackageKind::Appimage {
                archive::set_executable(&dest)?;
            }
            let stamp = Stamp::of(&dest)?;
            (content.clone(), vec![PackageFile { path: name, size: stamp.size, xxh3: stamp.xxh3 }])
        }
        PackageKind::Dmg => {
            super::dmg::extract(file, &tmp.path().join("montaje"), &content)?;
            let root = content_root(&content, plan.run.as_deref())?;
            let mut files = Vec::new();
            files_on_disk(&root, "", &mut files)?;
            (root, files)
        }
        other => return Err(Error::UnsupportedPackage { asset: plan.asset.clone(), kind: format!("{other:?}") }),
    };
    let run_stamp = match &plan.run {
        Some(run) => {
            let path = RelPath::new(run)?.under(&root);
            if !path.exists() {
                return Err(Error::RunNotFound { run: run.clone() });
            }
            if plan.after_extract.contains(&Step::ChmodX) && path.is_file() {
                archive::set_executable(&path)?;
            }
            // Una app de macOS es una carpeta: no hay un archivo que vigilar.
            if path.is_file() { Some(Stamp::of(&path)?) } else { None }
        }
        None => None,
    };
    if root.join(receipt::FILE_NAME).exists() {
        return Err(Error::PackageHasReceipt(plan.asset.clone()));
    }
    let receipt = Receipt {
        format: Receipt::format(),
        entry: plan.entry.clone(),
        game: plan.game.clone(),
        repo: plan.repo.clone(),
        version: plan.version.clone(),
        channel: plan.channel,
        platform: plan.platform,
        asset: plan.asset.clone(),
        url: plan.url.clone(),
        size: got.size,
        sha256: got.sha256.clone(),
        sha256_verified: plan.sha256.is_some(),
        run: plan.run.clone(),
        run_stamp,
        installed_at: receipt::now(),
        files,
        portable: None,
        settings: Vec::new(),
        self_update_allowed: false,
    };
    Ok(Prepared { root, receipt, _tmp: tmp })
}

/// La lista de archivos del paquete con las rutas relativas a la raíz del juego, que puede
/// ser la única carpeta del zip (ver `content_root`).
fn package_files(extracted: &[archive::Extracted], content: &Path, root: &Path) -> Result<Vec<PackageFile>> {
    let prefix = match root.strip_prefix(content) {
        Ok(p) if p.as_os_str().is_empty() => None,
        Ok(p) => Some(RelPath::new(&p.to_string_lossy())?),
        Err(_) => return Err(Error::PackageLayout(content.display().to_string())),
    };
    let mut files = Vec::with_capacity(extracted.len());
    for f in extracted {
        let path = match &prefix {
            None => f.path.clone(),
            Some(pre) => match strip_dir(&f.path, pre) {
                Some(rest) => rest,
                // Con una sola carpeta arriba no puede haber nada fuera de ella.
                None => return Err(Error::PackageLayout(f.path.to_string())),
            },
        };
        files.push(PackageFile { path, size: f.size, xxh3: f.hash });
    }
    Ok(files)
}

/// La lista de archivos del paquete sacada del disco, para lo que no trae una propia (un dmg).
/// Los enlaces no entran: los de dentro de una `.app` no son del jugador y la versión nueva
/// trae los suyos.
fn files_on_disk(dir: &Path, rel: &str, out: &mut Vec<PackageFile>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let kind = entry.file_type().map_err(|e| Error::io(entry.path(), e))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            files_on_disk(&entry.path(), &child, out)?;
        } else {
            let stamp = Stamp::of(&entry.path())?;
            out.push(PackageFile { path: RelPath::new(&child)?, size: stamp.size, xxh3: stamp.xxh3 });
        }
    }
    Ok(())
}

/// `a/b/c` sin la carpeta `a` es `b/c`. La comparación sigue al disco: en Windows y macOS no
/// distingue mayúsculas (dos entradas `Juego/x` y `juego/y` acaban en la misma carpeta).
fn strip_dir(path: &RelPath, dir: &RelPath) -> Option<RelPath> {
    let (first, rest) = path.as_str().split_once('/')?;
    if crate::install::update::same_key(first, dir.as_str()) { RelPath::new(rest).ok() } else { None }
}

/// Muchos zip meten todo en una carpeta (`SoH-Win64/soh.exe`). Si el ejecutable no está arriba
/// pero sí dentro de la única carpeta que hay, esa carpeta es la raíz del juego.
fn content_root(dir: &Path, run: Option<&str>) -> Result<PathBuf> {
    let Some(run) = run else { return Ok(dir.to_path_buf()) };
    let run = RelPath::new(run)?;
    if run.under(dir).exists() {
        return Ok(dir.to_path_buf());
    }
    let entries: Vec<_> = fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.flatten().collect();
    if let [only] = entries.as_slice() {
        let inner = only.path();
        if inner.is_dir() && run.under(&inner).exists() {
            return Ok(inner);
        }
    }
    Ok(dir.to_path_buf())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::install::receipt::RunState;
    use crate::install::recipe::tests::ZELDA;
    use crate::install::release::tests::RELEASES;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zelda() -> (Recipe, Vec<GhRelease>) {
        (Recipe::from_json(ZELDA).unwrap(), serde_json::from_str(RELEASES).unwrap())
    }

    #[test]
    fn planea_sin_bajar_nada() {
        let (r, rs) = zelda();
        let lib = Library::new("/biblioteca");
        let p = plan_from(&r, None, Platform::WindowsX64, Channel::Untested, None, &rs, &lib).unwrap();
        assert_eq!(p.version, "v1.2.2");
        assert_eq!(p.asset, "Zelda64Recompiled-v1.2.2-Windows.zip");
        assert_eq!(p.size, Some(28940501));
        assert_eq!(p.sha256.as_deref(), Some("d02b95f1cf5e4d44d6587fa6136c6db521f61aa41ba8700c05f91bea8b0a26d0"));
        assert_eq!(p.run.as_deref(), Some("Zelda64Recompiled.exe"));
        assert_eq!(p.dest, Path::new("/biblioteca").join("games").join("zelda-majoras-mask"));
        // La nightly es la beta, que no trae suma.
        let n = plan_from(&r, None, Platform::WindowsX64, Channel::Nightly, None, &rs, &lib).unwrap();
        assert_eq!(n.version, "v1.2.3-beta");
        assert_eq!(n.sha256, None);
        // Sin versión estable probada no hay canal estable.
        assert!(matches!(
            plan_from(&r, None, Platform::WindowsX64, Channel::Stable, None, &rs, &lib),
            Err(Error::NoRelease { .. })
        ));
    }

    #[test]
    fn rechaza_al_planear_lo_que_no_sabe_instalar() {
        let json = ZELDA.replace("Zelda64Recompiled-*-Windows.zip", "Zelda64Recompiled-*-Windows.rar");
        let r = Recipe::from_json(&json).unwrap();
        let mut rs: Vec<GhRelease> = serde_json::from_str(RELEASES).unwrap();
        rs[2].assets[1].name = "Zelda64Recompiled-v1.2.2-Windows.rar".into();
        let err = plan_from(&r, None, Platform::WindowsX64, Channel::Untested, None, &rs, &Library::new("/b")).unwrap_err();
        assert!(matches!(err, Error::UnsupportedPackage { .. }), "{err}");
    }

    pub(crate) fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    pub(crate) fn fake_plan(lib: &Library, run: Option<&str>) -> Plan {
        let (r, rs) = zelda();
        let mut p = plan_from(&r, None, Platform::WindowsX64, Channel::Untested, None, &rs, lib).unwrap();
        p.sha256 = None; // el zip de prueba no es el de verdad
        p.run = run.map(str::to_string);
        p
    }

    #[test]
    fn instala_un_zip_con_carpeta_dentro() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("Zelda64Recompiled/Zelda64Recompiled.exe", b"exe"), ("Zelda64Recompiled/assets/a.bin", b"a")]);
        let got = Downloaded { size: 3, sha256: "abc".into() };
        let plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        let r = install_downloaded(&lib, &plan, &z, &got).unwrap();
        // La carpeta de dentro pasa a ser la raíz del juego.
        assert!(plan.dest.join("Zelda64Recompiled.exe").is_file());
        assert!(plan.dest.join("assets").join("a.bin").is_file());
        assert!(!r.sha256_verified);
        // La lista de archivos va sin la carpeta de dentro, y sin el recibo.
        let files: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["Zelda64Recompiled.exe", "assets/a.bin"]);
        assert_eq!(r.files[0].size, 3);
        let back = Receipt::read(&plan.dest).unwrap().unwrap();
        assert_eq!(back, r);
        assert_eq!(back.check_run(&plan.dest).unwrap(), RunState::Intact);
        // No queda nada temporal y no se instala dos veces.
        let left: Vec<_> = fs::read_dir(&lib.games).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("zelda-majoras-mask")]);
        assert!(matches!(install_downloaded(&lib, &plan, &z, &got), Err(Error::AlreadyInstalled(_))));
    }

    #[test]
    fn no_deja_nada_si_falta_el_ejecutable() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("otro.exe", b"x")]);
        let plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        let err = install_downloaded(&lib, &plan, &z, &Downloaded { size: 1, sha256: "x".into() }).unwrap_err();
        assert!(matches!(err, Error::RunNotFound { .. }), "{err}");
        assert!(!plan.dest.exists());
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 0);
    }

    #[test]
    fn rechaza_una_descarga_que_no_coincide() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("Zelda64Recompiled.exe", b"x")]);
        let mut plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        plan.sha256 = Some("d02b95f1cf5e4d44d6587fa6136c6db521f61aa41ba8700c05f91bea8b0a26d0".into());
        let err = install_downloaded(&lib, &plan, &z, &Downloaded { size: 1, sha256: "otra".into() }).unwrap_err();
        assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");
        assert!(!plan.dest.exists());
    }

    #[test]
    fn lista_los_archivos_del_disco() {
        let t = tempfile::tempdir().unwrap();
        let app = t.path().join("OpenMW.app").join("Contents");
        fs::create_dir_all(app.join("MacOS")).unwrap();
        fs::write(app.join("MacOS").join("openmw"), b"mach-o").unwrap();
        fs::write(app.join("Info.plist"), b"<plist/>").unwrap();
        let mut files = Vec::new();
        files_on_disk(t.path(), "", &mut files).unwrap();
        let mut names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        names.sort();
        assert_eq!(names, ["OpenMW.app/Contents/Info.plist", "OpenMW.app/Contents/MacOS/openmw"]);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn un_dmg_fuera_de_macos_no_deja_nada() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let f = t.path().join("OpenMW-0.51.0-macOS-arm64.dmg");
        fs::write(&f, b"koly").unwrap();
        let mut plan = fake_plan(&lib, Some("OpenMW.app"));
        plan.kind = PackageKind::Dmg;
        plan.asset = "OpenMW-0.51.0-macOS-arm64.dmg".into();
        let err = install_downloaded(&lib, &plan, &f, &Downloaded { size: 4, sha256: "x".into() }).unwrap_err();
        assert!(matches!(err, Error::DmgOnlyMac(_)), "{err}");
        assert!(!plan.dest.exists());
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 0);
    }

    #[test]
    fn instala_un_ejecutable_suelto() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let f = t.path().join("SoH-Dewey-Bravo.appimage");
        fs::write(&f, b"\x7fELF").unwrap();
        let mut plan = fake_plan(&lib, None);
        plan.kind = PackageKind::Appimage;
        plan.asset = "SoH-Dewey-Bravo.appimage".into();
        plan.run = Some(plan.asset.clone());
        let r = install_downloaded(&lib, &plan, &f, &Downloaded { size: 4, sha256: "x".into() }).unwrap();
        assert!(plan.dest.join("SoH-Dewey-Bravo.appimage").is_file());
        assert!(r.run_stamp.is_some());
    }

    /// Planea cada receta contra sus releases de hoy: así se ve qué patrones ya no encajan.
    /// Una consulta a la API por receta (el límite sin clave es de 60 por hora).
    #[test]
    #[ignore = "usa la red y necesita UNRICOPIE_RECETAS=<json con todas las recetas>"]
    fn planea_todas_las_recetas_de_verdad() {
        let path = std::env::var("UNRICOPIE_RECETAS").expect("UNRICOPIE_RECETAS");
        let all: Vec<Recipe> = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let agent = net::agent();
        let lib = Library::new("/biblioteca");
        let (mut ok, mut fallos) = (0, Vec::new());
        for r in &all {
            let releases = match release::list(&agent, &r.repo) {
                Ok(x) => x,
                Err(e) => { fallos.push(format!("{}: {e}", r.entry)); continue; }
            };
            let channel = if r.release.rolling { Channel::Nightly } else { Channel::Untested };
            for host in [Platform::WindowsX64, Platform::LinuxX64] {
                match plan_from(r, None, host, channel, None, &releases, &lib) {
                    Ok(p) => {
                        ok += 1;
                        eprintln!("ok   {} {host}: {} {} sha256={}", r.entry, p.version, p.asset, p.sha256.is_some());
                    }
                    Err(Error::NoPlatform { .. }) => {}
                    Err(e) => fallos.push(format!("{} {host}: {e}", r.entry)),
                }
            }
        }
        eprintln!("\n{ok} planes bien; {} fallos:", fallos.len());
        for f in &fallos {
            eprintln!("  {f}");
        }
    }

    #[test]
    #[ignore = "usa la red y baja el zip de Windows de Zelda64Recomp (28,9 MB)"]
    fn instala_zelda64recomp_de_verdad() {
        let (r, _) = zelda();
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let agent = net::agent();
        let p = plan(&agent, &r, None, Platform::WindowsX64, Channel::Untested, None, &lib).unwrap();
        eprintln!("{p:#?}");
        assert!(p.sha256.is_some());
        let mut last = 0;
        let rec = install(&agent, &lib, &p, &mut |done, _| last = done).unwrap();
        eprintln!("{rec:#?}");
        assert!(rec.sha256_verified);
        assert_eq!(Some(last), p.size);
        assert_eq!(rec.check_run(&p.dest).unwrap(), RunState::Intact);
    }
}
