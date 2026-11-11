//! Los errores del núcleo, como datos para la ventana: un código por variante y sus datos
//! (rutas, nombres, versiones). La frase la escribe `Util.error` de app.slint en el idioma
//! elegido; aquí no hay texto en ningún idioma. Lo único libre es lo que dicen el sistema o
//! otras librerías (io, red, zip, json, toml), que llega tal cual.
use std::path::Path;

use unricopie_nucleo::install::Channel;
use unricopie_nucleo::Error;

/// Un error listo para mandarlo al hilo de la ventana (los mismos campos que `ErrorUi`).
#[derive(Debug, Clone, Default)]
pub struct Datos {
    pub codigo: i32,
    pub a: String,
    pub b: String,
    pub c: String,
    pub n: i32,
}

fn d(codigo: i32, a: impl Into<String>, b: impl Into<String>, c: impl Into<String>) -> Datos {
    Datos { codigo, a: a.into(), b: b.into(), c: c.into(), n: 0 }
}

fn p(x: &Path) -> String {
    x.display().to_string()
}

pub fn canal(c: Channel) -> i32 {
    match c {
        Channel::Stable => 0,
        Channel::Untested => 1,
        Channel::Nightly => 2,
    }
}

pub fn de(e: &Error) -> Datos {
    match e {
        Error::Io { path, source } => d(1, p(path), source.to_string(), ""),
        Error::Archive { path, reason } => d(2, p(path), reason.as_str(), ""),
        Error::UnsafePath(s) => d(3, s.as_str(), "", ""),
        Error::TooLarge { limit } => d(4, limit.to_string(), "", ""),
        Error::TooManyFiles { limit } => d(5, limit.to_string(), "", ""),
        Error::ChecksumMismatch { algo, expected, got } => d(6, *algo, expected.as_str(), got.as_str()),
        Error::BadLink(s) => d(7, s.as_str(), "", ""),
        Error::Network { url, reason } => d(8, url.as_str(), reason.as_str(), ""),
        Error::Json(e) => d(9, e.to_string(), "", ""),
        Error::BadRecipe(detalle) => {
            // Es un fallo nuestro: a la persona se le dice eso, y el detalle va al registro.
            eprintln!("receta con un error: {detalle}");
            d(10, "", "", "")
        }
        Error::NoPlatform { platform, .. } => d(11, platform.as_str(), "", ""),
        Error::NoRelease { repo, channel } => Datos { n: canal(*channel), ..d(12, repo.as_str(), "", "") },
        Error::NoAsset { tag, pattern } => d(13, tag.as_str(), pattern.as_str(), ""),
        Error::AmbiguousAsset { tag, pattern, matches } => d(14, tag.as_str(), pattern.as_str(), matches.join(", ")),
        Error::RunNotFound { run } => d(15, run.as_str(), "", ""),
        Error::AlreadyInstalled(x) => d(16, p(x), "", ""),
        Error::UnsupportedPackage { asset, kind } => d(17, asset.as_str(), kind.as_str(), ""),
        Error::PackageHasReceipt(asset) => d(18, asset.as_str(), "", ""),
        Error::PackageLayout(s) => d(19, s.as_str(), "", ""),
        Error::NotInstalled(x) => d(20, p(x), "", ""),
        Error::NoReceipt(x) => d(21, p(x), "", ""),
        Error::OtherGame { dir, entry } => d(22, p(dir), entry.as_str(), ""),
        Error::UpToDate(v) => d(23, v.as_str(), "", ""),
        Error::GameRunning(x) => d(24, p(x), "", ""),
        Error::NoPrevious { id, .. } => d(25, id.as_str(), "", ""),
        Error::NotPortable { platform, .. } => d(26, platform.as_str(), "", ""),
        Error::InTheWay(x) => d(27, p(x), "", ""),
        Error::Config { path, reason } => d(28, p(path), reason.as_str(), ""),
        Error::ConfigNotUtf8(x) => d(29, p(x), "", ""),
        Error::ConfigShape { path, key } if key.is_empty() => d(30, p(path), "", ""),
        Error::ConfigShape { path, key } => d(31, p(path), key.as_str(), ""),
        Error::ConfigValue(x) => d(32, p(x), "", ""),
        Error::CannotLaunch(x, razon) => d(33, p(x), razon.as_str(), ""),
        Error::NoRunInRecipe(_) => d(34, "", "", ""),
        Error::MacAppNotYet(_) => d(35, "", "", ""),
        Error::RunMissing(x) => d(36, p(x), "", ""),
        Error::CopyMismatch(_) => d(37, "", "", ""),
        Error::CopyIncomplete(f) => d(38, f.as_str(), "", ""),
        Error::NotARom(x) => d(39, p(x), "", ""),
        Error::CopyWantsFile(_) => d(40, "", "", ""),
        Error::CopyWantsFolder(_) => d(41, "", "", ""),
        Error::DmgOnlyMac(_) => d(42, "", "", ""),
        Error::NotInLibrary(x) => d(43, p(x), "", ""),
        Error::NoDataDir => d(44, "", "", ""),
        Error::Flagged { url } => d(45, url.as_str(), "", ""),
        Error::UnknownSystem => d(46, "", "", ""),
        Error::NotAMod(x) => d(47, x.as_str(), "", ""),
    }
}
