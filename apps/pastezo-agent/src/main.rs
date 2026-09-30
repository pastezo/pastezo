//! Pastezo background agent: watches the clipboard and saves every copy to the
//! history. No window, no UI framework — it has to stay tiny because it runs
//! all day. The Pastezo window is a separate process that reads the same
//! database and exits when it is closed.

#![cfg_attr(windows, windows_subsystem = "windows")] // no console window

use std::fs::{self, File};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clip_core::platform::hotkeys::Hotkeys;
use clip_core::platform::{release_free_memory, run_main_loop, SystemClipboard};
#[cfg(unix)]
use clip_core::ipc::Request;
use clip_core::{data_dir, ClipContent, History, Watcher};

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("pastezo-agent: no data folder on this OS");
        return;
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("pastezo-agent: cannot create {}: {e}", dir.display());
        return;
    }

    // One agent per user: the second one exits right away. The OS releases
    // the lock when the process ends, even after a crash.
    let lock = match File::create(dir.join("agent.lock")) {
        Ok(f) => f,
        Err(e) => return eprintln!("pastezo-agent: lock file: {e}"),
    };
    if lock.try_lock().is_err() {
        return;
    }

    let history = match History::open(&dir) {
        Ok(h) => Arc::new(h),
        Err(e) => return eprintln!("pastezo-agent: cannot open the history: {e}"),
    };

    // a unit struct on some platforms, stateful on others (Linux)
    #[allow(clippy::default_constructed_unit_structs)]
    let watcher = Watcher::new(Arc::new(SystemClipboard::default()));
    // the global shortcut that opens the window (Settings → General; none by default)
    let hotkeys = Hotkeys::start(dir.clone()).map(Arc::new);
    if let Some(h) = &hotkeys {
        h.reload();
    }
    // window → agent requests (see clip_core::ipc): Linux copies, a changed shortcut
    #[cfg(unix)]
    {
        #[cfg(target_os = "linux")]
        let (history, watcher) = (history.clone(), watcher.clone());
        let served = clip_core::ipc::serve(&dir, move |request| match request {
            #[cfg(target_os = "linux")]
            Request::Copy(id, part) => {
                let content = history.content_of(id, part).ok().flatten();
                content.is_some_and(|c| watcher.write(&c).is_ok())
            }
            #[cfg(not(target_os = "linux"))]
            Request::Copy(..) => false,
            Request::Hotkey => hotkeys.as_ref().is_some_and(|h| h.reload()),
        });
        if let Err(e) = served {
            eprintln!("pastezo-agent: ipc: {e}");
        }
    }
    // Windows: the window messages the shortcut's own window
    #[cfg(not(unix))]
    let _hotkeys = hotkeys;

    // Settings → General → Keep clips: old clips go at start, every hour,
    // and before each copy (a copy of an old clip then comes back as new)
    forget_old(&history);
    std::thread::spawn({
        let history = history.clone();
        move || loop {
            std::thread::sleep(Duration::from_secs(60 * 60));
            forget_old(&history);
        }
    });

    watcher.spawn(move |content, source_app| {
        forget_old(&history);
        if let Err(e) = history.add(&content, source_app.as_deref()) {
            eprintln!("pastezo-agent: failed to save a clip: {e}");
        }
        if matches!(content, ClipContent::Image(_)) {
            drop(content);
            release_free_memory();
        }
    });

    let _lock = lock; // held for the life of the process
    run_main_loop();
}

fn forget_old(history: &History) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    if let Err(e) = history.forget_old(now) {
        eprintln!("pastezo-agent: failed to forget old clips: {e}");
    }
}
