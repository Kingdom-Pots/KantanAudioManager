//! The application window: toolbar, timeline, status bar and hotkeys.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers};
use egui_phosphor::regular as icons;

use crate::about::AboutWindow;
use crate::export_dialog::ExportDialog;
use crate::import::{self, ImportError, SUPPORTED_EXTENSIONS};
use crate::playback::Player;
use crate::project_file;
use crate::session::{self, Orphans, Session};
use crate::peaks::clip_min_max;
use crate::recording::{self, Recorder};
use crate::timeline::{LivePreview, Selection, Timeline, TimelineAction, format_time};
use crate::{SilenceSettings, Frames, ClipId, Document, EditError, Project, Source, TimeRange, db_to_gain};

const PROJECT_RATE: u32 = 48000;
const ZOOM_STEP: f64 = 1.5;
const ICON_SIZE: f32 = 18.0;
const ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(30.0, 26.0);

enum Dialog {
    Rename { id: ClipId, text: String, needs_focus: bool },
    Amplify { ranges: Vec<TimeRange>, db: f32, peak: f32 },
}

/// How often unsaved work is written for crash recovery.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

/// What to do once unsaved changes have been dealt with.
enum AfterSave {
    Close,
    Open(PathBuf),
}

/// A question that has to be answered before carrying on.
enum Prompt {
    Unsaved(AfterSave),
    Recover(Orphans),
}

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
    export: ExportDialog,
    player: Player,
    dialog: Option<Dialog>,
    /// The document revision being played, to stop if the project changes.
    played_revision: u64,
    recorder: Recorder,
    inputs: Vec<recording::InputChoice>,
    /// None means the default microphone.
    input: Option<recording::InputChoice>,
    meter: f32,
    about: AboutWindow,
    auto_split: bool,
    silence: SilenceSettings,
    project_path: Option<PathBuf>,
    /// The document revision on disk. None means it has never been saved
    /// since being restored, so it counts as changed even with no edits.
    saved_revision: Option<u64>,
    prompt: Option<Prompt>,
    closing: bool,
    window_title: String,
    session: Option<Session>,
    last_autosave: Instant,
    autosaving: Option<Receiver<()>>,
    /// Recovered takes to delete once they've been imported.
    import_cleanup: Vec<PathBuf>,
}

impl App {
    pub fn new(ctx: &egui::Context, startup_files: Vec<PathBuf>) -> Self {
        let mut app = Self::base(ctx);
        app.begin_session();
        if !startup_files.is_empty() {
            app.start_import(ctx, startup_files);
        }
        app
    }

    fn base(ctx: &egui::Context) -> Self {
        ctx.set_theme(egui::Theme::Dark);
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        ctx.set_fonts(fonts);
        // Ctrl with + and - is free for the app, and the wheel zooms the track.
        ctx.options_mut(|options| options.zoom_with_keyboard = false);

        Self {
            document: Document::new(Project::new(PROJECT_RATE)),
            selection: Selection::default(),
            timeline: Timeline::default(),
            importing: None,
            error: None,
            native_dialogs: true,
            blocked_dialogs: 0,
            export: ExportDialog::default(),
            player: Player::new(true),
            dialog: None,
            recorder: Recorder::new(true),
            inputs: recording::input_choices(),
            input: None,
            meter: 0.0,
            played_revision: 0,
            about: AboutWindow::default(),
            auto_split: false,
            silence: SilenceSettings::default(),
            project_path: None,
            saved_revision: Some(0),
            prompt: None,
            closing: false,
            window_title: String::new(),
            session: None,
            last_autosave: Instant::now(),
            autosaving: None,
            import_cleanup: Vec::new(),
        }
    }

    /// Claims this run's files and looks for work left by a crash.
    fn begin_session(&mut self) {
        let Some(root) = session::data_dir() else { return };
        let orphans = session::find_orphans(&root);
        if orphans.is_empty() {
            orphans.discard();
        } else {
            self.prompt = Some(Prompt::Recover(orphans));
        }
        self.session = Session::start(&root);
    }

    /// Whether new audio gets split, and the settings for what counts as a pause.
    pub fn silence_state(&self) -> (bool, SilenceSettings) {
        (self.auto_split, self.silence)
    }

    pub fn set_silence(&mut self, settings: SilenceSettings) {
        self.silence = settings;
    }

    /// Everything stored in a project file besides the track.
    fn settings_pairs(&self) -> project_file::Settings {
        let mut pairs = self.export.settings.to_pairs();
        pairs.push(("auto_split".to_owned(), self.auto_split.to_string()));
        pairs.push(("silence_db".to_owned(), self.silence.threshold_db.to_string()));
        pairs.push(("silence_seconds".to_owned(), self.silence.min_silence_seconds.to_string()));
        pairs
    }

    fn apply_silence_pairs(&mut self, pairs: &project_file::Settings) {
        for (name, value) in pairs {
            match name.as_str() {
                "auto_split" => self.auto_split = value == "true",
                "silence_db" => {
                    let db: f32 = value.parse().unwrap_or(self.silence.threshold_db);
                    self.silence.threshold_db = db.clamp(-80.0, -10.0);
                }
                "silence_seconds" => {
                    let seconds: f32 = value.parse().unwrap_or(self.silence.min_silence_seconds);
                    self.silence.min_silence_seconds = seconds.clamp(0.05, 5.0);
                }
                _ => {}
            }
        }
    }

    fn silence_button(&mut self, ui: &mut egui::Ui) {
        let text = egui::RichText::new(icons::WAVEFORM).size(ICON_SIZE);
        let state = if self.auto_split { "on" } else { "off" };
        let hint = format!(
            "Split on silence is {state}. When on, new recordings and imports are cut into one clip per line at pauses. Right-click for settings."
        );
        let button = ui.add(egui::Button::new(text).min_size(ICON_BUTTON_SIZE).selected(self.auto_split));
        let button = named(button, "Split on silence").on_hover_text(hint);
        if button.clicked() {
            self.auto_split = !self.auto_split;
        }
        button.context_menu(|ui| {
            ui.label("What counts as a pause");
            ui.horizontal(|ui| {
                ui.label("Silence threshold");
                ui.add(
                    egui::DragValue::new(&mut self.silence.threshold_db)
                        .speed(0.5)
                        .range(-80.0..=-10.0)
                        .suffix(" dB"),
                )
                .on_hover_text("Anything quieter than this counts as silence. Raise it if background noise stops takes from splitting.");
            });
            ui.horizontal(|ui| {
                ui.label("Minimum pause");
                ui.add(
                    egui::DragValue::new(&mut self.silence.min_silence_seconds)
                        .speed(0.01)
                        .range(0.05..=5.0)
                        .suffix(" s"),
                )
                .on_hover_text("A quiet stretch must last this long to count as a pause. Raise it if lines get cut in two.");
            });
        });
    }

    /// Splits the selected clips at pauses.
    fn split_selected_on_silence(&mut self) {
        self.error = None;
        let ids = self.selection.clips.clone();
        let settings = self.silence;
        let before = self.document.project().clips().len();
        let result = self.document.edit("Split at silences", |project| {
            for &id in &ids {
                match project.split_on_silence(id, settings) {
                    Ok(()) | Err(EditError::NoChange) => {}
                    Err(error) => return Err(error),
                }
            }
            if project.clips().len() == before { Err(EditError::NoChange) } else { Ok(()) }
        });
        match result {
            Ok(()) => self.selection.clear(),
            Err(EditError::NoChange) => {
                self.error = Some("No pauses found with the current settings.".to_owned());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.document.revision())
            || self.recorder.is_recording()
            || self.recorder.is_finishing()
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    pub fn about_is_open(&self) -> bool {
        self.about.is_open()
    }

    pub fn is_closing(&self) -> bool {
        self.closing
    }

    pub fn window_title(&self) -> &str {
        &self.window_title
    }

    pub fn prompt_name(&self) -> Option<&str> {
        match &self.prompt {
            Some(Prompt::Unsaved(_)) => Some("unsaved"),
            Some(Prompt::Recover(_)) => Some("recover"),
            None => None,
        }
    }

    /// Writes the project, returning whether it worked.
    pub fn save_to(&mut self, path: PathBuf) -> bool {
        let settings = self.settings_pairs();
        match project_file::save(&path, self.document.project(), &settings) {
            Ok(()) => {
                self.project_path = Some(path);
                self.saved_revision = Some(self.document.revision());
                if let Some(session) = &self.session {
                    session.remove_autosave();
                }
                true
            }
            Err(error) => {
                self.error = Some(format!("Couldn't save: {error}"));
                false
            }
        }
    }

    fn save(&mut self) -> bool {
        match self.project_path.clone() {
            Some(path) => self.save_to(path),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        if !self.native_dialogs {
            self.blocked_dialogs += 1;
            return false;
        }
        let picked = rfd::FileDialog::new()
            .add_filter("Kantan project", &[project_file::EXTENSION])
            .set_file_name(format!("Untitled.{}", project_file::EXTENSION))
            .save_file();
        match picked {
            Some(path) => self.save_to(path),
            None => false,
        }
    }

    fn pick_project(&mut self) {
        if !self.native_dialogs {
            self.blocked_dialogs += 1;
            return;
        }
        let picked = rfd::FileDialog::new()
            .add_filter("Kantan project", &[project_file::EXTENSION])
            .pick_file();
        if let Some(path) = picked {
            self.request_open(path);
        }
    }

    /// Says so if a take needs finishing first, as it can't be saved until then.
    fn take_in_progress(&mut self, action: &str) -> bool {
        let busy = self.recorder.is_recording() || self.recorder.is_finishing();
        if busy {
            self.error = Some(format!("Stop recording before {action}."));
        }
        busy
    }

    /// Opens a project, first asking about any unsaved changes.
    pub fn request_open(&mut self, path: PathBuf) {
        // The take would otherwise land in the project being opened.
        if self.take_in_progress("opening a project") {
            return;
        }
        if self.is_dirty() {
            self.prompt = Some(Prompt::Unsaved(AfterSave::Open(path)));
        } else {
            self.open_project(path);
        }
    }

    /// Asks to close the app, first asking about any unsaved changes.
    pub fn request_close(&mut self) {
        if self.take_in_progress("closing") {
            return;
        }
        if self.is_dirty() {
            self.prompt = Some(Prompt::Unsaved(AfterSave::Close));
        } else {
            self.closing = true;
        }
    }

    fn open_project(&mut self, path: PathBuf) {
        match project_file::load(&path) {
            Ok((project, settings)) => {
                self.replace_document(project, &settings);
                self.project_path = Some(path);
            }
            Err(error) => self.error = Some(format!("Couldn't open: {error}")),
        }
    }

    fn replace_document(&mut self, project: Project, settings: &project_file::Settings) {
        self.player.stop();
        self.document = Document::new(project);
        self.saved_revision = Some(self.document.revision());
        self.selection = Selection::default();
        self.export.settings.apply_pairs(settings);
        self.apply_silence_pairs(settings);
        self.timeline.request_fit();
        self.error = None;
    }

    fn finish_prompt_action(&mut self, after: AfterSave) {
        match after {
            AfterSave::Close => self.closing = true,
            AfterSave::Open(path) => self.open_project(path),
        }
    }

    /// Restores a crashed session: its autosave, then any takes it was recording.
    fn recover(&mut self, ctx: &egui::Context, orphans: &Orphans) {
        let mut restored = false;
        if let Some(path) = &orphans.autosave {
            match project_file::load(path) {
                Ok((project, settings)) => {
                    self.replace_document(project, &settings);
                    restored = true;
                }
                Err(error) => self.error = Some(format!("Couldn't restore: {error}")),
            }
        }
        let mut takes = Vec::new();
        for (index, take) in orphans.takes.iter().enumerate() {
            // Import names a clip after its file.
            let named = take.with_file_name(format!("Recovered take {}.wav", index + 1));
            if std::fs::rename(take, &named).is_ok() {
                takes.push(named);
            }
        }
        orphans.discard_markers();
        if restored || !takes.is_empty() {
            // Nothing on disk matches this, so it needs saving.
            self.saved_revision = None;
        }
        if !takes.is_empty() {
            self.import_cleanup = takes.clone();
            self.start_import(ctx, takes);
        }
    }

    /// Writes unsaved work in the background every so often.
    fn update_autosave(&mut self) {
        if let Some(done) = &self.autosaving
            && done.try_recv().is_ok()
        {
            self.autosaving = None;
        }
        if self.autosaving.is_none()
            && self.last_autosave.elapsed() >= AUTOSAVE_INTERVAL
            && self.saved_revision != Some(self.document.revision())
        {
            self.autosave_now();
        }
    }

    /// Starts writing the recovery file right away.
    pub fn autosave_now(&mut self) -> Option<Receiver<()>> {
        let session = self.session.as_ref()?;
        let path = session.autosave_path().to_path_buf();
        let project = self.document.project().clone();
        let settings = self.settings_pairs();
        self.last_autosave = Instant::now();
        let (sender, receiver) = mpsc::channel();
        let (app_sender, app_receiver) = mpsc::channel();
        std::thread::spawn(move || {
            // A failure only means less to recover, and the next attempt may work.
            let _ = project_file::save(&path, &project, &settings);
            let _ = sender.send(());
            let _ = app_sender.send(());
        });
        self.autosaving = Some(app_receiver);
        Some(receiver)
    }

    fn show_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.prompt.take() else { return };
        match prompt {
            Prompt::Unsaved(after) => {
                let mut choice = None;
                egui::Modal::new(egui::Id::unique("unsaved_prompt")).show(ctx, |ui| {
                    ui.heading("Save your changes?");
                    ui.label("This project has changes that haven't been saved.");
                    ui.horizontal(|ui| {
                        if ui.button("Save changes").on_hover_text("Save the project, then carry on").clicked() {
                            choice = Some(0);
                        }
                        if ui.button("Don't save").on_hover_text("Carry on and lose the unsaved changes").clicked() {
                            choice = Some(1);
                        }
                        if ui.button("Cancel").on_hover_text("Go back to the project (Esc)").clicked() {
                            choice = Some(2);
                        }
                    });
                });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(2);
                }
                match choice {
                    Some(0) => {
                        self.recorder.stop();
                        if self.save() {
                            self.finish_prompt_action(after);
                        } else {
                            self.prompt = Some(Prompt::Unsaved(after));
                        }
                    }
                    Some(1) => {
                        self.recorder.stop();
                        self.finish_prompt_action(after);
                    }
                    Some(_) => {}
                    None => {
                        self.prompt = Some(Prompt::Unsaved(after));
                    }
                }
            }
            Prompt::Recover(orphans) => {
                let mut choice = None;
                egui::Modal::new(egui::Id::unique("recover_prompt")).show(ctx, |ui| {
                    ui.heading("Restore your work?");
                    ui.label("Kantan didn't close properly last time. Found:");
                    if orphans.autosave.is_some() {
                        ui.label("  - an autosaved project");
                    }
                    if !orphans.takes.is_empty() {
                        ui.label(format!("  - {} unfinished recording(s)", orphans.takes.len()));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Restore").on_hover_text("Bring this work back").clicked() {
                            choice = Some(true);
                        }
                        if ui.button("Discard").on_hover_text("Permanently delete this recovered work").clicked() {
                            choice = Some(false);
                        }
                    });
                });
                match choice {
                    Some(true) => {
                        self.recover(ctx, &orphans);
                    }
                    Some(false) => orphans.discard(),
                    None => {
                        self.prompt = Some(Prompt::Recover(orphans));
                    }
                }
            }
        }
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing && self.prompt.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close();
        }
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let name = self
            .project_path
            .as_deref()
            .and_then(Path::file_stem)
            .map_or_else(|| "Untitled".to_owned(), |n| n.to_string_lossy().into_owned());
        let mark = if self.is_dirty() { "*" } else { "" };
        let title = format!("{mark}{name} - Kantan Audio Manager");
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// An app driven without a screen. Actions that would open a native dialog
    /// are counted instead, because a dialog would block with nobody to answer.
    pub fn headless(ctx: &egui::Context) -> Self {
        let mut app = Self::base(ctx);
        // Only touch the data folder if a test has given it a private one.
        if std::env::var_os("KANTAN_DATA_DIR").is_some() {
            app.begin_session();
        }
        app.native_dialogs = false;
        app.player = Player::new(false);
        app.recorder = Recorder::new(false);
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

    pub fn export_dialog(&self) -> &ExportDialog {
        &self.export
    }

    pub fn export_dialog_mut(&mut self) -> &mut ExportDialog {
        &mut self.export
    }

    pub fn player(&self) -> &Player {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut Player {
        &mut self.player
    }

    pub fn recorder(&self) -> &Recorder {
        &self.recorder
    }

    /// One control for both: starts a take, or stops the one running.
    fn toggle_recording(&mut self) {
        self.error = None;
        if self.recorder.is_recording() {
            self.recorder.stop();
            return;
        }
        if self.recorder.is_finishing() {
            return;
        }
        self.player.stop();
        let rate = self.document.project().sample_rate();
        if let Err(error) = self.recorder.start(self.input.as_ref(), rate) {
            self.error = Some(format!("Couldn't record: {error}"));
            self.inputs = recording::input_choices();
        }
    }

    /// Adds finished takes, and keeps what was captured if the mic disappears.
    fn update_recording(&mut self, ctx: &egui::Context) {
        if let Some(reason) = self.recorder.device_lost() {
            self.recorder.stop();
            self.error = Some(format!("The recording input stopped working ({reason}). The take so far was kept."));
        }
        if self.recorder.is_recording() {
            self.meter = self.recorder.take_level().max(self.meter * 0.9);
            ctx.request_repaint();
        } else {
            self.meter = 0.0;
        }
        if self.recorder.is_finishing() {
            ctx.request_repaint();
        }
        match self.recorder.poll_finished() {
            Some(Ok(source)) => {
                let split = self.auto_split.then_some(self.silence);
                let result = self.document.edit("Record", |project| {
                    let name = project.next_recording_name();
                    let source = project.insert_source(source);
                    let id = project.append_clip(&name, source)?;
                    split_if_on(project, id, split)
                });
                self.report(result);
                self.timeline.request_fit();
            }
            Some(Err(error)) => self.error = Some(format!("Couldn't keep the take: {error}")),
            None => {}
        }
    }

    fn recording_controls(&mut self, ui: &mut egui::Ui) {
        let recording = self.recorder.is_recording();
        let red = Color32::from_rgb(235, 80, 70);
        let (icon, label, hint, color) = if recording {
            (icons::STOP, "Stop", "Stop recording and add the take as a clip (R)", Color32::WHITE)
        } else {
            (icons::RECORD, "Record", "Record a new clip from the selected input (R)", red)
        };
        let text = egui::RichText::new(icon).size(ICON_SIZE).color(color);
        let mut button = egui::Button::new(text).min_size(ICON_BUTTON_SIZE);
        if recording {
            button = button.fill(Color32::from_rgb(190, 50, 45));
        }
        if named(ui.add(button), label).on_hover_text(hint).clicked() {
            self.toggle_recording();
        }
        let current = self.input.as_ref().map_or_else(|| "Default mic".to_owned(), |i| i.label());
        // Device names can be very long, and mustn't push the other buttons along.
        let shown = if current.chars().count() > 24 {
            current.chars().take(23).chain(['\u{2026}']).collect()
        } else {
            current.clone()
        };
        ui.add_enabled_ui(!recording, |ui| {
            ui.label(egui::RichText::new(icons::MICROPHONE).size(ICON_SIZE))
                .on_hover_text("The recording input");
            egui::ComboBox::from_id_salt("recording_input")
                .selected_text(shown)
                .width(150.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.input, None, "Default mic")
                        .on_hover_text("Your Windows default microphone");
                    for input in &self.inputs {
                        let hint = if input.system_audio {
                            "Record what this device is playing"
                        } else {
                            "Record from this microphone"
                        };
                        ui.selectable_value(&mut self.input, Some(input.clone()), input.label())
                            .on_hover_text(hint);
                    }
                })
                .response
                .on_hover_text(format!("Recording from: {current}. Choose a microphone, or system audio."));
        });
        if recording {
            let (rect, meter) = ui.allocate_exact_size(egui::vec2(80.0, 12.0), egui::Sense::hover());
            meter.on_hover_text("Input level. Red means it's too loud and clipping.");
            ui.painter().rect_filled(rect, 2.0, Color32::from_gray(40));
            let mut bar = rect;
            bar.set_width(rect.width() * self.meter.clamp(0.0, 1.0));
            let color = if self.meter >= 0.99 {
                Color32::from_rgb(235, 90, 80)
            } else {
                Color32::from_rgb(110, 225, 130)
            };
            ui.painter().rect_filled(bar, 2.0, color);
        }
    }

    /// Pausing leaves the cursor where it stopped, so Space resumes from there.
    /// A range always plays from its start.
    fn toggle_playback(&mut self) {
        self.error = None;
        if self.recorder.is_recording() {
            return;
        }
        if self.player.is_playing() {
            let reached = self.player.stop();
            if let (Some(reached), None) = (reached, self.selection.range) {
                self.selection.cursor = reached;
            }
            return;
        }
        let project = self.document.project();
        let Some(track_end) = project.clips().last().map(|clip| clip.end()) else {
            self.error = Some("There is nothing to play.".to_owned());
            return;
        };
        // Selected clips play as a region, from the cursor if it's inside them.
        let cursor = self.selection.cursor;
        let selected = self.selection.clips.iter().filter_map(|&id| project.clip(id));
        let span = selected.fold(None, |span: Option<(Frames, Frames)>, clip| {
            Some(span.map_or((clip.start, clip.end()), |(a, b)| (a.min(clip.start), b.max(clip.end()))))
        });
        let (from, to) = match self.selection.range {
            Some(range) => (range.start, range.end),
            None if span.is_some() => {
                let (start, end) = span.unwrap_or((0, track_end));
                (if cursor >= start && cursor < end { cursor } else { start }, end)
            }
            // Starting past the end would be silent, so go back to the start.
            None if self.selection.cursor >= track_end => (0, track_end),
            None => (self.selection.cursor, track_end),
        };
        match self.player.start(project, from, to) {
            Ok(()) => self.played_revision = self.document.revision(),
            Err(error) => self.error = Some(format!("Couldn't play: {error}")),
        }
    }

    /// Ends playback once it finishes, or if the audio no longer matches the project.
    fn update_playback(&mut self, ctx: &egui::Context) {
        if !self.player.is_playing() {
            return;
        }
        if self.player.has_finished() || self.played_revision != self.document.revision() {
            self.player.stop();
        } else {
            ctx.request_repaint();
        }
    }

    pub fn set_amplify_db(&mut self, value: f32) -> bool {
        if let Some(Dialog::Amplify { db, .. }) = &mut self.dialog {
            *db = value;
            return true;
        }
        false
    }

    /// The open edit dialog's name and, for amplify, the gain and current peak.
    pub fn dialog_state(&self) -> (Option<&str>, f32, f32) {
        match &self.dialog {
            Some(Dialog::Rename { .. }) => (Some("rename"), 0.0, 0.0),
            Some(Dialog::Amplify { db, peak, .. }) => (Some("amplify"), *db, *peak),
            None => (None, 0.0, 0.0),
        }
    }

    fn undo(&mut self) {
        self.step_history(Document::undo);
    }

    fn redo(&mut self) {
        self.step_history(Document::redo);
    }

    fn step_history(&mut self, step: fn(&mut Document) -> Option<&'static str>) {
        self.error = None;
        // A drag in progress may be holding a clip this removes.
        self.timeline.cancel_drag();
        let was_empty = self.document.project().clips().is_empty();
        if step(&mut self.document).is_some() && was_empty {
            // Coming back from an empty track, show everything that returned.
            self.timeline.request_fit();
        }
    }

    /// Shows an edit's error, except that an edit changing nothing is no error.
    fn report(&mut self, result: Result<(), EditError>) {
        match result {
            Ok(()) | Err(EditError::NoChange) => {}
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Splits at both ends of a range if there is one, otherwise at `at`.
    fn split(&mut self, at: Frames) {
        self.error = None;
        let points = match self.selection.range {
            Some(range) => vec![range.start, range.end],
            None => vec![at],
        };
        let result = self.document.edit("Split", |project| {
            let mut any = None;
            for point in points {
                match project.split_at(point) {
                    Ok(_) => any = Some(()),
                    Err(EditError::NotInsideClip(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            any.ok_or(EditError::NotInsideClip(at))
        });
        if result.is_err() {
            self.error = Some("Put the cursor inside a clip to split it.".to_owned());
        }
    }

    fn merge(&mut self) {
        self.error = None;
        let ids = self.selection.clips.clone();
        match self.document.edit("Merge", |project| project.merge_clips(&ids)) {
            Ok(id) => self.selection.clips = vec![id],
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn delete(&mut self) {
        self.error = None;
        let ids = self.selection.clips.clone();
        let result = if let Some(range) = self.selection.range {
            let result = self.document.edit("Delete", |project| project.delete_range(range));
            if result.is_ok() {
                self.selection.range = None;
            }
            result
        } else {
            self.document.edit("Delete", |project| project.delete_clips(&ids))
        };
        self.report(result);
    }

    fn open_rename(&mut self, id: ClipId) {
        self.error = None;
        if let Some(clip) = self.document.project().clip(id) {
            self.dialog = Some(Dialog::Rename { id, text: clip.name.clone(), needs_focus: true });
        }
    }

    /// Amplifies the selected range, or else the selected clips.
    fn open_amplify(&mut self) {
        self.error = None;
        let project = self.document.project();
        let ranges: Vec<TimeRange> = match self.selection.range {
            Some(range) => vec![range],
            None => self
                .selection
                .clips
                .iter()
                .filter_map(|&id| project.clip(id))
                .map(|clip| TimeRange::new(clip.start, clip.end()))
                .collect(),
        };
        if ranges.is_empty() {
            self.error = Some("Select some audio to amplify.".to_owned());
            return;
        }
        let peak = peak_of(project, &ranges);
        self.dialog = Some(Dialog::Amplify { ranges, db: 0.0, peak });
    }

    fn run_action(&mut self, action: TimelineAction) {
        match action {
            TimelineAction::Move(id, start, snap) => {
                let result = self.document.edit("Move", |project| project.move_clip(id, start, snap));
                self.report(result);
            }
            TimelineAction::Rename(id) => self.open_rename(id),
            TimelineAction::Split(at) => self.split(at),
            TimelineAction::Merge => self.merge(),
            TimelineAction::Amplify => self.open_amplify(),
            TimelineAction::Delete => self.delete(),
            TimelineAction::Export => self.open_export(),
            TimelineAction::SplitOnSilence => self.split_selected_on_silence(),
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut close = false;
        let mut apply = false;
        let response = egui::Modal::new(egui::Id::unique("edit_dialog")).show(ctx, |ui| {
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            match &mut dialog {
                Dialog::Rename { text, needs_focus, .. } => {
                    ui.heading("Rename clip");
                    let edit = egui::TextEdit::singleline(text).desired_width(320.0).show(ui);
                    if std::mem::take(needs_focus) {
                        edit.response.request_focus();
                        let mut state = edit.state.clone();
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                        state.store(ui.ctx(), edit.response.id);
                    }
                }
                Dialog::Amplify { db, peak, .. } => {
                    ui.heading("Amplify");
                    ui.horizontal(|ui| {
                        ui.label("Gain");
                        ui.add(egui::DragValue::new(db).speed(0.1).range(-60.0..=60.0).suffix(" dB"))
                            .on_hover_text("How much louder (positive) or quieter (negative) to make it");
                        let can_max = *peak > 0.0;
                        let max = ui
                            .add_enabled(can_max, egui::Button::new("Max without clipping"))
                            .on_hover_text("Set the gain that makes the loudest point exactly full scale");
                        if max.clicked() {
                            *db = -20.0 * peak.log10();
                        }
                    });
                    let result = *peak * db_to_gain(*db);
                    ui.label(format!("Peak {} now, {} after", level_text(*peak), level_text(result)));
                    if result > 1.0001 {
                        ui.colored_label(Color32::from_rgb(240, 120, 110), "This will clip.");
                    }
                }
            }
            ui.horizontal(|ui| {
                apply = ui.button("Apply").on_hover_text("Make this change (Enter)").clicked() || enter;
                close = ui.button("Cancel").on_hover_text("Close without changing anything (Esc)").clicked();
            });
        });
        if apply {
            let result = match &dialog {
                Dialog::Rename { id, text, .. } => {
                    self.document.edit("Rename", |project| project.rename_clip(*id, text))
                }
                Dialog::Amplify { ranges, db, .. } => {
                    if *db == 0.0 {
                        Ok(())
                    } else {
                        self.document.edit("Amplify", |project| {
                            ranges.iter().try_for_each(|&range| project.amplify(range, *db))
                        })
                    }
                }
            };
            self.report(result);
            close = true;
        }
        if !close && !response.should_close() {
            self.dialog = Some(dialog);
        }
    }

    fn open_export(&mut self) {
        if self.document.project().clips().is_empty() {
            self.error = Some("There are no clips to export.".to_owned());
            return;
        }
        self.export.open(!self.selection.clips.is_empty());
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
        // A project file opens, rather than being imported as audio.
        if let [path] = paths.as_slice()
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(project_file::EXTENSION))
        {
            let path = path.clone();
            self.request_open(path);
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
        if failures.is_empty() {
            for path in self.import_cleanup.drain(..) {
                let _ = std::fs::remove_file(path);
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
        let split = self.auto_split.then_some(self.silence);
        let result = self.document.edit(label, |project| {
            for (name, source) in sources {
                let source = project.insert_source(source);
                let id = project.append_clip(&name, source)?;
                split_if_on(project, id, split)?;
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
        if ctx.input_mut(|i| i.consume_key(ctrl_shift, Key::S)) {
            self.save_as();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.save();
        }
        if ctx.input_mut(|i| i.consume_key(ctrl_shift, Key::O)) {
            self.pick_project();
        }
        // Shift variants go first: a plain Ctrl shortcut also matches with shift held.
        if ctx.input_mut(|i| i.consume_key(ctrl_shift, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y)) {
            self.redo();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            self.undo();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::R)) {
            self.toggle_recording();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::I)) {
            self.split(self.selection.cursor);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::J)) {
            self.merge();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::L)) {
            self.open_amplify();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
            self.delete();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
            match self.selection.clips.as_slice() {
                [id] => self.open_rename(*id),
                _ => self.error = Some("Select one clip to rename.".to_owned()),
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
            self.toggle_playback();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::E)) {
            self.open_export();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.pick_files(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            self.selection.select_all(self.document.project());
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            && !self.timeline.cancel_drag()
        {
            self.selection.clear();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let ctx = ui.ctx().clone();
            if icon_button(ui, icons::FOLDER_OPEN, "Open", "Open a project (Ctrl+Shift+O)").clicked() {
                self.pick_project();
            }
            if icon_button(ui, icons::FLOPPY_DISK, "Save", "Save the project (Ctrl+S, or Ctrl+Shift+S to save a copy)").clicked() {
                self.save();
            }
            ui.separator();

            if icon_button(ui, icons::FILE_AUDIO, "Import", "Import audio files as clips (Ctrl+O)").clicked() {
                self.pick_files(&ctx);
            }
            if icon_button(ui, icons::EXPORT, "Export", "Export clips to audio files (Ctrl+E)").clicked() {
                self.open_export();
            }
            ui.separator();

            self.recording_controls(ui);
            self.silence_button(ui);
            ui.separator();

            let playing = self.player.is_playing();
            let (icon, label) = if playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
            let hint = if playing { "Pause (Space)" } else { "Play from the cursor, or play the selected range (Space)" };
            if icon_button(ui, icon, label, hint).clicked() {
                self.toggle_playback();
            }
            ui.separator();

            let undo = self.document.undo_label().map(|l| format!("Undo: {l} (Ctrl+Z)"));
            let enabled = undo.is_some();
            let hint = undo.unwrap_or_else(|| "Nothing to undo".to_owned());
            if ui.add_enabled_ui(enabled, |ui| icon_button(ui, icons::ARROW_U_UP_LEFT, "Undo", &hint)).inner.clicked() {
                self.undo();
            }
            let redo = self.document.redo_label().map(|l| format!("Redo: {l} (Ctrl+Y)"));
            let enabled = redo.is_some();
            let hint = redo.unwrap_or_else(|| "Nothing to redo".to_owned());
            if ui.add_enabled_ui(enabled, |ui| icon_button(ui, icons::ARROW_U_UP_RIGHT, "Redo", &hint)).inner.clicked() {
                self.redo();
            }
            ui.separator();

            if icon_button(ui, icons::MAGNIFYING_GLASS_MINUS, "-", "Zoom out (Ctrl+Scroll)").clicked() {
                self.timeline.zoom_centered(1.0 / ZOOM_STEP);
            }
            if icon_button(ui, icons::MAGNIFYING_GLASS_PLUS, "+", "Zoom in (Ctrl+Scroll)").clicked() {
                self.timeline.zoom_centered(ZOOM_STEP);
            }
            if icon_button(ui, icons::ARROWS_OUT_LINE_HORIZONTAL, "Fit", "Show the whole track").clicked() {
                self.timeline.request_fit();
            }
            ui.separator();

            if icon_button(ui, icons::INFO, "About", "About this software, credits and licences").clicked() {
                self.about.open();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.recorder.is_recording() {
                let time = format_time(
                    (self.recorder.seconds() * 1000.0) as u64,
                    1000,
                );
                ui.colored_label(Color32::from_rgb(240, 120, 110), format!("Recording {time}"));
            } else if self.importing.is_some() {
                ui.spinner();
                ui.label("Importing...");
            } else if let Some(error) = &self.error {
                let dismiss = ui.small_button(icons::X);
                let dismissed = named(dismiss, "Dismiss").on_hover_text("Dismiss this message").clicked();
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
        // The export window is modal, and its text boxes need keys like Ctrl+A.
        self.update_window(&ctx);
        self.update_autosave();
        // An open menu gets the keys first, so Esc closes it and Del can't act behind it.
        let menu_open = egui::Popup::is_any_open(&ctx);
        if !self.export.open
            && self.dialog.is_none()
            && self.prompt.is_none()
            && !self.about.is_open()
            && !menu_open
        {
            self.handle_dropped_files(&ctx);
            self.handle_hotkeys(&ctx);
        }
        self.update_playback(&ctx);
        self.update_recording(&ctx);
        self.selection.retain_existing(self.document.project());

        let mut action = None;
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let playhead = self.player.position();
                let project = self.document.project();
                let (timeline, selection) = (&mut self.timeline, &mut self.selection);
                let seconds = self.recorder.seconds();
                let shown = self.recorder.with_live_levels(|levels| {
                    let rate = project.sample_rate() as f64;
                    let name = project.next_recording_name();
                    let live = LivePreview {
                        name: &name,
                        start: project.next_take_start(),
                        len: (seconds * rate) as Frames,
                        levels,
                        frames_per_level: rate / 100.0,
                    };
                    timeline.show(ui, project, selection, playhead, Some(live))
                });
                action = match shown {
                    Some(action) => action,
                    None => timeline.show(ui, project, selection, playhead, None),
                };
                self.paint_overlay(&ctx, rect, ui.painter());
            });
        if let Some(action) = action {
            self.run_action(action);
        }
        self.show_dialog(&ctx);
        self.show_prompt(&ctx);
        self.about.show(&ctx);
        self.export.show(
            &ctx,
            self.document.project(),
            &self.selection.clips,
            self.native_dialogs,
            &mut self.blocked_dialogs,
        );
    }
}

/// Gives an icon-only widget a name for screen readers and test tools.
fn named(response: egui::Response, name: &str) -> egui::Response {
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    response
}

/// A toolbar button showing only an icon, with a tooltip even while disabled.
fn icon_button(ui: &mut egui::Ui, icon: &str, name: &str, hint: &str) -> egui::Response {
    let text = egui::RichText::new(icon).size(ICON_SIZE);
    let response = ui.add(egui::Button::new(text).min_size(ICON_BUTTON_SIZE));
    named(response, name).on_hover_text(hint).on_disabled_hover_text(hint)
}

/// Splits newly added audio if that's switched on. Audio with no pauses just stays whole.
fn split_if_on(
    project: &mut Project,
    id: ClipId,
    settings: Option<SilenceSettings>,
) -> Result<(), EditError> {
    match settings.map(|settings| project.split_on_silence(id, settings)) {
        None | Some(Ok(())) | Some(Err(EditError::NoChange)) => Ok(()),
        Some(Err(error)) => Err(error),
    }
}

/// The loudest level in some ranges of the track, after gain.
fn peak_of(project: &Project, ranges: &[TimeRange]) -> f32 {
    let mut peak = 0.0f32;
    for range in ranges {
        for clip in project.clips() {
            let start = range.start.max(clip.start);
            let end = range.end.min(clip.end());
            if start < end {
                let (lo, hi) = clip_min_max(project, clip, start - clip.start, end - start);
                peak = peak.max(lo.abs()).max(hi.abs());
            }
        }
    }
    peak
}

fn level_text(level: f32) -> String {
    if level <= 0.0 {
        "silence".to_owned()
    } else {
        format!("{:.1} dB", 20.0 * level.log10())
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(|| "Clip".to_owned(), |n| n.to_string_lossy().into_owned())
}
