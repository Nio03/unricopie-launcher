//! Buscar las copias del jugador en una carpeta: «mira en mi carpeta de ROM», en vez de elegir
//! la copia de cada juego una por una.
//!
//! Se recorre la carpeta (sin seguir enlaces, hasta cierta profundidad y sin entrar en las
//! carpetas del sistema ni en la biblioteca), cada archivo candidato se lee **una vez**
//! calculando a la vez todos los hashes que alguna receta necesita, y se mira de qué recetas es
//! una copia aceptada. Solo cuenta el hash exacto: por el nombre no se adivina nada. El nombre
//! solo sirve para no leer lo que no puede ser (`hint`): si las recetas que faltan son de N64,
//! una ISO de 4 GB no se lee.
//!
//! Buscar (`scan`) solo lee. Guardar lo encontrado (`import_matches`) usa el `copy::import_with`
//! de siempre, que vuelve a comprobar cada archivo antes de copiarlo a la biblioteca.
//!
//! Lo que esta primera versión no busca, y lo dice en `Scan::skipped`:
//! - las copias que son una carpeta (los archivos del juego original de PC);
//! - las recetas sin hashes: no hay con qué comparar;
//! - las que piden la copia con su propio instalador.
//!
//! Tampoco mira dentro de archivos comprimidos (una ROM en un zip o un 7z no se encuentra).
//!
//! Todo está pensado para llamarlo desde un hilo: las entradas son datos propios (`Wanted`),
//! `cancel` es una bandera compartida que se mira entre archivos y a cada megabyte leído, y
//! `progress` cuenta los bytes leídos del total.
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::copy::{self, CopyCheck, Hashes, Imported, MAX_ROM};
use super::installer::Library;
use super::recipe::{Accepted, Convert, CopyHow, CopyKind, Recipe, What};
use crate::{Error, Result};

/// Una receta cuya copia se busca.
#[derive(Debug, Clone)]
pub struct Wanted {
    pub recipe: Recipe,
    /// El juego, si es una variante de la receta (la ventana todavía no las muestra: `None`).
    pub variant: Option<String>,
    /// Las copias de `copy.accepts-from`, ya leídas con `copy::fetch_accepts`. Se suman a las
    /// de la receta.
    pub extra: Vec<Accepted>,
    /// La receta tiene `accepts-from` y su lista no se pudo leer (o no se pidió): por qué.
    pub list_error: Option<String>,
}

impl Wanted {
    /// Sin leer nada de la red: solo con los hashes de la receta.
    pub fn new(recipe: Recipe) -> Wanted {
        Wanted { recipe, variant: None, extra: Vec::new(), list_error: None }
    }
}

/// Prepara las recetas para buscar sus copias. Si una publica su lista de copias aceptadas
/// (`accepts-from`, como Ship of Harkinian), se lee con `agent`; si no se puede leer, o no hay
/// `agent`, queda apuntado en `list_error` y se busca solo con lo que traiga la receta. No se
/// pide la lista de las que no se van a buscar (ya tienen copia comprobada, no la necesitan o
/// es una carpeta).
pub fn wanted(agent: Option<&ureq::Agent>, library: &Library, recipes: impl IntoIterator<Item = Recipe>) -> Vec<Wanted> {
    recipes
        .into_iter()
        .map(|recipe| {
            let mut w = Wanted::new(recipe);
            let rule = w.recipe.copy_rule(None);
            let Some(from) = rule.accepts_from.clone() else { return w };
            if !matches!(classify(library, &w), Class::Search(_) | Class::Skip(Why::NoHashes)) {
                return w;
            }
            match agent.map(|a| copy::fetch_accepts(a, &from)) {
                Some(Ok(list)) => w.extra = list,
                Some(Err(e)) => w.list_error = Some(e.to_string()),
                None => w.list_error = Some("sin red".into()),
            }
            w
        })
        .collect()
}

/// Los límites de la búsqueda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Cuántas carpetas hacia dentro se entra: con 0, solo los archivos de la propia carpeta.
    pub max_depth: usize,
    /// El archivo más grande que se lee cuando la plataforma de la copia no dice nada de su
    /// tamaño (las que sí, como N64 o GameCube, tienen su propio tope).
    pub max_file: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options { max_depth: 6, max_file: 16 << 30 }
    }
}

/// En qué va.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Recorriendo la carpeta: `files_done` son los candidatos vistos y `file`, la carpeta.
    Listing,
    /// Leyendo los candidatos: `bytes_done` de `bytes_total`.
    Hashing,
    /// Guardando lo encontrado (`import_matches`).
    Importing,
}

/// Cómo va, para la barra de progreso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress<'a> {
    pub phase: Phase,
    pub files_done: usize,
    /// 0 mientras se recorre la carpeta: todavía no se sabe.
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// El archivo (o la carpeta) con el que está.
    pub file: &'a Path,
}

/// Un archivo de la carpeta que es una copia aceptada de una receta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// La ficha de la receta (`recomps/zelda-majoras-mask`).
    pub entry: String,
    pub variant: Option<String>,
    /// El nombre de la copia aceptada con la que coincide (`Accepted::name`).
    pub accepted: String,
    pub path: PathBuf,
    pub size: u64,
    /// Coincide con los hashes de la receta. En esta versión siempre es `true`: lo que no
    /// coincide no se devuelve.
    pub verified: bool,
}

/// Por qué no se buscó la copia de una receta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Ya hay una copia comprobada en la biblioteca: la copia aceptada con la que coincide.
    AlreadyImported(String),
    /// La copia es una carpeta (los archivos del juego original): se elige a mano.
    Folder,
    /// La receta no trae hashes de archivos con los que comparar.
    NoHashes,
    /// No trae hashes propios y la lista que publica el proyecto no se pudo leer: el motivo.
    ListUnavailable(String),
    /// La pide su propio instalador.
    OwnInstaller,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub entry: String,
    pub variant: Option<String>,
    pub why: Why,
}

/// Lo que dio la búsqueda.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Lo encontrado: por receta (en el orden de `wanted`), primero la copia que la receta pone
    /// antes y, a igualdad, por ruta. Una receta puede salir varias veces (dos copias que
    /// acepta, o el mismo archivo repetido) y un archivo, en varias recetas.
    pub matches: Vec<Match>,
    /// Las recetas que no se buscaron y por qué. Las que no necesitan copia no salen.
    pub skipped: Vec<Skipped>,
    /// Cuántas recetas se buscaron.
    pub searched: usize,
    /// Los archivos candidatos y lo que pesan entre todos.
    pub files: usize,
    pub bytes: u64,
    /// Cuántos se llegaron a leer.
    pub hashed: usize,
    /// Carpetas y archivos que no se pudieron leer (sin permiso, en uso).
    pub unreadable: Vec<PathBuf>,
    /// Se cortó con `cancel`: lo de arriba es lo que dio tiempo a ver.
    pub cancelled: bool,
}

/// Qué se hace con una receta.
enum Class {
    Search(Searched),
    Skip(Why),
    /// No necesita copia: ni se busca ni se cuenta.
    NoCopy,
}

/// Una receta que se busca: las copias con las que se compara.
struct Searched {
    /// Solo las que son un archivo entero, son el juego y traen algún hash.
    accepts: Vec<Accepted>,
    /// Los hashes de la receta son de la ROM ya convertida a .z64.
    convert: bool,
}

fn classify(library: &Library, w: &Wanted) -> Class {
    let variant = w.variant.as_deref();
    let rule = w.recipe.copy_rule(variant);
    match rule.how {
        CopyHow::None => return Class::NoCopy,
        CopyHow::OwnInstaller => return Class::Skip(Why::OwnInstaller),
        _ => {}
    }
    if let Some(i) = copy::imported(library, &w.recipe, variant).filter(|i| i.verified) {
        return Class::Skip(Why::AlreadyImported(i.accepted.first().cloned().unwrap_or_default()));
    }
    if rule.kind == Some(CopyKind::Folder) {
        return Class::Skip(Why::Folder);
    }
    let all = || rule.accepts.iter().chain(&w.extra);
    let accepts: Vec<Accepted> = all().filter(|a| a.file.is_none() && a.what == What::Game && !a.hash.is_empty()).cloned().collect();
    if accepts.is_empty() {
        // Todas sus copias son archivos de dentro de una carpeta: es una carpeta, lo diga o no.
        let folder = all().next().is_some() && all().all(|a| a.file.is_some());
        return Class::Skip(match &w.list_error {
            _ if folder => Why::Folder,
            Some(e) => Why::ListUnavailable(e.clone()),
            None => Why::NoHashes,
        });
    }
    Class::Search(Searched { accepts, convert: rule.convert == Some(Convert::N64Z64) })
}

/// Qué archivos pueden ser la copia de una plataforma: sus extensiones (vacío: cualquiera) y
/// lo máximo que pesan. Es solo para no leer de más; lo que decide es el hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Hint {
    exts: &'static [&'static str],
    max: u64,
}

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

/// Las pistas de cada plataforma de las recetas (`Accepted::platform`). Las que no están aquí
/// (pc, dos, ps4… o sin plataforma) no dan pista: cualquier archivo hasta `Options::max_file`.
fn hint(platform: Option<&str>, opts: &Options) -> Hint {
    let (exts, max): (&'static [&'static str], u64) = match platform.unwrap_or("") {
        "n64" => (&["z64", "n64", "v64", "rom", "bin"], MAX_ROM),
        "nes" => (&["nes", "fds", "bin"], 64 * MB),
        "snes" => (&["sfc", "smc", "bin"], 64 * MB),
        "gb" | "gbc" => (&["gb", "gbc", "sgb", "bin"], 64 * MB),
        "gba" => (&["gba", "agb", "bin"], 128 * MB),
        "nds" => (&["nds"], GB),
        "3ds" => (&["3ds", "cci", "cia"], 8 * GB),
        "gamecube" | "gc" => (&["iso", "gcm", "ciso", "gcz", "rvz"], 2 * GB),
        "wii" => (&["iso", "wbfs", "ciso", "gcz", "rvz"], 9 * GB),
        "ps1" => (&["bin", "iso", "img", "chd"], GB),
        "ps2" => (&["iso", "bin", "chd"], 9 * GB),
        "psp" => (&["iso", "cso"], 2 * GB),
        "xbox" | "x360" => (&["iso", "xiso"], 9 * GB),
        "switch" => (&["xci", "nsp"], 64 * GB),
        _ => (&[], opts.max_file),
    };
    Hint { exts, max }
}

fn fits(hints: &BTreeSet<Hint>, ext: &str, size: u64) -> bool {
    hints.iter().any(|h| size <= h.max && (h.exts.is_empty() || h.exts.contains(&ext)))
}

/// Carpetas en las que no se entra: las del sistema y las que nunca guardan copias.
fn skip_dir(name: &str, path: &Path) -> bool {
    const NAMES: &[&str] = &["system volume information", "node_modules", "__macosx", "lost+found", "program files", "program files (x86)", "programdata", "appdata"];
    let lower = name.to_lowercase();
    name.starts_with('.') || name.starts_with('$') || NAMES.contains(&lower.as_str())
        // La carpeta de Windows, se llame como se llame (hay quien guarda sus juegos de PC en
        // una carpeta «Windows»: esa no tiene System32).
        || (lower == "windows" && path.join("System32").is_dir())
}

/// Las rutas como las compara el disco, para no entrar en la biblioteca.
fn key(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Lo que se va juntando al recorrer la carpeta.
struct Walk<'a> {
    opts: &'a Options,
    raw: &'a BTreeSet<Hint>,
    conv: &'a BTreeSet<Hint>,
    /// La biblioteca: sus copias y sus juegos no son «la carpeta del jugador».
    library: Vec<String>,
    cancel: &'a AtomicBool,
    files: Vec<(PathBuf, u64)>,
    unreadable: Vec<PathBuf>,
}

impl Walk<'_> {
    /// `false` si se cortó.
    fn dir(&mut self, dir: &Path, depth: usize, progress: &mut dyn FnMut(Progress<'_>)) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        progress(Progress { phase: Phase::Listing, files_done: self.files.len(), files_total: 0, bytes_done: 0, bytes_total: 0, file: dir });
        let Ok(entries) = fs::read_dir(dir) else {
            self.unreadable.push(dir.to_path_buf());
            return true;
        };
        let mut dirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            // `file_type` no sigue los enlaces: ni los simbólicos ni las uniones de Windows.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if depth < self.opts.max_depth && !skip_dir(&name, &path) && !self.library.contains(&key(&path)) {
                    dirs.push(path);
                }
            } else if kind.is_file() {
                let Ok(size) = entry.metadata().map(|m| m.len()) else { continue };
                let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if size > 0 && (fits(self.raw, &ext, size) || fits(self.conv, &ext, size)) {
                    self.files.push((path, size));
                }
            }
        }
        dirs.sort();
        dirs.iter().all(|d| self.dir(d, depth + 1, progress))
    }
}

/// Los hashes de un archivo: los suyos y, si es una ROM de N64, los de su forma .z64.
#[derive(Default)]
struct Read1 {
    raw: Option<Hashes>,
    converted: Option<Hashes>,
}

fn is_n64(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok()
        && matches!(magic, [0x80, 0x37, 0x12, 0x40] | [0x40, 0x12, 0x37, 0x80] | [0x37, 0x80, 0x40, 0x12])
}

/// Busca en `folder` las copias de las recetas de `wanted` (ver el módulo). Solo lee.
///
/// `progress` se llama a cada carpeta que se recorre y, al leer, a cada megabyte. `cancel` se
/// puede poner a `true` desde otro hilo: se deja de leer enseguida y vuelve lo que dio tiempo a
/// encontrar, con `cancelled`. El error es solo que `folder` no se pueda abrir.
pub fn scan(
    library: &Library,
    wanted: &[Wanted],
    folder: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress<'_>),
) -> Result<Scan> {
    let mut out = Scan::default();
    let mut searched: Vec<(usize, Searched)> = Vec::new();
    for (i, w) in wanted.iter().enumerate() {
        match classify(library, w) {
            Class::Search(s) => searched.push((i, s)),
            Class::Skip(why) => out.skipped.push(Skipped { entry: w.recipe.entry.clone(), variant: w.variant.clone(), why }),
            Class::NoCopy => {}
        }
    }
    out.searched = searched.len();
    // Abrirla antes de nada: si no es una carpeta que se pueda leer, es un error y no «nada».
    fs::read_dir(folder).map_err(|e| Error::io(folder, e))?;
    if searched.is_empty() {
        return Ok(out);
    }
    // Qué se calcula y qué archivos pueden ser, para las recetas que comparan el archivo tal
    // cual y para las que comparan la ROM convertida.
    let (mut raw_algos, mut conv_algos) = (Vec::new(), Vec::new());
    let (mut raw_hints, mut conv_hints) = (BTreeSet::new(), BTreeSet::new());
    for (_, s) in &searched {
        let (algos, hints) = if s.convert { (&mut conv_algos, &mut conv_hints) } else { (&mut raw_algos, &mut raw_hints) };
        algos.extend(copy::algos_for(&s.accepts));
        for a in &s.accepts {
            let mut h = hint(a.platform.as_deref(), opts);
            if s.convert {
                h.max = h.max.min(MAX_ROM);
            }
            hints.insert(h);
        }
    }
    for algos in [&mut raw_algos, &mut conv_algos] {
        algos.sort();
        algos.dedup();
    }

    let mut walk = Walk {
        opts,
        raw: &raw_hints,
        conv: &conv_hints,
        library: [&library.games, &library.downloads, &library.previous, &library.copies].iter().map(|p| key(p)).collect(),
        cancel,
        files: Vec::new(),
        unreadable: Vec::new(),
    };
    let whole = walk.dir(folder, 0, progress);
    let Walk { mut files, unreadable, .. } = walk;
    out.unreadable = unreadable;
    files.sort();
    out.files = files.len();
    out.bytes = files.iter().map(|(_, s)| s).sum();
    if !whole {
        out.cancelled = true;
        return Ok(out);
    }

    let mut found: Vec<((usize, usize), Match)> = Vec::new();
    let total = out.bytes;
    let mut before = 0u64;
    for (n, (path, size)) in files.iter().enumerate() {
        let (path, size) = (path.as_path(), *size);
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let mut at = |done: u64| {
            progress(Progress { phase: Phase::Hashing, files_done: n, files_total: files.len(), bytes_done: before + done.min(size), bytes_total: total, file: path });
            !cancel.load(Ordering::Relaxed)
        };
        at(0);
        let want_raw = !raw_algos.is_empty() && fits(&raw_hints, &ext, size);
        let want_conv = !conv_algos.is_empty() && fits(&conv_hints, &ext, size) && is_n64(path);
        let read: Result<Option<Read1>> = if want_conv {
            // Una ROM de N64 es pequeña: se lee entera, una vez, y de ahí salen las dos cosas.
            fs::read(path).map_err(|e| Error::io(path, e)).map(|data| {
                let raw = want_raw.then(|| copy::hash_bytes(&data, &raw_algos));
                let converted = copy::n64_to_z64(data).map(|z64| copy::hash_bytes(&z64, &conv_algos));
                Some(Read1 { raw, converted })
            })
        } else if want_raw {
            copy::hash_copy_while(path, &raw_algos, &mut at).map(|h| h.map(|raw| Read1 { raw: Some(raw), converted: None }))
        } else {
            Ok(Some(Read1::default()))
        };
        before += size;
        let read = match read {
            Ok(Some(r)) => r,
            Ok(None) => {
                out.cancelled = true;
                break;
            }
            // Un archivo que no se deja leer no para la búsqueda.
            Err(_) => {
                out.unreadable.push(path.to_path_buf());
                continue;
            }
        };
        out.hashed += 1;
        for (i, s) in &searched {
            let Some(hashes) = (if s.convert { &read.converted } else { &read.raw }) else { continue };
            if let CopyCheck::Matches(a) = copy::identify(&s.accepts, hashes) {
                let rank = s.accepts.iter().position(|x| std::ptr::eq(x, a)).unwrap_or(0);
                let w = &wanted[*i];
                found.push((
                    (*i, rank),
                    Match { entry: w.recipe.entry.clone(), variant: w.variant.clone(), accepted: a.name.clone(), path: path.to_path_buf(), size, verified: true },
                ));
            }
        }
    }
    if !out.cancelled {
        progress(Progress { phase: Phase::Hashing, files_done: files.len(), files_total: files.len(), bytes_done: out.bytes, bytes_total: out.bytes, file: folder });
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    out.matches = found.into_iter().map(|(_, m)| m).collect();
    Ok(out)
}

/// Qué pasó con un archivo encontrado al guardarlo.
#[derive(Debug)]
pub enum Outcome {
    /// Comprobado otra vez y guardado en la biblioteca.
    Imported(Imported),
    /// La biblioteca ya tenía una copia comprobada de ese juego: no se toca.
    AlreadyImported,
    /// Otro archivo de esta misma tanda ya quedó guardado como la copia de ese juego.
    Duplicate,
    /// No se pudo guardar (el archivo cambió desde que se buscó, no hay espacio…).
    Failed(Error),
}

#[derive(Debug)]
pub struct ImportResult {
    pub found: Match,
    pub outcome: Outcome,
}

/// Lo que dio guardar lo encontrado.
#[derive(Debug, Default)]
pub struct Imports {
    /// Un resultado por cada archivo de `matches` al que se llegó, en su orden.
    pub results: Vec<ImportResult>,
    /// Se cortó con `cancel`: los que faltan en `results` no se tocaron.
    pub cancelled: bool,
}

/// Guarda en la biblioteca lo que encontró `scan` (todo `Scan::matches`, o los que elija la
/// persona), con `copy::import_with`: cada archivo se vuelve a comprobar antes de copiarlo. Por
/// juego se guarda una sola copia, la primera de `matches` que se pueda; las demás quedan como
/// `Duplicate`. Si la biblioteca ya tiene una copia comprobada de un juego, no se sustituye.
///
/// `cancel` se mira entre un archivo y otro: el que se está copiando termina (o no queda nada
/// suyo). `progress` cuenta los bytes comprobados del total de `matches`.
pub fn import_matches(library: &Library, wanted: &[Wanted], matches: &[Match], cancel: &AtomicBool, progress: &mut dyn FnMut(Progress<'_>)) -> Imports {
    let mut out = Imports::default();
    let total: u64 = matches.iter().map(|m| m.size).sum();
    let mut before = 0u64;
    let mut done: Vec<(&str, Option<&str>)> = Vec::new();
    for (n, m) in matches.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let this = (m.entry.as_str(), m.variant.as_deref());
        let mut at = |bytes: u64| {
            progress(Progress { phase: Phase::Importing, files_done: n, files_total: matches.len(), bytes_done: before + bytes.min(m.size), bytes_total: total, file: &m.path })
        };
        at(0);
        let outcome = if done.contains(&this) {
            Outcome::Duplicate
        } else {
            match wanted.iter().find(|w| w.recipe.entry == m.entry && w.variant == m.variant) {
                None => Outcome::Failed(Error::BadRecipe(format!("no está la receta de {}", m.entry))),
                Some(w) if copy::imported(library, &w.recipe, this.1).is_some_and(|i| i.verified) => {
                    done.push(this);
                    Outcome::AlreadyImported
                }
                Some(w) => match copy::import_with(library, &w.recipe, this.1, &m.path, &w.extra, false, &mut at) {
                    Ok(i) => {
                        done.push(this);
                        Outcome::Imported(i)
                    }
                    Err(e) => Outcome::Failed(e),
                },
            }
        };
        before += m.size;
        out.results.push(ImportResult { found: m.clone(), outcome });
    }
    if !out.cancelled {
        progress(Progress { phase: Phase::Importing, files_done: matches.len(), files_total: matches.len(), bytes_done: total, bytes_total: total, file: Path::new("") });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::recipe::HashAlgo;
    use serde_json::json;
    use std::cell::RefCell;

    const HOST: &str = "windows-x64";

    fn recipe(entry: &str, copy: serde_json::Value) -> Recipe {
        let r = json!({
            "entry": entry,
            "repo": "prueba/prueba",
            "release": { "assets": { HOST: "x-*.zip" }, "run": { HOST: "x.exe" } },
            "copy": copy,
            "checked": { "date": "2026-11-20", "release": "v1", "sources": ["prueba"] }
        });
        Recipe::from_json(&r.to_string()).unwrap()
    }

    /// Una copia aceptada con los hashes de verdad de `data`.
    fn accepted(name: &str, platform: &str, data: &[u8], algos: &[HashAlgo]) -> serde_json::Value {
        let hash: serde_json::Map<String, serde_json::Value> = copy::hash_bytes(data, algos)
            .into_iter()
            .map(|(k, v)| (serde_json::to_value(k).unwrap().as_str().unwrap().to_string(), v.into()))
            .collect();
        json!({ "name": name, "platform": platform, "hash": hash })
    }

    fn file_recipe(entry: &str, platform: &str, data: &[u8]) -> Recipe {
        recipe(entry, json!({ "how": "in-app", "kind": "file", "accepts": [accepted("La buena", platform, data, &[HashAlgo::Sha1, HashAlgo::Md5])] }))
    }

    fn write(path: &Path, data: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn all(recipes: Vec<Recipe>) -> Vec<Wanted> {
        recipes.into_iter().map(Wanted::new).collect()
    }

    fn run(lib: &Library, wanted: &[Wanted], folder: &Path) -> Scan {
        scan(lib, wanted, folder, &Options::default(), &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    const GBA: &[u8] = b"un cartucho de GBA de mentira";
    const ISO: &[u8] = b"un disco de GameCube de mentira, algo mas largo";

    #[test]
    fn encuentra_las_copias_entre_senuelos_y_solo_lee() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        write(&roms.join("GBA").join("Europa").join("juego (E).gba"), GBA);
        write(&roms.join("GameCube").join("disco.iso"), ISO);
        // Señuelos: el nombre «bueno» con otro contenido, un byte de más, un archivo vacío y el
        // contenido bueno donde no puede estar (un .txt no es un cartucho de GBA).
        write(&roms.join("GBA").join("juego.gba"), b"otra cosa");
        write(&roms.join("GBA").join("casi.gba"), &[GBA, b"!"].concat());
        write(&roms.join("GBA").join("vacio.gba"), b"");
        write(&roms.join("notas").join("juego.txt"), GBA);
        let wanted = all(vec![
            file_recipe("decomps/juego-gba", "gba", GBA),
            file_recipe("ports/juego-gc", "gamecube", ISO),
            file_recipe("ports/no-esta", "gba", b"esta copia no esta en la carpeta"),
            recipe("ports/sin-copia", json!({ "how": "none" })),
        ]);
        let mut fases = Vec::new();
        let mut ultimo = (0, 0);
        let s = scan(&lib, &wanted, &roms, &Options::default(), &AtomicBool::new(false), &mut |p| {
            if fases.last() != Some(&p.phase) {
                fases.push(p.phase);
            }
            if p.phase == Phase::Hashing {
                assert!(p.bytes_done >= ultimo.0 && p.bytes_done <= p.bytes_total, "{p:?}");
                ultimo = (p.bytes_done, p.bytes_total);
            }
        })
        .unwrap();
        assert_eq!(fases, [Phase::Listing, Phase::Hashing]);
        assert_eq!(s.matches.len(), 2, "{:?}", s.matches);
        assert_eq!(
            s.matches[0],
            Match { entry: "decomps/juego-gba".into(), variant: None, accepted: "La buena".into(), path: roms.join("GBA").join("Europa").join("juego (E).gba"), size: GBA.len() as u64, verified: true }
        );
        assert_eq!((s.matches[1].entry.as_str(), s.matches[1].path.clone()), ("ports/juego-gc", roms.join("GameCube").join("disco.iso")));
        // Candidatos: los tres .gba con algo dentro y la .iso. Ni el vacío ni el .txt.
        assert_eq!((s.searched, s.files, s.hashed), (3, 4, 4));
        assert_eq!(s.bytes, (GBA.len() * 2 + 1 + 9 + ISO.len()) as u64);
        assert_eq!(ultimo, (s.bytes, s.bytes));
        assert!(s.skipped.is_empty() && s.unreadable.is_empty() && !s.cancelled);
        // Buscar no escribe nada: la biblioteca ni existe.
        assert!(!t.path().join("biblioteca").exists());
    }

    #[test]
    fn guarda_lo_encontrado_y_no_lo_repite() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        write(&roms.join("a").join("juego.gba"), GBA);
        // El mismo cartucho otra vez, en otra carpeta.
        write(&roms.join("b").join("repetido.gba"), GBA);
        // Dos proyectos aceptan el mismo cartucho.
        let wanted = all(vec![file_recipe("decomps/juego-gba", "gba", GBA), file_recipe("ports/otro-port", "gba", GBA)]);
        let s = run(&lib, &wanted, &roms);
        assert_eq!(s.matches.iter().map(|m| (m.entry.as_str(), m.path.clone())).collect::<Vec<_>>(), [
            ("decomps/juego-gba", roms.join("a").join("juego.gba")),
            ("decomps/juego-gba", roms.join("b").join("repetido.gba")),
            ("ports/otro-port", roms.join("a").join("juego.gba")),
            ("ports/otro-port", roms.join("b").join("repetido.gba")),
        ]);
        // Cada archivo se leyó una vez, aunque sirva para dos recetas.
        assert_eq!((s.files, s.hashed), (2, 2));

        let mut fin = (0, 0);
        let i = import_matches(&lib, &wanted, &s.matches, &AtomicBool::new(false), &mut |p| {
            assert_eq!(p.phase, Phase::Importing);
            fin = (p.bytes_done, p.bytes_total);
        });
        assert!(!i.cancelled);
        assert_eq!(fin, (GBA.len() as u64 * 4, GBA.len() as u64 * 4));
        let que: Vec<&str> = i
            .results
            .iter()
            .map(|r| match &r.outcome {
                Outcome::Imported(_) => "guardada",
                Outcome::AlreadyImported => "ya estaba",
                Outcome::Duplicate => "repetida",
                Outcome::Failed(_) => "fallo",
            })
            .collect();
        assert_eq!(que, ["guardada", "repetida", "guardada", "repetida"]);
        let Outcome::Imported(primera) = &i.results[0].outcome else { panic!() };
        assert!(primera.verified);
        assert_eq!(primera.accepted, ["La buena"]);
        for w in &wanted {
            let imp = copy::imported(&lib, &w.recipe, None).unwrap();
            assert!(imp.verified);
            assert_eq!(imp.source, roms.join("a").join("juego.gba"));
            assert_eq!(fs::read(&imp.path).unwrap(), GBA);
        }
        // La carpeta del jugador sigue como estaba.
        assert_eq!(fs::read(roms.join("b").join("repetido.gba")).unwrap(), GBA);

        // Otra búsqueda: ya tienen copia comprobada, así que no se lee ni un archivo.
        let otra = run(&lib, &wanted, &roms);
        assert!(otra.matches.is_empty());
        assert_eq!((otra.searched, otra.files, otra.hashed), (0, 0, 0));
        assert_eq!(otra.skipped.len(), 2);
        assert!(otra.skipped.iter().all(|k| k.why == Why::AlreadyImported("La buena".into())));
        // Y guardar otra vez lo de la primera búsqueda no sustituye lo guardado.
        let antes = copy::imported(&lib, &wanted[0].recipe, None).unwrap().imported_at;
        let i = import_matches(&lib, &wanted, &s.matches, &AtomicBool::new(false), &mut |_| {});
        assert!(matches!(i.results[0].outcome, Outcome::AlreadyImported));
        assert!(matches!(i.results[1].outcome, Outcome::Duplicate));
        assert!(matches!(i.results[2].outcome, Outcome::AlreadyImported));
        assert_eq!(copy::imported(&lib, &wanted[0].recipe, None).unwrap().imported_at, antes);
    }

    #[test]
    fn un_archivo_que_cambio_no_se_guarda() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        write(&roms.join("juego.gba"), GBA);
        write(&roms.join("z").join("juego.gba"), GBA);
        let wanted = all(vec![file_recipe("decomps/juego-gba", "gba", GBA)]);
        let s = run(&lib, &wanted, &roms);
        assert_eq!(s.matches.len(), 2);
        // Entre buscar y guardar, el primero cambia: se vuelve a comprobar, falla y vale el otro.
        write(&roms.join("juego.gba"), b"ya no es el mismo");
        let i = import_matches(&lib, &wanted, &s.matches, &AtomicBool::new(false), &mut |_| {});
        assert!(matches!(&i.results[0].outcome, Outcome::Failed(Error::CopyMismatch(e)) if e == "decomps/juego-gba"));
        assert!(matches!(i.results[1].outcome, Outcome::Imported(_)));
        assert_eq!(copy::imported(&lib, &wanted[0].recipe, None).unwrap().source, roms.join("z").join("juego.gba"));
    }

    /// Una ROM de N64 de mentira en orden .z64, y la misma en .v64 (de 2 en 2 bytes).
    fn rom_n64() -> (Vec<u8>, Vec<u8>) {
        let mut z64 = vec![0x80, 0x37, 0x12, 0x40];
        z64.extend((0..60u8).map(|i| i.wrapping_mul(7)));
        let v64 = z64.chunks(2).flat_map(|c| [c[1], c[0]]).collect();
        (z64, v64)
    }

    #[test]
    fn una_rom_de_n64_se_compara_ya_convertida() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        let (z64, v64) = rom_n64();
        write(&roms.join("N64").join("juego.v64"), &v64);
        // Empieza como una ROM pero es otra; y una que no empieza como una ROM.
        let mut otra = z64.clone();
        otra[10] ^= 0xff;
        write(&roms.join("N64").join("otra.z64"), &otra);
        write(&roms.join("N64").join("nada.n64"), b"esto no es una ROM de N64");
        let convertida = recipe("recomps/juego-n64", json!({ "how": "in-app", "kind": "file", "convert": "n64-z64", "accepts": [accepted("EE. UU.", "n64", &z64, &[HashAlgo::Xxh3_64])] }));
        // Otra receta quiere el archivo tal cual, en .v64, con otro algoritmo.
        let tal_cual = recipe("ports/juego-n64-sin-convertir", json!({ "how": "folder", "folder": ".", "accepts": [accepted("Volcado v64", "n64", &v64, &[HashAlgo::Sha256])] }));
        let wanted = all(vec![convertida, tal_cual]);
        let s = run(&lib, &wanted, &roms);
        assert_eq!(s.matches.iter().map(|m| (m.entry.as_str(), m.accepted.as_str(), m.path.clone())).collect::<Vec<_>>(), [
            ("recomps/juego-n64", "EE. UU.", roms.join("N64").join("juego.v64")),
            ("ports/juego-n64-sin-convertir", "Volcado v64", roms.join("N64").join("juego.v64")),
        ]);
        assert_eq!((s.files, s.hashed), (3, 3));
        let i = import_matches(&lib, &wanted, &s.matches, &AtomicBool::new(false), &mut |_| {});
        assert!(i.results.iter().all(|r| matches!(r.outcome, Outcome::Imported(_))), "{:?}", i.results);
        // Guardada ya convertida, como hace `import`.
        let imp = copy::imported(&lib, &wanted[0].recipe, None).unwrap();
        assert_eq!(imp.file.as_deref(), Some("juego.z64"));
        assert_eq!(fs::read(&imp.path).unwrap(), z64);
        assert_eq!(fs::read(copy::imported(&lib, &wanted[1].recipe, None).unwrap().path).unwrap(), v64);
    }

    #[test]
    fn se_puede_cancelar() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        for n in 0..5 {
            write(&roms.join(format!("{n}")).join("juego.gba"), if n == 4 { GBA } else { b"otro" });
        }
        let wanted = all(vec![file_recipe("decomps/juego-gba", "gba", GBA)]);
        // Antes de empezar: no recorre nada.
        let s = scan(&lib, &wanted, &roms, &Options::default(), &AtomicBool::new(true), &mut |_| panic!("no tenía que empezar")).unwrap();
        assert!(s.cancelled && s.matches.is_empty() && s.files == 0);
        // A mitad de recorrer la carpeta.
        let cancel = AtomicBool::new(false);
        let s = scan(&lib, &wanted, &roms, &Options::default(), &cancel, &mut |p| {
            assert_eq!(p.phase, Phase::Listing);
            if p.files_done == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(s.cancelled && s.hashed == 0 && s.matches.is_empty());
        // A mitad de leer: el quinto, que es el bueno, no se llega a leer.
        let cancel = AtomicBool::new(false);
        let s = scan(&lib, &wanted, &roms, &Options::default(), &cancel, &mut |p| {
            if p.phase == Phase::Hashing && p.files_done == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(s.cancelled && s.matches.is_empty());
        assert!(s.files == 5 && s.hashed < 5, "{s:?}");
        // Sin cancelar, está.
        let s = run(&lib, &wanted, &roms);
        assert_eq!((s.matches.len(), s.cancelled), (1, false));
        // Guardar también se corta: entre un archivo y otro.
        let cancel = AtomicBool::new(true);
        let i = import_matches(&lib, &wanted, &s.matches, &cancel, &mut |_| {});
        assert!(i.cancelled && i.results.is_empty());
        assert!(copy::imported(&lib, &wanted[0].recipe, None).is_none());
    }

    #[test]
    fn dice_lo_que_no_busca() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        write(&roms.join("juego.gba"), GBA);
        let con_lista = |entry: &str| recipe(entry, json!({ "how": "in-app", "kind": "file", "accepts-from": { "url": "https://example.invalid/lista.json", "format": "lista de {name, sha1}" } }));
        let mut sin_leer = Wanted::new(con_lista("ports/lista-sin-leer"));
        sin_leer.list_error = Some("sin red".into());
        // La lista ya leída (lo que hace `wanted` con red) sí se busca.
        let mut leida = Wanted::new(con_lista("ports/lista-leida"));
        leida.extra = vec![serde_json::from_value(accepted("De la lista", "gba", GBA, &[HashAlgo::Sha1])).unwrap()];
        let mut wanted = all(vec![
            recipe("ports/carpeta", json!({ "how": "folder", "folder": "data", "kind": "folder", "accepts": [{ "name": "GOG", "platform": "pc" }] })),
            recipe("ports/carpeta-sin-decirlo", json!({ "how": "in-app", "accepts": [{ "name": "pak0", "file": "base/pak0.pk4", "hash": { "md5": "00" } }] })),
            recipe("recomps/sin-hashes", json!({ "how": "in-app", "accepts": [{ "name": "Xbox 360", "platform": "x360" }] })),
            recipe("ports/instalador", json!({ "how": "own-installer" })),
            recipe("ports/sin-copia", json!({ "how": "none" })),
            // Solo trae el hash de una actualización: eso no es el juego.
            recipe("recomps/solo-parche", json!({ "how": "in-app", "accepts": [{ "name": "TU", "what": "title-update", "hash": { "sha1": "00" } }] })),
        ]);
        wanted.push(sin_leer);
        wanted.push(leida);
        let s = run(&lib, &wanted, &roms);
        let por_que: Vec<(&str, &Why)> = s.skipped.iter().map(|k| (k.entry.as_str(), &k.why)).collect();
        assert_eq!(por_que, [
            ("ports/carpeta", &Why::Folder),
            ("ports/carpeta-sin-decirlo", &Why::Folder),
            ("recomps/sin-hashes", &Why::NoHashes),
            ("ports/instalador", &Why::OwnInstaller),
            ("recomps/solo-parche", &Why::NoHashes),
            ("ports/lista-sin-leer", &Why::ListUnavailable("sin red".into())),
        ]);
        assert_eq!(s.searched, 1);
        assert_eq!(s.matches.iter().map(|m| (m.entry.as_str(), m.accepted.as_str())).collect::<Vec<_>>(), [("ports/lista-leida", "De la lista")]);
        // `wanted` sin red: apunta que la lista no se leyó, y solo en las que la necesitan.
        let ws = super::wanted(None, &lib, vec![con_lista("ports/lista-sin-leer"), file_recipe("decomps/juego-gba", "gba", GBA)]);
        assert_eq!(ws.iter().map(|w| w.list_error.as_deref()).collect::<Vec<_>>(), [Some("sin red"), None]);
        // Una carpeta que no existe es un error, no «nada encontrado».
        assert!(matches!(scan(&lib, &wanted, &t.path().join("no-existe"), &Options::default(), &AtomicBool::new(false), &mut |_| {}), Err(Error::Io { .. })));
        assert!(matches!(scan(&lib, &wanted, &roms.join("juego.gba"), &Options::default(), &AtomicBool::new(false), &mut |_| {}), Err(Error::Io { .. })));
    }

    #[test]
    fn respeta_los_limites() {
        let t = tempfile::tempdir().unwrap();
        // La biblioteca dentro de la carpeta que se busca (modo portable, o quien busca en C:).
        let roms = t.path().join("roms");
        let lib = Library::new(roms.join("Unricopie"));
        let wanted = all(vec![file_recipe("decomps/juego-gba", "gba", GBA)]);
        let hondo = roms.join("1").join("2").join("3");
        write(&hondo.join("juego.gba"), GBA);
        for dentro in ["$RECYCLE.BIN", ".oculta", "System Volume Information", "node_modules"] {
            write(&roms.join(dentro).join("juego.gba"), GBA);
        }
        write(&roms.join("Windows").join("System32").join("juego.gba"), GBA);
        write(&lib.copies.join("otro-juego").join("juego.gba"), GBA);
        write(&lib.downloads.join("juego.gba"), GBA);
        let vistas = RefCell::new(Vec::new());
        let buscar = |depth: usize| {
            let opts = Options { max_depth: depth, ..Options::default() };
            scan(&lib, &wanted, &roms, &opts, &AtomicBool::new(false), &mut |p| {
                if p.phase == Phase::Listing {
                    vistas.borrow_mut().push(p.file.to_path_buf());
                }
            })
            .unwrap()
        };
        // Tres carpetas hacia dentro: con 2 no llega.
        assert!(buscar(2).matches.is_empty());
        vistas.borrow_mut().clear();
        let s = buscar(3);
        assert_eq!(s.matches.iter().map(|m| m.path.clone()).collect::<Vec<_>>(), [hondo.join("juego.gba")]);
        assert_eq!(s.files, 1);
        // No entró en las del sistema ni en las de la biblioteca (sí en su raíz, que no es suya).
        assert_eq!(*vistas.borrow(), [roms.clone(), roms.join("1"), roms.join("1").join("2"), hondo.clone(), roms.join("Unricopie")]);
        // Una carpeta «Windows» sin System32 es de la persona.
        fs::remove_dir_all(roms.join("Windows").join("System32")).unwrap();
        write(&roms.join("Windows").join("juego.gba"), GBA);
        assert_eq!(buscar(3).matches.len(), 2);
        // El tope de tamaño de la plataforma: un «cartucho de GBA» de más de 128 MB no se lee.
        assert!(fits(&[hint(Some("gba"), &Options::default())].into(), "gba", 128 * MB));
        assert!(!fits(&[hint(Some("gba"), &Options::default())].into(), "gba", 128 * MB + 1));
        // Sin pista de plataforma vale cualquier extensión, hasta `max_file`.
        let pc = [hint(Some("pc"), &Options { max_file: 10, ..Options::default() })].into();
        assert!(fits(&pc, "wad", 10) && fits(&pc, "", 1) && !fits(&pc, "wad", 11));
    }
}
