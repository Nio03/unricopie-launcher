//! La red: un solo cliente HTTP para todo el núcleo y una descarga que nunca deja un archivo a
//! medias en su sitio. Se baja a un `.part`, se cuentan los bytes de verdad (un servidor puede
//! mentir sobre el tamaño), se calcula el SHA-256 por el camino y, si la fuente publica una
//! suma, se compara antes de dar la descarga por buena.
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use md5::Md5;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Así se presenta Unricopie ante los sitios (Nexus exige un nombre y versión constantes).
pub const APP_NAME: &str = "Unricopie";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// El cliente HTTP con el que se habla con todos los sitios.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(&format!("{APP_NAME}/{APP_VERSION} (+https://recompendium.com)"))
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build()
}

pub(crate) fn get_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let resp = agent.get(url).call().map_err(|e| net(url, e))?;
    resp.into_json::<T>().map_err(|e| Error::Network { url: url.into(), reason: e.to_string() })
}

pub(crate) fn net(url: &str, e: ureq::Error) -> Error {
    Error::Network { url: url.into(), reason: e.to_string() }
}

/// La suma que publica la fuente de un archivo, en hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    Md5(String),
    Sha256(String),
}

/// Lo que se bajó.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub size: u64,
    /// SHA-256 calculado mientras se bajaba, siempre: va al recibo de instalación.
    pub sha256: String,
}

/// Baja `url` a `dest`, sin pasar de `max_bytes`, y comprueba `expected` si hay. `progress`
/// recibe los bytes bajados y el total si el servidor lo dice.
pub fn download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    max_bytes: u64,
    expected: Option<&Expected>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Downloaded> {
    let resp = agent.get(url).call().map_err(|e| net(url, e))?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|v| v.parse().ok());
    if total.is_some_and(|t| t > max_bytes) {
        return Err(Error::TooLarge { limit: max_bytes });
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let tmp = dest.with_extension("part");
    let result = (|| {
        let mut out = File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        let mut reader = resp.into_reader();
        let mut sha = Sha256::new();
        let mut md5 = matches!(expected, Some(Expected::Md5(_))).then(Md5::new);
        let mut buf = vec![0u8; 1 << 16];
        let mut size = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| Error::io(&tmp, e))?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > max_bytes {
                return Err(Error::TooLarge { limit: max_bytes });
            }
            sha.update(&buf[..n]);
            if let Some(m) = md5.as_mut() {
                m.update(&buf[..n]);
            }
            out.write_all(&buf[..n]).map_err(|e| Error::io(&tmp, e))?;
            progress(size, total);
        }
        out.flush().map_err(|e| Error::io(&tmp, e))?;
        let sha256 = hex(&sha.finalize());
        match expected {
            Some(Expected::Sha256(want)) if !want.eq_ignore_ascii_case(&sha256) => {
                return Err(Error::ChecksumMismatch { algo: "SHA-256", expected: want.clone(), got: sha256 });
            }
            Some(Expected::Md5(want)) => {
                let got = hex(&md5.expect("se crea cuando se espera un MD5").finalize());
                if !want.eq_ignore_ascii_case(&got) {
                    return Err(Error::ChecksumMismatch { algo: "MD5", expected: want.clone(), got });
                }
            }
            _ => {}
        }
        Ok(Downloaded { size, sha256 })
    })();
    match result {
        Ok(d) => {
            fs::rename(&tmp, dest).map_err(|e| Error::io(dest, e))?;
            Ok(d)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
