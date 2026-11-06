//! Core of Kantan Audio Manager: a single track of named clips, each of which
//! exports as its own file.
//!
//! Audio is never modified. A [`Source`] holds immutable samples, and a [`Clip`]
//! is a list of [`Segment`]s pointing into sources, so every edit only rewrites
//! those lists and undo snapshots stay cheap.

pub mod app;
pub mod history;
pub mod import;
pub mod peaks;
pub mod project;
pub mod render;
pub mod timeline;

pub use history::Document;
pub use project::{
    Clip, ClipId, EditError, Frames, Project, Segment, SegmentKind, Source, SourceId, TimeRange,
    db_to_gain,
};
pub use render::{RenderedClip, render_clip};
