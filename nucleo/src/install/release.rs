//! Las releases de GitHub de un proyecto y cuál toca según el canal.
//!
//! - **sin probar**: la última release oficial (ni borrador ni prerelease);
//! - **nightly**: lo último que publicó el propio proyecto, prerelease incluida. Nunca algo
//!   compilado por nosotros: en muchos recomps la build lleva el código traducido del juego;
//! - **estable**: la versión que pasó nuestro banco de pruebas. Cuál es lo dicen los datos de
//!   unricopie-data (todavía no existen), así que aquí llega como etiqueta.
//!
//! GitHub publica el SHA-256 de cada archivo (`digest`) desde 2025; los subidos antes no lo
//! traen (el 1-11, 18 de los 77 archivos que piden las recetas). Con él se comprueba la
//! descarga; sin él, el recibo guarda el SHA-256 calculado y dice que no se pudo comparar.
//! Para el canal estable, el banco de pruebas podrá guardar el SHA-256 de lo que probó.
use std::fmt;

use serde::{Deserialize, Serialize};

use super::recipe::matches;
use crate::net::net;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Stable,
    Untested,
    Nightly,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Channel::Stable => "estable",
            Channel::Untested => "sin probar",
            Channel::Nightly => "nightly",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    /// `sha256:<hex>`.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl GhAsset {
    /// El SHA-256 que publica GitHub, si lo publica.
    pub fn sha256(&self) -> Option<&str> {
        self.digest.as_deref()?.strip_prefix("sha256:").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

/// Las últimas 30 releases de `repo` (que ya viene validado por la receta).
pub fn list(agent: &ureq::Agent, repo: &str) -> Result<Vec<GhRelease>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=30");
    let resp = agent
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| net(&url, e))?;
    resp.into_json().map_err(|e| Error::Network { url, reason: e.to_string() })
}

/// La release que toca en `channel`. Para `Stable`, la de `stable_tag`.
pub fn pick<'a>(releases: &'a [GhRelease], channel: Channel, stable_tag: Option<&str>) -> Option<&'a GhRelease> {
    let mut live: Vec<&GhRelease> = releases.iter().filter(|r| !r.draft).collect();
    // La API las da por fecha de creación; se ordenan por la de publicación, que es la que ve
    // la gente (una release creada antes puede publicarse después).
    live.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    match channel {
        Channel::Untested => live.into_iter().find(|r| !r.prerelease),
        Channel::Nightly => live.into_iter().next(),
        Channel::Stable => {
            let tag = stable_tag?;
            live.into_iter().find(|r| r.tag_name == tag)
        }
    }
}

/// El único archivo de `release` que encaja con `pattern` y con ninguno de `exclude`.
pub fn find_asset<'a>(release: &'a GhRelease, pattern: &str, exclude: &[String]) -> Result<&'a GhAsset> {
    let found: Vec<&GhAsset> = release
        .assets
        .iter()
        .filter(|a| matches(pattern, &a.name) && !exclude.iter().any(|x| matches(x, &a.name)))
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::NoAsset { tag: release.tag_name.clone(), pattern: pattern.into() }),
        many => Err(Error::AmbiguousAsset {
            tag: release.tag_name.clone(),
            pattern: pattern.into(),
            matches: many.iter().map(|a| a.name.clone()).collect(),
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Recorte de la respuesta real de la API para Zelda64Recomp (1-11), más una prerelease y un
    /// borrador inventados para probar los canales.
    pub(crate) const RELEASES: &str = r#"[
        {"tag_name": "v1.3.0-dev", "draft": true, "prerelease": false, "published_at": null, "assets": []},
        {"tag_name": "v1.2.3-beta", "draft": false, "prerelease": true, "published_at": "2025-09-10T00:00:00Z",
         "assets": [{"name": "Zelda64Recompiled-v1.2.3-beta-Windows.zip", "size": 10,
                     "browser_download_url": "https://example.invalid/beta.zip", "digest": null}]},
        {"tag_name": "v1.2.2", "draft": false, "prerelease": false, "published_at": "2025-08-27T00:58:35Z",
         "assets": [
            {"name": "Zelda64Recompiled-v1.2.2-Linux-X64.zip", "size": 18638525,
             "browser_download_url": "https://github.com/Zelda64Recomp/Zelda64Recomp/releases/download/v1.2.2/Zelda64Recompiled-v1.2.2-Linux-X64.zip",
             "digest": "sha256:de8b19cdb4e3ad0c10fd6d8127f7a6cc2a9eba106d89c753439178b718887c42"},
            {"name": "Zelda64Recompiled-v1.2.2-Windows.zip", "size": 28940501,
             "browser_download_url": "https://github.com/Zelda64Recomp/Zelda64Recomp/releases/download/v1.2.2/Zelda64Recompiled-v1.2.2-Windows.zip",
             "digest": "sha256:d02b95f1cf5e4d44d6587fa6136c6db521f61aa41ba8700c05f91bea8b0a26d0"}
         ]},
        {"tag_name": "v1.2.1", "draft": false, "prerelease": false, "published_at": "2025-08-18T22:23:16Z", "assets": []}
    ]"#;

    #[test]
    fn elige_por_canal() {
        let rs: Vec<GhRelease> = serde_json::from_str(RELEASES).unwrap();
        assert_eq!(pick(&rs, Channel::Untested, None).unwrap().tag_name, "v1.2.2");
        assert_eq!(pick(&rs, Channel::Nightly, None).unwrap().tag_name, "v1.2.3-beta");
        assert_eq!(pick(&rs, Channel::Stable, Some("v1.2.1")).unwrap().tag_name, "v1.2.1");
        assert!(pick(&rs, Channel::Stable, None).is_none());
        // Un borrador nunca, aunque se pida por su etiqueta.
        assert!(pick(&rs, Channel::Stable, Some("v1.3.0-dev")).is_none());
    }

    #[test]
    fn encuentra_el_archivo_y_su_suma() {
        let rs: Vec<GhRelease> = serde_json::from_str(RELEASES).unwrap();
        let r = pick(&rs, Channel::Untested, None).unwrap();
        let a = find_asset(r, "Zelda64Recompiled-*-Windows.zip", &[]).unwrap();
        assert_eq!(a.size, 28940501);
        assert_eq!(a.sha256(), Some("d02b95f1cf5e4d44d6587fa6136c6db521f61aa41ba8700c05f91bea8b0a26d0"));
        assert!(matches!(find_asset(r, "*-macOS.zip", &[]), Err(Error::NoAsset { .. })));
        assert!(matches!(find_asset(r, "Zelda64Recompiled-*.zip", &[]), Err(Error::AmbiguousAsset { .. })));
        let beta = pick(&rs, Channel::Nightly, None).unwrap();
        assert_eq!(find_asset(beta, "*-Windows.zip", &[]).unwrap().sha256(), None);
    }

    #[test]
    fn descarta_lo_excluido() {
        // Los nombres de la release 1.5.5 de dhewm3 (1-11).
        let names = [
            "dhewm3-1.5.5_Linux_amd64.tar.gz", "dhewm3-1.5.5_win32.zip", "dhewm3-mods-1.5.5_Linux_amd64.tar.gz",
            "dhewm3-mods-1.5.5_win32.zip", "dhewm3-wide-guis-v0.2.zip",
        ];
        let rel = GhRelease {
            tag_name: "1.5.5".into(),
            name: None,
            draft: false,
            prerelease: false,
            published_at: None,
            assets: names
                .iter()
                .map(|n| GhAsset { name: n.to_string(), size: 1, browser_download_url: String::new(), digest: None, updated_at: None })
                .collect(),
        };
        assert!(matches!(find_asset(&rel, "dhewm3-*_win32.zip", &[]), Err(Error::AmbiguousAsset { .. })));
        let ex = vec!["dhewm3-mods-*".to_string()];
        assert_eq!(find_asset(&rel, "dhewm3-*_win32.zip", &ex).unwrap().name, "dhewm3-1.5.5_win32.zip");
        assert_eq!(find_asset(&rel, "dhewm3-*_Linux_amd64.tar.gz", &ex).unwrap().name, "dhewm3-1.5.5_Linux_amd64.tar.gz");
    }

    #[test]
    #[ignore = "usa la red (API de GitHub, sin clave)"]
    fn lee_releases_de_verdad() {
        let rs = list(&crate::net::agent(), "Zelda64Recomp/Zelda64Recomp").unwrap();
        let r = pick(&rs, Channel::Untested, None).unwrap();
        let a = find_asset(r, "Zelda64Recompiled-*-Windows.zip", &[]).unwrap();
        assert!(a.sha256().is_some(), "GitHub publica el SHA-256: {a:?}");
    }
}
