//! Runs the real UI through the bridge, covering input that only exists at the
//! app level. Needs `--features bridge`.
#![cfg(feature = "bridge")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// A folder of sample files, removed when the test ends.
struct Samples(PathBuf);

impl Samples {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kantan_bridge_{}_{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, seconds) in [("intro", 2), ("line1", 1), ("line2", 1), ("line10", 1)] {
            write_wav(&dir.join(format!("{name}.wav")), seconds);
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        format!("\"{}\"", self.0.join(name).display())
    }

    /// An import line, deliberately out of order.
    fn import_line(&self, command: &str) -> String {
        let names = ["line10.wav", "line2.wav", "intro.wav", "line1.wav"];
        let paths: Vec<_> = names.iter().map(|n| self.path(n)).collect();
        format!("{command} {}", paths.join(" "))
    }
}

impl Drop for Samples {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A 48 kHz mono 16-bit WAV at half volume.
fn write_wav(path: &Path, seconds: u32) {
    let samples = 48000 * seconds;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&96000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples * 2).to_le_bytes());
    for _ in 0..samples {
        bytes.extend_from_slice(&16384i16.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Runs a script and returns each `state` command's JSON.
fn run(script: &str) -> Vec<Value> {
    let output = run_raw(script);
    assert!(
        output.status.success(),
        "bridge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn run_raw(script: &str) -> std::process::Output {
    run_with_data(script, None)
}

fn run_with_data(script: &str, data: Option<&Path>) -> std::process::Output {
    use std::io::Write;
    let mut command = Command::new(env!("CARGO_BIN_EXE_kantan-bridge"));
    if let Some(data) = data {
        command.env("KANTAN_DATA_DIR", data);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn names(state: &Value) -> Vec<&str> {
    state["clips"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect()
}

fn selected(state: &Value) -> Vec<&str> {
    state["clips"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["selected"].as_bool().unwrap())
        .map(|c| c["name"].as_str().unwrap())
        .collect()
}

#[test]
fn importing_sorts_files_and_undoes_as_one_step() {
    let samples = Samples::new("import");
    let states = run(&format!(
        "{}\nstate\nkey ctrl+z\nstate\nkey ctrl+y\nstate\nkey ctrl+z\nkey ctrl+shift+z\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0]), ["intro", "line1", "line2", "line10"]);
    assert!(names(&states[1]).is_empty());
    assert_eq!(names(&states[2]).len(), 4);
    assert_eq!(names(&states[3]).len(), 4);
}

#[test]
fn dropping_files_imports_them() {
    let samples = Samples::new("drop");
    let states = run(&format!("{}\nstate\n", samples.import_line("drop")));
    assert_eq!(names(&states[0]), ["intro", "line1", "line2", "line10"]);
    assert_eq!(states[0]["error"], Value::Null);
}

#[test]
fn a_broken_file_reports_an_error_and_the_rest_import() {
    let samples = Samples::new("broken");
    std::fs::write(samples.0.join("broken.wav"), b"not audio").unwrap();
    let states = run(&format!(
        "import {} {}\nstate\n",
        samples.path("broken.wav"),
        samples.path("line1.wav")
    ));
    assert_eq!(names(&states[0]), ["line1"]);
    assert!(states[0]["error"].as_str().unwrap().contains("broken"));
}

#[test]
fn clip_clicks_honour_modifiers() {
    let samples = Samples::new("clips");
    let states = run(&format!(
        "{}\nclip line1\nclip line10 shift\nstate\nclip line2 ctrl\nstate\nkey ctrl+a\nstate\nkey escape\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(selected(&states[0]), ["line1", "line2", "line10"]);
    assert_eq!(selected(&states[1]), ["line1", "line10"]);
    assert_eq!(selected(&states[2]).len(), 4);
    assert!(selected(&states[3]).is_empty());
}

#[test]
fn range_selection_replaces_clip_selection_in_either_direction() {
    let samples = Samples::new("range");
    let states = run(&format!(
        "{}\nclip line1\nrange 1.0 3.0\nstate\nrange 4.0 2.0\nstate\ntime 3.0\nstate\ntime 1.0 shift\nstate\n",
        samples.import_line("import")
    ));

    let close = |value: &Value, expected: f64| {
        assert!((value.as_f64().unwrap() - expected).abs() < 0.05, "{value} vs {expected}");
    };
    assert!(selected(&states[0]).is_empty());
    close(&states[0]["range"][0], 1.0);
    close(&states[0]["range"][1], 3.0);
    close(&states[1]["range"][0], 2.0);
    close(&states[1]["range"][1], 4.0);
    assert_eq!(states[2]["range"], Value::Null);
    close(&states[2]["cursor"], 3.0);
    close(&states[3]["range"][0], 1.0);
    close(&states[3]["range"][1], 3.0);
}

#[test]
fn zoom_keeps_the_pointer_time_in_place_and_fit_restores() {
    let samples = Samples::new("zoom");
    let states = run(&format!(
        "{}\nstate\nzoom 2.5 5\nstate\nbutton Fit\nstate\n",
        samples.import_line("import")
    ));
    let fit = states[0]["view"]["frames_per_point"].as_f64().unwrap();
    let zoomed = states[1]["view"]["frames_per_point"].as_f64().unwrap();
    assert!(zoomed < fit / 1.5, "{zoomed} should be well under {fit}");

    // 2.5s must be at the same screen position before and after zooming.
    let x = |state: &Value| {
        let from = state["view"]["from"].as_f64().unwrap();
        let per_point = state["view"]["frames_per_point"].as_f64().unwrap();
        (2.5 - from) * 48000.0 / per_point
    };
    assert!((x(&states[0]) - x(&states[1])).abs() < 2.0, "{} vs {}", x(&states[0]), x(&states[1]));

    assert_eq!(states[2]["view"]["frames_per_point"].as_f64().unwrap(), fit);
}

#[test]
fn panning_and_the_scrollbar_move_the_view() {
    let samples = Samples::new("pan");
    let states = run(&format!(
        "{}\nzoom 0 10\nstate\npan 3\nstate\npan -1\nstate\nscrollbar 200\nstate\nscrollbar -5000\nstate\n",
        samples.import_line("import")
    ));
    let from = |index: usize| states[index]["view"]["from"].as_f64().unwrap();
    assert_eq!(from(0), 0.0);
    assert!(from(1) > from(0), "pan forward: {}", from(1));
    assert!(from(2) < from(1), "pan back: {}", from(2));
    assert!(from(3) > from(2), "scrollbar forward: {}", from(3));
    assert_eq!(from(4), 0.0, "scrollbar stops at the start");
}

#[test]
fn toolbar_buttons_work_and_never_open_a_dialog() {
    let samples = Samples::new("buttons");
    let states = run(&format!(
        "{}\nbutton Undo\nstate\nbutton Redo\nstate\nbutton Import\nkey ctrl+o\nstate\n",
        samples.import_line("import")
    ));
    assert!(names(&states[0]).is_empty());
    assert_eq!(names(&states[1]).len(), 4);
    assert_eq!(states[2]["blocked_dialogs"], 2);
}

#[test]
fn errors_name_the_line_and_stop_the_script() {
    let output = run_raw("state\n\nclip missing\nstate\n");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("line 3") && stderr.contains("missing"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
}

/// A folder the export tests write into.
fn export_folder(samples: &Samples) -> String {
    samples.0.join("out").display().to_string()
}

fn files(state: &Value) -> Vec<&str> {
    state["export"]["files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect()
}

#[test]
fn exporting_writes_named_wavs_in_timeline_order() {
    let samples = Samples::new("export");
    let out = export_folder(&samples);
    let states = run(&format!(
        "{}\nkey ctrl+e\nexport folder \"{out}\"\nexport prefix VO_\nexport suffix _final\nexport numbering before\nexport rate 44100\nexport bits 24\nstate\nbutton \"Export files\"\nexport-wait\nstate\n",
        samples.import_line("import"),
    ));
    let expected = [
        "VO_001_intro_final.wav",
        "VO_002_line1_final.wav",
        "VO_003_line2_final.wav",
        "VO_004_line10_final.wav",
    ];
    assert_eq!(files(&states[0]), expected);
    assert_eq!(states[1]["export"]["report"]["written"], 4);

    let reader = hound::WavReader::open(samples.0.join("out").join(expected[0])).unwrap();
    assert_eq!(reader.spec().sample_rate, 44100);
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.duration(), 88200);

    // After exporting, the same names now exist, so a repeat must be blocked.
    assert_eq!(states[1]["export"]["files"][0]["problem"], "exists");
}

#[test]
fn export_only_covers_selected_clips_and_renumbers_them() {
    let samples = Samples::new("selected");
    let out = export_folder(&samples);
    let states = run(&format!(
        "{}\nclip line2\nclip line10 ctrl\nkey ctrl+e\nexport folder \"{out}\"\nexport numbering after\nexport digits 2\nstate\nexport scope all\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(files(&states[0]), ["line2_01.wav", "line10_02.wav"]);
    assert_eq!(files(&states[1]).len(), 4);
}

#[test]
fn problems_block_export_until_resolved() {
    let samples = Samples::new("blocked");
    let out = samples.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("intro.wav"), b"keep me").unwrap();
    let states = run(&format!(
        "{}\nkey ctrl+e\nbutton \"Export files\"\nexport-wait\nstate\nexport folder \"{}\"\nbutton \"Export files\"\nexport-wait\nstate\nbutton \"Replace existing\"\nexport-wait\nstate\n",
        samples.import_line("import"),
        out.display(),
    ));
    // No folder, then a conflict prompt: nothing is written before confirmation.
    assert_eq!(states[0]["export"]["report"], Value::Null);
    assert_eq!(states[1]["export"]["report"], Value::Null);
    assert_eq!(states[1]["export"]["files"][0]["problem"], "exists");
    assert_eq!(states[1]["export"]["conflict"], true);
    // Replacement was explicitly confirmed.
    assert_eq!(states[2]["export"]["report"]["written"], 4);
    assert!(hound::WavReader::open(out.join("intro.wav")).is_ok());
}

#[test]
fn export_window_owns_the_keyboard_and_escape_closes_it() {
    let samples = Samples::new("modal");
    let states = run(&format!(
        "{}\nkey ctrl+e\nkey ctrl+z\nkey ctrl+a\nstate\nkey escape\nstate\nkey ctrl+z\nkey ctrl+e\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0]).len(), 4, "undo must not run behind the window");
    assert!(selected(&states[0]).is_empty());
    assert_eq!(states[0]["export"]["open"], true);
    assert_eq!(states[1]["export"]["open"], false);
    // With no clips left, the window refuses to open and says why.
    assert_eq!(states[2]["export"]["open"], false);
    assert!(states[2]["error"].as_str().unwrap().contains("no clips"));
}

#[test]
fn space_plays_pauses_and_resumes_from_where_it_stopped() {
    let samples = Samples::new("play");
    let states = run(&format!(
        "{}\ntime 0.5\nkey space\nstate\nadvance 0.25\nstate\nkey space\nstate\nkey space\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["playback"]["playing"], true);
    let position = states[1]["playback"]["position"].as_f64().unwrap();
    assert!((position - 0.75).abs() < 0.05, "{position}");
    assert!((states[1]["playback"]["peak"].as_f64().unwrap() - 0.5).abs() < 0.01);
    assert_eq!(states[2]["playback"]["playing"], false);
    assert!((states[2]["cursor"].as_f64().unwrap() - position).abs() < 0.001);
    assert!((states[3]["playback"]["position"].as_f64().unwrap() - position).abs() < 0.001);
}

#[test]
fn playing_a_range_stays_in_it_and_gaps_are_silent() {
    let samples = Samples::new("playrange");
    // intro is 0-2s, then a half second gap before line1.
    let states = run(&format!(
        "{}\nrange 2.1 2.4\nbutton Play\nadvance 0.1\nstate\nadvance 1\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["playback"]["peak"], 0.0);
    assert_eq!(states[1]["playback"]["playing"], false, "stops at the range end");
    assert!(states[1]["range"].is_array());
}

#[test]
fn undo_stops_playback_and_an_empty_track_explains_itself() {
    let samples = Samples::new("playundo");
    let states = run(&format!(
        "{}\nkey space\nkey ctrl+z\nstate\nkey space\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["playback"]["playing"], false);
    assert!(states[1]["error"].as_str().unwrap().contains("nothing to play"));
}

fn starts(state: &Value) -> Vec<f64> {
    state["clips"].as_array().unwrap().iter().map(|c| c["start"].as_f64().unwrap()).collect()
}

#[test]
fn split_and_merge_hotkeys_with_undo() {
    let samples = Samples::new("split");
    let states = run(&format!(
        "{}\ntime 1.0\nkey ctrl+i\nstate\nclip intro\nclip \"intro 2\" shift\nkey ctrl+j\nstate\nkey ctrl+z\nkey ctrl+z\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0])[..2], ["intro", "intro 2"]);
    assert_eq!(names(&states[1]).len(), 4);
    assert_eq!(states[1]["clips"][0]["frames"], 96000);
    assert_eq!(selected(&states[1]), ["intro"]);
    assert_eq!(names(&states[2]).len(), 4);
}

#[test]
fn splitting_outside_a_clip_explains_itself() {
    let samples = Samples::new("splitgap");
    let states = run(&format!("{}\ntime 2.2\nkey ctrl+i\nstate\n", samples.import_line("import")));
    assert_eq!(names(&states[0]).len(), 4);
    assert!(states[0]["error"].as_str().unwrap().contains("inside a clip"));
}

#[test]
fn delete_removes_selected_clips_or_a_range() {
    let samples = Samples::new("delete");
    let states = run(&format!(
        "{}\nclip line1\nkey delete\nstate\nrange 0.5 1.0\nkey delete\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0]), ["intro", "line2", "line10"]);
    assert_eq!(names(&states[1])[..2], ["intro", "intro 2"]);
    assert_eq!(states[1]["range"], Value::Null);
}

#[test]
fn rename_by_double_click_and_f2_with_real_typing() {
    let samples = Samples::new("rename");
    let states = run(&format!(
        "{}\ndouble intro\nstate\ntype \"Hello there\"\nkey enter\nstate\nclip line1\nkey f2\ntype x\nkey escape\nstate\nkey f2\ntype y\nbutton Apply\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["dialog"]["open"], "rename");
    assert_eq!(names(&states[1])[0], "Hello there");
    assert_eq!(states[1]["dialog"]["open"], Value::Null);
    assert_eq!(names(&states[2])[1], "line1", "escape cancels");
    assert_eq!(names(&states[3])[1], "y");
}

#[test]
fn hotkeys_stay_out_of_text_boxes() {
    let samples = Samples::new("typing");
    let states = run(&format!(
        "{}\ndouble intro\nkey delete\nkey space\nkey ctrl+z\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0]).len(), 4);
    assert_eq!(states[0]["playback"]["playing"], false);
}

#[test]
fn dragging_a_header_moves_and_reorders() {
    let samples = Samples::new("move");
    let states = run(&format!(
        "{}\nmove line10 6.0\nstate\nmove intro 3.2\nstate\nkey ctrl+z\nstate\n",
        samples.import_line("import")
    ));
    assert!((starts(&states[0])[3] - 6.0).abs() < 0.03, "{:?}", starts(&states[0]));
    assert_eq!(names(&states[1])[0], "line1", "intro moved past line1");
    assert_eq!(names(&states[2])[0], "intro");
    assert_eq!(states[2]["undo"], "Move");
}

#[test]
fn right_click_menu_acts_on_the_clicked_clip_and_position() {
    let samples = Samples::new("menu");
    let states = run(&format!(
        "{}\nmenu-clip line1\nstate\nbutton Delete\nstate\nmenu-time 0.5\nbutton \"Split here\"\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(selected(&states[0]), ["line1"]);
    assert_eq!(names(&states[1]), ["intro", "line2", "line10"]);
    assert_eq!(names(&states[2])[..2], ["intro", "intro 2"]);
}

#[test]
fn amplify_dialog_previews_applies_and_undoes() {
    let samples = Samples::new("amplify");
    let states = run(&format!(
        "{}\nclip line1\nkey ctrl+l\nstate\namplify-db 6.02\nbutton Apply\nstate\nkey ctrl+l\nbutton \"Max without clipping\"\nstate\nbutton Cancel\nkey ctrl+z\nstate\nkey escape\nkey ctrl+l\nstate\n",
        samples.import_line("import")
    ));
    assert!((states[0]["dialog"]["peak"].as_f64().unwrap() - 0.5).abs() < 0.01);
    assert!((states[1]["clips"][1]["peak"].as_f64().unwrap() - 1.0).abs() < 0.01);
    assert!((states[1]["clips"][0]["peak"].as_f64().unwrap() - 0.5).abs() < 0.01, "others untouched");
    assert!(states[2]["dialog"]["db"].as_f64().unwrap().abs() < 0.1, "already at full scale");
    assert!((states[3]["clips"][1]["peak"].as_f64().unwrap() - 0.5).abs() < 0.01);
}

#[test]
fn a_held_drag_changes_nothing_until_release() {
    let samples = Samples::new("hold");
    let states = run(&format!(
        "{}\nstate\nmove-hold line10 6.0\nstate\nrelease\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(starts(&states[0]), starts(&states[1]));
    assert!((starts(&states[2])[3] - 6.0).abs() < 0.03);
}

#[test]
fn one_control_records_and_stops_and_takes_land_after_the_last_clip() {
    let samples = Samples::new("record");
    let states = run(&format!(
        "{}\nkey r\nstate\nrecord-feed 1 0.5\nkey r\nrecord-wait\nstate\nbutton Record\nrecord-feed 0.5 0.25\nbutton Stop\nrecord-wait\nstate\nkey ctrl+z\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["recording"]["active"], true);
    assert_eq!(states[1]["recording"]["active"], false);
    assert_eq!(states[1]["recording"]["finishing"], false);
    assert_eq!(states[1]["undo"], "Record");

    let clips = states[2]["clips"].as_array().unwrap();
    let (take1, take2) = (&clips[4], &clips[5]);
    assert_eq!(take1["name"], "Recording 1");
    assert_eq!(take2["name"], "Recording 2");
    assert_eq!(take1["frames"], 48000);
    assert!((take1["start"].as_f64().unwrap() - (clips[3]["end"].as_f64().unwrap() + 0.5)).abs() < 1e-9);
    assert!((take2["peak"].as_f64().unwrap() - 0.25).abs() < 0.02);
    assert_eq!(names(&states[3]).len(), 5);
}

#[test]
fn recording_blocks_playback_and_a_take_can_be_played_and_exported() {
    let samples = Samples::new("recplay");
    let out = export_folder(&samples);
    let states = run(&format!(
        "key r\nkey space\nstate\nrecord-feed 0.5 0.5\nkey r\nrecord-wait\nkey space\nadvance 0.1\nstate\nkey space\nkey ctrl+e\nexport folder \"{out}\"\nbutton \"Export files\"\nexport-wait\nstate\n"
    ));
    assert_eq!(states[0]["playback"]["playing"], false);
    assert!((states[1]["playback"]["peak"].as_f64().unwrap() - 0.5).abs() < 0.05);
    assert_eq!(states[2]["export"]["report"]["written"], 1);
    assert!(hound::WavReader::open(samples.0.join("out").join("Recording 1.wav")).is_ok());
}

#[test]
fn an_empty_take_explains_itself() {
    let states = run("key r\nkey r\nrecord-wait\nstate\n");
    assert!(names(&states[0]).is_empty());
    assert!(states[0]["error"].as_str().unwrap().contains("no audio"));
}

fn parse_states(output: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn saving_and_opening_restores_clips_edits_and_export_settings() {
    let samples = Samples::new("save");
    let path = samples.0.join("session.kantan");
    let path = path.display();
    let states = run(&format!(
        "{}\nstate\nclip line1\nkey ctrl+l\namplify-db -6.02\nbutton Apply\nkey ctrl+e\nexport prefix VO_\nexport numbering before\nkey escape\nsave \"{path}\"\nstate\nkey ctrl+s\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["project"]["dirty"], true);
    assert!(states[0]["project"]["title"].as_str().unwrap().starts_with("*Untitled"));
    assert_eq!(states[1]["project"]["dirty"], false);
    assert!(states[1]["project"]["title"].as_str().unwrap().starts_with("session"));
    assert_eq!(states[2]["blocked_dialogs"], 0, "ctrl+s reuses the known path");

    let reopened = run(&format!("open \"{path}\"\nkey ctrl+e\nstate\n"));
    assert_eq!(names(&reopened[0]), ["intro", "line1", "line2", "line10"]);
    assert!((reopened[0]["clips"][1]["peak"].as_f64().unwrap() - 0.25).abs() < 0.01);
    assert_eq!(files(&reopened[0])[0], "VO_001_intro.wav");
    assert_eq!(reopened[0]["project"]["dirty"], false);
    assert_eq!(reopened[0]["undo"], Value::Null);
}

#[test]
fn opening_a_damaged_project_keeps_the_current_one() {
    let samples = Samples::new("damaged");
    let bad = samples.0.join("bad.kantan");
    std::fs::write(&bad, b"not a project").unwrap();
    let states = run(&format!(
        "import {}\nsave \"{}\"\nopen \"{}\"\nstate\n",
        samples.path("line1.wav"),
        samples.0.join("ok.kantan").display(),
        bad.display(),
    ));
    assert_eq!(names(&states[0]), ["line1"]);
    assert!(states[0]["error"].as_str().unwrap().contains("damaged"));
}

#[test]
fn unsaved_changes_are_asked_about_before_closing_or_opening() {
    let samples = Samples::new("prompt");
    let saved = samples.0.join("a.kantan");
    let saved = saved.display();
    let states = run(&format!(
        "close\nstate\n{}\nclose\nstate\nkey delete\nbutton Cancel\nstate\nsave \"{saved}\"\nclip line1\nkey delete\nopen \"{saved}\"\nstate\nbutton \"Don't save\"\nstate\nclip line1\nkey delete\nclose\nbutton \"Don't save\"\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["project"]["closing"], true, "nothing to save, so it just closes");
    assert_eq!(states[1]["project"]["prompt"], "unsaved");
    assert_eq!(states[2]["project"]["prompt"], Value::Null);
    assert_eq!(names(&states[2]).len(), 4);
    assert_eq!(states[3]["project"]["prompt"], "unsaved");
    assert_eq!(names(&states[4]).len(), 4, "reopened without the deletion");
    assert_eq!(states[5]["project"]["closing"], true);
}

#[test]
fn a_real_crash_is_recovered_on_the_next_run() {
    let samples = Samples::new("crash");
    let data = samples.0.join("data");
    let crashed = run_with_data(
        &format!("{}\nclip line1\nkey delete\nautosave-now\nkey r\nrecord-feed 2 0.5\nsleep 1000\ncrash\n", samples.import_line("import")),
        Some(&data),
    );
    assert_eq!(crashed.status.code(), Some(101));

    let restored = run_with_data("state\nbutton Restore\nrecord-wait\nimport-wait\nstate\n", Some(&data));
    let states = parse_states(&restored);
    assert!(restored.status.success(), "{}", String::from_utf8_lossy(&restored.stderr));
    assert_eq!(states[0]["project"]["prompt"], "recover");
    assert_eq!(names(&states[0]).len(), 0);
    assert_eq!(states[1]["project"]["prompt"], Value::Null);
    assert_eq!(states[1]["project"]["dirty"], true, "recovered work still needs saving");
    let recovered = names(&states[1]);
    assert_eq!(recovered[..3], ["intro", "line2", "line10"], "the autosaved project");
    assert_eq!(recovered[3], "Recovered take 1", "the take in progress");
    let frames = states[1]["clips"][3]["frames"].as_u64().unwrap();
    assert!(frames >= 48000, "at least the flushed audio: {frames}");
    assert!(data.join("recordings").read_dir().unwrap().next().is_none(), "take file tidied");
}

#[test]
fn discarding_recovery_leaves_nothing_to_ask_about_next_time() {
    let samples = Samples::new("discard");
    let data = samples.0.join("data");
    run_with_data(&format!("{}\nautosave-now\ncrash\n", samples.import_line("import")), Some(&data));
    let first = parse_states(&run_with_data("state\nbutton Discard\nstate\n", Some(&data)));
    assert_eq!(first[0]["project"]["prompt"], "recover");
    assert_eq!(first[1]["project"]["prompt"], Value::Null);
    assert!(names(&first[1]).is_empty());
    let second = parse_states(&run_with_data("state\n", Some(&data)));
    assert_eq!(second[0]["project"]["prompt"], Value::Null);
}

#[test]
fn a_clean_exit_leaves_nothing_to_recover() {
    let samples = Samples::new("clean");
    let data = samples.0.join("data");
    run_with_data(&format!("{}\nautosave-now\n", samples.import_line("import")), Some(&data));
    let next = parse_states(&run_with_data("state\n", Some(&data)));
    assert_eq!(next[0]["project"]["prompt"], Value::Null);
}

#[test]
fn mp3_and_ogg_export_through_the_window_and_the_format_is_saved() {
    let samples = Samples::new("formats");
    let out = export_folder(&samples);
    let project = samples.0.join("p.kantan");
    let states = run(&format!(
        "{}\nkey ctrl+e\nexport folder \"{out}\"\nexport format mp3\nstate\nbutton \"Export files\"\nexport-wait\nstate\nexport format ogg\nbutton \"Export files\"\nexport-wait\nstate\nkey escape\nsave \"{}\"\n",
        samples.import_line("import"),
        project.display(),
    ));
    assert_eq!(files(&states[0])[0], "intro.mp3");
    assert_eq!(states[1]["export"]["report"]["written"], 4);
    assert_eq!(states[2]["export"]["report"]["written"], 4);
    for name in ["line1.mp3", "line1.ogg"] {
        let path = samples.0.join("out").join(name);
        let source = kantan_audio_manager::import::import_file(&path, 48000).unwrap();
        let frames = source.frames() as i64;
        assert!((frames - 48000).abs() < 4000, "{name}: {frames}");
    }
    let reopened = run(&format!("open \"{}\"\nkey ctrl+e\nstate\n", project.display()));
    assert_eq!(files(&reopened[0])[0], "intro.ogg");
}

/// Records two lines with a one second pause between them.
const TWO_LINES: &str = "key r\nrecord-feed 1 0.5\nrecord-feed 1 0\nrecord-feed 1 0.5\nkey r\nrecord-wait\n";

#[test]
fn the_toggle_decides_whether_new_takes_are_split() {
    let off = run(&format!("{TWO_LINES}state\n"));
    assert_eq!(names(&off[0]), ["Recording 1"]);
    assert_eq!(off[0]["silence"]["on"], false);

    let on = run(&format!("button \"Split on silence\"\nstate\n{TWO_LINES}state\nkey ctrl+z\nstate\n"));
    assert_eq!(on[0]["silence"]["on"], true);
    assert_eq!(names(&on[1]), ["Recording 1", "Recording 1 2"]);
    assert!(names(&on[2]).is_empty(), "one undo removes the whole take");
}

#[test]
fn settings_change_the_result_and_are_saved() {
    let samples = Samples::new("silence");
    let path = samples.0.join("p.kantan");
    let states = run(&format!(
        "button \"Split on silence\"\nsilence -30 2\n{TWO_LINES}state\nsave \"{}\"\n",
        path.display()
    ));
    assert_eq!(names(&states[0]), ["Recording 1"], "a two second minimum ignores this pause");

    let reopened = run(&format!("open \"{}\"\nstate\n", path.display()));
    assert_eq!(reopened[0]["silence"]["on"], true);
    assert_eq!(reopened[0]["silence"]["seconds"], 2.0);
    assert_eq!(reopened[0]["silence"]["db"], -30.0);
}

#[test]
fn existing_clips_split_from_their_menu_and_explain_when_nothing_is_found() {
    let states = run(&format!(
        "{TWO_LINES}menu-clip \"Recording 1\"\nbutton \"Split at silences\"\nstate\nsilence -30 3\nkey ctrl+z\nmenu-clip \"Recording 1\"\nbutton \"Split at silences\"\nstate\n"
    ));
    assert_eq!(names(&states[0]), ["Recording 1", "Recording 1 2"]);
    assert_eq!(names(&states[1]), ["Recording 1"]);
    assert!(states[1]["error"].as_str().unwrap().contains("No pauses"));
}

#[test]
fn the_toggle_splits_imports_and_its_settings_open_on_right_click() {
    let samples = Samples::new("silenceimport");
    let picture = samples.0.join("menu.png");
    let states = run(&format!(
        "menu-button \"Split on silence\"\nstate\nscreenshot \"{}\"\n",
        picture.display()
    ));
    assert_eq!(states[0]["silence"]["on"], false, "right-click doesn't toggle");
    assert!(picture.exists());
}

/// The parts of the state that undo must restore exactly.
fn track(state: &Value) -> Vec<Value> {
    state["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| serde_json::json!([c["name"], c["start"], c["frames"], c["segments"], c["peak"]]))
        .collect()
}

#[test]
fn every_edit_undoes_and_redoes_exactly() {
    let edits = [
        ("Split", "time 1.0\nkey ctrl+i"),
        ("Merge", "clip intro\nclip line1 shift\nkey ctrl+j"),
        ("Delete", "clip line1\nkey delete"),
        ("Delete", "range 0.5 1.0\nkey delete"),
        ("Rename", "clip line1\nkey f2\ntype Renamed\nkey enter"),
        ("Move", "move line10 9.0"),
        ("Amplify", "clip line1\nkey ctrl+l\namplify-db -6\nbutton Apply"),
        ("Amplify", "range 0.5 3.0\nkey ctrl+l\namplify-db -6\nbutton Apply"),
    ];
    for (label, edit) in edits {
        let samples = Samples::new(&format!("audit{}", edit.len()));
        let states = run(&format!(
            "{}\nstate\n{edit}\nstate\nkey ctrl+z\nstate\nkey ctrl+y\nstate\nkey ctrl+z\nkey ctrl+shift+z\nstate\nbutton Undo\nstate\nbutton Redo\nstate\n",
            samples.import_line("import")
        ));
        assert_ne!(track(&states[0]), track(&states[1]), "{edit} changed nothing");
        assert_eq!(states[1]["undo"], label, "{edit}");
        assert_eq!(track(&states[2]), track(&states[0]), "undo of {edit}");
        assert_eq!(states[2]["redo"], label, "{edit}");
        assert_eq!(track(&states[3]), track(&states[1]), "redo of {edit}");
        assert_eq!(track(&states[4]), track(&states[1]), "shift redo of {edit}");
        assert_eq!(track(&states[5]), track(&states[0]), "Undo button for {edit}");
        assert_eq!(track(&states[6]), track(&states[1]), "Redo button for {edit}");
    }
}

#[test]
fn record_and_silence_splits_undo_as_one_step_and_redo() {
    let states = run(&format!(
        "button \"Split on silence\"\n{TWO_LINES}state\nkey ctrl+z\nstate\nkey ctrl+y\nstate\n"
    ));
    assert_eq!(names(&states[0]).len(), 2);
    assert!(names(&states[1]).is_empty());
    assert_eq!(track(&states[2]), track(&states[0]));
}

#[test]
fn a_new_edit_after_undo_clears_redo_and_failed_edits_add_no_history() {
    let samples = Samples::new("history");
    let states = run(&format!(
        "{}\nclip line1\nkey delete\nkey ctrl+z\nclip line2\nkey delete\nstate\nkey ctrl+y\nstate\ntime 2.2\nkey ctrl+i\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["redo"], Value::Null);
    assert_eq!(track(&states[1]), track(&states[0]));
    assert_eq!(states[2]["undo"], "Delete", "the failed split added no step");
}

#[test]
fn escape_cancels_a_drag_and_keeps_the_selection() {
    let samples = Samples::new("cancel");
    let states = run(&format!(
        "{}\nclip line1\nstate\nmove-hold line10 6.0\nkey escape\nrelease\nstate\nmove-hold line10 6.0\nkey ctrl+z\nrelease\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(track(&states[1]), track(&states[0]), "the drag did nothing");
    assert_eq!(states[1]["undo"], "Import");
    assert_eq!(selected(&states[1]), ["line1"]);
    assert!(names(&states[2]).is_empty());
    assert_eq!(states[2]["error"], Value::Null, "no complaint about the vanished clip");
}

#[test]
fn space_does_not_also_press_a_focused_button() {
    let samples = Samples::new("focus");
    let states = run(&format!(
        "{}\nbutton Play\nstate\nkey space\nstate\nkey space\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(states[0]["playback"]["playing"], true);
    assert_eq!(states[1]["playback"]["playing"], false);
    assert_eq!(states[2]["playback"]["playing"], true);
}

#[test]
fn redo_from_empty_shows_the_whole_track() {
    let samples = Samples::new("refit");
    let states = run(&format!("{}\nstate\nkey ctrl+z\nkey ctrl+y\nstate\n", samples.import_line("import")));
    assert_eq!(states[1]["view"]["frames_per_point"], states[0]["view"]["frames_per_point"]);
}

#[test]
fn a_recording_blocks_opening_and_closing_so_the_take_is_safe() {
    let samples = Samples::new("busy");
    let path = samples.0.join("p.kantan");
    let other = samples.0.join("other.kantan");
    let states = run(&format!(
        "import {}\nsave \"{}\"\nkey r\nrecord-feed 1 0.5\nopen \"{}\"\nstate\nclose\nstate\nkey r\nrecord-wait\nstate\n",
        samples.path("line1.wav"),
        other.display(),
        path.display(),
    ));
    assert_eq!(states[0]["project"]["prompt"], Value::Null);
    assert!(states[0]["error"].as_str().unwrap().contains("Stop recording"));
    assert_eq!(states[1]["project"]["closing"], false);
    assert_eq!(names(&states[2]), ["line1", "Recording 1"]);
}

#[test]
fn an_old_error_clears_once_the_next_action_works() {
    let samples = Samples::new("stale");
    let states = run(&format!(
        "{}\ntime 2.2\nkey ctrl+i\nstate\ntime 1.0\nkey ctrl+i\nstate\n",
        samples.import_line("import")
    ));
    assert!(states[0]["error"].is_string());
    assert_eq!(states[1]["error"], Value::Null);
}

#[test]
fn saving_after_undo_keeps_redo_and_the_title_marks_changes() {
    let samples = Samples::new("savehistory");
    let path = samples.0.join("p.kantan");
    let states = run(&format!(
        "{}\nclip line1\nkey delete\nkey ctrl+z\nsave \"{}\"\nstate\nkey ctrl+y\nstate\n",
        samples.import_line("import"),
        path.display(),
    ));
    assert_eq!(states[0]["project"]["dirty"], false);
    assert_eq!(states[0]["redo"], "Delete");
    assert_eq!(names(&states[1]).len(), 3);
    assert_eq!(states[1]["project"]["dirty"], true);
}

#[test]
fn every_toolbar_button_has_an_explaining_tooltip() {
    let script = [
        ("Open", "Open a project"),
        ("Save", "Save the project"),
        ("Import", "Import audio"),
        ("Export", "Export clips"),
        ("Record", "Record a new clip"),
        ("Split on silence", "Right-click for settings"),
        ("Play", "Play from the cursor"),
        ("Undo", "Nothing to undo"),
        ("Redo", "Nothing to redo"),
        ("-", "Zoom out"),
        ("+", "Zoom in"),
        ("Fit", "whole track"),
    ]
    .map(|(label, tip)| format!("expect-tip \"{label}\" \"{tip}\"\n"))
    .concat();
    run(&script);
}

#[test]
fn tooltips_follow_state() {
    let samples = Samples::new("tips");
    run(&format!(
        "{}\nexpect-tip Undo \"Undo: Import\"\nkey space\nexpect-tip Pause \"Pause\"\nkey space\nkey r\nexpect-tip Stop \"Stop recording\"\nkey r\nrecord-wait\n",
        samples.import_line("import")
    ));
}

#[test]
fn export_window_controls_explain_themselves() {
    let samples = Samples::new("tipsexport");
    run(&format!(
        "{}\nkey ctrl+e\nexpect-tip \"Choose folder…\" \"where the files go\"\nexpect-tip \"Export files\" \"Choose a folder\"\nexpect-tip Close \"settings are kept\"\n",
        samples.import_line("import")
    ));
}

#[test]
fn about_shows_real_credits_and_opens_the_notices() {
    let states = run(
        "button About\nstate\nexpect-text \"Version 0.5.0\"\nexpect-text \"Produced by Kingdom Pots\"\nexpect-text \"Developed with Claude Opus 5.5\"\nexpect-text \"MIT License\"\nexpect-text LGPL-3.0\nkey space\nkey ctrl+e\nstate\nbutton \"View notices\"\nexpect-text \"Third-party notices\"\nexpect-text \"includes the\"\nkey escape\nexpect-text \"Produced by Kingdom Pots\"\nstate\nkey escape\nstate\n",
    );
    assert_eq!(states[0]["about_open"], true);
    assert_eq!(states[1]["playback"]["playing"], false, "hotkeys stay out of it");
    assert_eq!(states[1]["export"]["open"], false);
    assert_eq!(states[2]["about_open"], true, "escape steps back to About");
    assert_eq!(states[3]["about_open"], false);
}

#[test]
fn clicking_a_clip_body_selects_it_and_places_the_cursor() {
    let samples = Samples::new("body");
    let states = run(&format!(
        "{}\ntime 3.0\nstate\nkey ctrl+i\nstate\nkey ctrl+z\nrange 0.5 1.0\ntime 2.2\nstate\ntime 0.5\ntime 3.0 ctrl\nstate\nkey delete\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(selected(&states[0]), ["line1"]);
    assert!((states[0]["cursor"].as_f64().unwrap() - 3.0).abs() < 0.02);
    assert_eq!(names(&states[1]).len(), 5, "still splits at the click");
    assert!(selected(&states[2]).is_empty(), "the gap clears the selection");
    assert_eq!(states[2]["range"], Value::Null);
    assert_eq!(selected(&states[3]), ["intro", "line1"]);
    assert_eq!(names(&states[4]), ["line2", "line10"]);
}

#[test]
fn a_selected_clip_plays_as_a_region() {
    let samples = Samples::new("region");
    let states = run(&format!(
        "{}\nclip line1\nkey space\nstate\nadvance 2\nstate\n",
        samples.import_line("import")
    ));
    assert!((states[0]["playback"]["position"].as_f64().unwrap() - 2.5).abs() < 0.001);
    assert_eq!(states[1]["playback"]["playing"], false, "stops at the clip's end");
}

#[test]
fn dragging_snaps_to_edges_selects_the_clip_and_keeps_others_still() {
    let samples = Samples::new("snap");
    // intro 0-2, line1 2.5-3.5, line2 4-5, line10 5.5-6.5.
    let states = run(&format!(
        "{}\nmove line10 5.03\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(starts(&states[0]), [0.0, 2.5, 4.0, 5.0]);
    assert_eq!(selected(&states[0]), ["line10"]);
}

#[test]
fn dropping_onto_a_clip_lands_beside_it() {
    let samples = Samples::new("beside");
    let states = run(&format!("{}\nmove line10 3.8\nstate\n", samples.import_line("import")));
    // Its centre is before line2's, so it fits in the gap only by shifting line2.
    assert_eq!(names(&states[0]), ["intro", "line1", "line10", "line2"]);
    assert_eq!(starts(&states[0]), [0.0, 2.5, 3.5, 4.5]);
}

#[test]
fn a_take_previews_live_and_becomes_a_clip_when_stopped() {
    let samples = Samples::new("live");
    let picture = samples.0.join("live.png");
    let states = run(&format!(
        "import {}\nkey r\nstate\nrecord-feed 1 0.5\nsleep 500\nscreenshot \"{}\"\nstate\nkey ctrl+z\nstate\nkey r\nrecord-wait\nstate\n",
        samples.path("line1.wav"),
        picture.display(),
    ));
    assert_eq!(states[0]["recording"]["live_levels"], 0);
    assert_eq!(states[2]["recording"]["live_levels"], 100);
    assert!((states[2]["recording"]["seconds"].as_f64().unwrap() - 1.0).abs() < 0.01);
    assert_eq!(names(&states[2]), ["line1"], "not a real clip until it stops");
    assert!(picture.exists());
    assert_eq!(states[4]["recording"]["live_levels"], Value::Null);
}

#[test]
fn escape_closes_a_menu_first_and_hotkeys_wait_behind_it() {
    let samples = Samples::new("menukeys");
    let states = run(&format!(
        "{}\nmenu-clip line1\nkey delete\nstate\nkey escape\nstate\nkey delete\nstate\n",
        samples.import_line("import")
    ));
    assert_eq!(names(&states[0]).len(), 4, "delete does nothing behind the menu");
    assert_eq!(selected(&states[1]), ["line1"], "escape only closes the menu");
    assert_eq!(names(&states[2]).len(), 3, "keys work again once it's closed");
}

#[test]
fn conflict_cancel_and_copies_preserve_existing_exports() {
    let samples = Samples::new("copies");
    let out = samples.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("intro.wav"), b"original").unwrap();
    std::fs::write(out.join("intro (2).wav"), b"existing copy").unwrap();
    let states = run(&format!(
        "{}\nkey ctrl+e\nexport folder \"{}\"\nbutton \"Export files\"\nstate\nbutton Cancel\nstate\nbutton \"Export files\"\nbutton \"Make copies\"\nexport-wait\nstate\n",
        samples.import_line("import"), out.display(),
    ));
    assert_eq!(states[0]["export"]["conflict"], true);
    assert_eq!(states[1]["export"]["conflict"], false);
    assert_eq!(states[1]["export"]["open"], true);
    assert_eq!(states[2]["export"]["report"]["written"], 4);
    assert_eq!(std::fs::read(out.join("intro.wav")).unwrap(), b"original");
    assert_eq!(std::fs::read(out.join("intro (2).wav")).unwrap(), b"existing copy");
    assert!(hound::WavReader::open(out.join("intro (3).wav")).is_ok());
}
