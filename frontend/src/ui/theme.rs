//! The look of the menu: colours, fonts and measures, in one place.
//!
//! Everything is opaque and flat: no blur, gradient or transparency.
//! Animated states blend two opaque colours with [`mix`].

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

// Colours.
pub const BG: Color32 = Color32::from_rgb(0x0E, 0x10, 0x14);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
pub const SURFACE2: Color32 = Color32::from_rgb(0x1E, 0x22, 0x2B);
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x2F, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF2);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xA1, 0xA8, 0xB5);
pub const ACCENT: Color32 = Color32::from_rgb(0x5B, 0x9D, 0xFF);
pub const OK: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
/// Text and icons on top of the accent colour or a game tile.
pub const ON_ACCENT: Color32 = BG;
/// Why something could not be done (not part of the mockup's palette).
pub const ERROR: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x80);

/// Game tiles, by position in the list.
pub const TILES: [Color32; 8] = [
    Color32::from_rgb(0x8F, 0xB8, 0xFF),
    Color32::from_rgb(0xF5, 0xB7, 0x8A),
    Color32::from_rgb(0x8B, 0xE0, 0xB0),
    Color32::from_rgb(0xD9, 0xB8, 0xF0),
    Color32::from_rgb(0xF2, 0xD8, 0x8A),
    Color32::from_rgb(0x8F, 0xDC, 0xE6),
    Color32::from_rgb(0xF2, 0xA0, 0xB0),
    Color32::from_rgb(0xB8, 0xC2, 0xD9),
];

// Measures, in points.
pub const WINDOW_WIDTH: f32 = 1024.0;
pub const WINDOW_HEIGHT: f32 = 768.0;
pub const TOP_BAR: f32 = 64.0;
pub const FOOTER: f32 = 48.0;
pub const PADDING: f32 = 24.0;
pub const DETAILS_WIDTH: f32 = 320.0;
pub const ROW_HEIGHT: f32 = 60.0;
pub const ROW_GAP: f32 = 4.0;
pub const RADIUS: f32 = 10.0;
pub const CARD_RADIUS: f32 = 14.0;
pub const BORDER_WIDTH: f32 = 1.0;

const INTER_REGULAR: &str = "inter-regular";
const INTER_MEDIUM: &str = "inter-medium";
const INTER_SEMIBOLD: &str = "inter-semibold";
const JETBRAINS_MONO: &str = "jetbrains-mono";

/// Inter, for text.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(INTER_MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(INTER_SEMIBOLD.into()))
}

/// JetBrains Mono, for paths, sizes and key names.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// The colour `t` of the way from `from` to `to` (0 to 1), always opaque.
pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(channel(from.r(), to.r()), channel(from.g(), to.g()), channel(from.b(), to.b()))
}

/// The embedded fonts: nothing else is loaded, so the menu looks the same
/// on every machine. (Both are under the SIL Open Font License; the
/// licences are next to the files in `assets/fonts`.)
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();
    let files: [(&str, &'static [u8]); 4] = [
        (INTER_REGULAR, include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
        (INTER_MEDIUM, include_bytes!("../../assets/fonts/Inter-Medium.ttf")),
        (INTER_SEMIBOLD, include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
        (JETBRAINS_MONO, include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf")),
    ];
    for (name, bytes) in files {
        fonts.font_data.insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    }
    let families = [
        (FontFamily::Proportional, INTER_REGULAR),
        (FontFamily::Name(INTER_MEDIUM.into()), INTER_MEDIUM),
        (FontFamily::Name(INTER_SEMIBOLD.into()), INTER_SEMIBOLD),
        (FontFamily::Monospace, JETBRAINS_MONO),
    ];
    for (family, name) in families {
        fonts.families.insert(family, vec![name.to_string()]);
    }
    fonts
}

/// Sets the fonts and the colours egui itself draws with.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = BG;
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.popup_shadow = egui::Shadow::NONE;
    visuals.override_text_color = Some(TEXT);
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_the_palette_and_opaque() {
        assert_eq!(BG.to_array(), [0x0E, 0x10, 0x14, 0xFF]);
        assert_eq!(SURFACE.to_array(), [0x16, 0x19, 0x20, 0xFF]);
        assert_eq!(SURFACE2.to_array(), [0x1E, 0x22, 0x2B, 0xFF]);
        assert_eq!(BORDER.to_array(), [0x2A, 0x2F, 0x3A, 0xFF]);
        assert_eq!(TEXT.to_array(), [0xEC, 0xEE, 0xF2, 0xFF]);
        assert_eq!(TEXT_SECONDARY.to_array(), [0xA1, 0xA8, 0xB5, 0xFF]);
        assert_eq!(ACCENT.to_array(), [0x5B, 0x9D, 0xFF, 0xFF]);
        assert_eq!(OK.to_array(), [0x3D, 0xD6, 0x8C, 0xFF]);
        assert!(TILES.iter().chain([&ERROR, &ON_ACCENT]).all(|colour| colour.a() == 0xFF));
    }

    #[test]
    fn mixing_goes_from_one_colour_to_the_other_and_stays_opaque() {
        assert_eq!(mix(BG, SURFACE2, 0.0), BG);
        assert_eq!(mix(BG, SURFACE2, 1.0), SURFACE2);
        assert_eq!(mix(BG, SURFACE2, 7.0), SURFACE2);
        assert_eq!(mix(BG, SURFACE2, -1.0), BG);
        let half = mix(Color32::from_rgb(0, 100, 200), Color32::from_rgb(100, 0, 200), 0.5);
        assert_eq!(half.to_array(), [50, 50, 200, 0xFF]);
    }

    #[test]
    fn every_font_family_has_an_embedded_font() {
        let fonts = fonts();
        assert_eq!(fonts.font_data.len(), 4);
        for font in [regular(14.0), medium(14.0), semibold(14.0), mono(14.0)] {
            let names = fonts.families.get(&font.family).expect("family is defined");
            assert!(names.iter().all(|name| fonts.font_data.contains_key(name)));
            assert_eq!(names.len(), 1, "no fallback fonts");
        }
    }
}
