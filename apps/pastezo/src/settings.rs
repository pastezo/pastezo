//! User settings of the window, stored as `settings.json` in the data folder.
//! Only the window reads them (the agent has no UI settings yet).

use std::path::{Path, PathBuf};

use crate::{app_icon, themes};

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// a `themes::THEMES` id, or `themes::SYSTEM` (older settings)
    pub theme: &'static str,
    /// an `app_icon::VARIANTS` id
    pub icon: &'static str,
    pub text: TextStyle,
    /// Settings → General: the agent starts at login (`agent::set_autostart`)
    pub launch_at_login: bool,
    /// Settings → General: screen sharing and recording do not show the windows (`capture`)
    pub hide_from_capture: bool,
    /// The list window as it was last closed; `None`: the design's size, placed by the OS.
    pub window: Option<WindowFrame>,
    /// Last look for a newer version (`update.rs`), ms since the epoch.
    pub update_checked: Option<i64>,
}

/// Where the list window was and how big, to open it the same way next time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFrame {
    /// the content, logical px
    pub width: f32,
    pub height: f32,
    /// Top-left corner with the title bar, in screen pixels, and the scale it
    /// was taken at (macOS places windows in points: pixels / scale).
    /// `None` where the OS doesn't tell (Wayland).
    pub position: Option<(i32, i32, f32)>,
    pub maximized: bool,
}

impl WindowFrame {
    /// The window's `min-width` / `min-height` (`ui/app.slint`); anything
    /// smaller comes from a broken file.
    pub const MIN: (f32, f32) = (480.0, 360.0);
}

/// Settings → Typography: how the clips' text looks in the list.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// font family; the bundled MiSans or any installed one
    pub font: String,
    /// px
    pub size: f32,
    /// line height, em
    pub line: f32,
    /// space above and below each clip, px
    pub gap: f32,
}

impl TextStyle {
    pub const DEFAULT_FONT: &str = "MiSans";
    /// (min, max, step) of each slider
    pub const SIZE: (f32, f32, f32) = (12.0, 24.0, 1.0);
    /// the sliders with tick marks stop on the ticks only (the design's 1.45 em
    /// and 26 px are on them)
    pub const LINE: (f32, f32, f32) = (1.25, 1.95, 0.1);
    pub const GAP: (f32, f32, f32) = (10.0, 42.0, 4.0);
    /// Settings → Typography → Preset: (id, size, line, gap), on the sliders' steps
    pub const PRESETS: [(&str, f32, f32, f32); 3] =
        [("compact", 14.0, 1.35, 14.0), ("default", 17.0, 1.45, 26.0), ("large", 20.0, 1.55, 34.0)];

    /// This style with a preset's size, line height and spacing (the font stays).
    pub fn with_preset(self, id: &str) -> Self {
        match Self::PRESETS.iter().find(|p| p.0 == id) {
            Some(&(_, size, line, gap)) => TextStyle { size, line, gap, ..self },
            None => self,
        }
    }

    /// The preset these values are, if any ("" otherwise).
    pub fn preset(&self) -> &'static str {
        Self::PRESETS
            .iter()
            .find(|p| (p.1, p.2, p.3) == (self.size, self.line, self.gap))
            .map_or("", |p| p.0)
    }

    /// Within the sliders' ranges, on their steps.
    pub fn fitted(self) -> Self {
        let fit = |v: f32, (min, max, step): (f32, f32, f32), default: f32| {
            if !v.is_finite() {
                return default;
            }
            let v = min + ((v.clamp(min, max) - min) / step).round() * step;
            (v * 100.0).round() / 100.0
        };
        let d = Self::default();
        TextStyle {
            font: if self.font.trim().is_empty() { d.font } else { self.font },
            size: fit(self.size, Self::SIZE, d.size),
            line: fit(self.line, Self::LINE, d.line),
            gap: fit(self.gap, Self::GAP, d.gap),
        }
    }
}

impl Default for TextStyle {
    /// the design's values
    fn default() -> Self {
        TextStyle { font: Self::DEFAULT_FONT.into(), size: 17.0, line: 1.45, gap: 26.0 }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: themes::LIGHT, icon: app_icon::DEFAULT, text: TextStyle::default(), launch_at_login: false, hide_from_capture: false, window: None, update_checked: None }
    }
}

fn file(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

impl Settings {
    /// Missing or unreadable file: defaults (light theme).
    pub fn load(dir: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(file(dir)) else { return Self::default() };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let theme = json.get("theme").and_then(|t| t.as_str()).and_then(themes::known).unwrap_or(themes::LIGHT);
        let icon = json.get("icon").and_then(|t| t.as_str()).and_then(app_icon::known).unwrap_or(app_icon::DEFAULT);
        let t = json.get("text");
        let num = |k: &str, d: f32| t.and_then(|t| t.get(k)).and_then(|v| v.as_f64()).map_or(d, |v| v as f32);
        let d = TextStyle::default();
        let text = TextStyle {
            font: t.and_then(|t| t.get("font")).and_then(|v| v.as_str()).map_or(d.font.clone(), String::from),
            size: num("size", d.size),
            line: num("line", d.line),
            gap: num("gap", d.gap),
        }
        .fitted();
        // off unless turned on: the app does not add itself to the login items unasked
        let launch_at_login = json.get("launchAtLogin").and_then(|v| v.as_bool()).unwrap_or(false);
        let hide_from_capture = json.get("hideFromCapture").and_then(|v| v.as_bool()).unwrap_or(false);
        let window = json.get("window").and_then(|w| {
            let num = |k: &str| w.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
            let (width, height) = (num("width")? as f32, num("height")? as f32);
            let (min_w, min_h) = WindowFrame::MIN;
            if !(min_w..=100_000.0).contains(&width) || !(min_h..=100_000.0).contains(&height) {
                return None;
            }
            let position = match (num("x"), num("y"), num("scale")) {
                (Some(x), Some(y), Some(scale)) if scale > 0.0 => Some((x as i32, y as i32, scale as f32)),
                _ => None,
            };
            let maximized = w.get("maximized").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(WindowFrame { width, height, position, maximized })
        });
        let update_checked = json.get("updateChecked").and_then(|v| v.as_i64());
        Settings { theme, icon, text, launch_at_login, hide_from_capture, window, update_checked }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let t = &self.text;
        let mut json = serde_json::json!({
            "theme": self.theme,
            "icon": self.icon,
            "text": { "font": t.font, "size": t.size, "line": t.line, "gap": t.gap },
            "launchAtLogin": self.launch_at_login,
            "hideFromCapture": self.hide_from_capture,
        });
        if let Some(w) = &self.window {
            let mut frame = serde_json::json!({ "width": w.width, "height": w.height, "maximized": w.maximized });
            if let Some((x, y, scale)) = w.position {
                frame["x"] = x.into();
                frame["y"] = y.into();
                frame["scale"] = scale.into();
            }
            json["window"] = frame;
        }
        if let Some(t) = self.update_checked {
            json["updateChecked"] = t.into();
        }
        std::fs::write(file(dir), serde_json::to_string_pretty(&json)? + "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
        let text = TextStyle { font: "Helvetica".into(), size: 20.0, line: 1.65, gap: 14.0 };
        let window = Some(WindowFrame { width: 1000.0, height: 800.0, position: Some((-1200, 40, 2.0)), maximized: false });
        let s = Settings { theme: "nightshade", icon: "sky", text, launch_at_login: true, hide_from_capture: true, window, update_checked: Some(1_790_000_000_000) };
        s.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()), s);
        // a theme that no longer exists: back to the default
        std::fs::write(dir.path().join("settings.json"), r#"{"theme":"gone"}"#).unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
        std::fs::write(dir.path().join("settings.json"), "garbage").unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
        // a window smaller than it can be, or with no size: not kept
        std::fs::write(dir.path().join("settings.json"), r#"{"window":{"width":10,"height":800}}"#).unwrap();
        assert_eq!(Settings::load(dir.path()).window, None);
        std::fs::write(dir.path().join("settings.json"), r#"{"window":{"x":5,"y":5,"scale":1}}"#).unwrap();
        assert_eq!(Settings::load(dir.path()).window, None);
        // no position (Wayland): the size alone
        std::fs::write(dir.path().join("settings.json"), r#"{"window":{"width":700,"height":500}}"#).unwrap();
        let w = WindowFrame { width: 700.0, height: 500.0, position: None, maximized: false };
        assert_eq!(Settings::load(dir.path()).window, Some(w));
    }

    #[test]
    fn text_style_is_kept_within_the_sliders() {
        let t = TextStyle { font: " ".into(), size: 99.0, line: 1.43, gap: f32::NAN }.fitted();
        assert_eq!(t, TextStyle { font: "MiSans".into(), size: 24.0, line: 1.45, gap: 26.0 });
    }
}
