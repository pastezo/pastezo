//! The global shortcut on Windows: `RegisterHotKey` on a message-only
//! window in its own thread (`WM_HOTKEY`; no work while idle). The Pastezo
//! window asks for a changed shortcut with a message to that window
//! (`request_reload`): its class is known, and the answer comes back as the
//! message's result.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, OnceLock};
use std::thread;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, RegisterHotKey, UnregisterHotKey, MAPVK_VSC_TO_VK, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
    MOD_WIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, FindWindowExW,
    GetMessageW, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, RegisterClassW,
    SendMessageTimeoutW, SendMessageW, SetForegroundWindow, ShowWindow, ASFW_ANY, GW_OWNER, HWND_MESSAGE, MSG,
    SMTO_ABORTIFHUNG, SW_RESTORE, WM_APP, WM_HOTKEY, WNDCLASSW,
};

use crate::hotkey::{mods, Hotkey};

const CLASS: &str = "PastezoAgentHotkey";
/// "register the saved shortcut"; the result: 1 done, 0 refused
const WM_RELOAD: u32 = WM_APP + 1;
const ID: i32 = 1;

static DIR: OnceLock<PathBuf> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Whether this OS lets Pastezo have a global shortcut.
pub fn available() -> bool {
    true
}

/// Registers the shortcut from the data folder's `hotkey` file.
pub struct Hotkeys {
    /// the message-only window (an `HWND`, kept as a number to be `Send`)
    window: usize,
}

impl Hotkeys {
    /// `dir`: the data folder. One per process.
    pub fn start(dir: PathBuf) -> Option<Hotkeys> {
        DIR.set(dir).ok()?;
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || unsafe {
                let class = wide(CLASS);
                let instance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
                let window = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                let _ = tx.send(window as usize);
                if window.is_null() {
                    return;
                }
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }
            })
            .ok()?;
        let window = rx.recv().ok()?;
        (window != 0).then_some(Hotkeys { window })
    }

    /// Registers the saved shortcut in place of the previous one.
    /// `false`: the OS refused it (another app has it).
    pub fn reload(&self) -> bool {
        // RegisterHotKey works only in the window's own thread
        unsafe { SendMessageW(self.window as HWND, WM_RELOAD, 0, 0) == 1 }
    }
}

unsafe extern "system" fn window_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            if let Some(dir) = DIR.get() {
                open_window(dir);
            }
            0
        }
        WM_RELOAD => set(window, DIR.get().and_then(|d| Hotkey::load(d)).as_ref()) as LRESULT,
        _ => DefWindowProcW(window, msg, wparam, lparam),
    }
}

fn set(window: HWND, hotkey: Option<&Hotkey>) -> bool {
    unsafe {
        UnregisterHotKey(window, ID);
        let Some(hotkey) = hotkey else { return true };
        // the key in that place in the current layout
        let vk = MapVirtualKeyW(hotkey.key().scan as u32, MAPVK_VSC_TO_VK);
        if vk == 0 {
            return false;
        }
        let modifiers = [(mods::CTRL, MOD_CONTROL), (mods::ALT, MOD_ALT), (mods::SHIFT, MOD_SHIFT), (mods::SUPER, MOD_WIN)]
            .into_iter()
            .filter(|(m, _)| hotkey.mods & m != 0)
            .fold(MOD_NOREPEAT, |all, (_, bit)| all | bit);
        RegisterHotKey(window, ID, modifiers, vk) != 0
    }
}

/// The Pastezo window: brought to the front if it is open, started if not.
/// Windows lets the process that got the shortcut take the foreground, and
/// hand that right to the window it starts.
fn open_window(dir: &Path) {
    if crate::hotkey::window_pid(dir).is_some_and(bring_forward) {
        return;
    }
    let Some(window) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("Pastezo.exe"))) else { return };
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    super::spawn_and_reap(&mut Command::new(window));
}

struct Search {
    pid: u32,
    /// the list window (titled "Pastezo"), else another of its windows (Settings)
    list: HWND,
    other: HWND,
}

unsafe extern "system" fn each_window(window: HWND, search: LPARAM) -> i32 {
    let search = &mut *(search as *mut Search);
    let mut pid = 0;
    GetWindowThreadProcessId(window, &mut pid);
    if pid != search.pid || IsWindowVisible(window) == 0 || !GetWindow(window, GW_OWNER).is_null() {
        return 1;
    }
    let mut title = [0u16; 16];
    let len = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
    if String::from_utf16_lossy(&title[..len.max(0) as usize]) == "Pastezo" {
        search.list = window;
        return 0; // found: stop
    }
    if search.other.is_null() {
        search.other = window;
    }
    1
}

fn bring_forward(pid: u32) -> bool {
    let mut search = Search { pid, list: std::ptr::null_mut(), other: std::ptr::null_mut() };
    unsafe {
        EnumWindows(Some(each_window), &mut search as *mut Search as LPARAM);
        let window = if search.list.is_null() { search.other } else { search.list };
        if window.is_null() {
            return false;
        }
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window);
    }
    true
}

/// Window side: tells the agent the saved shortcut changed. `Some(false)`:
/// the OS refused it; `None`: no agent running (it reads the file when it starts).
pub fn request_reload(_dir: &Path) -> Option<bool> {
    let class = wide(CLASS);
    unsafe {
        let window = FindWindowExW(HWND_MESSAGE, std::ptr::null_mut(), class.as_ptr(), std::ptr::null());
        if window.is_null() {
            return None;
        }
        let mut result = 0usize;
        let sent = SendMessageTimeoutW(window, WM_RELOAD, 0, 0, SMTO_ABORTIFHUNG, 3000, &mut result);
        (sent != 0).then_some(result == 1)
    }
}
