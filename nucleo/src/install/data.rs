//! Las carpetas donde cada juego guarda partidas y ajustes (el `data-dir` de la receta, ver
//! FORMATO.md) y la copia que se guarda de ellas con cada versión anterior.
//!
//! Una ruta de la receta empieza por una variable (`{localappdata}/BanjoRecompiled`). Una
//! variable que este núcleo no conoce, o una ruta rara, se ignora sin romper la receta: una
//! receta nueva no debe romper un launcher viejo. Lo que está en `{game}` no se copia aparte,
//! porque al actualizar ya pasa solo a la versión nueva.
//!
//! Cada ruta puede ser una carpeta o un archivo suelto (`{home}/.config/sdlpal.cfg`).
//!
//! La copia de una versión anterior vive junto a ella, en `previous/<juego>/<id>.data/`: una
//! entrada por cada ruta guardada (`0`, `1`…) y un `datos.json` que dice de dónde salió cada
//! una, si existía y si era un archivo.
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::path::RelPath;
use crate::{Error, Result};

/// Dónde empieza una ruta de `data-dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Game,
    Home,
    AppData,
    LocalAppData,
    Documents,
    SavedGames,
    XdgConfig,
    XdgData,
}

/// Una ruta de `data-dir` ya leída.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPath {
    pub base: Base,
    pub rel: Option<RelPath>,
}

impl DataPath {
    /// `None` si la variable no es una de las conocidas, si la ruta lleva `..`, `\` o partes
    /// vacías, o si es una variable sola que no sea `{game}` (`{home}` copiaría toda la
    /// carpeta personal).
    pub fn parse(s: &str) -> Option<DataPath> {
        let rest = s.strip_prefix('{')?;
        let (var, rest) = rest.split_once('}')?;
        let base = match var {
            "game" => Base::Game,
            "home" => Base::Home,
            "appdata" => Base::AppData,
            "localappdata" => Base::LocalAppData,
            "documents" => Base::Documents,
            "saved-games" => Base::SavedGames,
            "xdg-config" => Base::XdgConfig,
            "xdg-data" => Base::XdgData,
            _ => return None,
        };
        if rest.is_empty() {
            return (base == Base::Game).then_some(DataPath { base, rel: None });
        }
        let tail = rest.strip_prefix('/')?;
        if tail.contains('\\') || tail.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return None;
        }
        let rel = RelPath::new(tail).ok()?;
        Some(DataPath { base, rel: Some(rel) })
    }

    /// La ruta en este equipo. `None` si la variable no tiene sentido aquí (`{appdata}` fuera de
    /// Windows) o no se encuentra.
    pub fn resolve(&self, places: &Places, game: &Path) -> Option<PathBuf> {
        let base = match self.base {
            Base::Game => Some(game.to_path_buf()),
            Base::Home => places.home.clone(),
            Base::AppData => places.appdata.clone(),
            Base::LocalAppData => places.localappdata.clone(),
            Base::Documents => places.documents.clone(),
            Base::SavedGames => places.saved_games.clone(),
            Base::XdgConfig => places.xdg_config.clone(),
            Base::XdgData => places.xdg_data.clone(),
        }?;
        Some(match &self.rel {
            Some(r) => r.under(&base),
            None => base,
        })
    }
}

/// Las carpetas del sistema que usan las variables. `current()` las busca en este equipo; las
/// pruebas las ponen a mano.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    pub home: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub localappdata: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub saved_games: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
}

impl Places {
    pub fn current() -> Places {
        let home = dirs::home_dir();
        let mut p = Places { home: home.clone(), ..Places::default() };
        if cfg!(windows) {
            p.appdata = dirs::config_dir(); // Roaming
            p.localappdata = dirs::data_local_dir();
            p.documents = dirs::document_dir(); // también si está en OneDrive
            p.saved_games = saved_games();
        } else if cfg!(target_os = "linux") {
            p.xdg_config = dirs::config_dir(); // $XDG_CONFIG_HOME o ~/.config
            p.xdg_data = dirs::data_dir(); // $XDG_DATA_HOME o ~/.local/share
        }
        p
    }

    /// Las carpetas de `data-dir` que hay que copiar aparte para un juego instalado en `game`:
    /// las de fuera de su carpeta, y nunca una que contenga la carpeta personal, el juego o la
    /// biblioteca (`library`), por si una receta se equivoca.
    pub fn outside_dirs(&self, paths: &[DataPath], game: &Path, library: &[&Path]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for p in paths {
            if p.base == Base::Game {
                continue;
            }
            let Some(dir) = p.resolve(self, game) else { continue };
            let protected = self.home.iter().map(PathBuf::as_path).chain([game]).chain(library.iter().copied());
            if !dir.is_absolute() || dir.starts_with(game) || protected.into_iter().any(|x| x.starts_with(&dir)) {
                continue;
            }
            // Una dentro de otra: basta con la de fuera.
            if out.iter().any(|o| dir.starts_with(o)) {
                continue;
            }
            out.retain(|o| !o.starts_with(&dir));
            out.push(dir);
        }
        out
    }
}

/// La carpeta «Partidas guardadas» (`FOLDERID_SavedGames`), donde la tenga el jugador: es la
/// que usan GZDoom o RBDOOM-3-BFG, y se puede mover de sitio.
#[cfg(windows)]
fn saved_games() -> Option<PathBuf> {
    known_folders::get_known_folder_path(known_folders::KnownFolder::SavedGames)
}

#[cfg(not(windows))]
fn saved_games() -> Option<PathBuf> {
    None
}

pub(crate) const MANIFEST: &str = "datos.json";

/// Lo que dice `datos.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Saved {
    pub dirs: Vec<SavedDir>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedDir {
    /// De dónde se copió.
    pub path: PathBuf,
    /// `false` si no existía (el juego aún no había guardado nada): volver a esa versión la
    /// deja otra vez sin ella.
    pub present: bool,
    /// Era un archivo suelto, no una carpeta.
    #[serde(default)]
    pub file: bool,
}

impl Saved {
    pub(crate) fn read(dir: &Path) -> Option<Saved> {
        let bytes = fs::read(dir.join(MANIFEST)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.dirs.iter().map(|d| d.path.clone()).collect()
    }
}

/// Copia cada carpeta o archivo de `dirs` a `dest/<n>` y escribe el `datos.json`. No cambia
/// nada en `dirs`. Si falla, no deja `dest` a medias.
pub(crate) fn save(dirs: &[PathBuf], dest: &Path) -> Result<()> {
    let result = (|| {
        fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
        let mut saved = Saved { dirs: Vec::new() };
        for (n, dir) in dirs.iter().enumerate() {
            let to = dest.join(n.to_string());
            let (present, file) = match fs::symlink_metadata(dir) {
                Ok(m) if m.is_dir() => (true, false),
                Ok(m) if m.is_file() => (true, true),
                // Un enlace no se sigue: se trata como si no hubiera nada.
                _ => (false, false),
            };
            if file {
                fs::copy(dir, &to).map_err(|e| Error::io(&to, e))?;
            } else if present {
                copy_tree(dir, &to)?;
            }
            saved.dirs.push(SavedDir { path: dir.clone(), present, file });
        }
        let json = serde_json::to_vec_pretty(&saved)?;
        let m = dest.join(MANIFEST);
        fs::write(&m, json).map_err(|e| Error::io(&m, e))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

/// Pone en su sitio lo guardado en `from`: lo de ahora se borra y se copia lo guardado (o se
/// deja sin nada si no existía). Quien llama ya guardó antes lo de ahora.
pub(crate) fn restore(saved: &Saved, from: &Path) -> Result<()> {
    for (n, d) in saved.dirs.iter().enumerate() {
        match fs::symlink_metadata(&d.path) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(&d.path).map_err(|e| Error::io(&d.path, e))?,
            Ok(_) => fs::remove_file(&d.path).map_err(|e| Error::io(&d.path, e))?,
            Err(_) => {}
        }
        let src = from.join(n.to_string());
        if d.present && d.file {
            if let Some(parent) = d.path.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            fs::copy(&src, &d.path).map_err(|e| Error::io(&d.path, e))?;
        } else if d.present {
            copy_tree(&src, &d.path)?;
        }
    }
    Ok(())
}

/// Copia una carpeta entera. Los enlaces simbólicos no se siguen ni se copian.
pub(crate) fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
    for entry in fs::read_dir(src).map_err(|e| Error::io(src, e))? {
        let entry = entry.map_err(|e| Error::io(src, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
        let to = dest.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            copy_tree(&path, &to)?;
        } else {
            fs::copy(&path, &to).map_err(|e| Error::io(&to, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_las_rutas() {
        let p = DataPath::parse("{localappdata}/BanjoRecompiled").unwrap();
        assert_eq!(p.base, Base::LocalAppData);
        assert_eq!(p.rel.unwrap().as_str(), "BanjoRecompiled");
        assert_eq!(DataPath::parse("{game}").unwrap(), DataPath { base: Base::Game, rel: None });
        assert_eq!(DataPath::parse("{documents}/My Games/vcmi").unwrap().rel.unwrap().as_str(), "My Games/vcmi");
        for bad in ["{home}", "{appdata}", "{nuevo}/x", "%APPDATA%/x", "{game}/../x", "{game}/a\\b", "{game}/", "{game}//a", "{game}x", "game/x"] {
            assert_eq!(DataPath::parse(bad), None, "{bad}");
        }
    }

    fn places(root: &Path) -> Places {
        Places {
            home: Some(root.join("home")),
            appdata: Some(root.join("home/AppData/Roaming")),
            localappdata: Some(root.join("home/AppData/Local")),
            xdg_config: Some(root.join("home/.config")),
            ..Places::default()
        }
    }

    #[test]
    fn solo_copia_lo_de_fuera_y_nunca_algo_que_lo_contenga_todo() {
        let t = tempfile::tempdir().unwrap();
        let pl = places(t.path());
        let lib = t.path().join("home/Unricopie");
        let game = lib.join("games/banjo-kazooie");
        let paths: Vec<DataPath> = [
            "{localappdata}/BanjoRecompiled",
            "{localappdata}/BanjoRecompiled/saves", // dentro de la anterior: sobra
            "{game}/user",                           // va con el juego
            "{documents}/Banjo",                     // sin carpeta Documentos en estas pruebas
            "{xdg-config}/Banjo",
            "{home}/Unricopie",                      // contiene la biblioteca: nunca
        ]
        .iter()
        .map(|s| DataPath::parse(s).unwrap())
        .collect();
        let got = pl.outside_dirs(&paths, &game, &[lib.as_path()]);
        assert_eq!(got, vec![t.path().join("home/AppData/Local/BanjoRecompiled"), t.path().join("home/.config/Banjo")]);
    }

    /// Para mirar a mano qué encuentra en este equipo:
    /// `cargo test muestra_las_carpetas -- --ignored --nocapture`.
    #[test]
    #[ignore = "solo enseña las carpetas de este equipo"]
    fn muestra_las_carpetas_de_este_equipo() {
        eprintln!("{:#?}", Places::current());
    }

    #[test]
    fn guarda_y_devuelve_las_carpetas() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("Local/Juego");
        fs::create_dir_all(a.join("saves")).unwrap();
        fs::write(a.join("saves/1.sav"), b"vieja").unwrap();
        fs::write(a.join("config.json"), b"{}").unwrap();
        let b = t.path().join("Roaming/NoExiste");
        let cfg = t.path().join("config/juego.cfg");
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(&cfg, b"volumen=3").unwrap();
        let dest = t.path().join("copia.data");
        save(&[a.clone(), b.clone(), cfg.clone()], &dest).unwrap();
        let saved = Saved::read(&dest).unwrap();
        assert_eq!(saved.paths(), vec![a.clone(), b.clone(), cfg.clone()]);
        assert!(!saved.dirs[1].present);
        assert!(saved.dirs[2].present && saved.dirs[2].file && !saved.dirs[0].file);
        // La de verdad no se tocó.
        assert_eq!(fs::read(a.join("saves/1.sav")).unwrap(), b"vieja");

        // El juego sigue: cambia la partida, añade otra y crea la carpeta que no había.
        fs::write(a.join("saves/1.sav"), b"nueva").unwrap();
        fs::write(a.join("saves/2.sav"), b"otra").unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(&cfg, b"volumen=9").unwrap();
        restore(&saved, &dest).unwrap();
        assert_eq!(fs::read(&cfg).unwrap(), b"volumen=3");
        assert_eq!(fs::read(a.join("saves/1.sav")).unwrap(), b"vieja");
        assert!(!a.join("saves/2.sav").exists());
        assert_eq!(fs::read(a.join("config.json")).unwrap(), b"{}");
        assert!(!b.exists());
    }
}
