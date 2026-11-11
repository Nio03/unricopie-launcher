//! Rutas relativas validadas: lo que se usa para todo archivo que sale de un zip, un mod o
//! una release, para que nada se escriba fuera de su carpeta.
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Ruta relativa, siempre con `/`, sin raíz y sin `..`. Es la ruta de un archivo dentro de un
/// mod o dentro de la carpeta de mods del juego.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    /// Normaliza (`\` pasa a `/`, se quitan `.` y separadores repetidos) y rechaza lo que
    /// podría salirse de la carpeta: rutas absolutas, `..`, unidades (`C:`) y nombres vacíos.
    pub fn new(raw: &str) -> Result<Self> {
        let s = raw.replace('\\', "/");
        if s.starts_with('/') || s.contains('\0') {
            return Err(Error::UnsafePath(raw.to_string()));
        }
        let mut parts = Vec::new();
        for part in s.split('/') {
            match part {
                "" | "." => continue,
                ".." => return Err(Error::UnsafePath(raw.to_string())),
                p if p.contains(':') => return Err(Error::UnsafePath(raw.to_string())),
                p => parts.push(p),
            }
        }
        if parts.is_empty() {
            return Err(Error::UnsafePath(raw.to_string()));
        }
        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Clave para comparar rutas: Windows (y Proton) no distingue mayúsculas, así que
    /// `Mods/A.nrm` y `mods/a.nrm` son el mismo archivo.
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    /// Nombre del archivo (la última parte).
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// La ruta dentro de `base`.
    pub fn under(&self, base: &Path) -> PathBuf {
        self.0.split('/').fold(base.to_path_buf(), |p, part| p.join(part))
    }

    /// Las carpetas que la contienen, de la más alta a la más profunda (`a`, `a/b` para `a/b/c`).
    pub fn parents(&self) -> Vec<RelPath> {
        let parts: Vec<&str> = self.0.split('/').collect();
        (1..parts.len()).map(|n| RelPath(parts[..n].join("/"))).collect()
    }
}

impl TryFrom<String> for RelPath {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        RelPath::new(&s)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> String {
        p.0
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
