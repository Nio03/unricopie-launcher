//! Nexus Mods. Lo que hay que saber (de su política de uso de la API y de su cliente oficial,
//! node-nexus-api):
//!
//! - Cada jugador usa **su propia clave** (o inicia sesión por SSO cuando Unricopie esté
//!   registrada en Nexus; registrarla es obligatorio antes de publicar el launcher). La clave
//!   nunca sale del equipo del jugador.
//! - Cada petición lleva `Application-Name` y `Application-Version` constantes.
//! - **Premium** puede pedir el enlace de descarga directamente. **Gratis**: el jugador pulsa
//!   «Mod manager download» en la web, eso abre un enlace `nxm://` con `key` y `expires`, y
//!   con ellos se pide el enlace. Unricopie tiene que registrarse como programa para `nxm://`.
//! - Está prohibido raspar la web o bajar datos en bloque para alojarlos en otro sitio.
//! - Su cliente oficial no pasa de 300 peticiones en ráfaga y recupera una por segundo.
use serde::Deserialize;
use url::Url;

use super::{net, APP_NAME, APP_VERSION};
use crate::{Error, Result};

/// Un enlace `nxm://juego/mods/123/files/456?key=…&expires=…&user_id=…`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NxmLink {
    pub game: String,
    pub mod_id: u64,
    pub file_id: u64,
    pub key: Option<String>,
    pub expires: Option<u64>,
    pub user_id: Option<u64>,
}

pub fn parse_nxm(link: &str) -> Result<NxmLink> {
    let bad = || Error::BadLink(link.to_string());
    let u = Url::parse(link).map_err(|_| bad())?;
    if u.scheme() != "nxm" {
        return Err(bad());
    }
    let game = u.host_str().ok_or_else(bad)?.to_lowercase();
    let parts: Vec<&str> = u.path_segments().ok_or_else(bad)?.collect();
    let (mod_id, file_id) = match parts.as_slice() {
        ["mods", m, "files", f] => (m.parse().map_err(|_| bad())?, f.parse().map_err(|_| bad())?),
        _ => return Err(bad()),
    };
    let q = |k: &str| u.query_pairs().find(|(name, _)| name == k).map(|(_, v)| v.to_string());
    Ok(NxmLink {
        game,
        mod_id,
        file_id,
        key: q("key"),
        expires: q("expires").and_then(|v| v.parse().ok()),
        user_id: q("user_id").and_then(|v| v.parse().ok()),
    })
}

/// La dirección de la API que da los enlaces de descarga de un archivo.
pub fn download_link_url(link: &NxmLink) -> String {
    let mut u = format!(
        "https://api.nexusmods.com/v1/games/{}/mods/{}/files/{}/download_link.json",
        link.game, link.mod_id, link.file_id
    );
    if let (Some(key), Some(expires)) = (&link.key, link.expires) {
        u.push_str(&format!("?key={key}&expires={expires}"));
    }
    u
}

#[derive(Debug, Deserialize)]
struct Mirror {
    #[serde(rename = "URI")]
    uri: String,
}

/// Pide los enlaces de descarga (uno por servidor espejo) con la clave del jugador.
pub fn download_urls(agent: &ureq::Agent, api_key: &str, link: &NxmLink) -> Result<Vec<String>> {
    let url = download_link_url(link);
    let resp = agent
        .get(&url)
        .set("apikey", api_key)
        .set("Application-Name", APP_NAME)
        .set("Application-Version", APP_VERSION)
        .call()
        .map_err(|e| net(&url, e))?;
    let mirrors: Vec<Mirror> = resp.into_json().map_err(|e| Error::Network { url: url.clone(), reason: e.to_string() })?;
    Ok(mirrors.into_iter().map(|m| m.uri).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_enlaces_nxm() {
        let l = parse_nxm("nxm://SonicUnleashed/mods/123/files/4567?key=abcDEF&expires=1798761600&user_id=42").unwrap();
        assert_eq!(l.game, "sonicunleashed");
        assert_eq!((l.mod_id, l.file_id), (123, 4567));
        assert_eq!(l.key.as_deref(), Some("abcDEF"));
        assert_eq!(l.expires, Some(1798761600));
        assert_eq!(
            download_link_url(&l),
            "https://api.nexusmods.com/v1/games/sonicunleashed/mods/123/files/4567/download_link.json?key=abcDEF&expires=1798761600"
        );
        // Premium: sin key ni expires.
        let p = parse_nxm("nxm://sonicunleashed/mods/1/files/2").unwrap();
        assert!(!download_link_url(&p).contains('?'));
    }

    #[test]
    fn rechaza_enlaces_raros() {
        for bad in ["https://www.nexusmods.com/x", "nxm://juego/mods/x/files/1", "nxm://juego/collections/abc", "nxm:"] {
            assert!(parse_nxm(bad).is_err(), "{bad}");
        }
    }
}
