//! Abrir un juego instalado: el ejecutable del recibo, con la carpeta del juego como carpeta de
//! trabajo (varios buscan ahí sus datos, sus ajustes o su `portable.txt`: los de HarbourMasters,
//! Fallout CE, SDLPAL, vkQuake…), con los argumentos y variables de su regla si Unricopie lo hizo
//! portable y con los que apagan su actualizador propio. `{game}` es la carpeta del juego.
//!
//! Antes, el launcher llama a `settings::apply`, que apaga el actualizador en su configuración,
//! y a `place::place`, que le deja la copia donde la busca. Si `copy.place` dice que la copia va
//! por argumento o variable (`{copy}`), aquí se pone.
//!
//! Falta: las apps de macOS (`.app`) y lo que pide `copy.how: argument`.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::installer::Library;
use super::portable::{check_rule, same_game};
use super::recipe::Recipe;
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
    let program = RelPath::new(run)?.under(dir);
    if program.is_dir() {
        return Err(Error::MacAppNotYet(program));
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
    Ok(Launch { program, args, env, cwd: dir.to_path_buf() })
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
    struct Kill(std::process::Child);
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
}
