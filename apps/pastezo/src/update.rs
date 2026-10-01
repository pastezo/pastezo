//! "A new version is out": once a day, when the window opens, the newest
//! release on GitHub is compared with this build; a newer one is offered in
//! a toast with a "Download" button (the release page). Nothing is installed.
//!
//! The request goes through the system (`net.rs`). No answer, no network:
//! nothing is shown, the next try is a day later.

use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

use crate::settings::Settings;
use crate::{toasts, App, ToastKind};

/// Where "Download" leads.
pub const RELEASES: &str = "https://github.com/pastezo/pastezo/releases/latest";
const API: &str = "https://api.github.com/repos/pastezo/pastezo/releases/latest";
const ACCEPT: &str = "application/vnd.github+json";
/// Between two checks.
const EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// The toast stays long enough to read it and press "Download".
const TOAST_TIME: Duration = Duration::from_secs(8);

impl App {
    /// When the window opens: once a day, a look for a newer version.
    pub(crate) fn check_for_update(self: &Rc<Self>) {
        let now = chrono::Local::now().timestamp_millis();
        if !due(self.settings().update_checked, now) {
            return;
        }
        // counted as done now: no answer (offline) waits a day too
        self.save_settings(Settings { update_checked: Some(now), ..self.settings() });
        let app = Rc::downgrade(self);
        let _ = slint::spawn_local(async move {
            let Some(version) = latest().await else { return };
            if let Some(app) = app.upgrade().filter(|_| newer(&version, env!("CARGO_PKG_VERSION"))) {
                app.offer_update(&version);
            }
        });
    }

    /// "Pastezo 0.2.0 is available · Download", after the toasts before it.
    pub(crate) fn offer_update(self: &Rc<Self>, version: &str) {
        let download: toasts::Action = Rc::new({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    (app.open_url)(RELEASES);
                    app.toaster.hide();
                }
            }
        });
        let text = self.i18n.t("toast.update", &[("version", version)]);
        self.toaster.queue_action(ToastKind::Info, text, self.i18n.t("toast.download", &[]), TOAST_TIME, download);
    }
}

/// Time for a check: never checked, a day has passed, or the clock went back.
pub fn due(last_ms: Option<i64>, now_ms: i64) -> bool {
    last_ms.is_none_or(|last| now_ms - last >= EVERY_MS || now_ms < last)
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// `latest` is a later version than `current` ("0.2.0" > "0.1.10").
pub fn newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// The lookup's answer once it is there, and who waits for it.
type Answer = Arc<Mutex<(Option<Option<String>>, Option<Waker>)>>;

/// The newest release's version ("0.2.0"), looked up on another thread.
pub fn latest() -> impl Future<Output = Option<String>> {
    let shared: Answer = Arc::default();
    let result = shared.clone();
    std::thread::spawn(move || {
        let version = fetch();
        let mut r = result.lock().unwrap();
        r.0 = Some(version);
        if let Some(waker) = r.1.take() {
            waker.wake();
        }
    });
    std::future::poll_fn(move |cx| {
        let mut r = shared.lock().unwrap();
        match r.0.take() {
            Some(version) => Poll::Ready(version),
            None => {
                r.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
}

/// The version in the newest release's `tag_name`.
fn fetch() -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(&crate::net::get(API, ACCEPT, 1024 * 1024).ok()?).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    parse(tag)?;
    Some(tag.trim_start_matches('v').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(newer("0.2.0", "0.1.2"));
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.2", "0.1.2"));
        assert!(!newer("0.1.1", "0.1.2"));
        assert!(!newer("nightly", "0.1.2"));
        assert!(!newer("0.2", "0.1.2"));
    }

    #[test]
    fn once_a_day() {
        let day = EVERY_MS;
        assert!(due(None, 5));
        assert!(!due(Some(1000), 1000 + day - 1));
        assert!(due(Some(1000), 1000 + day));
        assert!(due(Some(1000), 10), "the clock went back");
    }
}
