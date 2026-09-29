//! System clipboard backends, one per OS. Everything above this module
//! (`Watcher`, `History`, the app) is shared by all platforms.
//! Every backend is `Default`; the app creates it with `SystemClipboard::default()`.
//!
//! | OS      | Status  | How                                                                  |
//! |---------|---------|----------------------------------------------------------------------|
//! | macOS   | done    | `NSPasteboard.changeCount` polling every 250 ms (macOS has no change |
//! |         |         | notification; reading one integer), `NSWorkspace` for the source app |
//! | Windows | written, being tested | events: `WM_CLIPBOARDUPDATE` (no polling); text, `PNG`, `CF_DIB`; |
//! |         |         | skips `ExcludeClipboardContentFromMonitorProcessing` and friends;    |
//! |         |         | source app: foreground window → exe → its FileDescription            |
//! | Linux   | written, untested | `arboard` (X11, Wayland data-control, XWayland on GNOME);  |
//! |         |         | events: XFixes (X11/XWayland), Wayland data-control (ext / wlr),   |
//! |         |         | else (GNOME without XWayland) 1 s fingerprint polling;             |
//! |         |         | skips `x-kde-passwordManagerHint`;                                   |
//! |         |         | source app: X11 `_NET_ACTIVE_WINDOW` → `WM_CLASS`                    |
//! | Android | planned | Kotlin `ClipboardManager` via a Tauri plugin. Since Android 10 only  |
//! |         |         | the focused app may read the clipboard, so it is read when Pastezo   |
//! |         |         | comes to the foreground; skip `ClipDescription.EXTRA_IS_SENSITIVE`   |
//!
//! Until a backend exists, `Unsupported` keeps the app running: history is
//! shown, nothing new is captured.
//!
//! The global shortcut (`Hotkeys`, registered by the agent; see `crate::hotkey`):
//!
//! | OS      | How                                                                  |
//! |---------|----------------------------------------------------------------------|
//! | macOS   | Carbon `RegisterEventHotKey` (no Accessibility permission), loaded   |
//! |         | only once a shortcut is set; the window: `open Pastezo.app`          |
//! | Windows | `RegisterHotKey` on a message-only window (`WM_HOTKEY`); the window: |
//! |         | `SetForegroundWindow`, or started with `AllowSetForegroundWindow`    |
//! | Linux   | X11 `XGrabKey` on the root window; the window: `_NET_ACTIVE_WINDOW`. |
//! |         | Wayland lets no app listen to keys: none there (Settings says so)    |
//! | Android | none (no global shortcuts)                                           |

#[cfg(any(target_os = "windows", test))]
mod dib;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacClipboard as SystemClipboard;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsClipboard as SystemClipboard;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxClipboard as SystemClipboard;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::Unsupported as SystemClipboard;

#[cfg(target_os = "macos")]
#[path = "hotkeys_macos.rs"]
pub mod hotkeys;

#[cfg(target_os = "windows")]
#[path = "hotkeys_windows.rs"]
pub mod hotkeys;

#[cfg(target_os = "linux")]
#[path = "hotkeys_linux.rs"]
pub mod hotkeys;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub mod hotkeys {
    use std::path::{Path, PathBuf};

    pub fn available() -> bool {
        false
    }

    pub struct Hotkeys;

    impl Hotkeys {
        pub fn start(_dir: PathBuf) -> Option<Hotkeys> {
            None
        }

        pub fn reload(&self) -> bool {
            false
        }
    }

    pub fn request_reload(_dir: &Path) -> Option<bool> {
        None
    }
}

/// Starts `cmd` (the window, or `open` for it) and waits for it in a thread,
/// so no finished process is left behind as a zombie. While the last one
/// still runs, nothing new starts: a second press (or X11's key repeat)
/// before the window has taken its lock would open a second window.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn spawn_and_reap(cmd: &mut std::process::Command) {
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            let reaper = std::thread::Builder::new().name("window-reaper".into()).spawn(move || {
                let _ = child.wait();
                RUNNING.store(false, Ordering::SeqCst);
            });
            if reaper.is_err() {
                RUNNING.store(false, Ordering::SeqCst);
            }
        }
        Err(e) => {
            RUNNING.store(false, Ordering::SeqCst);
            eprintln!("pastezo-agent: cannot open the window: {e}");
        }
    }
}

/// Lowers the priority of the calling thread: clipboard work is never urgent
/// and must not compete with what the user is doing. On macOS the utility QoS
/// also lets the system coalesce the thread's timer wakeups with others.
pub(crate) fn background_priority() {
    #[cfg(target_vendor = "apple")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        // on Linux a thread id works as a "process" for setpriority
        libc::setpriority(libc::PRIO_PROCESS as _, libc::gettid() as _, 10);
    }
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Runs the calling (main) thread forever, doing whatever the OS needs there.
/// macOS: the main run loop, which keeps `NSWorkspace.frontmostApplication`
/// up to date in a process without a window, and delivers the global
/// shortcut (see `hotkeys`).
pub fn run_main_loop() -> ! {
    #[cfg(target_os = "macos")]
    hotkeys::run_main_loop();
    #[allow(unreachable_code)]
    loop {
        std::thread::park();
    }
}

/// Hands memory that was freed back to the OS. The allocator keeps freed
/// blocks for reuse; after decoding a big image that is tens of megabytes a
/// background process would carry around for nothing.
pub fn release_free_memory() {
    #[cfg(target_vendor = "apple")]
    unsafe {
        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
        }
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}
