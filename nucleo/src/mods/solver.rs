//! El solucionador: ordena los mods activos y dice, con nombre y archivo, cada problema.
//!
//! Trabaja en dos niveles, como se decidió para Unricopie:
//! 1. **Dependencias y versiones**: lo que declara cada mod (el manifest de Thunderstore, el
//!    `mod.json` de un `.nrm`) y lo que añada la curación. Una dependencia carga antes que el
//!    mod que la pide.
//! 2. **Archivos**: si dos mods traen el mismo archivo, gana el que carga después. Es un
//!    conflicto *resuelto* si hay una relación que decide quién va después (una dependencia o
//!    una regla «A después de B», del jugador o de la curación); si no, queda *sin resolver*
//!    y se avisa, como el rayo rojo de Vortex.
//!
//! El orden respeta dependencias y reglas, y en lo demás deja el orden que eligió el jugador.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::model::{lenient_version, InstalledMod, Mapping, ModId, RelPath};

/// Una regla entre dos mods. Las escribe el jugador o vienen de la curación (unricopie-data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "kebab-case")]
pub enum Rule {
    /// `then` carga después de `first`, así que sus archivos ganan.
    After { first: ModId, then: ModId },
    /// No pueden ir juntos.
    Incompatible { a: ModId, b: ModId, reason: String },
}

/// Un problema, dicho de forma que la interfaz pueda explicarlo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "kebab-case")]
pub enum Problem {
    /// El mod pide otro que no está instalado.
    MissingDependency { module: ModId, needs: ModId, version: Option<semver::VersionReq> },
    /// El mod pide otro que está instalado pero desactivado en este perfil.
    DisabledDependency { module: ModId, needs: ModId },
    /// La versión instalada de la dependencia no es la que pide.
    WrongVersion { module: ModId, needs: ModId, wants: semver::VersionReq, has: String },
    /// Dos mods activos que no pueden ir juntos.
    Incompatible { a: ModId, b: ModId, reason: String },
    /// Dependencias o reglas que se contradicen («A después de B» y «B después de A»).
    Cycle { mods: Vec<ModId> },
    /// Varios mods traen los mismos archivos. Gana `winner`.
    FileConflict { mods: Vec<ModId>, files: Vec<RelPath>, winner: ModId, resolved: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolution {
    /// Orden de carga final: el primero carga primero.
    pub order: Vec<ModId>,
    pub problems: Vec<Problem>,
}

impl Resolution {
    /// `true` si no hay nada que impida jugar (los conflictos resueltos no cuentan).
    pub fn is_clean(&self) -> bool {
        self.problems.iter().all(|p| matches!(p, Problem::FileConflict { resolved: true, .. }))
    }
}

/// Resuelve el orden de `enabled` (en el orden que eligió el jugador). `installed` son todos
/// los mods instalados del juego, activos o no, para distinguir «falta» de «desactivado».
pub fn resolve(installed: &[InstalledMod], enabled: &[ModId], rules: &[Rule], mapping: &Mapping) -> Resolution {
    let by_id: HashMap<&ModId, &InstalledMod> = installed.iter().map(|m| (&m.id, m)).collect();
    let active: Vec<&InstalledMod> = enabled.iter().filter_map(|id| by_id.get(id).copied()).collect();
    let active_ids: HashSet<&ModId> = active.iter().map(|m| &m.id).collect();
    let rank: HashMap<&ModId, usize> = active.iter().enumerate().map(|(i, m)| (&m.id, i)).collect();
    let mut problems = Vec::new();

    // Aristas «x carga antes que y».
    let mut edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for m in &active {
        for d in &m.dependencies {
            match by_id.get(&d.id) {
                None => problems.push(Problem::MissingDependency { module: m.id.clone(), needs: d.id.clone(), version: d.version.clone() }),
                Some(dep) if !active_ids.contains(&dep.id) => {
                    problems.push(Problem::DisabledDependency { module: m.id.clone(), needs: d.id.clone() })
                }
                Some(dep) => {
                    if let Some(req) = &d.version {
                        let ok = lenient_version(&dep.version).is_some_and(|v| req.matches(&v));
                        if !ok {
                            problems.push(Problem::WrongVersion { module: m.id.clone(), needs: d.id.clone(), wants: req.clone(), has: dep.version.clone() });
                        }
                    }
                    edges.entry(rank[&dep.id]).or_default().insert(rank[&m.id]);
                }
            }
        }
    }
    for r in rules {
        match r {
            Rule::After { first, then } => {
                if let (Some(&a), Some(&b)) = (rank.get(first), rank.get(then)) {
                    if a != b {
                        edges.entry(a).or_default().insert(b);
                    }
                }
            }
            Rule::Incompatible { a, b, reason } => {
                if active_ids.contains(a) && active_ids.contains(b) {
                    problems.push(Problem::Incompatible { a: a.clone(), b: b.clone(), reason: reason.clone() });
                }
            }
        }
    }

    // Orden topológico estable: entre los que pueden ir, primero el que el jugador puso antes.
    let n = active.len();
    let mut indegree = vec![0usize; n];
    for targets in edges.values() {
        for &t in targets {
            indegree[t] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order_idx = Vec::with_capacity(n);
    while let Some(i) = ready.pop_first() {
        order_idx.push(i);
        if let Some(targets) = edges.get(&i) {
            for &t in targets {
                indegree[t] -= 1;
                if indegree[t] == 0 {
                    ready.insert(t);
                }
            }
        }
    }
    if order_idx.len() < n {
        // Lo que quedó está en un ciclo (o depende de uno): va al final, en el orden del jugador.
        let stuck: Vec<usize> = (0..n).filter(|i| !order_idx.contains(i)).collect();
        problems.push(Problem::Cycle { mods: stuck.iter().map(|&i| active[i].id.clone()).collect() });
        order_idx.extend(stuck);
    }
    let order: Vec<ModId> = order_idx.iter().map(|&i| active[i].id.clone()).collect();
    let position: HashMap<usize, usize> = order_idx.iter().enumerate().map(|(pos, &i)| (i, pos)).collect();

    // Conflictos de archivos: quién trae cada ruta de destino.
    let mut providers: BTreeMap<String, (RelPath, Vec<usize>)> = BTreeMap::new();
    for (i, m) in active.iter().enumerate() {
        let mut seen = HashSet::new();
        for f in &m.files {
            if let Some(target) = mapping.target(&f.path) {
                if seen.insert(target.key()) {
                    providers.entry(target.key()).or_insert_with(|| (target.clone(), Vec::new())).1.push(i);
                }
            }
        }
    }
    let mut groups: BTreeMap<Vec<usize>, Vec<RelPath>> = BTreeMap::new();
    for (_, (path, mut who)) in providers {
        if who.len() > 1 {
            who.sort_by_key(|i| position[i]);
            groups.entry(who).or_default().push(path);
        }
    }
    for (who, files) in groups {
        let winner = *who.last().unwrap();
        let resolved = who[..who.len() - 1].iter().all(|&other| reaches(&edges, other, winner));
        problems.push(Problem::FileConflict {
            mods: who.iter().map(|&i| active[i].id.clone()).collect(),
            files,
            winner: active[winner].id.clone(),
            resolved,
        });
    }

    Resolution { order, problems }
}

/// ¿Hay un camino de aristas de `from` a `to`? (es decir, ¿algo obliga a que `to` cargue después?)
fn reaches(edges: &BTreeMap<usize, BTreeSet<usize>>, from: usize, to: usize) -> bool {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([from]);
    while let Some(x) = queue.pop_front() {
        if x == to {
            return true;
        }
        if seen.insert(x) {
            if let Some(next) = edges.get(&x) {
                queue.extend(next.iter().copied());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::model::{Curation, Dependency, ModFile, Source};

    fn m(id: &str, version: &str, files: &[&str], deps: &[(&str, Option<&str>)]) -> InstalledMod {
        InstalledMod {
            id: ModId::local(id),
            name: id.into(),
            version: version.into(),
            source: Source::Local,
            files: files.iter().map(|f| ModFile { path: RelPath::new(f).unwrap(), size: 1, hash: 1 }).collect(),
            dependencies: deps
                .iter()
                .map(|(d, v)| Dependency { id: ModId::local(d), version: v.map(|v| semver::VersionReq::parse(v).unwrap()) })
                .collect(),
            curation: Curation::Untested,
        }
    }
    fn ids(names: &[&str]) -> Vec<ModId> {
        names.iter().map(|n| ModId::local(n)).collect()
    }

    #[test]
    fn las_dependencias_cargan_antes() {
        let all = [m("ui", "1.0", &[], &[("api", Some(">=2.3.0"))]), m("api", "2.3.0", &[], &[])];
        let r = resolve(&all, &ids(&["ui", "api"]), &[], &Mapping::default());
        assert_eq!(r.order, ids(&["api", "ui"]));
        assert!(r.is_clean(), "{:?}", r.problems);
    }

    #[test]
    fn dice_que_falta_que_esta_desactivada_o_que_la_version_no_sirve() {
        let all = [
            m("a", "1", &[], &[("falta", None)]),
            m("b", "1", &[], &[("apagada", None)]),
            m("apagada", "1", &[], &[]),
            m("c", "1", &[], &[("vieja", Some(">=2.0.0"))]),
            m("vieja", "1.5", &[], &[]),
        ];
        let r = resolve(&all, &ids(&["a", "b", "c", "vieja"]), &[], &Mapping::default());
        assert!(r.problems.contains(&Problem::MissingDependency { module: ModId::local("a"), needs: ModId::local("falta"), version: None }));
        assert!(r.problems.contains(&Problem::DisabledDependency { module: ModId::local("b"), needs: ModId::local("apagada") }));
        assert!(r.problems.iter().any(|p| matches!(p, Problem::WrongVersion { has, .. } if has == "1.5")));
        assert!(!r.is_clean());
    }

    #[test]
    fn conflicto_sin_resolver_y_resuelto_con_una_regla() {
        let all = [m("rojo", "1", &["tex/link.o2r", "a.o2r"], &[]), m("azul", "1", &["TEX/Link.o2r"], &[])];
        let r = resolve(&all, &ids(&["rojo", "azul"]), &[], &Mapping::default());
        let conflict = r.problems.iter().find(|p| matches!(p, Problem::FileConflict { .. })).unwrap();
        match conflict {
            Problem::FileConflict { winner, resolved, files, .. } => {
                assert_eq!(winner, &ModId::local("azul"));
                assert!(!resolved);
                assert_eq!(files.len(), 1);
            }
            _ => unreachable!(),
        }
        assert!(!r.is_clean());

        // El jugador decide: azul después de rojo. Mismo ganador, pero resuelto.
        let rule = Rule::After { first: ModId::local("rojo"), then: ModId::local("azul") };
        let r = resolve(&all, &ids(&["rojo", "azul"]), &[rule], &Mapping::default());
        assert!(r.is_clean(), "{:?}", r.problems);

        // Y al revés: rojo después de azul, aunque el jugador los puso al revés.
        let rule = Rule::After { first: ModId::local("azul"), then: ModId::local("rojo") };
        let r = resolve(&all, &ids(&["rojo", "azul"]), &[rule], &Mapping::default());
        assert_eq!(r.order, ids(&["azul", "rojo"]));
        assert!(matches!(r.problems.as_slice(), [Problem::FileConflict { winner, resolved: true, .. }] if winner == &ModId::local("rojo")));
    }

    #[test]
    fn ciclos_e_incompatibles() {
        let all = [m("a", "1", &[], &[]), m("b", "1", &[], &[]), m("c", "1", &[], &[])];
        let rules = [
            Rule::After { first: ModId::local("a"), then: ModId::local("b") },
            Rule::After { first: ModId::local("b"), then: ModId::local("a") },
            Rule::Incompatible { a: ModId::local("a"), b: ModId::local("c"), reason: "cambian la misma cámara".into() },
        ];
        let r = resolve(&all, &ids(&["a", "b", "c"]), &rules, &Mapping::default());
        assert_eq!(r.order.len(), 3);
        assert!(r.problems.iter().any(|p| matches!(p, Problem::Cycle { mods } if mods.len() == 2)));
        assert!(r.problems.iter().any(|p| matches!(p, Problem::Incompatible { .. })));
    }

    #[test]
    fn sin_dependencias_respeta_el_orden_del_jugador() {
        let all = [m("x", "1", &[], &[]), m("y", "1", &[], &[]), m("z", "1", &[], &[])];
        let r = resolve(&all, &ids(&["z", "x", "y"]), &[], &Mapping::default());
        assert_eq!(r.order, ids(&["z", "x", "y"]));
    }
}
