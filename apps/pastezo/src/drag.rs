//! Dragging an image out of the window into another app or onto the desktop.
//!
//! The dragged thing is a file: a hard link to the stored original in a temp
//! folder, named like a macOS screenshot ("Pastezo 2026-09-28 21.53.07.png"),
//! so it lands with a readable name and no bytes are copied.
//!
//! | OS      | How                                                              |
//! |---------|------------------------------------------------------------------|
//! | macOS   | `NSDraggingSession` with the file URL, preview under the cursor   |
//! | Windows | the shell's own data object for the file + `SHDoDragDrop` (the    |
//! |         | shell draws the thumbnail); OLE is set up by winit for the window |
//! | Linux   | XDND source (see `linux`)                                         |

use std::path::{Path, PathBuf};

/// Folder for the files being dragged; emptied before every new drag.
fn drag_dir() -> PathBuf {
    std::env::temp_dir().join("pastezo-drag")
}

/// File name for a dragged image saved at `created_at` (Unix ms, local time).
pub fn file_name(created_at: i64) -> String {
    use chrono::TimeZone;
    let when = chrono::Local
        .timestamp_millis_opt(created_at)
        .single()
        .unwrap_or_else(chrono::Local::now);
    format!("Pastezo {}.png", when.format("%Y-%m-%d %H.%M.%S"))
}

/// A readable-named file with the original's content: a hard link when
/// possible (instant, no extra disk), a copy otherwise.
pub fn prepare(original: &Path, created_at: i64) -> std::io::Result<PathBuf> {
    let dir = drag_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(file_name(created_at));
    if std::fs::hard_link(original, &target).is_err() {
        std::fs::copy(original, &target)?;
    }
    Ok(target)
}

/// Starts a system drag of `file` from `window`, in the event being handled
/// right now (the mouse move of the gesture), showing `preview` under the
/// cursor at `size` (logical points) where the OS lets us. `false` if the OS
/// did not take the drag.
pub fn start(window: &slint::Window, file: &Path, preview: &Path, size: (f64, f64)) -> bool {
    platform::start(window, file, preview, size)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
        NSDraggingSource, NSImage,
    };
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "PastezoDragSource"]
        struct DragSource;

        unsafe impl NSObjectProtocol for DragSource {}

        unsafe impl NSDraggingSource for DragSource {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                // the receiver gets a copy; our stored original is never moved
                NSDragOperation::Copy
            }
        }
    );

    impl DragSource {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send![Self::alloc(mtm), init] }
        }
    }

    thread_local! {
        // AppKit does not retain the source for the whole session
        static SOURCE: RefCell<Option<Retained<DragSource>>> = const { RefCell::new(None) };
    }

    pub fn start(_window: &slint::Window, file: &Path, preview: &Path, (w, h): (f64, f64)) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let app = NSApplication::sharedApplication(mtm);
        // the mouse-dragged event that triggered the gesture
        let Some(event) = app.currentEvent() else { return false };
        let Some(window) = event.window(mtm).or_else(|| app.keyWindow()) else { return false };
        let Some(view) = window.contentView() else { return false };

        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
        let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&preview.to_string_lossy()));
        // the preview, centred under the cursor
        let at = view.convertPoint_fromView(event.locationInWindow(), None);
        let frame = NSRect::new(NSPoint::new(at.x - w / 2.0, at.y - h / 2.0), NSSize::new(w, h));
        let contents: Option<&AnyObject> = image.as_deref().map(|i| i.as_ref());
        unsafe { item.setDraggingFrame_contents(frame, contents) };

        let source = DragSource::new(mtm);
        let items = NSArray::from_retained_slice(&[item]);
        view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
        SOURCE.with(|s| *s.borrow_mut() = Some(source));
        true
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::path::Path;

    use windows::core::HSTRING;
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::DROPEFFECT_COPY;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop};

    /// Runs the whole drag (modal, like every OLE drag) and returns when the
    /// file is dropped or the drag is cancelled.
    pub fn start(_window: &slint::Window, file: &Path, _preview: &Path, _size: (f64, f64)) -> bool {
        let run = || -> windows::core::Result<()> {
            let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(file.as_os_str()), None)? };
            let data: IDataObject = unsafe { item.BindToHandler(None, &BHID_DataObject)? };
            let window = unsafe { GetActiveWindow() };
            unsafe { SHDoDragDrop(Some(window), &data, None, DROPEFFECT_COPY)? };
            Ok(())
        };
        match run() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("pastezo: cannot drag the image: {e}");
                false
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! On the window's X connection (the window runs on X11, XWayland under
    //! Wayland: see `select_backend`). Wayland itself lets a client start a drag
    //! only with the serial of its pointer press, which winit keeps to itself.
    use std::path::Path;

    use slint::winit_030::winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use slint::winit_030::{winit, WinitWindowAccessor};
    use x11rb::xcb_ffi::XCBConnection;

    /// Runs the whole drag (modal) and returns when it ends.
    pub fn start(window: &slint::Window, file: &Path, _preview: &Path, _size: (f64, f64)) -> bool {
        let handles = window
            .with_winit_window(|w: &winit::window::Window| Some((w.display_handle().ok()?.as_raw(), w.window_handle().ok()?.as_raw())))
            .flatten();
        let Some((RawDisplayHandle::Xlib(display), RawWindowHandle::Xlib(x_window))) = handles else { return false };
        let Some(display) = display.display else { return false };
        let Ok(xlib_xcb) = x11_dl::xlib_xcb::Xlib_xcb::open() else { return false };
        // SAFETY: the display is winit's, alive for as long as the window
        let raw = unsafe { (xlib_xcb.XGetXCBConnection)(display.as_ptr().cast()) };
        // SAFETY: a valid connection; not ours to close (should_drop = false)
        let Ok(conn) = (unsafe { XCBConnection::from_raw_xcb_connection(raw, false) }) else { return false };
        match crate::xdnd::run(&conn, x_window.window as u32, &crate::xdnd::uri_list(file)) {
            Ok(done) => done,
            Err(e) => {
                eprintln!("pastezo: cannot drag the image: {e}");
                false
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use std::path::Path;

    pub fn start(_window: &slint::Window, _file: &Path, _preview: &Path, _size: (f64, f64)) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn readable_name_like_a_screenshot() {
        let ms = chrono::Local.with_ymd_and_hms(2026, 9, 28, 21, 53, 7).unwrap().timestamp_millis();
        assert_eq!(file_name(ms), "Pastezo 2026-09-28 21.53.07.png");
    }

    #[test]
    fn prepared_file_has_the_original_content() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("abc.png");
        std::fs::write(&original, b"png bytes").unwrap();
        let first = prepare(&original, 0).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"png bytes");
        // the next drag replaces the previous file
        let ms = chrono::Local.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap().timestamp_millis();
        let second = prepare(&original, ms).unwrap();
        assert!(!first.exists() || first == second);
        assert!(second.ends_with("Pastezo 2026-01-02 03.04.05.png"));
    }
}
