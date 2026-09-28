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
/// up to date in a process without a window.
pub fn run_main_loop() -> ! {
    #[cfg(target_os = "macos")]
    {
        objc2_foundation::NSRunLoop::mainRunLoop().run();
    }
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
