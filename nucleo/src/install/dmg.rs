//! Abrir un dmg de macOS. Se monta con `hdiutil` (solo lectura y sin abrir el Finder), se copia
//! lo que trae con `ditto` y se desmonta, salga bien o mal. `ditto` conserva los enlaces de
//! dentro de la `.app` (los `Versions/Current` de sus frameworks), sus atributos y su firma.
//!
//! Se copia todo lo de la raíz menos los enlaces (el acceso a `Applications` que traen casi
//! todos) y lo oculto (`.background`, `.DS_Store`, `.fseventsd`…).
//!
//! **Sin probar en un Mac.** Se escribió en Windows el 1-11: allí solo se comprueba que compila
//! y lo que no depende de `hdiutil`. Fuera de macOS da `Unsupported`. Falta también qué pasa
//! con un dmg que pide aceptar una licencia al montarlo.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::path::RelPath;
use crate::{Error, Result};

/// Monta `dmg` en `mount` (una carpeta que no existe todavía) y copia lo que trae a `dest`.
pub(crate) fn extract(dmg: &Path, mount: &Path, dest: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::DmgOnlyMac(dmg.to_path_buf()));
    }
    fs::create_dir_all(mount).map_err(|e| Error::io(mount, e))?;
    run(dmg, Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(mount).arg(dmg))?;
    let _mounted = Detach(mount.to_path_buf());
    for entry in fs::read_dir(mount).map_err(|e| Error::io(mount, e))? {
        let entry = entry.map_err(|e| Error::io(mount, e))?;
        let symlink = entry.file_type().map_err(|e| Error::io(entry.path(), e))?.is_symlink();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if !wanted(&name, symlink) {
            continue;
        }
        run(dmg, Command::new("ditto").arg(entry.path()).arg(dest.join(&name)))?;
    }
    Ok(())
}

/// ¿Se copia esta entrada de la raíz del dmg? Ni enlaces ni ocultos, y solo nombres que sean
/// una ruta válida de una sola parte.
pub(crate) fn wanted(name: &str, symlink: bool) -> bool {
    !symlink && !name.starts_with('.') && RelPath::new(name).is_ok_and(|r| r.as_str() == name && !name.contains('/'))
}

fn run(dmg: &Path, cmd: &mut Command) -> Result<()> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| Error::io(dmg, e))?;
    if out.status.success() {
        Ok(())
    } else {
        let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(Error::Archive { path: dmg.to_path_buf(), reason })
    }
}

/// Desmonta al soltarlo, también si la copia falló.
struct Detach(PathBuf);

impl Drop for Detach {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-force"]).arg(&self.0).stdin(Stdio::null()).output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copia_la_app_y_deja_el_acceso_a_aplicaciones() {
        assert!(wanted("OpenMW.app", false));
        assert!(wanted("LEEME.txt", false));
        // El acceso directo a /Applications es un enlace.
        assert!(!wanted("Applications", true));
        for oculto in [".background", ".DS_Store", ".VolumeIcon.icns", ".fseventsd", ".Trashes"] {
            assert!(!wanted(oculto, false), "{oculto}");
        }
        assert!(!wanted("..", false));
        assert!(!wanted("a:b", false));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn fuera_de_macos_no_se_abre() {
        let t = tempfile::tempdir().unwrap();
        let err = extract(&t.path().join("x.dmg"), &t.path().join("m"), &t.path().join("d")).unwrap_err();
        assert!(matches!(err, Error::DmgOnlyMac(_)), "{err}");
        assert!(!t.path().join("m").exists());
    }
}
