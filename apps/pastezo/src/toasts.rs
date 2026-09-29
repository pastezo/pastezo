//! In-app notifications ("toasts"): a pill at the top centre of the window.
//! One at a time, in turn: a new one waits until the current one has gone
//! (except `show_action` — it replaces the current one at once). A button
//! after the text runs the toast's own action (`clicked`).
//! Text always comes from the translations (`toast.*` keys).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};

use crate::{AppWindow, SettingsWindow, Toast, ToastKind};

/// How long each kind stays on screen: errors need more time to be read.
fn duration(kind: ToastKind) -> Duration {
    Duration::from_millis(match kind {
        ToastKind::Success | ToastKind::Info => 2000,
        ToastKind::Warning => 3500,
        ToastKind::Error => 4000,
    })
}

/// The toast's slide-out (`animate y` of `ToastView` in `ui/app.slint`) and
/// a little pause before the next one comes in.
const SLIDE_OUT: Duration = Duration::from_millis(260);

/// A window with a toast: the list window, and the settings window
/// (Import & Export reports there).
pub trait ToastHost: ComponentHandle + 'static {
    fn set_toast(&self, toast: Toast);
    fn set_toast_shown(&self, shown: bool);
}

impl ToastHost for AppWindow {
    fn set_toast(&self, toast: Toast) {
        AppWindow::set_toast(self, toast)
    }
    fn set_toast_shown(&self, shown: bool) {
        AppWindow::set_toast_shown(self, shown)
    }
}

impl ToastHost for SettingsWindow {
    fn set_toast(&self, toast: Toast) {
        SettingsWindow::set_toast(self, toast)
    }
    fn set_toast_shown(&self, shown: bool) {
        SettingsWindow::set_toast_shown(self, shown)
    }
}

pub struct Toaster<W: ToastHost = AppWindow>(Rc<Queue<W>>);

/// What a toast's button does.
pub type Action = Rc<dyn Fn()>;

/// A toast, how long it stays, what its button does.
type Entry = (Toast, Duration, Option<Action>);

struct Queue<W: ToastHost> {
    ui: slint::Weak<W>,
    /// Hides the toast on screen when its time is up.
    hide: Timer,
    /// Shows the next one once the previous has slid away.
    next: Timer,
    /// On screen now; `None` while hidden or sliding away.
    current: RefCell<Option<Toast>>,
    /// the button of the one on screen
    action: RefCell<Option<Action>>,
    waiting: RefCell<VecDeque<Entry>>,
}

impl<W: ToastHost> Toaster<W> {
    pub fn new(ui: &W) -> Self {
        Toaster(Rc::new(Queue {
            ui: ui.as_weak(),
            hide: Timer::default(),
            next: Timer::default(),
            current: RefCell::new(None),
            action: RefCell::new(None),
            waiting: RefCell::new(VecDeque::new()),
        }))
    }

    /// Shown after the ones already waiting.
    pub fn show(&self, kind: ToastKind, text: impl Into<slint::SharedString>) {
        let toast = Toast { kind, text: text.into(), action: Default::default() };
        self.enqueue((toast, duration(kind), None));
    }

    /// With a button after the text (its label: `action`) that runs `clicked`,
    /// shown for `time`: long enough to press it. Shown at once, in place of
    /// the current one — for a button that is of no use later (Undo).
    pub fn show_action(&self, kind: ToastKind, text: impl Into<slint::SharedString>, action: impl Into<slint::SharedString>, time: Duration, clicked: Action) {
        Queue::display(&self.0, (Toast { kind, text: text.into(), action: action.into() }, time, Some(clicked)));
    }

    /// With a button, like `show_action`, but in turn after the ones on screen
    /// and waiting: for a button that is still of use later.
    pub fn queue_action(&self, kind: ToastKind, text: impl Into<slint::SharedString>, action: impl Into<slint::SharedString>, time: Duration, clicked: Action) {
        self.enqueue((Toast { kind, text: text.into(), action: action.into() }, time, Some(clicked)));
    }

    /// The button of the toast on screen was pressed.
    pub fn clicked(&self) {
        let action = self.0.action.borrow().clone();
        if let Some(action) = action {
            action();
        }
    }

    fn enqueue(&self, entry: Entry) {
        let q = &self.0;
        let toast = &entry.0;
        if q.current.borrow().as_ref() == Some(toast) {
            // the same one again ("Copied" twice) slides away and comes back:
            // the second copy is seen too
            q.waiting.borrow_mut().push_front(entry);
            return Queue::leave(q);
        }
        if q.current.borrow().is_none() && !q.next.running() {
            return Queue::display(q, entry);
        }
        let mut waiting = q.waiting.borrow_mut();
        // not twice in a row; nor again while it slides away to come back
        let coming_back = q.current.borrow().is_none() && waiting.front().is_some_and(|(first, ..)| first == toast);
        if !coming_back && waiting.back().is_none_or(|(last, ..)| last != toast) {
            waiting.push_back(entry);
        }
    }

    /// Hides the current one; the next waiting follows.
    pub fn hide(&self) {
        if self.0.current.borrow().is_some() {
            Queue::leave(&self.0);
        }
    }
}

impl<W: ToastHost> Queue<W> {
    fn display(q: &Rc<Self>, (toast, time, action): Entry) {
        q.next.stop();
        let Some(ui) = q.ui.upgrade() else { return };
        ui.set_toast(toast.clone());
        ui.set_toast_shown(true);
        *q.current.borrow_mut() = Some(toast);
        *q.action.borrow_mut() = action;
        let weak = Rc::downgrade(q);
        q.hide.start(TimerMode::SingleShot, time, move || {
            if let Some(q) = weak.upgrade() {
                Queue::leave(&q);
            }
        });
    }

    fn leave(q: &Rc<Self>) {
        q.hide.stop();
        q.current.borrow_mut().take();
        q.action.borrow_mut().take();
        if let Some(ui) = q.ui.upgrade() {
            ui.set_toast_shown(false);
        }
        if q.waiting.borrow().is_empty() {
            return;
        }
        let weak = Rc::downgrade(q);
        q.next.start(TimerMode::SingleShot, SLIDE_OUT, move || {
            let Some(q) = weak.upgrade() else { return };
            let next = q.waiting.borrow_mut().pop_front();
            if let Some(entry) = next {
                Queue::display(&q, entry);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing as testing;

    #[test]
    fn toasts_follow_one_another() {
        testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let toaster = Toaster::new(&ui);
        let on_screen = || ui.get_toast_shown().then(|| ui.get_toast().text.to_string());
        let wait = |ms| testing::mock_elapsed_time(Duration::from_millis(ms));

        toaster.show(ToastKind::Success, "Copied");
        toaster.show(ToastKind::Error, "Export failed");
        toaster.show(ToastKind::Error, "Export failed"); // waits once, not twice
        assert_eq!(on_screen().as_deref(), Some("Copied"));
        wait(1500);
        toaster.show(ToastKind::Success, "Copied"); // the same again: slides away and comes back
        assert_eq!(on_screen(), None, "slides away first");
        wait(SLIDE_OUT.as_millis() as u64);
        assert_eq!(on_screen().as_deref(), Some("Copied"));
        toaster.show(ToastKind::Success, "Copied");
        toaster.show(ToastKind::Success, "Copied"); // while sliding away: comes back once
        wait(SLIDE_OUT.as_millis() as u64);
        assert_eq!(on_screen().as_deref(), Some("Copied"));
        wait(2000);
        assert_eq!(on_screen(), None);
        wait(SLIDE_OUT.as_millis() as u64);
        assert_eq!(on_screen().as_deref(), Some("Export failed"));
        wait(4000 + SLIDE_OUT.as_millis() as u64 + 100);
        assert_eq!(on_screen(), None, "nothing more waiting");

        // a toast with a button comes at once, the waiting ones after it
        let pressed = Rc::new(std::cell::Cell::new(""));
        let press = |name: &'static str| -> Action {
            let pressed = pressed.clone();
            Rc::new(move || pressed.set(name))
        };
        toaster.show(ToastKind::Success, "Copied");
        toaster.queue_action(ToastKind::Info, "Update", "Download", Duration::from_secs(8), press("download"));
        toaster.show(ToastKind::Info, "Imported");
        toaster.show_action(ToastKind::Info, "Deleted", "Undo", Duration::from_secs(5), press("undo"));
        assert_eq!(on_screen().as_deref(), Some("Deleted"));
        toaster.clicked();
        assert_eq!(pressed.get(), "undo");
        toaster.hide();
        toaster.clicked();
        assert_eq!(pressed.replace(""), "undo", "nothing on screen: no button to press");
        // a queued button waits its turn
        wait(SLIDE_OUT.as_millis() as u64);
        assert_eq!(on_screen().as_deref(), Some("Update"));
        toaster.clicked();
        assert_eq!(pressed.get(), "download");
        wait(8000);
        assert_eq!(on_screen(), None);
        wait(SLIDE_OUT.as_millis() as u64);
        assert_eq!(on_screen().as_deref(), Some("Imported"));
        toaster.clicked();
        assert_eq!(pressed.get(), "download", "a toast without a button");
    }
}
