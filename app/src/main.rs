//! Unricopie: la interfaz, en Slint, sobre el núcleo (`unricopie-nucleo`).
//!
//! Para probarla:
//! - `UNRICOPIE_CATALOGO`: un catálogo de prueba (un JSON con las fichas y sus recetas) en vez
//!   del que se baja de recompendium.com (`datos.rs`).
//! - `UNRICOPIE_CAPTURA=ruta.png`: guarda una imagen de la ventana y cierra
//!   (`UNRICOPIE_CAPTURA_MS`: cuánto esperar antes; 1500 ms si no se dice).
//! - `UNRICOPIE_SITIO`: otra dirección para los datos (`catalogo.json`, `version.json`…), para
//!   probar contra el sitio en local.
//! - `UNRICOPIE_PRUEBA_SOLTAR=<ruta>[;<ruta>…]`: con la pestaña Mods abierta, añade esos archivos
//!   como mods propios, como si se soltaran en la ventana.
//! - `UNRICOPIE_PRUEBA_INSTALAR=<id de mod>`: con la pestaña Mods abierta, lo instala (con sus
//!   dependencias) en cuanto llega la lista de Thunderstore, por el mismo camino que el botón.
//! - `UNRICOPIE_PRUEBA_ACTUALIZAR=<ficha>`: actualiza ese juego en cuanto se sabe su última
//!   versión, como «Actualizar a …»; `UNRICOPIE_PRUEBA_VOLVER=<ficha>`: al abrir, vuelve a su
//!   versión anterior más reciente, como «Volver a esta».
//! - `UNRICOPIE_VISTA`: `biblioteca`, `explorar[:mods][:todos]`, `descargas`, `ajustes`, `tienda:<id>` o `juego:<id>[:<pestaña>[:buscar]]` (`buscar`: «Buscar mods» en la pestaña Mods).
//! - `UNRICOPIE_TEMA`: `noche`, `dia` o `contraste`; `UNRICOPIE_IDIOMA`: `es` o `en`. Los dos
//!   valen solo para esa vez (no se guardan).
// `deny` y no `forbid`: el código que genera Slint usa `unsafe` por dentro y lo permite para sí;
// en el nuestro sigue prohibido.
#![deny(unsafe_code)]
// Sin consola en Windows al abrirlo con doble clic (en debug sí, para ver los errores).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ajustes;
mod catalogo;
mod datos;
mod errores;
mod explorar;
mod ficha_mod;
mod mods;
mod opiniones;
mod tiempo;
mod trabajo;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use slint::{Color, Image, Model, ModelRc, SharedString, VecModel};
use trabajo::{codigo, Aviso, Tarea};
use unricopie_nucleo::install::Library;
use unricopie_nucleo::mods::sources::RemoteMod;
use unricopie_nucleo::mods::{ModId, RelPath};
use unricopie_nucleo::root::Root;

slint::include_modules!();

fn hex(c: &str) -> Color {
    let v = u32::from_str_radix(c.trim_start_matches('#'), 16).unwrap_or(0x5d6a99);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Tamaños como los dice la gente: «350 MB», «2,2 GB» (con punto en inglés).
fn tam(b: u64, idioma: &str) -> String {
    let mb = b as f64 / 1_048_576.0;
    let s = if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    };
    if idioma == "en" {
        s
    } else {
        s.replace('.', ",")
    }
}

fn error_ui(e: &errores::Datos) -> ErrorUi {
    ErrorUi { codigo: e.codigo, a: e.a.as_str().into(), b: e.b.as_str().into(), c: e.c.as_str().into(), n: e.n }
}

/// Una descarga de la cola, como la ve la ventana.
struct Bajada {
    entry: String,
    juego: String,
    proyecto: String,
    version: String,
    bajado: Option<u64>,
    total: Option<u64>,
    error: errores::Datos,
    progreso: f32,
    // 0 en cola, 1 bajando, 2 descomprimiendo, 3 hecha, 4 falló
    fase: i32,
    /// Es un mod (de `autor`) y no un juego.
    es_mod: bool,
    autor: String,
    /// Es la versión nueva de un juego ya instalado.
    actualiza: bool,
}

impl Bajada {
    fn activa(&self) -> bool {
        self.fase <= 2
    }
}

/// Todo lo que la ventana muestra, en su hilo.
struct Estado {
    ui: slint::Weak<AppWindow>,
    raiz: Option<PathBuf>,
    biblioteca: Option<Library>,
    /// `es` o `en`.
    idioma: String,
    catalogo: Vec<catalogo::Juego>,
    /// Los juegos ya pasados a Slint, con su consola para los filtros.
    juegos: Vec<(String, Juego)>,
    imagenes: HashMap<PathBuf, Image>,
    /// Biblioteca: 0 instalado hace poco, 1 nombre, 2 consola.
    orden_biblioteca: i32,
    busqueda: String,
    bajadas: Vec<Bajada>,
    cola: Option<Sender<Tarea>>,
    opiniones: HashMap<String, opiniones::Resumen>,
    /// El hilo de los mods (`mods::hilo`).
    mods_tx: Sender<mods::Orden>,
    /// El proyecto cuya pestaña Mods se ve, y cómo están los mods de cada juego (lo último que
    /// mandó el hilo, por carpeta del juego).
    mods_entry: String,
    mods_estados: HashMap<String, mods::Estado>,
    /// Explorar: los filtros, el orden y lo buscado en «Ver todos», de Juegos y de Mods.
    ex_juegos: explorar::Filtros,
    ex_mods: explorar::Filtros,
    /// Las listas de Thunderstore por comunidad, y cómo va cada una (0 cargando, 1 lista, 2 error).
    listas: HashMap<String, Vec<RemoteMod>>,
    listas_estado: HashMap<String, i32>,
    /// Lo que se busca en «Buscar mods», y cuántos resultados se dibujan (de a 50).
    mods_q: String,
    mods_ver: usize,
    /// La última versión oficial de cada juego instalado, por ficha (`trabajo::buscar_ultimas`).
    ultimas: HashMap<String, String>,
    /// Cuánto se jugó cada versión (`tiempo.rs`), para saber cuándo preguntar «¿Cómo te fue?».
    tiempos: tiempo::Tiempos,
    /// La ficha de mod abierta: el proyecto del juego y el id del mod.
    ficha_mod: Option<(String, String)>,
    /// El proyecto que está cambiando de versión y la versión guardada a la que pasa.
    cambiando: Option<(String, String)>,
}

thread_local! {
    static ESTADO: RefCell<Option<Estado>> = const { RefCell::new(None) };
    static PORTADAS: RefCell<CachePortadas> = RefCell::new(CachePortadas::default());
    static ICONOS: RefCell<CacheIconos> = RefCell::new(CacheIconos::default());
}

/// Los iconos de los mods que piden las tarjetas (`Iconos.icono(url, listos)`): los que ya están
/// en disco se cargan (las últimas, hasta `TOPE`); los que no, se piden una vez al hilo de
/// `mods::iconos`.
#[derive(Default)]
struct CacheIconos {
    raiz: Option<PathBuf>,
    tx: Option<Sender<(String, PathBuf)>>,
    hechas: HashMap<String, (Image, u64)>,
    pedidas: std::collections::HashSet<String>,
    reloj: u64,
}

impl CacheIconos {
    const TOPE: usize = 400;

    fn icono(&mut self, url: &str) -> Image {
        self.reloj += 1;
        if let Some((img, uso)) = self.hechas.get_mut(url) {
            *uso = self.reloj;
            return img.clone();
        }
        let Some(ruta) = self.raiz.as_deref().and_then(|r| mods::ruta_icono(r, url)) else { return Image::default() };
        if !ruta.is_file() {
            if self.pedidas.insert(url.to_string()) {
                if let Some(tx) = &self.tx {
                    let _ = tx.send((url.to_string(), ruta));
                }
            }
            return Image::default();
        }
        let Ok(img) = Image::load_from_path(&ruta) else { return Image::default() };
        if self.hechas.len() >= Self::TOPE {
            if let Some(vieja) = self.hechas.iter().min_by_key(|(_, (_, u))| *u).map(|(k, _)| k.clone()) {
                self.hechas.remove(&vieja);
            }
        }
        self.hechas.insert(url.to_string(), (img.clone(), self.reloj));
        img
    }
}

/// Las portadas que pide la ventana (`Portadas.portada(id)`): de qué archivo sale la de cada
/// juego y las últimas decodificadas, hasta `TOPE`. Va aparte de `Estado` porque Slint la
/// llama mientras dibuja.
#[derive(Default)]
struct CachePortadas {
    rutas: HashMap<String, PathBuf>,
    hechas: HashMap<PathBuf, (Image, u64)>,
    reloj: u64,
}

impl CachePortadas {
    const TOPE: usize = 300;

    fn portada(&mut self, id: &str) -> Image {
        let Some(ruta) = self.rutas.get(id).cloned() else { return Image::default() };
        self.reloj += 1;
        if let Some((img, uso)) = self.hechas.get_mut(&ruta) {
            *uso = self.reloj;
            return img.clone();
        }
        if !ruta.is_file() {
            return Image::default();
        }
        let Ok(img) = Image::load_from_path(&ruta) else { return Image::default() };
        if self.hechas.len() >= Self::TOPE {
            // Fuera la que hace más que no se ve.
            if let Some(vieja) = self.hechas.iter().min_by_key(|(_, (_, u))| *u).map(|(k, _)| k.clone()) {
                self.hechas.remove(&vieja);
            }
        }
        self.hechas.insert(ruta, (img.clone(), self.reloj));
        img
    }
}

/// Hace algo con el estado y la ventana, desde el hilo de la ventana.
fn con_estado(f: impl FnOnce(&mut Estado, &AppWindow)) {
    ESTADO.with(|e| {
        if let Some(st) = e.borrow_mut().as_mut() {
            if let Some(ui) = st.ui.upgrade() {
                f(st, &ui);
            }
        }
    });
}

fn aviso(ui: &AppWindow, codigo: i32, a: &str, error: &errores::Datos) {
    ui.set_aviso(codigo);
    ui.set_aviso_a(a.into());
    ui.set_aviso_error(error_ui(error));
}

impl Estado {
    fn imagen(&mut self, p: &Option<PathBuf>) -> Option<Image> {
        let p = p.as_ref()?;
        if let Some(i) = self.imagenes.get(p) {
            return Some(i.clone());
        }
        // Una portada que todavía no se bajó: se muestra la caja con el color de su consola.
        if !p.is_file() {
            return None;
        }
        let i = Image::load_from_path(p).ok()?;
        self.imagenes.insert(p.clone(), i.clone());
        Some(i)
    }

    /// Lo que la ventana necesita de un juego, con los textos de la ficha en el idioma elegido.
    /// Las imágenes solo se cargan con `imagenes` (el juego abierto): en las listas, cada caja
    /// pide su portada a `Portadas`.
    fn a_slint(&mut self, k: usize, imagenes: bool) -> Juego {
        let arte = {
            let e = self.catalogo[k].primero();
            [e.arte.portada.clone(), e.arte.fondo.clone(), e.arte.logo.clone(), e.arte.icono.clone()]
        };
        let con_portada = arte[0].as_ref().is_some_and(|p| p.is_file());
        let [portada, fondo, logo, icono] = if imagenes { arte.map(|p| self.imagen(&p)) } else { [None, None, None, None] };
        // Las capturas, también solo en el juego abierto (la ficha de Explorar).
        let capturas: Vec<Image> = if imagenes {
            let rutas: Vec<Option<PathBuf>> = self.catalogo[k].primero().arte.capturas.iter().cloned().map(Some).collect();
            rutas.iter().filter_map(|r| self.imagen(r)).collect()
        } else {
            Vec::new()
        };
        let idioma = self.idioma.as_str();
        let j = &self.catalogo[k];
        let e = j.primero();
        let bajando: Vec<&str> = self.bajadas.iter().filter(|b| b.activa()).map(|b| b.entry.as_str()).collect();
        let ahora = tiempo::ahora();
        let proyectos: Vec<Proyecto> = j
            .proyectos
            .iter()
            .map(|p| {
                let copia = self.biblioteca.as_ref().map(|b| p.copia(b));
                let op = self.opiniones.get(&p.e.entry).cloned().unwrap_or_default();
                let inst = p.instalado.as_ref();
                let jugado = inst.and_then(|(_, r)| self.tiempos.get(&tiempo::clave(&p.e.entry, &r.version)));
                // Lo que piden sus mods activos, para avisar en Versiones (solo en el juego abierto).
                let minimos: Vec<(String, String)> = match (imagenes, inst) {
                    (true, Some(_)) => self
                        .ctx_mods(&p.e.entry)
                        .and_then(|c| unricopie_nucleo::mods::game::GameMods::new(&c.raiz, &c.clave, &c.entry).minimum_versions().ok())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let aviso_instalada = inst.map(|(_, r)| aviso_mods(&minimos, &r.version)).unwrap_or_default();
                Proyecto {
                    entry: p.e.entry.as_str().into(),
                    nombre: p.nombre().into(),
                    metodo: p.e.metodo.as_str().into(),
                    toolchain: p.e.toolchain.as_str().into(),
                    autor: p.e.autor.as_str().into(),
                    repo: p.e.repo.as_str().into(),
                    nivel: p.nivel(),
                    recomendado: p.e.recomendado,
                    motivo: p.e.motivo.en(idioma).into(),
                    desc: p.e.desc.en(idioma).into(),
                    estrellas: if p.e.estrellas > 0 { mods::miles(p.e.estrellas as u64, idioma).into() } else { SharedString::new() },
                    requisitos: p.e.requisitos.en(idioma).into(),
                    copia: copia.as_ref().map_or(4, |c| c.codigo),
                    copia_nombre: copia.as_ref().map(|c| c.nombre.as_str()).unwrap_or_default().into(),
                    copia_falta: copia.as_ref().is_some_and(|c| c.falta),
                    pide_copia: p.pide_copia(),
                    copia_carpeta: p.copia_es_carpeta(),
                    instalable: p.instalable() && self.biblioteca.is_some(),
                    con_receta: p.con_receta(),
                    con_mods: p.receta.as_ref().is_some_and(|r| r.mods.is_some()),
                    sin_enlace: p.e.sin_enlace,
                    instalado: inst.is_some(),
                    bajando: bajando.contains(&p.e.entry.as_str()),
                    version: inst.map(|(_, r)| r.version.as_str()).unwrap_or_default().into(),
                    ultima: p.e.version.as_str().into(),
                    fecha_ultima: fecha_iso(&p.e.fecha, idioma).into(),
                    nueva: match (inst, self.ultimas.get(&p.e.entry)) {
                        (Some((_, r)), Some(u)) if *u != r.version => u.as_str().into(),
                        _ => SharedString::new(),
                    },
                    anteriores: ModelRc::new(VecModel::from(match (inst, &self.biblioteca) {
                        (Some((dir, _)), Some(bib)) if imagenes => anteriores(bib, dir, idioma, &minimos),
                        _ => Vec::new(),
                    })),
                    instalada_el: inst.map(|(_, r)| fecha(r.installed_at, idioma)).unwrap_or_default().into(),
                    cambiando: self.cambiando.as_ref().filter(|(e, _)| e == &p.e.entry).map(|(_, id)| id.as_str()).unwrap_or_default().into(),
                    aviso_mod: aviso_instalada.0.into(),
                    aviso_min: aviso_instalada.1.into(),
                    aviso_mas: aviso_instalada.2,
                    canal: inst.map_or(1, |(_, r)| errores::canal(r.channel)),
                    carpeta: inst.map(|(d, _)| d.display().to_string()).unwrap_or_default().into(),
                    portable: inst.is_some_and(|(_, r)| r.portable.is_some()),
                    op_abierto: jugado.is_some(),
                    op_listo: jugado.is_some_and(|t| t.listo(ahora)),
                    op_total: op.total,
                    op_bien: op.bien,
                    op_problemas: op.problemas,
                    op_no: op.no,
                }
            })
            .collect();
        let sub = j.instalado().map(|p| {
            let v = p.instalado.as_ref().map(|(_, r)| r.version.as_str()).unwrap_or_default();
            format!("{}, {v}", p.nombre())
        });
        let autor = |t: &str| SharedString::from(e.arte.autores.get(t).map(String::as_str).unwrap_or_default());
        let cuenta_mods = j.proyectos.iter().filter_map(|p| p.receta.as_ref()?.mods.as_ref()?.thunderstore.as_deref()).map(|c| self.cuenta_mods(c)).max().unwrap_or(0);
        Juego {
            id: j.id.as_str().into(),
            nombre: e.nombre.as_str().into(),
            consola_nombre: e.consola_nombre.as_str().into(),
            color: hex(&e.color),
            tinta: hex(&e.tinta),
            desarrollador: e.desarrollador.as_str().into(),
            anio: e.anio,
            desc: e.desc.en(idioma).into(),
            con_arte: if imagenes { portada.is_some() } else { con_portada },
            con_fondo: fondo.is_some(),
            con_logo: logo.is_some(),
            portada: portada.unwrap_or_default(),
            fondo: fondo.unwrap_or_default(),
            logo: logo.unwrap_or_default(),
            icono: icono.unwrap_or_default(),
            capturas: ModelRc::new(VecModel::from(capturas)),
            arte: match e.arte.fuente.as_str() {
                "steamgriddb" => 1,
                "igdb" => 2,
                _ => 0,
            },
            arte_portada: autor("portada"),
            arte_fondo: autor("fondo"),
            arte_logo: autor("logo"),
            arte_icono: autor("icono"),
            instalado: j.instalado().is_some(),
            mods: cuenta_mods,
            nivel: j.proyectos[0].nivel(),
            sub: sub.unwrap_or_default().into(),
            proyectos: ModelRc::new(VecModel::from(proyectos)),
        }
    }

    /// Vuelve a pasar todos los juegos a Slint y a llenar la ventana.
    fn reconstruir(&mut self, ui: &AppWindow) {
        self.juegos = (0..self.catalogo.len()).map(|k| (self.catalogo[k].primero().consola.clone(), self.a_slint(k, false))).collect();
        PORTADAS.with(|p| {
            p.borrow_mut().rutas = self.catalogo.iter().filter_map(|j| Some((j.id.clone(), j.primero().arte.portada.clone()?))).collect();
        });
        self.refrescar(ui);
        // El juego abierto, con lo nuevo (instalado, copia, idioma), sin cambiar de proyecto ni de pestaña.
        let id = ui.get_actual().id;
        if let Some(k) = self.catalogo.iter().position(|j| j.id == id.as_str()) {
            let j = self.a_slint(k, true);
            ui.set_actual(j);
        }
        self.mostrar_bajadas(ui);
    }

    fn refrescar(&self, ui: &AppWindow) {
        // Los instalados, en el orden elegido. `juegos` va en el mismo orden que `catalogo`.
        let mut instalados: Vec<(u64, &Juego)> = self
            .juegos
            .iter()
            .enumerate()
            .filter(|(_, (_, j))| j.instalado)
            .map(|(k, (_, j))| {
                let cuando = self.catalogo[k].instalado().and_then(|p| p.instalado.as_ref()).map_or(0, |(_, r)| r.installed_at);
                (cuando, j)
            })
            .collect();
        let nombre = |j: &Juego| j.nombre.to_lowercase();
        match self.orden_biblioteca {
            1 => instalados.sort_by_key(|(_, j)| nombre(j)),
            2 => instalados.sort_by(|(_, a), (_, b)| a.consola_nombre.cmp(&b.consola_nombre).then_with(|| nombre(a).cmp(&nombre(b)))),
            _ => instalados.sort_by(|(x, a), (y, b)| y.cmp(x).then_with(|| nombre(a).cmp(&nombre(b)))),
        }
        let biblioteca: Vec<Juego> = instalados.iter().map(|(_, j)| (*j).clone()).collect();
        let q = self.busqueda.to_lowercase();
        let mut lateral: Vec<Juego> = biblioteca.iter().filter(|j| j.nombre.to_lowercase().contains(&q)).cloned().collect();
        lateral.sort_by_key(|j| j.nombre.to_lowercase());
        ui.set_instalados(biblioteca.len() as i32);
        ui.set_biblioteca(ModelRc::new(VecModel::from(biblioteca)));
        ui.set_orden_biblioteca(self.orden_biblioteca);
        ui.set_lateral(ModelRc::new(VecModel::from(lateral)));
        ui.set_total_catalogo(self.juegos.len() as i32);
        ui.set_instalables(self.juegos.iter().filter(|(_, j)| j.proyectos.iter().any(|p| p.instalable)).count() as i32);
        explorar::mostrar(self, ui);
    }

    /// Los filtros de Explorar del modo que se ve.
    fn filtros_ex(&mut self, ui: &AppWindow) -> &mut explorar::Filtros {
        if ui.global::<ExplorarUi>().get_mods() {
            &mut self.ex_mods
        } else {
            &mut self.ex_juegos
        }
    }

    fn abrir(&mut self, ui: &AppWindow, id: &str) {
        if let Some(k) = self.catalogo.iter().position(|j| j.id == id) {
            let j = self.a_slint(k, true);
            ui.set_actual(j);
            // Si hay un proyecto instalado, la página abre en ese.
            let instalado = self.catalogo[k].proyectos.iter().position(|p| p.instalado.is_some());
            ui.set_proyecto(instalado.unwrap_or(0) as i32);
            ui.set_pestana(0);
            self.mods_entry.clear();
            ui.global::<ModsUi>().set_buscando(false);
            self.mostrar_mods(ui);
            aviso(ui, 0, "", &errores::Datos::default());
            if ui.get_vista() != Vista::Juego {
                ui.set_desde(ui.get_vista());
            }
            ui.set_vista(Vista::Juego);
        }
    }

    /// La página Mods: los juegos instalados con mods y los mods del elegido (`entry` lo elige;
    /// si no, sigue el que estaba o el primero).
    fn mostrar_pagina_mods(&mut self, ui: &AppWindow, entry: Option<&str>) {
        let mut juegos: Vec<(Juego, String)> = Vec::new();
        for k in 0..self.catalogo.len() {
            let Some(p) = self.catalogo[k].proyectos.iter().find(|p| p.instalado.is_some() && p.receta.as_ref().is_some_and(|r| r.mods.is_some())) else { continue };
            let e = p.e.entry.clone();
            juegos.push((self.a_slint(k, false), e));
        }
        let elegido = entry
            .and_then(|e| juegos.iter().find(|(_, x)| x == e))
            .or_else(|| juegos.iter().find(|(j, _)| j.id == ui.get_mods_elegido()))
            .or_else(|| juegos.first())
            .map(|(j, e)| (j.id.clone(), e.clone(), j.nombre.clone(), j.proyectos.iter().find(|p| p.entry.as_str() == e.as_str())));
        ui.set_mods_juegos(ModelRc::new(VecModel::from(juegos.into_iter().map(|(j, _)| j).collect::<Vec<_>>())));
        if let Some((id, e, nombre, p)) = elegido {
            ui.set_mods_elegido(id);
            ui.set_mods_juego(nombre);
            if let Some(p) = p {
                ui.set_mods_proyecto(p);
            }
            self.ver_mods(ui, &e);
        }
    }

    /// Busca la última versión oficial de cada juego instalado (en un hilo).
    fn buscar_ultimas(&self) {
        let Some(bib) = self.biblioteca.clone() else { return };
        let instalados = self
            .catalogo
            .iter()
            .flat_map(|j| j.proyectos.iter())
            .filter(|p| p.instalado.is_some())
            .filter_map(|p| Some((p.e.entry.clone(), p.receta.clone()?)))
            .collect();
        trabajo::buscar_ultimas(bib, instalados);
    }

    /// Una versión anterior guardada de `entry`: la `id` pedida o, sin ella, la más reciente. Con
    /// la biblioteca, la carpeta del juego, su `id` y su versión (para los mensajes).
    fn anterior(&self, entry: &str, id: Option<&str>) -> Option<(Library, PathBuf, String, String)> {
        let bib = self.biblioteca.clone()?;
        let (_, p) = self.proyecto(entry)?;
        let (dir, _) = p.instalado.clone()?;
        let a = unricopie_nucleo::install::update::previous_versions(&bib, &dir).ok()?.into_iter().find(|a| id.is_none_or(|id| a.id == id))?;
        Some((bib, dir, a.id, a.receipt.version))
    }

    /// «Usar esta». Con mods, en su hilo: los quita antes y los vuelve a poner después. Mientras
    /// cambia, la pestaña Versiones lo muestra y no deja pedir otro cambio.
    fn volver(&mut self, ui: &AppWindow, entry: &str, id: Option<&str>) {
        if self.cambiando.is_some() {
            return;
        }
        let Some((bib, dir, id, version)) = self.anterior(entry, id) else { return };
        self.cambiando = Some((entry.to_string(), id.clone()));
        self.reconstruir(ui);
        match self.ctx_mods(entry) {
            Some(ctx) => {
                if self.mods_tx.send(mods::Orden::Volver { ctx, biblioteca: bib, dir, id, version }).is_err() {
                    aviso(ui, codigo::COLA_NO_RESPONDE, "", &errores::Datos::default());
                }
            }
            None => trabajo::volver(bib, dir, id, version),
        }
    }

    /// «Actualizar a …»: la versión nueva se baja en Descargas, como una instalación.
    fn actualizar(&mut self, ui: &AppWindow, entry: &str) {
        let Some((j, p)) = self.proyecto(entry) else { return };
        if p.instalado.is_none() || self.bajadas.iter().any(|b| b.entry == entry && b.activa()) {
            return;
        }
        let Some(receta) = p.receta.clone() else { return };
        let (juego, nombre) = (j.id.clone(), p.nombre());
        let mods = self.ctx_mods(entry).map(Box::new);
        let Some(cola) = &self.cola else { return };
        if cola.send(Tarea::Actualizar { entry: entry.to_string(), receta, mods }).is_err() {
            aviso(ui, codigo::COLA_NO_RESPONDE, "", &errores::Datos::default());
            return;
        }
        self.bajadas.push(Bajada {
            entry: entry.to_string(),
            juego,
            proyecto: nombre,
            version: String::new(),
            bajado: None,
            total: None,
            error: errores::Datos::default(),
            progreso: -1.0,
            fase: 0,
            es_mod: false,
            autor: String::new(),
            actualiza: true,
        });
        aviso(ui, codigo::EN_DESCARGAS, "", &errores::Datos::default());
        self.reconstruir(ui);
    }

    fn proyecto(&self, entry: &str) -> Option<(&catalogo::Juego, &catalogo::Proyecto)> {
        self.catalogo.iter().find_map(|j| j.proyectos.iter().find(|p| p.e.entry == entry).map(|p| (j, p)))
    }

    /// La lista de descargas y la barra de abajo.
    fn mostrar_bajadas(&self, ui: &AppWindow) {
        let idioma = self.idioma.as_str();
        let lista: Vec<Descarga> = self
            .bajadas
            .iter()
            .rev()
            .map(|b| {
                let j = self.juegos.iter().find(|(_, j)| j.id == b.juego).map(|(_, j)| j.clone()).unwrap_or_default();
                Descarga {
                    es_mod: b.es_mod,
                    actualiza: b.actualiza,
                    autor: b.autor.as_str().into(),
                    entry: b.entry.as_str().into(),
                    juego_id: b.juego.as_str().into(),
                    titulo: format!("{} {}", b.proyecto, b.version).trim().into(),
                    juego: j.nombre.clone(),
                    fase: b.fase,
                    bajado: b.bajado.map(|x| tam(x, idioma)).unwrap_or_default().into(),
                    total: b.total.map(|x| tam(x, idioma)).unwrap_or_default().into(),
                    error: error_ui(&b.error),
                    progreso: b.progreso,
                    portada: j.portada.clone(),
                    con_arte: j.con_arte,
                    color: j.color,
                }
            })
            .collect();
        ui.set_descargas(ModelRc::new(VecModel::from(lista)));
        let activa = self.bajadas.iter().find(|b| b.fase == 1 || b.fase == 2);
        ui.set_descargando(activa.is_some());
        ui.set_descarga_titulo(activa.map(|b| format!("{} {}", b.proyecto, b.version).trim().to_string()).unwrap_or_default().into());
        ui.set_progreso(activa.map_or(-1.0, |b| b.progreso));
        ui.set_en_cola(self.bajadas.iter().filter(|b| b.fase == 0).count() as i32);
    }

    /// De qué juego son los mods del proyecto `entry`, si su receta tiene mods.
    fn ctx_mods(&self, entry: &str) -> Option<mods::Ctx> {
        let (_, p) = self.proyecto(entry)?;
        let receta = p.receta.as_ref()?;
        let regla = receta.mods.clone()?;
        let raiz = self.raiz.clone()?;
        let (clave, destino) = match &p.instalado {
            Some((dir, recibo)) => (dir.file_name()?.to_string_lossy().into_owned(), unricopie_nucleo::mods::game::target(receta, recibo, dir)),
            None => (self.biblioteca.as_ref()?.game_dir(receta, None).file_name()?.to_string_lossy().into_owned(), None),
        };
        Some(mods::Ctx { clave, entry: entry.to_string(), raiz, regla, destino })
    }

    /// La pestaña Mods de `entry`: pide cómo están sus mods y la lista de su comunidad.
    fn ver_mods(&mut self, ui: &AppWindow, entry: &str) {
        if self.mods_entry != entry {
            self.mods_q.clear();
            // Otro proyecto del mismo juego: vuelve a tu lista. (Al abrir otro juego lo hace `abrir`.)
            if !self.mods_entry.is_empty() {
                ui.global::<ModsUi>().set_buscando(false);
            }
        }
        self.mods_entry = entry.to_string();
        let Some(ctx) = self.ctx_mods(entry) else {
            self.mostrar_mods(ui);
            return;
        };
        if let Some(c) = ctx.regla.thunderstore.clone() {
            if !self.listas_estado.contains_key(&c) {
                self.listas_estado.insert(c.clone(), 0);
                mods::lista(ctx.raiz.clone(), c, false);
            }
        }
        ui.global::<ModsUi>().set_carpeta(ctx.destino.as_ref().map(|t| t.dir.display().to_string()).unwrap_or_default().into());
        ui.global::<ModsUi>().set_instalado(ctx.destino.is_some());
        let _ = self.mods_tx.send(mods::Orden::Ver(ctx));
        self.mostrar_mods(ui);
    }

    /// Manda una orden sobre los mods del proyecto que se ve.
    fn orden_mods(&self, f: impl FnOnce(mods::Ctx) -> mods::Orden) {
        if let Some(ctx) = self.ctx_mods(&self.mods_entry) {
            let _ = self.mods_tx.send(f(ctx));
        }
    }

    fn bajando_mods(&self) -> mods::Bajando {
        self.bajadas
            .iter()
            .filter(|b| b.es_mod && b.activa())
            .map(|b| (b.entry.clone(), (b.fase, b.progreso, b.proyecto.clone(), b.version.clone())))
            .collect()
    }

    /// Instala un mod de la lista de Thunderstore en el juego del proyecto `entry`, con las
    /// dependencias que le falten.
    fn instalar_mod(&mut self, ui: &AppWindow, entry: &str, id: &str) {
        let Some(ctx) = self.ctx_mods(entry) else { return };
        if ctx.destino.is_none() {
            return;
        }
        let Some(lista) = ctx.regla.thunderstore.as_ref().and_then(|c| self.listas.get(c)) else { return };
        let instalados = self.mods_estados.get(&ctx.clave).map(|m| m.instalados.clone()).unwrap_or_default();
        let todo: Vec<RemoteMod> = unricopie_nucleo::mods::game::with_dependencies(lista, &ModId(id.to_string()), &instalados).into_iter().cloned().collect();
        let juego = self.proyecto(&ctx.entry).map(|(j, _)| j.id.clone()).unwrap_or_default();
        let Some(cola) = &self.cola else { return };
        for r in todo {
            let clave = mods::bajada(&ctx.clave, &r.id);
            if self.bajadas.iter().any(|b| b.entry == clave && b.activa()) {
                continue;
            }
            if cola.send(Tarea::Mod { ctx: ctx.clone(), remoto: r.clone(), bajada: clave.clone() }).is_err() {
                aviso(ui, codigo::COLA_NO_RESPONDE, "", &errores::Datos::default());
                return;
            }
            self.bajadas.push(Bajada {
                entry: clave,
                juego: juego.clone(),
                proyecto: r.name.clone(),
                version: r.version.clone(),
                bajado: None,
                total: r.files.first().and_then(|f| f.size),
                error: errores::Datos::default(),
                progreso: -1.0,
                fase: 0,
                es_mod: true,
                actualiza: false,
                autor: r.author.clone(),
            });
        }
        self.mostrar_bajadas(ui);
        self.mostrar_mods(ui);
        explorar::mostrar(self, ui);
    }

    /// Si ahora se pueden soltar mods en la ventana: con la pestaña Mods abierta de un juego
    /// instalado que los admite. Devuelve el nombre del juego (vacío si no se puede).
    fn donde_soltar(&self, ui: &AppWindow) -> String {
        if ui.get_vista() != Vista::Juego || ui.get_pestana() != 1 {
            return String::new();
        }
        match self.ctx_mods(&self.mods_entry) {
            Some(c) if c.destino.is_some() => self.proyecto(&c.entry).map(|(j, _)| j.primero().nombre.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Los proyectos a los que Unricopie sabe ponerles mods (su receta dice de qué comunidad de
    /// Thunderstore), con su juego y la comunidad.
    fn con_mods(&self) -> Vec<(&catalogo::Juego, &catalogo::Proyecto, String)> {
        self.catalogo
            .iter()
            .flat_map(|j| j.proyectos.iter().filter_map(move |p| Some((j, p, p.receta.as_ref()?.mods.as_ref()?.thunderstore.clone()?))))
            .collect()
    }

    /// Pide las listas de Thunderstore que falten (de la caché de 6 h o de la red).
    fn pedir_listas(&mut self) {
        let Some(raiz) = self.raiz.clone() else { return };
        let comunidades: Vec<String> = self.con_mods().into_iter().map(|(_, _, c)| c).collect();
        for c in comunidades {
            if !self.listas_estado.contains_key(&c) {
                self.listas_estado.insert(c.clone(), 0);
                mods::lista(raiz.clone(), c, false);
            }
        }
    }

    /// Cuántos mods hay en una comunidad (sin retirados ni de contenido para adultos).
    fn cuenta_mods(&self, comunidad: &str) -> i32 {
        self.listas.get(comunidad).map_or(0, |l| l.iter().filter(|m| !m.deprecated && !m.info.nsfw).count() as i32)
    }

    /// Los juegos a los que Unricopie sabe ponerles mods, con su lista de Thunderstore y cómo están
    /// sus mods; y si todavía falta alguna lista.
    fn fuentes_mods(&self) -> (Vec<mods::Fuente<'_>>, bool) {
        let mut fuentes: Vec<mods::Fuente> = Vec::new();
        let mut cargando = false;
        for (j, p, c) in self.con_mods() {
            cargando |= self.listas_estado.get(&c) == Some(&0);
            let Some(lista) = self.listas.get(&c) else { continue };
            let Some(ctx) = self.ctx_mods(&p.e.entry) else { continue };
            if fuentes.iter().any(|f| f.juego_id == j.id) {
                continue;
            }
            let e = j.primero();
            fuentes.push(mods::Fuente {
                juego_id: j.id.clone(),
                juego: e.nombre.clone(),
                color: hex(&e.color),
                tinta: hex(&e.tinta),
                entry: p.e.entry.clone(),
                clave: ctx.clave.clone(),
                instalado: ctx.destino.is_some(),
                estado: self.mods_estados.get(&ctx.clave),
                lista,
            });
        }
        fuentes.sort_by_key(|f| std::cmp::Reverse(f.lista.len()));
        (fuentes, cargando)
    }

    /// Al pasar Explorar a Mods: las listas y cómo están los mods de cada juego instalado.
    fn abrir_explorar_mods(&mut self) {
        self.pedir_listas();
        let ctxs: Vec<mods::Ctx> = self.con_mods().iter().filter_map(|(_, p, _)| self.ctx_mods(&p.e.entry)).filter(|c| c.destino.is_some()).collect();
        for c in ctxs {
            let _ = self.mods_tx.send(mods::Orden::Ver(c));
        }
    }

    /// La pestaña Mods: la lista, los problemas, los perfiles y la de Thunderstore.
    /// Todo lo de la ficha del mod `id` del juego del proyecto `entry`: su fila (con su estado:
    /// instalado, bajando…), el mod de la lista, lo que necesita y los mods que lo usan.
    fn datos_mod(&self, entry: &str, id: &str) -> Option<(ModRemoto, RemoteMod, Vec<ModEnlace>, Vec<ModEnlace>)> {
        let (j, _) = self.proyecto(entry)?;
        let bajando = self.bajando_mods();
        let (fuentes, _) = self.fuentes_mods();
        let f = fuentes.iter().find(|f| f.juego_id == j.id)?;
        let m = f.lista.iter().find(|m| m.id.as_str() == id)?.clone();
        let r = mods::remotos(&[f], "", &bajando, &self.idioma, true, 0, &|_, x| x.id == m.id).pop()?;
        let enlace = |x: &RemoteMod, detalle: String| ModEnlace {
            id: x.id.as_str().into(),
            nombre: x.name.as_str().into(),
            autor: x.author.as_str().into(),
            detalle: detalle.into(),
            icono: x.info.icon.as_deref().unwrap_or_default().into(),
            en_lista: true,
        };
        let necesita = m
            .dependencies
            .iter()
            .map(|d| {
                let req = d.version.as_ref().map(|v| v.to_string()).unwrap_or_default();
                match f.lista.iter().find(|x| x.id == d.id) {
                    Some(x) => enlace(x, req),
                    None => ModEnlace {
                        id: d.id.as_str().into(),
                        nombre: d.id.as_str().trim_start_matches("thunderstore:").into(),
                        detalle: req.into(),
                        en_lista: false,
                        ..Default::default()
                    },
                }
            })
            .collect();
        let lo_usan = f
            .lista
            .iter()
            .filter(|x| !x.deprecated && x.dependencies.iter().any(|d| d.id == m.id))
            .map(|x| enlace(x, x.version.clone()))
            .collect();
        Some((r, m, necesita, lo_usan))
    }

    /// Abre la ficha de un mod (y pide su README y su registro de cambios).
    fn abrir_mod(&mut self, ui: &AppWindow, entry: &str, id: &str) {
        let entry = if entry.is_empty() { self.mods_entry.clone() } else { entry.to_string() };
        let Some((r, m, necesita, lo_usan)) = self.datos_mod(&entry, id) else { return };
        let idioma = self.idioma.clone();
        let dia = |iso: &str| fecha_iso(iso.get(..10).unwrap_or_default(), &idioma);
        let f = ui.global::<FichaMod>();
        let paquete = match &m.source {
            unricopie_nucleo::mods::Source::Thunderstore { full_name, .. } => format!("{full_name}-{}", m.version),
            _ => String::new(),
        };
        f.set_paquete(paquete.into());
        f.set_autor_url(ficha_mod::url_autor(&m).into());
        f.set_web(m.info.website.as_str().into());
        f.set_actualizado(dia(&m.info.updated).into());
        f.set_subido(dia(&m.info.created).into());
        f.set_me_gusta(mods::miles(m.info.likes, &idioma).into());
        f.set_categorias(m.info.categories.iter().map(|c| mayuscula(c)).collect::<Vec<_>>().join(", ").into());
        f.set_necesita(ModelRc::new(VecModel::from(necesita)));
        f.set_lo_usan(ModelRc::new(VecModel::from(lo_usan)));
        let versiones: Vec<VersionMod> = m
            .info
            .versions
            .iter()
            .map(|v| VersionMod {
                version: v.version.as_str().into(),
                fecha: dia(&v.date).into(),
                descargas: mods::miles(v.downloads, &idioma).into(),
                tam: v.size.map(|b| tam(b, &idioma)).unwrap_or_default().into(),
            })
            .collect();
        f.set_versiones(ModelRc::new(VecModel::from(versiones)));
        f.set_bloques(ModelRc::default());
        f.set_estado_readme(0);
        f.set_cambios(ModelRc::default());
        f.set_estado_cambios(0);
        f.set_pestana(0);
        f.set_r(r);
        // Es una página: se vuelve a donde se estaba (Explorar, Mods o el juego).
        if ui.get_vista() != Vista::FichaMod {
            ui.set_desde_mod(ui.get_vista());
        }
        ui.set_vista(Vista::FichaMod);
        self.ficha_mod = Some((entry, id.to_string()));
        ficha_mod::pedir(&m, ficha_mod::Que::Readme);
        ficha_mod::pedir(&m, ficha_mod::Que::Cambios);
    }

    /// Vuelve a calcular la fila de la ficha abierta (se instaló, se está bajando…).
    fn refrescar_ficha_mod(&self, ui: &AppWindow) {
        let f = ui.global::<FichaMod>();
        let Some((entry, id)) = self.ficha_mod.as_ref().filter(|_| ui.get_vista() == Vista::FichaMod) else { return };
        if let Some((r, ..)) = self.datos_mod(entry, id) {
            f.set_r(r);
        }
    }

    fn mostrar_mods(&self, ui: &AppWindow) {
        self.refrescar_ficha_mod(ui);
        let ctx = self.ctx_mods(&self.mods_entry);
        let comunidad = ctx.as_ref().and_then(|c| c.regla.thunderstore.clone()).unwrap_or_default();
        let lista = self.listas.get(&comunidad).map(Vec::as_slice);
        let bajando = self.bajando_mods();
        let estado = ctx.as_ref().and_then(|c| self.mods_estados.get(&c.clave));
        let v = estado.map(|e| mods::vista(e, lista, &bajando));
        let m = ui.global::<ModsUi>();
        m.set_entry(self.mods_entry.as_str().into());
        m.set_adoptados(estado.map_or(0, |e| e.adoptados as i32));
        m.set_anadidos(estado.map_or(0, |e| e.anadidos as i32));
        m.set_activos(estado.map_or(0, |e| e.resolucion.order.len() as i32));
        ui.global::<ModsUi>().set_cargado(estado.is_some());
        ui.global::<ModsUi>().set_comunidad(comunidad.as_str().into());
        ui.global::<ModsUi>().set_error(error_ui(&estado.and_then(|e| e.error.clone()).unwrap_or_default()));
        match v {
            Some(v) => {
                ui.global::<ModsUi>().set_filas(ModelRc::new(VecModel::from(v.filas)));
                ui.global::<ModsUi>().set_problemas(ModelRc::new(VecModel::from(v.problemas)));
                let indice = v.perfiles.iter().position(|p| p.as_str() == v.activo.as_str()).unwrap_or(0) as i32;
                ui.global::<ModsUi>().set_perfiles(ModelRc::new(VecModel::from(v.perfiles)));
                ui.global::<ModsUi>().set_perfil(v.activo.into());
                ui.global::<ModsUi>().set_perfil_indice(indice);
            }
            None => {
                ui.global::<ModsUi>().set_filas(ModelRc::default());
                ui.global::<ModsUi>().set_problemas(ModelRc::default());
                ui.global::<ModsUi>().set_perfiles(ModelRc::default());
                ui.global::<ModsUi>().set_perfil(SharedString::default());
            }
        }
        let mut remotos = match (&ctx, lista) {
            (Some(c), Some(l)) => {
                let f = mods::Fuente {
                    juego_id: String::new(),
                    juego: String::new(),
                    color: Color::default(),
                    tinta: Color::default(),
                    entry: c.entry.clone(),
                    clave: c.clave.clone(),
                    instalado: c.destino.is_some(),
                    estado,
                    lista: l,
                };
                mods::remotos(&[&f], &self.mods_q, &bajando, &self.idioma, false, 0, &|_, _| true)
            }
            _ => Vec::new(),
        };
        ui.global::<ModsUi>().set_total(lista.map_or(0, |l| l.iter().filter(|m| !m.deprecated && !m.info.nsfw).count() as i32));
        // Se dibujan de a 50: con todas a la vez (Zelda tiene casi 200) la ventana no da abasto.
        ui.global::<ModsUi>().set_coinciden(remotos.len() as i32);
        remotos.truncate(self.mods_ver);
        ui.global::<ModsUi>().set_remotos(ModelRc::new(VecModel::from(remotos)));
        ui.global::<ModsUi>().set_lista(*self.listas_estado.get(&comunidad).unwrap_or(&0));
    }

    /// Cambia el idioma de la ventana y de los textos de las fichas.
    fn cambiar_idioma(&mut self, ui: &AppWindow, idioma: &str) {
        // El español es el idioma en que están escritos los textos (el de origen).
        let r = slint::select_bundled_translation(if idioma == "en" { "en" } else { "" });
        if let Err(e) = r {
            eprintln!("no se pudo cambiar el idioma: {e}");
            return;
        }
        self.idioma = if idioma == "en" { "en" } else { "es" }.to_string();
        ui.set_idioma(self.idioma.as_str().into());
        self.reconstruir(ui);
    }

    fn guardar(&self, ui: &AppWindow) {
        let Some(raiz) = &self.raiz else { return };
        let tema = match ui.global::<Paleta>().get_tema() {
            Tema::Dia => "dia",
            Tema::Contraste => "contraste",
            Tema::Noche => "noche",
        };
        let a = ajustes::Ajustes { idioma: Some(self.idioma.clone()), tema: Some(tema.into()) };
        if let Err(e) = a.guardar(raiz) {
            eprintln!("no se pudieron guardar los ajustes: {e}");
        }
    }
}

/// Lo que llega de los hilos de trabajo (`trabajo::avisar`).
pub fn recibir(a: Aviso) {
    con_estado(|st, ui| match a {
        Aviso::Plan { entry, version, total } => {
            if let Some(b) = st.bajadas.iter_mut().find(|b| b.entry == entry && b.activa()) {
                b.version = version;
                b.fase = 1;
                b.total = total;
            }
            st.mostrar_bajadas(ui);
            if entry.starts_with("mod:") {
                st.mostrar_mods(ui);
            }
        }
        Aviso::Progreso { entry, bajado, total } => {
            if let Some(b) = st.bajadas.iter_mut().find(|b| b.entry == entry && b.activa()) {
                b.progreso = total.map(|t| bajado as f32 / t.max(1) as f32).unwrap_or(-1.0);
                b.bajado = Some(bajado);
                b.total = total;
                if total.is_some_and(|t| bajado >= t) {
                    b.fase = 2;
                }
            }
            st.mostrar_bajadas(ui);
            if entry.starts_with("mod:") {
                st.mostrar_mods(ui);
            }
        }
        Aviso::Instalado { entry, error } => {
            if let Some(b) = st.bajadas.iter_mut().find(|b| b.entry == entry && b.activa()) {
                b.fase = 3;
                b.progreso = 1.0;
                b.error = error.unwrap_or_default();
            }
            // Un mod: el hilo de los mods manda enseguida cómo quedó todo.
            if entry.starts_with("mod:") {
                st.mostrar_bajadas(ui);
                return;
            }
            if let Some(bib) = &st.biblioteca {
                catalogo::refrescar(&mut st.catalogo, bib);
            }
            st.reconstruir(ui);
            // Se instaló el juego cuya pestaña Mods está abierta: ya se le pueden poner.
            if st.mods_entry == entry {
                let e = entry.clone();
                st.ver_mods(ui, &e);
            }
        }
        Aviso::Fallo { entry, error } => {
            if let Some(b) = st.bajadas.iter_mut().find(|b| b.entry == entry && b.activa()) {
                b.fase = 4;
                b.error = error;
            }
            st.reconstruir(ui);
            if entry.starts_with("mod:") {
                st.mostrar_mods(ui);
                explorar::mostrar(st, ui);
            }
        }
        Aviso::Mensaje { codigo, a, error } => aviso(ui, codigo, &a, &error),
        // Cambió lo instalado (volver a una versión anterior, borrarla) o la copia: se vuelven a
        // leer los recibos.
        Aviso::Refrescar => {
            // Terminó el cambio de versión, si había uno (bien o mal: el mensaje ya lo dijo).
            st.cambiando = None;
            if let Some(bib) = &st.biblioteca {
                catalogo::refrescar(&mut st.catalogo, bib);
            }
            if let Some(raiz) = &st.raiz {
                st.tiempos = tiempo::leer(raiz);
            }
            st.reconstruir(ui);
        }
        // Se bajaron un catálogo o unas recetas nuevas: se vuelve a leer todo.
        Aviso::Datos => {
            let (Some(raiz), Some(bib)) = (st.raiz.clone(), st.biblioteca.clone()) else { return };
            match datos::cargar(&raiz) {
                Ok((entradas, _, pendientes)) => {
                    st.catalogo = mostrar_catalogo(ui, Ok(catalogo::juntar(entradas, &bib)));
                    st.reconstruir(ui);
                    st.pedir_listas();
                    trabajo::arte(pendientes);
                }
                Err(e) => eprintln!("{e}"),
            }
        }
        // Llegaron portadas: las que antes faltaban ya se pueden cargar.
        Aviso::Arte => st.reconstruir(ui),
        Aviso::Opiniones(op) => {
            st.opiniones = op;
            st.reconstruir(ui);
        }
        Aviso::Mods(e) => {
            st.mods_estados.insert(e.clave.clone(), *e);
            st.mostrar_mods(ui);
            explorar::mostrar(st, ui);
        }
        Aviso::Ultimas(u) => {
            st.ultimas = u;
            st.reconstruir(ui);
            // Para probar sin clics: `UNRICOPIE_PRUEBA_ACTUALIZAR=<ficha>` la actualiza una vez.
            static PRUEBA_ACT: std::sync::Once = std::sync::Once::new();
            if let Ok(entry) = std::env::var("UNRICOPIE_PRUEBA_ACTUALIZAR") {
                PRUEBA_ACT.call_once(|| st.actualizar(ui, &entry));
            }
        }
        Aviso::Readme { id, que, md } => {
            let f = ui.global::<FichaMod>();
            if f.get_r().id.as_str() != id {
                return;
            }
            let bloques: Option<Vec<BloqueMd>> = md.map(|md| ficha_mod::bloques(&md).into_iter().map(bloque_md).collect());
            let estado = match &bloques {
                Some(b) if !b.is_empty() => 1,
                _ => 2,
            };
            let modelo = ModelRc::new(VecModel::from(bloques.unwrap_or_default()));
            match que {
                ficha_mod::Que::Readme => {
                    f.set_bloques(modelo);
                    f.set_estado_readme(estado);
                }
                ficha_mod::Que::Cambios => {
                    f.set_cambios(modelo);
                    f.set_estado_cambios(estado);
                }
            }
        }
        Aviso::Version { version, pagina } => {
            ui.set_version_nueva(version.into());
            ui.set_version_pagina(pagina.into());
        }
        // Llegaron iconos: las tarjetas los vuelven a pedir.
        Aviso::Iconos => {
            let i = ui.global::<Iconos>();
            i.set_listos(i.get_listos() + 1);
        }
        Aviso::Thunderstore { comunidad, mods, error } => {
            if let Some(e) = &error {
                eprintln!("no se pudo leer la lista de Thunderstore de {comunidad}: {e}");
            }
            let ok = error.is_none() || !mods.is_empty();
            st.listas_estado.insert(comunidad.clone(), if ok { 1 } else { 2 });
            if ok {
                st.listas.insert(comunidad, mods);
                // La cuenta de mods de las cajas (y Explorar, que `reconstruir` vuelve a llenar).
                st.reconstruir(ui);
            }
            st.mostrar_mods(ui);
            explorar::mostrar(st, ui);
            // Para probar sin clics: `UNRICOPIE_PRUEBA_INSTALAR=<id>` pide ese mod una vez, como el botón.
            static PRUEBA: std::sync::Once = std::sync::Once::new();
            // También `<ficha>|<id>`, para no depender de la pestaña Mods (y ver Descargas).
            if let Ok(v) = std::env::var("UNRICOPIE_PRUEBA_INSTALAR") {
                let (entry, id) = match v.split_once('|') {
                    Some((e, i)) => (e.to_string(), i.to_string()),
                    None => (st.mods_entry.clone(), v),
                };
                // Solo cuando ya llegó la lista de ese juego (llegan de a una por comunidad).
                if !entry.is_empty() && st.datos_mod(&entry, &id).is_some() {
                    PRUEBA.call_once(|| st.instalar_mod(ui, &entry, &id));
                }
            }
            // `UNRICOPIE_PRUEBA_FICHA_MOD=<ficha>|<id del mod>`: abre la ficha de ese mod, como un clic.
            static FICHA: std::sync::Once = std::sync::Once::new();
            if let Some((entry, id)) = std::env::var("UNRICOPIE_PRUEBA_FICHA_MOD").ok().and_then(|v| v.split_once('|').map(|(a, b)| (a.to_string(), b.to_string()))) {
                if st.datos_mod(&entry, &id).is_some() {
                    FICHA.call_once(|| {
                        st.abrir_mod(ui, &entry, &id);
                        // `UNRICOPIE_PRUEBA_PESTANA_MOD=<n>`: y abre esa pestaña de la ficha.
                        if let Some(n) = std::env::var("UNRICOPIE_PRUEBA_PESTANA_MOD").ok().and_then(|v| v.parse().ok()) {
                            ui.global::<FichaMod>().set_pestana(n);
                        }
                        // `UNRICOPIE_PRUEBA_ENLACE_MOD=<url>`: y pulsa ese enlace del README.
                        if let Ok(url) = std::env::var("UNRICOPIE_PRUEBA_ENLACE_MOD") {
                            let w = ui.as_weak();
                            slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
                                if let Some(ui) = w.upgrade() {
                                    ui.global::<FichaMod>().invoke_abrir_web(url.into());
                                }
                            });
                        }
                    });
                }
            }
            // `UNRICOPIE_PRUEBA_SOLTAR=<ruta>[;<ruta>…]`: como si se soltaran esos archivos.
            static SOLTAR: std::sync::Once = std::sync::Once::new();
            if let Ok(rutas) = std::env::var("UNRICOPIE_PRUEBA_SOLTAR") {
                SOLTAR.call_once(|| {
                    let rutas: Vec<PathBuf> = rutas.split(';').filter(|r| !r.is_empty()).map(PathBuf::from).collect();
                    st.orden_mods(|c| mods::Orden::Locales(c, rutas));
                });
            }
        }
    });
}

/// Las versiones guardadas de lo instalado en `dir`, para la pestaña Versiones: cuándo se guardó
/// cada una (el número del principio de su `id`) y si algún mod activo pide una más nueva.
fn anteriores(bib: &Library, dir: &Path, idioma: &str, minimos: &[(String, String)]) -> Vec<Anterior> {
    unricopie_nucleo::install::update::previous_versions(bib, dir)
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            let guardada = a.id.split_once('-').and_then(|(s, _)| s.parse::<u64>().ok()).map(|s| fecha(s, idioma)).unwrap_or_default();
            let (aviso_mod, aviso_min, aviso_mas) = aviso_mods(minimos, &a.receipt.version);
            Anterior {
                id: a.id.as_str().into(),
                version: a.receipt.version.as_str().into(),
                instalada: fecha(a.receipt.installed_at, idioma).into(),
                guardada: guardada.into(),
                aviso_mod: aviso_mod.into(),
                aviso_min: aviso_min.into(),
                aviso_mas,
            }
        })
        .collect()
}

/// Los mods activos que piden una versión del juego más nueva que `version`: el primero, la
/// versión que pide y cuántos más hay. Vacío si ninguno.
fn aviso_mods(minimos: &[(String, String)], version: &str) -> (String, String, i32) {
    let piden: Vec<&(String, String)> = minimos.iter().filter(|(_, min)| mods::mas_nueva(version, min)).collect();
    match piden.first() {
        Some((nombre, min)) => (nombre.clone(), min.clone(), piden.len() as i32 - 1),
        None => (String::new(), String::new(), 0),
    }
}

/// Una fecha (UTC) en segundos desde 1970, escrita en el idioma elegido: «3 de noviembre de
/// 2026» o «November 3, 2026». (El paso de días a año, mes y día es el de Howard Hinnant.)
fn fecha(segundos: u64, idioma: &str) -> String {
    let z = (segundos / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dia = doy - (153 * mp + 2) / 5 + 1;
    let mes = if mp < 10 { mp + 3 } else { mp - 9 };
    let anio = yoe + era * 400 + i64::from(mes <= 2);
    const ES: [&str; 12] = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
    const EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let m = (mes - 1) as usize;
    if idioma == "en" {
        format!("{} {dia}, {anio}", EN[m])
    } else {
        format!("{dia} de {} de {anio}", ES[m])
    }
}

/// Un bloque del README para la ventana: los párrafos y las listas, como texto con estilo (con
/// enlaces); si el Markdown no se entiende, como texto llano.
fn bloque_md(b: ficha_mod::Bloque) -> BloqueMd {
    use ficha_mod::Bloque;
    let (tipo, md, plano) = match b {
        Bloque::Titulo(n, t) => (n as i32, None, t),
        Bloque::Texto(t) => (0, Some(t), String::new()),
        Bloque::Codigo(t) => (4, None, t),
        Bloque::Raya => (5, None, String::new()),
    };
    let texto = md
        .map(|md| slint::StyledText::from_markdown(&md).unwrap_or_else(|_| slint::StyledText::from_plain_text(&md)))
        .unwrap_or_default();
    BloqueMd { tipo, texto, plano: plano.into() }
}

/// La primera letra en mayúscula («mods» → «Mods»).
fn mayuscula(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|p| p.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Una fecha `AAAA-MM-DD` (la de una release), escrita en el idioma elegido; vacía si no se entiende.
fn fecha_iso(iso: &str, idioma: &str) -> String {
    let mut p = iso.splitn(3, '-').map(|x| x.parse::<i64>().ok());
    let (Some(Some(a)), Some(Some(m)), Some(Some(d))) = (p.next(), p.next(), p.next()) else { return String::new() };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return String::new();
    }
    // Días desde 1970 (Howard Hinnant, al revés que `fecha`).
    let (y, mm) = if m <= 2 { (a - 1, m + 9) } else { (a, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let dias = era * 146_097 + doe - 719_468;
    if dias < 0 {
        return String::new();
    }
    fecha(dias as u64 * 86_400, idioma)
}

/// Abre una página de GitHub en el navegador o una carpeta en el explorador. Solo eso: las
/// direcciones las arma la ventana con datos del catálogo y de la biblioteca.
pub fn abrir_enlace(destino: &str) {
    let ok = destino.starts_with("https://github.com/")
        || destino.starts_with("https://thunderstore.io/")
        || destino.starts_with("https://recompendium.com/")
        || Path::new(destino).is_dir();
    if !ok {
        eprintln!("no se abre {destino}: no es GitHub, Thunderstore ni una carpeta que exista");
        return;
    }
    if let Err(e) = open::that_detached(destino) {
        eprintln!("no se pudo abrir {destino}: {e}");
    }
}

/// Lo que queda por hacer con los datos después de abrir la ventana: buscar un catálogo más
/// nuevo y bajar las portadas que faltan.
struct Despues {
    bajar: bool,
    pendientes: datos::Pendientes,
}

fn leer_catalogo(ui: &AppWindow, raiz: Option<&Path>, biblioteca: Option<&Library>) -> (Vec<catalogo::Juego>, Option<Despues>) {
    let Some(bib) = biblioteca else {
        ui.set_catalogo(1);
        return (Vec::new(), None);
    };
    if let Some(archivo) = std::env::var_os("UNRICOPIE_CATALOGO") {
        return (mostrar_catalogo(ui, catalogo::leer_archivo(Path::new(&archivo), bib)), None);
    }
    let Some(raiz) = raiz else {
        ui.set_catalogo(1);
        return (Vec::new(), None);
    };
    // La primera vez, sin nada guardado, se espera a la red; después se abre con lo guardado y
    // el catálogo nuevo se busca en un hilo.
    let recien = !datos::hay_copia(raiz) && datos::bajar(raiz);
    match datos::cargar(raiz) {
        Ok((entradas, _, pendientes)) => {
            let js = mostrar_catalogo(ui, Ok(catalogo::juntar(entradas, bib)));
            (js, Some(Despues { bajar: !recien, pendientes }))
        }
        Err(e) => {
            eprintln!("{e}");
            ui.set_catalogo(2);
            (Vec::new(), None)
        }
    }
}

fn mostrar_catalogo(ui: &AppWindow, leido: Result<(Vec<catalogo::Juego>, Vec<String>), String>) -> Vec<catalogo::Juego> {
    match leido {
        Ok((js, malas)) => {
            for m in &malas {
                eprintln!("receta que no se puede leer: {m}");
            }
            ui.set_catalogo(0);
            ui.set_catalogo_proyectos(js.iter().map(|j| j.proyectos.len()).sum::<usize>() as i32);
            ui.set_catalogo_ok(malas.is_empty());
            js
        }
        Err(e) => {
            eprintln!("{e}");
            ui.set_catalogo(2);
            Vec::new()
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    // Las portadas de las cajas, a demanda (ver CachePortadas).
    ui.global::<Portadas>().on_portada(|id| PORTADAS.with(|p| p.borrow_mut().portada(&id)));
    // Y los iconos de los mods (CacheIconos).
    ui.global::<Iconos>().on_icono(|url, _| ICONOS.with(|c| c.borrow_mut().icono(&url)));

    let root = Root::find().ok();
    if let Some(r) = &root {
        ui.set_raiz(r.path.display().to_string().into());
        ui.set_portable(r.portable);
    }
    let guardados = root.as_ref().map(|r| ajustes::Ajustes::leer(&r.path)).unwrap_or_default();
    let biblioteca = root.as_ref().map(Root::library);
    let (catalogo, despues) = leer_catalogo(&ui, root.as_ref().map(|r| r.path.as_path()), biblioteca.as_ref());
    let mods_tx = mods::hilo();
    if let Some(r) = &root {
        ICONOS.with(|c| {
            let mut c = c.borrow_mut();
            c.raiz = Some(r.path.clone());
            c.tx = Some(mods::iconos());
        });
    }
    let cola = match (&biblioteca, &root) {
        (Some(b), Some(r)) => Some(trabajo::cola(b.clone(), r.portable, mods_tx.clone())),
        _ => None,
    };

    // El tema y el idioma: los de la variable de entorno (para las capturas), los guardados o,
    // la primera vez, Noche y el idioma del sistema (español si el sistema está en español).
    let tema = std::env::var("UNRICOPIE_TEMA").ok().or(guardados.tema.clone());
    match tema.as_deref() {
        Some("dia") => ui.global::<Paleta>().set_tema(Tema::Dia),
        Some("contraste") => ui.global::<Paleta>().set_tema(Tema::Contraste),
        _ => {}
    }
    let idioma = std::env::var("UNRICOPIE_IDIOMA").ok().or(guardados.idioma.clone()).unwrap_or_else(ajustes::idioma_del_sistema);

    ESTADO.with(|e| {
        *e.borrow_mut() = Some(Estado {
            ui: ui.as_weak(),
            raiz: root.as_ref().map(|r| r.path.clone()),
            biblioteca,
            idioma: String::new(),
            catalogo,
            juegos: Vec::new(),
            imagenes: HashMap::new(),
            orden_biblioteca: 0,
            busqueda: String::new(),
            bajadas: Vec::new(),
            cola,
            opiniones: HashMap::new(),
            mods_tx,
            mods_entry: String::new(),
            mods_estados: HashMap::new(),
            ex_juegos: explorar::Filtros::default(),
            ex_mods: explorar::Filtros::default(),
            listas: HashMap::new(),
            listas_estado: HashMap::new(),
            mods_q: String::new(),
            mods_ver: 50,
            ultimas: HashMap::new(),
            tiempos: root.as_ref().map(|r| tiempo::leer(&r.path)).unwrap_or_default(),
            ficha_mod: None,
            cambiando: None,
        })
    });
    con_estado(|st, ui| st.cambiar_idioma(ui, &idioma));
    con_estado(|st, _| st.pedir_listas());
    trabajo::leer_opiniones();
    con_estado(|st, _| st.buscar_ultimas());
    // Para probar sin clics: `UNRICOPIE_PRUEBA_VOLVER=<ficha>` vuelve a su versión anterior más
    // reciente. Lo pide dos veces seguidas, como un doble clic: tiene que cambiar una sola vez.
    if let Ok(entry) = std::env::var("UNRICOPIE_PRUEBA_VOLVER") {
        con_estado(|st, ui| {
            st.volver(ui, &entry, None);
            st.volver(ui, &entry, None);
        });
    }
    ui.set_version_app(env!("CARGO_PKG_VERSION").into());
    trabajo::buscar_version();
    if let (Some(d), Some(r)) = (despues, &root) {
        trabajo::datos(r.path.clone(), d.bajar, d.pendientes);
    }

    ui.on_abrir(|id| con_estado(|st, ui| st.abrir(ui, &id)));
    ui.on_ordenar_biblioteca(|i| {
        con_estado(|st, ui| {
            st.orden_biblioteca = i;
            st.refrescar(ui);
        })
    });
    ui.on_buscar(|q| {
        con_estado(|st, ui| {
            st.busqueda = q.to_string();
            st.refrescar(ui);
        })
    });
    ui.on_abrir_enlace(|url| abrir_enlace(&url));
    ui.on_cambiar_idioma(|idioma| {
        con_estado(|st, ui| {
            st.cambiar_idioma(ui, &idioma);
            st.guardar(ui);
        })
    });
    ui.on_guardar_ajustes(|| con_estado(|st, ui| st.guardar(ui)));
    ui.on_opinar(|entry, resultado| {
        con_estado(|st, _| {
            let Some((j, p)) = st.proyecto(&entry) else { return };
            let Some((_, r)) = &p.instalado else { return };
            trabajo::opinar(j.primero().nombre.clone(), entry.to_string(), r.version.clone(), resultado);
        })
    });
    ui.on_instalar(|entry| {
        con_estado(|st, ui| {
            let Some((j, p)) = st.proyecto(&entry) else { return };
            if st.bajadas.iter().any(|b| b.entry == p.e.entry && b.activa()) {
                return;
            }
            let Some(receta) = p.receta.clone() else { return };
            let (juego, nombre) = (j.id.clone(), p.nombre());
            let Some(cola) = &st.cola else { return };
            if cola.send(Tarea::Juego(entry.to_string(), receta)).is_err() {
                aviso(ui, codigo::COLA_NO_RESPONDE, "", &errores::Datos::default());
                return;
            }
            st.bajadas.push(Bajada {
                entry: entry.to_string(),
                juego,
                proyecto: nombre,
                version: String::new(),
                bajado: None,
                total: None,
                error: errores::Datos::default(),
                progreso: -1.0,
                fase: 0,
                es_mod: false,
                autor: String::new(),
                actualiza: false,
            });
            aviso(ui, codigo::EN_DESCARGAS, "", &errores::Datos::default());
            st.reconstruir(ui);
        })
    });
    ui.on_actualizar(|entry| con_estado(|st, ui| st.actualizar(ui, &entry)));
    ui.on_abrir_mods(|| con_estado(|st, ui| st.mostrar_pagina_mods(ui, None)));
    ui.on_ir_mods(|entry| {
        con_estado(|st, ui| {
            ui.set_vista(Vista::Mods);
            st.mostrar_pagina_mods(ui, Some(&entry));
        })
    });
    ui.on_elegir_juego_mods(|id| {
        con_estado(|st, ui| {
            let entry = st.catalogo.iter().find(|j| j.id == id.as_str()).and_then(|j| j.proyectos.iter().find(|p| p.instalado.is_some() && p.receta.as_ref().is_some_and(|r| r.mods.is_some()))).map(|p| p.e.entry.clone());
            if let Some(e) = entry {
                st.mostrar_pagina_mods(ui, Some(&e));
            }
        })
    });
    ui.on_volver(|entry, id| con_estado(|st, ui| st.volver(ui, &entry, Some(&id))));
    ui.on_borrar_anterior(|entry, id| {
        con_estado(|st, _| {
            if let Some((bib, dir, id, version)) = st.anterior(&entry, Some(&id)) {
                trabajo::borrar_anterior(bib, dir, id, version);
            }
        })
    });
    ui.on_jugar(|entry| {
        con_estado(|st, ui| {
            let Some(bib) = st.biblioteca.clone() else { return };
            let Some((j, p)) = st.proyecto(&entry) else { return };
            let Some((dir, recibo)) = p.instalado.clone() else { return };
            let Some(receta) = p.receta.clone() else { return };
            let nombre = j.primero().nombre.clone();
            let tiempo = st.raiz.clone().map(|r| (r, tiempo::clave(&entry, &recibo.version)));
            aviso(ui, codigo::PREPARANDO, &nombre, &errores::Datos::default());
            trabajo::jugar(bib, receta, dir, nombre, tiempo);
        })
    });
    ui.on_elegir_copia(|entry| {
        // El diálogo se abre fuera de `con_estado`: mientras está abierto, la ventana sigue
        // recibiendo avisos de los hilos y tiene que poder usar el estado.
        let mut datos = None;
        con_estado(|st, _| {
            let Some(bib) = st.biblioteca.clone() else { return };
            let Some((j, p)) = st.proyecto(&entry) else { return };
            let titulo = if st.idioma == "en" { "Your copy of {}" } else { "Tu copia de {}" }.replace("{}", &j.primero().nombre);
            let Some(receta) = p.receta.clone() else { return };
            datos = Some((bib, receta, titulo, p.copia_es_carpeta()));
        });
        let Some((bib, receta, titulo, carpeta)) = datos else { return };
        let dialogo = rfd::FileDialog::new().set_title(&titulo);
        let elegido = if carpeta { dialogo.pick_folder() } else { dialogo.pick_file() };
        if let Some(origen) = elegido {
            con_estado(|_, ui| aviso(ui, codigo::COMPROBANDO_COPIA, "", &errores::Datos::default()));
            trabajo::guardar_copia(bib, receta, origen);
        }
    });

    // La pestaña Mods.
    ui.global::<ModsUi>().on_ver(|entry| con_estado(|st, ui| st.ver_mods(ui, &entry)));
    ui.global::<ModsUi>().on_activar(|id, on| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Activar(c, ModId(id.to_string()), on))));
    ui.global::<ModsUi>().on_variante(|id, archivo, on| {
        con_estado(|st, _| {
            let Ok(f) = RelPath::new(&archivo) else { return };
            st.orden_mods(|c| mods::Orden::Variante(c, ModId(id.to_string()), f, on));
        })
    });
    ui.global::<ModsUi>().on_quitar(|id| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Quitar(c, ModId(id.to_string())))));
    // Ordenar a mano: arrastrar una fila, «Subir»/«Bajar» y «Que gane».
    ui.global::<ModsUi>().on_dato_arrastre(slint::DataTransfer::from);
    ui.global::<ModsUi>().on_mover(|id, destino, despues| {
        con_estado(|st, _| st.orden_mods(|c| mods::Orden::Mover(c, ModId(id.to_string()), ModId(destino.to_string()), despues)))
    });
    ui.global::<ModsUi>().on_correr(|id, delta| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Correr(c, ModId(id.to_string()), delta))));
    ui.global::<ModsUi>().on_preferir(|gana, ids| {
        con_estado(|st, _| {
            let otros: Vec<ModId> = ids.split('|').filter(|x| !x.is_empty() && *x != gana.as_str()).map(|x| ModId(x.to_string())).collect();
            st.orden_mods(|c| mods::Orden::Preferir(c, ModId(gana.to_string()), otros));
        })
    });
    // Mods propios: «Añadir desde archivo…». El diálogo se abre fuera de `con_estado`, como el de
    // la copia: mientras está abierto la ventana sigue recibiendo avisos.
    ui.global::<ModsUi>().on_anadir_archivo(|| {
        let mut titulo = None;
        con_estado(|st, _| {
            if st.ctx_mods(&st.mods_entry).is_some_and(|c| c.destino.is_some()) {
                let nombre = st.proyecto(&st.mods_entry).map(|(j, _)| j.primero().nombre.clone()).unwrap_or_default();
                titulo = Some(if st.idioma == "en" { format!("Mods for {nombre}") } else { format!("Mods para {nombre}") });
            }
        });
        let Some(titulo) = titulo else { return };
        let elegidos = rfd::FileDialog::new()
            .set_title(&titulo)
            .add_filter("Mods", &["zip", "7z", "nrm", "rtz", "dll", "so", "dylib"])
            .pick_files();
        if let Some(rutas) = elegidos {
            con_estado(|st, _| st.orden_mods(|c| mods::Orden::Locales(c, rutas)));
        }
    });
    ui.global::<ModsUi>().on_instalar(|id| {
        con_estado(|st, ui| {
            let entry = st.mods_entry.clone();
            st.instalar_mod(ui, &entry, &id);
        })
    });
    // Explorar: Juegos | Mods, el inicio con estantes y «Ver todos» con filtros.
    let ex = ui.global::<ExplorarUi>();
    ex.on_modo(|m| {
        con_estado(|st, ui| {
            let e = ui.global::<ExplorarUi>();
            e.set_mods(m);
            e.set_todos(false);
            if m {
                st.abrir_explorar_mods();
            }
            explorar::mostrar(st, ui);
        })
    });
    ex.on_ver_todos(|grupo, clave| {
        con_estado(|st, ui| {
            let f = st.filtros_ex(ui);
            f.marcados.clear();
            if !grupo.is_empty() {
                f.marcar(&grupo, &clave, true);
            }
            ui.global::<ExplorarUi>().set_todos(true);
            explorar::mostrar(st, ui);
        })
    });
    ex.on_volver(|| {
        con_estado(|st, ui| {
            ui.global::<ExplorarUi>().set_todos(false);
            explorar::mostrar(st, ui);
        })
    });
    ex.on_marcar(|grupo, clave, on| {
        con_estado(|st, ui| {
            st.filtros_ex(ui).marcar(&grupo, &clave, on);
            explorar::mostrar(st, ui);
        })
    });
    ex.on_limpiar(|| {
        con_estado(|st, ui| {
            st.filtros_ex(ui).marcados.clear();
            explorar::mostrar(st, ui);
        })
    });
    ex.on_ordenar(|i| {
        con_estado(|st, ui| {
            st.filtros_ex(ui).orden = i;
            explorar::mostrar(st, ui);
        })
    });
    // Buscar lleva a «Ver todos», con lo que haya marcado.
    ex.on_buscar(|q| {
        con_estado(|st, ui| {
            // Un solo buscador para Juegos y Mods: lo buscado vale para los dos.
            st.ex_juegos.q = q.to_string();
            st.ex_mods.q = q.to_string();
            if !q.trim().is_empty() {
                ui.global::<ExplorarUi>().set_todos(true);
            }
            explorar::mostrar(st, ui);
        })
    });
    ex.on_instalar(|entry, id| con_estado(|st, ui| st.instalar_mod(ui, &entry, &id)));
    ui.global::<FichaMod>().on_abrir(|entry, id| con_estado(|st, ui| st.abrir_mod(ui, &entry, &id)));
    // Los enlaces del README y la web del autor: un mod de la lista del juego abre su ficha aquí;
    // lo demás, cualquier dirección https, en el navegador (solo con un clic).
    ui.global::<FichaMod>().on_abrir_web(|url| {
        con_estado(|st, ui| {
            if let (Some(id), Some((entry, _))) = (ficha_mod::id_de_enlace(&url), st.ficha_mod.clone()) {
                if st.datos_mod(&entry, &id).is_some() {
                    st.abrir_mod(ui, &entry, &id);
                    return;
                }
            }
            if url.starts_with("https://") {
                if let Err(e) = open::that_detached(url.as_str()) {
                    eprintln!("no se pudo abrir {url}: {e}");
                }
            }
        })
    });
    // «Ver el juego»: su página, en el proyecto con mods y en la pestaña Mods.
    ex.on_ver_juego(|juego, entry| {
        con_estado(|st, ui| {
            st.abrir(ui, &juego);
            if let Some(i) = st.catalogo.iter().find(|j| j.id == juego.as_str()).and_then(|j| j.proyectos.iter().position(|p| p.e.entry == entry.as_str())) {
                ui.set_proyecto(i as i32);
            }
            ui.set_pestana(1);
        })
    });
    ui.global::<ModsUi>().on_buscar(|q| {
        con_estado(|st, ui| {
            st.mods_q = q.to_string();
            st.mods_ver = 50;
            st.mostrar_mods(ui);
        })
    });
    ui.global::<ModsUi>().on_mas(|| {
        con_estado(|st, ui| {
            st.mods_ver += 50;
            st.mostrar_mods(ui);
        })
    });
    ui.global::<ModsUi>().on_recargar(|| {
        con_estado(|st, ui| {
            let Some(ctx) = st.ctx_mods(&st.mods_entry) else { return };
            let Some(c) = ctx.regla.thunderstore.clone() else { return };
            st.listas_estado.insert(c.clone(), 0);
            mods::lista(ctx.raiz, c, true);
            st.mostrar_mods(ui);
        })
    });
    ui.global::<ModsUi>().on_perfil_elegir(|n| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Perfil(c, mods::OpPerfil::Elegir(n.to_string())))));
    ui.global::<ModsUi>().on_perfil_nuevo(|n, copiar| {
        con_estado(|st, _| st.orden_mods(|c| mods::Orden::Perfil(c, mods::OpPerfil::Nuevo { nombre: n.to_string(), copiar })))
    });
    ui.global::<ModsUi>().on_perfil_renombrar(|n| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Perfil(c, mods::OpPerfil::Renombrar(n.to_string())))));
    ui.global::<ModsUi>().on_perfil_borrar(|| con_estado(|st, _| st.orden_mods(|c| mods::Orden::Perfil(c, mods::OpPerfil::Borrar))));

    // Archivos soltados en la ventana (desde el Explorador): con la pestaña Mods abierta de un
    // juego instalado, se añaden como mods propios. Slint 1.18 no los pasa a DropArea, así que
    // llegan por winit; en Linux con Wayland winit no los recibe y queda «Añadir desde archivo…».
    {
        use slint::winit_030::winit::event::WindowEvent;
        use slint::winit_030::{EventResult, WinitWindowAccessor};
        ui.window().on_winit_window_event(|_, ev| {
            match ev {
                WindowEvent::HoveredFile(_) => con_estado(|st, ui| {
                    ui.set_soltar_donde(st.donde_soltar(ui).into());
                    ui.set_soltando(true);
                }),
                WindowEvent::HoveredFileCancelled => con_estado(|_, ui| ui.set_soltando(false)),
                WindowEvent::DroppedFile(ruta) => con_estado(|st, ui| {
                    ui.set_soltando(false);
                    if !st.donde_soltar(ui).is_empty() {
                        let r = ruta.clone();
                        st.orden_mods(|c| mods::Orden::Locales(c, vec![r]));
                    }
                }),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // Para las capturas: la vista con que arranca.
    if let Ok(v) = std::env::var("UNRICOPIE_VISTA") {
        match v.split(':').collect::<Vec<_>>().as_slice() {
            // explorar[:mods][:todos]
            ["explorar", resto @ ..] => {
                ui.set_vista(Vista::Explorar);
                let e = ui.global::<ExplorarUi>();
                e.set_mods(resto.contains(&"mods"));
                e.set_todos(resto.contains(&"todos"));
                con_estado(|st, ui| {
                    if resto.contains(&"mods") {
                        st.abrir_explorar_mods();
                    }
                    explorar::mostrar(st, ui);
                });
            }
            // tienda:<id>: la ficha de tienda, como al abrirlo desde Explorar.
            ["tienda", id] => {
                ui.set_vista(Vista::Explorar);
                con_estado(|st, ui| st.abrir(ui, id));
            }
            ["mods"] => {
                ui.set_vista(Vista::Mods);
                con_estado(|st, ui| st.mostrar_pagina_mods(ui, None));
            }
            ["descargas"] => ui.set_vista(Vista::Descargas),
            ["ajustes"] => ui.set_vista(Vista::Ajustes),
            ["juego", id, resto @ ..] => {
                con_estado(|st, ui| st.abrir(ui, id));
                if let Some(p) = resto.first().and_then(|p| p.parse().ok()) {
                    ui.set_pestana(p);
                }
                if resto.get(1) == Some(&"buscar") {
                    ui.global::<ModsUi>().set_buscando(true);
                }
            }
            _ => {}
        }
    }

    // UNRICOPIE_CAPTURA=ruta.png guarda una imagen de la ventana y cierra.
    if let Ok(ruta) = std::env::var("UNRICOPIE_CAPTURA") {
        // `UNRICOPIE_CAPTURA_TAM=ANCHOxALTO`: otro tamaño, para ver lo que queda abajo.
        let (ancho, alto) = std::env::var("UNRICOPIE_CAPTURA_TAM")
            .ok()
            .and_then(|t| t.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
            .unwrap_or((1280.0, 800.0));
        ui.window().set_size(slint::LogicalSize::new(ancho, alto));
        let w = ui.as_weak();
        // `UNRICOPIE_CAPTURA_MS`: cuánto esperar antes (1500 ms si no se dice), para lo que llega de un hilo.
        let ms = std::env::var("UNRICOPIE_CAPTURA_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
        slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
            if let Some(ui) = w.upgrade() {
                match ui.window().take_snapshot() {
                    Ok(img) => {
                        if let Err(e) = image::save_buffer(&ruta, img.as_bytes(), img.width(), img.height(), image::ExtendedColorType::Rgba8) {
                            eprintln!("no se pudo guardar la captura: {e}");
                        }
                    }
                    Err(e) => eprintln!("no se pudo capturar la ventana: {e}"),
                }
            }
            let _ = slint::quit_event_loop();
        });
    }
    ui.run()
}
