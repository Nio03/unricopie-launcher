//! Perfiles: qué mods están activos y en qué orden, por juego. Se pueden tener varios (por
//! ejemplo «Vanilla+», «Texturas HD» y «Randomizer») y cambiar entre ellos: cambiar de perfil
//! es purgar el despliegue y desplegar el del otro.
//!
//! También está aquí el aviso antes de actualizar el juego: cuántos mods del perfil están
//! probados con la versión nueva y cuántos no, para ofrecer las tres opciones (actualizar y
//! desactivar los no probados, quedarse en la versión actual o actualizar de todas formas).
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{InstalledMod, ModId, RelPath};
use super::solver::Rule;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub id: ModId,
    pub enabled: bool,
    /// Archivos del mod que no se ponen en este perfil: las variantes que el jugador no eligió
    /// (Modern Controller Overhaul trae un `.rtz` por mando).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off: Vec<RelPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Id del juego en el catálogo (por ejemplo `recomps/zelda-majoras-mask`).
    pub game: String,
    /// Todos los mods del perfil, en el orden que eligió el jugador.
    pub entries: Vec<ProfileEntry>,
    /// Reglas del jugador. Las de la curación se suman al resolver.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Profile {
    pub fn new(name: &str, game: &str) -> Self {
        Profile { name: name.into(), game: game.into(), entries: vec![], rules: vec![] }
    }

    /// Los activos, en el orden del jugador.
    pub fn enabled(&self) -> Vec<ModId> {
        self.entries.iter().filter(|e| e.enabled).map(|e| e.id.clone()).collect()
    }

    /// Añade un mod al final (activo) si no estaba.
    pub fn add(&mut self, id: ModId) {
        if !self.entries.iter().any(|e| e.id == id) {
            self.entries.push(ProfileEntry { id, enabled: true, off: Vec::new() });
        }
    }

    pub fn entry(&self, id: &ModId) -> Option<&ProfileEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    /// Pone `id` justo antes (o después, con `after`) de `target` en el orden del jugador, y quita
    /// las reglas suyas que digan lo contrario (si no, el solucionador lo devolvería a su sitio).
    pub fn move_near(&mut self, id: &ModId, target: &ModId, after: bool) {
        if id == target {
            return;
        }
        let Some(pos) = self.entries.iter().position(|e| &e.id == id) else { return };
        let e = self.entries.remove(pos);
        match self.entries.iter().position(|e| &e.id == target) {
            Some(t) => self.entries.insert(if after { t + 1 } else { t }, e),
            None => self.entries.insert(pos.min(self.entries.len()), e),
        }
        let (antes, despues) = if after { (target, id) } else { (id, target) };
        self.rules.retain(|r| !matches!(r, Rule::After { first, then } if first == despues && then == antes));
    }

    /// Que `winner` gane a `others` cuando traen los mismos archivos: carga después de ellos.
    /// Las reglas que dijeran lo contrario se quitan.
    pub fn prefer(&mut self, winner: &ModId, others: &[ModId]) {
        self.rules.retain(|r| {
            !matches!(r, Rule::After { first, then }
                if (first == winner && others.contains(then)) || (then == winner && others.contains(first)))
        });
        for o in others {
            if o != winner {
                self.rules.push(Rule::After { first: o.clone(), then: winner.clone() });
            }
        }
    }

    /// Quita un mod del perfil (y las reglas que lo nombran).
    pub fn remove(&mut self, id: &ModId) {
        self.entries.retain(|e| &e.id != id);
        self.rules.retain(|r| match r {
            Rule::After { first, then } => first != id && then != id,
            Rule::Incompatible { a, b, .. } => a != id && b != id,
        });
    }

    /// Enciende o apaga un archivo del mod en este perfil.
    pub fn set_file(&mut self, id: &ModId, file: &RelPath, on: bool) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
            e.off.retain(|f| f.key() != file.key());
            if !on {
                e.off.push(file.clone());
                e.off.sort();
            }
        }
    }

    /// `m` como se ve en este perfil: sin los archivos apagados.
    pub fn view(&self, m: &InstalledMod) -> InstalledMod {
        let mut m = m.clone();
        if let Some(e) = self.entry(&m.id).filter(|e| !e.off.is_empty()) {
            m.files.retain(|f| !e.off.iter().any(|o| o.key() == f.path.key()));
        }
        m
    }

    pub fn set_enabled(&mut self, id: &ModId, enabled: bool) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
            e.enabled = enabled;
        }
    }

    /// Mueve un mod a otra posición del orden.
    pub fn move_to(&mut self, id: &ModId, index: usize) {
        if let Some(pos) = self.entries.iter().position(|e| &e.id == id) {
            let e = self.entries.remove(pos);
            let index = index.min(self.entries.len());
            self.entries.insert(index, e);
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Guarda con escritura atómica: primero a un temporal y luego se renombra, para que un
    /// corte de luz no deje el perfil a medias.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }
}

/// Qué mods activos del perfil están probados con una versión del juego.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateCheck {
    pub tested: Vec<ModId>,
    pub untested: Vec<ModId>,
}

impl UpdateCheck {
    /// Sin mods activos, o todos probados: se puede actualizar sin preguntar.
    pub fn needs_question(&self) -> bool {
        !self.untested.is_empty()
    }
}

/// El aviso antes de actualizar el juego a `new_version`.
pub fn update_check(profile: &Profile, installed: &[InstalledMod], new_version: &str) -> UpdateCheck {
    let mut check = UpdateCheck::default();
    for id in profile.enabled() {
        let tested = installed.iter().find(|m| m.id == id).is_some_and(|m| m.curation.is_tested_with(new_version));
        if tested {
            check.tested.push(id);
        } else {
            check.untested.push(id);
        }
    }
    check
}

/// La opción «actualizar y desactivar los no probados»: devuelve el perfil con esos mods
/// desactivados (el jugador puede volver a activarlos cuando quiera).
pub fn disable_untested(profile: &Profile, check: &UpdateCheck) -> Profile {
    let mut p = profile.clone();
    for id in &check.untested {
        p.set_enabled(id, false);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::model::{Curation, Source};

    fn installed(id: &str, tested: &[&str]) -> InstalledMod {
        InstalledMod {
            id: ModId::local(id),
            name: id.into(),
            version: "1".into(),
            source: Source::Local,
            files: vec![],
            dependencies: vec![],
            curation: if tested.is_empty() {
                Curation::Untested
            } else {
                Curation::Tested { game_versions: tested.iter().map(|s| s.to_string()).collect() }
            },
        }
    }

    #[test]
    fn orden_activar_y_guardar() {
        let t = tempfile::tempdir().unwrap();
        let mut p = Profile::new("Texturas HD", "recomps/zelda-majoras-mask");
        for id in ["a", "b", "c"] {
            p.add(ModId::local(id));
        }
        p.add(ModId::local("a"));
        p.move_to(&ModId::local("c"), 0);
        p.set_enabled(&ModId::local("b"), false);
        assert_eq!(p.enabled(), vec![ModId::local("c"), ModId::local("a")]);
        let path = t.path().join("perfiles").join("hd.json");
        p.save(&path).unwrap();
        assert_eq!(Profile::load(&path).unwrap(), p);
    }

    #[test]
    fn mover_y_preferir() {
        let mut p = Profile::new("x", "g");
        for id in ["a", "b", "c"] {
            p.add(ModId::local(id));
        }
        let ids = |p: &Profile| p.entries.iter().map(|e| e.id.as_str().to_string()).collect::<Vec<_>>();
        // «c» después de «a»: mover «c» antes de «a» la contradice.
        p.rules.push(Rule::After { first: ModId::local("a"), then: ModId::local("c") });
        // «c» antes de «a»: la regla que decía lo contrario se va.
        p.move_near(&ModId::local("c"), &ModId::local("a"), false);
        assert_eq!(ids(&p), ["local:c", "local:a", "local:b"]);
        assert!(p.rules.is_empty());
        p.move_near(&ModId::local("c"), &ModId::local("b"), true);
        assert_eq!(ids(&p), ["local:a", "local:b", "local:c"]);
        p.prefer(&ModId::local("a"), &[ModId::local("b")]);
        assert_eq!(p.rules, [Rule::After { first: ModId::local("b"), then: ModId::local("a") }]);
        p.prefer(&ModId::local("b"), &[ModId::local("a")]);
        assert_eq!(p.rules, [Rule::After { first: ModId::local("a"), then: ModId::local("b") }]);
    }

    #[test]
    fn aviso_antes_de_actualizar() {
        let mut p = Profile::new("x", "g");
        p.add(ModId::local("probado"));
        p.add(ModId::local("viejo"));
        p.add(ModId::local("nuevo"));
        let all = [installed("probado", &["v1.2.2", "v1.3.0"]), installed("viejo", &["v1.2.2"]), installed("nuevo", &[])];
        let c = update_check(&p, &all, "v1.3.0");
        assert_eq!(c.tested, vec![ModId::local("probado")]);
        assert_eq!(c.untested, vec![ModId::local("viejo"), ModId::local("nuevo")]);
        assert!(c.needs_question());
        let safe = disable_untested(&p, &c);
        assert_eq!(safe.enabled(), vec![ModId::local("probado")]);
    }
}
