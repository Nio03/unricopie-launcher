//! Las opiniones de la gente sobre cómo funciona cada proyecto.
//!
//! - Se leen de `recompendium.com/reportes.json`, que suma el sitio a partir de los issues con
//!   la etiqueta «reporte» (scripts/refresh-reports.mjs del repo del sitio).
//! - «¿Cómo te fue?» abre el formulario de ese issue en GitHub ya relleno con el proyecto, la
//!   versión, el sistema y la tarjeta gráfica. La persona lo lee, añade su comentario y lo envía;
//!   Unricopie no publica nada por su cuenta.
use std::collections::HashMap;
use std::process::Command;

use serde::Deserialize;

pub const URL_REPORTES: &str = "https://recompendium.com/reportes.json";
const FORMULARIO: &str = "https://github.com/Nio03/unricopie/issues/new?template=reporte.yml";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Resumen {
    #[serde(default)]
    pub total: i32,
    #[serde(default)]
    pub bien: i32,
    #[serde(default)]
    pub problemas: i32,
    #[serde(default)]
    pub no: i32,
}

#[derive(Debug, Default, Deserialize)]
struct Archivo {
    #[serde(default)]
    juegos: HashMap<String, Resumen>,
}

/// Lee las opiniones de todos los proyectos. Sin red o sin archivo, ninguna.
pub fn leer() -> HashMap<String, Resumen> {
    let agent = unricopie_nucleo::net::agent();
    match agent.get(URL_REPORTES).call() {
        Ok(r) => r.into_json::<Archivo>().map(|a| a.juegos).unwrap_or_default(),
        Err(e) => {
            eprintln!("sin opiniones: {e}");
            HashMap::new()
        }
    }
}

/// Lo mismo que el desplegable del formulario (reporte.yml del sitio).
fn opcion(resultado: i32) -> &'static str {
    match resultado {
        0 => "Funciona bien / Works well",
        1 => "Funciona con problemas / Works with problems",
        _ => "No arranca / Doesn't start",
    }
}

/// Para una URL: deja letras, dígitos y `-_.~`, y codifica lo demás.
fn cod(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn salida(c: &mut Command) -> String {
    c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// «Windows 11 24H2» y la tarjeta gráfica, preguntándole al propio Windows.
#[cfg(windows)]
fn equipo() -> (String, String) {
    let clave = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let valor = |v: &str| {
        let s = salida(Command::new("reg").args(["query", clave, "/v", v]));
        s.lines().find_map(|l| l.split("REG_SZ").nth(1)).map(|x| x.trim().to_string()).unwrap_or_default()
    };
    // ProductName dice «Windows 10» también en Windows 11; el número de compilación decide.
    let compilacion: u32 = valor("CurrentBuild").parse().unwrap_or(0);
    let nombre = if compilacion >= 22000 { "Windows 11".to_string() } else { valor("ProductName") };
    let os = format!("{nombre} {}", valor("DisplayVersion")).trim().to_string();
    let todas = salida(Command::new("powershell").args(["-NoProfile", "-Command", "(Get-CimInstance Win32_VideoController).Name -join '|'"]));
    // Los adaptadores virtuales (pantallas remotas, el adaptador básico de Microsoft) no dicen
    // nada del equipo: fuera.
    let virtual_ = |n: &str| {
        let n = n.to_lowercase();
        ["virtual", "basic display", "remote", "indirect", "parsec"].iter().any(|v| n.contains(v))
    };
    let gpu = todas.split('|').map(str::trim).filter(|n| !n.is_empty() && !virtual_(n)).collect::<Vec<_>>().join(", ");
    (os, gpu)
}

#[cfg(not(windows))]
fn equipo() -> (String, String) {
    (format!("{} {}", std::env::consts::OS, std::env::consts::ARCH), String::new())
}

/// La URL del formulario, ya rellena.
pub fn url(nombre: &str, entry: &str, version: &str, resultado: i32) -> String {
    let (os, gpu) = equipo();
    let campos = [
        ("title", format!("[reporte] {nombre} {version}")),
        ("entry", entry.to_string()),
        ("version", version.to_string()),
        ("result", opcion(resultado).to_string()),
        ("os", os),
        ("gpu", gpu),
        ("launcher", env!("CARGO_PKG_VERSION").to_string()),
    ];
    let mut u = FORMULARIO.to_string();
    for (k, v) in campos {
        if !v.is_empty() {
            u.push_str(&format!("&{k}={}", cod(&v)));
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_url_lleva_el_formulario_relleno() {
        let u = url("The Legend of Zelda: Majora's Mask", "recomps/zelda-majoras-mask", "v1.2.2", 1);
        println!("{u}");
        assert!(u.starts_with("https://github.com/Nio03/unricopie/issues/new?template=reporte.yml&title="));
        assert!(u.contains("&entry=recomps%2Fzelda-majoras-mask"));
        assert!(u.contains("&version=v1.2.2"));
        assert!(u.contains("&result=Funciona%20con%20problemas%20%2F%20Works%20with%20problems"));
        assert!(!u.contains(' '));
    }
}
