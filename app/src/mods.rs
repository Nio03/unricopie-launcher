//! Los mods de la página del juego (pestaña Mods): el hilo que los cambia, la lista de
//! Thunderstore y lo que la ventana muestra.
//!
//! Todo lo que toca los archivos de mods (guardar uno bajado, quitarlo, activarlo, cambiar de
//! perfil y ponerlos en el juego) lo hace un solo hilo, de uno en uno y en orden: así una
//! descarga que termina y un interruptor que el jugador toca no se pisan. Después de cada orden
//! el hilo manda a la ventana cómo quedó todo (`Estado`); la ventana nunca lee esos archivos.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use slint::{ModelRc, VecModel};
use unricopie_nucleo::install::recipe::ModsRule;
use unricopie_nucleo::install::{update, Library, Plan};
use unricopie_nucleo::net::Downloaded;
use unricopie_nucleo::mods::game::{self, GameMods, Profiles, Target};
use unricopie_nucleo::mods::model::lenient_version;
use unricopie_nucleo::mods::solver::{Problem, Resolution};
use unricopie_nucleo::mods::sources::{thunderstore, RemoteMod};
use unricopie_nucleo::mods::{InstalledMod, ModId, RelPath, Source};
use unricopie_nucleo::{net, Error};

use crate::errores;
use crate::trabajo::{avisar, codigo, mensaje, Aviso};

/// De qué juego instalado (o por instalar) son los mods.
#[derive(Clone)]
pub struct Ctx {
    /// La carpeta del juego en la biblioteca (`zelda-majoras-mask`): sus mods van en
    /// `<raíz>/mods/<clave>`.
    pub clave: String,
    pub entry: String,
    pub raiz: PathBuf,
    pub regla: ModsRule,
    /// Dónde se ponen; `None` si el juego no está instalado.
    pub destino: Option<Target>,
}

impl Ctx {
    fn gm(&self) -> GameMods {
        GameMods::new(&self.raiz, &self.clave, &self.entry)
    }
}

pub enum OpPerfil {
    Elegir(String),
    Nuevo { nombre: String, copiar: bool },
    Renombrar(String),
    Borrar,
}

pub enum Orden {
    /// Solo leer cómo están (al abrir la pestaña).
    Ver(Ctx),
    Activar(Ctx, ModId, bool),
    Variante(Ctx, ModId, RelPath, bool),
    Quitar(Ctx, ModId),
    Perfil(Ctx, OpPerfil),
    /// Llegó un mod bajado: se guarda, se pone en el juego y se borra el archivo bajado.
    Importar { ctx: Ctx, remoto: Box<RemoteMod>, archivo: PathBuf, bajada: String },
    /// Arrastrado en la lista: el mod va justo antes (o después) de otro.
    Mover(Ctx, ModId, ModId, bool),
    /// «Subir» (-1) o «Bajar» (+1): un lugar en el orden de carga.
    Correr(Ctx, ModId, i32),
    /// «Que gane X»: en un conflicto de archivos, el primero gana a los demás.
    Preferir(Ctx, ModId, Vec<ModId>),
    /// Mods propios: archivos o carpetas que el jugador soltó en la ventana o eligió.
    Locales(Ctx, Vec<PathBuf>),
    /// La versión nueva del juego ya está bajada (`archivo`): se quitan los mods, se actualiza
    /// y se vuelven a poner. `entry` es su clave en la lista de descargas.
    Actualizar { ctx: Ctx, biblioteca: Library, plan: Box<Plan>, archivo: PathBuf, bajado: Downloaded, entry: String },
    /// Volver a la versión anterior guardada `id` (la `version` es para el mensaje).
    Volver { ctx: Ctx, biblioteca: Library, dir: PathBuf, id: String, version: String },
}

impl Orden {
    fn ctx(&self) -> &Ctx {
        match self {
            Orden::Ver(c)
            | Orden::Activar(c, ..)
            | Orden::Variante(c, ..)
            | Orden::Quitar(c, _)
            | Orden::Perfil(c, _)
            | Orden::Mover(c, ..)
            | Orden::Correr(c, ..)
            | Orden::Preferir(c, ..)
            | Orden::Locales(c, _) => c,
            Orden::Importar { ctx, .. } | Orden::Actualizar { ctx, .. } | Orden::Volver { ctx, .. } => ctx,
        }
    }
}

/// Lo que dejó una orden: el error, si lo hubo, y cuántos cambios del juego se adoptaron y
/// cuántos mods propios se añadieron.
struct Hecho {
    ctx: Ctx,
    error: Option<Error>,
    adoptados: usize,
    anadidos: usize,
}

/// Cómo quedaron los mods de un juego después de una orden.
pub struct Estado {
    pub clave: String,
    pub instalados: Vec<InstalledMod>,
    pub perfiles: Profiles,
    pub resolucion: Resolution,
    /// Las variantes de los mods que traen varias (`choose` de la receta).
    pub variantes: HashMap<ModId, Vec<RelPath>>,
    /// Lo que falló, si falló algo.
    pub error: Option<errores::Datos>,
    /// Cambios del menú del juego que se adoptaron, y mods propios añadidos, en esta orden.
    pub adoptados: usize,
    pub anadidos: usize,
}

/// El hilo de los mods.
pub fn hilo() -> Sender<Orden> {
    let (tx, rx) = mpsc::channel::<Orden>();
    std::thread::spawn(move || {
        for orden in rx {
            let h = hacer(orden);
            if let Some(e) = &h.error {
                eprintln!("mods: {e}");
            }
            let mut e = estado(&h.ctx, h.error);
            e.adoptados = h.adoptados;
            e.anadidos = h.anadidos;
            avisar(Aviso::Mods(Box::new(e)));
        }
    });
    tx
}

fn hacer(o: Orden) -> Hecho {
    // Antes de nada, lo que el jugador cambió en el menú del juego: si no, se pisaría.
    let adoptados = {
        let ctx = o.ctx();
        match &ctx.destino {
            Some(t) => ctx.gm().adopt(&ctx.regla, t).unwrap_or_else(|e| {
                eprintln!("mods: no se pudo leer lo cambiado en el juego: {e}");
                0
            }),
            None => 0,
        }
    };
    let (ctx, r, anadidos) = hacer_orden(o, adoptados);
    Hecho { ctx, error: r.err(), adoptados, anadidos }
}

fn hacer_orden(o: Orden, adoptados: usize) -> (Ctx, Result<(), Error>, usize) {
    let (ctx, r) = match o {
        // Si se adoptó algo, se vuelve a desplegar para que la carpeta muestre el perfil.
        Orden::Ver(ctx) => {
            let r = if adoptados > 0 { aplicar(&ctx) } else { Ok(()) };
            (ctx, r)
        }
        Orden::Mover(ctx, id, destino, despues) => {
            let r = cambiar(&ctx, |ps| ps.active_mut().move_near(&id, &destino, despues));
            (ctx, r)
        }
        Orden::Correr(ctx, id, delta) => {
            let r = correr(&ctx, &id, delta);
            (ctx, r)
        }
        Orden::Preferir(ctx, gana, otros) => {
            let r = cambiar(&ctx, |ps| ps.active_mut().prefer(&gana, &otros));
            (ctx, r)
        }
        Orden::Locales(ctx, rutas) => {
            let gm = ctx.gm();
            let mut n = 0;
            let mut fallo = None;
            for p in &rutas {
                match gm.add_local(p, &ctx.regla) {
                    Ok(_) => n += 1,
                    Err(e) => {
                        eprintln!("mods: no se pudo añadir {}: {e}", p.display());
                        fallo.get_or_insert(e);
                    }
                }
            }
            let r = aplicar(&ctx).and(fallo.map_or(Ok(()), Err));
            return (ctx, r, n);
        }
        Orden::Activar(ctx, id, on) => {
            let r = cambiar(&ctx, |ps| {
                let p = ps.active_mut();
                p.add(id.clone());
                p.set_enabled(&id, on);
            });
            (ctx, r)
        }
        Orden::Variante(ctx, id, archivo, on) => {
            let r = cambiar(&ctx, |ps| ps.active_mut().set_file(&id, &archivo, on));
            (ctx, r)
        }
        Orden::Quitar(ctx, id) => {
            let r = ctx.gm().remove(&id).and_then(|_| aplicar(&ctx));
            (ctx, r)
        }
        Orden::Perfil(ctx, op) => {
            let r = perfil(&ctx, op);
            (ctx, r)
        }
        // Actualizar o volver atrás cambian la carpeta del juego entera: antes se quitan los mods
        // (`update.rs` lo pide) y después se vuelven a poner, salga bien o mal.
        Orden::Actualizar { ctx, biblioteca, plan, archivo, bajado, entry } => {
            let r = ctx.gm().purge().and_then(|_| update::update_downloaded(&biblioteca, &plan, &archivo, &bajado).map(|_| ()));
            let _ = fs::remove_file(&archivo);
            let r = match r {
                Err(Error::UpToDate(_)) => Ok(()),
                r => r,
            };
            match &r {
                Ok(()) => avisar(Aviso::Instalado { entry, error: None }),
                Err(e) => avisar(Aviso::Fallo { entry, error: errores::de(e) }),
            }
            let puestos = aplicar(&ctx);
            (ctx, r.and(puestos))
        }
        Orden::Volver { ctx, biblioteca, dir, id, version } => {
            let r = ctx.gm().purge().and_then(|_| update::roll_back(&biblioteca, &dir, &id).map(|_| ()));
            match &r {
                Ok(()) => mensaje(codigo::VUELTA, version, None),
                Err(e) => mensaje(codigo::NO_SE_PUDO_VOLVER, version, Some(e)),
            }
            let puestos = aplicar(&ctx);
            avisar(Aviso::Refrescar);
            (ctx, r.and(puestos))
        }
        Orden::Importar { ctx, remoto, archivo, bajada } => {
            let guardado = ctx.gm().add(&remoto, &archivo, &ctx.regla);
            let _ = fs::remove_file(&archivo);
            match guardado {
                Ok(_) => {
                    avisar(Aviso::Instalado { entry: bajada, error: None });
                    let r = aplicar(&ctx);
                    (ctx, r)
                }
                Err(e) => {
                    avisar(Aviso::Fallo { entry: bajada, error: errores::de(&e) });
                    (ctx, Err(e))
                }
            }
        }
    };
    (ctx, r, 0)
}

/// «Subir» y «Bajar»: un lugar en el orden de carga que se ve (el resuelto).
fn correr(ctx: &Ctx, id: &ModId, delta: i32) -> Result<(), Error> {
    let orden = ctx.gm().resolve(&ctx.regla)?.order;
    let Some(i) = orden.iter().position(|x| x == id) else { return Ok(()) };
    let j = i as i64 + delta as i64;
    if j < 0 || j as usize >= orden.len() {
        return Ok(());
    }
    let vecino = orden[j as usize].clone();
    cambiar(ctx, |ps| ps.active_mut().move_near(id, &vecino, delta > 0))
}

/// Los perfiles, con los ajustes de sus mods: antes de dejar un perfil se guardan los del juego;
/// al llegar a otro se ponen los que tenía guardados.
fn perfil(ctx: &Ctx, op: OpPerfil) -> Result<(), Error> {
    let gm = ctx.gm();
    let mut ps = gm.profiles()?;
    let viejo = ps.active.clone();
    let destino = ctx.destino.as_ref();
    let guardar = |gm: &GameMods| -> Result<(), Error> {
        if let Some(t) = destino {
            gm.save_settings(&viejo, t)?;
        }
        Ok(())
    };
    match op {
        OpPerfil::Elegir(n) => {
            if n == viejo || !ps.list.iter().any(|p| p.name == n) {
                return Ok(());
            }
            guardar(&gm)?;
            ps.active = n.clone();
            gm.save_profiles(&ps)?;
            aplicar(ctx)?;
            if let Some(t) = destino {
                gm.restore_settings(&n, t)?;
            }
        }
        OpPerfil::Nuevo { nombre, copiar } => {
            guardar(&gm)?;
            let nuevo = ps.create(&nombre, copiar);
            gm.save_profiles(&ps)?;
            if copiar {
                gm.copy_settings(&viejo, &nuevo)?;
            }
            aplicar(ctx)?;
        }
        OpPerfil::Renombrar(n) => {
            if let Some(nuevo) = ps.rename(&viejo, &n) {
                gm.save_profiles(&ps)?;
                gm.rename_settings(&viejo, &nuevo)?;
            }
        }
        OpPerfil::Borrar => {
            if ps.delete(&viejo) {
                gm.save_profiles(&ps)?;
                gm.delete_settings(&viejo)?;
                aplicar(ctx)?;
                if let Some(t) = destino {
                    gm.restore_settings(&ps.active, t)?;
                }
            }
        }
    }
    Ok(())
}

/// Cambia los perfiles y pone el activo en el juego.
fn cambiar(ctx: &Ctx, f: impl FnOnce(&mut Profiles)) -> Result<(), Error> {
    let gm = ctx.gm();
    let mut ps = gm.profiles()?;
    f(&mut ps);
    gm.save_profiles(&ps)?;
    aplicar(ctx)
}

fn aplicar(ctx: &Ctx) -> Result<(), Error> {
    if let Some(t) = &ctx.destino {
        ctx.gm().apply(&ctx.regla, t)?;
    }
    Ok(())
}

fn estado(ctx: &Ctx, error: Option<Error>) -> Estado {
    let gm = ctx.gm();
    let mut error = error.map(|e| errores::de(&e));
    let mut anota = |e: Error| {
        if error.is_none() {
            error = Some(errores::de(&e));
        }
    };
    let instalados = gm.installed().unwrap_or_else(|e| {
        anota(e);
        Vec::new()
    });
    let perfiles = gm.profiles().unwrap_or_else(|e| {
        anota(e);
        Profiles::new(&ctx.entry)
    });
    let resolucion = gm.resolve(&ctx.regla).unwrap_or_default();
    let variantes = instalados
        .iter()
        .filter_map(|m| {
            let v = game::choices(m, &ctx.regla);
            (!v.is_empty()).then(|| (m.id.clone(), v))
        })
        .collect();
    Estado { clave: ctx.clave.clone(), instalados, perfiles, resolucion, variantes, error, adoptados: 0, anadidos: 0 }
}

// ---------- La lista de Thunderstore ----------

#[derive(Serialize, Deserialize)]
struct Cache {
    fecha: u64,
    /// Qué guarda de cada mod: una lista guardada con un formato más viejo se vuelve a pedir.
    #[serde(default)]
    formato: u32,
    mods: Vec<RemoteMod>,
}

/// 2: con la fecha de subida, los «me gusta», la web y las versiones (10-11).
const FORMATO_LISTA: u32 = 2;

/// Seis horas: la lista de una comunidad cambia poco y pesa medio MB.
const FRESCA: u64 = 6 * 3600;

/// Lee la lista de una comunidad en un hilo: de la copia de `<raíz>/datos/mods/` si es
/// reciente (o si no hay red) y si no, de la API de Thunderstore.
pub fn lista(raiz: PathBuf, comunidad: String, forzar: bool) {
    std::thread::spawn(move || {
        let (mods, error) = match leer_lista(&raiz, &comunidad, forzar) {
            Ok(m) => (m, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        avisar(Aviso::Thunderstore { comunidad, mods, error });
    });
}

fn leer_lista(raiz: &Path, comunidad: &str, forzar: bool) -> Result<Vec<RemoteMod>, String> {
    let archivo = raiz.join("datos").join("mods").join(format!("thunderstore-{comunidad}.json"));
    let ahora = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let guardada: Option<Cache> = fs::read(&archivo).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(c) = &guardada {
        if !forzar && c.formato >= FORMATO_LISTA && ahora.saturating_sub(c.fecha) < FRESCA {
            return Ok(c.mods.clone());
        }
    }
    match thunderstore::fetch(&net::agent(), comunidad) {
        Ok(mods) => {
            let c = Cache { fecha: ahora, formato: FORMATO_LISTA, mods };
            if let Some(dir) = archivo.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let tmp = archivo.with_extension("json.tmp");
            if let Ok(texto) = serde_json::to_vec(&c) {
                if fs::write(&tmp, texto).is_ok() {
                    let _ = fs::rename(&tmp, &archivo);
                }
            }
            Ok(c.mods)
        }
        Err(e) => guardada.map(|c| c.mods).ok_or_else(|| e.to_string()),
    }
}

// ---------- Lo que se dibuja ----------

/// La clave de una descarga de mod en la cola: `mod:<carpeta del juego>:<id>`.
pub fn bajada(clave: &str, id: &ModId) -> String {
    format!("mod:{clave}:{id}")
}

/// El nombre corto de un mod que no está ni instalado ni en la lista: lo que va tras el autor.
fn nombre_de_id(id: &ModId) -> String {
    let s = id.as_str();
    let tail = s.rsplit(':').next().unwrap_or(s);
    tail.split_once('-').map(|(_, n)| n).unwrap_or(tail).replace('_', " ")
}

/// Números como los escribe cada idioma: «50.624» y «50,624».
pub fn miles(n: u64, idioma: &str) -> String {
    let s = n.to_string();
    let sep = if idioma == "en" { ',' } else { '.' };
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// ¿`b` es una versión más nueva que `a`?
pub(crate) fn mas_nueva(a: &str, b: &str) -> bool {
    match (lenient_version(a), lenient_version(b)) {
        (Some(a), Some(b)) => b > a,
        _ => a != b,
    }
}

/// Las descargas de mods en curso: su clave, fase y progreso.
pub type Bajando = HashMap<String, (i32, f32, String, String)>;

pub struct Vista {
    pub filas: Vec<crate::ModFila>,
    pub problemas: Vec<crate::ProblemaMod>,
    pub perfiles: Vec<slint::SharedString>,
    pub activo: String,
}

/// Lo que muestra la lista de mods instalados, en el orden de carga.
pub fn vista(e: &Estado, lista: Option<&[RemoteMod]>, bajando: &Bajando) -> Vista {
    let remoto = |id: &ModId| lista.and_then(|l| l.iter().find(|m| &m.id == id));
    let nombre = |id: &ModId| -> String {
        e.instalados
            .iter()
            .find(|m| &m.id == id)
            .map(|m| m.name.clone())
            .or_else(|| remoto(id).map(|r| r.name.clone()))
            .unwrap_or_else(|| nombre_de_id(id))
    };
    let p = e.perfiles.active();
    let orden = &e.resolucion.order;
    // Quién pide a quién, entre los activos.
    let mut pedido: HashMap<&ModId, Vec<String>> = HashMap::new();
    for id in orden {
        if let Some(m) = e.instalados.iter().find(|m| &m.id == id) {
            for d in &m.dependencies {
                pedido.entry(&d.id).or_default().push(m.name.clone());
            }
        }
    }
    let mut filas: Vec<(usize, crate::ModFila)> = Vec::new();
    for m in &e.instalados {
        let entrada = p.entry(&m.id);
        let activo = entrada.is_some_and(|x| x.enabled);
        let pos = orden.iter().position(|x| x == &m.id);
        let r = remoto(&m.id);
        let autor = r.map(|r| r.author.clone()).unwrap_or_else(|| match &m.source {
            Source::Thunderstore { full_name, .. } => full_name.split('-').next().unwrap_or_default().to_string(),
            _ => String::new(),
        });
        let variantes: Vec<crate::Variante> = e
            .variantes
            .get(&m.id)
            .map(|v| {
                v.iter()
                    .map(|f| crate::Variante {
                        archivo: f.as_str().into(),
                        nombre: Path::new(f.file_name()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().into(),
                        activa: !entrada.is_some_and(|x| x.off.iter().any(|o| o.key() == f.key())),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let falta_elegir = !variantes.is_empty() && variantes.iter().all(|v| !v.activa);
        let pide = if activo { pedido.get(&m.id).map(|v| v.join(", ")).unwrap_or_default() } else { String::new() };
        let nueva = r.filter(|r| mas_nueva(&m.version, &r.version)).map(|r| r.version.clone()).unwrap_or_default();
        let (fase, progreso) = bajando.get(&bajada(&e.clave, &m.id)).map(|(f, p, _, _)| (*f, *p)).unwrap_or((-1, 0.0));
        let fila = crate::ModFila {
            id: m.id.as_str().into(),
            nombre: m.name.as_str().into(),
            autor: autor.into(),
            // Los propios no tienen versión que mostrar.
            version: if matches!(m.source, Source::Local) { "".into() } else { m.version.as_str().into() },
            orden: pos.map_or(0, |i| i as i32 + 1),
            activo,
            instalado: true,
            desc: r.map(|r| r.info.description.as_str()).unwrap_or_default().into(),
            pide: pide.into(),
            nueva: nueva.into(),
            variantes: ModelRc::new(VecModel::from(variantes)),
            falta_elegir,
            probado: false,
            fase,
            progreso,
            pagina: r.map(|r| r.page_url.as_str()).unwrap_or_default().into(),
            local: matches!(m.source, Source::Local),
        };
        // Primero los activos en su orden de carga; después los apagados, por nombre.
        filas.push((pos.unwrap_or(usize::MAX), fila));
    }
    filas.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.nombre.to_lowercase().cmp(&b.1.nombre.to_lowercase())));
    let mut filas: Vec<crate::ModFila> = filas.into_iter().map(|(_, f)| f).collect();
    // Los que se están bajando y todavía no están instalados, al final.
    let prefijo = format!("mod:{}:", e.clave);
    let mut nuevos: Vec<_> = bajando.iter().filter(|(k, _)| k.starts_with(&prefijo)).collect();
    nuevos.sort_by(|a, b| a.0.cmp(b.0));
    for (k, (fase, progreso, nombre, version)) in nuevos {
        let id = &k[prefijo.len()..];
        if filas.iter().any(|f| f.id == id) {
            continue;
        }
        filas.push(crate::ModFila {
            id: id.into(),
            nombre: nombre.as_str().into(),
            version: version.as_str().into(),
            fase: *fase,
            progreso: *progreso,
            ..Default::default()
        });
    }

    let mut problemas = Vec::new();
    for pr in &e.resolucion.problems {
        let en_lista = |id: &ModId| remoto(id).is_some();
        let pm = match pr {
            Problem::MissingDependency { module, needs, version } => crate::ProblemaMod {
                codigo: 1,
                a: nombre(module).into(),
                b: nombre(needs).into(),
                c: version.as_ref().map(|v| v.to_string()).unwrap_or_default().into(),
                accion: if en_lista(needs) { 1 } else { 0 },
                id: needs.as_str().into(),
                ..Default::default()
            },
            Problem::DisabledDependency { module, needs } => crate::ProblemaMod {
                codigo: 2,
                a: nombre(module).into(),
                b: nombre(needs).into(),
                accion: 2,
                id: needs.as_str().into(),
                ..Default::default()
            },
            Problem::WrongVersion { module, needs, wants, has } => crate::ProblemaMod {
                codigo: 3,
                a: nombre(module).into(),
                b: nombre(needs).into(),
                c: wants.to_string().into(),
                d: has.as_str().into(),
                accion: if en_lista(needs) { 3 } else { 0 },
                id: needs.as_str().into(),
                ..Default::default()
            },
            Problem::Incompatible { a, b, reason } => crate::ProblemaMod {
                codigo: 4,
                a: nombre(a).into(),
                b: nombre(b).into(),
                c: reason.as_str().into(),
                ..Default::default()
            },
            Problem::Cycle { mods } => crate::ProblemaMod {
                codigo: 5,
                a: mods.iter().map(nombre).collect::<Vec<_>>().join(", ").into(),
                ..Default::default()
            },
            // Dos botones: «Que gane» el primero de los otros, o «Que siga ganando» el de ahora.
            Problem::FileConflict { mods, files, winner, resolved: false } => {
                let otro = mods.iter().find(|m| *m != winner).cloned().unwrap_or_else(|| winner.clone());
                crate::ProblemaMod {
                    codigo: 6,
                    a: mods.iter().filter(|m| *m != winner).map(nombre).collect::<Vec<_>>().join(", ").into(),
                    b: nombre(winner).into(),
                    c: files.len().to_string().into(),
                    d: files.first().map(|f| f.as_str()).unwrap_or_default().into(),
                    e: nombre(&otro).into(),
                    accion: 4,
                    id: otro.as_str().into(),
                    accion2: 5,
                    id2: winner.as_str().into(),
                    ids: mods.iter().map(|m| m.as_str()).collect::<Vec<_>>().join("|").into(),
                }
            }
            Problem::FileConflict { resolved: true, .. } => continue,
        };
        problemas.push(pm);
    }
    let perfiles = e.perfiles.list.iter().map(|p| p.name.as_str().into()).collect();
    Vista { filas, problemas, perfiles, activo: e.perfiles.active.clone() }
}

/// Un juego al que Unricopie sabe ponerle mods, con su lista de Thunderstore y cómo están sus
/// mods (si ya llegó del hilo).
pub struct Fuente<'a> {
    pub juego_id: String,
    pub juego: String,
    pub color: slint::Color,
    pub tinta: slint::Color,
    /// El proyecto (la receta con `mods`) y la carpeta de sus mods.
    pub entry: String,
    pub clave: String,
    /// El juego está instalado: se le pueden poner.
    pub instalado: bool,
    pub estado: Option<&'a Estado>,
    pub lista: &'a [RemoteMod],
}

/// Los mods de Thunderstore de esas fuentes, juntos: sin los retirados ni los de contenido para
/// adultos, de los más bajados a los menos, con lo que busca el jugador. En Explorar
/// (`explorar`), el de un juego sin instalar lleva a la página del juego (estado 4); en la
/// pestaña del juego, el botón sale apagado.
///
/// `orden`: 0 los más bajados, 1 los recién actualizados, 2 por nombre. `pasa` filtra lo demás
/// (el tipo, si el juego o el mod están instalados).
pub fn remotos(
    fuentes: &[&Fuente],
    q: &str,
    bajando: &Bajando,
    idioma: &str,
    explorar: bool,
    orden: i32,
    pasa: &dyn Fn(&Fuente, &RemoteMod) -> bool,
) -> Vec<crate::ModRemoto> {
    let q = q.trim().to_lowercase();
    let mut v: Vec<(&Fuente, &RemoteMod)> = fuentes
        .iter()
        .flat_map(|f| f.lista.iter().map(move |m| (*f, m)))
        .filter(|(_, m)| !m.deprecated && !m.info.nsfw)
        .filter(|(f, m)| pasa(f, m))
        .filter(|(_, m)| {
            q.is_empty()
                || m.name.to_lowercase().contains(&q)
                || m.author.to_lowercase().contains(&q)
                || m.info.description.to_lowercase().contains(&q)
        })
        .collect();
    match orden {
        1 => v.sort_by(|(_, a), (_, b)| b.info.updated.cmp(&a.info.updated).then_with(|| a.name.cmp(&b.name))),
        2 => v.sort_by_key(|(_, m)| m.name.to_lowercase()),
        _ => v.sort_by(|(_, a), (_, b)| b.info.downloads.cmp(&a.info.downloads).then_with(|| a.name.cmp(&b.name))),
    }
    v.into_iter()
        .map(|(f, m)| {
            let instalado = f.estado.and_then(|e| e.instalados.iter().find(|i| i.id == m.id));
            let estado = if bajando.get(&bajada(&f.clave, &m.id)).is_some_and(|(fase, ..)| (0..=2).contains(fase)) {
                2
            } else if explorar && !f.instalado {
                4
            } else {
                match instalado {
                    Some(i) if mas_nueva(&i.version, &m.version) => 3,
                    Some(_) => 1,
                    None => 0,
                }
            };
            crate::ModRemoto {
                id: m.id.as_str().into(),
                nombre: m.name.as_str().into(),
                autor: m.author.as_str().into(),
                version: m.version.as_str().into(),
                desc: m.info.description.as_str().into(),
                descargas: miles(m.info.downloads, idioma).into(),
                tam: m.files.first().and_then(|f| f.size).map(|b| crate::tam(b, idioma)).unwrap_or_default().into(),
                biblioteca: m.info.categories.iter().any(|c| c == "libraries"),
                estado,
                pagina: m.page_url.as_str().into(),
                juego_id: f.juego_id.as_str().into(),
                juego: f.juego.as_str().into(),
                entry: f.entry.as_str().into(),
                color: f.color,
                tinta: f.tinta,
                icono: m.info.icon.as_deref().unwrap_or_default().into(),
            }
        })
        .collect()
}

// ---------- Los iconos ----------

/// Dónde se guarda el icono de `url` (`<raíz>/datos/arte/mods/<archivo>.png`). Solo los de
/// Thunderstore: la dirección viene de su lista y no se baja nada de otro sitio.
pub fn ruta_icono(raiz: &Path, url: &str) -> Option<PathBuf> {
    let resto = url.strip_prefix("https://ccdn.thunderstore.io/").or_else(|| url.strip_prefix("https://gcdn.thunderstore.io/"))?;
    let nombre = resto.rsplit('/').next()?;
    let nombre: String = nombre.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    if !nombre.to_lowercase().ends_with(".png") || nombre.starts_with('.') {
        return None;
    }
    Some(raiz.join("datos").join("arte").join("mods").join(nombre))
}

/// El hilo que baja los iconos que piden las tarjetas, de a uno. Avisa a la ventana como mucho
/// cada medio segundo, y al terminar cada tanda.
pub fn iconos() -> Sender<(String, PathBuf)> {
    let (tx, rx) = mpsc::channel::<(String, PathBuf)>();
    std::thread::spawn(move || {
        let agent = net::agent();
        let mut ultimo = std::time::Instant::now();
        let mut nuevos = false;
        loop {
            // Con algo nuevo y la cola vacía, se avisa; si no, se espera al siguiente.
            let siguiente = if nuevos {
                match rx.try_recv() {
                    Ok(x) => Some(x),
                    Err(mpsc::TryRecvError::Empty) => {
                        avisar(Aviso::Iconos);
                        nuevos = false;
                        rx.recv().ok()
                    }
                    Err(mpsc::TryRecvError::Disconnected) => None,
                }
            } else {
                rx.recv().ok()
            };
            let Some((url, dest)) = siguiente else { break };
            if dest.is_file() {
                continue;
            }
            if let Some(dir) = dest.parent() {
                let _ = fs::create_dir_all(dir);
            }
            // Un icono pesa unas decenas de KB; 2 MB es de sobra.
            match net::download(&agent, &url, &dest, 2 << 20, None, &mut |_, _| {}) {
                Ok(_) => nuevos = true,
                Err(e) => eprintln!("no se pudo bajar el icono {url}: {e}"),
            }
            if nuevos && ultimo.elapsed() >= std::time::Duration::from_millis(500) {
                avisar(Aviso::Iconos);
                ultimo = std::time::Instant::now();
                nuevos = false;
            }
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeros_y_nombres() {
        assert_eq!(miles(50624, "es"), "50.624");
        assert_eq!(miles(50624, "en"), "50,624");
        assert_eq!(miles(999, "es"), "999");
        assert_eq!(miles(1_234_567, "es"), "1.234.567");
        assert_eq!(nombre_de_id(&ModId::thunderstore("LT_Schmiddy-EZ_Text_Replacer_API")), "EZ Text Replacer API");
        assert!(mas_nueva("4.0.2", "4.0.3") && !mas_nueva("4.0.3", "4.0.3") && !mas_nueva("4.1.0", "4.0.3"));
    }
}
