//! Núcleo de Unricopie: todo lo que no es interfaz. Lo usan el launcher y el banco de
//! pruebas, así que no sabe nada de ventanas ni de Slint.
//!
//! - `install`: instalar juegos desde la release de cada proyecto, con la receta de
//!   unricopie-data, comprobando la descarga y la copia del jugador.
//! - `mods`: el gestor de mods, inspirado en Vortex: cada mod se descomprime una vez en su
//!   carpeta de staging y se despliega en el juego con enlaces duros, con perfiles, orden de
//!   carga y un solucionador que dice cuál es el conflicto.
//! - `root`: dónde guarda Unricopie lo suyo, en la carpeta del sistema o, en modo portable,
//!   junto al launcher.
//! - `archive`, `net` y `path`: descomprimir, bajar y validar rutas, para los dos.
#![forbid(unsafe_code)]

pub mod archive;
pub mod error;
pub mod install;
pub mod mods;
pub mod net;
pub mod path;
pub mod root;

pub use error::{Error, Result};
