//! The export window: options, a live filename preview, and the running job.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};

use crate::export::{
    BitDepth, ExportReport, FileProblem, NameOptions, NumberPosition, Numbering, OutputFormat,
    PlannedFile,
    export_clips, plan_files,
};
use crate::project_file::Settings;
use crate::{ClipId, Project};

const SAMPLE_RATES: [u32; 5] = [48000, 44100, 32000, 22050, 16000];
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
    pub overwrite: bool,
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

pub struct ExportDialog {
    pub open: bool,
    pub settings: ExportSettings,
    job: Option<Job>,
    report: Option<ExportReport>,
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
                overwrite: false,
            },
            job: None,
            report: None,
        }
    }
}

impl ExportDialog {
    /// Opens on the selected clips if there are any, otherwise on all of them.
    pub fn open(&mut self, any_selected: bool) {
        self.open = true;
        self.report = None;
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

    fn blocks_export(&self, file: &PlannedFile) -> bool {
        match file.problem {
            Some(FileProblem::Duplicate) => true,
            Some(FileProblem::Exists) => !self.settings.overwrite,
            None => false,
        }
    }

    fn can_start(&self, plan: &[PlannedFile]) -> bool {
        self.job.is_none()
            && self.settings.folder.is_some()
            && !plan.is_empty()
            && !plan.iter().any(|file| self.blocks_export(file))
    }

    /// Writes on a worker thread, using a snapshot of the project.
    fn start(&mut self, ctx: &egui::Context, project: &Project, selected: &[ClipId], plan: &[PlannedFile]) {
        let Some(folder) = self.settings.folder.clone() else { return };
        let files: Vec<(ClipId, PathBuf)> = self
            .clips(project, selected)
            .iter()
            .zip(plan)
            .map(|(clip, file)| (clip.id, folder.join(&file.file_name)))
            .collect();

        let total = files.len();
        let project = project.clone();
        let (rate, format) = (self.settings.sample_rate, self.settings.output_format());
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.report = None;
        std::thread::spawn(move || {
            let report = match std::fs::create_dir_all(&folder) {
                Ok(()) => export_clips(&project, &files, rate, format, |done| {
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
        let plan = self.plan(project, selected);

        let response = egui::Modal::new(egui::Id::unique("export_dialog")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Export clips");
            ui.add_space(4.0);
            self.options_ui(ui, !selected.is_empty(), native_dialogs, blocked_dialogs);
            ui.separator();
            preview_ui(ui, &plan, self.settings.overwrite);
            ui.separator();
            self.footer_ui(ui, ctx, project, selected, &plan);
        });

        if response.should_close() && self.job.is_none() {
            self.open = false;
        }
    }

    fn options_ui(
        &mut self,
        ui: &mut egui::Ui,
        any_selected: bool,
        native_dialogs: bool,
        blocked_dialogs: &mut u32,
    ) {
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            if ui.button("Folder...").on_hover_text("Choose where the files go").clicked() {
                if native_dialogs {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        s.folder = Some(folder);
                    }
                } else {
                    *blocked_dialogs += 1;
                }
            }
            match &s.folder {
                // Long paths and names must not stretch the window off screen.
                Some(folder) => {
                    let path = folder.display().to_string();
                    ui.add(egui::Label::new(&path).truncate()).on_hover_text(&path)
                }
                None => ui.colored_label(WARNING, "Choose a folder to export to"),
            };
        });
        ui.horizontal(|ui| {
            ui.label("Prefix");
            ui.add(egui::TextEdit::singleline(&mut s.names.prefix).desired_width(150.0))
                .on_hover_text("Added to the start of every filename");
            ui.label("Suffix");
            ui.add(egui::TextEdit::singleline(&mut s.names.suffix).desired_width(150.0))
                .on_hover_text("Added to the end of every filename, before the extension");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.numbering_enabled, "Numbering")
                .on_hover_text("Number the files in timeline order");
            ui.add_enabled_ui(s.numbering_enabled, |ui| {
                egui::ComboBox::from_id_salt("number_position")
                    .selected_text(match s.number_position {
                        NumberPosition::BeforeName => "Before name",
                        NumberPosition::AfterName => "After name",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.number_position, NumberPosition::BeforeName, "Before name");
                        ui.selectable_value(&mut s.number_position, NumberPosition::AfterName, "After name");
                    })
                    .response
                    .on_hover_text("Where the number goes in the filename");
                ui.label("Start");
                ui.add(egui::DragValue::new(&mut s.number_start))
                    .on_hover_text("The first file's number");
                ui.label("Digits");
                ui.add(egui::DragValue::new(&mut s.number_padding).range(1..=6))
                    .on_hover_text("Minimum digits. 3 gives 001, 002, 003.");
            });
        });
        ui.horizontal(|ui| {
            ui.label("Format");
            egui::ComboBox::from_id_salt("file_format")
                .selected_text(s.format.label())
                .width(70.0)
                .show_ui(ui, |ui| {
                    for format in [FileFormat::Wav, FileFormat::Mp3, FileFormat::Ogg] {
                        ui.selectable_value(&mut s.format, format, format.label());
                    }
                })
                .response
                .on_hover_text("WAV is uncompressed. MP3 (192 kbps) and OGG (quality 6) make smaller files.");
            ui.label("Sample rate");
            egui::ComboBox::from_id_salt("sample_rate")
                .selected_text(format!("{} Hz", s.sample_rate))
                .show_ui(ui, |ui| {
                    for rate in SAMPLE_RATES {
                        ui.selectable_value(&mut s.sample_rate, rate, format!("{rate} Hz"));
                    }
                })
                .response
                .on_hover_text("48000 or 44100 Hz suit most uses. Lower rates make smaller files with less detail.");
            // Only WAV has a bit depth.
            ui.add_enabled_ui(s.format == FileFormat::Wav, |ui| {
                ui.label("Bit depth");
                egui::ComboBox::from_id_salt("bit_depth")
                    .selected_text(match s.depth {
                        BitDepth::Bits16 => "16-bit",
                        BitDepth::Bits24 => "24-bit",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.depth, BitDepth::Bits16, "16-bit");
                        ui.selectable_value(&mut s.depth, BitDepth::Bits24, "24-bit");
                    })
                    .response
                    .on_hover_text("WAV precision. 16-bit suits most uses.")
                    .on_disabled_hover_text("Only WAV has a bit depth");
            });
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.only_selected, false, "All clips")
                .on_hover_text("Export every clip on the track");
            ui.add_enabled_ui(any_selected, |ui| {
                ui.radio_value(&mut s.only_selected, true, "Selected clips")
                    .on_hover_text("Export only the clips you selected")
                    .on_disabled_hover_text("No clips are selected");
            });
            ui.checkbox(&mut s.overwrite, "Overwrite existing files")
                .on_hover_text("Replace files that already have these names");
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
        if let Some(report) = &self.report {
            report_ui(ui, report);
        }
        ui.horizontal(|ui| {
            let can_start = self.can_start(plan);
            let export = ui
                .add_enabled(can_start, egui::Button::new("Export files"))
                .on_hover_text("Write the files listed above")
                .on_disabled_hover_text("Choose a folder and fix any problems shown in red first");
            if export.clicked() {
                self.start(ctx, project, selected, plan);
            }
            let running = self.job.is_some();
            let close = ui
                .add_enabled(!running, egui::Button::new("Close"))
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

fn preview_ui(ui: &mut egui::Ui, plan: &[PlannedFile], overwrite: bool) {
    let noun = if plan.len() == 1 { "file" } else { "files" };
    ui.label(format!("{} {noun}", plan.len()));
    egui::ScrollArea::vertical()
        .max_height(130.0)
        .auto_shrink([false, true])
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
                        Some(FileProblem::Exists) if overwrite => {
                            ui.colored_label(WARNING, "will overwrite");
                        }
                        Some(FileProblem::Exists) => {
                            ui.colored_label(PROBLEM, "already exists");
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
