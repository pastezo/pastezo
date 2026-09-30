include!("../pastezo/version_rc.rs");

fn main() {
    // Windows: the version details of pastezo-agent.exe
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../pastezo/version_rc.rs");
        let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("pastezo-agent.rc");
        std::fs::write(&path, version_rc("pastezo-agent", "Pastezo Agent")).unwrap();
        embed_resource::compile(&path, embed_resource::NONE).manifest_optional().expect("compile pastezo-agent.rc");
    }
}
