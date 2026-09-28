use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use clipboard_win::formats::{RawData, Unicode, CF_DIB, CF_UNICODETEXT};
use clipboard_win::options::NoClear;
use clipboard_win::{empty, get, is_format_avail, raw, register_format, seq_num, Clipboard, Monitor};
use super::dib::{dib_to_png, png_to_dib};
use crate::model::ClipContent;
use crate::watcher::ClipboardBackend;
use crate::{Error, Result};

/// Another app may hold the clipboard open for a moment; retry this many times.
const OPEN_ATTEMPTS: usize = 10;

/// Formats that password managers and other apps set on content that must
/// not end up in clipboard history.
/// https://learn.microsoft.com/windows/win32/dataxchg/clipboard-formats#cloud-clipboard-and-clipboard-history-formats
const PRIVATE_FORMATS: &[&str] = &[
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
];

/// Clipboard backend on top of the Win32 clipboard API.
///
/// Changes come from `WM_CLIPBOARDUPDATE` (`AddClipboardFormatListener`) on a
/// hidden message-only window in its own thread, so an idle clipboard costs
/// no CPU. If the listener cannot be created, it falls back to polling the
/// sequence number.
pub struct WindowsClipboard {
    updates: Option<Mutex<Receiver<()>>>,
}

impl Default for WindowsClipboard {
    fn default() -> Self {
        WindowsClipboard { updates: listen() }
    }
}

/// Starts the `WM_CLIPBOARDUPDATE` listener thread; `None` if it fails.
fn listen() -> Option<Mutex<Receiver<()>>> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("clipboard-listener".into())
        .spawn(move || {
            super::background_priority();
            // the window belongs to the thread that creates it
            let mut monitor = match Monitor::new() {
                Ok(m) => m,
                Err(_) => {
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let _ = ready_tx.send(true);
            while let Ok(true) = monitor.recv() {
                if tx.send(()).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    ready_rx.recv().ok()?.then(|| Mutex::new(rx))
}

impl ClipboardBackend for WindowsClipboard {
    fn change_count(&self) -> i64 {
        seq_num().map_or(0, |n| n.get() as i64)
    }

    fn read(&self) -> Option<ClipContent> {
        let _open = Clipboard::new_attempts(OPEN_ATTEMPTS).ok()?;
        if is_private() {
            return None;
        }
        if is_format_avail(CF_UNICODETEXT) {
            if let Ok(text) = get::<String, _>(Unicode) {
                return Some(ClipContent::Text(text));
            }
        }
        // Browsers, Office and most modern apps also put a PNG (keeps transparency).
        if let Some(png) = register_format("PNG").filter(|f| is_format_avail(f.get())) {
            if let Ok(bytes) = get::<Vec<u8>, _>(RawData(png.get())) {
                if bytes.starts_with(b"\x89PNG") {
                    return Some(ClipContent::Image(bytes));
                }
            }
        }
        // Windows synthesizes CF_DIB from any bitmap (screenshots, Paint, …).
        if is_format_avail(CF_DIB) {
            let dib = get::<Vec<u8>, _>(RawData(CF_DIB)).ok()?;
            return dib_to_png(&dib).map(ClipContent::Image);
        }
        None
    }

    fn write(&self, content: &ClipContent) -> Result<i64> {
        {
            let _open = Clipboard::new_attempts(OPEN_ATTEMPTS).map_err(win_err)?;
            empty().map_err(win_err)?;
            match content {
                ClipContent::Text(s) => raw::set_string_with(s, NoClear).map_err(win_err)?,
                ClipContent::Image(png) => {
                    // PNG for apps that understand it, DIB for everything else.
                    if let Some(format) = register_format("PNG") {
                        raw::set_without_clear(format.get(), png).map_err(win_err)?;
                    }
                    raw::set_without_clear(CF_DIB, &png_to_dib(png)?).map_err(win_err)?;
                }
            }
        } // clipboard is closed here; the sequence number is final
        Ok(self.change_count())
    }

    fn wait_for_change(&self, timeout: Duration) {
        match &self.updates {
            Some(rx) => {
                let rx = rx.lock().unwrap();
                if rx.recv_timeout(timeout).is_ok() {
                    // one check covers a burst of updates (apps often set several formats)
                    while rx.try_recv().is_ok() {}
                }
            }
            None => thread::sleep(timeout),
        }
    }

    fn check_interval(&self) -> Duration {
        match self.updates {
            Some(_) => Duration::from_secs(30), // safety net only
            None => Duration::from_millis(250),
        }
    }

    fn frontmost_app(&self) -> Option<String> {
        let exe = foreground_exe()?;
        file_description(&exe).or_else(|| {
            Path::new(&exe)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
    }
}

fn win_err(e: clipboard_win::ErrorCode) -> Error {
    Error::Clipboard(e.to_string())
}

fn is_private() -> bool {
    if PRIVATE_FORMATS
        .iter()
        .filter_map(|name| register_format(name))
        .any(|f| is_format_avail(f.get()))
    {
        return true;
    }
    // "CanIncludeInClipboardHistory" = DWORD 0 means "keep out of history"
    register_format("CanIncludeInClipboardHistory")
        .filter(|f| is_format_avail(f.get()))
        .and_then(|f| get::<Vec<u8>, _>(RawData(f.get())).ok())
        .is_some_and(|v| v.len() >= 4 && v[..4] == [0, 0, 0, 0])
}

/// Full path of the executable that owns the foreground window.
fn foreground_exe() -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Human-readable app name from the executable's version info
/// ("Google Chrome" rather than "chrome").
fn file_description(exe: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let path = wide(exe);
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, block.as_mut_ptr().cast()) == 0 {
            return None;
        }
        // first language/codepage pair the file declares
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let key = wide("\\VarFileInfo\\Translation");
        if VerQueryValueW(block.as_ptr().cast(), key.as_ptr(), &mut ptr, &mut len) == 0 || len < 4 {
            return None;
        }
        let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
        let key = wide(&format!(
            "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
            pair[0], pair[1]
        ));
        if VerQueryValueW(block.as_ptr().cast(), key.as_ptr(), &mut ptr, &mut len) == 0 || len == 0 {
            return None;
        }
        let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let name = String::from_utf16_lossy(text).trim_end_matches('\0').trim().to_string();
        (!name.is_empty()).then_some(name)
    }
}
