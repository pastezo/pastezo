//! The global shortcut that opens the Pastezo window from any app
//! (Settings → General). The window records it and saves it to the `hotkey`
//! file in the data folder; the agent reads that file and registers the
//! shortcut with the OS (`platform::hotkeys`): the window only lives while it
//! is open, the agent always runs.
//!
//! A key is its physical place on the keyboard (the name of winit's
//! `KeyCode`, "KeyV"), the same key whatever layout is on. The label is what
//! that key types in the user's layout ("A" for `KeyQ` on AZERTY), saved at
//! recording time.

use std::path::Path;

/// Modifier keys, as bits.
pub mod mods {
    pub const CTRL: u8 = 1;
    pub const ALT: u8 = 2;
    pub const SHIFT: u8 = 4;
    /// ⌘ on macOS, the Windows key, Super on Linux
    pub const SUPER: u8 = 8;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    /// `mods::*` bits
    pub mods: u8,
    /// a `KEYS` name
    pub key: &'static str,
    /// what the key types, upper case ("V")
    pub label: String,
}

/// A key the shortcut can end with.
pub struct Key {
    /// winit's `KeyCode` name
    pub name: &'static str,
    /// macOS virtual key code (`kVK_*`, the ANSI place)
    pub mac: u16,
    /// PC scan code (set 1); the same number as Linux's evdev code
    pub scan: u16,
    /// its label on a US keyboard
    pub label: &'static str,
}

const fn key(name: &'static str, mac: u16, scan: u16, label: &'static str) -> Key {
    Key { name, mac, scan, label }
}

pub const KEYS: &[Key] = &[
    key("KeyA", 0x00, 30, "A"),
    key("KeyB", 0x0B, 48, "B"),
    key("KeyC", 0x08, 46, "C"),
    key("KeyD", 0x02, 32, "D"),
    key("KeyE", 0x0E, 18, "E"),
    key("KeyF", 0x03, 33, "F"),
    key("KeyG", 0x05, 34, "G"),
    key("KeyH", 0x04, 35, "H"),
    key("KeyI", 0x22, 23, "I"),
    key("KeyJ", 0x26, 36, "J"),
    key("KeyK", 0x28, 37, "K"),
    key("KeyL", 0x25, 38, "L"),
    key("KeyM", 0x2E, 50, "M"),
    key("KeyN", 0x2D, 49, "N"),
    key("KeyO", 0x1F, 24, "O"),
    key("KeyP", 0x23, 25, "P"),
    key("KeyQ", 0x0C, 16, "Q"),
    key("KeyR", 0x0F, 19, "R"),
    key("KeyS", 0x01, 31, "S"),
    key("KeyT", 0x11, 20, "T"),
    key("KeyU", 0x20, 22, "U"),
    key("KeyV", 0x09, 47, "V"),
    key("KeyW", 0x0D, 17, "W"),
    key("KeyX", 0x07, 45, "X"),
    key("KeyY", 0x10, 21, "Y"),
    key("KeyZ", 0x06, 44, "Z"),
    key("Digit1", 0x12, 2, "1"),
    key("Digit2", 0x13, 3, "2"),
    key("Digit3", 0x14, 4, "3"),
    key("Digit4", 0x15, 5, "4"),
    key("Digit5", 0x17, 6, "5"),
    key("Digit6", 0x16, 7, "6"),
    key("Digit7", 0x1A, 8, "7"),
    key("Digit8", 0x1C, 9, "8"),
    key("Digit9", 0x19, 10, "9"),
    key("Digit0", 0x1D, 11, "0"),
    key("Minus", 0x1B, 12, "-"),
    key("Equal", 0x18, 13, "="),
    key("BracketLeft", 0x21, 26, "["),
    key("BracketRight", 0x1E, 27, "]"),
    key("Semicolon", 0x29, 39, ";"),
    key("Quote", 0x27, 40, "'"),
    key("Backquote", 0x32, 41, "`"),
    key("Backslash", 0x2A, 43, "\\"),
    key("Comma", 0x2B, 51, ","),
    key("Period", 0x2F, 52, "."),
    key("Slash", 0x2C, 53, "/"),
    key("Space", 0x31, 57, "Space"),
    key("F1", 0x7A, 59, "F1"),
    key("F2", 0x78, 60, "F2"),
    key("F3", 0x63, 61, "F3"),
    key("F4", 0x76, 62, "F4"),
    key("F5", 0x60, 63, "F5"),
    key("F6", 0x61, 64, "F6"),
    key("F7", 0x62, 65, "F7"),
    key("F8", 0x64, 66, "F8"),
    key("F9", 0x65, 67, "F9"),
    key("F10", 0x6D, 68, "F10"),
    key("F11", 0x67, 87, "F11"),
    key("F12", 0x6F, 88, "F12"),
];

fn find(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.name == name)
}

const FILE: &str = "hotkey";

impl Hotkey {
    /// A shortcut from a key press: `None` if the key can't end one, or if
    /// nothing but Shift is held (that is just typing). F-keys work alone.
    /// `typed` is what the key types in the current layout; used for the
    /// label when it is a Latin letter (AZERTY's `KeyQ` types "a").
    pub fn new(mods: u8, key: &str, typed: Option<&str>) -> Option<Hotkey> {
        let key = find(key)?;
        let f_key = key.name.starts_with('F') && key.name.len() > 1 && key.name[1..].bytes().all(|b| b.is_ascii_digit());
        if mods & (mods::CTRL | mods::ALT | mods::SUPER) == 0 && !f_key {
            return None;
        }
        let letter = typed.filter(|t| key.name.starts_with("Key") && t.len() == 1 && t.as_bytes()[0].is_ascii_alphabetic());
        let label = letter.map_or(key.label.to_string(), |t| t.to_ascii_uppercase());
        Some(Hotkey { mods, key: key.name, label })
    }

    pub fn key(&self) -> &'static Key {
        find(self.key).expect("a Hotkey holds a known key")
    }

    /// As the OS writes shortcuts: "⌃⌥⇧⌘V" on macOS, "Ctrl+Alt+Shift+Win+V"
    /// on Windows ("Super" on Linux).
    pub fn display(&self) -> String {
        let has = |m| self.mods & m != 0;
        if cfg!(target_os = "macos") {
            let mut s = String::new();
            for (m, sign) in [(mods::CTRL, "⌃"), (mods::ALT, "⌥"), (mods::SHIFT, "⇧"), (mods::SUPER, "⌘")] {
                if has(m) {
                    s.push_str(sign);
                }
            }
            return s + &self.label;
        }
        let win = if cfg!(windows) { "Win" } else { "Super" };
        let mut parts: Vec<&str> = [(mods::CTRL, "Ctrl"), (mods::ALT, "Alt"), (mods::SHIFT, "Shift"), (mods::SUPER, win)]
            .into_iter()
            .filter(|(m, _)| has(*m))
            .map(|(_, name)| name)
            .collect();
        parts.push(&self.label);
        parts.join("+")
    }

    /// The file's line: "Ctrl+Shift+KeyV V" (modifiers + key, then the label).
    fn to_line(&self) -> String {
        let mut s = String::new();
        for (m, name) in [(mods::CTRL, "Ctrl+"), (mods::ALT, "Alt+"), (mods::SHIFT, "Shift+"), (mods::SUPER, "Super+")] {
            if self.mods & m != 0 {
                s.push_str(name);
            }
        }
        format!("{s}{} {}", self.key, self.label)
    }

    fn from_line(line: &str) -> Option<Hotkey> {
        let (combo, label) = line.trim().split_once(' ')?;
        let mut parts: Vec<&str> = combo.split('+').collect();
        let key = find(parts.pop()?)?;
        let mut m = 0;
        for p in parts {
            m |= match p {
                "Ctrl" => mods::CTRL,
                "Alt" => mods::ALT,
                "Shift" => mods::SHIFT,
                "Super" => mods::SUPER,
                _ => return None,
            };
        }
        let label = label.trim();
        let mut hotkey = Hotkey::new(m, key.name, None)?;
        if !label.is_empty() {
            hotkey.label = label.to_string();
        }
        Some(hotkey)
    }

    /// The saved shortcut; `None`: none set (the default) or an unreadable file.
    pub fn load(dir: &Path) -> Option<Hotkey> {
        Hotkey::from_line(&std::fs::read_to_string(dir.join(FILE)).ok()?)
    }

    /// Saves `hotkey`, or removes the file for no shortcut.
    pub fn save(dir: &Path, hotkey: Option<&Hotkey>) -> std::io::Result<()> {
        let file = dir.join(FILE);
        match hotkey {
            Some(h) => std::fs::write(file, h.to_line() + "\n"),
            None => match std::fs::remove_file(file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }
}

/// Held by the window process while it runs: the shortcut brings that
/// window to the front instead of starting a second one.
pub struct WindowLock(#[allow(dead_code)] std::fs::File);

impl WindowLock {
    /// `None`: another window already holds it (or no data folder).
    pub fn claim(dir: &Path) -> Option<WindowLock> {
        let lock = std::fs::File::create(dir.join("window.lock")).ok()?;
        lock.try_lock().ok()?;
        // a separate file: Windows does not let others read a locked one
        let _ = std::fs::write(dir.join("window.pid"), std::process::id().to_string());
        Some(WindowLock(lock))
    }
}

/// The process id of the running window, if one runs.
pub fn window_pid(dir: &Path) -> Option<u32> {
    let lock = std::fs::File::options().write(true).create(true).truncate(false).open(dir.join("window.lock")).ok()?;
    if lock.try_lock().is_ok() {
        return None; // nobody holds it; released when `lock` is dropped
    }
    std::fs::read_to_string(dir.join("window.pid")).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_a_real_modifier() {
        assert_eq!(Hotkey::new(mods::SHIFT, "KeyV", None), None, "Shift+V is typing");
        assert_eq!(Hotkey::new(0, "KeyV", None), None);
        assert_eq!(Hotkey::new(mods::CTRL, "Escape", None), None, "not a key a shortcut can end with");
        assert!(Hotkey::new(0, "F5", None).is_some(), "F-keys alone");
        assert!(Hotkey::new(mods::ALT | mods::SHIFT, "KeyV", None).is_some());
    }

    #[test]
    fn label_follows_the_layout() {
        // AZERTY: the key in Q's place types "a"
        assert_eq!(Hotkey::new(mods::CTRL, "KeyQ", Some("a")).unwrap().label, "A");
        // Russian: "м" is not a Latin letter, the US label is used
        assert_eq!(Hotkey::new(mods::CTRL, "KeyV", Some("м")).unwrap().label, "V");
        assert_eq!(Hotkey::new(mods::CTRL, "Digit1", Some("&")).unwrap().label, "1");
    }

    #[test]
    fn displayed_as_the_os_does() {
        let h = Hotkey::new(mods::SUPER | mods::SHIFT | mods::CTRL, "KeyV", None).unwrap();
        let expected = if cfg!(target_os = "macos") {
            "⌃⇧⌘V"
        } else if cfg!(windows) {
            "Ctrl+Shift+Win+V"
        } else {
            "Ctrl+Shift+Super+V"
        };
        assert_eq!(h.display(), expected);
    }

    #[test]
    fn saved_and_loaded() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Hotkey::load(dir.path()), None, "none by default");
        let h = Hotkey::new(mods::CTRL | mods::ALT, "KeyQ", Some("a")).unwrap();
        Hotkey::save(dir.path(), Some(&h)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join(FILE)).unwrap(), "Ctrl+Alt+KeyQ A\n");
        assert_eq!(Hotkey::load(dir.path()), Some(h));
        Hotkey::save(dir.path(), None).unwrap();
        Hotkey::save(dir.path(), None).unwrap();
        assert_eq!(Hotkey::load(dir.path()), None);
        for bad in ["", "Ctrl+Nope X", "Hyper+KeyV V", "Shift+KeyV V", "KeyV"] {
            std::fs::write(dir.path().join(FILE), bad).unwrap();
            assert_eq!(Hotkey::load(dir.path()), None, "{bad:?}");
        }
    }

    #[test]
    fn the_running_window_is_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(window_pid(dir.path()), None);
        let lock = WindowLock::claim(dir.path()).unwrap();
        assert_eq!(window_pid(dir.path()), Some(std::process::id()));
        drop(lock);
        assert_eq!(window_pid(dir.path()), None);
    }
}
