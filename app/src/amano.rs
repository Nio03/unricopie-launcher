//! «Añadir un proyecto de GitHub», en Ajustes y en `unricopie add`: instalar un proyecto que no
//! está en el catálogo a partir de su repo. El trabajo es del núcleo (`install::added`): qué
//! archivo de la release toca aquí, qué programa abre el juego y dónde se guardan las recetas
//! añadidas. Aquí va lo que depende del catálogo (si el repo ya está, y con qué ficha queda), lo
//! que comparten la ventana y la línea de órdenes, y los hilos y avisos de la ventana.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use slint::{ModelRc, VecModel};
use unricopie_nucleo::install::added::{self, Found, Proposal, Why};
use unricopie_nucleo::install::release::{self, GhRelease};
use unricopie_nucleo::install::{installer, Channel, Library, Plan, Platform, Receipt, Recipe};
use unricopie_nucleo::{net, Error};

use crate::catalogo::{self, Juego, Proyecto};
use crate::trabajo::{avisar, Aviso};
use crate::{error_ui, errores, tam, Anadido, AppWindow, Estado};

/// Por qué un repo no se añade a mano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rechazo {
    /// Ya está en el catálogo con su receta: se instala desde ahí. La ficha, el juego bajo el
    /// que se ve y cómo se llama.
    EnCatalogo { entry: String, juego: String, nombre: String },
    /// Está en el catálogo como retirado (DMCA): eso Unricopie no lo instala nunca.
    Retirado { entry: String },
    /// Ya está añadido a mano, con esta ficha.
    YaAnadido { entry: String },
}

/// «The Legend of Zelda: Majora's Mask (Zelda64Recomp)»: el juego y el proyecto que lo lleva a PC.
pub fn titulo(p: &Proyecto) -> String {
    format!("{} ({})", p.e.nombre, p.nombre())
}

/// La ficha bajo la que quedaría `repo` (ya validado) si se añade a mano. Si el catálogo ya lo
/// instala, no: se usa su receta, que está revisada. Si el repo está en el catálogo en una sola
/// ficha sin receta, se queda con esa ficha (su juego, su portada); si no, una ficha propia.
pub fn ficha(juegos: &[Juego], repo: &str) -> Result<String, Rechazo> {
    let suyas = catalogo::del_repo(juegos, repo);
    if let Some(p) = suyas.iter().find(|p| p.e.a_mano) {
        return Err(Rechazo::YaAnadido { entry: p.e.entry.clone() });
    }
    if let Some(p) = suyas.iter().find(|p| p.con_receta()) {
        let juego = juegos.iter().find(|j| j.proyectos.iter().any(|x| x.e.entry == p.e.entry)).map(|j| j.id.clone()).unwrap_or_default();
        return Err(Rechazo::EnCatalogo { entry: p.e.entry.clone(), juego, nombre: titulo(p) });
    }
    if let Some(p) = suyas.iter().find(|p| p.e.retirado) {
        return Err(Rechazo::Retirado { entry: p.e.entry.clone() });
    }
    let entry = match suyas.as_slice() {
        [una] => una.e.entry.clone(),
        _ => added::entry_for(repo),
    };
    // Dos repos distintos que dan la misma ficha (`a-b/c` y `a/b-c`): el segundo no entra.
    if suyas.len() != 1 && juegos.iter().flat_map(|j| &j.proyectos).any(|p| p.e.entry == entry) {
        return Err(Rechazo::YaAnadido { entry });
    }
    Ok(entry)
}

/// Lo que queda después de instalar un proyecto añadido a mano.
pub struct Instalado {
    pub plan: Plan,
    /// Con el programa que se abre, que ya quedó también en la receta guardada.
    pub recibo: Receipt,
}

/// Instala lo propuesto y, si sale bien, guarda su receta entre las añadidas: desde ahí es un
/// juego más (abrir, actualizar, versiones). Lo mismo para «Instalar» y para `unricopie add`.
/// `plan_listo` se llama cuando ya se sabe qué se baja y `progreso`, mientras baja.
pub fn instalar(
    agent: &ureq::Agent,
    biblioteca: &Library,
    propuesta: &Proposal,
    releases: &[GhRelease],
    plan_listo: &mut dyn FnMut(&Plan),
    progreso: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Instalado, Error> {
    let host = Platform::current().ok_or(Error::UnknownSystem)?;
    let plan = installer::plan_from(&propuesta.recipe, None, host, Channel::Untested, None, releases, biblioteca)?;
    plan_listo(&plan);
    let (recibo, receta) = added::install(agent, biblioteca, &propuesta.recipe, &plan, progreso)?;
    added::add(biblioteca, &receta)?;
    Ok(Instalado { plan, recibo })
}

/// Por qué no se quita un proyecto de los añadidos.
#[derive(Debug)]
pub enum NoQuitar {
    /// No hay ninguno añadido con esa ficha.
    NoEsta,
    /// Está instalado, en esta carpeta, con esa receta: quitarla lo dejaría sin forma de abrirlo
    /// ni de actualizarlo. Unricopie no borra juegos por su cuenta: primero se quita la carpeta.
    Instalado(PathBuf),
    Error(Error),
}

/// Quita de los añadidos la receta de la ficha `entry`. Nunca toca lo instalado: si el juego
/// está instalado con esa receta, no se quita.
pub fn quitar(biblioteca: &Library, juegos: &[Juego], entry: &str) -> Result<Recipe, NoQuitar> {
    let en_uso = juegos.iter().flat_map(|j| &j.proyectos).find(|p| p.e.entry == entry && p.e.a_mano);
    if let Some((dir, _)) = en_uso.and_then(|p| p.instalado.as_ref()) {
        return Err(NoQuitar::Instalado(dir.clone()));
    }
    added::remove(biblioteca, entry).map_err(NoQuitar::Error)?.ok_or(NoQuitar::NoEsta)
}

// --- La ventana ---

/// Lo que se está añadiendo en Ajustes: el repo, su ficha, sus releases ya leídas y lo que se
/// propone instalar (cuando ya se sabe qué archivo).
pub struct Pendiente {
    repo: String,
    entry: String,
    releases: Vec<GhRelease>,
    propuesta: Option<Proposal>,
}

/// Lee las releases de `repo` en un hilo.
fn leer(repo: String) {
    std::thread::spawn(move || {
        let releases = release::list(&net::agent(), &repo);
        avisar(Aviso::AManoVersiones { repo, releases });
    });
}

/// Instala lo propuesto en un hilo, contando cómo va (un aviso cada 150 ms como mucho).
fn bajar(biblioteca: Library, propuesta: Proposal, releases: Vec<GhRelease>) {
    std::thread::spawn(move || {
        let mut ultimo: Option<Instant> = None;
        let hecho = instalar(&net::agent(), &biblioteca, &propuesta, &releases, &mut |_| {}, &mut |bajado, total| {
            let fin = total == Some(bajado);
            if fin || ultimo.is_none_or(|t| t.elapsed() >= Duration::from_millis(150)) {
                ultimo = Some(Instant::now());
                avisar(Aviso::AManoAvance { bajado, total });
            }
        });
        avisar(Aviso::AManoHecho(hecho.map(|i| i.plan.version)));
    });
}

impl Estado {
    /// «Buscar»: lo escrito pasa a ser un repo, se mira si el catálogo ya lo tiene y se leen sus
    /// versiones en GitHub.
    pub(crate) fn amano_buscar(&mut self, ui: &AppWindow, texto: &str) {
        self.amano = None;
        ui.set_amano_candidatos(ModelRc::default());
        let repo = match added::parse_repo(texto) {
            Ok(r) => r,
            Err(e) => return self.amano_error(ui, &e),
        };
        ui.set_amano_repo(repo.as_str().into());
        let entry = match ficha(&self.catalogo, &repo) {
            Ok(e) => e,
            Err(Rechazo::EnCatalogo { juego, nombre, .. }) => {
                ui.set_amano_nombre(nombre.into());
                ui.set_amano_juego(juego.into());
                return ui.set_amano_fase(8);
            }
            Err(Rechazo::Retirado { .. }) => return ui.set_amano_fase(9),
            Err(Rechazo::YaAnadido { .. }) => return ui.set_amano_fase(10),
        };
        self.amano = Some(Pendiente { repo: repo.clone(), entry, releases: Vec::new(), propuesta: None });
        ui.set_amano_fase(1);
        leer(repo);
    }

    /// Llegaron las releases del repo que se está añadiendo (o no se pudieron leer).
    pub(crate) fn amano_versiones(&mut self, ui: &AppWindow, repo: &str, releases: Result<Vec<GhRelease>, Error>) {
        let Some(p) = self.amano.as_mut().filter(|p| p.repo == repo) else { return };
        match releases {
            Ok(rs) => {
                p.releases = rs;
                self.amano_proponer(ui, None, None);
            }
            Err(e) => self.amano_error(ui, &e),
        }
    }

    /// Arma la receta con lo que se sabe: el archivo que eligió la persona (si hubo que elegir)
    /// y el programa (si hubo que elegir). `true` si ya hay algo que instalar.
    fn amano_proponer(&mut self, ui: &AppWindow, archivo: Option<&str>, programa: Option<&str>) -> bool {
        let Some(p) = self.amano.as_mut() else { return false };
        let hallado = Platform::current().ok_or(Error::UnknownSystem).and_then(|host| added::propose(&p.repo, &p.entry, &p.releases, host, archivo, programa));
        match hallado {
            Ok(Found::Ready(propuesta)) => {
                ui.set_amano_version(propuesta.tag.as_str().into());
                ui.set_amano_archivo(propuesta.asset.as_str().into());
                ui.set_amano_tam(tam(propuesta.size, &self.idioma).into());
                ui.set_amano_fase(2);
                p.propuesta = Some(*propuesta);
                true
            }
            Ok(Found::Choose { tag, assets }) => {
                p.propuesta = None;
                ui.set_amano_version(tag.into());
                ui.set_amano_instaladores(assets.rejected.iter().any(|r| r.why == Why::Installer));
                ui.set_amano_candidatos(ModelRc::new(VecModel::from(assets.candidates.iter().map(Into::into).collect::<Vec<slint::SharedString>>())));
                ui.set_amano_fase(3);
                false
            }
            Err(e) => {
                self.amano_error(ui, &e);
                false
            }
        }
    }

    /// La persona eligió uno de los candidatos: el archivo de la release (fase 3) o el programa
    /// que abre el juego (fase 5), y entonces se instala con él.
    pub(crate) fn amano_elegir(&mut self, ui: &AppWindow, elegido: &str) {
        match ui.get_amano_fase() {
            3 => {
                self.amano_proponer(ui, Some(elegido), None);
            }
            5 => {
                let Some(archivo) = self.amano.as_ref().and_then(|p| p.propuesta.as_ref()).map(|p| p.asset.clone()) else { return };
                if self.amano_proponer(ui, Some(&archivo), Some(elegido)) {
                    self.amano_instalar(ui);
                }
            }
            _ => {}
        }
    }

    /// «Instalar»: baja e instala lo propuesto, en su hilo.
    pub(crate) fn amano_instalar(&mut self, ui: &AppWindow) {
        let (Some(bib), Some(p)) = (self.biblioteca.clone(), self.amano.as_ref()) else { return };
        let Some(propuesta) = p.propuesta.clone() else { return };
        ui.set_amano_bajado("".into());
        ui.set_amano_total("".into());
        ui.set_amano_fase(4);
        bajar(bib, propuesta, p.releases.clone());
    }

    pub(crate) fn amano_avance(&self, ui: &AppWindow, bajado: u64, total: Option<u64>) {
        ui.set_amano_bajado(tam(bajado, &self.idioma).into());
        ui.set_amano_total(total.map(|t| tam(t, &self.idioma)).unwrap_or_default().into());
    }

    /// Terminó de instalar: bien (la versión), sin saber qué programa abrir (se ofrecen los que
    /// hay) o mal.
    pub(crate) fn amano_hecho(&mut self, ui: &AppWindow, hecho: Result<String, Error>) {
        match hecho {
            Ok(version) => {
                self.amano = None;
                ui.set_amano_version(version.into());
                ui.set_amano_fase(6);
                self.releer_catalogo(ui);
                self.buscar_ultimas();
            }
            Err(Error::ChooseRun { candidates }) => {
                ui.set_amano_candidatos(ModelRc::new(VecModel::from(candidates.iter().map(Into::into).collect::<Vec<slint::SharedString>>())));
                ui.set_amano_fase(5);
            }
            Err(e) => self.amano_error(ui, &e),
        }
    }

    /// «Quitar»: saca la receta de los añadidos. Con el juego instalado no se quita (el botón
    /// está apagado, y `quitar` lo vuelve a mirar).
    pub(crate) fn amano_quitar(&mut self, ui: &AppWindow, entry: &str) {
        let Some(bib) = self.biblioteca.clone() else { return };
        match quitar(&bib, &self.catalogo, entry) {
            Ok(_) => {
                ui.set_amano_fase(0);
                self.releer_catalogo(ui);
            }
            Err(NoQuitar::Error(e)) => self.amano_error(ui, &e),
            Err(NoQuitar::NoEsta | NoQuitar::Instalado(_)) => self.amano_lista(ui),
        }
    }

    fn amano_error(&mut self, ui: &AppWindow, e: &Error) {
        ui.set_amano_error(error_ui(&errores::de(e)));
        ui.set_amano_fase(7);
    }

    /// La lista de lo añadido a mano, como está en la biblioteca.
    pub(crate) fn amano_lista(&self, ui: &AppWindow) {
        let recetas = self.biblioteca.as_ref().and_then(|b| added::load(b).ok()).unwrap_or_default();
        let filas: Vec<Anadido> = recetas
            .iter()
            .map(|r| {
                let visto = self.catalogo.iter().find_map(|j| j.proyectos.iter().find(|p| p.e.entry == r.entry).map(|p| (j, p)));
                let en_uso = visto.is_some_and(|(_, p)| p.e.a_mano);
                let instalado = visto.filter(|_| en_uso).and_then(|(_, p)| p.instalado.as_ref());
                Anadido {
                    entry: r.entry.as_str().into(),
                    juego: visto.map(|(j, _)| j.id.as_str()).unwrap_or_default().into(),
                    nombre: visto.map_or_else(|| added::repo_name(&r.repo).to_string(), |(j, _)| j.primero().nombre.clone()).into(),
                    repo: r.repo.as_str().into(),
                    version: instalado.map(|(_, recibo)| recibo.version.as_str()).unwrap_or_default().into(),
                    instalado: instalado.is_some(),
                    carpeta: instalado.map(|(dir, _)| dir.display().to_string()).unwrap_or_default().into(),
                    en_uso,
                }
            })
            .collect();
        ui.set_amano_lista(ModelRc::new(VecModel::from(filas)));
    }

    /// Vuelve a leer el catálogo con lo añadido a mano, después de añadir o quitar: de donde lo
    /// lee la ventana al abrir (`UNRICOPIE_CATALOGO` o lo guardado), sin usar la red.
    fn releer_catalogo(&mut self, ui: &AppWindow) {
        let (Some(raiz), Some(bib)) = (self.raiz.clone(), self.biblioteca.clone()) else { return };
        let leido = match std::env::var_os("UNRICOPIE_CATALOGO") {
            Some(archivo) => catalogo::leer_archivo(std::path::Path::new(&archivo), &bib),
            None => crate::datos::cargar(&raiz).map(|(entradas, _, _)| catalogo::juntar(entradas, &bib)),
        };
        match leido {
            Ok(juegos) => {
                self.catalogo = crate::mostrar_catalogo(ui, Ok(juegos));
                self.reconstruir(ui);
            }
            Err(e) => eprintln!("{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogo::Entrada;

    fn entrada(entry: &str, juego: &str, repo: &str, receta: Option<serde_json::Value>, retirado: bool) -> Entrada {
        serde_json::from_value(serde_json::json!({
            "entry": entry, "juego": juego, "nombre": format!("Juego {juego}"), "proyecto": format!("Proyecto {entry}"), "repo": repo,
            "consola": "pc", "consola_nombre": "PC", "color": "#000000", "tinta": "#ffffff", "retirado": retirado, "receta": receta,
        }))
        .unwrap()
    }

    fn releases(tag: &str, names: &[&str]) -> Vec<GhRelease> {
        let assets: Vec<serde_json::Value> = names.iter().map(|n| serde_json::json!({ "name": n, "size": 10, "browser_download_url": format!("https://example.invalid/{n}"), "digest": null })).collect();
        serde_json::from_value(serde_json::json!([{ "tag_name": tag, "draft": false, "prerelease": false, "published_at": "2026-11-01T00:00:00Z", "assets": assets }])).unwrap()
    }

    /// La receta añadida a mano de `repo`, para Windows, ya con su programa.
    fn a_mano(repo: &str, entry: &str) -> Recipe {
        let rs = releases("v1", &["juego-v1-windows-x64.zip"]);
        let Found::Ready(p) = added::propose(repo, entry, &rs, Platform::WindowsX64, None, Some("juego.exe")).unwrap() else { panic!() };
        p.recipe
    }

    fn receta_del_catalogo(entry: &str, repo: &str) -> serde_json::Value {
        serde_json::json!({
            "entry": entry, "repo": repo,
            "release": { "assets": { "windows-x64": "x-*.zip" }, "run": { "windows-x64": "x.exe" } },
            "copy": { "how": "none" },
            "checked": { "date": "2026-11-20", "release": "v1", "sources": ["prueba"] }
        })
    }

    fn catalogo_de_prueba() -> Vec<Entrada> {
        vec![
            entrada("ports/con-receta", "uno", "Autor/ConReceta", Some(receta_del_catalogo("ports/con-receta", "Autor/ConReceta")), false),
            entrada("ports/sin-receta", "dos", "Autor/SinReceta", None, false),
            entrada("ports/retirado", "tres", "Autor/Retirado", None, true),
            // Un repo con dos fichas sin receta (dos juegos del mismo proyecto).
            entrada("ports/doble-a", "cuatro", "Autor/Doble", None, false),
            entrada("ports/doble-b", "cinco", "Autor/Doble", None, false),
        ]
    }

    #[test]
    fn decide_la_ficha_y_rechaza_lo_que_ya_instala_el_catalogo() {
        let t = tempfile::tempdir().unwrap();
        let bib = Library::new(t.path());
        let (juegos, malas) = catalogo::juntar(catalogo_de_prueba(), &bib);
        assert!(malas.is_empty(), "{malas:?}");
        // Ya está en el catálogo con receta: no, y se dice cuál es. Sin distinguir mayúsculas.
        for repo in ["Autor/ConReceta", "autor/conreceta"] {
            assert_eq!(ficha(&juegos, repo), Err(Rechazo::EnCatalogo { entry: "ports/con-receta".into(), juego: "uno".into(), nombre: "Juego uno (Proyecto ports/con-receta)".into() }));
        }
        assert_eq!(ficha(&juegos, "Autor/Retirado"), Err(Rechazo::Retirado { entry: "ports/retirado".into() }));
        // Está en el catálogo sin receta, en una sola ficha: se queda con esa ficha.
        assert_eq!(ficha(&juegos, "autor/sinreceta"), Ok("ports/sin-receta".into()));
        // En dos fichas: no se adivina cuál; va aparte, como lo que no está.
        assert_eq!(ficha(&juegos, "Autor/Doble"), Ok("added/autor-doble".into()));
        assert_eq!(ficha(&juegos, "Otro/Juego"), Ok("added/otro-juego".into()));

        // Una vez añadidos, no se añaden otra vez; ni otro repo que daría la misma ficha.
        added::add(&bib, &a_mano("Otro/Juego", "added/otro-juego")).unwrap();
        added::add(&bib, &a_mano("Autor/SinReceta", "ports/sin-receta")).unwrap();
        let (juegos, _) = catalogo::juntar(catalogo_de_prueba(), &bib);
        assert_eq!(ficha(&juegos, "otro/juego"), Err(Rechazo::YaAnadido { entry: "added/otro-juego".into() }));
        assert_eq!(ficha(&juegos, "Autor/SinReceta"), Err(Rechazo::YaAnadido { entry: "ports/sin-receta".into() }));
        assert_eq!(ficha(&juegos, "Otro-Juego/x"), Ok("added/otro-juego-x".into()));
        added::add(&bib, &a_mano("A-B/C", "added/a-b-c")).unwrap();
        let (juegos, _) = catalogo::juntar(catalogo_de_prueba(), &bib);
        assert_eq!(ficha(&juegos, "A/B-C"), Err(Rechazo::YaAnadido { entry: "added/a-b-c".into() }));
    }

    #[test]
    fn lo_anadido_entra_en_el_catalogo() {
        let t = tempfile::tempdir().unwrap();
        let bib = Library::new(t.path());
        added::add(&bib, &a_mano("Otro/Juego", "added/otro-juego")).unwrap();
        added::add(&bib, &a_mano("Autor/SinReceta", "ports/sin-receta")).unwrap();
        // Estas dos no cuentan: el catálogo ya trae la receta de una, y la otra está retirada.
        added::add(&bib, &a_mano("Autor/ConReceta", "ports/con-receta")).unwrap();
        added::add(&bib, &a_mano("Autor/Retirado", "ports/retirado")).unwrap();
        let (juegos, malas) = catalogo::juntar(catalogo_de_prueba(), &bib);
        assert!(malas.is_empty(), "{malas:?}");
        let proyecto = |entry: &str| juegos.iter().flat_map(|j| &j.proyectos).find(|p| p.e.entry == entry).unwrap();

        // Lo que no está en el catálogo es un juego más, con el nombre del repo.
        let nuevo = proyecto("added/otro-juego");
        assert!(nuevo.e.a_mano && nuevo.con_receta() && nuevo.receta.as_ref().unwrap().by_hand);
        assert_eq!((nuevo.e.nombre.as_str(), nuevo.nombre().as_str(), nuevo.e.autor.as_str(), nuevo.e.consola.as_str()), ("Juego", "Juego", "Otro", catalogo::CONSOLA_A_MANO));
        assert!(nuevo.e.desc.es.contains("a mano") && nuevo.e.desc.en.contains("by hand"));
        assert!(juegos.iter().any(|j| j.id == "added--otro-juego" && j.proyectos.len() == 1));
        // No pide copia: Unricopie no sabe nada de ese juego.
        assert!(!nuevo.pide_copia() && nuevo.uso_copia(false) <= 0);
        // El que estaba sin receta la tiene ahora, en su ficha y con su juego.
        let ficha_suya = proyecto("ports/sin-receta");
        assert!(ficha_suya.e.a_mano && ficha_suya.con_receta());
        assert_eq!((ficha_suya.e.nombre.as_str(), ficha_suya.e.consola.as_str()), ("Juego dos", "pc"));
        assert!(juegos.iter().any(|j| j.id == "dos"));
        // Manda el catálogo: su receta sigue siendo la suya, y lo retirado sigue sin instalarse.
        let oficial = proyecto("ports/con-receta");
        assert!(!oficial.e.a_mano && !oficial.receta.as_ref().unwrap().by_hand);
        assert_eq!(oficial.receta.as_ref().unwrap().release.assets[&Platform::WindowsX64], "x-*.zip");
        assert!(!proyecto("ports/retirado").e.a_mano && !proyecto("ports/retirado").con_receta());
        assert_eq!(juegos.iter().map(|j| j.proyectos.len()).sum::<usize>(), 6);
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert!(nuevo.instalable() && ficha_suya.instalable());
        }
    }

    #[test]
    fn quitar_no_toca_lo_instalado() {
        let t = tempfile::tempdir().unwrap();
        let bib = Library::new(t.path());
        let receta = a_mano("Otro/Juego", "added/otro-juego");
        added::add(&bib, &receta).unwrap();
        // Instalado: un recibo en su carpeta, como lo deja el instalador.
        let dir = bib.game_dir(&receta, None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("juego.exe"), b"exe").unwrap();
        let recibo = serde_json::json!({
            "format": 1, "entry": "added/otro-juego", "game": null, "repo": "Otro/Juego", "version": "v1", "channel": "untested", "platform": "windows-x64",
            "asset": "juego-v1-windows-x64.zip", "url": "https://example.invalid/x", "size": 3, "sha256": "abc", "sha256_verified": false,
            "run": "juego.exe", "run_stamp": null, "installed_at": 1, "files": []
        });
        std::fs::write(dir.join("unricopie.json"), recibo.to_string()).unwrap();
        let (juegos, _) = catalogo::juntar(Vec::new(), &bib);
        assert!(juegos[0].proyectos[0].instalado.is_some(), "aparece instalado, como cualquier juego");
        assert!(matches!(quitar(&bib, &juegos, "added/otro-juego"), Err(NoQuitar::Instalado(d)) if d == dir));
        assert_eq!(added::load(&bib).unwrap().len(), 1);
        assert!(dir.join("juego.exe").is_file());
        // Sin la carpeta, sí; y lo que no está, no está.
        std::fs::remove_dir_all(&dir).unwrap();
        let (juegos, _) = catalogo::juntar(Vec::new(), &bib);
        assert_eq!(quitar(&bib, &juegos, "added/otro-juego").unwrap().repo, "Otro/Juego");
        assert!(added::load(&bib).unwrap().is_empty());
        assert!(matches!(quitar(&bib, &juegos, "added/otro-juego"), Err(NoQuitar::NoEsta)));
        assert!(catalogo::juntar(Vec::new(), &bib).0.is_empty());
    }
}
