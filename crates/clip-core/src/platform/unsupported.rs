use crate::model::ClipContent;
use crate::watcher::ClipboardBackend;
use crate::{Error, Result};

/// Placeholder for platforms without a clipboard backend yet (see `mod.rs`).
/// Reports "no changes" so the watcher idles instead of crashing the app.
#[derive(Default)]
pub struct Unsupported;

impl ClipboardBackend for Unsupported {
    fn change_count(&self) -> i64 {
        0
    }

    fn read(&self) -> Option<ClipContent> {
        None
    }

    fn write(&self, _content: &ClipContent) -> Result<i64> {
        Err(Error::Clipboard(format!(
            "clipboard is not supported on {} yet",
            std::env::consts::OS
        )))
    }

    fn frontmost_app(&self) -> Option<String> {
        None
    }
}
