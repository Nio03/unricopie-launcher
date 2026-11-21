//! Instalación de juegos: lo básico de un launcher, hecho con el cuidado del catálogo.
//!
//! 1. La **receta** (`recipe`, de unricopie-data) dice qué archivo bajar para cada plataforma,
//!    qué ejecutable abrir y qué copia del juego acepta el proyecto, con sus hashes.
//! 2. Las **releases** (`release`) se leen de GitHub y se elige la del canal: estable (pasó
//!    nuestro banco de pruebas), sin probar (la última oficial) o nightly (lo último del
//!    proyecto).
//! 3. El **instalador** (`installer`) mira antes que haya espacio en el disco (`space`), baja
//!    comprobando el SHA-256 que publica GitHub (si lo publica), analiza lo bajado con el
//!    antivirus del equipo (`crate::antivirus`), descomprime sin riesgos, comprueba que el ejecutable está y deja el **recibo**
//!    (`receipt`, `unricopie.json`).
//! 4. La **copia** del jugador (`copy`) se comprueba contra los hashes de la receta antes de
//!    dársela al proyecto.
//! 5. Al **actualizar** (`update`), lo que no trajo el paquete (partidas, ajustes) pasa a la
//!    versión nueva, y la de antes se guarda entera para volver atrás, con una copia de las
//!    carpetas de fuera donde el juego guarda partidas y ajustes (`data`).
//! 6. El **modo portable** (`portable`) hace que el juego guarde todo en su carpeta, como dice
//!    su receta: marcadores que se crean una vez y argumentos o variables para abrirlo
//!    (`launch`, que lo abre con su carpeta como carpeta de trabajo; en Linux, con lo de
//!    `linux`: las AppImage sin FUSE, las librerías junto al ejecutable y las que faltan; en
//!    Windows, con lo de `windows`: avisar si faltan Visual C++ o Vulkan).
//! 7. Los **ajustes** (`settings`): antes de abrirlo, Unricopie apaga el actualizador propio del
//!    juego en su configuración, sin perder lo demás del archivo. Manda lo del jugador, salvo
//!    el actualizador, que se vuelve a apagar si alguien lo enciende.
//! 8. La **copia** del jugador se comprueba y se guarda una vez en `copies/<juego>/`
//!    (`copy::import`), y `place` se la da al juego: donde la busca, en su configuración o al
//!    abrirlo.
//!
//! 9. **Añadir a mano** (`added`): con un repo de GitHub que no está en el catálogo se arma una
//!    receta mínima (qué archivo de la release toca aquí y qué programa abre) y se instala por
//!    el mismo camino. Sin copia que comprobar, sin modo portable y marcada como añadida a mano.
//!
//! Falta: los instaladores y abrir las apps de macOS.
//! Los dmg (`dmg`) están escritos pero sin probar en un Mac.

pub mod added;
pub mod copy;
pub mod data;
pub(crate) mod dmg;
pub mod installer;
pub mod launch;
pub mod linux;
pub mod place;
pub mod portable;
pub mod receipt;
pub mod recipe;
pub mod release;
pub mod scan;
pub mod settings;
pub mod space;
pub mod update;
pub mod windows;

pub use installer::{install, plan, plan_tag, versions, Library, Plan, Version};
pub use launch::Launch;
pub use receipt::Receipt;
pub use recipe::{Platform, Portability, Recipe};
pub use release::Channel;
pub use update::{Merge, Previous, RolledBack, Status, Updated};
