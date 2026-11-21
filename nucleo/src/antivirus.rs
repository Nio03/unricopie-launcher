//! Analizar lo bajado con el antivirus del equipo, antes de descomprimirlo o instalarlo.
//!
//! En Windows se usa el analizador de línea de órdenes de Microsoft Defender, que viene con el
//! sistema: `MpCmdRun.exe -Scan -ScanType 3 -File <archivo> -DisableRemediation`. Es un análisis
//! a medida de ese archivo (también por dentro, si es un comprimido) que no borra ni pone nada
//! en cuarentena: solo dice qué encontró. Qué hacer lo decide Unricopie (parar la instalación y
//! borrar lo bajado).
//!
//! `MpCmdRun` sale con 0 si no encontró nada y con 2 si encontró algo… o si no pudo analizar
//! (Defender apagado porque hay otro antivirus: `CmdTool: Failed with hr = 0x800106ba`). Por eso
//! no basta el código de salida: `interpret` mira también lo que escribió.
//!
//! Si no se puede analizar (no hay Defender, está apagado, tarda demasiado), la instalación
//! sigue y en el recibo queda apuntado que no se analizó y por qué (`Scan::NotScanned`). En
//! Linux y macOS no hay con qué analizar por ahora.
//!
//! Lo que decide (`interpret`, `decode`, `timeout_for`, `newest`) son funciones sobre datos, para
//! probarlas sin un virus delante; `check` es la que abre el analizador.
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// El nombre del analizador que queda en el recibo.
pub const DEFENDER: &str = "Microsoft Defender";

/// Cómo fue el análisis de una descarga que se instaló. Es lo que guarda el recibo: si se
/// encontró algo no hay recibo, porque no se instala (`Error::Threat`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Scan {
    /// Se analizó con `scanner` y no encontró nada.
    Clean { scanner: String },
    /// No se analizó.
    NotScanned { reason: Reason },
}

/// Por qué no se analizó.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// En este sistema no hay analizador: Linux, macOS, o un Windows sin `MpCmdRun.exe`.
    NoScanner,
    /// Defender está, pero apagado: lo normal cuando hay otro antivirus instalado.
    Disabled,
    /// Tardó más de lo que se le da (`timeout_for`).
    Timeout,
    /// El analizador no arrancó, o salió con algo que no se entiende.
    Failed,
    /// Un motivo que esta versión no conoce (un recibo escrito por una más nueva).
    #[serde(other)]
    Unknown,
}

/// Lo que dijo el analizador de un archivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    /// Los nombres de lo que encontró (`Virus:DOS/EICAR_Test_File`), separados por comas; vacío
    /// si encontró algo y no se pudo leer el nombre.
    Threat(String),
    NotScanned(Reason),
}

/// Lee lo que hizo `MpCmdRun`: su código de salida (`None` si lo mató una señal) y lo que
/// escribió.
///
/// - Nombres de amenazas (`Threat  : …`) o «found N threats» con N mayor que cero: encontró
///   algo, salga con el código que salga.
/// - Código 0: limpio.
/// - Código 2 con un fallo (`hr = 0x…`): no analizó. Con los códigos de «el servicio está
///   parado» o «el servicio está deshabilitado», es que Defender está apagado.
/// - Código 2 sin fallo ni nombres: encontró algo y no se pudo leer qué. Se prefiere parar una
///   instalación de más a dejar pasar una detección.
/// - Cualquier otra cosa: no se sabe, no se analizó.
pub fn interpret(code: Option<i32>, output: &str) -> Outcome {
    let mut names: Vec<&str> = Vec::new();
    let mut found = 0u64;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Threat") {
            // «Threat                  : Virus:DOS/EICAR_Test_File», y no «Threat information».
            if let Some(name) = rest.trim_start().strip_prefix(':').map(str::trim) {
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        // «Scanning C:\… found 1 threats.»
        if let Some((_, rest)) = line.rsplit_once(" found ") {
            if let Some(n) = rest.strip_suffix(" threats.").and_then(|n| n.trim().parse::<u64>().ok()) {
                found += n;
            }
        }
    }
    if !names.is_empty() || found > 0 {
        return Outcome::Threat(names.join(", "));
    }
    let lower = output.to_ascii_lowercase();
    match code {
        Some(0) => Outcome::Clean,
        Some(2) if lower.contains("hr = 0x") || lower.contains("hr=0x") => {
            // 0x800106ba: el servicio de Defender no está en marcha. 0x80070422: está deshabilitado.
            let off = ["800106ba", "80070422"].iter().any(|hr| lower.contains(hr));
            Outcome::NotScanned(if off { Reason::Disabled } else { Reason::Failed })
        }
        Some(2) => Outcome::Threat(String::new()),
        _ => Outcome::NotScanned(Reason::Failed),
    }
}

/// Lo que escribió el analizador, como texto: UTF-16 si lo parece (una marca al principio, o un
/// cero en cada byte impar, que es como escriben algunas herramientas de Windows cuando su
/// salida no es una consola) y, si no, UTF-8 sin fallar por un byte suelto.
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |b: &[u8]| {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(rest);
    }
    let pairs = bytes.len() / 2;
    if pairs >= 4 && bytes.chunks_exact(2).filter(|c| c[1] == 0 && c[0] != 0).count() * 10 >= pairs * 9 {
        return utf16(bytes);
    }
    String::from_utf8_lossy(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)).into_owned()
}

/// Cuánto se espera al analizador según lo que pesa el archivo: un minuto, más un segundo por
/// cada 4 MB (un zip de 2 GB se analiza también por dentro), y nunca más de un cuarto de hora.
pub fn timeout_for(size: u64) -> Duration {
    Duration::from_secs((60 + size / (4 << 20)).min(15 * 60))
}

/// De las carpetas de `Platform` de Defender (`4.18.26100.4-0`), la de la versión más nueva.
/// Las que no son una versión (`CurrentVersion`) no cuentan.
pub fn newest<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let key = |name: &str| -> Option<Vec<u64>> {
        let parts: Vec<u64> = name.split(['.', '-']).map(|p| p.parse::<u64>().ok()).collect::<Option<_>>()?;
        (parts.len() >= 2).then_some(parts)
    };
    names.into_iter().filter_map(|n| key(n).map(|k| (k, n))).max().map(|(_, n)| n)
}

/// Analiza `file` con el antivirus del equipo y dice qué pasó. No borra ni cambia nada.
pub fn check(file: &Path) -> Outcome {
    imp::check(file)
}

/// `check` para una descarga que se va a instalar: lo que va al recibo o, si encontró algo, el
/// nombre de la amenaza (`Err`). Quien llama borra lo bajado y falla con `Error::Threat`.
pub fn scan(file: &Path) -> std::result::Result<Scan, String> {
    match check(file) {
        Outcome::Clean => Ok(Scan::Clean { scanner: DEFENDER.to_string() }),
        Outcome::NotScanned(reason) => Ok(Scan::NotScanned { reason }),
        Outcome::Threat(name) => Err(name),
    }
}

#[cfg(windows)]
mod imp {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::{decode, interpret, newest, timeout_for, Outcome, Reason};

    /// Sin esto, abrir un programa de consola desde la ventana hace parpadear una consola.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Dónde está `MpCmdRun.exe`: en la carpeta de Defender de siempre o, si ahí no está, en la
    /// de la versión más nueva de su plataforma (donde lo dejan sus actualizaciones).
    fn locate() -> Option<PathBuf> {
        for var in ["ProgramW6432", "ProgramFiles"] {
            if let Some(dir) = std::env::var_os(var) {
                let exe = Path::new(&dir).join("Windows Defender").join("MpCmdRun.exe");
                if exe.is_file() {
                    return Some(exe);
                }
            }
        }
        let platform = Path::new(&std::env::var_os("ProgramData")?).join("Microsoft").join("Windows Defender").join("Platform");
        let names: Vec<String> = std::fs::read_dir(&platform).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        let exe = platform.join(newest(names.iter().map(String::as_str))?).join("MpCmdRun.exe");
        exe.is_file().then_some(exe)
    }

    pub(super) fn check(file: &Path) -> Outcome {
        let Some(exe) = locate() else { return Outcome::NotScanned(Reason::NoScanner) };
        let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        let mut c = Command::new(exe);
        c.args(["-Scan", "-ScanType", "3", "-File"]).arg(file).arg("-DisableRemediation");
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
        let Ok(mut child) = c.spawn() else { return Outcome::NotScanned(Reason::Failed) };
        // Se lee en otro hilo para que el analizador no se quede esperando a que alguien lea.
        let reader = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = out.read_to_end(&mut bytes);
                bytes
            })
        });
        let deadline = Instant::now() + timeout_for(size);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let output = reader.and_then(|r| r.join().ok()).unwrap_or_default();
        match status {
            Some(status) => interpret(status.code(), &decode(&output)),
            None => Outcome::NotScanned(Reason::Timeout),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Outcome, Reason};

    pub(super) fn check(_file: &std::path::Path) -> Outcome {
        Outcome::NotScanned(Reason::NoScanner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo que escribe `MpCmdRun` de verdad con un archivo limpio (Windows 11, 20-11-2026).
    const LIMPIO: &str = "Scan starting...\r\nScan finished.\r\nScanning C:\\Users\\yo\\AppData\\Local\\Temp\\prueba\\inofensivo.txt found no threats.\r\n";

    /// Lo que escribe con `-DisableRemediation` cuando encuentra algo, según la documentación de
    /// Microsoft y los registros que la gente publica. No se prueba con un virus de verdad.
    const AMENAZA: &str = "Scan starting...\r\nScan finished.\r\nScanning C:\\b\\downloads\\juego.zip found 1 threats.\r\n\r\n\
<===========================LIST OF DETECTED THREATS==========================>\r\n\
----------------------------- Threat information ------------------------------\r\n\
Threat                  : Trojan:Win32/Wacatac.B!ml\r\n\
Resources               : 1 total\r\n\
    file                : C:\\b\\downloads\\juego.zip->dentro/juego.exe\r\n\
-------------------------------------------------------------------------------\r\n";

    #[test]
    fn limpio_si_sale_con_cero() {
        assert_eq!(interpret(Some(0), LIMPIO), Outcome::Clean);
        assert_eq!(interpret(Some(0), ""), Outcome::Clean);
    }

    #[test]
    fn amenaza_con_su_nombre() {
        assert_eq!(interpret(Some(2), AMENAZA), Outcome::Threat("Trojan:Win32/Wacatac.B!ml".into()));
        // Dos distintas, y una repetida que solo sale una vez.
        let dos = format!("{AMENAZA}Threat                  : Virus:DOS/Otro\r\nThreat : Virus:DOS/Otro\r\n");
        assert_eq!(interpret(Some(2), &dos), Outcome::Threat("Trojan:Win32/Wacatac.B!ml, Virus:DOS/Otro".into()));
        // Manda lo que escribió, no el código de salida.
        assert_eq!(interpret(Some(0), AMENAZA), Outcome::Threat("Trojan:Win32/Wacatac.B!ml".into()));
        // Dice que encontró algo pero no trae la lista.
        assert_eq!(interpret(Some(2), "Scanning C:\\a.zip found 3 threats.\r\n"), Outcome::Threat(String::new()));
        // Código 2 sin explicación: se trata como una detección.
        assert_eq!(interpret(Some(2), ""), Outcome::Threat(String::new()));
        // Una ruta con « found » en el nombre no cuenta como detección.
        assert_eq!(interpret(Some(0), "Scanning C:\\lost found 2 threats.\\a.txt found no threats.\r\n"), Outcome::Clean);
    }

    #[test]
    fn no_analizado_si_defender_esta_apagado_o_falla() {
        let apagado = "CmdTool: Failed with hr = 0x800106ba. Check C:\\Users\\yo\\AppData\\Local\\Temp\\MpCmdRun.log for more information\r\n";
        assert_eq!(interpret(Some(2), apagado), Outcome::NotScanned(Reason::Disabled));
        assert_eq!(interpret(Some(2), "CmdTool: Failed with hr = 0x80070422.\r\n"), Outcome::NotScanned(Reason::Disabled));
        assert_eq!(interpret(Some(2), "CmdTool: Failed with hr = 0x80508023. Check the log\r\n"), Outcome::NotScanned(Reason::Failed));
        // Otros códigos, o que lo mataran.
        assert_eq!(interpret(Some(1), ""), Outcome::NotScanned(Reason::Failed));
        assert_eq!(interpret(Some(-1073741819), LIMPIO), Outcome::NotScanned(Reason::Failed));
        assert_eq!(interpret(None, ""), Outcome::NotScanned(Reason::Failed));
    }

    #[test]
    fn lee_la_salida_en_utf8_y_en_utf16() {
        assert_eq!(decode(LIMPIO.as_bytes()), LIMPIO);
        let utf16: Vec<u8> = AMENAZA.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode(&utf16), AMENAZA);
        let con_marca = [&[0xff, 0xfe][..], &utf16].concat();
        assert_eq!(interpret(Some(2), &decode(&con_marca)), Outcome::Threat("Trojan:Win32/Wacatac.B!ml".into()));
        assert_eq!(decode(b"\xef\xbb\xbfhola"), "hola");
        assert_eq!(decode(b""), "");
        // Un byte suelto que no es UTF-8 no lo rompe.
        assert_eq!(decode(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn espera_mas_a_los_archivos_grandes() {
        assert_eq!(timeout_for(0), Duration::from_secs(60));
        assert_eq!(timeout_for(400 << 20), Duration::from_secs(160));
        assert_eq!(timeout_for(64 << 30), Duration::from_secs(900));
    }

    #[test]
    fn elige_la_plataforma_mas_nueva() {
        assert_eq!(newest(["4.18.26090.9-0", "4.18.26100.4-0", "CurrentVersion"]), Some("4.18.26100.4-0"));
        // Por números, no por letras: 26100 es más que 2610, y .10 más que .9.
        assert_eq!(newest(["4.18.2610.9-0", "4.18.26100.4-0"]), Some("4.18.26100.4-0"));
        assert_eq!(newest(["4.18.1.10-0", "4.18.1.9-0"]), Some("4.18.1.10-0"));
        assert_eq!(newest(["CurrentVersion"]), None);
        assert_eq!(newest([]), None);
    }

    #[test]
    fn el_recibo_guarda_el_resultado_con_nombres_estables() {
        let limpio = Scan::Clean { scanner: DEFENDER.into() };
        assert_eq!(serde_json::to_string(&limpio).unwrap(), r#"{"state":"clean","scanner":"Microsoft Defender"}"#);
        let sin = Scan::NotScanned { reason: Reason::NoScanner };
        assert_eq!(serde_json::to_string(&sin).unwrap(), r#"{"state":"not-scanned","reason":"no-scanner"}"#);
        for s in [limpio, sin, Scan::NotScanned { reason: Reason::Timeout }, Scan::NotScanned { reason: Reason::Disabled }] {
            assert_eq!(serde_json::from_str::<Scan>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        }
        // Un motivo que escriba una versión más nueva no impide leer el recibo.
        let nuevo: Scan = serde_json::from_str(r#"{"state":"not-scanned","reason":"algo-nuevo"}"#).unwrap();
        assert_eq!(nuevo, Scan::NotScanned { reason: Reason::Unknown });
    }

    /// Un análisis de verdad, con el Defender de este equipo, de un archivo de texto inofensivo:
    /// comprueba que la línea de órdenes funciona. Si Defender no está o está apagado, el
    /// resultado es «no analizado», que también vale; lo que no puede salir es una amenaza.
    #[test]
    #[cfg(windows)]
    fn analiza_un_archivo_inofensivo_con_el_defender_del_equipo() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("inofensivo con espacios.txt");
        std::fs::write(&f, "Archivo de texto inofensivo para probar el analizador.\n").unwrap();
        let visto = check(&f);
        eprintln!("Defender dijo: {visto:?}");
        assert!(matches!(visto, Outcome::Clean | Outcome::NotScanned(_)), "{visto:?}");
        assert!(f.is_file());
    }

    #[test]
    #[cfg(not(windows))]
    fn sin_analizador_fuera_de_windows() {
        assert_eq!(scan(Path::new("/no/importa")), Ok(Scan::NotScanned { reason: Reason::NoScanner }));
    }
}
