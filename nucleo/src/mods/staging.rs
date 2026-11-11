//! Staging: la carpeta propia de Unricopie donde cada mod queda descomprimido una sola vez.
//! El juego nunca ve esta carpeta; el despliegue enlaza desde aquí.
//!
//! Descomprimir es la parte peligrosa (el archivo viene de internet), así que cada ruta se
//! valida antes de escribir (nada de `..`, rutas absolutas ni enlaces simbólicos), se cuentan
//! los bytes de verdad mientras se escribe (un zip puede mentir sobre su tamaño) y todo se
//! descomprime en una carpeta temporal que solo pasa a su sitio al terminar bien.
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::model::{ModFile, ModId, RelPath};
use crate::archive::write_hashed;
pub use crate::archive::{hash_file, Limits};
use crate::{Error, Result};

pub struct Staging {
    root: PathBuf,
}

impl Staging {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Staging { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Carpeta de una versión de un mod: `<raíz>/<id>/<versión>`, con los caracteres que
    /// Windows no admite cambiados por `_`.
    pub fn mod_dir(&self, id: &ModId, version: &str) -> PathBuf {
        self.root.join(safe_name(id.as_str())).join(safe_name(version))
    }

    /// Descomprime un zip en la carpeta del mod (si ya estaba, la reemplaza) y devuelve sus
    /// archivos con tamaño y hash.
    pub fn import_zip(&self, archive: &Path, id: &ModId, version: &str, limits: Limits) -> Result<Vec<ModFile>> {
        let tmp = self.temp_dir()?;
        let result = crate::archive::extract_zip(archive, &tmp, limits)
            .map(|files| files.into_iter().map(|f| ModFile { path: f.path, size: f.size, hash: f.hash }).collect());
        self.finish(tmp, id, version, result)
    }

    /// Descomprime cualquier formato que sepa abrir el núcleo (zip, 7z, tar.gz, tar.xz).
    pub fn import_archive(&self, archive: &Path, kind: crate::archive::Kind, id: &ModId, version: &str, limits: Limits) -> Result<Vec<ModFile>> {
        let tmp = self.temp_dir()?;
        let result = crate::archive::extract(archive, kind, &tmp, limits)
            .map(|files| files.into_iter().map(|f| ModFile { path: f.path, size: f.size, hash: f.hash }).collect());
        self.finish(tmp, id, version, result)
    }

    /// Guarda tal cual un archivo suelto (un `.nrm`, un `.rtz`): es el mod entero.
    pub fn import_file(&self, file: &Path, id: &ModId, version: &str, limits: Limits) -> Result<Vec<ModFile>> {
        let tmp = self.temp_dir()?;
        let result = (|| {
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let rel = RelPath::new(&name)?;
            let mut src = File::open(file).map_err(|e| Error::io(file, e))?;
            let (size, hash) = write_hashed(&mut src, &rel.under(&tmp), limits.max_total, limits.max_total)?;
            Ok(vec![ModFile { path: rel, size, hash }])
        })();
        self.finish(tmp, id, version, result)
    }

    /// Copia una carpeta ya descomprimida (por ejemplo, un mod que el jugador arrastró).
    pub fn import_dir(&self, dir: &Path, id: &ModId, version: &str, limits: Limits) -> Result<Vec<ModFile>> {
        let tmp = self.temp_dir()?;
        let result = (|| {
            let mut files = Vec::new();
            let mut total = 0u64;
            let mut stack = vec![dir.to_path_buf()];
            while let Some(d) = stack.pop() {
                for entry in fs::read_dir(&d).map_err(|e| Error::io(&d, e))? {
                    let entry = entry.map_err(|e| Error::io(&d, e))?;
                    let path = entry.path();
                    let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
                    if kind.is_symlink() {
                        return Err(Error::UnsafePath(path.display().to_string()));
                    }
                    if kind.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if files.len() >= limits.max_files {
                        return Err(Error::TooManyFiles { limit: limits.max_files });
                    }
                    let rel_str = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().to_string();
                    let rel = RelPath::new(&rel_str)?;
                    let dest = rel.under(&tmp);
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                    }
                    let mut src = File::open(&path).map_err(|e| Error::io(&path, e))?;
                    let (size, hash) = write_hashed(&mut src, &dest, limits.max_total - total, limits.max_total)?;
                    total += size;
                    files.push(ModFile { path: rel, size, hash });
                }
            }
            files.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(files)
        })();
        self.finish(tmp, id, version, result)
    }

    /// Borra la carpeta de una versión de un mod (y la del mod si queda vacía).
    pub fn remove(&self, id: &ModId, version: &str) -> Result<()> {
        let dir = self.mod_dir(id, version);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        if let Some(parent) = dir.parent() {
            let _ = fs::remove_dir(parent); // solo si está vacía
        }
        Ok(())
    }

    fn temp_dir(&self) -> Result<PathBuf> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = self.root.join(format!(".tmp-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        Ok(dir)
    }

    /// Si todo fue bien, mueve la carpeta temporal a su sitio; si no, la borra.
    fn finish(&self, tmp: PathBuf, id: &ModId, version: &str, result: Result<Vec<ModFile>>) -> Result<Vec<ModFile>> {
        match result {
            Ok(files) => {
                let dest = self.mod_dir(id, version);
                if dest.exists() {
                    fs::remove_dir_all(&dest).map_err(|e| Error::io(&dest, e))?;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                }
                fs::rename(&tmp, &dest).map_err(|e| Error::io(&dest, e))?;
                Ok(files)
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&tmp);
                Err(e)
            }
        }
    }
}

fn safe_name(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let out = out.trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() { "_".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn importa_un_zip_con_hashes() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("m.zip");
        make_zip(&z, &[("manifest.json", b"{}"), ("mods/a.nrm", b"hola")]);
        let st = Staging::new(t.path().join("staging"));
        let id = ModId::thunderstore("autor-Mod");
        let files = st.import_zip(&z, &id, "1.0.0", Limits::default()).unwrap();
        assert_eq!(files.len(), 2);
        let a = files.iter().find(|f| f.path.as_str() == "mods/a.nrm").unwrap();
        assert_eq!(a.size, 4);
        assert_eq!(a.hash, xxhash_rust::xxh3::xxh3_64(b"hola"));
        assert!(st.mod_dir(&id, "1.0.0").join("mods").join("a.nrm").is_file());
        // No queda ninguna carpeta temporal.
        let leftovers: Vec<_> = fs::read_dir(st.root()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-")).collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn rechaza_rutas_que_salen_de_la_carpeta() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("malo.zip");
        make_zip(&z, &[("bien.txt", b"x"), ("../fuera.txt", b"x")]);
        let st = Staging::new(t.path().join("staging"));
        let err = st.import_zip(&z, &ModId::local("malo"), "1", Limits::default()).unwrap_err();
        assert!(matches!(err, Error::UnsafePath(_)), "{err}");
        assert!(!t.path().join("fuera.txt").exists());
        assert!(!st.mod_dir(&ModId::local("malo"), "1").exists());
    }

    #[test]
    fn corta_si_supera_el_limite() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("grande.zip");
        make_zip(&z, &[("a.bin", &[0u8; 5000]), ("b.bin", &[0u8; 5000])]);
        let st = Staging::new(t.path().join("staging"));
        let limits = Limits { max_total: 8000, max_files: 10 };
        let err = st.import_zip(&z, &ModId::local("g"), "1", limits).unwrap_err();
        assert!(matches!(err, Error::TooLarge { limit: 8000 }), "{err}");
        let limits = Limits { max_total: 1 << 20, max_files: 1 };
        assert!(matches!(st.import_zip(&z, &ModId::local("g"), "1", limits), Err(Error::TooManyFiles { .. })));
    }

    #[test]
    fn importa_una_carpeta() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("x.o2r"), b"abc").unwrap();
        let st = Staging::new(t.path().join("staging"));
        let files = st.import_dir(&src, &ModId::local("c"), "1", Limits::default()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path.as_str(), "sub/x.o2r");
    }

    #[test]
    fn nombres_seguros() {
        assert_eq!(safe_name("nexus:sonicunleashed:12"), "nexus_sonicunleashed_12");
        assert_eq!(safe_name("v1.0. "), "v1.0");
    }
}
