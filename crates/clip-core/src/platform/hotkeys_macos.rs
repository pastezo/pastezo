//! The global shortcut on macOS: Carbon's `RegisterEventHotKey` — needs no
//! Accessibility permission, the only system API for this that doesn't.
//! Carbon is loaded only once a shortcut is set (none by default), so an
//! agent without one does not carry it. Carbon wants the main thread, and
//! its presses arrive only through Carbon's own event loop (not a plain run
//! loop, measured): once Carbon is loaded, the agent's main thread switches
//! to `RunApplicationEventLoop` (`run_main_loop`), which also runs the main
//! run loop and the main dispatch queue as before.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};

use crate::hotkey::{mods, Hotkey};

type OsStatus = i32;
type Handler = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> OsStatus;

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
// Carbon modifier bits (Events.h)
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

type GetTarget = unsafe extern "C" fn() -> *mut c_void;
type Install = unsafe extern "C" fn(*mut c_void, Handler, u32, *const EventTypeSpec, *mut c_void, *mut *mut c_void) -> OsStatus;
type Register = unsafe extern "C" fn(u32, u32, EventHotKeyId, *mut c_void, u32, *mut *mut c_void) -> OsStatus;
type Unregister = unsafe extern "C" fn(*mut c_void) -> OsStatus;
type Run = unsafe extern "C" fn();

struct Carbon {
    target: GetTarget,
    install: Install,
    register: Register,
    unregister: Unregister,
    run: Run,
}

impl Carbon {
    fn load() -> Option<Carbon> {
        unsafe {
            let lib = libc::dlopen(c"/System/Library/Frameworks/Carbon.framework/Carbon".as_ptr(), libc::RTLD_LAZY);
            if lib.is_null() {
                return None;
            }
            let sym = |name: &std::ffi::CStr| {
                let p = libc::dlsym(lib, name.as_ptr());
                (!p.is_null()).then_some(p)
            };
            Some(Carbon {
                target: std::mem::transmute::<*mut c_void, GetTarget>(sym(c"GetApplicationEventTarget")?),
                install: std::mem::transmute::<*mut c_void, Install>(sym(c"InstallEventHandler")?),
                register: std::mem::transmute::<*mut c_void, Register>(sym(c"RegisterEventHotKey")?),
                unregister: std::mem::transmute::<*mut c_void, Unregister>(sym(c"UnregisterEventHotKey")?),
                run: std::mem::transmute::<*mut c_void, Run>(sym(c"RunApplicationEventLoop")?),
            })
        }
    }
}

/// Touched on the main thread only.
struct State {
    carbon: Option<Carbon>,
    handler: bool,
    /// the registered `EventHotKeyRef`
    current: usize,
}

static STATE: Mutex<State> = Mutex::new(State { carbon: None, handler: false, current: 0 });
static DIR: OnceLock<PathBuf> = OnceLock::new();

unsafe extern "C" fn pressed(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> OsStatus {
    if let Some(dir) = DIR.get() {
        open_window(dir);
    }
    0
}

/// Whether this OS lets Pastezo have a global shortcut.
pub fn available() -> bool {
    true
}

/// Registers the shortcut from the data folder's `hotkey` file.
pub struct Hotkeys;

impl Hotkeys {
    /// `dir`: the data folder. One per process.
    pub fn start(dir: PathBuf) -> Option<Hotkeys> {
        DIR.set(dir).ok()?;
        Some(Hotkeys)
    }

    /// Registers the saved shortcut in place of the previous one.
    /// `false`: the OS refused it (another app has it).
    pub fn reload(&self) -> bool {
        let hotkey = DIR.get().and_then(|d| Hotkey::load(d));
        let mut ok = false;
        on_main(|| ok = set(hotkey.as_ref()));
        ok
    }
}

fn on_main(work: impl FnOnce() + Send) {
    if unsafe { libc::pthread_main_np() } == 1 {
        work();
    } else {
        dispatch2::DispatchQueue::main().exec_sync(work);
    }
}

fn set(hotkey: Option<&Hotkey>) -> bool {
    let mut state = STATE.lock().unwrap();
    if state.current != 0 {
        if let Some(c) = &state.carbon {
            unsafe { (c.unregister)(state.current as *mut c_void) };
        }
        state.current = 0;
    }
    let Some(hotkey) = hotkey else { return true };
    if state.carbon.is_none() {
        state.carbon = Carbon::load();
        if state.carbon.is_some() {
            // leave the plain run loop for Carbon's (see `run_main_loop`)
            unsafe { CFRunLoopStop(CFRunLoopGetMain()) };
        }
    }
    let State { carbon: Some(c), handler, current } = &mut *state else { return false };
    unsafe {
        if !*handler {
            let spec = EventTypeSpec { class: K_EVENT_CLASS_KEYBOARD, kind: K_EVENT_HOT_KEY_PRESSED };
            *handler = (c.install)((c.target)(), pressed, 1, &spec, std::ptr::null_mut(), std::ptr::null_mut()) == 0;
        }
        let has = |m| hotkey.mods & m != 0;
        let modifiers = [(mods::SUPER, CMD_KEY), (mods::SHIFT, SHIFT_KEY), (mods::ALT, OPTION_KEY), (mods::CTRL, CONTROL_KEY)]
            .into_iter()
            .filter(|(m, _)| has(*m))
            .fold(0, |all, (_, bit)| all | bit);
        let id = EventHotKeyId { signature: u32::from_be_bytes(*b"PSTZ"), id: 1 };
        let mut out = std::ptr::null_mut();
        let status = (c.register)(hotkey.key().mac as u32, modifiers, id, (c.target)(), 0, &mut out);
        if status != 0 || out.is_null() {
            return false;
        }
        *current = out as usize;
    }
    true
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopStop(rl: *mut c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
}

/// `CFRunLoopRunInMode`: no sources or timers left to wait for.
const K_CF_RUN_LOOP_RUN_FINISHED: i32 = 1;

/// The agent's main thread, forever: the main run loop, or Carbon's event
/// loop once a shortcut has loaded Carbon (it stops the run loop to switch).
pub(super) fn run_main_loop() -> ! {
    loop {
        let run = STATE.lock().unwrap().carbon.as_ref().map(|c| c.run);
        if let Some(run) = run {
            unsafe { run() };
        }
        let why = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0e10, 0) };
        if why == K_CF_RUN_LOOP_RUN_FINISHED {
            break;
        }
    }
    loop {
        std::thread::park();
    }
}

/// The Pastezo window: brought to the front if it is open, started if not.
fn open_window(dir: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    // an installed app: Pastezo.app/Contents/Helpers/pastezo-agent. `open` starts
    // the app, or activates it when it runs
    if let Some(app) = exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|e| e == "app")) {
        super::spawn_and_reap(Command::new("/usr/bin/open").arg(app));
        return;
    }
    // a development build: the window's binary next to the agent
    match crate::hotkey::window_pid(dir) {
        Some(pid) => {
            if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) {
                #[allow(deprecated)] // the replacement (`activate`) is macOS 14+
                app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
            }
        }
        None => {
            if let Some(window) = exe.parent().map(|d| d.join("Pastezo")) {
                super::spawn_and_reap(&mut Command::new(window));
            }
        }
    }
}

/// Window side: tells the agent the saved shortcut changed. `Some(false)`:
/// the OS refused it; `None`: no agent running (it reads the file when it starts).
pub fn request_reload(dir: &Path) -> Option<bool> {
    crate::ipc::request(dir, crate::ipc::Request::Hotkey)
}
