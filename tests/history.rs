use std::sync::Arc;

use kantan_audio_manager::*;

fn document_with_clip() -> (Document, ClipId) {
    let mut doc = Document::new(Project::new(100));
    let id = doc
        .edit("Import", |p| {
            let source = p.add_source(1, vec![0.5; 100])?;
            p.append_clip("a", source)
        })
        .unwrap();
    (doc, id)
}

#[test]
fn undo_and_redo_walk_through_edits() {
    let (mut doc, id) = document_with_clip();
    let imported = doc.project().clone();

    doc.edit("Rename", |p| p.rename_clip(id, "b")).unwrap();
    let renamed = doc.project().clone();

    assert_eq!(doc.undo_label(), Some("Rename"));
    assert_eq!(doc.undo(), Some("Rename"));
    assert_eq!(doc.project(), &imported);

    assert_eq!(doc.redo_label(), Some("Rename"));
    assert_eq!(doc.redo(), Some("Rename"));
    assert_eq!(doc.project(), &renamed);
}

#[test]
fn undo_and_redo_stop_at_the_ends() {
    let (mut doc, _) = document_with_clip();
    assert_eq!(doc.redo(), None);
    assert_eq!(doc.undo(), Some("Import"));
    assert_eq!(doc.undo(), None);
    assert!(doc.project().clips().is_empty());
}

#[test]
fn a_new_edit_clears_redo() {
    let (mut doc, id) = document_with_clip();
    doc.edit("Rename", |p| p.rename_clip(id, "b")).unwrap();
    doc.undo();
    doc.edit("Rename", |p| p.rename_clip(id, "c")).unwrap();
    assert_eq!(doc.redo(), None);
    assert_eq!(doc.project().clip(id).unwrap().name, "c");
}

#[test]
fn a_failed_edit_changes_nothing_even_if_partly_applied() {
    let (mut doc, id) = document_with_clip();
    let before = doc.project().clone();

    let result = doc.edit("Broken", |p| {
        p.rename_clip(id, "changed")?;
        p.split_at(9_999)
    });

    assert_eq!(result, Err(EditError::NotInsideClip(9_999)));
    assert_eq!(doc.project(), &before);
    assert_eq!(doc.undo_label(), Some("Import"));
}

#[test]
fn snapshots_share_audio_instead_of_copying_it() {
    let (mut doc, id) = document_with_clip();
    let source = match doc.project().clip(id).unwrap().segments[0].kind {
        SegmentKind::Audio { source, .. } => source,
        SegmentKind::Silence => unreachable!(),
    };
    let original = Arc::clone(doc.project().source(source).unwrap());

    doc.edit("Amplify", |p| p.amplify(TimeRange::new(0, 50), 6.0)).unwrap();
    assert!(Arc::ptr_eq(&original, doc.project().source(source).unwrap()));

    doc.undo();
    assert!(Arc::ptr_eq(&original, doc.project().source(source).unwrap()));
}

/// Small deterministic generator, so a failure reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit.max(1)
    }
}

fn assert_valid(project: &Project) {
    let clips = project.clips();
    for pair in clips.windows(2) {
        assert!(pair[0].end() <= pair[1].start, "clips overlap or are out of order");
    }
    for clip in clips {
        assert!(!clip.segments.is_empty() && !clip.name.is_empty());
        for segment in &clip.segments {
            assert!(segment.len > 0, "empty segment");
            if let SegmentKind::Audio { source, offset, gain } = segment.kind {
                let frames = project.source(source).expect("missing source").frames();
                assert!(offset + segment.len <= frames, "segment runs past its source");
                assert!(gain.is_finite());
            }
        }
        let rendered = render_clip(project, clip);
        assert_eq!(
            rendered.samples.len() as u64,
            clip.len() * rendered.channels as u64,
            "rendered length does not match clip length"
        );
    }
}

/// Applies thousands of random edits, checking the track stays valid, then
/// verifies undo and redo reproduce every state exactly.
#[test]
fn random_edits_stay_valid_and_fully_undo() {
    for seed in 1..=20u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut doc = Document::new(Project::new(100));
        let mut states = vec![doc.project().clone()];

        for _ in 0..300 {
            let clips: Vec<ClipId> = doc.project().clips().iter().map(|c| c.id).collect();
            let timeline = doc.project().clips().last().map_or(1, |c| c.end() + 20);
            let some_clip = clips.get(rng.below(clips.len() as u64) as usize).copied();
            let position = rng.below(timeline);
            let other_position = rng.below(timeline);
            let choice = rng.below(7);
            let frames = 1 + rng.below(200) as usize;
            let stereo = rng.below(2) == 0;

            let result: Result<(), EditError> = doc.edit("Random", |p| {
                let clip = some_clip.ok_or(EditError::NothingSelected);
                match choice {
                    0 => {
                        let channels = if stereo { 2 } else { 1 };
                        let source = p.add_source(channels, vec![0.25; frames * channels as usize])?;
                        p.append_clip("take", source).map(drop)
                    }
                    1 => p.split_at(position).map(drop),
                    2 => {
                        let clip = clip?;
                        let index = clips.iter().position(|&c| c == clip).unwrap();
                        match clips.get(index + 1) {
                            Some(&next) => p.merge_clips(&[clip, next]).map(drop),
                            None => Err(EditError::ClipsNotAdjacent),
                        }
                    }
                    3 => p.move_clip(clip?, position),
                    4 => p.rename_clip(clip?, "renamed"),
                    5 => p.delete_clips(&[clip?]),
                    _ => p.amplify(TimeRange::new(position, other_position), 1.5),
                }
            });

            assert_valid(doc.project());
            if result.is_ok() {
                states.push(doc.project().clone());
            }
        }

        assert!(states.len() > 50, "too few edits succeeded to mean anything");

        for state in states.iter().rev().skip(1) {
            assert!(doc.undo().is_some());
            assert_eq!(doc.project(), state);
        }
        assert_eq!(doc.undo(), None);

        for state in states.iter().skip(1) {
            assert!(doc.redo().is_some());
            assert_eq!(doc.project(), state);
        }
        assert_eq!(doc.redo(), None);
    }
}
