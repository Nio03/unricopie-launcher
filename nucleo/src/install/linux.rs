//! Lo que hace falta para abrir un juego en Linux y no en los demás sistemas:
//!
//! - **AppImage sin FUSE 2.** El runtime clásico de las AppImage carga `libfuse.so.2` para
//!   montarse, y Ubuntu 24.04, Fedora y otras ya no la traen: el juego no arranca («AppImages
//!   require FUSE to run»). Con `APPIMAGE_EXTRACT_AND_RUN=1` el runtime se descomprime en la
//!   carpeta temporal y arranca igual. Solo se pone si FUSE 2 no está, porque descomprimir en cada
//!   arranque es más lento.
//! - **Librerías junto al ejecutable.** Varios proyectos traen sus `.so` al lado del binario y
//!   esperan que alguien ponga `LD_LIBRARY_PATH` (DevilutionX con `discord_game_sdk.so` y su
//!   `devilutionx.sh`). Se pone delante la carpeta del ejecutable y la del juego.
//! - **Librerías del sistema que faltan.** Un binario al que le falta `libSDL2-2.0.so.0` se cierra
//!   sin decir nada en una ventana. Antes de abrirlo se le pregunta a `ldd` y, si falta algo, se
//!   dice qué y con qué paquete se instala.
//!
//! Todo lo que se puede probar sin un Linux delante son funciones sobre datos; `adapt` es la que
//! mira el equipo.
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Error, Result};

const EXTRACT_AND_RUN: &str = "APPIMAGE_EXTRACT_AND_RUN";
const LD_LIBRARY_PATH: &str = "LD_LIBRARY_PATH";

/// Qué es el archivo que se va a abrir, por sus primeros bytes y su nombre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Program {
    /// Una AppImage: se monta (o se descomprime) sola y trae sus librerías.
    AppImage,
    /// Un ejecutable ELF del juego.
    Elf,
    /// Un script u otra cosa: lo que abra es cosa suya.
    Other,
}

/// Una AppImage se reconoce por la marca de su runtime (`AI` y el tipo en los bytes 8 a 10 del
/// ELF) o por su nombre, sin distinguir mayúsculas: el zip de 2 Ship 2 Harkinian trae
/// `2ship.appimage` y su receta dice que es un archivo comprimido, no una AppImage.
pub(crate) fn classify(path: &Path, appimage_in_recipe: bool) -> Program {
    let mut head = [0u8; 16];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = &head[..n];
    let elf = head.starts_with(b"\x7fELF");
    let mark = elf && head.len() >= 11 && &head[8..10] == b"AI" && matches!(head[10], 1 | 2);
    let name = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("appimage"));
    if mark || name || appimage_in_recipe {
        Program::AppImage
    } else if elf {
        Program::Elf
    } else {
        Program::Other
    }
}

/// Si `soname` está en el equipo: en la caché del enlazador (`/etc/ld.so.cache`, que es lo que
/// lista `ldconfig -p` y lo que consulta `dlopen`) o, si no, en alguna de `dirs`.
///
/// Se lee la caché en vez de llamar a `ldconfig -p` porque es lo mismo sin abrir un proceso y
/// sin depender de que `/sbin` esté en el PATH (en Debian no lo está para las personas). Las
/// carpetas cubren los sistemas sin caché (NixOS, los de musl) y una librería recién copiada.
pub(crate) fn has_library(soname: &str, cache: Option<&[u8]>, dirs: &[PathBuf]) -> bool {
    // En la caché cada nombre acaba en un byte cero.
    let needle = [soname.as_bytes(), b"\0"].concat();
    cache.is_some_and(|c| c.windows(needle.len()).any(|w| w == needle.as_slice())) || dirs.iter().any(|d| d.join(soname).exists())
}

/// Las carpetas donde las distribuciones ponen las librerías de 64 bits, y las de `LD_LIBRARY_PATH`.
fn library_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os(LD_LIBRARY_PATH).map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    let arch = if cfg!(target_arch = "aarch64") { "aarch64-linux-gnu" } else { "x86_64-linux-gnu" };
    for d in ["/usr/lib", "/lib"] {
        dirs.push(Path::new(d).join(arch));
    }
    dirs.extend(["/usr/lib64", "/lib64", "/usr/lib", "/lib", "/usr/local/lib"].map(PathBuf::from));
    dirs
}

/// Si una AppImage con el runtime clásico se puede montar: hace falta `libfuse.so.2` y el
/// dispositivo `/dev/fuse` (en un contenedor no suele estar).
fn fuse2_available() -> bool {
    Path::new("/dev/fuse").exists() && has_library("libfuse.so.2", fs::read("/etc/ld.so.cache").ok().as_deref(), &library_dirs())
}

/// La variable que hace que una AppImage se descomprima en vez de montarse, si hace falta: sin
/// FUSE 2 y sin que la persona (o la receta) ya haya dicho algo.
pub(crate) fn appimage_env(fuse2: bool, already_set: bool) -> Option<(OsString, OsString)> {
    (!fuse2 && !already_set).then(|| (OsString::from(EXTRACT_AND_RUN), OsString::from("1")))
}

/// `LD_LIBRARY_PATH` con `dirs` delante de lo que ya hubiera (`current`), sin repetir.
pub(crate) fn library_path(dirs: &[&Path], current: Option<&OsStr>) -> OsString {
    let mut all: Vec<PathBuf> = Vec::new();
    let old = current.map(|c| std::env::split_paths(c).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()).unwrap_or_default();
    for p in dirs.iter().map(|d| d.to_path_buf()).chain(old) {
        if !all.contains(&p) {
            all.push(p);
        }
    }
    // Una carpeta con el separador en el nombre no se puede poner: se salta.
    std::env::join_paths(all.iter().filter(|p| std::env::join_paths([p]).is_ok())).unwrap_or_default()
}

/// Los nombres de las librerías que `ldd` no encontró (`libX.so.1 => not found`), sin repetir.
pub(crate) fn parse_ldd(output: &str) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for line in output.lines() {
        let Some((name, found)) = line.split_once("=>") else { continue };
        let name = name.trim();
        if found.trim() == "not found" && !name.is_empty() && !missing.iter().any(|m| m == name) {
            missing.push(name.to_string());
        }
    }
    missing
}

/// Lo que le falta a `program` para arrancar, con el entorno con el que se va a abrir. Sin
/// `ldd`, o si no es un ejecutable dinámico, nada: se abre y que diga el sistema.
fn missing_libraries(program: &Path, env: &[(OsString, OsString)], dirs: &[&Path]) -> Vec<String> {
    let mut c = Command::new("ldd");
    c.arg(program).envs(env.iter().map(|(k, v)| (k, v)));
    // En inglés, para reconocer «not found».
    c.env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null());
    let Ok(out) = c.output() else { return Vec::new() };
    let mut missing = parse_ldd(&String::from_utf8_lossy(&out.stdout));
    // Por si `ldd` no hizo caso de LD_LIBRARY_PATH: lo que está en las carpetas del juego no falta.
    missing.retain(|lib| !dirs.iter().any(|d| d.join(lib).exists()));
    missing
}

/// La familia de la distribución, que decide con qué se instalan los paquetes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Debian, Ubuntu, Mint, Pop!_OS…: `apt`.
    Debian,
    /// Fedora, Bazzite, RHEL…: `dnf`.
    Fedora,
    /// Arch, SteamOS, Manjaro, CachyOS…: `pacman`.
    Arch,
}

impl Family {
    const ALL: [Family; 3] = [Family::Debian, Family::Fedora, Family::Arch];

    /// A partir de `/etc/os-release` (`ID` e `ID_LIKE`).
    pub fn from_os_release(text: &str) -> Option<Family> {
        let mut ids: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            if matches!(key.trim(), "ID" | "ID_LIKE") {
                ids.extend(value.trim().trim_matches(['"', '\'']).split_whitespace().map(str::to_ascii_lowercase));
            }
        }
        let has = |names: &[&str]| ids.iter().any(|i| names.contains(&i.as_str()));
        if has(&["debian", "ubuntu"]) {
            Some(Family::Debian)
        } else if has(&["fedora", "rhel", "centos"]) {
            Some(Family::Fedora)
        } else if has(&["arch", "steamos"]) {
            Some(Family::Arch)
        } else {
            None
        }
    }

    fn current() -> Option<Family> {
        let text = fs::read_to_string("/etc/os-release").or_else(|_| fs::read_to_string("/usr/lib/os-release")).ok()?;
        Family::from_os_release(&text)
    }

    fn name(self) -> &'static str {
        match self {
            Family::Debian => "Debian/Ubuntu",
            Family::Fedora => "Fedora",
            Family::Arch => "Arch/SteamOS",
        }
    }

    fn install(self) -> &'static str {
        match self {
            Family::Debian => "sudo apt install",
            Family::Fedora => "sudo dnf install",
            Family::Arch => "sudo pacman -S",
        }
    }
}

/// Las librerías que más faltan, con su paquete en Debian/Ubuntu, Fedora y Arch/SteamOS. El
/// nombre es el principio del soname.
const PACKAGES: [(&str, [&str; 3]); 8] = [
    ("libSDL2-2.0.so", ["libsdl2-2.0-0", "SDL2", "sdl2"]),
    ("libSDL2_image-2.0.so", ["libsdl2-image-2.0-0", "SDL2_image", "sdl2_image"]),
    ("libSDL2_mixer-2.0.so", ["libsdl2-mixer-2.0-0", "SDL2_mixer", "sdl2_mixer"]),
    ("libSDL2_ttf-2.0.so", ["libsdl2-ttf-2.0-0", "SDL2_ttf", "sdl2_ttf"]),
    ("libSDL2_net-2.0.so", ["libsdl2-net-2.0-0", "SDL2_net", "sdl2_net"]),
    ("libvulkan.so", ["libvulkan1", "vulkan-loader", "vulkan-icd-loader"]),
    ("libGL.so", ["libgl1", "libglvnd-glx", "libglvnd"]),
    ("libfuse.so.2", ["libfuse2", "fuse-libs", "fuse2"]),
];

/// La orden que instala lo que se conoce de `libs`: la de la distribución si se sabe cuál es
/// (`family`) y, si no, las tres. Vacío si no se conoce el paquete de ninguna.
pub fn install_hint(libs: &[String], family: Option<Family>) -> String {
    let command = |f: Family| -> Option<String> {
        let mut packages: Vec<&str> = Vec::new();
        for lib in libs {
            if let Some((_, names)) = PACKAGES.iter().find(|(prefix, _)| lib.starts_with(prefix)) {
                let name = names[f as usize];
                if !packages.contains(&name) {
                    packages.push(name);
                }
            }
        }
        (!packages.is_empty()).then(|| format!("{} {}", f.install(), packages.join(" ")))
    };
    match family {
        Some(f) => command(f).unwrap_or_default(),
        None => Family::ALL.iter().filter_map(|f| command(*f).map(|c| format!("{}: {c}", f.name()))).collect::<Vec<_>>().join(" · "),
    }
}

/// Deja `env` listo para abrir `program` (de la carpeta de juego `game`) en este Linux, y falla
/// con `MissingLibraries` si es un ejecutable al que le faltan librerías del sistema.
///
/// `LD_LIBRARY_PATH` se pone también a los scripts (lo heredan los binarios que abren); a las
/// AppImage no, que traen lo suyo, ni se les pregunta a `ldd`: su runtime es estático.
pub(crate) fn adapt(program: &Path, game: &Path, appimage_in_recipe: bool, env: &mut Vec<(OsString, OsString)>) -> Result<()> {
    let in_env = |env: &[(OsString, OsString)], key: &str| env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let kind = classify(program, appimage_in_recipe);
    if kind == Program::AppImage {
        let already = in_env(env, EXTRACT_AND_RUN).is_some() || std::env::var_os(EXTRACT_AND_RUN).is_some();
        // Mirar FUSE solo si hace falta.
        if !already {
            env.extend(appimage_env(fuse2_available(), false));
        }
        return Ok(());
    }
    let mut dirs: Vec<&Path> = Vec::new();
    if let Some(parent) = program.parent() {
        dirs.push(parent);
    }
    if !dirs.contains(&game) {
        dirs.push(game);
    }
    // Lo que diga la receta va detrás de las carpetas del juego y delante de lo del sistema.
    let current = in_env(env, LD_LIBRARY_PATH).or_else(|| std::env::var_os(LD_LIBRARY_PATH));
    env.retain(|(k, _)| k != LD_LIBRARY_PATH);
    env.push((OsString::from(LD_LIBRARY_PATH), library_path(&dirs, current.as_deref())));
    if kind == Program::Elf {
        let libs = missing_libraries(program, env, &dirs);
        if !libs.is_empty() {
            let hint = install_hint(&libs, Family::current());
            return Err(Error::MissingLibraries { program: program.to_path_buf(), libs, hint });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_una_appimage_por_la_marca_o_por_el_nombre() {
        let t = tempfile::tempdir().unwrap();
        let write = |name: &str, data: &[u8]| {
            let p = t.path().join(name);
            fs::write(&p, data).unwrap();
            p
        };
        let elf = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let appimage = b"\x7fELF\x02\x01\x01\x00AI\x02\x00\x00\x00\x00\x00";
        assert_eq!(classify(&write("juego", elf), false), Program::Elf);
        assert_eq!(classify(&write("juego.x86_64", appimage), false), Program::AppImage);
        // El nombre basta, en mayúsculas o minúsculas, y también lo que diga la receta.
        assert_eq!(classify(&write("2ship.appimage", elf), false), Program::AppImage);
        assert_eq!(classify(&write("Juego.AppImage", elf), false), Program::AppImage);
        assert_eq!(classify(&write("soh", elf), true), Program::AppImage);
        assert_eq!(classify(&write("juego.sh", b"#!/bin/sh\nexec ./juego\n"), false), Program::Other);
        assert_eq!(classify(&write("corto", b"\x7fEL"), false), Program::Other);
        assert_eq!(classify(&t.path().join("no-esta"), false), Program::Other);
    }

    #[test]
    fn busca_una_libreria_en_la_cache_y_en_las_carpetas() {
        let cache = b"glibc-ld.so.cache1.1\0libfuse3.so.3\0/lib/x86_64-linux-gnu/libfuse3.so.3\0libSDL2-2.0.so.0\0";
        assert!(has_library("libSDL2-2.0.so.0", Some(cache), &[]));
        // `libfuse3.so.3` no es `libfuse.so.2`, ni vale un nombre que solo empieza igual.
        assert!(!has_library("libfuse.so.2", Some(cache), &[]));
        assert!(!has_library("libSDL2-2.0.so", Some(cache), &[]));
        assert!(!has_library("libfuse.so.2", None, &[]));

        let t = tempfile::tempdir().unwrap();
        let dirs = [t.path().join("no-esta"), t.path().to_path_buf()];
        assert!(!has_library("libfuse.so.2", Some(cache), &dirs));
        fs::write(t.path().join("libfuse.so.2"), b"").unwrap();
        assert!(has_library("libfuse.so.2", Some(cache), &dirs));
        assert!(has_library("libfuse.so.2", None, &dirs));
    }

    #[test]
    fn solo_descomprime_la_appimage_si_no_hay_fuse() {
        assert_eq!(appimage_env(false, false), Some((OsString::from("APPIMAGE_EXTRACT_AND_RUN"), OsString::from("1"))));
        assert_eq!(appimage_env(true, false), None);
        // Si ya está puesta (a 0 o a 1), manda lo que haya.
        assert_eq!(appimage_env(false, true), None);
    }

    #[test]
    fn pone_las_carpetas_del_juego_delante() {
        let (bin, game) = (Path::new("/juegos/x/bin"), Path::new("/juegos/x"));
        assert_eq!(library_path(&[bin, game], None), OsString::from(["/juegos/x/bin", "/juegos/x"].join(SEP)));
        let old = OsString::from(["/opt/lib", "/juegos/x", "", "/otra"].join(SEP));
        let got = library_path(&[bin, game], Some(&old));
        assert_eq!(got, OsString::from(["/juegos/x/bin", "/juegos/x", "/opt/lib", "/otra"].join(SEP)));
        assert_eq!(library_path(&[], None), OsString::new());
    }

    /// El separador de PATH: `:` en Linux, que es donde se usa; `;` en Windows, donde solo se prueba.
    const SEP: &str = if cfg!(windows) { ";" } else { ":" };

    #[test]
    fn lee_lo_que_ldd_no_encuentra() {
        let out = "\tlinux-vdso.so.1 (0x00007ffd3a5f2000)\n\
                   \tlibSDL2-2.0.so.0 => not found\n\
                   \tlibSDL2_image-2.0.so.0 => not found\n\
                   \tlibstdc++.so.6 => /lib/x86_64-linux-gnu/libstdc++.so.6 (0x00007f2c1a000000)\n\
                   \tlibSDL2-2.0.so.0 => not found\n\
                   \t/lib64/ld-linux-x86-64.so.2 (0x00007f2c1a6d1000)\n";
        assert_eq!(parse_ldd(out), ["libSDL2-2.0.so.0", "libSDL2_image-2.0.so.0"]);
        assert!(parse_ldd("\tnot a dynamic executable\n").is_empty());
        assert!(parse_ldd("\tstatically linked\n").is_empty());
        assert!(parse_ldd("").is_empty());
    }

    #[test]
    fn reconoce_la_distribucion() {
        let ubuntu = "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(Family::from_os_release(ubuntu), Some(Family::Debian));
        assert_eq!(Family::from_os_release("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"), Some(Family::Debian));
        assert_eq!(Family::from_os_release("NAME=\"Fedora Linux\"\nID=fedora\n"), Some(Family::Fedora));
        assert_eq!(Family::from_os_release("ID=bazzite\nID_LIKE=\"fedora\"\n"), Some(Family::Fedora));
        assert_eq!(Family::from_os_release("ID=steamos\nID_LIKE=arch\n"), Some(Family::Arch));
        assert_eq!(Family::from_os_release("ID=cachyos\nID_LIKE=arch\n"), Some(Family::Arch));
        assert_eq!(Family::from_os_release("ID=nixos\n"), None);
        // `VERSION_ID` y compañía no cuentan.
        assert_eq!(Family::from_os_release("ID=void\nBUILD_ID=arch\nVERSION_ID=debian\n"), None);
    }

    #[test]
    fn dice_con_que_se_instala_lo_que_falta() {
        let libs: Vec<String> = ["libSDL2-2.0.so.0", "libSDL2_image-2.0.so.0", "librara.so.7", "libvulkan.so.1"].map(String::from).to_vec();
        assert_eq!(install_hint(&libs, Some(Family::Debian)), "sudo apt install libsdl2-2.0-0 libsdl2-image-2.0-0 libvulkan1");
        assert_eq!(install_hint(&libs, Some(Family::Fedora)), "sudo dnf install SDL2 SDL2_image vulkan-loader");
        assert_eq!(install_hint(&libs, Some(Family::Arch)), "sudo pacman -S sdl2 sdl2_image vulkan-icd-loader");
        let fuse = ["libfuse.so.2".to_string(), "libGL.so.1".to_string()];
        assert_eq!(
            install_hint(&fuse, None),
            "Debian/Ubuntu: sudo apt install libfuse2 libgl1 · Fedora: sudo dnf install fuse-libs libglvnd-glx · Arch/SteamOS: sudo pacman -S fuse2 libglvnd"
        );
        // De lo que no se conoce no se dice nada.
        assert_eq!(install_hint(&["librara.so.7".to_string()], Some(Family::Debian)), "");
        assert_eq!(install_hint(&["librara.so.7".to_string()], None), "");
        // `libfuse3.so.3` no es FUSE 2.
        assert_eq!(install_hint(&["libfuse3.so.3".to_string()], Some(Family::Debian)), "");
    }

    /// Un ejecutable de verdad al que le falta una librería: `ldd` lo dice y `adapt` no deja
    /// abrirlo; con la librería junto al ejecutable, sí. Se compila con el `cc` del equipo (sin
    /// él, la prueba no dice nada).
    #[test]
    #[cfg(target_os = "linux")]
    fn no_abre_un_ejecutable_al_que_le_faltan_librerias() {
        const SONAME: &str = "libunricopie_prueba.so.1";
        let t = tempfile::tempdir().unwrap();
        let cc = |args: &[&str]| Command::new("cc").args(args).current_dir(t.path()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        fs::write(t.path().join("lib.c"), "int unricopie_prueba(void) { return 7; }
").unwrap();
        fs::write(t.path().join("main.c"), "int unricopie_prueba(void);
int main(void) { return unricopie_prueba(); }
").unwrap();
        // La librería se guarda aparte, así que el ejecutable no la encuentra.
        fs::create_dir(t.path().join("aparte")).unwrap();
        let lib = format!("aparte/{SONAME}");
        if !cc(&["-shared", "-fPIC", &format!("-Wl,-soname,{SONAME}"), "-o", &lib, "lib.c"]) || !cc(&["-o", "juego", "main.c", &lib]) {
            eprintln!("sin compilador de C: no se prueba");
            return;
        }
        let (game, program) = (t.path(), t.path().join("juego"));
        assert_eq!(classify(&program, false), Program::Elf);
        let mut env = Vec::new();
        match adapt(&program, game, false, &mut env) {
            Err(Error::MissingLibraries { program: p, libs, hint }) => {
                assert_eq!(p, program);
                assert_eq!(libs, [SONAME]);
                assert_eq!(hint, "");
            }
            otro => panic!("tenía que faltar {SONAME}: {otro:?}"),
        }
        // Un script no se mira: lo que abra es cosa suya.
        let script = t.path().join("juego.sh");
        fs::write(&script, "#!/bin/sh
exec ./juego
").unwrap();
        adapt(&script, game, false, &mut Vec::new()).unwrap();

        // Con la librería junto al ejecutable ya no falta: la encuentra por LD_LIBRARY_PATH.
        fs::copy(t.path().join(&lib), t.path().join(SONAME)).unwrap();
        let mut env = Vec::new();
        adapt(&program, game, false, &mut env).unwrap();
        assert_eq!(env, [(OsString::from("LD_LIBRARY_PATH"), library_path(&[game], std::env::var_os("LD_LIBRARY_PATH").as_deref()))]);
        assert_eq!(Command::new(&program).envs(env.iter().map(|(k, v)| (k, v))).status().unwrap().code(), Some(7));
    }
}
