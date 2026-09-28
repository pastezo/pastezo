use std::fmt::Write;

fn main() {
    // Windows: the program's own icon (Explorer, Start menu); set before the window exists
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=pastezo.rc");
        println!("cargo:rerun-if-changed=icons/icon.ico");
        embed_resource::compile("pastezo.rc", embed_resource::NONE).manifest_optional().expect("compile pastezo.rc");
    }

    // Widget style per OS: the only std widget we use is ListView (its scrollbar).
    let style = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") | Ok("ios") => "cupertino",
        Ok("android") => "material",
        _ => "fluent",
    };
    // element names for the UI tests (ElementHandle); not in release builds
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    let config = slint_build::CompilerConfiguration::new().with_style(style.into()).with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).expect("compile ui/app.slint");

    // Every locales/<tag>.json is embedded; only the chosen one is parsed at runtime.
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("locales");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut tags: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".json").map(String::from))
        .collect();
    tags.sort();
    let mut code = String::from("pub static LOCALES: &[(&str, &str)] = &[\n");
    for t in &tags {
        writeln!(code, "    ({t:?}, include_str!({:?})),", dir.join(format!("{t}.json")).display().to_string()).unwrap();
    }
    code.push_str("];\n");
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("locales.rs"), code).unwrap();
}
