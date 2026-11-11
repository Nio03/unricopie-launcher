//! Dónde guarda Unricopie lo suyo: la biblioteca (juegos, descargas, versiones anteriores) y
//! sus ajustes.
//!
//! - **Normal:** en la carpeta de datos del sistema: `%LOCALAPPDATA%\Unricopie` en Windows,
//!   `$XDG_DATA_HOME/unricopie` (o `~/.local/share/unricopie`) en Linux y
//!   `~/Library/Application Support/Unricopie` en macOS.
//! - **Portable:** si junto al launcher hay un `portable.txt`, todo va a esa carpeta. Junto al
//!   launcher quiere decir junto al `.exe`, junto al `.AppImage` (no dentro, que es de solo
//!   lectura) o junto a la `.app` en macOS, como hacen Zelda64Recomp y los demás. En Flatpak no
//!   hay modo portable: la app solo ve su sandbox.
//!
//! En modo portable, el launcher hace portable cada juego que instala, si se puede
//! (`install::portable`).
//!
//! Falta: pasar los datos de un modo al otro desde los ajustes.
use std::path::{Path, PathBuf};

use crate::install::Library;
use crate::{Error, Result};

/// El archivo que pone en modo portable al launcher.
pub const PORTABLE_MARKER: &str = "portable.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub portable: bool,
}

impl Root {
    /// La de este launcher.
    pub fn find() -> Result<Root> {
        let exe = std::env::current_exe().map_err(|e| Error::io("current_exe", e))?;
        let flatpak = std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists();
        // El runtime de AppImage monta la app en APPDIR y dice en APPIMAGE dónde está el
        // archivo. Solo vale si este ejecutable está dentro: si no, son de otra AppImage que
        // abrió el launcher.
        let appimage = match (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR")) {
            (Some(file), Some(mount)) if exe.starts_with(&mount) => Some(PathBuf::from(file)),
            _ => None,
        };
        Root::locate(&exe, appimage.as_deref(), flatpak, system_dir())
            .ok_or(Error::NoDataDir)
    }

    /// `find` sin mirar el entorno, para probarlo.
    pub fn locate(exe: &Path, appimage: Option<&Path>, flatpak: bool, system: Option<PathBuf>) -> Option<Root> {
        if !flatpak {
            if let Some(dir) = beside(exe, appimage) {
                if dir.join(PORTABLE_MARKER).is_file() {
                    return Some(Root { path: dir, portable: true });
                }
            }
        }
        system.map(|path| Root { path, portable: false })
    }

    pub fn library(&self) -> Library {
        Library::new(&self.path)
    }
}

/// La carpeta donde la persona ve el launcher.
fn beside(exe: &Path, appimage: Option<&Path>) -> Option<PathBuf> {
    if let Some(file) = appimage {
        return file.parent().map(Path::to_path_buf);
    }
    let dir = exe.parent()?;
    // `Unricopie.app/Contents/MacOS/unricopie`: la carpeta donde está la `.app`.
    let contents = dir.parent().filter(|_| dir.file_name().is_some_and(|n| n == "MacOS"));
    if let Some(app) = contents.filter(|c| c.file_name().is_some_and(|n| n == "Contents")).and_then(Path::parent) {
        if app.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) {
            return app.parent().map(Path::to_path_buf);
        }
    }
    Some(dir.to_path_buf())
}

fn system_dir() -> Option<PathBuf> {
    let name = if cfg!(target_os = "linux") { "unricopie" } else { "Unricopie" };
    dirs::data_local_dir().map(|d| d.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn portable_si_hay_marcador_junto_al_launcher() {
        let t = tempfile::tempdir().unwrap();
        let sys = Some(t.path().join("sistema"));
        let exe = t.path().join("Unricopie").join("unricopie.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(Root::locate(&exe, None, false, sys.clone()).unwrap(), Root { path: sys.clone().unwrap(), portable: false });

        fs::write(exe.with_file_name(PORTABLE_MARKER), b"").unwrap();
        let r = Root::locate(&exe, None, false, sys.clone()).unwrap();
        assert_eq!(r, Root { path: exe.parent().unwrap().to_path_buf(), portable: true });
        assert_eq!(r.library().games, exe.parent().unwrap().join("games"));
        // En Flatpak, nunca.
        assert!(!Root::locate(&exe, None, true, sys).unwrap().portable);
    }

    #[test]
    fn junto_a_la_appimage_y_a_la_app() {
        let t = tempfile::tempdir().unwrap();
        let sys = Some(t.path().join("sistema"));
        fs::write(t.path().join(PORTABLE_MARKER), b"").unwrap();
        // La AppImage está en t; el ejecutable, en su montaje de solo lectura.
        let appimage = t.path().join("Unricopie.AppImage");
        let mounted = Path::new("/tmp/.mount_UnricoXYZ/usr/bin/unricopie");
        assert_eq!(Root::locate(mounted, Some(&appimage), false, sys.clone()).unwrap().path, t.path());
        // macOS: junto a la .app, no dentro.
        let mac = t.path().join("Unricopie.app").join("Contents").join("MacOS").join("unricopie");
        assert_eq!(Root::locate(&mac, None, false, sys.clone()).unwrap().path, t.path());
        // Una carpeta MacOS cualquiera no es una .app.
        let other = t.path().join("x").join("Contents").join("MacOS").join("unricopie");
        assert!(!Root::locate(&other, None, false, sys).unwrap().portable);
    }

    #[test]
    #[ignore = "solo muestra dónde quedaría en este equipo"]
    fn muestra_la_carpeta_de_este_equipo() {
        eprintln!("{:?}", Root::find().unwrap());
    }
}
