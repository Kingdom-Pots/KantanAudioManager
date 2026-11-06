//! Precomputed minimum and maximum levels, so a waveform can be drawn at any
//! zoom without reading every sample.

use crate::project::{Clip, Frames, Project, SegmentKind};

/// Frames covered by one value in the finest level.
const BASE_BUCKET: usize = 64;
/// How many times coarser each level is than the one before it.
const LEVEL_FACTOR: usize = 4;

/// Levels of `(min, max)` pairs, finest first, across all channels.
#[derive(Debug, PartialEq)]
pub(crate) struct Peaks {
    levels: Vec<Vec<(f32, f32)>>,
}

impl Peaks {
    pub(crate) fn build(samples: &[f32], channels: usize) -> Self {
        let base: Vec<_> = samples
            .chunks(BASE_BUCKET * channels)
            .map(min_max)
            .collect();

        let mut levels = vec![base];
        while levels[levels.len() - 1].len() > 1 {
            let coarser = levels[levels.len() - 1]
                .chunks(LEVEL_FACTOR)
                .map(|group| {
                    group.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| {
                        (lo.min(a), hi.max(b))
                    })
                })
                .collect();
            levels.push(coarser);
        }
        Self { levels }
    }

    /// The levels within a frame range. The result always contains the true
    /// values and may reach a quarter of the range beyond it. That is far less
    /// than one pixel when drawing, and is what keeps zoomed-out drawing fast.
    pub(crate) fn min_max(&self, samples: &[f32], channels: usize, start: usize, len: usize) -> (f32, f32) {
        let frames = samples.len() / channels;
        let start = start.min(frames);
        let end = (start + len).min(frames);
        if start == end {
            return (0.0, 0.0);
        }
        let len = end - start;

        // Pick the coarsest level with at least four values across the range.
        let mut level = None;
        let mut bucket = BASE_BUCKET;
        for candidate in 0..self.levels.len() {
            if bucket * LEVEL_FACTOR > len {
                break;
            }
            level = Some((candidate, bucket));
            bucket *= LEVEL_FACTOR;
        }

        match level {
            Some((level, bucket)) => {
                let first = start / bucket;
                let last = (end - 1) / bucket;
                self.levels[level][first..=last]
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| (lo.min(a), hi.max(b)))
            }
            None => min_max(&samples[start * channels..end * channels]),
        }
    }
}

fn min_max(values: &[f32]) -> (f32, f32) {
    values
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
}

/// The levels of a clip over `len` frames from `start` (relative to the clip),
/// after gain. Returns zeros where there is only silence or nothing at all.
pub fn clip_min_max(project: &Project, clip: &Clip, start: Frames, len: Frames) -> (f32, f32) {
    let end = start + len;
    let mut result: Option<(f32, f32)> = None;
    let mut position = 0;

    for segment in &clip.segments {
        let segment_end = position + segment.len;
        let from = start.max(position);
        let to = end.min(segment_end);
        if from < to {
            let (lo, hi) = match segment.kind {
                SegmentKind::Silence => (0.0, 0.0),
                SegmentKind::Audio { source, offset, gain } => {
                    let source = project
                        .source(source)
                        .expect("sources are never removed from a project");
                    let (lo, hi) = source.min_max(offset + (from - position), to - from);
                    (lo * gain, hi * gain)
                }
            };
            // A negative gain would swap which value is lower.
            let (lo, hi) = (lo.min(hi), lo.max(hi));
            result = Some(result.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
        position = segment_end;
        if position >= end {
            break;
        }
    }
    result.unwrap_or((0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Source, TimeRange};

    fn noise(frames: usize) -> Vec<f32> {
        let mut state = 0x2545_F491u32;
        (0..frames)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn peaks_contain_the_true_range_and_stay_close_to_it() {
        let samples = noise(500_000);
        let source = Source::new(1, samples.clone()).unwrap();

        for (start, len) in [
            (0, 1),
            (10, 63),
            (100, 64),
            (1000, 300),
            (1234, 5000),
            (40_000, 100_000),
            (0, 500_000),
        ] {
            let (lo, hi) = source.min_max(start as u64, len as u64);
            let (true_lo, true_hi) = min_max(&samples[start..start + len]);
            assert!(lo <= true_lo && hi >= true_hi, "range {start}+{len} misses audio");

            // Compare against the widest area the quarter-range slack allows.
            let wide_start = start.saturating_sub(len / 2);
            let wide_end = (start + len + len / 2).min(samples.len());
            let (wide_lo, wide_hi) = min_max(&samples[wide_start..wide_end]);
            assert!(lo >= wide_lo && hi <= wide_hi, "range {start}+{len} too wide");
        }
    }

    #[test]
    fn short_ranges_are_exact() {
        let samples = noise(1000);
        let source = Source::new(1, samples.clone()).unwrap();
        assert_eq!(source.min_max(100, 50), min_max(&samples[100..150]));
    }

    #[test]
    fn a_single_loud_sample_is_never_lost() {
        let mut samples = vec![0.0; 1_000_000];
        samples[777_777] = 0.9;
        let source = Source::new(1, samples).unwrap();
        assert_eq!(source.min_max(0, 1_000_000).1, 0.9);
        assert_eq!(source.min_max(777_000, 1000).1, 0.9);
        assert_eq!(source.min_max(0, 100_000).1, 0.0);
    }

    #[test]
    fn stereo_peaks_cover_both_channels() {
        let samples = vec![0.1, -0.5, 0.2, 0.7];
        let source = Source::new(2, samples).unwrap();
        assert_eq!(source.min_max(0, 2), (-0.5, 0.7));
        assert_eq!(source.min_max(1, 1), (0.2, 0.7));
    }

    #[test]
    fn ranges_past_the_end_are_safe() {
        let source = Source::new(1, vec![0.5; 100]).unwrap();
        assert_eq!(source.min_max(90, 1000), (0.5, 0.5));
        assert_eq!(source.min_max(500, 10), (0.0, 0.0));
    }

    #[test]
    fn clip_levels_include_gain_and_silence() {
        let mut p = Project::new(100);
        let source = p.add_source(1, vec![0.25; 100]).unwrap();
        let a = p.append_clip("a", source).unwrap();
        let source = p.add_source(1, vec![0.25; 100]).unwrap();
        let b = p.append_clip("b", source).unwrap();
        p.amplify(TimeRange::new(0, 10), 20.0).unwrap();
        p.merge_clips(&[a, b]).unwrap();
        let clip = p.clip(a).unwrap();

        // Amplified part.
        let (lo, hi) = clip_min_max(&p, clip, 0, 10);
        assert!((lo - 2.5).abs() < 1e-3 && (hi - 2.5).abs() < 1e-3);
        // Unchanged part.
        assert_eq!(clip_min_max(&p, clip, 20, 10), (0.25, 0.25));
        // The 50 frame gap between the merged clips.
        assert_eq!(clip_min_max(&p, clip, 110, 20), (0.0, 0.0));
        // Spanning audio and silence.
        assert_eq!(clip_min_max(&p, clip, 90, 20), (0.0, 0.25));
        // Past the end of the clip.
        assert_eq!(clip_min_max(&p, clip, 10_000, 10), (0.0, 0.0));
    }
}
