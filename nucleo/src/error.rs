//! El error del núcleo. Cada variante dice qué pasó y dónde, con datos (rutas, nombres,
//! versiones) y no con frases: el texto de abajo es para el banco de pruebas y los registros, y
//! la interfaz escribe cada variante en el idioma de la persona. Como texto libre solo queda lo
//! que viene del sistema o de otra librería (io, red, zip, json, toml).
use std::path::PathBuf;

use crate::install::release::Channel;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no se pudo usar {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no se pudo descomprimir {path}: {reason}")]
    Archive { path: PathBuf, reason: String },
    #[error("ruta no permitida dentro del archivo: {0}")]
    UnsafePath(String),
    #[error("lo descomprimido supera el límite de {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("el archivo trae más de {limit} archivos")]
    TooManyFiles { limit: usize },
    #[error("la descarga no coincide: se esperaba {algo} {expected} y llegó {got}")]
    ChecksumMismatch { algo: &'static str, expected: String, got: String },
    #[error("enlace no válido: {0}")]
    BadLink(String),
    #[error("error de red en {url}: {reason}")]
    Network { url: String, reason: String },
    #[error("JSON no válido: {0}")]
    Json(#[from] serde_json::Error),

    // Instalación de juegos.
    #[error("la receta no sirve: {0}")]
    BadRecipe(String),
    #[error("{entry} no tiene versión para {platform}")]
    NoPlatform { entry: String, platform: String },
    #[error("{repo} no tiene ninguna versión en el canal {channel}")]
    NoRelease { repo: String, channel: Channel },
    #[error("la versión {tag} no trae ningún archivo que encaje con «{pattern}»")]
    NoAsset { tag: String, pattern: String },
    #[error("la versión {tag} trae varios archivos que encajan con «{pattern}»: {matches:?}")]
    AmbiguousAsset { tag: String, pattern: String, matches: Vec<String> },
    #[error("después de instalar no aparece {run}")]
    RunNotFound { run: String },
    #[error("ya hay algo instalado en {0}")]
    AlreadyInstalled(PathBuf),
    #[error("todavía no se sabe instalar {asset} ({kind})")]
    UnsupportedPackage { asset: String, kind: String },
    #[error("{0} ya trae un unricopie.json")]
    PackageHasReceipt(String),
    #[error("el paquete tiene una forma inesperada: {0}")]
    PackageLayout(String),
    #[error("los dmg solo se abren en macOS: {0}")]
    DmgOnlyMac(PathBuf),
    #[error("este sistema todavía no tiene versión")]
    UnknownSystem,
    #[error("no se encuentra la carpeta de datos del sistema")]
    NoDataDir,

    // Actualizar y volver atrás.
    #[error("no hay nada instalado en {0}")]
    NotInstalled(PathBuf),
    #[error("lo que hay en {0} no lo instaló Unricopie (no tiene recibo)")]
    NoReceipt(PathBuf),
    #[error("en {dir} está instalado {entry}, que es otro juego")]
    OtherGame { dir: PathBuf, entry: String },
    #[error("ya está instalada la versión {0}")]
    UpToDate(String),
    #[error("el juego de {0} está abierto: hay que cerrarlo antes")]
    GameRunning(PathBuf),
    #[error("no hay una versión anterior «{id}» de {dir}")]
    NoPrevious { dir: PathBuf, id: String },
    #[error("{0} no es un juego de la biblioteca")]
    NotInLibrary(PathBuf),

    // Modo portable y abrir juegos.
    #[error("la receta de {entry} no dice cómo hacerlo portable en {platform}")]
    NotPortable { entry: String, platform: String },
    #[error("no se puede crear {0}: ya hay otra cosa con ese nombre")]
    InTheWay(PathBuf),
    /// `reason` es lo que dice el lector de json o toml.
    #[error("no se pudo usar la configuración {path}: {reason}")]
    Config { path: PathBuf, reason: String },
    #[error("la configuración {0} no está en UTF-8")]
    ConfigNotUtf8(PathBuf),
    /// `key` vacía: la configuración entera no es un objeto o una tabla.
    #[error("en la configuración {path}, «{key}» no es una sección")]
    ConfigShape { path: PathBuf, key: String },
    #[error("el valor no cabe en la configuración {0}")]
    ConfigValue(PathBuf),
    /// El texto es lo que dijo el sistema al abrirlo.
    #[error("no se puede abrir {0}: {1}")]
    CannotLaunch(PathBuf, String),
    #[error("la receta no dice qué ejecutable abrir en {0}")]
    NoRunInRecipe(PathBuf),
    #[error("abrir una app de macOS todavía no se hace: {0}")]
    MacAppNotYet(PathBuf),
    #[error("no está {0}")]
    RunMissing(PathBuf),

    // La copia del jugador.
    #[error("tu copia no es ninguna de las que acepta {0}")]
    CopyMismatch(String),
    #[error("a tu copia le falta {0}")]
    CopyIncomplete(String),
    #[error("{0} no es una ROM de N64")]
    NotARom(PathBuf),
    #[error("{0} pide un archivo, no una carpeta")]
    CopyWantsFile(String),
    #[error("{0} pide la carpeta del juego")]
    CopyWantsFolder(String),

    // Mods.
    #[error("el antivirus del sitio marcó {url}")]
    Flagged { url: String },
    #[error("{0} no trae nada que el juego use como mod")]
    NotAMod(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }
}
