//! El modo portable de cada juego: que guarde partidas y ajustes en su propia carpeta, como
//! dice la regla `portable` de su receta (FORMATO.md).
//!
//! - Los **archivos y carpetas** de la regla (`portable.txt`, `gzdoom_portable.ini`, `Support`)
//!   se crean una vez en la carpeta del juego. Así sigue siendo portable aunque se abra sin
//!   Unricopie, y al actualizar pasan solos a la versión nueva, como todo lo que no trae el
//!   paquete.
//! - Los **argumentos y variables** (`+set fs_homepath {game}`, `SHIP_HOME={game}`) se ponen
//!   cada vez que se abre (`launch`).
//!
//! El recibo apunta lo que se puso (`Receipt::portable`). Quitar el modo portable borra lo que
//! creó Unricopie si sigue tal cual. Lo demás no se borra nunca: un `gzdoom_portable.ini` en el
//! que el juego ya guarda sus ajustes, o una carpeta `Support` con partidas, se apartan con el
//! nombre `<nombre>.unricopie-off`, y volver a ponerlo los devuelve a su sitio. Lo que el juego
//! guardó en su carpeta mientras era portable se queda ahí, pero deja de verlo: el launcher
//! tiene que decirlo.
//!
//! Las dos cosas, con el juego cerrado: muchos escriben sus ajustes al salir.
use std::fs;
use std::path::{Path, PathBuf};

use super::receipt::{PackageFile, Placed, PortableState, Receipt, Stamp, FILE_NAME};
use super::recipe::{PortableRule, Portability, Recipe};
use super::update::{closed, installed, same_key};
use crate::path::RelPath;
use crate::{Error, Result};

/// Lo que se añade al nombre de lo que se aparta al quitar el modo portable.
pub const OFF: &str = ".unricopie-off";

/// Comprueba que una regla no se sale de la carpeta del juego ni pisa lo de Unricopie, y que
/// sus argumentos y variables solo usan `{game}`.
pub fn check_rule(rule: &PortableRule) -> Result<()> {
    let bad = |what: String| Err(Error::BadRecipe(format!("portable: {what}")));
    for raw in rule.files.keys().chain(&rule.dirs) {
        let rel = RelPath::new(raw)?;
        // Tal cual, sin `./` ni `\`: que la receta y el disco digan lo mismo.
        if rel.as_str() != raw {
            return bad(format!("ruta no válida «{raw}»"));
        }
        // El recibo (y su `.tmp`), en cualquier sistema y con cualquier mayúscula.
        if rel.as_str().to_ascii_lowercase().starts_with(FILE_NAME) || rel.file_name().contains(OFF) {
            return bad(format!("«{raw}» es un nombre que usa Unricopie"));
        }
    }
    if rule.files.keys().any(|f| rule.dirs.iter().any(|d| same_key(d, f))) {
        return bad("la misma ruta es archivo y carpeta".into());
    }
    for name in rule.env.keys() {
        let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return bad(format!("variable no válida «{name}»"));
        }
    }
    for v in rule.args.iter().chain(rule.env.values()) {
        if v.replace("{game}", "").contains(['{', '}']) {
            return bad(format!("«{v}» usa una variable que no es {{game}}"));
        }
    }
    Ok(())
}

/// Lo instalado en `dir` es el juego de `recipe`.
pub(crate) fn same_game(dir: &Path, receipt: &Receipt, recipe: &Recipe) -> Result<()> {
    if receipt.entry == recipe.entry {
        Ok(())
    } else {
        Err(Error::OtherGame { dir: dir.to_path_buf(), entry: receipt.entry.clone() })
    }
}

/// ¿Está en modo portable? (Puesto por Unricopie; uno que ya guarda todo en su carpeta lo dice
/// `Recipe::portability`.)
pub fn is_enabled(receipt: &Receipt) -> bool {
    receipt.portable.is_some()
}

/// Hace portable lo instalado en `dir`. Se puede llamar otra vez: solo pone lo que falte (si la
/// receta cambió, o alguien borró un marcador). Si el juego ya guarda todo en su carpeta, no
/// hace nada; si la receta no sabe cómo, da `NotPortable`. Si algo falla, quita lo que había
/// puesto en esta llamada.
pub fn enable(dir: &Path, recipe: &Recipe) -> Result<Receipt> {
    let mut receipt = installed(dir)?;
    same_game(dir, &receipt, recipe)?;
    let rule = match recipe.portability(receipt.platform) {
        Portability::Rule(r) => r,
        Portability::Already => return Ok(receipt),
        Portability::No => {
            return Err(Error::NotPortable { entry: recipe.entry.clone(), platform: receipt.platform.to_string() })
        }
    };
    check_rule(rule)?;
    closed(dir, &receipt)?;
    let mut state = receipt.portable.clone().unwrap_or_default();
    let mut undo = Vec::new();
    let mut result = apply(dir, rule, &receipt.files, &mut state, &mut undo);
    if result.is_ok() {
        receipt.portable = Some(state);
        result = receipt.write(dir);
    }
    if let Err(e) = result {
        for u in undo.into_iter().rev() {
            u.revert();
        }
        return Err(e);
    }
    Ok(receipt)
}

fn apply(dir: &Path, rule: &PortableRule, package: &[PackageFile], state: &mut PortableState, undo: &mut Vec<Undo>) -> Result<()> {
    // Primero las carpetas: un archivo de la regla puede ir dentro de una.
    for d in &rule.dirs {
        place(dir, d, None, package, state, undo)?;
    }
    for (f, content) in &rule.files {
        place(dir, f, Some(content), package, state, undo)?;
    }
    Ok(())
}

/// Lo que se hizo en esta llamada, para deshacerlo si algo falla después.
enum Undo {
    Created(PathBuf),
    Restored { path: PathBuf, off: PathBuf },
}

impl Undo {
    fn revert(self) {
        match self {
            Undo::Created(p) if p.is_dir() => {
                let _ = fs::remove_dir(&p);
            }
            Undo::Created(p) => {
                let _ = fs::remove_file(&p);
            }
            Undo::Restored { path, off } => {
                let _ = fs::rename(&path, &off);
            }
        }
    }
}

/// Pone un archivo (`content`) o una carpeta (`None`) de la regla.
fn place(dir: &Path, raw: &str, content: Option<&str>, package: &[PackageFile], state: &mut PortableState, undo: &mut Vec<Undo>) -> Result<()> {
    let rel = RelPath::new(raw)?;
    let is_dir = content.is_none();
    // Si ya lo trae el paquete, el proyecto lo hace portable por su cuenta: no es nuestro.
    if package.iter().any(|f| same_key(f.path.as_str(), rel.as_str())) {
        return Ok(());
    }
    let path = rel.under(dir);
    let off = off_path(&path);
    let placed = match fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() == is_dir && !m.file_type().is_symlink() => {
            // Ya está: si lo pusimos nosotros, se queda como se apuntó.
            if state.placed.iter().any(|p| same_key(p.path.as_str(), rel.as_str())) {
                return Ok(());
            }
            Placed { path: rel.clone(), dir: is_dir, created: false, stamp: None }
        }
        Ok(_) => return Err(Error::InTheWay(path)),
        Err(_) if fs::symlink_metadata(&off).is_ok_and(|m| m.is_dir() == is_dir) => {
            fs::rename(&off, &path).map_err(|e| Error::io(&path, e))?;
            undo.push(Undo::Restored { path: path.clone(), off });
            Placed { path: rel.clone(), dir: is_dir, created: false, stamp: None }
        }
        Err(_) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            let stamp = match content {
                None => {
                    fs::create_dir(&path).map_err(|e| Error::io(&path, e))?;
                    undo.push(Undo::Created(path.clone()));
                    None
                }
                Some(c) => {
                    fs::write(&path, c).map_err(|e| Error::io(&path, e))?;
                    undo.push(Undo::Created(path.clone()));
                    Some(Stamp::of(&path)?)
                }
            };
            Placed { path: rel.clone(), dir: is_dir, created: true, stamp }
        }
    };
    state.placed.retain(|p| !same_key(p.path.as_str(), rel.as_str()));
    state.placed.push(placed);
    Ok(())
}

/// Quita el modo portable de lo instalado en `dir`: borra lo que creó Unricopie si sigue tal
/// cual y aparta lo demás con `.unricopie-off`. Si no estaba en modo portable, no hace nada. Si
/// falla a mitad, se puede volver a llamar: lo que ya no está se salta.
pub fn disable(dir: &Path) -> Result<Receipt> {
    let mut receipt = installed(dir)?;
    let Some(state) = receipt.portable.clone() else { return Ok(receipt) };
    closed(dir, &receipt)?;
    // Primero los archivos: pueden estar dentro de una carpeta de la regla.
    let files = state.placed.iter().filter(|p| !p.dir);
    for p in files.chain(state.placed.iter().filter(|p| p.dir)) {
        if receipt.files.iter().any(|f| same_key(f.path.as_str(), p.path.as_str())) {
            continue;
        }
        let path = p.path.under(dir);
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if p.created && untouched(&path, &meta, p) {
            if p.dir {
                fs::remove_dir(&path).map_err(|e| Error::io(&path, e))?;
            } else {
                fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
            }
        } else {
            park(&path)?;
        }
    }
    receipt.portable = None;
    receipt.write(dir)?;
    Ok(receipt)
}

/// ¿Sigue como lo dejó Unricopie? Una carpeta, vacía; un archivo, igual que cuando se escribió.
fn untouched(path: &Path, meta: &fs::Metadata, p: &Placed) -> bool {
    if p.dir {
        meta.is_dir() && fs::read_dir(path).is_ok_and(|mut d| d.next().is_none())
    } else {
        meta.is_file() && p.stamp.is_some_and(|s| Stamp::of(path).is_ok_and(|now| now == s))
    }
}

/// Aparta `path` a `<path>.unricopie-off`. Si ya había uno de antes, ese pasa a
/// `.unricopie-off-2` (o el primer número libre): el que vuelve al poner otra vez el modo
/// portable es siempre el último.
fn park(path: &Path) -> Result<()> {
    let off = off_path(path);
    if fs::symlink_metadata(&off).is_ok() {
        let mut n = 2;
        let older = loop {
            let mut s = off.as_os_str().to_owned();
            s.push(format!("-{n}"));
            let candidate = PathBuf::from(s);
            if fs::symlink_metadata(&candidate).is_err() {
                break candidate;
            }
            n += 1;
        };
        fs::rename(&off, &older).map_err(|e| Error::io(&off, e))?;
    }
    fs::rename(path, &off).map_err(|e| Error::io(path, e))
}

fn off_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(OFF);
    PathBuf::from(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::install::installer::tests::{fake_plan, zip_with};
    use crate::install::installer::{install_downloaded, Library};
    use crate::install::recipe::tests::ZELDA;
    use crate::net::Downloaded;

    /// Zelda con una regla portable en Windows, como la de GZDoom (un ini que el juego usa
    /// para sus ajustes) más una carpeta, un argumento y una variable.
    pub(crate) fn zelda_portable() -> Recipe {
        let json = ZELDA.replace(
            r#""copy":"#,
            r#""portable": {
                "windows-x64": {
                    "files": { "portable.txt": "", "juego_portable.ini": "", "cfg/donde.json": "{\"mode\": \"portable\"}" },
                    "dirs": ["Support"],
                    "args": ["+set", "fs_homepath", "{game}"],
                    "env": { "APP_FOLDER_PATH": "{game}/datos" }
                }
            },
            "copy":"#,
        );
        Recipe::from_json(&json).unwrap()
    }

    pub(crate) fn instalado(t: &Path) -> PathBuf {
        let lib = Library::new(t);
        let z = t.join("bajado.zip");
        zip_with(&z, &[("Zelda64Recompiled.exe", b"exe"), ("cfg/otro.json", b"{}")]);
        let plan = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        install_downloaded(&lib, &plan, &z, &Downloaded { size: 3, sha256: "x".into() }).unwrap();
        plan.dest
    }

    #[test]
    fn pone_y_quita_el_modo_portable() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let r = zelda_portable();

        let rec = enable(&d, &r).unwrap();
        assert!(is_enabled(&rec));
        assert_eq!(Receipt::read(&d).unwrap().unwrap(), rec);
        assert!(d.join("portable.txt").is_file());
        assert_eq!(fs::read(d.join("cfg").join("donde.json")).unwrap(), b"{\"mode\": \"portable\"}");
        assert!(d.join("Support").is_dir());
        assert_eq!(rec.portable.as_ref().unwrap().placed.len(), 4);
        // Otra vez no cambia nada.
        assert_eq!(enable(&d, &r).unwrap(), rec);

        // El juego guarda sus ajustes en el ini y una partida en Support.
        fs::write(d.join("juego_portable.ini"), b"[ajustes]\nvolumen=3").unwrap();
        fs::write(d.join("Support").join("partida.sav"), b"partida").unwrap();

        let off = disable(&d).unwrap();
        assert!(!is_enabled(&off));
        // Lo que creó Unricopie y sigue igual, fuera...
        assert!(!d.join("portable.txt").exists());
        assert!(!d.join("cfg").join("donde.json").exists());
        // ...lo que usó el juego, apartado y no borrado...
        assert!(!d.join("juego_portable.ini").exists());
        assert_eq!(fs::read(d.join("juego_portable.ini.unricopie-off")).unwrap(), b"[ajustes]\nvolumen=3");
        assert!(d.join("Support.unricopie-off").join("partida.sav").is_file());
        // ...y lo del paquete, intacto.
        assert!(d.join("cfg").join("otro.json").is_file());

        // Volver a ponerlo devuelve los ajustes y las partidas.
        let again = enable(&d, &r).unwrap();
        assert_eq!(fs::read(d.join("juego_portable.ini")).unwrap(), b"[ajustes]\nvolumen=3");
        assert!(d.join("Support").join("partida.sav").is_file());
        let ini = again.portable.unwrap().placed.into_iter().find(|p| p.path.as_str() == "juego_portable.ini").unwrap();
        assert!(!ini.created, "lo que vuelve no es nuestro: no se borra nunca");
        disable(&d).unwrap();
        assert!(d.join("juego_portable.ini.unricopie-off").is_file());
        assert!(d.join("Support.unricopie-off").join("partida.sav").is_file());
    }

    #[test]
    fn sigue_portable_al_actualizar_y_al_volver_atras() {
        use crate::install::update::{roll_back, update_downloaded};
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        enable(&d, &zelda_portable()).unwrap();
        fs::write(d.join("juego_portable.ini"), b"volumen=3").unwrap();
        // La v2 ya trae su propio portable.txt; la carpeta Support sigue vacía.
        let z2 = t.path().join("v2.zip");
        zip_with(&z2, &[("Zelda64Recompiled.exe", b"exe 2"), ("cfg/otro.json", b"{}"), ("portable.txt", b"")]);
        let mut v2 = fake_plan(&lib, Some("Zelda64Recompiled.exe"));
        v2.version = "v2.0.0".into();
        v2.size = None;

        let up = update_downloaded(&lib, &v2, &z2, &Downloaded { size: 1, sha256: "y".into() }).unwrap();
        assert_eq!(fs::read(d.join("juego_portable.ini")).unwrap(), b"volumen=3");
        assert!(d.join("Support").is_dir(), "una carpeta vacía también pasa a la versión nueva");
        let placed: Vec<&str> = up.receipt.portable.as_ref().unwrap().placed.iter().map(|p| p.path.as_str()).collect();
        // portable.txt ya es del paquete: no es nuestro y no se quitará.
        assert!(!placed.contains(&"portable.txt"), "{placed:?}");
        assert_eq!(placed.len(), 3);
        disable(&d).unwrap();
        assert!(d.join("portable.txt").is_file());

        // La v1 vuelve como estaba: portable, con su recibo.
        let back = roll_back(&lib, &d, &up.previous.id).unwrap();
        assert!(is_enabled(&back.receipt));
        assert!(d.join("Support").is_dir());
    }

    #[test]
    fn respeta_lo_que_ya_habia() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        // La persona ya lo había hecho portable a mano.
        fs::write(d.join("portable.txt"), b"").unwrap();
        // Y de una vez anterior quedó un ini apartado, pero hay otro nuevo.
        fs::write(d.join("juego_portable.ini.unricopie-off"), b"viejo").unwrap();
        fs::write(d.join("juego_portable.ini"), b"nuevo").unwrap();
        enable(&d, &zelda_portable()).unwrap();
        assert_eq!(fs::read(d.join("juego_portable.ini")).unwrap(), b"nuevo");
        disable(&d).unwrap();
        // El suyo no se borra: se aparta.
        assert!(d.join("portable.txt.unricopie-off").is_file());
        // El último apartado es el que volverá; el de antes no se pierde.
        assert_eq!(fs::read(d.join("juego_portable.ini.unricopie-off")).unwrap(), b"nuevo");
        assert_eq!(fs::read(d.join("juego_portable.ini.unricopie-off-2")).unwrap(), b"viejo");
    }

    #[test]
    fn si_falla_no_deja_nada_a_medias() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        // Un archivo donde la regla quiere una carpeta.
        fs::write(d.join("Support"), b"no soy una carpeta").unwrap();
        let before = Receipt::read(&d).unwrap().unwrap();
        let err = enable(&d, &zelda_portable()).unwrap_err();
        assert!(matches!(err, Error::InTheWay(_)), "{err}");
        assert_eq!(Receipt::read(&d).unwrap().unwrap(), before);
        assert!(!d.join("portable.txt").exists());
        assert!(!d.join("cfg").join("donde.json").exists());
    }

    #[test]
    fn solo_si_la_receta_sabe_como() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let sin = Recipe::from_json(ZELDA).unwrap();
        assert!(matches!(enable(&d, &sin), Err(Error::NotPortable { .. })));
        // Si ya guarda todo en su carpeta, no hay nada que hacer.
        let ya = Recipe::from_json(&ZELDA.replace(r#""copy":"#, r#""data-dir": { "windows-x64": "{game}" }, "copy":"#)).unwrap();
        assert!(!is_enabled(&enable(&d, &ya).unwrap()));
        // Otro juego, no.
        let otro = Recipe::from_json(&ZELDA.replace("recomps/zelda-majoras-mask", "recomps/otro")).unwrap();
        assert!(matches!(enable(&d, &otro), Err(Error::OtherGame { .. })));
        assert!(!is_enabled(&disable(&d).unwrap()));
    }

    #[test]
    fn reglas_que_no_se_aplican() {
        let rule = |json: &str| serde_json::from_str::<PortableRule>(json).unwrap();
        assert!(check_rule(&rule(r#"{ "files": { "bin/.portable": "" }, "args": ["--configdir={game}/wz"] }"#)).is_ok());
        for bad in [
            r#"{ "files": { "../fuera.txt": "" } }"#,
            r#"{ "files": { "/raiz.txt": "" } }"#,
            r#"{ "files": { "./portable.txt": "" } }"#,
            r#"{ "files": { "a\\b.txt": "" } }"#,
            r#"{ "files": { "unricopie.json": "" } }"#,
            r#"{ "files": { "Unricopie.JSON": "" } }"#,
            r#"{ "dirs": ["x.unricopie-off"] }"#,
            r#"{ "files": { "Support": "" }, "dirs": ["Support"] }"#,
            r#"{ "env": { "1MAL": "{game}" } }"#,
            r#"{ "env": { "A=B": "{game}" } }"#,
            r#"{ "args": ["{home}/x"] }"#,
        ] {
            assert!(check_rule(&rule(bad)).is_err(), "{bad}");
        }
    }
}
