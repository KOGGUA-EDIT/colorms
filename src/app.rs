//! The Colorms window: custom-painted Material You picker.
//!
//! This is a faithful port of the original egui painter code onto the CPU
//! rasterizer in [`crate::gfx`]. All geometry, colours and interactions match
//! the previous build; only the shell (window bootstrap and presentation) is
//! different, so the app no longer needs eframe, winit's GL bridge or a GPU
//! driver at startup.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui::{
    self, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontData, FontDefinitions,
    FontFamily, Id, PointerButton, Rect, ResizeDirection, Sense, TextureHandle, TextureOptions, Ui,
    Vec2, pos2, vec2,
};

use crate::color::PickerColor;
use crate::db::Db;
use crate::theme::{self, Theme};
use crate::trace;
use crate::ui::{self, FONT_DISPLAY, FONT_MONO, Layout, Metrics};

const BRAND_BYTES: &[u8] = include_bytes!("../assets/brand_96.rgba");
const BRAND_SIZE: usize = 96;

const DEFAULT_DURATION: f32 = 1.9;
/// Resolution of the baked ambient bloom (it is a smooth radial ramp).
const GLOW_SIZE: usize = 128;

/// Window operations the app asks the shell to perform. The old build sent
/// `eframe` viewport commands; the new shell executes these directly with winit.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    Minimize,
    ToggleMaximize,
    Close,
    StartDrag,
    Resize(ResizeDirection),
}

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

/// CPU-only startup assets, built on background threads while the shell creates
/// the window. By the time the app is constructed they are normally ready.
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
                trace::mark(&format!(
                    "startup: wheel image generated ({:.1} ms)",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
                image
            })
            .expect("could not spawn the wheel texture thread");
        Self { fonts, wheel }
    }

    fn join(self, startup: Instant) -> (FontDefinitions, ColorImage) {
        let fonts = match self.fonts.join() {
            Ok(fonts) => fonts,
            Err(err) => {
                eprintln!("font loader thread failed: {err:?}");
                build_font_definitions()
            }
        };
        let wheel = match self.wheel.join() {
            Ok(image) => image,
            Err(err) => {
                eprintln!("wheel texture thread failed: {err:?}");
                ui::wheel_image(768)
            }
        };
        trace::mark(&format!(
            "startup: background assets joined ({:.1} ms)",
            startup.elapsed().as_secs_f64() * 1000.0
        ));
        (fonts, wheel)
    }
}

pub struct ColormsApp {
    color: PickerColor,
    db: Db,
    toast: Option<Toast>,
    pending_toast: Option<(String, f32)>,

    wheel: TextureHandle,
    slider: TextureHandle,
    brand: TextureHandle,
    /// Baked ambient bloom; regenerated when the accent colour changes.
    glow: Option<TextureHandle>,
    glow_key: [u8; 4],
    /// Anti-aliased corner mask for the window outline.
    border_corner: Option<TextureHandle>,
    border_side: u32,

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
    /// Window commands collected while painting, executed by the shell.
    pub commands: Vec<Command>,
}

impl ColormsApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &egui::Context,
        db: Db,
        initial_color: PickerColor,
        initial_size: [f32; 2],
        db_note: Option<String>,
        startup: Instant,
        assets: StartupAssets,
    ) -> Self {
        trace::mark("startup: app::new enter");
        ctx.set_visuals(egui::Visuals::dark());
        trace::mark("startup: visuals set");
        let (fonts, wheel_image) = assets.join(startup);
        ctx.set_fonts(fonts);
        trace::mark("startup: fonts installed");

        let wheel = ctx.load_texture("colorms-wheel", wheel_image, TextureOptions::LINEAR);
        trace::mark("startup: wheel texture");
        let slider = ctx.load_texture(
            "colorms-brightness",
            ui::brightness_image(108, 700),
            TextureOptions::LINEAR,
        );
        trace::mark("startup: slider texture");
        let brand = ctx.load_texture(
            "colorms-brand",
            ColorImage::from_rgba_unmultiplied([BRAND_SIZE, BRAND_SIZE], BRAND_BYTES),
            TextureOptions::LINEAR,
        );
        trace::mark("startup: slider+brand textures");

        let mut app = Self {
            color: initial_color.clamped(),
            db,
            toast: None,
            pending_toast: None,
            wheel,
            slider,
            brand,
            glow: None,
            glow_key: [0; 4],
            border_corner: None,
            border_side: 0,
            dirty: false,
            touched: false,
            copied_hex: None,
            last_size: vec2(initial_size[0], initial_size[1]),
            persisted_size: vec2(initial_size[0], initial_size[1]),
            last_size_change: Instant::now(),
            startup,
            startup_logged: false,
            commands: Vec::new(),
        };
        if let Some(note) = db_note {
            app.pending_toast = Some((note, 3.6));
        }
        app
    }

    fn copy_current(&mut self, ctx: &egui::Context, now: f64) {
        let hex = self.color.hex();
        ctx.copy_text(hex.clone());
        trace::mark(&format!("copied {hex} to clipboard"));
        self.copied_hex = Some(hex.clone());
        let saved = self.commit(now);
        if saved {
            self.show_toast(format!("已复制 {hex}"), now);
        }
    }

    /// Persist the selected colour. Returns false when the database write
    /// failed (the caller can surface that to the user).
    fn commit(&mut self, now: f64) -> bool {
        let mut ok = true;
        if let Err(err) = self.db.save_last_color(self.color) {
            ok = false;
            eprintln!("failed to save colour: {err}");
            self.show_toast("保存失败：数据库不可写".to_owned(), now);
        }
        self.dirty = false;
        self.touched = true;
        ok
    }

    pub fn persist_now(&mut self) {
        let _ = self.db.save_last_color(self.color);
        self.dirty = false;
        if self.last_size.x > 1.0 && self.last_size.y > 1.0 {
            let _ = self.db.save_window_size(self.last_size.x, self.last_size.y);
            self.persisted_size = self.last_size;
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
                Err(err) => eprintln!("failed to save window size: {err}"),
            }
            self.last_size_change = Instant::now();
        }
    }

    /// Corner mask for the window outline.
    ///
    /// The mask is generated in **physical pixels** and drawn at exactly that
    /// many pixels (the quad back in points is `side / ppp`), so every texel
    /// lands on one device pixel. Generating it in points and letting the
    /// composite scale it shifted the arc by half a texel against the straight
    /// segments, which read as the outline being one pixel out at the corners.
    fn border_corner_texture(
        &mut self,
        ctx: &egui::Context,
        radius: f32,
        thickness: f32,
        ppp: f32,
    ) -> (egui::TextureId, f32) {
        let radius_px = (radius * ppp).round().max(2.0);
        // Half a pixel of optical correction: an anti-aliased band of exactly
        // `thickness` pixels only has `thickness - 1` fully covered pixels once
        // it runs diagonally, so the arc would read thinner than the straight
        // parts of the same outline.
        let thickness_px = (thickness * ppp + 0.5).max(1.0);
        let side_px = radius_px + 2.0;
        let key = side_px as u32;
        if self.border_corner.is_none() || self.border_side != key {
            let image = ui::corner_ring_image(side_px as usize, radius_px, thickness_px);
            match &mut self.border_corner {
                Some(handle) => handle.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.border_corner = Some(ctx.load_texture(
                        "colorms-border",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            self.border_side = key;
        }
        let id = self
            .border_corner
            .as_ref()
            .map_or(egui::TextureId::default(), |handle| handle.id());
        (id, side_px / ppp)
    }

    /// Keep the ambient bloom in sync with the accent colour.
    fn glow_texture(&mut self, ctx: &egui::Context, accent: Color32) -> egui::TextureId {
        let key = accent.to_array();
        if self.glow.is_none() || self.glow_key != key {
            let image = ui::glow_image(GLOW_SIZE, accent);
            // Nearest sampling: the ramp is so faint (max alpha 31) that the
            // 4 px texel steps are invisible, while linear filtering would cost
            // four weighted fetches for every one of the 326k glow pixels.
            match &mut self.glow {
                Some(handle) => handle.set(image, egui::TextureOptions::NEAREST),
                None => {
                    self.glow = Some(ctx.load_texture(
                        "colorms-glow",
                        image,
                        egui::TextureOptions::NEAREST,
                    ));
                }
            }
            self.glow_key = key;
        }
        self.glow
            .as_ref()
            .map_or(egui::TextureId::default(), |handle| handle.id())
    }

    fn draw(&mut self, ctx: &egui::Context, ui: &Ui, full: Rect, now: f64, maximized: bool) {
        let layout = Layout::new(full);
        let theme = Theme::from_color(self.color);
        let metrics = Metrics::from_layout(&layout);
        let painter = ui.painter().clone();
        let radius = if maximized { 0 } else { ui::WINDOW_RADIUS };

        ui::paint_background(&painter, &layout, &theme, radius);
        let glow = self.glow_texture(ctx, theme.accent);
        ui::paint_ambient_glow(&painter, &layout, &metrics, glow);
        ui::titlebar_hairline(&painter, layout.title);

        self.draw_wheel(ctx, ui, &layout, &metrics, &theme);
        self.draw_brightness(ctx, ui, &layout, &metrics, &theme);
        self.draw_preview(&painter, &metrics);
        self.draw_hex_pill(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_copy_card(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_titlebar(ctx, ui, &layout, &theme, &painter, maximized);
        self.draw_toast(ctx, &painter, &layout, &theme, now);
        self.draw_resize_handles(ctx, ui, &layout, maximized);

        if !maximized {
            let radius = ui::WINDOW_RADIUS as f32;
            let ppp = ctx.pixels_per_point();
            let (corner, side) = self.border_corner_texture(ctx, radius, 2.0, ppp);
            // The strips overlap the corner masks so their union has no seam, and
            // an overlap is only invisible when both are painted with the same
            // opaque colour — so the translucent outline colour is flattened onto
            // the chassis colour first.
            ui::paint_window_border(
                &painter,
                layout.full,
                flatten(theme.window_border, theme.footer),
                radius,
                2.0 / ppp,
                side,
                corner,
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

        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter()
            .image(self.wheel.id(), rect, uv, Color32::WHITE);

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

        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let painter = ui.painter();
        painter.image(self.slider.id(), rect, uv, Color32::WHITE);

        // A small knob on the wheel side of the capsule makes the current
        // brightness obvious at a glance and mirrors Material sliders.
        let knob_center = pos2(
            rect.right() + layout.len(10.5),
            rect.bottom() - self.color.value * rect.height(),
        );
        let knob_radius = (6.5 * layout.scale).clamp(4.5, 10.0);
        let ring = (1.6 * layout.scale).clamp(1.0, 2.2);
        // Concentric discs instead of two `circle_stroke` calls: a 1–2 px ring
        // is tessellated into sub-pixel slivers that the rasterizer can only
        // approximate, which read as a frayed outline. Opaque discs stack
        // cleanly and leave a crisp ring.
        painter.circle_filled(knob_center + vec2(0.0, 1.5), knob_radius, theme.shadow);
        painter.circle_filled(
            knob_center,
            knob_radius + 1.0,
            Color32::from_black_alpha(60),
        );
        painter.circle_filled(knob_center, knob_radius, Color32::WHITE);
        painter.circle_filled(
            knob_center,
            (knob_radius - ring).max(1.0),
            theme::color32(self.color.rgb()),
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
        // The pill stays visible through its fill alone: the design's outlines
        // were dropped so the control area reads as flat Material surfaces.
        // The layer is flattened onto the backdrop and drawn opaque because the
        // rounded caps are composed from rects and discs, whose overlaps would
        // otherwise blend twice.
        let fill = over_backdrop(
            layout,
            metrics,
            theme,
            rect.center(),
            theme::lerp_color(theme.surface, theme.surface_hover, hover),
        );
        ui::fill_round_rect(painter, rect, metrics.pill_radius, fill);
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
        let hex = self.color.hex();
        let copied = self.copied_hex.as_deref() == Some(hex.as_str());
        let copied = ctx.animate_bool_with_time(Id::new("colorms-copy-filled"), copied, 0.18);

        // Two flat sheets instead of two outlines: the back sheet is a darker
        // surface, the front one sits on top and masks it where they overlap,
        // so the pair still reads as the folded-corner copy glyph. A successful
        // copy lifts the front sheet to white instead of recolouring a stroke.
        let front_layer = theme::lerp_color(theme.surface_hover, theme.surface_pressed, hover);
        let front_layer = theme::lerp_color(front_layer, Color32::from_white_alpha(190), copied);
        let back_layer = theme::lerp_color(theme.surface, theme.surface_hover, hover);
        let front = over_backdrop(
            layout,
            metrics,
            theme,
            metrics.copy_front.center(),
            front_layer,
        );
        let back = over_backdrop(
            layout,
            metrics,
            theme,
            metrics.copy_back.center(),
            back_layer,
        );
        ui::fill_round_rect(painter, metrics.copy_back, metrics.copy_radius, back);
        ui::fill_round_rect(painter, metrics.copy_front, metrics.copy_radius, front);
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
            self.commands.push(Command::ToggleMaximize);
        } else if drag.drag_started_by(PointerButton::Primary) {
            self.commands.push(Command::StartDrag);
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
                WindowAction::Minimize => self.commands.push(Command::Minimize),
                WindowAction::Maximize => self.commands.push(Command::ToggleMaximize),
                WindowAction::Close => {
                    self.persist_now();
                    self.commands.push(Command::Close);
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
        // Flat surface: the old 1 px outline was tessellated into sub-pixel
        // slivers and read as a frayed edge around the bubble.
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
                self.commands.push(Command::Resize(direction));
            }
        }
    }

    /// Paint one frame. Returns `true` when the shell should close the window.
    pub fn update(&mut self, ctx: &egui::Context, maximized: bool) -> bool {
        if !self.startup_logged {
            self.startup_logged = true;
            trace::mark(&format!(
                "startup: first update ({:.1} ms)",
                self.startup.elapsed().as_secs_f64() * 1000.0
            ));
        }
        let now = ctx.input(|i| i.time);
        let full = ctx.content_rect();
        if full.width() < 50.0 || full.height() < 50.0 {
            return false;
        }
        self.track_size(full.size(), maximized);

        if let Some((text, duration)) = self.pending_toast.take() {
            self.toast = Some(Toast {
                text,
                start: now,
                duration,
            });
        }

        // `egui-winit` turns Ctrl+C into `Event::Copy` instead of a `Key` press,
        // so the shortcut has to be recognised from the event list as well.
        let copy_requested = ctx.input(|i| {
            i.events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
                || (i.modifiers.command && i.key_pressed(egui::Key::C))
        });
        if copy_requested {
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
        false
    }
}

fn gradient_color_at(layout: &Layout, theme: &Theme, y: f32) -> Color32 {
    let height = layout.content.height().max(1.0);
    let t = ((y - layout.content.top()) / height).clamp(0.0, 1.0);
    theme::lerp_color(theme.surface_top, theme.surface_bottom, t)
}

/// Blends a translucent colour over an opaque one and drops the alpha channel.
/// Used where an opaque result is required — overlapping translucent pieces
/// double-blend at the seams, so the window outline is resolved up front.
fn flatten(color: Color32, under: Color32) -> Color32 {
    let a = color.a() as u32;
    let rest = 255 - a;
    let mix =
        |top: u8, bottom: u8| -> u8 { ((top as u32 * a + bottom as u32 * rest + 127) / 255) as u8 };
    Color32::from_rgb(
        mix(color.r(), under.r()),
        mix(color.g(), under.g()),
        mix(color.b(), under.b()),
    )
}

/// Flattens a translucent surface layer onto everything painted behind it (the
/// window gradient plus the ambient bloom) so the result can be painted opaque.
/// Translucent figures drawn as triangle fans smear at the seams where two
/// triangles share an edge, so flat controls are resolved here instead of at
/// blend time.
fn over_backdrop(
    layout: &Layout,
    metrics: &Metrics,
    theme: &Theme,
    pos: egui::Pos2,
    layer: Color32,
) -> Color32 {
    let base = gradient_color_at(layout, theme, pos.y);
    let radius = metrics.wheel_radius.max(1.0);
    let dx = (pos.x - metrics.wheel_center.x) / radius;
    let dy = (pos.y - metrics.wheel_center.y) / radius;
    // The bloom quad spans `wheel_radius * 1.55`, so its own uv radius is
    // `sqrt(dx^2 + dy^2)` — the 1.55 factor already sits in the quad size.
    let distance = (dx * dx + dy * dy).sqrt();
    let coverage = if distance >= 1.55 {
        0.0
    } else {
        let layers = (15.0f32 * (1.55 - distance) / 0.72).floor() + 1.0;
        1.0 - (1.0 - 2.0 / 255.0_f32).powi((layers as i32).min(16))
    };
    let accent = theme.accent;
    // `layer` is premultiplied (epaint stores surfaces that way), so a plain
    // src-over is just `layer + under * (1 - alpha)`.
    let inverse = 1.0 - layer.a() as f32 / 255.0;
    let flatten = |premult: u8, bloom: u8, back: u8| -> u8 {
        let under = bloom as f32 * coverage + back as f32 * (1.0 - coverage);
        (premult as f32 + under * inverse + 0.5) as u8
    };
    Color32::from_rgb(
        flatten(layer.r(), accent.r(), base.r()),
        flatten(layer.g(), accent.g(), base.g()),
        flatten(layer.b(), accent.b(), base.b()),
    )
}

pub fn build_font_definitions() -> FontDefinitions {
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
                eprintln!("font {file} unavailable: {err}");
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

    trace::mark(&format!(
        "prepared system fonts ({:.1} ms, {:?})",
        started.elapsed().as_secs_f64() * 1000.0,
        loaded
    ));
    fonts
}
