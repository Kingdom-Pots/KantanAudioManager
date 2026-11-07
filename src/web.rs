//! The browser version's stand-ins for files and folders: picking files, reading
//! dropped files, downloads and local storage.

use std::sync::mpsc::Sender;

use eframe::egui;
use wasm_bindgen::JsCast;

/// Files the browser handed over, delivered on a later frame.
pub enum WebEvent {
    /// Audio files to import, as names and contents.
    Import(Vec<(String, Vec<u8>)>),
    /// A project file to open.
    Open(String, Vec<u8>),
}

/// Asks the browser for files. The answer arrives through `sender`.
pub fn pick_files(sender: Sender<WebEvent>, ctx: egui::Context, audio: bool) {
    wasm_bindgen_futures::spawn_local(async move {
        let dialog = if audio {
            rfd::AsyncFileDialog::new().add_filter("Audio", &crate::import::SUPPORTED_EXTENSIONS)
        } else {
            rfd::AsyncFileDialog::new().add_filter("Kantan project", &[crate::project_file::EXTENSION])
        };
        let handles = if audio {
            dialog.pick_files().await.unwrap_or_default()
        } else {
            dialog.pick_file().await.into_iter().collect()
        };
        let mut files = Vec::new();
        for handle in handles {
            files.push((handle.file_name(), handle.read().await));
        }
        let event = match (audio, files.pop()) {
            (_, None) => return,
            (false, Some((name, bytes))) => WebEvent::Open(name, bytes),
            (true, Some(last)) => {
                files.push(last);
                WebEvent::Import(files)
            }
        };
        let _ = sender.send(event);
        ctx.request_repaint();
    });
}

/// Reads files dropped on the page.
pub fn read_dropped(files: Vec<egui::DroppedFileHandle>, sender: Sender<WebEvent>, ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        let mut read = Vec::new();
        for file in files {
            let name = file.path().to_string_lossy().into_owned();
            if let Ok(bytes) = file.bytes_async().await {
                read.push((name, bytes));
            }
        }
        // A single project file opens, and anything else imports as audio.
        let event = match read.as_slice() {
            [(name, _)] if name.to_lowercase().ends_with(".kantan") => {
                let (name, bytes) = read.remove(0);
                WebEvent::Open(name, bytes)
            }
            [] => return,
            _ => WebEvent::Import(read),
        };
        let _ = sender.send(event);
        ctx.request_repaint();
    });
}

/// Hands a file to the browser as a download.
pub fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let error = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(error)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(error)?;
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no page to download from")?;
    let link: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(error)?
        .dyn_into()
        .map_err(|_| "couldn't create a download link".to_owned())?;
    link.set_href(&url);
    link.set_download(name);
    link.click();
    // The browser has started the download by now.
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn stored(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

pub fn store(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}
