//! La línea de órdenes: el mismo `unricopie` sin ventana, para usarlo en scripts y en accesos
//! directos de Steam, Playnite o ES-DE.
//!
//!   unricopie list [--installed] [--json]
//!   unricopie install <id> [--version <etiqueta>]
//!   unricopie update <id> [--version <etiqueta>]
//!   unricopie versions <id> [--json]
//!   unricopie copies <carpeta> [--dry-run] [--json]
//!   unricopie launch <id> [--no-wait] [-- <argumentos para el juego>]
//!   unricopie path <id>
//!   unricopie add <owner/repo|dirección> [--asset <archivo>] [--run <programa>] [--json]
//!   unricopie remove-added <id>
//!
//! Sin argumentos se abre la ventana, como siempre. Cada orden hace lo mismo que su botón, con
//! las mismas funciones (`trabajo.rs`): `install` es Instalar, `update` es «Actualizar a …» y
//! `launch` es Jugar, que además cuenta el tiempo de juego (`tiempo.rs`). El catálogo y las
//! recetas salen de donde los saca la ventana (`datos.rs`).
//!
//! Dos cosas todavía no tienen botón: `versions` lista las versiones de un proyecto que se
//! pueden instalar aquí y `--version` instala una de ellas (con `update`, encima de la que
//! haya, sea más nueva o más vieja: es el mismo camino que actualizar, que guarda la anterior y
//! las partidas); `copies` busca en una carpeta las copias de los juegos y las guarda
//! (`nucleo::install::scan`).
//!
//! `add` instala un proyecto de GitHub que no está en el catálogo (`amano.rs` y
//! `nucleo::install::added`): dice qué archivo eligió y qué programa abre el juego, y si no lo
//! tiene claro da los candidatos para repetir la orden con `--asset` o `--run`. Lo añadido sale
//! en `list` marcado como tal, y desde ahí vale lo de siempre (`launch`, `update`, `versions`).
//!
//! Lo que se escribe aquí va en inglés: es para scripts y para gente de cualquier idioma. Sale
//! con 0 si todo fue bien, 1 si algo falló y 2 si la orden está mal escrita.
use std::cell::RefCell;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use unricopie_nucleo::antivirus::{Reason, Scan};
use unricopie_nucleo::install::added::{self, Found, Proposal, Why};
use unricopie_nucleo::install::release::{self, GhRelease};
use unricopie_nucleo::install::{self, scan, Channel, Library, Plan, Platform, Recipe, Version};
use unricopie_nucleo::root::Root;
use unricopie_nucleo::{net, Error};

use crate::amano::{self, NoQuitar, Rechazo};
use crate::catalogo::{self, Juego, Proyecto};
use crate::{consola, datos, mods, tiempo, trabajo};

const AYUDA: &str = "\
Usage:
  unricopie                                   Open the window
  unricopie list [--installed] [--json]
  unricopie install <id> [--version <tag>]
  unricopie update <id> [--version <tag>]
  unricopie versions <id> [--json]
  unricopie copies <folder> [--dry-run] [--json]
  unricopie launch <id> [--no-wait] [-- <game arguments>...]
  unricopie path <id>
  unricopie add <owner/repo|url> [--asset <name>] [--run <path>] [--json]
  unricopie remove-added <id>

Commands:
  list      The projects Unricopie can install on this system: id, installed
            version (- if it is not installed) and name.
              --installed   only the installed ones
              --json        as JSON: id, name, project, installed, version, path,
                            scan (how the antivirus scan of the download went:
                            state clean with its scanner, or not-scanned with
                            its reason; null if it is not known), added (true
                            for a project you added by hand)
            Projects added by hand (see `add`) are marked [added by hand].
  install   Download and install the latest official release of a project.
            The download is scanned with Microsoft Defender before it is
            unpacked (Windows only); if it finds a threat, nothing is installed.
              --version <tag>  that release instead (see `versions`)
  update    Update an installed project to the latest official release.
              --version <tag>  change it to that release instead, newer or
                            older. Like any update, the version you had is
                            kept to go back to, and so are your saves.
  versions  The releases of a project that can be installed on this system,
            newest first, with their date and size. * marks the installed one.
              --json        as JSON: tag, date, prerelease, asset, size,
                            latest, installed
  copies    Look in a folder (and its subfolders) for your copies of the games
            (ROMs, ISOs) and store the ones that match in the library. A file
            only counts if its hash is the one the project asks for; names are
            never guessed. Your files are only read, never changed. Copies
            that are a whole game folder, and files inside zip or 7z archives,
            are not found: choose those in the window.
              --dry-run     only say what was found, store nothing
              --json        as JSON: matches (id, name, copy, path, size,
                            result, error) and skipped (id, reason, detail)
  launch    Start an installed game like the Play button does, and wait until
            it exits, so Steam and other frontends can follow the session.
              --no-wait     return as soon as the game has started
              -- <args>     everything after -- is passed on to the game
  path      Print the folder where a project is installed.
  add       Install a GitHub project that is not in the catalogue, from the
            latest official release of its repository (owner/repo, or its
            github.com address). It goes through the same checks as the rest
            (size and SHA-256 from GitHub, antivirus scan, disk space), but
            Unricopie has not reviewed it: it does not know whether it works
            or what it needs, and it cannot check your copy of the game.
            Unricopie picks the file of the release for this system by its
            name, and after unpacking it, the program that starts the game.
            When either is not clear it guesses nothing: it lists the
            candidates and you run the command again with your choice.
            Installers (.msi, setup.exe) are not supported. A repository
            that the catalogue already installs is refused: use its id.
              --asset <name>  the file of the release to install
              --run <path>    the program that starts the game, inside its
                            folder
              --json        as JSON: status (installed, choose-asset or
                            choose-run), id, repo, release, asset, run, path,
                            scan, candidates, rejected (name, reason)
            Afterwards it is one more project: launch, update, versions and
            path work with its id.
  remove-added  Forget a project added by hand (its id, or owner/repo). It
            never deletes a game: if it is installed, delete its folder first.

<id> is the id that `unricopie list` shows (recomps/zelda-majoras-mask). The
part after the slash is enough when no other project shares it.

Options:
  --data-dir <dir>   Use <dir> as the library instead of the usual one
  --portable         Use the folder next to the program as the library, as if
                     portable.txt were there
  -h, --help         Show this help
  -V, --version      Show the version (after install or update, --version
                     takes a release tag instead)

Exit codes: 0 success, 1 error, 2 wrong usage.
";

/// Lo que se pide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orden {
    Ayuda,
    Version,
    Lista { instalados: bool, json: bool },
    /// `version`: esa release en vez de la última.
    Instalar { id: String, version: Option<String> },
    Actualizar { id: String, version: Option<String> },
    Versiones { id: String, json: bool },
    /// Busca en `carpeta` las copias de los juegos; con `simular`, sin guardar nada.
    Copias { carpeta: PathBuf, simular: bool, json: bool },
    Abrir { id: String, sin_esperar: bool, extra: Vec<OsString> },
    Ruta(String),
    /// `add`: instala un repo de GitHub que no está en el catálogo. `archivo` y `programa` son lo
    /// que elige la persona cuando Unricopie no lo tiene claro.
    Anadir { repo: String, archivo: Option<String>, programa: Option<String>, json: bool },
    /// `remove-added`: quita uno de los añadidos a mano.
    QuitarAnadido(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub orden: Orden,
    /// `--data-dir`: esta carpeta como biblioteca, en vez de la de siempre.
    pub data_dir: Option<PathBuf>,
    /// `--portable`: la carpeta del launcher, como si tuviera `portable.txt`.
    pub portable: bool,
}

/// Lee los argumentos (sin el nombre del programa). El error es la frase para la persona.
pub fn leer(args: Vec<OsString>) -> Result<Args, String> {
    let mut it = args.into_iter();
    let (mut data_dir, mut portable, mut ayuda, mut version) = (None, false, false, false);
    // La orden y su id, las opciones propias de una orden y lo que va detrás de `--`.
    let (mut palabras, mut opciones, mut extra) = (Vec::<String>::new(), Vec::<String>::new(), None);
    // `--version <etiqueta>` de `install` y `update`.
    let mut etiqueta: Option<String> = None;
    let sin_etiqueta = "--version needs a release tag (see `unricopie versions <id>`)";
    // `--asset <archivo>` y `--run <programa>` de `add`.
    let (mut archivo, mut programa) = (None::<String>, None::<String>);
    let (sin_archivo, sin_programa) = ("--asset needs the name of a file of the release", "--run needs the path of a program inside the game's folder");
    while let Some(a) = it.next() {
        let Some(s) = a.to_str() else {
            return Err(format!("this argument is not valid Unicode: {}", a.to_string_lossy()));
        };
        match s {
            "--" => extra = Some(it.by_ref().collect::<Vec<OsString>>()),
            "-h" | "--help" => ayuda = true,
            "-V" => version = true,
            // Detrás de `install` o `update` es la versión del juego; si no, la de Unricopie.
            "--version" if matches!(palabras.first().map(String::as_str), Some("install" | "update")) => {
                etiqueta = Some(it.next().and_then(|v| v.into_string().ok()).filter(|v| !v.is_empty()).ok_or(sin_etiqueta)?);
            }
            "--version" => version = true,
            _ if s.starts_with("--version=") => etiqueta = Some(s["--version=".len()..].to_string()).filter(|v| !v.is_empty()).ok_or(sin_etiqueta).map(Some)?,
            "--asset" => archivo = Some(it.next().and_then(|v| v.into_string().ok()).filter(|v| !v.is_empty()).ok_or(sin_archivo)?),
            _ if s.starts_with("--asset=") => archivo = Some(s["--asset=".len()..].to_string()).filter(|v| !v.is_empty()).ok_or(sin_archivo).map(Some)?,
            "--run" => programa = Some(it.next().and_then(|v| v.into_string().ok()).filter(|v| !v.is_empty()).ok_or(sin_programa)?),
            _ if s.starts_with("--run=") => programa = Some(s["--run=".len()..].to_string()).filter(|v| !v.is_empty()).ok_or(sin_programa).map(Some)?,
            "--portable" => portable = true,
            "--data-dir" => data_dir = Some(it.next().ok_or("--data-dir needs a folder")?),
            _ if s.starts_with("--data-dir=") => data_dir = Some(OsString::from(&s["--data-dir=".len()..])),
            "--installed" | "--json" | "--no-wait" | "--dry-run" => opciones.push(s.to_string()),
            _ if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => palabras.push(s.to_string()),
        }
    }
    if data_dir.as_ref().is_some_and(|d| d.is_empty()) {
        return Err("--data-dir needs a folder".into());
    }
    if data_dir.is_some() && portable {
        return Err("--data-dir and --portable cannot be used together".into());
    }
    let mut palabras = palabras.into_iter();
    let nombre = palabras.next();
    let orden = if ayuda || nombre.as_deref() == Some("help") {
        Orden::Ayuda
    } else if version {
        Orden::Version
    } else {
        let nombre = nombre.ok_or("no command given")?;
        let mut opcion = |o: &str| {
            let antes = opciones.len();
            opciones.retain(|x| x != o);
            opciones.len() != antes
        };
        let mut palabra = |que: &str| palabras.next().filter(|i| !i.is_empty()).ok_or(format!("`{nombre}` needs {que}"));
        let mut id = || palabra("the id of a project (see `unricopie list`)");
        let orden = match nombre.as_str() {
            "list" => Orden::Lista { instalados: opcion("--installed"), json: opcion("--json") },
            "install" => Orden::Instalar { id: id()?, version: etiqueta.take() },
            "update" => Orden::Actualizar { id: id()?, version: etiqueta.take() },
            "versions" => Orden::Versiones { id: id()?, json: opcion("--json") },
            "copies" => Orden::Copias { carpeta: PathBuf::from(palabra("the folder to look in")?), simular: opcion("--dry-run"), json: opcion("--json") },
            "path" => Orden::Ruta(id()?),
            "launch" => Orden::Abrir { id: id()?, sin_esperar: opcion("--no-wait"), extra: extra.take().unwrap_or_default() },
            "add" => Orden::Anadir { repo: palabra("a GitHub repository (owner/repo, or its address)")?, archivo: archivo.take(), programa: programa.take(), json: opcion("--json") },
            "remove-added" => Orden::QuitarAnadido(palabra("the id of a project added by hand (see `unricopie list`)")?),
            otra => return Err(format!("unknown command `{otra}`")),
        };
        if let Some(o) = opciones.first() {
            return Err(format!("`{nombre}` has no option {o}"));
        }
        if etiqueta.is_some() {
            return Err(format!("`{nombre}` has no option --version"));
        }
        if archivo.is_some() {
            return Err(format!("`{nombre}` has no option --asset"));
        }
        if programa.is_some() {
            return Err(format!("`{nombre}` has no option --run"));
        }
        if let Some(sobra) = palabras.next() {
            return Err(format!("unexpected argument `{sobra}`"));
        }
        if extra.is_some() {
            return Err(format!("`{nombre}` takes nothing after --"));
        }
        orden
    };
    Ok(Args { orden, data_dir: data_dir.map(PathBuf::from), portable })
}

/// Si `unricopie` se llamó con argumentos, hace lo pedido y devuelve con qué código salir; sin
/// argumentos devuelve `None` y se abre la ventana.
pub fn principal() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return None;
    }
    let mut err = Errores;
    let args = match leer(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "unricopie: {e}\nTry `unricopie --help`.");
            return Some(2);
        }
    };
    // `launch` no toca la consola si no hay un error que contar (`Errores`).
    if !matches!(args.orden, Orden::Abrir { .. }) {
        consola::conectar();
    }
    Some(correr(&args, &mut std::io::stdout(), &mut err))
}

/// La salida de errores, que se conecta a la consola de quien llamó (Windows) solo cuando hay
/// algo que escribir.
struct Errores;

impl Write for Errores {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        consola::conectar();
        std::io::stderr().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

/// Dónde se trabaja: la raíz de los datos y su biblioteca.
struct Entorno {
    raiz: PathBuf,
    /// El launcher está en modo portable: lo que se instala también lo es, si se puede.
    portable: bool,
    biblioteca: Library,
}

impl Entorno {
    fn de(args: &Args) -> Result<Entorno, String> {
        let root = match &args.data_dir {
            // Entera: es la carpeta de trabajo de los juegos, y lo que escribe `path`.
            Some(dir) => Root { path: std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?, portable: false },
            None if args.portable => Root::find_portable().map_err(|e| ingles(&e))?,
            None => Root::find().map_err(|e| ingles(&e))?,
        };
        Ok(Entorno { biblioteca: root.library(), raiz: root.path, portable: root.portable })
    }
}

fn correr(args: &Args, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match &args.orden {
        Orden::Ayuda => {
            let _ = write!(out, "Unricopie {}, the launcher of Recompendium (https://recompendium.com/unricopie)\n\n{AYUDA}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Orden::Version => {
            let _ = writeln!(out, "unricopie {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        _ => {}
    }
    // Antes de instalar o actualizar se busca un catálogo nuevo, como hace la ventana al abrir.
    let refrescar = matches!(args.orden, Orden::Instalar { .. } | Orden::Actualizar { .. } | Orden::Anadir { .. });
    let hecho = Entorno::de(args).and_then(|ent| {
        let juegos = leer_catalogo(&ent, refrescar)?;
        ejecutar(&args.orden, &ent, &juegos, out, err)
    });
    match hecho {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "unricopie: {e}");
            1
        }
    }
}

/// El catálogo con sus recetas, de donde lo saca la ventana: `UNRICOPIE_CATALOGO` si está; si
/// no, lo guardado en `<raíz>/datos/` o lo que viene junto al programa. Con `refrescar` se baja
/// antes el de recompendium.com; sin él, solo si no hay ninguno (para no depender de la red al
/// abrir un juego desde un acceso directo).
fn leer_catalogo(ent: &Entorno, refrescar: bool) -> Result<Vec<Juego>, String> {
    let mal = |e: String| format!("the catalogue could not be read: {e}");
    if let Some(archivo) = std::env::var_os("UNRICOPIE_CATALOGO") {
        return catalogo::leer_archivo(std::path::Path::new(&archivo), &ent.biblioteca).map(|(js, _)| js).map_err(mal);
    }
    if refrescar {
        datos::bajar(&ent.raiz);
    }
    let mut leido = datos::cargar(&ent.raiz);
    if leido.is_err() && !refrescar && datos::bajar(&ent.raiz) {
        leido = datos::cargar(&ent.raiz);
    }
    let (entradas, _, _) = leido.map_err(mal)?;
    Ok(catalogo::juntar(entradas, &ent.biblioteca).0)
}

fn ejecutar(orden: &Orden, ent: &Entorno, juegos: &[Juego], out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    match orden {
        Orden::Ayuda | Orden::Version => Ok(()),
        Orden::Lista { instalados, json } => {
            lista(juegos, *instalados, *json, out);
            Ok(())
        }
        Orden::Instalar { id, version } => instalar(ent, juegos, id, version.as_deref(), out, err),
        Orden::Actualizar { id, version } => actualizar(ent, juegos, id, version.as_deref(), out),
        Orden::Versiones { id, json } => {
            let p = buscar(juegos, id)?;
            let host = Platform::current().ok_or_else(|| ingles(&Error::UnknownSystem))?;
            let versiones = install::versions(&net::agent(), receta(p)?, None, host).map_err(|e| ingles(&e))?;
            lista_de_versiones(p, &versiones, *json, out);
            Ok(())
        }
        Orden::Copias { carpeta, simular, json } => copias(ent, juegos, carpeta, *simular, *json, Some(&net::agent()), out),
        Orden::Abrir { id, sin_esperar, extra } => abrir(ent, juegos, id, *sin_esperar, extra),
        Orden::Ruta(id) => {
            let (_, (dir, _)) = instalado(juegos, id)?;
            let _ = writeln!(out, "{}", dir.display());
            Ok(())
        }
        Orden::Anadir { repo, archivo, programa, json } => anadir(ent, juegos, repo, archivo.as_deref(), programa.as_deref(), *json, out),
        Orden::QuitarAnadido(id) => quitar_anadido(ent, juegos, id, out),
    }
}

/// El proyecto de ese id: la ficha entera (`recomps/zelda-majoras-mask`) o solo lo de después
/// de la barra, si no hay dos que Unricopie instale con el mismo.
fn buscar<'a>(juegos: &'a [Juego], id: &str) -> Result<&'a Proyecto, String> {
    let todos = || juegos.iter().flat_map(|j| &j.proyectos);
    if let Some(p) = todos().find(|p| p.e.entry == id) {
        return Ok(p);
    }
    let corto = |p: &&Proyecto| p.e.entry.split_once('/').is_some_and(|(_, c)| c == id);
    // Primero lo que Unricopie instala o tiene instalado; las demás fichas solo si no hay.
    let mut candidatos: Vec<&Proyecto> = todos().filter(corto).filter(|p| p.con_receta() || p.instalado.is_some()).collect();
    if candidatos.is_empty() {
        candidatos = todos().filter(corto).collect();
    }
    match candidatos.as_slice() {
        [p] => Ok(p),
        [] => Err(format!("there is no project with the id `{id}` (see `unricopie list`)")),
        varios => Err(format!("`{id}` could be {}: use the whole id", varios.iter().map(|p| p.e.entry.as_str()).collect::<Vec<_>>().join(" or "))),
    }
}

/// El proyecto de ese id, que tiene que estar instalado.
fn instalado<'a>(juegos: &'a [Juego], id: &str) -> Result<(&'a Proyecto, &'a (PathBuf, unricopie_nucleo::install::Receipt)), String> {
    let p = buscar(juegos, id)?;
    match &p.instalado {
        Some(i) => Ok((p, i)),
        None if p.instalable() => Err(format!("{} is not installed (install it with `unricopie install {}`)", p.e.entry, p.e.entry)),
        None => Err(format!("{} is not installed", p.e.entry)),
    }
}

/// La receta del proyecto, si Unricopie lo instala.
fn receta(p: &Proyecto) -> Result<&Recipe, String> {
    p.receta.as_ref().ok_or_else(|| format!("Unricopie does not know how to install {}", p.e.entry))
}

/// «The Legend of Zelda: Majora's Mask (Zelda64Recomp)»: el juego y el proyecto que lo lleva a PC.
fn titulo(p: &Proyecto) -> String {
    amano::titulo(p)
}

fn lista(juegos: &[Juego], instalados: bool, json: bool, out: &mut dyn Write) {
    let proyectos: Vec<&Proyecto> = juegos.iter().flat_map(|j| &j.proyectos).filter(|p| p.instalado.is_some() || (!instalados && p.instalable())).collect();
    if json {
        let filas: Vec<serde_json::Value> = proyectos
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.e.entry,
                    "name": p.e.nombre,
                    "project": p.nombre(),
                    "installed": p.instalado.is_some(),
                    "version": p.instalado.as_ref().map(|(_, r)| r.version.clone()),
                    "path": p.instalado.as_ref().map(|(dir, _)| dir.display().to_string()),
                    "scan": p.instalado.as_ref().and_then(|(_, r)| r.scan.as_ref()),
                    "added": p.e.a_mano,
                })
            })
            .collect();
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&filas).unwrap_or_default());
        return;
    }
    let version = |p: &Proyecto| p.instalado.as_ref().map(|(_, r)| r.version.clone()).unwrap_or_else(|| "-".into());
    let ancho_id = proyectos.iter().map(|p| p.e.entry.chars().count()).max().unwrap_or(0);
    let ancho_version = proyectos.iter().map(|p| version(p).chars().count()).max().unwrap_or(0);
    for p in proyectos {
        let marca = if p.e.a_mano { "  [added by hand]" } else { "" };
        let _ = writeln!(out, "{:ancho_id$}  {:ancho_version$}  {}{marca}", p.e.entry, version(p), titulo(p));
    }
}

/// Por qué un archivo de la release no se ofrece, en inglés y como va en el JSON.
fn motivo_archivo(why: Why) -> (&'static str, &'static str) {
    match why {
        Why::OtherSystem => ("other-system", "for another system"),
        Why::OtherArch => ("other-arch", "for another processor"),
        Why::Installer => ("installer", "an installer: Unricopie does not run installers"),
        Why::Extra => ("extra", "source code or debug files"),
        Why::Unsupported => ("unsupported", "not a package Unricopie can unpack"),
    }
}

/// `add`, hasta saber qué se instalaría: la ficha bajo la que queda el repo (o por qué no se
/// añade) y, con sus releases ya leídas, la receta. Si hay que elegir el archivo, escribe los
/// candidatos y devuelve el error.
#[allow(clippy::too_many_arguments)]
fn proponer(juegos: &[Juego], repo: &str, releases: &[GhRelease], host: Platform, archivo: Option<&str>, programa: Option<&str>, json: bool, out: &mut dyn Write) -> Result<Proposal, String> {
    let entry = ficha_de(juegos, repo)?;
    match added::propose(repo, &entry, releases, host, archivo, programa).map_err(|e| ingles(&e))? {
        Found::Ready(p) => Ok(*p),
        Found::Choose { tag, assets } => {
            if json {
                let salida = serde_json::json!({
                    "status": "choose-asset", "id": entry, "repo": repo, "release": tag, "candidates": assets.candidates,
                    "rejected": assets.rejected.iter().map(|r| serde_json::json!({ "name": r.name, "reason": motivo_archivo(r.why).0 })).collect::<Vec<_>>(),
                });
                let _ = writeln!(out, "{}", serde_json::to_string_pretty(&salida).unwrap_or_default());
            } else {
                let _ = match assets.candidates.len() {
                    0 => writeln!(out, "Release {tag} of {repo} has no file Unricopie can install on this system."),
                    _ => writeln!(out, "Release {tag} of {repo} has no single file that says it is for this system. It could be:"),
                };
                for c in &assets.candidates {
                    let _ = writeln!(out, "  {c}");
                }
                if !assets.rejected.is_empty() {
                    let _ = writeln!(out, "Not offered:");
                }
                for r in &assets.rejected {
                    let _ = writeln!(out, "  {} ({})", r.name, motivo_archivo(r.why).1);
                }
            }
            Err(match assets.candidates.len() {
                0 => format!("there is nothing to install from {repo} on this system"),
                _ => format!("choose the file with `unricopie add {repo} --asset <name>`"),
            })
        }
    }
}

/// La ficha bajo la que quedaría `repo`, o por qué no se añade a mano.
fn ficha_de(juegos: &[Juego], repo: &str) -> Result<String, String> {
    amano::ficha(juegos, repo).map_err(|r| match r {
        Rechazo::EnCatalogo { entry, nombre, .. } => {
            format!("{repo} is already in the catalogue: it is {entry}, {nombre}. Unricopie installs it with its reviewed recipe: `unricopie install {entry}`")
        }
        Rechazo::Retirado { entry } => format!("{repo} is in the catalogue as taken down ({entry}): Unricopie does not install it"),
        Rechazo::YaAnadido { entry } => format!("{repo} is already added as {entry}: `unricopie install {entry}` installs it and `unricopie remove-added {entry}` forgets it"),
    })
}

/// `add`: instala un repo de GitHub que no está en el catálogo y lo guarda entre los añadidos.
fn anadir(ent: &Entorno, juegos: &[Juego], texto: &str, archivo: Option<&str>, programa: Option<&str>, json: bool, out: &mut dyn Write) -> Result<(), String> {
    let repo = added::parse_repo(texto).map_err(|e| ingles(&e))?;
    // Antes de usar la red: si el catálogo ya lo instala, no hay nada que mirar.
    ficha_de(juegos, &repo)?;
    let host = Platform::current().ok_or_else(|| ingles(&Error::UnknownSystem))?;
    let agent = net::agent();
    let releases = release::list(&agent, &repo).map_err(|e| ingles(&e))?;
    let propuesta = proponer(juegos, &repo, &releases, host, archivo, programa, json, out)?;
    let entry = propuesta.recipe.entry.clone();
    if !json {
        let _ = writeln!(out, "Adding {repo} by hand as {entry}: it is not in the catalogue and Unricopie has not reviewed it");
    }
    let out = RefCell::new(out);
    let mut cuenta = Cuenta::default();
    let hecho = amano::instalar(
        &agent,
        &ent.biblioteca,
        &propuesta,
        &releases,
        &mut |plan| {
            if !json {
                anunciar(plan, *out.borrow_mut());
            }
        },
        &mut |bajado, total| {
            if let Some(linea) = cuenta.linea(bajado, total).filter(|_| !json) {
                let _ = writeln!(out.borrow_mut(), "{linea}");
            }
        },
    );
    let out = out.into_inner();
    match hecho {
        Ok(i) => {
            let run = i.recibo.run.clone().unwrap_or_default();
            if json {
                let salida = serde_json::json!({
                    "status": "installed", "id": entry, "repo": repo, "release": i.plan.version, "asset": i.plan.asset, "run": run,
                    "path": i.plan.dest.display().to_string(), "scan": i.recibo.scan,
                });
                let _ = writeln!(out, "{}", serde_json::to_string_pretty(&salida).unwrap_or_default());
            } else {
                let _ = writeln!(out, "{}", analisis(i.recibo.scan.as_ref()));
                let _ = writeln!(out, "Installed {entry} {} in {}", i.plan.version, i.plan.dest.display());
                let _ = writeln!(out, "It starts {run} (`unricopie launch {entry}`)");
            }
            Ok(())
        }
        Err(Error::ChooseRun { candidates }) => {
            let con_archivo = format!(" --asset {}", propuesta.asset);
            if json {
                let salida = serde_json::json!({ "status": "choose-run", "id": entry, "repo": repo, "release": propuesta.tag, "asset": propuesta.asset, "candidates": candidates });
                let _ = writeln!(out, "{}", serde_json::to_string_pretty(&salida).unwrap_or_default());
            } else {
                let _ = writeln!(out, "{} was downloaded and passed its checks, but nothing was installed: Unricopie cannot tell which program starts the game.", propuesta.asset);
                if !candidates.is_empty() {
                    let _ = writeln!(out, "It could be:");
                }
                for c in &candidates {
                    let _ = writeln!(out, "  {c}");
                }
            }
            Err(match candidates.len() {
                0 => format!("{} has no program Unricopie recognises; if you know which file starts the game: `unricopie add {repo}{con_archivo} --run <path>`", propuesta.asset),
                _ => format!("choose the program with `unricopie add {repo}{con_archivo} --run <path>`"),
            })
        }
        Err(e) => Err(ingles(&e)),
    }
}

/// `remove-added`: quita uno de los añadidos a mano, por su ficha, por lo de después de la barra
/// o por su repo. Nunca borra un juego.
fn quitar_anadido(ent: &Entorno, juegos: &[Juego], id: &str, out: &mut dyn Write) -> Result<(), String> {
    let todas = added::load(&ent.biblioteca).map_err(|e| ingles(&e))?;
    let repo = added::parse_repo(id).ok();
    let suyas: Vec<&Recipe> = todas.iter().filter(|r| r.entry == id || r.slug() == id || repo.as_ref().is_some_and(|x| x.eq_ignore_ascii_case(&r.repo))).collect();
    let receta = match suyas.as_slice() {
        [una] => *una,
        [] => return Err(format!("no project added by hand has the id `{id}` (see `unricopie list`)")),
        varias => return Err(format!("`{id}` could be {}: use the whole id", varias.iter().map(|r| r.entry.as_str()).collect::<Vec<_>>().join(" or "))),
    };
    match amano::quitar(&ent.biblioteca, juegos, &receta.entry) {
        Ok(_) => {
            let _ = writeln!(out, "Removed {} ({}) from the projects added by hand", receta.entry, receta.repo);
            Ok(())
        }
        Err(NoQuitar::Instalado(dir)) => Err(format!("{} is installed in {}: remove-added never deletes a game. Delete that folder first and run it again", receta.entry, dir.display())),
        Err(NoQuitar::NoEsta) => Err(format!("{} is not among the projects added by hand", receta.entry)),
        Err(NoQuitar::Error(e)) => Err(ingles(&e)),
    }
}

fn instalar(ent: &Entorno, juegos: &[Juego], id: &str, version: Option<&str>, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    let p = buscar(juegos, id)?;
    if let Some((dir, recibo)) = &p.instalado {
        let (ya, e) = (format!("{} is already installed ({}) in {}", p.e.entry, recibo.version, dir.display()), &p.e.entry);
        return Err(match version {
            Some(v) => format!("{ya}; `unricopie update {e} --version {v}` changes it to that version"),
            None => format!("{ya}; `unricopie update {e}` looks for a newer version"),
        });
    }
    let receta = receta(p)?;
    if !p.instalable() {
        return Err(format!("Unricopie cannot install {} on this system", p.e.entry));
    }
    let _ = writeln!(out, "Installing {}", titulo(p));
    let out = RefCell::new(out);
    let mut cuenta = Cuenta::default();
    let (plan, sin_portable) = trabajo::instalar_con(&net::agent(), &ent.biblioteca, ent.portable, receta, version, &mut |plan| anunciar(plan, *out.borrow_mut()), &mut |bajado, total| {
        if let Some(linea) = cuenta.linea(bajado, total) {
            let _ = writeln!(out.borrow_mut(), "{linea}");
        }
    })
    .map_err(|e| ingles(&e))?;
    let _ = writeln!(out.borrow_mut(), "{}", analisis_de(&plan.dest));
    let _ = writeln!(out.borrow_mut(), "Installed {} {} in {}", plan.entry, plan.version, plan.dest.display());
    if let Some(e) = sin_portable {
        let _ = writeln!(err, "unricopie: warning: it is installed, but it could not be made portable: {}", ingles(&e));
    }
    Ok(())
}

fn actualizar(ent: &Entorno, juegos: &[Juego], id: &str, version: Option<&str>, out: &mut dyn Write) -> Result<(), String> {
    let (p, (_, recibo)) = instalado(juegos, id)?;
    let receta = receta(p)?;
    // Si tiene mods, se quitan antes y se vuelven a poner después, como hace la ventana.
    let con_mods = mods::Ctx::de(&ent.raiz, &ent.biblioteca, &p.e.entry, receta, p.instalado.as_ref());
    let _ = match version {
        Some(v) => writeln!(out, "Changing {} from {} to {v}", titulo(p), recibo.version),
        None => writeln!(out, "Updating {} (installed: {})", titulo(p), recibo.version),
    };
    let out = RefCell::new(out);
    let mut cuenta = Cuenta::default();
    let hecho = trabajo::actualizar(&net::agent(), &ent.biblioteca, receta, version, con_mods.as_ref(), &mut |plan| anunciar(plan, *out.borrow_mut()), &mut |bajado, total| {
        if let Some(linea) = cuenta.linea(bajado, total) {
            let _ = writeln!(out.borrow_mut(), "{linea}");
        }
    });
    match hecho {
        Ok(plan) => {
            let _ = writeln!(out.borrow_mut(), "{}", analisis_de(&plan.dest));
            let _ = writeln!(out.borrow_mut(), "Updated {} to {} in {}", plan.entry, plan.version, plan.dest.display());
            // Lo que había sigue entero en la biblioteca: la ventana lo enseña para volver atrás.
            if let Some(anterior) = unricopie_nucleo::install::update::previous_versions(&ent.biblioteca, &plan.dest).ok().and_then(|v| v.into_iter().next()) {
                let _ = writeln!(out.borrow_mut(), "The version you had ({}) is kept in {}", anterior.receipt.version, anterior.path.display());
            }
            Ok(())
        }
        Err(Error::UpToDate(v)) => {
            let _ = writeln!(out.borrow_mut(), "{} is up to date ({v})", p.e.entry);
            Ok(())
        }
        Err(e) => Err(ingles(&e)),
    }
}

/// Cómo fue el análisis del antivirus de lo que se acaba de instalar en `dir`, como lo apunta
/// su recibo.
fn analisis_de(dir: &Path) -> String {
    let recibo = install::Receipt::read(dir).ok().flatten();
    analisis(recibo.as_ref().and_then(|r| r.scan.as_ref()))
}

/// El resultado del análisis del antivirus, en una línea.
fn analisis(scan: Option<&Scan>) -> String {
    let sin = |porque: &str| format!("Not scanned by an antivirus: {porque}");
    match scan {
        Some(Scan::Clean { scanner }) => format!("Scanned with {scanner}: no threats found"),
        Some(Scan::NotScanned { reason }) => sin(match reason {
            Reason::NoScanner => "there is no scanner on this system",
            Reason::Disabled => "Microsoft Defender is turned off",
            Reason::Timeout => "the scan took too long",
            Reason::Failed => "the scan could not be run",
            Reason::Unknown => "the reason is not known",
        }),
        None => sin("it was installed before Unricopie scanned downloads"),
    }
}

/// `versions`: las versiones que se pueden instalar aquí, la más nueva primero.
fn lista_de_versiones(p: &Proyecto, versiones: &[Version], json: bool, out: &mut dyn Write) {
    let instalada = p.instalado.as_ref().map(|(_, r)| r.version.as_str());
    // La que bajaría `install` sin más: la última que no es una prerelease.
    let ultima = versiones.iter().find(|v| !v.prerelease).map(|v| v.tag.as_str());
    if json {
        let filas: Vec<serde_json::Value> = versiones
            .iter()
            .map(|v| {
                serde_json::json!({
                    "tag": v.tag,
                    "date": v.published_at,
                    "prerelease": v.prerelease,
                    "asset": v.asset,
                    "size": v.size,
                    "latest": ultima == Some(v.tag.as_str()),
                    "installed": instalada == Some(v.tag.as_str()),
                })
            })
            .collect();
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&filas).unwrap_or_default());
        return;
    }
    if versiones.is_empty() {
        let _ = writeln!(out, "None of the latest releases of {} has a file for this system", p.e.repo);
    }
    let ancho = versiones.iter().map(|v| v.tag.chars().count()).max().unwrap_or(0);
    for v in versiones {
        let es_instalada = instalada == Some(v.tag.as_str());
        let fecha = v.published_at.as_deref().and_then(|f| f.get(..10)).unwrap_or("-");
        let peso = v.size.map(|s| crate::tam(s, "en")).unwrap_or_else(|| "-".into());
        let notas: Vec<&str> = [(v.prerelease, "prerelease"), (ultima == Some(v.tag.as_str()), "latest"), (es_instalada, "installed")].iter().filter(|(si, _)| *si).map(|(_, n)| *n).collect();
        let linea = format!("{} {:ancho$}  {fecha:10}  {peso:>8}  {}", if es_instalada { '*' } else { ' ' }, v.tag, notas.join(", "));
        let _ = writeln!(out, "{}", linea.trim_end());
    }
    if let Some(i) = instalada.filter(|i| !versiones.iter().any(|v| v.tag == *i)) {
        let _ = writeln!(out, "* {i} is installed; it is not among the latest releases");
    }
}

/// `copies`: busca en `carpeta` las copias de los juegos que Unricopie instala aquí (y de los
/// instalados) y guarda las que coinciden, salvo con `simular`. `agent` es para las listas de
/// copias que publica algún proyecto (`accepts-from`); sin él se busca solo con las recetas.
fn copias(ent: &Entorno, juegos: &[Juego], carpeta: &Path, simular: bool, json: bool, agent: Option<&ureq::Agent>, out: &mut dyn Write) -> Result<(), String> {
    let proyectos: Vec<&Proyecto> = juegos.iter().flat_map(|j| &j.proyectos).filter(|p| p.receta.is_some() && (p.instalado.is_some() || p.instalable())).collect();
    let nombre = |entry: &str| proyectos.iter().find(|p| p.e.entry == entry).map(|p| titulo(p)).unwrap_or_else(|| entry.to_string());
    let buscadas = scan::wanted(agent, &ent.biblioteca, proyectos.iter().filter_map(|p| p.receta.clone()));
    let sin_cortar = AtomicBool::new(false);
    if !json {
        let _ = writeln!(out, "Looking in {} for your copies of the games", carpeta.display());
    }
    let visto = scan::scan(&ent.biblioteca, &buscadas, carpeta, &scan::Options::default(), &sin_cortar, &mut contar_copias(out, json)).map_err(|e| ingles(&e))?;
    // Qué pasó con cada archivo encontrado: su resultado (como va en el JSON) y el error.
    let mut filas: Vec<(&scan::Match, &'static str, Option<String>)> = Vec::new();
    if simular {
        let mut vistos: Vec<&str> = Vec::new();
        for m in &visto.matches {
            let repetida = vistos.contains(&m.entry.as_str());
            vistos.push(&m.entry);
            filas.push((m, if repetida { "duplicate" } else { "would-import" }, None));
        }
    } else {
        let guardado = scan::import_matches(&ent.biblioteca, &buscadas, &visto.matches, &sin_cortar, &mut contar_copias(out, json));
        for (m, r) in visto.matches.iter().zip(&guardado.results) {
            filas.push(match &r.outcome {
                scan::Outcome::Imported(_) => (m, "imported", None),
                scan::Outcome::AlreadyImported => (m, "already-imported", None),
                scan::Outcome::Duplicate => (m, "duplicate", None),
                scan::Outcome::Failed(e) => (m, "failed", Some(ingles(e))),
            });
        }
    }
    let fallos = filas.iter().filter(|f| f.1 == "failed").count();
    let motivo = |w: &scan::Why| match w {
        scan::Why::AlreadyImported(copia) => ("already-imported", copia.clone()),
        scan::Why::Folder => ("folder", String::new()),
        scan::Why::NoHashes => ("no-hashes", String::new()),
        scan::Why::ListUnavailable(e) => ("list-unavailable", e.clone()),
        scan::Why::OwnInstaller => ("own-installer", String::new()),
    };
    if json {
        let salida = serde_json::json!({
            "folder": carpeta.display().to_string(),
            "dry_run": simular,
            "searched": visto.searched,
            "files": visto.files,
            "bytes": visto.bytes,
            "matches": filas.iter().map(|(m, que, error)| serde_json::json!({
                "id": m.entry, "name": nombre(&m.entry), "copy": m.accepted, "path": m.path.display().to_string(), "size": m.size, "result": que, "error": error,
            })).collect::<Vec<_>>(),
            "skipped": visto.skipped.iter().map(|k| {
                let (reason, detail) = motivo(&k.why);
                serde_json::json!({ "id": k.entry, "name": nombre(&k.entry), "reason": reason, "detail": detail })
            }).collect::<Vec<_>>(),
            "unreadable": visto.unreadable.iter().map(|x| x.display().to_string()).collect::<Vec<_>>(),
        });
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&salida).unwrap_or_default());
    } else {
        let juegos_buscados = if visto.searched == 1 { "1 game".to_string() } else { format!("{} games", visto.searched) };
        if filas.is_empty() {
            let _ = writeln!(out, "No copy found: {} read, looking for the copies of {juegos_buscados}", if visto.files == 1 { "1 file".to_string() } else { format!("{} files", visto.files) });
        }
        for (m, que, error) in &filas {
            let que = match (*que, error) {
                ("imported", _) => "Stored".to_string(),
                ("would-import", _) => "Found".to_string(),
                ("already-imported", _) => "Found, but the library already has a verified copy of".to_string(),
                ("duplicate", _) => "Found again, not needed for".to_string(),
                (_, e) => format!("Could not store ({}) the copy of", e.clone().unwrap_or_default()),
            };
            let _ = writeln!(out, "{que} {}: {}\n    {} ({})", nombre(&m.entry), m.accepted, m.path.display(), crate::tam(m.size, "en"));
        }
        if simular && !filas.is_empty() {
            let _ = writeln!(out, "Dry run: nothing was stored");
        }
        // Lo que no se buscó, por motivo, para que nadie crea que su copia «no coincide».
        let cuantas = |r: &str| visto.skipped.iter().filter(|k| motivo(&k.why).0 == r).count();
        for (r, texto) in [
            ("already-imported", "already have a verified copy in the library"),
            ("folder", "take the game's folder, not a file: choose it in the window"),
            ("no-hashes", "have no hashes in their recipe to compare with: choose the copy in the window"),
            ("own-installer", "ask for the copy in their own installer"),
            ("list-unavailable", "publish their list of copies online and it could not be read"),
        ] {
            if cuantas(r) > 0 {
                let _ = writeln!(out, "Not searched: {} {texto}", cuantas(r));
            }
        }
        if !visto.unreadable.is_empty() {
            let _ = writeln!(out, "{} files or folders could not be read", visto.unreadable.len());
        }
    }
    match fallos {
        0 => Ok(()),
        1 => Err("1 copy could not be stored".into()),
        n => Err(format!("{n} copies could not be stored")),
    }
}

/// Cómo va `copies`, en líneas: cuánto hay que leer (o guardar) y una línea cada 10 %. Con
/// `--json` no se escribe nada hasta el final.
fn contar_copias<'a>(out: &'a mut dyn Write, json: bool) -> impl FnMut(scan::Progress<'_>) + 'a {
    let mut cuenta = Cuenta::default();
    let mut anunciado = false;
    move |p| {
        if json || p.phase == scan::Phase::Listing || p.bytes_total == 0 {
            return;
        }
        if !anunciado {
            anunciado = true;
            let que = if p.phase == scan::Phase::Hashing { "Reading" } else { "Storing" };
            let _ = writeln!(out, "{que} {} {} ({})", p.files_total, if p.files_total == 1 { "file" } else { "files" }, crate::tam(p.bytes_total, "en"));
        }
        if let Some(linea) = cuenta.linea(p.bytes_done, Some(p.bytes_total)) {
            let _ = writeln!(out, "{linea}");
        }
    }
}

/// Lo que se va a bajar, en cuanto se sabe.
fn anunciar(plan: &Plan, out: &mut dyn Write) {
    let peso = plan.size.map(|s| format!(", {}", crate::tam(s, "en"))).unwrap_or_default();
    let _ = writeln!(out, "Downloading {} ({}{peso})", plan.asset, plan.version);
}

/// Cómo va la descarga, en líneas: una cada 10 % o, si no se sabe el total, cada 32 MB.
#[derive(Default)]
struct Cuenta {
    ultimo: u64,
}

impl Cuenta {
    fn linea(&mut self, bajado: u64, total: Option<u64>) -> Option<String> {
        let (paso, linea) = match total {
            Some(total) if total > 0 => {
                let paso = (bajado.saturating_mul(10) / total).min(10);
                (paso, format!("  {:3}%  {} of {}", paso * 10, crate::tam(bajado, "en"), crate::tam(total, "en")))
            }
            _ => (bajado >> 25, format!("  {}", crate::tam(bajado, "en"))),
        };
        (paso > self.ultimo).then(|| {
            self.ultimo = paso;
            linea
        })
    }
}

/// `launch`: lo que hace Jugar. No escribe nada si sale bien.
fn abrir(ent: &Entorno, juegos: &[Juego], id: &str, sin_esperar: bool, extra: &[OsString]) -> Result<(), String> {
    let (p, (dir, recibo)) = instalado(juegos, id)?;
    let receta = receta(p)?;
    let mut abierto = trabajo::abrir(&ent.biblioteca, receta, dir, extra).map_err(|e| ingles(&e))?;
    // Como en la ventana: se apunta que se abrió y, mientras esté abierto, cuánto se juega. Sin
    // esperar no hay quien cuente: solo queda apuntado que se abrió.
    let clave = tiempo::clave(&p.e.entry, &recibo.version);
    tiempo::abierto(&ent.raiz, &clave);
    if !sin_esperar {
        trabajo::contar_tiempo(&mut abierto.hijo, &ent.raiz, &clave);
    }
    Ok(())
}

fn canal(c: Channel) -> &'static str {
    match c {
        Channel::Stable => "stable",
        Channel::Untested => "untested",
        Channel::Nightly => "nightly",
    }
}

/// Los errores del núcleo en inglés (su texto propio está en español, para los registros). Sin
/// comodín: una variante nueva no compila hasta que tenga su frase.
fn ingles(e: &Error) -> String {
    let p = |x: &std::path::Path| x.display().to_string();
    match e {
        Error::Io { path, source } => format!("could not use {}: {source}", p(path)),
        Error::Archive { path, reason } => format!("could not unpack {}: {reason}", p(path)),
        Error::UnsafePath(s) => format!("the archive has a path that is not allowed: {s}"),
        Error::TooLarge { limit } => format!("it is larger than the limit of {limit} bytes"),
        Error::TooManyFiles { limit } => format!("the archive has more than {limit} files"),
        Error::ChecksumMismatch { algo, expected, got } => format!("the download does not match: expected {algo} {expected} and got {got}"),
        Error::BadLink(s) => format!("invalid link: {s}"),
        Error::Network { url, reason } => format!("network error at {url}: {reason}"),
        Error::Json(e) => format!("invalid JSON: {e}"),
        Error::BadRecipe(s) => format!("the install recipe has an error (it is our mistake, not yours): {s}"),
        Error::NoPlatform { entry, platform } => format!("{entry} has no version for {platform}"),
        Error::NoRelease { repo, channel } => format!("{repo} has no release in the {} channel", canal(*channel)),
        Error::NoSuchRelease { repo, tag } => format!("{repo} has no release with the tag {tag} (see `unricopie versions`)"),
        Error::NoAsset { tag, pattern } => format!("release {tag} has no file matching \"{pattern}\""),
        Error::AmbiguousAsset { tag, pattern, matches } => format!("release {tag} has several files matching \"{pattern}\": {}", matches.join(", ")),
        Error::RunNotFound { run } => format!("{run} is missing after installing"),
        Error::AlreadyInstalled(x) => format!("there is already something installed in {}", p(x)),
        Error::UnsupportedPackage { asset, kind } => format!("Unricopie cannot install {asset} yet ({kind})"),
        Error::PackageHasReceipt(asset) => format!("{asset} already has an unricopie.json inside"),
        Error::PackageLayout(s) => format!("the package has an unexpected layout: {s}"),
        Error::DmgOnlyMac(x) => format!("dmg files can only be opened on macOS: {}", p(x)),
        Error::NoSpace { path, needed, free } => {
            format!("not enough disk space in {}: {} needed, {} free", p(path), crate::tam(*needed, "en"), crate::tam(*free, "en"))
        }
        Error::Threat { file, threat } if threat.is_empty() => format!("the antivirus of this computer found a threat in {file}: the download was deleted and nothing was installed"),
        Error::Threat { file, threat } => format!("the antivirus of this computer found {threat} in {file}: the download was deleted and nothing was installed"),
        Error::BadRepo(s) => format!("`{s}` is not a GitHub repository: write owner/repo or its github.com address"),
        Error::NoSuchRepo(repo) => format!("GitHub has no public repository {repo}: check the spelling"),
        Error::ChooseRun { candidates } if candidates.is_empty() => "it has no program to start".into(),
        Error::ChooseRun { candidates } => format!("it is not clear which of its programs starts the game: {}", candidates.join(", ")),
        Error::UnknownSystem => "there are no versions for this system yet".into(),
        Error::NoDataDir => "the system's data folder could not be found".into(),
        Error::NotInstalled(x) => format!("there is nothing installed in {}", p(x)),
        Error::NoReceipt(x) => format!("what is in {} was not installed by Unricopie (it has no unricopie.json)", p(x)),
        Error::OtherGame { dir, entry } => format!("{} holds {entry}, which is another game", p(dir)),
        Error::UpToDate(v) => format!("version {v} is already installed"),
        Error::GameRunning(x) => format!("the game in {} is running: close it first", p(x)),
        Error::NoPrevious { dir, id } => format!("there is no previous version \"{id}\" of {}", p(dir)),
        Error::NotInLibrary(x) => format!("{} is not a game of the library", p(x)),
        Error::NotPortable { entry, platform } => format!("the recipe of {entry} does not say how to make it portable on {platform}"),
        Error::InTheWay(x) => format!("could not create {}: something else already has that name", p(x)),
        Error::Config { path, reason } => format!("could not use the settings file {}: {reason}", p(path)),
        Error::ConfigNotUtf8(x) => format!("the settings file {} is not UTF-8", p(x)),
        Error::ConfigShape { path, key } if key.is_empty() => format!("the settings file {} is not an object or a table", p(path)),
        Error::ConfigShape { path, key } => format!("in the settings file {}, \"{key}\" is not a section", p(path)),
        Error::ConfigValue(x) => format!("the value does not fit in the settings file {}", p(x)),
        Error::CannotLaunch(x, razon) => format!("could not start {}: {razon}", p(x)),
        Error::NoRunInRecipe(x) => format!("the recipe does not say which program to start in {}", p(x)),
        Error::MacAppNotYet(x) => format!("could not find the program inside the macOS app {}", p(x)),
        Error::MissingLibraries { program, libs, hint } if hint.is_empty() => {
            format!("{} needs system libraries that are missing: {}", p(program), libs.join(", "))
        }
        Error::MissingLibraries { program, libs, hint } => {
            format!("{} needs system libraries that are missing: {}. Install them with: {hint}", p(program), libs.join(", "))
        }
        Error::MissingRuntime { program, libs, url } => {
            format!("{} needs the Visual C++ libraries, which are missing: {}. Install them with Microsoft's installer: {url}", p(program), libs.join(", "))
        }
        Error::MissingVulkan { program } => format!("{} needs Vulkan (vulkan-1.dll), which is missing: updating the graphics driver installs it", p(program)),
        Error::RunMissing(x) => format!("{} is missing", p(x)),
        Error::CopyMismatch(s) => format!("your copy is none of the ones {s} accepts"),
        Error::CopyIncomplete(s) => format!("your copy is missing {s}"),
        Error::NotARom(x) => format!("{} is not an N64 ROM", p(x)),
        Error::CopyWantsFile(s) => format!("{s} asks for a file, not a folder"),
        Error::CopyWantsFolder(s) => format!("{s} asks for the game's folder"),
        Error::Flagged { url } => format!("the site's antivirus flagged {url}"),
        Error::NotAMod(s) => format!("{s} has nothing the game can use as a mod"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use unricopie_nucleo::net::Downloaded;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn orden(args: &[&str]) -> Orden {
        leer(os(args)).unwrap().orden
    }

    #[test]
    fn lee_las_ordenes() {
        assert_eq!(orden(&["list"]), Orden::Lista { instalados: false, json: false });
        assert_eq!(orden(&["list", "--json", "--installed"]), Orden::Lista { instalados: true, json: true });
        assert_eq!(orden(&["install", "recomps/zelda-majoras-mask"]), Orden::Instalar { id: "recomps/zelda-majoras-mask".into(), version: None });
        assert_eq!(orden(&["update", "zelda-majoras-mask"]), Orden::Actualizar { id: "zelda-majoras-mask".into(), version: None });
        // Detrás de `install` y `update`, `--version` es la del juego: antes o después del id.
        assert_eq!(orden(&["install", "x", "--version", "v1.2.1"]), Orden::Instalar { id: "x".into(), version: Some("v1.2.1".into()) });
        assert_eq!(orden(&["--data-dir", "d", "update", "--version=v1.0", "x"]), Orden::Actualizar { id: "x".into(), version: Some("v1.0".into()) });
        assert_eq!(orden(&["update", "--version", "-raro", "x"]), Orden::Actualizar { id: "x".into(), version: Some("-raro".into()) });
        assert_eq!(orden(&["versions", "x", "--json"]), Orden::Versiones { id: "x".into(), json: true });
        assert_eq!(orden(&["copies", "D:\\Mis ROM"]), Orden::Copias { carpeta: PathBuf::from("D:\\Mis ROM"), simular: false, json: false });
        assert_eq!(orden(&["copies", "--dry-run", "roms", "--json"]), Orden::Copias { carpeta: PathBuf::from("roms"), simular: true, json: true });
        assert_eq!(orden(&["path", "x"]), Orden::Ruta("x".into()));
        assert_eq!(orden(&["add", "EXOK/Celeste64"]), Orden::Anadir { repo: "EXOK/Celeste64".into(), archivo: None, programa: None, json: false });
        assert_eq!(
            orden(&["add", "--asset", "Juego 1.0.zip", "https://github.com/a/b", "--run=bin/juego.exe", "--json"]),
            Orden::Anadir { repo: "https://github.com/a/b".into(), archivo: Some("Juego 1.0.zip".into()), programa: Some("bin/juego.exe".into()), json: true }
        );
        assert_eq!(orden(&["add", "a/b", "--asset=x.zip", "--run", "-raro.exe"]), Orden::Anadir { repo: "a/b".into(), archivo: Some("x.zip".into()), programa: Some("-raro.exe".into()), json: false });
        assert_eq!(orden(&["remove-added", "added/a-b"]), Orden::QuitarAnadido("added/a-b".into()));
        assert_eq!(orden(&["launch", "x"]), Orden::Abrir { id: "x".into(), sin_esperar: false, extra: vec![] });
        // Lo de detrás de `--` es del juego, aunque parezca una opción nuestra.
        assert_eq!(orden(&["launch", "--no-wait", "x", "--", "--help", "-w", "a b"]), Orden::Abrir { id: "x".into(), sin_esperar: true, extra: os(&["--help", "-w", "a b"]) });
        for ayuda in [&["--help"][..], &["-h"], &["help"], &["launch", "x", "--help"], &["nada", "-h"]] {
            assert_eq!(orden(ayuda), Orden::Ayuda);
        }
        assert_eq!(orden(&["--version"]), Orden::Version);
        assert_eq!(orden(&["-V"]), Orden::Version);
    }

    #[test]
    fn lee_las_opciones_de_todas_las_ordenes() {
        let a = leer(os(&["--data-dir", "D:\\Juegos", "list"])).unwrap();
        assert_eq!((a.data_dir, a.portable), (Some(PathBuf::from("D:\\Juegos")), false));
        // Valen también detrás de la orden y con `=`.
        let a = leer(os(&["path", "x", "--data-dir=otra carpeta"])).unwrap();
        assert_eq!(a.data_dir, Some(PathBuf::from("otra carpeta")));
        let a = leer(os(&["launch", "--portable", "x"])).unwrap();
        assert!(a.portable && a.data_dir.is_none());
    }

    #[test]
    fn rechaza_lo_mal_escrito() {
        for mal in [
            &["instalar", "x"][..],
            &["install"],
            &["install", ""],
            &["install", "a", "b"],
            &["list", "--no-wait"],
            &["launch", "x", "--json"],
            &["path", "x", "--", "y"],
            &["list", "--todo"],
            &["--portable"],
            &["--data-dir"],
            &["list", "--data-dir="],
            &["list", "--data-dir", "x", "--portable"],
            &["install", "x", "--version"],
            &["install", "x", "--version="],
            &["list", "--version=v1"],
            &["path", "x", "--dry-run"],
            &["versions"],
            &["copies"],
            &["copies", "a", "b"],
            &["add"],
            &["add", "a/b", "c/d"],
            &["add", "a/b", "--asset"],
            &["add", "a/b", "--run="],
            &["add", "a/b", "--installed"],
            &["install", "x", "--asset", "y.zip"],
            &["list", "--run", "x.exe"],
            &["remove-added"],
            &["remove-added", "x", "--json"],
        ] {
            assert!(leer(os(mal)).is_err(), "{mal:?}");
        }
    }

    #[test]
    fn cuenta_la_descarga_de_diez_en_diez() {
        let mut c = Cuenta::default();
        let total = Some(200 << 20);
        assert_eq!(c.linea(1 << 20, total), None);
        assert_eq!(c.linea(20 << 20, total).as_deref(), Some("   10%  20 MB of 200 MB"));
        assert_eq!(c.linea(25 << 20, total), None);
        assert_eq!(c.linea(200 << 20, total).as_deref(), Some("  100%  200 MB of 200 MB"));
        // Sin total, cada 32 MB.
        let mut c = Cuenta::default();
        assert_eq!(c.linea(31 << 20, None), None);
        assert_eq!(c.linea(33 << 20, None).as_deref(), Some("  33 MB"));
    }

    // --- Una biblioteca de prueba, con un «juego» que es un script ---

    const ENTRY: &str = "ports/juego-de-prueba";

    /// El archivo que se «baja» y se abre: un script que apunta en su carpeta con qué
    /// argumentos y con qué variable se abrió.
    fn programa() -> (&'static str, &'static str) {
        if cfg!(windows) {
            ("juego.cmd", "@echo off\r\n>argumentos.txt echo %*\r\n>variable.txt echo %UNRICOPIE_PRUEBA%\r\n")
        } else {
            ("juego.sh", "#!/bin/sh\necho \"$@\" > argumentos.txt\necho \"$UNRICOPIE_PRUEBA\" > variable.txt\n")
        }
    }

    fn receta_de_prueba() -> serde_json::Value {
        let plataforma = Platform::current().expect("las pruebas necesitan una plataforma conocida").as_str();
        let (archivo, _) = programa();
        serde_json::json!({
            "entry": ENTRY,
            "repo": "prueba/juego-de-prueba",
            "release": {
                "assets": { plataforma: archivo },
                "type": { plataforma: "portable" },
                "run": { plataforma: archivo },
                "after-download": { plataforma: ["chmod-x"] }
            },
            "portable": { plataforma: { "files": { "portable.txt": "" }, "args": ["--portable"], "env": { "UNRICOPIE_PRUEBA": "{game}" } } },
            "copy": { "how": "none" },
            "checked": { "date": "2026-11-20", "release": "v1", "sources": ["prueba"] }
        })
    }

    fn ficha(entry: &str, juego: &str, nombre: &str, proyecto: &str) -> serde_json::Value {
        serde_json::json!({
            "entry": entry, "juego": juego, "nombre": nombre, "proyecto": proyecto, "repo": format!("prueba/{proyecto}"),
            "consola": "pc", "consola_nombre": "PC", "color": "#000000", "tinta": "#ffffff"
        })
    }

    /// Una raíz con el catálogo y las recetas guardados (`<raíz>/datos/`), como los deja la
    /// ventana: la ficha de prueba, otra con receta solo para una plataforma que no existe aquí
    /// y dos sin receta que comparten lo de después de la barra.
    fn entorno(raiz: &Path, portable: bool) -> Entorno {
        let datos = raiz.join("datos");
        fs::create_dir_all(&datos).unwrap();
        let catalogo = serde_json::json!({ "formato": 1, "entradas": [
            ficha(ENTRY, "juego-de-prueba", "Juego de prueba", "Port de prueba"),
            ficha("ports/de-otro-sistema", "de-otro-sistema", "De otro sistema", "De otro sistema"),
            ficha("ports/repetido", "repetido", "Repetido", "Repetido (port)"),
            ficha("recomps/repetido", "repetido", "Repetido", "Repetido (recomp)"),
        ]});
        let otra = if cfg!(windows) { "macos" } else { "windows-x64" };
        let mut de_otro = receta_de_prueba();
        de_otro["entry"] = "ports/de-otro-sistema".into();
        de_otro["release"] = serde_json::json!({ "assets": { otra: "x.zip" }, "run": { otra: "x" } });
        de_otro.as_object_mut().unwrap().remove("portable");
        let recetas = serde_json::json!({ "formato": 1, "recetas": { ENTRY: receta_de_prueba(), "ports/de-otro-sistema": de_otro } });
        fs::write(datos.join("catalogo.json"), catalogo.to_string()).unwrap();
        fs::write(datos.join("recetas.json"), recetas.to_string()).unwrap();
        Entorno { raiz: raiz.to_path_buf(), portable, biblioteca: Library::new(raiz) }
    }

    /// El catálogo guardado, sin red y sin mirar `UNRICOPIE_CATALOGO`.
    fn juegos(ent: &Entorno) -> Vec<Juego> {
        let (entradas, origen, _) = datos::cargar(&ent.raiz).unwrap();
        assert_eq!(origen, datos::Origen::Biblioteca);
        catalogo::juntar(entradas, &ent.biblioteca).0
    }

    /// El plan de instalar la versión `version`, con una release inventada, y el archivo «bajado».
    fn bajado(ent: &Entorno, version: &str) -> (Recipe, Plan, PathBuf, Downloaded) {
        let (archivo, contenido) = programa();
        let receta = Recipe::from_json(&receta_de_prueba().to_string()).unwrap();
        let releases: Vec<GhRelease> = serde_json::from_value(serde_json::json!([{
            "tag_name": version, "draft": false, "prerelease": false, "published_at": "2026-11-20T00:00:00Z",
            "assets": [{ "name": archivo, "size": contenido.len(), "browser_download_url": format!("https://example.invalid/{version}/{archivo}"), "digest": null }]
        }]))
        .unwrap();
        let plan = install::installer::plan_from(&receta, None, Platform::current().unwrap(), Channel::Untested, None, &releases, &ent.biblioteca).unwrap();
        fs::create_dir_all(&ent.biblioteca.downloads).unwrap();
        let ruta = ent.biblioteca.downloads.join(archivo);
        fs::write(&ruta, contenido).unwrap();
        (receta, plan, ruta, Downloaded { size: contenido.len() as u64, sha256: format!("sha-{version}"), scan: None })
    }

    /// Lo que hace `install` después de bajar: instalar y, con el launcher portable, hacerlo portable.
    fn instalar_falso(ent: &Entorno, version: &str) -> PathBuf {
        let (receta, plan, ruta, got) = bajado(ent, version);
        install::installer::install_downloaded(&ent.biblioteca, &plan, &ruta, &got).unwrap();
        assert!(trabajo::hacer_portable(ent.portable, &plan, &receta).is_none());
        plan.dest
    }

    /// Ejecuta una orden y devuelve el código, la salida y los errores.
    fn con(ent: &Entorno, args: &[&str]) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let codigo = match ejecutar(&orden(args), ent, &juegos(ent), &mut out, &mut err) {
            Ok(()) => 0,
            Err(e) => {
                err.extend(e.as_bytes());
                1
            }
        };
        (codigo, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn lista_lo_que_se_puede_instalar_y_lo_instalado() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        // Solo lo que tiene receta para este sistema.
        assert_eq!(con(&ent, &["list"]), (0, format!("{ENTRY}  -  Juego de prueba (Port de prueba)\n"), String::new()));
        assert_eq!(con(&ent, &["list", "--installed"]), (0, String::new(), String::new()));
        assert_eq!(con(&ent, &["list", "--installed", "--json"]).1, "[]\n");

        let dir = instalar_falso(&ent, "v1.0");
        assert_eq!(con(&ent, &["list", "--installed"]).1, format!("{ENTRY}  v1.0  Juego de prueba (Port de prueba)\n"));
        let (_, json, _) = con(&ent, &["list", "--json"]);
        let filas: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            filas,
serde_json::json!([{ "id": ENTRY, "name": "Juego de prueba", "project": "Port de prueba", "installed": true, "version": "v1.0", "path": dir.display().to_string(), "scan": null, "added": false }])
        );
        // Con el análisis del antivirus apuntado en el recibo, sale tal cual.
        let mut recibo = install::Receipt::read(&dir).unwrap().unwrap();
        recibo.scan = Some(Scan::Clean { scanner: "Microsoft Defender".into() });
        recibo.write(&dir).unwrap();
        let filas: serde_json::Value = serde_json::from_str(&con(&ent, &["list", "--json"]).1).unwrap();
        assert_eq!(filas[0]["scan"], serde_json::json!({ "state": "clean", "scanner": "Microsoft Defender" }));
        assert_eq!(analisis_de(&dir), "Scanned with Microsoft Defender: no threats found");
        recibo.scan = Some(Scan::NotScanned { reason: Reason::Disabled });
        recibo.write(&dir).unwrap();
        let filas: serde_json::Value = serde_json::from_str(&con(&ent, &["list", "--json"]).1).unwrap();
        assert_eq!(filas[0]["scan"], serde_json::json!({ "state": "not-scanned", "reason": "disabled" }));
        assert_eq!(analisis_de(&dir), "Not scanned by an antivirus: Microsoft Defender is turned off");
        assert_eq!(analisis(Some(&Scan::NotScanned { reason: Reason::NoScanner })), "Not scanned by an antivirus: there is no scanner on this system");
    }

    #[test]
    fn path_dice_donde_esta_y_falla_si_no_lo_esta() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let (codigo, out, err) = con(&ent, &["path", ENTRY]);
        assert_eq!((codigo, out.as_str()), (1, ""));
        assert!(err.contains("is not installed") && err.contains("unricopie install"), "{err}");
        // Un id que no existe, y uno corto que comparten dos fichas.
        let (codigo, _, err) = con(&ent, &["path", "no-existe"]);
        assert!(codigo == 1 && err.contains("no project with the id `no-existe`"), "{err}");
        let (codigo, _, err) = con(&ent, &["path", "repetido"]);
        assert!(codigo == 1 && err.contains("ports/repetido or recomps/repetido"), "{err}");

        let dir = instalar_falso(&ent, "v1.0");
        assert_eq!(dir, t.path().join("games").join("juego-de-prueba"));
        // Con la ficha entera o con lo de después de la barra.
        for id in [ENTRY, "juego-de-prueba"] {
            assert_eq!(con(&ent, &["path", id]), (0, format!("{}\n", dir.display()), String::new()));
        }
    }

    #[test]
    fn install_y_update_fallan_antes_de_usar_la_red() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let (codigo, _, err) = con(&ent, &["install", "no-existe"]);
        assert!(codigo == 1 && err.contains("no project"), "{err}");
        let (codigo, _, err) = con(&ent, &["install", "ports/repetido"]);
        assert!(codigo == 1 && err.contains("does not know how to install ports/repetido"), "{err}");
        let (codigo, _, err) = con(&ent, &["install", "de-otro-sistema"]);
        assert!(codigo == 1 && err.contains("cannot install ports/de-otro-sistema on this system"), "{err}");
        let (codigo, _, err) = con(&ent, &["update", ENTRY]);
        assert!(codigo == 1 && err.contains("is not installed"), "{err}");

        instalar_falso(&ent, "v1.0");
        let (codigo, out, err) = con(&ent, &["install", ENTRY]);
        assert!(codigo == 1 && out.is_empty() && err.contains("already installed (v1.0)"), "{err}");
    }

    /// Espera a que el script deje su archivo (con `--no-wait` puede no haber terminado).
    fn leer_cuando_este(archivo: &Path) -> String {
        for _ in 0..200 {
            if let Ok(t) = fs::read_to_string(archivo) {
                if t.ends_with('\n') {
                    return t.trim().to_string();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no aparece {}", archivo.display());
    }

    #[test]
    fn launch_abre_el_juego_en_su_carpeta_y_apunta_el_tiempo() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let (codigo, _, err) = con(&ent, &["launch", ENTRY]);
        assert!(codigo == 1 && err.contains("is not installed"), "{err}");
        assert!(tiempo::leer(t.path()).is_empty());

        let dir = instalar_falso(&ent, "v1.0");
        // Espera a que se cierre: al volver, el script ya escribió, y en la carpeta del juego.
        assert_eq!(con(&ent, &["launch", ENTRY, "--", "--nivel", "3"]), (0, String::new(), String::new()));
        assert_eq!(fs::read_to_string(dir.join("argumentos.txt")).unwrap().trim(), "--nivel 3");
        // Queda apuntado que esa versión se abrió, con la misma clave que usa la ventana.
        let tiempos = tiempo::leer(t.path());
        assert_eq!(tiempos.keys().collect::<Vec<_>>(), [&tiempo::clave(ENTRY, "v1.0")]);
        assert!(tiempos[&tiempo::clave(ENTRY, "v1.0")].primera > 0);

        // Sin esperar también se abre y se apunta.
        fs::remove_file(dir.join("argumentos.txt")).unwrap();
        assert_eq!(con(&ent, &["launch", "juego-de-prueba", "--no-wait"]), (0, String::new(), String::new()));
        assert_eq!(leer_cuando_este(&dir.join("argumentos.txt")), if cfg!(windows) { "ECHO is off." } else { "" });
        leer_cuando_este(&dir.join("variable.txt"));
    }

    #[test]
    fn launch_usa_lo_del_modo_portable_de_la_receta() {
        let t = tempfile::tempdir().unwrap();
        // Con el launcher portable, lo instalado se hace portable, como en la ventana.
        let ent = entorno(t.path(), true);
        let dir = instalar_falso(&ent, "v1.0");
        assert!(dir.join("portable.txt").is_file());
        assert_eq!(con(&ent, &["launch", ENTRY, "--", "mio"]).0, 0);
        // Primero los argumentos de la receta y después los de quien llama.
        assert_eq!(fs::read_to_string(dir.join("argumentos.txt")).unwrap().trim(), "--portable mio");
        assert_eq!(fs::read_to_string(dir.join("variable.txt")).unwrap().trim(), dir.display().to_string());
    }

    #[test]
    fn launch_cuenta_si_no_se_puede_abrir() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let dir = instalar_falso(&ent, "v1.0");
        fs::remove_file(dir.join(programa().0)).unwrap();
        let (codigo, out, err) = con(&ent, &["launch", ENTRY]);
        assert!(codigo == 1 && out.is_empty() && err.contains("is missing"), "{err}");
        assert!(tiempo::leer(t.path()).is_empty());
    }

    #[test]
    fn update_aplica_la_version_bajada_y_guarda_la_anterior() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let dir = instalar_falso(&ent, "v1.0");
        fs::write(dir.join("partida.sav"), "mi partida").unwrap();
        // Lo que hace `update` después de bajar (sin mods).
        let (_, plan, ruta, got) = bajado(&ent, "v1.1");
        trabajo::aplicar_version(&ent.biblioteca, &plan, &ruta, &got, None).unwrap();
        assert!(!ruta.exists());
        assert_eq!(con(&ent, &["list", "--installed"]).1, format!("{ENTRY}  v1.1  Juego de prueba (Port de prueba)\n"));
        assert_eq!(fs::read_to_string(dir.join("partida.sav")).unwrap(), "mi partida");
        assert_eq!(unricopie_nucleo::install::update::previous_versions(&ent.biblioteca, &dir).unwrap().len(), 1);
        // Otra vez la misma: ya está al día.
        let (_, plan, ruta, got) = bajado(&ent, "v1.1");
        assert!(matches!(trabajo::aplicar_version(&ent.biblioteca, &plan, &ruta, &got, None), Err(Error::UpToDate(v)) if v == "v1.1"));
    }

    #[test]
    fn versions_lista_y_marca_la_instalada() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let v = |tag: &str, fecha: &str, prerelease: bool, size: Option<u64>| Version { tag: tag.into(), published_at: Some(fecha.into()), prerelease, asset: format!("juego-{tag}.zip"), size };
        let versiones = [v("v1.2-beta", "2026-11-19T10:00:00Z", true, Some(31 << 20)), v("v1.1", "2026-10-02T08:30:00Z", false, Some(30 << 20)), v("v1.0", "2026-09-01T00:00:00Z", false, None)];
        let texto = |ent: &Entorno, vs: &[Version], json: bool| {
            let js = juegos(ent);
            let mut out = Vec::new();
            lista_de_versiones(buscar(&js, ENTRY).unwrap(), vs, json, &mut out);
            String::from_utf8(out).unwrap()
        };
        // Sin instalar: ninguna marcada.
        assert_eq!(texto(&ent, &versiones, false), "  v1.2-beta  2026-11-19     31 MB  prerelease\n  v1.1       2026-10-02     30 MB  latest\n  v1.0       2026-09-01         -\n");
        instalar_falso(&ent, "v1.0");
        assert_eq!(texto(&ent, &versiones, false).lines().last(), Some("* v1.0       2026-09-01         -  installed"));
        let filas: serde_json::Value = serde_json::from_str(&texto(&ent, &versiones, true)).unwrap();
        assert_eq!(filas[1], serde_json::json!({ "tag": "v1.1", "date": "2026-10-02T08:30:00Z", "prerelease": false, "asset": "juego-v1.1.zip", "size": 30 << 20, "latest": true, "installed": false }));
        assert_eq!((&filas[2]["installed"], &filas[2]["size"], &filas[0]["latest"]), (&true.into(), &serde_json::Value::Null, &false.into()));
        // La instalada ya no sale entre las últimas: se dice igual.
        assert_eq!(texto(&ent, &versiones[..2], false).lines().last(), Some("* v1.0 is installed; it is not among the latest releases"));
        assert!(texto(&ent, &[], false).starts_with("None of the latest releases of prueba/Port de prueba has a file for this system\n"));
    }

    #[test]
    fn install_con_version_sobre_lo_instalado_manda_a_update() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        instalar_falso(&ent, "v1.1");
        let (codigo, _, err) = con(&ent, &["install", ENTRY, "--version", "v1.0"]);
        assert!(codigo == 1 && err.contains("already installed (v1.1)") && err.contains(&format!("unricopie update {ENTRY} --version v1.0")), "{err}");
    }

    /// Lo que hace `update --version` después de bajar: el plan de esa etiqueta, por el camino
    /// de actualizar.
    #[test]
    fn update_a_una_version_anterior_guarda_la_que_habia() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let dir = instalar_falso(&ent, "v1.1");
        fs::write(dir.join("partida.sav"), "mi partida").unwrap();
        let (_, plan, ruta, got) = bajado(&ent, "v1.0");
        trabajo::aplicar_version(&ent.biblioteca, &plan, &ruta, &got, None).unwrap();
        assert_eq!(con(&ent, &["list", "--installed"]).1, format!("{ENTRY}  v1.0  Juego de prueba (Port de prueba)\n"));
        assert_eq!(fs::read_to_string(dir.join("partida.sav")).unwrap(), "mi partida");
        let anteriores = unricopie_nucleo::install::update::previous_versions(&ent.biblioteca, &dir).unwrap();
        assert_eq!(anteriores.iter().map(|a| a.receipt.version.as_str()).collect::<Vec<_>>(), ["v1.1"]);
        assert_eq!(fs::read_to_string(anteriores[0].path.join("partida.sav")).unwrap(), "mi partida");
    }

    const ROM: &[u8] = b"una ROM de mentira para las pruebas";

    /// `entorno`, con una receta que pide una copia: un archivo con los hashes de `ROM`.
    fn entorno_con_copia(raiz: &Path) -> Entorno {
        use unricopie_nucleo::install::copy::hash_bytes;
        use unricopie_nucleo::install::recipe::HashAlgo;
        let ent = entorno(raiz, false);
        let archivo = raiz.join("datos").join("recetas.json");
        let mut recetas: serde_json::Value = serde_json::from_str(&fs::read_to_string(&archivo).unwrap()).unwrap();
        let sha1 = hash_bytes(ROM, &[HashAlgo::Sha1])[&HashAlgo::Sha1].clone();
        recetas["recetas"][ENTRY]["copy"] = serde_json::json!({ "how": "in-app", "kind": "file", "accepts": [{ "name": "Europa", "platform": "gba", "hash": { "sha1": sha1 } }] });
        fs::write(&archivo, recetas.to_string()).unwrap();
        ent
    }

    fn copies(ent: &Entorno, carpeta: &Path, simular: bool, json: bool) -> (Result<(), String>, String) {
        let mut out = Vec::new();
        let r = copias(ent, &juegos(ent), carpeta, simular, json, None, &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn copies_busca_en_la_carpeta_y_guarda_lo_que_coincide() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno_con_copia(&t.path().join("biblioteca"));
        let roms = t.path().join("roms");
        let buena = roms.join("GBA").join("juego.gba");
        fs::create_dir_all(buena.parent().unwrap()).unwrap();
        fs::write(&buena, ROM).unwrap();
        fs::write(roms.join("GBA").join("otro.gba"), b"otra cosa").unwrap();
        fs::write(roms.join("juego.txt"), ROM).unwrap();
        let receta = Recipe::from_json(&receta_de_prueba().to_string()).unwrap();
        let guardada = || unricopie_nucleo::install::copy::imported(&ent.biblioteca, &receta, None);

        // En seco: lo dice y no guarda nada.
        let (r, out) = copies(&ent, &roms, true, false);
        assert_eq!(r, Ok(()));
        assert!(out.starts_with(&format!("Looking in {} for your copies of the games\nReading 2 files (0.0 MB)\n", roms.display())), "{out}");
        assert!(out.ends_with(&format!("  100%  0.0 MB of 0.0 MB\nFound Juego de prueba (Port de prueba): Europa\n    {} (0.0 MB)\nDry run: nothing was stored\n", buena.display())), "{out}");
        assert!(guardada().is_none());
        let (_, json) = copies(&ent, &roms, true, true);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!((&v["dry_run"], &v["files"], &v["searched"]), (&true.into(), &2.into(), &1.into()));
        assert_eq!(
            v["matches"],
            serde_json::json!([{ "id": ENTRY, "name": "Juego de prueba (Port de prueba)", "copy": "Europa", "path": buena.display().to_string(), "size": ROM.len(), "result": "would-import", "error": null }])
        );

        // De verdad: queda guardada y comprobada, y el archivo del jugador sigue donde estaba.
        let (r, out) = copies(&ent, &roms, false, false);
        assert_eq!(r, Ok(()));
        assert!(out.contains("Stored Juego de prueba (Port de prueba): Europa\n") && !out.contains("Dry run"), "{out}");
        let copia = guardada().unwrap();
        assert!(copia.verified && copia.accepted == ["Europa"] && copia.source == buena);
        assert_eq!(fs::read(&buena).unwrap(), ROM);

        // Otra vez: ya la tiene, así que ni se busca.
        let (r, out) = copies(&ent, &roms, false, false);
        assert_eq!(r, Ok(()));
        assert!(out.contains("No copy found: 0 files read, looking for the copies of 0 games\n"), "{out}");
        assert!(out.contains("Not searched: 1 already have a verified copy in the library\n"), "{out}");
        let (_, json) = copies(&ent, &roms, false, true);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["skipped"], serde_json::json!([{ "id": ENTRY, "name": "Juego de prueba (Port de prueba)", "reason": "already-imported", "detail": "Europa" }]));

        // Una carpeta que no existe es un error.
        let (r, _) = copies(&ent, &t.path().join("no-existe"), true, false);
        assert!(r.is_err_and(|e| e.starts_with("could not use ")));
    }

    /// Una release inventada de un repo, con esos archivos.
    fn release_de(tag: &str, nombres: &[&str]) -> Vec<GhRelease> {
        let assets: Vec<serde_json::Value> = nombres.iter().map(|n| serde_json::json!({ "name": n, "size": 3, "browser_download_url": format!("https://example.invalid/{tag}/{n}"), "digest": null })).collect();
        serde_json::from_value(serde_json::json!([{ "tag_name": tag, "draft": false, "prerelease": false, "published_at": "2026-11-20T00:00:00Z", "assets": assets }])).unwrap()
    }

    /// Los nombres de una release con un archivo para este sistema y otro para otro.
    fn archivos_de_aqui() -> (&'static str, &'static str) {
        match std::env::consts::OS {
            "windows" => ("Juego-v1.0-windows.zip", "Juego-v1.0-linux.zip"),
            "macos" => ("Juego-v1.0-macos.zip", "Juego-v1.0-windows.zip"),
            _ => ("Juego-v1.0-linux.zip", "Juego-v1.0-windows.zip"),
        }
    }

    #[test]
    fn add_propone_o_deja_elegir_sin_adivinar() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let js = juegos(&ent);
        let host = Platform::current().unwrap();
        let (aqui, otro) = archivos_de_aqui();
        let probar = |nombres: &[&str], archivo: Option<&str>, json: bool| {
            let mut out = Vec::new();
            let r = proponer(&js, "Autor/Juego", &release_de("v1.0", nombres), host, archivo, None, json, &mut out);
            (r, String::from_utf8(out).unwrap())
        };
        // Un solo archivo para este sistema: sin preguntar, y sin escribir nada todavía.
        let (r, out) = probar(&[aqui, otro, "Juego-v1.0.msi"], None, false);
        let p = r.unwrap();
        assert_eq!((p.recipe.entry.as_str(), p.asset.as_str(), p.tag.as_str(), out.as_str()), ("added/autor-juego", aqui, "v1.0", ""));
        assert!(p.recipe.by_hand);
        // Dos que no dicen de qué sistema son: se listan, con lo que no se ofrece y por qué.
        let dudosos = ["Juego-v1.0-a.zip", "Juego-v1.0-b.zip", otro, "Juego-v1.0.msi", "SHA256SUMS.txt"];
        let (r, out) = probar(&dudosos, None, false);
        assert_eq!(r.unwrap_err(), "choose the file with `unricopie add Autor/Juego --asset <name>`");
        assert_eq!(
            out,
            format!(
                "Release v1.0 of Autor/Juego has no single file that says it is for this system. It could be:\n  Juego-v1.0-a.zip\n  Juego-v1.0-b.zip\nNot offered:\n  {otro} (for another system)\n  Juego-v1.0.msi ({})\n  SHA256SUMS.txt (not a package Unricopie can unpack)\n",
                if cfg!(windows) { "an installer: Unricopie does not run installers" } else { "for another system" }
            )
        );
        let (r, out) = probar(&dudosos, None, true);
        assert!(r.is_err());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!((&v["status"], &v["id"], &v["release"]), (&"choose-asset".into(), &"added/autor-juego".into(), &"v1.0".into()));
        assert_eq!(v["candidates"], serde_json::json!(["Juego-v1.0-a.zip", "Juego-v1.0-b.zip"]));
        assert_eq!(v["rejected"][0], serde_json::json!({ "name": otro, "reason": "other-system" }));
        // Con `--asset`, ese.
        let (r, out) = probar(&dudosos, Some("Juego-v1.0-b.zip"), false);
        assert_eq!((r.unwrap().asset.as_str(), out.as_str()), ("Juego-v1.0-b.zip", ""));
        // Nada que instalar aquí, y los instaladores no se instalan aunque se pidan.
        let (r, out) = probar(&[otro, "LEEME.txt"], None, false);
        assert_eq!(r.unwrap_err(), "there is nothing to install from Autor/Juego on this system");
        assert!(out.starts_with("Release v1.0 of Autor/Juego has no file Unricopie can install on this system.\nNot offered:\n"), "{out}");
        let (r, _) = probar(&dudosos, Some("Juego-v1.0.msi"), false);
        assert_eq!(r.unwrap_err(), "Unricopie cannot install Juego-v1.0.msi yet (Installer)");

        // El repo que el catálogo ya instala no se añade (ni se usa la red): se dice con qué id se instala.
        let (codigo, _, err) = con(&ent, &["add", "https://github.com/Prueba/Juego-de-Prueba"]);
        assert_eq!(codigo, 1);
        assert!(err.starts_with("Prueba/Juego-de-Prueba is already in the catalogue: it is ports/") && err.contains("reviewed recipe: `unricopie install ports/"), "{err}");
        // Y lo que no es un repo se dice antes de usar la red.
        let (codigo, _, err) = con(&ent, &["add", "https://gitlab.com/a/b"]);
        assert!(codigo == 1 && err.starts_with("`https://gitlab.com/a/b` is not a GitHub repository"), "{err}");
    }

    /// Lo que hace `add` después de bajar: instalar buscando el programa y guardar la receta.
    fn anadir_falso(ent: &Entorno, repo: &str) -> (Recipe, PathBuf) {
        let (aqui, _) = archivos_de_aqui();
        let host = Platform::current().unwrap();
        let releases = release_de("v1.0", &[aqui]);
        let mut nada = Vec::new();
        let propuesta = proponer(&juegos(ent), repo, &releases, host, None, None, false, &mut nada).unwrap();
        let plan = install::installer::plan_from(&propuesta.recipe, None, host, Channel::Untested, None, &releases, &ent.biblioteca).unwrap();
        // Un zip con el juego dentro de una carpeta: el programa de este sistema y un dato.
        let (programa, contenido): (&str, &[u8]) = match std::env::consts::OS {
            "windows" => ("Juego.exe", b"MZ"),
            "macos" => ("Juego.app/Contents/MacOS/juego", b"mach-o"),
            _ => ("juego", b"\x7fELF"),
        };
        fs::create_dir_all(&ent.biblioteca.downloads).unwrap();
        let ruta = ent.biblioteca.downloads.join(aqui);
        let mut zip = zip::ZipWriter::new(fs::File::create(&ruta).unwrap());
        for (nombre, datos) in [(format!("Juego-v1.0/{programa}"), contenido), ("Juego-v1.0/datos/a.bin".to_string(), &b"a"[..])] {
            zip.start_file(nombre, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(datos).unwrap();
        }
        zip.finish().unwrap();
        let got = Downloaded { size: 3, sha256: "sha".into(), scan: None };
        let (_, receta) = added::install_downloaded(&ent.biblioteca, &propuesta.recipe, &plan, &ruta, &got).unwrap();
        added::add(&ent.biblioteca, &receta).unwrap();
        (receta, plan.dest)
    }

    #[test]
    fn lo_anadido_a_mano_sale_en_list_y_se_quita_con_remove_added() {
        let t = tempfile::tempdir().unwrap();
        let ent = entorno(t.path(), false);
        let (receta, dir) = anadir_falso(&ent, "Autor/Juego");
        assert_eq!(dir, t.path().join("games").join("autor-juego"));
        assert!(receta.resolve(None, Platform::current().unwrap()).unwrap().run.is_some());
        // Está en la lista como un juego más, marcado; y `path` y el id corto valen.
        let (_, out, _) = con(&ent, &["list"]);
        assert_eq!(out, format!("{:21}  v1.0  Juego (Juego)  [added by hand]\n{ENTRY}  -     Juego de prueba (Port de prueba)\n", "added/autor-juego"));
        let (_, json, _) = con(&ent, &["list", "--installed", "--json"]);
        let filas: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(filas, serde_json::json!([{ "id": "added/autor-juego", "name": "Juego", "project": "Juego", "installed": true, "version": "v1.0", "path": dir.display().to_string(), "scan": null, "added": true }]));
        assert_eq!(con(&ent, &["path", "autor-juego"]), (0, format!("{}\n", dir.display()), String::new()));
        // Otra vez no: ya está.
        let (codigo, _, err) = con(&ent, &["add", "autor/juego"]);
        assert!(codigo == 1 && err.starts_with("autor/juego is already added as added/autor-juego"), "{err}");

        // Instalado no se quita, y no se borra nada.
        for id in ["added/autor-juego", "autor-juego", "Autor/Juego", "https://github.com/autor/juego"] {
            let (codigo, out, err) = con(&ent, &["remove-added", id]);
            assert!(codigo == 1 && out.is_empty() && err.contains("remove-added never deletes a game") && err.contains(&dir.display().to_string()), "{id}: {err}");
        }
        assert!(dir.join("datos").join("a.bin").is_file());
        assert_eq!(added::load(&ent.biblioteca).unwrap().len(), 1);
        // Sin su carpeta sigue en la lista, sin instalar, y ya se puede quitar.
        fs::remove_dir_all(&dir).unwrap();
        assert!(con(&ent, &["list"]).1.contains("added/autor-juego      -  Juego (Juego)  [added by hand]\n"));
        assert_eq!(con(&ent, &["remove-added", "autor-juego"]), (0, "Removed added/autor-juego (Autor/Juego) from the projects added by hand\n".into(), String::new()));
        assert_eq!(con(&ent, &["list"]).1, format!("{ENTRY}  -  Juego de prueba (Port de prueba)\n"));
        let (codigo, _, err) = con(&ent, &["remove-added", "autor-juego"]);
        assert!(codigo == 1 && err.contains("no project added by hand has the id `autor-juego`"), "{err}");
        // Las órdenes de siempre no quitan nada del catálogo.
        let (codigo, _, err) = con(&ent, &["remove-added", ENTRY]);
        assert!(codigo == 1 && err.contains("no project added by hand"), "{err}");
    }

    #[test]
    fn los_errores_salen_en_ingles() {
        assert_eq!(ingles(&Error::BadRepo("x y".into())), "`x y` is not a GitHub repository: write owner/repo or its github.com address");
        assert_eq!(ingles(&Error::ChooseRun { candidates: vec!["a.exe".into(), "b.exe".into()] }), "it is not clear which of its programs starts the game: a.exe, b.exe");
        assert_eq!(ingles(&Error::ChooseRun { candidates: vec![] }), "it has no program to start");
        assert_eq!(ingles(&Error::NoSuchRelease { repo: "a/b".into(), tag: "v9".into() }), "a/b has no release with the tag v9 (see `unricopie versions`)");
        assert_eq!(ingles(&Error::UpToDate("v1".into())), "version v1 is already installed");
        assert_eq!(
            ingles(&Error::NoSpace { path: "D:/b/games".into(), needed: 5 << 30, free: 300 << 20 }),
            "not enough disk space in D:/b/games: 5.0 GB needed, 300 MB free"
        );
        assert_eq!(
            ingles(&Error::Threat { file: "juego.zip".into(), threat: "Trojan:Win32/Prueba".into() }),
            "the antivirus of this computer found Trojan:Win32/Prueba in juego.zip: the download was deleted and nothing was installed"
        );
        assert!(ingles(&Error::Threat { file: "juego.zip".into(), threat: String::new() }).contains("found a threat in juego.zip"));
        let falta = Error::MissingRuntime { program: "juego.exe".into(), libs: vec!["msvcp140.dll".into(), "vcruntime140.dll".into()], url: "https://aka.ms/vs/17/release/vc_redist.x64.exe".into() };
        assert_eq!(
            ingles(&falta),
            "juego.exe needs the Visual C++ libraries, which are missing: msvcp140.dll, vcruntime140.dll. Install them with Microsoft's installer: https://aka.ms/vs/17/release/vc_redist.x64.exe"
        );
        assert!(ingles(&Error::MissingVulkan { program: "juego.exe".into() }).contains("updating the graphics driver installs it"));
        assert_eq!(ingles(&Error::NoRelease { repo: "a/b".into(), channel: Channel::Untested }), "a/b has no release in the untested channel");
    }
}
