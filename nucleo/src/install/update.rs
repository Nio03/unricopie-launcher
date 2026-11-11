//! Actualizar un juego instalado sin perder lo del jugador, y volver atrás.
//!
//! El recibo guarda la lista de archivos que trajo el paquete, con su XXH3. Al actualizar,
//! cada archivo de la instalación actual se compara con esa lista (lo que había) y con la del
//! paquete nuevo (lo que viene):
//!
//! - **No lo trajo el paquete** (lo creó el juego o el jugador: partidas, ajustes,
//!   `portable.txt`, también carpetas vacías): pasa a la versión nueva. Si la nueva trae uno
//!   distinto en esa misma ruta, se queda el del jugador y se avisa (`kept`).
//! - **Lo trajo el paquete y está como llegó:** manda la versión nueva; si la nueva ya no lo
//!   trae, desaparece.
//! - **Lo trajo el paquete y cambió** (un ajuste editado, o el actualizador propio del
//!   proyecto cambió el exe): si la versión nueva trae ese archivo igual que la vieja, se
//!   conserva el cambio (`kept`); si trae otro, o ya no lo trae, manda la nueva y se avisa
//!   (`replaced`).
//!
//! Nada se pierde: la instalación de antes, entera (el programa y lo del jugador tal como
//! estaban), se guarda en `previous/<carpeta del juego>/<id>`, y volver atrás la intercambia
//! con la actual, así que también se puede volver adelante. El núcleo nunca borra una versión
//! anterior por su cuenta: lo hace `remove_previous` cuando la persona lo pide.
//!
//! Antes de actualizar o de volver atrás, el juego tiene que estar cerrado y sus mods quitados
//! (`mods::deploy::purge`); después se vuelven a desplegar. Que esté cerrado se comprueba con
//! el ejecutable del recibo (`is_running`): en Windows, mover la carpeta de un juego abierto
//! solo falla si su carpeta de trabajo está dentro, así que no basta con eso (probado el 1-11
//! con un exe en marcha).
//!
//! Las carpetas de fuera donde el proyecto guarda partidas y ajustes (el `data-dir` de la
//! receta, como `%LOCALAPPDATA%\BanjoRecompiled`) no se tocan al actualizar, pero se guarda una
//! copia con la versión anterior (`previous/<carpeta>/<id>.data`, ver `data`). Al volver atrás,
//! se guarda primero cómo están ahora (con la versión que se aparta) y luego se ponen las de esa
//! versión: si la nueva convirtió las partidas a otro formato, la vieja vuelve a tener las suyas.
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::data::{self, Saved};
use super::installer::{self, Library, Plan};
use super::receipt::{self, PackageFile, Receipt, Stamp};
use crate::net::Downloaded;
use crate::path::RelPath;
use crate::{Error, Result};

/// Si hay otra versión para lo que está instalado en `plan.dest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    UpToDate(Receipt),
    /// Otra versión, u otro archivo con la misma etiqueta (una release rehecha, o la versión
    /// de 64 bits donde antes solo había de 32).
    Available(Receipt),
}

/// Lo que dejó una actualización.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    pub receipt: Receipt,
    /// Dónde quedó la versión de antes.
    pub previous: Previous,
    pub merge: Merge,
}

/// Qué pasó con lo del jugador al actualizar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Merge {
    /// Archivos de la instalación de antes que pasaron a la nueva: los que creó el juego o el
    /// jugador, y los cambios que se conservan.
    pub carried: usize,
    /// Se quedó el del jugador aunque el paquete nuevo trae otro en esa ruta.
    pub kept: Vec<RelPath>,
    /// Archivos del paquete que alguien cambió y que la versión nueva sustituye o quita. El
    /// cambio sigue en la versión anterior guardada.
    pub replaced: Vec<RelPath>,
    /// Lo que no se pudo pasar (enlaces, nombres que no son UTF-8 o que chocan con una carpeta
    /// de la versión nueva), relativo a la carpeta del juego. Sigue en la versión anterior.
    pub skipped: Vec<PathBuf>,
}

/// Una versión anterior guardada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    /// El nombre de su carpeta: `<segundos desde 1970>-<versión>`.
    pub id: String,
    pub path: PathBuf,
    pub receipt: Receipt,
    /// Las carpetas de fuera del juego que se guardaron con ella.
    pub data: Vec<PathBuf>,
}

/// Lo que dejó volver atrás.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledBack {
    /// La versión que vuelve a estar instalada.
    pub receipt: Receipt,
    /// Dónde quedó la que estaba instalada (`None` si no había ninguna, porque una
    /// actualización falló a mitad de camino).
    pub previous: Option<Previous>,
}

pub fn status(plan: &Plan) -> Result<Status> {
    if !plan.dest.exists() {
        return Ok(Status::NotInstalled);
    }
    let current = installed(&plan.dest)?;
    if current.entry != plan.entry || current.game != plan.game {
        return Err(Error::OtherGame { dir: plan.dest.clone(), entry: current.entry });
    }
    Ok(if differs(&current, plan) { Status::Available(current) } else { Status::UpToDate(current) })
}

fn differs(current: &Receipt, plan: &Plan) -> bool {
    current.version != plan.version
        || current.asset != plan.asset
        || plan.sha256.as_deref().is_some_and(|s| !s.eq_ignore_ascii_case(&current.sha256))
        || plan.size.is_some_and(|s| s != current.size)
}

pub(crate) fn installed(dir: &Path) -> Result<Receipt> {
    Receipt::read(dir)?.ok_or_else(|| Error::NoReceipt(dir.to_path_buf()))
}

fn pending(plan: &Plan) -> Result<Receipt> {
    match status(plan)? {
        Status::NotInstalled => Err(Error::NotInstalled(plan.dest.clone())),
        Status::UpToDate(r) => Err(Error::UpToDate(r.version)),
        Status::Available(r) => Ok(r),
    }
}

/// Baja la versión de `plan` y actualiza lo instalado en `plan.dest`. Comprueba antes de bajar
/// que hay algo que actualizar.
pub fn update(agent: &ureq::Agent, library: &Library, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Updated> {
    closed(&plan.dest, &pending(plan)?)?;
    let (file, got) = installer::fetch(agent, library, plan, progress)?;
    let result = update_downloaded(library, plan, &file, &got);
    let _ = fs::remove_file(&file);
    result
}

/// Solo baja la versión de `plan` (comprobando antes que hay algo que actualizar), sin tocar lo
/// instalado: para aplicarla después con `update_downloaded`. Así la app baja en su cola y
/// aplica en el hilo de los mods, que antes de cambiar la carpeta quita los mods. El juego
/// puede seguir abierto mientras baja; al aplicar sí tiene que estar cerrado.
pub fn download(agent: &ureq::Agent, library: &Library, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(PathBuf, Downloaded)> {
    pending(plan)?;
    installer::fetch(agent, library, plan, progress)
}

/// La parte de la actualización que no usa la red: lo bajado ya está en `file`.
///
/// Prepara la versión nueva aparte, le pasa lo del jugador y solo entonces cambia una carpeta
/// por otra. Si algo falla antes, la instalación de antes no se ha tocado.
pub fn update_downloaded(library: &Library, plan: &Plan, file: &Path, got: &Downloaded) -> Result<Updated> {
    let current = pending(plan)?;
    closed(&plan.dest, &current)?;
    let mut new = installer::prepare(library, plan, file, got)?;
    let merge = merge(&plan.dest, &current.files, &new.receipt.files, &new.root)?;
    // Los marcadores del modo portable no son del paquete: ya pasaron con lo del jugador.
    new.receipt.portable = current.portable.clone().map(|mut p| {
        p.placed.retain(|x| !new.receipt.files.iter().any(|f| same_key(f.path.as_str(), x.path.as_str())));
        p
    });
    // Lo escrito en la configuración y la decisión sobre el actualizador siguen valiendo: la
    // configuración pasó con lo del jugador (o está fuera, en su carpeta de datos).
    new.receipt.settings = current.settings.clone();
    new.receipt.self_update_allowed = current.self_update_allowed;
    new.receipt.write(&new.root)?;
    // Otra vez: preparar puede llevar un rato.
    closed(&plan.dest, &current)?;
    let previous = set_aside(library, &plan.dest, current, &plan.data_dirs)?;
    if let Err(e) = fs::rename(&new.root, &plan.dest) {
        if fs::rename(&previous.path, &plan.dest).is_ok() {
            let _ = fs::remove_dir_all(data_dir_of(&previous.path));
        }
        return Err(Error::io(&plan.dest, e));
    }
    Ok(Updated { receipt: new.receipt, previous, merge })
}

/// ¿Está abierto el ejecutable del juego? Un ejecutable en marcha no se puede abrir para
/// escribir: Windows da «archivo en uso» y Linux, ETXTBSY. Abrirlo así no cambia nada (no se
/// escribe ni se trunca). Solo vigila el ejecutable de `run`: si el juego se abrió desde otro,
/// no lo nota, y en macOS puede que tampoco.
pub fn is_running(dir: &Path, receipt: &Receipt) -> bool {
    let Some(path) = receipt.run.as_deref().and_then(|r| RelPath::new(r).ok()).map(|r| r.under(dir)) else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    match fs::OpenOptions::new().write(true).open(&path) {
        Ok(_) => false,
        Err(e) => busy(&e),
    }
}

fn busy(e: &std::io::Error) -> bool {
    if cfg!(windows) {
        e.raw_os_error() == Some(32) // ERROR_SHARING_VIOLATION
    } else if cfg!(unix) {
        e.raw_os_error() == Some(26) // ETXTBSY
    } else {
        false
    }
}

pub(crate) fn closed(dir: &Path, receipt: &Receipt) -> Result<()> {
    if is_running(dir, receipt) { Err(Error::GameRunning(dir.to_path_buf())) } else { Ok(()) }
}

/// Las versiones anteriores guardadas de lo instalado en `dir`, la más reciente primero.
pub fn previous_versions(library: &Library, dir: &Path) -> Result<Vec<Previous>> {
    let base = previous_root(library, dir)?;
    let entries = match fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&base, e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(&base, e))?;
        let path = entry.path();
        let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
        if !path.is_dir() {
            continue;
        }
        // Sin recibo legible no es algo que guardáramos nosotros (ni una copia `.data`).
        if let Ok(Some(receipt)) = Receipt::read(&path) {
            let data = saved_paths(&path);
            out.push(Previous { id, path, receipt, data });
        }
    }
    out.sort_by_key(|p| Reverse(order(&p.id)));
    Ok(out)
}

/// Vuelve a la versión anterior `id`: la cambia por la instalada, que pasa a ser una versión
/// anterior más (así se puede volver adelante sin bajar nada).
///
/// Si esa versión guardó carpetas de fuera, las de ahora se guardan con la que se aparta y se
/// ponen las suyas. Si no hay nada instalado (una actualización que falló a mitad), las de
/// ahora no se tocan: la actualización no las cambia, y sin versión que apartar no habría dónde
/// guardarlas.
pub fn roll_back(library: &Library, dir: &Path, id: &str) -> Result<RolledBack> {
    let chosen = find_previous(library, dir, id)?;
    let chosen_data = data_dir_of(&chosen.path);
    let saved = Saved::read(&chosen_data);
    let previous = if dir.exists() {
        let current = installed(dir)?;
        closed(dir, &current)?;
        let now = saved.as_ref().map(Saved::paths).unwrap_or_default();
        Some(set_aside(library, dir, current, &now)?)
    } else {
        None
    };
    if let Err(e) = fs::rename(&chosen.path, dir) {
        if let Some(p) = &previous {
            if fs::rename(&p.path, dir).is_ok() {
                let _ = fs::remove_dir_all(data_dir_of(&p.path));
            }
        }
        return Err(Error::io(dir, e));
    }
    if let (Some(s), Some(_)) = (&saved, &previous) {
        data::restore(s, &chosen_data)?;
        // Ya están en su sitio: la copia sobra.
        let _ = fs::remove_dir_all(&chosen_data);
    }
    Ok(RolledBack { receipt: chosen.receipt, previous })
}

/// Borra la versión anterior `id`. Solo cuando la persona lo pide.
pub fn remove_previous(library: &Library, dir: &Path, id: &str) -> Result<()> {
    let p = find_previous(library, dir, id)?;
    fs::remove_dir_all(&p.path).map_err(|e| Error::io(&p.path, e))?;
    let d = data_dir_of(&p.path);
    if d.exists() {
        fs::remove_dir_all(&d).map_err(|e| Error::io(&d, e))?;
    }
    Ok(())
}

/// `<id>` → `<id>.data`, donde va la copia de las carpetas de fuera.
fn data_dir_of(previous: &Path) -> PathBuf {
    let mut s = previous.as_os_str().to_owned();
    s.push(".data");
    PathBuf::from(s)
}

fn saved_paths(previous: &Path) -> Vec<PathBuf> {
    Saved::read(&data_dir_of(previous)).map(|s| s.paths()).unwrap_or_default()
}

fn find_previous(library: &Library, dir: &Path, id: &str) -> Result<Previous> {
    let missing = || Error::NoPrevious { dir: dir.to_path_buf(), id: id.to_string() };
    // Un nombre de carpeta y nada más: ni `..` ni rutas.
    if id.starts_with('.') || !RelPath::new(id).is_ok_and(|r| r.as_str() == id && !id.contains('/')) {
        return Err(missing());
    }
    let path = previous_root(library, dir)?.join(id);
    match Receipt::read(&path) {
        Ok(Some(receipt)) if path.is_dir() => {
            let data = saved_paths(&path);
            Ok(Previous { id: id.to_string(), path, receipt, data })
        }
        _ => Err(missing()),
    }
}

/// `<previous>/<carpeta del juego>`, comprobando que `dir` es un juego de la biblioteca.
fn previous_root(library: &Library, dir: &Path) -> Result<PathBuf> {
    match dir.file_name() {
        Some(name) if dir.parent() == Some(library.games.as_path()) => Ok(library.previous.join(name)),
        _ => Err(Error::NotInLibrary(dir.to_path_buf())),
    }
}

/// Mueve la instalación de `dir` a `previous/<carpeta>/<id>`, con una copia de las carpetas
/// de fuera `data_dirs` en `<id>.data`. Si algo falla, no queda nada a medias.
fn set_aside(library: &Library, dir: &Path, receipt: Receipt, data_dirs: &[PathBuf]) -> Result<Previous> {
    let base = previous_root(library, dir)?;
    fs::create_dir_all(&base).map_err(|e| Error::io(&base, e))?;
    let stem = format!("{}-{}", receipt::now(), safe_name(&receipt.version));
    let mut id = stem.clone();
    let mut n = 2;
    while base.join(&id).exists() || data_dir_of(&base.join(&id)).exists() {
        id = format!("{stem}-{n}");
        n += 1;
    }
    let path = base.join(&id);
    let data_path = data_dir_of(&path);
    if !data_dirs.is_empty() {
        data::save(data_dirs, &data_path)?;
    }
    if let Err(e) = fs::rename(dir, &path) {
        let _ = fs::remove_dir_all(&data_path);
        return Err(Error::io(dir, e));
    }
    Ok(Previous { id, path, receipt, data: data_dirs.to_vec() })
}

/// La etiqueta como nombre de carpeta: las de GitHub pueden llevar `/` y otras cosas.
fn safe_name(version: &str) -> String {
    let s: String = version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .take(60)
        .collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() { "version".into() } else { s }
}

/// Para ordenar los id: primero los segundos, luego el resto.
fn order(id: &str) -> (u64, String) {
    (id.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0), id.to_string())
}

/// Compara rutas como el disco: en Windows y macOS sin distinguir mayúsculas.
pub(crate) fn same_key(a: &str, b: &str) -> bool {
    fold(a) == fold(b)
}

fn fold(s: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) { s.to_lowercase() } else { s.to_string() }
}

/// Pasa a `new_root` lo del jugador que hay en `old`, según las reglas de arriba.
fn merge(old: &Path, before: &[PackageFile], after: &[PackageFile], new_root: &Path) -> Result<Merge> {
    let before: HashMap<String, &PackageFile> = before.iter().map(|f| (fold(f.path.as_str()), f)).collect();
    let after: HashMap<String, &PackageFile> = after.iter().map(|f| (fold(f.path.as_str()), f)).collect();
    let mut m = Merge::default();
    let mut files = Vec::new();
    let mut empty = Vec::new();
    walk(old, old, "", &mut files, &mut empty, &mut m.skipped)?;
    // Las carpetas vacías también son del jugador: para algunos juegos que exista una carpeta
    // ya cambia algo (`Support` en OpenRA, `saves` en Skate 3).
    for rel in empty {
        let dest = rel.under(new_root);
        if dest.is_dir() {
            continue;
        }
        if dest.exists() || rel.parents().iter().any(|p| p.under(new_root).is_file()) {
            m.skipped.push(rel.under(Path::new("")));
            continue;
        }
        fs::create_dir_all(&dest).map_err(|e| Error::io(&dest, e))?;
    }
    for (rel, src) in files {
        let k = fold(rel.as_str());
        let size = fs::metadata(&src).map_err(|e| Error::io(&src, e))?.len();
        let mut ours = None;
        let keep = match (before.get(&k), after.get(&k)) {
            (None, None) => true,
            (None, Some(t)) => {
                if same(&src, size, &mut ours, t)? {
                    false
                } else {
                    m.kept.push(rel.clone());
                    true
                }
            }
            (Some(b), t) => {
                if same(&src, size, &mut ours, b)? {
                    false // está como llegó: manda la versión nueva
                } else {
                    match t {
                        // El jugador ya tiene lo mismo que trae la nueva.
                        Some(t) if same(&src, size, &mut ours, t)? => false,
                        Some(t) if t.stamp() == b.stamp() => {
                            m.kept.push(rel.clone());
                            true
                        }
                        _ => {
                            m.replaced.push(rel.clone());
                            false
                        }
                    }
                }
            }
        };
        if keep {
            if copy_into(&src, &rel, new_root)? {
                m.carried += 1;
            } else {
                m.skipped.push(rel.under(Path::new("")));
            }
        }
    }
    Ok(m)
}

/// ¿`src` es igual que `f`? Si el tamaño no coincide no hace falta leerlo, y si hay que
/// leerlo se lee una sola vez.
fn same(src: &Path, size: u64, ours: &mut Option<Stamp>, f: &PackageFile) -> Result<bool> {
    if size != f.size {
        return Ok(false);
    }
    let stamp = match ours {
        Some(s) => *s,
        None => *ours.insert(Stamp::of(src)?),
    };
    Ok(stamp == f.stamp())
}

/// Copia (nunca enlaza: la versión anterior tiene que quedar tal cual aunque el juego escriba
/// encima) `src` a `rel` dentro de `root`. `false` si la versión nueva tiene una carpeta en
/// esa ruta, o un archivo donde haría falta una carpeta.
fn copy_into(src: &Path, rel: &RelPath, root: &Path) -> Result<bool> {
    let dest = rel.under(root);
    if dest.is_dir() || rel.parents().iter().any(|p| p.under(root).is_file()) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    fs::copy(src, &dest).map_err(|e| Error::io(&dest, e))?;
    Ok(true)
}

/// Los archivos de `dir`, con su ruta relativa a `root`, y las carpetas vacías (`empty`). Se
/// salta el recibo y apunta en `skipped` lo que no se puede pasar.
fn walk(
    root: &Path,
    dir: &Path,
    rel: &str,
    out: &mut Vec<(RelPath, PathBuf)>,
    empty: &mut Vec<RelPath>,
    skipped: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries = fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.peekable();
    if entries.peek().is_none() && !rel.is_empty() {
        if let Ok(r) = RelPath::new(rel) {
            empty.push(r);
        }
        return Ok(());
    }
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let path = entry.path();
        let skip = |skipped: &mut Vec<PathBuf>| skipped.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            skip(skipped);
            continue;
        };
        // Un enlace podría apuntar fuera; una `\` en un nombre de Unix no es una carpeta.
        if kind.is_symlink() || name.contains('\\') {
            skip(skipped);
            continue;
        }
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if kind.is_dir() {
            walk(root, &path, &child, out, empty, skipped)?;
        } else if rel.is_empty() && (name == receipt::FILE_NAME || name == format!("{}.tmp", receipt::FILE_NAME)) {
            continue;
        } else {
            match RelPath::new(&child) {
                Ok(r) => out.push((r, path.clone())),
                Err(_) => skip(skipped),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::installer::tests::{fake_plan, zip_with};
    use crate::install::receipt::RunState;
    use std::ffi::OsString;

    fn plan_v(lib: &Library, version: &str) -> Plan {
        let mut p = fake_plan(lib, Some("Zelda64Recompiled.exe"));
        p.version = version.into();
        p.size = None;
        p
    }

    fn got(sha: &str) -> Downloaded {
        Downloaded { size: 1, sha256: sha.into() }
    }

    fn read(dir: &Path, rel: &str) -> Vec<u8> {
        fs::read(RelPath::new(rel).unwrap().under(dir)).unwrap()
    }

    fn names(v: &[RelPath]) -> Vec<&str> {
        let mut n: Vec<&str> = v.iter().map(RelPath::as_str).collect();
        n.sort();
        n
    }

    /// Instala una v1 y juega con ella: deja partidas, edita ajustes y cambia el exe como lo
    /// haría el actualizador propio del proyecto.
    fn v1_jugada(t: &Path, lib: &Library) -> PathBuf {
        let z = t.join("v1.zip");
        zip_with(&z, &[
            ("Zelda64Recompiled.exe", b"exe 1"),
            ("assets/a.bin", b"a"),
            ("config.ini", b"por defecto"),
            ("leeme.txt", b"leeme 1"),
            ("vieja.dll", b"vieja"),
        ]);
        let v1 = plan_v(lib, "v1.0.0");
        installer::install_downloaded(lib, &v1, &z, &got("uno")).unwrap();
        let d = v1.dest;
        fs::create_dir_all(d.join("saves")).unwrap();
        fs::write(d.join("saves").join("slot1.sav"), b"partida").unwrap();
        fs::write(d.join("portable.txt"), b"").unwrap();
        fs::write(d.join("config.ini"), b"mis ajustes").unwrap();
        fs::write(d.join("leeme.txt"), b"con notas mias").unwrap();
        fs::write(d.join("Zelda64Recompiled.exe"), b"exe 1.1").unwrap();
        fs::write(d.join("ajustes.json"), b"{\"mio\":1}").unwrap();
        d
    }

    fn zip_v2(t: &Path) -> PathBuf {
        let z = t.join("v2.zip");
        zip_with(&z, &[
            ("Zelda64Recompiled.exe", b"exe 2"),
            ("assets/a.bin", b"a"),
            ("config.ini", b"por defecto"),
            ("leeme.txt", b"leeme 2"),
            ("nueva.dll", b"nueva"),
            ("ajustes.json", b"{}"),
        ]);
        z
    }

    #[test]
    fn actualiza_conservando_lo_del_jugador() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = v1_jugada(t.path(), &lib);
        let z2 = zip_v2(t.path());
        let v2 = plan_v(&lib, "v2.0.0");
        assert!(matches!(status(&v2).unwrap(), Status::Available(r) if r.version == "v1.0.0"));

        let up = update_downloaded(&lib, &v2, &z2, &got("dos")).unwrap();

        // El programa es el de la v2...
        assert_eq!(read(&d, "Zelda64Recompiled.exe"), b"exe 2");
        assert_eq!(read(&d, "nueva.dll"), b"nueva");
        assert!(!d.join("vieja.dll").exists());
        // ...y lo del jugador sigue ahí.
        assert_eq!(read(&d, "saves/slot1.sav"), b"partida");
        assert!(d.join("portable.txt").is_file());
        assert_eq!(read(&d, "config.ini"), b"mis ajustes");
        assert_eq!(read(&d, "ajustes.json"), b"{\"mio\":1}");
        // Lo que la v2 cambió manda, y se avisa.
        assert_eq!(read(&d, "leeme.txt"), b"leeme 2");
        assert_eq!(names(&up.merge.kept), ["ajustes.json", "config.ini"]);
        assert_eq!(names(&up.merge.replaced), ["Zelda64Recompiled.exe", "leeme.txt"]);
        assert_eq!(up.merge.carried, 4);
        assert!(up.merge.skipped.is_empty());

        // El recibo nuevo lista solo lo que trae el paquete nuevo.
        assert_eq!(up.receipt.version, "v2.0.0");
        assert_eq!(Receipt::read(&d).unwrap().unwrap(), up.receipt);
        assert_eq!(up.receipt.files.len(), 6);
        assert_eq!(up.receipt.check_run(&d).unwrap(), RunState::Intact);

        // La v1 quedó entera, tal como estaba antes de actualizar.
        let prev = previous_versions(&lib, &d).unwrap();
        assert_eq!(prev, vec![up.previous.clone()]);
        assert_eq!(prev[0].receipt.version, "v1.0.0");
        assert!(prev[0].id.ends_with("-v1.0.0"), "{}", prev[0].id);
        assert_eq!(read(&prev[0].path, "leeme.txt"), b"con notas mias");
        assert_eq!(read(&prev[0].path, "Zelda64Recompiled.exe"), b"exe 1.1");
        assert!(prev[0].path.join("vieja.dll").is_file());
        assert_eq!(read(&prev[0].path, "saves/slot1.sav"), b"partida");

        // No queda nada temporal, y no se actualiza dos veces.
        let left: Vec<_> = fs::read_dir(&lib.games).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![OsString::from("zelda-majoras-mask")]);
        assert!(matches!(status(&v2).unwrap(), Status::UpToDate(_)));
        assert!(matches!(update_downloaded(&lib, &v2, &z2, &got("dos")), Err(Error::UpToDate(_))));
    }

    #[test]
    fn vuelve_atras_y_otra_vez_adelante() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = v1_jugada(t.path(), &lib);
        let up = update_downloaded(&lib, &plan_v(&lib, "v2.0.0"), &zip_v2(t.path()), &got("dos")).unwrap();
        // Una partida nueva con la v2.
        fs::write(d.join("saves").join("slot2.sav"), b"con la v2").unwrap();

        let back = roll_back(&lib, &d, &up.previous.id).unwrap();
        assert_eq!(back.receipt.version, "v1.0.0");
        assert_eq!(Receipt::read(&d).unwrap().unwrap().version, "v1.0.0");
        assert_eq!(read(&d, "Zelda64Recompiled.exe"), b"exe 1.1");
        assert_eq!(read(&d, "leeme.txt"), b"con notas mias");
        assert!(!d.join("saves").join("slot2.sav").exists());
        // La v2, con la partida nueva, es ahora la versión anterior.
        let v2 = back.previous.unwrap();
        assert_eq!(v2.receipt.version, "v2.0.0");
        assert_eq!(read(&v2.path, "saves/slot2.sav"), b"con la v2");
        assert_eq!(previous_versions(&lib, &d).unwrap(), vec![v2.clone()]);

        // Y adelante otra vez, sin bajar nada.
        let fwd = roll_back(&lib, &d, &v2.id).unwrap();
        assert_eq!(fwd.receipt.version, "v2.0.0");
        assert_eq!(read(&d, "saves/slot2.sav"), b"con la v2");
        let v1 = fwd.previous.unwrap();
        assert_eq!(v1.receipt.version, "v1.0.0");

        // Ids que no son una versión guardada.
        for bad in ["no-existe", "..", "../games", ".tmp", "a/b", ""] {
            assert!(matches!(roll_back(&lib, &d, bad), Err(Error::NoPrevious { .. })), "{bad}");
        }
        // Borrar una versión anterior es explícito.
        remove_previous(&lib, &d, &v1.id).unwrap();
        assert!(previous_versions(&lib, &d).unwrap().is_empty());
        assert_eq!(Receipt::read(&d).unwrap().unwrap().version, "v2.0.0");
    }

    #[test]
    fn si_falla_no_toca_nada() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = v1_jugada(t.path(), &lib);
        // Una v2 rota: no trae el ejecutable.
        let z = t.path().join("rota.zip");
        zip_with(&z, &[("otro.exe", b"x")]);
        let err = update_downloaded(&lib, &plan_v(&lib, "v2.0.0"), &z, &got("dos")).unwrap_err();
        assert!(matches!(err, Error::RunNotFound { .. }), "{err}");
        assert_eq!(Receipt::read(&d).unwrap().unwrap().version, "v1.0.0");
        assert_eq!(read(&d, "saves/slot1.sav"), b"partida");
        assert!(previous_versions(&lib, &d).unwrap().is_empty());
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 1);

        // Otro juego en esa carpeta, o algo que no instaló Unricopie: no se toca.
        let mut otro = plan_v(&lib, "v2.0.0");
        otro.entry = "recomps/otro".into();
        assert!(matches!(status(&otro), Err(Error::OtherGame { .. })));
        fs::remove_file(d.join(receipt::FILE_NAME)).unwrap();
        assert!(matches!(status(&plan_v(&lib, "v2.0.0")), Err(Error::NoReceipt(_))));
        // Y sin nada instalado, no hay qué actualizar.
        let t2 = tempfile::tempdir().unwrap();
        let lib2 = Library::new(t2.path());
        assert_eq!(status(&plan_v(&lib2, "v2.0.0")).unwrap(), Status::NotInstalled);
    }

    #[test]
    fn una_release_rehecha_con_la_misma_etiqueta_tambien_es_nueva() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        v1_jugada(t.path(), &lib);
        let mut p = plan_v(&lib, "v1.0.0");
        assert!(matches!(status(&p).unwrap(), Status::UpToDate(_)));
        p.sha256 = Some("otra".into());
        assert!(matches!(status(&p).unwrap(), Status::Available(_)));
    }

    /// Con un exe de verdad en marcha (una copia de `ping.exe`), abierto desde fuera de su
    /// carpeta, que es el caso en que Windows sí deja mover la carpeta.
    #[cfg(windows)]
    #[test]
    fn no_actualiza_ni_vuelve_atras_con_el_juego_abierto() {
        struct Kill(std::process::Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = v1_jugada(t.path(), &lib);
        let exe = d.join("Zelda64Recompiled.exe");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        fs::copy(Path::new(&system).join("System32").join("PING.EXE"), &exe).unwrap();
        let current = Receipt::read(&d).unwrap().unwrap();
        assert!(!is_running(&d, &current));

        let mut game = Kill(
            std::process::Command::new(&exe)
                .args(["-n", "30", "127.0.0.1"])
                .current_dir(t.path())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        assert!(is_running(&d, &current));
        let v2 = plan_v(&lib, "v2.0.0");
        let z2 = zip_v2(t.path());
        let err = update_downloaded(&lib, &v2, &z2, &got("dos")).unwrap_err();
        assert!(matches!(err, Error::GameRunning(_)), "{err}");
        assert_eq!(Receipt::read(&d).unwrap().unwrap().version, "v1.0.0");
        assert!(previous_versions(&lib, &d).unwrap().is_empty());

        game.0.kill().unwrap();
        game.0.wait().unwrap();
        assert!(!is_running(&d, &current));
        let up = update_downloaded(&lib, &v2, &z2, &got("dos")).unwrap();

        // Volver atrás tampoco, si la v2 está abierta: la v2 trae otro exe, así que se pone
        // otra vez `ping` en su lugar y se apunta en el recibo.
        fs::copy(Path::new(&system).join("System32").join("PING.EXE"), &exe).unwrap();
        let mut r = Receipt::read(&d).unwrap().unwrap();
        r.run_stamp = Some(Stamp::of(&exe).unwrap());
        r.write(&d).unwrap();
        let _game = Kill(
            std::process::Command::new(&exe)
                .args(["-n", "30", "127.0.0.1"])
                .current_dir(t.path())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let err = roll_back(&lib, &d, &up.previous.id).unwrap_err();
        assert!(matches!(err, Error::GameRunning(_)), "{err}");
        assert_eq!(Receipt::read(&d).unwrap().unwrap().version, "v2.0.0");
    }

    /// De punta a punta con releases de verdad: instala la v1.2.1 de Zelda64Recomp, deja cosas
    /// del jugador, actualiza a la v1.2.2, vuelve atrás y vuelve adelante.
    #[test]
    #[ignore = "usa la red y baja dos zip de Zelda64Recomp (v1.2.1 y v1.2.2, 28,9 MB cada uno)"]
    fn actualiza_zelda64recomp_de_verdad() {
        use crate::install::recipe::{tests::ZELDA, Recipe};
        use crate::install::release::{self, Channel};
        use crate::install::Platform;
        let recipe = Recipe::from_json(ZELDA).unwrap();
        let agent = crate::net::agent();
        let all = release::list(&agent, &recipe.repo).unwrap();
        let old: Vec<_> = all.iter().filter(|r| r.tag_name != "v1.2.2").cloned().collect();
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let plan = |rs: &[release::GhRelease]| {
            installer::plan_from(&recipe, None, Platform::WindowsX64, Channel::Untested, None, rs, &lib).unwrap()
        };

        let p1 = plan(&old);
        assert_eq!(p1.version, "v1.2.1");
        let r1 = installer::install(&agent, &lib, &p1, &mut |_, _| {}).unwrap();
        assert!(r1.sha256_verified);
        eprintln!("v1.2.1: {} archivos en el paquete", r1.files.len());
        let d = p1.dest.clone();
        fs::write(d.join("portable.txt"), b"").unwrap();
        fs::create_dir_all(d.join("saves")).unwrap();
        fs::write(d.join("saves").join("prueba.bin"), b"partida de prueba").unwrap();

        let p2 = plan(&all);
        assert_eq!(p2.version, "v1.2.2");
        assert!(matches!(status(&p2).unwrap(), Status::Available(_)));
        let up = update(&agent, &lib, &p2, &mut |_, _| {}).unwrap();
        eprintln!("v1.2.2: {} archivos; {:?}", up.receipt.files.len(), up.merge);
        assert!(up.receipt.sha256_verified);
        assert_eq!(up.receipt.check_run(&d).unwrap(), RunState::Intact);
        assert_eq!(read(&d, "saves/prueba.bin"), b"partida de prueba");
        assert!(d.join("portable.txt").is_file());
        assert_eq!(up.merge.carried, 2);
        assert!(up.merge.kept.is_empty() && up.merge.replaced.is_empty() && up.merge.skipped.is_empty());
        let changed: Vec<_> = up
            .receipt
            .files
            .iter()
            .filter(|f| !r1.files.iter().any(|o| o.path == f.path && o.stamp() == f.stamp()))
            .map(|f| f.path.as_str())
            .collect();
        eprintln!("cambiaron entre versiones: {changed:?}");

        let back = roll_back(&lib, &d, &up.previous.id).unwrap();
        assert_eq!(back.receipt.version, "v1.2.1");
        assert_eq!(back.receipt.check_run(&d).unwrap(), RunState::Intact);
        assert_eq!(read(&d, "saves/prueba.bin"), b"partida de prueba");
        let fwd = roll_back(&lib, &d, &back.previous.unwrap().id).unwrap();
        assert_eq!(fwd.receipt.version, "v1.2.2");
        assert_eq!(fwd.receipt.check_run(&d).unwrap(), RunState::Intact);
        assert_eq!(previous_versions(&lib, &d).unwrap().len(), 1);
        // No queda nada en descargas ni carpetas temporales.
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&lib.downloads).unwrap().count(), 0);
    }

    #[test]
    fn guarda_y_devuelve_las_partidas_de_fuera() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = v1_jugada(t.path(), &lib);
        let fuera = t.path().join("AppData").join("Local").join("Zelda64Recompiled");
        fs::create_dir_all(fuera.join("saves")).unwrap();
        fs::write(fuera.join("saves").join("1.sav"), b"partida con la v1").unwrap();
        let mut v2 = plan_v(&lib, "v2.0.0");
        v2.data_dirs = vec![fuera.clone()];

        let up = update_downloaded(&lib, &v2, &zip_v2(t.path()), &got("dos")).unwrap();
        assert_eq!(up.previous.data, vec![fuera.clone()]);
        // Actualizar no toca las de verdad.
        assert_eq!(read(&fuera, "saves/1.sav"), b"partida con la v1");
        // La v2 convierte la partida a su formato y crea otra.
        fs::write(fuera.join("saves").join("1.sav"), b"convertida por la v2").unwrap();
        fs::write(fuera.join("saves").join("2.sav"), b"nueva con la v2").unwrap();

        let back = roll_back(&lib, &d, &up.previous.id).unwrap();
        assert_eq!(read(&fuera, "saves/1.sav"), b"partida con la v1");
        assert!(!fuera.join("saves").join("2.sav").exists());
        // Las de la v2 quedan guardadas con la v2, y la copia de la v1 ya sobra.
        let v2prev = back.previous.unwrap();
        assert_eq!(v2prev.data, vec![fuera.clone()]);
        assert!(!data_dir_of(&up.previous.path).exists());

        let fwd = roll_back(&lib, &d, &v2prev.id).unwrap();
        assert_eq!(read(&fuera, "saves/1.sav"), b"convertida por la v2");
        assert_eq!(read(&fuera, "saves/2.sav"), b"nueva con la v2");
        let v1prev = fwd.previous.unwrap();
        assert_eq!(previous_versions(&lib, &d).unwrap(), vec![v1prev.clone()]);
        assert_eq!(v1prev.data, vec![fuera.clone()]);

        remove_previous(&lib, &d, &v1prev.id).unwrap();
        assert_eq!(fs::read_dir(lib.previous.join("zelda-majoras-mask")).unwrap().count(), 0);
        assert_eq!(read(&fuera, "saves/2.sav"), b"nueva con la v2");
    }

    #[test]
    fn nombres_de_carpeta_para_las_versiones() {
        assert_eq!(safe_name("v1.2.2"), "v1.2.2");
        assert_eq!(safe_name("release/2026 octubre"), "release_2026_octubre");
        assert_eq!(safe_name(".."), "version");
        assert!(order("1700000000-v2") > order("999-v9"));
    }
}
