fn main() {
    // El estilo de los controles estándar no importa mucho: casi todo está dibujado a mano con
    // los colores del tema. Fluent oscuro es el que mejor casa con Noche.
    // Los textos están escritos en español; las traducciones (lang/<idioma>/LC_MESSAGES/unricopie.po)
    // van dentro del ejecutable, sin contexto por componente para que el .po sea simple.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("no se pudo compilar ui/app.slint");
    println!("cargo:rerun-if-changed=lang");
    recursos_de_windows();
}

/// En Windows, el icono del .exe (el pastel, en siete tamaños) y los datos que muestra
/// «Propiedades»: nombre, versión y licencia.
fn recursos_de_windows() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = dir.join("ui").join("img").join("unricopie.ico");
    let v = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut n: Vec<u16> = v.split(['.', '-']).take(3).map(|x| x.parse().unwrap_or(0)).collect();
    n.resize(3, 0);
    let rc = format!(
        r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Recompendium"
      VALUE "FileDescription", "Unricopie"
      VALUE "FileVersion", "{v}"
      VALUE "InternalName", "unricopie"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "unricopie.exe"
      VALUE "ProductName", "Unricopie"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        // En el .rc, cada barra invertida de la ruta va doble.
        ico = ico.display().to_string().replace('\\', "\\\\"),
        a = n[0],
        b = n[1],
        c = n[2],
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("unricopie.rc");
    std::fs::write(&out, rc).expect("no se pudo escribir unricopie.rc");
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().expect("no se pudo compilar el icono");
    println!("cargo:rerun-if-changed=ui/img/unricopie.ico");
}
