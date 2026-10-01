//! Platform-independent core of the clipboard manager: history storage,
//! clipboard watching, and per-platform clipboard backends.

mod code;
mod history;
pub mod hotkey;
#[cfg(unix)]
pub mod ipc;
mod model;
pub mod platform;
mod search;
mod store;
pub mod tags;
mod watcher;

pub use hotkey::{Hotkey, WindowLock};
pub use history::{data_dir, Deleted, History, MAX_CLIP_BYTES};
pub use model::{Clip, ClipContent, ClipKind};
pub use search::{Search, SearchKind};
pub use store::{CopyCount, CopyKind};
pub use watcher::{ClipboardBackend, Watcher};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    #[error("clipboard: {0}")]
    Clipboard(String),
    #[error("clip {0} has no content")]
    Missing(i64),
    #[error("{0}")]
    InvalidTag(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
