//! El tiempo de juego de cada versión instalada, para saber cuándo preguntar «¿Cómo te fue?»
//! (decisión del usuario, 9-11):
//! - «Funciona bien» y «Con problemas», con una hora de juego o pasado un día desde que esa
//!   versión se abrió por primera vez;
//! - «No arranca», en cuanto se abrió una vez (si no arranca, nunca sumaría una hora);
//! - por versión: al actualizar, la nueva vuelve a esperar.
//!
//! Vive en `<raíz>/tiempo.json`, con una clave por ficha y versión.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const UNA_HORA: u64 = 3_600;
pub const UN_DIA: u64 = 86_400;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tiempo {
    /// Cuándo se abrió por primera vez (segundos desde 1970).
    pub primera: u64,
    /// Segundos de juego.
    pub segundos: u64,
}

impl Tiempo {
    /// Ya se le puede preguntar si funciona bien o con problemas.
    pub fn listo(&self, ahora: u64) -> bool {
        self.segundos >= UNA_HORA || ahora.saturating_sub(self.primera) >= UN_DIA
    }
}

pub type Tiempos = HashMap<String, Tiempo>;

pub fn clave(entry: &str, version: &str) -> String {
    format!("{entry}@{version}")
}

fn archivo(raiz: &Path) -> PathBuf {
    raiz.join("tiempo.json")
}

/// Lo guardado; vacío si no hay nada o no se puede leer.
pub fn leer(raiz: &Path) -> Tiempos {
    std::fs::read_to_string(archivo(raiz)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn ahora() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// De a uno: dos juegos que se cierran a la vez no se pisan el archivo.
static CANDADO: Mutex<()> = Mutex::new(());

/// Lee, cambia lo de `clave` (si no estaba, empieza ahora) y guarda, sin dejar el archivo a
/// medio escribir.
fn cambiar(raiz: &Path, clave: &str, f: impl FnOnce(&mut Tiempo)) {
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = leer(raiz);
    f(t.entry(clave.to_string()).or_insert(Tiempo { primera: ahora(), segundos: 0 }));
    let ruta = archivo(raiz);
    let tmp = ruta.with_extension("json.tmp");
    match serde_json::to_string_pretty(&t) {
        Ok(texto) => {
            if let Err(e) = std::fs::write(&tmp, texto).and_then(|_| std::fs::rename(&tmp, &ruta)) {
                eprintln!("no se pudo guardar {}: {e}", ruta.display());
            }
        }
        Err(e) => eprintln!("no se pudo guardar el tiempo de juego: {e}"),
    }
}

/// Se abrió el juego: la primera vez, desde ahora cuenta el día.
pub fn abierto(raiz: &Path, clave: &str) {
    cambiar(raiz, clave, |_| {});
}

/// Suma segundos de juego.
pub fn sumar(raiz: &Path, clave: &str, segundos: u64) {
    cambiar(raiz, clave, |t| t.segundos += segundos);
}

/// Espera a que `hijo` se cierre y le pasa a `sumar` lo jugado, cada `cada` como mucho: si se
/// cierra Unricopie con el juego abierto, lo jugado hasta entonces ya quedó contado. Solo
/// segundos enteros; lo que sobra pasa a la siguiente vez.
pub fn medir(hijo: &mut Child, cada: Duration, mut sumar: impl FnMut(u64)) {
    let mut desde = Instant::now();
    loop {
        let fin = !matches!(hijo.try_wait(), Ok(None));
        let pasado = desde.elapsed();
        if fin || pasado >= cada {
            let s = pasado.as_secs();
            if s > 0 {
                sumar(s);
                desde += Duration::from_secs(s);
            }
        }
        if fin {
            return;
        }
        std::thread::sleep(Duration::from_millis(250).min(cada));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listo_con_una_hora_o_un_dia() {
        let t = Tiempo { primera: 1_000_000, segundos: 0 };
        assert!(!t.listo(1_000_000 + 60));
        assert!(!t.listo(1_000_000 + UN_DIA - 1));
        assert!(t.listo(1_000_000 + UN_DIA));
        assert!(Tiempo { primera: 1_000_000, segundos: UNA_HORA }.listo(1_000_000 + 10));
        assert!(!Tiempo { primera: 1_000_000, segundos: UNA_HORA - 1 }.listo(1_000_000 + 10));
    }

    #[test]
    fn abrir_y_sumar_por_version() {
        let d = tempfile::tempdir().unwrap();
        let (v1, v2) = (clave("recomps/zelda-majoras-mask", "v1.2.1"), clave("recomps/zelda-majoras-mask", "v1.2.2"));
        abierto(d.path(), &v1);
        let primera = leer(d.path())[&v1].primera;
        assert!(primera > 0);
        sumar(d.path(), &v1, 1_800);
        sumar(d.path(), &v1, 1_800);
        // Abrirla otra vez no mueve la primera vez.
        abierto(d.path(), &v1);
        let t = leer(d.path());
        assert_eq!(t[&v1], Tiempo { primera, segundos: 3_600 });
        assert!(t[&v1].listo(primera));
        // La versión nueva empieza de cero.
        assert!(!t.contains_key(&v2));
    }

    #[test]
    fn mide_lo_que_esta_abierto() {
        // Un proceso que dura unos 2 segundos.
        #[cfg(windows)]
        let mut hijo = std::process::Command::new("ping").args(["-n", "3", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        #[cfg(not(windows))]
        let mut hijo = std::process::Command::new("sleep").arg("2").spawn().unwrap();
        let mut total = 0;
        let mut veces = 0;
        medir(&mut hijo, Duration::from_millis(500), |s| {
            total += s;
            veces += 1;
        });
        assert!((1..=3).contains(&total), "{total} s");
        assert!(veces >= 1);
    }
}
