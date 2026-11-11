//! La ficha de un mod dentro de Unricopie, como la de Thunderstore (10-11, pedidos del usuario:
//! abrir Thunderstore sin avisar «se siente como un virus»; y después, «mira la comparación»).
//! Muestra los datos de la lista de Thunderstore y su README y su registro de cambios, que se
//! piden a su API pública. Thunderstore queda como enlace externo, a la vista.
//!
//! El README se parte en bloques: los títulos aparte (`StyledText` de Slint no los admite), los
//! párrafos y las listas como Markdown (con los enlaces que se pueden pulsar), el código y las
//! rayas. Las imágenes y el HTML se quitan.
use std::time::Duration;

use unricopie_nucleo::mods::sources::RemoteMod;
use unricopie_nucleo::mods::Source;
use unricopie_nucleo::net;

use crate::trabajo::{avisar, Aviso};

/// El README más largo que se muestra.
const MAX_TEXTO: usize = 200_000;

/// Qué se pide: el README o el registro de cambios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Que {
    Readme,
    Cambios,
}

/// La dirección del README o del registro de cambios de la versión de un mod de Thunderstore.
fn url(m: &RemoteMod, que: Que) -> Option<String> {
    let Source::Thunderstore { full_name, .. } = &m.source else { return None };
    let (autor, nombre) = full_name.split_once('-')?;
    let limpio = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let parte = match que {
        Que::Readme => "readme",
        Que::Cambios => "changelog",
    };
    (limpio(autor) && limpio(nombre) && m.version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
        .then(|| format!("https://thunderstore.io/api/experimental/package/{autor}/{nombre}/{}/{parte}/", m.version))
}

/// El mod al que lleva un enlace a Thunderstore (`https://thunderstore.io/c/<comunidad>/p/<autor>/<nombre>/`
/// o `https://thunderstore.io/package/<autor>/<nombre>/`), como id de Unricopie.
pub fn id_de_enlace(url: &str) -> Option<String> {
    let resto = url.strip_prefix("https://thunderstore.io/")?;
    let partes: Vec<&str> = resto.split(['/', '?', '#']).collect();
    let (autor, nombre) = match partes.as_slice() {
        ["c", _, "p", autor, nombre, ..] => (*autor, *nombre),
        ["package", autor, nombre, ..] => (*autor, *nombre),
        _ => return None,
    };
    let limpio = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (limpio(autor) && limpio(nombre)).then(|| format!("thunderstore:{autor}-{nombre}"))
}

/// La página del autor en Thunderstore.
pub fn url_autor(m: &RemoteMod) -> String {
    match &m.source {
        Source::Thunderstore { community, .. } => format!("https://thunderstore.io/c/{community}/p/{}/", m.author),
        _ => String::new(),
    }
}

/// Pide el README o el registro de cambios en un hilo y lo manda a la ventana como Markdown
/// (`None` si no se pudo leer o el mod no lo publica).
pub fn pedir(m: &RemoteMod, que: Que) {
    let id = m.id.as_str().to_string();
    let Some(url) = url(m, que) else {
        avisar(Aviso::Readme { id, que, md: None });
        return;
    };
    std::thread::spawn(move || {
        let md = (|| -> Option<String> {
            let r = net::agent().get(&url).timeout(Duration::from_secs(20)).call().ok()?;
            let v: serde_json::Value = serde_json::from_str(&r.into_string().ok()?).ok()?;
            let md = v.get("markdown")?.as_str()?;
            let mut fin = md.len().min(MAX_TEXTO);
            while !md.is_char_boundary(fin) {
                fin -= 1;
            }
            Some(md[..fin].to_string())
        })();
        avisar(Aviso::Readme { id, que, md });
    });
}

/// Un trozo del README.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bloque {
    /// Un título, con su nivel (1 el más grande) y su texto llano.
    Titulo(u8, String),
    /// Párrafos y listas, en Markdown sin títulos, imágenes ni HTML.
    Texto(String),
    Codigo(String),
    Raya,
}

/// Parte el Markdown de un README en bloques.
pub fn bloques(md: &str) -> Vec<Bloque> {
    let mut out = Vec::new();
    let mut parrafo: Vec<String> = Vec::new();
    let mut codigo: Option<Vec<String>> = None;
    let cerrar = |parrafo: &mut Vec<String>, out: &mut Vec<Bloque>| {
        let t = parrafo.join("\n").trim().to_string();
        if !t.is_empty() {
            out.push(Bloque::Texto(t));
        }
        parrafo.clear();
    };
    for linea in md.lines() {
        let t = linea.trim_end();
        let s = t.trim_start();
        if s.starts_with("```") || s.starts_with("~~~") {
            match codigo.take() {
                Some(c) => out.push(Bloque::Codigo(c.join("\n"))),
                None => {
                    cerrar(&mut parrafo, &mut out);
                    codigo = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(c) = codigo.as_mut() {
            c.push(t.to_string());
            continue;
        }
        let s = limpiar_linea(s);
        let s = s.trim();
        if s.is_empty() {
            cerrar(&mut parrafo, &mut out);
            continue;
        }
        // Título con almohadillas.
        let almohadillas = s.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&almohadillas) && s[almohadillas..].starts_with(' ') {
            cerrar(&mut parrafo, &mut out);
            let texto = llano(s[almohadillas..].trim().trim_end_matches('#').trim());
            if !texto.is_empty() {
                out.push(Bloque::Titulo(almohadillas.min(3) as u8, texto));
            }
            continue;
        }
        // Título subrayado (=== o --- bajo una sola línea) o raya.
        let solo = |c: char| s.len() >= 3 && s.chars().all(|x| x == c || x == ' ');
        if solo('=') || solo('-') || solo('*') || solo('_') {
            if parrafo.len() == 1 && (solo('=') || solo('-')) {
                let texto = llano(&parrafo.remove(0));
                out.push(Bloque::Titulo(if solo('=') { 1 } else { 2 }, texto));
            } else {
                cerrar(&mut parrafo, &mut out);
                out.push(Bloque::Raya);
            }
            continue;
        }
        // Tablas: cada fila, sus celdas separadas por «·»; la línea de guiones, fuera.
        if s.starts_with('|') {
            let celdas: Vec<&str> = s.trim_matches('|').split('|').map(str::trim).collect();
            if celdas.iter().all(|c| c.chars().all(|x| matches!(x, '-' | ':' | ' '))) {
                continue;
            }
            // Con dos espacios al final: en Markdown, cada fila en su línea.
            parrafo.push(celdas.into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" · ") + "  ");
            continue;
        }
        // Citas: sin el «>».
        let s = s.trim_start_matches('>').trim_start();
        parrafo.push(s.to_string());
    }
    if let Some(c) = codigo {
        out.push(Bloque::Codigo(c.join("\n")));
    }
    cerrar(&mut parrafo, &mut out);
    out
}

/// Una línea sin HTML (con `<br>` como salto) ni imágenes, y sin los enlaces que se quedaron
/// vacíos al quitar la imagen que llevaban.
fn limpiar_linea(s: &str) -> String {
    let s = quitar_html(s);
    let s = quitar_imagenes(&s);
    enlazar_sueltas(&quitar_enlaces_vacios(&s))
}

/// Las direcciones sueltas (`https://…` en medio del texto) como enlaces de Markdown; las que ya
/// son enlaces, o van entre `<>` o dentro de código, quedan como están.
fn enlazar_sueltas(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let empieza = |i: usize, p: &str| p.chars().enumerate().all(|(k, x)| c.get(i + k) == Some(&x));
    while i < c.len() {
        // Un enlace de Markdown entero: tal cual.
        if c[i] == '[' {
            if let Some(cierre) = (i + 1..c.len()).find(|&k| c[k] == ']') {
                if c.get(cierre + 1) == Some(&'(') {
                    if let Some(fin) = cierre_parentesis(&c, cierre + 2) {
                        out.extend(&c[i..=fin]);
                        i = fin + 1;
                        continue;
                    }
                }
            }
        }
        // Código o `<dirección>`: tal cual, hasta que se cierra.
        if c[i] == '`' || (c[i] == '<' && empieza(i + 1, "http")) {
            let cierra = if c[i] == '`' { '`' } else { '>' };
            if let Some(fin) = (i + 1..c.len()).find(|&k| c[k] == cierra) {
                out.extend(&c[i..=fin]);
                i = fin + 1;
                continue;
            }
        }
        if empieza(i, "https://") || empieza(i, "http://") {
            let mut fin = (i..c.len()).find(|&k| c[k].is_whitespace() || matches!(c[k], '<' | '>' | '"' | '`')).unwrap_or(c.len());
            // El punto o la coma del final son de la frase, no de la dirección.
            while fin > i && matches!(c[fin - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | '*' | '_') {
                fin -= 1;
            }
            let url: String = c[i..fin].iter().collect();
            out.push_str(&format!("[{url}]({url})"));
            i = fin;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `[](url)` fuera (lo que queda de un enlace que solo llevaba una imagen).
fn quitar_enlaces_vacios(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '[' && c.get(i + 1) == Some(&']') && c.get(i + 2) == Some(&'(') {
            if let Some(fin) = cierre_parentesis(&c, i + 3) {
                i = fin + 1;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// El texto llano de un trozo de Markdown en línea: sin énfasis ni código, los enlaces con su
/// texto.
fn llano(s: &str) -> String {
    let s = enlaces_a_texto(s);
    s.replace("**", "").replace("__", "").replace(['`', '*'], "").trim().to_string()
}

fn quitar_html(s: &str) -> String {
    let mut out = String::new();
    let mut etiqueta = String::new();
    let mut dentro = false;
    for c in s.chars() {
        match c {
            '<' => {
                dentro = true;
                etiqueta.clear();
            }
            '>' if dentro => {
                dentro = false;
                let e = etiqueta.trim().to_ascii_lowercase();
                if e.starts_with("br") {
                    out.push_str("  \n");
                }
            }
            _ if dentro => etiqueta.push(c),
            _ => out.push(c),
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
}

/// El final de un `(destino)` de Markdown que empieza en `desde`, contando los paréntesis que
/// abre la dirección (`demo(1).gif`).
fn cierre_parentesis(c: &[char], desde: usize) -> Option<usize> {
    let mut nivel = 0;
    (desde..c.len()).find(|&k| match c[k] {
        '(' => {
            nivel += 1;
            false
        }
        ')' if nivel == 0 => true,
        ')' => {
            nivel -= 1;
            false
        }
        _ => false,
    })
}

/// `![alt](url)` fuera.
fn quitar_imagenes(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '!' && c.get(i + 1) == Some(&'[') {
            if let Some(cierre) = (i + 2..c.len()).find(|&k| c[k] == ']') {
                if c.get(cierre + 1) == Some(&'(') {
                    if let Some(fin) = cierre_parentesis(&c, cierre + 2) {
                        i = fin + 1;
                        continue;
                    }
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `[texto](url)` → `texto`.
fn enlaces_a_texto(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '[' {
            if let Some(cierre) = (i + 1..c.len()).find(|&k| c[k] == ']') {
                if c.get(cierre + 1) == Some(&'(') {
                    if let Some(fin) = cierre_parentesis(&c, cierre + 2) {
                        out.extend(&c[i + 1..cierre]);
                        i = fin + 1;
                        continue;
                    }
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlaza_las_direcciones_sueltas() {
        assert_eq!(
            enlazar_sueltas("by Danielryb: https://thunderstore.io/c/z/p/danielryb/. See [x](https://a/b) or `https://c` or <https://d>."),
            "by Danielryb: [https://thunderstore.io/c/z/p/danielryb/](https://thunderstore.io/c/z/p/danielryb/). See [x](https://a/b) or `https://c` or <https://d>."
        );
    }

    #[test]
    fn reconoce_los_enlaces_a_otros_mods() {
        assert_eq!(id_de_enlace("https://thunderstore.io/c/zelda-64-recompiled/p/tomtee/Interface_Helper/").as_deref(), Some("thunderstore:tomtee-Interface_Helper"));
        assert_eq!(id_de_enlace("https://thunderstore.io/package/Nerrel/MMN64HD/").as_deref(), Some("thunderstore:Nerrel-MMN64HD"));
        assert_eq!(id_de_enlace("https://thunderstore.io/c/zelda-64-recompiled/p/danielryb/"), None);
        assert_eq!(id_de_enlace("https://github.com/x/y"), None);
    }

    #[test]
    fn parte_el_readme_en_bloques() {
        let md = "# Modern Controller Overhaul\n\n![banner](https://x/y.png)\n\nA mod by **tomtee**. See [the wiki](https://w).\n\n- One\n* Two\n\n---\n\n<p align=\"center\">Hi &amp; bye<br>there</p>\n\nSetext\n======\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode here\n```\n\nAn example: ![demo](https://g/demo(1).gif?raw=true) [![badge](https://b/s.svg)](https://l)\n";
        let b = bloques(md);
        assert_eq!(
            b,
            vec![
                Bloque::Titulo(1, "Modern Controller Overhaul".into()),
                Bloque::Texto("A mod by **tomtee**. See [the wiki](https://w).".into()),
                Bloque::Texto("- One\n* Two".into()),
                Bloque::Raya,
                Bloque::Texto("Hi & bye  \nthere".into()),
                Bloque::Titulo(1, "Setext".into()),
                Bloque::Texto("a · b  \n1 · 2".into()),
                Bloque::Codigo("code here".into()),
                Bloque::Texto("An example:".into()),
            ]
        );
    }
}
