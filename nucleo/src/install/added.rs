//! Añadir a mano un proyecto de GitHub que no está en el catálogo (lo pidió la gente: otros
//! launchers dejan poner cualquier repo).
//!
//! Con `owner/repo` se leen sus releases (`release`) y se arma una receta mínima para este
//! equipo, con reglas cortas y prudentes:
//!
//! - **El archivo** (`pick_asset`): por las palabras del nombre (windows, win64, linux, x86_64,
//!   appimage, macos, universal…) y la extensión. Solo se elige solo cuando hay un único archivo
//!   que dice ser de este sistema; si hay varios, o el nombre no dice de qué sistema es, o es un
//!   `.exe` suelto (que puede ser un instalador), se devuelven para que elija la persona. Los
//!   instaladores (`.msi`, `setup.exe`) no se usan.
//! - **El patrón** (`pattern`): el nombre elegido con `*` donde va la versión, para que las
//!   actualizaciones sigan encontrando el archivo.
//! - **El programa** (`find_run`): no se puede saber antes de bajar. Después de descomprimir se
//!   buscan los `.exe` (sin desinstaladores, redistribuibles y demás), una AppImage o un ELF, o la
//!   `.app`; si hay uno claro se usa y, si no, se devuelven para elegir. Lo elegido queda en la
//!   receta, y desde ahí abrir y actualizar van por el camino de siempre.
//!
//! Lo demás no cambia: tamaño y SHA-256 de GitHub, antivirus del equipo, espacio en disco,
//! recibo, versiones y actualizaciones. Lo que una receta añadida a mano **no** tiene: copia
//! del juego que comprobar (`copy.how: none`, Unricopie no sabe nada de ese juego), modo
//! portable, ni forma de apagar el actualizador propio.
//!
//! **Dónde se marca.** En la receta (`Recipe::by_hand`), no en el recibo: lo que no está
//! revisado es la receta; la descarga pasa las mismas comprobaciones y su recibo es igual que
//! los demás (y sigue sin llevar nada de la receta). Así, si un día el catálogo trae la receta
//! de ese proyecto, lo instalado deja de ser «a mano» sin tocar nada.
//!
//! **Dónde se guardan.** En `<biblioteca>/added.json` (`Library::added`): con la biblioteca, para
//! que viajen con ella en modo portable y `--data-dir` las aísle; y fuera de `datos/`, que es
//! lo que se baja de recompendium.com y se puede pisar o borrar.
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::installer::{self, Library, Plan};
use super::receipt::{self, PackageFile, Receipt, Stamp};
use super::recipe::{matches, PackageKind, Platform, Recipe, Step};
use super::release::{self, Channel, GhRelease};
use crate::path::RelPath;
use crate::{archive, Error, Result};

/// El tipo de las fichas que no están en el catálogo: `added/<owner>-<repo>`.
pub const KIND: &str = "added";
const FORMAT: u32 = 1;

/// `owner/repo` a partir de lo que escriba la persona: eso mismo, o una dirección de GitHub
/// (`https://github.com/owner/repo`, con o sin `.git`, `/releases` y lo que siga).
pub fn parse_repo(text: &str) -> Result<String> {
    let bad = || Error::BadRepo(text.trim().to_string());
    let lower = text.trim().to_ascii_lowercase();
    let mut rest = text.trim();
    let mut skip = 0;
    let scheme = ["https://", "http://"].iter().find(|p| lower.starts_with(**p));
    if let Some(p) = scheme {
        skip += p.len();
    }
    let host = ["www.github.com/", "github.com/"].iter().find(|h| lower[skip..].starts_with(**h));
    match host {
        Some(h) => skip += h.len(),
        // Una dirección de otro sitio.
        None if scheme.is_some() => return Err(bad()),
        None => {}
    }
    rest = &rest[skip..];
    rest = rest.split(['?', '#']).next().unwrap_or_default();
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let (Some(owner), Some(name)) = (parts.next(), parts.next()) else { return Err(bad()) };
    // Sin `github.com` delante solo vale `owner/repo`, sin nada detrás.
    if host.is_none() && parts.next().is_some() {
        return Err(bad());
    }
    let name = name.strip_suffix(".git").unwrap_or(name);
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && s != "." && s != "..";
    if !ok(owner) || !ok(name) {
        return Err(bad());
    }
    Ok(format!("{owner}/{name}"))
}

/// La ficha de un repo que no está en el catálogo: `added/exok-celeste64`.
pub fn entry_for(repo: &str) -> String {
    let slug: String = repo.to_ascii_lowercase().chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' }).collect();
    format!("{KIND}/{slug}")
}

/// El nombre del repo, sin el dueño.
pub fn repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

// --- Qué archivo de la release toca ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Linux,
    Mac,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    X64,
    X86,
    Arm64,
    /// Compilación universal de macOS.
    Universal,
    /// Un procesador para el que no hay plataforma (ARM de 32 bits, RISC-V…).
    Other,
}

fn os_of(p: Platform) -> Os {
    use Platform::*;
    match p {
        WindowsX64 | WindowsX86 | WindowsArm64 => Os::Windows,
        LinuxX64 | LinuxArm64 | LinuxFlatpak => Os::Linux,
        Macos | MacosArm64 | MacosX64 => Os::Mac,
    }
}

fn platform_of(os: Os, arch: Arch) -> Option<Platform> {
    Some(match (os, arch) {
        (Os::Windows, Arch::X64) => Platform::WindowsX64,
        (Os::Windows, Arch::X86) => Platform::WindowsX86,
        (Os::Windows, Arch::Arm64) => Platform::WindowsArm64,
        (Os::Linux, Arch::X64) => Platform::LinuxX64,
        (Os::Linux, Arch::Arm64) => Platform::LinuxArm64,
        (Os::Mac, Arch::Universal) => Platform::Macos,
        (Os::Mac, Arch::Arm64) => Platform::MacosArm64,
        (Os::Mac, Arch::X64) => Platform::MacosX64,
        _ => return None,
    })
}

/// Por qué un archivo de la release no se ofrece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Why {
    /// Es de otro sistema (o de una consola, Android, la web…).
    OtherSystem,
    /// Es de este sistema pero de otro procesador.
    OtherArch,
    /// Un instalador (`.msi`, `setup.exe`): Unricopie no los ejecuta.
    Installer,
    /// Código fuente, símbolos de depuración y cosas así.
    Extra,
    /// No es un paquete que Unricopie sepa abrir (un `.rar`, un `.deb`, una suma, un texto).
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub name: String,
    pub why: Why,
}

/// Lo que salió de mirar los nombres de una release.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Assets {
    /// El archivo, si hay uno solo que dice ser de este sistema.
    pub chosen: Option<String>,
    /// Los que podrían servir aquí, del más probable al menos.
    pub candidates: Vec<String>,
    pub rejected: Vec<Rejected>,
}

/// Las palabras de un nombre de archivo, en minúsculas (`x86_64` cuenta como una: `x64`).
fn tokens(name: &str) -> Vec<String> {
    let n = name.to_ascii_lowercase().replace("x86_64", "x64").replace("x86-64", "x64");
    n.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// De qué sistema dice ser una palabra: `Some(Some(os))` uno de los nuestros, `Some(None)` otro
/// (una consola, Android, la web), `None` si no dice nada.
fn system_word(t: &str) -> Option<Option<Os>> {
    let any = |list: &[&str]| list.contains(&t);
    let around = |list: &[&str]| list.iter().any(|k| t.starts_with(k) || t.ends_with(k));
    if any(&["win", "w64", "w32", "msvc", "mingw", "mingw64", "mingw32"]) || around(&["windows", "win64", "win32"]) {
        Some(Some(Os::Windows))
    } else if any(&["debian", "fedora", "steamdeck", "steamos", "appimage"]) || around(&["linux"]) || t.starts_with("ubuntu") {
        Some(Some(Os::Linux))
    } else if any(&["mac", "darwin", "apple"]) || around(&["macos", "osx"]) {
        Some(Some(Os::Mac))
    } else if any(&[
        "android", "apk", "ios", "switch", "nx", "nro", "vita", "psvita", "ps2", "ps3", "ps4", "ps5", "psp", "wii", "wiiu", "3ds", "n3ds", "web", "wasm", "emscripten", "html5",
        "freebsd", "openbsd", "netbsd", "haiku", "dreamcast", "xbox", "uwp", "webos", "miyoo", "portmaster", "raspberrypi", "rpi",
    ]) {
        Some(None)
    } else {
        None
    }
}

fn arch_word(t: &str, os: Option<Os>) -> Option<Arch> {
    let any = |list: &[&str]| list.contains(&t);
    if any(&["x64", "amd64", "win64", "64bit", "linux64", "mingw64", "w64"]) {
        Some(Arch::X64)
    } else if any(&["x86", "i686", "i586", "i386", "ia32", "win32", "32bit", "linux32", "mingw32", "w32"]) {
        Some(Arch::X86)
    } else if any(&["arm64", "aarch64", "armv8"]) {
        Some(Arch::Arm64)
    } else if any(&["universal", "universal2"]) {
        Some(Arch::Universal)
    } else if any(&["arm", "arm32", "armv6", "armv7", "armv7l", "armhf", "riscv64", "ppc", "ppc64", "ppc64le", "s390x", "mips", "loongarch64"]) {
        Some(Arch::Other)
    } else if os == Some(Os::Mac) && t == "intel" {
        Some(Arch::X64)
    } else if os == Some(Os::Mac) && any(&["silicon", "m1"]) {
        Some(Arch::Arm64)
    } else {
        None
    }
}

/// Lo único que hay en `found`, si todo lo que hay dice lo mismo.
fn only<T: PartialEq + Copy>(found: &[T]) -> Option<T> {
    let first = *found.first()?;
    found.iter().all(|x| *x == first).then_some(first)
}

/// Un archivo que se puede instalar aquí.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Usable {
    /// La plataforma bajo la que va en la receta (la del archivo, que puede ser un sustituto).
    platform: Platform,
    kind: PackageKind,
    /// Menos es mejor. Desde `UNSURE`, nunca se elige solo.
    rank: u32,
}

/// Desde aquí no se elige solo: un `.exe` suelto (puede ser un instalador con otro nombre) o un
/// archivo cuyo nombre no dice de qué sistema es.
const UNSURE: u32 = 50;

/// Qué es un archivo de una release para el equipo `host`, solo por su nombre.
fn classify(name: &str, host: Platform) -> std::result::Result<Usable, Why> {
    let words = tokens(name);
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    let Some(kind) = PackageKind::from_name(name) else { return Err(Why::Unsupported) };
    // El sistema: primero la extensión, que no engaña, y si no dice nada, las palabras.
    let by_ext = match kind {
        PackageKind::Installer | PackageKind::Portable => Some(Os::Windows),
        PackageKind::Appimage | PackageKind::Deb | PackageKind::Rpm | PackageKind::Flatpak | PackageKind::Snap => Some(Os::Linux),
        PackageKind::Dmg => Some(Os::Mac),
        PackageKind::Archive => None,
    };
    let systems: Vec<Option<Os>> = words.iter().filter_map(|w| system_word(w)).collect();
    let os = match by_ext {
        Some(os) => Some(os),
        // Dos sistemas en el mismo nombre: no se sabe.
        None => match only(&systems) {
            Some(Some(os)) => Some(os),
            Some(None) => return Err(Why::OtherSystem),
            None => None,
        },
    };
    if os.is_some_and(|os| os != os_of(host)) {
        return Err(Why::OtherSystem);
    }
    match kind {
        PackageKind::Installer => return Err(Why::Installer),
        PackageKind::Portable if words.iter().any(|w| ["setup", "installer", "install"].iter().any(|k| w.ends_with(k))) => return Err(Why::Installer),
        PackageKind::Deb | PackageKind::Rpm | PackageKind::Flatpak | PackageKind::Snap => return Err(Why::Unsupported),
        PackageKind::Archive if archive::Kind::from_name(name).is_none() => return Err(Why::Unsupported),
        _ => {}
    }
    if has(&["src", "source", "sources", "symbols", "pdb", "debug", "dbg", "debuginfo", "debugsymbols", "sdk", "devel", "docs", "soundtrack"]) {
        return Err(Why::Extra);
    }
    let archs: Vec<Arch> = words.iter().filter_map(|w| arch_word(w, os)).collect();
    let (platform, mut rank) = match only(&archs).and_then(|a| Some((a, platform_of(os.unwrap_or(os_of(host)), a)?))) {
        // Un procesador que no es de ninguna plataforma (o que no existe en este sistema).
        None if archs.contains(&Arch::Other) => return Err(Why::OtherArch),
        // No dice el procesador (o dice varios): justo detrás del que sí dice el de este equipo.
        None => (host, 1),
        Some((_, p)) => match host.fallbacks().iter().position(|f| *f == p) {
            Some(i) => (p, 2 * i as u32),
            None => return Err(Why::OtherArch),
        },
    };
    if os.is_none() {
        rank += 2 * UNSURE;
    }
    if kind == PackageKind::Portable {
        rank += UNSURE;
    }
    Ok(Usable { platform, kind, rank })
}

/// Elige entre los archivos de una release el de este equipo, mirando solo sus nombres.
pub fn pick_asset(names: &[&str], host: Platform) -> Assets {
    let mut usable: Vec<(u32, &str)> = Vec::new();
    let mut out = Assets::default();
    for name in names {
        match classify(name, host) {
            Ok(u) => usable.push((u.rank, name)),
            Err(why) => out.rejected.push(Rejected { name: name.to_string(), why }),
        }
    }
    usable.sort();
    if let Some((best, name)) = usable.first() {
        if *best < UNSURE && usable.iter().filter(|(r, _)| r == best).count() == 1 {
            out.chosen = Some(name.to_string());
        }
    }
    out.candidates = usable.into_iter().map(|(_, n)| n.to_string()).collect();
    out
}

/// El nombre de un archivo con `*` donde va la versión: la etiqueta de la release (con o sin su
/// `v`) o, si no aparece, lo primero con forma de versión (`1.2`, `v0.9.1`). Sin versión, el
/// nombre tal cual.
pub fn pattern(name: &str, tag: &str) -> String {
    let bare = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    for version in [tag, bare] {
        // Una etiqueta de una letra o un dígito («1») cambiaría cualquier cosa del nombre.
        if version.len() >= 3 || (version.len() == 2 && version.chars().all(|c| c.is_ascii_digit())) {
            if let Some(i) = name.find(version) {
                return format!("{}*{}", &name[..i], &name[i + version.len()..]);
            }
        }
    }
    match version_span(name) {
        Some((a, b)) => format!("{}*{}", &name[..a], &name[b..]),
        None => name.to_string(),
    }
}

/// Dónde hay algo con forma de versión en `name`: dígitos con al menos un punto en medio, al
/// principio de una palabra, con su `v` si la lleva. La extensión no cuenta (`juego-1.2.7z`).
fn version_span(name: &str) -> Option<(usize, usize)> {
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let word_start = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if word_start && (b[i] == b'v' || b[i] == b'V') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if word_start && b[i].is_ascii_digit() {
            // Los grupos de dígitos separados por puntos: dónde termina cada uno.
            let mut ends = Vec::new();
            let mut j = i;
            loop {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                ends.push(j);
                if j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            // Un grupo pegado a una letra es otra cosa: la extensión `.7z`, `1.2.3beta`.
            if b.get(j).is_some_and(u8::is_ascii_alphabetic) {
                ends.pop();
            }
            if ends.len() >= 2 {
                return Some((start, ends[ends.len() - 1]));
            }
            i = j.max(i + 1);
        } else {
            i = start + 1;
        }
    }
    None
}

/// Lo que se propone instalar: la receta para este equipo, y de qué release y archivo salió.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub recipe: Recipe,
    pub tag: String,
    pub asset: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Ready(Box<Proposal>),
    /// No hay un único archivo claro en la release `tag`: elige la persona (o no hay ninguno).
    Choose { tag: String, assets: Assets },
}

/// Arma la receta de `repo` (ya validado, `parse_repo`) para el equipo `host`, con las releases
/// ya leídas. Sale de la última release oficial, la misma que instala el canal «sin probar».
///
/// - `entry`: la ficha bajo la que queda (`entry_for`, o la del catálogo si el repo está ahí
///   sin receta).
/// - `asset`: el archivo que eligió la persona, si `pick_asset` no pudo.
/// - `run`: el programa que eligió, si después de instalar no se supo cuál era.
pub fn propose(repo: &str, entry: &str, releases: &[GhRelease], host: Platform, asset: Option<&str>, run: Option<&str>) -> Result<Found> {
    let rel = release::pick(releases, Channel::Untested, None).ok_or_else(|| Error::NoRelease { repo: repo.to_string(), channel: Channel::Untested })?;
    let names: Vec<&str> = rel.assets.iter().map(|a| a.name.as_str()).collect();
    let (name, usable) = match asset {
        Some(wanted) => {
            let name = *names.iter().find(|n| **n == wanted).ok_or_else(|| Error::NoAsset { tag: rel.tag_name.clone(), pattern: wanted.to_string() })?;
            let kind = PackageKind::from_name(name);
            let unsupported = |kind: String| Error::UnsupportedPackage { asset: name.to_string(), kind };
            let usable = match (classify(name, host), kind) {
                (Ok(u), _) => u,
                // Lo que no se sabe instalar no se instala aunque se pida: un instalador, un
                // paquete del sistema, un formato que no se sabe abrir.
                (Err(Why::Installer), _) => return Err(unsupported("Installer".into())),
                // El nombre dice que es de otro sistema o un extra, pero la persona lo eligió a
                // sabiendas: los nombres también se equivocan.
                (Err(_), Some(kind @ (PackageKind::Portable | PackageKind::Appimage | PackageKind::Dmg))) => Usable { platform: host, kind, rank: 0 },
                (Err(_), Some(PackageKind::Archive)) if archive::Kind::from_name(name).is_some() => Usable { platform: host, kind: PackageKind::Archive, rank: 0 },
                (Err(_), kind) => return Err(unsupported(kind.map_or("?".to_string(), |k| format!("{k:?}")))),
            };
            (name, usable)
        }
        None => {
            let assets = pick_asset(&names, host);
            match assets.chosen.as_deref().and_then(|c| names.iter().find(|n| **n == c)) {
                Some(name) => (*name, classify(name, host).map_err(|_| Error::BadRecipe(format!("{name} dejó de servir")))?),
                None => return Ok(Found::Choose { tag: rel.tag_name.clone(), assets }),
            }
        }
    };
    let size = rel.assets.iter().find(|a| a.name == name).map_or(0, |a| a.size);
    let (pat, exclude) = unique_pattern(name, &rel.tag_name, &names);
    let key = usable.platform.as_str();
    let mut rule = serde_json::json!({ "assets": { key: pat } });
    if !exclude.is_empty() {
        rule["exclude"] = exclude.into();
    }
    // Un ejecutable suelto o una AppImage es lo que se abre; en un archivo comprimido no se sabe
    // hasta descomprimirlo (`find_run`), salvo que la persona ya lo haya dicho.
    let run = match (run, usable.kind) {
        (Some(r), _) => Some(RelPath::new(r)?.as_str().to_string()),
        (None, PackageKind::Portable | PackageKind::Appimage) => Some("{asset}".to_string()),
        (None, _) => None,
    };
    if let Some(r) = run {
        rule["run"] = serde_json::json!({ key: r });
    }
    // Un zip no guarda los permisos de Linux: el programa se marca como ejecutable.
    if usable.kind == PackageKind::Archive && os_of(usable.platform) != Os::Windows {
        rule["after-extract"] = serde_json::json!({ key: ["chmod-x"] });
    }
    let json = serde_json::json!({
        "entry": entry,
        "repo": repo,
        "release": rule,
        "copy": { "how": "none" },
        "checked": { "date": today(), "release": rel.tag_name, "sources": [format!("https://github.com/{repo}/releases")] },
        "by-hand": true,
    });
    let recipe = Recipe::from_json(&json.to_string())?;
    Ok(Found::Ready(Box::new(Proposal { recipe, tag: rel.tag_name.clone(), asset: name.to_string(), size })))
}

/// El patrón de `name` y, si en la misma release encaja con más archivos (`juego-*-win.zip` y
/// `juego-*-debug-win.zip`), los patrones de esos otros para excluirlos. Si ni así queda uno
/// solo, el nombre exacto: se instala esta versión y la actualización dirá que no encuentra el
/// archivo, que es mejor que bajar otro.
fn unique_pattern(name: &str, tag: &str, names: &[&str]) -> (String, Vec<String>) {
    let pat = pattern(name, tag);
    let others: Vec<&str> = names.iter().copied().filter(|n| *n != name && matches(&pat, n)).collect();
    if others.is_empty() {
        return (pat, Vec::new());
    }
    let mut exclude: Vec<String> = others.iter().map(|n| pattern(n, tag)).collect();
    exclude.sort();
    exclude.dedup();
    if exclude.iter().any(|x| matches(x, name)) {
        return (name.to_string(), Vec::new());
    }
    (pat, exclude)
}

/// Hoy (UTC), `AAAA-MM-DD`, para `checked.date`. (De días a fecha, como Howard Hinnant.)
fn today() -> String {
    let z = (receipt::now() / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

// --- Qué programa se abre ---

/// Lo que salió de buscar el programa entre lo descomprimido.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Runs {
    pub chosen: Option<String>,
    /// Los programas que hay, de los de más arriba a los de más adentro.
    pub candidates: Vec<String>,
}

/// Programas que vienen con muchos juegos y no son el juego: desinstaladores, redistribuibles
/// de Microsoft, lo que manda los informes de fallos, actualizadores.
fn helper(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let (dirs, file) = match lower.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", lower.as_str()),
    };
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    let starts = ["unins", "vc_redist", "vcredist", "crashpad", "crash_handler", "crashhandler", "crashreport", "crashsender", "unitycrashhandler", "dxsetup", "dxwebsetup", "directx", "oalinst", "dotnet", "windowsdesktop-runtime", "setup", "install"];
    let inside = ["updater", "redist", "crashpad"];
    let exact = ["update", "notification_helper", "createdump", "qtwebengineprocess"];
    starts.iter().any(|s| stem.starts_with(s))
        || inside.iter().any(|s| stem.contains(s))
        || exact.contains(&stem)
        || dirs.split('/').any(|d| d.contains("redist") || d == "__macosx")
}

/// Para comparar nombres: solo letras y dígitos, en minúsculas.
fn plain(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Busca el programa del juego entre `files` (rutas dentro de `root`, la carpeta del juego):
///
/// - Windows: los `.exe` que no son ayudantes (`helper`);
/// - Linux: las AppImage o, si no hay, los ELF que no son librerías (`.so`);
/// - macOS: las `.app` (la de más afuera).
///
/// Se elige uno solo si no hay duda: el único que se llama como el repo; si no, el único que
/// hay; si no, el único de la carpeta de más arriba que tenga alguno. Si no, ninguno.
pub fn find_run(root: &Path, files: &[&str], platform: Platform, repo: &str) -> Runs {
    let mut found: Vec<String> = match os_of(platform) {
        Os::Windows => files.iter().filter(|f| f.to_ascii_lowercase().ends_with(".exe") && !helper(f)).map(|f| f.to_string()).collect(),
        Os::Linux => {
            let appimages: Vec<String> = files.iter().filter(|f| f.to_ascii_lowercase().ends_with(".appimage")).map(|f| f.to_string()).collect();
            if appimages.is_empty() {
                files.iter().filter(|f| !library(f) && !helper(f) && is_elf(root, f)).map(|f| f.to_string()).collect()
            } else {
                appimages
            }
        }
        Os::Mac => {
            let mut apps: Vec<String> = files.iter().filter_map(|f| outer_app(f)).collect();
            apps.sort();
            apps.dedup();
            apps
        }
    };
    let depth = |f: &String| f.matches('/').count();
    found.sort_by(|a, b| depth(a).cmp(&depth(b)).then_with(|| a.to_lowercase().cmp(&b.to_lowercase())));
    let name = plain(repo_name(repo));
    let stem = |f: &String| {
        let file = f.rsplit('/').next().unwrap_or(f);
        plain(file.rsplit_once('.').map_or(file, |(s, _)| s))
    };
    let named: Vec<&String> = found.iter().filter(|f| stem(f) == name).collect();
    let top: Vec<&String> = found.iter().filter(|f| Some(depth(f)) == found.first().map(depth)).collect();
    let chosen = match (named.as_slice(), found.as_slice(), top.as_slice()) {
        ([one], _, _) => Some((*one).clone()),
        (_, [one], _) => Some(one.clone()),
        (_, _, [one]) => Some((*one).clone()),
        _ => None,
    };
    Runs { chosen, candidates: found }
}

/// Una librería de Linux: `libfoo.so`, `libfoo.so.2`.
fn library(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    file.ends_with(".so") || file.contains(".so.")
}

fn is_elf(root: &Path, file: &str) -> bool {
    use std::io::Read;
    let Ok(rel) = RelPath::new(file) else { return false };
    let mut magic = [0u8; 4];
    fs::File::open(rel.under(root)).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"\x7fELF"
}

/// La `.app` de más afuera de una ruta: `Juego.app` para `Juego.app/Contents/MacOS/juego`.
fn outer_app(path: &str) -> Option<String> {
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        // Solo carpetas: el último trozo es el archivo.
        if part.to_ascii_lowercase().ends_with(".app") && end < path.len() {
            return Some(path[..end].to_string());
        }
        end += 1;
    }
    None
}

// --- Instalar ---

/// Instala lo del plan de una receta añadida a mano. Si la receta ya dice qué se abre, es
/// `installer::install` sin más. Si no (la primera vez), baja y comprueba igual (`fetch`:
/// espacio, tamaño, SHA-256 y antivirus), descomprime, busca el programa (`find_run`) y solo
/// entonces deja el recibo y mueve el juego a su sitio.
///
/// Devuelve el recibo y la receta ya con el programa apuntado: es la que hay que guardar
/// (`add`). Si no se sabe cuál es el programa, `Error::ChooseRun` con los que hay, y no queda
/// nada instalado: se vuelve a intentar con `propose(…, run)`.
pub fn install(agent: &ureq::Agent, library: &Library, recipe: &Recipe, plan: &Plan, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(Receipt, Recipe)> {
    if plan.run.is_some() {
        return Ok((installer::install(agent, library, plan, progress)?, recipe.clone()));
    }
    if plan.dest.exists() {
        return Err(Error::AlreadyInstalled(plan.dest.clone()));
    }
    let (file, got) = installer::fetch(agent, library, plan, progress)?;
    let result = install_downloaded(library, recipe, plan, &file, &got);
    let _ = fs::remove_file(&file);
    result
}

/// La parte de `install` que no usa la red, para un plan sin `run`: lo bajado ya está en `file`.
pub fn install_downloaded(library: &Library, recipe: &Recipe, plan: &Plan, file: &Path, got: &crate::net::Downloaded) -> Result<(Receipt, Recipe)> {
    if plan.dest.exists() {
        return Err(Error::AlreadyInstalled(plan.dest.clone()));
    }
    let mut p = installer::prepare(library, plan, file, got)?;
    // Sin `run`, `prepare` no pudo saber si el juego viene dentro de una carpeta
    // (`Juego-1.2/juego.exe`). Si todo viene dentro de una sola, la raíz es esa, como en las
    // recetas de siempre; una `.app` no cuenta: esa carpeta es el programa.
    if let Some(top) = single_folder(&p.receipt.files) {
        let prefix = format!("{top}/");
        let mut inner = Vec::with_capacity(p.receipt.files.len());
        for f in &p.receipt.files {
            let rest = f.path.as_str().strip_prefix(&prefix).ok_or_else(|| Error::PackageLayout(f.path.to_string()))?;
            inner.push(PackageFile { path: RelPath::new(rest)?, size: f.size, xxh3: f.xxh3 });
        }
        p.root = RelPath::new(&top)?.under(&p.root);
        p.receipt.files = inner;
    }
    let paths: Vec<&str> = p.receipt.files.iter().map(|f| f.path.as_str()).collect();
    let runs = find_run(&p.root, &paths, plan.platform, &plan.repo);
    let Some(run) = runs.chosen else { return Err(Error::ChooseRun { candidates: runs.candidates }) };
    let program = RelPath::new(&run)?.under(&p.root);
    if plan.after_extract.contains(&Step::ChmodX) && program.is_file() {
        archive::set_executable(&program)?;
    }
    if p.root.join(receipt::FILE_NAME).exists() {
        return Err(Error::PackageHasReceipt(plan.asset.clone()));
    }
    // Una app de macOS es una carpeta: no hay un archivo que vigilar.
    p.receipt.run_stamp = if program.is_file() { Some(Stamp::of(&program)?) } else { None };
    p.receipt.run = Some(run.clone());
    p.receipt.write(&p.root)?;
    fs::rename(&p.root, &plan.dest).map_err(|e| Error::io(&plan.dest, e))?;
    let mut recipe = recipe.clone();
    recipe.release.run.insert(plan.platform, run);
    Ok((p.receipt, recipe))
}

/// La carpeta en la que viene todo el paquete, si viene todo dentro de una sola.
fn single_folder(files: &[PackageFile]) -> Option<String> {
    let (top, _) = files.first()?.path.as_str().split_once('/')?;
    let prefix = format!("{top}/");
    (!top.to_ascii_lowercase().ends_with(".app") && files.iter().all(|f| f.path.as_str().starts_with(&prefix))).then(|| top.to_string())
}

// --- Las añadidas, guardadas en la biblioteca ---

#[derive(Serialize, Deserialize)]
struct Stored {
    format: u32,
    recipes: Vec<serde_json::Value>,
}

/// Las recetas añadidas a mano de esta biblioteca. Si no hay archivo, ninguna. Una que este
/// launcher no entiende se salta (y se pierde al guardar: la escribió una versión más nueva).
pub fn load(library: &Library) -> Result<Vec<Recipe>> {
    let text = match fs::read_to_string(&library.added) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&library.added, e)),
    };
    let stored: Stored = serde_json::from_str(&text).map_err(|e| Error::Config { path: library.added.clone(), reason: e.to_string() })?;
    if stored.format > FORMAT {
        return Err(Error::Config { path: library.added.clone(), reason: format!("formato {} más nuevo que este launcher", stored.format) });
    }
    Ok(stored
        .recipes
        .into_iter()
        .filter_map(|v| Recipe::from_json(&v.to_string()).ok())
        // Lo que hay en este archivo es siempre «a mano», diga lo que diga.
        .map(|r| Recipe { by_hand: true, ..r })
        .collect())
}

/// Lo guarda sin dejar nunca el archivo a medias: se escribe aparte y se renombra.
fn save(library: &Library, recipes: &[Recipe]) -> Result<()> {
    let stored = Stored { format: FORMAT, recipes: recipes.iter().map(serde_json::to_value).collect::<std::result::Result<_, _>>()? };
    if let Some(dir) = library.added.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = library.added.with_extension(format!("json.{}-{nanos}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(&stored)?).map_err(|e| Error::io(&tmp, e))?;
    fs::rename(&tmp, &library.added).map_err(|e| Error::io(&library.added, e))
}

/// Guarda `recipe` entre las añadidas; si ya había una de esa ficha, la sustituye.
pub fn add(library: &Library, recipe: &Recipe) -> Result<()> {
    recipe.check()?;
    let mut all = load(library)?;
    all.retain(|r| r.entry != recipe.entry);
    all.push(Recipe { by_hand: true, ..recipe.clone() });
    save(library, &all)
}

/// Quita la receta añadida de la ficha `entry` y la devuelve; `None` si no estaba. Solo quita
/// la receta: lo instalado no se toca (quien llama mira antes si lo está).
pub fn remove(library: &Library, entry: &str) -> Result<Option<Recipe>> {
    let mut all = load(library)?;
    let Some(i) = all.iter().position(|r| r.entry == entry) else { return Ok(None) };
    let gone = all.remove(i);
    save(library, &all)?;
    Ok(Some(gone))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::installer::tests::zip_with;
    use crate::install::receipt::RunState;
    use crate::net::Downloaded;

    const W: Platform = Platform::WindowsX64;
    const L: Platform = Platform::LinuxX64;
    const M: Platform = Platform::MacosArm64;

    #[test]
    fn lee_el_repo_de_lo_que_escriba_la_persona() {
        for bien in [
            "EXOK/Celeste64",
            "  EXOK/Celeste64  ",
            "https://github.com/EXOK/Celeste64",
            "https://github.com/EXOK/Celeste64/",
            "https://github.com/EXOK/Celeste64.git",
            "http://www.github.com/EXOK/Celeste64/releases/tag/v1.1.1",
            "github.com/EXOK/Celeste64/releases?page=2#x",
            "HTTPS://GitHub.com/EXOK/Celeste64",
        ] {
            assert_eq!(parse_repo(bien).unwrap(), "EXOK/Celeste64", "{bien}");
        }
        assert_eq!(parse_repo("k4zmu2a/SpaceCadetPinball").unwrap(), "k4zmu2a/SpaceCadetPinball");
        assert_eq!(parse_repo("a.b/c_d-e.f").unwrap(), "a.b/c_d-e.f");
        for mal in ["", "Celeste64", "EXOK/", "/Celeste64", "EXOK/Celeste64/releases", "https://gitlab.com/a/b", "https://github.com/EXOK", "a b/c", "../x", "a/..", "a/b c", "https://example.com/github.com/a/b"] {
            assert!(matches!(parse_repo(mal), Err(Error::BadRepo(_))), "{mal}");
        }
        assert_eq!(entry_for("EXOK/Celeste64"), "added/exok-celeste64");
        assert_eq!(entry_for("a.b/c_d"), "added/a-b-c-d");
        assert_eq!(repo_name("EXOK/Celeste64"), "Celeste64");
    }

    fn elegido(names: &[&str], host: Platform) -> Option<String> {
        pick_asset(names, host).chosen
    }

    fn por_que(names: &[&str], host: Platform, name: &str) -> Option<Why> {
        pick_asset(names, host).rejected.into_iter().find(|r| r.name == name).map(|r| r.why)
    }

    #[test]
    fn elige_el_archivo_de_cada_sistema() {
        // Celeste 64 (v1.1.1): uno por sistema y procesador.
        let celeste = ["Celeste64-v1.1.1-Linux-arm.zip", "Celeste64-v1.1.1-Linux-arm64.zip", "Celeste64-v1.1.1-Linux-x64.zip", "Celeste64-v1.1.1-macOS-x64.zip", "Celeste64-v1.1.1-Windows-x64.zip"];
        assert_eq!(elegido(&celeste, W).as_deref(), Some("Celeste64-v1.1.1-Windows-x64.zip"));
        assert_eq!(elegido(&celeste, L).as_deref(), Some("Celeste64-v1.1.1-Linux-x64.zip"));
        assert_eq!(elegido(&celeste, Platform::LinuxArm64).as_deref(), Some("Celeste64-v1.1.1-Linux-arm64.zip"));
        // Un Mac con Apple Silicon corre la de Intel con Rosetta; Windows en ARM, la de x64.
        assert_eq!(elegido(&celeste, M).as_deref(), Some("Celeste64-v1.1.1-macOS-x64.zip"));
        assert_eq!(elegido(&celeste, Platform::WindowsArm64).as_deref(), Some("Celeste64-v1.1.1-Windows-x64.zip"));
        assert_eq!(por_que(&celeste, W, "Celeste64-v1.1.1-Linux-x64.zip"), Some(Why::OtherSystem));
        assert_eq!(por_que(&celeste, L, "Celeste64-v1.1.1-Linux-arm.zip"), Some(Why::OtherArch));
        assert_eq!(por_que(&celeste, L, "Celeste64-v1.1.1-Linux-arm64.zip"), Some(Why::OtherArch));
        assert_eq!(pick_asset(&celeste, W).candidates, ["Celeste64-v1.1.1-Windows-x64.zip"]);

        // Zelda64Recomp: sin procesador en Windows; en Linux, zip y AppImage a la vez.
        let zelda = ["Zelda64Recompiled-v1.2.2-Linux-ARM64.AppImage", "Zelda64Recompiled-v1.2.2-Linux-X64.AppImage", "Zelda64Recompiled-v1.2.2-Linux-X64.zip", "Zelda64Recompiled-v1.2.2-macOS.zip", "Zelda64Recompiled-v1.2.2-Windows.zip"];
        assert_eq!(elegido(&zelda, W).as_deref(), Some("Zelda64Recompiled-v1.2.2-Windows.zip"));
        assert_eq!(elegido(&zelda, M).as_deref(), Some("Zelda64Recompiled-v1.2.2-macOS.zip"));
        assert_eq!(elegido(&zelda, L), None, "zip o AppImage: elige la persona");
        assert_eq!(pick_asset(&zelda, L).candidates, ["Zelda64Recompiled-v1.2.2-Linux-X64.AppImage", "Zelda64Recompiled-v1.2.2-Linux-X64.zip"]);

        // Triples de Rust, `x86_64` con guion bajo, y los dos procesadores de Windows.
        let rust = ["juego-x86_64-pc-windows-msvc.zip", "juego-i686-pc-windows-msvc.zip", "juego-aarch64-apple-darwin.tar.gz", "juego-x86_64-apple-darwin.tar.gz", "juego-x86_64-unknown-linux-gnu.tar.xz", "juego-aarch64-unknown-linux-gnu.tar.xz"];
        assert_eq!(elegido(&rust, W).as_deref(), Some("juego-x86_64-pc-windows-msvc.zip"));
        assert_eq!(elegido(&rust, Platform::WindowsX86).as_deref(), Some("juego-i686-pc-windows-msvc.zip"));
        assert_eq!(por_que(&rust, Platform::WindowsX86, "juego-x86_64-pc-windows-msvc.zip"), Some(Why::OtherArch));
        assert_eq!(elegido(&rust, L).as_deref(), Some("juego-x86_64-unknown-linux-gnu.tar.xz"));
        assert_eq!(elegido(&rust, M).as_deref(), Some("juego-aarch64-apple-darwin.tar.gz"));
        assert_eq!(elegido(&rust, Platform::MacosX64).as_deref(), Some("juego-x86_64-apple-darwin.tar.gz"));

        // dhewm3: `win32` es Windows de 32 bits, que sirve en uno de 64 si no hay otra cosa; pero
        // con dos que encajan igual (el juego y sus mods) no se adivina.
        let dhewm = ["dhewm3-1.5.5_Linux_amd64.tar.gz", "dhewm3-1.5.5_win32.zip", "dhewm3-mods-1.5.5_Linux_amd64.tar.gz", "dhewm3-mods-1.5.5_win32.zip", "dhewm3-wide-guis-v0.2.zip"];
        assert_eq!(elegido(&dhewm, W), None);
        assert_eq!(pick_asset(&dhewm, W).candidates, ["dhewm3-1.5.5_win32.zip", "dhewm3-mods-1.5.5_win32.zip", "dhewm3-wide-guis-v0.2.zip"]);
        assert_eq!(elegido(&["juego_win32.zip", "juego_linux.tar.gz"], W).as_deref(), Some("juego_win32.zip"));
        // El de 64 gana al de 32, y al que no dice procesador.
        assert_eq!(elegido(&["juego-win32.zip", "juego-win64.zip"], W).as_deref(), Some("juego-win64.zip"));
        assert_eq!(elegido(&["juego-windows.zip", "juego-windows-x64.zip"], W).as_deref(), Some("juego-windows-x64.zip"));
        // macOS: universal, Intel y Apple Silicon; y un dmg.
        let mac = ["Juego-macOS-universal.zip", "Juego-macOS-intel.zip", "Juego-Windows.zip"];
        assert_eq!(elegido(&mac, M).as_deref(), Some("Juego-macOS-universal.zip"));
        assert_eq!(elegido(&mac, Platform::MacosX64).as_deref(), Some("Juego-macOS-intel.zip"));
        assert_eq!(elegido(&["Juego-1.0.dmg", "Juego-1.0-win64.zip"], M).as_deref(), Some("Juego-1.0.dmg"));
        // Los nombres de las máquinas de GitHub Actions.
        let ci = ["juego-ubuntu-22.04.tar.gz", "juego-windows-latest.zip", "juego-macos-14.zip"];
        assert_eq!(elegido(&ci, W).as_deref(), Some("juego-windows-latest.zip"));
        assert_eq!(elegido(&ci, L).as_deref(), Some("juego-ubuntu-22.04.tar.gz"));
        assert_eq!(elegido(&ci, M).as_deref(), Some("juego-macos-14.zip"));
        // Pegado al nombre también se entiende.
        assert_eq!(elegido(&["SoH-Win64.zip", "SoHLinux.zip", "soh-mac.zip"], W).as_deref(), Some("SoH-Win64.zip"));
        assert_eq!(elegido(&["JuegoWindows.zip", "JuegoLinux.zip"], L).as_deref(), Some("JuegoLinux.zip"));
    }

    #[test]
    fn no_adivina_cuando_no_esta_claro() {
        // Solo Windows, sin decirlo: se ofrece, no se elige.
        let a = pick_asset(&["SpaceCadetPinball.zip"], W);
        assert_eq!((a.chosen, a.candidates.as_slice()), (None, &["SpaceCadetPinball.zip".to_string()][..]));
        // Varias variantes para el mismo sistema.
        assert_eq!(elegido(&["juego-win64-dx11.zip", "juego-win64-vulkan.zip"], W), None);
        assert_eq!(elegido(&["juego-win64.zip", "juego-win64.7z"], W), None);
        // Un .exe suelto puede ser un instalador con otro nombre: se ofrece detrás de un zip, y
        // solo nunca se elige.
        assert_eq!(elegido(&["Juego-1.0-x64.exe"], W), None);
        assert_eq!(pick_asset(&["Juego-1.0-x64.exe"], W).candidates, ["Juego-1.0-x64.exe"]);
        assert_eq!(elegido(&["Juego-1.0-x64.exe", "Juego-1.0-win-x64.zip"], W).as_deref(), Some("Juego-1.0-win-x64.zip"));
        // Dos sistemas en el mismo nombre: no dice nada.
        assert_eq!(elegido(&["juego-windows-linux.zip"], W), None);
        assert_eq!(pick_asset(&["juego-windows-linux.zip"], W).candidates.len(), 1);
        // El que dice el sistema gana al que no dice nada.
        assert_eq!(elegido(&["juego.zip", "juego-windows.zip"], W).as_deref(), Some("juego-windows.zip"));
        // Nada.
        assert_eq!(pick_asset(&[], W), Assets::default());
    }

    #[test]
    fn descarta_instaladores_fuentes_y_lo_que_no_sabe_abrir() {
        let todo = [
            "Juego-1.0-Setup.exe", "JuegoSetup.exe", "juego-installer-x64.exe", "Juego-1.0.msi", "juego-1.0-src.zip", "juego-source.tar.gz", "juego-win64-debug.zip", "juego-win64-symbols.7z", "juego-win64.pdb",
            "SHA256SUMS.txt", "juego-win64.zip.sha256", "juego-windows.rar", "juego_1.0_amd64.deb", "juego-1.0.x86_64.rpm", "juego.flatpak", "juego-android.apk", "juego-switch.zip", "juego-web.zip", "juego-linux-armhf.tar.gz",
        ];
        let a = pick_asset(&todo, W);
        assert_eq!((a.chosen.clone(), a.candidates.len()), (None, 0), "{a:?}");
        let w = |n: &str| por_que(&todo, W, n).unwrap();
        for n in ["Juego-1.0-Setup.exe", "JuegoSetup.exe", "juego-installer-x64.exe", "Juego-1.0.msi"] {
            assert_eq!(w(n), Why::Installer, "{n}");
        }
        for n in ["juego-1.0-src.zip", "juego-source.tar.gz", "juego-win64-debug.zip", "juego-win64-symbols.7z"] {
            assert_eq!(w(n), Why::Extra, "{n}");
        }
        for n in ["juego-win64.pdb", "SHA256SUMS.txt", "juego-win64.zip.sha256", "juego-windows.rar", "juego-android.apk"] {
            assert_eq!(w(n), Why::Unsupported, "{n}");
        }
        for n in ["juego_1.0_amd64.deb", "juego-1.0.x86_64.rpm", "juego.flatpak", "juego-switch.zip", "juego-web.zip", "juego-linux-armhf.tar.gz"] {
            assert_eq!(w(n), Why::OtherSystem, "{n}");
        }
        // En Linux los paquetes del sistema tampoco: no se instalan en la biblioteca.
        let l = |n: &str| por_que(&todo, L, n).unwrap();
        assert_eq!((l("juego_1.0_amd64.deb"), l("juego-1.0.x86_64.rpm"), l("juego.flatpak")), (Why::Unsupported, Why::Unsupported, Why::Unsupported));
        assert_eq!(l("juego-linux-armhf.tar.gz"), Why::OtherArch);
        assert_eq!(l("Juego-1.0.msi"), Why::OtherSystem);
        // Solo el código fuente: no hay nada que instalar.
        assert_eq!(pick_asset(&["juego-1.0-src.zip", "juego-1.0-source.tar.gz"], L).candidates.len(), 0);
    }

    #[test]
    fn pone_el_comodin_donde_va_la_version() {
        assert_eq!(pattern("Celeste64-v1.1.1-Windows-x64.zip", "v1.1.1"), "Celeste64-*-Windows-x64.zip");
        // La etiqueta lleva `v` y el nombre no; o al revés, y entonces la `v` del nombre se va con ella.
        assert_eq!(pattern("dhewm3-1.5.5_win32.zip", "v1.5.5"), "dhewm3-*_win32.zip");
        assert_eq!(pattern("juego-v2.0-win64.zip", "2.0"), "juego-v*-win64.zip");
        // La etiqueta no sale en el nombre: lo que tenga forma de versión.
        assert_eq!(pattern("OpenRA-release-20250330-x64-winportable.zip", "release-20250330"), "OpenRA-*-x64-winportable.zip");
        assert_eq!(pattern("juego-0.9.12-linux-x86_64.tar.gz", "nightly"), "juego-*-linux-x86_64.tar.gz");
        assert_eq!(pattern("juego_v1.2_win64.7z", "latest"), "juego_*_win64.7z");
        assert_eq!(pattern("juego-1.2.7z", "otra"), "juego-*.7z");
        // Sin versión, el nombre tal cual; `x86_64`, `win64` o `sm64` no son versiones.
        for fijo in ["SpaceCadetPinball.zip", "sm64-win64.zip", "juego-x86_64.AppImage", "juego-win32.7z", "Juego.2.zip"] {
            assert_eq!(pattern(fijo, "v9.9.9"), fijo);
        }
        // Una etiqueta de un carácter no se busca: cambiaría cualquier cosa.
        assert_eq!(pattern("juego-1-win64.zip", "1"), "juego-1-win64.zip");
        assert_eq!(pattern("juego-12-win64.zip", "12"), "juego-*-win64.zip");
        for (name, tag) in [("Celeste64-v1.1.1-Windows-x64.zip", "v1.1.1"), ("juego-1.2.7z", "otra"), ("juego_v1.2_win64.7z", "latest")] {
            assert!(matches(&pattern(name, tag), name));
        }
    }

    fn releases(tag: &str, names: &[&str]) -> Vec<GhRelease> {
        let assets: Vec<serde_json::Value> = names
            .iter()
            .map(|n| serde_json::json!({ "name": n, "size": 1234, "browser_download_url": format!("https://example.invalid/{tag}/{n}"), "digest": null }))
            .collect();
        serde_json::from_value(serde_json::json!([
            { "tag_name": "v9-beta", "draft": false, "prerelease": true, "published_at": "2026-11-19T00:00:00Z", "assets": [] },
            { "tag_name": tag, "draft": false, "prerelease": false, "published_at": "2026-11-01T00:00:00Z", "assets": assets },
        ]))
        .unwrap()
    }

    fn lista(found: Found) -> Proposal {
        match found {
            Found::Ready(p) => *p,
            Found::Choose { assets, .. } => panic!("había que elegir: {assets:?}"),
        }
    }

    #[test]
    fn arma_la_receta() {
        let rs = releases("v1.1.1", &["Celeste64-v1.1.1-Linux-x64.zip", "Celeste64-v1.1.1-Windows-x64.zip"]);
        let p = lista(propose("EXOK/Celeste64", "added/exok-celeste64", &rs, W, None, None).unwrap());
        assert_eq!((p.tag.as_str(), p.asset.as_str(), p.size), ("v1.1.1", "Celeste64-v1.1.1-Windows-x64.zip", 1234));
        let r = &p.recipe;
        assert!(r.by_hand && r.repo == "EXOK/Celeste64" && r.slug() == "exok-celeste64");
        assert_eq!(r.release.assets[&W], "Celeste64-*-Windows-x64.zip");
        // Nada de lo que Unricopie no sabe de un juego que no conoce.
        use crate::install::recipe::CopyHow;
        assert_eq!(r.copy.how, CopyHow::None);
        assert!(r.copy.accepts.is_empty() && r.portable.is_empty() && r.self_update.is_none() && r.data_dir.is_empty() && r.mods.is_none());
        assert_eq!((r.checked.release.as_str(), r.checked.date.len()), ("v1.1.1", 10));
        // El programa todavía no se sabe; y se planea por el camino de siempre.
        let res = r.resolve(None, W).unwrap();
        assert_eq!((res.run, res.kind), (None, PackageKind::Archive));
        let lib = Library::new("/b");
        let plan = installer::plan_from(r, None, W, Channel::Untested, None, &rs, &lib).unwrap();
        assert_eq!((plan.version.as_str(), plan.asset.as_str(), plan.run), ("v1.1.1", "Celeste64-v1.1.1-Windows-x64.zip", None));
        assert_eq!(plan.dest, Path::new("/b").join("games").join("exok-celeste64"));
        // La versión siguiente encaja con el mismo patrón.
        let nuevas = releases("v1.2.0", &["Celeste64-v1.2.0-Windows-x64.zip"]);
        assert_eq!(installer::plan_from(r, None, W, Channel::Untested, None, &nuevas, &lib).unwrap().asset, "Celeste64-v1.2.0-Windows-x64.zip");
        // Vuelve a leerse igual, y la marca no se pierde.
        let otra_vez = Recipe::from_json(&serde_json::to_string(r).unwrap()).unwrap();
        assert_eq!(&otra_vez, r);
        // En Linux, el zip no guarda permisos: el programa se marca como ejecutable.
        let l = lista(propose("EXOK/Celeste64", "added/exok-celeste64", &rs, L, None, None).unwrap());
        assert_eq!(l.recipe.resolve(None, L).unwrap().after_extract, [Step::ChmodX]);
        // Con el programa ya elegido.
        let con = lista(propose("EXOK/Celeste64", "added/exok-celeste64", &rs, W, None, Some("bin\\Celeste64.exe")).unwrap());
        assert_eq!(con.recipe.release.run[&W], "bin/Celeste64.exe");
        assert!(matches!(propose("EXOK/Celeste64", "added/exok-celeste64", &rs, W, None, Some("../fuera.exe")), Err(Error::UnsafePath(_))));
        // Las recetas del catálogo no llevan la marca, y siguen leyéndose.
        assert!(!Recipe::from_json(crate::install::recipe::tests::ZELDA).unwrap().by_hand);
        assert!(!crate::install::recipe::tests::ZELDA.contains("by-hand"));
    }

    #[test]
    fn deja_elegir_el_archivo_y_rechaza_los_instaladores() {
        let names = ["juego-1.0-win64-dx11.zip", "juego-1.0-win64-vulkan.zip", "juego-1.0-setup.exe", "juego-1.0-linux.tar.gz", "juego-1.0.rar"];
        let rs = releases("1.0", &names);
        let Found::Choose { tag, assets } = propose("a/juego", "added/a-juego", &rs, W, None, None).unwrap() else { panic!() };
        assert_eq!((tag.as_str(), assets.candidates.len(), assets.rejected.len()), ("1.0", 2, 3));
        // La persona elige uno: el patrón excluye al otro, para que las actualizaciones no duden.
        let p = lista(propose("a/juego", "added/a-juego", &rs, W, Some("juego-1.0-win64-vulkan.zip"), None).unwrap());
        assert_eq!((p.recipe.release.assets[&W].as_str(), p.recipe.release.exclude.len()), ("juego-*-win64-vulkan.zip", 0));
        let plan = installer::plan_from(&p.recipe, None, W, Channel::Untested, None, &rs, &Library::new("/b")).unwrap();
        assert_eq!(plan.asset, "juego-1.0-win64-vulkan.zip");
        // Un instalador no, aunque se pida; ni un formato que no se sabe abrir; ni uno que no está.
        for (asset, kind) in [("juego-1.0-setup.exe", "Installer"), ("juego-1.0.rar", "Archive")] {
            let err = propose("a/juego", "added/a-juego", &rs, W, Some(asset), None).unwrap_err();
            assert!(matches!(&err, Error::UnsupportedPackage { asset: a, kind: k } if a == asset && k == kind), "{err}");
        }
        assert!(matches!(propose("a/juego", "added/a-juego", &rs, W, Some("otro.zip"), None), Err(Error::NoAsset { .. })));
        // Uno que por el nombre es de otro sistema sí, si la persona lo elige a sabiendas.
        let raro = lista(propose("a/juego", "added/a-juego", &rs, W, Some("juego-1.0-linux.tar.gz"), None).unwrap());
        assert_eq!(raro.recipe.release.assets[&W], "juego-*-linux.tar.gz");

        // Dos archivos que encajan con el mismo patrón: el otro queda excluido.
        let rs = releases("v2.0", &["dhewm3-2.0_win32.zip", "dhewm3-mods-2.0_win32.zip"]);
        let p = lista(propose("a/dhewm3", "added/a-dhewm3", &rs, W, Some("dhewm3-2.0_win32.zip"), None).unwrap());
        assert_eq!(p.recipe.release.assets[&Platform::WindowsX86], "dhewm3-*_win32.zip");
        assert_eq!(p.recipe.release.exclude, ["dhewm3-mods-*_win32.zip"]);
        assert_eq!(installer::plan_from(&p.recipe, None, W, Channel::Untested, None, &rs, &Library::new("/b")).unwrap().asset, "dhewm3-2.0_win32.zip");

        // Un .exe suelto elegido por la persona, y una AppImage: lo que se abre es el archivo.
        let rs = releases("v3", &["Juego-v3-x64.exe"]);
        let p = lista(propose("a/juego", "added/a-juego", &rs, W, Some("Juego-v3-x64.exe"), None).unwrap());
        let plan = installer::plan_from(&p.recipe, None, W, Channel::Untested, None, &rs, &Library::new("/b")).unwrap();
        assert_eq!((plan.kind, plan.run.as_deref()), (PackageKind::Portable, Some("Juego-v3-x64.exe")));
        let rs = releases("v3", &["Juego-v3-x86_64.AppImage"]);
        let p = lista(propose("a/juego", "added/a-juego", &rs, L, None, None).unwrap());
        let plan = installer::plan_from(&p.recipe, None, L, Channel::Untested, None, &rs, &Library::new("/b")).unwrap();
        assert_eq!((plan.kind, plan.run.as_deref()), (PackageKind::Appimage, Some("Juego-v3-x86_64.AppImage")));

        // Sin release oficial (solo una de prueba, o ninguna).
        let solo_beta: Vec<GhRelease> = releases("x", &[]).into_iter().filter(|r| r.prerelease).collect();
        assert!(matches!(propose("a/juego", "added/a-juego", &solo_beta, W, None, None), Err(Error::NoRelease { .. })));
        assert!(matches!(propose("a/juego", "added/a-juego", &[], W, None, None), Err(Error::NoRelease { .. })));
    }

    fn programa(files: &[&str], platform: Platform, repo: &str) -> Runs {
        find_run(Path::new("/no-existe"), files, platform, repo)
    }

    #[test]
    fn encuentra_el_programa_en_windows() {
        // Uno solo.
        let r = programa(&["Celeste64.exe", "Content/a.bin", "SDL2.dll"], W, "EXOK/Celeste64");
        assert_eq!(r.chosen.as_deref(), Some("Celeste64.exe"));
        // Con ayudantes alrededor: no cuentan, ni como candidatos.
        let con_ayudantes = [
            "Juego.exe", "unins000.exe", "Uninstall.exe", "vc_redist.x64.exe", "VC_redist.x86.exe", "crashpad_handler.exe", "UnityCrashHandler64.exe", "CrashReportClient.exe", "JuegoUpdater.exe", "updater.exe", "Update.exe",
            "_CommonRedist/DirectX/DXSETUP.exe", "redist/oalinst.exe", "dotnet-runtime-8.0-win-x64.exe", "setup.exe", "notification_helper.exe",
        ];
        let r = programa(&con_ayudantes, W, "otro/nombre");
        assert_eq!((r.chosen.as_deref(), r.candidates.len()), (Some("Juego.exe"), 1));
        // Varios: el que se llama como el repo, esté donde esté y se escriba como se escriba.
        let r = programa(&["tools/editor.exe", "bin/Space-Cadet_Pinball.exe", "launcher.exe", "config.exe"], W, "k4zmu2a/SpaceCadetPinball");
        assert_eq!(r.chosen.as_deref(), Some("bin/Space-Cadet_Pinball.exe"));
        // Varios y ninguno se llama así: el único de arriba.
        let r = programa(&["tools/editor.exe", "tools/pack.exe", "juego.exe"], W, "a/otro");
        assert_eq!(r.chosen.as_deref(), Some("juego.exe"));
        assert_eq!(r.candidates, ["juego.exe", "tools/editor.exe", "tools/pack.exe"]);
        // Dos arriba: no se adivina, y se dan todos, los de arriba primero.
        let r = programa(&["tools/editor.exe", "juego.exe", "juego-dx11.exe"], W, "a/otro");
        assert_eq!(r.chosen, None);
        assert_eq!(r.candidates, ["juego-dx11.exe", "juego.exe", "tools/editor.exe"]);
        // Ninguno (un paquete solo de datos, o solo ayudantes).
        assert_eq!(programa(&["datos.pak", "leeme.txt", "unins000.exe"], W, "a/b"), Runs::default());
    }

    #[test]
    fn encuentra_el_programa_en_linux_y_macos() {
        let t = tempfile::tempdir().unwrap();
        let escribir = |name: &str, data: &[u8]| {
            let p = RelPath::new(name).unwrap().under(t.path());
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, data).unwrap();
        };
        for (name, data) in [("juego", &b"\x7fELF\x02\x01"[..]), ("lib/libSDL2-2.0.so.0", b"\x7fELF"), ("libsteam_api.so", b"\x7fELF"), ("juego.sh", b"#!/bin/sh\n"), ("datos.pak", b"PACK"), ("vacio", b""), ("crashpad_handler", b"\x7fELF")] {
            escribir(name, data);
        }
        let files = ["juego", "lib/libSDL2-2.0.so.0", "libsteam_api.so", "juego.sh", "datos.pak", "vacio", "crashpad_handler", "no-esta"];
        // El único ELF que no es una librería ni un ayudante.
        let r = find_run(t.path(), &files, L, "a/otro");
        assert_eq!((r.chosen.as_deref(), r.candidates.len()), (Some("juego"), 1));
        // Dos ELF arriba: elige la persona, salvo que uno se llame como el repo.
        escribir("editor", b"\x7fELF");
        let files = ["juego", "editor"];
        assert_eq!(find_run(t.path(), &files, L, "a/otro").chosen, None);
        assert_eq!(find_run(t.path(), &files, L, "a/Juego").chosen.as_deref(), Some("juego"));
        // Una AppImage manda sobre lo demás.
        let r = find_run(t.path(), &["juego", "Juego-x86_64.AppImage"], L, "a/otro");
        assert_eq!((r.chosen.as_deref(), r.candidates.len()), (Some("Juego-x86_64.AppImage"), 1));

        // macOS: la .app de más afuera, no las que lleva dentro.
        let app = ["Juego.app/Contents/MacOS/juego", "Juego.app/Contents/Info.plist", "Juego.app/Contents/Frameworks/Ayudante.app/Contents/MacOS/ayudante", "LEEME.txt"];
        let r = programa(&app, M, "a/otro");
        assert_eq!((r.chosen.as_deref(), r.candidates.len()), (Some("Juego.app"), 1));
        let dos = ["Juego.app/Contents/MacOS/juego", "Editor.app/Contents/MacOS/editor"];
        assert_eq!(programa(&dos, M, "a/otro").chosen, None);
        assert_eq!(programa(&dos, M, "a/juego").chosen.as_deref(), Some("Juego.app"));
        // Un archivo que se llama `algo.app` no es una app.
        assert_eq!(programa(&["datos/algo.app"], M, "a/b"), Runs::default());
    }

    /// La receta, el plan (sin `run`) y la biblioteca de una release inventada con un zip.
    fn preparado(t: &Path, repo: &str, asset: &str) -> (Library, Recipe, Plan) {
        let lib = Library::new(t);
        let rs = releases("v1.0", &[asset]);
        let p = lista(propose(repo, &entry_for(repo), &rs, W, Some(asset), None).unwrap());
        let plan = installer::plan_from(&p.recipe, None, W, Channel::Untested, None, &rs, &lib).unwrap();
        assert_eq!(plan.run, None);
        (lib, p.recipe, plan)
    }

    #[test]
    fn instala_buscando_el_programa() {
        let t = tempfile::tempdir().unwrap();
        let (lib, receta, plan) = preparado(t.path(), "EXOK/Celeste64", "Celeste64-v1.0-Windows-x64.zip");
        let z = t.path().join("bajado.zip");
        // Todo dentro de una carpeta con la versión en el nombre, como tantos zip.
        zip_with(&z, &[("Celeste64-v1.0/Celeste64.exe", b"exe"), ("Celeste64-v1.0/Content/a.bin", b"a"), ("Celeste64-v1.0/unins000.exe", b"u")]);
        let got = Downloaded { size: 3, sha256: "abc".into(), scan: None };
        let (recibo, con_run) = install_downloaded(&lib, &receta, &plan, &z, &got).unwrap();
        // La carpeta de dentro es la raíz: el programa queda sin ella, y así la versión siguiente
        // (`Celeste64-v1.1/…`) lo encuentra igual.
        assert_eq!(recibo.run.as_deref(), Some("Celeste64.exe"));
        assert!(plan.dest.join("Celeste64.exe").is_file() && plan.dest.join("Content").join("a.bin").is_file());
        let files: Vec<&str> = recibo.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["Celeste64.exe", "Content/a.bin", "unins000.exe"]);
        let leido = Receipt::read(&plan.dest).unwrap().unwrap();
        assert_eq!(leido, recibo);
        assert_eq!(leido.check_run(&plan.dest).unwrap(), RunState::Intact);
        assert_eq!((leido.entry.as_str(), leido.repo.as_str()), ("added/exok-celeste64", "EXOK/Celeste64"));
        // La receta que vuelve ya dice qué se abre: desde ahora va por el camino de siempre.
        assert_eq!(con_run.release.run[&W], "Celeste64.exe");
        assert_eq!(con_run.resolve(None, W).unwrap().run.as_deref(), Some("Celeste64.exe"));
        // No queda nada temporal, y no se instala dos veces.
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 1);
        assert!(matches!(install_downloaded(&lib, &receta, &plan, &z, &got), Err(Error::AlreadyInstalled(_))));

        // La actualización, con la receta guardada y otra carpeta dentro del zip: el instalador de
        // siempre encuentra el programa.
        let rs = releases("v1.1", &["Celeste64-v1.1-Windows-x64.zip"]);
        let plan2 = installer::plan_from(&con_run, None, W, Channel::Untested, None, &rs, &lib).unwrap();
        assert_eq!(plan2.run.as_deref(), Some("Celeste64.exe"));
        let z2 = t.path().join("bajado2.zip");
        zip_with(&z2, &[("Celeste64-v1.1/Celeste64.exe", b"exe nuevo"), ("Celeste64-v1.1/Content/a.bin", b"a")]);
        let hecho = crate::install::update::update_downloaded(&lib, &plan2, &z2, &got).unwrap();
        assert_eq!(hecho.receipt.version, "v1.1");
        assert_eq!(fs::read(plan.dest.join("Celeste64.exe")).unwrap(), b"exe nuevo");
    }

    #[test]
    fn si_no_sabe_que_programa_es_no_instala_nada() {
        let t = tempfile::tempdir().unwrap();
        let (lib, receta, plan) = preparado(t.path(), "a/juego", "juego-v1.0-win64.zip");
        let z = t.path().join("bajado.zip");
        zip_with(&z, &[("dx11.exe", b"a"), ("vulkan.exe", b"b"), ("datos/x.bin", b"c")]);
        let got = Downloaded { size: 3, sha256: "abc".into(), scan: None };
        let err = install_downloaded(&lib, &receta, &plan, &z, &got).unwrap_err();
        assert!(matches!(&err, Error::ChooseRun { candidates } if candidates == &["dx11.exe", "vulkan.exe"]), "{err}");
        assert!(!plan.dest.exists());
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 0);
        // Sin ningún programa dentro, lo mismo, sin candidatos.
        let z = t.path().join("datos.zip");
        zip_with(&z, &[("datos/x.bin", b"c"), ("leeme.txt", b"d")]);
        assert!(matches!(install_downloaded(&lib, &receta, &plan, &z, &got), Err(Error::ChooseRun { candidates }) if candidates.is_empty()));
        assert_eq!(fs::read_dir(&lib.games).unwrap().count(), 0);

        // La persona elige: con el programa en la receta, instala el instalador de siempre.
        let rs = releases("v1.0", &["juego-v1.0-win64.zip"]);
        let elegida = lista(propose("a/juego", "added/a-juego", &rs, W, Some("juego-v1.0-win64.zip"), Some("vulkan.exe")).unwrap()).recipe;
        let plan = installer::plan_from(&elegida, None, W, Channel::Untested, None, &rs, &lib).unwrap();
        let z = t.path().join("bajado.zip");
        let recibo = installer::install_downloaded(&lib, &plan, &z, &got).unwrap();
        assert_eq!(recibo.run.as_deref(), Some("vulkan.exe"));
        // Y un paquete que ya trae un recibo no cuela por este camino tampoco.
        let (lib2, receta2, plan2) = preparado(&t.path().join("otra"), "a/juego", "juego-v1.0-win64.zip");
        fs::create_dir_all(&lib2.games).unwrap();
        let z = t.path().join("con-recibo.zip");
        zip_with(&z, &[("juego/juego.exe", b"a"), ("juego/unricopie.json", b"{}")]);
        assert!(matches!(install_downloaded(&lib2, &receta2, &plan2, &z, &got), Err(Error::PackageHasReceipt(_))));
        assert!(!plan2.dest.exists());
    }

    #[test]
    fn guarda_y_quita_las_anadidas() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path().join("biblioteca"));
        assert_eq!(lib.added, t.path().join("biblioteca").join("added.json"));
        // Sin archivo no hay ninguna, y quitar una que no está no hace nada.
        assert!(load(&lib).unwrap().is_empty());
        assert_eq!(remove(&lib, "added/a-b").unwrap(), None);
        assert!(!lib.added.exists());

        let receta = |repo: &str| lista(propose(repo, &entry_for(repo), &releases("v1", &["juego-v1-win64.zip"]), W, None, None).unwrap()).recipe;
        let (a, b) = (receta("a/uno"), receta("b/dos"));
        add(&lib, &a).unwrap();
        add(&lib, &b).unwrap();
        assert_eq!(load(&lib).unwrap(), [a.clone(), b.clone()]);
        // Añadir otra vez la misma ficha la sustituye (ahora con el programa), sin repetirla.
        let mut a2 = a.clone();
        a2.release.run.insert(W, "uno.exe".into());
        add(&lib, &a2).unwrap();
        let todas = load(&lib).unwrap();
        assert_eq!(todas.iter().map(|r| r.entry.as_str()).collect::<Vec<_>>(), ["added/b-dos", "added/a-uno"]);
        assert_eq!(todas[1].release.run[&W], "uno.exe");
        assert!(todas.iter().all(|r| r.by_hand));
        // No queda nada temporal junto al archivo.
        let junto: Vec<_> = fs::read_dir(lib.added.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(junto, [std::ffi::OsString::from("added.json")]);

        assert_eq!(remove(&lib, "added/b-dos").unwrap(), Some(b));
        assert_eq!(load(&lib).unwrap(), [a2]);

        // Una receta que no se entiende se salta; un archivo roto o de un formato más nuevo es un error.
        fs::write(&lib.added, r#"{ "format": 1, "recipes": [{ "entry": "x" }, { "entry": "added/a-b", "repo": "a/b", "release": {}, "copy": { "how": "none" }, "checked": { "date": "", "release": "", "sources": [] } }] }"#).unwrap();
        let leidas = load(&lib).unwrap();
        assert_eq!(leidas.len(), 1);
        assert!(leidas[0].by_hand, "lo de este archivo es siempre a mano");
        fs::write(&lib.added, "{").unwrap();
        assert!(matches!(load(&lib), Err(Error::Config { .. })));
        fs::write(&lib.added, r#"{ "format": 2, "recipes": [] }"#).unwrap();
        assert!(matches!(load(&lib), Err(Error::Config { .. })));
    }

    #[test]
    #[ignore = "usa la red (API de GitHub, sin clave)"]
    fn propone_celeste64_de_verdad() {
        let rs = release::list(&crate::net::agent(), "EXOK/Celeste64").unwrap();
        let p = lista(propose("EXOK/Celeste64", "added/exok-celeste64", &rs, W, None, None).unwrap());
        eprintln!("{} {} {:?}", p.tag, p.asset, p.recipe.release.assets);
        assert!(p.asset.contains("Windows") || p.asset.contains("win"));
    }
}
