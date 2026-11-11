//! Lo que tarda va fuera de la ventana, en hilos: bajar e instalar (de uno en uno, en cola),
//! comprobar y guardar la copia del jugador y preparar el juego para abrirlo. Cada hilo cuenta
//! cómo va con `Aviso`, que la ventana recibe en su propio hilo. No se mandan frases: códigos y
//! datos, y la ventana los escribe en el idioma elegido (`Util.aviso` en app.slint).
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use unricopie_nucleo::install::{self, copy, launch, place, portable, settings, update, Channel, Library, Plan, Platform, Portability, Recipe, Status};
use unricopie_nucleo::mods::sources::{self, RemoteMod};
use unricopie_nucleo::{net, Error};

use crate::errores::{self, Datos};

/// Los mensajes sobre el juego abierto (los mismos números que `Util.aviso`).
pub mod codigo {
    pub const EN_DESCARGAS: i32 = 1;
    pub const COLA_NO_RESPONDE: i32 = 2;
    pub const PREPARANDO: i32 = 3;
    pub const COMPROBANDO_COPIA: i32 = 4;
    pub const COPIA_COMPROBADA: i32 = 5;
    pub const COPIA_SIN_COMPROBAR: i32 = 6;
    pub const COPIA_NO_COINCIDE: i32 = 7;
    pub const COPIA_ERROR: i32 = 8;
    pub const ABRIENDO: i32 = 9;
    pub const ABRIENDO_SIN_COPIA: i32 = 10;
    pub const ABRIENDO_ACTUALIZADOR: i32 = 11;
    pub const ABRIENDO_SIN_COPIA_Y_ACTUALIZADOR: i32 = 12;
    pub const NO_SE_PUDO_ABRIR: i32 = 13;
    pub const VUELTA: i32 = 14;
    pub const NO_SE_PUDO_VOLVER: i32 = 15;
    pub const ANTERIOR_BORRADA: i32 = 16;
    pub const NO_SE_PUDO_BORRAR: i32 = 17;
}

/// Lo que un hilo le cuenta a la ventana.
pub enum Aviso {
    /// Ya se sabe qué versión se baja y cuánto pesa.
    Plan { entry: String, version: String, total: Option<u64> },
    Progreso { entry: String, bajado: u64, total: Option<u64> },
    /// Instalado; `error` si no se pudo hacer portable.
    Instalado { entry: String, error: Option<Datos> },
    Fallo { entry: String, error: Datos },
    /// Un mensaje sobre el juego abierto: su código, un dato y el error, si lo hubo.
    Mensaje { codigo: i32, a: String, error: Datos },
    /// Cambió lo instalado o la copia: hay que volver a leerlo.
    Refrescar,
    /// Llegaron las opiniones de la gente (recompendium.com/reportes.json).
    Opiniones(std::collections::HashMap<String, crate::opiniones::Resumen>),
    /// Hay un catálogo o unas recetas nuevas en `<raíz>/datos/`.
    Datos,
    /// Llegaron portadas a `<raíz>/datos/arte/`.
    Arte,
    /// Cómo quedaron los mods de un juego después de una orden (`mods::hilo`).
    Mods(Box<crate::mods::Estado>),
    /// La lista de una comunidad de Thunderstore.
    Thunderstore { comunidad: String, mods: Vec<RemoteMod>, error: Option<String> },
    /// Llegaron iconos de mods a `<raíz>/datos/arte/mods/`.
    Iconos,
    /// Hay una versión de Unricopie más nueva: su número y su página.
    Version { version: String, pagina: String },
    /// La última versión oficial de cada juego instalado (por ficha), para avisar si hay una
    /// más nueva que la instalada.
    Ultimas(HashMap<String, String>),
    /// Llegó el README o el registro de cambios de un mod (en Markdown), o no se pudo leer.
    Readme { id: String, que: crate::ficha_mod::Que, md: Option<String> },
}

/// Lo que baja la cola: un juego, la versión nueva de un juego o un mod.
pub enum Tarea {
    Juego(String, Recipe),
    /// Si el juego tiene mods (`mods`), la versión bajada se aplica en el hilo de los mods, que
    /// antes los quita y después los vuelve a poner.
    Actualizar { entry: String, receta: Recipe, mods: Option<Box<crate::mods::Ctx>> },
    /// `bajada` es su clave en la lista de descargas (`mods::bajada`).
    Mod { ctx: crate::mods::Ctx, remoto: RemoteMod, bajada: String },
}

/// Después de abrir: busca un catálogo nuevo y, si no lo hay, baja las portadas que faltan
/// (si lo hay, la ventana lo vuelve a leer y pide sus portadas con `arte`).
pub fn datos(raiz: std::path::PathBuf, bajar: bool, pendientes: crate::datos::Pendientes) {
    std::thread::spawn(move || {
        if bajar && crate::datos::bajar(&raiz) {
            avisar(Aviso::Datos);
        } else {
            crate::datos::bajar_arte(pendientes, 16, || avisar(Aviso::Arte));
        }
    });
}

/// Baja las portadas que faltan, en un hilo.
pub fn arte(pendientes: crate::datos::Pendientes) {
    if pendientes.is_empty() {
        return;
    }
    std::thread::spawn(move || crate::datos::bajar_arte(pendientes, 16, || avisar(Aviso::Arte)));
}

/// Mira en un hilo si hay una versión nueva de Unricopie.
pub fn buscar_version() {
    std::thread::spawn(|| {
        if let Some((version, pagina)) = crate::datos::version_nueva() {
            avisar(Aviso::Version { version, pagina });
        }
    });
}

/// Lee las opiniones en un hilo, para no frenar el arranque.
pub fn leer_opiniones() {
    std::thread::spawn(|| avisar(Aviso::Opiniones(crate::opiniones::leer())));
}

/// Abre el formulario de «¿Cómo te fue?» ya relleno. En un hilo: preguntarle a Windows por el
/// sistema y la tarjeta gráfica tarda un momento.
pub fn opinar(nombre: String, entry: String, version: String, resultado: i32) {
    std::thread::spawn(move || {
        let url = crate::opiniones::url(&nombre, &entry, &version, resultado);
        crate::abrir_enlace(&url);
    });
}

/// Manda el aviso al hilo de la ventana.
pub fn avisar(a: Aviso) {
    let _ = slint::invoke_from_event_loop(move || crate::recibir(a));
}

pub(crate) fn mensaje(codigo: i32, a: impl Into<String>, error: Option<&Error>) {
    avisar(Aviso::Mensaje { codigo, a: a.into(), error: error.map(errores::de).unwrap_or_default() });
}

/// La cola de descargas: un hilo que baja de uno en uno lo que le llega. Los juegos los instala
/// él; los mods se los pasa al hilo de los mods (`mods`) para que los guarde y los ponga.
pub fn cola(biblioteca: Library, launcher_portable: bool, mods: Sender<crate::mods::Orden>) -> Sender<Tarea> {
    let (tx, rx) = mpsc::channel::<Tarea>();
    std::thread::spawn(move || {
        let agent = net::agent();
        for tarea in rx {
            match tarea {
                Tarea::Juego(entry, receta) => match instalar(&agent, &biblioteca, launcher_portable, &entry, &receta) {
                    Ok(error) => avisar(Aviso::Instalado { entry, error: error.as_ref().map(errores::de) }),
                    Err(e) => avisar(Aviso::Fallo { entry, error: errores::de(&e) }),
                },
                Tarea::Actualizar { entry, receta, mods: con_mods } => match bajar_version(&agent, &biblioteca, &entry, &receta) {
                    // Ya estaba al día (alguien actualizó por su cuenta): no hay nada que hacer.
                    Err(Error::UpToDate(_)) => avisar(Aviso::Instalado { entry, error: None }),
                    Err(e) => avisar(Aviso::Fallo { entry, error: errores::de(&e) }),
                    Ok((plan, archivo, bajado)) => match con_mods {
                        Some(ctx) => {
                            let orden = crate::mods::Orden::Actualizar { ctx: *ctx, biblioteca: biblioteca.clone(), plan: Box::new(plan), archivo, bajado, entry: entry.clone() };
                            if mods.send(orden).is_err() {
                                avisar(Aviso::Fallo { entry, error: errores::Datos::default() });
                            }
                        }
                        None => {
                            let r = update::update_downloaded(&biblioteca, &plan, &archivo, &bajado);
                            let _ = std::fs::remove_file(&archivo);
                            match r {
                                Ok(_) => avisar(Aviso::Instalado { entry, error: None }),
                                Err(e) => avisar(Aviso::Fallo { entry, error: errores::de(&e) }),
                            }
                        }
                    },
                },
                Tarea::Mod { ctx, remoto, bajada } => match bajar_mod(&agent, &ctx, &remoto, &bajada) {
                    Ok(archivo) => {
                        let orden = crate::mods::Orden::Importar { ctx, remoto: Box::new(remoto), archivo, bajada: bajada.clone() };
                        if mods.send(orden).is_err() {
                            avisar(Aviso::Fallo { entry: bajada, error: errores::Datos::default() });
                        }
                    }
                    Err(e) => avisar(Aviso::Fallo { entry: bajada, error: errores::de(&e) }),
                },
            }
        }
    });
    tx
}

/// Baja un mod a `<raíz>/downloads/mods/<juego>/`, contando cómo va.
fn bajar_mod(agent: &ureq::Agent, ctx: &crate::mods::Ctx, remoto: &RemoteMod, bajada: &str) -> Result<PathBuf, Error> {
    let archivo = remoto.files.first().ok_or_else(|| Error::BadRecipe(format!("{} no tiene archivo", remoto.id)))?;
    avisar(Aviso::Plan { entry: bajada.to_string(), version: remoto.version.clone(), total: archivo.size });
    let nombre: String = archivo.name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let dest = ctx.raiz.join("downloads").join("mods").join(&ctx.clave).join(nombre);
    let mut ultimo: Option<Instant> = None;
    // 16 GB, como el límite al descomprimir: el pack de texturas más grande pesa unos 4 GB.
    sources::download_with(agent, archivo, &dest, 16 << 30, &mut |bajado, total| {
        let fin = total == Some(bajado);
        if fin || ultimo.is_none_or(|t| t.elapsed() >= Duration::from_millis(150)) {
            ultimo = Some(Instant::now());
            avisar(Aviso::Progreso { entry: bajada.to_string(), bajado, total });
        }
    })?;
    Ok(dest)
}

/// Devuelve el error de hacerlo portable, si lo hubo (el juego queda instalado igual).
fn instalar(agent: &ureq::Agent, biblioteca: &Library, launcher_portable: bool, entry: &str, receta: &Recipe) -> Result<Option<Error>, Error> {
    let host = Platform::current().ok_or(Error::UnknownSystem)?;
    // Sin datos del banco de pruebas todavía, se baja la última release oficial (sin probar).
    let plan = install::plan(agent, receta, None, host, Channel::Untested, None, biblioteca)?;
    avisar(Aviso::Plan { entry: entry.to_string(), version: plan.version.clone(), total: plan.size });
    install::install(agent, biblioteca, &plan, &mut contar(entry))?;
    // Con el launcher en modo portable, cada juego que se instala también lo es, si se puede.
    if launcher_portable {
        if let Portability::Rule(_) = receta.portability(plan.platform) {
            if let Err(e) = portable::enable(&plan.dest, receta) {
                return Ok(Some(e));
            }
        }
    }
    Ok(None)
}

/// Cuenta cómo va la descarga de `entry`: un aviso cada 150 ms como mucho, y siempre el último.
fn contar(entry: &str) -> impl FnMut(u64, Option<u64>) + '_ {
    let mut ultimo: Option<Instant> = None;
    move |bajado, total| {
        let fin = total == Some(bajado);
        if fin || ultimo.is_none_or(|t| t.elapsed() >= Duration::from_millis(150)) {
            ultimo = Some(Instant::now());
            avisar(Aviso::Progreso { entry: entry.to_string(), bajado, total });
        }
    }
}

/// Baja la versión nueva de un juego instalado, sin tocar lo instalado todavía.
fn bajar_version(agent: &ureq::Agent, biblioteca: &Library, entry: &str, receta: &Recipe) -> Result<(Plan, PathBuf, unricopie_nucleo::net::Downloaded), Error> {
    let host = Platform::current().ok_or(Error::UnknownSystem)?;
    let plan = install::plan(agent, receta, None, host, Channel::Untested, None, biblioteca)?;
    avisar(Aviso::Plan { entry: entry.to_string(), version: plan.version.clone(), total: plan.size });
    let (archivo, bajado) = update::download(agent, biblioteca, &plan, &mut contar(entry))?;
    Ok((plan, archivo, bajado))
}

/// Mira, en un hilo, la última versión oficial de cada juego instalado. Lo que no se puede
/// mirar (sin red, o GitHub que no contesta) se deja para la próxima vez, sin avisar.
pub fn buscar_ultimas(biblioteca: Library, instalados: Vec<(String, Recipe)>) {
    if instalados.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Some(host) = Platform::current() else { return };
        let agent = net::agent();
        let mut ultimas = HashMap::new();
        for (entry, receta) in instalados {
            let plan = match install::plan(&agent, &receta, None, host, Channel::Untested, None, &biblioteca) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("no se pudo mirar la última versión de {entry}: {e}");
                    continue;
                }
            };
            if matches!(update::status(&plan), Ok(Status::Available(_) | Status::UpToDate(_))) {
                ultimas.insert(entry, plan.version);
            }
        }
        avisar(Aviso::Ultimas(ultimas));
    });
}

/// Vuelve a una versión anterior guardada de un juego sin mods (los que tienen mods lo hacen en
/// el hilo de los mods, que antes los quita).
pub fn volver(biblioteca: Library, dir: PathBuf, id: String, version: String) {
    std::thread::spawn(move || {
        match update::roll_back(&biblioteca, &dir, &id) {
            Ok(_) => mensaje(codigo::VUELTA, version, None),
            Err(e) => mensaje(codigo::NO_SE_PUDO_VOLVER, version, Some(&e)),
        }
        avisar(Aviso::Refrescar);
    });
}

/// Borra una versión anterior guardada, cuando la persona lo pide.
pub fn borrar_anterior(biblioteca: Library, dir: PathBuf, id: String, version: String) {
    std::thread::spawn(move || {
        match update::remove_previous(&biblioteca, &dir, &id) {
            Ok(()) => mensaje(codigo::ANTERIOR_BORRADA, version, None),
            Err(e) => mensaje(codigo::NO_SE_PUDO_BORRAR, version, Some(&e)),
        }
        avisar(Aviso::Refrescar);
    });
}

/// Comprueba la copia que eligió la persona y la guarda en la biblioteca.
pub fn guardar_copia(biblioteca: Library, receta: Recipe, origen: PathBuf) {
    std::thread::spawn(move || {
        // Si el proyecto publica su lista de copias (accepts-from, como Ship of Harkinian), se lee
        // para comprobarla con ella; sin red, se comprueba solo con la receta.
        let lista = match &receta.copy.accepts_from {
            Some(from) => copy::fetch_accepts(&net::agent(), from).unwrap_or_else(|e| {
                eprintln!("no se pudo leer la lista de copias aceptadas: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        match copy::import_with(&biblioteca, &receta, None, &origen, &lista, false, &mut |_| {}) {
            Ok(i) if i.verified => mensaje(codigo::COPIA_COMPROBADA, i.accepted.first().cloned().unwrap_or_default(), None),
            Ok(_) => mensaje(codigo::COPIA_SIN_COMPROBAR, "", None),
            Err(Error::CopyMismatch(_)) => mensaje(codigo::COPIA_NO_COINCIDE, "", None),
            Err(e) => mensaje(codigo::COPIA_ERROR, "", Some(&e)),
        }
        avisar(Aviso::Refrescar);
    });
}

/// Le da al juego la copia, apaga su actualizador y lo abre. Con `tiempo` (la raíz y la clave
/// de esa versión, `tiempo::clave`), cuenta cuánto se juega, para «¿Cómo te fue?».
pub fn jugar(biblioteca: Library, receta: Recipe, dir: PathBuf, nombre: String, tiempo: Option<(PathBuf, String)>) {
    std::thread::spawn(move || {
        let r = (|| -> Result<(bool, bool, std::process::Child), Error> {
            let puesto = place::place(&biblioteca, &dir, &receta)?;
            let aplicado = settings::apply(&dir, &receta)?;
            let l = launch::prepare(&biblioteca, &dir, &receta)?;
            let hijo = l.command().spawn().map_err(|e| Error::CannotLaunch(l.program.clone(), e.to_string()))?;
            Ok((puesto.no_copy, !aplicado.reapplied.is_empty(), hijo))
        })();
        let mut hijo = match r {
            Ok((sin_copia, actualizador, hijo)) => {
                let c = match (sin_copia, actualizador) {
                    (false, false) => codigo::ABRIENDO,
                    (true, false) => codigo::ABRIENDO_SIN_COPIA,
                    (false, true) => codigo::ABRIENDO_ACTUALIZADOR,
                    (true, true) => codigo::ABRIENDO_SIN_COPIA_Y_ACTUALIZADOR,
                };
                mensaje(c, nombre, None);
                hijo
            }
            Err(e) => return mensaje(codigo::NO_SE_PUDO_ABRIR, nombre, Some(&e)),
        };
        let Some((raiz, clave)) = tiempo else { return };
        // Ya se abrió una vez: «No arranca» se puede marcar desde ahora.
        crate::tiempo::abierto(&raiz, &clave);
        avisar(Aviso::Refrescar);
        crate::tiempo::medir(&mut hijo, Duration::from_secs(60), |s| crate::tiempo::sumar(&raiz, &clave, s));
        avisar(Aviso::Refrescar);
    });
}
