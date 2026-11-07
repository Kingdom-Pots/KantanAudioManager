//! Turns clips into WAV files: working out filenames, then writing them.

use std::collections::HashSet;
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
    /// Without the dot.
    pub extension: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Bits16,
    Bits24,
}

/// What to write. MP3 is 192 kbps and OGG is Vorbis at quality 6, both good for voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Wav(BitDepth),
    Mp3,
    Ogg,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav(_) => "wav",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
        }
    }
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
            let file_name = format!("{stem}.{}", options.extension);
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

/// How a batch handles names that already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingFiles {
    Refuse,
    Replace,
    KeepBoth,
}

/// Writes one WAV per clip. `progress` is called after each file with the number done.
pub fn export_clips(
    project: &Project,
    files: &[(ClipId, PathBuf)],
    sample_rate: u32,
    format: OutputFormat,
    progress: impl FnMut(usize),
) -> ExportReport {
    export_clips_with_policy(project, files, sample_rate, format, ExistingFiles::Replace, progress)
}

/// Publishes complete files using the conflict choice made for this batch.
pub fn export_clips_with_policy(
    project: &Project,
    files: &[(ClipId, PathBuf)],
    sample_rate: u32,
    format: OutputFormat,
    policy: ExistingFiles,
    mut progress: impl FnMut(usize),
) -> ExportReport {
    let reserved: HashSet<String> = files.iter().map(|(_, path)| path.to_string_lossy().to_lowercase()).collect();
    let mut report = ExportReport::default();
    for (index, (id, path)) in files.iter().enumerate() {
        let shown = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        match write_clip(project, *id, path, sample_rate, format, policy, &reserved) {
            Ok((clipped, written_path)) => {
                report.written += 1;
                if clipped {
                    report.clipped.push(written_path.file_name().unwrap_or_default().to_string_lossy().into_owned());
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
    format: OutputFormat,
    policy: ExistingFiles,
    reserved: &HashSet<String>,
) -> Result<(bool, PathBuf), String> {
    let clip = project.clip(id).ok_or("the clip no longer exists")?;
    let rendered = render_clip(project, clip);
    let samples = resample(
        &rendered.samples,
        rendered.channels as usize,
        project.sample_rate(),
        sample_rate,
    )
    .map_err(|e| e.to_string())?;

    // Each job owns a unique temporary file. Publish only after encoding completes.
    let temp = tempfile::Builder::new()
        .prefix(".kantan-export-")
        .suffix(".part")
        .tempfile_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(|error| error.to_string())?
        .into_temp_path();
    let channels = rendered.channels;
    match format {
        OutputFormat::Wav(depth) => write_wav(&temp, &samples, channels, sample_rate, depth),
        OutputFormat::Mp3 => write_mp3(&temp, &samples, channels, sample_rate),
        OutputFormat::Ogg => write_ogg(&temp, &samples, channels, sample_rate),
    }?;
    let published = publish_file(temp, path, policy, reserved)?;
    Ok((samples.iter().any(|s| s.abs() > 1.0), published))
}

fn publish_file(
    mut temp: tempfile::TempPath,
    path: &Path,
    policy: ExistingFiles,
    reserved: &HashSet<String>,
) -> Result<PathBuf, String> {
    if policy == ExistingFiles::Replace {
        temp.persist(path).map_err(|error| error.to_string())?;
        return Ok(path.to_path_buf());
    }
    let mut destination = path.to_path_buf();
    let mut copy_number = 2u64;
    loop {
        match temp.persist_noclobber(&destination) {
            Ok(()) => return Ok(destination),
            Err(error) if policy == ExistingFiles::KeepBoth && error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temp = error.path;
                loop {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                    let extension = path.extension().unwrap_or_default().to_string_lossy();
                    destination = path.with_file_name(format!("{stem} ({copy_number}).{extension}"));
                    copy_number += 1;
                    if !destination.exists() && !reserved.contains(&destination.to_string_lossy().to_lowercase()) {
                        break;
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
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

fn write_mp3(path: &Path, samples: &[f32], channels: u16, sample_rate: u32) -> Result<(), String> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, MonoPcm};

    let error = |e: &dyn std::fmt::Debug| format!("MP3 encoder: {e:?}");
    let mut encoder = Builder::new()
        .ok_or("MP3 encoder: couldn't start")?
        .with_num_channels(channels as u8)
        .map_err(|e| error(&e))?
        .with_sample_rate(sample_rate)
        .map_err(|e| error(&e))?
        .with_brate(Bitrate::Kbps192)
        .map_err(|e| error(&e))?
        .with_quality(mp3lame_encoder::Quality::Best)
        .map_err(|e| error(&e))?
        .build()
        .map_err(|e| error(&e))?;

    let pcm: Vec<i16> = samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect();
    let mut output = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(pcm.len()));
    if channels == 1 {
        encoder.encode_to_vec(MonoPcm(&pcm), &mut output).map_err(|e| error(&e))?;
    } else {
        encoder.encode_to_vec(InterleavedPcm(&pcm), &mut output).map_err(|e| error(&e))?;
    }
    encoder.flush_to_vec::<FlushNoGap>(&mut output).map_err(|e| error(&e))?;
    std::fs::write(path, output).map_err(|e| e.to_string())
}

fn write_ogg(path: &Path, samples: &[f32], channels: u16, sample_rate: u32) -> Result<(), String> {
    use std::num::{NonZeroU8, NonZeroU32};
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

    let rate = NonZeroU32::new(sample_rate).ok_or("invalid sample rate")?;
    let count = NonZeroU8::new(channels as u8).ok_or("invalid channel count")?;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let error = |e: vorbis_rs::VorbisError| format!("OGG encoder: {e}");
    let mut encoder = VorbisEncoderBuilder::new(rate, count, std::io::BufWriter::new(file))
        .map_err(error)?
        .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr { target_quality: 0.6 })
        .build()
        .map_err(error)?;

    let width = channels as usize;
    for block in samples.chunks(4096 * width) {
        let planes: Vec<Vec<f32>> = (0..width)
            .map(|channel| {
                block.iter().skip(channel).step_by(width).map(|s| s.clamp(-1.0, 1.0)).collect()
            })
            .collect();
        encoder.encode_audio_block(&planes).map_err(error)?;
    }
    encoder.finish().map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Source, TimeRange};

    fn options() -> NameOptions {
        NameOptions { prefix: String::new(), suffix: String::new(), numbering: None, extension: "wav" }
    }

    fn numbering(position: NumberPosition, start: u32, padding: usize) -> Option<Numbering> {
        Some(Numbering { position, start, padding })
    }

    fn names(files: &[PlannedFile]) -> Vec<&str> {
        files.iter().map(|f| f.file_name.as_str()).collect()
    }

    /// Encodes one second of tone, then decodes it again with the import code.
    fn round_trip(format: OutputFormat, channels: u16) -> Source {
        let dir = TempDir::new(&format!("{}{channels}", format.extension()));
        let path = dir.0.join(format!("a.{}", format.extension()));
        let samples: Vec<f32> = (0..48000)
            .flat_map(|i| {
                let v = (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
                std::iter::repeat_n(v, channels as usize)
            })
            .collect();
        let (project, id) = project_with_clip(channels, samples);
        let report = export_clips(&project, &[(id, path.clone())], 44100, format, |_| {});
        assert_eq!(report.failed, Vec::<String>::new());
        crate::import::import_file(&path, 44100).unwrap()
    }

    #[test]
    fn mp3_and_ogg_decode_back_to_the_same_audio() {
        for format in [OutputFormat::Mp3, OutputFormat::Ogg] {
            for channels in [1, 2] {
                let source = round_trip(format, channels);
                assert_eq!(source.channels, channels, "{format:?}");
                // Lossy formats pad the ends a little.
                let frames = source.frames() as i64;
                assert!((frames - 44100).abs() < 4000, "{format:?} {channels}: {frames}");
                let (lo, hi) = source.min_max(10000, 20000);
                assert!((hi - 0.5).abs() < 0.05 && (lo + 0.5).abs() < 0.05, "{format:?}: {lo} {hi}");
            }
        }
    }

    #[test]
    fn every_offered_sample_rate_encodes() {
        for rate in [48000, 44100, 32000, 22050, 16000] {
            for format in [OutputFormat::Mp3, OutputFormat::Ogg] {
                let dir = TempDir::new(&format!("rate{rate}{}", format.extension()));
                let path = dir.0.join("a");
                let (project, id) = project_with_clip(1, vec![0.25; 48000]);
                let report = export_clips(&project, &[(id, path)], rate, format, |_| {});
                assert_eq!(report.written, 1, "{format:?} at {rate}: {:?}", report.failed);
            }
        }
    }

    #[test]
    fn the_extension_follows_the_format() {
        let opts = NameOptions { extension: OutputFormat::Mp3.extension(), ..options() };
        assert_eq!(names(&plan_files(&["a"], &opts, |_| false)), ["a.mp3"]);
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
            extension: "wav",
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
        let report = export_clips(&project, &[(id, path.clone())], 48000, OutputFormat::Wav(BitDepth::Bits16), |n| seen.push(n));

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

        export_clips(&project, &[(id, path.clone())], 44100, OutputFormat::Wav(BitDepth::Bits24), |_| {});

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

        let report = export_clips(&project, &[(id, path.clone())], 48000, OutputFormat::Wav(BitDepth::Bits16), |_| {});

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

        export_clips(&project, &[(id, path.clone())], 48000, OutputFormat::Wav(BitDepth::Bits16), |_| {});

        assert!(hound::WavReader::open(&path).is_ok());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn one_failure_does_not_stop_the_rest() {
        let dir = TempDir::new("fail");
        let (project, id) = project_with_clip(1, vec![0.5]);
        let bad = dir.0.join("missing_folder").join("bad.wav");
        let good = dir.0.join("good.wav");

        let report = export_clips(&project, &[(id, bad), (id, good.clone())], 48000, OutputFormat::Wav(BitDepth::Bits16), |_| {});

        assert_eq!(report.written, 1);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].starts_with("bad.wav"));
        assert!(good.exists());
    }
    #[test]
    fn unconfirmed_export_preserves_a_file_that_appeared_after_preview() {
        let dir = TempDir::new("refuse");
        let path = dir.0.join("a.wav");
        std::fs::write(&path, b"original").unwrap();
        let (project, id) = project_with_clip(1, vec![0.5]);
        let report = export_clips_with_policy(&project, &[(id, path.clone())],
            48000, OutputFormat::Wav(BitDepth::Bits16), ExistingFiles::Refuse, |_| {});
        assert_eq!(report.written, 0);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn copies_reserve_names_for_the_remaining_batch() {
        let dir = TempDir::new("reserved_copy");
        let path = dir.0.join("a.wav");
        let next = dir.0.join("a (2).wav");
        std::fs::write(&path, b"original").unwrap();
        let (project, id) = project_with_clip(1, vec![0.5]);
        let report = export_clips_with_policy(&project, &[(id, path.clone()), (id, next.clone())],
            48000, OutputFormat::Wav(BitDepth::Bits16), ExistingFiles::KeepBoth, |_| {});
        assert_eq!(report.written, 2);
        assert!(report.failed.is_empty());
        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert!(next.exists());
        assert!(dir.0.join("a (3).wav").exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 3);
    }

}
