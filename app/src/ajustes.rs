//! Los ajustes del launcher (idioma y tema), en `ajustes.json` junto a la biblioteca: en la
//! carpeta de datos del sistema o, en modo portable, junto al launcher.
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const ARCHIVO: &str = "ajustes.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ajustes {
    /// `es` o `en`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idioma: Option<String>,
    /// `noche`, `dia` o `contraste`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tema: Option<String>,
}

impl Ajustes {
    /// Los guardados; si no hay o no se pueden leer, los de siempre.
    pub fn leer(raiz: &Path) -> Ajustes {
        std::fs::read(raiz.join(ARCHIVO)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Se escribe a un temporal y se cambia de nombre, para no dejar nunca un archivo a medias.
    pub fn guardar(&self, raiz: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(raiz)?;
        let tmp = raiz.join(format!("{ARCHIVO}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, raiz.join(ARCHIVO))
    }
}

/// Español si el sistema está en español; si no, inglés.
pub fn idioma_del_sistema() -> String {
    match sys_locale::get_locale() {
        Some(l) if l.to_lowercase().starts_with("es") => "es".into(),
        _ => "en".into(),
    }
}
