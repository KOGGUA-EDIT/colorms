//! Geometry, generated textures and small custom-painted controls.
//!
//! Colorms does not use egui's stock widgets: everything is painted on a canvas
//! that matches `ui.vectorcraft` (576×576 artboard) and scales with the window.

use crate::color::hsv_to_rgb;
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect,
    Stroke, StrokeKind, Vec2, pos2, vec2,
};

/// Side of the original artboard, in vector units.
pub const BOARD: f32 = 576.0;
pub const TITLE_H: f32 = 46.0;
pub const FOOTER_H: f32 = 54.0;
pub const WINDOW_RADIUS: u8 = 20;
pub const RESIZE_GRAB: f32 = 6.0;

pub const FONT_DISPLAY: &str = "colorms-display";
pub const FONT_MONO: &str = "colorms-mono";

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub full: Rect,
    pub title: Rect,
    pub content: Rect,
    pub footer: Rect,
    pub board: Rect,
    pub scale: f32,
}

impl Layout {
    pub fn new(full: Rect) -> Self {
        let title_bottom = (full.min.y + TITLE_H).min(full.max.y);
        let title = Rect::from_min_max(full.min, pos2(full.max.x, title_bottom));

        let footer_top = (full.max.y - FOOTER_H).max(title_bottom);
        let footer = Rect::from_min_max(pos2(full.min.x, footer_top), full.max);
        let content =
            Rect::from_min_max(pos2(full.min.x, title_bottom), pos2(full.max.x, footer_top));

        let scale = (content.width() / BOARD)
            .min(content.height() / BOARD)
            .clamp(0.05, 8.0);
        let board = Rect::from_center_size(content.center(), Vec2::splat(BOARD * scale));

        Self {
            full,
            title,
            content,
            footer,
            board,
            scale,
        }
    }

    #[inline]
    pub fn point(&self, x: f32, y: f32) -> Pos2 {
        self.board.min + vec2(x, y) * self.scale
    }

    #[inline]
    pub fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(self.point(x0, y0), self.point(x1, y1))
    }

    #[inline]
    pub fn len(&self, value: f32) -> f32 {
        value * self.scale
    }

    /// Font size in vector units, capped so tiny/large windows stay readable.
    #[inline]
    pub fn font_size(&self, base: f32) -> f32 {
        (base * self.scale).clamp(base * 0.78, base * 1.65)
    }
}

/// Positions lifted straight out of the VectorCraft document.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub brightness: Rect,
    pub wheel: Rect,
    pub wheel_center: Pos2,
    pub wheel_radius: f32,
    pub preview_center: Pos2,
    pub preview_radius: f32,
    pub hex_pill: Rect,
    pub copy_front: Rect,
    pub copy_back: Rect,
    pub copy_radius: f32,
    pub pill_radius: f32,
}

impl Metrics {
    pub fn from_layout(layout: &Layout) -> Self {
        let brightness = layout.rect(28.109, 46.620, 83.718, 406.362);
        let wheel = layout.rect(108.129, 46.620, 467.871, 406.362);
        let preview_center = layout.point(174.614, 457.660);
        let hex_pill = layout.rect(320.672, 485.145, 467.871, 524.145);
        let copy_front = layout.rect(473.121, 374.018, 527.999, 450.334);
        let copy_back = layout.rect(487.972, 362.390, 542.851, 438.706);
        Self {
            brightness,
            wheel,
            wheel_center: wheel.center(),
            wheel_radius: wheel.width() * 0.5,
            preview_center,
            preview_radius: layout.len(66.485),
            hex_pill,
            copy_front,
            copy_back,
            copy_radius: layout.len(13.196),
            pill_radius: layout.len(18.300),
        }
    }

    pub fn copy_union(&self) -> Rect {
        self.copy_front.union(self.copy_back)
    }
}

/// The colour wheel as an anti-aliased, premultiplied-safe RGBA image.
///
/// Pixels beyond the rim keep the rim colour with zero alpha so bilinear
/// filtering never produces a dark halo around the wheel.
pub fn wheel_image(size: usize) -> ColorImage {
    let size = size.max(32);
    let mut pixels = Vec::with_capacity(size * size);
    let radius = size as f32 * 0.5;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - radius;
            let dy = y as f32 + 0.5 - radius;
            let distance = (dx * dx + dy * dy).sqrt();
            let saturation = (distance / radius).clamp(0.0, 1.0);
            let hue = (-dy).atan2(dx).to_degrees().rem_euclid(360.0);
            let rgb = hsv_to_rgb(hue, saturation, 1.0);
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0 + 0.5) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(rgb.r, rgb.g, rgb.b, alpha));
        }
    }
    ColorImage::new([size, size], pixels)
}

/// The vertical black→white brightness capsule, with the same rounded silhouette
/// as the VectorCraft artboard. Generated once and reused.
pub fn brightness_image(width: usize, height: usize) -> ColorImage {
    let width = width.max(8);
    let height = height.max(16);
    let radius = width as f32 * 0.5;
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        let value = 1.0 - (y as f32 / (height - 1) as f32);
        let level = (value * 255.0).round() as u8;
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let center_y = py.clamp(radius, height as f32 - radius);
            let distance = ((px - radius).powi(2) + (py - center_y).powi(2)).sqrt() - radius;
            let coverage = (0.5 - distance).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0 + 0.5) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(level, level, level, alpha));
        }
    }
    ColorImage::new([width, height], pixels)
}

pub fn corner_radius(radius: f32) -> CornerRadius {
    CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

pub fn font_display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_DISPLAY.into()))
}

pub fn font_mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_MONO.into()))
}

pub fn font_body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Soft shadow behind a rounded rectangle, faked with a few translucent layers.
pub fn rounded_shadow(painter: &Painter, rect: Rect, radius: f32, color: Color32, offset_y: f32) {
    for i in 0..4 {
        let grow = 1.5 + i as f32 * 1.5;
        let alpha = ((color.a() as f32) * (0.32 - i as f32 * 0.065)).max(0.0) as u8;
        if alpha == 0 {
            continue;
        }
        let r = Rect::from_center_size(
            rect.center() + vec2(0.0, offset_y),
            rect.size() + Vec2::splat(grow),
        );
        painter.rect_filled(
            r,
            CornerRadius::same((radius + grow).round().clamp(0.0, 255.0) as u8),
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
        );
    }
}

pub fn paint_minimize_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let half = size * 0.5;
    painter.line_segment(
        [center - vec2(half, 0.0), center + vec2(half, 0.0)],
        Stroke::new(1.2_f32, color),
    );
}

pub fn paint_maximize_icon(
    painter: &Painter,
    center: Pos2,
    size: f32,
    color: Color32,
    maximized: bool,
) {
    let half = size * 0.5;
    let stroke = Stroke::new(1.2_f32, color);
    if maximized {
        let front = Rect::from_min_max(
            center - vec2(half, half * 0.65),
            center + vec2(half * 0.65, half),
        );
        let back = Rect::from_min_max(
            center - vec2(half * 0.65, half),
            center + vec2(half, half * 0.65),
        );
        let r = CornerRadius::same(2);
        painter.rect_stroke(back, r, stroke, StrokeKind::Inside);
        painter.rect_filled(front, r, Color32::TRANSPARENT);
        painter.rect_stroke(front, r, stroke, StrokeKind::Inside);
    } else {
        let rect = Rect::from_center_size(center, Vec2::splat(size * 0.86));
        painter.rect_stroke(rect, CornerRadius::same(2), stroke, StrokeKind::Inside);
    }
}

pub fn paint_close_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let half = size * 0.42;
    let stroke = Stroke::new(1.25_f32, color);
    painter.line_segment(
        [center + vec2(-half, -half), center + vec2(half, half)],
        stroke,
    );
    painter.line_segment(
        [center + vec2(half, -half), center + vec2(-half, half)],
        stroke,
    );
}

/// Tiny centered hairline used to separate the custom titlebar.
pub fn titlebar_hairline(painter: &Painter, rect: Rect) {
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
    );
}

pub fn footer_hairline(painter: &Painter, rect: Rect) {
    painter.hline(
        rect.x_range(),
        rect.min.y + 0.5,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 18)),
    );
}

pub fn paint_window_border(painter: &Painter, full: Rect, color: Color32, radius: u8) {
    painter.rect_stroke(
        full.shrink(0.5),
        CornerRadius::same(radius),
        Stroke::new(1.0_f32, color),
        StrokeKind::Inside,
    );
}

/// The full-window rounded chassis plus the adaptive content gradient.
pub fn paint_background(
    painter: &Painter,
    layout: &Layout,
    theme: &crate::theme::Theme,
    radius: u8,
) {
    let full = layout.full;
    let corner = CornerRadius::same(radius);
    painter.rect_filled(full, corner, theme.footer);

    if layout.content.height() > 1.0 {
        // Two-stop vertical gradient from the color-tinted surface down to the
        // exact footer colour. Keeping the top edge below the rounded corners
        // means the transparent window corners stay clean.
        let mut mesh = egui::Mesh::default();
        let top = theme.surface_top;
        let bottom = theme.surface_bottom;
        let rect = layout.content;
        let base = mesh.vertices.len() as u32;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: rect.left_top(),
            uv: egui::epaint::WHITE_UV,
            color: top,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: rect.right_top(),
            uv: egui::epaint::WHITE_UV,
            color: top,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: rect.right_bottom(),
            uv: egui::epaint::WHITE_UV,
            color: bottom,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: rect.left_bottom(),
            uv: egui::epaint::WHITE_UV,
            color: bottom,
        });
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Ambient bloom behind the wheel, clipped to the content surface.
pub fn paint_ambient_glow(
    painter: &Painter,
    layout: &Layout,
    metrics: &Metrics,
    theme: &crate::theme::Theme,
) {
    let painter = painter.with_clip_rect(layout.content.intersect(layout.full));
    let center = metrics.wheel_center;
    let radius = metrics.wheel_radius;
    let base = theme.accent;
    // Many very faint layers give a smoother falloff than a few strong rings.
    for layer in 0..16 {
        let t = layer as f32 / 15.0;
        let factor = 1.55 - 0.72 * t;
        painter.circle_filled(
            center,
            radius * factor,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 2),
        );
    }
}

pub fn text_ellipsized(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font_id: FontId,
    color: Color32,
    max_width: f32,
) {
    let galley = painter.layout(text.to_owned(), font_id, color, max_width);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_board_square_and_inside_content() {
        let layout = Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0)));
        assert!((layout.board.width() - layout.board.height()).abs() < 0.01);
        assert!(layout.content.contains_rect(layout.board));
        assert!(layout.footer.height() >= FOOTER_H - 0.01);
    }

    #[test]
    fn metrics_match_the_vectorcraft_artboard() {
        let layout = Layout::new(Rect::from_min_size(
            pos2(0.0, 0.0),
            vec2(BOARD, BOARD + TITLE_H + FOOTER_H),
        ));
        let metrics = Metrics::from_layout(&layout);
        assert!((metrics.brightness.width() - 55.609).abs() < 0.01);
        assert!((metrics.brightness.height() - 359.743).abs() < 0.01);
        assert!((metrics.wheel_radius * 2.0 - 359.743).abs() < 0.01);
        assert!((metrics.hex_pill.width() - 147.199).abs() < 0.02);
        // The preview sits against the wheel's left edge and shares a baseline
        // with the hex pill; the pill ends exactly under the wheel's right edge.
        assert!(
            (metrics.preview_center.x - metrics.preview_radius - metrics.wheel.left()).abs() < 0.02
        );
        assert!(
            (metrics.preview_center.y + metrics.preview_radius - metrics.hex_pill.bottom()).abs()
                < 0.02
        );
        assert!((metrics.hex_pill.right() - metrics.wheel.right()).abs() < 0.02);
    }

    #[test]
    fn wheel_image_is_antialiased_at_the_rim() {
        let image = wheel_image(64);
        let corner = image.pixels[0];
        assert_eq!(corner.a(), 0);
        let center = image.pixels[(32 * 64) + 32];
        assert_eq!(center.a(), 255);
        assert!(center.r() >= 240 && center.g() >= 240 && center.b() >= 240);
    }

    #[test]
    fn brightness_image_has_capsule_silhouette() {
        let image = brightness_image(32, 128);
        assert_eq!(image.pixels[0].a(), 0);
        assert_eq!(image.pixels[16 * 32].a(), 255);
        assert_eq!(image.pixels[31].a(), 0);
    }
}
