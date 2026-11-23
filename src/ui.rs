//! Geometry, generated textures and small custom-painted controls.
//!
//! Colorms does not use egui's stock widgets: everything is painted on a canvas
//! that matches `ui.vectorcraft` (576×576 artboard) and scales with the window.

use crate::color::hsv_to_rgb;
use egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect,
    Stroke, StrokeKind, Vec2, pos2, vec2,
};

/// Side of the original artboard, in vector units.
pub const BOARD: f32 = 576.0;
pub const TITLE_H: f32 = 46.0;
/// The recent-colour strip was removed, so the artboard now runs to the
/// bottom edge; the field stays for the toast, which hangs above it.
pub const FOOTER_H: f32 = 0.0;
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

/// Positions lifted from the VectorCraft document, extended with the MD3
/// top-right eyedropper and redesigned copy action button.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub brightness: Rect,
    pub wheel: Rect,
    pub wheel_center: Pos2,
    pub wheel_radius: f32,
    pub preview_center: Pos2,
    pub preview_radius: f32,
    pub hex_pill: Rect,
    pub eyedropper: Rect,
    pub copy_button: Rect,
    pub pill_radius: f32,
}

impl Metrics {
    pub fn from_layout(layout: &Layout) -> Self {
        let brightness = layout.rect(28.109, 46.620, 83.718, 406.362);
        let wheel = layout.rect(108.129, 46.620, 467.871, 406.362);
        let preview_center = layout.point(174.614, 457.660);
        let hex_pill = layout.rect(320.672, 485.145, 467.871, 524.145);
        // Right action column (centered at x = 516, symmetric to brightness
        // capsule on the left): top-right 44dp MD3 Icon Button for the screen
        // eyedropper, and lower-right 56dp MD3 Primary Action Button for copy.
        let eyedropper = layout.rect(494.0, 46.620, 538.0, 90.620);
        let copy_button = layout.rect(488.0, 380.0, 544.0, 436.0);
        Self {
            brightness,
            wheel,
            wheel_center: wheel.center(),
            wheel_radius: wheel.width() * 0.5,
            preview_center,
            preview_radius: layout.len(66.485),
            hex_pill,
            eyedropper,
            copy_button,
            pill_radius: layout.len(19.5),
        }
    }
}

/// The colour wheel as an anti-aliased, premultiplied-safe RGBA image.
///
/// Pixels beyond the rim keep the rim colour with zero alpha so bilinear
/// filtering never produces a dark halo around the wheel.
pub fn wheel_image(size: usize) -> ColorImage {
    let size = size.max(32);
    let radius = size as f32 * 0.5;
    // Precompute the fully saturated hue ring once. For V=1, any lower
    // saturation is just a lerp from white towards that ring, which avoids a
    // full HSV conversion (and its branches) for every one of the pixels.
    const HUE_STEPS: usize = 4096;
    let mut hue_ring = vec![(0u8, 0u8, 0u8); HUE_STEPS];
    for (step, entry) in hue_ring.iter_mut().enumerate() {
        let hue = step as f32 * (360.0 / HUE_STEPS as f32);
        let rgb = hsv_to_rgb(hue, 1.0, 1.0);
        *entry = (rgb.r, rgb.g, rgb.b);
    }
    let mut pixels = vec![Color32::TRANSPARENT; size * size];
    // Generating the wheel is the one genuinely parallel part of the startup
    // path: 768² pixels of atan2 and square root. Split it over the cores, one
    // horizontal band per worker.
    let workers = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .clamp(1, 16);
    let rows = size.div_ceil(workers);
    std::thread::scope(|scope| {
        for (band, rows_start) in pixels.chunks_mut(rows * size).zip((0..size).step_by(rows)) {
            let hue_ring = &hue_ring;
            scope.spawn(move || {
                for (row, line) in band.chunks_mut(size).enumerate() {
                    let y = rows_start + row;
                    let dy = y as f32 + 0.5 - radius;
                    for (x, pixel) in line.iter_mut().enumerate() {
                        let dx = x as f32 + 0.5 - radius;
                        let distance = (dx * dx + dy * dy).sqrt();
                        let saturation = (distance / radius).clamp(0.0, 1.0);
                        let angle = (-dy).atan2(dx);
                        let index = ((angle.rem_euclid(std::f32::consts::TAU)
                            * (HUE_STEPS as f32 / std::f32::consts::TAU))
                            as usize)
                            .min(HUE_STEPS - 1);
                        let (hue_r, hue_g, hue_b) = hue_ring[index];
                        let mix = |channel: u8| -> u8 {
                            (255.0 + (channel as f32 - 255.0) * saturation).round() as u8
                        };
                        let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
                        let alpha = (coverage * 255.0 + 0.5) as u8;
                        *pixel = Color32::from_rgba_unmultiplied(
                            mix(hue_r),
                            mix(hue_g),
                            mix(hue_b),
                            alpha,
                        );
                    }
                }
            });
        }
    });
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

/// Opaque rounded rectangle built from plain rects and circles.
///
/// `rect_filled` with a corner radius tessellates the caps as triangle fans
/// whose slivers are narrower than a pixel at the rim; the CPU rasterizer
/// cannot tile those exactly, which left a torn wedge in the hex pill's right
/// cap. Two overlapping rects plus four corner discs cover the same area with
/// shapes it does rasterize exactly, and because the fill is opaque the
/// overlaps are invisible.
pub fn fill_round_rect(painter: &Painter, rect: Rect, radius: f32, color: Color32) {
    let radius = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    if radius < 0.5 {
        painter.rect_filled(rect, CornerRadius::ZERO, color);
        return;
    }
    // Corners first: the rects below are axis-aligned and fully opaque, so they
    // cover the inner half of each disc's anti-aliased fringe and every fringe
    // pixel is blended exactly once. Coincident discs (a pill, where the radius
    // is half the height) are drawn once, otherwise their fringes would blend
    // twice and leave a dotted ring inside the cap.
    let mut corners: Vec<Pos2> = Vec::with_capacity(4);
    for corner in [
        pos2(rect.left() + radius, rect.top() + radius),
        pos2(rect.right() - radius, rect.top() + radius),
        pos2(rect.left() + radius, rect.bottom() - radius),
        pos2(rect.right() - radius, rect.bottom() - radius),
    ] {
        if corners.iter().all(|other| other.distance(corner) > 0.01) {
            corners.push(corner);
        }
    }
    for corner in corners {
        painter.circle_filled(corner, radius, color);
    }
    painter.rect_filled(
        Rect::from_min_max(
            pos2(rect.left(), rect.top() + radius),
            pos2(rect.right(), rect.bottom() - radius),
        ),
        CornerRadius::ZERO,
        color,
    );
    painter.rect_filled(
        Rect::from_min_max(
            pos2(rect.left() + radius, rect.top()),
            pos2(rect.right() - radius, rect.bottom()),
        ),
        CornerRadius::ZERO,
        color,
    );
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

/// Material Symbols `content_copy` icon (24dp optical grid scaled to `size`).
pub fn paint_copy_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let u = size / 24.0;
    let stroke = Stroke::new((1.9 * u).clamp(1.4, 2.8), color);
    let r = CornerRadius::same((2.4 * u).round().clamp(1.0, 6.0) as u8);

    // Front sheet (bottom-right)
    let front = Rect::from_min_max(
        center + vec2(-3.5 * u, -4.0 * u),
        center + vec2(8.5 * u, 10.0 * u),
    );
    painter.rect_stroke(front, r, stroke, StrokeKind::Middle);

    // Back sheet (top-left L-bracket so it never crosses into the front sheet)
    let bx0 = center.x - 8.5 * u;
    let by0 = center.y - 9.5 * u;
    let bx1 = center.x + 3.5 * u;
    let by1 = center.y + 4.5 * u;
    let cr = 2.4 * u;
    let back_pts = vec![
        pos2(bx0, by1),
        pos2(bx0, by0 + cr),
        pos2(bx0 + cr * 0.3, by0 + cr * 0.3),
        pos2(bx0 + cr, by0),
        pos2(bx1, by0),
    ];
    painter.add(egui::Shape::line(back_pts, stroke));
}

/// Material Symbols `check` icon shown after copying.
pub fn paint_check_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let u = size / 24.0;
    let stroke = Stroke::new((2.2 * u).clamp(1.6, 3.2), color);
    let pts = vec![
        center + vec2(-7.0 * u, 0.5 * u),
        center + vec2(-2.0 * u, 5.5 * u),
        center + vec2(7.5 * u, -5.0 * u),
    ];
    painter.add(egui::Shape::line(pts, stroke));
}

/// Material Symbols `colorize` (eyedropper / pipette) icon on a 24dp grid.
pub fn paint_eyedropper_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let u = size / 24.0;
    let stroke = Stroke::new((1.8 * u).clamp(1.3, 2.6), color);

    // 45-degree rotated local frame: `a` points along the barrel toward the
    // top-right bulb, `p` is perpendicular.
    let a = vec2(std::f32::consts::FRAC_1_SQRT_2, -std::f32::consts::FRAC_1_SQRT_2);
    let p = vec2(std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
    let pt = |along: f32, perp: f32| center + a * (along * u) + p * (perp * u);

    // Tapered glass barrel + tip (outlined polygon)
    let barrel = vec![
        pt(-9.5, 0.0),
        pt(-6.8, -2.3),
        pt(2.6, -2.3),
        pt(2.6, 2.3),
        pt(-6.8, 2.3),
    ];
    painter.add(egui::Shape::closed_line(barrel, stroke));

    // Collar crossbar between barrel and bulb
    painter.line_segment([pt(2.6, -4.4), pt(2.6, 4.4)], Stroke::new((2.0 * u).clamp(1.5, 2.8), color));

    // Rounded rubber bulb at the top-right end
    let bulb_end = pt(7.0, 0.0);
    let bulb_start = pt(3.2, 0.0);
    painter.line_segment([bulb_start, bulb_end], Stroke::new((5.0 * u).clamp(3.2, 7.0), color));
    painter.circle_filled(bulb_end, (2.5 * u).clamp(1.6, 3.5), color);
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

/// Analytic SDF anti-aliased rounded-rectangle outline mask (white
/// premultiplied alpha). Evaluated as a single signed-distance field so the
/// straight edges and corner arcs are mathematically seamless with zero
/// tangent-point cracks.
pub fn round_rect_ring_image(
    width: usize,
    height: usize,
    radius: f32,
    thickness: f32,
) -> ColorImage {
    let width = width.max(4);
    let height = height.max(4);
    let mut pixels = vec![Color32::TRANSPARENT; width * height];
    let half_w = width as f32 * 0.5;
    let half_h = height as f32 * 0.5;
    // Leave 0.5 px margin so the outer AA fringe never clips at the texture edge.
    let bx = (half_w - 0.5).max(1.0);
    let by = (half_h - 0.5).max(1.0);
    let r = radius.min(bx).min(by).max(0.0);
    let inner_bx = bx - r;
    let inner_by = by - r;
    for y in 0..height {
        let py = (y as f32 + 0.5 - half_h).abs();
        for x in 0..width {
            let px = (x as f32 + 0.5 - half_w).abs();
            let qx = px - inner_bx;
            let qy = py - inner_by;
            // Exact signed distance to the outer rounded-rect boundary
            // (negative inside, 0 on boundary, positive outside).
            let sd = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                + qx.max(qy).min(0.0)
                - r;
            let outer_cov = (0.5 - sd).clamp(0.0, 1.0);
            let inner_cov = (0.5 - (sd + thickness)).clamp(0.0, 1.0);
            let coverage = (outer_cov - inner_cov).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = (coverage * 255.0 + 0.5) as u8;
            pixels[y * width + x] = Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha);
        }
    }
    ColorImage::new([width, height], pixels)
}

/// Coverage mask for one corner of a large rounded-rectangle stroke (the
/// window chassis outline).
pub fn corner_ring_image(side: usize, radius: f32, thickness: f32) -> ColorImage {
    let mut pixels = vec![Color32::TRANSPARENT; side * side];
    let outer = radius;
    let inner = (radius - thickness).max(0.0);
    for y in 0..side {
        let py = y as f32 + 0.5;
        for x in 0..side {
            let px = x as f32 + 0.5;
            let dx = px - radius;
            let dy = py - radius;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (outer - distance + 0.5).clamp(0.0, 1.0)
                - (inner - distance + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = (coverage * 255.0 + 0.5) as u8;
            pixels[y * side + x] = Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha);
        }
    }
    ColorImage::new([side, side], pixels)
}

/// Analytic SDF anti-aliased circle ring mask (white premultiplied alpha).
///
/// Tint it with any `Color32` at draw time for razor-smooth circular outlines
/// (colour wheel rim, swatch border, circular icon buttons) with zero aliasing.
pub fn circle_ring_image(size: usize, thickness: f32) -> ColorImage {
    let size = size.max(4);
    let mut pixels = vec![Color32::TRANSPARENT; size * size];
    let center = size as f32 * 0.5;
    let outer = center - 0.5;
    let inner = (outer - thickness).max(0.0);
    for y in 0..size {
        let dy = y as f32 + 0.5 - center;
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (outer - distance + 0.5).clamp(0.0, 1.0)
                - (inner - distance + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = (coverage * 255.0 + 0.5) as u8;
            pixels[y * size + x] = Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha);
        }
    }
    ColorImage::new([size, size], pixels)
}

/// Draw an analytic anti-aliased circular outline centred at `center`.
pub fn paint_circle_ring(
    painter: &Painter,
    center: Pos2,
    diameter: f32,
    color: Color32,
    ring_tex: egui::TextureId,
) {
    let rect = Rect::from_center_size(center, Vec2::splat(diameter));
    painter.image(
        ring_tex,
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        color,
    );
}

/// Analytic anti-aliased rounded-rectangle outline built from four exact
/// quarter-circle SDF corner masks and four non-overlapping straight strips.
pub fn paint_round_rect_stroke(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    thick: f32,
    side: f32,
    color: Color32,
    corner: egui::TextureId,
) {
    let radius = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    if radius < 1.0 {
        painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(thick, color), StrokeKind::Inside);
        return;
    }
    let (left, top, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let side = side.min(rect.width() * 0.5).min(rect.height() * 0.5);

    // Four straight strips stopping cleanly at the corner tangent lines.
    if right - left > side * 2.0 + 0.01 {
        painter.rect_filled(
            Rect::from_min_max(pos2(left + side, top), pos2(right - side, top + thick)),
            CornerRadius::ZERO,
            color,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(left + side, bottom - thick), pos2(right - side, bottom)),
            CornerRadius::ZERO,
            color,
        );
    }
    if bottom - top > side * 2.0 + 0.01 {
        painter.rect_filled(
            Rect::from_min_max(pos2(left, top + side), pos2(left + thick, bottom - side)),
            CornerRadius::ZERO,
            color,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(right - thick, top + side), pos2(right, bottom - side)),
            CornerRadius::ZERO,
            color,
        );
    }

    for (quad, uv) in [
        (
            Rect::from_min_max(pos2(left, top), pos2(left + side, top + side)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        ),
        (
            Rect::from_min_max(pos2(right - side, top), pos2(right, top + side)),
            Rect::from_min_max(pos2(1.0, 0.0), pos2(0.0, 1.0)),
        ),
        (
            Rect::from_min_max(pos2(left, bottom - side), pos2(left + side, bottom)),
            Rect::from_min_max(pos2(0.0, 1.0), pos2(1.0, 0.0)),
        ),
        (
            Rect::from_min_max(pos2(right - side, bottom - side), pos2(right, bottom)),
            Rect::from_min_max(pos2(1.0, 1.0), pos2(0.0, 0.0)),
        ),
    ] {
        painter.image(corner, quad, uv, color);
    }
}

/// Alias kept for the window chassis outline.
pub fn paint_window_border(
    painter: &Painter,
    full: Rect,
    color: Color32,
    radius: f32,
    thick: f32,
    side: f32,
    corner: egui::TextureId,
) {
    paint_round_rect_stroke(painter, full, radius, thick, side, color, corner);
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
    // The 1 px outline is painted on top of the finished frame by
    // `paint_window_border`: a ring built from fills leaves the rim to the
    // outer shape's sub-pixel corner slivers, which the rasterizer cannot tile
    // without gaps.
    painter.rect_filled(full, corner, theme.footer);

    if layout.content.height() > 1.0 {
        // Two-stop vertical gradient from the color-tinted surface down to the
        // exact footer colour. The gradient stops one corner radius above the
        // bottom edge: it is a plain quad, so anything it covers becomes opaque
        // and would square off the rounded corners below it. Its last stop is
        // the chassis colour there, so the hand-over is invisible.
        let mut mesh = egui::Mesh::default();
        let top = theme.surface_top;
        let bottom = theme.surface_bottom;
        let rect = Rect::from_min_max(
            layout.content.min,
            egui::pos2(
                layout.content.max.x,
                (layout.content.max.y - radius as f32).max(layout.content.min.y),
            ),
        );
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
///
/// The original drew 16 nested circles at alpha 2, one per layer. Compositing
/// `n` copies of a `2/255` alpha source is identical to a single source with
/// alpha `1 - (1 - 2/255)^n`, so the whole bloom bakes into one radial ramp
/// texture: a single textured quad instead of ~6000 thin triangles.
pub fn glow_image(size: usize, accent: Color32) -> ColorImage {
    const LAYERS: f32 = 15.0;
    let mut alpha = [0.0f32; 17];
    for (count, value) in alpha.iter_mut().enumerate() {
        *value = 1.0 - (1.0 - 2.0 / 255.0f32).powi(count as i32);
    }
    let (ar, ag, ab) = (accent.r() as f32, accent.g() as f32, accent.b() as f32);
    let mut pixels = Vec::with_capacity(size * size);
    let half = size as f32 * 0.5;
    for y in 0..size {
        let v = (y as f32 + 0.5 - half) / half;
        for x in 0..size {
            let u = (x as f32 + 0.5 - half) / half;
            let distance = (u * u + v * v).sqrt() * 1.55;
            let coverage = if distance >= 1.55 {
                0.0
            } else {
                let layers = (LAYERS * (1.55 - distance) / 0.72).floor() + 1.0;
                alpha[(layers as usize).min(16)]
            };
            pixels.push(Color32::from_rgba_premultiplied(
                (ar * coverage + 0.5) as u8,
                (ag * coverage + 0.5) as u8,
                (ab * coverage + 0.5) as u8,
                (255.0 * coverage + 0.5) as u8,
            ));
        }
    }
    ColorImage::new([size, size], pixels)
}

/// Draw the baked bloom quad behind the wheel.
pub fn paint_ambient_glow(
    painter: &Painter,
    layout: &Layout,
    metrics: &Metrics,
    glow: egui::TextureId,
    radius: u8,
) {
    // The bloom is translucent, but painting it over the transparent window
    // corners would still make them partly opaque; stop it at the same line as
    // the surface gradient.
    let mut clip = layout.content.intersect(layout.full);
    clip.max.y = (clip.max.y - radius as f32).max(clip.min.y);
    let painter = painter.with_clip_rect(clip);
    let half = metrics.wheel_radius * 1.55;
    let rect = Rect::from_center_size(metrics.wheel_center, Vec2::splat(half * 2.0));
    painter.image(
        glow,
        rect,
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
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
