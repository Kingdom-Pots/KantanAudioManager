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
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_kantan-bridge"))
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
        "{}\nkey ctrl+e\nbutton \"Export files\"\nexport-wait\nstate\nexport folder \"{}\"\nbutton \"Export files\"\nexport-wait\nstate\nexport overwrite on\nbutton \"Export files\"\nexport-wait\nstate\n",
        samples.import_line("import"),
        out.display(),
    ));
    // No folder, then an existing file: nothing is written either time.
    assert_eq!(states[0]["export"]["report"], Value::Null);
    assert_eq!(states[1]["export"]["report"], Value::Null);
    assert_eq!(states[1]["export"]["files"][0]["problem"], "exists");
    // Overwriting allowed.
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
