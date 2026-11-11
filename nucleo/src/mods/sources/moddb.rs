//! ModDB. No tiene API pública y no pudimos confirmar que sus condiciones permitan bajar de
//! forma automática (su web rechaza los clientes que no son navegadores), así que Unricopie no
//! la raspa: el jugador abre la página del archivo, lo baja en su navegador y Unricopie lo
//! importa (`Staging::import_zip`), recordando de qué página vino para mostrar el enlace y
//! avisar de versiones nuevas a mano. La curación puede guardar la página de cada mod probado.
use url::Url;

use crate::mods::model::{ModId, Source};
use crate::{Error, Result};

/// Una página de descarga de ModDB, por ejemplo
/// `https://www.moddb.com/mods/brutal-doom/downloads/brutal-doom-v21-beta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDbPage {
    pub url: String,
    /// El último tramo de la dirección, que identifica el archivo.
    pub slug: String,
}

impl ModDbPage {
    pub fn parse(link: &str) -> Result<Self> {
        let bad = || Error::BadLink(link.to_string());
        let u = Url::parse(link).map_err(|_| bad())?;
        let host = u.host_str().ok_or_else(bad)?;
        if u.scheme() != "https" || !(host == "www.moddb.com" || host == "moddb.com") {
            return Err(bad());
        }
        let parts: Vec<&str> = u.path_segments().ok_or_else(bad)?.filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [kind, _, section, slug] if matches!(*kind, "mods" | "games" | "addons") && matches!(*section, "downloads" | "addons") => {
                Ok(ModDbPage { url: format!("https://www.moddb.com/{}", parts.join("/")), slug: slug.to_string() })
            }
            _ => Err(bad()),
        }
    }

    pub fn id(&self) -> ModId {
        ModId::moddb(&self.slug)
    }

    pub fn source(&self) -> Source {
        Source::ModDb { url: self.url.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_paginas_de_descarga() {
        let p = ModDbPage::parse("https://www.moddb.com/mods/brutal-doom/downloads/brutal-doom-v21-beta").unwrap();
        assert_eq!(p.slug, "brutal-doom-v21-beta");
        assert_eq!(p.id(), ModId::moddb("brutal-doom-v21-beta"));
        let p = ModDbPage::parse("https://moddb.com/games/doom/addons/hd-textures/").unwrap();
        assert_eq!(p.url, "https://www.moddb.com/games/doom/addons/hd-textures");
    }

    #[test]
    fn rechaza_lo_que_no_es_una_descarga() {
        for bad in [
            "https://www.moddb.com/mods/brutal-doom",
            "http://www.moddb.com/mods/a/downloads/b",
            "https://www.moddb.com.evil.example/mods/a/downloads/b",
            "https://www.nexusmods.com/mods/a/downloads/b",
        ] {
            assert!(ModDbPage::parse(bad).is_err(), "{bad}");
        }
    }
}
