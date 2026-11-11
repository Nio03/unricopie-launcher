//! El recibo de instalación: un `unricopie.json` en la carpeta de cada juego instalado, con
//! qué se instaló, de dónde, el hash del ejecutable y la lista de archivos que trajo el
//! paquete. Sirve para reparar, para ver si alguien tocó la instalación, para encontrarla
//! aunque se mueva la carpeta y, al actualizar, para saber qué es del proyecto y qué es del
//! jugador.
//!
//! El formato no se documenta en público por ahora (se decide cuando el launcher salga) y no
//! lleva nada de la receta.
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::recipe::Platform;
use super::release::Channel;
use crate::archive::hash_file;
use crate::path::RelPath;
use crate::{Error, Result};

pub const FILE_NAME: &str = "unricopie.json";
const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub format: u32,
    /// La ficha: `recomps/zelda-majoras-mask`.
    pub entry: String,
    /// El juego de la variante, si la receta tiene varias.
    pub game: Option<String>,
    pub repo: String,
    /// La etiqueta de la release.
    pub version: String,
    pub channel: Channel,
    pub platform: Platform,
    /// El archivo que se bajó y de dónde.
    pub asset: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// `true` si el SHA-256 coincidió con el que publica GitHub; `false` si no había con qué
    /// comparar (una URL fuera de GitHub, o un archivo sin suma).
    pub sha256_verified: bool,
    pub run: Option<String>,
    pub run_stamp: Option<Stamp>,
    /// Segundos desde 1970 (UTC).
    pub installed_at: u64,
    /// Los archivos que trajo el paquete, con su tamaño y su XXH3 tal como llegaron. Lo que
    /// no está aquí lo creó el juego o el jugador (partidas, ajustes, `portable.txt`).
    pub files: Vec<PackageFile>,
    /// Si Unricopie lo hizo portable, lo que puso para eso (ver `install::portable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<PortableState>,
    /// Las claves que Unricopie escribió en la configuración del juego (ver `install::settings`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<WrittenSetting>,
    /// La persona dejó que el juego se actualice solo: Unricopie no apaga su actualizador.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub self_update_allowed: bool,
}

/// Una clave que escribió Unricopie, con lo que había antes para poder devolverlo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrittenSetting {
    /// La ruta tal como la dice la receta (`{data}/general.json`): sigue valiendo aunque la
    /// biblioteca cambie de sitio.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub key: String,
    pub wrote: serde_json::Value,
    /// `None` si la clave no estaba.
    pub before: Option<serde_json::Value>,
}

/// Lo que puso Unricopie en la carpeta del juego para hacerlo portable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableState {
    pub placed: Vec<Placed>,
}

/// Un archivo o carpeta de la regla `portable`, ya en la carpeta del juego.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    pub path: RelPath,
    #[serde(default)]
    pub dir: bool,
    /// Lo creó Unricopie. Si no (ya estaba, o volvió de un `.unricopie-off`), puede llevar
    /// ajustes del jugador y nunca se borra.
    pub created: bool,
    /// Un archivo creado: cómo lo dejó, para saber luego si el juego escribió en él.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<Stamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    pub path: RelPath,
    pub size: u64,
    pub xxh3: u64,
}

impl PackageFile {
    pub fn stamp(&self) -> Stamp {
        Stamp { size: self.size, xxh3: self.xxh3 }
    }
}

/// Tamaño y XXH3 de un archivo, para notar si cambió.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub xxh3: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> Result<Stamp> {
        let (size, xxh3) = hash_file(path)?;
        Ok(Stamp { size, xxh3 })
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Receipt {
    pub(crate) fn format() -> u32 {
        FORMAT
    }

    /// Lo guarda en `dir` sin dejar nunca un recibo a medias: se escribe aparte y se renombra.
    pub fn write(&self, dir: &Path) -> Result<()> {
        let path = dir.join(FILE_NAME);
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, json).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }

    /// El recibo de `dir`, si lo hay.
    pub fn read(dir: &Path) -> Result<Option<Receipt>> {
        let path = dir.join(FILE_NAME);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Cómo está el ejecutable respecto a lo que se instaló.
    pub fn check_run(&self, dir: &Path) -> Result<RunState> {
        let (Some(run), Some(stamp)) = (&self.run, &self.run_stamp) else {
            return Ok(RunState::NotTracked);
        };
        let path = RelPath::new(run)?.under(dir);
        if !path.is_file() {
            return Ok(RunState::Missing);
        }
        Ok(if Stamp::of(&path)? == *stamp { RunState::Intact } else { RunState::Changed })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Intact,
    /// Otro ejecutable: el actualizador propio del proyecto, un parche o algo peor.
    Changed,
    Missing,
    /// Sin ejecutable que vigilar (una app de macOS, por ejemplo).
    NotTracked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarda_lee_y_vigila_el_ejecutable() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("juego.exe"), b"v1").unwrap();
        let r = Receipt {
            format: Receipt::format(),
            entry: "recomps/prueba".into(),
            game: None,
            repo: "a/b".into(),
            version: "v1".into(),
            channel: Channel::Untested,
            platform: Platform::WindowsX64,
            asset: "prueba.zip".into(),
            url: "https://example.invalid/prueba.zip".into(),
            size: 1,
            sha256: "00".into(),
            sha256_verified: true,
            run: Some("juego.exe".into()),
            run_stamp: Some(Stamp::of(&t.path().join("juego.exe")).unwrap()),
            installed_at: now(),
            files: vec![PackageFile { path: RelPath::new("juego.exe").unwrap(), size: 2, xxh3: 0 }],
            portable: None,
            settings: Vec::new(),
            self_update_allowed: false,
        };
        r.write(t.path()).unwrap();
        assert!(!t.path().join("unricopie.json.tmp").exists());
        let back = Receipt::read(t.path()).unwrap().unwrap();
        assert_eq!(back, r);
        assert_eq!(back.check_run(t.path()).unwrap(), RunState::Intact);
        fs::write(t.path().join("juego.exe"), b"v2").unwrap();
        assert_eq!(back.check_run(t.path()).unwrap(), RunState::Changed);
        fs::remove_file(t.path().join("juego.exe")).unwrap();
        assert_eq!(back.check_run(t.path()).unwrap(), RunState::Missing);
        assert_eq!(Receipt::read(&t.path().join("no-existe")).unwrap(), None);
    }
}
