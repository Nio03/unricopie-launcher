//! Despliegue: pone en la carpeta de mods del juego los archivos de los mods activos, en
//! orden, y guarda un registro exacto de lo que puso para poder quitarlo después.
//!
//! Como en Vortex:
//! - se usan **enlaces duros** desde staging (no ocupan espacio de más y son instantáneos);
//!   si el disco no lo permite (staging y juego en volúmenes distintos), se copia;
//! - si dos mods traen el mismo archivo, gana el que carga **después**;
//! - si el juego ya tenía un archivo en esa ruta, se guarda una copia y se devuelve al purgar;
//! - al purgar solo se borra lo que pusimos nosotros y **no ha cambiado**: si el juego o el
//!   jugador lo modificó, se deja y se avisa.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::{InstalledMod, Mapping, ModId, RelPath};
use super::staging::hash_file;
use crate::{Error, Result};

/// Un archivo que se va a desplegar: quién lo trae, de dónde sale y adónde va.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub target: RelPath,
    pub from: ModId,
    pub source: PathBuf,
    pub size: u64,
    pub hash: u64,
}

/// Qué archivo gana en cada ruta. Las rutas que traen varios mods salen en
/// `Solver`/`solver::resolve` como conflicto; aquí solo cuenta el ganador.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
}

/// Arma el plan. `mods` va en orden de carga (el primero carga primero) con la carpeta de
/// staging de cada uno; `mapping` dice qué archivo va adónde (lo fija la receta del juego).
pub fn plan(mods: &[(&InstalledMod, PathBuf)], mapping: &Mapping) -> Plan {
    let mut by_key: BTreeMap<String, PlannedFile> = BTreeMap::new();
    for (m, dir) in mods {
        for f in &m.files {
            let Some(target) = mapping.target(&f.path) else { continue };
            by_key.insert(
                target.key(),
                PlannedFile { target, from: m.id.clone(), source: f.path.under(dir), size: f.size, hash: f.hash },
            );
        }
    }
    Plan { files: by_key.into_values().collect() }
}

/// Cómo se ponen los archivos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    /// Enlace duro, y copia si el volumen no lo admite.
    HardLink,
    /// Siempre copia (para juegos que modifican sus archivos de mods).
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployedFile {
    pub path: RelPath,
    pub from: ModId,
    pub size: u64,
    pub hash: u64,
    /// `true` si quedó como enlace duro, `false` si se copió.
    pub linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub path: RelPath,
    pub stored_at: PathBuf,
}

/// El registro de un despliegue. Se guarda (por ejemplo como JSON) para poder purgarlo
/// aunque el launcher se haya cerrado entre medias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Deployment {
    pub target_root: PathBuf,
    pub backup_root: PathBuf,
    pub files: Vec<DeployedFile>,
    /// Carpetas que creamos nosotros, para borrarlas al purgar si quedan vacías.
    pub created_dirs: Vec<RelPath>,
    pub backups: Vec<Backup>,
}

/// Lo que pasó al purgar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PurgeReport {
    pub removed: usize,
    pub restored: usize,
    /// Archivos que pusimos pero que alguien cambió después: no se tocan.
    pub kept_modified: Vec<RelPath>,
    /// Archivos que ya no estaban.
    pub missing: Vec<RelPath>,
}

/// Despliega el plan en `target_root` (la carpeta de mods del juego). Los archivos del juego
/// que se pisen se guardan en `backup_root`. Si algo falla a mitad, deshace lo que hizo.
pub fn deploy(plan: &Plan, target_root: &Path, backup_root: &Path, method: Method) -> Result<Deployment> {
    let mut dep = Deployment {
        target_root: target_root.to_path_buf(),
        backup_root: backup_root.to_path_buf(),
        ..Default::default()
    };
    match deploy_into(plan, &mut dep, method) {
        Ok(()) => Ok(dep),
        Err(e) => {
            let _ = purge(&dep);
            Err(e)
        }
    }
}

fn deploy_into(plan: &Plan, dep: &mut Deployment, method: Method) -> Result<()> {
    if !dep.target_root.exists() {
        fs::create_dir_all(&dep.target_root).map_err(|e| Error::io(&dep.target_root, e))?;
    }
    for f in &plan.files {
        for dir in f.target.parents() {
            let d = dir.under(&dep.target_root);
            if !d.exists() {
                fs::create_dir(&d).map_err(|e| Error::io(&d, e))?;
                dep.created_dirs.push(dir);
            }
        }
        let dest = f.target.under(&dep.target_root);
        if dest.exists() {
            let stored_at = f.target.under(&dep.backup_root);
            if let Some(parent) = stored_at.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            move_file(&dest, &stored_at)?;
            dep.backups.push(Backup { path: f.target.clone(), stored_at });
        }
        let linked = match method {
            Method::HardLink => match fs::hard_link(&f.source, &dest) {
                Ok(()) => true,
                Err(e) if cannot_link(&e) => {
                    copy(&f.source, &dest)?;
                    false
                }
                Err(e) => return Err(Error::io(&dest, e)),
            },
            Method::Copy => {
                copy(&f.source, &dest)?;
                false
            }
        };
        dep.files.push(DeployedFile { path: f.target.clone(), from: f.from.clone(), size: f.size, hash: f.hash, linked });
    }
    Ok(())
}

/// Quita lo desplegado: borra nuestros archivos sin cambios, devuelve las copias de lo que
/// había y borra las carpetas que creamos si quedaron vacías.
pub fn purge(dep: &Deployment) -> Result<PurgeReport> {
    let mut report = PurgeReport::default();
    for f in &dep.files {
        let path = f.path.under(&dep.target_root);
        if !path.exists() {
            report.missing.push(f.path.clone());
            continue;
        }
        let (size, hash) = hash_file(&path)?;
        if size == f.size && hash == f.hash {
            fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
            report.removed += 1;
        } else {
            report.kept_modified.push(f.path.clone());
        }
    }
    for b in &dep.backups {
        let dest = b.path.under(&dep.target_root);
        if !dest.exists() && b.stored_at.exists() {
            move_file(&b.stored_at, &dest)?;
            report.restored += 1;
        }
    }
    let mut dirs = dep.created_dirs.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.as_str().matches('/').count()));
    for d in dirs {
        let _ = fs::remove_dir(d.under(&dep.target_root)); // solo si está vacía
    }
    Ok(report)
}

/// Purga el despliegue anterior (si lo hay) y despliega el nuevo plan. Es lo que se llama al
/// cambiar de perfil o de orden.
pub fn redeploy(previous: Option<&Deployment>, plan: &Plan, target_root: &Path, backup_root: &Path, method: Method) -> Result<(Deployment, Option<PurgeReport>)> {
    let report = match previous {
        Some(p) => Some(purge(p)?),
        None => None,
    };
    Ok((deploy(plan, target_root, backup_root, method)?, report))
}

/// El enlace duro no se puede hacer: volúmenes distintos o sistema de archivos que no lo admite.
fn cannot_link(e: &io::Error) -> bool {
    #[cfg(windows)]
    const CROSS_DEVICE: i32 = 17; // ERROR_NOT_SAME_DEVICE
    #[cfg(not(windows))]
    const CROSS_DEVICE: i32 = 18; // EXDEV
    e.raw_os_error() == Some(CROSS_DEVICE) || e.kind() == io::ErrorKind::Unsupported
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).map(|_| ()).map_err(|e| Error::io(to, e))
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy(from, to)?;
            fs::remove_file(from).map_err(|e| Error::io(from, e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::model::{Curation, ModFile, Source};

    fn fake_mod(staging: &Path, id: &str, files: &[(&str, &[u8])]) -> (InstalledMod, PathBuf) {
        let dir = staging.join(id);
        let mut list = Vec::new();
        for (name, data) in files {
            let p = RelPath::new(name).unwrap();
            let full = p.under(&dir);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, data).unwrap();
            list.push(ModFile { path: p, size: data.len() as u64, hash: xxhash_rust::xxh3::xxh3_64(data) });
        }
        let m = InstalledMod {
            id: ModId::local(id),
            name: id.into(),
            version: "1".into(),
            source: Source::Local,
            files: list,
            dependencies: vec![],
            curation: Curation::Untested,
        };
        (m, dir)
    }

    #[test]
    fn gana_el_que_carga_despues_y_purga_deja_todo_como_estaba() {
        let t = tempfile::tempdir().unwrap();
        let staging = t.path().join("staging");
        let game = t.path().join("juego").join("mods");
        let backups = t.path().join("copias");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("original.o2r"), b"del juego").unwrap();

        let (a, da) = fake_mod(&staging, "a", &[("tex/link.o2r", b"A"), ("solo-a.o2r", b"a")]);
        let (b, db) = fake_mod(&staging, "b", &[("TEX/Link.o2r", b"B"), ("original.o2r", b"pisa")]);
        let p = plan(&[(&a, da), (&b, db)], &Mapping::default());
        assert_eq!(p.files.len(), 3);
        let link = p.files.iter().find(|f| f.target.key() == "tex/link.o2r").unwrap();
        assert_eq!(link.from, ModId::local("b"), "el que carga después gana");

        let dep = deploy(&p, &game, &backups, Method::HardLink).unwrap();
        assert_eq!(fs::read(game.join("TEX").join("Link.o2r")).unwrap(), b"B");
        assert_eq!(fs::read(game.join("original.o2r")).unwrap(), b"pisa");
        assert!(dep.files.iter().all(|f| f.linked), "mismo volumen: enlaces duros");
        assert_eq!(dep.backups.len(), 1);

        let r = purge(&dep).unwrap();
        assert_eq!(r.removed, 3);
        assert_eq!(r.restored, 1);
        assert_eq!(fs::read(game.join("original.o2r")).unwrap(), b"del juego");
        assert!(!game.join("TEX").exists(), "la carpeta que creamos se borra");
        assert!(staging.join("b").join("TEX").join("Link.o2r").exists(), "staging no se toca");
    }

    #[test]
    fn no_borra_lo_que_cambio_el_juego() {
        let t = tempfile::tempdir().unwrap();
        let (a, da) = fake_mod(&t.path().join("st"), "a", &[("config.ini", b"x=1")]);
        let game = t.path().join("juego");
        let dep = deploy(&plan(&[(&a, da)], &Mapping::default()), &game, &t.path().join("bk"), Method::Copy).unwrap();
        assert!(!dep.files[0].linked);
        fs::write(game.join("config.ini"), b"x=2").unwrap();
        let r = purge(&dep).unwrap();
        assert_eq!(r.removed, 0);
        assert_eq!(r.kept_modified.len(), 1);
        assert!(game.join("config.ini").exists());
    }

    #[test]
    fn el_registro_se_guarda_y_se_puede_purgar_despues() {
        let t = tempfile::tempdir().unwrap();
        let (a, da) = fake_mod(&t.path().join("st"), "a", &[("a.nrm", b"1")]);
        let game = t.path().join("juego");
        let dep = deploy(&plan(&[(&a, da)], &Mapping::default()), &game, &t.path().join("bk"), Method::HardLink).unwrap();
        let json = serde_json::to_string(&dep).unwrap();
        let back: Deployment = serde_json::from_str(&json).unwrap();
        assert_eq!(purge(&back).unwrap().removed, 1);
        assert!(!game.join("a.nrm").exists());
    }
}
