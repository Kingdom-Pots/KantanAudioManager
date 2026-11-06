//! The application window: toolbar, timeline, status bar and hotkeys.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, Key, Modifiers};

use crate::import::{self, ImportError, SUPPORTED_EXTENSIONS};
use crate::timeline::{Selection, Timeline, format_time};
use crate::{Document, Project, Source};

const PROJECT_RATE: u32 = 48000;
const ZOOM_STEP: f64 = 1.5;

/// Result of importing one file, in import order.
struct Imported {
    name: String,
    result: Result<Source, ImportError>,
}

pub struct App {
    document: Document,
    selection: Selection,
    timeline: Timeline,
    importing: Option<Receiver<Vec<Imported>>>,
    error: Option<String>,
    native_dialogs: bool,
    blocked_dialogs: u32,
}

impl App {
    pub fn new(ctx: &egui::Context, startup_files: Vec<PathBuf>) -> Self {
        ctx.set_theme(egui::Theme::Dark);
        // Ctrl with + and - is free for the app, and the wheel zooms the track.
        ctx.options_mut(|options| options.zoom_with_keyboard = false);

        let mut app = Self {
            document: Document::new(Project::new(PROJECT_RATE)),
            selection: Selection::default(),
            timeline: Timeline::default(),
            importing: None,
            error: None,
            native_dialogs: true,
            blocked_dialogs: 0,
        };
        if !startup_files.is_empty() {
            app.start_import(ctx, startup_files);
        }
        app
    }

    /// An app driven without a screen. Actions that would open a native dialog
    /// are counted instead, because a dialog would block with nobody to answer.
    pub fn headless(ctx: &egui::Context) -> Self {
        let mut app = Self::new(ctx, Vec::new());
        app.native_dialogs = false;
        app
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_importing(&self) -> bool {
        self.importing.is_some()
    }

    pub fn blocked_dialogs(&self) -> u32 {
        self.blocked_dialogs
    }

    fn pick_files(&mut self, ctx: &egui::Context) {
        if !self.native_dialogs {
            self.blocked_dialogs += 1;
            return;
        }
        let picked = rfd::FileDialog::new()
            .add_filter("Audio", &SUPPORTED_EXTENSIONS)
            .pick_files();
        if let Some(paths) = picked {
            self.start_import(ctx, paths);
        }
    }

    /// Decodes on a worker thread so a large batch doesn't freeze the window.
    pub fn start_import(&mut self, ctx: &egui::Context, mut paths: Vec<PathBuf>) {
        if self.importing.is_some() {
            self.error = Some("Wait for the current import to finish.".to_owned());
            return;
        }
        self.error = None;
        paths.sort_by(|a, b| import::natural_cmp(&file_name(a), &file_name(b)));

        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let rate = self.document.project().sample_rate();
        std::thread::spawn(move || {
            let imported = paths
                .iter()
                .map(|path| Imported {
                    name: file_stem(path),
                    result: import::import_file(path, rate),
                })
                .collect();
            // Fails only if the window has closed, when nobody needs the result.
            let _ = sender.send(imported);
            ctx.request_repaint();
        });
        self.importing = Some(receiver);
    }

    fn finish_import(&mut self) {
        let Some(receiver) = &self.importing else { return };
        let Ok(imported) = receiver.try_recv() else { return };
        self.importing = None;

        let mut failures = Vec::new();
        let mut sources = Vec::new();
        for Imported { name, result } in imported {
            match result {
                Ok(source) => sources.push((name, source)),
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        if !failures.is_empty() {
            self.error = Some(format!("Couldn't import {}", failures.join("; ")));
        }
        if sources.is_empty() {
            return;
        }

        // One undo step, however many files were in the batch.
        let label = "Import";
        let result = self.document.edit(label, |project| {
            for (name, source) in sources {
                let source = project.insert_source(source);
                project.append_clip(&name, source)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => self.timeline.request_fit(),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect()
        });
        if !dropped.is_empty() {
            self.start_import(ctx, dropped);
        }
    }

    fn handle_hotkeys(&mut self, ctx: &egui::Context) {
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        // Shift variants go first: a plain Ctrl shortcut also matches with shift held.
        if ctx.input_mut(|i| i.consume_key(ctrl_shift, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y)) {
            self.document.redo();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            self.document.undo();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.pick_files(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            self.selection.select_all(self.document.project());
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.selection.clear();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let ctx = ui.ctx().clone();
            if ui.button("Import").on_hover_text("Import audio files (Ctrl+O)").clicked() {
                self.pick_files(&ctx);
            }
            ui.separator();

            let undo = self.document.undo_label().map(|l| format!("Undo {l} (Ctrl+Z)"));
            let button = ui.add_enabled(undo.is_some(), egui::Button::new("Undo"));
            if button.on_hover_text(undo.unwrap_or_default()).clicked() {
                self.document.undo();
            }
            let redo = self.document.redo_label().map(|l| format!("Redo {l} (Ctrl+Y)"));
            let button = ui.add_enabled(redo.is_some(), egui::Button::new("Redo"));
            if button.on_hover_text(redo.unwrap_or_default()).clicked() {
                self.document.redo();
            }
            ui.separator();

            let hint = "Zoom (Ctrl+Scroll)";
            if ui.button("-").on_hover_text(hint).clicked() {
                self.timeline.zoom_centered(1.0 / ZOOM_STEP);
            }
            if ui.button("+").on_hover_text(hint).clicked() {
                self.timeline.zoom_centered(ZOOM_STEP);
            }
            if ui.button("Fit").on_hover_text("Show the whole track").clicked() {
                self.timeline.request_fit();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.importing.is_some() {
                ui.spinner();
                ui.label("Importing...");
            } else if let Some(error) = &self.error {
                let dismissed = ui.small_button("x").on_hover_text("Dismiss").clicked();
                ui.colored_label(Color32::from_rgb(240, 120, 110), error);
                if dismissed {
                    self.error = None;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(self.selection_summary());
            });
        });
    }

    pub fn selection_summary(&self) -> String {
        let project = self.document.project();
        let rate = project.sample_rate();
        let clips = project.clips().len();
        let selected = self.selection.clips.len();

        if let Some(range) = self.selection.range {
            let start = format_time(range.start, rate);
            let end = format_time(range.end, rate);
            let length = format_time(range.end - range.start, rate);
            format!("Selection {start} - {end} (length {length})")
        } else if selected > 0 {
            format!("{selected} of {clips} selected")
        } else {
            let cursor = format_time(self.selection.cursor, rate);
            let noun = if clips == 1 { "clip" } else { "clips" };
            format!("Cursor {cursor}  |  {clips} {noun}")
        }
    }

    /// Shows a hint on an empty project, and while files are dragged over.
    fn paint_overlay(&self, ctx: &egui::Context, rect: egui::Rect, painter: &egui::Painter) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let text = if hovering {
            "Drop to import"
        } else if self.document.project().clips().is_empty() && self.importing.is_none() {
            "Drag audio files here, or press Ctrl+O"
        } else {
            return;
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(18.0),
            Color32::from_gray(150),
        );
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.finish_import();
        self.handle_dropped_files(&ctx);
        self.handle_hotkeys(&ctx);
        self.selection.retain_existing(self.document.project());

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                self.timeline.show(ui, self.document.project(), &mut self.selection);
                self.paint_overlay(&ctx, rect, ui.painter());
            });
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(|| "Clip".to_owned(), |n| n.to_string_lossy().into_owned())
}
