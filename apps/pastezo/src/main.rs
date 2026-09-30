//! Pastezo window: shows the clipboard history that the background agent
//! (`pastezo-agent`) records. Closing the window ends this process — the UI
//! costs nothing while it is not open.

#![cfg_attr(windows, windows_subsystem = "windows")] // no console window

mod agent;
mod app_icon;
mod backup;
mod capture;
mod drag;
mod hotkey;
mod i18n;
mod messages;
mod paste;
mod query;
mod settings;
mod snapshot;
mod stats;
mod system;
mod themes;
mod thumbs;
#[cfg(target_os = "linux")]
mod xdnd;
mod toasts;
mod update;

slint::include_modules!();

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use clip_core::platform::SystemClipboard;
use clip_core::{Clip, ClipboardBackend, History};
#[cfg(test)]
use clip_core::ClipContent;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::i18n::I18n;
use crate::messages::Run;
use crate::settings::{Settings, TextStyle, WindowFrame};
use crate::toasts::Toaster;

/// Rows per page (loaded as the list scrolls).
const PAGE: u32 = 50;
/// Search results shown at most.
const SEARCH_LIMIT: u32 = 200;
/// Delete animation (fade + collapse, see ClipRow) before the row leaves the model.
const REMOVE_ANIMATION: Duration = Duration::from_millis(280);
/// While the window is open: how often to look for clips saved by the agent.
/// One `PRAGMA data_version` (no disk access unless something changed).
const REFRESH: Duration = Duration::from_millis(400);
/// Characters of a text shown in the preview: every line of any usual clip,
/// without laying out megabytes (a log pasted by accident) in one Text.
const PREVIEW_CHARS: usize = 200_000;
/// How long a deleted clip can be brought back ("Undo" in the toast, ⌘Z).
const UNDO_TIME: Duration = Duration::from_secs(5);

/// Opens a web link in the default browser (swapped out in tests).
type OpenUrl = Box<dyn Fn(&str)>;
/// Steps aside and pastes into the previous app (swapped out in tests).
type PasteInto = Box<dyn Fn(&slint::Window)>;

struct App {
    ui: slint::Weak<AppWindow>,
    history: Arc<History>,
    clipboard: Arc<dyn ClipboardBackend>,
    open_url: OpenUrl,
    paste_into: RefCell<PasteInto>,
    toaster: Toaster,
    i18n: Rc<I18n>,
    os: &'static str,
    /// where settings.json lives (None in tests: not saved)
    settings_dir: Option<std::path::PathBuf>,
    settings: RefCell<Settings>,
    /// created on demand, dropped when closed (no memory while not shown)
    settings_window: RefCell<Option<SettingsWindow>>,
    settings_toaster: RefCell<Option<Toaster<SettingsWindow>>>,
    /// an export or import is under way (from choosing the file to its end)
    busy: Cell<bool>,
    /// looks for the result of the background work while it runs
    job: RefCell<Option<Timer>>,
    /// deleted clips that "Undo" can still bring back, the last one last
    deleted: RefCell<Vec<clip_core::Deleted>>,
    /// lets them go for good when the undo time is over
    forget: Timer,
    model: Rc<VecModel<ClipItem>>,
    loaded: Cell<u32>,
    has_more: Cell<bool>,
    query: RefCell<String>,
    data_version: Cell<i64>,
    timers: RefCell<Vec<Rc<Timer>>>,
}

/// Runs for a horizontal layout (which always goes left to right): in a
/// right-to-left UI the first run ends up on the right. Spaces at the edges of
/// a run become gaps on the correct visual side: text layout drops a space at
/// the end of a right-to-left run.
fn runs(runs: Vec<Run>, rtl: bool) -> ModelRc<Part> {
    let mut parts: Vec<Part> = runs
        .into_iter()
        .map(|r| {
            let lead = r.text.chars().take_while(|c| c.is_whitespace()).count() as i32;
            let trail = r.text.chars().rev().take_while(|c| c.is_whitespace()).count() as i32;
            let text = r.text.trim();
            // a right-to-left run starts on the right
            let (left_gap, right_gap) = if is_rtl_text(text) { (trail, lead) } else { (lead, trail) };
            Part { text: text.into(), bold: r.bold, left_gap, right_gap }
        })
        .collect();
    if rtl {
        parts.reverse();
    }
    ModelRc::new(VecModel::from(parts))
}

/// Direction of a text by its first strong character, like `dir="auto"` in
/// browsers: Hebrew, Arabic, Syriac, Thaana, N'Ko are right-to-left.
/// Whether a row would look the same (the runs are new models on every load,
/// so they are compared by their text).
fn same_row(a: &ClipItem, b: &ClipItem) -> bool {
    let text = |m: &ModelRc<Part>| m.iter().map(|p| p.text).collect::<Vec<_>>();
    a.id == b.id
        && a.preview == b.preview
        && a.removing == b.removing
        && a.link == b.link
        && a.thumb_path == b.thumb_path
        && a.text_rtl == b.text_rtl
        && a.pinned == b.pinned
        && text(&a.source) == text(&b.source)
        && text(&a.time) == text(&b.time)
}

fn is_rtl_text(s: &str) -> bool {
    s.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

impl App {
    fn item(&self, c: Clip, now: chrono::DateTime<chrono::Local>) -> ClipItem {
        let thumb = c.thumb_path.unwrap_or_default();
        let rtl = self.i18n.rtl;
        let (tw, th) = if thumb.is_empty() { (0.0, 0.0) } else { thumbs::display_size(&thumb) };
        ClipItem {
            id: c.id as i32,
            is_image: c.image_path.is_some(),
            // code reads left to right whatever its comments are in
            text_rtl: !c.code && c.preview.as_deref().is_some_and(is_rtl_text),
            removing: false,
            preview: c.preview.unwrap_or_default().into(),
            is_link: c.link.is_some(),
            is_code: c.code,
            link: c.link.unwrap_or_default().into(),
            thumb_path: thumb.into(),
            thumb_width: tw,
            thumb_height: th,
            source: match &c.source_app {
                Some(app) => runs(self.i18n.runs("clip.copiedFrom", &[("app", app)]), rtl),
                None => runs(Vec::new(), rtl),
            },
            time: runs(self.i18n.timestamp(c.created_at, now), rtl),
            pinned: c.pinned,
        }
    }

    fn items(&self, clips: Vec<Clip>) -> Vec<ClipItem> {
        let now = chrono::Local::now();
        clips.into_iter().map(|c| self.item(c, now)).collect()
    }

    fn searching(&self) -> bool {
        !self.query.borrow().trim().is_empty()
    }

    /// Reloads what is on screen: the loaded pages, or the search results.
    fn reload(&self) {
        let _ = self.history.data_version().map(|v| self.data_version.set(v));
        let clips = if self.searching() {
            self.history.search(&query::parse(&self.query.borrow(), chrono::Local::now()), SEARCH_LIMIT)
        } else {
            let n = self.loaded.get().max(PAGE);
            self.history.list(0, n).inspect(|c| {
                self.loaded.set(c.len() as u32);
                self.has_more.set(c.len() as u32 == n);
            })
        };
        match clips {
            Ok(c) => self.show(self.items(c)),
            Err(e) => eprintln!("pastezo: {e}"),
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_searching(self.searching());
        }
    }

    /// Puts `items` in the list by clip, not by position: rows of clips that
    /// stay keep their place in the model (a new clip is inserted above them,
    /// a pinned one is moved), and only rows that look different are updated.
    /// A recreated or reused row would forget the mouse over it, or keep a text
    /// selection made in another clip; a copy makes the agent touch the database,
    /// which reloads the list right under the pointer.
    fn show(&self, items: Vec<ClipItem>) {
        let model = &self.model;
        let wanted: std::collections::HashSet<i32> = items.iter().map(|c| c.id).collect();
        for i in (0..model.row_count()).rev() {
            if !wanted.contains(&model.row_data(i).map_or(-1, |r| r.id)) {
                model.remove(i);
            }
        }
        for (i, item) in items.iter().enumerate() {
            match model.row_data(i) {
                Some(old) if old.id == item.id => {
                    if !same_row(&old, item) {
                        model.set_row_data(i, item.clone());
                    }
                }
                _ => {
                    // further down (moved, e.g. pinned): take it out, then insert here
                    if let Some(j) = (i + 1..model.row_count()).find(|&j| model.row_data(j).is_some_and(|r| r.id == item.id)) {
                        model.remove(j);
                    }
                    model.insert(i, item.clone());
                }
            }
        }
        while model.row_count() > items.len() {
            model.remove(model.row_count() - 1);
        }
        if let Some(ui) = self.ui.upgrade() {
            if self.position(ui.get_selected_id() as i64).is_none() {
                ui.set_selected_id(-1);
            }
        }
    }

    fn load_more(&self) {
        if self.searching() || !self.has_more.get() {
            return;
        }
        match self.history.list(self.loaded.get(), PAGE) {
            Ok(page) => {
                self.has_more.set(page.len() as u32 == PAGE);
                self.loaded.set(self.loaded.get() + page.len() as u32);
                for item in self.items(page) {
                    self.model.push(item);
                }
            }
            Err(e) => eprintln!("pastezo: {e}"),
        }
    }

    /// Another process (the agent) changed the history since we last looked.
    fn changed_elsewhere(&self) -> bool {
        matches!(self.history.data_version(), Ok(v) if v != self.data_version.get())
    }

    fn copy(&self, id: i64) {
        // the copied clip becomes the selected one (for the keyboard)
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected_id(id as i32);
        }
        match self.put_on_clipboard(id, None) {
            Ok(()) => self.toaster.show(ToastKind::Success, self.i18n.t("toast.copied", &[])),
            Err(e) => {
                eprintln!("pastezo: copy failed: {e}");
                self.toaster.show(ToastKind::Error, self.i18n.t("toast.copyFailed", &[]));
            }
        }
    }

    /// Return, ⌘1…⌘9: the clip onto the clipboard, then into the app the
    /// user came from (see `paste`).
    fn paste(&self, id: i64) {
        if let Err(e) = self.put_on_clipboard(id, None) {
            eprintln!("pastezo: copy failed: {e}");
            return self.toaster.show(ToastKind::Error, self.i18n.t("toast.copyFailed", &[]));
        }
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.get_preview_open() {
            self.close_preview();
        }
        (self.paste_into.borrow())(ui.window());
    }

    fn paste_selected(&self) {
        if let Some((_, id)) = self.selected() {
            self.paste(id);
        }
    }

    /// ⌘N / Ctrl+N: the N-th clip of the list (from 1), rows on their way out skipped.
    fn paste_nth(&self, n: usize) {
        if let Some(item) = self.model.iter().filter(|c| !c.removing).nth(n.wrapping_sub(1)) {
            self.paste(item.id as i64);
        }
    }

    fn selected(&self) -> Option<(usize, i64)> {
        let ui = self.ui.upgrade()?;
        let id = ui.get_selected_id() as i64;
        Some((self.position(id)?, id))
    }

    /// ↑/↓: the selection moves by `delta` rows from the clip the mouse moved
    /// onto, else from the selected one (from nothing: to the first row); the
    /// list scrolls to it and loads more near the end.
    fn move_selection(&self, delta: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let count = self.model.row_count();
        if count == 0 {
            return;
        }
        // from the clip the mouse moved onto (once: then from the selection),
        // else from the selected one
        let hovered = ui.get_hover_id();
        ui.set_hover_id(-1);
        let from = self.position(hovered as i64).or(self.selected().map(|(i, _)| i));
        let index = match from {
            Some(i) => (i as i64 + delta as i64).clamp(0, count as i64 - 1) as usize,
            None => 0,
        };
        if index + 3 >= count {
            self.load_more();
        }
        let Some(item) = self.model.row_data(index) else { return };
        ui.set_selected_id(item.id);
        ui.invoke_reveal(index as i32);
    }

    fn copy_selected(&self) {
        if let Some((_, id)) = self.selected() {
            self.copy(id);
        }
    }

    /// ⌫ / Delete: the selected clip goes, the selection moves to the next one
    /// (the previous one at the end of the list).
    fn delete_selected(self: &Rc<Self>) {
        let Some((index, id)) = self.selected() else { return };
        let next = self
            .model
            .row_data(index + 1)
            .or_else(|| index.checked_sub(1).and_then(|i| self.model.row_data(i)))
            .map_or(-1, |c| c.id);
        self.delete(id);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected_id(next);
        }
    }

    /// Space: the whole selected clip in the preview (full text, or the
    /// original image).
    fn preview_selected(&self) {
        let Some((index, id)) = self.selected() else { return };
        let (Some(ui), Some(item)) = (self.ui.upgrade(), self.model.row_data(index)) else { return };
        let Ok(Some(clip)) = self.history.get(id) else { return };
        if let Some(path) = &clip.image_path {
            ui.set_preview_image(slint::Image::load_from_path(std::path::Path::new(path)).unwrap_or_default());
            ui.set_preview_is_image(true);
        } else {
            let text = match self.history.content(&clip) {
                Ok(clip_core::ClipContent::Text(t)) => t,
                _ => clip.preview.clone().unwrap_or_default(),
            };
            let text = match text.char_indices().nth(PREVIEW_CHARS) {
                Some((cut, _)) => format!("{}…", &text[..cut]),
                None => text,
            };
            ui.set_preview_text(text.into());
            ui.set_preview_is_code(clip.code);
            ui.set_preview_is_image(false);
        }
        ui.set_preview_source(item.source.clone());
        ui.set_preview_time(item.time.clone());
        ui.set_preview_open(true);
    }

    fn close_preview(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_preview_open(false);
        ui.invoke_focus_list();
        // the full text or image is not kept once it is off screen
        ui.set_preview_text(SharedString::default());
        ui.set_preview_image(slint::Image::default());
    }

    /// A printable key typed in the list: the search opens with it.
    fn type_to_search(&self, text: &str) -> bool {
        let Some(ui) = self.ui.upgrade() else { return false };
        // Slint's special keys (arrows, F-keys…) are private-use characters
        let printable = text.chars().all(|c| !c.is_control() && !('\u{E000}'..='\u{F8FF}').contains(&c));
        if !printable || text.trim().is_empty() {
            return false;
        }
        let query = if ui.get_search_open() { format!("{}{text}", ui.get_query()) } else { text.to_string() };
        ui.set_search_open(true);
        ui.set_query(query.as_str().into());
        ui.invoke_query_edited(query.into());
        ui.invoke_focus_search_at_end();
        true
    }

    /// ⌘C / Ctrl+C on text selected in a row: bytes `start..end` of its text.
    /// Like the copy button, it does not become a new clip.
    fn copy_selection(&self, id: i64, start: usize, end: usize) {
        if start >= end {
            return;
        }
        if let Err(e) = self.put_on_clipboard(id, Some(start..end)) {
            eprintln!("pastezo: copy failed: {e}");
            self.toaster.show(ToastKind::Error, self.i18n.t("toast.copyFailed", &[]));
        }
    }

    /// Clip `id` (or bytes `part` of its text) onto the clipboard.
    fn put_on_clipboard(&self, id: i64, part: Option<std::ops::Range<usize>>) -> Result<(), String> {
        // Linux: the clipboard lives only as long as its owner process, and this
        // window exits when closed — the agent sets it instead
        #[cfg(target_os = "linux")]
        if clip_core::data_dir().is_some_and(|dir| clip_core::ipc::request_copy(&dir, id, part.clone())) {
            return Ok(());
        }
        let content = self.history.content_of(id, part).map_err(|e| e.to_string())?.ok_or("the clip is gone")?;
        // the agent sees the change; the note tells it this is not a new copy
        let _ = self.history.mark_own_write(&content);
        self.clipboard.write(&content).map(|_| ()).map_err(|e| e.to_string())
    }

    /// The clip leaves the history at once; "Undo" (⌘Z) can bring it back
    /// while the toast is shown, then its files go for good.
    fn delete(self: &Rc<Self>, id: i64) {
        match self.history.take(id) {
            Ok(Some(taken)) => self.deleted.borrow_mut().push(taken),
            Ok(None) => {}
            Err(e) => return eprintln!("pastezo: {e}"),
        }
        self.offer_undo();
        let _ = self.history.data_version().map(|v| self.data_version.set(v));
        let Some(i) = self.position(id) else { return };
        // the row fades and collapses first; it leaves the model afterwards
        let mut row = self.model.row_data(i).unwrap();
        row.removing = true;
        self.model.set_row_data(i, row);
        let app = Rc::downgrade(self);
        Timer::single_shot(REMOVE_ANIMATION, move || {
            let Some(app) = app.upgrade() else { return };
            // look it up again: the list may have changed meanwhile
            if let Some(i) = app.position(id) {
                app.model.remove(i);
                app.loaded.set(app.loaded.get().saturating_sub(1));
            }
        });
    }

    /// "Deleted — Undo" for `UNDO_TIME` (again after each deletion); then the
    /// deleted clips are gone for good.
    fn offer_undo(self: &Rc<Self>) {
        let undo: toasts::Action = Rc::new({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.undo_delete();
                }
            }
        });
        self.toaster.show_action(ToastKind::Info, self.i18n.t("toast.deleted", &[]), self.i18n.t("toast.undo", &[]), UNDO_TIME, undo);
        let app = Rc::downgrade(self);
        self.forget.start(TimerMode::SingleShot, UNDO_TIME, move || {
            if let Some(app) = app.upgrade() {
                app.forget_deleted();
            }
        });
    }

    /// The deleted clips can no longer come back: their image files go.
    fn forget_deleted(&self) {
        self.forget.stop();
        for d in self.deleted.borrow_mut().drain(..).collect::<Vec<_>>() {
            self.history.discard(d);
        }
    }

    /// ⌘Z / "Undo": the last deleted clip back in its place, selected; the
    /// one deleted before it can follow while the toast is shown.
    fn undo_delete(self: &Rc<Self>) {
        let Some(taken) = self.deleted.borrow_mut().pop() else { return };
        match self.history.restore(&taken) {
            Ok(clip) => {
                self.reload();
                if let (Some(ui), Some(i)) = (self.ui.upgrade(), self.position(clip.id)) {
                    ui.set_selected_id(clip.id as i32);
                    ui.invoke_reveal(i as i32);
                }
            }
            Err(e) => {
                eprintln!("pastezo: cannot undo: {e}");
                self.history.discard(taken);
            }
        }
        if self.deleted.borrow().is_empty() {
            self.forget.stop();
            self.toaster.hide();
        } else {
            self.offer_undo();
        }
    }

    /// Pins a clip to the top of the list, or unpins it.
    fn toggle_pin(&self, id: i64) {
        let Some(item) = self.position(id).and_then(|i| self.model.row_data(i)) else { return };
        if let Err(e) = self.history.set_pinned(id, !item.pinned) {
            return eprintln!("pastezo: {e}");
        }
        self.reload();
    }

    /// Drags the original image out of the window (as a file). Returns whether
    /// the OS took over the drag.
    fn drag_image(&self, id: i64) -> bool {
        let Ok(Some(clip)) = self.history.get(id) else { return false };
        let (Some(original), Some(thumb)) = (&clip.image_path, &clip.thumb_path) else { return false };
        let file = match drag::prepare(std::path::Path::new(original), clip.created_at) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("pastezo: cannot prepare the drag: {e}");
                return false;
            }
        };
        let (w, h) = thumbs::display_size(thumb);
        let Some(ui) = self.ui.upgrade() else { return false };
        drag::start(ui.window(), &file, std::path::Path::new(thumb), (w as f64, h as f64))
    }

    fn position(&self, id: i64) -> Option<usize> {
        self.model.iter().position(|c| c.id as i64 == id)
    }

    fn open_link(&self, id: i64) {
        if let Ok(Some(Clip { link: Some(url), .. })) = self.history.get(id) {
            // only http(s), checked when the clip was saved (clip_core::link_of)
            (self.open_url)(&url);
        }
    }
}

/// UI strings and OS facts for any of our windows (each window has its own globals).
fn fill_env<T: ComponentHandle>(ui: &T, i18n: &Rc<I18n>, os: &str)
where
    for<'a> Env<'a>: slint::Global<'a, T>,
{
    let env = Env::get(ui);
    env.set_tab_typography(i18n.t("settings.typography", &[]).into());
    env.set_text_font_label(i18n.t("settings.textFont", &[]).into());
    env.set_font_size_label(i18n.t("settings.fontSize", &[]).into());
    env.set_line_height_label(i18n.t("settings.lineHeight", &[]).into());
    env.set_clip_spacing_label(i18n.t("settings.clipSpacing", &[]).into());
    env.set_reset_font(i18n.t("settings.resetFont", &[]).into());
    env.set_preset_label(i18n.t("settings.preset", &[]).into());
    env.set_preset_compact(i18n.t("settings.presetCompact", &[]).into());
    env.set_preset_default(i18n.t("settings.presetDefault", &[]).into());
    env.set_preset_large(i18n.t("settings.presetLarge", &[]).into());
    env.on_format_points({
        let i18n = i18n.clone();
        move |v| i18n.t("settings.points", &[("value", &i18n.decimal(v))]).into()
    });
    env.on_format_em({
        let i18n = i18n.clone();
        move |v| i18n.t("settings.em", &[("value", &i18n.decimal(v))]).into()
    });
    env.set_rtl(i18n.rtl);
    env.set_macos(os == "macos");
    env.set_touch(matches!(os, "android" | "ios"));
    env.set_copy_label(i18n.t("clip.copy", &[]).into());
    env.set_delete_label(i18n.t("clip.delete", &[]).into());
    env.set_pin_label(i18n.t("clip.pin", &[]).into());
    env.set_unpin_label(i18n.t("clip.unpin", &[]).into());
    env.set_search_placeholder(i18n.t("search.placeholder", &[]).into());
    env.set_empty_text(i18n.t("list.empty", &[]).into());
    env.set_no_results_text(i18n.t("search.noResults", &[]).into());
    env.set_settings_label(i18n.t("settings.title", &[]).into());
    env.set_settings_title(i18n.t("settings.title", &[]).into());
    env.set_tab_general(i18n.t("settings.general", &[]).into());
    env.set_tab_themes(i18n.t("settings.themes", &[]).into());
    env.set_tab_app_icon(i18n.t("settings.appIcon", &[]).into());
    env.set_theme_light(i18n.t("settings.light", &[]).into());
    env.set_theme_dark(i18n.t("settings.dark", &[]).into());
    env.set_theme_sample(i18n.t("settings.sample", &[]).into());
    env.set_sample_source(runs(i18n.runs("clip.copiedFrom", &[("app", "Safari")]), i18n.rtl));
    env.set_sample_time(runs(i18n.timestamp(chrono::Local::now().timestamp_millis(), chrono::Local::now()), i18n.rtl));
    env.set_launch_label(i18n.t("settings.launch", &[]).into());
    env.set_launch_at_login(i18n.t("settings.launchAtLogin", &[]).into());
    env.set_can_hide_from_capture(capture::AVAILABLE);
    env.set_privacy_label(i18n.t("settings.privacy", &[]).into());
    env.set_hide_from_capture(i18n.t("settings.hideFromCapture", &[]).into());
    env.set_hotkey_label(i18n.t("settings.hotkey", &[]).into());
    env.set_hotkey_record(i18n.t("settings.hotkeyRecord", &[]).into());
    env.set_hotkey_typing(i18n.t("settings.hotkeyTyping", &[]).into());
    env.set_hotkey_clear(i18n.t("settings.hotkeyClear", &[]).into());
    env.set_hotkey_hint(i18n.t("settings.hotkeyHint", &[]).into());
    env.set_hotkey_wayland(i18n.t("settings.hotkeyWayland", &[]).into());
    env.set_history_label(i18n.t("settings.history", &[]).into());
    env.set_keep_label(i18n.t("settings.keep", &[]).into());
    env.set_keep_week(i18n.t("settings.keepWeek", &[]).into());
    env.set_keep_month(i18n.t("settings.keepMonth", &[]).into());
    env.set_keep_year(i18n.t("settings.keepYear", &[]).into());
    env.set_keep_forever(i18n.t("settings.keepForever", &[]).into());
    env.set_keep_hint(i18n.t("settings.keepHint", &[]).into());
    env.set_clear_all(i18n.t("settings.clearAll", &[]).into());
    env.set_clear_confirm(i18n.t("settings.clearConfirm", &[]).into());
    env.set_clear_warning(i18n.t("settings.clearWarning", &[]).into());
    env.set_cancel(i18n.t("settings.cancel", &[]).into());
    env.set_tab_import_export(i18n.t("settings.importExport", &[]).into());
    env.set_tab_statistics(i18n.t("settings.statistics", &[]).into());
    env.set_stats_today_label(i18n.t("stats.today", &[]).into());
    env.set_stats_week_label(i18n.t("stats.week", &[]).into());
    env.set_stats_month_label(i18n.t("stats.month", &[]).into());
    env.set_stats_by_day(i18n.t("stats.byDay", &[]).into());
    env.set_stats_text_label(i18n.t("stats.text", &[]).into());
    env.set_stats_link_label(i18n.t("stats.link", &[]).into());
    env.set_stats_image_label(i18n.t("stats.image", &[]).into());
    env.set_export_label(i18n.t("settings.exportLabel", &[]).into());
    env.set_export_button(i18n.t("settings.exportButton", &[]).into());
    env.set_export_hint(i18n.t("settings.exportHint", &[]).into());
    env.set_import_label(i18n.t("settings.importLabel", &[]).into());
    env.set_import_button(i18n.t("settings.importButton", &[]).into());
    env.set_import_hint(i18n.t("settings.importHint", &[]).into());
}

/// A theme (`themes::THEMES` id, or `themes::SYSTEM`) for a window: our
/// colours (Theme global) and the OS-drawn parts (title bar, traffic lights,
/// menus) through winit, light or dark to match.
fn apply_theme<T: ComponentHandle>(ui: &T, id: &str)
where
    for<'a> Theme<'a>: slint::Global<'a, T>,
    for<'a> Palette<'a>: slint::Global<'a, T>,
{
    let global = Theme::get(ui);
    let theme = themes::find(id);
    // the standard controls (check boxes, buttons, fields…) in the theme's light
    // or dark look; "system" leaves them following the OS
    if let Some(t) = theme {
        let scheme = if t.is_dark() { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
        Palette::get(ui).set_color_scheme(scheme);
    }
    global.set_follow_system(theme.is_none());
    if let Some(t) = theme {
        global.set_chosen(t.colors());
    } else {
        global.set_system_light(themes::find(themes::LIGHT).unwrap().colors());
        global.set_system_dark(themes::find(themes::DARK).unwrap().colors());
    }
    use slint::winit_030::{winit, WinitWindowAccessor};
    ui.window().with_winit_window(|w: &winit::window::Window| {
        w.set_theme(theme.map(|t| if t.is_dark() { winit::window::Theme::Dark } else { winit::window::Theme::Light }))
    });
}

/// The clips' text in the list follows Settings → Typography.
fn apply_text_style(ui: &AppWindow, t: &TextStyle) {
    let g = ClipText::get(ui);
    g.set_font(t.font.as_str().into());
    g.set_size(t.size);
    g.set_line(t.line);
    g.set_gap(t.gap);
}

fn show_text_style(w: &SettingsWindow, t: &TextStyle) {
    w.set_text_font(t.font.as_str().into());
    w.set_text_size(t.size);
    w.set_text_line(t.line);
    w.set_text_gap(t.gap);
    w.set_text_preset(t.preset().into());
}

/// Installed font families for the font list: the bundled MiSans first, then
/// the system's in alphabetical order (hidden system faces left out).
fn font_families() -> ModelRc<SharedString> {
    let mut collection = fontique::Collection::new(fontique::CollectionOptions { shared: false, system_fonts: true });
    let mut names: Vec<String> = collection
        .family_names()
        .filter(|n| !n.starts_with('.') && *n != TextStyle::DEFAULT_FONT)
        .map(String::from)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    let all = std::iter::once(TextStyle::DEFAULT_FONT.to_string()).chain(names).map(SharedString::from);
    ModelRc::new(VecModel::from(all.collect::<Vec<_>>()))
}

/// The icons of the App Icon tab (previews are read from disk when it opens).
fn icon_choices() -> ModelRc<IconChoice> {
    let choices: Vec<IconChoice> = app_icon::VARIANTS
        .iter()
        .map(|(id, name)| IconChoice { id: (*id).into(), name: (*name).into(), preview: app_icon::preview(id) })
        .collect();
    ModelRc::new(VecModel::from(choices))
}

/// Windows / Linux: the window and taskbar icon follow the chosen app icon
/// (`set` gives it to one window). macOS draws no window icons: the Dock's is
/// set by `app_icon::apply`.
fn set_window_icon(set: impl FnOnce(slint::Image), id: &str) {
    if cfg!(not(target_os = "macos")) {
        if let Ok(image) = slint::Image::load_from_path(&app_icon::file(id)) {
            set(image);
        }
    }
}

/// The cards of the Themes tab.
fn theme_choices(i18n: &I18n) -> ModelRc<ThemeChoice> {
    let choices: Vec<ThemeChoice> = themes::THEMES
        .iter()
        .map(|t| ThemeChoice {
            id: t.id.into(),
            name: match t.name {
                Some(n) => n.into(),
                None => i18n.t(if t.id == themes::DARK { "settings.dark" } else { "settings.light" }, &[]).into(),
            },
            colors: t.colors(),
        })
        .collect();
    ModelRc::new(VecModel::from(choices))
}

impl App {
    fn close_settings(&self) {
        self.settings_toaster.borrow_mut().take();
        let window = self.settings_window.borrow_mut().take();
        if let Some(w) = window {
            let _ = w.hide();
        }
    }

    fn open_settings(self: &Rc<Self>) {
        if let Some(w) = self.settings_window.borrow().as_ref() {
            let _ = w.show();
            return;
        }
        let Ok(w) = SettingsWindow::new() else { return };
        fill_env(&w, &self.i18n, self.os);
        w.set_themes(theme_choices(&self.i18n));
        w.set_launch_at_login(self.settings().launch_at_login);
        w.on_launch_at_login_changed({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.save_settings(Settings { launch_at_login: on, ..app.settings() });
                    agent::set_autostart(on);
                }
            }
        });
        self.wire_hotkey(&w);
        w.set_hide_from_capture(self.settings().hide_from_capture);
        w.on_hide_from_capture_changed({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.set_hide_from_capture(on);
                }
            }
        });
        stats::show(&w, &self.history, &self.i18n, chrono::Local::now());
        w.set_theme_id(self.settings().theme.into());
        show_text_style(&w, &self.settings().text);
        let range = |(min, max, step): (f32, f32, f32)| ModelRc::new(VecModel::from(vec![min, max, step]));
        w.set_size_range(range(TextStyle::SIZE));
        w.set_line_range(range(TextStyle::LINE));
        w.set_gap_range(range(TextStyle::GAP));
        w.on_text_style_edited({
            let app = Rc::downgrade(self);
            let w = w.as_weak();
            move || {
                let (Some(app), Some(w)) = (app.upgrade(), w.upgrade()) else { return };
                app.set_text_style(TextStyle {
                    font: w.get_text_font().into(),
                    size: w.get_text_size(),
                    line: w.get_text_line(),
                    gap: w.get_text_gap(),
                });
            }
        });
        w.on_text_preset_chosen({
            let app = Rc::downgrade(self);
            move |id| {
                if let Some(app) = app.upgrade() {
                    app.set_text_style(app.settings().text.with_preset(&id));
                }
            }
        });
        w.on_text_style_reset({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.set_text_style(TextStyle::default());
                }
            }
        });
        w.on_fonts_wanted({
            let w = w.as_weak();
            move || {
                let Some(w) = w.upgrade() else { return };
                // once per settings window: reading the system's font list takes a moment
                if w.get_fonts().row_count() == 0 {
                    w.set_fonts(font_families());
                }
            }
        });
        w.set_icons(icon_choices());
        w.set_icon_id(self.settings().icon.into());
        set_window_icon(|i| w.set_window_icon(i), self.settings().icon);
        w.on_icon_chosen({
            let app = Rc::downgrade(self);
            move |id| {
                if let Some(app) = app.upgrade() {
                    app.set_icon(&id);
                }
            }
        });
        w.on_theme_chosen({
            let app = Rc::downgrade(self);
            move |id| {
                if let Some(app) = app.upgrade() {
                    app.set_theme(&id);
                }
            }
        });
        w.on_export_history({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.export_history();
                }
            }
        });
        w.on_import_history({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.import_history();
                }
            }
        });
        w.set_keep_days(self.history.keep_days().map_or(0, |d| d as i32));
        w.on_keep_days_chosen({
            let app = Rc::downgrade(self);
            let w = w.as_weak();
            move |days| {
                let (Some(app), Some(w)) = (app.upgrade(), w.upgrade()) else { return };
                app.set_keep_days(u32::try_from(days).ok().filter(|&d| d > 0));
                w.set_keep_days(app.history.keep_days().map_or(0, |d| d as i32));
            }
        });
        w.on_clear_history({
            let app = Rc::downgrade(self);
            move || {
                if let Some(app) = app.upgrade() {
                    app.clear_history();
                }
            }
        });
        w.window().on_close_requested({
            let app = Rc::downgrade(self);
            move || {
                // dropped right after this callback returns
                let app = app.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(app) = app.upgrade() {
                        app.close_settings();
                    }
                });
                slint::CloseRequestResponse::HideWindow
            }
        });
        w.on_minimize_window({
            let w = w.as_weak();
            move || {
                if let Some(w) = w.upgrade() {
                    w.window().set_minimized(true);
                }
            }
        });
        w.on_quit(|| {
            let _ = slint::quit_event_loop();
        });
        w.on_close_window({
            let app = Rc::downgrade(self);
            move || {
                // from the window's own key handler: drop it once that returns
                let app = app.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(app) = app.upgrade() {
                        app.close_settings();
                    }
                });
            }
        });
        let _ = w.show();
        if self.settings().hide_from_capture {
            capture::apply(w.window(), true);
        }
        apply_theme(&w, self.settings().theme);
        *self.settings_toaster.borrow_mut() = Some(Toaster::new(&w));
        *self.settings_window.borrow_mut() = Some(w);
    }

    /// The settings window's toast while it is open, else the list's.
    fn notify(&self, kind: ToastKind, text: String) {
        match self.settings_toaster.borrow().as_ref() {
            Some(t) => t.show(kind, text),
            None => self.toaster.show(kind, text),
        }
    }

    /// The system open/save dialog for history files, over the settings window.
    fn history_dialog(&self) -> rfd::AsyncFileDialog {
        let dialog = rfd::AsyncFileDialog::new().add_filter(self.i18n.t("settings.historyFile", &[]), &[backup::EXTENSION]);
        match self.settings_window.borrow().as_ref() {
            Some(w) => dialog.set_parent(&w.window().window_handle()),
            None => dialog,
        }
    }

    /// Import & Export → "Export History…": asks where to save, then writes.
    fn export_history(self: &Rc<Self>) {
        if self.busy.replace(true) {
            return;
        }
        let name = format!("Pastezo {}.{}", chrono::Local::now().format("%Y-%m-%d"), backup::EXTENSION);
        let dialog = self.history_dialog().set_file_name(name);
        let app = Rc::downgrade(self);
        let _ = slint::spawn_local(async move {
            let file = dialog.save_file().await;
            let Some(app) = app.upgrade() else { return };
            match file {
                Some(f) => app.export_to(f.path().to_path_buf()),
                None => app.busy.set(false),
            }
        });
    }

    /// Import & Export → "Import History…": asks for the file, then adds its clips.
    fn import_history(self: &Rc<Self>) {
        if self.busy.replace(true) {
            return;
        }
        let dialog = self.history_dialog();
        let app = Rc::downgrade(self);
        let _ = slint::spawn_local(async move {
            let file = dialog.pick_file().await;
            let Some(app) = app.upgrade() else { return };
            match file {
                Some(f) => app.import_from(f.path().to_path_buf()),
                None => app.busy.set(false),
            }
        });
    }

    fn export_to(self: &Rc<Self>, path: std::path::PathBuf) {
        let history = self.history.clone();
        self.in_background(
            move || backup::export(&history, &path),
            |app, result| match result {
                Ok(n) => app.notify(ToastKind::Success, app.i18n.t("toast.exported", &[("count", &n.to_string())])),
                Err(e) => {
                    eprintln!("pastezo: export failed: {e}");
                    app.notify(ToastKind::Error, app.i18n.t("toast.exportFailed", &[]));
                }
            },
        );
    }

    fn import_from(self: &Rc<Self>, path: std::path::PathBuf) {
        let history = self.history.clone();
        self.in_background(
            move || backup::import(&history, &path),
            |app, result| {
                // images were decoded for their previews: hand the memory back
                clip_core::platform::release_free_memory();
                match result {
                    Ok(n) => app.notify(ToastKind::Success, app.i18n.t("toast.imported", &[("count", &n.to_string())])),
                    Err(e) => {
                        eprintln!("pastezo: import failed: {e}");
                        app.notify(ToastKind::Error, app.i18n.t("toast.importFailed", &[]));
                    }
                }
                app.reload();
            },
        );
    }

    /// Runs `work` on another thread (the window stays responsive; a big
    /// history takes seconds) and gives its result to `done` here. A timer
    /// looks for the result only while the work runs.
    fn in_background<R: Send + 'static>(
        self: &Rc<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&App, R) + 'static,
    ) {
        self.busy.set(true);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        let app = Rc::downgrade(self);
        let mut done = Some(done);
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(app) = app.upgrade() else { return };
            let result = match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
                Ok(r) => Some(r),
            };
            if let Some(t) = app.job.borrow().as_ref() {
                t.stop();
            }
            app.busy.set(false);
            if let (Some(r), Some(done)) = (result, done.take()) {
                done(&app, r);
            }
        });
        *self.job.borrow_mut() = Some(timer);
    }

    /// Settings → General → Keep clips: saved for the agent, and the clips
    /// already too old go now.
    fn set_keep_days(&self, days: Option<u32>) {
        if let Err(e) = self.history.set_keep_days(days) {
            return eprintln!("pastezo: {e}");
        }
        self.forget_old();
    }

    /// Deletes the clips older than Settings → General → Keep clips.
    fn forget_old(&self) {
        match self.history.forget_old(chrono::Utc::now().timestamp_millis()) {
            Ok(0) => {}
            Ok(_) => self.reload(),
            Err(e) => eprintln!("pastezo: {e}"),
        }
    }

    fn clear_history(&self) {
        if let Err(e) = self.history.clear() {
            return eprintln!("pastezo: {e}");
        }
        self.loaded.set(0);
        self.reload();
    }

    fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    fn save_settings(&self, settings: Settings) {
        if let Some(dir) = &self.settings_dir {
            if let Err(e) = settings.save(dir) {
                eprintln!("pastezo: cannot save settings: {e}");
            }
        }
        *self.settings.borrow_mut() = settings;
    }

    /// Settings → Typography: keeps it within the sliders, saves it, and
    /// restyles the list; the settings window shows the kept values.
    fn set_text_style(&self, text: TextStyle) {
        let text = text.fitted();
        // a std slider (Windows, Linux) reports every pixel of a drag, not only
        // new steps: nothing to save or restyle until the step changes
        if text == self.settings().text {
            if let Some(w) = self.settings_window.borrow().as_ref() {
                show_text_style(w, &text);
            }
            return;
        }
        self.save_settings(Settings { text: text.clone(), ..self.settings() });
        if let Some(ui) = self.ui.upgrade() {
            apply_text_style(&ui, &text);
        }
        if let Some(w) = self.settings_window.borrow().as_ref() {
            show_text_style(w, &text);
        }
    }

    /// Settings → General: every window of ours in or out of screen captures.
    fn set_hide_from_capture(&self, hide: bool) {
        self.save_settings(Settings { hide_from_capture: hide, ..self.settings() });
        if let Some(ui) = self.ui.upgrade() {
            capture::apply(ui.window(), hide);
        }
        if let Some(w) = self.settings_window.borrow().as_ref() {
            capture::apply(w.window(), hide);
        }
    }

    fn set_icon(&self, id: &str) {
        let Some(id) = app_icon::known(id) else { return };
        self.save_settings(Settings { icon: id, ..self.settings() });
        app_icon::apply(id, false);
        if let Some(ui) = self.ui.upgrade() {
            set_window_icon(|i| ui.set_window_icon(i), id);
        }
        if let Some(w) = self.settings_window.borrow().as_ref() {
            set_window_icon(|i| w.set_window_icon(i), id);
            w.set_icon_id(id.into());
        }
    }

    fn set_theme(&self, id: &str) {
        let Some(id) = themes::known(id) else { return };
        self.save_settings(Settings { theme: id, ..self.settings() });
        if let Some(ui) = self.ui.upgrade() {
            apply_theme(&ui, id);
        }
        if let Some(w) = self.settings_window.borrow().as_ref() {
            apply_theme(w, id);
            w.set_theme_id(id.into());
        }
    }
}

/// Size and place of the list window before it is shown: as it was closed.
fn restore_window(ui: &AppWindow, frame: &WindowFrame) {
    ui.window().set_size(slint::LogicalSize::new(frame.width, frame.height));
    if let Some((x, y, scale)) = frame.position {
        // macOS places windows in points; Windows and X11 in pixels
        if cfg!(target_os = "macos") {
            ui.window().set_position(slint::LogicalPosition::new(x as f32 / scale, y as f32 / scale));
        } else {
            ui.window().set_position(slint::PhysicalPosition::new(x, y));
        }
    }
}

/// A place from last time on a display that is gone now (an unplugged
/// monitor): the window goes to the middle of the main display instead.
fn keep_on_screen(ui: &AppWindow) {
    use slint::winit_030::{winit, WinitWindowAccessor};
    ui.window().with_winit_window(|w: &winit::window::Window| {
        let Ok(pos) = w.outer_position() else { return };
        let size = w.outer_size();
        // enough of the title bar to take hold of the window
        let grip = (40.0 * w.scale_factor()) as i32;
        let reachable = w.available_monitors().any(|m| {
            let (mp, ms) = (m.position(), m.size());
            let width = (pos.x + size.width as i32).min(mp.x + ms.width as i32) - pos.x.max(mp.x);
            let height = (pos.y + grip).min(mp.y + ms.height as i32) - pos.y.max(mp.y);
            width >= 2 * grip && height > 0
        });
        if reachable {
            return;
        }
        if let Some(m) = w.primary_monitor().or_else(|| w.available_monitors().next()) {
            let (mp, ms) = (m.position(), m.size());
            let x = mp.x + (ms.width as i32 - size.width as i32).max(0) / 2;
            let y = mp.y + (ms.height as i32 - size.height as i32).max(0) / 2;
            w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        }
    });
}

/// Saves the window's size and place a second after it was last moved or
/// resized. Not on closing: by then winit has let go of the window, and ⌘Q
/// on macOS ends the process without returning from the event loop.
fn remember_window_frame(ui: &AppWindow, app: &Rc<App>) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
    let save = Timer::default();
    let (weak, app) = (ui.as_weak(), Rc::downgrade(app));
    ui.window().on_winit_window_event(move |_, event| {
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            let (weak, app) = (weak.clone(), app.clone());
            save.start(TimerMode::SingleShot, Duration::from_secs(1), move || {
                let (Some(ui), Some(app)) = (weak.upgrade(), app.upgrade()) else { return };
                let last = app.settings().window;
                if let Some(frame) = window_frame(&ui, last).filter(|f| Some(*f) != last) {
                    app.save_settings(Settings { window: Some(frame), ..app.settings() });
                }
            });
        }
        EventResult::Propagate
    });
}

/// The list window now, to be restored next time; `None`: nothing worth
/// keeping (minimized, full screen). Maximized keeps the size and place it
/// goes back to (`last`).
fn window_frame(ui: &AppWindow, last: Option<WindowFrame>) -> Option<WindowFrame> {
    use slint::winit_030::{winit, WinitWindowAccessor};
    ui.window()
        .with_winit_window(|w: &winit::window::Window| {
            if w.is_minimized() == Some(true) || w.fullscreen().is_some() {
                return None;
            }
            let scale = w.scale_factor();
            let size = w.inner_size().to_logical::<f32>(scale);
            let position = w.outer_position().ok().map(|p| (p.x, p.y, scale as f32));
            let now = WindowFrame { width: size.width, height: size.height, position, maximized: false };
            if w.is_maximized() {
                return Some(WindowFrame { maximized: true, ..last.unwrap_or(now) });
            }
            Some(now)
        })
        .flatten()
}

fn select_backend() -> Result<(), slint::PlatformError> {
    // Software rendering: no GPU context or GPU-side surfaces (measured ~50 MB
    // less than femtovg); a list of text is cheap to draw on the CPU.
    #[allow(unused_mut)]
    let mut selector = slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into());
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        // the design: no title bar, the traffic lights over the content
        selector = selector.with_winit_window_attributes_hook(|attrs| {
            attrs.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
        });
    }
    // Linux: X11 (XWayland under Wayland), so an image can be dragged out of the
    // window: Wayland lets a client start a drag only with its pointer-press
    // serial, which winit does not share (see drag.rs). Only when an X server
    // is there: a Wayland session without XWayland keeps native Wayland (and
    // the window opens, without dragging images out).
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty()) {
        use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = slint::winit_030::winit::event_loop::EventLoop::with_user_event();
        builder.with_x11();
        selector = selector.with_winit_event_loop_builder(builder);
    }
    selector.select()
}

/// Fills the window with the history and connects every action.
fn wire(
    ui: &AppWindow,
    history: Arc<History>,
    i18n: I18n,
    os: &'static str,
    clipboard: Arc<dyn ClipboardBackend>,
    open_url: OpenUrl,
    settings_dir: Option<std::path::PathBuf>,
) -> Rc<App> {
    let i18n = Rc::new(i18n);
    fill_env(ui, &i18n, os);
    let settings = settings_dir.as_deref().map(Settings::load).unwrap_or_default();
    apply_theme(ui, settings.theme);
    apply_text_style(ui, &settings.text);

    let model = Rc::new(VecModel::default());
    ui.set_clips(ModelRc::from(model.clone()));
    let app = Rc::new(App {
        ui: ui.as_weak(),
        history,
        clipboard,
        open_url,
        paste_into: RefCell::new(Box::new(paste::into_previous_app)),
        toaster: Toaster::new(ui),
        i18n,
        os,
        settings_dir,
        settings: RefCell::new(settings),
        settings_window: RefCell::new(None),
        settings_toaster: RefCell::new(None),
        busy: Cell::new(false),
        job: RefCell::new(None),
        deleted: RefCell::new(Vec::new()),
        forget: Timer::default(),
        model,
        loaded: Cell::new(0),
        has_more: Cell::new(true),
        query: RefCell::new(String::new()),
        data_version: Cell::new(0),
        timers: RefCell::new(Vec::new()),
    });
    // the agent deletes them too, but may not have run since they got too old
    app.forget_old();
    app.reload();

    let scale = ui.window().scale_factor();
    Thumbs::get(ui).on_load(move |path| thumbs::load(&path, scale));

    ui.on_load_more({
        let app = app.clone();
        move || app.load_more()
    });
    ui.on_move_selection({
        let app = app.clone();
        move |delta| app.move_selection(delta)
    });
    ui.on_copy_selected({
        let app = app.clone();
        move || app.copy_selected()
    });
    ui.on_paste_selected({
        let app = app.clone();
        move || app.paste_selected()
    });
    ui.on_paste_nth({
        let app = app.clone();
        move |n| app.paste_nth(n.max(0) as usize)
    });
    ui.on_delete_selected({
        let app = app.clone();
        move || app.delete_selected()
    });
    ui.on_preview_selected({
        let app = app.clone();
        move || app.preview_selected()
    });
    ui.on_close_preview({
        let app = app.clone();
        move || app.close_preview()
    });
    ui.on_type_to_search({
        let app = app.clone();
        move |text| app.type_to_search(&text)
    });
    ui.on_toggle_pin({
        let app = app.clone();
        move |id| app.toggle_pin(id as i64)
    });
    ui.on_undo_delete({
        let app = app.clone();
        move || app.undo_delete()
    });
    ui.on_toast_action({
        let app = Rc::downgrade(&app);
        move || {
            if let Some(app) = app.upgrade() {
                app.toaster.clicked();
            }
        }
    });
    ui.on_delete({
        let app = app.clone();
        move |id| app.delete(id as i64)
    });
    ui.on_copy_selection({
        let app = app.clone();
        move |id, start, end| app.copy_selection(id as i64, start.max(0) as usize, end.max(0) as usize)
    });
    ui.on_open_link({
        let app = app.clone();
        move |id| app.open_link(id as i64)
    });
    ui.on_minimize_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_minimized(true);
            }
        }
    });
    // like ⌘Q: every window closes, the process ends (the agent stays)
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    ui.on_close_window({
        let weak = ui.as_weak();
        move || {
            // like the close button: the window's process ends (the agent stays)
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        }
    });
    ui.on_open_settings({
        let app = app.clone();
        move || app.open_settings()
    });
    ui.on_drag_image({
        let app = app.clone();
        let weak = ui.as_weak();
        move |id| {
            if app.drag_image(id as i64) {
                // AppKit now tracks the mouse and Slint never sees the button
                // go up: release it here so nothing stays pressed or hovered
                if let Some(ui) = weak.upgrade() {
                    use slint::platform::{PointerEventButton, WindowEvent};
                    let position = slint::LogicalPosition::new(-1.0, -1.0);
                    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
                    ui.window().dispatch_event(WindowEvent::PointerExited);
                }
            }
        }
    });
    let copied_reset = Rc::new(Timer::default());
    ui.on_copy({
        let app = app.clone();
        let weak = ui.as_weak();
        move |id| {
            app.copy(id as i64);
            let Some(ui) = weak.upgrade() else { return };
            ui.set_copied_id(id);
            let weak = weak.clone();
            // the check mark stays for 1.2 s, as in the design
            copied_reset.start(TimerMode::SingleShot, Duration::from_millis(1200), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_copied_id(-1);
                }
            });
        }
    });
    let debounce = Rc::new(Timer::default());
    ui.on_query_edited({
        let app = app.clone();
        move |q: SharedString| {
            let app = app.clone();
            debounce.start(TimerMode::SingleShot, Duration::from_millis(120), move || {
                *app.query.borrow_mut() = q.to_string();
                app.reload();
            });
        }
    });

    // clips the agent saves while the window is open
    let refresh = Rc::new(Timer::default());
    refresh.start(TimerMode::Repeated, REFRESH, {
        let app = Rc::downgrade(&app);
        move || {
            if let Some(app) = app.upgrade() {
                if app.changed_elsewhere() {
                    app.reload();
                    // the agent counted a copy: Statistics, if open, follows
                    if let Some(w) = app.settings_window.borrow().as_ref() {
                        stats::show(w, &app.history, &app.i18n, chrono::Local::now());
                    }
                }
            }
        }
    });
    app.timers.borrow_mut().push(refresh);
    app
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    select_backend()?;
    agent::ensure_running();

    let dir = clip_core::data_dir().ok_or("no data folder on this OS")?;
    // the global shortcut brings this window forward instead of opening another
    let _running = clip_core::WindowLock::claim(&dir);
    // Settings → General: register (or remove) the agent's start at login
    agent::set_autostart(Settings::load(&dir).launch_at_login);
    let history = Arc::new(History::open(&dir)?);
    let mut sys = system::system_info();
    // design checks in other languages: PASTEZO_LANG=ar cargo run -p pastezo
    if let Ok(lang) = std::env::var("PASTEZO_LANG") {
        sys.languages = vec![lang];
    }
    let i18n = I18n::new(&sys);
    // macOS app menu: "Settings… ⌘," (must be registered before the window exists)
    let app_slot: Rc<RefCell<Option<std::rc::Weak<App>>>> = Rc::default();
    i_slint_backend_winit::set_settings_menu_item(i18n.t("settings.menu", &[]), {
        let slot = app_slot.clone();
        move || {
            if let Some(app) = slot.borrow().as_ref().and_then(|a| a.upgrade()) {
                app.open_settings();
            }
        }
    });
    // macOS: the standard Window menu (⌘M minimizes, ⌘W closes the front window)
    i_slint_backend_winit::set_window_menu(["menu.window", "menu.minimize", "menu.zoom", "menu.close"].map(|k| i18n.t(k, &[])));
    let ui = AppWindow::new()?;
    #[allow(clippy::default_constructed_unit_structs)]
    let clipboard: Arc<dyn ClipboardBackend> = Arc::new(SystemClipboard::default());
    let open_url: OpenUrl = Box::new(|url| {
        if let Err(e) = open::that_detached(url) {
            eprintln!("pastezo: cannot open {url}: {e}");
        }
    });
    let app = wire(&ui, history, i18n, sys.os, clipboard, open_url, Some(dir.clone()));
    *app_slot.borrow_mut() = Some(Rc::downgrade(&app));

    // startup (fonts, translations, first page) leaves freed heap behind;
    // hand it back to the OS once the window is up
    Timer::single_shot(Duration::from_secs(1), clip_core::platform::release_free_memory);

    snapshot::setup(&ui, &app);

    // the window as it was last closed (snapshots: always the design's size)
    let remember_window = std::env::var_os("PASTEZO_SNAPSHOT").is_none();
    let frame = app.settings().window.filter(|_| remember_window);
    if let Some(frame) = &frame {
        restore_window(&ui, frame);
    }
    ui.show()?;
    // the native window exists only once shown
    if app.settings().hide_from_capture {
        capture::apply(ui.window(), true);
    }
    if frame.is_some() {
        keep_on_screen(&ui);
    }
    if frame.is_some_and(|f| f.maximized) {
        ui.window().set_maximized(true);
    }
    if remember_window {
        remember_window_frame(&ui, &app);
    }
    // the chosen app icon, once AppKit has finished launching (set earlier, it is reset)
    let icon = app.settings().icon;
    Timer::single_shot(Duration::ZERO, move || app_icon::apply(icon, true));
    set_window_icon(|i| ui.set_window_icon(i), icon);
    // the OS-drawn parts (title bar, menus) exist only once the window is shown
    apply_theme(&ui, app.settings().theme);
    if let Ok(t) = std::env::var("PASTEZO_THEME") {
        apply_theme(&ui, &t);
    }
    if remember_window {
        app.check_for_update();
    }
    slint::run_event_loop()?;
    // the window is closed: what was deleted can no longer be undone
    app.forget_deleted();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing as testing;
    use std::sync::Mutex;

    /// Stands in for the system clipboard.
    #[derive(Default)]
    pub(crate) struct FakeClipboard(Mutex<Option<ClipContent>>);

    impl ClipboardBackend for FakeClipboard {
        fn change_count(&self) -> i64 {
            0
        }
        fn read(&self) -> Option<ClipContent> {
            self.0.lock().unwrap().clone()
        }
        fn write(&self, c: &ClipContent) -> clip_core::Result<i64> {
            *self.0.lock().unwrap() = Some(c.clone());
            Ok(0)
        }
        fn frontmost_app(&self) -> Option<String> {
            None
        }
    }

    pub(crate) fn sys() -> system::SystemInfo {
        system::SystemInfo {
            os: "macos",
            languages: vec!["en-US".into()],
            region: Some("US".into()),
            hour12: Some(true),
            time_locale: None,
        }
    }

    fn previews(ui: &AppWindow) -> Vec<String> {
        ui.get_clips().iter().map(|c| c.preview.to_string()).collect()
    }

    /// Everything a user does in the window, against a real (temporary) database.
    #[test]
    fn window_actions() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        // the agent: a second connection to the same database, as the other process
        let agent = History::open(dir.path()).unwrap();
        for t in ["831 310", "https://example.com/", "Albuquerque"] {
            agent.add(&ClipContent::Text(t.into()), Some("Safari")).unwrap();
        }

        let ui = AppWindow::new().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let opened = Rc::new(RefCell::new(Vec::<String>::new()));
        let open_url: OpenUrl = Box::new({
            let opened = opened.clone();
            move |u| opened.borrow_mut().push(u.to_string())
        });
        let _app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", clipboard.clone(), open_url, None);
        assert_eq!(previews(&ui), ["Albuquerque", "https://example.com/", "831 310"]);
        let first = ui.get_clips().row_data(0).unwrap();
        assert_eq!(first.source.row_data(1).unwrap().text, "Safari");
        assert!(first.time.row_data(0).unwrap().text == "Today");

        // copy: the full text goes to the clipboard, the row keeps its place,
        // and the agent does not record it again
        let copy_buttons: Vec<_> = testing::ElementHandle::find_by_accessible_label(&ui, "Copy").collect();
        assert_eq!(copy_buttons.len(), 3);
        copy_buttons[2].invoke_accessible_default_action();
        assert_eq!(*clipboard.0.lock().unwrap(), Some(ClipContent::Text("831 310".into())));
        assert_eq!(ui.get_copied_id(), ui.get_clips().row_data(2).unwrap().id);
        assert!(ui.get_toast_shown());
        assert_eq!(ui.get_toast().kind, ToastKind::Success);
        assert_eq!(ui.get_toast().text, "Copied");
        assert!(agent.add(&ClipContent::Text("831 310".into()), Some("Pastezo")).unwrap().is_none());
        testing::mock_elapsed_time(Duration::from_millis(1300));
        assert_eq!(ui.get_copied_id(), -1);
        testing::mock_elapsed_time(Duration::from_millis(800));
        assert!(!ui.get_toast_shown(), "success toast hides after 2 s");
        assert_eq!(previews(&ui)[2], "831 310");

        // link: opened in the browser, never in the app
        testing::ElementHandle::find_by_accessible_label(&ui, "https://example.com/")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(*opened.borrow(), ["https://example.com/"]);

        // delete: gone from the database at once, from the list after the animation
        testing::ElementHandle::find_by_accessible_label(&ui, "Delete").next().unwrap().invoke_accessible_default_action();
        assert!(ui.get_clips().row_data(0).unwrap().removing);
        testing::mock_elapsed_time(REMOVE_ANIMATION + Duration::from_millis(20));
        assert_eq!(previews(&ui), ["https://example.com/", "831 310"]);
        assert_eq!(history.list(0, 10).unwrap().len(), 2);

        // a copy recorded by the agent shows up while the window is open
        agent.add(&ClipContent::Text("fresh".into()), Some("Notes")).unwrap();
        testing::mock_elapsed_time(REFRESH + Duration::from_millis(50));
        assert_eq!(previews(&ui)[0], "fresh");

        // search (debounced), then back to the full list
        ui.invoke_query_edited("xampl".into());
        testing::mock_elapsed_time(Duration::from_millis(200));
        assert_eq!(previews(&ui), ["https://example.com/"]);
        assert!(ui.get_searching());
        // nothing found: empty list while searching -> the "Nothing found" text
        ui.invoke_query_edited("zzqqxx".into());
        testing::mock_elapsed_time(Duration::from_millis(200));
        assert!(previews(&ui).is_empty() && ui.get_searching());
        assert!(testing::ElementHandle::find_by_element_type_name(&ui, "Text")
            .any(|t| t.accessible_label().as_deref() == Some("Nothing found")));
        ui.invoke_query_edited("".into());
        testing::mock_elapsed_time(Duration::from_millis(200));
        assert_eq!(previews(&ui).len(), 3);
        assert!(!ui.get_searching());
    }

    /// Text selected in a row is copied with ⌘C (just the selection), and the
    /// copy does not come back as a new clip.
    #[test]
    fn copying_selected_text() {
        use slint::platform::{Key, PointerEventButton, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let agent = History::open(dir.path()).unwrap();
        agent.add(&ClipContent::Text("Hello world".into()), Some("Notes")).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let clipboard = Arc::new(FakeClipboard::default());
        let _app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", clipboard.clone(), Box::new(|_| {}), None);

        // the selectable layer exists only under the mouse
        let selectable = || testing::ElementHandle::find_by_element_id(&ui, "ClipRow::selectable").next();
        assert!(selectable().is_none());
        let shown = testing::ElementHandle::find_by_element_id(&ui, "ClipRow::shown").next().unwrap();
        let (p, sz) = (shown.absolute_position(), shown.size());
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(p.x + 5., p.y + sz.height / 2.) });
        // drag across the start of the text
        let text = selectable().expect("created on hover");
        let (at, size) = (text.absolute_position(), text.size());
        let y = at.y + size.height / 2.;
        let press = |x: f32, e: fn(slint::LogicalPosition) -> WindowEvent| ui.window().dispatch_event(e(slint::LogicalPosition::new(x, y)));
        press(at.x + 1., |position| WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        press(at.x + 40., |position| WindowEvent::PointerMoved { position });
        press(at.x + 40., |position| WindowEvent::PointerReleased { position, button: PointerEventButton::Left });

        let key = |k: slint::SharedString, down: bool| {
            ui.window().dispatch_event(if down { WindowEvent::KeyPressed { text: k } } else { WindowEvent::KeyReleased { text: k } })
        };
        let copy = || {
            key(Key::Control.into(), true);
            key("c".into(), true);
            key("c".into(), false);
            key(Key::Control.into(), false);
            match clipboard.0.lock().unwrap().take() {
                Some(ClipContent::Text(t)) => t,
                other => panic!("nothing copied: {other:?}"),
            }
        };
        // a new clip arrives while the text is selected: the list shifts, the
        // selection stays with its clip
        agent.add(&ClipContent::Text("Something newer".into()), Some("Notes")).unwrap();
        testing::mock_elapsed_time(REFRESH + Duration::from_millis(50));
        assert_eq!(previews(&ui), ["Something newer", "Hello world"]);
        let copied = copy();
        assert!(!copied.is_empty() && "Hello world".starts_with(&copied) && copied.len() < 11, "{copied:?}");
        // later the focus leaves the text (no caret blinking); the selection stays
        testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(copy(), copied);
        // it stays while it holds the selection, even with the mouse gone
        ui.window().dispatch_event(WindowEvent::PointerExited);
        assert!(selectable().is_some());
        // the agent sees the clipboard change and skips it
        assert!(agent.add(&ClipContent::Text(copied), Some("Pastezo")).unwrap().is_none());
        assert_eq!(history.list(0, 10).unwrap().len(), 2, "no clip for the copied piece");
    }

    /// The pin keeps a clip at the top of the list, newer copies below it;
    /// pressing it again puts the clip back in its place.
    #[test]
    fn pinned_clips_stay_on_top() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for t in ["first", "second", "third"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        let _app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        // hidden for now (Features.pins): switched on here
        Features::get(&ui).set_pins(true);
        assert_eq!(previews(&ui), ["third", "second", "first"]);
        let pins = || testing::ElementHandle::find_by_accessible_label(&ui, "Pin").collect::<Vec<_>>();
        pins()[2].invoke_accessible_default_action();
        assert_eq!(previews(&ui), ["first", "third", "second"]);
        assert!(ui.get_clips().row_data(0).unwrap().pinned);
        // a new copy (saved by the agent, another connection) goes under the pinned one
        let agent = History::open(dir.path()).unwrap();
        agent.add(&ClipContent::Text("fourth".into()), None).unwrap();
        testing::mock_elapsed_time(REFRESH + Duration::from_millis(50));
        assert_eq!(previews(&ui)[..2], ["first", "fourth"]);
        testing::ElementHandle::find_by_accessible_label(&ui, "Unpin").next().unwrap().invoke_accessible_default_action();
        assert_eq!(previews(&ui), ["fourth", "third", "second", "first"]);
        assert!(!ui.get_clips().row_data(3).unwrap().pinned);
    }

    /// A click anywhere on a row copies it, on its text too (a click, not a
    /// drag: dragging selects); the buttons and links keep their own actions.
    #[test]
    fn clicking_a_row_copies_it() {
        use slint::platform::{PointerEventButton, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        history.add(&ClipContent::Text("Hello world".into()), None).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let clipboard = Arc::new(FakeClipboard::default());
        let app = wire(&ui, history, I18n::new(&sys()), "macos", clipboard.clone(), Box::new(|_| {}), None);
        ui.show().unwrap();
        let click = |x: f32, y: f32| {
            let p = slint::LogicalPosition::new(x, y);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: p });
            ui.window().dispatch_event(WindowEvent::PointerPressed { position: p, button: PointerEventButton::Left });
            ui.window().dispatch_event(WindowEvent::PointerReleased { position: p, button: PointerEventButton::Left });
        };
        let copied = || clipboard.0.lock().unwrap().take();
        let shown = testing::ElementHandle::find_by_element_id(&ui, "ClipRow::shown").next().unwrap();
        let (at, size) = (shown.absolute_position(), shown.size());

        // the grey area right of the text, left of the buttons
        click(at.x + size.width - 40., at.y + size.height / 2.);
        assert_eq!(copied(), Some(ClipContent::Text("Hello world".into())));
        testing::mock_elapsed_time(Duration::from_millis(2500));

        // on the text: copied once the click has settled
        click(at.x + 10., at.y + size.height / 2.);
        assert_eq!(copied(), None, "not before the click settles");
        testing::mock_elapsed_time(Duration::from_millis(350));
        assert_eq!(copied(), Some(ClipContent::Text("Hello world".into())));
        testing::mock_elapsed_time(Duration::from_millis(2500));
        // the keys still work after it (the text had the focus and gave it up)
        let cmd_f = || {
            use slint::platform::Key;
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: "f".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: "f".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        };
        cmd_f();
        assert!(ui.get_search_open(), "⌘F after a click on the text");
        testing::mock_elapsed_time(Duration::from_millis(300));
        cmd_f();
        assert!(!ui.get_search_open());

        // press, hold still, then drag: a selection, not a click (nothing copied)
        let y = at.y + size.height / 2.;
        let p = |x: f32| slint::LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: p(at.x + 1.) });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position: p(at.x + 1.), button: PointerEventButton::Left });
        testing::mock_elapsed_time(Duration::from_millis(600));
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: p(at.x + 40.) });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position: p(at.x + 40.), button: PointerEventButton::Left });
        testing::mock_elapsed_time(Duration::from_millis(600));
        assert_eq!(copied(), None, "holding before a drag must not copy the row");

        // the row holding the selection (and the focus) goes away: the keys
        // still reach the window
        app.clear_history();
        testing::mock_elapsed_time(Duration::from_millis(1000));
        assert_eq!(ui.get_clips().row_count(), 0);
        cmd_f();
        assert!(ui.get_search_open(), "⌘F after the focused row was removed");

    }

    /// ⌘F opens and closes the search from the start and wherever the focus
    /// is, including a text being selected in a row.
    #[test]
    fn cmd_f_works_everywhere() {
        use slint::platform::{Key, PointerEventButton, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        history.add(&ClipContent::Text("Hello world".into()), None).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        ui.show().unwrap();
        let cmd_f = || {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: "f".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: "f".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        };
        let click = |p: slint::LogicalPosition| {
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: p });
            ui.window().dispatch_event(WindowEvent::PointerPressed { position: p, button: PointerEventButton::Left });
            ui.window().dispatch_event(WindowEvent::PointerReleased { position: p, button: PointerEventButton::Left });
        };
        cmd_f();
        assert!(ui.get_search_open(), "right after start");
        cmd_f();
        assert!(!ui.get_search_open());

        let shown = testing::ElementHandle::find_by_element_id(&ui, "ClipRow::shown").next().unwrap();
        let at = shown.absolute_position();
        click(slint::LogicalPosition::new(at.x + 5., at.y + 5.));
        cmd_f();
        assert!(ui.get_search_open(), "with the row text focused");
        cmd_f();
        click(slint::LogicalPosition::new(at.x + 5., at.y + 5.));
        testing::mock_elapsed_time(Duration::from_millis(600));
        cmd_f();
        assert!(ui.get_search_open(), "after the focus moved off the text");
    }

    /// Settings → Themes: a card switches the theme (and saves it); light by default.
    #[test]
    fn choosing_a_theme() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        w.set_tab(2);
        let press = |label: &str| {
            testing::ElementHandle::find_by_accessible_label(&w, label).next().unwrap().invoke_accessible_default_action()
        };
        assert_eq!(app.settings().theme, themes::LIGHT, "light by default");
        assert_eq!(w.get_themes().row_count(), themes::THEMES.len());
        press("Dark");
        assert_eq!(app.settings().theme, themes::DARK);
        assert!(Theme::get(&ui).get_dark());
        assert_eq!(w.get_theme_id(), themes::DARK);
        press("Light");
        assert_eq!(app.settings().theme, themes::LIGHT);
        assert!(!Theme::get(&ui).get_dark());
        // one of the added themes (cards further down are created as they scroll
        // into view): its colours reach the list window
        w.invoke_theme_chosen("nightshade".into());
        assert_eq!(app.settings().theme, "nightshade");
        let colors = Theme::get(&ui).get_colors();
        assert_eq!(colors, themes::find("nightshade").unwrap().colors());
        assert!(colors.dark);
    }

    /// Settings → Typography: the sliders restyle the list (kept within their
    /// ranges and steps), "Reset font" brings the design back.
    #[test]
    fn typography_restyles_the_list() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        w.set_tab(1);
        assert_eq!(Env::get(&w).invoke_format_em(1.45), "1.45 em");
        assert_eq!(ClipText::get(&ui).get_size(), 17.0);

        w.set_text_font("Georgia".into());
        w.set_text_size(20.0);
        w.set_text_line(1.63); // not on a step: snapped
        w.set_text_gap(100.0); // out of range: clamped
        w.invoke_text_style_edited();
        let g = ClipText::get(&ui);
        assert_eq!((g.get_font().as_str(), g.get_size(), g.get_line(), g.get_gap()), ("Georgia", 20.0, 1.65, 42.0));
        assert_eq!(w.get_text_line(), 1.65, "the window shows the kept value");
        assert_eq!(app.settings().text.font, "Georgia");

        assert_eq!(w.get_text_preset(), "");
        w.invoke_text_preset_chosen("compact".into());
        assert_eq!(app.settings().text, TextStyle { font: "Georgia".into(), size: 14.0, line: 1.35, gap: 14.0 }, "the font stays");
        assert_eq!((w.get_text_size(), w.get_text_preset().as_str()), (14.0, "compact"));

        testing::ElementHandle::find_by_accessible_label(&w, "Reset font").next().unwrap().invoke_accessible_default_action();
        assert_eq!(app.settings().text, TextStyle::default());
        assert_eq!(ClipText::get(&ui).get_size(), 17.0);
    }

    /// The list from the keyboard: arrows select, ⌘C copies, ⌫ deletes, typing
    /// searches (↓ to the results), Space previews the whole clip, Return pastes.
    /// ↑/↓ start from the clip under the mouse, then go on from the selection.
    #[test]
    fn arrows_start_from_the_hovered_clip() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for t in ["one", "two", "three", "four", "five", "six"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 1400.));
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        ui.show().unwrap();
        let key = |k: slint::SharedString| {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: k.clone() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: k });
        };
        let id_of = |i: usize| ui.get_clips().row_data(i).unwrap().id;
        let mut rows: Vec<_> = testing::ElementHandle::find_by_element_id(&ui, "ClipRow::shown").collect();
        rows.sort_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y));
        let hover = |row: usize| {
            let (at, size) = (rows[row].absolute_position(), rows[row].size());
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(at.x + size.width - 60., at.y + size.height / 2.) });
        };

        hover(2);
        key(Key::DownArrow.into());
        assert_eq!(ui.get_selected_id(), id_of(3), "the one below the hovered clip");
        key(Key::DownArrow.into());
        assert_eq!(ui.get_selected_id(), id_of(4), "then on from the selection, the mouse resting");
        hover(1);
        key(Key::UpArrow.into());
        assert_eq!(ui.get_selected_id(), id_of(0), "the one above the newly hovered clip");
    }

    #[test]
    fn the_list_works_from_the_keyboard() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let long = format!("long {}", "word ".repeat(1000)); // more than the list shows
        for t in ["apple", "banana", long.as_str()] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let clipboard = Arc::new(FakeClipboard::default());
        let app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", clipboard.clone(), Box::new(|_| {}), None);
        let pasted = Rc::new(Cell::new(0));
        *app.paste_into.borrow_mut() = Box::new({
            let pasted = pasted.clone();
            move |_| pasted.set(pasted.get() + 1)
        });
        ui.show().unwrap();
        let key = |k: slint::SharedString| {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: k.clone() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: k });
        };
        let copied = || match clipboard.0.lock().unwrap().take() {
            Some(ClipContent::Text(t)) => Some(t),
            _ => None,
        };
        let id_of = |i: usize| ui.get_clips().row_data(i).unwrap().id;

        key(Key::DownArrow.into());
        assert_eq!(ui.get_selected_id(), id_of(0));
        key(Key::DownArrow.into());
        assert_eq!(ui.get_selected_id(), id_of(1), "banana");
        // Return pastes into the previous app; ⌘C only copies
        key(Key::Return.into());
        assert_eq!(copied().as_deref(), Some("banana"));
        assert_eq!(pasted.get(), 1);
        // Return in the preview pastes too, and closes it
        key(" ".into());
        assert!(ui.get_preview_open());
        key(Key::Return.into());
        assert!(!ui.get_preview_open());
        assert_eq!(copied().as_deref(), Some("banana"));
        assert_eq!(pasted.get(), 2);
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        key("c".into());
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert_eq!(copied().as_deref(), Some("banana"));
        assert_eq!(pasted.get(), 2, "⌘C does not paste");

        // Space: the whole text of the long clip, not the list's preview
        key(Key::UpArrow.into());
        key(" ".into());
        assert!(ui.get_preview_open());
        assert_eq!(ui.get_preview_text().as_str(), long);
        key(" ".into());
        assert!(!ui.get_preview_open());

        // ⌫ deletes the selected clip; the selection moves to the next one
        key(Key::Backspace.into());
        testing::mock_elapsed_time(REMOVE_ANIMATION + Duration::from_millis(20));
        assert_eq!(previews(&ui), ["banana", "apple"]);
        assert_eq!(ui.get_selected_id(), id_of(0));

        // typing opens the search with the letters typed
        key("a".into());
        key("p".into());
        assert!(ui.get_search_open());
        assert_eq!(ui.get_query().as_str(), "ap");
        testing::mock_elapsed_time(Duration::from_millis(200));
        assert_eq!(previews(&ui), ["apple"]);
        // Return in the search field pastes the first result
        key(Key::Return.into());
        assert_eq!(copied().as_deref(), Some("apple"));
        assert_eq!(pasted.get(), 3);
    }

    /// ⌘1…⌘9 paste the N-th clip of the list, from the search field too; rows
    /// on their way out do not count.
    #[test]
    fn cmd_digit_pastes_the_nth_clip() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for t in ["one", "two", "three", "four"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let clipboard = Arc::new(FakeClipboard::default());
        let app = wire(&ui, history, I18n::new(&sys()), "macos", clipboard.clone(), Box::new(|_| {}), None);
        let pasted = Rc::new(Cell::new(0));
        *app.paste_into.borrow_mut() = Box::new({
            let pasted = pasted.clone();
            move |_| pasted.set(pasted.get() + 1)
        });
        ui.show().unwrap();
        let cmd = |k: &str| {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: k.into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: k.into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        };
        let copied = || match clipboard.0.lock().unwrap().take() {
            Some(ClipContent::Text(t)) => Some(t),
            _ => None,
        };

        cmd("3");
        assert_eq!(copied().as_deref(), Some("two"));
        assert_eq!(pasted.get(), 1);
        cmd("9");
        assert_eq!((copied(), pasted.get()), (None, 1), "no 9th clip");
        cmd("0");
        assert_eq!((copied(), pasted.get()), (None, 1), "⌘0 is not a clip");

        // the first row is being deleted: ⌘1 is the next one
        app.delete(ui.get_clips().row_data(0).unwrap().id as i64);
        cmd("1");
        assert_eq!(copied().as_deref(), Some("three"));

        // from the search field, among the results
        cmd("f");
        ui.invoke_query_edited("o".into());
        testing::mock_elapsed_time(Duration::from_millis(200));
        assert!(ui.get_search_focused());
        cmd("2");
        assert_eq!(copied().as_deref(), Some("one"));
        assert_eq!(pasted.get(), 3);
    }

    /// The settings work from the keyboard (macOS controls are drawn by us):
    /// Tab moves between controls, Space toggles, arrows move a slider.
    #[test]
    fn ctrl_w_closes_the_settings_on_windows_and_linux() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "linux", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        // after a tab switch too (the focused control is gone with its tab)
        w.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
        let tab = testing::ElementHandle::find_by_accessible_label(&w, "Typography").next().unwrap();
        tab.invoke_accessible_default_action();
        assert_eq!(w.get_tab(), 1);
        let ctrl: slint::SharedString = Key::Control.into();
        w.window().dispatch_event(WindowEvent::KeyPressed { text: ctrl.clone() });
        w.window().dispatch_event(WindowEvent::KeyPressed { text: "w".into() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text: "w".into() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text: ctrl });
        testing::mock_elapsed_time(Duration::from_millis(1));
        assert!(app.settings_window.borrow().is_none());
    }

    /// Settings → Statistics shows what was copied today, by kind.
    #[test]
    fn statistics_count_the_copies() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for t in ["one", "two", "one", "https://pastezo.app"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert_eq!(w.get_stats_total(), "4");
        assert_eq!(w.get_stats_total_label(), "copies in total");
        assert_eq!((w.get_stats_today(), w.get_stats_week(), w.get_stats_month()), ("4".into(), "4".into(), "4".into()));
        let today = w.get_stats_days().row_data(0).unwrap();
        assert_eq!((today.text, today.link, today.image, today.total), (3, 1, 0, 4));
        assert_eq!(w.get_stats_week_max(), 4);
        assert_eq!(w.get_stats_month_days().row_count(), 30);
        assert_eq!(w.get_stats_month_days().row_data(29), Some(4));
    }

    /// Settings → General → "Open Pastezo at login" is kept in settings.json
    /// (on by default) and shown again next time.
    #[test]
    fn launch_at_login_is_saved() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), Some(dir.path().into()));
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert!(w.get_launch_at_login(), "on by default");
        let check = testing::ElementHandle::find_by_accessible_label(&w, "Open Pastezo at login").next().unwrap();
        check.invoke_accessible_default_action();
        assert!(!w.get_launch_at_login());
        assert!(!Settings::load(dir.path()).launch_at_login);
        app.close_settings();
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert!(!w.get_launch_at_login(), "shown as saved");
    }

    /// Settings → General → "Hide Pastezo during screen sharing": off by
    /// default, kept in settings.json, shown again next time.
    #[test]
    fn hide_from_capture_is_saved() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), Some(dir.path().into()));
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert_eq!(Env::get(&w).get_can_hide_from_capture(), capture::AVAILABLE);
        if !capture::AVAILABLE {
            return;
        }
        assert!(!w.get_hide_from_capture(), "off by default");
        let check = testing::ElementHandle::find_by_accessible_label(&w, "Hide Pastezo during screen sharing").next().unwrap();
        check.invoke_accessible_default_action();
        assert!(w.get_hide_from_capture());
        assert!(Settings::load(dir.path()).hide_from_capture);
        app.close_settings();
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        assert!(w.get_hide_from_capture(), "shown as saved");
    }

    #[test]
    fn settings_work_from_the_keyboard() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        let key = |k: slint::SharedString| {
            w.window().dispatch_event(WindowEvent::KeyPressed { text: k.clone() });
            w.window().dispatch_event(WindowEvent::KeyReleased { text: k });
        };
        // General: the first control is the launch check box
        key(Key::Tab.into());
        let launch = testing::ElementHandle::find_by_accessible_label(&w, "Open Pastezo at login").next().unwrap();
        assert_eq!(launch.accessible_checked(), Some(true));
        key(" ".into());
        assert_eq!(launch.accessible_checked(), Some(false));

        // Typography: "Aa", the three presets, then the font size slider
        w.set_tab(1);
        for _ in 0..5 {
            key(Key::Tab.into());
        }
        key(Key::RightArrow.into());
        assert_eq!(ClipText::get(&ui).get_size(), 18.0);
        key(Key::LeftArrow.into());
        key(Key::LeftArrow.into());
        assert_eq!(ClipText::get(&ui).get_size(), 16.0);
    }

    /// Settings → App Icon: every variant is offered with its preview; a click
    /// saves the choice.
    #[test]
    fn choosing_an_app_icon() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        w.set_tab(3);
        assert_eq!(app.settings().icon, app_icon::DEFAULT);
        assert_eq!(w.get_icons().row_count(), app_icon::VARIANTS.len());
        assert!(w.get_icons().iter().all(|c| c.preview.size().width > 0), "a preview is missing");
        // (applying needs the main thread: in tests it changes nothing on the system)
        w.invoke_icon_chosen("sky".into());
        assert_eq!(app.settings().icon, "sky");
        assert_eq!(w.get_icon_id(), "sky");
        w.invoke_icon_chosen("nonexistent".into());
        assert_eq!(app.settings().icon, "sky");
    }

    /// Settings → General → "Clear all": asks once more, then empties the
    /// history and the list; "Cancel" keeps everything.
    #[test]
    fn clearing_the_history() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for t in ["one", "two"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        let press = |label: &str| {
            testing::ElementHandle::find_by_accessible_label(&w, label).next().unwrap().invoke_accessible_default_action()
        };

        press("Clear all");
        assert!(w.get_confirming_clear());
        press("Cancel");
        assert!(!w.get_confirming_clear());
        assert_eq!(previews(&ui).len(), 2);

        press("Clear all");
        press("Delete");
        assert!(!w.get_confirming_clear());
        assert!(previews(&ui).is_empty());
        assert!(history.list(0, 10).unwrap().is_empty());
    }

    /// Settings → General → Keep clips: the choice is saved for the agent and
    /// the clips already too old leave the list at once; pinned ones stay.
    #[test]
    fn keeping_clips_for_a_while() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let day = 24 * 60 * 60 * 1000;
        let now = chrono::Utc::now().timestamp_millis();
        history.import(&ClipContent::Text("old".into()), None, now - 10 * day, false).unwrap();
        history.import(&ClipContent::Text("old pinned".into()), None, now - 10 * day, true).unwrap();
        history.add(&ClipContent::Text("new".into()), None).unwrap();
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        let press = |label: &str| {
            testing::ElementHandle::find_by_accessible_label(&w, label).next().unwrap().invoke_accessible_default_action()
        };
        assert_eq!(w.get_keep_days(), 0, "for ever by default");
        assert_eq!(previews(&ui).len(), 3);

        press("A month");
        assert_eq!((history.keep_days(), w.get_keep_days()), (Some(30), 30));
        assert_eq!(previews(&ui).len(), 3);

        press("A week");
        assert_eq!(history.keep_days(), Some(7));
        assert_eq!(previews(&ui), ["old pinned", "new"]);

        press("Forever");
        assert_eq!((history.keep_days(), w.get_keep_days()), (None, 0));
    }

    /// Settings → Import & Export: the history goes to a file and comes back
    /// into another history (in the background); the settings window reports.
    #[test]
    fn exporting_and_importing_the_history() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(&dir.path().join("a")).unwrap());
        for t in ["one", "two"] {
            history.add(&ClipContent::Text(t.into()), Some("Notes")).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        let app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app.open_settings();
        let w = app.settings_window.borrow().as_ref().unwrap().clone_strong();
        testing::ElementHandle::find_by_accessible_label(&w, "Import & Export").next().unwrap().invoke_accessible_default_action();
        assert_eq!(w.get_tab(), 4);
        assert!(testing::ElementHandle::find_by_accessible_label(&w, "Export History…").next().is_some());
        let wait = |app: &App| {
            for _ in 0..500 {
                if !app.busy.get() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
                testing::mock_elapsed_time(Duration::from_millis(100));
            }
            panic!("still busy");
        };

        let file = dir.path().join("history.json");
        app.export_to(file.clone());
        wait(&app);
        assert_eq!(w.get_toast().text, "Clips exported: 2");
        assert_eq!(w.get_toast().kind, ToastKind::Success);

        // another history, with one of the two already in it
        let other = Arc::new(History::open(&dir.path().join("b")).unwrap());
        other.add(&ClipContent::Text("one".into()), None).unwrap();
        let ui2 = AppWindow::new().unwrap();
        let app2 = wire(&ui2, other, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        app2.import_from(file.clone());
        wait(&app2);
        assert_eq!(ui2.get_toast().text, "New clips added: 1", "no settings window: the list's toast");
        assert_eq!(previews(&ui2), ["one", "two"]);

        std::fs::write(&file, "not json").unwrap();
        app2.import_from(file);
        wait(&app2);
        // after the previous toast has gone
        testing::mock_elapsed_time(Duration::from_millis(2100));
        testing::mock_elapsed_time(Duration::from_millis(300));
        assert_eq!(ui2.get_toast().kind, ToastKind::Error);
    }

    /// A deleted clip comes back with "Undo" in the toast or ⌘Z, in its place
    /// and selected; after the undo time it is gone for good, image file too.
    #[test]
    fn undoing_a_deletion() {
        use slint::platform::{Key, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let png = {
            let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let (image, _) = history.add(&ClipContent::Image(png), None).unwrap().unwrap();
        for t in ["one", "two"] {
            history.add(&ClipContent::Text(t.into()), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        let _app = wire(&ui, history.clone(), I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        ui.show().unwrap();
        let texts = || previews(&ui).into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>();
        let press = |label: &str| testing::ElementHandle::find_by_accessible_label(&ui, label).next().unwrap().invoke_accessible_default_action();

        // delete "two" (first row): the toast offers to undo
        testing::ElementHandle::find_by_accessible_label(&ui, "Delete").next().unwrap().invoke_accessible_default_action();
        testing::mock_elapsed_time(REMOVE_ANIMATION + Duration::from_millis(20));
        assert_eq!(texts(), ["one"]);
        assert_eq!((ui.get_toast().text.as_str(), ui.get_toast().action.as_str()), ("Deleted", "Undo"));
        press("Undo");
        assert_eq!(texts(), ["two", "one"]);
        assert_eq!(ui.get_selected_id(), ui.get_clips().row_data(0).unwrap().id);
        assert!(!ui.get_toast_shown());

        // two deletions, ⌘Z twice: both back
        for _ in 0..2 {
            testing::ElementHandle::find_by_accessible_label(&ui, "Delete").next().unwrap().invoke_accessible_default_action();
            testing::mock_elapsed_time(REMOVE_ANIMATION + Duration::from_millis(20));
        }
        assert!(texts().is_empty());
        let cmd_z = || {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: "z".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: "z".into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        };
        cmd_z();
        assert_eq!(texts(), ["one"]);
        cmd_z();
        assert_eq!(texts(), ["two", "one"]);

        // the image: deleted, not undone — its file stays for the undo time, then goes
        let file = std::path::PathBuf::from(image.image_path.unwrap());
        let last = ui.get_clips().row_count() - 1;
        let delete_buttons: Vec<_> = testing::ElementHandle::find_by_accessible_label(&ui, "Delete").collect();
        delete_buttons[last].invoke_accessible_default_action();
        testing::mock_elapsed_time(Duration::from_secs(4));
        assert!(file.exists());
        testing::mock_elapsed_time(Duration::from_secs(2));
        assert!(!file.exists());
        cmd_z();
        assert_eq!(history.list(0, 10).unwrap().len(), 2, "too late to undo");
    }

    /// The search field understands filters (`type:`, `app:`) and forgives typos.
    #[test]
    fn searching_with_filters_and_typos() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        history.add(&ClipContent::Text("Albuquerque, New Mexico".into()), Some("Notes")).unwrap();
        history.add(&ClipContent::Text("https://example.com/albuquerque".into()), Some("Safari")).unwrap();
        let ui = AppWindow::new().unwrap();
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        let search = |q: &str| {
            ui.invoke_query_edited(q.into());
            testing::mock_elapsed_time(Duration::from_millis(200));
            previews(&ui)
        };
        assert_eq!(search("Albuqerque"), ["https://example.com/albuquerque", "Albuquerque, New Mexico"], "both with a typo: newest first");
        assert_eq!(search("albuquerque type:link"), ["https://example.com/albuquerque"]);
        assert_eq!(search("app:notes"), ["Albuquerque, New Mexico"]);
        assert!(search("app:Figma").is_empty() && ui.get_searching());
    }

    /// The search field slides out of the magnifier (animated width), takes the
    /// focus, and slides back in on Escape, clearing the query.
    #[test]
    fn search_field_slides() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let ui = AppWindow::new().unwrap();
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        let bx = || testing::ElementHandle::find_by_element_id(&ui, "AppWindow::search-box").next().unwrap();
        let width = || bx().size().width;
        assert_eq!(width(), 0.0);

        testing::ElementHandle::find_by_accessible_label(&ui, "Search").next().unwrap().invoke_accessible_default_action();
        assert!(ui.get_search_open());
        testing::mock_elapsed_time(Duration::from_millis(60));
        let mid = width();
        assert!(mid > 0.0 && mid < 252.0, "mid-animation width {mid}");
        testing::mock_elapsed_time(Duration::from_millis(300));
        assert_eq!(width(), 252.0);
        assert!(ui.get_search_focused());

        ui.set_query("abc".into());
        ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
        assert!(!ui.get_search_open());
        assert_eq!(ui.get_query(), "");
        testing::mock_elapsed_time(Duration::from_millis(300));
        assert_eq!(width(), 0.0);
    }

    /// Press on an image preview and move a few pixels: one drag request.
    #[test]
    fn dragging_an_image_starts_once() {
        use slint::platform::{PointerEventButton, WindowEvent};
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        let png = {
            let img = image::RgbaImage::from_pixel(400, 200, image::Rgba([200, 30, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        history.add(&ClipContent::Image(png), Some("Preview")).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(900.0, 720.0));
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        let requests = Rc::new(RefCell::new(Vec::new()));
        ui.on_drag_image({
            let r = requests.clone();
            move |id| r.borrow_mut().push(id)
        });

        let area = testing::ElementHandle::find_by_element_id(&ui, "ClipRow::drag-area").next().unwrap();
        let (pos, size) = (area.absolute_position(), area.size());
        let at = |dx: f32| slint::LogicalPosition::new(pos.x + size.width / 2.0 + dx, pos.y + size.height / 2.0);
        let w = ui.window();
        w.dispatch_event(WindowEvent::PointerMoved { position: at(0.0) });
        w.dispatch_event(WindowEvent::PointerPressed { position: at(0.0), button: PointerEventButton::Left });
        w.dispatch_event(WindowEvent::PointerMoved { position: at(2.0) });
        assert!(requests.borrow().is_empty(), "a tiny move is not a drag");
        w.dispatch_event(WindowEvent::PointerMoved { position: at(10.0) });
        w.dispatch_event(WindowEvent::PointerMoved { position: at(30.0) });
        assert_eq!(requests.borrow().len(), 1, "one drag per gesture");
        w.dispatch_event(WindowEvent::PointerReleased { position: at(30.0), button: PointerEventButton::Left });
    }

    #[test]
    fn text_direction() {
        assert!(is_rtl_text("مرحبا بالعالم"));
        assert!(is_rtl_text("  123 שלום"));
        assert!(!is_rtl_text("Albuquerque"));
        assert!(!is_rtl_text("Привет"));
        assert!(!is_rtl_text("你好"));
        assert!(!is_rtl_text("123"));
    }

    #[test]
    fn pages_load_as_the_list_scrolls() {
        testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(History::open(dir.path()).unwrap());
        for i in 0..120 {
            history.add(&ClipContent::Text(format!("clip {i}")), None).unwrap();
        }
        let ui = AppWindow::new().unwrap();
        let _app = wire(&ui, history, I18n::new(&sys()), "macos", Arc::new(FakeClipboard::default()), Box::new(|_| {}), None);
        assert_eq!(ui.get_clips().row_count(), PAGE as usize);
        // scrolling with the wheel towards the end loads the next page
        ui.window().set_size(slint::LogicalSize::new(900., 720.));
        ui.show().unwrap();
        let at = slint::LogicalPosition::new(450., 400.);
        for _ in 0..200 {
            ui.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled { position: at, delta_x: 0., delta_y: -120. });
            if ui.get_clips().row_count() > PAGE as usize {
                break;
            }
        }
        assert!(ui.get_clips().row_count() > PAGE as usize, "no page loaded while scrolling");
        ui.invoke_load_more();
        ui.invoke_load_more();
        ui.invoke_load_more();
        assert_eq!(ui.get_clips().row_count(), 120);
        assert_eq!(previews(&ui)[119], "clip 0");
    }
}
