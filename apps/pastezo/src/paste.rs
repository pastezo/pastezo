//! Pasting a clip into the app the user came from (Return, ⌘1…⌘9): the clip
//! is already on the clipboard; Pastezo steps aside, the OS gives the focus
//! back to the previous app, and the paste shortcut is pressed there.
//!
//! | OS                     | Stepping aside                          | The paste keys                                        |
//! |------------------------|-----------------------------------------|-------------------------------------------------------|
//! | macOS                  | `NSApp.hide` (the previous app is back) | ⌘V through `CGEventPost`; needs Accessibility, asked  |
//! |                        |                                         | for on the first paste; without it: only copied       |
//! | Windows                | the window is minimized                 | Ctrl+V through `SendInput`                            |
//! | Linux, X11             | the window is minimized                 | Ctrl+V through XTest                                  |
//! | Linux, Wayland session | the window is minimized                 | none (XTest does not reach Wayland apps): only copied |

use std::time::Duration;

/// From stepping aside to the keys: the previous app has to be active by then.
const DELAY: Duration = Duration::from_millis(100);

/// Steps aside from `window` and pastes into the app that gets the focus.
pub fn into_previous_app(window: &slint::Window) {
    let keys = platform::can_press_keys();
    platform::step_aside(window);
    if keys {
        slint::Timer::single_shot(DELAY, platform::press_paste);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::Cell;
    use std::ffi::c_void;

    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state: i32) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    const COMBINED_SESSION_STATE: i32 = 0;
    const HID_EVENT_TAP: u32 = 0;
    const FLAG_COMMAND: u64 = 0x0010_0000;
    /// `kVK_ANSI_V`: the key of V (⌘ shortcuts on non-Latin layouts use it too)
    const KEY_V: u16 = 9;

    thread_local! {
        static ASKED: Cell<bool> = const { Cell::new(false) };
    }

    /// Accessibility lets an app press keys in other apps. Not granted yet:
    /// macOS asks for it (once per launch), and this paste only copies.
    pub fn can_press_keys() -> bool {
        if unsafe { AXIsProcessTrusted() } {
            return true;
        }
        if !ASKED.replace(true) {
            // CFString and CFBoolean are toll-free bridged with NSString, NSNumber
            let key: &NSString = unsafe { &*(kAXTrustedCheckOptionPrompt as *const NSString) };
            let options = NSDictionary::from_slices(&[key], &[&*NSNumber::new_bool(true)]);
            let options: *const NSDictionary<NSString, NSNumber> = &*options;
            unsafe { AXIsProcessTrustedWithOptions(options.cast()) };
        }
        false
    }

    pub fn step_aside(_window: &slint::Window) {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).hide(None);
        }
    }

    pub fn press_paste() {
        unsafe {
            let source = CGEventSourceCreate(COMBINED_SESSION_STATE);
            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(source, KEY_V, down);
                if event.is_null() {
                    continue;
                }
                CGEventSetFlags(event, FLAG_COMMAND);
                CGEventPost(HID_EVENT_TAP, event);
                CFRelease(event);
            }
            if !source.is_null() {
                CFRelease(source);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
    };

    pub fn can_press_keys() -> bool {
        true
    }

    /// A minimized window hands the focus to the one below it: where the user came from.
    pub fn step_aside(window: &slint::Window) {
        window.set_minimized(true);
    }

    pub fn press_paste() {
        let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        let none = KEYBD_EVENT_FLAGS(0);
        let inputs = [key(VK_CONTROL, none), key(VK_V, none), key(VK_V, KEYEVENTF_KEYUP), key(VK_CONTROL, KEYEVENTF_KEYUP)];
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use x11_dl::{xlib, xtest};

    const XK_V: u64 = 0x0076;
    const XK_CONTROL_L: u64 = 0xffe3;

    /// XTest reaches only X11 apps: in a Wayland session the clip is only copied.
    pub fn can_press_keys() -> bool {
        let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        set("DISPLAY") && !set("WAYLAND_DISPLAY")
    }

    /// A minimized window hands the focus to the one below it: where the user came from.
    pub fn step_aside(window: &slint::Window) {
        window.set_minimized(true);
    }

    /// On a connection of its own (libX11 and libXtst are loaded only now).
    pub fn press_paste() {
        let (Ok(x), Ok(xt)) = (xlib::Xlib::open(), xtest::Xf86vmode::open()) else {
            return eprintln!("pastezo: no XTest, cannot paste");
        };
        unsafe {
            let display = (x.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return;
            }
            let control = (x.XKeysymToKeycode)(display, XK_CONTROL_L) as u32;
            let v = (x.XKeysymToKeycode)(display, XK_V) as u32;
            if control != 0 && v != 0 {
                for (code, down) in [(control, 1), (v, 1), (v, 0), (control, 0)] {
                    (xt.XTestFakeKeyEvent)(display, code, down, 0);
                }
                (x.XFlush)(display);
            }
            (x.XCloseDisplay)(display);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub fn can_press_keys() -> bool {
        false
    }
    pub fn step_aside(window: &slint::Window) {
        window.set_minimized(true);
    }
    pub fn press_paste() {}
}
