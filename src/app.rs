//! The Colorms window: custom-painted Material You picker.
//!
//! This is a faithful port of the original egui painter code onto the CPU
//! rasterizer in [`crate::gfx`]. All geometry, colours and interactions match
//! the previous build; only the shell (window bootstrap and presentation) is
//! different, so the app no longer needs eframe, winit's GL bridge or a GPU
//! driver at startup.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui::{
    self, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontData, FontDefinitions,
    FontFamily, Id, PointerButton, Pos2, Rect, ResizeDirection, Sense, TextureHandle,
    TextureOptions, Ui, Vec2, pos2, vec2,
};

use crate::color::{PickerColor, Rgb8};
use crate::db::Db;
use crate::theme::{self, STATE_HOVER, STATE_PRESSED, Theme};
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
    StartEyedropper,
    CancelEyedropper,
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
    /// Analytic SDF anti-aliased corner masks for the window chassis border.
    corner_rings: HashMap<(u32, u32), TextureHandle>,
    /// Analytic SDF anti-aliased rounded-rect rings for controls, keyed by
    /// `(w_px, h_px, radius_px, thick_tenths)`.
    round_rect_rings: HashMap<(u32, u32, u32, u32), TextureHandle>,
    /// Analytic SDF anti-aliased full circle rings keyed by `(diameter_px, thick_tenths)`.
    circle_rings: HashMap<(u32, u32), TextureHandle>,

    dirty: bool,
    touched: bool,
    /// Hex value that was last copied; the primary copy button shows the
    /// confirmed checkmark state while this matches the active selection.
    copied_hex: Option<String>,
    /// True while the Windows screen eyedropper is actively sampling pixels.
    eyedropper_active: bool,
    /// Colour snapshot saved before eyedropper started, restored on cancel.
    eyedropper_saved_color: Option<PickerColor>,
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
            corner_rings: HashMap::new(),
            round_rect_rings: HashMap::new(),
            circle_rings: HashMap::new(),
            dirty: false,
            touched: false,
            copied_hex: None,
            eyedropper_active: false,
            eyedropper_saved_color: None,
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

    pub fn is_eyedropper_active(&self) -> bool {
        self.eyedropper_active
    }

    fn toggle_eyedropper(&mut self, now: f64) {
        if self.eyedropper_active {
            self.cancel_eyedropper(now);
        } else {
            self.eyedropper_saved_color = Some(self.color);
            self.eyedropper_active = true;
            self.commands.push(Command::StartEyedropper);
            self.show_toast("移动鼠标吸色，左键确认，右键/Esc 取消".to_owned(), now);
        }
    }

    /// Live-preview a pixel sampled from the screen while eyedropper is active.
    pub fn preview_eyedropper_color(&mut self, rgb: Rgb8) {
        if !self.eyedropper_active {
            return;
        }
        let next = PickerColor::from_rgb(rgb);
        if next.rgb() != self.color.rgb() {
            self.color = next;
        }
    }

    /// Confirm the sampled screen colour and exit eyedropper mode.
    pub fn confirm_eyedropper(&mut self, rgb: Rgb8, now: f64) {
        self.eyedropper_active = false;
        self.eyedropper_saved_color = None;
        self.color = PickerColor::from_rgb(rgb);
        let hex = self.color.hex();
        self.dirty = true;
        let saved = self.commit(now);
        if saved {
            self.show_toast(format!("已吸取 {hex}"), now);
        }
    }

    /// Cancel eyedropper mode and restore the colour from before picking.
    pub fn cancel_eyedropper(&mut self, now: f64) {
        if !self.eyedropper_active {
            return;
        }
        self.eyedropper_active = false;
        if let Some(prev) = self.eyedropper_saved_color.take() {
            self.color = prev;
        }
        self.commands.push(Command::CancelEyedropper);
        self.show_toast("已取消吸色".to_owned(), now);
    }

    /// Paint an analytic SDF anti-aliased rounded-rectangle outline.
    ///
    /// Small/medium controls use a single seamless SDF texture so the straight
    /// sides and corner arcs never show a tangent-point seam; the full-window
    /// chassis border uses 4 corner masks + 4 strips to keep texture memory
    /// tiny.
    #[allow(clippy::too_many_arguments)]
    fn paint_round_outline(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        radius: f32,
        thickness_dp: f32,
        color: Color32,
    ) {
        let ppp = ctx.pixels_per_point();
        let clamped_radius = radius
            .min(rect.width() * 0.5)
            .min(rect.height() * 0.5)
            .max(0.0);
        let w_px = (rect.width() * ppp).round().max(4.0) as u32 + 2;
        let h_px = (rect.height() * ppp).round().max(4.0) as u32 + 2;
        let thickness_px = (thickness_dp * ppp).max(1.0);

        if w_px <= 480 && h_px <= 480 {
            let radius_px = (clamped_radius * ppp).round();
            let key = (w_px, h_px, radius_px as u32, (thickness_px * 10.0).round() as u32);
            if self.round_rect_rings.len() > 32 && !self.round_rect_rings.contains_key(&key) {
                self.round_rect_rings.clear();
            }
            let handle = self.round_rect_rings.entry(key).or_insert_with(|| {
                let image = ui::round_rect_ring_image(
                    w_px as usize,
                    h_px as usize,
                    radius_px,
                    thickness_px,
                );
                ctx.load_texture(
                    format!("colorms-rr-{}-{}-{}-{}", key.0, key.1, key.2, key.3),
                    image,
                    TextureOptions::LINEAR,
                )
            });
            let draw_rect = Rect::from_center_size(
                rect.center(),
                vec2(w_px as f32 / ppp, h_px as f32 / ppp),
            );
            painter.image(
                handle.id(),
                draw_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                color,
            );
            return;
        }

        let radius_px = (clamped_radius * ppp).round().max(2.0);
        let side_px = radius_px as usize;
        let key = (side_px as u32, (thickness_px * 10.0).round() as u32);
        if self.corner_rings.len() > 16 && !self.corner_rings.contains_key(&key) {
            self.corner_rings.clear();
        }
        let handle = self.corner_rings.entry(key).or_insert_with(|| {
            let image = ui::corner_ring_image(side_px, radius_px, thickness_px);
            ctx.load_texture(
                format!("colorms-corner-{}-{}", key.0, key.1),
                image,
                TextureOptions::LINEAR,
            )
        });
        ui::paint_round_rect_stroke(
            painter,
            rect,
            clamped_radius,
            thickness_px / ppp,
            side_px as f32 / ppp,
            color,
            handle.id(),
        );
    }

    /// Paint an analytic SDF anti-aliased circular outline centred at `center`.
    #[allow(clippy::too_many_arguments)]
    fn paint_circle_outline(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        center: Pos2,
        radius: f32,
        thickness_dp: f32,
        color: Color32,
    ) {
        let ppp = ctx.pixels_per_point();
        let diameter_px = ((radius * 2.0 * ppp).round().max(8.0) as u32) + 2;
        let thickness_px = (thickness_dp * ppp).max(1.0);
        let key = (diameter_px, (thickness_px * 10.0).round() as u32);
        if self.circle_rings.len() > 24 && !self.circle_rings.contains_key(&key) {
            self.circle_rings.clear();
        }
        let handle = self.circle_rings.entry(key).or_insert_with(|| {
            let image = ui::circle_ring_image(diameter_px as usize, thickness_px);
            ctx.load_texture(
                format!("colorms-ring-{}-{}", key.0, key.1),
                image,
                TextureOptions::LINEAR,
            )
        });
        ui::paint_circle_ring(painter, center, diameter_px as f32 / ppp, color, handle.id());
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
        ui::paint_ambient_glow(&painter, &layout, &metrics, glow, radius);
        ui::titlebar_hairline(&painter, layout.title);

        self.draw_wheel(ctx, ui, &layout, &metrics, &theme);
        self.draw_brightness(ctx, ui, &layout, &metrics, &theme);
        self.draw_preview(ctx, &painter, &metrics, &theme);
        self.draw_hex_pill(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_eyedropper_button(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_copy_button(ctx, ui, &layout, &metrics, &theme, now);
        self.draw_titlebar(ctx, ui, &layout, &theme, &painter, maximized);
        self.draw_toast(ctx, &painter, &layout, &theme, now);
        self.draw_resize_handles(ctx, ui, &layout, maximized);

        if !maximized {
            let radius = ui::WINDOW_RADIUS as f32;
            let border_color = flatten(theme.window_border, theme.footer);
            self.paint_round_outline(ctx, &painter, layout.full, radius, 1.5, border_color);
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
        let hover = ctx.animate_bool_with_time(
            Id::new("colorms-wheel-hover"),
            response.hovered() || response.dragged(),
            0.15,
        );
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
        let painter = ui.painter().clone();
        painter.image(self.wheel.id(), rect, uv, Color32::WHITE);

        // Restored smooth analytic SDF rim outline around the colour wheel.
        let rim_color = theme::lerp_color(theme.outline_variant, theme.outline_emphasis, hover);
        self.paint_circle_outline(ctx, &painter, center, radius, 1.25, rim_color);

        let angle = self.color.hue.to_radians();
        let marker_center =
            center + vec2(angle.cos(), -angle.sin()) * (self.color.saturation * radius);
        let marker_radius = (10.0 * layout.scale).clamp(7.0, 14.0);
        let ring = (3.0 * layout.scale).clamp(2.0, 4.5);
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
        let hover = ctx.animate_bool_with_time(
            Id::new("colorms-brightness-hover"),
            response.hovered() || response.is_pointer_button_down_on(),
            0.14,
        );
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
        let painter = ui.painter().clone();
        painter.image(self.slider.id(), rect, uv, Color32::WHITE);

        // Restored smooth analytic SDF capsule outline so the dark bottom
        // edge stays crisp against the dark surface.
        let border_color = theme::lerp_color(theme.outline_variant, theme.outline_emphasis, hover);
        self.paint_round_outline(ctx, &painter, rect, rect.width() * 0.5, 1.25, border_color);

        // A small knob on the wheel side of the capsule makes the current
        // brightness obvious at a glance and mirrors Material sliders.
        let knob_center = pos2(
            rect.right() + layout.len(10.5),
            rect.bottom() - self.color.value * rect.height(),
        );
        let knob_radius = (6.5 * layout.scale).clamp(4.5, 10.0);
        let ring = (1.6 * layout.scale).clamp(1.0, 2.2);
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

    fn draw_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        metrics: &Metrics,
        theme: &Theme,
    ) {
        let center = metrics.preview_center;
        let radius = metrics.preview_radius.max(2.0);
        painter.circle_filled(
            center + vec2(0.0, 2.0),
            radius,
            Color32::from_black_alpha(72),
        );
        painter.circle_filled(center, radius, theme::color32(self.color.rgb()));
        // Restored analytic SDF ring so dark swatches remain clearly bounded.
        self.paint_circle_outline(ctx, painter, center, radius, 1.25, theme.outline_variant);
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
        let pressed = ctx.animate_bool_with_time(
            Id::new("colorms-hex-pressed"),
            response.is_pointer_button_down_on(),
            0.08,
        );
        if response.clicked() {
            self.copy_current(ctx, now);
        }
        let painter = ui.painter().clone();
        // MD3 Outlined / Tonal pill: surface-container-high + on-surface state
        // layer (8% hover, 10% pressed) + analytic SDF outline-variant border.
        let mut fill = theme::apply_state_layer(
            theme.surface_container_high,
            theme.on_surface,
            STATE_HOVER * hover,
        );
        if pressed > 0.001 {
            fill = theme::apply_state_layer(fill, theme.on_surface, STATE_PRESSED * pressed);
        }
        let pill_radius = metrics.pill_radius.min(rect.height() * 0.5);
        ui::fill_round_rect(&painter, rect, pill_radius, fill);
        let border_color = theme::lerp_color(theme.outline_variant, theme.outline_emphasis, hover);
        self.paint_round_outline(ctx, &painter, rect, pill_radius, 1.25, border_color);

        let font_size = (15.0 * layout.scale).clamp(13.0, 22.0);
        let font = ui::font_mono(font_size);
        painter.text(
            rect.center() + vec2(0.0, font_size * 0.14),
            Align2::CENTER_CENTER,
            self.color.hex(),
            font,
            theme.on_surface,
        );
    }

    /// Top-right MD3 Outlined / Toggle Icon Button for the screen eyedropper.
    fn draw_eyedropper_button(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
        now: f64,
    ) {
        let visual_rect = metrics.eyedropper;
        // Enforce MD3 >= 48x48dp touch target around the 44dp visual icon button.
        let hit_size = visual_rect.size().max(Vec2::splat(48.0 * layout.scale));
        let hit_rect = Rect::from_center_size(visual_rect.center(), hit_size);
        let response = ui.interact(hit_rect, Id::new("colorms-eyedropper"), Sense::click());
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        if response.clicked() {
            self.toggle_eyedropper(now);
        }

        let hover = ctx.animate_bool_with_time(
            Id::new("colorms-eyedropper-hover"),
            response.hovered(),
            0.12,
        );
        let pressed = ctx.animate_bool_with_time(
            Id::new("colorms-eyedropper-pressed"),
            response.is_pointer_button_down_on(),
            0.08,
        );
        let active = ctx.animate_bool_with_time(
            Id::new("colorms-eyedropper-active"),
            self.eyedropper_active,
            0.16,
        );

        let painter = ui.painter().clone();
        // M3 Expressive shape morphing: full circle at rest -> 12dp rounded
        // square when pressed or toggled active.
        let full_radius = visual_rect.width() * 0.5;
        let square_radius = layout.len(12.0);
        let morph = active.max(pressed * 0.85);
        let radius = full_radius + (square_radius - full_radius) * morph;

        let base_fill = theme::lerp_color(theme.surface_container_low, theme.accent_container, active);
        let fg = theme::lerp_color(
            theme::lerp_color(theme.on_surface_variant, theme.on_surface, hover),
            theme.on_accent_container,
            active,
        );
        let mut fill = theme::apply_state_layer(base_fill, fg, STATE_HOVER * hover);
        if pressed > 0.001 {
            fill = theme::apply_state_layer(fill, fg, STATE_PRESSED * pressed);
        }
        ui::fill_round_rect(&painter, visual_rect, radius, fill);

        let rest_border = theme::lerp_color(theme.outline_variant, theme.outline_emphasis, hover);
        let border_color = theme::lerp_color(rest_border, theme.primary, active);
        let border_width = 1.25 + 0.5 * active;
        self.paint_round_outline(ctx, &painter, visual_rect, radius, border_width, border_color);

        let icon_size = layout.len(22.0).clamp(16.0, 30.0);
        ui::paint_eyedropper_icon(&painter, visual_rect.center(), icon_size, fg);
    }

    /// Redesigned MD3 Primary Action Button (Regular 56dp FAB / Filled Tonal
    /// Icon Button) — the single primary action on screen.
    fn draw_copy_button(
        &mut self,
        ctx: &egui::Context,
        ui: &Ui,
        layout: &Layout,
        metrics: &Metrics,
        theme: &Theme,
        now: f64,
    ) {
        let rect = metrics.copy_button;
        let response = ui.interact(rect, Id::new("colorms-copy"), Sense::click());
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let hover =
            ctx.animate_bool_with_time(Id::new("colorms-copy-hover"), response.hovered(), 0.12);
        let pressed = ctx.animate_bool_with_time(
            Id::new("colorms-copy-pressed"),
            response.is_pointer_button_down_on(),
            0.08,
        );
        if response.clicked() {
            self.copy_current(ctx, now);
        }

        let painter = ui.painter().clone();
        let hex = self.color.hex();
        let is_copied = self.copied_hex.as_deref() == Some(hex.as_str());
        let copied = ctx.animate_bool_with_time(Id::new("colorms-copy-filled"), is_copied, 0.18);

        // MD3 Shape scale & Expressive Shape Morphing:
        // Regular FAB 16dp (large) -> 12dp (medium) on press -> full circle on copied.
        let base_radius = layout.len(16.0);
        let pressed_radius = layout.len(12.0);
        let copied_radius = rect.width() * 0.5;
        let radius = (base_radius + (copied_radius - base_radius) * copied)
            + (pressed_radius - base_radius) * pressed * (1.0 - copied);

        // MD3 colour roles: primary-container / on-primary-container at rest,
        // transitioning cleanly to primary / on-primary when copied.
        let container = theme::lerp_color(theme.accent_container, theme.primary, copied);
        let on_container = theme::lerp_color(theme.on_accent_container, theme.on_primary, copied);

        // State layer: 8% hover + 10% pressed using the foreground on-* role.
        let mut fill = theme::apply_state_layer(container, on_container, STATE_HOVER * hover);
        if pressed > 0.001 {
            fill = theme::apply_state_layer(fill, on_container, STATE_PRESSED * pressed);
        }

        // Subtle Level-1 elevation shadow (pure black <= 20% alpha), tucked
        // cleanly inside the button silhouette so it never shows as a hard line.
        ui::fill_round_rect(
            &painter,
            rect.shrink(0.5).translate(vec2(0.0, 2.0)),
            (radius - 0.5).max(0.0),
            Color32::from_black_alpha(44),
        );
        ui::fill_round_rect(&painter, rect, radius, fill);

        // Crisp 1.25px analytic SDF outline matching the restored stroke language.
        let border_color = theme::lerp_color(
            theme::lerp_color(theme.outline_variant, theme.outline_emphasis, hover),
            theme.primary,
            copied * 0.65,
        );
        self.paint_round_outline(ctx, &painter, rect, radius, 1.25, border_color);

        let icon_size = layout.len(24.0).clamp(18.0, 34.0);
        if copied > 0.5 {
            ui::paint_check_icon(&painter, rect.center(), icon_size, on_container);
        } else {
            ui::paint_copy_icon(&painter, rect.center(), icon_size, on_container);
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
            let layer = match action {
                WindowAction::Close => Color32::from_rgba_unmultiplied(
                    theme.danger.r(),
                    theme.danger.g(),
                    theme.danger.b(),
                    (80.0 * hover) as u8,
                ),
                _ => Color32::from_white_alpha((22.0 * hover) as u8),
            };
            // epaint closes a circle fan on an extra segment at angle zero, and
            // where its fringe triangles overlap a *translucent* fill each layer
            // adds its alpha again — that is the bright ragged seam on the right
            // of these buttons. Painting the disc opaque makes the overlap
            // invisible, so the layer is flattened onto the title bar first.
            painter.circle_filled(rect.center(), 14.0, over_premultiplied(layer, theme.footer));
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
        // MD3 Snackbar: extra-small 4dp radius, inverse-surface / inverse-on-surface.
        let font = ui::font_body(13.0);
        let max_width = (layout.full.width() - 48.0).max(80.0);
        let fg = theme.inverse_on_surface;
        let text_color = Color32::from_rgba_unmultiplied(
            fg.r(),
            fg.g(),
            fg.b(),
            (255.0 * alpha) as u8,
        );
        let galley = painter.layout(toast.text.clone(), font, text_color, max_width);
        let size = galley.size();
        let padding = vec2(16.0, 10.0);
        let rect_size = vec2(size.x + padding.x * 2.0, (size.y + padding.y * 2.0).max(40.0));
        let slide = (1.0 - alpha) * 6.0;
        let center = pos2(
            layout.full.center().x,
            layout.footer.top() - 20.0 - rect_size.y * 0.5 - slide,
        );
        let rect = Rect::from_center_size(center, rect_size);
        let bg = theme.inverse_surface;
        painter.rect_filled(
            rect.translate(vec2(0.0, 2.0)),
            CornerRadius::same(4),
            Color32::from_black_alpha((64.0 * alpha) as u8),
        );
        painter.rect_filled(
            rect,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), (246.0 * alpha) as u8),
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
        if ctx.input(|i| !i.modifiers.command && !i.modifiers.alt && i.key_pressed(egui::Key::I)) {
            self.toggle_eyedropper(now);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.eyedropper_active {
                self.cancel_eyedropper(now);
            } else {
                self.toast = None;
            }
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

/// Composites a premultiplied layer over an opaque colour and drops the alpha
/// channel. Overlapping translucent geometry (epaint's fan seams, for instance)
/// becomes invisible once the result is opaque.
fn over_premultiplied(top: Color32, under: Color32) -> Color32 {
    let a = top.a() as u32;
    let rest = 255 - a;
    let mix =
        |premult: u8, bottom: u8| -> u8 { (premult as u32 + bottom as u32 * rest / 255) as u8 };
    Color32::from_rgb(
        mix(top.r(), under.r()),
        mix(top.g(), under.g()),
        mix(top.b(), under.b()),
    )
}

/// Blends a translucent colour over an opaque one and drops the alpha channel.
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
