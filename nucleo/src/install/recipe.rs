//! La receta de instalación de un proyecto, tal como la define `FORMATO.md` en unricopie-data,
//! y cómo se elige lo que toca para el equipo del jugador.
//!
//! Las recetas se escriben en YAML en ese repo privado; aquí llegan ya convertidas a JSON (el
//! núcleo no lee YAML). No se rechazan campos desconocidos: una receta nueva no debe romper un
//! launcher viejo. La que valida el formato es `scripts/validar.py`, en unricopie-data.
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::data::DataPath;
use crate::{Error, Result};

/// Las plataformas de FORMATO.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    WindowsX64,
    WindowsX86,
    WindowsArm64,
    LinuxX64,
    LinuxArm64,
    /// Un paquete Flatpak: se elige a propósito, nunca como sustituto de `linux-x64`.
    LinuxFlatpak,
    /// Compilación universal de macOS.
    Macos,
    MacosArm64,
    MacosX64,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::WindowsX64 => "windows-x64",
            Platform::WindowsX86 => "windows-x86",
            Platform::WindowsArm64 => "windows-arm64",
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::LinuxFlatpak => "linux-flatpak",
            Platform::Macos => "macos",
            Platform::MacosArm64 => "macos-arm64",
            Platform::MacosX64 => "macos-x64",
        }
    }

    /// La plataforma de este equipo, si es una de las que conocen las recetas.
    pub fn current() -> Option<Platform> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => Some(Platform::WindowsX64),
            ("windows", "x86") => Some(Platform::WindowsX86),
            ("windows", "aarch64") => Some(Platform::WindowsArm64),
            ("linux", "x86_64") => Some(Platform::LinuxX64),
            ("linux", "aarch64") => Some(Platform::LinuxArm64),
            ("macos", "aarch64") => Some(Platform::MacosArm64),
            ("macos", "x86_64") => Some(Platform::MacosX64),
            _ => None,
        }
    }

    /// Las plataformas cuyos archivos sirven en esta, de la mejor a la peor (FORMATO.md,
    /// «Plataformas»): Windows de 64 bits corre los de 32; Windows 11 en ARM, también los de
    /// x64; un Mac con Apple Silicon, la compilación universal y luego la de Intel con Rosetta.
    pub fn fallbacks(self) -> &'static [Platform] {
        use Platform::*;
        match self {
            WindowsX64 => &[WindowsX64, WindowsX86],
            WindowsX86 => &[WindowsX86],
            WindowsArm64 => &[WindowsArm64, WindowsX64, WindowsX86],
            LinuxX64 => &[LinuxX64],
            LinuxArm64 => &[LinuxArm64],
            LinuxFlatpak => &[LinuxFlatpak],
            Macos => &[Macos],
            MacosArm64 => &[MacosArm64, Macos, MacosX64],
            MacosX64 => &[MacosX64, Macos],
        }
    }

    /// Las plataformas del mismo sistema, para lo que depende del sistema y no del procesador
    /// (dónde guarda las partidas). Flatpak va siempre aparte: vive en su sandbox.
    pub fn same_system(self) -> &'static [Platform] {
        use Platform::*;
        match self {
            WindowsX64 | WindowsX86 | WindowsArm64 => &[WindowsX64, WindowsX86, WindowsArm64],
            LinuxX64 | LinuxArm64 => &[LinuxX64, LinuxArm64],
            LinuxFlatpak => &[LinuxFlatpak],
            Macos | MacosArm64 | MacosX64 => &[Macos, MacosArm64, MacosX64],
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

type PerPlatform<T> = BTreeMap<Platform, T>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Recipe {
    /// La ficha de Recompendium: `recomps/zelda-majoras-mask`.
    pub entry: String,
    /// `owner/name` en GitHub.
    pub repo: String,
    pub release: ReleaseRule,
    #[serde(default)]
    pub self_update: Option<SelfUpdate>,
    #[serde(default)]
    pub variants: Vec<Variant>,
    pub copy: CopyRule,
    #[serde(default)]
    pub space: BTreeMap<String, String>,
    /// Dónde guarda partidas y ajustes: una ruta o una lista, con variables (`data::DataPath`).
    #[serde(default)]
    pub data_dir: PerPlatform<OneOrMany>,
    /// Cómo se hace portable (que guarde todo en su carpeta), si el proyecto lo permite.
    #[serde(default)]
    pub portable: PerPlatform<PortableRule>,
    /// Sus mods: de dónde salen, dónde los busca el juego y cómo sabe cuáles están activos.
    #[serde(default)]
    pub mods: Option<ModsRule>,
    pub checked: Checked,
    #[serde(default)]
    pub pending: Vec<String>,
}

impl OneOrMany {
    pub fn items(&self) -> &[String] {
        match self {
            OneOrMany::One(s) => std::slice::from_ref(s),
            OneOrMany::Many(v) => v,
        }
    }
}

/// Los mods de un juego (FORMATO.md, `mods`). Unricopie los baja a su carpeta de staging y
/// pone en `dir` los del perfil activo (`mods::game`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ModsRule {
    /// La comunidad de Thunderstore (`zelda-64-recompiled`).
    #[serde(default)]
    pub thunderstore: Option<String>,
    /// Dónde busca el juego los mods: `{data}/mods`, `{game}/mods`.
    pub dir: String,
    /// Si el juego guarda en un archivo suyo cuáles están activos y en qué orden, cuál es: así
    /// Unricopie se lo escribe según el perfil.
    #[serde(default)]
    pub loader: Option<Loader>,
    /// Solo estas extensiones del paquete (en minúsculas, con punto). Vacío: todas.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Nombres de archivo que nunca se ponen.
    #[serde(default)]
    pub skip_names: Vec<String>,
    /// Todo va a `dir` sin las carpetas del paquete.
    #[serde(default)]
    pub flatten: bool,
    /// Carpeta del paquete que se quita del principio de cada ruta, si la trae.
    #[serde(default)]
    pub strip_prefix: Option<String>,
    /// Extensiones de las que, si un paquete trae más de un archivo, el jugador elige cuáles
    /// usar (los cuatro `.rtz` de Modern Controller Overhaul, uno por mando).
    #[serde(default)]
    pub choose: Vec<String>,
}

/// El archivo donde el juego guarda qué mods están activos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Loader {
    /// N64ModernRuntime (los recomps de N64Recomp): `mods.json` en su carpeta de datos, con
    /// `enabled_mods` y `mod_order`, y el id de cada mod en el `mod.json` de su `.nrm`.
    N64recomp,
}

impl ModsRule {
    /// Cómo pasan los archivos de un paquete a `dir`.
    pub fn mapping(&self) -> crate::mods::Mapping {
        crate::mods::Mapping {
            extensions: self.extensions.iter().map(|e| e.to_lowercase()).collect(),
            skip_names: self.skip_names.clone(),
            flatten: self.flatten,
            strip_prefix: self.strip_prefix.clone(),
        }
    }

    /// ¿Hay que preguntar por este archivo, si el paquete trae más de uno de su tipo?
    pub fn is_choice(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.choose.iter().any(|e| name.ends_with(&e.to_lowercase()))
    }

    pub fn check(&self) -> Result<()> {
        let bad = |why: String| Err(Error::BadRecipe(format!("mods: {why}")));
        if let Some(c) = &self.thunderstore {
            if c.is_empty() || !c.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                return bad(format!("comunidad de Thunderstore no válida «{c}»"));
            }
        }
        let dir_ok = self.dir == "{data}" || self.dir == "{game}" || super::settings::path_ok(&self.dir);
        if !dir_ok {
            return bad(format!("carpeta no válida «{}»", self.dir));
        }
        for e in self.extensions.iter().chain(&self.choose) {
            if !e.starts_with('.') || e.len() < 2 || e[1..].contains(['.', '/', '\\']) {
                return bad(format!("extensión no válida «{e}»"));
            }
        }
        if let Some(prefix) = &self.strip_prefix {
            if crate::path::RelPath::new(prefix).is_err() {
                return bad(format!("strip-prefix no válido «{prefix}»"));
            }
        }
        Ok(())
    }
}

/// Lo que hace que un juego guarde todo en su carpeta en una plataforma (FORMATO.md,
/// `portable`). Lo aplica `install::portable`: los archivos y carpetas se crean una vez en la
/// carpeta del juego; los argumentos y las variables se ponen cada vez que se abre.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PortableRule {
    /// Archivos que se crean, relativos a la carpeta del juego, con lo que llevan dentro (`""`,
    /// vacío, para un marcador como `portable.txt`).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Carpetas que se crean (OpenRA usa `Support` si existe).
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Argumentos que se añaden al abrirlo; `{game}` es la carpeta del juego.
    #[serde(default)]
    pub args: Vec<String>,
    /// Variables de entorno que se ponen al abrirlo; `{game}` es la carpeta del juego.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Dónde guarda ya portable. Si falta, en `{game}`.
    #[serde(default)]
    pub data_dir: Option<OneOrMany>,
}

impl PortableRule {
    /// Dónde guarda ya portable. Las rutas que este núcleo no entiende no están.
    pub fn data_dirs(&self) -> Vec<DataPath> {
        match &self.data_dir {
            Some(v) => v.items().iter().filter_map(|s| DataPath::parse(s)).collect(),
            None => DataPath::parse("{game}").into_iter().collect(),
        }
    }
}

/// Si un juego puede guardar todo en su carpeta en una plataforma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Portability<'a> {
    /// Ya lo hace: todo su `data-dir` está en `{game}`.
    Already,
    /// Se puede, con esta regla.
    Rule(&'a PortableRule),
    /// La receta no sabe cómo (o no se puede, como en Flatpak): guarda en su `data-dir`.
    No,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    #[default]
    Github,
    Url,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseRule {
    #[serde(default)]
    pub source: SourceKind,
    /// Con `source: url`: plantilla con `{version}`.
    #[serde(default)]
    pub url: PerPlatform<String>,
    #[serde(default)]
    pub tag_prefix: Option<String>,
    /// La etiqueta se mueve (`latest`, `ci-dev-build`): la versión es el archivo, no la etiqueta.
    #[serde(default)]
    pub rolling: bool,
    /// Patrón del nombre del archivo, con `*`.
    #[serde(default)]
    pub assets: PerPlatform<String>,
    /// Patrones de archivos que nunca se eligen (`dhewm3-mods-*`), para todas las plataformas.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default, rename = "type")]
    pub kind: PerPlatform<PackageKind>,
    #[serde(default)]
    pub run: PerPlatform<String>,
    #[serde(default)]
    pub after_download: PerPlatform<Vec<Step>>,
    #[serde(default)]
    pub after_extract: PerPlatform<Vec<Step>>,
    #[serde(default)]
    pub installer_args: PerPlatform<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    /// zip, tar.gz…: se descomprime.
    Archive,
    /// msi o exe de instalación.
    Installer,
    /// Un ejecutable suelto.
    Portable,
    Appimage,
    Dmg,
    Deb,
    Rpm,
    Flatpak,
    Snap,
}

impl PackageKind {
    /// FORMATO.md: si la receta no dice `type`, se deduce de la extensión.
    pub fn from_name(name: &str) -> Option<PackageKind> {
        let n = name.to_ascii_lowercase();
        let ends = |exts: &[&str]| exts.iter().any(|e| n.ends_with(e));
        Some(if ends(&[".zip", ".7z", ".rar", ".tar.gz", ".tgz", ".tar.xz", ".tar.bz2"]) {
            PackageKind::Archive
        } else if ends(&[".msi"]) {
            PackageKind::Installer
        } else if ends(&[".exe"]) {
            PackageKind::Portable
        } else if ends(&[".appimage"]) {
            PackageKind::Appimage
        } else if ends(&[".dmg"]) {
            PackageKind::Dmg
        } else if ends(&[".deb"]) {
            PackageKind::Deb
        } else if ends(&[".rpm"]) {
            PackageKind::Rpm
        } else if ends(&[".flatpak"]) {
            PackageKind::Flatpak
        } else if ends(&[".snap"]) {
            PackageKind::Snap
        } else {
            return None;
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// Dar permiso de ejecución.
    ChmodX,
}

/// El actualizador propio del proyecto (FORMATO.md, `self-update`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfUpdate {
    /// Pregunta a la persona antes de bajar nada.
    pub asks: bool,
    /// Baja la versión nueva y cambia sus propios archivos. `false` si solo avisa (o abre la
    /// web): no rompe nada, pero se apaga igual para que no confunda.
    #[serde(default = "yes")]
    pub downloads: bool,
    /// Cómo se apaga: una o varias formas, cada una para sus plataformas.
    #[serde(default, deserialize_with = "one_or_list")]
    pub disable: Vec<SelfUpdateOff>,
}

/// Una forma de apagar el actualizador: una clave en un archivo de configuración del juego
/// (`file`, `key`, `value`, y `section` si va dentro de una), o argumentos y variables al abrirlo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SelfUpdateOff {
    /// Dónde vale. Vacío, en todas.
    #[serde(default)]
    pub platforms: Vec<Platform>,
    /// Solo con el juego en modo normal o solo en modo portable (UZDoom guarda sus ajustes en
    /// otro archivo cuando es portable). Si falta, en los dos.
    #[serde(default)]
    pub mode: Option<Mode>,
    /// `{game}/config.toml`, `{data}/general.json`… (`{data}` es la carpeta de datos del juego:
    /// la de `data-dir`, o la de `portable` si está en modo portable).
    #[serde(default)]
    pub file: Option<String>,
    /// Si falta, por la extensión.
    #[serde(default)]
    pub format: Option<ConfigFormat>,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl SelfUpdateOff {
    /// ¿Vale en `platform`, con el juego portable o no?
    pub fn applies_to(&self, platform: Platform, portable: bool) -> bool {
        let mode = match self.mode {
            None => true,
            Some(Mode::Normal) => !portable,
            Some(Mode::Portable) => portable,
        };
        mode && (self.platforms.is_empty() || self.platforms.contains(&platform))
    }
}

/// El modo en que está el juego: el de siempre o portable (`install::portable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Normal,
    Portable,
}

/// Los formatos de configuración que Unricopie sabe tocar (`install::settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Toml,
    Ini,
}

impl ConfigFormat {
    pub fn from_name(name: &str) -> Option<ConfigFormat> {
        let n = name.to_ascii_lowercase();
        Some(if n.ends_with(".json") {
            ConfigFormat::Json
        } else if n.ends_with(".toml") {
            ConfigFormat::Toml
        } else if n.ends_with(".ini") {
            ConfigFormat::Ini
        } else {
            return None;
        })
    }
}

/// Un valor o una lista, para los campos que admiten las dos formas.
fn one_or_list<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrList<T> {
        One(T),
        List(Vec<T>),
    }
    Ok(match OneOrList::deserialize(d)? {
        OneOrList::One(x) => vec![x],
        OneOrList::List(v) => v,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    /// Id del registro de juegos de Recompendium.
    pub game: String,
    pub name: String,
    #[serde(default)]
    pub release: Option<VariantRelease>,
    #[serde(default)]
    pub copy: Option<CopyRule>,
}

/// Lo que una variante puede cambiar de `release`: cada campo que trae sustituye entero al de
/// arriba; los demás se heredan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VariantRelease {
    #[serde(default)]
    pub assets: Option<PerPlatform<String>>,
    #[serde(default, rename = "type")]
    pub kind: Option<PerPlatform<PackageKind>>,
    #[serde(default)]
    pub run: Option<PerPlatform<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CopyRule {
    pub how: CopyHow,
    #[serde(default)]
    pub first_run: Option<FirstRun>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub argument: Option<String>,
    #[serde(default)]
    pub accepts: Vec<Accepted>,
    #[serde(default)]
    pub accepts_from: Option<AcceptsFrom>,
    /// Qué da la persona: un archivo (una ROM, una ISO) o la carpeta del juego original. Si
    /// falta, lo que elija.
    #[serde(default)]
    pub kind: Option<CopyKind>,
    /// Cómo se pasa la copia a la forma que espera el juego antes de comprobarla y guardarla.
    #[serde(default)]
    pub convert: Option<Convert>,
    /// Cómo le llega al juego la copia que Unricopie guardó en la biblioteca (`install::place`).
    #[serde(default)]
    pub place: Vec<Place>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Convert {
    /// Una ROM de N64 al orden de bytes .z64, como hace N64ModernRuntime: se rellena hasta un
    /// múltiplo de 4 y se dan la vuelta los bytes si viene como .v64 o .n64.
    N64Z64,
}

/// Una forma de dar al juego la copia guardada. Con `{copy}`, la ruta de la copia en la
/// biblioteca (el archivo, o la carpeta). Se hacen todas las que valen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Place {
    #[serde(default)]
    pub platforms: Vec<Platform>,
    #[serde(default)]
    pub mode: Option<Mode>,
    /// Solo si falta este archivo (lo que el juego genera con la copia, como `{data}/oot.o2r`).
    #[serde(default)]
    pub when_missing: Option<String>,
    /// Dejar el archivo de la copia en esta ruta (`{data}/mm.n64.us.1.0.z64`).
    #[serde(default)]
    pub store: Option<String>,
    /// Copiar lo que hay en la carpeta de la copia a esta carpeta, sin pisar nada.
    #[serde(default)]
    pub files: Option<String>,
    /// Con `files`: copiar solo esta subcarpeta de la copia (`data`, `base`), sin distinguir
    /// mayúsculas. Lo grande se sigue leyendo de la copia y aquí queda solo lo que el juego (o un
    /// mod) necesita tener en su carpeta.
    #[serde(default)]
    pub from: Option<String>,
    /// Con `files`: enlazar los archivos en vez de copiarlos (enlace duro, en el mismo disco)
    /// y dejarlos de solo lectura, lo que protege también la copia de la biblioteca. Para lo
    /// que el juego solo lee, como los `.pk4` de Doom 3. Si no se puede enlazar, se copia.
    #[serde(default)]
    pub link: bool,
    /// Argumentos al abrirlo (`["--rom-file", "{copy}"]`).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Escribir la ruta en su configuración.
    #[serde(default)]
    pub config: Option<PlaceConfig>,
}

impl Place {
    pub fn applies_to(&self, platform: Platform, portable: bool) -> bool {
        let off = SelfUpdateOff { platforms: self.platforms.clone(), mode: self.mode, ..Default::default() };
        off.applies_to(platform, portable)
    }
}

/// La ruta de la copia en una clave de la configuración del juego. Manda lo del jugador: si ya
/// tiene otra ruta, no se toca.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceConfig {
    pub file: String,
    #[serde(default)]
    pub format: Option<ConfigFormat>,
    #[serde(default)]
    pub section: Option<String>,
    pub key: String,
    /// Con `{copy}`.
    pub value: String,
    /// El juego acepta el archivo con solo esa clave: si no existe, se crea.
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyHow {
    /// La propia app pide la copia; Unricopie la verifica antes.
    InApp,
    /// El proyecto trae su asistente.
    OwnInstaller,
    /// La copia va en una carpeta concreta (`folder`).
    Folder,
    /// Se pasa la ruta de la copia al ejecutable (`argument`).
    Argument,
    /// No hace falta copia.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirstRun {
    ExtractAssets,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub name: String,
    #[serde(default)]
    pub what: What,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub region: Option<OneOrMany>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub hash: BTreeMap<HashAlgo, String>,
    #[serde(default = "yes")]
    pub required: bool,
    #[serde(default)]
    pub note: Option<String>,
    /// En una copia que es una carpeta, de qué archivo es: `base/pak000.pk4`.
    #[serde(default)]
    pub file: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum What {
    #[default]
    Game,
    TitleUpdate,
    Dlc,
}

/// Un valor o una lista (una región o varias; una ruta o varias).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// Los hashes que admite FORMATO.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HashAlgo {
    #[serde(rename = "sha1")]
    Sha1,
    #[serde(rename = "sha256")]
    Sha256,
    #[serde(rename = "md5")]
    Md5,
    #[serde(rename = "crc32")]
    Crc32,
    #[serde(rename = "xxh3_64")]
    Xxh3_64,
    #[serde(rename = "xxh3_128")]
    Xxh3_128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptsFrom {
    pub url: String,
    /// Texto libre en la receta («lista de {name, sha1}»).
    pub format: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    pub date: String,
    pub release: String,
    pub sources: Vec<String>,
}

/// De dónde sale el archivo a bajar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRule {
    /// Un archivo de la release de GitHub cuyo nombre encaja con el patrón.
    Pattern(String),
    /// Una URL fuera de GitHub, con `{version}`.
    Url(String),
}

/// Lo que toca de una receta para una plataforma (y una variante, si la hay).
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<'a> {
    pub recipe: &'a Recipe,
    pub variant: Option<&'a Variant>,
    /// La plataforma cuyos archivos se usan (puede ser un sustituto de la del equipo).
    pub platform: Platform,
    pub asset: AssetRule,
    /// Archivos que no se eligen aunque encajen con el patrón.
    pub exclude: Vec<String>,
    pub kind: PackageKind,
    pub run: Option<String>,
    pub after_download: Vec<Step>,
    pub after_extract: Vec<Step>,
    pub installer_args: Vec<String>,
    pub copy: &'a CopyRule,
    /// Dónde guarda partidas y ajustes. Las rutas que este núcleo no entiende no están.
    pub data_dirs: Vec<DataPath>,
    /// Cómo se hace portable aquí, si se puede.
    pub portable: Option<&'a PortableRule>,
}

impl Recipe {
    pub fn from_json(json: &str) -> Result<Recipe> {
        let r: Recipe = serde_json::from_str(json)?;
        r.check()?;
        Ok(r)
    }

    /// Lo mínimo para no hacer nada raro con la receta: repo y entrada con forma válida.
    pub fn check(&self) -> Result<()> {
        let ok_part = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && s != "." && s != "..";
        match self.repo.split_once('/') {
            Some((o, n)) if ok_part(o) && ok_part(n) => {}
            _ => return Err(Error::BadRecipe(format!("repo no válido: {}", self.repo))),
        }
        match self.entry.split_once('/') {
            Some((kind, id))
                if ["recomps", "ports", "decomps", "tools", "mods"].contains(&kind)
                    && !id.is_empty()
                    && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') => {}
            _ => return Err(Error::BadRecipe(format!("entrada no válida: {}", self.entry))),
        }
        if let Some(m) = &self.mods {
            m.check()?;
        }
        Ok(())
    }

    /// La regla de la copia de `variant`, o la de arriba.
    pub fn copy_rule(&self, variant: Option<&str>) -> &CopyRule {
        variant
            .and_then(|g| self.variants.iter().find(|v| v.game == g))
            .and_then(|v| v.copy.as_ref())
            .unwrap_or(&self.copy)
    }

    /// El id de la ficha, sin el tipo: `zelda-majoras-mask`.
    pub fn slug(&self) -> &str {
        self.entry.split_once('/').map(|(_, id)| id).unwrap_or(&self.entry)
    }

    /// Elige qué bajar y cómo instalarlo en `host`. `variant` es el id de juego de una variante.
    pub fn resolve(&self, variant: Option<&str>, host: Platform) -> Result<Resolved<'_>> {
        let var = match variant {
            None => None,
            Some(g) => Some(
                self.variants
                    .iter()
                    .find(|v| v.game == g)
                    .ok_or_else(|| Error::BadRecipe(format!("{} no tiene la variante {g}", self.entry)))?,
            ),
        };
        let vr = var.and_then(|v| v.release.as_ref());
        let assets = vr.and_then(|r| r.assets.as_ref()).unwrap_or(&self.release.assets);
        let kinds = vr.and_then(|r| r.kind.as_ref()).unwrap_or(&self.release.kind);
        let runs = vr.and_then(|r| r.run.as_ref()).unwrap_or(&self.release.run);
        let sources = match self.release.source {
            SourceKind::Github => assets,
            SourceKind::Url => &self.release.url,
        };
        let platform = *host
            .fallbacks()
            .iter()
            .find(|p| sources.contains_key(p))
            .ok_or_else(|| Error::NoPlatform { entry: self.entry.clone(), platform: host.to_string() })?;
        let name = &sources[&platform];
        let asset = match self.release.source {
            SourceKind::Github => AssetRule::Pattern(name.clone()),
            SourceKind::Url => AssetRule::Url(name.clone()),
        };
        let kind = match kinds.get(&platform) {
            Some(k) => *k,
            None => PackageKind::from_name(name)
                .ok_or_else(|| Error::BadRecipe(format!("{}: no se sabe qué es «{name}» y la receta no dice `type`", self.entry)))?,
        };
        Ok(Resolved {
            recipe: self,
            variant: var,
            platform,
            asset,
            exclude: self.release.exclude.clone(),
            kind,
            run: runs.get(&platform).cloned(),
            after_download: self.release.after_download.get(&platform).cloned().unwrap_or_default(),
            after_extract: self.release.after_extract.get(&platform).cloned().unwrap_or_default(),
            installer_args: self.release.installer_args.get(&platform).cloned().unwrap_or_default(),
            copy: var.and_then(|v| v.copy.as_ref()).unwrap_or(&self.copy),
            data_dirs: self.data_dirs(platform),
            portable: self.portable_rule(platform),
        })
    }

    /// Las rutas de `data-dir` de `platform`, o de otra del mismo sistema si falta la suya.
    pub fn data_dirs(&self, platform: Platform) -> Vec<DataPath> {
        let found = self
            .data_dir
            .get(&platform)
            .or_else(|| platform.same_system().iter().find_map(|p| self.data_dir.get(p)));
        found.map(|v| v.items().iter().filter_map(|s| DataPath::parse(s)).collect()).unwrap_or_default()
    }

    /// La regla `portable` de `platform`, o de otra del mismo sistema si falta la suya. Nunca
    /// en Flatpak: la app solo ve su sandbox, no su carpeta.
    pub fn portable_rule(&self, platform: Platform) -> Option<&PortableRule> {
        if platform == Platform::LinuxFlatpak {
            return None;
        }
        self.portable.get(&platform).or_else(|| platform.same_system().iter().find_map(|p| self.portable.get(p)))
    }

    /// Si puede guardar todo en su carpeta en `platform`. Primero la regla de la receta; sin
    /// ella, ya es portable si todo lo que dice `data-dir` está en `{game}`.
    pub fn portability(&self, platform: Platform) -> Portability<'_> {
        use super::data::Base;
        if let Some(r) = self.portable_rule(platform) {
            return Portability::Rule(r);
        }
        let dirs = self.data_dirs(platform);
        if platform != Platform::LinuxFlatpak && !dirs.is_empty() && dirs.iter().all(|d| d.base == Base::Game) {
            Portability::Already
        } else {
            Portability::No
        }
    }
}

/// `*` encaja con cualquier texto (también vacío); lo demás, tal cual. Es lo único que usan
/// los patrones de las recetas.
pub fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const ZELDA: &str = r#"{
        "entry": "recomps/zelda-majoras-mask",
        "repo": "Zelda64Recomp/Zelda64Recomp",
        "release": {
            "assets": {
                "windows-x64": "Zelda64Recompiled-*-Windows.zip",
                "linux-x64": "Zelda64Recompiled-*-Linux-X64.zip",
                "macos": "Zelda64Recompiled-*-macOS.zip"
            },
            "run": { "windows-x64": "Zelda64Recompiled.exe", "linux-x64": "Zelda64Recompiled" },
            "after-extract": { "linux-x64": ["chmod-x"] }
        },
        "copy": {
            "how": "in-app",
            "accepts": [{ "name": "Majora's Mask (N64, EE. UU.)", "platform": "n64", "region": "us",
                          "hash": { "xxh3_64": "ef18b4a9e2386169" } }]
        },
        "checked": { "date": "2026-10-30", "release": "v1.2.2", "sources": ["prueba"] }
    }"#;

    #[test]
    fn lee_una_receta() {
        let r = Recipe::from_json(ZELDA).unwrap();
        assert_eq!(r.slug(), "zelda-majoras-mask");
        assert_eq!(r.copy.how, CopyHow::InApp);
        let a = &r.copy.accepts[0];
        assert!(a.required);
        assert_eq!(a.what, What::Game);
        assert_eq!(a.hash[&HashAlgo::Xxh3_64], "ef18b4a9e2386169");
        assert_eq!(a.region, Some(OneOrMany::One("us".into())));
    }

    #[test]
    fn elige_la_plataforma_y_sus_sustitutos() {
        let r = Recipe::from_json(ZELDA).unwrap();
        let w = r.resolve(None, Platform::WindowsX64).unwrap();
        assert_eq!(w.asset, AssetRule::Pattern("Zelda64Recompiled-*-Windows.zip".into()));
        assert_eq!(w.kind, PackageKind::Archive);
        assert_eq!(w.run.as_deref(), Some("Zelda64Recompiled.exe"));
        // Windows en ARM usa la de x64.
        assert_eq!(r.resolve(None, Platform::WindowsArm64).unwrap().platform, Platform::WindowsX64);
        // Un Mac con Apple Silicon, la universal.
        assert_eq!(r.resolve(None, Platform::MacosArm64).unwrap().platform, Platform::Macos);
        let l = r.resolve(None, Platform::LinuxX64).unwrap();
        assert_eq!(l.after_extract, vec![Step::ChmodX]);
        assert!(matches!(r.resolve(None, Platform::LinuxArm64), Err(Error::NoPlatform { .. })));
        // Flatpak nunca sustituye a nada, ni nada a Flatpak.
        assert!(r.resolve(None, Platform::LinuxFlatpak).is_err());
    }

    #[test]
    fn las_variantes_sustituyen_campos_enteros() {
        let json = r#"{
            "entry": "ports/openra", "repo": "OpenRA/OpenRA",
            "release": {
                "assets": { "windows-x64": "OpenRA-*-x64.exe", "linux-x64": "OpenRA-Red-Alert-x86_64.AppImage" },
                "type": { "windows-x64": "installer" },
                "run": { "windows-x64": "RedAlert.exe" }
            },
            "variants": [
                { "game": "command-and-conquer", "name": "Tiberian Dawn",
                  "release": { "assets": { "linux-x64": "OpenRA-Tiberian-Dawn-x86_64.AppImage" } } }
            ],
            "copy": { "how": "none" },
            "checked": { "date": "2026-10-31", "release": "x", "sources": ["prueba"] }
        }"#;
        let r = Recipe::from_json(json).unwrap();
        let td = r.resolve(Some("command-and-conquer"), Platform::LinuxX64).unwrap();
        assert_eq!(td.asset, AssetRule::Pattern("OpenRA-Tiberian-Dawn-x86_64.AppImage".into()));
        assert_eq!(td.kind, PackageKind::Appimage);
        // `assets` de la variante sustituye entero al de arriba: ya no hay Windows.
        assert!(r.resolve(Some("command-and-conquer"), Platform::WindowsX64).is_err());
        // Sin variante, la de arriba; `run` y `type` se heredan.
        let ra = r.resolve(None, Platform::WindowsX64).unwrap();
        assert_eq!(ra.kind, PackageKind::Installer);
        assert!(r.resolve(Some("no-existe"), Platform::LinuxX64).is_err());
    }

    #[test]
    fn rechaza_repos_y_entradas_raros() {
        for (repo, entry) in [("../x", "recomps/a"), ("a/b/c", "recomps/a"), ("a", "recomps/a"), ("a/b", "otra/a"), ("a/b", "recomps/A B")] {
            let json = ZELDA.replace("Zelda64Recomp/Zelda64Recomp", repo).replace("recomps/zelda-majoras-mask", entry);
            assert!(Recipe::from_json(&json).is_err(), "{repo} {entry}");
        }
    }

    #[test]
    fn patrones() {
        assert!(matches("Zelda64Recompiled-*-Windows.zip", "Zelda64Recompiled-v1.2.2-Windows.zip"));
        assert!(!matches("Zelda64Recompiled-*-Windows.zip", "Zelda64Recompiled-v1.2.2-Windows.zip.sig"));
        assert!(matches("SoH-*.appimage", "SoH-Dewey-Bravo.appimage"));
        assert!(matches("a*b*c", "abc"));
        assert!(matches("a*b*c", "a-x-b-y-c"));
        assert!(!matches("a*b*c", "a-x-c"));
        assert!(!matches("ab*ba", "aba")); // el inicio y el final no pueden solaparse
        assert!(matches("exacto.zip", "exacto.zip"));
        assert!(!matches("exacto.zip", "Exacto.zip"));
    }

    #[test]
    fn carpetas_de_datos_por_sistema() {
        use crate::install::data::Base;
        let json = ZELDA.replace(
            r#""copy":"#,
            r#""data-dir": {
                "windows-x64": "{localappdata}/Zelda64Recompiled",
                "linux-x64": ["{xdg-config}/Zelda64Recompiled", "{nuevo}/algo", "{game}/user"]
            },
            "copy":"#,
        );
        let r = Recipe::from_json(&json).unwrap();
        let bases = |p: Platform| r.data_dirs(p).iter().map(|d| d.base).collect::<Vec<_>>();
        assert_eq!(bases(Platform::WindowsX64), [Base::LocalAppData]);
        // Lo que depende del sistema y no del procesador sirve en todo Windows y todo Linux...
        assert_eq!(bases(Platform::WindowsArm64), [Base::LocalAppData]);
        // ...y la variable que este núcleo no conoce se ignora sin romper la receta.
        assert_eq!(bases(Platform::LinuxArm64), [Base::XdgConfig, Base::Game]);
        // Flatpak va aparte.
        assert!(r.data_dirs(Platform::LinuxFlatpak).is_empty());
        assert_eq!(r.resolve(None, Platform::WindowsX64).unwrap().data_dirs.len(), 1);
    }

    #[test]
    fn portable_por_sistema() {
        use crate::install::data::Base;
        let json = ZELDA.replace(
            r#""copy":"#,
            r#""data-dir": {
                "windows-x64": "{game}",
                "linux-x64": "{xdg-config}/Zelda64Recompiled",
                "linux-flatpak": "{home}/.var/app/x/config",
                "macos": "{home}/Library/Application Support/Zelda64Recompiled"
            },
            "portable": {
                "linux-x64": { "files": { "portable.txt": "" } },
                "linux-flatpak": { "files": { "portable.txt": "" } },
                "macos": { "env": { "APP_FOLDER_PATH": "{game}" }, "data-dir": ["{game}", "{home}/Library/Logs/Z"] }
            },
            "copy":"#,
        );
        let r = Recipe::from_json(&json).unwrap();
        // Windows ya guarda en su carpeta; Linux necesita el marcador, también en ARM.
        assert_eq!(r.portability(Platform::WindowsX64), Portability::Already);
        assert_eq!(r.portability(Platform::WindowsArm64), Portability::Already);
        let Portability::Rule(l) = r.portability(Platform::LinuxArm64) else { panic!() };
        assert_eq!(l.files.keys().collect::<Vec<_>>(), ["portable.txt"]);
        assert_eq!(l.data_dirs().iter().map(|d| d.base).collect::<Vec<_>>(), [Base::Game]);
        let Portability::Rule(m) = r.portability(Platform::MacosArm64) else { panic!() };
        assert_eq!(m.data_dirs().iter().map(|d| d.base).collect::<Vec<_>>(), [Base::Game, Base::Home]);
        // Flatpak nunca, aunque la receta lo diga.
        assert_eq!(r.portability(Platform::LinuxFlatpak), Portability::No);
        assert!(r.resolve(None, Platform::LinuxFlatpak).is_err());
        assert!(r.resolve(None, Platform::LinuxX64).unwrap().portable.is_some());
        // Sin data-dir ni regla, no se sabe.
        assert_eq!(Recipe::from_json(ZELDA).unwrap().portability(Platform::WindowsX64), Portability::No);
    }

    /// Las recetas son privadas y no se copian a este repo: la prueba las lee de un JSON con la
    /// lista, que se genera desde unricopie-data.
    #[test]
    #[ignore = "necesita UNRICOPIE_RECETAS=<json con todas las recetas>"]
    fn lee_todas_las_recetas_de_verdad() {
        let path = std::env::var("UNRICOPIE_RECETAS").expect("UNRICOPIE_RECETAS");
        let all: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let (mut instalables, mut rutas, mut portables, mut actualizadores, mut lugares) = (0, 0, 0, 0, 0);
        for v in &all {
            let r: Recipe = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{}: {e}", v["entry"]));
            r.check().unwrap();
            let games: Vec<Option<&str>> = std::iter::once(None).chain(r.variants.iter().map(|x| Some(x.game.as_str()))).collect();
            for g in games {
                for p in [Platform::WindowsX64, Platform::LinuxX64, Platform::MacosArm64] {
                    match r.resolve(g, p) {
                        Ok(res) => {
                            if p == Platform::WindowsX64 && g.is_none() {
                                let name = match &res.asset { AssetRule::Pattern(s) | AssetRule::Url(s) => s.clone() };
                                let ok = match res.kind {
                                    PackageKind::Archive => crate::archive::Kind::from_name(&name).is_some(),
                                    PackageKind::Portable | PackageKind::Appimage => true,
                                    _ => false,
                                };
                                if ok { instalables += 1 } else { eprintln!("todavía no: {} ({:?}, {name})", r.entry, res.kind) }
                            }
                        }
                        Err(Error::NoPlatform { .. }) => {}
                        Err(e) => panic!("{} {g:?} {p}: {e}", r.entry),
                    }
                }
            }
            // Cada ruta de data-dir la entiende este núcleo: ninguna se ignora en silencio.
            for (p, v) in &r.data_dir {
                assert_eq!(r.data_dirs(*p).len(), v.items().len(), "{} {p}: {:?}", r.entry, v.items());
                rutas += v.items().len();
            }
            // Cada forma de apagar el actualizador vale.
            for off in r.self_update.iter().flat_map(|s| &s.disable) {
                crate::install::settings::check_off(off).unwrap_or_else(|e| panic!("{}: {e}", r.entry));
                actualizadores += 1;
            }
            // Cada forma de dar la copia al juego vale.
            for rule in std::iter::once(&r.copy).chain(r.variants.iter().filter_map(|v| v.copy.as_ref())) {
                for p in &rule.place {
                    crate::install::place::check_place(p).unwrap_or_else(|e| panic!("{}: {e}", r.entry));
                    lugares += 1;
                }
            }
            // Y cada regla portable se puede aplicar.
            for (p, rule) in &r.portable {
                crate::install::portable::check_rule(rule).unwrap_or_else(|e| panic!("{} {p}: {e}", r.entry));
                let n = rule.data_dir.as_ref().map_or(1, |v| v.items().len());
                assert_eq!(rule.data_dirs().len(), n, "{} {p}", r.entry);
                portables += 1;
            }
        }
        eprintln!(
            "{} recetas; en Windows x64 se pueden instalar {instalables}; {rutas} rutas de data-dir; {portables} reglas portables; {actualizadores} formas de apagar el actualizador; {lugares} formas de dar la copia",
            all.len()
        );
    }

    #[test]
    fn tipo_por_extension() {
        assert_eq!(PackageKind::from_name("x.tar.xz"), Some(PackageKind::Archive));
        assert_eq!(PackageKind::from_name("Setup.MSI"), Some(PackageKind::Installer));
        assert_eq!(PackageKind::from_name("juego.exe"), Some(PackageKind::Portable));
        assert_eq!(PackageKind::from_name("P.T.PC.Port.Setup-linux"), None);
    }
}
