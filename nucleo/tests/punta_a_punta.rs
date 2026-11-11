//! De punta a punta con mods reales de Thunderstore: bajar, descomprimir en staging, resolver
//! el orden con sus dependencias, desplegar con enlaces duros y purgar.
//! Baja unos 3,6 MB, así que no corre por defecto: `cargo test -- --ignored punta_a_punta`.
use std::path::PathBuf;

use unricopie_nucleo::mods::deploy::{self, Method};
use unricopie_nucleo::mods::sources::{self, thunderstore};
use unricopie_nucleo::mods::staging::{Limits, Staging};
use unricopie_nucleo::mods::{solver, Curation, InstalledMod, Mapping, ModId};

#[test]
#[ignore = "usa la red y baja 3,6 MB"]
fn punta_a_punta_modern_controller_overhaul() {
    let agent = sources::agent();
    let remote = thunderstore::fetch(&agent, "zelda-64-recompiled").unwrap();
    // El jugador los pone en un orden que no sirve: el mod antes que sus dependencias.
    let wanted = ["tomtee-Modern_Controller_Overhaul", "tomtee-Interface_Helper", "LT_Schmiddy-EZ_Text_Replacer_API"];

    let t = tempfile::tempdir().unwrap();
    let staging = Staging::new(t.path().join("staging"));
    let mut installed = Vec::new();
    for name in wanted {
        let r = remote.iter().find(|m| m.id == ModId::thunderstore(name)).expect(name);
        let zip = t.path().join("descargas").join(&r.files[0].name);
        sources::download(&agent, &r.files[0], &zip, 50 << 20).unwrap();
        let files = staging.import_zip(&zip, &r.id, &r.version, Limits::default()).unwrap();
        eprintln!("{} {}: {:?}", r.id, r.version, files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>());
        installed.push(InstalledMod {
            id: r.id.clone(),
            name: r.name.clone(),
            version: r.version.clone(),
            source: r.source.clone(),
            files,
            dependencies: r.dependencies.clone(),
            curation: Curation::Untested,
        });
    }

    // Zelda 64: Recompiled carga los .nrm de su carpeta mods; el resto del paquete
    // (manifest.json, icon.png, README.md) no se despliega.
    let mapping = Mapping { extensions: vec![".nrm".into()], flatten: true, ..Default::default() };
    let enabled: Vec<ModId> = wanted.iter().map(|n| ModId::thunderstore(n)).collect();
    let res = solver::resolve(&installed, &enabled, &[], &mapping);
    assert!(res.is_clean(), "{:?}", res.problems);
    assert_eq!(res.order.last(), Some(&ModId::thunderstore("tomtee-Modern_Controller_Overhaul")));

    let in_order: Vec<(&InstalledMod, PathBuf)> = res
        .order
        .iter()
        .map(|id| {
            let m = installed.iter().find(|m| &m.id == id).unwrap();
            (m, staging.mod_dir(&m.id, &m.version))
        })
        .collect();
    let plan = deploy::plan(&in_order, &mapping);
    assert!(!plan.files.is_empty(), "cada paquete trae al menos un .nrm");
    let game_mods = t.path().join("Zelda64Recompiled").join("mods");
    let dep = deploy::deploy(&plan, &game_mods, &t.path().join("copias"), Method::HardLink).unwrap();
    eprintln!("desplegados: {:?}", dep.files.iter().map(|f| (f.path.as_str(), f.linked)).collect::<Vec<_>>());
    assert!(dep.files.iter().all(|f| f.linked));

    let report = deploy::purge(&dep).unwrap();
    assert_eq!(report.removed, dep.files.len());
    assert!(std::fs::read_dir(&game_mods).unwrap().next().is_none(), "la carpeta de mods queda vacía");
}
