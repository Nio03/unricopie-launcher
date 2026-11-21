//! «Tus copias» en Ajustes: buscar en una carpeta las copias de los juegos (ROM, ISO) y guardar
//! en la biblioteca las que la persona elija. El trabajo es del núcleo (`install::scan`); aquí
//! solo van los hilos y los avisos a la ventana, con el progreso espaciado para no saturarla.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use unricopie_nucleo::install::scan::{self, Imports, Match, Phase, Progress, Scan, Wanted};
use unricopie_nucleo::install::{Library, Recipe};
use unricopie_nucleo::{net, Error};

use crate::trabajo::{avisar, Aviso};

/// Cuánto se lleva: la fase (0 mirando qué hay, 1 leyendo, 2 guardando), archivos y bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Avance {
    pub fase: i32,
    pub hechos: usize,
    pub total: usize,
    pub bytes: u64,
    pub bytes_total: u64,
}

/// Un aviso de progreso cada tanto, y siempre al cambiar de fase.
fn contar() -> impl FnMut(Progress<'_>) {
    let mut ultimo: Option<(Instant, Phase)> = None;
    move |p: Progress<'_>| {
        let ahora = Instant::now();
        if ultimo.is_some_and(|(t, f)| f == p.phase && ahora.duration_since(t) < Duration::from_millis(150)) {
            return;
        }
        ultimo = Some((ahora, p.phase));
        let fase = match p.phase {
            Phase::Listing => 0,
            Phase::Hashing => 1,
            Phase::Importing => 2,
        };
        avisar(Aviso::CopiasAvance(Avance { fase, hechos: p.files_done, total: p.files_total, bytes: p.bytes_done, bytes_total: p.bytes_total }));
    }
}

/// Busca en `carpeta` las copias de los juegos de `recetas` (los instalados y los que se pueden
/// instalar aquí). Lee las listas de copias que publica algún proyecto (`accepts-from`) antes de
/// empezar. Al terminar avisa con lo encontrado; las buscadas vuelven para poder guardar después.
pub fn buscar(biblioteca: Library, recetas: Vec<Recipe>, carpeta: PathBuf, cortar: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let buscadas = scan::wanted(Some(&net::agent()), &biblioteca, recetas);
        let r = scan::scan(&biblioteca, &buscadas, &carpeta, &scan::Options::default(), &cortar, &mut contar());
        avisar(Aviso::CopiasHalladas { buscadas, resultado: r.map(Box::new) });
    });
}

/// Guarda en la biblioteca las copias elegidas (`elegidas`, de lo que devolvió `buscar`).
pub fn guardar(biblioteca: Library, buscadas: Vec<Wanted>, elegidas: Vec<Match>, cortar: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let hecho = scan::import_matches(&biblioteca, &buscadas, &elegidas, &cortar, &mut contar());
        avisar(Aviso::CopiasGuardadas(Box::new(hecho)));
        avisar(Aviso::Refrescar);
    });
}

/// Lo que llega a la ventana al terminar de buscar.
pub type Resultado = Result<Box<Scan>, Error>;
/// Lo que llega al terminar de guardar.
pub type Guardado = Box<Imports>;
