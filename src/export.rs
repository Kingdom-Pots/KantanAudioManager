//! Turns clips into WAV files: working out filenames, then writing them.

use std::path::{Path, PathBuf};

use crate::import::resample;
use crate::{ClipId, Project, render_clip};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberPosition {
    /// `prefix` `001_` `name` `suffix`
    BeforeName,
    /// `prefix` `name` `_001` `suffix`
    AfterName,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Numbering {
    pub position: NumberPosition,
    pub start: u32,
    /// Minimum digits, filled with leading zeros.
    pub padding: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NameOptions {
    pub prefix: String,
    pub suffix: String,
    pub numbering: Option<Numbering>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Bits16,
    Bits24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileProblem {
    /// Another clip in this export would get the same filename.
    Duplicate,
    /// A file with this name is already in the folder.
    Exists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub file_name: String,
    pub problem: Option<FileProblem>,
    /// Characters Windows doesn't allow in filenames had to be changed.
    pub sanitized: bool,
}

/// Works out the filename for each clip, in the order given.
pub fn plan_files(
    clip_names: &[&str],
    options: &NameOptions,
    exists: impl Fn(&str) -> bool,
) -> Vec<PlannedFile> {
    let stems: Vec<(String, bool)> = clip_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let raw = format!(
                "{}{}{}",
                options.prefix,
                numbered(name, index, options.numbering.as_ref()),
                options.suffix
            );
            let clean = sanitize(&raw);
            let changed = clean != raw;
            (clean, changed)
        })
        .collect();

    // Windows filenames ignore case, so these would overwrite each other.
    let lowered: Vec<String> = stems.iter().map(|(stem, _)| stem.to_lowercase()).collect();

    stems
        .iter()
        .enumerate()
        .map(|(index, (stem, sanitized))| {
            let file_name = format!("{stem}.wav");
            let duplicate = lowered.iter().filter(|other| **other == lowered[index]).count() > 1;
            let problem = if duplicate {
                Some(FileProblem::Duplicate)
            } else if exists(&file_name) {
                Some(FileProblem::Exists)
            } else {
                None
            };
            PlannedFile { file_name, problem, sanitized: *sanitized }
        })
        .collect()
}

fn numbered(name: &str, index: usize, numbering: Option<&Numbering>) -> String {
    let Some(numbering) = numbering else {
        return name.to_owned();
    };
    let number = numbering.start as u64 + index as u64;
    let digits = format!("{number:0width$}", width = numbering.padding);
    match numbering.position {
        NumberPosition::BeforeName => format!("{digits}_{name}"),
        NumberPosition::AfterName => format!("{name}_{digits}"),
    }
}

/// Makes a name valid on Windows, keeping it as recognizable as possible.
fn sanitize(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    // Windows silently strips trailing dots and spaces.
    let trimmed = replaced.trim_end_matches(['.', ' ']).trim_start_matches(' ');
    if trimmed.is_empty() {
        return "_".to_owned();
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(trimmed)) {
        return format!("{trimmed}_");
    }
    trimmed.to_owned()
}

/// What happened in an export. One file failing doesn't stop the others.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportReport {
    pub written: usize,
    pub failed: Vec<String>,
    /// Files whose audio went past full scale and was limited to it.
    pub clipped: Vec<String>,
}

/// Writes one WAV per clip. `progress` is called after each file with the number done.
pub fn export_clips(
    project: &Project,
    files: &[(ClipId, PathBuf)],
    sample_rate: u32,
    depth: BitDepth,
    mut progress: impl FnMut(usize),
) -> ExportReport {
    let mut report = ExportReport::default();
    for (index, (id, path)) in files.iter().enumerate() {
        let shown = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        match write_clip(project, *id, path, sample_rate, depth) {
            Ok(clipped) => {
                report.written += 1;
                if clipped {
                    report.clipped.push(shown);
                }
            }
            Err(error) => report.failed.push(format!("{shown}: {error}")),
        }
        progress(index + 1);
    }
    report
}

/// Returns whether the audio had to be limited to full scale.
fn write_clip(
    project: &Project,
    id: ClipId,
    path: &Path,
    sample_rate: u32,
    depth: BitDepth,
) -> Result<bool, String> {
    let clip = project.clip(id).ok_or("the clip no longer exists")?;
    let rendered = render_clip(project, clip);
    let samples = resample(
        &rendered.samples,
        rendered.channels as usize,
        project.sample_rate(),
        sample_rate,
    )
    .map_err(|e| e.to_string())?;

    // A partial file must never replace a good one, or look like a finished export.
    let mut temp = path.as_os_str().to_owned();
    temp.push(".part");
    let temp = PathBuf::from(temp);
    let result = write_wav(&temp, &samples, rendered.channels, sample_rate, depth)
        .and_then(|()| std::fs::rename(&temp, path).map_err(|e| e.to_string()));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result?;

    Ok(samples.iter().any(|s| s.abs() > 1.0))
}

fn write_wav(
    path: &Path,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    depth: BitDepth,
) -> Result<(), String> {
    let bits_per_sample = match depth {
        BitDepth::Bits16 => 16,
        BitDepth::Bits24 => 24,
    };
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format: hound::SampleFormat::Int,
    };
    let error = |e: hound::Error| e.to_string();
    let mut writer = hound::WavWriter::create(path, spec).map_err(error)?;
    for &sample in samples {
        let sample = sample.clamp(-1.0, 1.0);
        match depth {
            BitDepth::Bits16 => writer
                .write_sample((sample * i16::MAX as f32).round() as i16)
                .map_err(error)?,
            BitDepth::Bits24 => writer
                .write_sample((sample as f64 * 8_388_607.0).round() as i32)
                .map_err(error)?,
        }
    }
    writer.finalize().map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimeRange;

    fn options() -> NameOptions {
        NameOptions { prefix: String::new(), suffix: String::new(), numbering: None }
    }

    fn numbering(position: NumberPosition, start: u32, padding: usize) -> Option<Numbering> {
        Some(Numbering { position, start, padding })
    }

    fn names(files: &[PlannedFile]) -> Vec<&str> {
        files.iter().map(|f| f.file_name.as_str()).collect()
    }

    #[test]
    fn plain_names_use_the_clip_name() {
        let files = plan_files(&["Hello", "声_01"], &options(), |_| false);
        assert_eq!(names(&files), ["Hello.wav", "声_01.wav"]);
        assert!(files.iter().all(|f| f.problem.is_none() && !f.sanitized));
    }

    #[test]
    fn prefix_suffix_and_numbers_combine() {
        let opts = NameOptions {
            prefix: "SFX_".to_owned(),
            suffix: "_final".to_owned(),
            numbering: numbering(NumberPosition::BeforeName, 1, 3),
        };
        let files = plan_files(&["Explosion", "Door"], &opts, |_| false);
        assert_eq!(names(&files), ["SFX_001_Explosion_final.wav", "SFX_002_Door_final.wav"]);

        let opts = NameOptions {
            numbering: numbering(NumberPosition::AfterName, 9, 1),
            ..options()
        };
        let files = plan_files(&["a", "b"], &opts, |_| false);
        assert_eq!(names(&files), ["a_9.wav", "b_10.wav"]);
    }

    #[test]
    fn duplicates_are_flagged_ignoring_case() {
        let files = plan_files(&["Line", "line", "other"], &options(), |_| false);
        assert_eq!(files[0].problem, Some(FileProblem::Duplicate));
        assert_eq!(files[1].problem, Some(FileProblem::Duplicate));
        assert_eq!(files[2].problem, None);
    }

    #[test]
    fn numbering_resolves_duplicates() {
        let opts = NameOptions {
            numbering: numbering(NumberPosition::AfterName, 1, 2),
            ..options()
        };
        let files = plan_files(&["Line", "Line"], &opts, |_| false);
        assert!(files.iter().all(|f| f.problem.is_none()));
    }

    #[test]
    fn existing_files_are_flagged() {
        let files = plan_files(&["a", "b"], &options(), |name| name == "b.wav");
        assert_eq!(files[0].problem, None);
        assert_eq!(files[1].problem, Some(FileProblem::Exists));
    }

    #[test]
    fn invalid_names_are_made_safe() {
        let files = plan_files(&["a/b:c?", "end. ", "CON", "...", "  x"], &options(), |_| false);
        assert_eq!(names(&files), ["a_b_c_.wav", "end.wav", "CON_.wav", "_.wav", "x.wav"]);
        assert!(files.iter().all(|f| f.sanitized));
    }

    #[test]
    fn invalid_characters_can_create_duplicates() {
        let files = plan_files(&["a?", "a*"], &options(), |_| false);
        assert!(files.iter().all(|f| f.problem == Some(FileProblem::Duplicate)));
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kantan_export_{}_{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn project_with_clip(channels: u16, samples: Vec<f32>) -> (Project, ClipId) {
        let mut project = Project::new(48000);
        let source = project.add_source(channels, samples).unwrap();
        let id = project.append_clip("a", source).unwrap();
        (project, id)
    }

    #[test]
    fn writes_16_bit_wav_with_exact_values() {
        let dir = TempDir::new("16");
        let path = dir.0.join("a.wav");
        let (project, id) = project_with_clip(2, vec![0.5, -0.5, 1.0, -1.0]);

        let mut seen = Vec::new();
        let report = export_clips(&project, &[(id, path.clone())], 48000, BitDepth::Bits16, |n| seen.push(n));

        assert_eq!(report, ExportReport { written: 1, ..Default::default() });
        assert_eq!(seen, [1]);
        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (2, 48000, 16));
        let values: Vec<i16> = reader.samples().map(Result::unwrap).collect();
        assert_eq!(values, [16384, -16384, 32767, -32767]);
    }

    #[test]
    fn writes_24_bit_and_resamples_to_the_chosen_rate() {
        let dir = TempDir::new("24");
        let path = dir.0.join("a.wav");
        let (project, id) = project_with_clip(1, vec![0.25; 48000]);

        export_clips(&project, &[(id, path.clone())], 44100, BitDepth::Bits24, |_| {});

        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (1, 44100, 24));
        assert_eq!(reader.duration(), 44100);
        let middle: i32 = reader.samples().nth(22050).unwrap().unwrap();
        assert!((middle - 2_097_152).abs() < 3000, "{middle}");
    }

    #[test]
    fn amplified_audio_is_exported_and_clipping_is_reported() {
        let dir = TempDir::new("clip");
        let path = dir.0.join("a.wav");
        let (mut project, id) = project_with_clip(1, vec![0.5, 0.5]);
        project.amplify(TimeRange::new(0, 1), 20.0).unwrap();

        let report = export_clips(&project, &[(id, path.clone())], 48000, BitDepth::Bits16, |_| {});

        assert_eq!(report.clipped, ["a.wav"]);
        let values: Vec<i16> = hound::WavReader::open(&path).unwrap().samples().map(Result::unwrap).collect();
        assert_eq!(values, [32767, 16384]);
    }

    #[test]
    fn overwriting_replaces_and_leaves_no_temp_files() {
        let dir = TempDir::new("over");
        let path = dir.0.join("a.wav");
        std::fs::write(&path, b"old").unwrap();
        let (project, id) = project_with_clip(1, vec![0.5]);

        export_clips(&project, &[(id, path.clone())], 48000, BitDepth::Bits16, |_| {});

        assert!(hound::WavReader::open(&path).is_ok());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn one_failure_does_not_stop_the_rest() {
        let dir = TempDir::new("fail");
        let (project, id) = project_with_clip(1, vec![0.5]);
        let bad = dir.0.join("missing_folder").join("bad.wav");
        let good = dir.0.join("good.wav");

        let report = export_clips(&project, &[(id, bad), (id, good.clone())], 48000, BitDepth::Bits16, |_| {});

        assert_eq!(report.written, 1);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].starts_with("bad.wav"));
        assert!(good.exists());
    }
}
