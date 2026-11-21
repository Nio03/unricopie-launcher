//! Lo que hace falta para abrir un juego en Windows, comprobado antes de abrirlo (como hace
//! `linux` con `ldd`):
//!
//! - **Visual C++.** Casi todos los proyectos se compilan con MSVC y necesitan las DLL del
//!   «Visual C++ Redistributable 2015-2022» (`vcruntime140.dll`, `msvcp140.dll`…). Si no están,
//!   Windows enseña «no se encontró VCRUNTIME140.dll» y poca gente sabe qué hacer con eso.
//! - **Vulkan.** Un juego que enlaza con `vulkan-1.dll` no arranca si el controlador de la
//!   gráfica no lo instaló.
//!
//! Se lee la tabla de importaciones del ejecutable y de las DLL que trae a su lado y de las que
//! depende: es la lista de lo que el cargador de Windows exige para arrancar. Las importaciones
//! diferidas y lo que el juego carga por su cuenta (`LoadLibrary`) no se miran: un juego puede
//! probar Vulkan así y usar otra cosa si no está, y eso no es un fallo.
//!
//! El lector de PE es propio, y pequeño, en vez de una librería como `object`: solo hacen falta
//! los nombres de las DLL importadas, se lee del disco nada más que las cabeceras y esa tabla
//! (los ejecutables de las recompilaciones pasan de cien megas, y `object` pide el archivo o la
//! sección entera en memoria), y cada desplazamiento que viene del archivo se comprueba. Un
//! archivo que no es un PE, o que está mal, no da error: no se comprueba y se abre.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// `IMAGE_FILE_MACHINE_*`: para qué procesador es el ejecutable.
const MACHINE_X86: u16 = 0x014c;
const MACHINE_ARM64: u16 = 0xaa64;

/// Las DLL que instala el «Visual C++ Redistributable 2015-2022», en minúsculas. Las de
/// depuración (`vcruntime140d.dll`) no están: no las instala, y un juego publicado no las pide.
const VC_RUNTIME: [&str; 16] = [
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "vcruntime140_threads.dll",
    "msvcp140.dll",
    "msvcp140_1.dll",
    "msvcp140_2.dll",
    "msvcp140_atomic_wait.dll",
    "msvcp140_codecvt_ids.dll",
    "concrt140.dll",
    "vcomp140.dll",
    "vccorlib140.dll",
    "vcamp140.dll",
    "mfc140.dll",
    "mfc140u.dll",
    "mfcm140.dll",
    "mfcm140u.dll",
];

const VULKAN: &str = "vulkan-1.dll";

/// Cuántos archivos se leen como mucho siguiendo las DLL del juego.
const MAX_MODULES: usize = 64;
/// Cuántas DLL importadas se leen de un archivo como mucho.
const MAX_IMPORTS: usize = 1024;

/// Lo que se saca de un ejecutable o una DLL de Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pe {
    pub(crate) machine: u16,
    /// Los nombres de las DLL que importa, en minúsculas y sin repetir.
    pub(crate) imports: Vec<String>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// Hasta `len` bytes a partir de `at`; menos si el archivo se acaba antes.
fn read_at<R: Read + Seek>(r: &mut R, at: u64, len: u64) -> Option<Vec<u8>> {
    r.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = Vec::new();
    r.take(len).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Una sección: dónde va en memoria (`va`) y dónde está en el archivo (`raw`, `raw_size`).
struct Section {
    va: u32,
    raw: u32,
    raw_size: u32,
}

/// En qué parte del archivo está la dirección de memoria `rva`, y cuántos bytes quedan desde ahí
/// en su sección.
fn offset_of(sections: &[Section], rva: u32) -> Option<(u64, u64)> {
    sections.iter().find_map(|s| {
        let within = rva.checked_sub(s.va)?;
        (within < s.raw_size).then(|| (u64::from(s.raw) + u64::from(within), u64::from(s.raw_size - within)))
    })
}

/// La máquina y las DLL que importa un PE (de 32 o de 64 bits). `None` si no lo es o si algo
/// no cuadra.
pub(crate) fn read_pe<R: Read + Seek>(r: &mut R) -> Option<Pe> {
    let dos = read_at(r, 0, 64)?;
    if !dos.starts_with(b"MZ") {
        return None;
    }
    let pe_at = u64::from(u32_at(&dos, 0x3c)?);
    // La firma, la cabecera COFF (20 bytes) y después la opcional.
    let coff = read_at(r, pe_at, 24)?;
    if !coff.starts_with(b"PE\0\0") {
        return None;
    }
    let machine = u16_at(&coff, 4)?;
    let n_sections = usize::from(u16_at(&coff, 6)?).min(96);
    let opt_size = u64::from(u16_at(&coff, 20)?);
    let opt = read_at(r, pe_at + 24, opt_size)?;
    // Dónde empiezan los directorios de datos: depende de si es PE32 o PE32+.
    let dirs = match u16_at(&opt, 0)? {
        0x010b => 96,
        0x020b => 112,
        _ => return None,
    };
    let n_dirs = u32_at(&opt, dirs - 4)?;
    let table = read_at(r, pe_at + 24 + opt_size, n_sections as u64 * 40)?;
    let sections: Vec<Section> = table
        .chunks_exact(40)
        .filter_map(|s| Some(Section { va: u32_at(s, 12)?, raw_size: u32_at(s, 16)?, raw: u32_at(s, 20)? }))
        .collect();
    let mut imports: Vec<String> = Vec::new();
    // El directorio 1 es la tabla de importaciones. Sin él (una DLL solo de recursos), nada.
    let import_rva = if n_dirs > 1 { u32_at(&opt, dirs + 8)? } else { 0 };
    if import_rva != 0 {
        let (at, left) = offset_of(&sections, import_rva)?;
        // Una entrada de 20 bytes por DLL, hasta una toda a ceros.
        let entries = read_at(r, at, left.min(MAX_IMPORTS as u64 * 20))?;
        for entry in entries.chunks_exact(20) {
            if entry.iter().all(|b| *b == 0) {
                break;
            }
            let (name_at, name_left) = offset_of(&sections, u32_at(entry, 12)?)?;
            let bytes = read_at(r, name_at, name_left.min(260))?;
            // Sin el cero del final, el archivo está cortado.
            let name = &bytes[..bytes.iter().position(|b| *b == 0)?];
            // Un nombre de archivo: sin rutas ni caracteres raros.
            if name.is_empty() || !name.iter().all(|b| b.is_ascii_graphic() || *b == b' ') || name.iter().any(|b| matches!(b, b'/' | b'\\' | b':')) {
                return None;
            }
            let name = String::from_utf8_lossy(name).to_ascii_lowercase();
            if !imports.contains(&name) {
                imports.push(name);
            }
        }
    }
    Some(Pe { machine, imports })
}

fn read_file(path: &Path) -> Option<Pe> {
    read_pe(&mut File::open(path).ok()?)
}

/// Lo que le falta a un juego para arrancar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Missing {
    /// Las DLL de Visual C++ que no están, por orden de nombre.
    pub(crate) runtime: Vec<String>,
    pub(crate) vulkan: bool,
    /// La máquina del ejecutable (0 si no es un PE).
    pub(crate) machine: u16,
}

/// Lo que importan `program` y las DLL del juego de las que depende (las que están en `local`,
/// que es donde Windows las busca antes que en el sistema): la máquina del ejecutable y todos
/// los nombres. `None` si `program` no es un PE.
fn needed(program: &Path, local: &[&Path]) -> Option<(u16, Vec<String>)> {
    let first = read_file(program)?;
    let machine = first.machine;
    let mut all: Vec<String> = Vec::new();
    let mut pending = vec![first];
    let mut read = 1;
    while let Some(pe) = pending.pop() {
        for name in pe.imports {
            if all.contains(&name) {
                continue;
            }
            // Una DLL del juego: lo que ella importe también hace falta.
            if read < MAX_MODULES {
                if let Some(dll) = local.iter().map(|d| d.join(&name)).find(|p| p.is_file()) {
                    read += 1;
                    pending.extend(read_file(&dll));
                }
            }
            all.push(name);
        }
    }
    Some((machine, all))
}

/// Lo que le falta a `program` (de la carpeta de juego `game`). `system` da las carpetas de
/// sistema para la máquina del ejecutable. Windows busca cada DLL en la carpeta del ejecutable,
/// en la de sistema y en la de trabajo, que al abrirlo es la del juego.
pub(crate) fn missing(program: &Path, game: &Path, system: &dyn Fn(u16) -> Vec<PathBuf>) -> Missing {
    let mut local: Vec<&Path> = Vec::new();
    if let Some(parent) = program.parent() {
        local.push(parent);
    }
    if !local.contains(&game) {
        local.push(game);
    }
    let Some((machine, all)) = needed(program, &local) else { return Missing::default() };
    let system = system(machine);
    let absent = |name: &str| !local.iter().copied().chain(system.iter().map(PathBuf::as_path)).any(|d| d.join(name).is_file());
    let mut runtime: Vec<String> = all.iter().filter(|n| VC_RUNTIME.contains(&n.as_str()) && absent(n)).cloned().collect();
    runtime.sort();
    let vulkan = all.iter().any(|n| n == VULKAN) && absent(VULKAN);
    Missing { runtime, vulkan, machine }
}

/// De dónde se baja el Visual C++ Redistributable 2015-2022 para esa máquina: los enlaces
/// permanentes de Microsoft a la última versión.
pub fn redist_url(machine: u16) -> &'static str {
    match machine {
        MACHINE_X86 => "https://aka.ms/vs/17/release/vc_redist.x86.exe",
        MACHINE_ARM64 => "https://aka.ms/vs/17/release/vc_redist.arm64.exe",
        _ => "https://aka.ms/vs/17/release/vc_redist.x64.exe",
    }
}

/// Las carpetas de sistema donde Windows busca las DLL de un programa de esa máquina. Unricopie
/// es de 64 bits, así que `System32` es la de verdad y la de los programas de 32 bits es
/// `SysWOW64`.
fn system_dirs(machine: u16) -> Vec<PathBuf> {
    let root = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let wow = root.join("SysWOW64");
    if machine == MACHINE_X86 && wow.is_dir() {
        vec![wow]
    } else {
        vec![root.join("System32")]
    }
}

/// Falla con `MissingRuntime` o `MissingVulkan` si a `program` (de la carpeta de juego `game`)
/// le falta algo de eso en este Windows. Si no es un ejecutable de Windows (un `.bat`), nada.
pub(crate) fn check(program: &Path, game: &Path) -> Result<()> {
    let m = missing(program, game, &system_dirs);
    if !m.runtime.is_empty() {
        return Err(Error::MissingRuntime { program: program.to_path_buf(), libs: m.runtime, url: redist_url(m.machine).to_string() });
    }
    if m.vulkan {
        return Err(Error::MissingVulkan { program: program.to_path_buf() });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;

    /// Un PE mínimo de 64 bits (o de 32 con `MACHINE_X86`) que importa esas DLL: las cabeceras
    /// y una sola sección con la tabla de importaciones y los nombres.
    pub(crate) fn fake_pe(machine: u16, imports: &[&str]) -> Vec<u8> {
        let pe32 = machine == MACHINE_X86;
        let opt_size: usize = if pe32 { 96 + 16 * 8 } else { 112 + 16 * 8 };
        let (pe_at, raw, va) = (0x80usize, 0x400usize, 0x1000u32);
        // La sección: las entradas (más la de ceros) y, detrás, los nombres.
        let table = (imports.len() + 1) * 20;
        let mut section = vec![0u8; table];
        for (i, name) in imports.iter().enumerate() {
            let name_rva = va + section.len() as u32;
            section[i * 20 + 12..i * 20 + 16].copy_from_slice(&name_rva.to_le_bytes());
            section.extend_from_slice(name.as_bytes());
            section.push(0);
        }
        let mut f = vec![0u8; raw];
        f[..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&(pe_at as u32).to_le_bytes());
        f[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
        f[pe_at + 4..pe_at + 6].copy_from_slice(&machine.to_le_bytes());
        f[pe_at + 6..pe_at + 8].copy_from_slice(&1u16.to_le_bytes());
        f[pe_at + 20..pe_at + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());
        let opt = pe_at + 24;
        f[opt..opt + 2].copy_from_slice(&(if pe32 { 0x010bu16 } else { 0x020b }).to_le_bytes());
        let dirs = opt + if pe32 { 96 } else { 112 };
        f[dirs - 4..dirs].copy_from_slice(&16u32.to_le_bytes());
        f[dirs + 8..dirs + 12].copy_from_slice(&va.to_le_bytes());
        f[dirs + 12..dirs + 16].copy_from_slice(&(table as u32).to_le_bytes());
        let s = opt + opt_size;
        f[s..s + 6].copy_from_slice(b".idata");
        f[s + 8..s + 12].copy_from_slice(&(section.len() as u32).to_le_bytes());
        f[s + 12..s + 16].copy_from_slice(&va.to_le_bytes());
        f[s + 16..s + 20].copy_from_slice(&(section.len() as u32).to_le_bytes());
        f[s + 20..s + 24].copy_from_slice(&(raw as u32).to_le_bytes());
        f.extend_from_slice(&section);
        f
    }

    const X64: u16 = 0x8664;

    #[test]
    fn lee_las_importaciones_de_un_pe() {
        let pe = read_pe(&mut Cursor::new(fake_pe(X64, &["KERNEL32.dll", "VCRUNTIME140.dll", "vulkan-1.dll", "kernel32.DLL"]))).unwrap();
        assert_eq!(pe.machine, X64);
        assert_eq!(pe.imports, ["kernel32.dll", "vcruntime140.dll", "vulkan-1.dll"]);
        let de32 = read_pe(&mut Cursor::new(fake_pe(MACHINE_X86, &["MSVCP140.dll"]))).unwrap();
        assert_eq!((de32.machine, de32.imports), (MACHINE_X86, vec!["msvcp140.dll".to_string()]));
        // Sin importaciones.
        assert_eq!(read_pe(&mut Cursor::new(fake_pe(X64, &[]))).unwrap().imports, Vec::<String>::new());
    }

    #[test]
    fn lo_que_no_es_un_pe_o_esta_roto_no_se_lee() {
        assert_eq!(read_pe(&mut Cursor::new(b"exe".to_vec())), None);
        assert_eq!(read_pe(&mut Cursor::new(b"#!/bin/sh\necho hola\n".to_vec())), None);
        assert_eq!(read_pe(&mut Cursor::new(Vec::new())), None);
        let bueno = fake_pe(X64, &["a.dll", "b.dll"]);
        // Cortado por cualquier sitio: nunca un pánico, y nunca una lista inventada.
        for corte in 0..bueno.len() {
            if let Some(pe) = read_pe(&mut Cursor::new(bueno[..corte].to_vec())) {
                assert!(pe.imports.iter().all(|n| n == "a.dll" || n == "b.dll"), "{corte}: {:?}", pe.imports);
            }
        }
        // La tabla de importaciones apunta fuera de las secciones.
        let mut fuera = bueno.clone();
        let dirs = 0x80 + 24 + 112;
        fuera[dirs + 8..dirs + 12].copy_from_slice(&0x00ff_0000u32.to_le_bytes());
        assert_eq!(read_pe(&mut Cursor::new(fuera)), None);
        // Un nombre con una ruta dentro.
        assert_eq!(read_pe(&mut Cursor::new(fake_pe(X64, &["..\\otra\\a.dll"]))), None);
    }

    /// Una DLL de Windows de verdad.
    #[test]
    #[cfg(windows)]
    fn lee_una_dll_del_sistema() {
        let system = system_dirs(X64).remove(0);
        let pe = read_file(&system.join("kernel32.dll")).unwrap();
        assert!(pe.machine == X64 || pe.machine == MACHINE_ARM64, "{:#x}", pe.machine);
        assert!(pe.imports.iter().any(|n| n == "ntdll.dll"), "{:?}", pe.imports);
        assert!(pe.imports.iter().all(|n| n.ends_with(".dll")), "{:?}", pe.imports);
        // Y un ejecutable.
        let cmd = read_file(&system.join("cmd.exe")).unwrap();
        assert!(!cmd.imports.is_empty());
    }

    #[test]
    fn dice_que_dll_de_visual_c_faltan() {
        let t = tempfile::tempdir().unwrap();
        let (game, system) = (t.path().join("juego"), t.path().join("System32"));
        fs::create_dir_all(game.join("bin")).unwrap();
        fs::create_dir_all(&system).unwrap();
        let exe = game.join("bin").join("juego.exe");
        fs::write(&exe, fake_pe(X64, &["KERNEL32.dll", "VCRUNTIME140.dll", "VCRUNTIME140_1.dll", "MSVCP140.dll", "SDL2.dll"])).unwrap();
        fs::write(system.join("kernel32.dll"), b"").unwrap();
        let sys = |_| vec![system.clone()];

        // No está ninguna: las tres, por orden. SDL2 no es cosa de Visual C++.
        let m = missing(&exe, &game, &sys);
        assert_eq!(m.runtime, ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"]);
        assert!(!m.vulkan && m.machine == X64);

        // Una en el sistema, otra junto al ejecutable y otra en la carpeta del juego: nada falta.
        fs::write(system.join("vcruntime140.dll"), b"").unwrap();
        fs::write(game.join("bin").join("msvcp140.dll"), b"").unwrap();
        fs::write(game.join("vcruntime140_1.dll"), b"").unwrap();
        assert_eq!(missing(&exe, &game, &sys).runtime, Vec::<String>::new());
    }

    #[test]
    fn sigue_las_dll_que_trae_el_juego() {
        let t = tempfile::tempdir().unwrap();
        let (game, system) = (t.path().join("juego"), t.path().join("System32"));
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&system).unwrap();
        let exe = game.join("juego.exe");
        // El exe no pide nada de Visual C++, pero su motor.dll sí, y esta, a su vez, otra.
        fs::write(&exe, fake_pe(X64, &["kernel32.dll", "motor.dll"])).unwrap();
        fs::write(game.join("motor.dll"), fake_pe(X64, &["sonido.dll", "msvcp140.dll", "juego.exe"])).unwrap();
        fs::write(game.join("sonido.dll"), fake_pe(X64, &["vcomp140.dll", "motor.dll"])).unwrap();
        let sys = |_| vec![system.clone()];
        let m = missing(&exe, &game, &sys);
        assert_eq!(m.runtime, ["msvcp140.dll", "vcomp140.dll"]);
        fs::write(system.join("msvcp140.dll"), b"").unwrap();
        fs::write(system.join("vcomp140.dll"), b"").unwrap();
        assert_eq!(missing(&exe, &game, &sys), Missing { runtime: Vec::new(), vulkan: false, machine: X64 });
    }

    #[test]
    fn dice_si_falta_vulkan() {
        let t = tempfile::tempdir().unwrap();
        let (game, system) = (t.path().join("juego"), t.path().join("System32"));
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&system).unwrap();
        let exe = game.join("juego.exe");
        fs::write(&exe, fake_pe(X64, &["kernel32.dll", "vulkan-1.dll"])).unwrap();
        let sys = |_| vec![system.clone()];
        let m = missing(&exe, &game, &sys);
        assert!(m.vulkan && m.runtime.is_empty());
        // Con el controlador de la gráfica instalado, o si el juego trae la suya.
        fs::write(system.join("vulkan-1.dll"), b"").unwrap();
        assert!(!missing(&exe, &game, &sys).vulkan);
        fs::remove_file(system.join("vulkan-1.dll")).unwrap();
        fs::write(game.join("vulkan-1.dll"), b"").unwrap();
        assert!(!missing(&exe, &game, &sys).vulkan);
        // Un juego que no la importa no la necesita para arrancar.
        fs::write(&exe, fake_pe(X64, &["kernel32.dll"])).unwrap();
        fs::remove_file(game.join("vulkan-1.dll")).unwrap();
        assert!(!missing(&exe, &game, &sys).vulkan);
    }

    #[test]
    fn lo_que_no_es_un_ejecutable_de_windows_no_se_comprueba() {
        let t = tempfile::tempdir().unwrap();
        let bat = t.path().join("juego.bat");
        fs::write(&bat, "@echo off\r\n").unwrap();
        assert_eq!(missing(&bat, t.path(), &|_| Vec::new()), Missing::default());
        assert!(check(&bat, t.path()).is_ok());
        assert!(check(&t.path().join("no-existe.exe"), t.path()).is_ok());
    }

    #[test]
    fn el_enlace_del_redistribuible_es_el_de_la_maquina() {
        assert_eq!(redist_url(X64), "https://aka.ms/vs/17/release/vc_redist.x64.exe");
        assert_eq!(redist_url(MACHINE_X86), "https://aka.ms/vs/17/release/vc_redist.x86.exe");
        assert_eq!(redist_url(MACHINE_ARM64), "https://aka.ms/vs/17/release/vc_redist.arm64.exe");
    }

    /// En este Windows, con Visual C++ y Vulkan instalados o no: un ejecutable del sistema que
    /// no pide ninguna de las dos cosas se abre.
    #[test]
    #[cfg(windows)]
    fn un_programa_del_sistema_pasa() {
        let cmd = system_dirs(X64).remove(0).join("cmd.exe");
        assert!(check(&cmd, cmd.parent().unwrap()).is_ok());
    }
}
