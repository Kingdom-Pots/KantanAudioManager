//! Core of Kantan Audio Manager: a single track of named clips, each of which
//! exports as its own file.
//!
//! Audio is never modified. A [`Source`] holds immutable samples, and a [`Clip`]
//! is a list of [`Segment`]s pointing into sources, so every edit only rewrites
//! those lists and undo snapshots stay cheap.

/// Runs work on a background thread on the desktop. Browsers have no threads
/// here, so the web version runs it straight away, briefly holding up the page.
pub fn run_in_background(work: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(work);
    #[cfg(target_arch = "wasm32")]
    work();
}

pub mod about;
pub mod app;
pub mod export;
pub mod export_dialog;
pub mod history;
pub mod import;
pub mod peaks;
pub mod playback;
pub mod prefs;
pub mod project;
pub mod project_file;
pub mod recording;
pub mod render;
pub mod session;
pub mod timeline;
#[cfg(target_arch = "wasm32")]
pub mod web;

pub use history::Document;
pub use project::{
    Clip, ClipId, EditError, Frames, Project, Segment, SegmentKind, SilenceSettings, Source, SourceId,
    TimeRange,
    db_to_gain,
};
pub use render::{RenderedClip, render_clip};
