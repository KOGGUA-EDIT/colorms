//! The window shell: a winit window whose pixels are produced by the CPU
//! rasterizer in [`crate::gfx`] and blitted with GDI.
//!
//! The previous build used `eframe`, which pulls in a full GL stack: creating a
//! WGL context cost ~210 ms of GPU driver initialisation on the launch critical
//! path. This shell never creates a graphics device — the whole cold start is
//! window creation, font loading and one rasterized frame.

use std::time::{Duration, Instant};

use egui::Color32;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Icon, Window, WindowId};

use crate::app::{ColormsApp, Command, StartupAssets};
use crate::color::PickerColor;
use crate::db::Db;
use crate::gfx::Canvas;
use crate::trace;
use crate::ui;

/// Everything `main` prepared before the window exists.
pub struct Boot {
    /// Opened on a background thread so SQLite never blocks the first frame.
    pub session: std::thread::JoinHandle<Session>,
    pub startup: Instant,
    pub assets: StartupAssets,
}

/// Persisted state, loaded off the critical path.
pub struct Session {
    pub db: Db,
    pub initial_color: PickerColor,
    pub size: [f32; 2],
    pub db_note: Option<String>,
}

impl Session {
    fn fallback() -> Self {
        Self {
            db: Db::in_memory().unwrap_or_else(|_| Db::in_memory().expect("in-memory database")),
            initial_color: PickerColor::default(),
            size: DEFAULT_SIZE,
            db_note: Some("数据库不可用，本次改动只保存在内存".to_owned()),
        }
    }
}

/// Open the database and restore the last colour and window size.
pub fn open_session() -> Session {
    let (db, db_note) = match Db::open_default() {
        Ok(db) => (db, None),
        Err(err) => {
            eprintln!("cannot open the default database: {err}");
            match Db::in_memory() {
                Ok(db) => (db, Some("数据库不可用，本次改动只保存在内存".to_owned())),
                Err(err) => {
                    eprintln!("cannot create an in-memory database: {err}");
                    return Session::fallback();
                }
            }
        }
    };
    let initial_color = match db.load_last_color() {
        Ok(Some(color)) => color,
        Ok(None) => PickerColor::default(),
        Err(err) => {
            eprintln!("could not restore the last colour: {err}");
            PickerColor::default()
        }
    };
    let size = db
        .load_window_size()
        .ok()
        .flatten()
        .map(|[width, height]| [width.clamp(520.0, 1800.0), height.clamp(580.0, 1200.0)])
        .unwrap_or(DEFAULT_SIZE);
    if let Some(path) = db.path() {
        trace::mark(&format!("startup: database at {path:?}"));
    }
    trace::mark("startup: session ready");
    Session {
        db,
        initial_color,
        size,
        db_note,
    }
}

const ICON_BYTES: &[u8] = include_bytes!("../assets/icon_256.rgba");
const ICON_SIZE: u32 = 256;
const MIN_SIZE: [f64; 2] = [520.0, 580.0];
const DEFAULT_SIZE: [f32; 2] = [640.0, 720.0];
const BACKDROP: Color32 = Color32::from_rgb(32, 32, 32);

pub fn run(boot: Boot) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    trace::mark("startup: event loop ready");
    let mut host = Host::new(boot);
    event_loop.run_app(&mut host)?;
    Ok(())
}

struct Host {
    boot: Option<Boot>,
    window: Option<Window>,
    ctx: Option<egui::Context>,
    state: Option<egui_winit::State>,
    app: Option<ColormsApp>,
    canvas: Canvas,
    surface: Option<Surface>,
    region_key: (u32, u32, i32, bool),
    next_repaint: Option<Instant>,
    shown: bool,
    icon_set: bool,
    dumped: bool,
    frames: u32,
}

impl Host {
    fn new(boot: Boot) -> Self {
        Self {
            boot: Some(boot),
            window: None,
            ctx: None,
            state: None,
            app: None,
            canvas: Canvas::new(),
            surface: None,
            region_key: (0, 0, -1, false),
            next_repaint: None,
            shown: false,
            icon_set: false,
            dumped: false,
            frames: 0,
        }
    }

    fn create(&mut self, event_loop: &ActiveEventLoop) {
        let boot = self.boot.take().expect("the window was already created");
        trace::mark("startup: create() enter");
        trace::mark("startup: icon ready");
        let attributes = Window::default_attributes()
            .with_title("Colorms")
            .with_inner_size(LogicalSize::new(
                DEFAULT_SIZE[0] as f64,
                DEFAULT_SIZE[1] as f64,
            ))
            .with_min_inner_size(LogicalSize::new(MIN_SIZE[0], MIN_SIZE[1]))
            .with_decorations(false)
            .with_transparent(false)
            .with_visible(false);
        trace::mark("startup: window attributes built");
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                eprintln!("cannot create the Colorms window: {err}");
                event_loop.exit();
                return;
            }
        };
        trace::mark("startup: window created");
        center(&window, DEFAULT_SIZE);
        trace::mark("startup: window centred");

        let hwnd = window_handle(&window);
        Foreground::prepare(hwnd);
        trace::mark("startup: chrome prepared");

        // The database thread has been running since `main`; joining it here
        // keeps SQLite off the critical path while the window was created.
        let session = match boot.session.join() {
            Ok(session) => session,
            Err(_) => Session::fallback(),
        };
        if session.size != DEFAULT_SIZE {
            let _ = window.request_inner_size(LogicalSize::new(
                session.size[0] as f64,
                session.size[1] as f64,
            ));
            center(&window, session.size);
        }
        trace::mark("startup: session joined");
        let startup = boot.startup;
        let assets = boot.assets;

        let ctx = egui::Context::default();
        let mut canvas = Canvas::new();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            window.theme(),
            Some(canvas.texture_limit()),
        );
        let app = ColormsApp::new(
            &ctx,
            session.db,
            session.initial_color,
            session.size,
            session.db_note,
            startup,
            assets,
        );
        trace::mark("startup: egui state ready");
        canvas.resize(1, 1);
        self.surface = Surface::new(hwnd);
        self.window = Some(window);
        self.ctx = Some(ctx);
        self.state = Some(state);
        self.app = Some(app);
        self.canvas = canvas;
        self.apply_region();
    }

    fn apply_region(&mut self) {
        let (Some(window), Some(_)) = (self.window.as_ref(), self.surface.as_ref()) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor();
        let maximized = window.is_maximized();
        let radius = if maximized {
            0
        } else {
            (ui::WINDOW_RADIUS as f64 * scale).round() as i32
        };
        let key = (
            size.width,
            size.height,
            if maximized { -1 } else { radius },
            maximized,
        );
        if self.region_key == key {
            return;
        }
        self.region_key = key;
        Foreground::apply_region(window_handle(window), size.width, size.height, radius);
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(ctx), Some(state), Some(app), Some(surface)) = (
            self.window.as_ref(),
            self.ctx.as_ref(),
            self.state.as_mut(),
            self.app.as_mut(),
            self.surface.as_mut(),
        ) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }

        let raw_input = state.take_egui_input(window);
        let maximized = window.is_maximized();
        let profile = !self.shown;
        let t_run = Instant::now();
        let output = ctx.run(raw_input, |ctx| {
            app.update(ctx, maximized);
        });
        state.handle_platform_output(window, output.platform_output);

        let pixels_per_point = output.pixels_per_point;
        let t_tess = Instant::now();
        let primitives = ctx.tessellate(output.shapes, pixels_per_point);
        let tess = t_tess.elapsed();
        let primitive_count = primitives.len();
        let t_raster = Instant::now();
        self.canvas.resize(size.width, size.height);
        let t_clear = Instant::now();
        self.canvas.clear(BACKDROP);
        let t_upload = Instant::now();
        self.canvas.apply_textures(&output.textures_delta);
        let t_draw = Instant::now();
        self.canvas.rasterize(&primitives, pixels_per_point);
        let raster = t_raster.elapsed();
        if std::env::var_os("COLORMS_GFX_TRACE").is_some_and(|value| value != "0") {
            eprintln!(
                "gfx frame: resize {:.2} ms, clear {:.2} ms, upload {:.2} ms, draw {:.2} ms",
                (t_clear - t_raster).as_secs_f64() * 1000.0,
                (t_upload - t_clear).as_secs_f64() * 1000.0,
                (t_draw - t_upload).as_secs_f64() * 1000.0,
                (Instant::now() - t_draw).as_secs_f64() * 1000.0,
            );
        }

        let t_present = Instant::now();
        if !surface.present(size.width, size.height, self.canvas.pixels()) {
            eprintln!("could not present the Colorms frame");
        }
        let present = t_present.elapsed();
        let t_show = Instant::now();
        let mut icon_took = Duration::ZERO;
        if !self.shown {
            window.set_visible(true);
            self.shown = true;
            trace::mark("startup: first frame visible");
        }
        let show = t_show.elapsed();
        if !self.icon_set {
            // The taskbar icon costs ~15 ms to convert and is invisible until
            // the window is on screen, so it is set after the first frame.
            let t_icon = Instant::now();
            if let Some(icon) = load_icon() {
                window.set_window_icon(Some(icon));
            }
            icon_took = t_icon.elapsed();
            self.icon_set = true;
            trace::mark("startup: window icon installed");
        }
        self.frames += 1;
        if self.shown {
            if let Some(path) = std::env::var_os("COLORMS_DUMP") {
                let every = std::env::var_os("COLORMS_DUMP_EVERY").is_some();
                if every || !self.dumped {
                    self.dumped = true;
                    let mut path = std::path::PathBuf::from(path);
                    if every {
                        let stem = path
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "frame".to_owned());
                        path.set_file_name(format!("{stem}-{:04}.bmp", self.frames));
                    }
                    if let Err(err) = self.canvas.save_bmp(&path) {
                        eprintln!("cannot dump the frame to {path:?}: {err}");
                    } else {
                        trace::mark(&format!("startup: frame dumped to {path:?}"));
                    }
                }
            }
        }
        if profile {
            trace::mark(&format!(
                "frame: {} primitives, egui {:.1} ms, tessellate {:.1} ms, raster {:.1} ms, present {:.1} ms, show {:.1} ms, icon {:.1} ms",
                primitive_count,
                t_tess.duration_since(t_run).as_secs_f64() * 1000.0,
                tess.as_secs_f64() * 1000.0,
                raster.as_secs_f64() * 1000.0,
                present.as_secs_f64() * 1000.0,
                show.as_secs_f64() * 1000.0,
                icon_took.as_secs_f64() * 1000.0,
            ));
            trace::flush();
        }

        let commands: Vec<Command> = app.commands.drain(..).collect();
        for command in commands {
            match command {
                Command::Minimize => window.set_minimized(true),
                Command::ToggleMaximize => window.set_maximized(!maximized),
                Command::Close => {
                    app.persist_now();
                    event_loop.exit();
                }
                Command::StartDrag => {
                    let _ = window.drag_window();
                }
                Command::Resize(direction) => {
                    let _ = window.drag_resize_window(map_direction(direction));
                }
            }
        }

        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|viewport| viewport.repaint_delay)
            .unwrap_or(Duration::MAX);
        if delay.is_zero() {
            self.next_repaint = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            window.request_redraw();
        } else if delay < Duration::from_secs(3600) {
            let at = Instant::now() + delay;
            self.next_repaint = Some(at);
            event_loop.set_control_flow(ControlFlow::WaitUntil(at));
        } else {
            self.next_repaint = None;
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl ApplicationHandler for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        self.create(event_loop);
        self.draw(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        if !matches!(event, WindowEvent::RedrawRequested) {
            if let Some(state) = self.state.as_mut() {
                let response = state.on_window_event(window, &event);
                if response.repaint {
                    window.request_redraw();
                }
            }
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                if let Some(app) = self.app.as_mut() {
                    app.persist_now();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.apply_region();
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let Some(at) = self.next_repaint else {
            return;
        };
        if Instant::now() >= at {
            self.next_repaint = None;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(app) = self.app.as_mut() {
            app.persist_now();
        }
        trace::flush();
        self.surface = None;
    }
}

fn map_direction(direction: egui::ResizeDirection) -> winit::window::ResizeDirection {
    use winit::window::ResizeDirection as Winit;
    match direction {
        egui::ResizeDirection::East => Winit::East,
        egui::ResizeDirection::North => Winit::North,
        egui::ResizeDirection::NorthEast => Winit::NorthEast,
        egui::ResizeDirection::NorthWest => Winit::NorthWest,
        egui::ResizeDirection::South => Winit::South,
        egui::ResizeDirection::SouthEast => Winit::SouthEast,
        egui::ResizeDirection::SouthWest => Winit::SouthWest,
        egui::ResizeDirection::West => Winit::West,
    }
}

fn load_icon() -> Option<Icon> {
    match Icon::from_rgba(ICON_BYTES.to_vec(), ICON_SIZE, ICON_SIZE) {
        Ok(icon) => Some(icon),
        Err(err) => {
            eprintln!("cannot load the window icon: {err}");
            None
        }
    }
}

fn center(window: &Window, size: [f32; 2]) {
    let Some(monitor) = window.current_monitor() else {
        return;
    };
    let monitor_size = monitor.size();
    let monitor_position = monitor.position();
    let scale = monitor.scale_factor();
    let width = (size[0] as f64 * scale) as i32;
    let height = (size[1] as f64 * scale) as i32;
    let x = monitor_position.x + (monitor_size.width as i32 - width) / 2;
    let y = monitor_position.y + (monitor_size.height as i32 - height) / 2;
    window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
}

fn window_handle(window: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle() {
        Ok(handle) => match handle.as_raw() {
            RawWindowHandle::Win32(win32) => win32.hwnd.get(),
            _ => 0,
        },
        Err(err) => {
            eprintln!("cannot get the native window handle: {err}");
            0
        }
    }
}

/// The offscreen DIB that every rasterized frame is copied into and blitted from.
struct Surface {
    hwnd: isize,
    memory_dc: isize,
    bitmap: isize,
    previous: isize,
    bits: *mut std::ffi::c_void,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(hwnd: isize) -> Option<Self> {
        if hwnd == 0 {
            return None;
        }
        unsafe {
            // The window DC is taken only for the blit itself: holding one
            // across messages keeps the window from validating its paint area.
            let window_dc = GetDC(hwnd);
            if window_dc == 0 {
                return None;
            }
            let memory_dc = CreateCompatibleDC(window_dc);
            ReleaseDC(hwnd, window_dc);
            if memory_dc == 0 {
                return None;
            }
            Some(Self {
                hwnd,
                memory_dc,
                bitmap: 0,
                previous: 0,
                bits: std::ptr::null_mut(),
                width: 0,
                height: 0,
            })
        }
    }

    fn ensure(&mut self, width: i32, height: i32) -> bool {
        if width == self.width && height == self.height && !self.bits.is_null() {
            return true;
        }
        unsafe {
            if self.bitmap != 0 {
                SelectObject(self.memory_dc, self.previous);
                DeleteObject(self.bitmap);
                self.bitmap = 0;
                self.bits = std::ptr::null_mut();
            }
            let mut info: BitmapInfo = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BitmapInfoHeader>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            info.bmiHeader.biSizeImage = (width as u32) * (height as u32) * 4;
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(self.memory_dc, &info, DIB_RGB_COLORS, &mut bits, 0, 0);
            if bitmap == 0 || bits.is_null() {
                return false;
            }
            self.previous = SelectObject(self.memory_dc, bitmap);
            self.bitmap = bitmap;
            self.bits = bits;
            self.width = width;
            self.height = height;
            true
        }
    }

    fn present(&mut self, width: u32, height: u32, pixels: &[u32]) -> bool {
        let (w, h) = (width as i32, height as i32);
        if w <= 0 || h <= 0 || !self.ensure(w, h) {
            return false;
        }
        let count = (w as usize) * (h as usize);
        if pixels.len() < count {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits as *mut u32, count);
            let window_dc = GetDC(self.hwnd);
            if window_dc == 0 {
                return false;
            }
            // GDI blits straight into the DWM redirection surface, so the
            // compositor does the actual presentation (and there is no tearing).
            BitBlt(window_dc, 0, 0, w, h, self.memory_dc, 0, 0, SRCCOPY);
            ReleaseDC(self.hwnd, window_dc);
            ValidateRect(self.hwnd, std::ptr::null());
        }
        true
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if self.bitmap != 0 {
                SelectObject(self.memory_dc, self.previous);
                DeleteObject(self.bitmap);
            }
            if self.memory_dc != 0 {
                DeleteDC(self.memory_dc);
            }
        }
    }
}

type Hdc = isize;

#[repr(C)]
#[allow(non_snake_case)]
struct BitmapInfoHeader {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
#[allow(non_snake_case)]
struct RgbQuad {
    rgbBlue: u8,
    rgbGreen: u8,
    rgbRed: u8,
    rgbReserved: u8,
}

#[repr(C)]
#[allow(non_snake_case)]
struct BitmapInfo {
    bmiHeader: BitmapInfoHeader,
    bmiColors: [RgbQuad; 1],
}

const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
const GWL_STYLE: i32 = -16;
const GCLP_HBRBACKGROUND: i32 = -10;
const SWP_FRAMECHANGED: u32 = 0x0020;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDC(hwnd: isize) -> Hdc;
    fn ReleaseDC(hwnd: isize, hdc: Hdc) -> i32;
    fn ValidateRect(hwnd: isize, rect: *const std::ffi::c_void) -> i32;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn SetClassLongPtrW(hwnd: isize, index: i32, value: isize) -> usize;
    fn SetWindowPos(
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
    fn SetWindowRgn(hwnd: isize, region: isize, redraw: i32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    fn DeleteDC(hdc: Hdc) -> i32;
    fn CreateDIBSection(
        hdc: Hdc,
        info: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut std::ffi::c_void,
        section: isize,
        offset: u32,
    ) -> isize;
    fn SelectObject(hdc: Hdc, object: isize) -> isize;
    fn DeleteObject(object: isize) -> i32;
    fn BitBlt(
        dst: Hdc,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        src: Hdc,
        src_x: i32,
        src_y: i32,
        rop: u32,
    ) -> i32;
    fn CreateRoundRectRgn(x1: i32, y1: i32, x2: i32, y2: i32, w: i32, h: i32) -> isize;
    fn CreateSolidBrush(color: u32) -> isize;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: isize,
        attribute: u32,
        value: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
}

/// Window dressing that has to happen before the first frame is shown.
struct Foreground;

impl Foreground {
    /// Removes the native caption and DWM animations, matching the old build.
    fn prepare(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let stripped = style & !(WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX);
            if stripped != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, stripped as isize);
                SetWindowPos(
                    hwnd,
                    0,
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            // The class brush paints the window background; the default white
            // flash would be visible between `ShowWindow` and our first blit.
            let brush = CreateSolidBrush(0x0020_2020); // 0x00BBGGRR
            if brush != 0 {
                SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
            }
            let disabled: i32 = 1;
            let value = &disabled as *const i32 as *const std::ffi::c_void;
            // 2 = DWMWA_NCRENDERING_POLICY, 3 = DWMWA_TRANSITIONS_FORCEDISABLED.
            DwmSetWindowAttribute(hwnd, 2, value, 4);
            // Disabling the open/close transitions removes the ~250 ms DWM
            // window animation that would otherwise sit between `ShowWindow`
            // and the first visible frame.
            DwmSetWindowAttribute(hwnd, 3, value, 4);
        }
    }

    fn apply_region(hwnd: isize, width: u32, height: u32, radius: i32) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            if radius <= 0 {
                SetWindowRgn(hwnd, 0, 1);
                return;
            }
            let region = CreateRoundRectRgn(
                0,
                0,
                width as i32 + 1,
                height as i32 + 1,
                radius * 2,
                radius * 2,
            );
            if region != 0 {
                // The system owns the region after a successful call.
                SetWindowRgn(hwnd, region, 1);
            }
        }
    }
}
