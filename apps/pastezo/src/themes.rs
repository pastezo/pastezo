//! Colour themes of the window. The first two are the design (light, dark);
//! the rest are defined by four colours, everything else is derived from them.
//!
//! `SYSTEM` is not a theme: it follows the OS between the first two.

use crate::ThemeColors;
use slint::Color;

pub const SYSTEM: &str = "system";
pub const LIGHT: &str = "light";
pub const DARK: &str = "dark";

pub struct Theme {
    pub id: &'static str,
    /// Proper name, the same in every language. `None`: the built-in ones,
    /// named through the translations (`settings.light` / `settings.dark`).
    pub name: Option<&'static str>,
    pub background: u32,
    pub text: u32,
    /// links in the list
    pub link: u32,
    /// card outline in the settings (light themes; dark ones have none)
    pub border: Option<u32>,
}

const fn t(id: &'static str, name: &'static str, background: u32, text: u32, link: u32, border: Option<u32>) -> Theme {
    Theme { id, name: Some(name), background, text, link, border }
}

/// In the order of the settings: the design's two, then light, then dark ones.
pub const THEMES: &[Theme] = &[
    Theme { id: LIGHT, name: None, background: 0xffffff, text: 0x111111, link: 0x111111, border: Some(0xdfdfdf) },
    Theme { id: DARK, name: None, background: 0x1e1e1e, text: 0xececec, link: 0xececec, border: None },
    // light
    t("paper", "Paper", 0xfbfafc, 0x000000, 0x62768d, Some(0xe2e1e3)),
    t("crisp", "Crisp", 0xf8f8f8, 0x1f1f1f, 0x2f6fd4, Some(0xdfe0df)),
    t("memo", "Memo", 0xffffff, 0x464646, 0xefb440, Some(0xe5e5e5)),
    t("snowfield", "Snowfield", 0xfdfdfd, 0x475269, 0x93453f, Some(0xe1e1e1)),
    t("glacier", "Glacier", 0xffffff, 0x454c5d, 0xc0714f, Some(0xe5e5e5)),
    t("lilac-mist", "Lilac Mist", 0xeff1f5, 0x4d4f67, 0x8839ef, Some(0xd8d8dc)),
    t("harbor-fog", "Harbor Fog", 0xeaeaed, 0x363b56, 0x965027, Some(0xd3d3d4)),
    t("pewter", "Pewter", 0xf0f0f0, 0x48525f, 0x783e4c, Some(0xd9d9d8)),
    t("morning", "Morning", 0xfafafa, 0x6b6982, 0xe0702a, Some(0xe1e1e1)),
    t("heatwave", "Heatwave", 0xfbfaf9, 0x5e575f, 0xaa55aa, Some(0xe1e1e1)),
    t("blush", "Blush", 0xf9f4ee, 0x575377, 0xd7827e, Some(0xe0dcd6)),
    t("parchment", "Parchment", 0xfaf8f5, 0x483612, 0x193482, Some(0xe1dedd)),
    t("pistachio", "Pistachio", 0xfbfaf1, 0x424038, 0x4a8468, Some(0xe2e1d9)),
    t("meadow", "Meadow", 0xfefbf0, 0x505659, 0x35779b, Some(0xe5e2d8)),
    t("hay", "Hay", 0xf8f5da, 0x3c3937, 0x5f8a61, Some(0xdfddc4)),
    t("sunlit", "Sunlit", 0xfdf5df, 0x2c373d, 0x945912, Some(0xe4ddc8)),
    // dark
    t("ink", "Ink", 0x1e1f1e, 0xdbdbdb, 0xeab544, None),
    t("onyx", "Onyx", 0x000000, 0xd4d4d4, 0xd99a33, None),
    t("midnight-teal", "Midnight Teal", 0x000000, 0xd4d4d4, 0x66a7a8, None),
    t("violet-hour", "Violet Hour", 0x000000, 0xd4d4d4, 0x9d8fdb, None),
    t("beacon", "Beacon", 0x18181e, 0xfcfae2, 0x8fd1b0, None),
    t("abyss", "Abyss", 0x12151b, 0x78a7b6, 0xd67f5e, None),
    t("ember", "Ember", 0x131c29, 0xf6f6ee, 0x7cc8dd, None),
    t("deep-sea", "Deep Sea", 0x182737, 0xdeefee, 0x72c79b, None),
    t("lagoon", "Lagoon", 0x0e303a, 0x909b99, 0x2aa198, None),
    t("mint", "Mint", 0x242e32, 0xdde2e7, 0xdf6c6c, None),
    t("forest-night", "Forest Night", 0x1f2326, 0xd0c6ad, 0xe67e80, None),
    t("haze", "Haze", 0x20232e, 0xd8d7d1, 0xf6cd76, None),
    t("slate", "Slate", 0x292b32, 0xdcdddd, 0x7fa9dd, None),
    t("coal", "Coal", 0x292c2f, 0x9b9b9b, 0x90aebc, None),
    t("arctic", "Arctic", 0x30343f, 0xededf2, 0xa3be8c, None),
    t("nightshade", "Nightshade", 0x363845, 0xfffeff, 0x8be9fd, None),
    t("neon-city", "Neon City", 0x25283a, 0x9ca4ca, 0xbb9af7, None),
    t("twilight", "Twilight", 0x252739, 0xcbd2f2, 0xc6a0f6, None),
    t("dusk-rose", "Dusk Rose", 0x232135, 0xdfdef2, 0xeb6f92, None),
    t("library", "Library", 0x584343, 0xc3b3a9, 0xe0b492, None),
];

pub fn find(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.id == id)
}

/// The id as stored in the settings, if it names a theme (or the system one).
pub fn known(id: &str) -> Option<&'static str> {
    if id == SYSTEM {
        return Some(SYSTEM);
    }
    find(id).map(|t| t.id)
}

fn rgb(v: u32) -> Color {
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// `f` of the way from `a` to `b`, per channel.
fn mix(a: u32, b: u32, f: f32) -> Color {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        (x + (y - x) * f).round() as u8
    };
    Color::from_rgb_u8(ch(16), ch(8), ch(0))
}

impl Theme {
    pub fn is_dark(&self) -> bool {
        let c = |s: u32| ((self.background >> s) & 0xff) as f32;
        0.299 * c(16) + 0.587 * c(8) + 0.114 * c(0) < 128.0
    }

    /// Every colour the window needs. The built-in two keep the design's exact
    /// values; the others mix their text into their background (the same
    /// proportions as the design's greys).
    pub fn colors(&self) -> ThemeColors {
        let dark = self.is_dark();
        let (bg, text) = (self.background, self.text);
        let exact = |light: u32, dark_v: u32| rgb(if dark { dark_v } else { light });
        let builtin = self.name.is_none();
        let derive = |f_light: f32, f_dark: f32, light: u32, dark_v: u32| {
            if builtin { exact(light, dark_v) } else { mix(bg, text, if dark { f_dark } else { f_light }) }
        };
        ThemeColors {
            background: rgb(bg),
            border: self.border.map_or(Color::from_argb_u8(0, 0, 0, 0), rgb),
            text: rgb(text),
            link: rgb(self.link),
            row_hover: derive(0.03, 0.04, 0xf8f8f8, 0x262626),
            meta: derive(0.27, 0.37, 0xc4c4c4, 0x6b6b6b),
            meta_strong: derive(0.41, 0.61, 0xa3a3a3, 0x9b9b9b),
            icon: derive(0.33, 0.46, 0xb8b8b8, 0x7c7c7c),
            icon_hover: derive(0.65, 0.90, 0x6b6b6b, 0xd6d6d6),
            search_icon: derive(0.35, 0.46, 0xb0b0b0, 0x7c7c7c),
            // lighter than the window: towards white (the light design's white stays white)
            field_background: mix(bg, 0xffffff, if dark { 0.06 } else { 0.6 }),
            field_border: derive(0.11, 0.14, 0xe3e3e3, 0x3a3a3a),
            field_border_focus: derive(0.25, 0.30, 0xc8c8c8, 0x5c5c5c),
            placeholder: derive(0.38, 0.39, 0xa9a9a9, 0x6f6f6f),
            dark,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_builtins_come_first() {
        let mut ids: Vec<_> = THEMES.iter().map(|t| t.id).collect();
        assert_eq!(&ids[..2], [LIGHT, DARK]);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), THEMES.len());
        assert!(find(SYSTEM).is_none(), "`system` is not a theme id");
    }

    #[test]
    fn builtins_keep_the_design_colours() {
        let light = find(LIGHT).unwrap().colors();
        assert_eq!(light.meta, rgb(0xc4c4c4));
        assert!(!light.dark);
        let dark = find(DARK).unwrap().colors();
        assert_eq!(dark.row_hover, rgb(0x262626));
        assert!(dark.dark);
    }

    #[test]
    fn light_and_dark_are_told_apart_by_the_background() {
        assert!(!find("paper").unwrap().is_dark());
        assert!(find("library").unwrap().is_dark());
        assert!(THEMES.iter().filter(|t| t.name.is_some()).all(|t| t.border.is_some() != t.is_dark()));
    }
}
