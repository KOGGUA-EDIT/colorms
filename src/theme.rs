//! Material-You-flavoured palette derived from the colour on the wheel.
//!
//! Colorms is dark-only: the content surface is tinted by the selected colour,
//! while the very bottom strip (the footer) stays exactly `#202020` as asked.

use crate::color::{PickerColor, Rgb8, readable_on};
use eframe::egui::Color32;

pub const FOOTER_RGB: Rgb8 = Rgb8::new(0x20, 0x20, 0x20);

#[derive(Debug, Clone)]
pub struct Theme {
    /// The selected colour, slightly lifted so it stays visible on dark chrome.
    pub accent: Color32,
    /// Filled-container tone used by the copy card.
    pub accent_container: Color32,
    /// Text/icon colour on top of [`Self::accent_container`].
    pub on_accent_container: Color32,
    /// Top of the content surface gradient.
    pub surface_top: Color32,
    /// Bottom of the content surface gradient / footer.
    pub surface_bottom: Color32,
    pub footer: Color32,
    pub title_text: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub outline: Color32,
    pub outline_strong: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_pressed: Color32,
    pub glow: Color32,
    pub window_border: Color32,
    pub danger: Color32,
    pub toast_bg: Color32,
    pub shadow: Color32,
}

impl Theme {
    pub fn from_color(color: PickerColor) -> Self {
        let color = color.clamped();
        let accent_rgb = color.rgb();
        let accent_light = accent_rgb.mix(Rgb8::WHITE, 0.10);
        let container_rgb = color.tonal_container();
        let on_container = readable_on(container_rgb);

        // A whisper of the selected colour over a neutral dark base.
        let base = Rgb8::new(0x1B, 0x1A, 0x1F);
        let surface_top_rgb = base.mix(color.surface_tint(), 0.58);

        Self {
            accent: color32(accent_rgb),
            accent_container: color32(container_rgb),
            on_accent_container: color32(on_container),
            surface_top: color32(surface_top_rgb),
            surface_bottom: color32(FOOTER_RGB),
            footer: color32(FOOTER_RGB),
            title_text: color32(Rgb8::new(0xE6, 0xE1, 0xE5)),
            text: color32(Rgb8::new(0xE6, 0xE1, 0xE5)),
            text_muted: Color32::from_rgba_unmultiplied(0xE6, 0xE1, 0xE5, 150),
            outline: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 26),
            outline_strong: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 46),
            surface: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 14),
            surface_hover: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 26),
            surface_pressed: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 38),
            glow: Color32::from_rgba_unmultiplied(
                accent_light.r,
                accent_light.g,
                accent_light.b,
                22,
            ),
            window_border: Color32::from_rgba_unmultiplied(
                accent_light.r,
                accent_light.g,
                accent_light.b,
                84,
            ),
            danger: color32(Rgb8::new(0xFF, 0xB4, 0xAB)),
            toast_bg: color32(Rgb8::new(0x2B, 0x29, 0x30)),
            shadow: Color32::from_black_alpha(90),
        }
    }

    /// Surface colour for the top strip of the content area.
    pub fn title_surface(&self) -> Color32 {
        self.footer
    }
}

pub fn color32(rgb: Rgb8) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

/// Smoothly interpolate two colours. Useful for hover transitions that are
/// painted outside of egui's style system. Interpolation happens in egui's
/// premultiplied space, so translucent colours keep their intended alpha.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_is_exactly_202020() {
        let theme = Theme::from_color(PickerColor::new(210.0, 0.8, 0.9));
        assert_eq!(theme.footer, Color32::from_rgb(0x20, 0x20, 0x20));
        assert_eq!(theme.surface_bottom, Color32::from_rgb(0x20, 0x20, 0x20));
    }

    #[test]
    fn surface_is_dark_and_tinted() {
        let theme = Theme::from_color(PickerColor::new(0.0, 1.0, 1.0));
        let top = theme.surface_top;
        assert!((top.r() as u32 + top.g() as u32 + top.b() as u32) < 200);
    }

    #[test]
    fn lerp_color_mixes_channels() {
        assert_eq!(
            lerp_color(Color32::BLACK, Color32::WHITE, 0.5),
            Color32::from_rgb(128, 128, 128)
        );
    }

    #[test]
    fn lerp_color_preserves_translucent_surface() {
        let translucent = Color32::from_rgba_unmultiplied(255, 255, 255, 26);
        assert_eq!(lerp_color(translucent, translucent, 0.0), translucent);
        assert_eq!(lerp_color(translucent, translucent, 1.0), translucent);
    }
}
