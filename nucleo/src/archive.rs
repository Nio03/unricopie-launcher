//! Descomprimir sin riesgos. Lo usan los mods (staging) y la instalación de juegos.
//!
//! El archivo viene de internet, así que cada ruta se valida antes de escribir (nada de `..`,
//! rutas absolutas ni enlaces), se cuentan los bytes de verdad mientras se escribe (un zip
//! puede mentir sobre su tamaño) y se escribe solo dentro de `dest`, que el que llama crea
//! vacía y mueve a su sitio al terminar bien.
//!
//! Formatos: zip, tar.gz, tar.xz y 7z (LZMA y LZMA2 con sus filtros, sin cifrado). Los enlaces
//! simbólicos se rechazan, también los de los tar de Linux y los que guarda 7-Zip de Unix; si una
//! receta los necesita, habrá que admitirlos comprobando que apuntan dentro.
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::Path;

use xxhash_rust::xxh3::Xxh3;

use crate::path::RelPath;
use crate::{Error, Result};

/// Límites al descomprimir.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Bytes descomprimidos en total.
    pub max_total: u64,
    /// Número de archivos.
    pub max_files: usize,
}

impl Default for Limits {
    /// 16 GB y 200.000 archivos: cabe el pack de texturas más grande que conocemos
    /// (OoT Reloaded 4K, 3,7 GB) con margen, pero no una bomba de zip.
    fn default() -> Self {
        Limits { max_total: 16 << 30, max_files: 200_000 }
    }
}

/// Un archivo escrito, con su tamaño y su XXH3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub path: RelPath,
    pub size: u64,
    pub hash: u64,
    /// Tenía permiso de ejecución en el archivo (solo lo dicen los zip y tar hechos en Unix).
    pub executable: bool,
}

/// Formato de un archivo comprimido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    TarGz,
    TarXz,
    SevenZ,
}

impl Kind {
    /// Por la extensión del nombre. `None` si no es un formato que sepamos abrir.
    pub fn from_name(name: &str) -> Option<Kind> {
        let n = name.to_ascii_lowercase();
        if n.ends_with(".zip") {
            Some(Kind::Zip)
        } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
            Some(Kind::TarGz)
        } else if n.ends_with(".tar.xz") || n.ends_with(".txz") {
            Some(Kind::TarXz)
        } else if n.ends_with(".7z") {
            Some(Kind::SevenZ)
        } else {
            None
        }
    }
}

/// Descomprime `archive` dentro de `dest` (que ya existe).
pub fn extract(archive: &Path, kind: Kind, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    match kind {
        Kind::Zip => extract_zip(archive, dest, limits),
        Kind::TarGz => extract_tar_gz(archive, dest, limits),
        Kind::TarXz => {
            let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
            // 1 GB de diccionario como mucho: el `xz -9` más grande usa 64 MB.
            let xz = lzma_rust2::XzReader::new_mem_limit(BufReader::new(file), true, 1 << 20);
            extract_tar(archive, xz, dest, limits)
        }
        Kind::SevenZ => extract_7z(archive, dest, limits),
    }
}

pub fn extract_zip(archive: &Path, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    let bad = |reason: String| Error::Archive { path: archive.to_path_buf(), reason };
    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| bad(e.to_string()))?;
    if zip.len() > limits.max_files {
        return Err(Error::TooManyFiles { limit: limits.max_files });
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| bad(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let mode = entry.unix_mode();
        // Enlaces simbólicos: podrían apuntar fuera de la carpeta. No se aceptan.
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) || entry.enclosed_name().is_none() {
            return Err(Error::UnsafePath(entry.name().to_string()));
        }
        let rel = RelPath::new(entry.name())?;
        let executable = mode.is_some_and(|m| m & 0o111 != 0);
        let (size, hash) = write_entry(&mut entry, &rel, dest, executable, limits.max_total - total, limits.max_total)?;
        total += size;
        out.push(Extracted { path: rel, size, hash, executable });
    }
    Ok(out)
}

pub fn extract_tar_gz(archive: &Path, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    extract_tar(archive, flate2::read::GzDecoder::new(BufReader::new(file)), dest, limits)
}

/// Un tar ya descomprimido por `reader` (gzip o xz). `archive` es solo para los errores.
fn extract_tar(archive: &Path, mut reader: impl Read, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    let bad = |reason: String| Error::Archive { path: archive.to_path_buf(), reason };
    let out = extract_tar_entries(archive, &mut reader, dest, limits)?;
    // El tar acaba antes que el archivo: se lee el resto para que el descompresor compruebe su
    // suma (el CRC de gzip, el CRC64 de xz) y que no está cortado.
    std::io::copy(&mut reader, &mut std::io::sink()).map_err(|e| bad(e.to_string()))?;
    Ok(out)
}

fn extract_tar_entries(archive: &Path, reader: impl Read, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    let bad = |reason: String| Error::Archive { path: archive.to_path_buf(), reason };
    let mut tar = tar::Archive::new(reader);
    let mut out = Vec::new();
    let mut total = 0u64;
    for entry in tar.entries().map_err(|e| bad(e.to_string()))? {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let kind = entry.header().entry_type();
        // Lee la ruta en bruto: `path()` de la biblioteca ya rechaza algunos casos, pero aquí
        // la validación es siempre la misma que para un zip.
        let raw = String::from_utf8_lossy(&entry.path_bytes()).to_string();
        if kind.is_dir() {
            continue;
        }
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() || kind.is_gnu_longname() {
            continue; // metadatos; la biblioteca ya los aplica a la entrada siguiente
        }
        if !kind.is_file() {
            return Err(Error::UnsafePath(raw));
        }
        if out.len() >= limits.max_files {
            return Err(Error::TooManyFiles { limit: limits.max_files });
        }
        let rel = RelPath::new(&raw)?;
        let executable = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
        let (size, hash) = write_entry(&mut entry, &rel, dest, executable, limits.max_total - total, limits.max_total)?;
        total += size;
        out.push(Extracted { path: rel, size, hash, executable });
    }
    Ok(out)
}

/// Atributos de Windows que guarda 7-Zip: si lleva `UNIX_EXTENSION`, los 16 bits de arriba son
/// el modo de Unix (con el tipo de archivo, como en un tar).
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;

pub fn extract_7z(archive: &Path, dest: &Path, limits: Limits) -> Result<Vec<Extracted>> {
    let bad = |reason: String| Error::Archive { path: archive.to_path_buf(), reason };
    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    let mut reader = sevenz_rust2::ArchiveReader::new(BufReader::new(file), sevenz_rust2::Password::empty())
        .map_err(|e| bad(e.to_string()))?;
    if reader.archive().files.len() > limits.max_files {
        return Err(Error::TooManyFiles { limit: limits.max_files });
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    // Los errores nuestros salen del cierre por aquí: el lector solo sabe de los suyos.
    let mut failed: Option<Error> = None;
    let result = reader.for_each_entries(|entry, data| {
        if failed.is_some() {
            return Ok(false);
        }
        let step = (|| -> Result<()> {
            if entry.is_directory() {
                return Ok(());
            }
            let attr = entry.windows_attributes();
            let unix = (attr & FILE_ATTRIBUTE_UNIX_EXTENSION != 0).then_some(attr >> 16);
            let link = unix.is_some_and(|m| m & 0o170000 == 0o120000) || attr & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            if link || entry.is_anti_item() {
                return Err(Error::UnsafePath(entry.name().to_string()));
            }
            let rel = RelPath::new(entry.name())?;
            let executable = unix.is_some_and(|m| m & 0o111 != 0);
            let (size, hash) = write_entry(data, &rel, dest, executable, limits.max_total - total, limits.max_total)?;
            total += size;
            out.push(Extracted { path: rel, size, hash, executable });
            Ok(())
        })();
        match step {
            Ok(()) => Ok(true),
            Err(e) => {
                failed = Some(e);
                Ok(false)
            }
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    result.map_err(|e| bad(e.to_string()))?;
    Ok(out)
}

fn write_entry(src: &mut (impl Read + ?Sized), rel: &RelPath, dest: &Path, executable: bool, budget: u64, limit: u64) -> Result<(u64, u64)> {
    let target = rel.under(dest);
    if target.exists() {
        // Dos entradas con la misma ruta (o la misma salvo mayúsculas en Windows).
        return Err(Error::UnsafePath(format!("{rel} aparece dos veces")));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let r = write_hashed(src, &target, budget, limit)?;
    if executable {
        set_executable(&target)?;
    }
    Ok(r)
}

/// Copia `src` a `dest` calculando el XXH3 por el camino y cortando si pasa de `budget`.
pub(crate) fn write_hashed(src: &mut (impl Read + ?Sized), dest: &Path, budget: u64, limit: u64) -> Result<(u64, u64)> {
    let mut out = File::create(dest).map_err(|e| Error::io(dest, e))?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = src.read(&mut buf).map_err(|e| Error::io(dest, e))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > budget {
            return Err(Error::TooLarge { limit });
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| Error::io(dest, e))?;
    }
    out.flush().map_err(|e| Error::io(dest, e))?;
    Ok((size, hasher.digest()))
}

/// Tamaño y XXH3 de un archivo en disco.
pub fn hash_file(path: &Path) -> Result<(u64, u64)> {
    let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, hasher.digest()))
}

/// Da permiso de ejecución (en Unix; en Windows no hace falta).
pub fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(path).map_err(|e| Error::io(path, e))?.permissions();
        perm.set_mode(perm.mode() | 0o755);
        fs::set_permissions(path, perm).map_err(|e| Error::io(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn make_tar_gz(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        fill_tar(gz, entries).finish().unwrap();
    }

    fn make_tar_xz(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let xz = lzma_rust2::XzWriter::new(File::create(path).unwrap(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        fill_tar(xz, entries).finish().unwrap();
    }

    fn fill_tar<W: Write>(w: W, entries: &[(&str, &[u8], u32)]) -> W {
        let mut b = tar::Builder::new(w);
        for (name, data, mode) in entries {
            let mut h = tar::Header::new_gnu();
            // Nombre escrito a mano para poder probar rutas que `set_path` no dejaría poner.
            let n = name.as_bytes();
            h.as_old_mut().name[..n.len()].copy_from_slice(n);
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    /// Un 7z con `entries` (ruta, datos, modo de Unix si lo lleva). Con `bcj`, con el filtro
    /// de ejecutables x86 delante de LZMA2, como hace 7-Zip con los .exe.
    fn make_7z(path: &Path, entries: &[(&str, &[u8], Option<u32>)], bcj: bool) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod};
        let mut w = ArchiveWriter::create(path).unwrap();
        if bcj {
            w.set_content_methods(vec![EncoderMethod::BCJ_X86_FILTER.into(), EncoderMethod::LZMA2.into()]);
        }
        for (name, data, unix) in entries {
            let mut e = ArchiveEntry::new_file(name);
            if let Some(m) = unix {
                e.has_windows_attributes = true;
                e.windows_attributes = FILE_ATTRIBUTE_UNIX_EXTENSION | (m << 16);
            }
            w.push_archive_entry(e, Some(*data)).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn descomprime_un_7z() {
        let exe: Vec<u8> = (0..200_000u32).flat_map(|i| [0xE8, (i % 251) as u8, 0, 0]).collect();
        for bcj in [false, true] {
            let t = tempfile::tempdir().unwrap();
            let a = t.path().join("RBDOOM-3-BFG.7z");
            make_7z(&a, &[
                ("juego/RBDOOM-3-BFG.exe", &exe, None),
                ("juego/base/default.cfg", b"seta r_fullscreen 1", None),
                ("juego/vacio.txt", b"", None),
                ("juego/bin/juego", b"elf", Some(0o100755)),
            ], bcj);
            let dest = t.path().join("dest");
            fs::create_dir(&dest).unwrap();
            let mut files = extract(&a, Kind::SevenZ, &dest, Limits::default()).unwrap();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(names, ["juego/RBDOOM-3-BFG.exe", "juego/base/default.cfg", "juego/bin/juego", "juego/vacio.txt"], "bcj={bcj}");
            assert_eq!(fs::read(dest.join("juego").join("RBDOOM-3-BFG.exe")).unwrap(), exe, "bcj={bcj}");
            assert_eq!(files[0].hash, xxhash_rust::xxh3::xxh3_64(&exe));
            assert_eq!(fs::read(dest.join("juego").join("vacio.txt")).unwrap(), b"");
            assert!(files[2].executable && !files[0].executable);
        }
    }

    #[test]
    fn rechaza_enlaces_y_rutas_peligrosas_en_7z() {
        for (name, unix) in [("enlace", Some(0o120777)), ("../fuera.txt", None), ("a/../../fuera.txt", None)] {
            let t = tempfile::tempdir().unwrap();
            let a = t.path().join("malo.7z");
            make_7z(&a, &[("bien.txt", b"x", None), (name, b"/etc/passwd", unix)], false);
            let dest = t.path().join("dest");
            fs::create_dir(&dest).unwrap();
            let err = extract(&a, Kind::SevenZ, &dest, Limits::default()).unwrap_err();
            assert!(matches!(err, Error::UnsafePath(_)), "{name}: {err}");
            assert!(!t.path().join("fuera.txt").exists());
            assert!(!dest.join("enlace").exists());
        }
    }

    #[test]
    fn un_7z_no_pasa_del_limite() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("bomba.7z");
        let ceros = vec![0u8; 1 << 20];
        make_7z(&a, &[("a.bin", &ceros, None), ("b.bin", &ceros, None)], false);
        let dest = t.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let err = extract(&a, Kind::SevenZ, &dest, Limits { max_total: 1_500_000, max_files: 10 }).unwrap_err();
        assert!(matches!(err, Error::TooLarge { .. }), "{err}");
        let err = extract(&a, Kind::SevenZ, &t.path().join("otra"), Limits { max_total: 1 << 30, max_files: 1 }).unwrap_err();
        assert!(matches!(err, Error::TooManyFiles { .. }), "{err}");
    }

    #[test]
    fn descomprime_un_tar_xz_y_rechaza_lo_peligroso() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("devilutionx-linux-x86_64.tar.xz");
        make_tar_xz(&a, &[("devilutionx/devilutionx", b"elf", 0o755), ("devilutionx/README.txt", b"hola", 0o644)]);
        let dest = t.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let files = extract(&a, Kind::from_name("x.tar.xz").unwrap(), &dest, Limits::default()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().find(|f| f.path.as_str() == "devilutionx/devilutionx").unwrap().executable);
        assert_eq!(fs::read(dest.join("devilutionx").join("README.txt")).unwrap(), b"hola");

        let malo = t.path().join("malo.tar.xz");
        make_tar_xz(&malo, &[("../fuera.txt", b"x", 0o644)]);
        let dest2 = t.path().join("dest2");
        fs::create_dir(&dest2).unwrap();
        assert!(matches!(extract(&malo, Kind::TarXz, &dest2, Limits::default()), Err(Error::UnsafePath(_))));
        assert!(!t.path().join("fuera.txt").exists());
        // Un xz cortado (sin su final, donde va la suma) no pasa aunque el tar esté entero.
        let roto = t.path().join("roto.tar.xz");
        let mut bytes = fs::read(&a).unwrap();
        let n = bytes.len();
        bytes.truncate(n - 20);
        fs::write(&roto, bytes).unwrap();
        let dest3 = t.path().join("dest3");
        fs::create_dir(&dest3).unwrap();
        assert!(extract(&roto, Kind::TarXz, &dest3, Limits::default()).is_err());
    }

    #[test]
    fn reconoce_el_formato_por_el_nombre() {
        assert_eq!(Kind::from_name("Zelda64Recompiled-v1.2.2-Windows.zip"), Some(Kind::Zip));
        assert_eq!(Kind::from_name("openloco-linux.TAR.GZ"), Some(Kind::TarGz));
        assert_eq!(Kind::from_name("app.tgz"), Some(Kind::TarGz));
        assert_eq!(Kind::from_name("SoH.appimage"), None);
        assert_eq!(Kind::from_name("RBDOOM-3-BFG-1.6.0-full-win64-20250101.7z"), Some(Kind::SevenZ));
        assert_eq!(Kind::from_name("devilutionx-linux-x86_64.tar.xz"), Some(Kind::TarXz));
        assert_eq!(Kind::from_name("juego.rar"), None);
    }

    #[test]
    fn descomprime_un_tar_gz_con_permisos() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a.tar.gz");
        make_tar_gz(&a, &[("juego/bin/juego", b"elf", 0o755), ("juego/datos.txt", b"hola", 0o644)]);
        let dest = t.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let files = extract(&a, Kind::TarGz, &dest, Limits::default()).unwrap();
        assert_eq!(files.len(), 2);
        let bin = files.iter().find(|f| f.path.as_str() == "juego/bin/juego").unwrap();
        assert!(bin.executable);
        assert!(!files.iter().find(|f| f.path.as_str() == "juego/datos.txt").unwrap().executable);
        assert_eq!(fs::read(dest.join("juego").join("datos.txt")).unwrap(), b"hola");
        // Sin los 8 bytes del final (el CRC y el tamaño de gzip) no pasa, aunque el tar esté entero.
        let mut bytes = fs::read(&a).unwrap();
        bytes.truncate(bytes.len() - 8);
        let cortado = t.path().join("cortado.tar.gz");
        fs::write(&cortado, bytes).unwrap();
        let d2 = t.path().join("d2");
        fs::create_dir(&d2).unwrap();
        assert!(extract(&cortado, Kind::TarGz, &d2, Limits::default()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(dest.join("juego/bin/juego")).unwrap().permissions().mode();
            assert!(m & 0o111 != 0);
        }
    }

    #[test]
    fn rechaza_rutas_peligrosas_en_tar() {
        for bad in ["../fuera.txt", "/etc/fuera.txt", "a/../../fuera.txt"] {
            let t = tempfile::tempdir().unwrap();
            let a = t.path().join("malo.tar.gz");
            make_tar_gz(&a, &[("bien.txt", b"x", 0o644), (bad, b"x", 0o644)]);
            let dest = t.path().join("dest");
            fs::create_dir(&dest).unwrap();
            let err = extract(&a, Kind::TarGz, &dest, Limits::default()).unwrap_err();
            assert!(matches!(err, Error::UnsafePath(_)), "{bad}: {err}");
            assert!(!t.path().join("fuera.txt").exists());
        }
    }

    #[test]
    fn rechaza_rutas_repetidas() {
        // La misma ruta dos veces (aquí con `/` y con `\`), que algunos zip traen de verdad:
        // la segunda no puede pisar a la primera en silencio.
        let t = tempfile::tempdir().unwrap();
        let dest = t.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let a2 = t.path().join("doble.zip");
        let mut w = zip::ZipWriter::new(File::create(&a2).unwrap());
        w.start_file("datos/a.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"1").unwrap();
        w.start_file("datos\\a.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"2").unwrap();
        w.finish().unwrap();
        let err = extract(&a2, Kind::Zip, &dest, Limits::default()).unwrap_err();
        assert!(matches!(err, Error::UnsafePath(_)), "{err}");
    }
}
