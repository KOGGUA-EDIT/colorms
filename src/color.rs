//! Colour model for Colorms.
//!
//! The picker works in HSV: the wheel chooses hue + saturation and the vertical
//! capsule chooses value ("明度").  All conversions here are pure functions so
//! they can be unit tested and benchmarked without a window.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// An 8-bit sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb8 {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Relative luminance as defined by WCAG 2.1.
    pub fn relative_luminance(self) -> f32 {
        fn channel(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// Linear interpolation in sRGB space. `t == 0` returns `self`.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Self::new(
            lerp(self.r, other.r),
            lerp(self.g, other.g),
            lerp(self.b, other.b),
        )
    }

    pub fn lighten(self, amount: f32) -> Self {
        self.mix(Self::WHITE, amount)
    }

    pub fn darken(self, amount: f32) -> Self {
        self.mix(Self::BLACK, amount)
    }
}

impl fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A colour selected on the Colorms wheel.
///
/// * `hue` — degrees in `[0, 360)`, 0 = red at the right of the wheel.
/// * `saturation` — `[0, 1]`, 0 = wheel centre.
/// * `value` — `[0, 1]`, the position on the black→white brightness capsule.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickerColor {
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
}

impl Default for PickerColor {
    fn default() -> Self {
        // Material 3 baseline purple (#6750A4) is a pleasant first impression.
        Self {
            hue: 256.4,
            saturation: 0.512,
            value: 0.643,
        }
    }
}

impl PickerColor {
    pub fn new(hue: f32, saturation: f32, value: f32) -> Self {
        Self {
            hue,
            saturation,
            value,
        }
        .clamped()
    }

    pub fn clamped(mut self) -> Self {
        if !self.hue.is_finite() {
            self.hue = 0.0;
        }
        if !self.saturation.is_finite() {
            self.saturation = 0.0;
        }
        if !self.value.is_finite() {
            self.value = 0.0;
        }
        self.hue = self.hue.rem_euclid(360.0);
        self.saturation = self.saturation.clamp(0.0, 1.0);
        self.value = self.value.clamp(0.0, 1.0);
        self
    }

    pub fn rgb(self) -> Rgb8 {
        hsv_to_rgb(self.hue, self.saturation, self.value)
    }

    pub fn hex(self) -> String {
        self.rgb().to_hex()
    }

    pub fn from_hex(hex: &str) -> Result<Self, ColorError> {
        let rgb = parse_hex(hex).ok_or_else(|| ColorError::InvalidHex(hex.to_owned()))?;
        Ok(Self::from_rgb(rgb))
    }

    pub fn from_rgb(rgb: Rgb8) -> Self {
        let (hue, saturation, value) = rgb_to_hsv(rgb);
        Self {
            hue,
            saturation,
            value,
        }
    }

    /// Black or white, whichever reads better on top of this colour.
    pub fn readable_text(self) -> Rgb8 {
        readable_on(self.rgb())
    }

    /// A light, Material-toned version of this colour suitable as a filled
    /// container (e.g. the copy button).
    pub fn tonal_container(self) -> Rgb8 {
        hsv_to_rgb(self.hue, (self.saturation * 0.48).clamp(0.0, 0.35), 0.94)
    }

    /// A dark version of this colour used to tint surfaces.
    pub fn surface_tint(self) -> Rgb8 {
        hsv_to_rgb(self.hue, (self.saturation * 0.7).clamp(0.0, 0.45), 0.24)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ColorError {
    #[error("invalid hex colour: {0}")]
    InvalidHex(String),
}

pub fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> Rgb8 {
    let h = if hue.is_finite() {
        hue.rem_euclid(360.0)
    } else {
        0.0
    };
    let s = saturation.clamp(0.0, 1.0);
    let v = value.clamp(0.0, 1.0);

    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;

    fn to_u8(x: f32) -> u8 {
        (x.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
    }

    Rgb8::new(to_u8(r1 + m), to_u8(g1 + m), to_u8(b1 + m))
}

pub fn rgb_to_hsv(rgb: Rgb8) -> (f32, f32, f32) {
    let r = rgb.r as f32 / 255.0;
    let g = rgb.g as f32 / 255.0;
    let b = rgb.b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue.rem_euclid(360.0), saturation, max)
}

pub fn parse_hex(hex: &str) -> Option<Rgb8> {
    let s = hex.trim().trim_start_matches('#').trim();
    let parse = |slice: &str| u8::from_str_radix(slice, 16).ok();
    match s.len() {
        3 => {
            let r = parse(&s[0..1])?;
            let g = parse(&s[1..2])?;
            let b = parse(&s[2..3])?;
            Some(Rgb8::new(r * 17, g * 17, b * 17))
        }
        6 => Some(Rgb8::new(
            parse(&s[0..2])?,
            parse(&s[2..4])?,
            parse(&s[4..6])?,
        )),
        _ => None,
    }
}

pub fn readable_on(background: Rgb8) -> Rgb8 {
    if contrast_ratio(Rgb8::WHITE, background) >= contrast_ratio(Rgb8::BLACK, background) {
        Rgb8::WHITE
    } else {
        Rgb8::BLACK
    }
}

pub fn contrast_ratio(a: Rgb8, b: Rgb8) -> f32 {
    let la = a.relative_luminance();
    let lb = b.relative_luminance();
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), Rgb8::new(255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), Rgb8::new(0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), Rgb8::new(0, 0, 255));
        assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), Rgb8::new(255, 0, 0));
        assert_eq!(hsv_to_rgb(0.0, 1.0, 0.5), Rgb8::new(128, 0, 0));
    }

    #[test]
    fn hsv_black_white_and_grey() {
        assert_eq!(hsv_to_rgb(0.0, 0.0, 0.0), Rgb8::BLACK);
        assert_eq!(hsv_to_rgb(0.0, 0.0, 1.0), Rgb8::WHITE);
        assert_eq!(hsv_to_rgb(200.0, 0.0, 0.5), Rgb8::new(128, 128, 128));
    }

    #[test]
    fn rgb_hsv_roundtrip() {
        for hue in [0.0, 1.0, 59.0, 60.0, 133.0, 180.0, 240.0, 300.0, 359.9] {
            for saturation in [0.0, 0.05, 0.5, 0.99, 1.0] {
                for value in [0.0, 0.02, 0.5, 0.98, 1.0] {
                    let color = PickerColor::new(hue, saturation, value);
                    let round = PickerColor::from_rgb(color.rgb());
                    assert!(
                        (round.value - value).abs() < 0.01,
                        "value {value} -> {round:?}"
                    );
                    if saturation > 0.02 && value > 0.02 {
                        assert!(
                            (round.saturation - saturation).abs() < 0.02,
                            "saturation {saturation} -> {round:?}"
                        );
                        // Hue becomes unreliable for near-black or near-grey 8-bit colours.
                        if value > 0.25 && saturation > 0.1 {
                            let dh = (round.hue - hue.rem_euclid(360.0) + 540.0) % 360.0 - 180.0;
                            assert!(dh.abs() < 1.0, "hue {hue} -> {round:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hex_roundtrip() {
        let c = PickerColor::new(211.0, 0.71, 0.87);
        let hex = c.hex();
        assert_eq!(hex.len(), 7);
        assert_eq!(PickerColor::from_hex(&hex).unwrap().hex(), hex);
        assert_eq!(parse_hex("#fff"), Some(Rgb8::WHITE));
        assert_eq!(parse_hex("00ff00"), Some(Rgb8::new(0, 255, 0)));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }

    #[test]
    fn clamping_normalises() {
        let c = PickerColor::new(-90.0, 4.0, -2.0);
        assert_eq!(c.hue, 270.0);
        assert_eq!(c.saturation, 1.0);
        assert_eq!(c.value, 0.0);
        let c = PickerColor::new(f32::NAN, f32::INFINITY, f32::NEG_INFINITY);
        assert_eq!(c, PickerColor::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn contrast_and_readability() {
        assert!((contrast_ratio(Rgb8::BLACK, Rgb8::WHITE) - 21.0).abs() < 0.01);
        assert_eq!(readable_on(Rgb8::BLACK), Rgb8::WHITE);
        assert_eq!(readable_on(Rgb8::WHITE), Rgb8::BLACK);
        assert_eq!(readable_on(Rgb8::new(255, 230, 0)), Rgb8::BLACK);
        assert_eq!(readable_on(Rgb8::new(20, 30, 40)), Rgb8::WHITE);
    }

    #[test]
    fn material_tones_are_light() {
        let c = PickerColor::new(256.4, 0.512, 0.643);
        let container = c.tonal_container();
        assert!(container.relative_luminance() > 0.4);
        assert!(c.surface_tint().relative_luminance() < 0.08);
    }
}
