//! Los tipos de los mods: de dónde vienen, qué archivos traen, qué piden y si los probamos.
use std::fmt;

use serde::{Deserialize, Serialize};

pub use crate::path::RelPath;

/// Identificador estable de un mod: la fuente y su id en ella, por ejemplo
/// `thunderstore:tomtee-Interface_Helper` o `gamebanana:644553`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModId(pub String);

impl ModId {
    pub fn thunderstore(full_name: &str) -> Self {
        ModId(format!("thunderstore:{full_name}"))
    }
    pub fn gamebanana(mod_id: u64) -> Self {
        ModId(format!("gamebanana:{mod_id}"))
    }
    pub fn nexus(game: &str, mod_id: u64) -> Self {
        ModId(format!("nexus:{game}:{mod_id}"))
    }
    pub fn moddb(slug: &str) -> Self {
        ModId(format!("moddb:{slug}"))
    }
    pub fn local(name: &str) -> Self {
        ModId(format!("local:{name}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// De dónde viene un mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// Paquete de Thunderstore (`comunidad`, `Autor-Nombre`).
    Thunderstore { community: String, full_name: String },
    /// Mod de GameBanana, por su número.
    GameBanana { mod_id: u64 },
    /// Mod de Nexus Mods: el dominio del juego (`sonicunleashed`) y el número del mod.
    Nexus { game: String, mod_id: u64 },
    /// Página de ModDB. ModDB no tiene API: el jugador baja el archivo en el navegador y
    /// Unricopie lo importa, recordando de qué página vino.
    ModDb { url: String },
    /// Un archivo que el jugador añadió a mano.
    Local,
}

/// Lo que un mod necesita de otro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub id: ModId,
    /// Versión mínima o rango; `None` si cualquiera sirve.
    pub version: Option<semver::VersionReq>,
}

/// Si probamos el mod, y con qué versiones del juego. Viene de unricopie-data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Curation {
    /// Lo probamos y funciona con estas versiones del proyecto del juego (por ejemplo `v1.2.2`).
    Tested { game_versions: Vec<String> },
    /// Nadie lo probó todavía: la interfaz lo muestra como «sin probar».
    #[default]
    Untested,
}

impl Curation {
    pub fn is_tested_with(&self, game_version: &str) -> bool {
        match self {
            Curation::Tested { game_versions } => game_versions.iter().any(|v| v == game_version),
            Curation::Untested => false,
        }
    }
}

/// Un archivo dentro de un mod ya descomprimido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModFile {
    pub path: RelPath,
    pub size: u64,
    /// XXH3 de 64 bits del contenido.
    pub hash: u64,
}

/// Un mod descomprimido en staging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledMod {
    pub id: ModId,
    pub name: String,
    pub version: String,
    pub source: Source,
    pub files: Vec<ModFile>,
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub curation: Curation,
}

/// Cómo pasan los archivos de un mod a la carpeta de mods del juego. Lo dice la receta de
/// mods de cada juego (unricopie-data), así que es un dato y no código.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Mapping {
    /// Solo estas extensiones (en minúsculas, con punto). Vacío: todas.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Nombres de archivo que nunca se despliegan (el manifest de Thunderstore, el icono…).
    #[serde(default)]
    pub skip_names: Vec<String>,
    /// Si es `true`, se despliega solo el nombre del archivo, sin sus carpetas.
    #[serde(default)]
    pub flatten: bool,
    /// Carpeta del mod que se quita del principio de cada ruta, si la trae.
    #[serde(default)]
    pub strip_prefix: Option<String>,
}

impl Mapping {
    /// Dónde va `path` dentro de la carpeta de mods del juego, o `None` si no se despliega.
    pub fn target(&self, path: &RelPath) -> Option<RelPath> {
        let name = path.file_name().to_lowercase();
        if self.skip_names.iter().any(|s| s.to_lowercase() == name) {
            return None;
        }
        if !self.extensions.is_empty() && !self.extensions.iter().any(|e| name.ends_with(&e.to_lowercase())) {
            return None;
        }
        if self.flatten {
            return RelPath::new(path.file_name()).ok();
        }
        if let Some(prefix) = &self.strip_prefix {
            let want: Vec<&str> = prefix.split('/').filter(|p| !p.is_empty()).collect();
            let parts: Vec<&str> = path.as_str().split('/').collect();
            let matches = parts.len() > want.len()
                && want.iter().zip(&parts).all(|(w, p)| w.to_lowercase() == p.to_lowercase());
            if matches {
                return RelPath::new(&parts[want.len()..].join("/")).ok();
            }
        }
        Some(path.clone())
    }
}

/// Lee versiones como las escriben los autores («1.2», «3.0 + 1.0», «v2.0.0-beta») y las
/// convierte en semver, rellenando con ceros. `None` si no empieza por un número.
pub fn lenient_version(raw: &str) -> Option<semver::Version> {
    let s = raw.trim().trim_start_matches(['v', 'V']);
    let mut nums = Vec::new();
    for part in s.split('.') {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            break;
        }
        nums.push(digits.parse::<u64>().ok()?);
        if nums.len() == 3 || digits.len() != part.len() {
            break;
        }
    }
    if nums.is_empty() {
        return None;
    }
    nums.resize(3, 0);
    Some(semver::Version::new(nums[0], nums[1], nums[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relpath_normaliza_y_rechaza_salidas() {
        assert_eq!(RelPath::new("a\\b/./c.nrm").unwrap().as_str(), "a/b/c.nrm");
        assert_eq!(RelPath::new("a//b").unwrap().as_str(), "a/b");
        for mala in ["../x", "a/../../x", "/etc/passwd", "C:/Windows/x.dll", "C:x", "", "./", "a/\0b"] {
            assert!(RelPath::new(mala).is_err(), "debería rechazar {mala:?}");
        }
        assert_eq!(RelPath::new("Mods/A.NRM").unwrap().key(), "mods/a.nrm");
        let p = RelPath::new("a/b/c").unwrap();
        assert_eq!(p.parents().iter().map(|x| x.as_str()).collect::<Vec<_>>(), ["a", "a/b"]);
    }

    #[test]
    fn mapping_filtra_y_aplana() {
        let m = Mapping {
            extensions: vec![".nrm".into(), ".rtz".into()],
            skip_names: vec!["manifest.json".into()],
            flatten: true,
            strip_prefix: None,
        };
        let t = |s: &str| m.target(&RelPath::new(s).unwrap()).map(|p| p.as_str().to_string());
        assert_eq!(t("mods/MMN64HD.rtz").as_deref(), Some("MMN64HD.rtz"));
        assert_eq!(t("manifest.json"), None);
        assert_eq!(t("icon.png"), None);

        let m = Mapping { strip_prefix: Some("mods".into()), ..Default::default() };
        let t = |s: &str| m.target(&RelPath::new(s).unwrap()).map(|p| p.as_str().to_string());
        assert_eq!(t("Mods/Link/model.o2r").as_deref(), Some("Link/model.o2r"));
        assert_eq!(t("other/x.o2r").as_deref(), Some("other/x.o2r"));
    }

    #[test]
    fn versiones_flexibles() {
        let v = |s| lenient_version(s).map(|v| v.to_string());
        assert_eq!(v("1.2").as_deref(), Some("1.2.0"));
        assert_eq!(v("v2.0.0-beta").as_deref(), Some("2.0.0"));
        assert_eq!(v("3.0 + 1.0").as_deref(), Some("3.0.0"));
        assert_eq!(v("1.0.0.0.0.0.0.2").as_deref(), Some("1.0.0"));
        assert_eq!(v("beta"), None);
    }

    #[test]
    fn curacion() {
        let c = Curation::Tested { game_versions: vec!["v1.2.2".into()] };
        assert!(c.is_tested_with("v1.2.2"));
        assert!(!c.is_tested_with("v1.3.0"));
        assert!(!Curation::Untested.is_tested_with("v1.2.2"));
    }
}
