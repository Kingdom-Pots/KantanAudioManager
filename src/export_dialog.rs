//! The export window: options, a live filename preview, and the running job.
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};

#[cfg(not(target_arch = "wasm32"))]
use crate::export::export_clips_with_policy;
use crate::export::{
    BitDepth, ExportReport, FileProblem, NameOptions, NumberPosition, Numbering, OutputFormat,
    PlannedFile,
    ExistingFiles, plan_files,
};
use crate::project_file::Settings;
use crate::{ClipId, Project};

const SAMPLE_RATES: [u32; 5] = [48000, 44100, 32000, 22050, 16000];
/// A compact preview with room for a small batch.
const PREVIEW_HEIGHT: f32 = 96.0;
const PROBLEM: Color32 = Color32::from_rgb(240, 120, 110);
const WARNING: Color32 = Color32::from_rgb(235, 190, 90);

enum JobMessage {
    Progress(usize),
    Finished(ExportReport),
}

struct Job {
    receiver: Receiver<JobMessage>,
    total: usize,
    done: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Wav,
    Mp3,
    Ogg,
}

impl FileFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Wav => "WAV",
            Self::Mp3 => "MP3",
            Self::Ogg => "OGG",
        }
    }
}

/// The settings, kept between exports so a batch can be repeated easily.
pub struct ExportSettings {
    pub folder: Option<PathBuf>,
    pub names: NameOptions,
    /// Used whenever numbering is switched on, so it isn't lost when off.
    pub numbering_enabled: bool,
    pub number_position: NumberPosition,
    pub number_start: u32,
    pub number_padding: usize,
    pub sample_rate: u32,
    pub depth: BitDepth,
    pub format: FileFormat,
    pub only_selected: bool,
}

impl ExportSettings {
    /// The settings worth keeping in a project file. Overwriting isn't one of
    /// them, because it should always be a deliberate choice.
    pub fn to_pairs(&self) -> Settings {
        let mut pairs = vec![
            ("prefix", self.names.prefix.clone()),
            ("suffix", self.names.suffix.clone()),
            ("numbering", self.numbering_enabled.to_string()),
            ("number_after", (self.number_position == NumberPosition::AfterName).to_string()),
            ("number_start", self.number_start.to_string()),
            ("number_padding", self.number_padding.to_string()),
            ("sample_rate", self.sample_rate.to_string()),
            ("bits24", (self.depth == BitDepth::Bits24).to_string()),
            ("format", self.format.label().to_owned()),
        ];
        if let Some(folder) = &self.folder {
            pairs.push(("folder", folder.display().to_string()));
        }
        pairs.into_iter().map(|(name, value)| (name.to_owned(), value)).collect()
    }

    /// Applies saved settings, ignoring anything unrecognized or out of range.
    pub fn apply_pairs(&mut self, pairs: &Settings) {
        for (name, value) in pairs {
            match name.as_str() {
                "prefix" => self.names.prefix = value.clone(),
                "suffix" => self.names.suffix = value.clone(),
                "folder" => self.folder = Some(PathBuf::from(value)),
                "numbering" => self.numbering_enabled = value == "true",
                "number_after" => {
                    self.number_position = if value == "true" {
                        NumberPosition::AfterName
                    } else {
                        NumberPosition::BeforeName
                    };
                }
                "number_start" => self.number_start = value.parse().unwrap_or(self.number_start),
                "number_padding" => {
                    let padding = value.parse().unwrap_or(self.number_padding);
                    self.number_padding = padding.clamp(1, 6);
                }
                "sample_rate" => {
                    let rate = value.parse().unwrap_or(self.sample_rate);
                    if SAMPLE_RATES.contains(&rate) {
                        self.sample_rate = rate;
                    }
                }
                "format" => {
                    self.format = match value.as_str() {
                        "MP3" => FileFormat::Mp3,
                        "OGG" => FileFormat::Ogg,
                        _ => FileFormat::Wav,
                    };
                }
                "bits24" => {
                    self.depth = if value == "true" { BitDepth::Bits24 } else { BitDepth::Bits16 };
                }
                _ => {}
            }
        }
    }

    pub fn output_format(&self) -> OutputFormat {
        match self.format {
            FileFormat::Wav => OutputFormat::Wav(self.depth),
            FileFormat::Mp3 => OutputFormat::Mp3,
            FileFormat::Ogg => OutputFormat::Ogg,
        }
    }

    fn name_options(&self) -> NameOptions {
        NameOptions {
            extension: self.output_format().extension(),
            numbering: self.numbering_enabled.then_some(Numbering {
                position: self.number_position,
                start: self.number_start,
                padding: self.number_padding,
            }),
            ..self.names.clone()
        }
    }
}

struct PendingExport {
    project: Project,
    folder: PathBuf,
    files: Vec<(ClipId, PathBuf)>,
    rate: u32,
    format: OutputFormat,
    conflicts: Vec<String>,
}

pub struct ExportDialog {
    pub open: bool,
    pub settings: ExportSettings,
    job: Option<Job>,
    report: Option<ExportReport>,
    pending: Option<PendingExport>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            settings: ExportSettings {
                folder: None,
                names: NameOptions { prefix: String::new(), suffix: String::new(), numbering: None, extension: "wav" },
                numbering_enabled: false,
                number_position: NumberPosition::BeforeName,
                number_start: 1,
                number_padding: 3,
                sample_rate: 48000,
                depth: BitDepth::Bits16,
                format: FileFormat::Wav,
                only_selected: false,
            },
            job: None,
            report: None,
            pending: None,
        }
    }
}

impl ExportDialog {
    /// Opens on the selected clips if there are any, otherwise on all of them.
    pub fn open(&mut self, any_selected: bool) {
        self.open = true;
        self.report = None;
        self.pending = None;
        self.settings.only_selected = any_selected;
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn report(&self) -> Option<&ExportReport> {
        self.report.as_ref()
    }

    /// The clips this export covers, in timeline order.
    fn clips<'a>(&self, project: &'a Project, selected: &[ClipId]) -> Vec<&'a crate::Clip> {
        project
            .clips()
            .iter()
            .filter(|clip| !self.settings.only_selected || selected.contains(&clip.id))
            .collect()
    }

    /// The files this export would write, given what is in the folder now.
    pub fn plan(&self, project: &Project, selected: &[ClipId]) -> Vec<PlannedFile> {
        let names: Vec<&str> = self
            .clips(project, selected)
            .iter()
            .map(|clip| clip.name.as_str())
            .collect();
        let folder = self.settings.folder.as_ref();
        plan_files(&names, &self.settings.name_options(), |file| {
            folder.is_some_and(|folder| folder.join(file).exists())
        })
    }

    fn can_start(&self, plan: &[PlannedFile]) -> bool {
        self.job.is_none()
            && self.pending.is_none()
            && (self.settings.folder.is_some() || cfg!(target_arch = "wasm32"))
            && !plan.is_empty()
            && !plan.iter().any(|file| file.problem == Some(FileProblem::Duplicate))
    }

    pub fn awaiting_conflict(&self) -> bool {
        self.pending.is_some()
    }

    /// The browser version packs every file into one ZIP and downloads it.
    #[cfg(target_arch = "wasm32")]
    fn request_export(&mut self, _ctx: &egui::Context, project: &Project, selected: &[ClipId], plan: &[PlannedFile]) {
        use std::io::Write;
        let format = self.settings.output_format();
        let mut report = ExportReport::default();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (clip, file) in self.clips(project, selected).iter().zip(plan) {
            match crate::export::encode_clip(project, clip.id, self.settings.sample_rate, format) {
                Ok((bytes, clipped)) => {
                    let added = archive
                        .start_file(file.file_name.as_str(), options)
                        .map_err(|e| e.to_string())
                        .and_then(|()| archive.write_all(&bytes).map_err(|e| e.to_string()));
                    match added {
                        Ok(()) => {
                            report.written += 1;
                            if clipped {
                                report.clipped.push(file.file_name.clone());
                            }
                        }
                        Err(error) => report.failed.push(format!("{}: {error}", file.file_name)),
                    }
                }
                Err(error) => report.failed.push(format!("{}: {error}", file.file_name)),
            }
        }
        let downloaded = archive
            .finish()
            .map_err(|e| e.to_string())
            .and_then(|zip| crate::web::download("Kantan export.zip", &zip.into_inner()));
        if let Err(error) = downloaded {
            report.failed.push(format!("the download: {error}"));
            report.written = 0;
        }
        self.report = Some(report);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_export(&mut self, ctx: &egui::Context, project: &Project, selected: &[ClipId], plan: &[PlannedFile]) {
        let Some(folder) = self.settings.folder.clone() else { return };
        let files = self.clips(project, selected).iter().zip(plan)
            .map(|(clip, file)| (clip.id, folder.join(&file.file_name))).collect();
        let pending = PendingExport {
            project: project.clone(),
            folder,
            files,
            rate: self.settings.sample_rate,
            format: self.settings.output_format(),
            conflicts: plan.iter().filter(|file| file.problem == Some(FileProblem::Exists))
                .map(|file| file.file_name.clone()).collect(),
        };
        self.report = None;
        if pending.conflicts.is_empty() {
            self.start(ctx, pending, ExistingFiles::Refuse);
        } else {
            self.pending = Some(pending);
            ctx.request_repaint();
        }
    }

    /// Runs the confirmed snapshot on a worker.
    #[cfg(not(target_arch = "wasm32"))]
    fn start(&mut self, ctx: &egui::Context, pending: PendingExport, policy: ExistingFiles) {
        let PendingExport { project, folder, files, rate, format, .. } = pending;
        let total = files.len();
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.report = None;
        std::thread::spawn(move || {
            let report = match std::fs::create_dir_all(&folder) {
                Ok(()) => export_clips_with_policy(&project, &files, rate, format, policy, |done| {
                    let _ = sender.send(JobMessage::Progress(done));
                    ctx.request_repaint();
                }),
                Err(error) => ExportReport {
                    failed: vec![format!("{}: {error}", folder.display())],
                    ..Default::default()
                },
            };
            let _ = sender.send(JobMessage::Finished(report));
            ctx.request_repaint();
        });
        self.job = Some(Job { receiver, total, done: 0 });
    }

    fn poll(&mut self) {
        let Some(job) = &mut self.job else { return };
        while let Ok(message) = job.receiver.try_recv() {
            match message {
                JobMessage::Progress(done) => job.done = done,
                JobMessage::Finished(report) => {
                    self.report = Some(report);
                    self.job = None;
                    return;
                }
            }
        }
    }

    /// Polls the job even while closed, then draws the window if it's open.
    /// `native_dialogs` is false when running without a screen, in which case
    /// asking for a folder only adds to `blocked_dialogs`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        selected: &[ClipId],
        native_dialogs: bool,
        blocked_dialogs: &mut u32,
    ) {
        self.poll();
        if !self.open {
            return;
        }
        let response = egui::Modal::new(egui::Id::unique("export_dialog")).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.heading("Export clips");
            ui.weak("Create one audio file for each clip.");
            ui.add_space(12.0);
            let selected_count = project.clips().iter().filter(|clip| selected.contains(&clip.id)).count();
            let mut plan = Vec::new();
            egui::ScrollArea::vertical()
                .id_salt("export_options")
                .max_height((ctx.content_rect().height() - 140.0).max(80.0))
                .show(ui, |ui| {
                    ui.add_enabled_ui(self.job.is_none() && self.pending.is_none(), |ui| {
                        self.options_ui(ui, project.clips().len(), selected_count, native_dialogs, blocked_dialogs);
                    });
                    ui.add_space(8.0);
                    // Calculate after edits so the export always matches the preview.
                    plan = self.plan(project, selected);
                    preview_ui(ui, &plan);
                    if let Some(report) = &self.report {
                        ui.add_space(6.0);
                        report_ui(ui, report);
                    }
                });
            ui.add_space(8.0);
            ui.separator();
            self.footer_ui(ui, ctx, project, selected, &plan);
        });

        if response.should_close() && self.job.is_none() && self.pending.is_none() {
            self.open = false;
        }
        if self.pending.is_some() {
            let confirmation = egui::Modal::new(egui::Id::unique("export_conflict")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Files already exist");
                ui.add_space(8.0);
                self.conflict_ui(ui, ctx);
            });
            if confirmation.should_close() {
                self.pending = None;
                ctx.request_repaint();
            }
        }
    }

    fn conflict_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pending = self.pending.as_ref().unwrap();
        let count = pending.conflicts.len();
        ui.label(RichText::new(if count == 1 {
            "A file with this name already exists.".to_owned()
        } else {
            format!("{count} files with matching names already exist.")
        }).strong());
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
            for name in &pending.conflicts {
                ui.add(egui::Label::new(RichText::new(name).monospace()).truncate()).on_hover_text(name);
            }
        });
        ui.add_space(10.0);
        ui.weak("Replace matching files, or keep them and export numbered copies.");
        ui.add_space(12.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            if ui.button("Replace existing").clicked() {
                choice = Some(ExistingFiles::Replace);
            }
            if ui.button("Make copies").clicked() {
                choice = Some(ExistingFiles::KeepBoth);
            }
            if ui.button("Cancel").clicked() {
                self.pending = None;
                ctx.request_repaint();
            }
        });
        // The browser version downloads a ZIP instead, so it never has conflicts to settle.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(policy) = choice {
            let pending = self.pending.take().unwrap();
            self.start(ctx, pending, policy);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = choice;
    }

    fn options_ui(
        &mut self,
        ui: &mut egui::Ui,
        clip_count: usize,
        selected_count: usize,
        native_dialogs: bool,
        blocked_dialogs: &mut u32,
    ) {
        #[cfg(target_arch = "wasm32")]
        let _ = (native_dialogs, &blocked_dialogs);
        let s = &mut self.settings;
        if selected_count == 0 {
            s.only_selected = false;
        }
        ui.columns(2, |columns| {
            let width = columns[0].available_width();
            if columns[0].add_sized([width, 34.0], egui::Button::new(format!("All clips  ({clip_count})")).selected(!s.only_selected)).clicked() {
                s.only_selected = false;
            }
            let width = columns[1].available_width();
            if columns[1].add_enabled_ui(selected_count > 0, |ui| {
                ui.add_sized([width, 34.0], egui::Button::new(format!("Selected clips  ({selected_count})")).selected(s.only_selected))
            }).inner.clicked() {
                s.only_selected = true;
            }
        });
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).inner_margin(12).corner_radius(8).show(ui, |ui| {
            ui.columns(2, |columns| {
                columns[0].label(RichText::new("Prefix").strong());
                columns[0].add(egui::TextEdit::singleline(&mut s.names.prefix).desired_width(f32::INFINITY).hint_text("Before the clip name"));
                columns[1].label(RichText::new("Suffix").strong());
                columns[1].add(egui::TextEdit::singleline(&mut s.names.suffix).desired_width(f32::INFINITY).hint_text("After the clip name"));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Format").strong());
                egui::ComboBox::from_id_salt("file_format").selected_text(s.format.label()).width(100.0)
                    .show_ui(ui, |ui| {
                        // MP3 and OGG need encoders that only exist in the desktop version.
                        let formats: &[FileFormat] = if cfg!(target_arch = "wasm32") {
                            &[FileFormat::Wav]
                        } else {
                            &[FileFormat::Wav, FileFormat::Mp3, FileFormat::Ogg]
                        };
                        for &format in formats {
                            ui.selectable_value(&mut s.format, format, format.label());
                        }
                    });
                ui.weak(match s.format {
                    FileFormat::Wav => "Uncompressed audio",
                    FileFormat::Mp3 => "Smaller files · 192 kbps",
                    FileFormat::Ogg => "Smaller files · Vorbis quality 6",
                });
            });
        });
        ui.add_space(10.0);
        ui.label(RichText::new("Destination").strong());
        ui.horizontal(|ui| {
            if cfg!(target_arch = "wasm32") {
                ui.weak("The files download together as one ZIP.");
                return;
            }
            if ui.button("Choose folder…").on_hover_text("Choose where the files go").clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                if native_dialogs {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        s.folder = Some(folder);
                    }
                } else {
                    *blocked_dialogs += 1;
                }
            }
            if let Some(folder) = &s.folder {
                let path = folder.display().to_string();
                ui.add(egui::Label::new(&path).truncate()).on_hover_text(&path);
            } else {
                ui.weak("Choose where to save your files");
            }
        });
        ui.add_space(6.0);
        egui::CollapsingHeader::new("Advanced options").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Sample rate");
                egui::ComboBox::from_id_salt("sample_rate").selected_text(format!("{} Hz", s.sample_rate))
                    .show_ui(ui, |ui| {
                        for rate in SAMPLE_RATES {
                            ui.selectable_value(&mut s.sample_rate, rate, format!("{rate} Hz"));
                        }
                    });
                if s.format == FileFormat::Wav {
                    ui.label("Bit depth");
                    egui::ComboBox::from_id_salt("bit_depth")
                        .selected_text(if s.depth == BitDepth::Bits24 { "24-bit" } else { "16-bit" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.depth, BitDepth::Bits16, "16-bit");
                            ui.selectable_value(&mut s.depth, BitDepth::Bits24, "24-bit");
                        });
                }
            });
            ui.add_space(4.0);
            ui.checkbox(&mut s.numbering_enabled, "Number files");
            ui.add_enabled_ui(s.numbering_enabled, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("number_position")
                        .selected_text(if s.number_position == NumberPosition::BeforeName { "Before name" } else { "After name" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.number_position, NumberPosition::BeforeName, "Before name");
                            ui.selectable_value(&mut s.number_position, NumberPosition::AfterName, "After name");
                        });
                    ui.label("Start");
                    ui.add(egui::DragValue::new(&mut s.number_start));
                    ui.label("Digits");
                    ui.add(egui::DragValue::new(&mut s.number_padding).range(1..=6));
                });
            });
        });
    }

    fn footer_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        project: &Project,
        selected: &[ClipId],
        plan: &[PlannedFile],
    ) {
        ui.horizontal(|ui| {
            let can_start = self.can_start(plan);
            let export = ui
                .add_enabled(can_start, egui::Button::new(RichText::new("Export files").strong()).min_size(egui::vec2(120.0, 32.0)).fill(ui.visuals().selection.bg_fill))
                .on_hover_text("Write the files listed above")
                .on_disabled_hover_text("Choose a folder and fix any problems shown in red first");
            if export.clicked() {
                self.request_export(ctx, project, selected, plan);
            }
            let running = self.job.is_some();
            let close = ui
                .add_enabled(!running, egui::Button::new("Close").min_size(egui::vec2(76.0, 32.0)))
                .on_hover_text("Close this window. Your settings are kept. (Esc)")
                .on_disabled_hover_text("Wait for the export to finish");
            if close.clicked() {
                self.open = false;
            }
            if let Some(job) = &self.job {
                ui.add(
                    egui::ProgressBar::new(job.done as f32 / job.total.max(1) as f32)
                        .text(format!("{} of {}", job.done, job.total)),
                );
            }
        });
    }
}

fn preview_ui(ui: &mut egui::Ui, plan: &[PlannedFile]) {
    let noun = if plan.len() == 1 { "file" } else { "files" };
    ui.label(RichText::new(format!("Preview · {} {noun}", plan.len())).strong());
    egui::ScrollArea::vertical()
        .max_height(PREVIEW_HEIGHT)
        .min_scrolled_height(PREVIEW_HEIGHT)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for file in plan {
                ui.horizontal(|ui| {
                    // Leave room for any problem shown beside the name.
                    ui.set_max_width(540.0);
                    let name = RichText::new(&file.file_name).monospace();
                    ui.scope(|ui| {
                        ui.set_max_width(320.0);
                        ui.add(egui::Label::new(name).truncate()).on_hover_text(&file.file_name);
                    });
                    match file.problem {
                        Some(FileProblem::Duplicate) => {
                            ui.colored_label(PROBLEM, "same name as another clip");
                        }
                        Some(FileProblem::Exists) => {
                            ui.colored_label(WARNING, "already exists");
                        }
                        None => {}
                    }
                    if file.sanitized {
                        ui.colored_label(WARNING, "invalid characters changed");
                    }
                });
            }
        });
}

fn report_ui(ui: &mut egui::Ui, report: &ExportReport) {
    let noun = if report.written == 1 { "file" } else { "files" };
    ui.colored_label(Color32::from_rgb(130, 210, 140), format!("Exported {} {noun}.", report.written));
    if !report.clipped.is_empty() {
        ui.colored_label(WARNING, format!("Limited to full scale: {}", report.clipped.join(", ")));
    }
    if !report.failed.is_empty() {
        ui.colored_label(PROBLEM, format!("Failed: {}", report.failed.join("; ")));
    }
}
