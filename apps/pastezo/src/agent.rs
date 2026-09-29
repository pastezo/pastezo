//! The background agent (`pastezo-agent`) records the clipboard; this window
//! only shows the history. The window makes sure the agent runs; a second
//! agent exits by itself (lock file), so starting it again is harmless.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Next to the window's executable; in the macOS app `Contents/Helpers/`
/// (see `scripts/bundle-macos.sh`).
fn agent_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) { "pastezo-agent.exe" } else { "pastezo-agent" };
    let dir = exe.parent()?;
    let helpers = dir.parent().map(|contents| contents.join("Helpers").join(name)).filter(|_| cfg!(target_os = "macos"));
    [helpers, Some(dir.join(name))].into_iter().flatten().find(|p| p.exists())
}

pub fn ensure_running() {
    let Some(path) = agent_path() else {
        eprintln!("pastezo: pastezo-agent not found next to the app");
        return;
    };
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("pastezo: cannot start pastezo-agent: {e}");
    }
}

/// Development builds (cargo `target/` folder) are never registered to start
/// at login — only an installed app is.
fn is_installed(path: &std::path::Path) -> bool {
    !path.components().any(|c| c.as_os_str() == "target")
}

/// Settings → General → "Open Pastezo at login": the agent starts at login
/// (`true`) or not. Re-run on every launch: the entry follows the app if it
/// was moved. Errors are only logged — recording still works for this session
/// because `ensure_running` started the agent; turning it off leaves the
/// running agent alone (it stops at logout and does not come back).
pub fn set_autostart(enabled: bool) {
    let Some(path) = agent_path() else { return };
    if !is_installed(&path) {
        return;
    }
    let result = if enabled { autostart::register(&path) } else { autostart::unregister() };
    if let Err(e) = result {
        eprintln!("pastezo: cannot change start at login: {e}");
    }
}

#[cfg(target_os = "macos")]
mod autostart {
    use std::path::Path;

    const LABEL: &str = "app.pastezo.agent";

    pub fn register(agent: &Path) -> std::io::Result<()> {
        let dir = dirs_home()?.join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir)?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <!-- restart after a crash, not after a normal exit (a second agent exits 0) -->
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <!-- launchd keeps it at background priority -->
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
</dict>
</plist>
"#,
            xml_escape(&agent.to_string_lossy())
        );
        let file = dir.join(format!("{LABEL}.plist"));
        if std::fs::read_to_string(&file).ok().as_deref() != Some(plist.as_str()) {
            std::fs::write(&file, plist)?;
        }
        Ok(())
    }

    pub fn unregister() -> std::io::Result<()> {
        let file = dirs_home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist"));
        super::remove_if_there(&file)
    }

    fn dirs_home() -> std::io::Result<std::path::PathBuf> {
        std::env::var_os("HOME").map(Into::into).ok_or_else(|| std::io::Error::other("no HOME"))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }
}

#[cfg(target_os = "windows")]
mod autostart {
    use std::path::Path;

    use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Pastezo Agent";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn register(agent: &Path) -> std::io::Result<()> {
        let key = wide(RUN);
        let name = wide(NAME);
        let value = wide(&format!("\"{}\"", agent.display()));
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(rc as i32))
        }
    }

    pub fn unregister() -> std::io::Result<()> {
        let (key, name) = (wide(RUN), wide(NAME));
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        // ERROR_FILE_NOT_FOUND: it was not there
        if rc == 0 || rc == 2 {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(rc as i32))
        }
    }
}

#[cfg(target_os = "linux")]
mod autostart {
    use std::path::Path;

    fn dir() -> std::io::Result<std::path::PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .ok_or_else(|| std::io::Error::other("no config folder"))?;
        Ok(config.join("autostart"))
    }

    /// XDG autostart, honoured by GNOME, KDE, Xfce and others.
    pub fn register(agent: &Path) -> std::io::Result<()> {
        let dir = dir()?;
        std::fs::create_dir_all(&dir)?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Pastezo Agent\nExec=\"{}\"\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            agent.display()
        );
        std::fs::write(dir.join("pastezo-agent.desktop"), entry)
    }

    pub fn unregister() -> std::io::Result<()> {
        super::remove_if_there(&dir()?.join("pastezo-agent.desktop"))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod autostart {
    pub fn register(_agent: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }

    pub fn unregister() -> std::io::Result<()> {
        Ok(())
    }
}

/// Deletes `file`; already gone is fine.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_if_there(file: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dev_builds_are_not_registered() {
        assert!(!super::is_installed(std::path::Path::new("/Users/me/pastezo/target/release/pastezo-agent")));
        assert!(super::is_installed(std::path::Path::new("/Applications/Pastezo.app/Contents/Helpers/pastezo-agent")));
    }
}
