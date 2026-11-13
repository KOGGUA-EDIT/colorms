#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use anyhow::Context as _;
use colorms::app::{ColormsApp, StartupAssets};
use colorms::color::PickerColor;
use colorms::db::{self, Db};
use eframe::egui;
use tracing_subscriber::EnvFilter;

const ICON_BYTES: &[u8] = include_bytes!("../assets/icon_256.rgba");
const ICON_SIZE: usize = 256;
const DEFAULT_SIZE: [f32; 2] = [640.0, 720.0];

fn main() -> anyhow::Result<()> {
    let startup = std::time::Instant::now();
    let _guard = init_tracing();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Colorms");
    tracing::info!(
        ms = startup.elapsed().as_secs_f64() * 1000.0,
        "startup: tracing ready"
    );

    // Read the fonts and generate the colour-wheel image while eframe creates
    // the window and its GL context; this keeps ~80 ms off the critical path.
    let startup_assets = StartupAssets::spawn();

    let (db, db_note) = open_db()?;
    tracing::info!(
        ms = startup.elapsed().as_secs_f64() * 1000.0,
        "startup: database ready"
    );
    let initial_color = match db.load_last_color() {
        Ok(Some(color)) => color,
        Ok(None) => PickerColor::default(),
        Err(err) => {
            tracing::warn!("could not restore last colour: {err}");
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
        tracing::info!(?path, "database ready");
    } else {
        tracing::warn!("running with an in-memory database");
    }

    let viewport = egui::ViewportBuilder::default()
        .with_title("Colorms")
        .with_inner_size(size)
        .with_min_inner_size([520.0, 580.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_icon(egui::IconData {
            rgba: ICON_BYTES.to_vec(),
            width: ICON_SIZE as u32,
            height: ICON_SIZE as u32,
        });

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        vsync: true,
        ..Default::default()
    };

    tracing::info!(
        ms = startup.elapsed().as_secs_f64() * 1000.0,
        "startup: entering eframe"
    );
    eframe::run_native(
        "Colorms",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ColormsApp::new(
                cc,
                db,
                initial_color,
                size,
                db_note,
                startup,
                startup_assets,
            )) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|err| anyhow::anyhow!("eframe failed to run: {err}"))
}

fn open_db() -> anyhow::Result<(Db, Option<String>)> {
    match Db::open_default() {
        Ok(db) => Ok((db, None)),
        Err(err) => {
            tracing::error!("cannot open default database: {err}");
            let db = Db::in_memory().context("could not create an in-memory database")?;
            Ok((db, Some("数据库不可用，本次改动只保存在内存".to_owned())))
        }
    }
}

fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = db::config_dir().join("logs");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create log directory {dir:?}: {err}");
        return None;
    }
    let appender = tracing_appender::rolling::daily(&dir, "colorms.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,colorms=debug"));
    let result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .try_init();
    if let Err(err) = result {
        eprintln!("could not install tracing subscriber: {err}");
        return None;
    }
    Some(guard)
}
