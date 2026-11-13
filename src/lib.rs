//! Colorms — a Material You colour maker.
//!
//! The binary entry point is `src/main.rs`; the library target exists so the
//! pure colour math and persistence can be unit tested and benchmarked.

pub mod app;
pub mod color;
pub mod db;
pub mod gfx;
#[cfg(windows)]
pub mod platform;
pub mod theme;
pub mod trace;
pub mod ui;
