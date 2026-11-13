//! The Colorms window: custom-painted Material You picker.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontData, FontDefinitions,
    FontFamily, Id, PointerButton, Rect, ResizeDirection, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, Vec2, ViewportCommand, pos2, vec2,
};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::color::PickerColor;
use crate::db::{Db, RECENT_LIMIT, StoredColor};
use crate::theme::{self, Theme};
use crate::ui::{self, FONT_DISPLAY, FONT_MONO, Layout, Metrics};

const BRAND_BYTES: &[u8] = include_bytes!("../assets/brand_96.rgba");
const BRAND_SIZE: usize = 96;

const DEFAULT_DURATION: f32 = 1.9;

struct Toast {
    text: String,
    start: f64,
    duration: f32,
}

impl Toast {
    fn alpha(&self, now: f64) -> f32 {
        let elapsed = (now - self.start) as f32;
        if elapsed < 0.0 || elapsed >= self.duration {
            return 0.0;
        }
        let fade_in = (elapsed / 0.14).clamp(0.0, 1.0);
        let fade_out = ((self.duration - elapsed) / 0.32).clamp(0.0, 1.0);
        (fade_in * fade_out).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, Debug)]
enum WindowAction {
    Minimize,
    Maximize,
    Close,
}

/// CPU-only startup assets, built on background threads while eframe creates
/// the window and its OpenGL context. By the time the app creator runs they are
/// normally ready, so only the GPU upload stays on the critical path.
pub struct StartupAssets {
    fonts: JoinHandle<FontDefinitions>,
    wheel: JoinHandle<ColorImage>,
}

impl StartupAssets {
    pub fn spawn() -> Self {
        let fonts = std::thread::Builder::new()
            .name("colorms-fonts".to_owned())
            .spawn(build_font_definitions)
            .expect("could not spawn the font loader thread");
        let wheel = std::thread::Builder::new()
            .name("colorms-wheel".to_owned())
            .spawn(|| {
                let started = Instant::now();
                let image = ui::wheel_image(768);
                tracing::info!(
                    ms = started.elapsed().as_secs_f64() * 1000.0,
                    "startup: wheel image generated"
                );
                image
            })
            .expect("could not spawn the wheel texture thread");
        Self { fonts, wheel }
    }

    fn join(self, startup: Instant) -> (FontDefinitions, ColorImage) {
        let fonts = match self.fonts.join() {
            Ok(fonts) => fonts,
            Err(err) => {
                tracing::error!("font loader thread failed: {err:?}");
                build_font_definitions()
            }
        };
        let wheel = match self.wheel.join() {
            Ok(image) => image,
            Err(err) => {
                tracing::error!("wheel texture thread failed: {err:?}");
                ui::wheel_image(768)
            }
        };
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: background assets joined"
        );
        (fonts, wheel)
    }
}

pub struct ColormsApp {
    color: PickerColor,
    db: Db,
    recent: Vec<StoredColor>,
    toast: Option<Toast>,
    pending_toast: Option<(String, f32)>,

    wheel: TextureHandle,
    slider: TextureHandle,
    brand: TextureHandle,

    dirty: bool,
    touched: bool,
    /// Hex value that was last copied from the card/pill; the copy card turns
    /// into a filled accent button while this still matches the selection.
    copied_hex: Option<String>,
    last_size: Vec2,
    persisted_size: Vec2,
    last_size_change: Instant,
    startup: Instant,
    startup_logged: bool,
    #[cfg(target_os = "windows")]
    window_handle: Option<isize>,
    #[cfg(target_os = "windows")]
    applied_region: Option<(i32, i32, i32, bool)>,
}

impl ColormsApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        db: Db,
        initial_color: PickerColor,
        initial_size: [f32; 2],
        db_note: Option<String>,
        startup: Instant,
        assets: StartupAssets,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        #[cfg(target_os = "windows")]
        let window_handle = cc
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(window) => Some(window.hwnd.get() as isize),
                _ => None,
            });
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: app::new enter"
        );
        #[cfg(target_os = "windows")]
        if let Some(hwnd) = window_handle {
            // The window is created hidden and only shown after the first
            // painted frame. Disable DWM's zoom/fade transition and non-client
            // rendering so the pre-paint ghost (native titlebar + blank body)
            // never shows during startup.
            unsafe {
                win32_region::prepare_for_show(hwnd);
            }
        }
        ctx.set_visuals(egui::Visuals::dark());
        let (fonts, wheel_image) = assets.join(startup);
        ctx.set_fonts(fonts);
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: fonts installed"
        );

        let wheel = ctx.load_texture("colorms-wheel", wheel_image, TextureOptions::LINEAR);
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: wheel texture"
        );
        let slider = ctx.load_texture(
            "colorms-brightness",
            ui::brightness_image(108, 700),
            TextureOptions::LINEAR,
        );
        let brand = ctx.load_texture(
            "colorms-brand",
            ColorImage::from_rgba_unmultiplied([BRAND_SIZE, BRAND_SIZE], BRAND_BYTES),
            TextureOptions::LINEAR,
        );
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: slider+brand textures"
        );

        let recent = match db.recent_colors(RECENT_LIMIT) {
            Ok(colors) => colors,
            Err(err) => {
                tracing::warn!("could not load recent colours: {err}");
                Vec::new()
            }
        };
        tracing::info!(
            ms = startup.elapsed().as_secs_f64() * 1000.0,
            "startup: recent colours"
        );

        let mut app = Self {
            color: initial_color.clamped(),
            db,
            recent,
            toast: None,
            pending_toast: None,
            wheel,
            slider,
            brand,
            dirty: false,
            touched: false,
            copied_hex: None,
            last_size: vec2(initial_size[0], initial_size[1]),
            persisted_size: vec2(initial_size[0], initial_size[1]),
            last_size_change: Instant::now(),
            startup,
            startup_logged: false,
            #[cfg(target_os = "windows")]
            window_handle,
            #[cfg(target_os = "windows")]
            applied_region: None,
        };
        if let Some(note) = db_note {
            app.pending_toast = Some((note, 3.6));
        }
        app
    }

    fn copy_current(&mut self, ctx: &egui::Context, now: f64) {
        let hex = self.color.hex();
        ctx.copy_text(hex.clone());
        tracing::info!("copied {hex} to clipboard");
        self.copied_hex = Some(hex.clone());
        let saved = self.commit(now);
        if saved {
            self.show_toast(format!("已复制 {hex}"), now);
        }
    }

    /// Persist the selected colour and refresh the footer. Returns false when
    /// the database write failed (the caller can surface that to the user).
    fn commit(&mut self, now: f64) -> bool {
        let hex = self.color.hex();
        let mut ok = true;
        match self.db.save_color(self.color) {
            Ok(()) => self.refresh_recent(),
            Err(err) => {
                ok = false;
                tracing::error!("failed to save colour {hex}: {err}");
                self.show_toast("保存失败：数据库不可写".to_owned(), now);
            }
        }
        if let Err(err) = self.db.save_last_color(self.color) {
            tracing::warn!("failed to save last colour: {err}");
        }
        self.dirty = false;
        self.touched = true;
        ok
    }

    fn persist_now(&mut self) {
        let _ = self.db.save_last_color(self.color);
        if self.dirty || self.touched {
            if let Err(err) = self.db.save_color(self.color) {
                tracing::warn!("failed to save colour on exit: {err}");
            }
            self.dirty = false;
            self.refresh_recent();
        }
        if self.last_size.x > 1.0 && self.last_size.y > 1.0 {
            let _ = self.db.save_window_size(self.last_size.x, self.last_size.y);
            self.persisted_size = self.last_size;
        }
    }

    fn refresh_recent(&mut self) {
        match self.db.recent_colors(RECENT_LIMIT) {
            Ok(colors) => self.recent = colors,
            Err(err) => tracing::warn!("could not refresh recent colours: {err}"),
        }
    }

    fn show_toast(&mut self, text: String, now: f64) {
        self.toast = Some(Toast {
            text,
            start: now,
            duration: DEFAULT_DURATION,
        });
    }

    fn track_size(&mut self, size: Vec2, maximized: bool) {
        // Remember the restored (non-maximized) size only, so closing while
        // maximized does not make the next launch restore a screen-sized window.
        if maximized {
            return;
        }
        if (size - self.last_size).length() > 0.5 {
            self.last_size = size;
            self.last_size_change = Instant::now();
        }
        if (size - self.persisted_size).length() > 0.5
            && self.last_size_change.elapsed() >= Duration::from_millis(850)
        {
            match self.db.save_window_size(size.x, size.y) {
                Ok(()) => self.persisted_size = size,
                Err(err) => tracing::warn!("failed to save window size: {err}"),
            }
            self.last_size_change = Instant::now();
        }
    }

    #[cfg(target_os = "windows")]
    fn update_rounded_region(&mut self, ctx: &egui::Context, maximized: bool) {
        let Some(hwnd) = self.window_handle else {
            return;
        };
        let mut rect = win32_region::Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { win32_region::GetClientRect(hwnd, &mut rect) } == 0 {
            return;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return;
        }
        // A rounded window region removes the square, black corners that DWM
        // leaves behind around the transparent chassis on Windows.
        let radius = if maximized {
            0
        } else {
            (ui::WINDOW_RADIUS as f32 * ctx.pixels_per_point()).round() as i32
        };
        let key = (width, height, radius, maximized);
        if self.applied_region == Some(key) {
            return;
        }
        if radius <= 0 {
            if unsafe { win32_region::SetWindowRgn(hwnd, 0, 1) } != 0 {
                self.applied_region = Some(key);
            }
            return;
        }
        let region = unsafe {
            win32_region::CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2)
        };
        if region == 0 {
            return;
        }
        if unsafe { win32_region::SetWindowRgn(hwnd, region, 1) } != 0 {
            self.applied_region = Some(key);
        } else {
            // SetWindowRgn did not take ownership on failure.
            unsafe {
                win32_region::DeleteObject(region);
            }
            self.applied_region = None;
        }
    }

    fn draw(&mut self, ctx: &egui::Context, ui: &Ui, full: Rect, now: f64, maximized: bool) {
        let layout = Layout::new(full);
        let theme = Theme::from_color(self.color);
        let metrics = Metrics::from_layout(&layout);
        let painter = ui.painter().clone();
        let radius = if maximized { 0 } else { ui::WINDOW_RADIUS };

        ui::paint_background(&painter, &layout, &theme, radius);
        ui::paint_ambient_glow(&painter, &layout, &metrics, &theme);
        ui::titlebar_hairline(&painter, layout.title);
        ui::footer_hairline(&painter, layout.footer);

        self.draw_wheel(ctx, ui, &layout, &metrics, &theme);
        self.draw_brightness(ctx, ui, &layout, &metrics, &theme);
        self.draw_preview(&painter, &metrics);
        self.draw_hex_pill(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_copy_card(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_footer(ctx, ui, &layout, &theme, now);
        self.draw_titlebar(ctx, ui, &layout, &theme, &painter, maximized);
        self.draw_toast(ctx, &painter, &layout, &theme, now);
        self.draw_resize_handles(ctx, ui, &layout, maximized);

        if !maximized {
            ui::paint_window_border(
                &painter,
                layout.full,
                theme.window_border,
                ui::WINDOW_RADIUS,
            );
        }
    }

    fn draw_wheel(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
    ) {
        let rect = metrics.wheel;
        let center = metrics.wheel_center;
        let radius = metrics.wheel_radius;
        let response = ui.interact(rect, Id::new("colorms-wheel"), Sense::click_and_drag());

        if response.hovered() || response.dragged() {
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }
        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.interact_pointer_pos() {
                let delta = pos - center;
                let distance = delta.length();
                let saturation = (distance / radius.max(1.0)).clamp(0.0, 1.0);
                let mut changed = false;
                if (saturation - self.color.saturation).abs() > 0.0005 {
                    self.color.saturation = saturation;
                    changed = true;
                }
                if distance > 1.5 {
                    let hue = (-delta.y).atan2(delta.x).to_degrees().rem_euclid(360.0);
                    if (hue - self.color.hue).abs() > 0.01 {
                        self.color.hue = hue;
                        changed = true;
                    }
                }
                if changed {
                    self.dirty = true;
                    self.touched = true;
                }
            }
        }

        let hover =
            ctx.animate_bool_with_time(Id::new("colorms-wheel-hover"), response.hovered(), 0.18);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter()
            .image(self.wheel.id(), rect, uv, Color32::WHITE);

        let rim = theme::lerp_color(theme.outline, theme.outline_strong, hover);
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(1.0_f32, rim));
        if hover > 0.01 {
            ui.painter().circle_stroke(
                center,
                radius + 2.0,
                Stroke::new(
                    1.0_f32,
                    Color32::from_rgba_unmultiplied(
                        theme.accent.r(),
                        theme.accent.g(),
                        theme.accent.b(),
                        (60.0 * hover) as u8,
                    ),
                ),
            );
        }

        let angle = self.color.hue.to_radians();
        let marker_center =
            center + vec2(angle.cos(), -angle.sin()) * (self.color.saturation * radius);
        let marker_radius = (10.0 * layout.scale).clamp(7.0, 14.0);
        let ring = (3.0 * layout.scale).clamp(2.0, 4.5);
        let painter = ui.painter();
        painter.circle_filled(
            marker_center + vec2(0.0, 1.5),
            marker_radius + 1.0,
            theme.shadow,
        );
        painter.circle_filled(marker_center, marker_radius, Color32::WHITE);
        painter.circle_filled(
            marker_center,
            (marker_radius - ring).max(1.0),
            theme::color32(self.color.rgb()),
        );
        painter.circle_stroke(
            marker_center,
            marker_radius,
            Stroke::new(1.0_f32, Color32::from_black_alpha(70)),
        );
    }

    fn draw_brightness(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
    ) {
        let rect = metrics.brightness;
        let response = ui.interact(rect, Id::new("colorms-brightness"), Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            ctx.set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.interact_pointer_pos() {
                let t = ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
                let value = 1.0 - t;
                if (value - self.color.value).abs() > 0.0005 {
                    self.color.value = value;
                    self.dirty = true;
                    self.touched = true;
                }
            }
        }

        let hover = ctx.animate_bool_with_time(
            Id::new("colorms-brightness-hover"),
            response.hovered() || response.is_pointer_button_down_on(),
            0.12,
        );
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let painter = ui.painter();
        painter.image(self.slider.id(), rect, uv, Color32::WHITE);
        painter.rect_stroke(
            rect,
            ui::corner_radius(rect.width() * 0.5),
            Stroke::new(
                1.0_f32,
                theme::lerp_color(theme.outline, theme.outline_strong, hover),
            ),
            StrokeKind::Inside,
        );

        let font_small = ui::font_body((9.8 * layout.scale).clamp(8.6, 13.0));
        let font_pct = ui::font_mono((9.8 * layout.scale).clamp(8.6, 13.0));
        let percent = format!("{}%", (self.color.value * 100.0).round() as i32);
        let w1 = text_width(painter, "明度", font_small.clone());
        let w2 = text_width(painter, &percent, font_pct.clone());
        let gap = layout.len(7.0);
        let total = w1 + gap + w2;
        let start_x = rect.center().x - total * 0.5;
        let label_y = layout.point(55.913, 24.5).y;
        painter.text(
            pos2(start_x, label_y),
            Align2::LEFT_CENTER,
            "明度",
            font_small,
            theme.text_muted,
        );
        painter.text(
            pos2(start_x + w1 + gap, label_y),
            Align2::LEFT_CENTER,
            percent,
            font_pct,
            theme.text,
        );

        // A small knob on the wheel side of the capsule makes the current
        // brightness obvious at a glance and mirrors Material sliders.
        let knob_center = pos2(
            rect.right() + layout.len(10.5),
            rect.bottom() - self.color.value * rect.height(),
        );
        let knob_radius = (6.5 * layout.scale).clamp(4.5, 10.0);
        painter.circle_filled(knob_center + vec2(0.0, 1.5), knob_radius, theme.shadow);
        painter.circle_filled(knob_center, knob_radius, theme::color32(self.color.rgb()));
        painter.circle_stroke(
            knob_center,
            knob_radius,
            Stroke::new((1.6 * layout.scale).clamp(1.0, 2.2), Color32::WHITE),
        );
        painter.circle_stroke(
            knob_center,
            knob_radius + 1.0,
            Stroke::new(1.0_f32, Color32::from_black_alpha(60)),
        );
    }

    fn draw_preview(&self, painter: &egui::Painter, metrics: &Metrics) {
        let center = metrics.preview_center;
        let radius = metrics.preview_radius.max(2.0);
        painter.circle_filled(
            center + vec2(0.0, 2.0),
            radius,
            Color32::from_black_alpha(90),
        );
        painter.circle_filled(center, radius, theme::color32(self.color.rgb()));
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.0_f32, Color32::from_white_alpha(55)),
        );
        painter.circle_stroke(
            center,
            radius + 1.0,
            Stroke::new(1.0_f32, Color32::from_black_alpha(55)),
        );
    }

    fn draw_hex_pill(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
        now: f64,
    ) {
        let rect = metrics.hex_pill;
        let response = ui.interact(rect, Id::new("colorms-hex"), Sense::click());
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let hover =
            ctx.animate_bool_with_time(Id::new("colorms-hex-hover"), response.hovered(), 0.12);
        if response.clicked() {
            self.copy_current(ctx, now);
        }
        let painter = ui.painter();
        let radius = ui::corner_radius(metrics.pill_radius);
        painter.rect_filled(
            rect,
            radius,
            theme::lerp_color(theme.surface, theme.surface_hover, hover),
        );
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(
                1.0_f32,
                theme::lerp_color(theme.outline, theme.outline_strong, hover),
            ),
            StrokeKind::Inside,
        );
        let font_size = (16.0 * layout.scale).clamp(13.0, 23.0);
        let font = ui::font_mono(font_size);
        // Optical centring: the text layout box includes descender space, so
        // CENTER_CENTER puts a digits-only string a few pixels too high.
        painter.text(
            rect.center() + vec2(0.0, font_size * 0.17),
            Align2::CENTER_CENTER,
            self.color.hex(),
            font,
            theme.text,
        );
    }

    fn draw_copy_card(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
        now: f64,
    ) {
        let response = ui.interact(
            metrics.copy_union(),
            Id::new("colorms-copy"),
            Sense::click(),
        );
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let hover =
            ctx.animate_bool_with_time(Id::new("colorms-copy-hover"), response.hovered(), 0.14);
        if response.clicked() {
            self.copy_current(ctx, now);
        }

        let painter = ui.painter();
        let radius = ui::corner_radius(metrics.copy_radius);
        let hex = self.color.hex();
        let copied = self.copied_hex.as_deref() == Some(hex.as_str());
        let copied = ctx.animate_bool_with_time(Id::new("colorms-copy-filled"), copied, 0.18);
        let rest = theme::lerp_color(theme.text_muted, theme.text, 0.45 * hover);
        let stroke_color = theme::lerp_color(rest, Color32::WHITE, copied);
        let stroke = Stroke::new((1.35 * layout.scale).clamp(1.0, 2.6), stroke_color);

        // Both sheets are outlines only. The front sheet is painted with the
        // surface colour so it masks the back sheet where the two overlap;
        // together they read as the single folded-corner copy glyph from the
        // design. A successful copy turns the whole outline white.
        painter.rect_stroke(metrics.copy_back, radius, stroke, StrokeKind::Inside);
        let mask = gradient_color_at(layout, theme, metrics.copy_front.center().y);
        painter.rect_filled(metrics.copy_front, radius, mask);
        painter.rect_stroke(metrics.copy_front, radius, stroke, StrokeKind::Inside);
    }

    fn draw_footer(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        theme: &Theme,
        now: f64,
    ) {
        let footer = layout.footer;
        if footer.height() < 8.0 {
            return;
        }
        let painter = ui.painter();
        let center_y = footer.center().y;
        let label_font = ui::font_display(10.5);
        let label = "最近";
        painter.text(
            pos2(footer.left() + 24.0, center_y),
            Align2::LEFT_CENTER,
            label,
            label_font.clone(),
            theme.text_muted,
        );
        let label_w = text_width(painter, label, label_font);
        let dots_left = footer.left() + 24.0 + label_w + 18.0;

        let hint_font = ui::font_body(9.5);
        let hint = "点击恢复 · 右键删除";
        let hint_w = text_width(painter, hint, hint_font.clone());
        let show_hint = footer.width() >= 560.0;
        let hint_left = footer.right() - 24.0 - hint_w;
        if show_hint && !self.recent.is_empty() {
            painter.text(
                pos2(hint_left, center_y),
                Align2::LEFT_CENTER,
                hint,
                hint_font,
                theme.text_muted,
            );
        }

        if self.recent.is_empty() {
            painter.text(
                pos2(dots_left, center_y),
                Align2::LEFT_CENTER,
                "调整颜色后会自动记录",
                ui::font_body(10.0),
                theme.text_muted,
            );
            return;
        }

        let radius = 7.5;
        let gap = 9.0;
        let step = radius * 2.0 + gap;
        let right_limit = if show_hint {
            hint_left - 16.0
        } else {
            footer.right() - 20.0
        };
        let max_dots = (((right_limit - dots_left + gap) / step).floor().max(0.0) as usize).min(12);
        let count = self.recent.len().min(max_dots);
        if count == 0 {
            return;
        }

        let current_hex = self.color.hex();
        let snapshot = self.recent.clone();
        let mut removed: Option<String> = None;

        for (index, stored) in snapshot.iter().take(count).enumerate() {
            let center = pos2(dots_left + radius + index as f32 * step, center_y);
            let hit = Rect::from_center_size(center, Vec2::splat(radius * 2.0 + 6.0));
            let response = ui.interact(hit, Id::new(("colorms-recent", index)), Sense::click());
            if response.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            let hover = ctx.animate_bool_with_time(
                Id::new(("colorms-recent-hover", index)),
                response.hovered(),
                0.10,
            );
            if response.clicked() {
                self.color = stored.color.clamped();
                self.dirty = true;
                self.touched = true;
            }
            if response.secondary_clicked() {
                removed = Some(stored.hex.clone());
            }

            let dot_radius = radius + hover * 1.2;
            let selected = stored.hex.eq_ignore_ascii_case(&current_hex);
            painter.circle_filled(
                center + vec2(0.0, 0.8),
                dot_radius,
                Color32::from_black_alpha(90),
            );
            painter.circle_filled(center, dot_radius, theme::color32(stored.color.rgb()));
            painter.circle_stroke(
                center,
                dot_radius,
                Stroke::new(
                    1.0_f32,
                    theme::lerp_color(
                        Color32::from_rgba_unmultiplied(255, 255, 255, 70),
                        Color32::WHITE,
                        hover,
                    ),
                ),
            );
            if selected {
                painter.circle_stroke(
                    center,
                    dot_radius + 3.0,
                    Stroke::new(1.8_f32, Color32::WHITE),
                );
            }
        }

        if let Some(hex) = removed {
            match self.db.delete_color(&hex) {
                Ok(_) => {
                    self.refresh_recent();
                    self.show_toast(format!("已移除 {hex}"), now);
                    ctx.request_repaint();
                }
                Err(err) => {
                    tracing::error!("failed to delete {hex}: {err}");
                    self.show_toast("删除失败：数据库不可写".to_owned(), now);
                }
            }
        }
    }

    fn draw_titlebar(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        theme: &Theme,
        painter: &egui::Painter,
        maximized: bool,
    ) {
        let title = layout.title;
        if title.height() < 10.0 {
            return;
        }
        let center_y = title.center().y;
        let icon_size = (title.height() * 0.47).clamp(16.0, 26.0);
        let icon_rect = Rect::from_min_size(
            pos2(title.left() + 16.0, center_y - icon_size * 0.5),
            Vec2::splat(icon_size),
        );
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(self.brand.id(), icon_rect, uv, Color32::WHITE);
        painter.text(
            pos2(icon_rect.right() + 10.0, center_y),
            Align2::LEFT_CENTER,
            "Colorms",
            ui::font_display(14.0),
            theme.title_text,
        );

        let button = 30.0;
        let gap = 4.0;
        let right = title.right() - 10.0;
        let close_rect =
            Rect::from_center_size(pos2(right - button * 0.5, center_y), Vec2::splat(button));
        let max_rect = close_rect.translate(vec2(-(button + gap), 0.0));
        let min_rect = max_rect.translate(vec2(-(button + gap), 0.0));

        // The drag area stops before the window buttons so their clicks never
        // start a window drag.
        let drag_rect = Rect::from_min_max(
            title.left_top(),
            pos2(min_rect.left() - 8.0, title.bottom()),
        );
        let drag = ui.interact(
            drag_rect,
            Id::new("colorms-titlebar"),
            Sense::click_and_drag(),
        );
        if drag.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }

        self.window_button(
            ctx,
            ui,
            painter,
            theme,
            min_rect,
            "colorms-win-min",
            WindowAction::Minimize,
            maximized,
        );
        self.window_button(
            ctx,
            ui,
            painter,
            theme,
            max_rect,
            "colorms-win-max",
            WindowAction::Maximize,
            maximized,
        );
        self.window_button(
            ctx,
            ui,
            painter,
            theme,
            close_rect,
            "colorms-win-close",
            WindowAction::Close,
            maximized,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn window_button(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        painter: &egui::Painter,
        theme: &Theme,
        rect: Rect,
        id: &str,
        action: WindowAction,
        maximized: bool,
    ) {
        let response = ui.interact(rect, Id::new(id), Sense::click());
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let hover = ctx.animate_bool_with_time(Id::new((id, "hover")), response.hovered(), 0.12);
        if hover > 0.001 {
            let bg = match action {
                WindowAction::Close => Color32::from_rgba_unmultiplied(
                    theme.danger.r(),
                    theme.danger.g(),
                    theme.danger.b(),
                    (80.0 * hover) as u8,
                ),
                _ => Color32::from_white_alpha((22.0 * hover) as u8),
            };
            painter.circle_filled(rect.center(), 14.0, bg);
        }

        let icon_color = match (action, response.hovered()) {
            (WindowAction::Close, true) => Color32::WHITE,
            (_, true) => Color32::WHITE,
            _ => theme.text_muted,
        };
        match action {
            WindowAction::Minimize => {
                ui::paint_minimize_icon(painter, rect.center(), 10.0, icon_color);
            }
            WindowAction::Maximize => {
                ui::paint_maximize_icon(painter, rect.center(), 10.0, icon_color, maximized);
            }
            WindowAction::Close => {
                ui::paint_close_icon(painter, rect.center(), 10.0, icon_color);
            }
        }

        if response.clicked() {
            match action {
                WindowAction::Minimize => {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                WindowAction::Maximize => {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                WindowAction::Close => {
                    self.persist_now();
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    fn draw_toast(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        layout: &Layout,
        theme: &Theme,
        now: f64,
    ) {
        let Some(toast) = &self.toast else {
            return;
        };
        let alpha = toast.alpha(now);
        if alpha <= 0.001 {
            return;
        }
        let font = ui::font_body(12.0);
        let max_width = (layout.full.width() - 48.0).max(80.0);
        let text_color = Color32::from_rgba_unmultiplied(230, 225, 235, (238.0 * alpha) as u8);
        let galley = painter.layout(toast.text.clone(), font, text_color, max_width);
        let size = galley.size();
        let padding = vec2(14.0, 8.0);
        let rect_size = size + padding * 2.0;
        let slide = (1.0 - alpha) * 6.0;
        let center = pos2(
            layout.full.center().x,
            layout.footer.top() - 20.0 - rect_size.y * 0.5 - slide,
        );
        let rect = Rect::from_center_size(center, rect_size);
        painter.rect_filled(
            rect,
            CornerRadius::same(rect_size.y.min(24.0).round() as u8),
            Color32::from_rgba_unmultiplied(
                theme.toast_bg.r(),
                theme.toast_bg.g(),
                theme.toast_bg.b(),
                (240.0 * alpha) as u8,
            ),
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(rect_size.y.min(24.0).round() as u8),
            Stroke::new(
                1.0_f32,
                Color32::from_rgba_unmultiplied(255, 255, 255, (36.0 * alpha) as u8),
            ),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - size * 0.5, galley, text_color);
        ctx.request_repaint();
    }

    fn draw_resize_handles(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        maximized: bool,
    ) {
        if maximized || layout.full.width() < 40.0 || layout.full.height() < 40.0 {
            return;
        }
        let full = layout.full;
        let g = ui::RESIZE_GRAB;
        let c = 14.0;
        let strips: [(&str, Rect, ResizeDirection, CursorIcon); 4] = [
            (
                "colorms-resize-n",
                Rect::from_min_max(
                    pos2(full.left() + c, full.top()),
                    pos2(full.right() - c, full.top() + g),
                ),
                ResizeDirection::North,
                CursorIcon::ResizeVertical,
            ),
            (
                "colorms-resize-s",
                Rect::from_min_max(
                    pos2(full.left() + c, full.bottom() - g),
                    pos2(full.right() - c, full.bottom()),
                ),
                ResizeDirection::South,
                CursorIcon::ResizeVertical,
            ),
            (
                "colorms-resize-w",
                Rect::from_min_max(
                    pos2(full.left(), full.top() + c),
                    pos2(full.left() + g, full.bottom() - c),
                ),
                ResizeDirection::West,
                CursorIcon::ResizeHorizontal,
            ),
            (
                "colorms-resize-e",
                Rect::from_min_max(
                    pos2(full.right() - g, full.top() + c),
                    pos2(full.right(), full.bottom() - c),
                ),
                ResizeDirection::East,
                CursorIcon::ResizeHorizontal,
            ),
        ];
        let corners: [(&str, Rect, ResizeDirection, CursorIcon); 4] = [
            (
                "colorms-resize-nw",
                Rect::from_min_max(full.left_top(), pos2(full.left() + c, full.top() + c)),
                ResizeDirection::NorthWest,
                CursorIcon::ResizeNwSe,
            ),
            (
                "colorms-resize-ne",
                Rect::from_min_max(
                    pos2(full.right() - c, full.top()),
                    pos2(full.right(), full.top() + c),
                ),
                ResizeDirection::NorthEast,
                CursorIcon::ResizeNeSw,
            ),
            (
                "colorms-resize-sw",
                Rect::from_min_max(
                    pos2(full.left(), full.bottom() - c),
                    pos2(full.left() + c, full.bottom()),
                ),
                ResizeDirection::SouthWest,
                CursorIcon::ResizeNeSw,
            ),
            (
                "colorms-resize-se",
                Rect::from_min_max(
                    pos2(full.right() - c, full.bottom() - c),
                    pos2(full.right(), full.bottom()),
                ),
                ResizeDirection::SouthEast,
                CursorIcon::ResizeNwSe,
            ),
        ];
        let primary_pressed = ctx.input(|i| i.pointer.primary_pressed());
        for (name, rect, direction, cursor) in strips.into_iter().chain(corners) {
            let response = ui.interact(rect, Id::new(name), Sense::drag());
            if response.hovered() {
                ctx.set_cursor_icon(cursor);
            }
            if primary_pressed && response.is_pointer_button_down_on() {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
            }
        }
    }
}

impl eframe::App for ColormsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.startup_logged {
            self.startup_logged = true;
            tracing::info!(
                ms = self.startup.elapsed().as_secs_f64() * 1000.0,
                "startup: first update"
            );
        }
        let now = ctx.input(|i| i.time);
        let full = ctx.content_rect();
        if full.width() < 50.0 || full.height() < 50.0 {
            return;
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        #[cfg(target_os = "windows")]
        self.update_rounded_region(ctx, maximized);
        self.track_size(full.size(), maximized);

        if let Some((text, duration)) = self.pending_toast.take() {
            self.toast = Some(Toast {
                text,
                start: now,
                duration,
            });
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            self.persist_now();
        }

        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C)) {
            self.copy_current(ctx, now);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.toast = None;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                self.draw(ctx, ui, full, now, maximized);
            });

        let primary_down = ctx.input(|i| i.pointer.primary_down());
        if self.dirty && !primary_down {
            self.commit(now);
        }
        if let Some(toast) = &self.toast {
            let elapsed = now - toast.start;
            if elapsed >= toast.duration as f64 {
                self.toast = None;
            } else {
                // Keep animating the fade even on the frame the toast was created.
                ctx.request_repaint();
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // The window is opaque now; rounded corners come from the Win32
        // window region, so the GL clear colour just needs to match the
        // chassis and avoid a black flash before the first painted frame.
        [32.0 / 255.0, 32.0 / 255.0, 32.0 / 255.0, 1.0]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_now();
        tracing::info!("Colorms exited");
    }
}

fn gradient_color_at(layout: &Layout, theme: &Theme, y: f32) -> Color32 {
    let height = layout.content.height().max(1.0);
    let t = ((y - layout.content.top()) / height).clamp(0.0, 1.0);
    theme::lerp_color(theme.surface_top, theme.surface_bottom, t)
}

fn text_width(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

fn build_font_definitions() -> FontDefinitions {
    let started = Instant::now();
    let mut fonts = FontDefinitions::default();
    let fonts_dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
        .join("Fonts");

    let candidates = [
        ("Segoe UI", "segoeui.ttf"),
        ("Segoe UI Bold", "segoeuib.ttf"),
        ("DengXian", "Deng.ttf"),
        ("Consolas", "consola.ttf"),
    ];
    let mut loaded: Vec<&str> = Vec::new();
    for (key, file) in candidates {
        match std::fs::read(fonts_dir.join(file)) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                loaded.push(key);
            }
            Err(err) => {
                tracing::debug!("font {file} unavailable: {err}");
            }
        }
    }
    let has = |key: &str| loaded.contains(&key);

    let default_prop = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let default_mono = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mut proportional = Vec::new();
    if has("Segoe UI") {
        proportional.push("Segoe UI".to_owned());
    }
    if has("DengXian") {
        proportional.push("DengXian".to_owned());
    }
    proportional.extend(default_prop.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);

    let mut monospace = Vec::new();
    if has("Consolas") {
        monospace.push("Consolas".to_owned());
    }
    if has("DengXian") {
        monospace.push("DengXian".to_owned());
    }
    monospace.extend(default_mono.iter().cloned());
    fonts.families.insert(FontFamily::Monospace, monospace);

    let mut display = Vec::new();
    for key in ["Segoe UI Bold", "Segoe UI", "DengXian"] {
        if has(key) && !display.iter().any(|existing: &String| existing == key) {
            display.push(key.to_owned());
        }
    }
    display.extend(default_prop);
    fonts
        .families
        .insert(FontFamily::Name(FONT_DISPLAY.into()), display);

    let mut mono_display = Vec::new();
    if has("Consolas") {
        mono_display.push("Consolas".to_owned());
    }
    if has("DengXian") {
        mono_display.push("DengXian".to_owned());
    }
    mono_display.extend(default_mono);
    fonts
        .families
        .insert(FontFamily::Name(FONT_MONO.into()), mono_display);

    tracing::info!(
        ms = started.elapsed().as_secs_f64() * 1000.0,
        loaded = ?loaded,
        "prepared system fonts"
    );
    fonts
}

#[cfg(target_os = "windows")]
mod win32_region {
    #[repr(C)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    unsafe extern "system" {
        pub fn GetClientRect(hwnd: isize, rect: *mut Rect) -> i32;
        pub fn SetWindowRgn(hwnd: isize, region: isize, redraw: i32) -> i32;
        pub fn CreateRoundRectRgn(
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
            width: i32,
            height: i32,
        ) -> isize;
        pub fn DeleteObject(object: isize) -> i32;
        fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn SetWindowPos(
            hwnd: isize,
            after: isize,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> i32;
        fn SetClassLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn CreateSolidBrush(color: u32) -> isize;
    }

    const GWL_STYLE: i32 = -16;
    const GCLP_HBRBACKGROUND: i32 = -10;
    const WS_CAPTION: isize = 0x00C0_0000;
    const WS_SYSMENU: isize = 0x0008_0000;
    const WS_MINIMIZEBOX: isize = 0x0002_0000;
    const WS_MAXIMIZEBOX: isize = 0x0001_0000;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_FRAMECHANGED: u32 = 0x0020;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    pub unsafe fn prepare_for_show(hwnd: isize) {
        // Drop the native caption/sysmenu bits that DWM uses for the pre-paint
        // ghost. The app paints its own titlebar and window buttons.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        let stripped = style & !(WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX);
        if stripped != style {
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_STYLE, stripped);
                SetWindowPos(
                    hwnd,
                    0,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        // The class background is what Windows paints before the first GL
        // frame; make it the same #202020 as the window chassis instead of
        // white.
        let brush = unsafe { CreateSolidBrush(0x0020_2020) };
        if brush != 0 {
            unsafe {
                SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
            }
        }

        let on: i32 = 1;
        // DWMWA_NCRENDERING_POLICY = 2, DWMNCRP_DISABLED = 1.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                2,
                (&on as *const i32).cast::<core::ffi::c_void>(),
                std::mem::size_of::<i32>() as u32,
            )
        };
        // DWMWA_TRANSITIONS_FORCEDISABLED = 3.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                3,
                (&on as *const i32).cast::<core::ffi::c_void>(),
                std::mem::size_of::<i32>() as u32,
            )
        };
    }
}
