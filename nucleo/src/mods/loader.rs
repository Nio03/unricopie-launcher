//! Cómo se le dice al juego qué mods están activos y en qué orden, cuando lo guarda en un
//! archivo suyo (`Loader` en la receta). Unricopie lo escribe después de cada despliegue, así
//! el juego abre con lo que dice el perfil.
//!
//! Hoy hay uno: el de N64ModernRuntime, la base de los recomps de N64Recomp (Zelda 64:
//! Recompiled, Banjo: Recompiled…). Comprobado en su código (librecomp, df7e820):
//! - los mods son los `.nrm` (un zip con `mod.json`) y los paquetes de texturas `.rtz`, que el
//!   juego registra con `register_mod_container_type("rtz")`; el id es el campo `id` de su
//!   `mod.json`, y un `.rtz` sin `mod.json` usa el nombre del archivo sin la extensión;
//! - `mods.json`, en su carpeta de datos, guarda `enabled_mods` (los activos) y `mod_order`
//!   (todos los que conoce, en orden). Un mod que no está en `mod_order` es nuevo y se activa
//!   solo si su manifiesto no dice lo contrario; uno que está solo se activa si también está
//!   en `enabled_mods`. Por eso Unricopie lo escribe: si el jugador apagó un mod en el juego,
//!   volver a ponerlo con un perfil no bastaría para activarlo.
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};

use crate::{Error, Result};

/// N64ModernRuntime.
pub mod n64recomp {
    use super::*;

    /// El archivo, dentro de la carpeta de datos del juego.
    pub const CONFIG: &str = "mods.json";
    /// La carpeta con los ajustes de cada mod (`<id>.json`), dentro de la carpeta de datos.
    pub const SETTINGS: &str = "mod_config";

    /// Lo que dice ahora `mods.json`: los activos y el orden de todos los que conoce el juego.
    pub fn read_config(config: &Path) -> Option<(HashSet<String>, Vec<String>)> {
        let v = read(config)?;
        let strings = |key: &str| -> Vec<String> {
            v.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
        };
        Some((strings("enabled_mods").into_iter().collect(), strings("mod_order")))
    }

    /// ¿El juego lo trata como un mod? (las bibliotecas nativas que van al lado, no).
    pub fn is_mod(name: &str) -> bool {
        let name = name.to_lowercase();
        name.ends_with(".nrm") || name.ends_with(".rtz")
    }

    /// El id con que el juego conoce el mod de `path`: el `id` de su `mod.json`, o el nombre
    /// del archivo sin la extensión si no lo trae (así hace el juego con los `.rtz`).
    pub fn mod_id(path: &Path) -> String {
        manifest_id(path).unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
    }

    fn manifest_id(path: &Path) -> Option<String> {
        manifest(path)?.get("id")?.as_str().map(str::to_string)
    }

    /// La versión del juego que pide como mínimo el mod de `path` (`minimum_recomp_version` de
    /// su `mod.json`), si la pide.
    pub fn minimum_version(path: &Path) -> Option<String> {
        manifest(path)?.get("minimum_recomp_version")?.as_str().map(str::to_string)
    }

    fn manifest(path: &Path) -> Option<Value> {
        let mut zip = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
        let entry = zip.by_name("mod.json").ok()?;
        // Un manifiesto ocupa pocos KB; más de 1 MB no es un manifiesto.
        let mut text = Vec::new();
        entry.take(1 << 20).read_to_end(&mut text).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Escribe `config` (`<datos>/mods.json`) para que estén activos los mods de `deployed`,
    /// en ese orden. `managed` son los ids de todos los mods de Unricopie para este juego,
    /// estén o no en el perfil: los que no estén en `deployed` salen de las dos listas. Los
    /// mods que el jugador puso a mano en la carpeta se dejan como estaban.
    pub fn write(config: &Path, deployed: &[String], managed: &HashSet<String>) -> Result<()> {
        let now = read(config);
        let strings = |key: &str| -> Vec<String> {
            now.as_ref()
                .and_then(|v| v.get(key))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let ours: HashSet<&str> = deployed.iter().map(String::as_str).collect();
        let keep = |id: &String| !managed.contains(id) && !ours.contains(id.as_str());
        let mut enabled: Vec<String> = deployed.to_vec();
        enabled.extend(strings("enabled_mods").into_iter().filter(keep));
        let mut order: Vec<String> = deployed.to_vec();
        order.extend(strings("mod_order").into_iter().filter(keep));
        dedup(&mut enabled);
        dedup(&mut order);

        let mut out = match now {
            Some(Value::Object(map)) => Value::Object(map),
            _ => json!({}),
        };
        out["enabled_mods"] = json!(enabled);
        out["mod_order"] = json!(order);
        if let Some(parent) = config.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let text = serde_json::to_string_pretty(&out)?;
        let tmp = config.with_extension("json.unricopie-tmp");
        fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, config).map_err(|e| Error::io(config, e))
    }

    /// Lo que hay ahora, como lo lee el juego: el archivo y, si no se puede leer, su `.bak`.
    fn read(config: &Path) -> Option<Value> {
        let parse = |p: &Path| fs::read(p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        parse(config).or_else(|| {
            let mut bak = config.as_os_str().to_owned();
            bak.push(".bak");
            parse(Path::new(&bak))
        })
    }

    fn dedup(v: &mut Vec<String>) {
        let mut seen = HashSet::new();
        v.retain(|x| seen.insert(x.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::n64recomp::*;
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn nrm(path: &Path, manifest: Option<&str>) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        if let Some(m) = manifest {
            w.start_file("mod.json", SimpleFileOptions::default()).unwrap();
            w.write_all(m.as_bytes()).unwrap();
        }
        w.start_file("mod_binary.bin", SimpleFileOptions::default()).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
    }

    #[test]
    fn el_id_sale_del_manifiesto_o_del_nombre() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("mm_modern_controller_overhaul.nrm");
        nrm(&a, Some(r#"{ "game_id": "mm", "id": "mm_modern_controller_overhaul", "version": "4.0.3", "minimum_recomp_version": "1.2.0" }"#));
        assert_eq!(mod_id(&a), "mm_modern_controller_overhaul");
        assert_eq!(minimum_version(&a).as_deref(), Some("1.2.0"));
        let b = t.path().join("MCO_Xbox.rtz");
        nrm(&b, None);
        assert_eq!(mod_id(&b), "MCO_Xbox");
        assert_eq!(minimum_version(&b), None);
        let c = t.path().join("roto.nrm");
        fs::write(&c, b"no es un zip").unwrap();
        assert_eq!(mod_id(&c), "roto");
        assert!(is_mod("A.NRM") && is_mod("b.rtz") && !is_mod("helper.dll") && !is_mod("README.md"));
    }

    #[test]
    fn escribe_los_del_perfil_y_respeta_los_puestos_a_mano() {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join(CONFIG);
        // Lo que dejó el juego: un mod puesto a mano activo, uno nuestro apagado en el juego y
        // uno nuestro que ya no está en el perfil.
        fs::write(
            &cfg,
            r#"{ "enabled_mods": ["a_mano", "viejo"], "mod_order": ["viejo", "a_mano", "apagado", "otro_a_mano"] }"#,
        )
        .unwrap();
        let managed: HashSet<String> = ["apagado", "viejo", "api"].map(String::from).into();
        write(&cfg, &["api".into(), "apagado".into()], &managed).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(v["enabled_mods"], json!(["api", "apagado", "a_mano"]));
        assert_eq!(v["mod_order"], json!(["api", "apagado", "a_mano", "otro_a_mano"]));
        assert!(!t.path().join("mods.json.unricopie-tmp").exists());
    }

    #[test]
    fn sin_archivo_o_roto_usa_la_copia_del_juego() {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("datos").join(CONFIG);
        write(&cfg, &["x".into()], &HashSet::new()).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(v["enabled_mods"], json!(["x"]));

        fs::write(&cfg, b"{ roto").unwrap();
        fs::write(t.path().join("datos").join("mods.json.bak"), r#"{ "enabled_mods": ["a_mano"], "mod_order": ["a_mano"] }"#).unwrap();
        write(&cfg, &["x".into()], &HashSet::new()).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(v["enabled_mods"], json!(["x", "a_mano"]));
    }
}
