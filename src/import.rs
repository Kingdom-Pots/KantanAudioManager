//! Reads audio files and converts them to the project's sample rate.

use std::cmp::Ordering;
use std::fmt;
use std::path::Path;

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::project::{EditError, Source};

/// File extensions offered by the import dialog.
pub const SUPPORTED_EXTENSIONS: [&str; 4] = ["wav", "mp3", "flac", "ogg"];

#[derive(Debug)]
pub struct ImportError(String);

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImportError {}

impl From<EditError> for ImportError {
    fn from(error: EditError) -> Self {
        Self(error.to_string())
    }
}

fn error(message: impl fmt::Display) -> ImportError {
    ImportError(message.to_string())
}

/// Decoded audio, interleaved, at the file's own sample rate.
struct Decoded {
    channels: usize,
    rate: u32,
    samples: Vec<f32>,
}

/// Reads a file into a source at `project_rate`. The source is built here so
/// this whole slow step can run off the UI thread.
pub fn import_file(path: &Path, project_rate: u32) -> Result<Source, ImportError> {
    let decoded = decode(path)?;
    let (channels, samples) = reduce_channels(decoded.channels, decoded.samples);
    let samples = resample(&samples, channels, decoded.rate, project_rate)?;
    Ok(Source::new(channels as u16, samples)?)
}

fn decode(path: &Path) -> Result<Decoded, ImportError> {
    let file = std::fs::File::open(path).map_err(error)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|_| error("not a supported audio file"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| error("no audio found in file"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| error("no audio found in file"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|_| error("unsupported audio format"))?;

    let mut result = Decoded { channels: 0, rate: 0, samples: Vec::new() };
    let mut packet_samples = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // A new chained stream starts here; only the first one is imported.
            Err(DecodeError::ResetRequired) => break,
            Err(e) => return Err(error(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            // One damaged packet shouldn't lose the rest of the file.
            Err(DecodeError::DecodeError(_) | DecodeError::IoError(_)) => continue,
            Err(e) => return Err(error(e)),
        };
        result.channels = buffer.spec().channels().count();
        result.rate = buffer.spec().rate();
        packet_samples.resize(buffer.samples_interleaved(), 0.0f32);
        buffer.copy_to_slice_interleaved(&mut packet_samples);
        result.samples.extend_from_slice(&packet_samples);
    }

    if result.samples.is_empty() {
        return Err(error("no audio found in file"));
    }
    Ok(result)
}

/// Mono and stereo pass through. Anything wider is averaged to mono, because
/// keeping only the first two channels of surround audio would drop the centre
/// channel, which is where speech usually sits.
fn reduce_channels(channels: usize, samples: Vec<f32>) -> (usize, Vec<f32>) {
    if channels <= 2 {
        return (channels, samples);
    }
    let mono = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    (1, mono)
}

/// Converts interleaved audio to another rate, returning exactly the number of
/// frames that length should have at the new rate.
fn resample(
    samples: &[f32],
    channels: usize,
    from: u32,
    to: u32,
) -> Result<Vec<f32>, ImportError> {
    if from == to {
        return Ok(samples.to_vec());
    }
    let frames_in = samples.len() / channels;
    let expected = (frames_in as u64 * to as u64 + from as u64 / 2) / from as u64;

    let mut resampler = Fft::<f32>::new(from as usize, to as usize, 1024, channels, FixedSync::Input)
        .map_err(error)?;
    let capacity = resampler.process_all_needed_output_len(frames_in);
    let mut output = vec![0.0f32; capacity * channels];

    let input = InterleavedSlice::new(samples, channels, frames_in).map_err(error)?;
    let mut adapter = InterleavedSlice::new_mut(&mut output, channels, capacity).map_err(error)?;
    resampler
        .process_all_into_buffer(&input, &mut adapter, frames_in, None)
        .map_err(error)?;

    // The resampler rounds its length, which can leave it a frame or so off.
    output.resize(expected as usize * channels, 0.0);
    Ok(output)
}

/// Orders names the way Windows Explorer does, so `line2` comes before
/// `line10`. Import order decides clip order, and so export numbering.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let (number_a, rest_a) = split_digits(a);
            let (number_b, rest_b) = split_digits(b);
            a = rest_a;
            b = rest_b;
            // Without leading zeros, a longer number is a bigger number.
            number_a
                .len()
                .cmp(&number_b.len())
                .then_with(|| number_a.cmp(number_b))
        } else {
            a = &a[x.len_utf8()..];
            b = &b[y.len_utf8()..];
            x.to_lowercase().cmp(y.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// Splits off leading digits, dropping leading zeros from the number.
fn split_digits(text: &str) -> (&str, &str) {
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    (text[..end].trim_start_matches('0'), &text[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, rate: u32) -> Vec<f32> {
        (0..frames)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn resampling_gives_the_exact_expected_length() {
        for (frames, from, to, expected) in [
            (44100, 44100, 48000, 48000),
            (22050, 22050, 48000, 48000),
            (48000, 48000, 44100, 44100),
            (96000, 96000, 48000, 48000),
            (10, 44100, 48000, 11),
            (1, 44100, 48000, 1),
        ] {
            let out = resample(&vec![0.1; frames], 1, from, to).unwrap();
            assert_eq!(out.len(), expected, "{frames} frames, {from} to {to}");
        }
    }

    #[test]
    fn resampling_keeps_stereo_frames_whole() {
        let out = resample(&vec![0.1; 44100 * 2], 2, 44100, 48000).unwrap();
        assert_eq!(out.len(), 48000 * 2);
    }

    #[test]
    fn resampling_preserves_a_tone() {
        let out = resample(&sine(44100, 44100), 1, 44100, 48000).unwrap();
        let reference = sine(48000, 48000);
        // Skip the edges, where the filter has less audio to work with.
        for (i, (got, want)) in out.iter().zip(&reference).enumerate().skip(2000).take(44000) {
            assert!((got - want).abs() < 0.02, "frame {i}: {got} vs {want}");
        }
    }

    #[test]
    fn same_rate_is_untouched() {
        let input = sine(1000, 48000);
        assert_eq!(resample(&input, 1, 48000, 48000).unwrap(), input);
    }

    #[test]
    fn surround_averages_to_mono_and_stereo_is_kept() {
        let (channels, samples) = reduce_channels(3, vec![0.0, 0.3, 0.6, 0.3, 0.3, 0.3]);
        assert_eq!(channels, 1);
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.3).abs() < 1e-6 && (samples[1] - 0.3).abs() < 1e-6);
        assert_eq!(reduce_channels(2, vec![0.1, 0.2]), (2, vec![0.1, 0.2]));
    }

    #[test]
    fn natural_order_matches_explorer() {
        let mut names = vec!["line10", "Line2", "line1", "intro", "line02b", "line2a", "z", ""];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["", "intro", "line1", "Line2", "line2a", "line02b", "line10", "z"]);
    }

    #[test]
    fn natural_order_handles_non_ascii_names() {
        assert_eq!(natural_cmp("声1", "声2"), Ordering::Less);
        assert_eq!(natural_cmp("声10", "声2"), Ordering::Greater);
        assert_eq!(natural_cmp("é", "é"), Ordering::Equal);
    }

    /// Writes a minimal 16-bit PCM WAV for decoding tests.
    fn write_wav(path: &Path, channels: u16, rate: u32, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kantan_test_{}_{name}", std::process::id()))
    }

    #[test]
    fn imports_a_wav_with_its_channels_and_rate() {
        let path = temp_path("stereo.wav");
        write_wav(&path, 2, 48000, &[16384, -16384, 8192, -8192]);
        let source = import_file(&path, 48000).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(source.channels, 2);
        assert_eq!(source.frames(), 2);
        assert!((source.samples[0] - 0.5).abs() < 1e-3);
        assert!((source.samples[1] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn imports_and_resamples_to_the_project_rate() {
        let path = temp_path("mono.wav");
        write_wav(&path, 1, 24000, &vec![1000i16; 24000]);
        let source = import_file(&path, 48000).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(source.channels, 1);
        assert_eq!(source.frames(), 48000);
    }

    #[test]
    fn bad_files_give_an_error_instead_of_panicking() {
        let path = temp_path("garbage.wav");
        std::fs::write(&path, b"this is not audio at all").unwrap();
        let result = import_file(&path, 48000);
        std::fs::remove_file(&path).unwrap();
        assert!(result.is_err());

        assert!(import_file(Path::new("does/not/exist.wav"), 48000).is_err());
    }
}
