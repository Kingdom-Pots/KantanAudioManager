use crate::project::{Clip, Frames, Project, SegmentKind};

/// A clip's final audio, interleaved. Values are not clamped, so they can
/// exceed 1.0 after amplifying; output stages decide how to handle that.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedClip {
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Renders a clip of this project. The result is stereo if any of its audio is
/// stereo, in which case mono parts play equally on both sides.
pub fn render_clip(project: &Project, clip: &Clip) -> RenderedClip {
    let source_of = |id| {
        project
            .source(id)
            .expect("sources are never removed from a project")
    };

    let channels = clip
        .segments
        .iter()
        .filter_map(|segment| match segment.kind {
            SegmentKind::Audio { source, .. } => Some(source_of(source).channels),
            SegmentKind::Silence => None,
        })
        .max()
        .unwrap_or(1);

    let mut samples = Vec::with_capacity(clip.len() as usize * channels as usize);
    for (index, segment) in clip.segments.iter().enumerate() {
        match segment.kind {
            SegmentKind::Silence => {
                samples.resize(samples.len() + segment.len as usize * channels as usize, 0.0);
            }
            SegmentKind::Audio { source, offset, .. } => {
                let source = source_of(source);
                let width = source.channels as usize;
                let first = offset as usize * width;
                let last = first + segment.len as usize * width;
                for (local, frame) in source.samples[first..last].chunks_exact(width).enumerate() {
                    let gain = clip.gain_at(index, local as Frames, project.sample_rate());
                    for channel in 0..channels as usize {
                        // A mono frame repeats its one value on every channel.
                        samples.push(frame[channel.min(width - 1)] * gain);
                    }
                }
            }
        }
    }

    RenderedClip { channels, samples }
}
