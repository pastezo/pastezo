//! Pastezo background agent: watches the clipboard and saves every copy to the
//! history. No window, no UI framework — it has to stay tiny because it runs
//! all day. The Pastezo window is a separate process that reads the same
//! database and exits when it is closed.

#![cfg_attr(windows, windows_subsystem = "windows")] // no console window

use std::fs::{self, File};
use std::sync::Arc;

use clip_core::platform::{release_free_memory, run_main_loop, SystemClipboard};
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
    // Linux: copies made from the window go through the agent (see clip_core::ipc)
    #[cfg(target_os = "linux")]
    {
        let (history, watcher) = (history.clone(), watcher.clone());
        let served = clip_core::ipc::serve(&dir, move |id, part| {
            let content = history.content_of(id, part).ok().flatten();
            content.is_some_and(|c| watcher.write(&c).is_ok())
        });
        if let Err(e) = served {
            eprintln!("pastezo-agent: ipc: {e}");
        }
    }

    watcher.spawn(move |content, source_app| {
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
