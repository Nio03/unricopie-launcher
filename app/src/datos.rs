//! Los datos del launcher, de recompendium.com/unricopie/: el catálogo público (las fichas del
//! sitio, CC BY-SA) y las recetas, que van aparte y son nuestras.
//!
//! Se guardan en `<raíz>/datos/`. Sin red se usa lo último que se bajó y, la primera vez, la
//! copia que viene junto al programa (`<carpeta del exe>/datos/`). Las portadas se bajan del
//! sitio a `<raíz>/datos/arte/`, en un hilo, después de abrir la ventana.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use serde::Deserialize;
use unricopie_nucleo::net;

use crate::catalogo::{Entrada, Texto};

pub const SITIO: &str = "https://recompendium.com/unricopie";

/// La dirección de los datos. `UNRICOPIE_SITIO` la cambia, para probar contra el sitio en local.
pub fn sitio() -> String {
    std::env::var("UNRICOPIE_SITIO").unwrap_or_else(|_| SITIO.to_string())
}
/// Las versiones del formato que entiende esta versión del launcher.
const FORMATO: u32 = 1;
const MAX_DATOS: u64 = 64 << 20;
const MAX_PORTADA: u64 = 4 << 20;
const ARCHIVOS: [&str; 2] = ["catalogo.json", "recetas.json"];

#[derive(Deserialize)]
struct Catalogo {
    formato: u32,
    entradas: Vec<Entrada>,
}

#[derive(Deserialize)]
struct Recetas {
    formato: u32,
    recetas: HashMap<String, serde_json::Value>,
    #[serde(default)]
    recomendados: HashMap<String, Recomendado>,
}

#[derive(Deserialize)]
struct Recomendado {
    entry: String,
    #[serde(default)]
    reason: Motivo,
}

/// El motivo del recomendado: en los dos idiomas o, si solo hay uno, en español.
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum Motivo {
    #[default]
    Nada,
    Solo(String),
    Idiomas(Texto),
}

/// De dónde salió lo que se muestra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origen {
    /// Bajado ahora o en una sesión anterior.
    Biblioteca,
    /// La copia que viene con el programa: todavía no se pudo bajar nada.
    Programa,
}

fn carpeta(raiz: &Path) -> PathBuf {
    raiz.join("datos")
}

/// La copia que viene junto al exe, para el primer arranque sin red.
fn junto_al_programa(nombre: &str) -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join("datos").join(nombre);
    p.is_file().then_some(p)
}

/// Baja el catálogo y las recetas. `true` si bajó los dos; si algo falla, lo de antes se queda.
/// Si en recompendium.com hay una versión de Unricopie más nueva que esta: su número y la página
/// de donde se baja. Nunca se actualiza solo: la ventana lo avisa y la persona decide.
pub fn version_nueva() -> Option<(String, String)> {
    let agente = net::agent();
    let resp = agente.get(&format!("{}/version.json", sitio())).call().ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
    let version = v.get("version")?.as_str()?.to_string();
    let pagina = v.get("pagina")?.as_str()?.to_string();
    // Solo se abre una página de GitHub o del sitio.
    if !pagina.starts_with("https://github.com/") && !pagina.starts_with("https://recompendium.com/") {
        return None;
    }
    let lenient = unricopie_nucleo::mods::model::lenient_version;
    let (nueva, esta) = (lenient(&version)?, lenient(env!("CARGO_PKG_VERSION"))?);
    (nueva > esta).then_some((version, pagina))
}

pub fn bajar(raiz: &Path) -> bool {
    let agente = net::agent();
    let dir = carpeta(raiz);
    ARCHIVOS.iter().all(|nombre| {
        let r = net::download(&agente, &format!("{}/{nombre}", sitio()), &dir.join(nombre), MAX_DATOS, None, &mut |_, _| {});
        if let Err(e) = &r {
            eprintln!("no se pudo bajar {nombre}: {e}");
        }
        r.is_ok()
    })
}

/// Si ya hay algo bajado en la biblioteca (para no esperar a la red al abrir).
pub fn hay_copia(raiz: &Path) -> bool {
    ARCHIVOS.iter().all(|n| carpeta(raiz).join(n).is_file())
}

/// Lo que hay que bajar del arte: la dirección y dónde va.
pub type Pendientes = Vec<(String, PathBuf)>;

/// Lee el catálogo y las recetas y los junta. Las portadas pasan a ser archivos de
/// `<raíz>/datos/arte/`; las que faltan se devuelven para bajarlas.
pub fn cargar(raiz: &Path) -> Result<(Vec<Entrada>, Origen, Pendientes), String> {
    let dir = carpeta(raiz);
    let (cat_path, origen) = match dir.join("catalogo.json") {
        p if p.is_file() => (p, Origen::Biblioteca),
        _ => (junto_al_programa("catalogo.json").ok_or("no hay catálogo")?, Origen::Programa),
    };
    let rec_path = Some(dir.join("recetas.json")).filter(|p| p.is_file()).or_else(|| junto_al_programa("recetas.json"));

    let leer = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let cat: Catalogo = serde_json::from_str(&leer(&cat_path)?).map_err(|e| format!("{}: {e}", cat_path.display()))?;
    if cat.formato > FORMATO {
        return Err(format!("el catálogo es de un formato más nuevo ({}): hay que actualizar Unricopie", cat.formato));
    }
    let rec: Option<Recetas> = match &rec_path {
        Some(p) => Some(serde_json::from_str(&leer(p)?).map_err(|e| format!("{}: {e}", p.display()))?),
        None => None,
    };
    if rec.as_ref().is_some_and(|r| r.formato > FORMATO) {
        return Err("las recetas son de un formato más nuevo: hay que actualizar Unricopie".into());
    }
    let (mut recetas, recomendados) = rec.map(|r| (r.recetas, r.recomendados)).unwrap_or_default();

    let arte = dir.join("arte");
    let mut pendientes = Pendientes::new();
    // Primero el arte de lo que Unricopie instala: es lo que se ve al abrir.
    let mut despues = Pendientes::new();
    let mut entradas = cat.entradas;
    for e in &mut entradas {
        // Lo retirado por DMCA no se instala nunca, aunque quedara una receta.
        e.receta = if e.retirado { None } else { recetas.remove(&e.entry) };
        if let Some(r) = e.juego.as_ref().and_then(|j| recomendados.get(j)) {
            if r.entry == e.entry {
                e.recomendado = true;
                e.motivo = match &r.reason {
                    Motivo::Nada => Texto::default(),
                    Motivo::Solo(s) => Texto { es: s.clone(), en: String::new() },
                    Motivo::Idiomas(t) => t.clone(),
                };
            }
        }
        let primero = e.receta.is_some();
        let mut pasar = |campo: &mut Option<PathBuf>| {
            let Some(url) = campo.as_ref().map(|p| p.to_string_lossy().into_owned()).filter(|u| u.starts_with("https://")) else { return };
            let Some(nombre) = nombre_local(&url) else {
                *campo = None;
                return;
            };
            let local = arte.join(nombre);
            if !local.is_file() && !pendientes.iter().chain(despues.iter()).any(|(u, _)| u == &url) {
                if primero { &mut pendientes } else { &mut despues }.push((url, local.clone()));
            }
            *campo = Some(local);
        };
        for campo in [&mut e.arte.portada, &mut e.arte.fondo, &mut e.arte.logo, &mut e.arte.icono] {
            pasar(campo);
        }
        let capturas = std::mem::take(&mut e.arte.capturas);
        e.arte.capturas = capturas
            .into_iter()
            .filter_map(|c| {
                let mut c = Some(c);
                pasar(&mut c);
                c
            })
            .collect();
        // Sin icono propio, el icono es la portada (como con IGDB).
        if e.arte.icono.is_none() {
            e.arte.icono = e.arte.portada.clone();
        }
    }
    pendientes.append(&mut despues);
    Ok((entradas, origen, pendientes))
}

/// El nombre del archivo local de una imagen del sitio. Las portadas tienen nombre propio
/// (`/portadas/<juego>.webp`); el resto del arte va por juego (`/arte/<juego>/fondo.webp`) y se
/// guarda como `<juego>-fondo.webp`, para que los de distintos juegos no se pisen.
fn nombre_local(url: &str) -> Option<String> {
    let mut partes = url.rsplit('/');
    let archivo = partes.next().filter(|n| !n.is_empty())?;
    let nombre = match partes.next() {
        Some(carpeta) if carpeta != "portadas" && !carpeta.is_empty() && !carpeta.contains('.') => format!("{carpeta}-{archivo}"),
        _ => archivo.to_string(),
    };
    (!nombre.contains(['\\', ':', '/']) && !nombre.starts_with('.')).then_some(nombre)
}

/// Cuántas imágenes se bajan a la vez.
const HILOS_ARTE: usize = 8;

/// Baja el arte que falta, `HILOS_ARTE` a la vez y en el orden de `pendientes`. Cada `cada`
/// imágenes llama a `avance` para que la ventana las muestre sin esperar al final.
pub fn bajar_arte(pendientes: Pendientes, cada: usize, mut avance: impl FnMut()) {
    let agente = net::agent();
    let siguiente = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        for _ in 0..HILOS_ARTE.min(pendientes.len()) {
            let (tx, agente, siguiente, pendientes) = (tx.clone(), agente.clone(), &siguiente, &pendientes);
            s.spawn(move || {
                while let Some((url, local)) = pendientes.get(siguiente.fetch_add(1, Ordering::Relaxed)) {
                    if let Err(e) = net::download(&agente, url, local, MAX_PORTADA, None, &mut |_, _| {}) {
                        eprintln!("no se pudo bajar {url}: {e}");
                    }
                    let _ = tx.send(());
                }
            });
        }
        drop(tx);
        for (n, ()) in rx.iter().enumerate() {
            if (n + 1) % cada == 0 {
                avance();
            }
        }
    });
    avance();
}
