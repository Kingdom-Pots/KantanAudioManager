use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::peaks::Peaks;

/// A count of, or position in, audio frames at the project sample rate.
pub type Frames = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClipId(pub(crate) u32);

/// Converts a decibel change to a linear gain factor.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    ClipNotFound(ClipId),
    EmptyName,
    InvalidSource(&'static str),
    InvalidGain,
    /// The requested position is not strictly inside any clip.
    NotInsideClip(Frames),
    /// Merging needs at least two clips with no other clip between them.
    ClipsNotAdjacent,
    /// The selection is empty or does not touch any clip.
    NothingSelected,
    /// The edit would leave the project exactly as it is.
    NoChange,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClipNotFound(id) => write!(f, "clip {} no longer exists", id.0),
            Self::EmptyName => f.write_str("clip name cannot be empty"),
            Self::InvalidSource(why) => write!(f, "invalid audio: {why}"),
            Self::InvalidGain => f.write_str("gain must be a finite number"),
            Self::NotInsideClip(at) => write!(f, "position {at} is not inside a clip"),
            Self::ClipsNotAdjacent => {
                f.write_str("select two or more clips with no other clip between them")
            }
            Self::NothingSelected => f.write_str("nothing is selected"),
            Self::NoChange => f.write_str("nothing to change"),
        }
    }
}

impl std::error::Error for EditError {}

/// Immutable audio at the project sample rate, interleaved. Mono or stereo.
#[derive(Debug, PartialEq)]
pub struct Source {
    pub channels: u16,
    pub samples: Vec<f32>,
    peaks: Peaks,
}

impl Source {
    /// Validates the audio and builds its waveform data.
    pub fn new(channels: u16, samples: Vec<f32>) -> Result<Self, EditError> {
        if channels != 1 && channels != 2 {
            return Err(EditError::InvalidSource("only mono and stereo are supported"));
        }
        if samples.is_empty() {
            return Err(EditError::InvalidSource("no audio"));
        }
        if !samples.len().is_multiple_of(channels as usize) {
            return Err(EditError::InvalidSource("incomplete final frame"));
        }
        let peaks = Peaks::build(&samples, channels as usize);
        Ok(Self { channels, samples, peaks })
    }

    pub fn frames(&self) -> Frames {
        self.samples.len() as Frames / self.channels as Frames
    }

    /// The lowest and highest levels in a frame range, across all channels.
    pub fn min_max(&self, start: Frames, len: Frames) -> (f32, f32) {
        self.peaks
            .min_max(&self.samples, self.channels as usize, start as usize, len as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SegmentKind {
    Audio {
        source: SourceId,
        /// First frame used within the source.
        offset: Frames,
        /// Linear gain applied when rendering.
        gain: f32,
    },
    /// Fills the gap left between clips when they are merged.
    Silence,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub len: Frames,
}

impl Segment {
    /// Splits at `at` frames in. `at` must be strictly inside the segment.
    fn split(self, at: Frames) -> (Segment, Segment) {
        debug_assert!(at > 0 && at < self.len);
        let right_kind = match self.kind {
            SegmentKind::Audio { source, offset, gain } => SegmentKind::Audio {
                source,
                offset: offset + at,
                gain,
            },
            SegmentKind::Silence => SegmentKind::Silence,
        };
        (
            Segment { kind: self.kind, len: at },
            Segment { kind: right_kind, len: self.len - at },
        )
    }
}

/// Splits a segment list `at` frames from its start. Either side may be empty.
fn split_segments(segments: &[Segment], at: Frames) -> (Vec<Segment>, Vec<Segment>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut pos = 0;
    for &segment in segments {
        let end = pos + segment.len;
        if end <= at {
            left.push(segment);
        } else if pos >= at {
            right.push(segment);
        } else {
            let (a, b) = segment.split(at - pos);
            left.push(a);
            right.push(b);
        }
        pos = end;
    }
    (left, right)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    /// Also the base of the filename this clip exports to.
    pub name: String,
    /// Position of the first frame on the timeline.
    pub start: Frames,
    pub segments: Vec<Segment>,
}

impl Clip {
    pub fn len(&self) -> Frames {
        self.segments.iter().map(|s| s.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn end(&self) -> Frames {
        self.start + self.len()
    }
}

impl Clip {
    /// The gain for a frame `local` frames into segment `index`. A segment
    /// following audio at another gain eases in over 5 ms, so that amplifying
    /// part of a clip can't cause an audible click at the edge.
    pub fn gain_at(&self, index: usize, local: Frames, sample_rate: u32) -> f32 {
        let SegmentKind::Audio { gain, .. } = self.segments[index].kind else {
            return 0.0;
        };
        let ramp = sample_rate as Frames / 200;
        let previous = index.checked_sub(1).map(|i| self.segments[i].kind);
        if let Some(SegmentKind::Audio { gain: before, .. }) = previous
            && before != gain
            && local < ramp
            && self.segments[index].len >= ramp
        {
            let t = (local + 1) as f32 / (ramp + 1) as f32;
            return before + (gain - before) * t;
        }
        gain
    }
}

/// What counts as a pause between lines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceSettings {
    /// Anything quieter than this is silence.
    pub threshold_db: f32,
    /// A quiet stretch must last this long to count as a pause.
    pub min_silence_seconds: f32,
}

impl Default for SilenceSettings {
    fn default() -> Self {
        Self { threshold_db: -40.0, min_silence_seconds: 0.5 }
    }
}

/// Kept either side of each line, so soft starts and endings aren't cut off.
const SILENCE_PADDING_SECONDS: f32 = 0.05;
/// Sounds shorter than this are treated as clicks rather than lines.
const MIN_SOUND_SECONDS: f32 = 0.03;

/// The parts of some interleaved audio that are not silence, in frames.
fn sound_regions(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    settings: SilenceSettings,
) -> Vec<(Frames, Frames)> {
    let frames = samples.len() / channels;
    let threshold = db_to_gain(settings.threshold_db);
    let to_frames = |seconds: f32| (seconds * sample_rate as f32) as usize;
    let min_silence = to_frames(settings.min_silence_seconds).max(1);

    // Runs of sound, joined across any quiet stretch too short to be a pause.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, frame) in samples.chunks_exact(channels).enumerate() {
        if frame.iter().any(|s| s.abs() >= threshold) {
            match runs.last_mut() {
                Some((_, end)) if index - *end < min_silence => *end = index + 1,
                _ => runs.push((index, index + 1)),
            }
        }
    }

    let padding = to_frames(SILENCE_PADDING_SECONDS);
    let min_sound = to_frames(MIN_SOUND_SECONDS);
    runs.into_iter()
        .filter(|(start, end)| end - start >= min_sound)
        .map(|(start, end)| {
            (start.saturating_sub(padding) as Frames, (end + padding).min(frames) as Frames)
        })
        .collect()
}

/// A half-open span of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Frames,
    pub end: Frames,
}

impl TimeRange {
    /// Accepts the ends in either order, as a drag selection can run backwards.
    pub fn new(a: Frames, b: Frames) -> Self {
        Self { start: a.min(b), end: a.max(b) }
    }
}

/// The single track. Clips are always sorted by position and never overlap.
///
/// Cloning is cheap because the audio itself is shared.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    sample_rate: u32,
    sources: BTreeMap<SourceId, Arc<Source>>,
    clips: Vec<Clip>,
    next_source: u32,
    next_clip: u32,
}

impl Project {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            sources: BTreeMap::new(),
            clips: Vec::new(),
            next_source: 0,
            next_clip: 0,
        }
    }

    pub(crate) fn next_ids(&self) -> (u32, u32) {
        (self.next_source, self.next_clip)
    }

    /// Rebuilds a project from a file, whose contents have been checked.
    pub(crate) fn from_parts(
        sample_rate: u32,
        sources: BTreeMap<SourceId, Arc<Source>>,
        clips: Vec<Clip>,
        next_source: u32,
        next_clip: u32,
    ) -> Self {
        Self { sample_rate, sources, clips, next_source, next_clip }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// All clips in timeline order.
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn source(&self, id: SourceId) -> Option<&Arc<Source>> {
        self.sources.get(&id)
    }

    /// Silence left between a new take and the clip before it: half a second.
    pub fn take_gap(&self) -> Frames {
        self.sample_rate as Frames / 2
    }

    /// Stores audio that is already at the project sample rate.
    pub fn add_source(&mut self, channels: u16, samples: Vec<f32>) -> Result<SourceId, EditError> {
        Ok(self.insert_source(Source::new(channels, samples)?))
    }

    /// Stores a source that has already been built, such as on a worker thread.
    pub fn insert_source(&mut self, source: Source) -> SourceId {
        let id = SourceId(self.next_source);
        self.next_source += 1;
        self.sources.insert(id, Arc::new(source));
        id
    }

    /// The first `Recording N` name that no clip uses.
    pub fn next_recording_name(&self) -> String {
        (1..)
            .map(|n| format!("Recording {n}"))
            .find(|name| self.clips.iter().all(|c| c.name != *name))
            .expect("an unused number always exists")
    }

    /// Adds a whole source as a new clip after the last one.
    pub fn append_clip(&mut self, name: &str, source: SourceId) -> Result<ClipId, EditError> {
        let name = clean_name(name)?;
        let frames = self
            .source(source)
            .ok_or(EditError::InvalidSource("unknown source"))?
            .frames();
        let start = self.clips.last().map_or(0, |c| c.end() + self.take_gap());
        let id = self.new_clip_id();
        self.clips.push(Clip {
            id,
            name,
            start,
            segments: vec![Segment {
                kind: SegmentKind::Audio { source, offset: 0, gain: 1.0 },
                len: frames,
            }],
        });
        Ok(id)
    }

    pub fn rename_clip(&mut self, id: ClipId, name: &str) -> Result<(), EditError> {
        let name = clean_name(name)?;
        let index = self.index_of(id)?;
        if self.clips[index].name == name {
            return Err(EditError::NoChange);
        }
        self.clips[index].name = name;
        Ok(())
    }

    /// Removes clips, leaving their space on the timeline empty.
    pub fn delete_clips(&mut self, ids: &[ClipId]) -> Result<(), EditError> {
        if ids.is_empty() {
            return Err(EditError::NothingSelected);
        }
        for &id in ids {
            self.index_of(id)?;
        }
        self.clips.retain(|c| !ids.contains(&c.id));
        Ok(())
    }

    /// Removes part of the timeline, leaving a gap. A clip with its middle
    /// removed becomes two clips.
    pub fn delete_range(&mut self, range: TimeRange) -> Result<(), EditError> {
        let touches = |c: &Clip| c.start < range.end && c.end() > range.start;
        if range.start >= range.end || !self.clips.iter().any(touches) {
            return Err(EditError::NothingSelected);
        }
        // The end goes first, so the part that survives gets the lowest number.
        for at in [range.end, range.start] {
            match self.split_at(at) {
                Ok(_) | Err(EditError::NotInsideClip(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.clips.retain(|c| c.start < range.start || c.end() > range.end);
        Ok(())
    }

    /// Cuts a clip into one clip per stretch of sound, removing the pauses and
    /// keeping everything at its place on the timeline. The first part keeps the
    /// name and the rest are numbered. It's `NoChange` if there's nothing to cut.
    pub fn split_on_silence(&mut self, id: ClipId, settings: SilenceSettings) -> Result<(), EditError> {
        let clip = &self.clips[self.index_of(id)?];
        let (base, start, end) = (clip.name.clone(), clip.start, clip.end());
        let rendered = crate::render_clip(self, clip);
        let regions = sound_regions(
            &rendered.samples,
            rendered.channels as usize,
            self.sample_rate,
            settings,
        );
        let whole = regions.as_slice() == [(0, end - start)];
        if regions.is_empty() || whole {
            return Err(EditError::NoChange);
        }

        // Everything between the regions goes, working backwards so positions hold.
        let mut gaps = Vec::new();
        let mut position = start;
        for (from, to) in &regions {
            gaps.push((position, start + from));
            position = start + to;
        }
        gaps.push((position, end));
        for (from, to) in gaps.into_iter().rev().filter(|(from, to)| from < to) {
            self.delete_range(TimeRange::new(from, to))?;
        }

        // Clear the names first, so numbering runs in timeline order.
        let pieces: Vec<usize> = (0..self.clips.len())
            .filter(|&i| self.clips[i].start >= start && self.clips[i].end() <= end)
            .collect();
        for &index in &pieces {
            self.clips[index].name.clear();
        }
        for (order, &index) in pieces.iter().enumerate() {
            self.clips[index].name = if order == 0 { base.clone() } else { self.numbered_name(&base) };
        }
        Ok(())
    }

    /// Splits the clip under `at` in two. The left part keeps the id and name;
    /// the right part gets a new id and a numbered name, which is returned.
    pub fn split_at(&mut self, at: Frames) -> Result<ClipId, EditError> {
        let index = self
            .clips
            .iter()
            .position(|c| at > c.start && at < c.end())
            .ok_or(EditError::NotInsideClip(at))?;

        let (left, right) = split_segments(&self.clips[index].segments, at - self.clips[index].start);
        let name = self.numbered_name(&self.clips[index].name);
        let id = self.new_clip_id();
        self.clips[index].segments = left;
        self.clips.insert(index + 1, Clip { id, name, start: at, segments: right });
        Ok(id)
    }

    /// Joins clips into the earliest one, which keeps its id and name. Gaps
    /// between them become silence, and any selected order is accepted.
    pub fn merge_clips(&mut self, ids: &[ClipId]) -> Result<ClipId, EditError> {
        let mut indices = ids
            .iter()
            .map(|&id| self.index_of(id))
            .collect::<Result<Vec<_>, _>>()?;
        indices.sort_unstable();
        indices.dedup();

        let contiguous = indices.windows(2).all(|pair| pair[1] == pair[0] + 1);
        if indices.len() < 2 || !contiguous {
            return Err(EditError::ClipsNotAdjacent);
        }

        let first = indices[0];
        let merged: Vec<Clip> = self.clips.drain(first + 1..=indices[indices.len() - 1]).collect();
        let target = &mut self.clips[first];
        for clip in merged {
            let gap = clip.start - target.end();
            if gap > 0 {
                target.segments.push(Segment { kind: SegmentKind::Silence, len: gap });
            }
            target.segments.extend(clip.segments);
        }
        Ok(target.id)
    }

    /// Moves a clip towards `requested_start`.
    ///
    /// Its midpoint decides where it lands in the order, so dragging past
    /// another clip reorders them. It is then kept clear of the clip before it,
    /// and pushes later clips right only as far as needed to avoid overlap.
    pub fn move_clip(&mut self, id: ClipId, requested_start: Frames) -> Result<(), EditError> {
        let index = self.index_of(id)?;
        let before = self.clips.clone();

        let mut clip = self.clips.remove(index);
        let midpoint = requested_start + clip.len() / 2;
        let insert_at = self
            .clips
            .iter()
            .position(|c| midpoint < c.start + c.len() / 2)
            .unwrap_or(self.clips.len());

        let earliest = insert_at.checked_sub(1).map_or(0, |i| self.clips[i].end());
        clip.start = requested_start.max(earliest);
        let mut end = clip.end();
        self.clips.insert(insert_at, clip);

        for later in &mut self.clips[insert_at + 1..] {
            later.start = later.start.max(end);
            end = later.end();
        }

        if self.clips == before {
            return Err(EditError::NoChange);
        }
        Ok(())
    }

    /// Changes the gain of everything in `range`, across as many clips as it
    /// touches. Gain applied more than once accumulates.
    pub fn amplify(&mut self, range: TimeRange, db: f32) -> Result<(), EditError> {
        if !db.is_finite() {
            return Err(EditError::InvalidGain);
        }
        let factor = db_to_gain(db);

        let mut touched = false;
        for clip in &mut self.clips {
            let start = range.start.max(clip.start);
            let end = range.end.min(clip.end());
            if start >= end {
                continue;
            }
            touched = true;

            let (before, rest) = split_segments(&clip.segments, start - clip.start);
            let (mut selected, after) = split_segments(&rest, end - start);
            for segment in &mut selected {
                if let SegmentKind::Audio { gain, .. } = &mut segment.kind {
                    *gain *= factor;
                }
            }
            clip.segments = [before, selected, after].concat();
        }

        if touched { Ok(()) } else { Err(EditError::NothingSelected) }
    }

    fn index_of(&self, id: ClipId) -> Result<usize, EditError> {
        self.clips
            .iter()
            .position(|c| c.id == id)
            .ok_or(EditError::ClipNotFound(id))
    }

    fn new_clip_id(&mut self) -> ClipId {
        let id = ClipId(self.next_clip);
        self.next_clip += 1;
        id
    }

    /// `base` followed by the lowest number no other clip's name uses.
    fn numbered_name(&self, base: &str) -> String {
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|name| self.clips.iter().all(|c| c.name != *name))
            .expect("an unused number always exists")
    }
}

fn clean_name(name: &str) -> Result<String, EditError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EditError::EmptyName);
    }
    Ok(name.to_owned())
}
