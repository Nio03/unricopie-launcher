//! Thunderstore: la casa de los mods de los recomps de N64 (Zelda 64: Recompiled, Banjo:
//! Recompiled…). Su API pública lista todos los paquetes de una comunidad, con versiones,
//! tamaño y dependencias, sin clave.
use serde::Deserialize;

use super::{get_json, ModInfo, RemoteFile, RemoteMod, VersionInfo};
use crate::mods::model::{Dependency, ModId, Source};
use crate::Result;

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    full_name: String,
    owner: String,
    package_url: String,
    #[serde(default)]
    is_deprecated: bool,
    #[serde(default)]
    has_nsfw_content: bool,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    date_updated: String,
    #[serde(default)]
    date_created: String,
    #[serde(default)]
    rating_score: u64,
    versions: Vec<Version>,
}

#[derive(Debug, Deserialize)]
struct Version {
    version_number: String,
    #[serde(default)]
    dependencies: Vec<String>,
    download_url: String,
    #[serde(default)]
    file_size: Option<u64>,
    #[serde(default = "yes")]
    is_active: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    date_created: String,
    #[serde(default)]
    website_url: String,
}

fn yes() -> bool {
    true
}

/// Todos los paquetes de una comunidad (por ejemplo `zelda-64-recompiled`).
pub fn fetch(agent: &ureq::Agent, community: &str) -> Result<Vec<RemoteMod>> {
    let url = format!("https://thunderstore.io/c/{community}/api/v1/package/");
    let json: serde_json::Value = get_json(agent, &url)?;
    parse(community, &json.to_string())
}

/// Convierte la respuesta de la API en mods. Toma la versión activa más nueva de cada paquete.
pub fn parse(community: &str, json: &str) -> Result<Vec<RemoteMod>> {
    let packages: Vec<Package> = serde_json::from_str(json)?;
    Ok(packages
        .into_iter()
        .filter_map(|p| {
            let v = p.versions.iter().find(|v| v.is_active)?;
            Some(RemoteMod {
                id: ModId::thunderstore(&p.full_name),
                name: p.name.replace('_', " "),
                author: p.owner.clone(),
                version: v.version_number.clone(),
                source: Source::Thunderstore { community: community.into(), full_name: p.full_name.clone() },
                page_url: p.package_url.clone(),
                files: vec![RemoteFile {
                    name: format!("{}-{}.zip", p.full_name, v.version_number),
                    url: v.download_url.clone(),
                    size: v.file_size,
                    version: Some(v.version_number.clone()),
                    description: None,
                    md5: None,
                    scan_ok: None,
                }],
                dependencies: v.dependencies.iter().filter_map(|d| dependency(d)).collect(),
                deprecated: p.is_deprecated,
                info: ModInfo {
                    description: v.description.clone(),
                    icon: v.icon.clone(),
                    downloads: p.versions.iter().map(|v| v.downloads).sum(),
                    categories: p.categories.iter().map(|c| c.to_lowercase()).collect(),
                    nsfw: p.has_nsfw_content,
                    updated: p.date_updated.clone(),
                    created: p.date_created.clone(),
                    likes: p.rating_score,
                    website: v.website_url.clone(),
                    versions: p
                        .versions
                        .iter()
                        .filter(|v| v.is_active)
                        .map(|v| VersionInfo { version: v.version_number.clone(), date: v.date_created.clone(), downloads: v.downloads, size: v.file_size })
                        .collect(),
                },
            })
        })
        .collect())
}

/// `Autor-Nombre-1.2.3` → el paquete `Autor-Nombre` en la versión 1.2.3 o mayor (así las
/// interpreta r2modman: la versión del manifest es la mínima).
fn dependency(s: &str) -> Option<Dependency> {
    let (full_name, version) = s.rsplit_once('-')?;
    let req = semver::VersionReq::parse(&format!(">={version}")).ok();
    Some(Dependency { id: ModId::thunderstore(full_name), version: req })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_la_comunidad_de_zelda_64_recompiled() {
        let json = include_str!("../../../tests/datos/thunderstore-zelda.json");
        let mods = parse("zelda-64-recompiled", json).unwrap();
        assert_eq!(mods.len(), 7);
        let mco = mods.iter().find(|m| m.id == ModId::thunderstore("tomtee-Modern_Controller_Overhaul")).unwrap();
        assert_eq!(mco.name, "Modern Controller Overhaul");
        assert_eq!(mco.version, "4.0.3");
        let deps: Vec<_> = mco.dependencies.iter().map(|d| (d.id.as_str(), d.version.as_ref().unwrap().to_string())).collect();
        assert_eq!(
            deps,
            [
                ("thunderstore:LT_Schmiddy-EZ_Text_Replacer_API", ">=2.3.0".to_string()),
                ("thunderstore:tomtee-Interface_Helper", ">=1.1.0".to_string()),
            ]
        );
        assert!(!mco.info.versions.is_empty() && mco.info.versions[0].version == "4.0.3", "las versiones, la más nueva primero");
        let hd = mods.iter().find(|m| m.id == ModId::thunderstore("Nerrel-MMN64HD")).unwrap();
        assert!(hd.files[0].size.unwrap() > 3_000_000_000, "MMN64HD pesa unos 3,5 GB");
        assert!(hd.files[0].url.starts_with("https://thunderstore.io/package/download/"));
    }

    #[test]
    #[ignore = "usa la red: cargo test -- --ignored"]
    fn en_vivo() {
        let mods = fetch(&super::super::agent(), "zelda-64-recompiled").unwrap();
        assert!(mods.len() > 100);
    }
}
