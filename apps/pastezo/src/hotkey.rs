//! Settings → General → "Shortcut:": the global shortcut that opens Pastezo
//! from any app (none by default). A click on the button starts recording:
//! the next key press with ⌘/Ctrl/Alt (or an F-key alone) becomes the
//! shortcut, Esc cancels, ⌫/Delete removes it. Keys are read from winit
//! (the physical key and what it types), not from Slint, which gives only
//! the text. The shortcut is saved to the `hotkey` file and registered by the
//! agent (`clip_core::hotkey`); one the OS refuses (another app has it) is
//! not kept.

use std::cell::Cell;
use std::rc::Rc;

use clip_core::hotkey::{mods, Hotkey};
use clip_core::platform::hotkeys;
use slint::ComponentHandle;

use crate::{App, SettingsWindow, ToastKind};

impl App {
    /// The settings window's shortcut row.
    pub(crate) fn wire_hotkey(self: &Rc<Self>, w: &SettingsWindow) {
        w.set_hotkey_available(hotkeys::available());
        w.set_hotkey(self.hotkey_text().into());
        w.on_hotkey_clear({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.set_hotkey(None);
                }
            }
        });
        listen(w, Rc::downgrade(self));
    }

    fn hotkey_text(&self) -> String {
        self.settings_dir.as_deref().and_then(Hotkey::load).map(|h| h.display()).unwrap_or_default()
    }

    /// A key pressed while recording: `key` is winit's `KeyCode` name,
    /// `typed` what it types in the current layout.
    pub(crate) fn hotkey_key(&self, mods: u8, key: &str, typed: Option<&str>) {
        let Some(w) = self.settings_window.borrow().as_ref().map(|w| w.as_weak()) else { return };
        let Some(w) = w.upgrade() else { return };
        match key {
            "Escape" => w.set_hotkey_recording(false),
            "Backspace" | "Delete" if mods == 0 => self.set_hotkey(None),
            _ => {
                if let Some(hotkey) = Hotkey::new(mods, key, typed) {
                    self.set_hotkey(Some(hotkey));
                }
            }
        }
    }

    /// Saves the shortcut (`None`: none) and has the agent register it; if
    /// the OS refuses it, the previous one stays and a toast says why.
    pub(crate) fn set_hotkey(&self, hotkey: Option<Hotkey>) {
        if let Some(dir) = &self.settings_dir {
            let previous = Hotkey::load(dir);
            if let Err(e) = Hotkey::save(dir, hotkey.as_ref()) {
                eprintln!("pastezo: cannot save the shortcut: {e}");
            } else if hotkeys::request_reload(dir) == Some(false) {
                let _ = Hotkey::save(dir, previous.as_ref());
                hotkeys::request_reload(dir);
                self.notify(ToastKind::Error, self.i18n.t("toast.hotkeyTaken", &[]));
            }
        }
        if let Some(w) = self.settings_window.borrow().as_ref() {
            w.set_hotkey_recording(false);
            let shown = match &self.settings_dir {
                Some(_) => self.hotkey_text(),
                None => hotkey.map(|h| h.display()).unwrap_or_default(), // tests
            };
            w.set_hotkey(shown.into());
        }
    }
}

/// While the settings window records a shortcut, its key presses go to
/// `App::hotkey_key` and nowhere else. On macOS the menus' own shortcuts
/// (⌘W, ⌘Q, ⌘H, ⌘M, ⌘,) act before the window sees them: they can't be recorded.
fn listen(w: &SettingsWindow, app: std::rc::Weak<App>) {
    use slint::winit_030::winit::event::{ElementState, WindowEvent};
    use slint::winit_030::winit::keyboard::{Key, ModifiersState, PhysicalKey};
    use slint::winit_030::{EventResult, WinitWindowAccessor};

    let held = Cell::new(ModifiersState::empty());
    let weak = w.as_weak();
    w.window().on_winit_window_event(move |_, event| {
        if let WindowEvent::ModifiersChanged(m) = event {
            held.set(m.state());
        }
        let (Some(w), Some(app)) = (weak.upgrade(), app.upgrade()) else { return EventResult::Propagate };
        if !w.get_hotkey_recording() {
            return EventResult::Propagate;
        }
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let m = held.get();
                        let bits = [(m.control_key(), mods::CTRL), (m.alt_key(), mods::ALT), (m.shift_key(), mods::SHIFT), (m.super_key(), mods::SUPER)]
                            .into_iter()
                            .filter(|(on, _)| *on)
                            .fold(0, |all, (_, bit)| all | bit);
                        let typed = typed(event);
                        let typed = match &typed {
                            Key::Character(s) => Some(s.as_str()),
                            _ => None,
                        };
                        app.hotkey_key(bits, &format!("{code:?}"), typed);
                    }
                }
                EventResult::PreventDefault
            }
            // clicked away: no longer recording
            WindowEvent::Focused(false) => {
                w.set_hotkey_recording(false);
                EventResult::Propagate
            }
            _ => EventResult::Propagate,
        }
    });
}

/// What the key types in the current layout, without the modifiers
/// (⌥V types "√" on macOS, the label should say "V").
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn typed(event: &slint::winit_030::winit::event::KeyEvent) -> slint::winit_030::winit::keyboard::Key {
    use slint::winit_030::winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    event.key_without_modifiers()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn typed(event: &slint::winit_030::winit::event::KeyEvent) -> slint::winit_030::winit::keyboard::Key {
    event.logical_key.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::sys;
    use crate::{wire, AppWindow, I18n};
    use clip_core::History;
    use i_slint_backend_testing as testing;
    use std::sync::Arc;

    /// Recording: Esc cancels, a combination is saved and shown, ⌫ and
    /// "Clear" remove it; plain letters and Shift+letter are not shortcuts.
    #[test]
    fn shortcut_is_recorded_and_cleared() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let clipboard = Arc::new(crate::tests::FakeClipboard::default());
        let app = wire(&ui, history, I18n::new(&sys()), "macos", clipboard, Box::new(|_| {}), Some(dir.path().into()));
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert_eq!(w.get_hotkey(), "", "none by default");
        let clear = || testing::ElementHandle::find_by_accessible_label(&w, "Clear").next();
        assert!(clear().is_none());

        let record = testing::ElementHandle::find_by_accessible_label(&w, "Record Shortcut").next().unwrap();
        record.invoke_accessible_default_action();
        assert!(w.get_hotkey_recording());
        app.hotkey_key(0, "Escape", None);
        assert!(!w.get_hotkey_recording(), "Esc: cancelled");

        w.set_hotkey_recording(true);
        app.hotkey_key(0, "KeyV", Some("v"));
        app.hotkey_key(mods::SHIFT, "KeyV", Some("v"));
        assert!(w.get_hotkey_recording(), "typing is not a shortcut");
        app.hotkey_key(mods::ALT | mods::SUPER, "KeyV", Some("v"));
        assert!(!w.get_hotkey_recording());
        let saved = Hotkey::load(dir.path()).unwrap();
        assert_eq!(saved, Hotkey::new(mods::ALT | mods::SUPER, "KeyV", None).unwrap());
        assert_eq!(w.get_hotkey(), saved.display());

        w.set_hotkey_recording(true);
        app.hotkey_key(0, "Backspace", None);
        assert_eq!(Hotkey::load(dir.path()), None, "⌫ removes it");
        assert_eq!(w.get_hotkey(), "");

        app.set_hotkey(Hotkey::new(mods::CTRL, "F5", None));
        clear().unwrap().invoke_accessible_default_action();
        assert_eq!(Hotkey::load(dir.path()), None);
        assert_eq!(w.get_hotkey(), "");
    }
}
