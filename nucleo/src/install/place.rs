//! Dar al juego la copia del jugador que Unricopie guardó en la biblioteca (`copy::import`),
//! como dice `copy.place` de la receta (FORMATO.md):
//! - `store`: se deja el archivo donde lo busca el juego (la ROM de Zelda,
//!   `{data}/mm.n64.us.1.0.z64`);
//! - `files`: lo que hay en la carpeta de la copia (o solo su subcarpeta `from`) se copia a otra
//!   carpeta, sin pisar nada. Lo que va a la carpeta del juego es lo que los mods y los perfiles
//!   pueden cambiar; la copia de la biblioteca no se toca nunca;
//! - `config`: se escribe la ruta en su configuración. Manda lo del jugador: si ya tiene un
//!   valor, no se toca;
//! - `args` y `env`: se ponen al abrirlo (`launch`), con `{copy}` cambiado por la ruta.
//!
//! Con `when-missing`, solo si falta ese archivo: Ship of Harkinian recibe la ROM solo hasta que
//! genera su `oot.o2r`. `place` lo hace antes de abrir el juego, con el juego cerrado. Si todavía
//! no hay copia, no hace nada y lo dice (`Placed::no_copy`): el juego la pedirá por su cuenta.
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::copy::{self, Imported};
use super::installer::Library;
use super::portable::same_game;
use super::receipt::Receipt;
use super::recipe::{ConfigFormat, CopyRule, Place, Recipe};
use super::settings::{self, ConfigKey};
use super::update::{closed, installed};
use crate::path::RelPath;
use crate::{Error, Result};

/// Lo que hizo `place`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placed {
    /// Archivos de la copia que se dejaron donde los busca el juego.
    pub stored: Vec<PathBuf>,
    /// Claves de la configuración que se escribieron.
    pub written: Vec<String>,
    /// Configuraciones que todavía no existen (se escriben después de abrirlo una vez).
    pub missing_files: Vec<PathBuf>,
    /// No hay copia guardada: el juego la pedirá, o el launcher se la pide a la persona.
    pub no_copy: bool,
}

/// Comprueba una forma de `place`: que haga algo, rutas que no se salgan y solo `{game}` y
/// `{copy}` en argumentos, variables y valores.
pub fn check_place(p: &Place) -> Result<()> {
    let bad = |what: String| Err(Error::BadRecipe(format!("copy.place: {what}")));
    if p.store.is_none() && p.files.is_none() && p.config.is_none() && p.args.is_empty() && p.env.is_empty() {
        return bad("no hace nada".into());
    }
    for f in p.store.iter().chain(&p.when_missing) {
        if !settings::path_ok(f) {
            return bad(format!("ruta no válida «{f}»"));
        }
    }
    if let Some(f) = &p.files {
        if !dir_ok(f) {
            return bad(format!("carpeta no válida «{f}»"));
        }
    }
    if p.link && p.files.is_none() {
        return bad("«link» solo vale con files".into());
    }
    if let Some(sub) = &p.from {
        if p.files.is_none() {
            return bad(format!("«from: {sub}» solo vale con files"));
        }
        if RelPath::new(sub).is_err() {
            return bad(format!("subcarpeta no válida «{sub}»"));
        }
    }
    if let Some(c) = &p.config {
        if !settings::path_ok(&c.file) || c.key.is_empty() || c.section.as_deref() == Some("") {
            return bad(format!("config no válida en «{}»", c.file));
        }
        if c.format.or_else(|| ConfigFormat::from_name(&c.file)).is_none() {
            return bad(format!("no se sabe el formato de «{}»", c.file));
        }
    }
    let values = p.args.iter().chain(p.env.values()).chain(p.config.as_ref().map(|c| &c.value));
    for v in values {
        if v.replace("{game}", "").replace("{copy}", "").contains(['{', '}']) {
            return bad(format!("«{v}» usa una variable que no es {{game}} ni {{copy}}"));
        }
    }
    for name in p.env.keys() {
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return bad(format!("variable no válida «{name}»"));
        }
    }
    Ok(())
}

/// Una carpeta: lo mismo que una ruta de archivo, o la variable sola (`{data}`, `{game}`).
fn dir_ok(f: &str) -> bool {
    f == "{data}" || f == "{game}" || settings::path_ok(f)
}

fn resolve_dir(f: &str, recipe: &Recipe, receipt: &Receipt, dir: &Path) -> Option<PathBuf> {
    match f {
        "{data}" => settings::data_folder(recipe, receipt, dir),
        "{game}" => Some(dir.to_path_buf()),
        _ => settings::resolve(f, recipe, receipt, dir),
    }
}

/// Las formas que valen ahora: su plataforma, su modo y, si llevan `when-missing`, que falte ese
/// archivo.
fn ways<'a>(rule: &'a CopyRule, recipe: &'a Recipe, receipt: &Receipt, dir: &Path) -> Vec<&'a Place> {
    let portable = receipt.portable.is_some();
    // Una variante sin `place` propio usa el de la receta: UZDoom pasa `-iwad {copy}` igual con
    // Doom que con Doom II, y cada variante solo trae sus copias aceptadas.
    let places = if rule.place.is_empty() { &recipe.copy.place } else { &rule.place };
    places
        .iter()
        .filter(|p| p.applies_to(receipt.platform, portable))
        .filter(|p| match &p.when_missing {
            None => true,
            Some(f) => settings::resolve(f, recipe, receipt, dir).is_some_and(|path| !path.exists()),
        })
        .collect()
}

/// Antes de abrir el juego: le deja la copia donde la busca y escribe su ruta en la
/// configuración, según `copy.place`.
pub fn place(library: &Library, dir: &Path, recipe: &Recipe) -> Result<Placed> {
    let receipt = installed(dir)?;
    same_game(dir, &receipt, recipe)?;
    let rule = recipe.copy_rule(receipt.game.as_deref());
    let mut out = Placed::default();
    let todo = ways(rule, recipe, &receipt, dir);
    if todo.is_empty() {
        return Ok(out);
    }
    for p in &todo {
        check_place(p)?;
    }
    let Some(copy) = copy::imported(library, recipe, receipt.game.as_deref()) else {
        out.no_copy = true;
        return Ok(out);
    };
    // Lo que va por argumento o variable lo pone `launch`.
    if todo.iter().all(|p| p.store.is_none() && p.files.is_none() && p.config.is_none()) {
        return Ok(out);
    }
    closed(dir, &receipt)?;
    for p in todo {
        if let Some(to) = &p.store {
            let Some(dest) = settings::resolve(to, recipe, &receipt, dir) else { continue };
            if copy.file.is_none() {
                return Err(Error::BadRecipe(format!("{}: store necesita una copia que sea un archivo", recipe.entry)));
            }
            // Si ya hay uno, es del juego o del jugador: no se pisa.
            if !dest.exists() {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                }
                fs::copy(&copy.path, &dest).map_err(|e| Error::io(&dest, e))?;
                out.stored.push(dest);
            }
        }
        if let Some(to) = &p.files {
            let Some(dest) = resolve_dir(to, recipe, &receipt, dir) else { continue };
            if copy.file.is_some() {
                return Err(Error::BadRecipe(format!("{}: files necesita una copia que sea una carpeta", recipe.entry)));
            }
            let src = match &p.from {
                Some(sub) => match copy::find_ignoring_case(&copy.path, &RelPath::new(sub)?) {
                    Some(s) if s.is_dir() => s,
                    _ => return Err(Error::CopyIncomplete(format!("la carpeta «{sub}»"))),
                },
                None => copy.path.clone(),
            };
            copy_missing(&src, &dest, p.link, &mut out.stored)?;
        }
        if let Some(c) = &p.config {
            let Some(path) = settings::resolve(&c.file, recipe, &receipt, dir) else { continue };
            let format = c.format.or_else(|| ConfigFormat::from_name(&c.file)).ok_or_else(|| Error::BadRecipe(c.file.clone()))?;
            let value = Value::String(text_of(&expand(&c.value, dir, &copy)));
            let k = ConfigKey { file: &c.file, format, section: c.section.as_deref(), key: &c.key, value: &value };
            if !path.is_file() {
                if !c.create {
                    out.missing_files.push(path);
                    continue;
                }
                create_empty(&path, format)?;
            }
            // Manda lo del jugador: si ya tiene un valor, no se toca.
            if settings::read(&path, &k)?.is_none() {
                settings::write(&path, &k)?;
                out.written.push(k.label());
            }
        }
    }
    Ok(out)
}

/// Un archivo de configuración vacío pero válido, para los juegos que lo aceptan con una sola
/// clave (`create`).
fn create_empty(path: &Path, format: ConfigFormat) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let text = match format {
        ConfigFormat::Json => "{}\n",
        ConfigFormat::Toml | ConfigFormat::Ini => "",
    };
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// Copia a `dest` lo de `src` que no esté ya. Los enlaces no se siguen. Con `link`, cada
/// archivo se enlaza (enlace duro) y queda de solo lectura; si no se puede enlazar (otro
/// disco, un sistema de archivos sin enlaces), se copia.
fn copy_missing(src: &Path, dest: &Path, link: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
    for entry in fs::read_dir(src).map_err(|e| Error::io(src, e))? {
        let entry = entry.map_err(|e| Error::io(src, e))?;
        let kind = entry.file_type().map_err(|e| Error::io(entry.path(), e))?;
        let name = entry.file_name();
        // Sin distinguir mayúsculas: en Linux, «Data» y «data» serían dos carpetas.
        let existing = name.to_str().and_then(|n| RelPath::new(n).ok()).and_then(|r| copy::find_ignoring_case(dest, &r));
        let to = existing.unwrap_or_else(|| dest.join(&name));
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            copy_missing(&entry.path(), &to, link, out)?;
        } else if !to.exists() {
            if !(link && link_read_only(&entry.path(), &to)) {
                fs::copy(entry.path(), &to).map_err(|e| Error::io(&to, e))?;
            }
            out.push(to);
        }
    }
    Ok(())
}

/// Enlace duro de `src` en `to`, de solo lectura. Un enlace duro es el mismo archivo con dos
/// nombres y los atributos son del archivo: la copia de la biblioteca también queda de solo
/// lectura, y nadie puede escribir en ella a través del juego. false si no se pudo enlazar.
fn link_read_only(src: &Path, to: &Path) -> bool {
    if fs::hard_link(src, to).is_err() {
        return false;
    }
    let read_only = fs::metadata(to).map(|m| {
        let mut p = m.permissions();
        p.set_readonly(true);
        p
    });
    match read_only.and_then(|p| fs::set_permissions(to, p)) {
        Ok(()) => true,
        // Sin solo lectura el enlace no protege la copia: mejor quitarlo y copiar.
        Err(_) => {
            let _ = fs::remove_file(to);
            false
        }
    }
}

/// `{game}` y `{copy}` cambiados por sus rutas, sin pasar por texto.
fn expand(s: &str, game: &Path, copy: &Imported) -> OsString {
    let mut out = OsString::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        out.push(&rest[..i]);
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("{game}") {
            out.push(game.as_os_str());
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{copy}") {
            out.push(copy.path.as_os_str());
            rest = t;
        } else {
            out.push("{");
            rest = &tail[1..];
        }
    }
    out.push(rest);
    out
}

fn text_of(s: &OsString) -> String {
    s.to_string_lossy().into_owned()
}

/// Argumentos y variables de entorno para abrir el juego.
pub(crate) type Extras = (Vec<OsString>, Vec<(OsString, OsString)>);

/// Los argumentos y variables de `copy.place` para abrirlo (para `launch`). Nada si todavía no
/// hay copia guardada.
pub(crate) fn launch_extras(library: &Library, dir: &Path, recipe: &Recipe, receipt: &Receipt) -> Result<Extras> {
    let rule = recipe.copy_rule(receipt.game.as_deref());
    let todo: Vec<&Place> = ways(rule, recipe, receipt, dir).into_iter().filter(|p| !p.args.is_empty() || !p.env.is_empty()).collect();
    let (mut args, mut env) = (Vec::new(), Vec::new());
    if todo.is_empty() {
        return Ok((args, env));
    }
    let Some(copy) = copy::imported(library, recipe, receipt.game.as_deref()) else { return Ok((args, env)) };
    for p in todo {
        check_place(p)?;
        args.extend(p.args.iter().map(|a| expand(a, dir, &copy)));
        env.extend(p.env.iter().map(|(k, v)| (OsString::from(k), expand(v, dir, &copy))));
    }
    Ok((args, env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::copy::{import, import_with, imported, n64_to_z64};
    use crate::install::recipe::Accepted;
    use crate::install::portable::tests::instalado;
    use crate::install::recipe::tests::ZELDA;

    /// Una ROM de prueba que empieza como una de N64 (`80 37 12 40`).
    fn rom_z64() -> Vec<u8> {
        let mut r = vec![0x80, 0x37, 0x12, 0x40];
        r.extend((0..1020u32).map(|i| (i % 251) as u8));
        r
    }

    fn swap(data: &[u8], x: usize) -> Vec<u8> {
        let mut out = data.to_vec();
        for (i, b) in data.iter().enumerate() {
            out[(i & !3) | ((i & 3) ^ x)] = *b;
        }
        out
    }

    #[test]
    fn pasa_las_roms_de_n64_a_z64() {
        let z = rom_z64();
        assert_eq!(n64_to_z64(z.clone()).unwrap(), z);
        assert_eq!(n64_to_z64(swap(&z, 1)).unwrap(), z, ".v64");
        assert_eq!(n64_to_z64(swap(&z, 3)).unwrap(), z, ".n64");
        // Se rellena hasta un múltiplo de 4.
        let mut corta = z.clone();
        corta.truncate(1022);
        assert_eq!(n64_to_z64(corta).unwrap().len(), 1024);
        assert!(n64_to_z64(b"PK\x03\x04 un zip".to_vec()).is_none());
        assert!(n64_to_z64(vec![0x80]).is_none());
    }

    fn receta(copy: Value) -> Recipe {
        let mut v: Value = serde_json::from_str(ZELDA).unwrap();
        v["copy"] = copy;
        v["data-dir"] = serde_json::json!({ "windows-x64": "{game}/datos" });
        Recipe::from_json(&v.to_string()).unwrap()
    }

    fn zelda_con_copia(place: &str, hash: &str) -> Recipe {
        let place: Value = serde_json::from_str(place).unwrap();
        receta(serde_json::json!({
            "how": "in-app", "kind": "file", "convert": "n64-z64", "place": place,
            "accepts": [{ "name": "Majora's Mask (N64, EE. UU.)", "hash": { "xxh3_64": hash } }]
        }))
    }

    #[test]
    fn guarda_la_rom_comprobada_y_la_deja_donde_la_busca() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&rom_z64()));
        let r = zelda_con_copia(r#"[{ "store": "{data}/mm.n64.us.1.0.z64" }]"#, &hash);

        // Sin copia todavía: no hace nada y lo dice.
        assert!(place(&lib, &d, &r).unwrap().no_copy);

        // La persona da su ROM en .v64: se convierte, se comprueba y se guarda en la biblioteca.
        let src = t.path().join("Majora (USA).v64");
        fs::write(&src, swap(&rom_z64(), 1)).unwrap();
        let imp = import(&lib, &r, None, &src, false, &mut |_| {}).unwrap();
        assert!(imp.verified);
        assert_eq!(imp.accepted, ["Majora's Mask (N64, EE. UU.)"]);
        assert_eq!(imp.path, lib.copies.join("zelda-majoras-mask").join("Majora (USA).z64"));
        assert_eq!(fs::read(&imp.path).unwrap(), rom_z64());
        assert_eq!(imported(&lib, &r, None).unwrap(), imp);

        // Y se deja donde la busca el juego, una vez.
        let p = place(&lib, &d, &r).unwrap();
        let rom = d.join("datos").join("mm.n64.us.1.0.z64");
        assert_eq!(p.stored, std::slice::from_ref(&rom));
        assert_eq!(fs::read(&rom).unwrap(), rom_z64());
        assert_eq!(place(&lib, &d, &r).unwrap(), Placed::default());
    }

    #[test]
    fn comprueba_con_la_lista_que_publica_el_proyecto() {
        // Como Ship of Harkinian: la receta no trae hashes, los publica el proyecto (accepts-from).
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let r = receta(serde_json::json!({ "how": "in-app", "kind": "file", "convert": "n64-z64" }));
        let src = t.path().join("Majora.z64");
        fs::write(&src, rom_z64()).unwrap();
        let lista: Vec<Accepted> = serde_json::from_value(serde_json::json!([
            { "name": "MM US 1.0", "hash": { "xxh3_64": format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&rom_z64())) } }
        ]))
        .unwrap();
        // Sin la lista no hay con qué comprobarla.
        assert!(!import(&lib, &r, None, &src, false, &mut |_| {}).unwrap().verified);
        // Con la lista, sí.
        let imp = import_with(&lib, &r, None, &src, &lista, false, &mut |_| {}).unwrap();
        assert!(imp.verified);
        assert_eq!(imp.accepted, ["MM US 1.0"]);
        // Y una que no está en la lista no se guarda.
        fs::write(&src, swap(&rom_z64(), 2)).unwrap();
        let otra = t.path().join("otra.z64");
        let mut distinta = rom_z64();
        distinta[100] ^= 0xff;
        fs::write(&otra, distinta).unwrap();
        assert!(matches!(import_with(&lib, &r, None, &otra, &lista, false, &mut |_| {}), Err(Error::CopyMismatch(_))));
    }

    #[test]
    fn una_variante_sin_place_usa_el_de_la_receta() {
        // Como UZDoom: `-iwad {copy}` está en la receta y la variante (Doom) solo trae sus copias.
        // El 7-11 la variante no lo heredaba y el motor buscaba solo otra IWAD.
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let wad = b"IWAD de prueba".to_vec();
        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&wad));
        let mut v: Value = serde_json::from_str(ZELDA).unwrap();
        v["copy"] = serde_json::json!({ "how": "folder", "kind": "file", "place": [{ "args": ["-iwad", "{copy}"] }] });
        v["variants"] = serde_json::json!([{
            "game": "doom", "name": "Doom",
            "copy": { "how": "folder", "accepts": [{ "name": "doom.wad", "hash": { "xxh3_64": hash } }] }
        }]);
        let r = Recipe::from_json(&v.to_string()).unwrap();
        let mut rec = Receipt::read(&d).unwrap().unwrap();
        rec.game = Some("doom".into());
        rec.write(&d).unwrap();
        let src = t.path().join("DOOM.WAD");
        fs::write(&src, &wad).unwrap();
        let imp = import(&lib, &r, Some("doom"), &src, false, &mut |_| {}).unwrap();
        assert!(imp.verified);
        let (args, _) = launch_extras(&lib, &d, &r, &rec).unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], OsString::from("-iwad"));
        assert_eq!(PathBuf::from(&args[1]), imp.path);
    }

    #[test]
    fn no_escribe_nada_hasta_comprobar_la_copia() {
        // El 3-11, copiar una ISO entera para compararla después tumbó un disco USB. Ahora la copia
        // se comprueba donde está, y si no es la que pide el juego no se escribe nada.
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        // Donde va la biblioteca de copias hay un archivo: cualquier intento de escribir falla.
        fs::write(&lib.copies, b"no es una carpeta").unwrap();

        // Una ROM que no es: se rechaza sin llegar a escribir.
        let src = t.path().join("otra.z64");
        fs::write(&src, rom_z64()).unwrap();
        let no_es = zelda_con_copia("[]", "0000000000000000");
        assert!(matches!(import(&lib, &no_es, None, &src, false, &mut |_| {}), Err(Error::CopyMismatch(_))));
        // La que sí es llega a escribir (y aquí choca con el archivo de en medio).
        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&rom_z64()));
        assert!(matches!(import(&lib, &zelda_con_copia("[]", &hash), None, &src, false, &mut |_| {}), Err(Error::Io { .. })));

        // Una imagen de disco sin convertir: se lee entera donde está y no se copia.
        let iso = t.path().join("juego.iso");
        fs::write(&iso, vec![7u8; 4096]).unwrap();
        let r_iso = receta(serde_json::json!({
            "how": "in-app", "kind": "file", "accepts": [{ "name": "Juego (ISO)", "hash": { "sha1": "00" } }]
        }));
        let mut leidos = 0;
        assert!(matches!(import(&lib, &r_iso, None, &iso, false, &mut |n| leidos = n), Err(Error::CopyMismatch(_))));
        assert_eq!(leidos, 4096);

        // Una carpeta: se comprueban sus archivos en el origen.
        let carpeta = t.path().join("juego");
        fs::create_dir(&carpeta).unwrap();
        fs::write(carpeta.join("DATA.BIN"), b"x").unwrap();
        let r_dir = receta(serde_json::json!({
            "how": "in-app", "kind": "folder", "accepts": [{ "name": "Datos", "file": "data.bin", "hash": { "sha1": "00" } }]
        }));
        assert!(matches!(import(&lib, &r_dir, None, &carpeta, false, &mut |_| {}), Err(Error::CopyMismatch(_))));
    }

    #[test]
    fn no_guarda_una_copia_que_no_es() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let r = zelda_con_copia("[]", "0000000000000000");
        let src = t.path().join("otra.z64");
        fs::write(&src, rom_z64()).unwrap();
        assert!(matches!(import(&lib, &r, None, &src, false, &mut |_| {}), Err(Error::CopyMismatch(_))));
        assert!(imported(&lib, &r, None).is_none());
        // Si la persona insiste, se guarda, pero no como comprobada.
        let imp = import(&lib, &r, None, &src, true, &mut |_| {}).unwrap();
        assert!(!imp.verified && imp.accepted.is_empty());
        // Un archivo que no es una ROM, nunca.
        fs::write(&src, b"PK\x03\x04").unwrap();
        assert!(matches!(import(&lib, &r, None, &src, true, &mut |_| {}), Err(Error::NotARom(_))));
        // Y la que había sigue ahí.
        assert!(imported(&lib, &r, None).is_some());
    }

    #[test]
    fn pasa_la_rom_al_abrirlo_solo_si_falta_lo_que_genera() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&rom_z64()));
        let r = zelda_con_copia(r#"[{ "when-missing": "{data}/oot.o2r", "args": ["{copy}"] }, { "env": { "ROM": "{copy}" } }]"#, &hash);
        // Sin copia, al abrirlo no se añade nada.
        assert!(crate::install::launch::prepare(&lib, &d, &r).unwrap().args.is_empty());
        let src = t.path().join("mm.z64");
        fs::write(&src, rom_z64()).unwrap();
        let imp = import(&lib, &r, None, &src, false, &mut |_| {}).unwrap();
        let l = crate::install::launch::prepare(&lib, &d, &r).unwrap();
        assert_eq!(l.args, [imp.path.as_os_str().to_owned()]);
        assert_eq!(l.env, [(OsString::from("ROM"), imp.path.as_os_str().to_owned())]);
        // Cuando el juego ya generó su archivo, la ROM ya no se pasa; la variable, sí.
        fs::create_dir_all(d.join("datos")).unwrap();
        fs::write(d.join("datos").join("oot.o2r"), b"o2r").unwrap();
        let l = crate::install::launch::prepare(&lib, &d, &r).unwrap();
        assert!(l.args.is_empty());
        assert_eq!(l.env.len(), 1);
    }

    #[test]
    fn la_carpeta_del_juego_original() {
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let pak0 = b"pak0 de verdad";
        let md5 = {
            use md5::Digest;
            crate::net::hex(&md5::Md5::digest(pak0))
        };
        let r = receta(serde_json::json!({
            "how": "folder", "kind": "folder",
            "place": [
                { "files": "{game}" },
                { "config": { "file": "{data}/q3.json", "key": "game_path", "value": "{copy}", "create": true } }
            ],
            "accepts": [
                { "name": "pak0", "file": "baseq3/pak0.pk3", "hash": { "md5": md5 } },
                { "name": "pak1", "file": "baseq3/pak1.pk3", "required": false }
            ]
        }));
        let src = t.path().join("Quake III Arena");
        fs::create_dir_all(src.join("BASEQ3")).unwrap();
        // Le falta pak0: no se guarda.
        assert!(matches!(import(&lib, &r, None, &src, false, &mut |_| {}), Err(Error::CopyIncomplete(_))));
        // Con pak0 en mayúsculas, como en un CD, sí.
        fs::write(src.join("BASEQ3").join("PAK0.PK3"), pak0).unwrap();
        let imp = import(&lib, &r, None, &src, false, &mut |_| {}).unwrap();
        assert!(imp.verified && imp.file.is_none());
        assert_eq!(imp.accepted, ["pak0"]);

        // La carpeta del juego ya trae baseq3 con algo suyo: se añade lo de la copia sin pisarlo.
        fs::create_dir_all(d.join("baseq3")).unwrap();
        fs::write(d.join("baseq3").join("vm.pk3"), b"del port").unwrap();
        let p = place(&lib, &d, &r).unwrap();
        assert!(d.join("baseq3").join("vm.pk3").is_file());
        assert_eq!(fs::read(d.join("baseq3").join("PAK0.PK3")).unwrap(), pak0);
        assert_eq!(p.stored.len(), 1);
        // Y la ruta, en su configuración, que se crea porque la receta lo permite.
        let cfg: Value = serde_json::from_str(&fs::read_to_string(d.join("datos").join("q3.json")).unwrap()).unwrap();
        assert_eq!(cfg["game_path"], Value::String(imp.path.to_string_lossy().into_owned()));
        // Si la persona pone otra ruta, se queda la suya.
        fs::write(d.join("datos").join("q3.json"), "{\"game_path\": \"D:/Juegos/Quake3\"}").unwrap();
        assert!(place(&lib, &d, &r).unwrap().written.is_empty());
        assert!(fs::read_to_string(d.join("datos").join("q3.json")).unwrap().contains("D:/Juegos/Quake3"));
    }

    #[test]
    fn copia_solo_la_subcarpeta_y_lo_grande_se_lee_de_la_copia() {
        // Como Fallout: los .dat grandes se leen de la copia por argumento y solo `data` va a la
        // carpeta del juego, donde despliegan los mods y cada perfil tiene lo suyo.
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let r = receta(serde_json::json!({
            "how": "folder", "kind": "folder",
            "place": [
                { "files": "{game}/data", "from": "data" },
                { "args": ["[system]master_dat={copy}/master.dat"] }
            ],
            "accepts": [{ "name": "master.dat", "file": "master.dat", "required": false }]
        }));
        let src = t.path().join("Fallout");
        fs::create_dir_all(src.join("DATA").join("SOUND")).unwrap();
        fs::write(src.join("master.dat"), b"grande").unwrap();
        fs::write(src.join("DATA").join("SOUND").join("musica.acm"), b"musica").unwrap();
        let imp = import(&lib, &r, None, &src, true, &mut |_| {}).unwrap();

        // Un mod ya desplegado en data se queda como está.
        fs::create_dir_all(d.join("data")).unwrap();
        fs::write(d.join("data").join("mod.txt"), b"del mod").unwrap();
        let p = place(&lib, &d, &r).unwrap();
        assert_eq!(p.stored, [d.join("data").join("SOUND").join("musica.acm")]);
        assert_eq!(fs::read(d.join("data").join("mod.txt")).unwrap(), b"del mod");
        assert!(!d.join("master.dat").exists() && !d.join("data").join("master.dat").exists());
        let (args, _) = launch_extras(&lib, &d, &r, &Receipt::read(&d).unwrap().unwrap()).unwrap();
        assert_eq!(args, [OsString::from(format!("[system]master_dat={}/master.dat", imp.path.display()))]);

        // Si la copia no trae esa carpeta, se dice qué le falta y no se copia nada.
        fs::remove_dir_all(imp.path.join("DATA")).unwrap();
        fs::remove_dir_all(d.join("data").join("SOUND")).unwrap();
        assert!(matches!(place(&lib, &d, &r), Err(Error::CopyIncomplete(_))));
        assert!(!d.join("data").join("SOUND").exists());
    }

    /// Con una ROM de Majora's Mask de verdad (la que guardó el propio Zelda64Recomp, por
    /// ejemplo): tiene que salir comprobada con el XXH3 de la receta y quedar donde la busca.
    #[test]
    #[ignore = "necesita UNRICOPIE_ROM_MM=<ruta de una ROM de Majora's Mask de EE. UU.>"]
    fn importa_una_rom_de_verdad() {
        let src = PathBuf::from(std::env::var("UNRICOPIE_ROM_MM").expect("UNRICOPIE_ROM_MM"));
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let r = zelda_con_copia(r#"[{ "store": "{data}/mm.n64.us.1.0.z64" }]"#, "ef18b4a9e2386169");
        let imp = import(&lib, &r, None, &src, false, &mut |_| {}).unwrap();
        eprintln!("{imp:#?}");
        assert!(imp.verified);
        let p = place(&lib, &d, &r).unwrap();
        assert_eq!(p.stored.len(), 1);
        let stored = fs::read(&p.stored[0]).unwrap();
        assert_eq!(xxhash_rust::xxh3::xxh3_64(&stored), 0xef18b4a9e2386169);
        // La misma ROM en .v64 y en .n64: se convierte y sale igual de comprobada.
        for (ext, x) in [("v64", 1), ("n64", 3)] {
            let other = t.path().join(format!("mm.{ext}"));
            fs::write(&other, swap(&stored, x)).unwrap();
            let imp = import(&lib, &r, None, &other, false, &mut |_| {}).unwrap();
            assert!(imp.verified, "{ext}");
            assert_eq!(fs::read(&imp.path).unwrap(), stored, "{ext}");
        }
    }

    #[test]
    fn enlaza_de_solo_lectura_y_la_copia_no_se_toca() {
        // Como RBDOOM-3-BFG: los .pk4 de base/ se enlazan en vez de copiarse (7,6 GB menos),
        // y el solo lectura impide que el juego, o un mod, escriba en la copia a través del enlace.
        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let d = instalado(t.path());
        let r = receta(serde_json::json!({
            "how": "folder", "kind": "folder",
            "place": [{ "files": "{game}/base", "from": "base", "link": true }],
            "accepts": [{ "name": "pak000", "file": "base/pak000.pk4", "required": false }]
        }));
        let src = t.path().join("DOOM 3 BFG Edition");
        fs::create_dir_all(src.join("base")).unwrap();
        fs::write(src.join("base").join("pak000.pk4"), b"original").unwrap();
        let imp = import(&lib, &r, None, &src, true, &mut |_| {}).unwrap();
        let in_copy = imp.path.join("base").join("pak000.pk4");

        let p = place(&lib, &d, &r).unwrap();
        let linked = d.join("base").join("pak000.pk4");
        assert_eq!(p.stored, std::slice::from_ref(&linked));
        assert!(fs::metadata(&linked).unwrap().permissions().readonly());
        // El atributo es del archivo, así que la copia también queda de solo lectura.
        assert!(fs::metadata(&in_copy).unwrap().permissions().readonly());
        // Escribir a través del juego falla, y la copia sigue igual.
        assert!(fs::write(&linked, b"modificado").is_err());
        assert_eq!(fs::read(&in_copy).unwrap(), b"original");
        // Al quitar el juego se va el enlace, no la copia.
        fs::remove_dir_all(&d).unwrap();
        assert_eq!(fs::read(&in_copy).unwrap(), b"original");
        // Y la copia se puede borrar igual, aunque sea de solo lectura.
        fs::remove_dir_all(&imp.path).unwrap();
        assert!(!in_copy.exists());
    }

    #[test]
    fn formas_que_no_valen() {
        let p = |json: &str| serde_json::from_str::<Place>(json).unwrap();
        assert!(check_place(&p(r#"{ "args": ["--rom-file", "{copy}"] }"#)).is_ok());
        assert!(check_place(&p(r#"{ "files": "{data}" }"#)).is_ok());
        assert!(check_place(&p(r#"{ "files": "{game}/base", "from": "base" }"#)).is_ok());
        for bad in [
            r#"{ "args": ["{copy}"], "link": true }"#,
            r#"{ "args": ["{copy}"], "from": "data" }"#,
            r#"{ "files": "{game}", "from": "../fuera" }"#,
            r#"{}"#,
            r#"{ "when-missing": "{data}/x" }"#,
            r#"{ "store": "{data}/../fuera.z64" }"#,
            r#"{ "files": "{home}" }"#,
            r#"{ "args": ["{home}/x"] }"#,
            r#"{ "env": { "1X": "{copy}" } }"#,
            r#"{ "config": { "file": "{data}/x.cfg", "key": "a", "value": "{copy}" } }"#,
        ] {
            assert!(check_place(&p(bad)).is_err(), "{bad}");
        }
    }
}
