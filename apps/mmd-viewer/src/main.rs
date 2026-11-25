//! MMD Viewer, the desktop shell.
//!
//! Release builds hide the console window. Logging still goes to stderr when a
//! console is attached and to `logs/mmd-viewer.log`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod session;
mod ui;

use anyhow::Context;
use mmd_platform_windows::show_error_dialog;

fn main() {
    if let Err(error) = run() {
        let message = format!("{error:#}");
        eprintln!("MMD Viewer failed:\n{message}");
        if let Err(dialog_error) = show_error_dialog("MMD Viewer", &message) {
            eprintln!("failed to show the error dialog: {dialog_error}");
        }
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    mmd_telemetry::init().context("failed to initialize logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting MMD Viewer");
    let error = app::run()?;
    if let Some(error) = error {
        return Err(error);
    }
    tracing::info!("MMD Viewer shut down");
    Ok(())
}
