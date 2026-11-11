//! Gestor de mods, con la filosofía de Vortex (el gestor de Nexus Mods):
//!
//! 1. Cada mod se descomprime una sola vez en su carpeta de **staging** (`staging`).
//! 2. Un **perfil** dice qué mods están activos y en qué orden los quiere el jugador (`profile`).
//! 3. El **solucionador** ordena según dependencias y reglas («A después de B») y dice cuál es
//!    cada problema: una dependencia que falta, una versión que no sirve, dos mods
//!    incompatibles o dos mods que traen el mismo archivo (`solver`).
//! 4. El **despliegue** pone los archivos en el juego con enlaces duros (o copias si el disco
//!    no lo permite), guarda qué puso y lo quita sin tocar nada más (`deploy`).
//!
//! Las fuentes (`sources`) son Thunderstore, GameBanana, Nexus Mods y ModDB. La curación
//! (qué mods se probaron con qué versión del juego) viene de las recetas de unricopie-data;
//! aquí solo se lleva el estado (`Curation`).

pub mod deploy;
pub mod game;
pub mod loader;
pub mod model;
pub mod profile;
pub mod solver;
pub mod sources;
pub mod staging;

pub use model::{Curation, Dependency, InstalledMod, Mapping, ModFile, ModId, RelPath, Source};
