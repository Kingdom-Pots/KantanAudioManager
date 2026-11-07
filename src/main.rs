// Release builds run without a console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use eframe::egui;
use kantan_audio_manager::app::App;

/// The browser version draws into the page's canvas.
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;
    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("kantan_canvas"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("the page has a canvas with the id kantan_canvas");
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|creation| Ok(Box::new(App::new(&creation.egui_ctx, Vec::new())))),
            )
            .await;
        if let Err(error) = started {
            web_sys::console::error_1(&error);
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    // Files passed on the command line, such as with "Open with", get imported.
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kantan Audio Manager")
            .with_inner_size([1100.0, 420.0])
            .with_min_inner_size([840.0, 420.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/logo/exports/kantan_256.png"))
                    .expect("the embedded icon is a valid PNG"),
            ),
        ..Default::default()
    };

    eframe::run_native(
        "Kantan Audio Manager",
        options,
        Box::new(|creation| Ok(Box::new(App::new(&creation.egui_ctx, files)))),
    )
}
