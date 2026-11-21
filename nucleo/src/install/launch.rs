//! Abrir un juego instalado: el ejecutable del recibo, con la carpeta del juego como carpeta de
//! trabajo (varios buscan ahí sus datos, sus ajustes o su `portable.txt`: los de HarbourMasters,
//! Fallout CE, SDLPAL, vkQuake…), con los argumentos y variables de su regla si Unricopie lo hizo
//! portable y con los que apagan su actualizador propio. `{game}` es la carpeta del juego.
//!
//! Antes, el launcher llama a `settings::apply`, que apaga el actualizador en su configuración,
//! y a `place::place`, que le deja la copia donde la busca. Si `copy.place` dice que la copia va
//! por argumento o variable (`{copy}`), aquí se pone.
//!
//! En Linux, además (`linux::adapt`): una AppImage se abre con `APPIMAGE_EXTRACT_AND_RUN=1` si el
//! equipo no tiene FUSE 2, lo demás lleva la carpeta del ejecutable y la del juego delante en
//! `LD_LIBRARY_PATH`, y a un ejecutable al que le faltan librerías del sistema no se le abre: se
//! dice cuáles (`Error::MissingLibraries`).
//!
//! En Windows (`windows::check`): si el ejecutable, o una DLL suya, importa las librerías de
//! Visual C++ o `vulkan-1.dll` y no están, tampoco se abre (`Error::MissingRuntime`,
//! `Error::MissingVulkan`).
//!
//! Falta: las apps de macOS (`.app`) y lo que pide `copy.how: argument`.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::installer::Library;
use super::portable::{check_rule, same_game};
use super::recipe::{PackageKind, Platform, Recipe};
use super::update::installed;
use crate::path::RelPath;
use crate::{Error, Result};

/// Cómo abrirlo. `command()` lo deja listo para `spawn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
}

impl Launch {
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args).current_dir(&self.cwd);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }
}

/// Cómo abrir lo instalado en `dir` (el juego de `recipe`), de la biblioteca `library`.
pub fn prepare(library: &Library, dir: &Path, recipe: &Recipe) -> Result<Launch> {
    let receipt = installed(dir)?;
    same_game(dir, &receipt, recipe)?;
    let run = receipt
        .run
        .as_deref()
        .ok_or_else(|| Error::NoRunInRecipe(dir.to_path_buf()))?;
    let mut program = RelPath::new(run)?.under(dir);
    // Una app de macOS es una carpeta: se abre su ejecutable directamente (no con `open`), para
    // que le lleguen las variables de la receta y se pueda esperar a que se cierre.
    if program.is_dir() {
        program = bundle_executable(&program).ok_or(Error::MacAppNotYet(program))?;
    }
    if !program.is_file() {
        return Err(Error::RunMissing(program));
    }
    let (mut args, mut env) = (Vec::new(), Vec::new());
    // Si la receta ya no trae la regla, se abre sin ella.
    if let (Some(_), Some(rule)) = (&receipt.portable, recipe.portable_rule(receipt.platform)) {
        check_rule(rule)?;
        args = rule.args.iter().map(|a| expand(a, dir)).collect();
        env = rule.env.iter().map(|(k, v)| (OsString::from(k), expand(v, dir))).collect();
    }
    for off in super::settings::launch_extras(recipe, &receipt) {
        super::settings::check_off(off)?;
        args.extend(off.args.iter().map(|a| expand(a, dir)));
        env.extend(off.env.iter().map(|(k, v)| (OsString::from(k), expand(v, dir))));
    }
    let (copy_args, copy_env) = super::place::launch_extras(library, dir, recipe, &receipt)?;
    args.extend(copy_args);
    env.extend(copy_env);
    // Solo lo instalado para Linux y en Linux: lo de otro sistema no se puede abrir aquí.
    if cfg!(target_os = "linux") && matches!(receipt.platform, Platform::LinuxX64 | Platform::LinuxArm64) {
        let appimage = recipe.resolve(receipt.game.as_deref(), receipt.platform).is_ok_and(|r| r.kind == PackageKind::Appimage);
        super::linux::adapt(&program, dir, appimage, &mut env)?;
    }
    // Lo mismo en Windows: si faltan Visual C++ o Vulkan, se dice antes de que lo diga Windows.
    if cfg!(windows) && matches!(receipt.platform, Platform::WindowsX64 | Platform::WindowsX86 | Platform::WindowsArm64) {
        super::windows::check(&program, dir)?;
    }
    Ok(Launch { program, args, env, cwd: dir.to_path_buf() })
}

/// El ejecutable de una app de macOS (`Juego.app/Contents/MacOS/…`): el que nombra
/// `CFBundleExecutable` en su `Info.plist`; si el plist es binario o no lo dice, el único archivo
/// de `MacOS`, o el que se llama como la app. Sin adivinar más: si no, `None`.
pub fn bundle_executable(app: &Path) -> Option<PathBuf> {
    let macos = app.join("Contents").join("MacOS");
    if let Ok(plist) = std::fs::read_to_string(app.join("Contents").join("Info.plist")) {
        if let Some(name) = plist_string(&plist, "CFBundleExecutable") {
            let exe = macos.join(&name);
            if !name.contains(['/', '\\']) && exe.is_file() {
                return Some(exe);
            }
        }
    }
    let files: Vec<PathBuf> = std::fs::read_dir(&macos).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    if let [solo] = files.as_slice() {
        return Some(solo.clone());
    }
    let stem = app.file_stem()?.to_string_lossy().to_lowercase();
    files.into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == stem))
}

/// El texto de `<key>clave</key><string>…</string>` en un plist XML.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = plist.split_once(&format!("<key>{key}</key>"))?.1.trim_start();
    let value = after.strip_prefix("<string>")?.split_once("</string>")?.0.trim();
    (!value.is_empty()).then(|| value.replace("&amp;", "&"))
}

/// `{game}` → la carpeta del juego, sin pasar por texto (la ruta puede no ser UTF-8).
fn expand(s: &str, game: &Path) -> OsString {
    let mut out = OsString::new();
    for (i, part) in s.split("{game}").enumerate() {
        if i > 0 {
            out.push(game.as_os_str());
        }
        out.push(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::portable::tests::{instalado, zelda_portable};
    use crate::install::portable::{disable, enable};
    use std::fs;

    #[test]
    fn abre_en_su_carpeta_y_con_lo_del_modo_portable() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        let r = zelda_portable();

        let normal = prepare(&Library::new(t.path()), &d, &r).unwrap();
        assert_eq!(normal.program, d.join("Zelda64Recompiled.exe"));
        assert_eq!(normal.cwd, d);
        assert!(normal.args.is_empty() && normal.env.is_empty());

        enable(&d, &r).unwrap();
        let p = prepare(&Library::new(t.path()), &d, &r).unwrap();
        let mut game = d.as_os_str().to_owned();
        assert_eq!(p.args, [OsString::from("+set"), "fs_homepath".into(), game.clone()]);
        game.push("/datos");
        assert_eq!(p.env, [(OsString::from("APP_FOLDER_PATH"), game)]);
        let c = p.command();
        assert_eq!(c.get_current_dir(), Some(d.as_path()));
        assert_eq!(c.get_args().count(), 3);

        disable(&d).unwrap();
        assert!(prepare(&Library::new(t.path()), &d, &r).unwrap().args.is_empty());
    }

    #[test]
    fn no_abre_lo_que_no_esta() {
        let t = tempfile::tempdir().unwrap();
        let d = instalado(t.path());
        fs::remove_file(d.join("Zelda64Recompiled.exe")).unwrap();
        assert!(matches!(prepare(&Library::new(t.path()), &d, &zelda_portable()), Err(Error::RunMissing(_))));
        assert!(matches!(prepare(&Library::new(t.path()), &t.path().join("nada"), &zelda_portable()), Err(Error::NoReceipt(_))));
    }

    /// Lo que hay en una carpeta: ruta, tamaño y fecha de cada cosa.
    #[cfg(windows)]
    fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        fn go(d: &Path, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
            let Ok(rd) = fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let m = e.metadata().unwrap();
                out.push((e.path(), m.len(), m.modified().unwrap()));
                if m.is_dir() {
                    go(&e.path(), out);
                }
            }
        }
        let mut out = Vec::new();
        go(dir, &mut out);
        out.sort();
        out
    }

    /// Cierra el juego pase lo que pase.
    #[cfg(windows)]
    struct Kill(std::process::Child);
    #[cfg(windows)]
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Instala Zelda64Recomp de verdad, lo hace portable con la regla de su receta y lo abre
    /// unos segundos: lo que escriba tiene que quedar en su carpeta y la de
    /// `%LOCALAPPDATA%\Zelda64Recompiled` no se tiene que tocar.
    #[test]
    #[cfg(windows)]
    #[ignore = "usa la red, baja el zip de Windows de Zelda64Recomp (28,9 MB) y abre el juego"]
    fn abre_zelda64recomp_portable_de_verdad() {
        use crate::install::data::Places;
        use crate::install::installer::{install, plan, Library};
        use crate::install::recipe::tests::ZELDA;
        use crate::install::recipe::Platform;
        use crate::install::release::Channel;
        use std::time::{Duration, Instant};

        let recipe = Recipe::from_json(&ZELDA.replace(
            r#""copy":"#,
            r#""data-dir": { "windows-x64": "{localappdata}/Zelda64Recompiled" },
            "portable": { "windows-x64": { "files": { "portable.txt": "" } } },
            "copy":"#,
        ))
        .unwrap();
        let system = Places::current().localappdata.unwrap().join("Zelda64Recompiled");
        let before = snapshot(&system);

        let t = tempfile::tempdir().unwrap();
        let lib = Library::new(t.path());
        let agent = crate::net::agent();
        let p = plan(&agent, &recipe, None, Platform::WindowsX64, Channel::Untested, None, &lib).unwrap();
        let rec = install(&agent, &lib, &p, &mut |_, _| {}).unwrap();
        assert!(rec.sha256_verified);
        let d = p.dest.clone();
        let package: Vec<PathBuf> = rec.files.iter().map(|f| f.path.under(&d)).collect();

        crate::install::portable::enable(&d, &recipe).unwrap();
        let l = prepare(&lib, &d, &recipe).unwrap();
        eprintln!("abre {} en {}", l.program.display(), l.cwd.display());
        let started = Instant::now();
        let mut child = Kill(l.command().spawn().unwrap());
        // Hasta 20 s, o hasta que haya escrito sus ajustes y pasen 3 s más.
        let new_files = |d: &Path| -> Vec<PathBuf> {
            snapshot(d)
                .into_iter()
                .map(|(p, ..)| p)
                .filter(|p| p.is_file() && !package.contains(p))
                .filter(|p| !p.ends_with("unricopie.json") && !p.ends_with("portable.txt"))
                .collect()
        };
        let mut seen = None;
        while started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(500));
            if seen.is_none() && !new_files(&d).is_empty() {
                seen = Some(Instant::now());
            }
            if seen.is_some_and(|s| s.elapsed() > Duration::from_secs(3)) {
                break;
            }
        }
        let alive = child.0.try_wait().unwrap().is_none();
        drop(child);
        let written = new_files(&d);
        eprintln!("seguía abierto al cerrarlo: {alive}; escribió en su carpeta:");
        for f in &written {
            eprintln!("  {} ({} bytes)", f.strip_prefix(&d).unwrap().display(), fs::metadata(f).unwrap().len());
        }
        assert!(!written.is_empty(), "no escribió nada en su carpeta");
        assert_eq!(snapshot(&system), before, "tocó {}", system.display());

        // Quitar el modo portable deja lo que escribió el juego.
        crate::install::portable::disable(&d).unwrap();
        assert!(!d.join("portable.txt").exists());
        assert!(written.iter().all(|f| f.is_file()));
    }

    #[test]
    fn cambia_game_por_la_carpeta() {
        let g = Path::new("/juegos/x");
        assert_eq!(expand("{game}", g), OsString::from("/juegos/x"));
        assert_eq!(expand("--configdir={game}/wz", g), OsString::from("--configdir=/juegos/x/wz"));
        assert_eq!(expand("sin nada", g), OsString::from("sin nada"));
    }

    #[test]
    fn encuentra_el_ejecutable_de_una_app_de_macos() {
        let t = tempfile::tempdir().unwrap();
        let app = |nombre: &str, plist: Option<&str>, archivos: &[&str]| {
            let a = t.path().join(nombre);
            let m = a.join("Contents").join("MacOS");
            fs::create_dir_all(&m).unwrap();
            if let Some(p) = plist {
                fs::write(a.join("Contents").join("Info.plist"), p).unwrap();
            }
            for f in archivos {
                fs::write(m.join(f), b"x").unwrap();
            }
            a
        };
        // Lo que dice el Info.plist manda, aunque haya otros archivos.
        let plist = "<plist><dict>\n\t<key>CFBundleName</key>\n\t<string>Zelda</string>\n\t<key>CFBundleExecutable</key>\n\t<string>Zelda64Recompiled</string>\n</dict></plist>";
        let z = app("Zelda64Recompiled.app", Some(plist), &["Zelda64Recompiled", "ayuda"]);
        assert_eq!(bundle_executable(&z), Some(z.join("Contents/MacOS/Zelda64Recompiled")));
        // Plist binario (no se lee) y un solo archivo: ese.
        let b = app("Banjo.app", Some("bplist00\u{1}raro"), &["BanjoRecompiled"]);
        assert_eq!(bundle_executable(&b), Some(b.join("Contents/MacOS/BanjoRecompiled")));
        // Sin plist y varios: el que se llama como la app, sin mayúsculas.
        let o = app("OpenMW.app", None, &["openmw", "openmw-launcher"]);
        assert_eq!(bundle_executable(&o), Some(o.join("Contents/MacOS/openmw")));
        // Varios y ninguno se llama así: no se adivina.
        let x = app("Raro.app", None, &["a", "b"]);
        assert_eq!(bundle_executable(&x), None);
        // Un plist que apunta fuera de MacOS no vale.
        let malo = "<key>CFBundleExecutable</key><string>../../fuera</string>";
        let f = app("Fuera.app", Some(malo), &["Fuera"]);
        assert_eq!(bundle_executable(&f), Some(f.join("Contents/MacOS/Fuera")));
    }
}
