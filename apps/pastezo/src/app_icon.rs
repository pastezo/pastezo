//! Alternative app icons (Settings → App Icon).
//!
//! Each is a PNG in `icons/variants/`: `<id>.png` (512 px, applied) and
//! `<id>-preview.png` (the settings grid). They ship as files, not in the
//! binary (see `dir`): `Pastezo.app/Contents/Resources/icons/`, `icons\` next
//! to Pastezo.exe, `share/pastezo/icons/` next to `bin/` on Linux; the source
//! tree when run with `cargo run`.
//!
//! | OS      | What changes                                                        |
//! |---------|---------------------------------------------------------------------|
//! | macOS   | the bundle's icon (Finder, Dock, Launchpad; kept after quitting)    |
//! |         | and the Dock icon of the running app                                |
//! | Windows | the window and taskbar icon (an .exe cannot change its own icon)    |
//! | Linux   | the window icon, and the launcher / dock icon: `app.pastezo` in the |
//! |         | user's icon theme (`~/.local/share/icons/hicolor/512x512/apps`)     |

use std::path::PathBuf;

pub const DEFAULT: &str = "pastezo";

/// The default first, then the rest in the order of the settings grid.
pub const VARIANTS: &[(&str, &str)] = &[
    (DEFAULT, "Pastezo"),
    ("sky", "Sky"),
    ("retro", "Retro"),
    ("petal", "Petal"),
    ("navy", "Navy"),
    ("lime", "Lime"),
    ("peekaboo", "Peekaboo"),
    ("honey", "Honey"),
    ("orchid", "Orchid"),
    ("reef", "Reef"),
    ("pebble", "Pebble"),
    ("clementine", "Clementine"),
    ("prism", "Prism"),
    ("lilac", "Lilac"),
    ("sage", "Sage"),
    ("midnight", "Midnight"),
    ("berry", "Berry"),
    ("aqua", "Aqua"),
    ("sketch", "Sketch"),
    ("pond", "Pond"),
];

/// The id as stored in the settings, if it names a variant.
pub fn known(id: &str) -> Option<&'static str> {
    VARIANTS.iter().find(|(v, _)| *v == id).map(|(v, _)| *v)
}

fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // …/Pastezo.app/Contents/MacOS/Pastezo
    let contents = exe.parent()?.parent()?;
    (contents.file_name()? == "Contents" && contents.parent()?.extension()? == "app").then(|| contents.parent().unwrap().to_path_buf())
}

/// Where the variants are: inside the app bundle (macOS), next to the program
/// (Windows, portable Linux), `../share/pastezo/icons` (installed on Linux),
/// else the source tree (`cargo run`).
fn dir() -> PathBuf {
    if let Some(app) = bundle() {
        return app.join("Contents/Resources/icons");
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    if let Some(exe_dir) = exe_dir {
        for d in [exe_dir.join("icons"), exe_dir.join("../share/pastezo/icons")] {
            if d.join(format!("{DEFAULT}.png")).is_file() {
                return d;
            }
        }
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/variants"))
}

pub fn file(id: &str) -> PathBuf {
    dir().join(format!("{id}.png"))
}

pub fn preview(id: &str) -> slint::Image {
    slint::Image::load_from_path(&dir().join(format!("{id}-preview.png"))).unwrap_or_default()
}

/// Makes `id` the app's icon (see the table above). `startup`: only what the
/// running process needs, unless an update of the app put its default icon
/// back (a new `Pastezo.app`, `install.sh`): then the chosen one is set again.
pub fn apply(id: &str, startup: bool) {
    platform::apply(id, startup);
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
    use objc2_foundation::NSString;

    pub fn apply(id: &str, startup: bool) {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        let bundle = super::bundle();
        // At launch a bundle shows its own icon (with the chosen one set on it
        // earlier): nothing to load. Run from the source tree, the Dock gets the
        // small preview: a 512 px image costs ~8 MB of decoded copies.
        if startup {
            if bundle.is_none() {
                let path = super::dir().join(format!("{id}-preview.png"));
                if let Some(icon) = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path.to_string_lossy())) {
                    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
                }
                return;
            }
            // A custom icon of a bundle is the file "Icon\r" inside it. An update
            // replaces the whole bundle and drops it: set the chosen one again.
            if id == super::DEFAULT || bundle.as_ref().is_some_and(|b| b.join("Icon\r").exists()) {
                return;
            }
        }
        let path = NSString::from_str(&super::file(id).to_string_lossy());
        let Some(icon) = NSImage::initWithContentsOfFile(NSImage::alloc(), &path) else { return };
        // the Dock tile of this running process (set explicitly for the default
        // too: clearing it leaves the Dock showing the previous one until relaunch)
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
        // the bundle itself, so Finder, Launchpad and the Dock keep it after quitting
        if let Some(bundle) = bundle {
            let path = NSString::from_str(&bundle.to_string_lossy());
            let image = if id == super::DEFAULT { None } else { Some(&*icon) };
            let ok = NSWorkspace::sharedWorkspace().setIcon_forFile_options(image, &path, NSWorkspaceIconCreationOptions::empty());
            if !ok {
                eprintln!("pastezo: cannot set the app icon (is Pastezo.app writable?)");
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! The launcher and dock take `Icon=app.pastezo` from the icon theme, and
    //! the user's own theme folder comes first: the chosen icon goes there
    //! (the default one too: `install.sh` puts it in that very place).

    fn target() -> Option<std::path::PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".local/share")))?;
        Some(data.join("icons/hicolor/512x512/apps/app.pastezo.png"))
    }

    pub fn apply(id: &str, startup: bool) {
        let Some(target) = target() else { return };
        // `install.sh` of an update puts the default icon there again
        if startup && (id == super::DEFAULT || super::same_file(&super::file(id), &target)) {
            return;
        }
        let result = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::copy(super::file(id), &target).map(|_| ()));
        if let Err(e) = result {
            eprintln!("pastezo: cannot set the app icon: {e}");
        }
        // desktops re-read the theme folder when it changes
        if let Some(theme) = target.ancestors().nth(3) {
            let _ = std::fs::File::open(theme).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        }
    }
}

/// Whether two files hold the same bytes (the size alone tells most apart).
#[cfg(any(target_os = "linux", test))]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    let len = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).ok();
    len(a).is_some() && len(a) == len(b) && std::fs::read(a).ok() == std::fs::read(b).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    // Windows: the window and taskbar icon follow the choice (set by the window).
    pub fn apply(_id: &str, _startup: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_compares_contents() {
        assert!(same_file(&file("sky"), &file("sky")));
        assert!(!same_file(&file("sky"), &file("navy")));
        assert!(!same_file(&file("sky"), std::path::Path::new("/no/such/file")));
    }

    #[test]
    fn every_variant_has_its_files() {
        let mut ids: Vec<_> = VARIANTS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids[0], DEFAULT);
        for id in &ids {
            assert!(file(id).is_file(), "{id}.png");
            assert!(dir().join(format!("{id}-preview.png")).is_file(), "{id}-preview.png");
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), VARIANTS.len());
        assert_eq!(known("sky"), Some("sky"));
        assert_eq!(known("gone"), None);
    }
}
