//! El banco de pruebas de Unricopie. Con una biblioteca aparte (no la del jugador):
//!
//! - `instalar`: instala cada receta del catálogo desde su última release oficial, con todas
//!   las comprobaciones del núcleo (tamaño, SHA-256 de GitHub, descompresión segura, ejecutable).
//! - `copias`: busca la copia de cada juego instalado entre las ROM de una carpeta (solo la lee) y
//!   la importa solo si coincide con los hashes de la receta. Si la receta no trae hashes, no
//!   adivina: anota las candidatas por nombre para revisarlas a mano.
//! - `abrir`: abre cada juego como lo haría el launcher (copia en su sitio, actualizador apagado,
//!   portable), espera unos segundos y mira si sigue abierto. Lo ejecuta la persona.
//!
//! Lo que pasa todo eso es lo que irá al canal estable. Los resultados se guardan en
//! `<biblioteca>/banco/resultado.json` después de cada juego, así que se puede cortar y seguir.
//!
//! Uso:
//!   unricopie-banco instalar --biblioteca D:\Banco --catalogo catalogo.json [--solo a,b]
//!   unricopie-banco copias   --biblioteca D:\Banco --catalogo catalogo.json --roms D:\Copias
//!   unricopie-banco abrir    --biblioteca D:\Banco --catalogo catalogo.json [--segundos 25]
//!   unricopie-banco limpiar  --biblioteca D:\Banco --catalogo catalogo.json
//! La biblioteca va en un disco interno: la escritura es pesada y un disco USB puede no aguantarla.
//! `abrir` anota lo que cada juego deja en AppData o Documentos; `limpiar` lo mueve al banco.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use unricopie_nucleo::install::copy;
use unricopie_nucleo::install::recipe::{CopyHow, CopyKind};
use unricopie_nucleo::install::{self as inst, launch, place, portable, settings, Channel, Library, Platform, Receipt, Recipe};
use unricopie_nucleo::{net, Error};

/// Lo que se apunta de cada paso.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Paso {
    ok: bool,
    /// Segundos desde 1970 (UTC).
    cuando: u64,
    detalle: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    datos: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Resultado {
    nombre: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instalar: Option<Paso>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copia: Option<Paso>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abrir: Option<Paso>,
}

struct Args {
    orden: String,
    biblioteca: PathBuf,
    catalogo: PathBuf,
    roms: Option<PathBuf>,
    solo: Vec<String>,
    segundos: u64,
    /// `copias`: probar también las candidatas cuyo nombre no se parece al del juego.
    a_fondo: bool,
    /// `--variante <juego>`: en las recetas que la tienen (UZDoom con `doom`), esa variante.
    variante: Option<String>,
    /// `copias --desde <ruta>`: importa esa copia (una carpeta de GOG o Steam, una ROM suelta).
    desde: Option<PathBuf>,
    /// Con `--desde`: si la receta no trae hashes, la guarda igual, como no comprobada.
    sin_comprobar: bool,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let orden = it.next().ok_or("falta la orden: instalar, copias, abrir o limpiar")?;
    let (mut biblioteca, mut catalogo, mut roms, mut solo, mut segundos, mut a_fondo, mut variante) = (None, None, None, Vec::new(), 25, false, None);
    let (mut desde, mut sin_comprobar) = (None, false);
    while let Some(a) = it.next() {
        let mut valor = || it.next().ok_or(format!("a {a} le falta el valor"));
        match a.as_str() {
            "--biblioteca" => biblioteca = Some(PathBuf::from(valor()?)),
            "--catalogo" => catalogo = Some(PathBuf::from(valor()?)),
            "--roms" => roms = Some(PathBuf::from(valor()?)),
            "--solo" => solo = valor()?.split(',').map(str::trim).map(String::from).collect(),
            "--segundos" => segundos = valor()?.parse().map_err(|_| "--segundos no es un número")?,
            "--a-fondo" => a_fondo = true,
            "--variante" => variante = Some(valor()?),
            "--desde" => desde = Some(PathBuf::from(valor()?)),
            "--sin-comprobar" => sin_comprobar = true,
            otro => return Err(format!("no conozco {otro}")),
        }
    }
    Ok(Args {
        orden,
        biblioteca: biblioteca.ok_or("falta --biblioteca")?,
        catalogo: catalogo.ok_or("falta --catalogo")?,
        roms,
        solo,
        segundos,
        a_fondo,
        variante,
        desde,
        sin_comprobar,
    })
}

fn ahora() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn paso(ok: bool, detalle: impl Into<String>) -> Paso {
    Paso { ok, cuando: ahora(), detalle: detalle.into(), datos: BTreeMap::new() }
}

/// Una receta del catálogo de prueba, con el nombre del juego.
struct Entrada {
    nombre: String,
    receta: Recipe,
}

fn leer_catalogo(path: &Path) -> Result<Vec<Entrada>, String> {
    #[derive(Deserialize)]
    struct E {
        nombre: String,
        receta: serde_json::Value,
    }
    let texto = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let es: Vec<E> = serde_json::from_str(&texto).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for e in es {
        match Recipe::from_json(&e.receta.to_string()) {
            Ok(receta) => out.push(Entrada { nombre: e.nombre, receta }),
            Err(err) => eprintln!("receta que no se puede leer: {err}"),
        }
    }
    Ok(out)
}

struct Banco {
    lib: Library,
    variante: Option<String>,
    archivo: PathBuf,
    res: BTreeMap<String, Resultado>,
}

impl Banco {
    fn abrir(biblioteca: &Path) -> Banco {
        let archivo = biblioteca.join("banco").join("resultado.json");
        let res = fs::read(&archivo).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Banco { lib: Library::new(biblioteca), variante: None, archivo, res }
    }

    fn guardar(&self) {
        if let Some(d) = self.archivo.parent() {
            let _ = fs::create_dir_all(d);
        }
        let tmp = self.archivo.with_extension("json.tmp");
        if fs::write(&tmp, serde_json::to_vec_pretty(&self.res).unwrap_or_default()).is_ok() {
            let _ = fs::rename(&tmp, &self.archivo);
        }
    }

    /// La variante pedida, si esta receta la tiene; si no, la instalación base.
    fn var(&self, r: &Recipe) -> Option<String> {
        self.variante.clone().filter(|v| r.variants.iter().any(|x| &x.game == v))
    }

    fn de(&mut self, e: &Entrada) -> &mut Resultado {
        let clave = match self.var(&e.receta) {
            Some(v) => format!("{}--{v}", e.receta.entry),
            None => e.receta.entry.clone(),
        };
        let r = self.res.entry(clave).or_default();
        r.nombre = e.nombre.clone();
        r
    }
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\nUso: unricopie-banco instalar|copias|abrir|limpiar --biblioteca <carpeta> --catalogo <json> [--roms <carpeta>] [--solo a,b] [--segundos N] [--variante juego] [--a-fondo]");
            std::process::exit(2);
        }
    };
    let entradas = match leer_catalogo(&a.catalogo) {
        Ok(es) => es.into_iter().filter(|e| a.solo.is_empty() || a.solo.iter().any(|s| e.receta.entry.ends_with(s.as_str()))).collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut banco = Banco::abrir(&a.biblioteca);
    banco.variante = a.variante.clone();
    match a.orden.as_str() {
        "instalar" => instalar(&mut banco, &entradas),
        "copias" if a.roms.is_none() && a.desde.is_none() => eprintln!("copias necesita --roms <carpeta> o --desde <ruta>"),
        "copias" => copias(&mut banco, &entradas, a.roms.as_deref(), a.desde.as_deref(), a.sin_comprobar, a.a_fondo),
        "abrir" => abrir(&mut banco, &entradas, a.segundos),
        "limpiar" => limpiar(&mut banco, &a.biblioteca),
        otro => eprintln!("no conozco la orden {otro}: instalar, copias, abrir o limpiar"),
    }
}

// --- instalar ---

fn instalar(banco: &mut Banco, entradas: &[Entrada]) {
    let Some(host) = Platform::current() else {
        eprintln!("este sistema no tiene plataforma");
        return;
    };
    let agent = net::agent();
    for (i, e) in entradas.iter().enumerate() {
        let r = &e.receta;
        let var = banco.var(r);
        print!("[{}/{}] {}{} … ", i + 1, entradas.len(), r.entry, var.as_deref().map(|v| format!(" ({v})")).unwrap_or_default());
        let dir = banco.lib.game_dir(r, var.as_deref());
        if let Ok(Some(rec)) = Receipt::read(&dir) {
            println!("ya instalado ({})", rec.version);
            continue;
        }
        match r.resolve(var.as_deref(), host) {
            Err(err) => {
                println!("se salta: {err}");
                banco.de(e).instalar = Some(paso(false, format!("sin versión: {err}")));
                banco.guardar();
                continue;
            }
            Ok(res) if res.run.is_none() => {
                println!("se salta: la receta no tiene run");
                banco.de(e).instalar = Some(paso(false, "la receta no tiene run"));
                banco.guardar();
                continue;
            }
            Ok(_) => {}
        }
        let t = Instant::now();
        let resultado = inst::plan(&agent, r, var.as_deref(), host, Channel::Untested, None, &banco.lib).and_then(|plan| {
            let mut ultimo = 0u64;
            let rec = inst::install(&agent, &banco.lib, &plan, &mut |b, total| {
                if let Some(total) = total {
                    let pct = b * 10 / total.max(1);
                    if pct > ultimo {
                        ultimo = pct;
                        print!("{}% ", pct * 10);
                    }
                }
            })?;
            Ok((plan, rec))
        });
        let p = match resultado {
            Ok((plan, rec)) => {
                println!("instalado {} ({} MB, {:.0} s)", rec.version, rec.size / 1_048_576, t.elapsed().as_secs_f32());
                let mut p = paso(true, format!("{} instalado", rec.version));
                p.datos.insert("version".into(), rec.version.clone());
                p.datos.insert("archivo".into(), plan.asset.clone());
                p.datos.insert("tamano".into(), rec.size.to_string());
                p.datos.insert("sha256_github".into(), rec.sha256_verified.to_string());
                p.datos.insert("archivos".into(), rec.files.len().to_string());
                p.datos.insert("segundos".into(), t.elapsed().as_secs().to_string());
                p
            }
            Err(err) => {
                println!("FALLA: {err}");
                paso(false, err.to_string())
            }
        };
        banco.de(e).instalar = Some(p);
        banco.guardar();
    }
}

// --- copias ---

/// Las subcarpetas de `--roms` donde se buscan las copias de cada plataforma de las recetas.
fn carpetas(plataforma: &str) -> &'static [&'static str] {
    match plataforma {
        "n64" => &["Nintendo64"],
        "nes" => &["NintendoEntertainmentSystem"],
        "snes" => &["NintendoSuperNintendo"],
        "gb" => &["NintendoGameBoy"],
        "gbc" => &["NintendoGameBoyColor"],
        "gba" => &["NintendoGameBoyAdvance"],
        "gamecube" | "gc" => &["NintendoGameCube"],
        "wii" => &["NintendoWii"],
        "wiiu" => &["NintendoWiiU"],
        "switch" => &["NintendoSwitch"],
        "ps1" => &["PlayStation"],
        "ps2" => &["PlayStation2"],
        "ps4" => &["PlayStation4"],
        "x360" => &["Xbox360"],
        "xbox" => &["Xbox"],
        "pc" | "dos" => &["Windows", "GZDoom"],
        _ => &[],
    }
}

/// Las palabras de un nombre, para ordenar las candidatas: «Legend of Zelda, The - Majora's Mask» → {legend, zelda, majora, mask}.
fn palabras(s: &str) -> BTreeSet<String> {
    const VACIAS: &[&str] = &["the", "of", "and", "a", "de", "la", "el", "los", "las", "y"];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !VACIAS.contains(w))
        .map(|w| w.trim_end_matches('s').to_string())
        .collect()
}

/// Los archivos (y, si la copia es una carpeta, las carpetas) de esas carpetas, hasta dos niveles,
/// de los que más se parecen al nombre del juego a los que menos.
fn candidatas(roms: &Path, plataformas: &BTreeSet<String>, nombre: &str, carpeta: bool) -> Vec<(usize, PathBuf)> {
    let quiero = palabras(nombre);
    let mut out = Vec::new();
    for p in plataformas {
        for c in carpetas(p) {
            let base = roms.join(c);
            let Ok(nivel1) = fs::read_dir(&base) else { continue };
            for a in nivel1.flatten() {
                let path = a.path();
                let mut mirar = vec![path.clone()];
                if path.is_dir() && !carpeta {
                    if let Ok(nivel2) = fs::read_dir(&path) {
                        mirar.extend(nivel2.flatten().map(|b| b.path()));
                    }
                }
                for m in mirar {
                    if m.is_dir() != carpeta {
                        continue;
                    }
                    let nombre_archivo = m.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let parecido = palabras(&nombre_archivo).intersection(&quiero).count();
                    out.push((parecido, m));
                }
            }
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.0));
    out
}

/// Lo que tiene que quedar libre en el disco de la biblioteca después de importar una copia.
const MARGEN: u64 = 10 << 30;

/// El tamaño de un archivo, o de todo lo que hay dentro de una carpeta.
fn tamano(p: &Path) -> u64 {
    match fs::metadata(p) {
        Ok(m) if m.is_file() => m.len(),
        Ok(m) if m.is_dir() => fs::read_dir(p).map(|it| it.flatten().map(|e| tamano(&e.path())).sum()).unwrap_or(0),
        _ => 0,
    }
}

/// Los bytes libres del disco donde está `p`, preguntados a PowerShell (el banco no usa
/// `unsafe`, y la API de Windows lo pediría).
#[cfg(windows)]
fn libre(p: &Path) -> Option<u64> {
    use std::path::{Component, Prefix};
    let letra = match p.components().next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => l as char,
            _ => return None,
        },
        _ => return None,
    };
    let salida = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("(Get-PSDrive -Name {letra}).Free")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&salida.stdout).trim().parse().ok()
}

#[cfg(not(windows))]
fn libre(_: &Path) -> Option<u64> {
    None
}

fn copias(banco: &mut Banco, entradas: &[Entrada], roms: Option<&Path>, desde: Option<&Path>, sin_comprobar: bool, a_fondo: bool) {
    for e in entradas {
        let r = &e.receta;
        let var = banco.var(r);
        let dir = banco.lib.game_dir(r, var.as_deref());
        if !matches!(Receipt::read(&dir), Ok(Some(_))) {
            continue;
        }
        print!("{} … ", r.entry);
        let regla = r.copy_rule(var.as_deref());
        let p = match regla.how {
            CopyHow::None => paso(true, "no hace falta copia"),
            CopyHow::OwnInstaller => paso(false, "la pide su propio instalador: a mano"),
            _ if copy::imported(&banco.lib, r, var.as_deref()).is_some_and(|i| i.verified) => paso(true, "ya estaba importada y comprobada"),
            _ => match (desde, roms) {
                (Some(d), _) => importar_desde(&banco.lib, e, var.as_deref(), d, sin_comprobar),
                (None, Some(roms)) => buscar_copia(&banco.lib, e, var.as_deref(), roms, a_fondo),
                (None, None) => paso(false, "sin --roms ni --desde"),
            },
        };
        println!("{}", p.detalle);
        banco.de(e).copia = Some(p);
        banco.guardar();
    }
}

/// `--desde <ruta>`: importa esa copia concreta. Si la receta trae hashes, tiene que coincidir;
/// si no trae ninguno, solo se guarda con `--sin-comprobar`, y queda como no comprobada.
fn importar_desde(lib: &Library, e: &Entrada, var: Option<&str>, desde: &Path, sin_comprobar: bool) -> Paso {
    let r = &e.receta;
    let regla = r.copy_rule(var);
    let lista = match &regla.accepts_from {
        Some(from) => match copy::fetch_accepts(&net::agent(), from) {
            Ok(a) => a,
            Err(err) => return paso(false, format!("no se pudo leer la lista de copias aceptadas: {err}")),
        },
        None => Vec::new(),
    };
    let aceptadas: Vec<_> = regla.accepts.iter().chain(&lista).cloned().collect();
    let sin_hashes = copy::algos_for(&aceptadas).is_empty();
    if sin_hashes && !sin_comprobar {
        return paso(false, "la receta no trae hashes: hace falta --sin-comprobar");
    }
    let tam = tamano(desde);
    if libre(&lib.copies).is_some_and(|l| l < tam + MARGEN) {
        return paso(false, format!("no queda espacio para importar {} ({} MB) dejando {} GB libres", desde.display(), tam >> 20, MARGEN >> 30));
    }
    // Solo se acepta sin comprobar cuando no hay con qué comparar: si hay hashes, tiene que coincidir.
    match copy::import_with(lib, r, var, desde, &lista, sin_comprobar && sin_hashes, &mut |_| {}) {
        Ok(i) => {
            let mut p = paso(true, if i.verified { format!("comprobada e importada: {}", i.accepted.join(", ")) } else { if sin_hashes { "importada sin comprobar: la receta no trae hashes".to_string() } else { "importada sin comprobar: ningún archivo de la receta se pudo comparar (¿falta file en sus accepts?)".to_string() } });
            p.datos.insert("origen".into(), desde.display().to_string());
            p.datos.insert("comprobada".into(), i.verified.to_string());
            p
        }
        Err(err) => paso(false, format!("no se pudo importar {}: {err}", desde.display())),
    }
}

fn buscar_copia(lib: &Library, e: &Entrada, var: Option<&str>, roms: &Path, a_fondo: bool) -> Paso {
    let r = &e.receta;
    let regla = r.copy_rule(var);
    // La lista que publica el proyecto (accepts-from) se suma a la de la receta.
    let lista = match &regla.accepts_from {
        Some(from) => match copy::fetch_accepts(&net::agent(), from) {
            Ok(a) => a,
            Err(err) => return paso(false, format!("no se pudo leer la lista de copias aceptadas: {err}")),
        },
        None => Vec::new(),
    };
    let aceptadas: Vec<_> = regla.accepts.iter().chain(&lista).cloned().collect();
    let algos = copy::algos_for(&aceptadas);
    let mut plataformas: BTreeSet<String> = aceptadas.iter().filter_map(|a| a.platform.clone()).collect();
    if plataformas.is_empty() {
        plataformas.extend(["pc".to_string()]);
    }
    let es_carpeta = regla.kind == Some(CopyKind::Folder);
    let cands = candidatas(roms, &plataformas, &e.nombre, es_carpeta);
    if algos.is_empty() {
        // Sin hashes no hay con qué comprobar: no se importa nada, se apuntan las candidatas.
        let mejores: Vec<String> = cands.iter().filter(|(n, _)| *n > 0).take(3).map(|(_, p)| p.display().to_string()).collect();
        let mut p = paso(false, if mejores.is_empty() { "la receta no trae hashes y no hay candidatas por nombre".to_string() } else { "la receta no trae hashes: candidatas por nombre, sin importar".to_string() });
        for (i, m) in mejores.iter().enumerate() {
            p.datos.insert(format!("candidata{}", i + 1), m.clone());
        }
        return p;
    }
    // Con hashes: las candidatas cuyo nombre se parece al del juego (con --a-fondo, todas). El
    // núcleo comprueba cada una leyéndola donde está y solo copia la que coincide.
    let mut probadas = 0;
    for (parecido, cand) in &cands {
        if *parecido == 0 && !a_fondo {
            continue;
        }
        if cand.is_file() && fs::metadata(cand).map(|m| m.len() > 8 << 30).unwrap_or(true) {
            continue;
        }
        // Si coincide se copia a la biblioteca: tiene que caber dejando margen en el disco.
        let tam = tamano(cand);
        if libre(&lib.copies).is_some_and(|l| l < tam + MARGEN) {
            return paso(false, format!("no queda espacio para importar {} ({} MB) dejando {} GB libres", cand.display(), tam >> 20, MARGEN >> 30));
        }
        probadas += 1;
        match copy::import_with(lib, r, var, cand, &lista, false, &mut |_| {}) {
            Ok(i) if i.verified => {
                let mut p = paso(true, format!("comprobada e importada: {}", i.accepted.join(", ")));
                p.datos.insert("origen".into(), cand.display().to_string());
                return p;
            }
            Ok(_) => {
                // Sin hashes con que comparar no debería pasar aquí; si pasa, no se deja guardada.
                let _ = std::fs::remove_dir_all(lib.copy_dir(r, var));
            }
            Err(Error::CopyMismatch(_)) | Err(Error::NotARom(_)) | Err(Error::CopyWantsFile(_)) | Err(Error::CopyWantsFolder(_)) => {}
            Err(err) => eprintln!("  {}: {err}", cand.display()),
        }
    }
    paso(false, if probadas == 0 {
        "no hay candidatas con un nombre parecido (con --a-fondo se prueban todas)".to_string()
    } else {
        format!("ninguna de {probadas} candidatas coincide con los hashes de la receta")
    })
}

// --- abrir ---

fn abrir(banco: &mut Banco, entradas: &[Entrada], segundos: u64) {
    for e in entradas {
        let r = &e.receta;
        let var = banco.var(r);
        let dir = banco.lib.game_dir(r, var.as_deref());
        if !matches!(Receipt::read(&dir), Ok(Some(_))) {
            continue;
        }
        println!("{} …", r.entry);
        let nombre = match &var {
            Some(v) => format!("{}--{v}", r.entry.replace('/', "_")),
            None => r.entry.replace('/', "_"),
        };
        let foto_pantalla = banco.archivo.with_file_name("capturas").join(format!("{nombre}.png"));
        let p = abrir_uno(&banco.lib, r, &dir, segundos, &foto_pantalla);
        println!("  {}", p.detalle);
        banco.de(e).abrir = Some(p);
        banco.guardar();
    }
}

/// Captura de toda la pantalla principal, con PowerShell y System.Drawing (el banco no usa
/// `unsafe`). Se marca el proceso como consciente del escalado para que salga entera.
fn captura(dest: &Path) -> bool {
    if let Some(d) = dest.parent() {
        let _ = fs::create_dir_all(d);
    }
    let ruta = dest.display().to_string().replace('\'', "''");
    let ps = format!(
        r#"Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();' -Name Dpi -Namespace Banco; [Banco.Dpi]::SetProcessDPIAware() | Out-Null; Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds; $i = New-Object System.Drawing.Bitmap $b.Width, $b.Height; $g = [System.Drawing.Graphics]::FromImage($i); $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size); $i.Save('{ruta}', [System.Drawing.Imaging.ImageFormat]::Png)"#
    );
    std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps]).status().is_ok_and(|s| s.success()) && dest.exists()
}

/// Si todo lo que `copy.place` escribe va dentro de la carpeta del juego (`{game}`), se puede
/// poner la copia aunque el juego no sea portable: no toca las carpetas de la persona.
fn copia_solo_en_el_juego(r: &Recipe) -> bool {
    let en_el_juego = |f: &String| f == "{game}" || f.starts_with("{game}/");
    r.copy.place.iter().chain(r.variants.iter().filter_map(|v| v.copy.as_ref()).flat_map(|c| &c.place)).all(|p| {
        p.store.iter().chain(&p.files).chain(&p.when_missing).chain(p.config.as_ref().map(|c| &c.file)).all(en_el_juego)
    })
}

fn abrir_uno(lib: &Library, r: &Recipe, dir: &Path, segundos: u64, foto_pantalla: &Path) -> Paso {
    let preparar = || -> Result<(launch::Launch, bool, bool), Error> {
        // El banco no escribe nada fuera de su carpeta. Si el juego se puede hacer portable, se
        // hace; si no, sus datos viven en las carpetas de la persona (AppData, Documentos), y ahí
        // no se pone la copia ni se tocan ajustes: el 6-11 el banco intentó escribir el
        // uzdoom.ini real de la persona.
        let aislado = match portable::enable(dir, r) {
            Ok(_) => true,
            Err(Error::NotPortable { .. }) => false,
            Err(err) => return Err(err),
        };
        let mut sin_copia = false;
        if aislado || copia_solo_en_el_juego(r) {
            sin_copia = place::place(lib, dir, r)?.no_copy;
        }
        if aislado {
            settings::apply(dir, r)?;
        }
        Ok((launch::prepare(lib, dir, r)?, sin_copia, aislado))
    };
    let (l, sin_copia, aislado) = match preparar() {
        Ok(x) => x,
        Err(err) => return paso(false, format!("no se pudo preparar: {err}")),
    };
    // Lo que el juego deja en las carpetas de la persona: se mira el primer nivel antes y
    // después. Puede colarse algo que la persona cree justo en ese momento.
    let carpetas = carpetas_de_la_persona();
    let antes = foto(&carpetas);
    let mut hijo = match l.command().spawn() {
        Ok(h) => h,
        Err(err) => return paso(false, format!("no arranca: {err}")),
    };
    let t = Instant::now();
    let mut cerrado = None;
    // Unos segundos antes de cerrarlo se saca una captura: «sigue abierto» no dice si se juega
    // o si está mostrando un error.
    let mut capturada = None;
    while t.elapsed() < Duration::from_secs(segundos) {
        if let Ok(Some(estado)) = hijo.try_wait() {
            cerrado = Some((estado, t.elapsed().as_secs_f32()));
            break;
        }
        if capturada.is_none() && t.elapsed() >= Duration::from_secs(segundos.saturating_sub(5)) {
            capturada = Some(captura(foto_pantalla));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if cerrado.is_none() {
        let _ = hijo.kill();
        let _ = hijo.wait();
    }
    std::thread::sleep(Duration::from_secs(1));
    let fuera: Vec<String> = foto(&carpetas).difference(&antes).map(|p| p.display().to_string()).collect();
    let nota = if fuera.is_empty() { String::new() } else { format!(" · dejó {} cosas fuera de su carpeta", fuera.len()) };
    let mut p = match cerrado {
        Some((estado, s)) => paso(false, format!("se cerró a los {s:.0} s ({estado}){nota}")),
        None => paso(true, format!("siguió abierto {segundos} s{}{nota}", if sin_copia { " (sin copia: puede estar pidiéndola)" } else { "" })),
    };
    p.datos.insert("sin_copia".into(), sin_copia.to_string());
    p.datos.insert("aislado".into(), aislado.to_string());
    if capturada == Some(true) {
        println!("  captura: {}", foto_pantalla.display());
        p.datos.insert("captura".into(), foto_pantalla.display().to_string());
    }
    if !fuera.is_empty() {
        for f in &fuera {
            println!("  fuera: {f}");
        }
        p.datos.insert("escribe_fuera".into(), fuera.join("; "));
    }
    p
}

// --- lo que un juego deja fuera de su carpeta ---

/// Las carpetas de la persona donde los juegos suelen dejar datos: AppData, Documentos, My
/// Games, Saved Games y la propia carpeta de usuario.
fn carpetas_de_la_persona() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for sub in ["AppData/LocalLow", "Documents", "Documents/My Games", "OneDrive/Documents", "OneDrive/Documents/My Games", "Saved Games"] {
            v.push(home.join(sub));
        }
        v.push(home);
    }
    v
}

/// Lo que hay ahora en el primer nivel de esas carpetas.
fn foto(carpetas: &[PathBuf]) -> BTreeSet<PathBuf> {
    carpetas.iter().filter_map(|c| fs::read_dir(c).ok()).flat_map(|it| it.flatten().map(|e| e.path())).collect()
}

/// `limpiar`: mueve a `<biblioteca>/fuera-de-sitio/<juego>/` lo que cada juego dejó en las
/// carpetas de la persona según el último `abrir`. Mueve, no borra: se puede devolver.
fn limpiar(banco: &mut Banco, biblioteca: &Path) {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let base = biblioteca.join("fuera-de-sitio");
    for (entry, r) in banco.res.iter_mut() {
        let Some(a) = r.abrir.as_mut() else { continue };
        let Some(lista) = a.datos.get("escribe_fuera").cloned() else { continue };
        let mut movidas = Vec::new();
        for p in lista.split("; ").map(PathBuf::from).filter(|p| p.exists()) {
            let rel = home.as_ref().and_then(|h| p.strip_prefix(h).ok()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(p.file_name().unwrap_or_default()));
            let dest = base.join(entry.replace('/', "_")).join(rel);
            if let Some(par) = dest.parent() {
                let _ = fs::create_dir_all(par);
            }
            match fs::rename(&p, &dest) {
                Ok(()) => {
                    println!("{entry}: {} → {}", p.display(), dest.display());
                    movidas.push(dest.display().to_string());
                }
                Err(err) => eprintln!("{entry}: no se pudo mover {}: {err}", p.display()),
            }
        }
        if !movidas.is_empty() {
            a.datos.insert("movido_a".into(), movidas.join("; "));
        }
    }
    banco.guardar();
}
