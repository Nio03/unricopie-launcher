//! Comprobar la copia del jugador antes de dársela al proyecto: es lo que ningún otro launcher
//! hace. Se calcula de una sola pasada cada hash que la receta conoce (una ISO puede pesar
//! varios GB) y se busca cuál de las copias aceptadas es.
//!
//! Si la receta no tiene hashes de esa copia (Sonic Unleashed, por ejemplo), no se puede
//! decir nada y se dice así: `CopyCheck::Unknown`, nunca «comprobada».
//!
//! Una vez comprobada, la copia se guarda una sola vez en la biblioteca, en `copies/<juego>/`
//! (`import`), y `place` se la da al juego: fuera de su carpeta, las actualizaciones no la
//! duplican y sigue ahí aunque se desinstale el juego (decidido el 1-11).
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::Xxh3;

use super::installer::{Library, TmpDir};
use super::receipt;
use super::recipe::{Accepted, AcceptsFrom, Convert, CopyKind, CopyRule, HashAlgo, Recipe};
use crate::net::{get_json, hex};
use crate::path::RelPath;
use crate::{Error, Result};

/// Los hashes calculados de un archivo, en hexadecimal y minúsculas.
pub type Hashes = BTreeMap<HashAlgo, String>;

/// Los hashes de `algos`, calculados todos a la vez a medida que llegan los bytes.
struct Hasher {
    algos: Vec<HashAlgo>,
    sha1: Option<Sha1>,
    sha256: Option<Sha256>,
    md5: Option<Md5>,
    crc: Option<crc32fast::Hasher>,
    xxh: Option<Xxh3>,
}

impl Hasher {
    fn new(algos: &[HashAlgo]) -> Self {
        let want = |a| algos.contains(&a);
        Hasher {
            algos: algos.to_vec(),
            sha1: want(HashAlgo::Sha1).then(Sha1::new),
            sha256: want(HashAlgo::Sha256).then(Sha256::new),
            md5: want(HashAlgo::Md5).then(Md5::new),
            crc: want(HashAlgo::Crc32).then(crc32fast::Hasher::new),
            xxh: (want(HashAlgo::Xxh3_64) || want(HashAlgo::Xxh3_128)).then(Xxh3::new),
        }
    }

    fn update(&mut self, b: &[u8]) {
        if let Some(h) = self.sha1.as_mut() { h.update(b) }
        if let Some(h) = self.sha256.as_mut() { h.update(b) }
        if let Some(h) = self.md5.as_mut() { h.update(b) }
        if let Some(h) = self.crc.as_mut() { h.update(b) }
        if let Some(h) = self.xxh.as_mut() { h.update(b) }
    }

    fn finish(self) -> Hashes {
        let Hasher { algos, sha1, sha256, md5, crc, xxh } = self;
        let mut out = Hashes::new();
        if let Some(h) = sha1 { out.insert(HashAlgo::Sha1, hex(&h.finalize())); }
        if let Some(h) = sha256 { out.insert(HashAlgo::Sha256, hex(&h.finalize())); }
        if let Some(h) = md5 { out.insert(HashAlgo::Md5, hex(&h.finalize())); }
        if let Some(h) = crc { out.insert(HashAlgo::Crc32, format!("{:08x}", h.finalize())); }
        if let Some(h) = xxh {
            if algos.contains(&HashAlgo::Xxh3_64) { out.insert(HashAlgo::Xxh3_64, format!("{:016x}", h.digest())); }
            if algos.contains(&HashAlgo::Xxh3_128) { out.insert(HashAlgo::Xxh3_128, format!("{:032x}", h.digest128())); }
        }
        out
    }
}

/// Calcula de una pasada los hashes de `algos`, leyendo el archivo donde está. `progress` recibe
/// los bytes leídos.
pub fn hash_copy(path: &Path, algos: &[HashAlgo], progress: &mut dyn FnMut(u64)) -> Result<Hashes> {
    let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut h = Hasher::new(algos);
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(h.finish())
}

/// Lo mismo que `hash_copy` con los bytes ya en memoria (una ROM convertida).
pub fn hash_bytes(data: &[u8], algos: &[HashAlgo]) -> Hashes {
    let mut h = Hasher::new(algos);
    h.update(data);
    h.finish()
}

/// Qué es la copia que eligió el jugador.
#[derive(Debug, Clone, PartialEq)]
pub enum CopyCheck<'a> {
    /// Coincide con una copia aceptada (todos los hashes que se pudieron comparar).
    Matches(&'a Accepted),
    /// Ninguna copia aceptada tiene esos hashes: no es la que pide el proyecto, o está
    /// modificada, o es otra región o revisión.
    NoMatch,
    /// La receta no tiene hashes con los que comparar.
    Unknown,
}

/// Los algoritmos que hacen falta para comparar con `accepts`.
pub fn algos_for(accepts: &[Accepted]) -> Vec<HashAlgo> {
    let mut v: Vec<HashAlgo> = accepts.iter().flat_map(|a| a.hash.keys().copied()).collect();
    v.sort();
    v.dedup();
    v
}

/// Busca cuál de `accepts` es la copia con estos hashes.
pub fn identify<'a>(accepts: &'a [Accepted], hashes: &Hashes) -> CopyCheck<'a> {
    let mut comparable = false;
    for a in accepts {
        let pairs: Vec<(&String, &String)> = a.hash.iter().filter_map(|(k, want)| hashes.get(k).map(|got| (want, got))).collect();
        if pairs.is_empty() {
            continue;
        }
        comparable = true;
        if pairs.iter().all(|(want, got)| want.eq_ignore_ascii_case(got)) {
            return CopyCheck::Matches(a);
        }
    }
    if comparable { CopyCheck::NoMatch } else { CopyCheck::Unknown }
}

#[derive(Deserialize)]
struct Listed {
    name: String,
    #[serde(flatten)]
    rest: BTreeMap<String, serde_json::Value>,
}

/// Lee la lista de copias que publica el propio proyecto (`copy.accepts-from`). El formato que
/// hay hoy (SoH, `docs/supportedHashes.json`) es una lista de objetos con `name` y uno o más
/// hashes con el nombre del algoritmo (`sha1`); los campos que no son hashes se ignoran.
pub fn fetch_accepts(agent: &ureq::Agent, from: &AcceptsFrom) -> Result<Vec<Accepted>> {
    if !from.url.starts_with("https://") {
        return Err(Error::BadRecipe(format!("accepts-from tiene que ser https: {}", from.url)));
    }
    let listed: Vec<Listed> = get_json(agent, &from.url)?;
    Ok(parse_listed(listed))
}

fn parse_listed(listed: Vec<Listed>) -> Vec<Accepted> {
    listed
        .into_iter()
        .filter_map(|l| {
            let hash: BTreeMap<HashAlgo, String> = l
                .rest
                .iter()
                .filter_map(|(k, v)| {
                    let algo: HashAlgo = serde_json::from_value(serde_json::Value::String(k.clone())).ok()?;
                    Some((algo, v.as_str()?.to_ascii_lowercase()))
                })
                .collect();
            (!hash.is_empty()).then(|| Accepted {
                name: l.name,
                what: Default::default(),
                platform: None,
                region: None,
                id: None,
                hash,
                required: true,
                note: None,
                file: None,
            })
        })
        .collect()
}

// --- Guardar la copia en la biblioteca ---

/// Lo que se sabe de una copia guardada, en `copies/<juego>/`.
pub const COPY_MANIFEST: &str = "unricopie-copia.json";
/// Dentro de `copies/<juego>/`, la carpeta de una copia que es una carpeta.
const FOLDER: &str = "copia";
/// Una ROM convertida se lee entera: más que esto no es una ROM.
const MAX_ROM: u64 = 512 << 20;

/// Una copia guardada en la biblioteca.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Imported {
    /// El archivo o la carpeta de la copia. No se guarda: sale de dónde está.
    #[serde(skip)]
    pub path: PathBuf,
    /// El nombre del archivo; `None` si la copia es una carpeta.
    pub file: Option<String>,
    /// Las copias aceptadas con las que coincide (su `name`).
    pub accepted: Vec<String>,
    /// Se comprobó con los hashes de la receta. `false` si la receta no los tiene, o si la
    /// persona la guardó aunque no coincidía.
    pub verified: bool,
    /// De dónde la sacó la persona.
    pub source: PathBuf,
    pub imported_at: u64,
}

/// Comprueba la copia que eligió la persona (`source`, un archivo o una carpeta) y la guarda en
/// `copies/<juego>/`, convertida si la receta lo pide. Si no es ninguna de las que acepta la
/// receta, no se guarda, salvo con `allow_unverified`. Si ya había una, la sustituye: la de
/// antes solo se borra cuando la nueva está en su sitio.
pub fn import(
    library: &Library,
    recipe: &Recipe,
    variant: Option<&str>,
    source: &Path,
    allow_unverified: bool,
    progress: &mut dyn FnMut(u64),
) -> Result<Imported> {
    import_with(library, recipe, variant, source, &[], allow_unverified, progress)
}

/// `import`, comprobando también contra `extra`: las copias de `copy.accepts-from` que ya se
/// leyeron con `fetch_accepts` (Ship of Harkinian no trae hashes en la receta, sino una lista
/// que publica el proyecto). Se suman a las de la receta.
pub fn import_with(
    library: &Library,
    recipe: &Recipe,
    variant: Option<&str>,
    source: &Path,
    extra: &[Accepted],
    allow_unverified: bool,
    progress: &mut dyn FnMut(u64),
) -> Result<Imported> {
    let rule = recipe.copy_rule(variant);
    let accepts: Vec<Accepted> = rule.accepts.iter().chain(extra).cloned().collect();
    let is_dir = source.is_dir();
    match (rule.kind, is_dir) {
        (Some(CopyKind::File), true) => return Err(Error::CopyWantsFile(recipe.entry.clone())),
        (Some(CopyKind::Folder), false) => return Err(Error::CopyWantsFolder(recipe.entry.clone())),
        _ => {}
    }
    // Primero se comprueba la copia donde está, sin escribir nada: una ISO de varios GB que no es
    // la que pide el juego no llega a copiarse. El 3-11, copiar antes de comparar llenó de
    // escrituras un disco USB hasta tumbarlo.
    let Checked { accepted, verified, file, converted } = if is_dir {
        check_folder(recipe, &accepts, source, allow_unverified)?
    } else {
        check_file(recipe, rule, &accepts, source, allow_unverified, progress)?
    };
    fs::create_dir_all(&library.copies).map_err(|e| Error::io(&library.copies, e))?;
    let tmp = TmpDir::new(&library.copies)?;
    match (&file, converted) {
        (None, _) => super::data::copy_tree(source, &tmp.path().join(FOLDER))?,
        (Some(name), Some(data)) => {
            let to = tmp.path().join(name);
            fs::write(&to, data).map_err(|e| Error::io(&to, e))?;
        }
        (Some(name), None) => {
            let to = tmp.path().join(name);
            fs::copy(source, &to).map_err(|e| Error::io(&to, e))?;
        }
    }
    let mut imported = Imported { path: PathBuf::new(), file, accepted, verified, source: source.to_path_buf(), imported_at: receipt::now() };
    let manifest = tmp.path().join(COPY_MANIFEST);
    fs::write(&manifest, serde_json::to_vec_pretty(&imported)?).map_err(|e| Error::io(&manifest, e))?;
    let dest = library.copy_dir(recipe, variant);
    let mut old = dest.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    if dest.exists() {
        let _ = fs::remove_dir_all(&old);
        fs::rename(&dest, &old).map_err(|e| Error::io(&dest, e))?;
    }
    if let Err(e) = fs::rename(tmp.path(), &dest) {
        if old.exists() {
            let _ = fs::rename(&old, &dest);
        }
        return Err(Error::io(&dest, e));
    }
    let _ = fs::remove_dir_all(&old);
    imported.path = located(&dest, &imported);
    Ok(imported)
}

/// La copia guardada para el juego de `recipe` (y `variant`), si la hay.
pub fn imported(library: &Library, recipe: &Recipe, variant: Option<&str>) -> Option<Imported> {
    let dir = library.copy_dir(recipe, variant);
    let mut imp: Imported = serde_json::from_slice(&fs::read(dir.join(COPY_MANIFEST)).ok()?).ok()?;
    imp.path = located(&dir, &imp);
    imp.path.exists().then_some(imp)
}

fn located(dir: &Path, imp: &Imported) -> PathBuf {
    match &imp.file {
        Some(f) => dir.join(f),
        None => dir.join(FOLDER),
    }
}

/// Lo que se decidió de una copia antes de guardarla.
struct Checked {
    /// Las copias aceptadas con las que coincide.
    accepted: Vec<String>,
    verified: bool,
    /// El nombre con el que se guarda el archivo; `None` si la copia es una carpeta.
    file: Option<String>,
    /// La ROM ya convertida, si la receta pide convertirla.
    converted: Option<Vec<u8>>,
}

/// Comprueba un archivo leyéndolo donde está. Si hay que convertirlo (una ROM de N64, que es
/// pequeña), se convierte en memoria y se comprueba ya convertido.
fn check_file(
    recipe: &Recipe,
    rule: &CopyRule,
    accepts: &[Accepted],
    source: &Path,
    allow: bool,
    progress: &mut dyn FnMut(u64),
) -> Result<Checked> {
    let raw = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| Error::UnsafePath(source.display().to_string()))?;
    let mut name = RelPath::new(raw)?.file_name().to_string();
    // Solo las copias aceptadas que son el archivo entero (las de una carpeta llevan `file`).
    let accepts: Vec<Accepted> = accepts.iter().filter(|a| a.file.is_none()).cloned().collect();
    let algos = algos_for(&accepts);
    let (hashes, converted) = match rule.convert {
        Some(Convert::N64Z64) => {
            let size = fs::metadata(source).map_err(|e| Error::io(source, e))?.len();
            if size > MAX_ROM {
                return Err(Error::NotARom(source.to_path_buf()));
            }
            let data = fs::read(source).map_err(|e| Error::io(source, e))?;
            let z64 = n64_to_z64(data).ok_or_else(|| Error::NotARom(source.to_path_buf()))?;
            let stem = Path::new(&name).file_stem().and_then(|s| s.to_str()).unwrap_or("rom").to_string();
            name = format!("{stem}.z64");
            progress(z64.len() as u64);
            (hash_bytes(&z64, &algos), Some(z64))
        }
        // Sin hashes con los que comparar no hace falta leerla.
        None if algos.is_empty() => (Hashes::new(), None),
        None => (hash_copy(source, &algos, progress)?, None),
    };
    let (accepted, verified) = match identify(&accepts, &hashes) {
        CopyCheck::Matches(a) => (vec![a.name.clone()], true),
        CopyCheck::Unknown => (Vec::new(), false),
        CopyCheck::NoMatch if allow => (Vec::new(), false),
        CopyCheck::NoMatch => return Err(Error::CopyMismatch(recipe.entry.clone())),
    };
    Ok(Checked { accepted, verified, file: Some(name), converted })
}

/// Comprueba una carpeta leyendo sus archivos donde están.
fn check_folder(recipe: &Recipe, accepts: &[Accepted], source: &Path, allow: bool) -> Result<Checked> {
    let (mut accepted, mut compared, mut all_ok) = (Vec::new(), false, true);
    for a in accepts {
        let Some(f) = &a.file else { continue };
        let Some(path) = find_ignoring_case(source, &RelPath::new(f)?) else {
            if a.required {
                return Err(Error::CopyIncomplete(f.clone()));
            }
            continue;
        };
        if a.hash.is_empty() {
            continue;
        }
        let algos: Vec<HashAlgo> = a.hash.keys().copied().collect();
        let got = hash_copy(&path, &algos, &mut |_| {})?;
        compared = true;
        if a.hash.iter().all(|(k, want)| got.get(k).is_some_and(|g| g.eq_ignore_ascii_case(want))) {
            accepted.push(a.name.clone());
        } else if a.required {
            all_ok = false;
        }
    }
    if !all_ok && !allow {
        return Err(Error::CopyMismatch(recipe.entry.clone()));
    }
    Ok(Checked { accepted, verified: compared && all_ok, file: None, converted: None })
}

/// `rel` dentro de `root` sin distinguir mayúsculas: las copias de juegos de DOS y Windows
/// traen `DATA/MASTER.DAT` o `data/master.dat` según de dónde salgan.
pub(crate) fn find_ignoring_case(root: &Path, rel: &RelPath) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for part in rel.as_str().split('/') {
        let exact = at.join(part);
        at = if exact.exists() {
            exact
        } else {
            fs::read_dir(&at).ok()?.flatten().find(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(part)))?.path()
        };
    }
    Some(at)
}

/// Una ROM de N64 en el orden .z64, como `select_rom` de N64ModernRuntime: se rellena con ceros
/// hasta un múltiplo de 4 y, según sus cuatro primeros bytes (`80 37 12 40` en .z64), se dan
/// la vuelta de 4 en 4 (.n64) o de 2 en 2 (.v64). `None` si no empieza como una ROM.
pub fn n64_to_z64(mut data: Vec<u8>) -> Option<Vec<u8>> {
    data.resize((data.len() + 3) & !3, 0);
    let swap = match data.get(..4)? {
        [0x80, 0x37, 0x12, 0x40] => return Some(data),
        [0x40, 0x12, 0x37, 0x80] => 3,
        [0x37, 0x80, 0x40, 0x12] => 1,
        _ => return None,
    };
    for word in data.chunks_exact_mut(4) {
        let w = [word[0], word[1], word[2], word[3]];
        for (i, b) in w.into_iter().enumerate() {
            word[i ^ swap] = b;
        }
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calcula_los_hashes_de_siempre() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("abc.bin");
        std::fs::write(&p, b"abc").unwrap();
        let all = [HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Md5, HashAlgo::Crc32, HashAlgo::Xxh3_64, HashAlgo::Xxh3_128];
        let mut seen = 0;
        let h = hash_copy(&p, &all, &mut |n| seen = n).unwrap();
        assert_eq!(seen, 3);
        // Vectores de prueba publicados de cada algoritmo para «abc».
        assert_eq!(h[&HashAlgo::Sha1], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h[&HashAlgo::Sha256], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(h[&HashAlgo::Md5], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h[&HashAlgo::Crc32], "352441c2");
        assert_eq!(h[&HashAlgo::Xxh3_64], format!("{:016x}", xxhash_rust::xxh3::xxh3_64(b"abc")));
        assert_eq!(h[&HashAlgo::Xxh3_128], format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"abc")));
        // Solo lo que se pide.
        let solo = hash_copy(&p, &[HashAlgo::Crc32], &mut |_| {}).unwrap();
        assert_eq!(solo.len(), 1);
        // Con los bytes en memoria sale lo mismo.
        assert_eq!(hash_bytes(b"abc", &all), h);
    }

    fn accepted(name: &str, hash: &[(HashAlgo, &str)]) -> Accepted {
        Accepted {
            name: name.into(),
            what: Default::default(),
            platform: None,
            region: None,
            id: None,
            hash: hash.iter().map(|(k, v)| (*k, v.to_string())).collect(),
            required: true,
            note: None,
            file: None,
        }
    }

    #[test]
    fn identifica_la_copia() {
        let accepts = vec![
            accepted("EE. UU.", &[(HashAlgo::Xxh3_64, "ef18b4a9e2386169")]),
            accepted("Europa", &[(HashAlgo::Xxh3_64, "0123456789abcdef"), (HashAlgo::Sha1, "aa")]),
        ];
        assert_eq!(algos_for(&accepts), vec![HashAlgo::Sha1, HashAlgo::Xxh3_64]);
        let us: Hashes = [(HashAlgo::Xxh3_64, "EF18B4A9E2386169".to_string())].into();
        assert!(matches!(identify(&accepts, &us), CopyCheck::Matches(a) if a.name == "EE. UU."));
        // Coincide un hash pero no el otro: no es esa copia.
        let mixta: Hashes = [(HashAlgo::Xxh3_64, "0123456789abcdef".to_string()), (HashAlgo::Sha1, "bb".to_string())].into();
        assert_eq!(identify(&accepts, &mixta), CopyCheck::NoMatch);
        // Sin hashes en la receta no se puede decir nada.
        let sin = vec![accepted("Sonic Unleashed (Xbox 360)", &[])];
        assert_eq!(identify(&sin, &us), CopyCheck::Unknown);
    }

    #[test]
    fn lee_la_lista_de_soh() {
        // Recorte de docs/supportedHashes.json de Shipwright (1-11).
        let json = r#"[{"name": "PAL 1.0", "sha1": "328a1f1beba30ce5e178f031662019eb32c5f3b5"},
                       {"name": "PAL GC", "sha1": "0227D7C0074F2D0AC935631990DA8EC5914597B4", "nota": 3},
                       {"name": "sin hash"}]"#;
        let listed: Vec<Listed> = serde_json::from_str(json).unwrap();
        let acc = parse_listed(listed);
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[1].hash[&HashAlgo::Sha1], "0227d7c0074f2d0ac935631990da8ec5914597b4");
    }
}
