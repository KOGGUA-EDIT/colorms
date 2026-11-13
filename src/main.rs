#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::time::Instant;

use anyhow::Context as _;
use colorms::app::StartupAssets;
use colorms::platform::{self, Boot};
use colorms::trace;

fn main() -> anyhow::Result<()> {
    trace::mark("main enter");
    let startup = Instant::now();

    // Read the system fonts and generate the colour-wheel image on background
    // threads while the shell creates the window; this keeps them off the
    // critical path to the first frame.
    let assets = StartupAssets::spawn();

    // SQLite is opened on its own thread too: the window and the first frame do
    // not depend on it, so the join happens once the window already exists.
    let session = std::thread::Builder::new()
        .name("colorms-db".to_owned())
        .spawn(platform::open_session)
        .context("could not start the database thread")?;

    platform::run(Boot {
        session,
        startup,
        assets,
    })
}
