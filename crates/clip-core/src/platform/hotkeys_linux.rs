//! The global shortcut on Linux: X11 `XGrabKey` on the root window, events
//! read in their own thread (no work while idle). Wayland lets no app listen
//! to keys outside its windows (and XWayland sees keys only while an X
//! window has the focus), so there is no shortcut in a Wayland session:
//! Settings suggests a system shortcut that starts Pastezo instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, GrabMode, Keycode, ModMask, Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::hotkey::{mods, Hotkey};

/// Num Lock and Caps Lock also count as modifiers in X11: the shortcut is
/// grabbed with each of them on and off.
const LOCKS: [u16; 4] = [0, 1 << 1, 1 << 4, 1 << 1 | 1 << 4]; // none, Lock, Mod2 (Num Lock), both

/// A Wayland session: no global shortcuts (see the module notes).
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Whether this session lets Pastezo have a global shortcut: X11, not Wayland.
pub fn available() -> bool {
    !wayland() && std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Registers the shortcut from the data folder's `hotkey` file.
pub struct Hotkeys {
    dir: PathBuf,
    conn: Arc<RustConnection>,
    root: Window,
    /// the grabbed modifiers and key code
    current: Mutex<Option<(ModMask, Keycode)>>,
}

impl Hotkeys {
    /// `dir`: the data folder. `None` without X11, or in a Wayland session.
    pub fn start(dir: PathBuf) -> Option<Hotkeys> {
        if wayland() {
            return None;
        }
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let conn = Arc::new(conn);
        let (events, window_dir) = (conn.clone(), dir.clone());
        thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || loop {
                match events.wait_for_event() {
                    Ok(Event::KeyPress(_)) => open_window(&events, root, &window_dir),
                    Ok(_) => {}
                    Err(_) => return,
                }
            })
            .ok()?;
        Some(Hotkeys { dir, conn, root, current: Mutex::new(None) })
    }

    /// Grabs the saved shortcut in place of the previous one.
    /// `false`: another app has grabbed it.
    pub fn reload(&self) -> bool {
        let hotkey = Hotkey::load(&self.dir);
        let mut current = self.current.lock().unwrap();
        if let Some((modifiers, key)) = current.take() {
            self.ungrab(modifiers, key);
        }
        let Some(hotkey) = hotkey else {
            let _ = self.conn.flush();
            return true;
        };
        // X key codes are evdev codes + 8
        let key = (hotkey.key().scan + 8) as Keycode;
        let modifiers = [(mods::CTRL, ModMask::CONTROL), (mods::ALT, ModMask::M1), (mods::SHIFT, ModMask::SHIFT), (mods::SUPER, ModMask::M4)]
            .into_iter()
            .filter(|(m, _)| hotkey.mods & m != 0)
            .fold(ModMask::from(0u16), |all, (_, bit)| all | bit);
        for lock in LOCKS {
            let grabbed = self
                .conn
                .grab_key(false, self.root, modifiers | ModMask::from(lock), key, GrabMode::ASYNC, GrabMode::ASYNC)
                .ok()
                .and_then(|cookie| cookie.check().ok());
            if grabbed.is_none() {
                self.ungrab(modifiers, key);
                let _ = self.conn.flush();
                return false;
            }
        }
        *current = Some((modifiers, key));
        true
    }

    fn ungrab(&self, modifiers: ModMask, key: Keycode) {
        for lock in LOCKS {
            let _ = self.conn.ungrab_key(key, self.root, modifiers | ModMask::from(lock));
        }
    }
}

fn atom(conn: &RustConnection, name: &str) -> Option<Atom> {
    Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
}

/// The Pastezo window: brought to the front if it is open, started if not.
fn open_window(conn: &RustConnection, root: Window, dir: &Path) {
    if crate::hotkey::window_pid(dir).is_some_and(|pid| bring_forward(conn, root, pid).is_some()) {
        return;
    }
    if let Some(window) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("Pastezo"))) {
        super::spawn_and_reap(&mut Command::new(window));
    }
}

/// Asks the window manager to activate the window of process `pid`
/// (`_NET_ACTIVE_WINDOW`, as a pager does).
fn bring_forward(conn: &RustConnection, root: Window, pid: u32) -> Option<()> {
    let (list, wm_pid, active) = (atom(conn, "_NET_CLIENT_LIST")?, atom(conn, "_NET_WM_PID")?, atom(conn, "_NET_ACTIVE_WINDOW")?);
    let windows: Vec<Window> = conn.get_property(false, root, list, AtomEnum::WINDOW, 0, 4096).ok()?.reply().ok()?.value32()?.collect();
    let window = windows.into_iter().find(|&w| {
        let reply = conn.get_property(false, w, wm_pid, AtomEnum::CARDINAL, 0, 1).ok().and_then(|c| c.reply().ok());
        reply.and_then(|r| r.value32().and_then(|mut v| v.next())) == Some(pid)
    })?;
    // source 2: a pager, which window managers let through focus stealing prevention
    let event = ClientMessageEvent::new(32, window, active, [2, x11rb::CURRENT_TIME, 0, 0, 0]);
    conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event).ok()?;
    conn.flush().ok()
}

/// Window side: tells the agent the saved shortcut changed. `Some(false)`:
/// the OS refused it; `None`: no agent running (it reads the file when it starts).
pub fn request_reload(dir: &Path) -> Option<bool> {
    crate::ipc::request(dir, crate::ipc::Request::Hotkey)
}
