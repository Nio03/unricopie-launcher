//! ¿Cabe? Antes de bajar un juego se mira el espacio libre del disco de la biblioteca, para
//! fallar al principio y no a medio descomprimir.
//!
//! Hace falta sitio para dos cosas a la vez: lo bajado (en `downloads`, lo que pesa el archivo)
//! y lo instalado (en `games`). Lo que ocupa lo instalado no se sabe antes de bajar, así que se
//! estima: un comprimido, tres veces lo que pesa (los zip y 7z de juegos suelen quedar entre una
//! y tres veces; con tres casi nunca se queda corto, y si se queda, la instalación falla como
//! antes, sin dejar nada a medias); un ejecutable suelto o una AppImage, lo mismo que pesa,
//! porque se copia. Se suma un margen para no dejar el disco a cero.
//!
//! Después de bajar y antes de descomprimir se mira otra vez, ya con el dato de verdad si el
//! archivo es un zip: su índice dice lo que ocupa descomprimido y leerlo no cuesta nada. Los
//! tar.gz y tar.xz no lo dicen sin descomprimirlos enteros, y para ellos (y los 7z) sigue
//! valiendo la estimación.
//!
//! El espacio libre lo da `fs4` (`statvfs` en Unix y `GetDiskFreeSpaceExW` en Windows): es
//! pequeña, no trae más dependencias que las que ya había (`rustix` y `windows-sys`) y evita
//! escribir `unsafe` aquí. Es el espacio que puede usar esta persona, con sus cuotas. Si no se
//! puede saber (un disco de red raro), no se bloquea nada.
use std::fs::File;
use std::path::{Path, PathBuf};

use super::installer::{Library, Plan};
use super::recipe::PackageKind;
use crate::archive;
use crate::{Error, Result};

/// Cuántas veces lo que pesa un comprimido se calcula que ocupa descomprimido.
pub const EXTRACT_FACTOR: u64 = 3;
/// Lo que se deja libre además de lo que hace falta.
pub const RESERVE: u64 = 64 << 20;

/// Lo que se calcula que ocupa instalado un paquete de `size` bytes.
pub fn installed_estimate(kind: PackageKind, size: u64) -> u64 {
    match kind {
        PackageKind::Portable | PackageKind::Appimage => size,
        _ => size.saturating_mul(EXTRACT_FACTOR),
    }
}

/// El espacio libre en el disco de `path`, para esta persona. Si `path` todavía no existe, se
/// mira la primera carpeta de arriba que sí. `None` si no se puede saber.
pub fn free(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
}

/// Dónde no cabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Where {
    Downloads,
    Games,
}

/// Lo que falta, si falta: dónde, cuánto se necesita y cuánto hay. `download` es lo que va a
/// ocupar lo bajado y `install`, lo instalado; `same` dice si las dos carpetas están en el mismo
/// disco (entonces tiene que caber todo junto). Un espacio libre desconocido no bloquea.
pub(crate) fn shortfall(download: u64, install: u64, free_downloads: Option<u64>, free_games: Option<u64>, same: bool) -> Option<(Where, u64, u64)> {
    let lacks = |place: Where, needed: u64, free: Option<u64>| {
        let needed = needed.saturating_add(RESERVE);
        free.filter(|f| *f < needed).map(|f| (place, needed, f))
    };
    if same {
        return lacks(Where::Games, download.saturating_add(install), free_games.or(free_downloads));
    }
    // Sin nada que escribir en un disco, no se le pide ni el margen.
    let downloads = if download > 0 { lacks(Where::Downloads, download, free_downloads) } else { None };
    downloads.or_else(|| if install > 0 { lacks(Where::Games, install, free_games) } else { None })
}

/// Si `a` y `b` están en el mismo disco. Si no se puede saber, se supone que sí, que es lo
/// normal: las dos carpetas cuelgan de la de la biblioteca.
fn same_volume(a: &Path, b: &Path) -> bool {
    let existing = |p: &Path| p.ancestors().find(|x| x.exists()).map(Path::to_path_buf);
    let (Some(a), Some(b)) = (existing(a), existing(b)) else { return true };
    volume(&a).zip(volume(&b)).is_none_or(|(x, y)| x == y)
}

/// Algo que identifica el disco de una carpeta que existe.
#[cfg(unix)]
fn volume(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

/// La letra de unidad o el recurso de red de la ruta ya resuelta (sin enlaces por medio).
#[cfg(not(unix))]
fn volume(path: &Path) -> Option<std::ffi::OsString> {
    match path.canonicalize().ok()?.components().next()? {
        std::path::Component::Prefix(p) => Some(p.as_os_str().to_ascii_lowercase()),
        _ => None,
    }
}

fn check(library: &Library, download: u64, install: u64) -> Result<()> {
    let same = same_volume(&library.downloads, &library.games);
    match shortfall(download, install, free(&library.downloads), free(&library.games), same) {
        Some((place, needed, free)) => {
            let path: PathBuf = if place == Where::Downloads { library.downloads.clone() } else { library.games.clone() };
            Err(Error::NoSpace { path, needed, free })
        }
        None => Ok(()),
    }
}

/// Antes de bajar: ¿caben lo bajado y lo instalado (estimado)? Sin tamaño conocido (una receta
/// con `url`, fuera de GitHub) no se puede saber todavía: se mira después de bajar.
pub(crate) fn check_before_download(library: &Library, plan: &Plan) -> Result<()> {
    match plan.size {
        Some(size) => check(library, size, installed_estimate(plan.kind, size)),
        None => Ok(()),
    }
}

/// Lo que ocupa descomprimido un zip según su índice, que se lee sin descomprimir nada.
pub(crate) fn zip_uncompressed(file: &Path) -> Option<u64> {
    let zip = zip::ZipArchive::new(File::open(file).ok()?).ok()?;
    zip.decompressed_size().and_then(|n| u64::try_from(n).ok())
}

/// Después de bajar y antes de descomprimir: lo bajado (`size` bytes) ya está en el disco, así
/// que solo tiene que caber lo instalado. Con un zip, lo que dice su índice; si no, la estimación.
pub(crate) fn check_before_extract(library: &Library, plan: &Plan, file: &Path, size: u64) -> Result<()> {
    let exact = match (plan.kind, archive::Kind::from_name(&plan.asset)) {
        (PackageKind::Archive, Some(archive::Kind::Zip)) => zip_uncompressed(file),
        _ => None,
    };
    check(library, 0, exact.unwrap_or_else(|| installed_estimate(plan.kind, size)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::installer::tests::{fake_plan, zip_with};

    const GB: u64 = 1 << 30;

    #[test]
    fn estima_lo_instalado_segun_el_paquete() {
        assert_eq!(installed_estimate(PackageKind::Archive, 100), 300);
        assert_eq!(installed_estimate(PackageKind::Dmg, 100), 300);
        assert_eq!(installed_estimate(PackageKind::Portable, 100), 100);
        assert_eq!(installed_estimate(PackageKind::Appimage, 100), 100);
        assert_eq!(installed_estimate(PackageKind::Archive, u64::MAX), u64::MAX);
    }

    #[test]
    fn en_un_solo_disco_tiene_que_caber_todo_junto() {
        // 1 GB bajado y 3 GB instalado: 4 GB y el margen.
        let need = 4 * GB + RESERVE;
        assert_eq!(shortfall(GB, 3 * GB, Some(need), Some(need), true), None);
        assert_eq!(shortfall(GB, 3 * GB, Some(need - 1), Some(need - 1), true), Some((Where::Games, need, need - 1)));
        assert_eq!(shortfall(GB, 3 * GB, Some(0), Some(0), true), Some((Where::Games, need, 0)));
        // Si solo se sabe el de una de las dos carpetas, vale ese.
        assert_eq!(shortfall(GB, 3 * GB, Some(GB), None, true), Some((Where::Games, need, GB)));
    }

    #[test]
    fn en_dos_discos_cada_uno_lo_suyo() {
        // Cabe lo bajado en uno y lo instalado en el otro, aunque juntos no cabrían en ninguno.
        assert_eq!(shortfall(GB, 3 * GB, Some(2 * GB), Some(3 * GB + RESERVE), false), None);
        assert_eq!(shortfall(GB, 3 * GB, Some(GB), Some(100 * GB), false), Some((Where::Downloads, GB + RESERVE, GB)));
        assert_eq!(shortfall(GB, 3 * GB, Some(100 * GB), Some(3 * GB), false), Some((Where::Games, 3 * GB + RESERVE, 3 * GB)));
        // Ya bajado: al disco de las descargas no se le pide nada.
        assert_eq!(shortfall(0, 3 * GB, Some(0), Some(100 * GB), false), None);
    }

    #[test]
    fn si_no_se_sabe_el_espacio_no_se_bloquea() {
        assert_eq!(shortfall(GB, 3 * GB, None, None, true), None);
        assert_eq!(shortfall(GB, 3 * GB, None, None, false), None);
        assert_eq!(shortfall(u64::MAX, u64::MAX, None, Some(GB), false), Some((Where::Games, u64::MAX, GB)));
    }

    #[test]
    fn mira_el_disco_de_verdad() {
        let t = tempfile::tempdir().unwrap();
        // De una carpeta que existe y de una que todavía no.
        let libre = free(t.path()).unwrap();
        assert!(libre > 0);
        assert!(free(&t.path().join("games").join("todavia-no")).is_some());
        assert!(same_volume(&t.path().join("downloads"), &t.path().join("games")));

        let lib = Library::new(t.path());
        let mut plan = fake_plan(&lib, None);
        // Un juego pequeño cabe.
        plan.size = Some(1 << 20);
        assert!(check_before_download(&lib, &plan).is_ok());
        // Sin tamaño no se sabe: se deja pasar.
        plan.size = None;
        assert!(check_before_download(&lib, &plan).is_ok());
        // Uno que no cabe en ningún disco: el error dice cuánto hace falta y cuánto hay.
        plan.size = Some(1 << 50);
        match check_before_download(&lib, &plan).unwrap_err() {
            Error::NoSpace { path, needed, free } => {
                assert_eq!(path, lib.games);
                assert_eq!(needed, (4u64 << 50) + RESERVE);
                assert!(free < needed && free > 0);
            }
            e => panic!("{e}"),
        }
    }

    #[test]
    fn de_un_zip_se_sabe_lo_que_ocupa_descomprimido() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("a.zip");
        zip_with(&z, &[("uno.bin", &[7u8; 5000]), ("dos/tres.bin", &[1u8; 300])]);
        assert_eq!(zip_uncompressed(&z), Some(5300));
        std::fs::write(t.path().join("no.zip"), b"no es un zip").unwrap();
        assert_eq!(zip_uncompressed(&t.path().join("no.zip")), None);

        let lib = Library::new(t.path());
        let plan = fake_plan(&lib, None);
        assert!(check_before_extract(&lib, &plan, &z, 200).is_ok());
        // Si no se puede leer el índice, vale la estimación.
        assert!(matches!(check_before_extract(&lib, &plan, &t.path().join("no.zip"), 1 << 50), Err(Error::NoSpace { .. })));
    }
}
