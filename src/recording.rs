//! Records takes from a microphone. Audio goes straight to a temp file as well
//! as memory, so a crash can't lose a take, and is mono, which suits voice work.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::Source;
use crate::import::resample;

/// The rate simulated input claims, different from the project's so that
/// headless runs also exercise resampling.
const SIMULATED_RATE: u32 = 44100;

/// Something that can be recorded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChoice {
    pub name: String,
    /// Records what's playing on an output device, rather than a microphone.
    pub system_audio: bool,
}

impl InputChoice {
    pub fn label(&self) -> String {
        if self.system_audio {
            format!("{} (system audio)", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// Microphones first, then each output device's system audio.
pub fn input_choices() -> Vec<InputChoice> {
    let host = cpal::default_host();
    let names = |devices: Result<Vec<cpal::Device>, _>| -> Vec<String> {
        let devices: Vec<cpal::Device> = devices.unwrap_or_default();
        devices
            .iter()
            .filter_map(|device: &cpal::Device| device.description().ok().map(|d| d.name().to_string()))
            .collect()
    };
    let inputs = names(host.input_devices().map(Iterator::collect).map_err(|_| ()));
    let outputs = names(host.output_devices().map(Iterator::collect).map_err(|_| ()));
    let choice = |system_audio| move |name| InputChoice { name, system_audio };
    inputs
        .into_iter()
        .map(choice(false))
        .chain(outputs.into_iter().map(choice(true)))
        .collect()
}

fn downmix(chunk: &[f32], channels: usize) -> impl Iterator<Item = f32> + '_ {
    chunk
        .chunks_exact(channels)
        .map(move |frame| frame.iter().sum::<f32>() / channels as f32)
}

fn temp_path() -> Option<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = crate::session::recordings_dir(&crate::session::data_dir()?);
    std::fs::create_dir_all(&dir).ok()?;
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    Some(dir.join(format!("take-{}-{number}.wav", std::process::id())))
}

/// Runs on its own thread until the input ends, then builds the finished take.
fn write_take(
    chunks: Receiver<Vec<f32>>,
    channels: usize,
    rate: u32,
    project_rate: u32,
    frames: Arc<AtomicU64>,
) -> Result<Source, String> {
    let path = temp_path();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    // Without a temp file the take is still kept in memory, only less crash-safe.
    let mut file = path.as_ref().and_then(|p| hound::WavWriter::create(p, spec).ok());

    let mut samples = Vec::new();
    let mut unflushed = 0;
    for chunk in chunks {
        let start = samples.len();
        samples.extend(downmix(&chunk, channels));
        if let Some(writer) = &mut file {
            for &sample in &samples[start..] {
                let _ = writer.write_sample(sample);
            }
            unflushed += samples.len() - start;
            // Flushing updates the header, leaving a readable file after a crash.
            if unflushed >= rate as usize {
                let _ = writer.flush();
                unflushed = 0;
            }
        }
        frames.store(samples.len() as u64, Ordering::Relaxed);
    }
    drop(file);

    let result = if samples.is_empty() {
        Err("the take has no audio".to_owned())
    } else {
        resample(&samples, 1, rate, project_rate)
            .map_err(|e| e.to_string())
            .and_then(|out| Source::new(1, out).map_err(|e| e.to_string()))
    };
    // A failed take with audio keeps its file, as the only copy of that audio.
    if let Some(path) = &path
        && (result.is_ok() || samples.is_empty())
    {
        let _ = std::fs::remove_file(path);
    }
    result
}

struct Active {
    /// Stops the input when dropped, which ends the writer thread.
    _stream: Option<InputStreams>,
    simulated: Option<Sender<Vec<f32>>>,
    channels: usize,
    rate: u32,
    frames: Arc<AtomicU64>,
    peak: Arc<AtomicU32>,
    lost: Arc<AtomicBool>,
}

pub struct Recorder {
    use_device: bool,
    active: Option<Active>,
    finishing: Option<Receiver<Result<Source, String>>>,
}

impl Recorder {
    /// `use_device` is false to take simulated input from `feed`.
    pub fn new(use_device: bool) -> Self {
        Self { use_device, active: None, finishing: None }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn is_finishing(&self) -> bool {
        self.finishing.is_some()
    }

    /// Whether the microphone stopped working mid-take.
    pub fn device_lost(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.lost.load(Ordering::Relaxed))
    }

    pub fn seconds(&self) -> f64 {
        self.active.as_ref().map_or(0.0, |a| {
            a.frames.load(Ordering::Relaxed) as f64 / a.rate as f64
        })
    }

    /// The peak input level since this was last called.
    pub fn take_level(&self) -> f32 {
        self.active
            .as_ref()
            .map_or(0.0, |a| f32::from_bits(a.peak.swap(0, Ordering::Relaxed)))
    }

    /// Starts a take on a chosen input, or the default microphone.
    pub fn start(&mut self, device: Option<&InputChoice>, project_rate: u32) -> Result<(), String> {
        let (sender, chunks) = mpsc::channel::<Vec<f32>>();
        let peak = Arc::new(AtomicU32::new(0));
        let lost = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));

        let (stream, simulated, channels, rate) = if self.use_device {
            let (stream, channels, rate) = open_input(device, sender, peak.clone(), lost.clone())?;
            (Some(stream), None, channels, rate)
        } else {
            (None, Some(sender), 1, SIMULATED_RATE)
        };

        let (done, finishing) = mpsc::channel();
        let writer_frames = frames.clone();
        std::thread::spawn(move || {
            let _ = done.send(write_take(chunks, channels, rate, project_rate, writer_frames));
        });
        self.finishing = Some(finishing);
        self.active = Some(Active { _stream: stream, simulated, channels, rate, frames, peak, lost });
        Ok(())
    }

    /// Ends the take. `poll_finished` then delivers it.
    pub fn stop(&mut self) {
        self.active = None;
    }

    /// The finished take, once it has been processed after stopping.
    pub fn poll_finished(&mut self) -> Option<Result<Source, String>> {
        if self.active.is_some() {
            return None;
        }
        let result = self.finishing.as_ref()?.try_recv().ok()?;
        self.finishing = None;
        Some(result)
    }

    /// Without a device, delivers audio as a microphone would.
    pub fn feed(&self, samples: Vec<f32>) {
        if let Some(active) = &self.active
            && let Some(sender) = &active.simulated
        {
            debug_assert_eq!(active.channels, 1);
            let loudest = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            store_peak(&active.peak, loudest);
            let _ = sender.send(samples);
        }
    }
}

fn store_peak(peak: &AtomicU32, level: f32) {
    if level > f32::from_bits(peak.load(Ordering::Relaxed)) {
        peak.store(level.to_bits(), Ordering::Relaxed);
    }
}

/// Dropping these stops recording.
struct InputStreams {
    _capture: cpal::Stream,
    /// Windows only delivers system audio while something plays, so this plays
    /// silence to keep real pauses in the recording.
    _keep_alive: Option<cpal::Stream>,
}

fn silent_output(device: &cpal::Device) -> Result<cpal::Stream, String> {
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unsupported device format: {:?}", supported.sample_format()));
    }
    let stream = device
        .build_output_stream(
            supported.config(),
            |output: &mut [f32], _| output.fill(0.0),
            |_| {},
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn open_input(
    choice: Option<&InputChoice>,
    sender: Sender<Vec<f32>>,
    peak: Arc<AtomicU32>,
    lost: Arc<AtomicBool>,
) -> Result<(InputStreams, usize, u32), String> {
    let host = cpal::default_host();
    let named = |mut devices: Box<dyn Iterator<Item = cpal::Device>>, name: &str| {
        devices
            .find(|d| d.description().is_ok_and(|desc| desc.name() == name))
            .ok_or_else(|| format!("'{name}' isn't available"))
    };
    let device = match choice {
        Some(InputChoice { name, system_audio: true }) => {
            named(Box::new(host.output_devices().map_err(|e| e.to_string())?), name)?
        }
        Some(InputChoice { name, system_audio: false }) => {
            named(Box::new(host.input_devices().map_err(|e| e.to_string())?), name)?
        }
        None => host.default_input_device().ok_or("no microphone found")?,
    };
    let system_audio = choice.is_some_and(|c| c.system_audio);
    let supported = if system_audio {
        device.default_output_config()
    } else {
        device.default_input_config()
    }
    .map_err(|e| e.to_string())?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unsupported input format: {:?}", supported.sample_format()));
    }
    let keep_alive = if system_audio { Some(silent_output(&device)?) } else { None };
    let config = supported.config();
    let (channels, rate) = (config.channels as usize, config.sample_rate);

    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                let loudest = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                store_peak(&peak, loudest);
                let _ = sender.send(data.to_vec());
            },
            move |_| lost.store(true, Ordering::Relaxed),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((InputStreams { _capture: stream, _keep_alive: keep_alive }, channels, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(recorder: &mut Recorder) -> Result<Source, String> {
        for _ in 0..2000 {
            if let Some(result) = recorder.poll_finished() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the take never finished");
    }

    #[test]
    fn a_take_is_resampled_to_the_project_rate() {
        let mut recorder = Recorder::new(false);
        recorder.start(None, 48000).unwrap();
        assert!(recorder.is_recording());
        recorder.feed(vec![0.5; SIMULATED_RATE as usize / 2]);
        recorder.feed(vec![0.5; SIMULATED_RATE as usize / 2]);
        assert_eq!(recorder.take_level(), 0.5);
        assert_eq!(recorder.take_level(), 0.0);
        recorder.stop();

        let source = finished(&mut recorder).unwrap();
        assert_eq!((source.channels, source.frames()), (1, 48000));
        assert!(!recorder.is_finishing());
    }

    #[test]
    fn an_empty_take_is_an_error() {
        let mut recorder = Recorder::new(false);
        recorder.start(None, 48000).unwrap();
        recorder.stop();
        assert!(finished(&mut recorder).is_err());
    }

    /// Opens system audio capture for a moment and discards everything, keeping
    /// nothing. Run by hand with `cargo test real_system_audio -- --ignored`.
    #[test]
    #[ignore = "needs a sound device"]
    fn real_system_audio_delivers_data_during_silence() {
        let choice = input_choices().into_iter().find(|c| c.system_audio).expect("an output device");
        let (sender, chunks) = mpsc::channel();
        let streams = open_input(
            Some(&choice),
            sender,
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(streams);
        let delivered: usize = chunks.iter().map(|c| c.len()).sum();
        assert!(delivered > 0, "no data from {}", choice.label());
    }

    #[test]
    fn stereo_input_becomes_mono() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2).collect::<Vec<_>>(), [0.5, 0.5]);
    }

    #[test]
    fn the_temp_file_is_readable_before_the_take_ends() {
        let (sender, chunks) = mpsc::channel();
        let path_dir = crate::session::recordings_dir(&crate::session::data_dir().unwrap());
        let frames = Arc::new(AtomicU64::new(0));
        let writer = {
            let frames = frames.clone();
            std::thread::spawn(move || write_take(chunks, 1, 1000, 1000, frames))
        };
        sender.send(vec![0.25; 2500]).unwrap();
        while frames.load(Ordering::Relaxed) < 2500 {
            std::thread::yield_now();
        }

        // Some file in the folder, with this process's id, holds the flushed audio.
        let prefix = format!("take-{}-", std::process::id());
        let readable = std::fs::read_dir(&path_dir).unwrap().flatten().any(|entry| {
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && hound::WavReader::open(entry.path()).is_ok_and(|r| r.duration() >= 2500)
        });
        assert!(readable);

        drop(sender);
        writer.join().unwrap().unwrap();
    }
}
