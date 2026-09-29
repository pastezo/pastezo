//! Settings → General → "Hide Pastezo during screen sharing": the OS leaves
//! our windows out of screenshots, screen recordings and screen sharing.
//!
//! | OS      | How                                                                     |
//! |---------|-------------------------------------------------------------------------|
//! | macOS   | `NSWindow.sharingType = none` (apps capturing through ScreenCaptureKit  |
//! |         | on macOS 15+ may still show it: the OS gives no stronger way)            |
//! | Windows | `SetWindowDisplayAffinity`: `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+), |
//! |         | before that `WDA_MONITOR` (the window is black in the capture)           |
//! | Linux   | no API (X11 and Wayland let any capture see every window): not offered   |

/// Whether this OS can leave a window out of screen captures.
pub const AVAILABLE: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Leaves `window` out of screen captures (`hide`), or lets them see it again.
pub fn apply(window: &slint::Window, hide: bool) {
    use slint::winit_030::{winit, WinitWindowAccessor};
    window.with_winit_window(|w: &winit::window::Window| platform::apply(w, hide));
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSView, NSWindowSharingType};
    use slint::winit_030::winit;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn apply(w: &winit::window::Window, hide: bool) {
        let Ok(handle) = w.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        // the content view winit made for this window, alive as long as it is
        let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
        if let Some(window) = view.window() {
            window.setSharingType(if hide { NSWindowSharingType::None } else { NSWindowSharingType::ReadOnly });
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use slint::winit_030::winit;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE};

    pub fn apply(w: &winit::window::Window, hide: bool) {
        let Ok(handle) = w.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        let hwnd = HWND(h.hwnd.get() as *mut _);
        unsafe {
            if !hide {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_NONE);
            } else if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
                // before Windows 10 2004
                let _ = SetWindowDisplayAffinity(hwnd, WDA_MONITOR);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use slint::winit_030::winit;

    pub fn apply(_w: &winit::window::Window, _hide: bool) {}
}
