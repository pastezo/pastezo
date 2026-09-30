// Windows: the "Details" tab of an .exe (product, version, description) as an
// .rc VERSIONINFO block, from the package's Cargo.toml. Shared by the build.rs
// of both programs (`include!`). A program without it looks suspicious to
// antivirus heuristics.

/// `name`: the .exe's file name without `.exe`; `title`: how Task Manager
/// and the firewall prompt call the program.
#[allow(dead_code)] // only when building for Windows
fn version_rc(name: &str, title: &str) -> String {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let n = |k: &str| std::env::var(k).unwrap().parse::<u16>().unwrap_or(0);
    let numbers = format!("{},{},{},0", n("CARGO_PKG_VERSION_MAJOR"), n("CARGO_PKG_VERSION_MINOR"), n("CARGO_PKG_VERSION_PATCH"));
    format!(
        r#"1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Pastezo"
      VALUE "FileDescription", "{title}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{name}"
      VALUE "OriginalFilename", "{name}.exe"
      VALUE "ProductName", "Pastezo"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
