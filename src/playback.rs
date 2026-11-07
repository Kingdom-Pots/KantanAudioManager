//! Plays the track: a reader that produces audio, and a player that connects it
//! to the sound device, or leaves it to be advanced by hand when headless.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{Frames, Project, SegmentKind};

/// One frame of the track as left and right, after gain. Gaps are silent.
fn stereo_frame(project: &Project, frame: Frames) -> [f32; 2] {
    let clips = project.clips();
    let index = clips.partition_point(|clip| clip.start <= frame);
    let Some(clip) = index.checked_sub(1).map(|i| &clips[i]) else {
        return [0.0; 2];
    };
    let mut local = frame - clip.start;
    for (index, segment) in clip.segments.iter().enumerate() {
        if local < segment.len {
            let SegmentKind::Audio { source, offset, .. } = segment.kind else {
                return [0.0; 2];
            };
            let Some(source) = project.source(source) else {
                return [0.0; 2];
            };
            let gain = clip.gain_at(index, local, project.sample_rate());
            let width = source.channels as usize;
            let first = (offset + local) as usize * width;
            let right = first + width - 1;
            return [source.samples[first] * gain, source.samples[right] * gain];
        }
        local -= segment.len;
    }
    [0.0; 2]
}

/// Reads a span of the track at the device's sample rate. It doesn't allocate,
/// so it is safe to run on the audio thread.
struct TrackReader {
    project: Project,
    position: f64,
    end: Frames,
    /// Project frames per device frame.
    step: f64,
    shared_position: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
}

impl TrackReader {
    /// Fills interleaved output, limited to full scale, and returns its peak.
    fn fill(&mut self, output: &mut [f32], channels: usize) -> f32 {
        let mut peak = 0.0f32;
        for out in output.chunks_exact_mut(channels) {
            if self.position >= self.end as f64 {
                out.fill(0.0);
                self.finished.store(true, Ordering::Relaxed);
                continue;
            }
            let frame = self.position as Frames;
            let [mut left, mut right] = stereo_frame(&self.project, frame);
            let fraction = (self.position - frame as f64) as f32;
            if fraction > 0.0 {
                // Good enough for previewing; exports use the proper resampler.
                let [next_left, next_right] = stereo_frame(&self.project, frame + 1);
                left += (next_left - left) * fraction;
                right += (next_right - right) * fraction;
            }
            let (left, right) = (left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0));
            peak = peak.max(left.abs()).max(right.abs());
            match out {
                [mono] => *mono = (left + right) / 2.0,
                [first, second, rest @ ..] => {
                    *first = left;
                    *second = right;
                    rest.fill(0.0);
                }
                [] => {}
            }
            self.position += self.step;
        }
        let position = (self.position as Frames).min(self.end);
        self.shared_position.store(position, Ordering::Relaxed);
        peak
    }
}

struct Active {
    position: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
    /// Stops the audio when dropped.
    _stream: Option<cpal::Stream>,
    manual: Option<TrackReader>,
}

/// Starts and stops playback.
pub struct Player {
    use_device: bool,
    active: Option<Active>,
}

impl Player {
    /// `use_device` is false to run without sound, advancing by hand.
    pub fn new(use_device: bool) -> Self {
        Self { use_device, active: None }
    }

    pub fn is_playing(&self) -> bool {
        self.active.is_some()
    }

    pub fn position(&self) -> Option<Frames> {
        self.active.as_ref().map(|a| a.position.load(Ordering::Relaxed))
    }

    pub fn has_finished(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.finished.load(Ordering::Relaxed))
    }

    /// Plays a snapshot of the project from `from` until `to`.
    pub fn start(&mut self, project: &Project, from: Frames, to: Frames) -> Result<(), String> {
        self.active = None;
        let position = Arc::new(AtomicU64::new(from));
        let finished = Arc::new(AtomicBool::new(false));
        let reader = TrackReader {
            project: project.clone(),
            position: from as f64,
            end: to,
            step: 1.0,
            shared_position: position.clone(),
            finished: finished.clone(),
        };
        let (stream, manual) = if self.use_device {
            (Some(open_stream(reader, project.sample_rate())?), None)
        } else {
            (None, Some(reader))
        };
        self.active = Some(Active { position, finished, _stream: stream, manual });
        Ok(())
    }

    /// Stops and returns where it had reached.
    pub fn stop(&mut self) -> Option<Frames> {
        let position = self.position();
        self.active = None;
        position
    }

    /// Without a device, pulls audio as a device would, returning its peak.
    pub fn advance(&mut self, frames: usize) -> f32 {
        let Some(reader) = self.active.as_mut().and_then(|a| a.manual.as_mut()) else {
            return 0.0;
        };
        let mut buffer = vec![0.0; frames * 2];
        reader.fill(&mut buffer, 2)
    }
}

/// Plays on the default device. The audio callback owns the reader.
fn open_stream(mut reader: TrackReader, project_rate: u32) -> Result<cpal::Stream, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no sound output device found")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let channels = config.channels as usize;
    reader.step = project_rate as f64 / config.sample_rate as f64;

    // If the device goes away, end playback instead of appearing stuck.
    let finished = reader.finished.clone();
    let on_error = move |_| finished.store(true, Ordering::Relaxed);

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |output: &mut [f32], _| {
                reader.fill(output, channels);
            },
            on_error,
            None,
        ),
        format => return Err(format!("unsupported sound device format: {format:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimeRange;

    /// 100 Hz project: clip "a" at 0..4, gap, clip "b" at 54..56.
    fn project() -> Project {
        let mut project = Project::new(100);
        let a = project.add_source(1, vec![0.1, 0.2, 0.3, 0.4]).unwrap();
        project.append_clip("a", a).unwrap();
        let b = project.add_source(2, vec![0.5, -0.5, 0.6, -0.6]).unwrap();
        project.append_clip("b", b).unwrap();
        project
    }

    fn reader(project: &Project, from: Frames, to: Frames, step: f64) -> TrackReader {
        TrackReader {
            project: project.clone(),
            position: from as f64,
            end: to,
            step,
            shared_position: Arc::new(AtomicU64::new(from)),
            finished: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn frames_come_from_clips_and_gaps_are_silent() {
        let p = project();
        assert_eq!(stereo_frame(&p, 1), [0.2, 0.2]);
        assert_eq!(stereo_frame(&p, 4), [0.0, 0.0]);
        assert_eq!(stereo_frame(&p, 55), [0.6, -0.6]);
        assert_eq!(stereo_frame(&p, 56), [0.0, 0.0]);
    }

    #[test]
    fn gain_applies_and_output_is_limited() {
        let mut p = project();
        p.amplify(TimeRange::new(0, 1), 80.0).unwrap();
        let mut r = reader(&p, 0, 2, 1.0);
        let mut out = [0.0; 4];
        assert_eq!(r.fill(&mut out, 2), 1.0);
        assert_eq!(out, [1.0, 1.0, 0.2, 0.2]);
    }

    #[test]
    fn stops_at_the_end_and_reports_position() {
        let p = project();
        let mut r = reader(&p, 2, 4, 1.0);
        let mut out = [9.0; 8];
        r.fill(&mut out, 2);
        assert_eq!(out, [0.3, 0.3, 0.4, 0.4, 0.0, 0.0, 0.0, 0.0]);
        assert!(r.finished.load(Ordering::Relaxed));
        assert_eq!(r.shared_position.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn device_channel_counts_are_handled() {
        let p = project();
        let mut mono = [0.0; 1];
        reader(&p, 55, 56, 1.0).fill(&mut mono, 1);
        assert_eq!(mono, [0.0]); // 0.6 and -0.6 average out.

        let mut surround = [9.0; 4];
        reader(&p, 55, 56, 1.0).fill(&mut surround, 4);
        assert_eq!(surround, [0.6, -0.6, 0.0, 0.0]);
    }

    #[test]
    fn slower_devices_interpolate_and_keep_duration() {
        let p = project();
        let mut r = reader(&p, 0, 2, 0.5);
        let mut out = [0.0; 4];
        r.fill(&mut out, 1);
        let expected = [0.1, 0.15, 0.2, 0.25];
        for (got, want) in out.iter().zip(expected) {
            assert!((got - want).abs() < 1e-5, "{out:?}");
        }
        assert!(!r.finished.load(Ordering::Relaxed));
    }

    #[test]
    fn manual_player_advances_and_finishes() {
        let p = project();
        let mut player = Player::new(false);
        player.start(&p, 0, 4).unwrap();
        assert!(player.advance(2) > 0.0);
        assert_eq!(player.position(), Some(2));
        assert!(!player.has_finished());
        player.advance(5);
        assert!(player.has_finished());
        assert_eq!(player.stop(), Some(4));
        assert!(!player.is_playing());
    }

    /// Opens the real sound device, playing only silence. Run by hand with
    /// `cargo test real_device -- --ignored`.
    #[test]
    #[ignore = "needs a sound device"]
    fn real_device_plays_silence_and_advances() {
        let mut project = Project::new(48000);
        let source = project.add_source(1, vec![0.0; 48000]).unwrap();
        project.append_clip("silence", source).unwrap();
        let mut player = Player::new(true);
        player.start(&project, 0, 24000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1000));
        assert!(player.has_finished(), "position {:?}", player.position());
        assert_eq!(player.stop(), Some(24000));
    }
}
