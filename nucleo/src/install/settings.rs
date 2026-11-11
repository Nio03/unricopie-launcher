//! Lo que Unricopie escribe en la configuración de cada juego. Por ahora, apagar su
//! actualizador propio (`self-update` de la receta): si el juego se baja y cambia su propio
//! ejecutable, se rompen los canales y la vuelta atrás. Después vendrán unos pocos ajustes útiles.
//!
//! **Quién gana (decidido el 1-11): lo del jugador.** Unricopie apunta en el recibo lo que
//! escribió, y lo que había antes, y antes de abrir el juego (`apply`) lee cada clave:
//! - si ya tiene el valor que quiere Unricopie, no hace nada;
//! - si falta la clave, la escribe;
//! - si tiene otro valor, gana el juego y no se toca. La excepción es el actualizador: se vuelve
//!   a apagar y se avisa (`Applied::reapplied`), salvo que la persona lo deje encendido
//!   (`allow_self_update`, que además devuelve el valor que había antes).
//!
//! Solo con el juego cerrado y solo la clave de la receta. No se pierde el resto del archivo:
//! en toml e ini se conservan los comentarios y el orden; en json, el orden de las claves. Un
//! archivo que no existe no se crea: muchos juegos lo generan entero la primera vez, y uno a
//! medias podría romperlo. Queda en `Applied::missing_files` y se escribe la vez siguiente.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::data::{DataPath, Places};
use super::portable::same_game;
use super::receipt::{Receipt, WrittenSetting, FILE_NAME};
use super::recipe::{ConfigFormat, Recipe, SelfUpdateOff};
use super::update::{closed, installed};
use crate::path::RelPath;
use crate::{Error, Result};

/// Lo que hizo `apply`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Claves que se escribieron por primera vez.
    pub written: Vec<String>,
    /// El actualizador estaba encendido otra vez (desde el juego o por una versión nueva) y se
    /// volvió a apagar: el launcher tiene que avisar.
    pub reapplied: Vec<String>,
    /// Archivos que todavía no existen: se escribirán después de abrir el juego una vez.
    pub missing_files: Vec<PathBuf>,
}

/// Una clave de un archivo de configuración, ya comprobada.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigKey<'a> {
    pub file: &'a str,
    pub format: ConfigFormat,
    pub section: Option<&'a str>,
    pub key: &'a str,
    pub value: &'a Value,
}

impl ConfigKey<'_> {
    pub(crate) fn label(&self) -> String {
        match self.section {
            Some(s) => format!("{} [{s}] {}", self.file, self.key),
            None => format!("{} {}", self.file, self.key),
        }
    }

    fn same(&self, w: &WrittenSetting) -> bool {
        w.file == self.file && w.section.as_deref() == self.section && w.key == self.key
    }
}

/// La clave de `off`, si la tiene. Error si está a medias o es rara.
pub(crate) fn config_key(off: &SelfUpdateOff) -> Result<Option<ConfigKey<'_>>> {
    let bad = |what: &str| Err(Error::BadRecipe(format!("self-update: {what}")));
    let (file, key, value) = match (&off.file, &off.key, &off.value) {
        (None, None, None) => return Ok(None),
        (Some(f), Some(k), Some(v)) => (f.as_str(), k.as_str(), v),
        _ => return bad("una clave necesita file, key y value"),
    };
    if key.is_empty() || off.section.as_deref() == Some("") {
        return bad("clave o sección vacía");
    }
    if !path_ok(file) {
        return bad(&format!("ruta no válida «{file}»"));
    }
    let format = match off.format.or_else(|| ConfigFormat::from_name(file)) {
        Some(f) => f,
        None => return bad(&format!("no se sabe el formato de «{file}»")),
    };
    if format == ConfigFormat::Ini && !(value.is_string() || value.is_boolean() || value.is_number()) {
        return bad("en un ini, el valor es un texto, un número o un sí/no");
    }
    Ok(Some(ConfigKey { file, format, section: off.section.as_deref(), key, value }))
}

/// Comprueba una forma de apagar el actualizador entera.
pub fn check_off(off: &SelfUpdateOff) -> Result<()> {
    let key = config_key(off)?;
    if key.is_none() && off.args.is_empty() && off.env.is_empty() {
        return Err(Error::BadRecipe("self-update: disable sin clave, argumentos ni variables".into()));
    }
    let rule = super::recipe::PortableRule { args: off.args.clone(), env: off.env.clone(), ..Default::default() };
    super::portable::check_rule(&rule)
}

/// `{data}/<ruta>` o una ruta de `data-dir` (`{game}/config.toml`), siempre a un archivo.
pub(crate) fn path_ok(file: &str) -> bool {
    match file.strip_prefix("{data}/") {
        Some(tail) => RelPath::new(tail).is_ok_and(|r| r.as_str() == tail),
        None => DataPath::parse(file).is_some_and(|d| d.rel.is_some()),
    }
}

/// La carpeta de datos del juego tal como está ahora: la de `portable` si Unricopie lo hizo
/// portable, y si no la primera de `data-dir`.
pub(crate) fn data_folder(recipe: &Recipe, receipt: &Receipt, dir: &Path) -> Option<PathBuf> {
    let portable = receipt.portable.as_ref().and(recipe.portable_rule(receipt.platform)).map(|r| r.data_dirs());
    let dirs = portable.unwrap_or_else(|| recipe.data_dirs(receipt.platform));
    dirs.first()?.resolve(&Places::current(), dir)
}

/// Dónde está el archivo en este equipo. `None` si no se puede saber aquí.
pub(crate) fn resolve(file: &str, recipe: &Recipe, receipt: &Receipt, dir: &Path) -> Option<PathBuf> {
    let path = match file.strip_prefix("{data}/") {
        Some(tail) => RelPath::new(tail).ok()?.under(&data_folder(recipe, receipt, dir)?),
        None => DataPath::parse(file)?.resolve(&Places::current(), dir)?,
    };
    // El recibo es de Unricopie, nunca del juego.
    (path != dir.join(FILE_NAME)).then_some(path)
}

/// Qué hacer con una clave: lo que hay ahora, lo que quiere Unricopie y si manda Unricopie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Nothing,
    Write,
    /// Tiene otro valor y gana el jugador.
    Keep,
}

fn decide(now: Option<&Value>, want: &Value, format: ConfigFormat, enforced: bool) -> Action {
    match now {
        Some(v) if same_value(v, want, format) => Action::Nothing,
        None => Action::Write,
        Some(_) if enforced => Action::Write,
        Some(_) => Action::Keep,
    }
}

/// En un ini todo es texto: `false`, `0` y `"false"` se comparan por cómo se escriben.
fn same_value(now: &Value, want: &Value, format: ConfigFormat) -> bool {
    match format {
        ConfigFormat::Ini => ini_text(now).eq_ignore_ascii_case(&ini_text(want)),
        _ => now == want,
    }
}

/// Antes de abrir el juego: apaga su actualizador (si la persona no lo dejó encendido) según
/// las reglas de arriba.
pub fn apply(dir: &Path, recipe: &Recipe) -> Result<Applied> {
    let mut receipt = installed(dir)?;
    same_game(dir, &receipt, recipe)?;
    let mut out = Applied::default();
    if receipt.self_update_allowed {
        return Ok(out);
    }
    let Some(su) = &recipe.self_update else { return Ok(out) };
    let keys: Vec<ConfigKey<'_>> = su
        .disable
        .iter()
        .filter(|o| o.applies_to(receipt.platform, receipt.portable.is_some()))
        .filter_map(|o| config_key(o).transpose())
        .collect::<Result<_>>()?;
    if keys.is_empty() {
        return Ok(out);
    }
    closed(dir, &receipt)?;
    let mut changed = false;
    let result = (|| -> Result<()> {
        for k in keys {
            let Some(path) = resolve(k.file, recipe, &receipt, dir) else { continue };
            if !path.is_file() {
                out.missing_files.push(path);
                continue;
            }
            let now = read(&path, &k)?;
            if decide(now.as_ref(), k.value, k.format, true) != Action::Write {
                continue;
            }
            write(&path, &k)?;
            changed = true;
            match receipt.settings.iter().position(|w| k.same(w)) {
                // Ya lo había apagado: alguien lo volvió a encender.
                Some(_) if now.is_some() => out.reapplied.push(k.label()),
                Some(_) => out.written.push(k.label()),
                None => {
                    receipt.settings.push(WrittenSetting {
                        file: k.file.to_string(),
                        section: k.section.map(str::to_string),
                        key: k.key.to_string(),
                        wrote: k.value.clone(),
                        before: now,
                    });
                    out.written.push(k.label());
                }
            }
        }
        Ok(())
    })();
    // Lo que sí se escribió queda en el recibo aunque falle una clave posterior: sin eso no se
    // sabría qué valor había antes (el 6-11, en el uzdoom.ini de la persona, se perdió así).
    if changed {
        receipt.write(dir)?;
    }
    result?;
    Ok(out)
}

/// La persona decide si el juego puede actualizarse solo. Al dejarlo, cada clave que escribió
/// Unricopie y que sigue igual vuelve a tener el valor de antes (o desaparece si no estaba).
pub fn allow_self_update(dir: &Path, recipe: &Recipe, allow: bool) -> Result<Receipt> {
    let mut receipt = installed(dir)?;
    same_game(dir, &receipt, recipe)?;
    closed(dir, &receipt)?;
    receipt.self_update_allowed = allow;
    if allow {
        let written = std::mem::take(&mut receipt.settings);
        for w in written {
            let Some(path) = resolve(&w.file, recipe, &receipt, dir) else { continue };
            let format = recipe
                .self_update
                .iter()
                .flat_map(|s| &s.disable)
                .filter_map(|o| config_key(o).ok().flatten())
                .find(|k| k.same(&w))
                .map(|k| k.format)
                .or_else(|| ConfigFormat::from_name(&w.file));
            let (Some(format), true) = (format, path.is_file()) else { continue };
            let ours = ConfigKey { file: &w.file, format, section: w.section.as_deref(), key: &w.key, value: &w.wrote };
            // Si alguien ya lo cambió, se queda como está.
            if read(&path, &ours)?.is_some_and(|v| same_value(&v, &w.wrote, format)) {
                match &w.before {
                    Some(b) => write(&path, &ConfigKey { value: b, ..ours.clone() })?,
                    None => remove(&path, &ours)?,
                }
            }
        }
    }
    receipt.write(dir)?;
    Ok(receipt)
}

/// Los argumentos y variables con que se apaga el actualizador al abrirlo (para `launch`).
pub(crate) fn launch_extras<'a>(recipe: &'a Recipe, receipt: &Receipt) -> Vec<&'a SelfUpdateOff> {
    if receipt.self_update_allowed {
        return Vec::new();
    }
    let portable = receipt.portable.is_some();
    recipe.self_update.iter().flat_map(|s| &s.disable).filter(|o| o.applies_to(receipt.platform, portable)).collect()
}

// --- Leer y escribir una clave sin tocar lo demás ---

fn load(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    String::from_utf8(bytes).map_err(|_| Error::ConfigNotUtf8(path.to_path_buf()))
}

/// Lo que dijo el lector de json o toml.
fn config_err(path: &Path, reason: impl ToString) -> Error {
    Error::Config { path: path.to_path_buf(), reason: reason.to_string() }
}

/// La sección `key` (vacía: la configuración entera) no es un objeto o una tabla.
fn shape_err(path: &Path, key: &str) -> Error {
    Error::ConfigShape { path: path.to_path_buf(), key: key.to_string() }
}

/// Escribe aparte y renombra: un corte a medias no deja la configuración rota.
fn save(path: &Path, text: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".unricopie-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
    // En Windows, OneDrive o un antivirus pueden tener el archivo abierto un momento justo después
    // de cambiarlo: se reintenta un poco. Si no se puede, el temporal no se queda tirado.
    let mut tries = 0u64;
    loop {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 4 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(150 * tries));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(Error::io(path, e));
            }
        }
    }
}

pub(crate) fn read(path: &Path, k: &ConfigKey<'_>) -> Result<Option<Value>> {
    let text = load(path)?;
    match k.format {
        ConfigFormat::Json => {
            let doc: Value = serde_json::from_str(&text).map_err(|e| config_err(path, e))?;
            let obj = match k.section {
                Some(s) => doc.get(s),
                None => Some(&doc),
            };
            Ok(obj.and_then(|o| o.get(k.key)).cloned())
        }
        ConfigFormat::Toml => {
            let doc: toml_edit::DocumentMut = text.parse().map_err(|e| config_err(path, e))?;
            let item = match k.section {
                Some(s) => doc.get(s).and_then(|t| t.get(k.key)),
                None => doc.get(k.key),
            };
            Ok(item.and_then(|i| i.as_value()).map(toml_to_json))
        }
        ConfigFormat::Ini => Ok(ini_find(&text, k.section, k.key).map(|(_, v)| Value::String(v))),
    }
}

pub(crate) fn write(path: &Path, k: &ConfigKey<'_>) -> Result<()> {
    let text = load(path)?;
    let new = match k.format {
        ConfigFormat::Json => {
            let mut doc: Value = serde_json::from_str(&text).map_err(|e| config_err(path, e))?;
            let root = doc.as_object_mut().ok_or_else(|| shape_err(path, ""))?;
            let obj = match k.section {
                Some(s) => root
                    .entry(s)
                    .or_insert_with(|| Value::Object(Default::default()))
                    .as_object_mut()
                    .ok_or_else(|| shape_err(path, s))?,
                None => root,
            };
            obj.insert(k.key.to_string(), k.value.clone());
            let mut s = serde_json::to_string_pretty(&doc).map_err(|e| config_err(path, e))?;
            if text.ends_with('\n') {
                s.push('\n');
            }
            s
        }
        ConfigFormat::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| config_err(path, e))?;
            let value = json_to_toml(k.value).ok_or_else(|| Error::ConfigValue(path.to_path_buf()))?;
            let table = match k.section {
                Some(s) => doc
                    .entry(s)
                    .or_insert(toml_edit::table())
                    .as_table_like_mut()
                    .ok_or_else(|| shape_err(path, s))?,
                None => doc.as_table_mut() as &mut dyn toml_edit::TableLike,
            };
            match table.get_mut(k.key).and_then(|i| i.as_value_mut()) {
                // Se cambia el valor y se queda su adorno (el comentario de la misma línea).
                Some(old) => {
                    let decor = old.decor().clone();
                    *old = value;
                    *old.decor_mut() = decor;
                }
                None => {
                    table.insert(k.key, toml_edit::Item::Value(value));
                }
            }
            doc.to_string()
        }
        ConfigFormat::Ini => ini_set(&text, k.section, k.key, Some(&ini_text(k.value))),
    };
    save(path, &new)
}

fn remove(path: &Path, k: &ConfigKey<'_>) -> Result<()> {
    let text = load(path)?;
    let new = match k.format {
        ConfigFormat::Json => {
            let mut doc: Value = serde_json::from_str(&text).map_err(|e| config_err(path, e))?;
            let obj = match k.section {
                Some(s) => doc.get_mut(s),
                None => Some(&mut doc),
            };
            if let Some(o) = obj.and_then(Value::as_object_mut) {
                o.shift_remove(k.key);
            }
            let mut s = serde_json::to_string_pretty(&doc).map_err(|e| config_err(path, e))?;
            if text.ends_with('\n') {
                s.push('\n');
            }
            s
        }
        ConfigFormat::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| config_err(path, e))?;
            match k.section {
                Some(s) => {
                    if let Some(t) = doc.get_mut(s).and_then(|i| i.as_table_like_mut()) {
                        t.remove(k.key);
                    }
                }
                None => {
                    doc.remove(k.key);
                }
            }
            doc.to_string()
        }
        ConfigFormat::Ini => ini_set(&text, k.section, k.key, None),
    };
    save(path, &new)
}

fn toml_to_json(v: &toml_edit::Value) -> Value {
    match v {
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value()).map(Value::Number).unwrap_or(Value::Null),
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        // Fechas, listas y tablas no son ajustes que toque Unricopie: se comparan como texto.
        other => Value::String(other.to_string().trim().to_string()),
    }
}

fn json_to_toml(v: &Value) -> Option<toml_edit::Value> {
    Some(match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) if n.is_i64() => n.as_i64()?.into(),
        Value::Number(n) => n.as_f64()?.into(),
        Value::String(s) => s.as_str().into(),
        _ => return None,
    })
}

fn ini_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// La línea y el valor de `key` en `section` (`None`: antes de la primera sección). Sin
/// distinguir mayúsculas, como Windows. Los comentarios empiezan por `;` o `#`.
fn ini_find(text: &str, section: Option<&str>, key: &str) -> Option<(usize, String)> {
    let mut current: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            current = Some(name.trim().to_string());
            continue;
        }
        if t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        let in_section = match (section, &current) {
            (None, None) => true,
            (Some(s), Some(c)) => c.eq_ignore_ascii_case(s),
            _ => false,
        };
        if let (true, Some((k, v))) = (in_section, t.split_once('=')) {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some((i, v.trim().to_string()));
            }
        }
    }
    None
}

/// `text` con `key` puesto a `value` (o quitado, con `None`), sin tocar las demás líneas. Si no
/// estaba, va al final de su sección; si tampoco hay sección, se añade al final.
fn ini_set(text: &str, section: Option<&str>, key: &str, value: Option<&str>) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match (ini_find(text, section, key), value) {
        (Some((i, _)), Some(v)) => {
            let line = &lines[i];
            let eq = line.find('=').unwrap_or(line.len());
            let space = if line[eq + 1..].starts_with(' ') { " " } else { "" };
            lines[i] = format!("{}{space}{v}", &line[..=eq]);
        }
        (Some((i, _)), None) => {
            lines.remove(i);
        }
        (None, None) => return text.to_string(),
        (None, Some(v)) => {
            let entry = format!("{key}={v}");
            // Dónde empieza y acaba la sección.
            let header = |l: &str| l.trim().starts_with('[') && l.trim().ends_with(']');
            let start = match section {
                None => Some(0),
                Some(s) => lines
                    .iter()
                    .position(|l| header(l) && l.trim()[1..l.trim().len() - 1].trim().eq_ignore_ascii_case(s))
                    .map(|p| p + 1),
            };
            match start {
                Some(start) => {
                    let end = lines[start..].iter().position(|l| header(l)).map_or(lines.len(), |p| start + p);
                    // Tras la última línea con algo, para no dejarla detrás de los blancos.
                    let at = (start..end).rev().find(|&j| !lines[j].trim().is_empty()).map_or(start, |j| j + 1);
                    lines.insert(at, entry);
                }
                None => {
                    if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                        lines.push(String::new());
                    }
                    lines.push(format!("[{}]", section.unwrap_or_default()));
                    lines.push(entry);
                }
            }
        }
    }
    let mut out = lines.join(nl);
    if text.ends_with('\n') || text.is_empty() {
        out.push_str(nl);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::portable::tests::instalado;
    use crate::install::recipe::tests::ZELDA;

    fn key<'a>(file: &'a str, format: ConfigFormat, section: Option<&'a str>, k: &'a str, value: &'a Value) -> ConfigKey<'a> {
        ConfigKey { file, format, section, key: k, value }
    }

    #[test]
    fn no_deja_el_temporal_si_no_puede_escribir() {
        // Donde va el archivo hay una carpeta: el renombrado falla y el temporal no se queda.
        let t = tempfile::tempdir().unwrap();
        let destino = t.path().join("uzdoom.ini");
        fs::create_dir(&destino).unwrap();
        assert!(save(&destino, "updater_check_updates=false\n").is_err());
        assert!(!t.path().join("uzdoom.ini.unricopie-tmp").exists());
        // Y donde sí se puede, escribe y no deja nada más.
        let bien = t.path().join("bien.ini");
        save(&bien, "a=1\n").unwrap();
        assert_eq!(fs::read_to_string(&bien).unwrap(), "a=1\n");
        assert!(!t.path().join("bien.ini.unricopie-tmp").exists());
    }

    #[test]
    fn toca_una_clave_de_un_toml_sin_perder_lo_demas() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("config.toml");
        let orig = "# Ajustes de OpenCE\n[update]\nauto = true # mira al arrancar\nchannel = \"stable\"\n\n[video]\nvsync = true\n";
        fs::write(&f, orig).unwrap();
        let off = Value::Bool(false);
        let k = key("{game}/config.toml", ConfigFormat::Toml, Some("update"), "auto", &off);
        assert_eq!(read(&f, &k).unwrap(), Some(Value::Bool(true)));
        write(&f, &k).unwrap();
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            "# Ajustes de OpenCE\n[update]\nauto = false # mira al arrancar\nchannel = \"stable\"\n\n[video]\nvsync = true\n"
        );
        // Una clave nueva en una sección que no estaba.
        let n = Value::from(3);
        write(&f, &key("x", ConfigFormat::Toml, Some("nueva"), "n", &n)).unwrap();
        assert_eq!(read(&f, &key("x", ConfigFormat::Toml, Some("nueva"), "n", &n)).unwrap(), Some(n.clone()));
        remove(&f, &key("x", ConfigFormat::Toml, Some("nueva"), "n", &n)).unwrap();
        assert_eq!(read(&f, &key("x", ConfigFormat::Toml, Some("nueva"), "n", &n)).unwrap(), None);
    }

    #[test]
    fn toca_una_clave_de_un_json_sin_desordenarlo() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("general.json");
        fs::write(&f, "{\n    \"targeting_mode\": \"Switch\",\n    \"check_updates\": true,\n    \"analog_cam\": false\n}\n").unwrap();
        let off = Value::Bool(false);
        let k = key("{data}/general.json", ConfigFormat::Json, None, "check_updates", &off);
        write(&f, &k).unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        let keys: Vec<&String> = back.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["targeting_mode", "check_updates", "analog_cam"]);
        assert_eq!(back["check_updates"], Value::Bool(false));
        assert!(fs::read_to_string(&f).unwrap().ends_with("}\n"));
    }

    #[test]
    fn toca_una_clave_de_un_ini_sin_perder_lo_demas() {
        let orig = "; ajustes\r\n[General]\r\nCheckForUpdates = 1\r\nLanguage=es\r\n\r\n[Video]\r\nFullscreen=0\r\n";
        let s = ini_set(orig, Some("general"), "checkforupdates", Some("0"));
        assert_eq!(s, "; ajustes\r\n[General]\r\nCheckForUpdates = 0\r\nLanguage=es\r\n\r\n[Video]\r\nFullscreen=0\r\n");
        // Una clave que no estaba va al final de su sección, antes del blanco.
        let s2 = ini_set(&s, Some("General"), "AutoUpdate", Some("false"));
        assert_eq!(s2, "; ajustes\r\n[General]\r\nCheckForUpdates = 0\r\nLanguage=es\r\nAutoUpdate=false\r\n\r\n[Video]\r\nFullscreen=0\r\n");
        // Y una sección que no estaba, al final.
        let s3 = ini_set("[A]\nx=1\n", Some("Update"), "auto", Some("0"));
        assert_eq!(s3, "[A]\nx=1\n\n[Update]\nauto=0\n");
        assert_eq!(ini_find(&s3, Some("update"), "AUTO").unwrap().1, "0");
        // Quitarla deja lo demás.
        assert_eq!(ini_set(&s2, Some("General"), "AutoUpdate", None), s);
        // Lo comentado no cuenta.
        assert!(ini_find("[A]\n;x=1\n", Some("A"), "x").is_none());
    }

    #[test]
    fn quien_gana() {
        let f = Value::Bool(false);
        let t = Value::Bool(true);
        assert_eq!(decide(Some(&f), &f, ConfigFormat::Json, false), Action::Nothing);
        assert_eq!(decide(None, &f, ConfigFormat::Json, false), Action::Write);
        // Lo cambió el jugador: gana él...
        assert_eq!(decide(Some(&t), &f, ConfigFormat::Json, false), Action::Keep);
        // ...salvo en el actualizador.
        assert_eq!(decide(Some(&t), &f, ConfigFormat::Json, true), Action::Write);
        // En un ini, 0 y "0" son lo mismo.
        assert_eq!(decide(Some(&Value::String("0".into())), &Value::from(0), ConfigFormat::Ini, false), Action::Nothing);
    }

    fn zelda_con_actualizador(file: &str) -> Recipe {
        let json = ZELDA.replace(
            r#""copy":"#,
            &format!(
                r#""data-dir": {{ "windows-x64": "{{game}}/datos" }},
                "self-update": {{ "asks": false, "disable": [
                    {{ "file": "{file}", "key": "check_updates", "value": false }},
                    {{ "platforms": ["windows-x64"], "args": ["--no-update"] }}
                ] }},
                "copy":"#
            ),
        );
        Recipe::from_json(&json).unwrap()
    }

    #[test]
    fn apaga_el_actualizador_y_lo_vuelve_a_apagar() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let r = zelda_con_actualizador("{data}/general.json");
        // Antes de la primera vez no hay archivo: no se crea.
        let a = apply(&d, &r).unwrap();
        assert_eq!(a.missing_files, [d.join("datos").join("general.json")]);
        assert!(!d.join("datos").exists());

        // El juego lo crea al abrirlo, con el actualizador encendido.
        fs::create_dir_all(d.join("datos")).unwrap();
        fs::write(d.join("datos").join("general.json"), "{\"volume\": 3, \"check_updates\": true}").unwrap();
        let a = apply(&d, &r).unwrap();
        assert_eq!(a.written, ["{data}/general.json check_updates"]);
        assert!(a.reapplied.is_empty());
        let json = |d: &Path| -> Value { serde_json::from_str(&fs::read_to_string(d.join("datos").join("general.json")).unwrap()).unwrap() };
        assert_eq!(json(&d)["check_updates"], Value::Bool(false));
        assert_eq!(json(&d)["volume"], Value::from(3));
        let rec = Receipt::read(&d).unwrap().unwrap();
        assert_eq!(rec.settings[0].before, Some(Value::Bool(true)));
        // Otra vez, nada.
        assert_eq!(apply(&d, &r).unwrap(), Applied::default());

        // Lo encienden desde el juego: se vuelve a apagar y se avisa.
        fs::write(d.join("datos").join("general.json"), "{\"volume\": 3, \"check_updates\": true}").unwrap();
        let a = apply(&d, &r).unwrap();
        assert_eq!(a.reapplied, ["{data}/general.json check_updates"]);
        assert_eq!(json(&d)["check_updates"], Value::Bool(false));

        // Al abrirlo, también el argumento.
        let l = crate::install::launch::prepare(&crate::install::installer::Library::new(t.path()), &d, &r).unwrap();
        assert_eq!(l.args, [std::ffi::OsString::from("--no-update")]);

        // La persona lo deja encendido: vuelve lo de antes y ya no se toca ni se pone el argumento.
        let rec = allow_self_update(&d, &r, true).unwrap();
        assert!(rec.self_update_allowed && rec.settings.is_empty());
        assert_eq!(json(&d)["check_updates"], Value::Bool(true));
        assert_eq!(apply(&d, &r).unwrap(), Applied::default());
        assert!(crate::install::launch::prepare(&crate::install::installer::Library::new(t.path()), &d, &r).unwrap().args.is_empty());
        // Y otra vez apagado.
        allow_self_update(&d, &r, false).unwrap();
        apply(&d, &r).unwrap();
        assert_eq!(json(&d)["check_updates"], Value::Bool(false));
    }

    #[test]
    fn cada_modo_con_su_archivo() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let json = ZELDA.replace(
            r#""copy":"#,
            r#""data-dir": { "windows-x64": "{game}/normal" },
                "portable": { "windows-x64": { "files": { "juego_portable.ini": "" } } },
                "self-update": { "asks": true, "disable": [
                    { "mode": "normal", "file": "{game}/normal/juego.ini", "section": "GlobalSettings", "key": "updater_check_updates", "value": false },
                    { "mode": "portable", "file": "{game}/juego_portable.ini", "section": "GlobalSettings", "key": "updater_check_updates", "value": false }
                ] },
                "copy":"#,
        );
        let r = Recipe::from_json(&json).unwrap();
        assert!(r.self_update.as_ref().unwrap().downloads, "si no se dice, se supone que baja");
        fs::create_dir_all(d.join("normal")).unwrap();
        fs::write(d.join("normal").join("juego.ini"), "[GlobalSettings]\n").unwrap();
        // Normal: solo su archivo, y el portable ni se menciona aunque no exista.
        let a = apply(&d, &r).unwrap();
        assert_eq!(a.written.len(), 1);
        assert!(a.missing_files.is_empty());
        // Portable: el ini que crea la regla, ya con la clave.
        crate::install::portable::enable(&d, &r).unwrap();
        let a = apply(&d, &r).unwrap();
        assert_eq!(a.written, ["{game}/juego_portable.ini [GlobalSettings] updater_check_updates"]);
        assert_eq!(fs::read_to_string(d.join("juego_portable.ini")).unwrap(), "[GlobalSettings]\nupdater_check_updates=false\n");
    }

    #[test]
    fn al_dejarlo_quita_la_clave_si_no_estaba() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let r = zelda_con_actualizador("{game}/general.json");
        fs::write(d.join("general.json"), "{\"volume\": 3}").unwrap();
        apply(&d, &r).unwrap();
        assert!(fs::read_to_string(d.join("general.json")).unwrap().contains("check_updates"));
        allow_self_update(&d, &r, true).unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(d.join("general.json")).unwrap()).unwrap();
        assert_eq!(back, serde_json::json!({"volume": 3}));
    }

    #[test]
    fn formas_de_apagarlo_que_no_valen() {
        let off = |json: &str| serde_json::from_str::<SelfUpdateOff>(json).unwrap();
        assert!(check_off(&off(r#"{ "file": "{game}/config.toml", "section": "update", "key": "auto", "value": false }"#)).is_ok());
        assert!(check_off(&off(r#"{ "args": ["--no-update"] }"#)).is_ok());
        for bad in [
            r#"{}"#,
            r#"{ "file": "{game}/config.toml", "key": "auto" }"#,
            r#"{ "file": "config.toml", "key": "auto", "value": false }"#,
            r#"{ "file": "{game}", "key": "auto", "value": false }"#,
            r#"{ "file": "{data}/../x.json", "key": "a", "value": false }"#,
            r#"{ "file": "{game}/ajustes.cfg", "key": "a", "value": false }"#,
            r#"{ "file": "{game}/a.ini", "key": "a", "value": [1] }"#,
            r#"{ "args": ["{home}"] }"#,
        ] {
            assert!(check_off(&off(bad)).is_err(), "{bad}");
        }
        // El recibo nunca es la configuración del juego.
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let r = zelda_con_actualizador("{game}/unricopie.json");
        let before = fs::read(d.join(FILE_NAME)).unwrap();
        apply(&d, &r).unwrap();
        assert_eq!(fs::read(d.join(FILE_NAME)).unwrap(), before);
    }
}
