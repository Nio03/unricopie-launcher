//! El catálogo que ve el launcher: cada juego con los proyectos que lo llevan a PC, su receta y
//! su arte, más lo que hay instalado en la biblioteca (los recibos `unricopie.json`).
//!
//! Sale del catálogo público de Recompendium y de las recetas, que se bajan aparte
//! (`datos.rs`). Para probar, `UNRICOPIE_CATALOGO` apunta a un archivo con las dos cosas juntas.
//! Las fichas sin receta también se muestran: Unricopie no las instala y lleva a su proyecto.
//!
//! A eso se suma lo que la persona añadió a mano desde GitHub (`poner_a_mano`): las recetas de
//! `<biblioteca>/added.json`, que entran como un juego más o en la ficha del catálogo de ese repo.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use unricopie_nucleo::install::recipe::{CopyHow, CopyKind, CopyUse};
use unricopie_nucleo::install::{added, copy, Library, Platform, Receipt, Recipe};

/// La «consola» (la clave de los filtros) de lo añadido a mano que no está en el catálogo: con
/// ella Explorar, que es el catálogo, lo deja fuera.
pub const CONSOLA_A_MANO: &str = "a-mano";

/// Un texto de la ficha en los dos idiomas del sitio.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Texto {
    #[serde(default)]
    pub es: String,
    #[serde(default)]
    pub en: String,
}

impl Texto {
    /// En el idioma pedido; vacío si la ficha no lo tiene en ese idioma (no se mezcla).
    pub fn en(&self, idioma: &str) -> &str {
        if idioma == "en" {
            &self.en
        } else {
            &self.es
        }
    }
}

/// Una entrada del archivo: una ficha del sitio con su receta.
#[derive(Debug, Deserialize)]
pub struct Entrada {
    pub entry: String,
    pub juego: Option<String>,
    pub nombre: String,
    pub proyecto: String,
    pub repo: String,
    pub consola: String,
    pub consola_nombre: String,
    pub color: String,
    pub tinta: String,
    #[serde(default)]
    pub desarrollador: String,
    #[serde(default)]
    pub anio: i32,
    #[serde(default)]
    pub estado: String,
    #[serde(default)]
    pub desc: Texto,
    /// Cómo llega a PC: `recomp`, `decomp-port`, `source-port`… (el texto lo pone la ventana).
    #[serde(default)]
    pub metodo: String,
    #[serde(default)]
    pub toolchain: String,
    #[serde(default)]
    pub autor: String,
    #[serde(default)]
    pub requisitos: Texto,
    #[serde(default)]
    pub recomendado: bool,
    #[serde(default)]
    pub motivo: Texto,
    #[serde(default)]
    pub arte: Arte,
    /// Retirada por DMCA: queda como registro, sin enlace ni instalación.
    #[serde(default)]
    pub retirado: bool,
    /// Sin enlace por prudencia (Switch 2 y otros, ver el sitio).
    #[serde(default)]
    pub sin_enlace: bool,
    /// La receta, si Unricopie sabe instalarla.
    #[serde(default)]
    pub receta: Option<serde_json::Value>,
    /// La última release del proyecto en GitHub, su fecha (`AAAA-MM-DD`) y las estrellas.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub fecha: String,
    #[serde(default)]
    pub estrellas: u32,
    /// Su receta no es del catálogo: la añadió la persona a mano desde GitHub (`poner_a_mano`).
    #[serde(default)]
    pub a_mano: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Arte {
    pub portada: Option<PathBuf>,
    pub fondo: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    pub icono: Option<PathBuf>,
    /// Capturas del juego (de IGDB), para la ficha de Explorar.
    #[serde(default)]
    pub capturas: Vec<PathBuf>,
    /// `steamgriddb` o `igdb`.
    #[serde(default)]
    pub fuente: String,
    /// Los autores de cada imagen, en SteamGridDB.
    #[serde(default)]
    pub autores: HashMap<String, String>,
}

/// Cómo está la copia del jugador para un proyecto (los mismos códigos que `Proyecto.copia`).
pub struct EstadoCopia {
    pub codigo: i32,
    /// La copia aceptada con la que coincide la suya.
    pub nombre: String,
    /// Está instalado y no la tiene.
    pub falta: bool,
}

/// Un proyecto, ya con su receta leída (si la tiene) y lo que haya instalado.
pub struct Proyecto {
    pub e: Entrada,
    pub receta: Option<Recipe>,
    pub instalado: Option<(PathBuf, Receipt)>,
}

impl Proyecto {
    /// El nombre del proyecto. Si la ficha se llama como el juego, el del repositorio
    /// (`Zelda64Recomp`), que es como lo conoce la gente.
    pub fn nombre(&self) -> String {
        if self.e.proyecto == self.e.nombre {
            self.e.repo.rsplit('/').next().unwrap_or(&self.e.repo).to_string()
        } else {
            self.e.proyecto.clone()
        }
    }

    /// El estado de la ficha como nivel: 0 totalmente jugable, 1 jugable, 2 experimental. Lo
    /// añadido a mano que no tiene ficha en el catálogo no tiene estado: 3, sin revisar.
    pub fn nivel(&self) -> i32 {
        match self.e.estado.as_str() {
            "fully" => 0,
            "playable" => 1,
            "" if self.e.consola == CONSOLA_A_MANO => 3,
            _ => 2,
        }
    }

    /// Unricopie sabe instalarlo: hay receta.
    pub fn con_receta(&self) -> bool {
        self.receta.is_some()
    }

    /// Unricopie puede pedir, comprobar y guardar la copia del jugador para este proyecto.
    pub fn pide_copia(&self) -> bool {
        self.receta.as_ref().is_some_and(|r| !matches!(r.copy.how, CopyHow::None | CopyHow::OwnInstaller))
    }

    /// La copia la da la persona como carpeta (los archivos del juego original), no como archivo.
    pub fn copia_es_carpeta(&self) -> bool {
        self.receta.as_ref().is_some_and(|r| r.copy.kind == Some(CopyKind::Folder))
    }

    pub fn copia(&self, biblioteca: &Library) -> EstadoCopia {
        let Some(receta) = &self.receta else {
            return EstadoCopia { codigo: 0, nombre: String::new(), falta: false };
        };
        let (codigo, nombre, falta) = match receta.copy.how {
            CopyHow::None => (0, String::new(), false),
            CopyHow::OwnInstaller => (1, String::new(), false),
            _ => match copy::imported(biblioteca, receta, None) {
                Some(i) if i.verified => (2, i.accepted.first().cloned().unwrap_or_default(), false),
                Some(_) => (3, String::new(), false),
                None => (4, String::new(), self.instalado.is_some()),
            },
        };
        EstadoCopia { codigo, nombre, falta }
    }

    /// Para qué usa el juego la copia (los mismos códigos que `Proyecto.copia-uso`): -1 sin
    /// receta, 0 no hace falta, 1 la pide su instalador, 2 solo la primera vez, 3 cada vez.
    pub fn uso_copia(&self, portable: bool) -> i32 {
        let (Some(receta), Some(plataforma)) = (&self.receta, Platform::current()) else { return -1 };
        match receta.copy.usage(plataforma, portable) {
            CopyUse::None => 0,
            CopyUse::Installer => 1,
            CopyUse::Once => 2,
            CopyUse::Always => 3,
        }
    }

    /// La receta dice qué bajar y qué abrir en este equipo.
    pub fn instalable(&self) -> bool {
        let Some(receta) = &self.receta else { return false };
        !self.e.retirado && Platform::current().and_then(|p| receta.resolve(None, p).ok()).is_some_and(|r| r.run.is_some())
    }
}

/// Un juego del catálogo con sus proyectos: el recomendado primero y luego el que mejor va.
pub struct Juego {
    pub id: String,
    pub proyectos: Vec<Proyecto>,
}

impl Juego {
    pub fn primero(&self) -> &Entrada {
        &self.proyectos[0].e
    }

    pub fn instalado(&self) -> Option<&Proyecto> {
        self.proyectos.iter().find(|p| p.instalado.is_some())
    }
}

/// Lee el archivo de pruebas (`UNRICOPIE_CATALOGO`): las fichas con su receta dentro.
pub fn leer_archivo(archivo: &Path, biblioteca: &Library) -> Result<(Vec<Juego>, Vec<String>), String> {
    let texto = std::fs::read_to_string(archivo).map_err(|e| format!("no se puede leer {}: {e}", archivo.display()))?;
    let entradas: Vec<Entrada> = serde_json::from_str(&texto).map_err(|e| format!("{} no es válido: {e}", archivo.display()))?;
    Ok(juntar(entradas, biblioteca))
}

/// Junta las fichas por juego. Una receta que no se puede leer deja la ficha sin instalar y se
/// cuenta (es un error nuestro, no del jugador).
pub fn juntar(mut entradas: Vec<Entrada>, biblioteca: &Library) -> (Vec<Juego>, Vec<String>) {
    match added::load(biblioteca) {
        Ok(a_mano) => poner_a_mano(&mut entradas, a_mano),
        Err(e) => eprintln!("no se pudieron leer los proyectos añadidos a mano: {e}"),
    }
    let instalados = recibos(biblioteca);
    let mut malas = Vec::new();
    let mut por_juego: BTreeMap<String, Vec<Proyecto>> = BTreeMap::new();
    for e in entradas {
        let receta = match e.receta.as_ref().map(|v| Recipe::from_json(&v.to_string())) {
            None => None,
            Some(Ok(r)) => Some(r),
            Some(Err(err)) => {
                malas.push(format!("{}: {err}", e.entry));
                None
            }
        };
        let instalado = instalados.get(&e.entry).cloned();
        let id = e.juego.clone().unwrap_or_else(|| e.entry.replace('/', "--"));
        por_juego.entry(id).or_default().push(Proyecto { e, receta, instalado });
    }
    let mut juegos: Vec<Juego> = por_juego
        .into_iter()
        .map(|(id, mut proyectos)| {
            // El recomendado primero; luego lo que Unricopie instala, y el que mejor va.
            proyectos.sort_by_key(|p| (!p.e.recomendado, !p.con_receta(), p.nivel(), p.nombre().to_lowercase()));
            Juego { id, proyectos }
        })
        .collect();
    juegos.sort_by_key(|j| orden(&j.primero().nombre));
    (juegos, malas)
}

/// Suma al catálogo las recetas añadidas a mano. La que es de una ficha del catálogo (el repo
/// estaba, pero sin receta) pasa a ser la receta de esa ficha, con su portada y sus datos; las
/// demás entran como un juego más, con el nombre del repo. Manda siempre el catálogo: si la
/// ficha ya tiene receta (llegó después de añadirla) o está retirada, la añadida a mano no cuenta.
pub fn poner_a_mano(entradas: &mut Vec<Entrada>, a_mano: Vec<Recipe>) {
    for receta in a_mano {
        let Ok(valor) = serde_json::to_value(&receta) else { continue };
        if let Some(e) = entradas.iter_mut().find(|e| e.entry == receta.entry) {
            if e.receta.is_none() && !e.retirado {
                e.receta = Some(valor);
                e.a_mano = true;
            }
            continue;
        }
        let nombre = added::repo_name(&receta.repo);
        let nueva = serde_json::json!({
            "entry": receta.entry,
            "juego": null,
            "nombre": nombre,
            "proyecto": nombre,
            "repo": receta.repo,
            "consola": CONSOLA_A_MANO,
            "consola_nombre": "GitHub",
            "color": "#6e5494",
            "tinta": "#ffffff",
            "autor": receta.repo.split('/').next().unwrap_or_default(),
            "desc": {
                "es": "Lo añadiste a mano desde GitHub. No está en el catálogo: Unricopie no lo revisó, no sabe qué necesita y no puede comprobar tu copia del juego.",
                "en": "You added it by hand from GitHub. It isn't in the catalog: Unricopie hasn't reviewed it, doesn't know what it needs and can't check your copy of the game.",
            },
            "a_mano": true,
            "receta": valor,
        });
        match serde_json::from_value(nueva) {
            Ok(e) => entradas.push(e),
            Err(e) => eprintln!("no se pudo mostrar {}: {e}", receta.entry),
        }
    }
}

/// Las fichas del catálogo que son del repo `repo`, por lo que dice la ficha o por lo que dice
/// su receta, que es de donde se baja (GitHub no distingue mayúsculas).
pub fn del_repo<'a>(juegos: &'a [Juego], repo: &str) -> Vec<&'a Proyecto> {
    let es = |r: &str| r.eq_ignore_ascii_case(repo);
    juegos.iter().flat_map(|j| &j.proyectos).filter(|p| es(&p.e.repo) || p.receta.as_ref().is_some_and(|r| es(&r.repo))).collect()
}

/// Vuelve a leer los recibos, después de instalar o desinstalar.
pub fn refrescar(juegos: &mut [Juego], biblioteca: &Library) {
    let instalados = recibos(biblioteca);
    for p in juegos.iter_mut().flat_map(|j| j.proyectos.iter_mut()) {
        p.instalado = instalados.get(&p.e.entry).cloned();
    }
}

/// Para ordenar por nombre sin que «The» mande.
fn orden(nombre: &str) -> String {
    let n = nombre.to_lowercase();
    n.strip_prefix("the ").map(str::to_string).unwrap_or(n)
}

/// Los recibos de lo instalado, por ficha. Solo lee: si la biblioteca no existe, no hay nada.
fn recibos(biblioteca: &Library) -> HashMap<String, (PathBuf, Receipt)> {
    let mut out = HashMap::new();
    let Ok(dirs) = std::fs::read_dir(&biblioteca.games) else { return out };
    for d in dirs.flatten() {
        let dir = d.path();
        if let Ok(Some(r)) = Receipt::read(&dir) {
            // Las variantes (un juego de varios en una receta) todavía no se muestran aparte.
            if r.game.is_none() {
                out.insert(r.entry.clone(), (dir, r));
            }
        }
    }
    out
}
