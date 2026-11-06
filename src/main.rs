// Release builds run without a console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod timeline;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    // Files passed on the command line, such as with "Open with", get imported.
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kantan Audio Manager")
            .with_inner_size([1100.0, 420.0])
            .with_min_inner_size([500.0, 240.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Kantan Audio Manager",
        options,
        Box::new(|creation| Ok(Box::new(app::App::new(&creation.egui_ctx, files)))),
    )
}
