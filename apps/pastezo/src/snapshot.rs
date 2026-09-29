//! Window snapshots for design checks without access to the screen:
//! `PASTEZO_SNAPSHOT=/path/shot.png cargo run -p pastezo`, plus options:
//! `PASTEZO_LANG`, `PASTEZO_THEME=<theme id>` (not saved),
//! `PASTEZO_SNAPSHOT_SEARCH=<ms>`, `PASTEZO_SNAPSHOT_TOAST=<kind|undo|update>`,
//! `PASTEZO_SNAPSHOT_REMOVE=<ms>`, `PASTEZO_SNAPSHOT_QUERY=<text>`,
//! `PASTEZO_SNAPSHOT_SETTINGS=<tab>` (shoots the settings window instead),
//! `PASTEZO_SNAPSHOT_CONFIRM=1` (there: "Clear all" already pressed once),
//! `PASTEZO_SNAPSHOT_PREVIEW=<row>` (that row selected and previewed, Space).

use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, Timer};

use crate::{App, AppWindow, ToastKind};

fn save(shot: Result<slint::SharedPixelBuffer<slint::Rgba8Pixel>, slint::PlatformError>, path: &str) {
    if let Ok(shot) = shot {
        let _ = image::save_buffer(path, shot.as_bytes(), shot.width(), shot.height(), image::ExtendedColorType::Rgba8);
    }
    if std::env::var_os("PASTEZO_STAY").is_none() {
        let _ = slint::quit_event_loop();
    }
}

pub fn setup(ui: &AppWindow, app: &Rc<App>) {
    let Ok(path) = std::env::var("PASTEZO_SNAPSHOT") else { return };
    let theme = std::env::var("PASTEZO_THEME").ok();

    if let Some(tab) = std::env::var("PASTEZO_SNAPSHOT_SETTINGS").ok().and_then(|t| t.parse::<i32>().ok()) {
        let app = app.clone();
        Timer::single_shot(Duration::from_millis(400), move || {
            app.open_settings();
            let w = app.settings_window.borrow();
            let Some(w) = w.as_ref() else { return };
            w.set_tab(tab);
            w.set_confirming_clear(std::env::var_os("PASTEZO_SNAPSHOT_CONFIRM").is_some());
            if let Some(t) = &theme {
                crate::apply_theme(w, t);
                w.set_theme_id(t.into());
            }
            let weak = w.as_weak();
            let path = path.clone();
            Timer::single_shot(Duration::from_millis(500), move || {
                if let Some(w) = weak.upgrade() {
                    save(w.window().take_snapshot(), &path);
                }
            });
        });
        return;
    }

    let weak = ui.as_weak();
    // PASTEZO_SNAPSHOT_SEARCH=<ms>: open the search first and shoot <ms> later
    let search_after: Option<u64> = std::env::var("PASTEZO_SNAPSHOT_SEARCH").ok().and_then(|v| v.parse().ok());
    if search_after.is_some() {
        let weak = ui.as_weak();
        Timer::single_shot(Duration::from_millis(500), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_search_open(true);
            }
        });
    }
    // PASTEZO_SNAPSHOT_TOAST=success|error|info|warning: show that notification;
    // undo: the one after a deletion, with its button; update: a newer version is out
    if std::env::var("PASTEZO_SNAPSHOT_TOAST").as_deref() == Ok("undo") {
        app.toaster.show_action(ToastKind::Info, app.i18n.t("toast.deleted", &[]), app.i18n.t("toast.undo", &[]), crate::UNDO_TIME, Rc::new(|| {}));
    } else if std::env::var("PASTEZO_SNAPSHOT_TOAST").as_deref() == Ok("update") {
        app.offer_update("0.2.0");
    } else if let Ok(kind) = std::env::var("PASTEZO_SNAPSHOT_TOAST") {
        let kind = match kind.as_str() {
            "error" => ToastKind::Error,
            "info" => ToastKind::Info,
            "warning" => ToastKind::Warning,
            _ => ToastKind::Success,
        };
        let key = if kind == ToastKind::Error { "toast.copyFailed" } else { "toast.copied" };
        app.toaster.show(kind, app.i18n.t(key, &[]));
    }
    // PASTEZO_SNAPSHOT_REMOVE=<ms>: play the delete animation on the second
    // row (display only, the database is not touched) and shoot <ms> later
    let remove_after: Option<u64> = std::env::var("PASTEZO_SNAPSHOT_REMOVE").ok().and_then(|v| v.parse().ok());
    if remove_after.is_some() {
        let app = app.clone();
        Timer::single_shot(Duration::from_millis(500), move || {
            if let Some(mut row) = app.model.row_data(1) {
                row.removing = true;
                app.model.set_row_data(1, row);
            }
        });
    }
    // PASTEZO_SNAPSHOT_PREVIEW=<row>: select the row and open its preview
    if let Some(row) = std::env::var("PASTEZO_SNAPSHOT_PREVIEW").ok().and_then(|r| r.parse::<i32>().ok()) {
        let weak = ui.as_weak();
        Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                for _ in 0..=row {
                    ui.invoke_move_selection(1);
                }
                ui.invoke_preview_selected();
            }
        });
    }
    // PASTEZO_SNAPSHOT_QUERY=<text>: search for <text> (e.g. to see "no results")
    let query = std::env::var("PASTEZO_SNAPSHOT_QUERY").ok();
    if let Some(q) = query.clone() {
        let weak = ui.as_weak();
        Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_search_open(true);
                ui.set_query(q.clone().into());
                ui.invoke_query_edited(q.into());
            }
        });
    }
    let delay = search_after.or(remove_after).or(query.map(|_| 400)).map_or(800, |ms| 500 + ms);
    Timer::single_shot(Duration::from_millis(delay), move || {
        let ui = weak.unwrap();
        save(ui.window().take_snapshot(), &path);
    });

}
