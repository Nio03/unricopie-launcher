//! Instalar un juego de principio a fin: planear (qué versión y qué archivo, sin bajar nada),
//! comprobar que cabe en el disco, bajar comprobando el SHA-256 de GitHub, analizar lo bajado con
//! el antivirus del equipo, descomprimir en una carpeta temporal, comprobar que
//! el ejecutable está, dejar el recibo y solo entonces mover todo a su sitio. Si algo falla,
//! no queda nada a medias en la biblioteca.
//!
//! Formatos: zip, tar.gz, tar.xz, 7z (también uno dentro de otro, un nivel), ejecutables sueltos
//! y AppImage. Los instaladores (msi, exe de instalación) y los dmg se rechazan al planear,
//! antes de bajar nada. Para actualizar un juego ya instalado está `update`.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::data::{DataPath, Places};
use super::receipt::{self, PackageFile, Receipt, Stamp};
use super::recipe::{AssetRule, PackageKind, Platform, Recipe, Step};
use super::release::{self, Channel, GhRelease};
use super::space;
use crate::antivirus::{self, Scan};
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
    /// Las recetas de los proyectos que la persona añadió a mano desde GitHub (`added`): un
    /// archivo de la biblioteca, para que viajen con ella (modo portable, `--data-dir`).
    pub added: PathBuf,
}

impl Library {
    pub fn new(root: impl AsRef<Path>) -> Library {
        let root = root.as_ref();
        Library {
            games: root.join("games"),
            downloads: root.join("downloads"),
            previous: root.join("previous"),
            copies: root.join("copies"),
            added: root.join("added.json"),
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
    let rel = release::pick(releases, channel, stable_tag)
        .ok_or_else(|| Error::NoRelease { repo: recipe.repo.clone(), channel })?;
    plan_release(recipe, variant, host, channel, rel, library)
}

/// Planea una versión concreta, la de la etiqueta `tag`, con las releases ya leídas: para
/// instalar una anterior, o para volver a ella desde una más nueva (con `update`, que guarda la
/// que había y pasa lo del jugador, igual que al actualizar). Con las mismas comprobaciones que
/// la última: el archivo de esta plataforma, su tamaño y su SHA-256. Si la etiqueta no está,
/// `Error::NoSuchRelease`; si está pero no trae archivo para esta plataforma, `Error::NoAsset`.
pub fn plan_tag_from(recipe: &Recipe, variant: Option<&str>, host: Platform, tag: &str, releases: &[GhRelease], library: &Library) -> Result<Plan> {
    let rel = release::by_tag(releases, tag).ok_or_else(|| Error::NoSuchRelease { repo: recipe.repo.clone(), tag: tag.to_string() })?;
    plan_release(recipe, variant, host, channel_of(rel), rel, library)
}

/// `plan_tag_from` leyendo de GitHub: entre las últimas releases y, si no está ahí, por su
/// etiqueta (una versión de hace años ya no sale en la lista).
pub fn plan_tag(agent: &ureq::Agent, recipe: &Recipe, variant: Option<&str>, host: Platform, tag: &str, library: &Library) -> Result<Plan> {
    // Antes de usar la red: sin versión para este sistema no hay nada que buscar.
    recipe.resolve(variant, host)?;
    let releases = release::list(agent, &recipe.repo)?;
    match release::by_tag(&releases, tag) {
        Some(rel) => plan_release(recipe, variant, host, channel_of(rel), rel, library),
        None => {
            let rel = release::get(agent, &recipe.repo, tag)?;
            plan_release(recipe, variant, host, channel_of(&rel), &rel, library)
        }
    }
}

/// El canal que se apunta en el recibo de una versión elegida por su etiqueta: lo que es para
/// su proyecto, una prerelease (nightly) o una release oficial (sin probar).
fn channel_of(rel: &GhRelease) -> Channel {
    if rel.prerelease { Channel::Nightly } else { Channel::Untested }
}

/// Una versión de un proyecto que se puede instalar en esta plataforma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// La etiqueta de la release: lo que se le pasa a `plan_tag`.
    pub tag: String,
    /// Cuándo se publicó, como lo da GitHub (`2025-08-27T00:58:35Z`).
    pub published_at: Option<String>,
    pub prerelease: bool,
    /// El archivo que se bajaría.
    pub asset: String,
    /// Lo que pesa; `None` si no está en GitHub (recetas con `url`).
    pub size: Option<u64>,
}

/// Las versiones de `releases` que se pueden instalar en `host` (las que traen el archivo que
/// pide la receta, y solo ese), de la más nueva a la más vieja. Los borradores no cuentan.
pub fn versions_from(recipe: &Recipe, variant: Option<&str>, host: Platform, releases: &[GhRelease]) -> Result<Vec<Version>> {
    let res = recipe.resolve(variant, host)?;
    let mut out: Vec<Version> = releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|rel| {
            let a = asset_of(recipe, &res, rel).ok()?;
            supported(res.kind, &a.name).ok()?;
            Some(Version { tag: rel.tag_name.clone(), published_at: rel.published_at.clone(), prerelease: rel.prerelease, asset: a.name, size: a.size })
        })
        .collect();
    // Como `release::pick`: por fecha de publicación, no por el orden de la API.
    out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(out)
}

/// `versions_from` leyendo las últimas 30 releases de GitHub.
pub fn versions(agent: &ureq::Agent, recipe: &Recipe, variant: Option<&str>, host: Platform) -> Result<Vec<Version>> {
    // Antes de usar la red: si no hay versión para este sistema, no hay nada que listar.
    recipe.resolve(variant, host)?;
    let releases = release::list(agent, &recipe.repo)?;
    versions_from(recipe, variant, host, &releases)
}

/// El archivo de una release que toca según la receta.
struct Asset {
    name: String,
    url: String,
    size: Option<u64>,
    sha256: Option<String>,
}

fn asset_of(recipe: &Recipe, res: &super::recipe::Resolved<'_>, rel: &GhRelease) -> Result<Asset> {
    let a = match &res.asset {
        AssetRule::Pattern(p) => {
            let a = release::find_asset(rel, p, &res.exclude)?;
            Asset { name: a.name.clone(), url: a.browser_download_url.clone(), size: Some(a.size), sha256: a.sha256().map(str::to_string) }
        }
        AssetRule::Url(template) => {
            let tag = rel.tag_name.as_str();
            let version = recipe.release.tag_prefix.as_deref().and_then(|p| tag.strip_prefix(p)).unwrap_or(tag);
            let url = template.replace("{version}", version);
            let name = url.rsplit('/').next().unwrap_or_default().to_string();
            Asset { name, url, size: None, sha256: None }
        }
    };
    if !a.url.starts_with("https://") || a.name.is_empty() {
        return Err(Error::BadRecipe(format!("dirección de descarga no válida: {}", a.url)));
    }
    Ok(a)
}

/// Planea instalar la release `rel`, la que sea: la del canal (`plan_from`) o una elegida por
/// su etiqueta (`plan_tag_from`).
pub fn plan_release(recipe: &Recipe, variant: Option<&str>, host: Platform, channel: Channel, rel: &GhRelease, library: &Library) -> Result<Plan> {
    if rel.draft {
        return Err(Error::NoSuchRelease { repo: recipe.repo.clone(), tag: rel.tag_name.clone() });
    }
    let res = recipe.resolve(variant, host)?;
    let Asset { name: asset, url, size, sha256 } = asset_of(recipe, &res, rel)?;
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

/// Baja el archivo del plan a la carpeta de descargas, comprobando el tamaño y el SHA-256, y lo
/// analiza con el antivirus del equipo. Antes de bajar mira que haya espacio en el disco.
pub(crate) fn fetch(agent: &ureq::Agent, library: &Library, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(PathBuf, Downloaded)> {
    let file = library.downloads.join(RelPath::new(&plan.asset)?.file_name());
    // Antes de bajar nada: que quepan lo bajado y lo instalado.
    space::check_before_download(library, plan)?;
    let expected = plan.sha256.clone().map(Expected::Sha256);
    // GitHub dice el tamaño exacto: más que eso es que algo va mal.
    let max = plan.size.unwrap_or(16 << 30);
    let mut got = net::download(agent, &plan.url, &file, max, expected.as_ref(), progress)?;
    got.scan = Some(scanned(&file, &plan.asset, antivirus::scan(&file))?);
    Ok((file, got))
}

/// Qué hacer con lo que dijo el antivirus de lo bajado (`file`, que se llama `asset`): si está
/// limpio o no se pudo analizar, se sigue, y eso es lo que va al recibo; si encontró algo, se
/// borra lo bajado y no se instala.
pub(crate) fn scanned(file: &Path, asset: &str, result: std::result::Result<Scan, String>) -> Result<Scan> {
    result.map_err(|threat| {
        let _ = fs::remove_file(file);
        Error::Threat { file: asset.to_string(), threat }
    })
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
    // Otra vez el espacio, ahora que se sabe lo que pesa lo bajado (y, de un zip, lo que ocupa).
    space::check_before_extract(library, plan, file, got.size)?;
    let tmp = TmpDir::new(&library.games)?;
    let content = tmp.path().join("contenido");
    fs::create_dir_all(&content).map_err(|e| Error::io(&content, e))?;
    let (root, files) = match plan.kind {
        PackageKind::Archive => {
            let kind = archive::Kind::from_name(&plan.asset).ok_or_else(|| Error::UnsupportedPackage { asset: plan.asset.clone(), kind: "Archive".into() })?;
            let extracted = archive::extract(file, kind, &content, Limits::default())?;
            let (content, extracted) = unwrap_nested(tmp.path(), content, extracted, plan.run.as_deref())?;
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
        scan: got.scan.clone(),
    };
    Ok(Prepared { root, receipt, _tmp: tmp })
}

/// Un archivo dentro de otro: los artefactos de GitHub Actions suelen ser un zip con un solo
/// `tar.gz` dentro, que es el que guarda los permisos de ejecución (el zip de Linux de
/// Zelda64Recomp trae solo `Zelda64Recompiled.tar.gz`). Si lo descomprimido es un único archivo
/// y es de un formato que sabemos abrir, se descomprime también, con las mismas comprobaciones
/// y límites que el de fuera, y el juego es lo que trae. Solo un nivel, y no si ese archivo es
/// justo lo que la receta manda abrir.
fn unwrap_nested(tmp: &Path, content: PathBuf, extracted: Vec<archive::Extracted>, run: Option<&str>) -> Result<(PathBuf, Vec<archive::Extracted>)> {
    let [only] = extracted.as_slice() else { return Ok((content, extracted)) };
    let Some(kind) = archive::Kind::from_name(only.path.as_str()) else { return Ok((content, extracted)) };
    if run.is_some_and(|r| RelPath::new(r).is_ok_and(|r| r.under(&content).exists())) {
        return Ok((content, extracted));
    }
    let inner = only.path.under(&content);
    let nested = tmp.join("anidado");
    fs::create_dir_all(&nested).map_err(|e| Error::io(&nested, e))?;
    let files = archive::extract(&inner, kind, &nested, Limits::default())?;
    // Lo de fuera ya no hace falta. La carpeta temporal se borra entera al terminar, pero así
    // no ocupa el doble mientras tanto.
    let _ = fs::remove_dir_all(&content);
    Ok((nested, files))
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
    fn planea_una_version_concreta() {
        let (r, rs) = zelda();
        let lib = Library::new("/biblioteca");
        // La última, pedida por su etiqueta, es el mismo plan que el del canal.
        let ultima = plan_from(&r, None, Platform::WindowsX64, Channel::Untested, None, &rs, &lib).unwrap();
        assert_eq!(plan_tag_from(&r, None, Platform::WindowsX64, "v1.2.2", &rs, &lib).unwrap(), ultima);
        // Una prerelease queda apuntada como nightly, con su archivo y sin suma.
        let beta = plan_tag_from(&r, None, Platform::WindowsX64, "v1.2.3-beta", &rs, &lib).unwrap();
        assert_eq!((beta.version.as_str(), beta.channel, beta.asset.as_str()), ("v1.2.3-beta", Channel::Nightly, "Zelda64Recompiled-v1.2.3-beta-Windows.zip"));
        assert_eq!(beta.dest, ultima.dest);
        // La etiqueta existe pero esa release no trae nada para esta plataforma.
        assert!(matches!(plan_tag_from(&r, None, Platform::WindowsX64, "v1.2.1", &rs, &lib), Err(Error::NoAsset { tag, .. }) if tag == "v1.2.1"));
        assert!(matches!(plan_tag_from(&r, None, Platform::LinuxX64, "v1.2.3-beta", &rs, &lib), Err(Error::NoAsset { .. })));
        // Una que no existe, y un borrador, que es como si no existiera.
        for tag in ["v0.1", "v1.3.0-dev", ""] {
            assert!(matches!(plan_tag_from(&r, None, Platform::WindowsX64, tag, &rs, &lib), Err(Error::NoSuchRelease { tag: t, .. }) if t == tag));
        }
    }

    #[test]
    fn lista_las_versiones_de_esta_plataforma() {
        let (r, mut rs) = zelda();
        let vs = versions_from(&r, None, Platform::WindowsX64, &rs).unwrap();
        // Ni el borrador ni la v1.2.1, que no trae archivos; la más nueva primero.
        assert_eq!(vs.iter().map(|v| v.tag.as_str()).collect::<Vec<_>>(), ["v1.2.3-beta", "v1.2.2"]);
        assert!(vs[0].prerelease && !vs[1].prerelease);
        assert_eq!((vs[1].asset.as_str(), vs[1].size, vs[1].published_at.as_deref()), ("Zelda64Recompiled-v1.2.2-Windows.zip", Some(28940501), Some("2025-08-27T00:58:35Z")));
        // En Linux la beta no tiene archivo.
        let linux = versions_from(&r, None, Platform::LinuxX64, &rs).unwrap();
        assert_eq!(linux.iter().map(|v| v.tag.as_str()).collect::<Vec<_>>(), ["v1.2.2"]);
        // Por fecha de publicación, venga como venga de la API; y una release con dos archivos
        // que encajan no se puede instalar, así que no sale.
        rs.reverse();
        let dup = rs.iter_mut().find(|x| x.tag_name == "v1.2.3-beta").unwrap();
        let otro = dup.assets[0].clone();
        dup.assets.push(release::GhAsset { name: "Zelda64Recompiled-v1.2.3-beta-b-Windows.zip".into(), ..otro });
        let vs = versions_from(&r, None, Platform::WindowsX64, &rs).unwrap();
        assert_eq!(vs.iter().map(|v| v.tag.as_str()).collect::<Vec<_>>(), ["v1.2.2"]);
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

    #[test]
    fn el_resultado_del_antivirus_va_al_recibo_y_una_amenaza_borra_lo_bajado() {
        use crate::antivirus::{Reason, Scan, DEFENDER};
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("Zelda64Recompiled.exe", b"exe")]);
        let plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));

        // Limpio, o sin analizar: se sigue con eso apuntado, y lo bajado no se toca.
        let limpio = Scan::Clean { scanner: DEFENDER.into() };
        assert_eq!(scanned(&z, &plan.asset, Ok(limpio.clone())).unwrap(), limpio);
        let sin = Scan::NotScanned { reason: Reason::Disabled };
        assert_eq!(scanned(&z, &plan.asset, Ok(sin.clone())).unwrap(), sin);
        assert!(z.is_file());
        let r = install_downloaded(&lib, &plan, &z, &Downloaded { size: 3, sha256: "abc".into(), scan: Some(sin.clone()) }).unwrap();
        assert_eq!(r.scan, Some(sin));
        assert_eq!(Receipt::read(&plan.dest).unwrap().unwrap().scan, r.scan);

        // Una amenaza: el error lleva su nombre y el del archivo, y lo bajado se borra.
        let err = scanned(&z, &plan.asset, Err("Trojan:Win32/Prueba".into())).unwrap_err();
        assert!(matches!(&err, Error::Threat { file, threat } if file == &plan.asset && threat == "Trojan:Win32/Prueba"), "{err}");
        assert!(!z.exists());
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
        let got = Downloaded { size: 3, sha256: "abc".into(), scan: None };
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

    /// Un tar.gz en memoria con `entries` (ruta, datos, modo).
    fn tar_gz_with(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, data, mode) in entries {
            let mut h = tar::Header::new_gnu();
            // Nombre escrito a mano para poder probar rutas que `set_path` no dejaría poner.
            let n = name.as_bytes();
            h.as_old_mut().name[..n.len()].copy_from_slice(n);
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    /// El zip de Linux de Zelda64Recomp: dentro solo hay un tar.gz, y el juego está en él.
    #[test]
    fn instala_un_archivo_dentro_de_otro() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        let dentro = tar_gz_with(&[("Zelda64Recompiled", b"elf", 0o755), ("assets/a.bin", b"a", 0o644)]);
        zip_with(&z, &[("Zelda64Recompiled.tar.gz", &dentro)]);
        let plan = fake_plan(&lib, Some("Zelda64Recompiled"));
        let r = install_downloaded(&lib, &plan, &z, &Downloaded { size: 3, sha256: "abc".into(), scan: None }).unwrap();
        assert!(plan.dest.join("Zelda64Recompiled").is_file());
        assert!(plan.dest.join("assets").join("a.bin").is_file());
        // El tar.gz de en medio no queda, ni en el juego ni en el recibo.
        assert!(!plan.dest.join("Zelda64Recompiled.tar.gz").exists());
        let files: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["Zelda64Recompiled", "assets/a.bin"]);
        assert_eq!(Receipt::read(&plan.dest).unwrap().unwrap().check_run(&plan.dest).unwrap(), RunState::Intact);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(fs::metadata(plan.dest.join("Zelda64Recompiled")).unwrap().permissions().mode() & 0o111, 0);
        }
        let left: Vec<_> = fs::read_dir(&lib.games).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("zelda-majoras-mask")]);
    }

    /// El de dentro también puede traer su carpeta única, como el de fuera.
    #[test]
    fn el_archivo_de_dentro_puede_traer_su_carpeta() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        let dentro = tar_gz_with(&[("juego-linux/juego", b"elf", 0o755), ("juego-linux/datos.bin", b"d", 0o644)]);
        zip_with(&z, &[("juego.tgz", &dentro)]);
        let plan = fake_plan(&lib, Some("juego"));
        let r = install_downloaded(&lib, &plan, &z, &Downloaded { size: 3, sha256: "abc".into(), scan: None }).unwrap();
        let files: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["juego", "datos.bin"]);
        assert!(plan.dest.join("datos.bin").is_file());
    }

    /// Lo de dentro pasa por las mismas comprobaciones que lo de fuera, y solo se baja un nivel.
    #[test]
    fn el_archivo_de_dentro_se_comprueba_igual() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let got = Downloaded { size: 1, sha256: "x".into(), scan: None };
        let plan = fake_plan(&lib, Some("juego"));

        let z = t.path().join("malo.zip");
        zip_with(&z, &[("juego.tar.gz", &tar_gz_with(&[("../fuera", b"x", 0o644)]))]);
        let err = install_downloaded(&lib, &plan, &z, &got).unwrap_err();
        assert!(matches!(err, Error::UnsafePath(_)), "{err}");
        assert!(!t.path().join("fuera").exists() && !lib.games.join("fuera").exists());
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 0);

        // Un archivo dentro de otro dentro de otro: el segundo se queda como está.
        let z = t.path().join("doble.zip");
        let medio = t.path().join("medio.zip");
        zip_with(&medio, &[("juego.tar.gz", &tar_gz_with(&[("juego", b"elf", 0o755)]))]);
        zip_with(&z, &[("medio.zip", &fs::read(&medio).unwrap())]);
        let err = install_downloaded(&lib, &plan, &z, &got).unwrap_err();
        assert!(matches!(err, Error::RunNotFound { .. }), "{err}");

        // Si lo que la receta manda abrir es ese único archivo, no se toca.
        let z = t.path().join("suelto.zip");
        zip_with(&z, &[("juego.zip", b"no es un zip")]);
        let plan = fake_plan(&lib, Some("juego.zip"));
        let r = install_downloaded(&lib, &plan, &z, &got).unwrap();
        assert_eq!(r.files.len(), 1);
        assert!(plan.dest.join("juego.zip").is_file());
    }

    #[test]
    fn no_deja_nada_si_falta_el_ejecutable() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("otro.exe", b"x")]);
        let plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        let err = install_downloaded(&lib, &plan, &z, &Downloaded { size: 1, sha256: "x".into(), scan: None }).unwrap_err();
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
        let err = install_downloaded(&lib, &plan, &z, &Downloaded { size: 1, sha256: "otra".into(), scan: None }).unwrap_err();
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
        let err = install_downloaded(&lib, &plan, &f, &Downloaded { size: 4, sha256: "x".into(), scan: None }).unwrap_err();
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
        let r = install_downloaded(&lib, &plan, &f, &Downloaded { size: 4, sha256: "x".into(), scan: None }).unwrap();
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
