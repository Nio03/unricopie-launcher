//! Los mods de un juego instalado, juntos: lo que se bajó, los perfiles y lo que está puesto en
//! la carpeta del juego. Es lo que usa la interfaz.
//!
//! Todo vive en `<raíz>/mods/<carpeta del juego>/`:
//! - `staging/`: cada mod descomprimido una vez (`staging`);
//! - `instalados.json`: los mods bajados, con sus archivos;
//! - `perfiles.json`: los perfiles y cuál está activo;
//! - `despliegue.json`: lo que está puesto ahora en el juego, para poder quitarlo;
//! - `copias/`: los archivos del juego que pisó un mod, para devolverlos.
//!
//! Cada cambio (activar, instalar, quitar, cambiar de perfil) termina en `apply`: quita lo que
//! había, resuelve el orden del perfil activo, pone sus archivos y, si el juego guarda en un
//! archivo cuáles están activos (`Loader`), se lo escribe. Así la carpeta del juego siempre
//! muestra el perfil activo, aunque se abra el juego desde fuera de Unricopie.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::deploy::{self, Deployment, Method, PurgeReport};
use super::loader;
use super::model::{InstalledMod, Mapping, ModId, RelPath};
use super::profile::Profile;
use super::solver::{self, Resolution};
use super::sources::RemoteMod;
use super::staging::{Limits, Staging};
use crate::install::receipt::Receipt;
use crate::install::recipe::{Loader, ModsRule, Recipe};
use crate::install::settings;
use crate::{Error, Result};

/// Dónde se ponen los mods de un juego instalado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// La carpeta donde el juego los busca.
    pub dir: PathBuf,
    /// El archivo donde el juego guarda cuáles están activos, si Unricopie se lo escribe.
    pub loader: Option<(Loader, PathBuf)>,
    /// La carpeta donde el juego guarda los ajustes de cada mod, si la tiene: Unricopie la guarda
    /// y la restaura con cada perfil.
    pub settings: Option<PathBuf>,
}

/// La carpeta de mods del juego instalado en `game_dir`, según su receta y cómo quedó
/// instalado (portable o no). `None` si la receta no trae mods o la ruta no se sabe aquí.
pub fn target(recipe: &Recipe, receipt: &Receipt, game_dir: &Path) -> Option<Target> {
    let rule = recipe.mods.as_ref()?;
    let dir = match rule.dir.as_str() {
        "{data}" => settings::data_folder(recipe, receipt, game_dir)?,
        "{game}" => game_dir.to_path_buf(),
        f => settings::resolve(f, recipe, receipt, game_dir)?,
    };
    let (loader, ajustes) = match rule.loader {
        Some(Loader::N64recomp) => {
            let data = settings::data_folder(recipe, receipt, game_dir)?;
            (Some((Loader::N64recomp, data.join(loader::n64recomp::CONFIG))), Some(data.join(loader::n64recomp::SETTINGS)))
        }
        None => (None, None),
    };
    Some(Target { dir, loader, settings: ajustes })
}

/// Los perfiles de un juego y cuál está activo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    pub active: String,
    pub list: Vec<Profile>,
}

impl Profiles {
    /// El nombre del primer perfil. La interfaz lo traduce al mostrarlo.
    pub const FIRST: &'static str = "Principal";

    pub fn new(game: &str) -> Profiles {
        Profiles { active: Self::FIRST.into(), list: vec![Profile::new(Self::FIRST, game)] }
    }

    pub fn active(&self) -> &Profile {
        self.list.iter().find(|p| p.name == self.active).unwrap_or(&self.list[0])
    }

    pub fn active_mut(&mut self) -> &mut Profile {
        let i = self.list.iter().position(|p| p.name == self.active).unwrap_or(0);
        &mut self.list[i]
    }

    /// Un perfil nuevo, vacío o copiado del activo, y pasa a ser el activo. Si el nombre ya
    /// existe, se le suma un número.
    pub fn create(&mut self, name: &str, copy_active: bool) -> String {
        let name = self.free_name(name.trim());
        let mut p = if copy_active { self.active().clone() } else { Profile::new(&name, &self.active().game) };
        p.name = name.clone();
        self.list.push(p);
        self.active = name.clone();
        name
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Option<String> {
        let to = to.trim();
        if to.is_empty() || !self.list.iter().any(|p| p.name == from) {
            return None;
        }
        let to = if to == from { to.to_string() } else { self.free_name(to) };
        let p = self.list.iter_mut().find(|p| p.name == from)?;
        p.name = to.clone();
        if self.active == from {
            self.active = to.clone();
        }
        Some(to)
    }

    /// Borra un perfil; el último no se puede borrar. Si era el activo, pasa a serlo el primero.
    pub fn delete(&mut self, name: &str) -> bool {
        if self.list.len() < 2 || !self.list.iter().any(|p| p.name == name) {
            return false;
        }
        self.list.retain(|p| p.name != name);
        if self.active == name {
            self.active = self.list[0].name.clone();
        }
        true
    }

    fn free_name(&self, want: &str) -> String {
        let base = if want.is_empty() { Self::FIRST } else { want };
        let taken = |n: &str| self.list.iter().any(|p| p.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }
}

/// Lo que pasó al instalar un mod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub installed: InstalledMod,
    /// Si es nuevo en el perfil activo y trae varias variantes, cuáles (todas apagadas hasta
    /// que el jugador elija).
    pub choices: Vec<RelPath>,
}

/// Lo que pasó al aplicar el perfil activo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    pub resolution: Resolution,
    /// Lo que se quitó del despliegue anterior.
    pub purged: Option<PurgeReport>,
    /// Archivos puestos.
    pub files: usize,
}

/// Los mods de un juego instalado.
#[derive(Debug, Clone)]
pub struct GameMods {
    dir: PathBuf,
    /// La ficha del juego, para los perfiles nuevos.
    game: String,
}

impl GameMods {
    /// `folder` es el nombre de la carpeta del juego en la biblioteca (`zelda-majoras-mask`), así
    /// cada variante instalada tiene sus mods.
    pub fn new(root: &Path, folder: &str, game: &str) -> GameMods {
        GameMods { dir: root.join("mods").join(folder), game: game.to_string() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn staging(&self) -> Staging {
        Staging::new(self.dir.join("staging"))
    }

    pub fn installed(&self) -> Result<Vec<InstalledMod>> {
        Ok(self.load("instalados.json")?.unwrap_or_default())
    }

    pub fn profiles(&self) -> Result<Profiles> {
        let mut p: Profiles = self.load("perfiles.json")?.unwrap_or_else(|| Profiles::new(&self.game));
        if p.list.is_empty() {
            p = Profiles::new(&self.game);
        }
        Ok(p)
    }

    pub fn save_profiles(&self, p: &Profiles) -> Result<()> {
        self.save("perfiles.json", p)
    }

    pub fn deployment(&self) -> Result<Option<Deployment>> {
        Ok(self.load::<Deployment>("despliegue.json")?.map(|d| self.rebase(d)))
    }

    /// El registro guarda rutas absolutas. Si la carpeta de Unricopie se movió o se copió (el
    /// modo portable lo permite), las que caían dentro de la de antes se pasan a la de ahora: si
    /// no, purgar borraría los mods puestos en la carpeta original, y en la nueva quedarían los
    /// viejos. Lo que estaba fuera (la carpeta de datos del juego, sin modo portable) no cambia.
    ///
    /// La carpeta de antes sale de `backup_root`, que siempre es `<raíz>/mods/<juego>/copias`.
    fn rebase(&self, mut d: Deployment) -> Deployment {
        let Some(antes) = d.backup_root.parent() else { return d };
        if antes == self.dir {
            return d;
        }
        let (Some(vieja), Some(nueva)) = (antes.parent().and_then(Path::parent), self.dir.parent().and_then(Path::parent)) else { return d };
        let (vieja, nueva) = (vieja.to_path_buf(), nueva.to_path_buf());
        let pasar = |p: &mut PathBuf| {
            if let Ok(rel) = p.strip_prefix(&vieja) {
                *p = nueva.join(rel);
            }
        };
        pasar(&mut d.target_root);
        pasar(&mut d.backup_root);
        for b in &mut d.backups {
            pasar(&mut b.stored_at);
        }
        d
    }

    /// Guarda en staging el mod bajado en `archive` y lo suma, activo, al perfil activo. Si ya
    /// estaba en otra versión, la reemplaza. No toca la carpeta del juego: eso es `apply`.
    pub fn add(&self, remote: &RemoteMod, archive: &Path, rule: &ModsRule) -> Result<Added> {
        let files = self.staging().import_zip(archive, &remote.id, &remote.version, Limits::default())?;
        let m = InstalledMod {
            id: remote.id.clone(),
            name: remote.name.clone(),
            version: remote.version.clone(),
            source: remote.source.clone(),
            files,
            dependencies: remote.dependencies.clone(),
            curation: Default::default(),
        };
        self.store(m, rule)
    }

    /// Un mod que el jugador añadió a mano (soltándolo en la ventana o con «Añadir desde
    /// archivo»): un comprimido (se descomprime), una carpeta o un archivo suelto (un `.nrm`, un
    /// `.rtz`), que se guarda tal cual. Si no trae nada que el juego use, no se queda.
    pub fn add_local(&self, path: &Path, rule: &ModsRule) -> Result<Added> {
        let nombre = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = crate::archive::Kind::from_name(&nombre);
        let base = match kind {
            Some(_) => {
                let n = nombre.to_lowercase();
                let fin = [".tar.gz", ".tar.xz", ".tgz", ".txz", ".zip", ".7z"].iter().find(|e| n.ends_with(*e)).map_or(0, |e| e.len());
                nombre[..nombre.len() - fin].to_string()
            }
            None if path.is_dir() => nombre.clone(),
            None => path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or(nombre.clone()),
        };
        let id = ModId::local(&base);
        // Los mods añadidos a mano no traen versión que se pueda comparar.
        let version = "local";
        let staging = self.staging();
        let files = if path.is_dir() {
            staging.import_dir(path, &id, version, Limits::default())?
        } else if let Some(k) = kind {
            staging.import_archive(path, k, &id, version, Limits::default())?
        } else {
            staging.import_file(path, &id, version, Limits::default())?
        };
        let mapping = rule.mapping();
        if !files.iter().any(|f| mapping.target(&f.path).is_some()) {
            staging.remove(&id, version)?;
            return Err(Error::NotAMod(nombre));
        }
        let m = InstalledMod {
            id,
            name: base,
            version: version.into(),
            source: super::model::Source::Local,
            files,
            dependencies: Vec::new(),
            curation: Default::default(),
        };
        self.store(m, rule)
    }

    /// Lo común de `add` y `add_local`: la lista de instalados (reemplaza la versión anterior) y el
    /// perfil activo (activo, con las variantes por elegir).
    fn store(&self, m: InstalledMod, rule: &ModsRule) -> Result<Added> {
        let staging = self.staging();
        let mut installed = self.installed()?;
        if let Some(old) = installed.iter().find(|o| o.id == m.id) {
            if old.version != m.version {
                staging.remove(&old.id, &old.version)?;
            }
        }
        installed.retain(|o| o.id != m.id);
        installed.push(m.clone());
        self.save("instalados.json", &installed)?;

        let mut profiles = self.profiles()?;
        let choices = choices(&m, rule);
        let p = profiles.active_mut();
        let new_here = p.entry(&m.id).is_none();
        p.add(m.id.clone());
        p.set_enabled(&m.id, true);
        // Lo apagado que ya no viene en esta versión se olvida.
        if let Some(e) = p.entries.iter_mut().find(|e| e.id == m.id) {
            e.off.retain(|f| m.files.iter().any(|x| x.path.key() == f.key()));
        }
        let ask = new_here && choices.len() > 1;
        if ask {
            for f in &choices {
                p.set_file(&m.id, f, false);
            }
        }
        self.save_profiles(&profiles)?;
        Ok(Added { installed: m, choices: if ask { choices } else { Vec::new() } })
    }

    /// Quita un mod: de la lista, de todos los perfiles y de staging. Tampoco toca el juego.
    pub fn remove(&self, id: &ModId) -> Result<()> {
        let mut installed = self.installed()?;
        let Some(pos) = installed.iter().position(|m| &m.id == id) else { return Ok(()) };
        let m = installed.remove(pos);
        self.save("instalados.json", &installed)?;
        let mut profiles = self.profiles()?;
        for p in &mut profiles.list {
            p.remove(id);
        }
        self.save_profiles(&profiles)?;
        self.staging().remove(&m.id, &m.version)
    }

    /// El orden y los problemas del perfil activo, sin tocar nada.
    pub fn resolve(&self, rule: &ModsRule) -> Result<Resolution> {
        let installed = self.installed()?;
        let profiles = self.profiles()?;
        let p = profiles.active();
        let seen: Vec<InstalledMod> = installed.iter().map(|m| p.view(m)).collect();
        Ok(solver::resolve(&seen, &p.enabled(), &p.rules, &rule.mapping()))
    }

    /// Quita lo que había puesto y pone el perfil activo en `target`.
    pub fn apply(&self, rule: &ModsRule, target: &Target) -> Result<Applied> {
        let purged = self.purge()?;
        let installed = self.installed()?;
        let profiles = self.profiles()?;
        let p = profiles.active();
        let mapping = rule.mapping();
        let seen: Vec<InstalledMod> = installed.iter().map(|m| p.view(m)).collect();
        let resolution = solver::resolve(&seen, &p.enabled(), &p.rules, &mapping);
        let staging = self.staging();
        let in_order: Vec<(&InstalledMod, PathBuf)> = resolution
            .order
            .iter()
            .filter_map(|id| seen.iter().find(|m| &m.id == id))
            .map(|m| (m, staging.mod_dir(&m.id, &m.version)))
            .collect();
        let plan = deploy::plan(&in_order, &mapping);
        let dep = deploy::deploy(&plan, &target.dir, &self.dir.join("copias"), Method::HardLink)?;
        self.save("despliegue.json", &dep)?;

        if let Some((Loader::N64recomp, config)) = &target.loader {
            let n64 = |name: &str| loader::n64recomp::is_mod(name);
            let mut deployed = Vec::new();
            for id in &resolution.order {
                for f in dep.files.iter().filter(|f| &f.from == id && n64(f.path.file_name())) {
                    deployed.push(loader::n64recomp::mod_id(&f.path.under(&target.dir)));
                }
            }
            let managed: HashSet<String> = installed
                .iter()
                .flat_map(|m| {
                    let dir = staging.mod_dir(&m.id, &m.version);
                    m.files
                        .iter()
                        .filter(|f| mapping.target(&f.path).is_some() && n64(f.path.file_name()))
                        .map(move |f| loader::n64recomp::mod_id(&f.path.under(&dir)))
                })
                .collect();
            loader::n64recomp::write(config, &deployed, &managed)?;
        }
        Ok(Applied { resolution, purged, files: dep.files.len() })
    }

    /// La versión mínima del juego que piden los mods activos del perfil activo: el nombre de
    /// cada mod y la versión (`minimum_recomp_version` de sus `.nrm`). Solo los que la piden.
    pub fn minimum_versions(&self) -> Result<Vec<(String, String)>> {
        let activos = self.profiles()?.active().enabled();
        let staging = self.staging();
        let mut out = Vec::new();
        for m in self.installed()?.iter().filter(|m| activos.contains(&m.id)) {
            let dir = staging.mod_dir(&m.id, &m.version);
            let nrm = m.files.iter().filter(|f| f.path.as_str().to_ascii_lowercase().ends_with(".nrm"));
            if let Some(v) = nrm.filter_map(|f| loader::n64recomp::minimum_version(&f.path.under(&dir))).next() {
                out.push((m.name.clone(), v));
            }
        }
        Ok(out)
    }

    /// Quita del juego lo que puso el último `apply`.
    pub fn purge(&self) -> Result<Option<PurgeReport>> {
        let Some(dep) = self.deployment()? else { return Ok(None) };
        let report = deploy::purge(&dep)?;
        let path = self.dir.join("despliegue.json");
        fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
        Ok(Some(report))
    }

    // ---------- Los ajustes de los mods, por perfil ----------

    fn settings_store(&self, profile: &str) -> PathBuf {
        let seguro: String = profile.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
        self.dir.join("ajustes").join(seguro.trim_end_matches(['.', ' ']))
    }

    /// Guarda con el perfil `profile` los ajustes que el juego tiene ahora para sus mods (los
    /// archivos de `target.settings`). Devuelve cuántos guardó.
    pub fn save_settings(&self, profile: &str, target: &Target) -> Result<usize> {
        let Some(dir) = &target.settings else { return Ok(0) };
        let store = self.settings_store(profile);
        if store.exists() {
            fs::remove_dir_all(&store).map_err(|e| Error::io(&store, e))?;
        }
        copy_flat(dir, &store)
    }

    /// Pone en el juego los ajustes guardados con `profile`, si los hay. Solo pisa esos archivos:
    /// los de mods que no estaban cuando se guardaron quedan como están.
    pub fn restore_settings(&self, profile: &str, target: &Target) -> Result<usize> {
        let Some(dir) = &target.settings else { return Ok(0) };
        copy_flat(&self.settings_store(profile), dir)
    }

    /// Un perfil nuevo copiado de otro empieza con sus mismos ajustes.
    pub fn copy_settings(&self, from: &str, to: &str) -> Result<usize> {
        copy_flat(&self.settings_store(from), &self.settings_store(to))
    }

    pub fn rename_settings(&self, from: &str, to: &str) -> Result<()> {
        let (a, b) = (self.settings_store(from), self.settings_store(to));
        if a.exists() && !b.exists() {
            fs::rename(&a, &b).map_err(|e| Error::io(&a, e))?;
        }
        Ok(())
    }

    pub fn delete_settings(&self, profile: &str) -> Result<()> {
        let d = self.settings_store(profile);
        if d.exists() {
            fs::remove_dir_all(&d).map_err(|e| Error::io(&d, e))?;
        }
        Ok(())
    }

    // ---------- Lo que cambió el jugador en el menú del juego ----------

    /// Adopta en el perfil activo lo que el jugador cambió en el menú de mods del juego desde el
    /// último despliegue: los mods (o variantes) que apagó y el orden en que los dejó. Así no se
    /// pisa al volver a desplegar. Devuelve cuántos cambios adoptó.
    pub fn adopt(&self, rule: &ModsRule, target: &Target) -> Result<usize> {
        let Some((Loader::N64recomp, config)) = &target.loader else { return Ok(0) };
        let Some(dep) = self.deployment()? else { return Ok(0) };
        let Some((activos, orden)) = loader::n64recomp::read_config(config) else { return Ok(0) };
        let installed = self.installed()?;
        let mapping = rule.mapping();
        // Cada mod puesto: su id para el juego, de qué mod es, de qué archivo sale y si es una variante.
        let mut puestos: Vec<(String, ModId, RelPath, bool)> = Vec::new();
        for f in &dep.files {
            if !loader::n64recomp::is_mod(f.path.file_name()) {
                continue;
            }
            let en_disco = f.path.under(&target.dir);
            let (Some(m), true) = (installed.iter().find(|m| m.id == f.from), en_disco.is_file()) else { continue };
            let Some(src) = m.files.iter().find(|x| mapping.target(&x.path).is_some_and(|t| t.key() == f.path.key())) else { continue };
            let variante = choices(m, rule).iter().any(|c| c.key() == src.path.key());
            puestos.push((loader::n64recomp::mod_id(&en_disco), m.id.clone(), src.path.clone(), variante));
        }
        let mut profiles = self.profiles()?;
        // El orden que escribió Unricopie (el resuelto), para ver si el juego lo cambió.
        let escrito = {
            let p = profiles.active();
            let seen: Vec<InstalledMod> = installed.iter().map(|m| p.view(m)).collect();
            solver::resolve(&seen, &p.enabled(), &p.rules, &mapping).order
        };
        let p = profiles.active_mut();
        let mut cambios = 0;
        for (n64, de, archivo, variante) in &puestos {
            if !orden.contains(n64) || activos.contains(n64) {
                continue;
            }
            if *variante {
                if !p.entry(de).is_some_and(|e| e.off.iter().any(|o| o.key() == archivo.key())) {
                    p.set_file(de, archivo, false);
                    cambios += 1;
                }
            } else if p.entry(de).is_some_and(|e| e.enabled) {
                p.set_enabled(de, false);
                cambios += 1;
            }
        }
        // El orden en que el juego tiene los mods principales, comparado con el que escribimos.
        let mut juego: Vec<ModId> = Vec::new();
        for n in &orden {
            if let Some((_, de, _, _)) = puestos.iter().find(|(x, _, _, v)| x == n && !*v) {
                if !juego.contains(de) {
                    juego.push(de.clone());
                }
            }
        }
        let antes: Vec<ModId> = escrito.into_iter().filter(|id| juego.contains(id)).collect();
        if juego.len() > 1 && juego != antes {
            let mut huecos: Vec<usize> = juego.iter().filter_map(|id| p.entries.iter().position(|e| &e.id == id)).collect();
            huecos.sort_unstable();
            let entradas: Vec<_> = juego.iter().filter_map(|id| p.entry(id).cloned()).collect();
            for (hueco, e) in huecos.into_iter().zip(entradas) {
                p.entries[hueco] = e;
            }
            cambios += 1;
        }
        if cambios > 0 {
            self.save_profiles(&profiles)?;
        }
        Ok(cambios)
    }

    fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.dir.join(name);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Escritura atómica: a un temporal y luego se renombra.
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        fs::write(&tmp, serde_json::to_string_pretty(value)?).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }
}

/// Copia los archivos sueltos de `from` (no sus carpetas) a `to`. Sin `from`, nada.
fn copy_flat(from: &Path, to: &Path) -> Result<usize> {
    let Ok(leidos) = fs::read_dir(from) else { return Ok(0) };
    fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    let mut n = 0;
    for e in leidos.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            let dest = to.join(e.file_name());
            fs::copy(e.path(), &dest).map_err(|err| Error::io(&dest, err))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Los archivos de `m` entre los que el jugador elige, según `choose` de la receta: solo si
/// trae más de uno de ese tipo y se ponen en el juego.
pub fn choices(m: &InstalledMod, rule: &ModsRule) -> Vec<RelPath> {
    let mapping: Mapping = rule.mapping();
    let v: Vec<RelPath> = m
        .files
        .iter()
        .filter(|f| rule.is_choice(f.path.file_name()) && mapping.target(&f.path).is_some())
        .map(|f| f.path.clone())
        .collect();
    if v.len() > 1 { v } else { Vec::new() }
}

/// Lo que hay que bajar para instalar `id`: sus dependencias que faltan (de la lista de su
/// fuente, primero las más hondas) y él mismo al final, salvo lo que ya está instalado en la
/// versión que se pide. Las que no están en la lista no salen: el solucionador avisará.
pub fn with_dependencies<'a>(listing: &'a [RemoteMod], id: &ModId, installed: &[InstalledMod]) -> Vec<&'a RemoteMod> {
    let by_id: HashMap<&ModId, &RemoteMod> = listing.iter().map(|m| (&m.id, m)).collect();
    let have: HashMap<&ModId, &str> = installed.iter().map(|m| (&m.id, m.version.as_str())).collect();
    let mut out: Vec<&RemoteMod> = Vec::new();
    let mut seen = HashSet::new();
    fn visit<'a>(
        id: &ModId,
        req: Option<&semver::VersionReq>,
        top: bool,
        by_id: &HashMap<&ModId, &'a RemoteMod>,
        have: &HashMap<&ModId, &str>,
        seen: &mut HashSet<ModId>,
        out: &mut Vec<&'a RemoteMod>,
    ) {
        if !seen.insert(id.clone()) {
            return;
        }
        let Some(m) = by_id.get(id) else { return };
        for d in &m.dependencies {
            visit(&d.id, d.version.as_ref(), false, by_id, have, seen, out);
        }
        let ok = match have.get(id) {
            None => false,
            // El que pidió el jugador se baja si su versión es otra; una dependencia, solo si la
            // que hay no cumple lo que piden.
            Some(v) if top => *v == m.version,
            Some(v) => match req {
                None => true,
                Some(r) => super::model::lenient_version(v).is_some_and(|v| r.matches(&v)),
            },
        };
        if !ok {
            out.push(m);
        }
    }
    visit(id, None, true, &by_id, &have, &mut seen, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::model::{Dependency, Source};
    use crate::mods::sources::RemoteFile;
    use serde_json::Value;
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    /// Un `.nrm` de verdad (un zip con su `mod.json`).
    fn nrm(id: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file("mod.json", SimpleFileOptions::default()).unwrap();
        w.write_all(format!(r#"{{ "id": "{id}" }}"#).as_bytes()).unwrap();
        w.finish().unwrap();
        buf.into_inner()
    }

    fn remote(name: &str, version: &str, deps: &[(&str, &str)]) -> RemoteMod {
        RemoteMod {
            id: ModId::thunderstore(name),
            name: name.into(),
            author: "a".into(),
            version: version.into(),
            source: Source::Thunderstore { community: "zelda-64-recompiled".into(), full_name: name.into() },
            page_url: String::new(),
            files: vec![RemoteFile { name: format!("{name}.zip"), url: String::new(), size: None, version: None, description: None, md5: None, scan_ok: None }],
            dependencies: deps
                .iter()
                .map(|(d, v)| Dependency { id: ModId::thunderstore(d), version: Some(semver::VersionReq::parse(v).unwrap()) })
                .collect(),
            deprecated: false,
            info: Default::default(),
        }
    }

    fn zelda() -> ModsRule {
        ModsRule {
            thunderstore: Some("zelda-64-recompiled".into()),
            dir: "{data}/mods".into(),
            loader: Some(Loader::N64recomp),
            extensions: vec![".nrm".into(), ".rtz".into(), ".dll".into(), ".so".into(), ".dylib".into()],
            flatten: true,
            choose: vec![".rtz".into()],
            ..Default::default()
        }
    }

    /// Copia una carpeta entera (para simular que la persona copia su Unricopie portable).
    fn copiar(de: &Path, a: &Path) {
        fs::create_dir_all(a).unwrap();
        for e in fs::read_dir(de).unwrap() {
            let e = e.unwrap();
            let destino = a.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copiar(&e.path(), &destino);
            } else {
                fs::copy(e.path(), &destino).unwrap();
            }
        }
    }

    #[test]
    fn una_carpeta_portable_copiada_no_toca_los_mods_de_la_original() {
        let t = tempfile::tempdir().unwrap();
        let original = t.path().join("Unricopie");
        // Modo portable: el juego y sus mods, dentro de la carpeta de Unricopie. El juego ya
        // traía un `api.nrm` propio, que se guarda en `copias` y vuelve al purgar.
        let target = |raiz: &Path| {
            let data = raiz.join("games").join("zelda-majoras-mask");
            Target { dir: data.join("mods"), loader: Some((Loader::N64recomp, data.join("mods.json"))), settings: None }
        };
        let gm = GameMods::new(&original, "zelda-majoras-mask", "recomps/zelda-majoras-mask");
        let rule = zelda();
        let api = t.path().join("api.zip");
        zip(&api, &[("manifest.json", b"{}"), ("api.nrm", &nrm("ez_api"))]);
        gm.add(&remote("LT_Schmiddy-EZ_Text_Replacer_API", "2.3.0", &[]), &api, &rule).unwrap();
        let t_orig = target(&original);
        fs::create_dir_all(&t_orig.dir).unwrap();
        fs::write(t_orig.dir.join("api.nrm"), b"del juego").unwrap();
        gm.apply(&rule, &t_orig).unwrap();
        assert_eq!(fs::read(t_orig.dir.join("api.nrm")).unwrap(), nrm("ez_api"));

        // La persona copia la carpeta y, en la copia, quita los mods.
        let copia = t.path().join("Unricopie en el USB");
        copiar(&original, &copia);
        let gm_copia = GameMods::new(&copia, "zelda-majoras-mask", "recomps/zelda-majoras-mask");
        let dep = gm_copia.deployment().unwrap().unwrap();
        assert_eq!(dep.target_root, target(&copia).dir);
        assert!(dep.backup_root.starts_with(&copia) && dep.backups.iter().all(|b| b.stored_at.starts_with(&copia)));
        let r = gm_copia.purge().unwrap().unwrap();
        assert_eq!((r.removed, r.restored), (1, 1));
        assert_eq!(fs::read(target(&copia).dir.join("api.nrm")).unwrap(), b"del juego", "en la copia vuelve el del juego");
        assert_eq!(fs::read(t_orig.dir.join("api.nrm")).unwrap(), nrm("ez_api"), "la original no se toca");

        // En la original, todo sigue igual: su registro apunta a ella.
        assert_eq!(gm.deployment().unwrap().unwrap().target_root, t_orig.dir);
        assert_eq!(gm.purge().unwrap().unwrap().restored, 1);
        assert_eq!(fs::read(t_orig.dir.join("api.nrm")).unwrap(), b"del juego");
    }

    #[test]
    fn instalar_elegir_variante_aplicar_y_cambiar_de_perfil() {
        let t = tempfile::tempdir().unwrap();
        let gm = GameMods::new(&t.path().join("raiz"), "zelda-majoras-mask", "recomps/zelda-majoras-mask");
        let data = t.path().join("Zelda64Recompiled");
        let target = Target {
            dir: data.join("mods"),
            loader: Some((Loader::N64recomp, data.join("mods.json"))),
            settings: Some(data.join("mod_config")),
        };
        let rule = zelda();

        let api = t.path().join("api.zip");
        zip(&api, &[("manifest.json", b"{}"), ("icon.png", b"png"), ("api.nrm", &nrm("ez_api"))]);
        let mco = t.path().join("mco.zip");
        zip(&mco, &[
            ("manifest.json", b"{}"),
            ("README.md", b"#"),
            ("mco.nrm", &nrm("mm_mco")),
            ("MCO_Xbox.rtz", b"xbox"),
            ("MCO_Deck.rtz", b"deck"),
        ]);
        let r_api = remote("LT_Schmiddy-EZ_Text_Replacer_API", "2.3.0", &[]);
        let r_mco = remote("tomtee-Modern_Controller_Overhaul", "4.0.3", &[("LT_Schmiddy-EZ_Text_Replacer_API", ">=2.3.0")]);

        // El jugador instala MCO: se pide su dependencia primero.
        let listing = [r_mco.clone(), r_api.clone()];
        let todo = with_dependencies(&listing, &r_mco.id, &[]);
        assert_eq!(todo.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), [r_api.id.as_str(), r_mco.id.as_str()]);

        assert!(gm.add(&r_api, &api, &rule).unwrap().choices.is_empty());
        let added = gm.add(&r_mco, &mco, &rule).unwrap();
        assert_eq!(added.choices.len(), 2, "trae dos .rtz: se pregunta");

        // Sin elegir, ninguna variante se pone; los .nrm sí, con la dependencia antes.
        let a = gm.apply(&rule, &target).unwrap();
        assert!(a.resolution.is_clean(), "{:?}", a.resolution.problems);
        assert_eq!(a.resolution.order, [r_api.id.clone(), r_mco.id.clone()]);
        assert!(data.join("mods").join("api.nrm").is_file() && data.join("mods").join("mco.nrm").is_file());
        assert!(!data.join("mods").join("MCO_Xbox.rtz").exists() && !data.join("mods").join("manifest.json").exists());
        let cfg: Value = serde_json::from_slice(&fs::read(data.join("mods.json")).unwrap()).unwrap();
        assert_eq!(cfg["enabled_mods"], serde_json::json!(["ez_api", "mm_mco"]));

        // Elige la de Xbox.
        let mut ps = gm.profiles().unwrap();
        ps.active_mut().set_file(&r_mco.id, &RelPath::new("MCO_Xbox.rtz").unwrap(), true);
        gm.save_profiles(&ps).unwrap();
        gm.apply(&rule, &target).unwrap();
        assert_eq!(fs::read(data.join("mods").join("MCO_Xbox.rtz")).unwrap(), b"xbox");
        assert!(!data.join("mods").join("MCO_Deck.rtz").exists());
        let cfg: Value = serde_json::from_slice(&fs::read(data.join("mods.json")).unwrap()).unwrap();
        assert_eq!(cfg["enabled_mods"], serde_json::json!(["ez_api", "mm_mco", "MCO_Xbox"]));

        // Un perfil nuevo y vacío: la carpeta queda sin nuestros mods y el juego no los activa.
        let mut ps = gm.profiles().unwrap();
        assert_eq!(ps.create("Sin mods", false), "Sin mods");
        gm.save_profiles(&ps).unwrap();
        let a = gm.apply(&rule, &target).unwrap();
        assert_eq!(a.files, 0);
        assert_eq!(a.purged.unwrap().removed, 3);
        assert!(fs::read_dir(data.join("mods")).unwrap().next().is_none());
        let cfg: Value = serde_json::from_slice(&fs::read(data.join("mods.json")).unwrap()).unwrap();
        assert_eq!(cfg["enabled_mods"], serde_json::json!([]));

        // De vuelta al principal, y quitar MCO: queda la API.
        let mut ps = gm.profiles().unwrap();
        ps.active = Profiles::FIRST.into();
        gm.save_profiles(&ps).unwrap();
        gm.remove(&r_mco.id).unwrap();
        let a = gm.apply(&rule, &target).unwrap();
        assert_eq!(a.files, 1);
        assert!(!gm.staging().mod_dir(&r_mco.id, "4.0.3").exists());
        assert!(gm.profiles().unwrap().list.iter().all(|p| p.entry(&r_mco.id).is_none()));
    }

    #[test]
    fn mods_propios_ajustes_por_perfil_y_lo_cambiado_en_el_juego() {
        let t = tempfile::tempdir().unwrap();
        let gm = GameMods::new(&t.path().join("raiz"), "z", "recomps/zelda-majoras-mask");
        let data = t.path().join("Zelda64Recompiled");
        let target = Target {
            dir: data.join("mods"),
            loader: Some((Loader::N64recomp, data.join("mods.json"))),
            settings: Some(data.join("mod_config")),
        };
        let rule = zelda();

        // Un .nrm suelto, un zip con un .rtz y una carpeta: los tres entran.
        let sueltos = t.path().join("sueltos");
        fs::create_dir_all(&sueltos).unwrap();
        fs::write(sueltos.join("camara.nrm"), nrm("mm_camara")).unwrap();
        zip(&sueltos.join("Texturas HD.zip"), &[("README.md", b"#"), ("hd/Texturas.rtz", b"rtz")]);
        let carpeta = sueltos.join("musica");
        fs::create_dir_all(&carpeta).unwrap();
        fs::write(carpeta.join("musica.nrm"), nrm("mm_musica")).unwrap();
        for p in [sueltos.join("camara.nrm"), sueltos.join("Texturas HD.zip"), carpeta.clone()] {
            gm.add_local(&p, &rule).unwrap();
        }
        // Algo que no sirve como mod no se queda.
        fs::write(sueltos.join("leeme.txt"), b"hola").unwrap();
        assert!(matches!(gm.add_local(&sueltos.join("leeme.txt"), &rule), Err(Error::NotAMod(_))));
        assert!(!gm.staging().mod_dir(&ModId::local("leeme"), "local").exists());
        let ids: Vec<String> = gm.installed().unwrap().iter().map(|m| m.id.as_str().to_string()).collect();
        assert_eq!(ids, ["local:camara", "local:Texturas HD", "local:musica"]);
        gm.apply(&rule, &target).unwrap();
        let mut puestos: Vec<String> = fs::read_dir(data.join("mods")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        puestos.sort();
        assert_eq!(puestos, ["Texturas.rtz", "camara.nrm", "musica.nrm"]);

        // Reordenar: la música antes que la cámara.
        let mut ps = gm.profiles().unwrap();
        ps.active_mut().move_near(&ModId::local("musica"), &ModId::local("camara"), false);
        gm.save_profiles(&ps).unwrap();
        gm.apply(&rule, &target).unwrap();
        let cfg: serde_json::Value = serde_json::from_slice(&fs::read(data.join("mods.json")).unwrap()).unwrap();
        assert_eq!(cfg["mod_order"], serde_json::json!(["mm_musica", "mm_camara", "Texturas"]));

        // En el menú del juego, el jugador apaga la cámara y pone las texturas primero.
        fs::write(
            data.join("mods.json"),
            r#"{ "enabled_mods": ["mm_musica", "Texturas"], "mod_order": ["Texturas", "mm_musica", "mm_camara"] }"#,
        )
        .unwrap();
        assert_eq!(gm.adopt(&rule, &target).unwrap(), 2);
        let p = gm.profiles().unwrap();
        assert!(!p.active().entry(&ModId::local("camara")).unwrap().enabled);
        let orden: Vec<&str> = p.active().entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(orden, ["local:Texturas HD", "local:musica", "local:camara"]);
        // Desplegado de nuevo, ya no hay nada que adoptar.
        gm.apply(&rule, &target).unwrap();
        assert_eq!(gm.adopt(&rule, &target).unwrap(), 0);

        // Los ajustes de los mods van con cada perfil.
        let ajustes = data.join("mod_config");
        fs::create_dir_all(&ajustes).unwrap();
        fs::write(ajustes.join("mm_musica.json"), r#"{"volumen": 80}"#).unwrap();
        gm.save_settings("Principal", &target).unwrap();
        fs::write(ajustes.join("mm_musica.json"), r#"{"volumen": 20}"#).unwrap();
        gm.save_settings("Silencio", &target).unwrap();
        assert_eq!(gm.restore_settings("Principal", &target).unwrap(), 1);
        assert_eq!(fs::read_to_string(ajustes.join("mm_musica.json")).unwrap(), r#"{"volumen": 80}"#);
        gm.rename_settings("Silencio", "Calma").unwrap();
        gm.restore_settings("Calma", &target).unwrap();
        assert_eq!(fs::read_to_string(ajustes.join("mm_musica.json")).unwrap(), r#"{"volumen": 20}"#);
        gm.delete_settings("Calma").unwrap();
        assert_eq!(gm.restore_settings("Calma", &target).unwrap(), 0);
    }

    #[test]
    fn falta_una_dependencia_y_se_dice() {
        let t = tempfile::tempdir().unwrap();
        let gm = GameMods::new(t.path(), "g", "recomps/g");
        let rule = zelda();
        let mco = t.path().join("mco.zip");
        zip(&mco, &[("mco.nrm", &nrm("mm_mco"))]);
        let r_mco = remote("tomtee-Modern_Controller_Overhaul", "4.0.3", &[("LT_Schmiddy-EZ_Text_Replacer_API", ">=2.3.0")]);
        gm.add(&r_mco, &mco, &rule).unwrap();
        let r = gm.resolve(&rule).unwrap();
        assert!(matches!(r.problems.as_slice(), [solver::Problem::MissingDependency { .. }]), "{:?}", r.problems);
    }

    #[test]
    fn perfiles_con_nombres_libres() {
        let mut ps = Profiles::new("recomps/g");
        assert_eq!(ps.create("Principal", true), "Principal 2");
        assert_eq!(ps.active, "Principal 2");
        assert_eq!(ps.rename("Principal 2", "Texturas HD").as_deref(), Some("Texturas HD"));
        assert_eq!(ps.active, "Texturas HD");
        assert!(ps.delete("Texturas HD"));
        assert_eq!(ps.active, "Principal");
        assert!(!ps.delete("Principal"), "el último no se borra");
    }

    #[test]
    fn dependencias_que_ya_estan_no_se_bajan() {
        let api = remote("api", "2.3.0", &[]);
        let ui = remote("ui", "1.0.0", &[("api", ">=2.0.0")]);
        let installed = vec![InstalledMod {
            id: api.id.clone(),
            name: "api".into(),
            version: "2.1.0".into(),
            source: Source::Local,
            files: vec![],
            dependencies: vec![],
            curation: Default::default(),
        }];
        let listing = [ui.clone(), api.clone()];
        let todo = with_dependencies(&listing, &ui.id, &installed);
        assert_eq!(todo.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), [ui.id.as_str()]);
        // Si la instalada no cumple, se baja la nueva.
        let listing = [remote("ui", "1.0.0", &[("api", ">=2.3.0")]), api.clone()];
        let todo = with_dependencies(&listing, &ui.id, &installed);
        assert_eq!(todo.len(), 2);
    }
}
