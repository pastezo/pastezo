use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::model::ClipContent;
use crate::Result;

/// Access to the system clipboard. One implementation per platform.
pub trait ClipboardBackend: Send + Sync + 'static {
    /// A counter that changes every time the clipboard content changes.
    fn change_count(&self) -> i64;
    /// Current content, or `None` if it is empty, unsupported, or marked
    /// as private (e.g. passwords from a password manager).
    fn read(&self) -> Option<ClipContent>;
    /// Replaces the clipboard content. Returns the new change count.
    fn write(&self, content: &ClipContent) -> Result<i64>;
    /// Name of the app the user is working in right now.
    fn frontmost_app(&self) -> Option<String>;

    /// Blocks until the clipboard may have changed, or at most `timeout`.
    /// Backends with OS change notifications return as soon as one arrives
    /// and cost nothing while idle; the default just sleeps (polling).
    fn wait_for_change(&self, timeout: Duration) {
        thread::sleep(timeout);
    }

    /// Longest time between two checks: short for polling backends, a long
    /// safety net for event-driven ones.
    fn check_interval(&self) -> Duration {
        Duration::from_millis(250)
    }
}

/// Polls the clipboard in a background thread and reports new content.
pub struct Watcher {
    backend: Arc<dyn ClipboardBackend>,
    last_seen: AtomicI64,
    /// Keeps `poll` and `write` from interleaving, so our own write is
    /// never picked up as a new copy.
    io: Mutex<()>,
}

impl Watcher {
    pub fn new(backend: Arc<dyn ClipboardBackend>) -> Arc<Self> {
        let last_seen = AtomicI64::new(backend.change_count());
        Arc::new(Watcher {
            backend,
            last_seen,
            io: Mutex::new(()),
        })
    }

    /// Starts watching. `on_change` gets the new content and the source app.
    pub fn spawn<F>(self: &Arc<Self>, mut on_change: F)
    where
        F: FnMut(ClipContent, Option<String>) + Send + 'static,
    {
        let this = Arc::clone(self);
        thread::Builder::new()
            .name("clipboard-watcher".into())
            .spawn(move || {
                crate::platform::background_priority();
                loop {
                    if let Some((content, app)) = this.poll() {
                        on_change(content, app);
                    }
                    this.backend.wait_for_change(this.backend.check_interval());
                }
            })
            .expect("spawn clipboard watcher");
    }

    /// Checks the clipboard once.
    pub fn poll(&self) -> Option<(ClipContent, Option<String>)> {
        let _io = self.io.lock().unwrap();
        let count = self.backend.change_count();
        if self.last_seen.swap(count, Ordering::SeqCst) == count {
            return None;
        }
        let content = self.backend.read()?;
        Some((content, self.backend.frontmost_app()))
    }

    /// Writes to the clipboard without reporting it back as new content.
    pub fn write(&self, content: &ClipContent) -> Result<()> {
        let _io = self.io.lock().unwrap();
        let count = self.backend.write(content)?;
        self.last_seen.store(count, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        count: AtomicI64,
        content: Mutex<Option<ClipContent>>,
    }

    impl Fake {
        fn copy(&self, s: &str) {
            *self.content.lock().unwrap() = Some(ClipContent::Text(s.into()));
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ClipboardBackend for Fake {
        fn change_count(&self) -> i64 {
            self.count.load(Ordering::SeqCst)
        }
        fn read(&self) -> Option<ClipContent> {
            self.content.lock().unwrap().clone()
        }
        fn write(&self, content: &ClipContent) -> Result<i64> {
            *self.content.lock().unwrap() = Some(content.clone());
            Ok(self.count.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn frontmost_app(&self) -> Option<String> {
            Some("Safari".into())
        }
    }

    #[test]
    fn reports_changes_once() {
        let fake = Arc::new(Fake::default());
        let w = Watcher::new(fake.clone());
        assert!(w.poll().is_none());
        fake.copy("hello");
        let (c, app) = w.poll().unwrap();
        assert_eq!(c, ClipContent::Text("hello".into()));
        assert_eq!(app.as_deref(), Some("Safari"));
        assert!(w.poll().is_none());
    }

    #[test]
    fn own_writes_are_ignored() {
        let fake = Arc::new(Fake::default());
        let w = Watcher::new(fake.clone());
        w.write(&ClipContent::Text("mine".into())).unwrap();
        assert!(w.poll().is_none());
        assert_eq!(fake.read(), Some(ClipContent::Text("mine".into())));
    }
}
