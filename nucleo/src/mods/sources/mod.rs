//! Fuentes de mods. Cada una convierte lo que publica su sitio en `RemoteMod`, que es lo que
//! la interfaz muestra y lo que se descarga.
//!
//! | Fuente       | Cómo se lee                                   | Cómo se baja                          |
//! |--------------|-----------------------------------------------|---------------------------------------|
//! | Thunderstore | API pública sin clave                         | directo                               |
//! | GameBanana   | API pública (apiv11); MD5 y antivirus por archivo | directo, comprobando el MD5       |
//! | Nexus Mods   | API con la clave del jugador                  | Premium: directo; gratis: con el enlace `nxm://` del botón de la web |
//! | ModDB        | no tiene API                                  | el jugador lo baja en el navegador y se importa |
//!
//! Nunca se vuelven a subir mods a otro sitio: muchos autores lo prohíben (GameBanana lo
//! pregunta en la licencia de cada mod), así que siempre se bajan de su fuente.
pub mod gamebanana;
pub mod moddb;
pub mod nexus;
pub mod thunderstore;

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{Dependency, ModId, Source};
pub use crate::net::{agent, APP_NAME, APP_VERSION};
pub(crate) use crate::net::{get_json, net};
use crate::net::Expected;
use crate::{Error, Result};

/// Un mod tal como lo publica su fuente, antes de bajarlo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMod {
    pub id: ModId,
    pub name: String,
    pub author: String,
    pub version: String,
    pub source: Source,
    pub page_url: String,
    pub files: Vec<RemoteFile>,
    pub dependencies: Vec<Dependency>,
    /// Marcado como obsoleto, retirado o en la papelera por su sitio.
    pub deprecated: bool,
    /// Lo que se muestra en la lista antes de bajarlo, si la fuente lo publica.
    #[serde(default)]
    pub info: ModInfo,
}

/// Lo que se muestra de un mod en la lista de su fuente.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModInfo {
    pub description: String,
    /// La dirección de su icono.
    pub icon: Option<String>,
    /// Descargas de todas sus versiones.
    pub downloads: u64,
    /// Categorías de su sitio, en minúsculas («libraries», «modpacks»).
    pub categories: Vec<String>,
    /// El sitio dice que tiene contenido para adultos.
    pub nsfw: bool,
    /// Última actualización, como la da el sitio (ISO 8601).
    pub updated: String,
    /// Cuándo se subió por primera vez (ISO 8601), si el sitio lo dice.
    #[serde(default)]
    pub created: String,
    /// Los «me gusta» del sitio.
    #[serde(default)]
    pub likes: u64,
    /// La web del proyecto que da el autor (su repositorio, casi siempre), si la da.
    #[serde(default)]
    pub website: String,
    /// Todas sus versiones, la más nueva primero.
    #[serde(default)]
    pub versions: Vec<VersionInfo>,
}

/// Una versión publicada de un mod, para su ficha.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VersionInfo {
    pub version: String,
    /// Cuándo se publicó (ISO 8601).
    pub date: String,
    pub downloads: u64,
    pub size: Option<u64>,
}

/// Un archivo descargable de un mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
    pub version: Option<String>,
    pub description: Option<String>,
    /// MD5 que publica el sitio, si publica alguno.
    pub md5: Option<String>,
    /// Resultado del antivirus del sitio, si lo pasa: `Some(false)` no se instala.
    pub scan_ok: Option<bool>,
}

/// Baja `file` a `dest` (primero a un temporal), sin pasar de `max_bytes`, y comprueba el MD5
/// si la fuente lo publica. Si el antivirus de la fuente lo marcó, no se baja.
pub fn download(agent: &ureq::Agent, file: &RemoteFile, dest: &Path, max_bytes: u64) -> Result<()> {
    download_with(agent, file, dest, max_bytes, &mut |_, _| {})
}

/// `download`, contando cómo va: bytes bajados y el total, si se sabe.
pub fn download_with(
    agent: &ureq::Agent,
    file: &RemoteFile,
    dest: &Path,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    if file.scan_ok == Some(false) {
        return Err(Error::Flagged { url: file.url.clone() });
    }
    let expected = file.md5.clone().map(Expected::Md5);
    crate::net::download(agent, &file.url, dest, max_bytes, expected.as_ref(), progress)?;
    Ok(())
}
