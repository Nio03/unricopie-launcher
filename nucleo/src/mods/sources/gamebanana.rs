//! GameBanana: la casa de los mods de los ports de libultraship (Ship of Harkinian,
//! 2Ship2Harkinian, Starship…) y de Unleashed Recompiled. Su API pública (apiv11) da, por
//! cada archivo, el MD5 y el resultado de su antivirus, que se usan al bajar.
//!
//! El «1-click» de GameBanana (el botón de la web que abre el gestor) se consigue
//! registrando Unricopie como herramienta en GameBanana; eso es un paso aparte.
use serde::Deserialize;

use super::{get_json, RemoteFile, RemoteMod};
use crate::mods::model::{ModId, Source};
use crate::Result;

#[derive(Debug, Deserialize)]
struct Profile {
    #[serde(rename = "_idRow")]
    id: u64,
    #[serde(rename = "_sName")]
    name: String,
    #[serde(rename = "_sVersion", default)]
    version: Option<String>,
    #[serde(rename = "_sProfileUrl")]
    profile_url: String,
    #[serde(rename = "_bIsObsolete", default)]
    obsolete: bool,
    #[serde(rename = "_bIsTrashed", default)]
    trashed: bool,
    #[serde(rename = "_bIsWithheld", default)]
    withheld: bool,
    #[serde(rename = "_aSubmitter")]
    submitter: Submitter,
    #[serde(rename = "_aFiles", default)]
    files: Vec<File>,
}

#[derive(Debug, Deserialize)]
struct Submitter {
    #[serde(rename = "_sName")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(rename = "_sFile")]
    name: String,
    #[serde(rename = "_sDownloadUrl")]
    url: String,
    #[serde(rename = "_nFilesize", default)]
    size: Option<u64>,
    #[serde(rename = "_sVersion", default)]
    version: Option<String>,
    #[serde(rename = "_sDescription", default)]
    description: Option<String>,
    #[serde(rename = "_sMd5Checksum", default)]
    md5: Option<String>,
    #[serde(rename = "_sAnalysisState", default)]
    analysis_state: Option<String>,
    #[serde(rename = "_sAnalysisResult", default)]
    analysis_result: Option<String>,
}

/// Un mod por su número.
pub fn fetch_mod(agent: &ureq::Agent, mod_id: u64) -> Result<RemoteMod> {
    let url = format!("https://gamebanana.com/apiv11/Mod/{mod_id}/ProfilePage");
    let json: serde_json::Value = get_json(agent, &url)?;
    parse_profile(&json.to_string())
}

/// Los números de los mods de un juego de GameBanana, de 50 en 50 (`page` empieza en 1).
pub fn list_game(agent: &ureq::Agent, game_id: u64, page: u32) -> Result<Vec<(u64, String)>> {
    let url = format!(
        "https://gamebanana.com/apiv11/Mod/Index?_nPerpage=50&_aFilters%5BGeneric_Game%5D={game_id}&_nPage={page}&_sSort=Generic_MostDownloaded"
    );
    let json: serde_json::Value = get_json(agent, &url)?;
    Ok(json["_aRecords"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| Some((r["_idRow"].as_u64()?, r["_sName"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default())
}

pub fn parse_profile(json: &str) -> Result<RemoteMod> {
    let p: Profile = serde_json::from_str(json)?;
    Ok(RemoteMod {
        info: Default::default(),
        id: ModId::gamebanana(p.id),
        name: p.name,
        author: p.submitter.name,
        version: p.version.unwrap_or_default(),
        source: Source::GameBanana { mod_id: p.id },
        page_url: p.profile_url,
        files: p
            .files
            .into_iter()
            .map(|f| RemoteFile {
                scan_ok: match (f.analysis_state.as_deref(), f.analysis_result.as_deref()) {
                    (Some("done"), Some(r)) => Some(r == "ok"),
                    _ => None,
                },
                name: f.name,
                url: f.url,
                size: f.size,
                version: f.version,
                description: f.description,
                md5: f.md5.filter(|m| !m.is_empty()),
            })
            .collect(),
        dependencies: vec![],
        deprecated: p.obsolete || p.trashed || p.withheld,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_un_mod_de_2ship2harkinian() {
        let json = include_str!("../../../tests/datos/gamebanana-644553.json");
        let m = parse_profile(json).unwrap();
        assert_eq!(m.id, ModId::gamebanana(644553));
        assert_eq!(m.name, "3DS Link");
        assert_eq!(m.author, "BeefyMcWhatnow0");
        assert!(!m.deprecated);
        assert_eq!(m.files.len(), 3);
        let f = &m.files[0];
        assert_eq!(f.url, "https://gamebanana.com/dl/1788174");
        assert_eq!(f.md5.as_deref(), Some("b818c2e204f5e512e0e427e94f631a52"));
        assert_eq!(f.scan_ok, Some(true));
        assert_eq!(f.version.as_deref(), Some("3.0.1"));
    }

    #[test]
    #[ignore = "usa la red: cargo test -- --ignored"]
    fn en_vivo() {
        let a = super::super::agent();
        let ids = list_game(&a, 20371, 1).unwrap();
        assert!(!ids.is_empty());
        let m = fetch_mod(&a, ids[0].0).unwrap();
        assert!(!m.files.is_empty());
    }
}
