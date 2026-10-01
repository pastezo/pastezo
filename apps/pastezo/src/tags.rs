//! Optional smart-tag filtering. Visibility is saved with the window settings.
use std::rc::Rc;
use clip_core::tags::TagRule;
use slint::{Color, ComponentHandle, Model, ModelRc, VecModel};
use crate::{App, AppWindow, TagItem, TagsText};

fn color(hex: &str) -> Color {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x3578D4);
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

impl App {
    fn tag_name(&self, rule: &TagRule) -> String {
        if rule.name.is_empty() {
            let kind = if rule.kind == "color" { "colorTag" } else { &rule.kind };
            self.i18n.t(&format!("tags.{kind}"), &[])
        } else {
            rule.name.clone()
        }
    }

    pub(crate) fn refresh_tags(&self) {
        if !self.settings.borrow().show_tags {
            *self.tag.borrow_mut() = None;
            if let Some(ui) = self.ui.upgrade() {
                ui.set_show_tags(false);
                if ui.get_tags().row_count() != 0 { ui.set_tags(ModelRc::default()); }
                ui.set_selected_tag("".into());
            }
            return;
        }
        let Ok(rules) = self.history.tag_rules() else { return };
        if self.tag.borrow().as_ref().is_some_and(|id| !rules.iter().any(|r| r.id == *id && r.enabled && r.count > 0)) {
            *self.tag.borrow_mut() = None;
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_show_tags(true);
            let tags = ModelRc::new(VecModel::from(rules.iter().filter(|r| r.enabled && r.count > 0).map(|r| TagItem {
                id: r.id.clone().into(), name: self.tag_name(r).into(), tint: color(&r.color),
                count: r.count.min(i32::MAX as u32) as i32,
            }).collect::<Vec<_>>()));
            if ui.get_tags().iter().collect::<Vec<_>>() != tags.iter().collect::<Vec<_>>() { ui.set_tags(tags); }
            ui.set_selected_tag(self.tag.borrow().clone().unwrap_or_default().into());
        }
    }

    pub(crate) fn wire_tag_filters(self: &Rc<Self>, ui: &AppWindow) {
        ui.global::<TagsText>().set_all(self.i18n.t("tags.all", &[]).into());
        ui.set_show_tags(self.settings.borrow().show_tags);
        ui.on_choose_tag({ let app = self.clone(); move |id| {
            if !app.settings.borrow().show_tags { return; }
            let same = app.tag.borrow().as_deref() == Some(id.as_str());
            *app.tag.borrow_mut() = if id.is_empty() || same { None } else { Some(id.to_string()) };
            app.loaded.set(crate::PAGE);
            if let Some(ui) = app.ui.upgrade() { ui.set_selected_id(-1); ui.invoke_scroll_top(); }
            app.reload();
        }});
    }

    pub(crate) fn set_show_tags(&self, on: bool) {
        self.save_settings(crate::settings::Settings { show_tags: on, ..self.settings() });
        if !on { *self.tag.borrow_mut() = None; }
        self.loaded.set(crate::PAGE);
        if let Some(ui) = self.ui.upgrade() { ui.set_selected_id(-1); ui.invoke_scroll_top(); }
        if let Some(w) = self.settings_window.borrow().as_ref() { w.set_show_tags(on); }
        self.reload();
    }
}

