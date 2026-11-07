//! The export window: options, a live filename preview, and the running job.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};

use crate::export::{
    BitDepth, ExportReport, FileProblem, NameOptions, NumberPosition, Numbering, PlannedFile,
    export_clips, plan_files,
};
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
    pub only_selected: bool,
    pub overwrite: bool,
}

impl ExportSettings {
    fn name_options(&self) -> NameOptions {
        NameOptions {
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
                names: NameOptions { prefix: String::new(), suffix: String::new(), numbering: None },
                numbering_enabled: false,
                number_position: NumberPosition::BeforeName,
                number_start: 1,
                number_padding: 3,
                sample_rate: 48000,
                depth: BitDepth::Bits16,
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
        let (rate, depth) = (self.settings.sample_rate, self.settings.depth);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.report = None;
        std::thread::spawn(move || {
            let report = match std::fs::create_dir_all(&folder) {
                Ok(()) => export_clips(&project, &files, rate, depth, |done| {
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
            if ui.button("Folder...").clicked() {
                if native_dialogs {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        s.folder = Some(folder);
                    }
                } else {
                    *blocked_dialogs += 1;
                }
            }
            match &s.folder {
                Some(folder) => ui.label(folder.display().to_string()),
                None => ui.colored_label(WARNING, "Choose a folder to export to"),
            };
        });
        ui.horizontal(|ui| {
            ui.label("Prefix");
            ui.add(egui::TextEdit::singleline(&mut s.names.prefix).desired_width(150.0));
            ui.label("Suffix");
            ui.add(egui::TextEdit::singleline(&mut s.names.suffix).desired_width(150.0));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.numbering_enabled, "Numbering");
            ui.add_enabled_ui(s.numbering_enabled, |ui| {
                egui::ComboBox::from_id_salt("number_position")
                    .selected_text(match s.number_position {
                        NumberPosition::BeforeName => "Before name",
                        NumberPosition::AfterName => "After name",
                    })
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
        ui.horizontal(|ui| {
            ui.label("Sample rate");
            egui::ComboBox::from_id_salt("sample_rate")
                .selected_text(format!("{} Hz", s.sample_rate))
                .show_ui(ui, |ui| {
                    for rate in SAMPLE_RATES {
                        ui.selectable_value(&mut s.sample_rate, rate, format!("{rate} Hz"));
                    }
                });
            ui.label("Bit depth");
            egui::ComboBox::from_id_salt("bit_depth")
                .selected_text(match s.depth {
                    BitDepth::Bits16 => "16-bit",
                    BitDepth::Bits24 => "24-bit",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.depth, BitDepth::Bits16, "16-bit");
                    ui.selectable_value(&mut s.depth, BitDepth::Bits24, "24-bit");
                });
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.only_selected, false, "All clips");
            ui.add_enabled_ui(any_selected, |ui| {
                ui.radio_value(&mut s.only_selected, true, "Selected clips");
            });
            ui.checkbox(&mut s.overwrite, "Overwrite existing files");
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
            let export = ui.add_enabled(can_start, egui::Button::new("Export files"));
            if export.clicked() {
                self.start(ctx, project, selected, plan);
            }
            let running = self.job.is_some();
            if ui.add_enabled(!running, egui::Button::new("Close")).clicked() {
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
                    ui.label(RichText::new(&file.file_name).monospace());
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
