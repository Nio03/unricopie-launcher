//! Explorar, como Nexus Mods (decidido el 8-11): al entrar, estantes («Se instalan con
//! Unricopie» y uno por consola; en Mods, «Los más bajados» y uno por juego), cada uno con
//! «Ver todos ›». «Ver todos» muestra la cuadrícula con la columna de filtros (casillas con su
//! cuenta) y «Ordenar». Dentro de un grupo de filtros vale cualquiera de lo marcado; entre
//! grupos, todos.
use std::collections::{BTreeMap, BTreeSet};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use unricopie_nucleo::mods::sources::RemoteMod;

use crate::{hex, mods, AppWindow, Estado, EstanteJuegos, EstanteMods, ExplorarUi, GrupoFiltro, Juego, OpcionFiltro};

/// Cuántos van en cada estante del inicio (se dibujan los que caben en una fila).
/// Cuántas cajas lleva cada estante (se desplaza en horizontal).
const ESTANTE: usize = 30;

/// Lo marcado en la columna de filtros, el orden y lo buscado, de un modo (Juegos o Mods).
#[derive(Default)]
pub struct Filtros {
    pub marcados: BTreeMap<String, BTreeSet<String>>,
    pub orden: i32,
    pub q: String,
}

impl Filtros {
    pub fn marcar(&mut self, grupo: &str, clave: &str, on: bool) {
        let g = self.marcados.entry(grupo.to_string()).or_default();
        if on {
            g.insert(clave.to_string());
        } else {
            g.remove(clave);
        }
    }

    /// Lo marcado de un grupo, si hay algo.
    fn de(&self, grupo: &str) -> Option<&BTreeSet<String>> {
        self.marcados.get(grupo).filter(|g| !g.is_empty())
    }

    fn marcada(&self, grupo: &str, clave: &str) -> bool {
        self.marcados.get(grupo).is_some_and(|g| g.contains(clave))
    }

    fn hay(&self) -> bool {
        self.marcados.values().any(|g| !g.is_empty())
    }
}

fn modelo<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn opcion(f: &Filtros, grupo: &str, clave: &str, nombre: &str, color: slint::Color, marca: i32, cuenta: usize) -> OpcionFiltro {
    OpcionFiltro {
        clave: clave.into(),
        nombre: nombre.into(),
        color,
        marca,
        cuenta: cuenta as i32,
        marcada: f.marcada(grupo, clave),
    }
}

/// Llena Explorar según el modo (Juegos o Mods) y la vista (inicio o «Ver todos»).
pub fn mostrar(st: &Estado, ui: &AppWindow) {
    if ui.global::<ExplorarUi>().get_mods() {
        mostrar_mods(st, ui);
    } else {
        mostrar_juegos(st, ui);
    }
}

fn instalable(j: &Juego) -> bool {
    j.proyectos.iter().any(|p| p.instalable)
}

/// Los colores de un estado (como `Util.nivel-color`): verde, ámbar y violeta.
fn color_estado(nivel: i32) -> slint::Color {
    match nivel {
        0 => hex("#4cc27a"),
        1 => hex("#f2a93b"),
        _ => hex("#b28cf5"),
    }
}

fn mostrar_juegos(st: &Estado, ui: &AppWindow) {
    let ex = ui.global::<ExplorarUi>();
    ex.set_estantes_mods(ModelRc::default());
    ex.set_filas(ModelRc::default());
    // Las consolas, de la que más juegos tiene a la que menos: (clave, nombre, color, juegos).
    let mut por_consola: BTreeMap<&str, (SharedString, slint::Color, Vec<&Juego>)> = BTreeMap::new();
    for (c, j) in &st.juegos {
        por_consola.entry(c.as_str()).or_insert_with(|| (j.consola_nombre.clone(), j.color, Vec::new())).2.push(j);
    }
    let mut consolas: Vec<(&str, SharedString, slint::Color, Vec<&Juego>)> = por_consola.into_iter().map(|(k, (n, c, v))| (k, n, c, v)).collect();
    consolas.sort_by(|a, b| b.3.len().cmp(&a.3.len()).then_with(|| a.1.cmp(&b.1)));

    if !ex.get_todos() {
        let mut estantes = Vec::new();
        let inst: Vec<&Juego> = st.juegos.iter().map(|(_, j)| j).filter(|j| instalable(j)).collect();
        if !inst.is_empty() {
            estantes.push(EstanteJuegos {
                grupo: "unricopie".into(),
                clave: "instalables".into(),
                cuenta: inst.len() as i32,
                juegos: modelo(inst.iter().take(ESTANTE).map(|j| (*j).clone()).collect()),
                ..Default::default()
            });
        }
        for (clave, nombre, color, js) in &consolas {
            estantes.push(EstanteJuegos {
                grupo: "consola".into(),
                clave: (*clave).into(),
                titulo: nombre.clone(),
                color: *color,
                con_color: true,
                cuenta: js.len() as i32,
                juegos: modelo(js.iter().take(ESTANTE).map(|j| (*j).clone()).collect()),
            });
        }
        ex.set_estantes(modelo(estantes));
        ex.set_juegos(ModelRc::default());
        ex.set_filtros(ModelRc::default());
        ex.set_filtrado(false);
        return;
    }

    ex.set_estantes(ModelRc::default());
    let f = &st.ex_juegos;
    let q = f.q.trim().to_lowercase();
    let consola = f.de("consola");
    let estado = f.de("estado");
    let unricopie = f.de("unricopie");
    let mut v: Vec<&Juego> = st
        .juegos
        .iter()
        .filter(|(c, j)| {
            (q.is_empty() || j.nombre.to_lowercase().contains(&q))
                && consola.is_none_or(|s| s.contains(c.as_str()))
                && estado.is_none_or(|s| s.contains(&j.nivel.to_string()))
                && unricopie.is_none_or(|s| (s.contains("instalables") && instalable(j)) || (s.contains("instalados") && j.instalado))
        })
        .map(|(_, j)| j)
        .collect();
    let nombre = |j: &Juego| j.nombre.to_lowercase();
    match f.orden {
        // Más jugables primero.
        1 => v.sort_by(|a, b| a.nivel.cmp(&b.nivel).then_with(|| nombre(a).cmp(&nombre(b)))),
        // Más nuevos y más viejos; sin año, al final.
        2 => v.sort_by(|a, b| (a.anio == 0).cmp(&(b.anio == 0)).then_with(|| b.anio.cmp(&a.anio)).then_with(|| nombre(a).cmp(&nombre(b)))),
        3 => v.sort_by(|a, b| (a.anio == 0).cmp(&(b.anio == 0)).then_with(|| a.anio.cmp(&b.anio)).then_with(|| nombre(a).cmp(&nombre(b)))),
        _ => v.sort_by_key(|j| nombre(j)),
    }
    ex.set_resultados(v.len() as i32);
    ex.set_juegos(modelo(v.into_iter().cloned().collect()));

    let mut grupos = Vec::new();
    grupos.push(GrupoFiltro {
        clave: "consola".into(),
        opciones: modelo(consolas.iter().map(|(k, n, c, js)| opcion(f, "consola", k, n, *c, 1, js.len())).collect()),
    });
    let por_estado = |n: i32| st.juegos.iter().filter(|(_, j)| j.nivel == n).count();
    grupos.push(GrupoFiltro {
        clave: "estado".into(),
        opciones: modelo((0..3).map(|n| opcion(f, "estado", &n.to_string(), "", color_estado(n), 2, por_estado(n))).collect()),
    });
    let n_inst = st.juegos.iter().filter(|(_, j)| instalable(j)).count();
    let n_instalados = st.juegos.iter().filter(|(_, j)| j.instalado).count();
    grupos.push(GrupoFiltro {
        clave: "unricopie".into(),
        opciones: modelo(vec![
            opcion(f, "unricopie", "instalables", "", Default::default(), 0, n_inst),
            opcion(f, "unricopie", "instalados", "", Default::default(), 0, n_instalados),
        ]),
    });
    ex.set_filtros(modelo(grupos));
    ex.set_filtrado(f.hay());
    ex.set_orden(f.orden);
}

/// Los mods de una fuente que se muestran: sin los retirados ni los de contenido para adultos.
fn visibles<'a>(x: &mods::Fuente<'a>) -> Vec<&'a RemoteMod> {
    x.lista.iter().filter(|m| !m.deprecated && !m.info.nsfw).collect()
}

/// Las categorías de Thunderstore que se ofrecen como «Tipo», en este orden.
const TIPOS: [&str; 6] = ["mods", "libraries", "modpacks", "audio", "tools", "misc"];

fn mostrar_mods(st: &Estado, ui: &AppWindow) {
    let ex = ui.global::<ExplorarUi>();
    ex.set_estantes(ModelRc::default());
    ex.set_juegos(ModelRc::default());
    let bajando = st.bajando_mods();
    let (fuentes, cargando) = st.fuentes_mods();
    let cuenta = |f: &mods::Fuente| f.lista.iter().filter(|m| !m.deprecated && !m.info.nsfw).count();
    ex.set_total_mods(fuentes.iter().map(cuenta).sum::<usize>() as i32);
    ex.set_juegos_mods(fuentes.len() as i32);
    ex.set_cargando(cargando);
    let todas: Vec<&mods::Fuente> = fuentes.iter().collect();
    let siempre = |_: &mods::Fuente, _: &RemoteMod| true;

    if !ex.get_todos() {
        let mut estantes = Vec::new();
        if !fuentes.is_empty() {
            let mut top = mods::remotos(&todas, "", &bajando, &st.idioma, true, 0, &siempre);
            let n = top.len();
            top.truncate(ESTANTE);
            estantes.push(EstanteMods { grupo: String::new().into(), clave: "mas-bajados".into(), cuenta: n as i32, mods: modelo(top), ..Default::default() });
        }
        for f in &fuentes {
            let mut v = mods::remotos(&[f], "", &bajando, &st.idioma, true, 0, &siempre);
            let n = v.len();
            v.truncate(ESTANTE);
            estantes.push(EstanteMods {
                grupo: "juego".into(),
                clave: f.juego_id.as_str().into(),
                titulo: f.juego.as_str().into(),
                color: f.color,
                con_color: true,
                cuenta: n as i32,
                mods: modelo(v),
            });
        }
        ex.set_estantes_mods(modelo(estantes));
        ex.set_filas(ModelRc::default());
        ex.set_filtros(ModelRc::default());
        ex.set_filtrado(false);
        return;
    }

    ex.set_estantes_mods(ModelRc::default());
    let f = &st.ex_mods;
    let juego = f.de("juego");
    let tipo = f.de("tipo");
    let unricopie = f.de("unricopie");
    let elegidas: Vec<&mods::Fuente> = fuentes.iter().filter(|x| juego.is_none_or(|s| s.contains(&x.juego_id))).collect();
    let instalado = |x: &mods::Fuente, m: &RemoteMod| x.estado.is_some_and(|e| e.instalados.iter().any(|i| i.id == m.id));
    let pasa = |x: &mods::Fuente, m: &RemoteMod| {
        tipo.is_none_or(|s| m.info.categories.iter().any(|c| s.contains(c)))
            && unricopie.is_none_or(|s| (s.contains("mis-juegos") && x.instalado) || (s.contains("instalados") && instalado(x, m)))
    };
    let filas = mods::remotos(&elegidas, &f.q, &bajando, &st.idioma, true, f.orden, &pasa);
    ex.set_resultados(filas.len() as i32);
    ex.set_filas(modelo(filas));

    let mut grupos = Vec::new();
    grupos.push(GrupoFiltro {
        clave: "juego".into(),
        opciones: modelo(fuentes.iter().map(|x| opcion(f, "juego", &x.juego_id, &x.juego, x.color, 1, cuenta(x))).collect()),
    });
    let tipos: Vec<OpcionFiltro> = TIPOS
        .iter()
        .map(|t| (t, fuentes.iter().map(|x| visibles(x).iter().filter(|m| m.info.categories.iter().any(|c| c == t)).count()).sum::<usize>()))
        .filter(|(_, n)| *n > 0)
        .map(|(t, n)| opcion(f, "tipo", t, "", Default::default(), 0, n))
        .collect();
    grupos.push(GrupoFiltro { clave: "tipo".into(), opciones: modelo(tipos) });
    let mis_juegos: usize = fuentes.iter().filter(|x| x.instalado).map(cuenta).sum();
    let instalados: usize = fuentes.iter().map(|x| visibles(x).iter().filter(|m| instalado(x, m)).count()).sum();
    grupos.push(GrupoFiltro {
        clave: "unricopie".into(),
        opciones: modelo(vec![
            opcion(f, "unricopie", "mis-juegos", "", Default::default(), 0, mis_juegos),
            opcion(f, "unricopie", "instalados", "", Default::default(), 0, instalados),
        ]),
    });
    ex.set_filtros(modelo(grupos));
    ex.set_filtrado(f.hay());
    ex.set_orden(f.orden);
}
