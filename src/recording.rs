//! Records takes from a microphone. Audio goes straight to a temp file as well
//! as memory, so a crash can't lose a take, and is mono, which suits voice work.
// The browser version records through web_mic instead of the desktop audio devices.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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
    // A browser can only offer the microphone it asks permission for.
    if cfg!(target_arch = "wasm32") {
        return Vec::new();
    }
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

/// The live preview keeps one level per 10 ms.
fn live_frames_per_peak(rate: u32) -> usize {
    (rate as usize / 100).max(1)
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

/// Collects a take as audio arrives: in memory, in a temp file where there is
/// one, and as levels for the live preview.
struct TakeWriter {
    channels: usize,
    rate: u32,
    samples: Vec<f32>,
    per_peak: usize,
    peaked: usize,
    path: Option<PathBuf>,
    file: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    unflushed: usize,
    frames: Arc<AtomicU64>,
    live: Arc<Mutex<Vec<(f32, f32)>>>,
}

impl TakeWriter {
    fn new(channels: usize, rate: u32, frames: Arc<AtomicU64>, live: Arc<Mutex<Vec<(f32, f32)>>>) -> Self {
        let path = temp_path();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        // Without a temp file the take is still kept in memory, only less crash-safe.
        let file = path.as_ref().and_then(|p| hound::WavWriter::create(p, spec).ok());
        Self {
            channels,
            rate,
            samples: Vec::new(),
            per_peak: live_frames_per_peak(rate),
            peaked: 0,
            path,
            file,
            unflushed: 0,
            frames,
            live,
        }
    }

    fn push(&mut self, chunk: &[f32]) {
        let start = self.samples.len();
        self.samples.extend(downmix(chunk, self.channels));
        if let Some(writer) = &mut self.file {
            for &sample in &self.samples[start..] {
                let _ = writer.write_sample(sample);
            }
            self.unflushed += self.samples.len() - start;
            // Flushing updates the header, leaving a readable file after a crash.
            if self.unflushed >= self.rate as usize {
                let _ = writer.flush();
                self.unflushed = 0;
            }
        }
        // Levels for the live preview, in whole steps so old ones never change.
        if self.samples.len() >= self.peaked + self.per_peak {
            let mut levels = self.live.lock().unwrap_or_else(|e| e.into_inner());
            while self.samples.len() >= self.peaked + self.per_peak {
                let step = &self.samples[self.peaked..self.peaked + self.per_peak];
                levels.push(step.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s))));
                self.peaked += self.per_peak;
            }
        }
        self.frames.store(self.samples.len() as u64, Ordering::Relaxed);
    }

    /// Builds the finished take at the project's rate.
    fn finish(mut self, project_rate: u32) -> Result<Source, String> {
        drop(self.file.take());
        let result = if self.samples.is_empty() {
            Err("the take has no audio".to_owned())
        } else {
            resample(&self.samples, 1, self.rate, project_rate)
                .map_err(|e| e.to_string())
                .and_then(|out| Source::new(1, out).map_err(|e| e.to_string()))
        };
        // A failed take with audio keeps its file, as the only copy of that audio.
        if let Some(path) = &self.path
            && (result.is_ok() || self.samples.is_empty())
        {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

/// Runs on its own thread until the input ends, then builds the finished take.
fn write_take(
    chunks: Receiver<Vec<f32>>,
    channels: usize,
    rate: u32,
    project_rate: u32,
    frames: Arc<AtomicU64>,
    live: Arc<Mutex<Vec<(f32, f32)>>>,
) -> Result<Source, String> {
    let mut writer = TakeWriter::new(channels, rate, frames, live);
    for chunk in chunks {
        writer.push(&chunk);
    }
    writer.finish(project_rate)
}

/// What keeps the input running; dropping it stops recording.
#[cfg(not(target_arch = "wasm32"))]
type Capture = InputStreams;
#[cfg(target_arch = "wasm32")]
type Capture = web_mic::WebInput;

struct Active {
    /// Stops the input when dropped, which ends the writer thread.
    _stream: Option<Capture>,
    /// In the browser there's no writer thread, so the take is written here,
    /// a frame at a time.
    #[cfg(target_arch = "wasm32")]
    inline_writer: Option<(Receiver<Vec<f32>>, TakeWriter, u32)>,
    simulated: Option<Sender<Vec<f32>>>,
    channels: usize,
    rate: u32,
    frames: Arc<AtomicU64>,
    peak: Arc<AtomicU32>,
    lost: Arc<Mutex<Option<String>>>,
    live: Arc<Mutex<Vec<(f32, f32)>>>,
}

pub struct Recorder {
    use_device: bool,
    active: Option<Active>,
    finishing: Option<Receiver<Result<Source, String>>>,
    /// Where the browser version delivers its finished take.
    #[cfg(target_arch = "wasm32")]
    inline_done: Option<Sender<Result<Source, String>>>,
}

impl Recorder {
    /// `use_device` is false to take simulated input from `feed`.
    pub fn new(use_device: bool) -> Self {
        Self {
            use_device,
            active: None,
            finishing: None,
            #[cfg(target_arch = "wasm32")]
            inline_done: None,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn is_finishing(&self) -> bool {
        self.finishing.is_some()
    }

    /// Why the input stopped working mid-take, if it did.
    pub fn device_lost(&self) -> Option<String> {
        let active = self.active.as_ref()?;
        active.lost.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn seconds(&self) -> f64 {
        self.active.as_ref().map_or(0.0, |a| {
            a.frames.load(Ordering::Relaxed) as f64 / a.rate as f64
        })
    }

    /// Passes the take's levels so far to `read`, one (min, max) per 10 ms.
    pub fn with_live_levels<T>(&self, read: impl FnOnce(&[(f32, f32)]) -> T) -> Option<T> {
        let active = self.active.as_ref()?;
        let levels = active.live.lock().unwrap_or_else(|e| e.into_inner());
        Some(read(&levels))
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
        let lost = Arc::new(Mutex::new(None));
        let frames = Arc::new(AtomicU64::new(0));

        let (stream, simulated, channels, rate) = if self.use_device {
            #[cfg(not(target_arch = "wasm32"))]
            let (stream, channels, rate) = open_input(device, sender, peak.clone(), lost.clone())?;
            #[cfg(target_arch = "wasm32")]
            let (stream, channels, rate) = {
                let _ = device;
                web_mic::open(sender, peak.clone(), lost.clone())?
            };
            (Some(stream), None, channels, rate)
        } else {
            (None, Some(sender), 1, SIMULATED_RATE)
        };

        let (done, finishing) = mpsc::channel();
        let writer_frames = frames.clone();
        let live = Arc::new(Mutex::new(Vec::new()));
        let writer_live = live.clone();
        #[cfg(not(target_arch = "wasm32"))]
        crate::run_in_background(move || {
            let _ = done.send(write_take(chunks, channels, rate, project_rate, writer_frames, writer_live));
        });
        #[cfg(target_arch = "wasm32")]
        let inline_writer = {
            self.inline_done = Some(done);
            Some((chunks, TakeWriter::new(channels, rate, writer_frames, writer_live), project_rate))
        };
        self.finishing = Some(finishing);
        self.active = Some(Active {
            _stream: stream,
            #[cfg(target_arch = "wasm32")]
            inline_writer,
            simulated,
            channels,
            rate,
            frames,
            peak,
            lost,
            live,
        });
        Ok(())
    }

    /// In the browser, writes the audio that arrived since the last frame.
    /// On the desktop a thread does this, so it does nothing.
    pub fn pump(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(Active { inline_writer: Some((chunks, writer, _)), .. }) = &mut self.active {
            while let Ok(chunk) = chunks.try_recv() {
                writer.push(&chunk);
            }
        }
    }

    /// Ends the take. `poll_finished` then delivers it.
    pub fn stop(&mut self) {
        self.pump();
        #[cfg(target_arch = "wasm32")]
        if let Some(Active { inline_writer: Some((_, writer, project_rate)), .. }) = self.active.take()
            && let Some(done) = self.inline_done.take()
        {
            let _ = done.send(writer.finish(project_rate));
        }
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

/// The browser's microphone, through `getUserMedia`.
#[cfg(target_arch = "wasm32")]
mod web_mic {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};

    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    /// Dropping this releases the microphone.
    pub struct WebInput {
        context: web_sys::AudioContext,
        stream: Rc<RefCell<Option<web_sys::MediaStream>>>,
        _on_audio: Closure<dyn FnMut(web_sys::AudioProcessingEvent)>,
    }

    impl Drop for WebInput {
        fn drop(&mut self) {
            if let Some(stream) = self.stream.borrow_mut().take() {
                for track in stream.get_tracks().iter() {
                    if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
                        track.stop();
                    }
                }
            }
            let _ = self.context.close();
        }
    }

    /// Starts listening. The browser asks permission first, so audio starts a
    /// moment later; a refusal arrives through `lost`.
    pub fn open(
        sender: Sender<Vec<f32>>,
        peak: Arc<AtomicU32>,
        lost: Arc<Mutex<Option<String>>>,
    ) -> Result<(WebInput, usize, u32), String> {
        let error = |e: wasm_bindgen::JsValue| format!("{e:?}");
        let context = web_sys::AudioContext::new().map_err(error)?;
        let rate = context.sample_rate() as u32;
        let processor = context
            .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(4096, 1, 1)
            .map_err(error)?;
        let on_audio = Closure::<dyn FnMut(web_sys::AudioProcessingEvent)>::new(move |event: web_sys::AudioProcessingEvent| {
            if let Ok(buffer) = event.input_buffer()
                && let Ok(data) = buffer.get_channel_data(0)
            {
                let loudest = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                super::store_peak(&peak, loudest);
                let _ = sender.send(data);
            }
        });
        processor.set_onaudioprocess(Some(on_audio.as_ref().unchecked_ref()));
        // A silent path to the speakers, because browsers only run a processor that leads somewhere.
        let mute = context.create_gain().map_err(error)?;
        mute.gain().set_value(0.0);
        processor.connect_with_audio_node(&mute).map_err(error)?;
        mute.connect_with_audio_node(&context.destination()).map_err(error)?;

        let stream = Rc::new(RefCell::new(None));
        let navigator = web_sys::window().ok_or("no browser window")?.navigator();
        let devices = navigator.media_devices().map_err(|_| "this browser can't record".to_owned())?;
        let constraints = web_sys::MediaStreamConstraints::new();
        constraints.set_audio(&wasm_bindgen::JsValue::TRUE);
        let request = devices.get_user_media_with_constraints(&constraints).map_err(error)?;
        let (held, connect_context) = (stream.clone(), context.clone());
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(request).await {
                Ok(value) => {
                    let media: web_sys::MediaStream = value.unchecked_into();
                    match connect_context.create_media_stream_source(&media) {
                        Ok(source) => {
                            let _ = source.connect_with_audio_node(&processor);
                        }
                        Err(e) => *lost.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{e:?}")),
                    }
                    *held.borrow_mut() = Some(media);
                }
                Err(_) => {
                    *lost.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some("the browser didn't allow microphone access".to_owned());
                }
            }
        });
        Ok((WebInput { context, stream, _on_audio: on_audio }, 1, rate))
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
    lost: Arc<Mutex<Option<String>>>,
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
            move |error: cpal::Error| {
                // Glitches and reroutes leave the stream running, so a take goes on.
                if crate::playback::is_fatal(&error) {
                    *lost.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
                }
            },
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
    fn live_levels_grow_during_the_take_and_show_its_shape() {
        let mut recorder = Recorder::new(false);
        recorder.start(None, 48000).unwrap();
        recorder.feed(vec![0.5; SIMULATED_RATE as usize / 10]);
        recorder.feed(vec![-0.25; SIMULATED_RATE as usize / 10]);
        let expected = 20;
        for _ in 0..2000 {
            if recorder.with_live_levels(|l| l.len()) == Some(expected) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (first, last) = recorder.with_live_levels(|l| (l[0], l[l.len() - 1])).unwrap();
        assert_eq!(first, (0.5, 0.5));
        assert_eq!(last, (-0.25, -0.25));
        recorder.stop();
        assert_eq!(recorder.with_live_levels(|l| l.len()), None);
        finished(&mut recorder).unwrap();
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
        for choice in input_choices().into_iter().filter(|c| c.system_audio) {
            let (sender, chunks) = mpsc::channel();
            let lost = Arc::new(Mutex::new(None));
            let peak = Arc::new(AtomicU32::new(0));
            let opened = open_input(Some(&choice), sender, peak.clone(), lost.clone());
            let Ok(streams) = opened.map_err(|e| println!("{}: can't open: {e}", choice.label())) else {
                continue;
            };
            std::thread::sleep(std::time::Duration::from_millis(1000));
            drop(streams);
            let delivered: usize = chunks.iter().map(|c| c.len()).sum();
            let level = f32::from_bits(peak.load(Ordering::Relaxed));
            println!("{}: {delivered} samples, peak {level}, error {:?}", choice.label(), lost.lock().unwrap());
            assert!(delivered > 0, "no data from {}", choice.label());
            assert_eq!(*lost.lock().unwrap(), None, "{}", choice.label());
        }
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
            std::thread::spawn(move || write_take(chunks, 1, 1000, 1000, frames, Arc::default()))
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
