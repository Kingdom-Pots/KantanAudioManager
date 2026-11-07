use kantan_audio_manager::*;

const RATE: u32 = 100;
/// `Project::take_gap` at the test sample rate.
const GAP: Frames = 50;

/// A mono clip whose samples are `first, first + 1, ...`, so tests can see
/// exactly which frames ended up where.
fn add_ramp(project: &mut Project, name: &str, first: u32, frames: u32) -> ClipId {
    let samples = (first..first + frames).map(|v| v as f32).collect();
    let source = project.add_source(1, samples).unwrap();
    project.append_clip(name, source).unwrap()
}

fn rendered(project: &Project, id: ClipId) -> Vec<f32> {
    render_clip(project, project.clip(id).unwrap()).samples
}

fn ramp(first: u32, frames: u32) -> Vec<f32> {
    (first..first + frames).map(|v| v as f32).collect()
}

fn names(project: &Project) -> Vec<&str> {
    project.clips().iter().map(|c| c.name.as_str()).collect()
}

fn starts(project: &Project) -> Vec<Frames> {
    project.clips().iter().map(|c| c.start).collect()
}

#[test]
fn takes_are_appended_with_a_gap() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10);
    add_ramp(&mut p, "b", 0, 20);
    add_ramp(&mut p, "c", 0, 5);
    assert_eq!(starts(&p), [0, 10 + GAP, 30 + 2 * GAP]);
    assert_eq!(names(&p), ["a", "b", "c"]);
}

#[test]
fn invalid_sources_are_rejected() {
    let mut p = Project::new(RATE);
    assert!(p.add_source(1, vec![]).is_err());
    assert!(p.add_source(3, vec![0.0; 3]).is_err());
    assert!(p.add_source(2, vec![0.0; 3]).is_err());
}

#[test]
fn split_keeps_audio_and_numbers_the_right_part() {
    let mut p = Project::new(RATE);
    let left = add_ramp(&mut p, "Line", 0, 10);

    let right = p.split_at(4).unwrap();

    assert_eq!(rendered(&p, left), ramp(0, 4));
    assert_eq!(rendered(&p, right), ramp(4, 6));
    assert_eq!(names(&p), ["Line", "Line 2"]);
    assert_eq!(starts(&p), [0, 4]);
}

#[test]
fn split_names_skip_ones_already_used() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "Line", 0, 10);
    p.split_at(5).unwrap();
    p.split_at(2).unwrap();
    assert_eq!(names(&p), ["Line", "Line 3", "Line 2"]);
}

#[test]
fn split_only_works_strictly_inside_a_clip() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10);
    assert_eq!(p.split_at(0), Err(EditError::NotInsideClip(0)));
    assert_eq!(p.split_at(10), Err(EditError::NotInsideClip(10)));
    assert_eq!(p.split_at(30), Err(EditError::NotInsideClip(30)));
}

#[test]
fn merge_fills_gaps_with_silence_and_keeps_first_name() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 1, 3);
    let b = add_ramp(&mut p, "b", 10, 2);

    // Selection order must not matter.
    let merged = p.merge_clips(&[b, a]).unwrap();

    assert_eq!(merged, a);
    assert_eq!(names(&p), ["a"]);
    let expected = [ramp(1, 3), vec![0.0; GAP as usize], ramp(10, 2)].concat();
    assert_eq!(rendered(&p, a), expected);
}

#[test]
fn merging_a_split_restores_the_audio() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 0, 10);
    let b = p.split_at(3).unwrap();
    p.merge_clips(&[a, b]).unwrap();
    assert_eq!(rendered(&p, a), ramp(0, 10));
}

#[test]
fn merge_requires_adjacent_clips() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 0, 5);
    add_ramp(&mut p, "b", 0, 5);
    let c = add_ramp(&mut p, "c", 0, 5);
    assert_eq!(p.merge_clips(&[a, c]), Err(EditError::ClipsNotAdjacent));
    assert_eq!(p.merge_clips(&[a]), Err(EditError::ClipsNotAdjacent));
    assert_eq!(p.merge_clips(&[a, a]), Err(EditError::ClipsNotAdjacent));
}

#[test]
fn move_into_free_space_keeps_order() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10);
    let b = add_ramp(&mut p, "b", 0, 10);
    p.move_clip(b, 500).unwrap();
    assert_eq!(starts(&p), [0, 500]);
    assert_eq!(names(&p), ["a", "b"]);
}

#[test]
fn dragging_past_another_clip_reorders_them() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 0, 10); // 0..10
    add_ramp(&mut p, "b", 0, 10); // 60..70
    add_ramp(&mut p, "c", 0, 10); // 120..130

    // a's midpoint lands past b's midpoint but before c's.
    p.move_clip(a, 62).unwrap();

    assert_eq!(names(&p), ["b", "a", "c"]);
    assert_eq!(starts(&p), [60, 70, 120]);
}

#[test]
fn dropping_onto_a_clip_pushes_later_clips_only_as_needed() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10); // 0..10
    add_ramp(&mut p, "b", 0, 10); // 60..70
    add_ramp(&mut p, "c", 0, 10); // 120..130
    let d = add_ramp(&mut p, "d", 0, 100); // 180..280

    // d's midpoint (50) is past a's but before b's, so it lands between them,
    // stays clear of a, and pushes b. c starts exactly where b now ends.
    p.move_clip(d, 0).unwrap();

    assert_eq!(names(&p), ["a", "d", "b", "c"]);
    assert_eq!(starts(&p), [0, 10, 110, 120]);
}

#[test]
fn moving_to_the_same_place_changes_nothing() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 0, 10);
    add_ramp(&mut p, "b", 0, 10);
    assert_eq!(p.move_clip(a, 0), Err(EditError::NoChange));
}

#[test]
fn rename_trims_and_rejects_empty() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 0, 5);
    p.rename_clip(a, "  Hello  ").unwrap();
    assert_eq!(names(&p), ["Hello"]);
    assert_eq!(p.rename_clip(a, "   "), Err(EditError::EmptyName));
    assert_eq!(p.rename_clip(a, "Hello"), Err(EditError::NoChange));
}

#[test]
fn delete_leaves_the_space_empty() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10);
    let b = add_ramp(&mut p, "b", 0, 10);
    add_ramp(&mut p, "c", 0, 10);
    p.delete_clips(&[b]).unwrap();
    assert_eq!(names(&p), ["a", "c"]);
    assert_eq!(starts(&p), [0, 120]);
    assert_eq!(p.delete_clips(&[b]), Err(EditError::ClipNotFound(b)));
}

#[test]
fn amplify_only_changes_the_selected_frames() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 1, 6);

    p.amplify(TimeRange::new(2, 4), 20.0).unwrap();

    let out = rendered(&p, a);
    let expected = [1.0, 2.0, 30.0, 40.0, 5.0, 6.0];
    for (got, want) in out.iter().zip(expected) {
        assert!((got - want).abs() < 1e-3, "{out:?}");
    }
}

#[test]
fn amplify_spans_clips_and_accepts_a_backwards_selection() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 1, 10); // 0..10
    let b = add_ramp(&mut p, "b", 1, 10); // 60..70

    p.amplify(TimeRange::new(65, 5), 20.0).unwrap();

    let (a_out, b_out) = (rendered(&p, a), rendered(&p, b));
    assert_eq!(a_out[4], 5.0);
    assert!((a_out[5] - 60.0).abs() < 1e-3);
    assert!((b_out[4] - 50.0).abs() < 1e-3);
    assert_eq!(b_out[5], 6.0);
}

#[test]
fn amplify_accumulates() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 1, 2);
    p.amplify(TimeRange::new(0, 2), 20.0).unwrap();
    p.amplify(TimeRange::new(0, 2), -20.0).unwrap();
    assert!((rendered(&p, a)[1] - 2.0).abs() < 1e-3);
}

#[test]
fn amplify_rejects_bad_input() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 1, 10);
    assert_eq!(p.amplify(TimeRange::new(0, 5), f32::NAN), Err(EditError::InvalidGain));
    assert_eq!(p.amplify(TimeRange::new(3, 3), 6.0), Err(EditError::NothingSelected));
    assert_eq!(p.amplify(TimeRange::new(20, 30), 6.0), Err(EditError::NothingSelected));
}

#[test]
fn amplify_leaves_merged_silence_silent() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "a", 1, 2);
    let b = add_ramp(&mut p, "b", 1, 2);
    p.merge_clips(&[a, b]).unwrap();
    p.amplify(TimeRange::new(0, 1000), 20.0).unwrap();
    let out = rendered(&p, a);
    assert!(out[2..2 + GAP as usize].iter().all(|&s| s == 0.0));
}

#[test]
fn stereo_clips_render_interleaved() {
    let mut p = Project::new(RATE);
    let source = p.add_source(2, vec![1.0, 2.0, 3.0, 4.0]).unwrap();
    let a = p.append_clip("a", source).unwrap();
    let out = render_clip(&p, p.clip(a).unwrap());
    assert_eq!(out.channels, 2);
    assert_eq!(out.samples, [1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn merging_mono_with_stereo_renders_stereo() {
    let mut p = Project::new(RATE);
    let mono = add_ramp(&mut p, "mono", 7, 2);
    let source = p.add_source(2, vec![1.0, 2.0]).unwrap();
    let stereo = p.append_clip("stereo", source).unwrap();

    p.merge_clips(&[mono, stereo]).unwrap();

    let out = render_clip(&p, p.clip(mono).unwrap());
    assert_eq!(out.channels, 2);
    assert_eq!(&out.samples[..4], [7.0, 7.0, 8.0, 8.0]);
    assert_eq!(out.samples.len(), (2 + GAP as usize + 1) * 2);
    assert_eq!(&out.samples[out.samples.len() - 2..], [1.0, 2.0]);
}

#[test]
fn deleting_a_range_inside_a_clip_splits_it_and_leaves_a_gap() {
    let mut p = Project::new(RATE);
    let a = add_ramp(&mut p, "Line", 0, 10);
    p.delete_range(TimeRange::new(3, 6)).unwrap();
    assert_eq!(names(&p), ["Line", "Line 2"]);
    assert_eq!(starts(&p), [0, 6]);
    assert_eq!(rendered(&p, a), ramp(0, 3));
    assert_eq!(rendered(&p, p.clips()[1].id), ramp(6, 4));
}

#[test]
fn deleting_a_range_trims_edges_and_removes_covered_clips() {
    let mut p = Project::new(RATE);
    add_ramp(&mut p, "a", 0, 10); // 0..10
    add_ramp(&mut p, "b", 0, 10); // 60..70
    let c = add_ramp(&mut p, "c", 0, 10); // 120..130
    p.delete_range(TimeRange::new(5, 125)).unwrap();
    assert_eq!(names(&p), ["a", "c 2"]);
    assert_eq!(starts(&p), [0, 125]);
    assert!(p.clip(c).is_none() || p.clip(c).unwrap().len() == 5);
    assert_eq!(p.delete_range(TimeRange::new(40, 50)), Err(EditError::NothingSelected));
    assert_eq!(p.delete_range(TimeRange::new(3, 3)), Err(EditError::NothingSelected));
}

#[test]
fn amplify_edges_ease_instead_of_jumping() {
    let mut p = Project::new(48000);
    let source = p.add_source(1, vec![0.1; 4800]).unwrap();
    let a = p.append_clip("a", source).unwrap();
    p.amplify(TimeRange::new(1000, 2000), 20.0).unwrap();
    let out = rendered(&p, a);
    let biggest_step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    assert!(biggest_step < 0.01, "{biggest_step}");
    assert!((out[1500] - 1.0).abs() < 1e-3 && (out[500] - 0.1).abs() < 1e-6);
    assert!((out[2500] - 0.1).abs() < 1e-6);
}

/// Sound and silence by turns, in tenths of a second at 1000 Hz.
fn add_pattern(p: &mut Project, name: &str, pattern: &[(f32, usize)]) -> ClipId {
    let samples: Vec<f32> = pattern
        .iter()
        .flat_map(|&(level, tenths)| std::iter::repeat_n(level, tenths * 100))
        .collect();
    let source = p.add_source(1, samples).unwrap();
    p.append_clip(name, source).unwrap()
}

const PAUSES: SilenceSettings = SilenceSettings { threshold_db: -40.0, min_silence_seconds: 0.5 };

#[test]
fn silence_splits_one_clip_per_line_and_removes_pauses() {
    let mut p = Project::new(1000);
    let a = add_pattern(&mut p, "Take", &[(0.0, 10), (0.5, 10), (0.0, 10), (0.5, 20), (0.0, 10)]);
    p.split_on_silence(a, PAUSES).unwrap();
    assert_eq!(names(&p), ["Take", "Take 2"]);
    // Sound is at 1000..2000 and 3000..5000, with 50 ms kept around it.
    assert_eq!(starts(&p), [950, 2950]);
    assert_eq!(p.clips()[0].len(), 1100);
    assert_eq!(p.clips()[1].len(), 2100);
}

#[test]
fn short_pauses_stay_inside_a_line() {
    let mut p = Project::new(1000);
    let a = add_pattern(&mut p, "Take", &[(0.5, 10), (0.0, 3), (0.5, 10)]);
    assert_eq!(p.split_on_silence(a, PAUSES), Err(EditError::NoChange));
    assert_eq!(names(&p), ["Take"]);
}

#[test]
fn the_threshold_decides_what_counts_as_silence() {
    let mut p = Project::new(1000);
    let a = add_pattern(&mut p, "Take", &[(0.5, 10), (0.05, 10), (0.5, 10)]);
    assert_eq!(p.split_on_silence(a, PAUSES), Err(EditError::NoChange));
    let strict = SilenceSettings { threshold_db: -20.0, ..PAUSES };
    p.split_on_silence(a, strict).unwrap();
    assert_eq!(names(&p), ["Take", "Take 2"]);
}

#[test]
fn entirely_silent_clips_and_lone_clicks_are_left_alone() {
    let mut p = Project::new(1000);
    let a = add_pattern(&mut p, "Quiet", &[(0.0, 20)]);
    assert_eq!(p.split_on_silence(a, PAUSES), Err(EditError::NoChange));
    assert_eq!(names(&p), ["Quiet"]);
}

#[test]
fn numbering_avoids_names_already_used() {
    let mut p = Project::new(1000);
    add_pattern(&mut p, "Take 2", &[(0.5, 1)]);
    let a = add_pattern(&mut p, "Take", &[(0.5, 10), (0.0, 10), (0.5, 10), (0.0, 10), (0.5, 10)]);
    p.split_on_silence(a, PAUSES).unwrap();
    assert_eq!(names(&p), ["Take 2", "Take", "Take 3", "Take 4"]);
}
