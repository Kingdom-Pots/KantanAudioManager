//! Drives the real app UI with no window, using a small script of commands.
//!
//! Nothing touches the screen, mouse or keyboard: input is simulated inside the
//! process and screenshots are rendered offscreen. See CLAUDE.md for usage.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use kantan_audio_manager::app::App;
use kantan_audio_manager::export_dialog::FileFormat;
use kantan_audio_manager::export::{BitDepth, FileProblem, NumberPosition};
use kantan_audio_manager::peaks::clip_min_max;
use serde_json::json;

const USAGE: &str = "\
Usage: kantan-bridge [SCRIPT]    Runs a script, or reads commands from stdin.

Commands, one per line. # starts a comment, and \"quotes\" allow spaces.
  size W H                   Resize the virtual window (default 1100 420)
  import PATH...             Import files, as the file picker would, and wait
  drop PATH...               Drag and drop files onto the window, and wait
  button LABEL               Click a toolbar button by its label
  key COMBO                  Press keys, such as ctrl+z, ctrl+shift+z or escape
  clip NAME [MODS]           Click a clip's header
  time SECONDS [MODS]        Click the waveform lane at a time
  range FROM TO              Drag a range selection between two times, in seconds
  zoom SECONDS NOTCHES       Ctrl+Scroll at a time; positive notches zoom in
  pan NOTCHES                Scroll the wheel; positive notches move later in time
  scrollbar POINTS           Drag the overview's window sideways
  overview-click SECONDS     Click the overview at a time
  overview-edge left|right POINTS
                             Drag one edge of the overview's window
  menu-overview              Right-click the overview for its settings
  track-view SECONDS HEIGHT  Set the visible range and clip height
  click X Y [MODS]           Click at a position, in points
  drag X1 Y1 X2 Y2 [MODS]    Drag between positions, in points
  export KEY VALUE           Set an option in the open export window:
                             folder, prefix, suffix, rate,
                             format wav|mp3|ogg, bits 16|24, scope all|selected,
                             numbering off|before|after, start, digits
  export-wait                Wait for a running export to finish
  advance SECONDS            Pull playing audio as a sound device would;
                             state then shows the peak heard
  double NAME                Double-click a clip's header
  menu-clip NAME             Right-click a clip's header; then use button LABEL
  menu-time SECONDS          Right-click the lane at a time
  move NAME SECONDS          Drag a clip by its header to a new start time
  move-hold NAME SECONDS     Same, but keep the button held to inspect the drag
  release                    Let go of a held drag
  type TEXT                  Type into the focused text box
  amplify-db DB              Set the gain in the open amplify window
  record-feed SECONDS LEVEL  Deliver a tone to a take, as a microphone would
  record-wait                Wait for a stopped take to be added
  import-wait                Wait for an import started by something else
  sleep MILLISECONDS         Give background threads real time to work
  menu-button LABEL          Right-click a toolbar button
  silence DB SECONDS         Set what counts as a pause
  expect-tip LABEL TEXT      Hover a control and fail unless its tooltip has TEXT
  expect-text TEXT           Fail unless the window shows TEXT
  save PATH                  Save the project to a path
  open PATH                  Open a project, asking about unsaved changes
  close                      Ask to close the window
  autosave-now               Write the recovery file and wait for it
  crash                      End the process at once, with no cleanup
  state                      Print the app's state as one line of JSON
  screenshot PATH            Render the window to a PNG file

MODS are shift, ctrl, or both joined with +.
";

const DEFAULT_SIZE: [f32; 2] = [1100.0, 420.0];
/// Generous enough for scrolling to finish easing at 60 steps per second.
const MAX_SETTLE_STEPS: u64 = 240;
const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq)]
enum Command {
    Size(f32, f32),
    Import(Vec<PathBuf>),
    Drop(Vec<PathBuf>),
    Button(String),
    Key(Modifiers, egui::Key),
    Clip(String, Modifiers),
    Time(f64, Modifiers),
    Range(f64, f64),
    Zoom(f64, f32),
    Pan(f32),
    Scrollbar(f32),
    OverviewClick(f64),
    OverviewEdge(bool, f32),
    MenuOverview,
    TrackView(f32, f32),
    Click(Pos2, Modifiers),
    Drag(Pos2, Pos2, Modifiers),
    ExportSet(String, String),
    ExportWait,
    ImportWait,
    ExpectText(String),
    ExpectTip(String, String),
    MenuButton(String),
    Silence(f32, f32),
    Sleep(u64),
    Save(PathBuf),
    Open(PathBuf),
    Close,
    AutosaveNow,
    Crash,
    RecordFeed(f64, f32),
    RecordWait,
    Double(String),
    MenuClip(String),
    MenuTime(f64),
    Move(String, f64),
    MoveHold(String, f64),
    Release,
    Type(String),
    AmplifyDb(f32),
    Advance(f64),
    State,
    Screenshot(PathBuf),
}

/// Splits a line on whitespace, keeping "quoted parts" whole and stopping at a
/// comment.
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current: Option<String> = None;
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.get_or_insert_with(String::new);
            }
            '#' if !quoted => break,
            c if c.is_whitespace() && !quoted => tokens.extend(current.take()),
            c => current.get_or_insert_with(String::new).push(c),
        }
    }
    if quoted {
        return Err("unclosed quote".to_owned());
    }
    tokens.extend(current);
    Ok(tokens)
}

fn parse_mods(text: &str) -> Result<Modifiers, String> {
    let mut modifiers = Modifiers::NONE;
    for part in text.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "shift" => modifiers.shift = true,
            "ctrl" => modifiers = modifiers.plus(Modifiers::CTRL | Modifiers::COMMAND),
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(modifiers)
}

fn parse_key(text: &str) -> Result<(Modifiers, egui::Key), String> {
    let (mods, key) = match text.rsplit_once('+') {
        Some((mods, key)) => (parse_mods(mods)?, key),
        None => (Modifiers::NONE, text),
    };
    // Key names are capitalized: "a" is "A", and "escape" is "Escape".
    let mut chars = key.chars();
    let name = match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    };
    egui::Key::from_name(&name)
        .map(|key| (mods, key))
        .ok_or_else(|| format!("unknown key '{key}'"))
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("'{text}' is not a number"))
}

fn optional_mods(token: Option<&String>) -> Result<Modifiers, String> {
    token.map_or(Ok(Modifiers::NONE), |text| parse_mods(text))
}

/// Parses one line. `None` means there was nothing on it to run.
fn parse(line: &str) -> Result<Option<Command>, String> {
    let tokens = tokenize(line)?;
    let Some((name, args)) = tokens.split_first() else {
        return Ok(None);
    };
    let paths = || -> Result<Vec<PathBuf>, String> {
        if args.is_empty() {
            return Err(format!("{name} needs at least one path"));
        }
        Ok(args.iter().map(PathBuf::from).collect())
    };
    let command = match (name.as_str(), args) {
        ("size", [w, h]) => Command::Size(number(w)?, number(h)?),
        ("import", _) => Command::Import(paths()?),
        ("drop", _) => Command::Drop(paths()?),
        ("button", [label]) => Command::Button(label.clone()),
        ("key", [combo]) => {
            let (mods, key) = parse_key(combo)?;
            Command::Key(mods, key)
        }
        ("clip", [clip, rest @ ..]) if rest.len() <= 1 => {
            Command::Clip(clip.clone(), optional_mods(rest.first())?)
        }
        ("time", [seconds, rest @ ..]) if rest.len() <= 1 => {
            Command::Time(number(seconds)?, optional_mods(rest.first())?)
        }
        ("range", [from, to]) => Command::Range(number(from)?, number(to)?),
        ("zoom", [seconds, notches]) => Command::Zoom(number(seconds)?, number(notches)?),
        ("pan", [notches]) => Command::Pan(number(notches)?),
        ("scrollbar", [points]) => Command::Scrollbar(number(points)?),
        ("overview-click", [seconds]) => Command::OverviewClick(number(seconds)?),
        ("overview-edge", [side, points]) => match side.as_str() {
            "left" => Command::OverviewEdge(true, number(points)?),
            "right" => Command::OverviewEdge(false, number(points)?),
            _ => return Err(format!("overview-edge needs left or right, not '{side}'")),
        },
        ("menu-overview", []) => Command::MenuOverview,
        ("track-view", [seconds, height]) => Command::TrackView(number(seconds)?, number(height)?),
        ("click", [x, y, rest @ ..]) if rest.len() <= 1 => {
            Command::Click(pos2(number(x)?, number(y)?), optional_mods(rest.first())?)
        }
        ("drag", [x1, y1, x2, y2, rest @ ..]) if rest.len() <= 1 => Command::Drag(
            pos2(number(x1)?, number(y1)?),
            pos2(number(x2)?, number(y2)?),
            optional_mods(rest.first())?,
        ),
        ("export", [key, value]) => Command::ExportSet(key.clone(), value.clone()),
        ("export-wait", []) => Command::ExportWait,
        ("advance", [seconds]) => Command::Advance(number(seconds)?),
        ("double", [name]) => Command::Double(name.clone()),
        ("menu-clip", [name]) => Command::MenuClip(name.clone()),
        ("menu-time", [seconds]) => Command::MenuTime(number(seconds)?),
        ("move", [name, seconds]) => Command::Move(name.clone(), number(seconds)?),
        ("move-hold", [name, seconds]) => Command::MoveHold(name.clone(), number(seconds)?),
        ("release", []) => Command::Release,
        ("type", [text]) => Command::Type(text.clone()),
        ("amplify-db", [db]) => Command::AmplifyDb(number(db)?),
        ("record-feed", [seconds, level]) => Command::RecordFeed(number(seconds)?, number(level)?),
        ("record-wait", []) => Command::RecordWait,
        ("sleep", [ms]) => Command::Sleep(number(ms)?),
        ("menu-button", [label]) => Command::MenuButton(label.clone()),
        ("silence", [db, seconds]) => Command::Silence(number(db)?, number(seconds)?),
        ("expect-tip", [label, text]) => Command::ExpectTip(label.clone(), text.clone()),
        ("expect-text", [text]) => Command::ExpectText(text.clone()),
        ("import-wait", []) => Command::ImportWait,
        ("save", [path]) => Command::Save(PathBuf::from(path)),
        ("open", [path]) => Command::Open(PathBuf::from(path)),
        ("close", []) => Command::Close,
        ("autosave-now", []) => Command::AutosaveNow,
        ("crash", []) => Command::Crash,
        ("state", []) => Command::State,
        ("screenshot", [path]) => Command::Screenshot(PathBuf::from(path)),
        (other, _) => return Err(format!("unknown command or wrong arguments: '{other}'")),
    };
    Ok(Some(command))
}

/// A file dropped on the window by the simulated desktop.
#[derive(Debug)]
struct DroppedPath(PathBuf);

impl egui::DroppedFile for DroppedPath {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

struct Bridge {
    harness: Harness<'static, App>,
    last_peak: f32,
    held: Option<Pos2>,
}

impl Bridge {
    fn new() -> Self {
        let harness = Harness::builder()
            .with_size(DEFAULT_SIZE)
            .with_pixels_per_point(1.0)
            .with_os(egui::os::OperatingSystem::Windows)
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(MAX_SETTLE_STEPS)
            .with_accessibility_check(false)
            .allow_missing_glyphs()
            .with_context_setup(|ctx| {
                // The same fallback fonts the real window gets from eframe.
                ctx.add_font_provider(Arc::new(egui_system_font_provider::SystemFontProvider::new()));
            })
            .wgpu()
            .build_eframe(|creation| App::headless(&creation.egui_ctx));
        Self { harness, last_peak: 0.0, held: None }
    }

    /// Runs frames until the app has nothing left to animate.
    fn settle(&mut self) {
        // Returns None if it never settles, such as during an import's spinner.
        let _ = self.harness.run_ok();
    }

    fn wait_for_import(&mut self) -> Result<(), String> {
        let started = Instant::now();
        // The worker's result is only picked up when a frame runs.
        self.harness.step();
        while self.harness.state().is_importing() {
            if started.elapsed() > IMPORT_TIMEOUT {
                return Err("timed out waiting for the import".to_owned());
            }
            std::thread::sleep(Duration::from_millis(5));
            self.harness.step();
        }
        self.settle();
        Ok(())
    }

    fn press(&self, pos: Pos2, pressed: bool, modifiers: Modifiers) {
        self.harness.event(egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        });
    }

    fn click(&mut self, pos: Pos2, modifiers: Modifiers) {
        self.harness.event(egui::Event::ModifiersChanged(modifiers));
        self.harness.hover_at(pos);
        self.press(pos, true, modifiers);
        self.press(pos, false, modifiers);
        self.harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
        self.settle();
    }

    fn drag(&mut self, from: Pos2, to: Pos2, modifiers: Modifiers) {
        const STEPS: usize = 8;
        self.harness.event(egui::Event::ModifiersChanged(modifiers));
        self.harness.hover_at(from);
        self.press(from, true, modifiers);
        for step in 1..=STEPS {
            self.harness.hover_at(from.lerp(to, step as f32 / STEPS as f32));
        }
        self.press(to, false, modifiers);
        self.harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
        self.settle();
    }

    fn wheel(&mut self, pos: Pos2, notches: f32, modifiers: Modifiers) {
        self.harness.event(egui::Event::ModifiersChanged(modifiers));
        self.harness.hover_at(pos);
        self.harness.event(egui::Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, notches),
            phase: egui::TouchPhase::Move,
            source: MouseWheelSource::Wheel,
            modifiers,
        });
        self.harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
        self.settle();
    }

    /// Where a clip's header is, and where it must be dragged to start at a time.
    fn move_points(&self, name: &str, seconds: f64) -> Result<(Pos2, Pos2), String> {
        let from = self.header_point(name)?;
        let app = self.harness.state();
        let clip = app.document().project().clips().iter().find(|c| c.name == name);
        let start = clip.map_or(0.0, |c| c.start as f64);
        let frames = seconds * self.sample_rate() - start;
        let points = frames / app.timeline().view().frames_per_point;
        Ok((from, from + vec2(points as f32, 0.0)))
    }

    fn header_point(&self, name: &str) -> Result<Pos2, String> {
        let app = self.harness.state();
        let clip = app
            .document()
            .project()
            .clips()
            .iter()
            .find(|clip| clip.name == name)
            .ok_or_else(|| format!("no clip named '{name}'"))?;
        app.timeline()
            .clip_header_point(clip)
            .ok_or_else(|| format!("clip '{name}' is not on screen"))
    }

    fn secondary_click(&mut self, pos: Pos2) {
        self.harness.hover_at(pos);
        for pressed in [true, false] {
            self.harness.event(egui::Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        self.settle();
    }

    fn sample_rate(&self) -> f64 {
        self.harness.state().document().project().sample_rate() as f64
    }

    /// The lane position of a time, which must be on screen.
    fn point_at_time(&self, seconds: f64) -> Result<Pos2, String> {
        let frame = seconds * self.sample_rate();
        let point = self.harness.state().timeline().lane_point(frame);
        if !self.harness.ctx.content_rect().x_range().contains(point.x) {
            return Err(format!("{seconds}s is not on screen; use pan, zoom or button Fit"));
        }
        Ok(point)
    }

    /// Somewhere over the track, for scrolling without a particular time in mind.
    fn point_over_track(&self) -> Pos2 {
        let rect = self.harness.ctx.content_rect();
        pos2(rect.center().x, rect.center().y)
    }

    fn run(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Size(width, height) => {
                self.harness.set_size(vec2(width, height));
                self.settle();
            }
            Command::Import(paths) => {
                let ctx = self.harness.ctx.clone();
                self.harness.state_mut().start_import(&ctx, paths);
                self.wait_for_import()?;
            }
            Command::Drop(paths) => {
                self.harness.input_mut().dropped_files = paths
                    .into_iter()
                    .map(|path| Arc::new(DroppedPath(path)) as egui::DroppedFileHandle)
                    .collect();
                self.wait_for_import()?;
            }
            Command::Button(label) => {
                match self.harness.query_by_label(&label).or_else(|| self.harness.query_by_label_contains(&label)) {
                    Some(button) => button.click(),
                    None => return Err(format!("no button labelled '{label}'")),
                }
                self.settle();
            }
            Command::Key(modifiers, key) => {
                self.harness.key_press_modifiers(modifiers, key);
                self.settle();
            }
            Command::Clip(name, modifiers) => {
                let point = self.header_point(&name)?;
                self.click(point, modifiers);
            }
            Command::Double(name) => {
                let point = self.header_point(&name)?;
                self.harness.hover_at(point);
                for _ in 0..2 {
                    self.press(point, true, Modifiers::NONE);
                    self.press(point, false, Modifiers::NONE);
                }
                self.settle();
            }
            Command::MenuClip(name) => {
                let point = self.header_point(&name)?;
                self.secondary_click(point);
            }
            Command::MenuTime(seconds) => {
                let point = self.point_at_time(seconds)?;
                self.secondary_click(point);
            }
            Command::Move(name, seconds) => {
                let (from, to) = self.move_points(&name, seconds)?;
                self.drag(from, to, Modifiers::NONE);
            }
            Command::MoveHold(name, seconds) => {
                let (from, to) = self.move_points(&name, seconds)?;
                self.harness.hover_at(from);
                self.press(from, true, Modifiers::NONE);
                for step in 1..=8 {
                    self.harness.hover_at(from.lerp(to, step as f32 / 8.0));
                }
                self.settle();
                self.held = Some(to);
            }
            Command::Release => {
                let at = self.held.take().ok_or("nothing is being held")?;
                self.press(at, false, Modifiers::NONE);
                self.settle();
            }
            Command::Type(text) => {
                self.harness.event(egui::Event::Text(text));
                self.settle();
            }
            Command::AmplifyDb(db) => {
                if !self.harness.state_mut().set_amplify_db(db) {
                    return Err("the amplify window isn't open".to_owned());
                }
                self.settle();
            }
            Command::Time(seconds, modifiers) => {
                let point = self.point_at_time(seconds)?;
                self.click(point, modifiers);
            }
            Command::Range(from, to) => {
                let (from, to) = (self.point_at_time(from)?, self.point_at_time(to)?);
                self.drag(from, to, Modifiers::NONE);
            }
            Command::Zoom(seconds, notches) => {
                let point = self.point_at_time(seconds)?;
                self.wheel(point, notches, Modifiers::CTRL | Modifiers::COMMAND);
            }
            Command::Pan(notches) => {
                // A wheel notch down moves the content up, so later in time.
                let point = self.point_over_track();
                self.wheel(point, -notches, Modifiers::NONE);
            }
            Command::Scrollbar(points) => {
                let window = self.harness.state().timeline().overview_window_center();
                self.drag(window, window + vec2(points, 0.0), Modifiers::NONE);
            }
            Command::OverviewClick(seconds) => {
                let point = self.harness.state().timeline().overview_point(seconds * self.sample_rate());
                self.click(point, Modifiers::NONE);
            }
            Command::OverviewEdge(left, points) => {
                let edge = self.harness.state().timeline().overview_edge_point(left);
                self.drag(edge, edge + vec2(points, 0.0), Modifiers::NONE);
            }
            Command::MenuOverview => {
                let point = self.harness.state().timeline().overview_window_center();
                self.secondary_click(point);
            }
            Command::TrackView(visible_seconds, clip_height) => {
                let settings = kantan_audio_manager::timeline::TrackSettings { visible_seconds, clip_height };
                self.harness.state_mut().timeline_mut().set_settings(settings);
                self.settle();
            }
            Command::Click(pos, modifiers) => self.click(pos, modifiers),
            Command::Drag(from, to, modifiers) => self.drag(from, to, modifiers),
            Command::ExportSet(key, value) => {
                self.export_set(&key, &value)?;
                self.settle();
            }
            Command::ExportWait => {
                let started = Instant::now();
                self.harness.step();
                while self.harness.state().export_dialog().is_running() {
                    if started.elapsed() > IMPORT_TIMEOUT {
                        return Err("timed out waiting for the export".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    self.harness.step();
                }
                self.settle();
            }
            Command::Advance(seconds) => {
                let frames = (seconds * self.sample_rate()) as usize;
                self.last_peak = self.harness.state_mut().player_mut().advance(frames);
                // Playing repaints constantly, so run frames rather than settling.
                self.harness.run_steps(3);
            }
            Command::RecordFeed(seconds, level) => {
                let frames = (seconds * 44100.0) as usize;
                self.harness.state().recorder().feed(vec![level; frames]);
                self.harness.run_steps(3);
            }
            Command::RecordWait => {
                let started = Instant::now();
                self.harness.step();
                while self.harness.state().recorder().is_finishing() {
                    if started.elapsed() > IMPORT_TIMEOUT {
                        return Err("timed out waiting for the take".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    self.harness.step();
                }
                self.settle();
            }
            Command::Sleep(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Command::MenuButton(label) => {
                let center = self
                    .harness
                    .query_by_label(&label)
                    .ok_or_else(|| format!("no button labelled '{label}'"))?
                    .rect()
                    .center();
                self.secondary_click(center);
            }
            Command::Silence(threshold_db, min_silence_seconds) => {
                self.harness.state_mut().set_silence(kantan_audio_manager::SilenceSettings {
                    threshold_db,
                    min_silence_seconds,
                });
                self.settle();
            }
            Command::ExpectTip(label, text) => {
                let center = self
                    .harness
                    .query_by_label(&label)
                    .ok_or_else(|| format!("no control labelled '{label}'"))?
                    .rect()
                    .center();
                self.harness.hover_at(center);
                // Tooltips wait a moment before appearing.
                self.harness.run_steps(90);
                if self.harness.query_all_by_label_contains(&text).next().is_none() {
                    return Err(format!("hovering '{label}' showed no tooltip with '{text}'"));
                }
            }
            Command::ExpectText(text) => {
                if self.harness.query_all_by_label_contains(&text).next().is_none() {
                    return Err(format!("the window doesn't show '{text}'"));
                }
            }
            Command::ImportWait => self.wait_for_import()?,
            Command::Save(path) => {
                if !self.harness.state_mut().save_to(path) {
                    return Err("the save failed".to_owned());
                }
                self.settle();
            }
            Command::Open(path) => {
                self.harness.state_mut().request_open(path);
                self.settle();
            }
            Command::Close => {
                self.harness.state_mut().request_close();
                self.settle();
            }
            Command::AutosaveNow => {
                let done = self
                    .harness
                    .state_mut()
                    .autosave_now()
                    .ok_or("autosave needs KANTAN_DATA_DIR to be set")?;
                done.recv_timeout(IMPORT_TIMEOUT).map_err(|e| e.to_string())?;
            }
            Command::Crash => std::process::exit(101),
            Command::State => println!("{}", self.state()),
            Command::Screenshot(path) => {
                let image = self.harness.render()?;
                image.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                println!("{}", json!({ "screenshot": path }));
            }
        }
        Ok(())
    }

    fn export_set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let dialog = self.harness.state_mut().export_dialog_mut();
        if !dialog.open {
            return Err("the export window isn't open".to_owned());
        }
        let s = &mut dialog.settings;
        match (key, value) {
            ("folder", _) => s.folder = Some(PathBuf::from(value)),
            ("prefix", _) => s.names.prefix = value.to_owned(),
            ("suffix", _) => s.names.suffix = value.to_owned(),
            ("rate", _) => s.sample_rate = number(value)?,
            ("format", "wav") => s.format = FileFormat::Wav,
            ("format", "mp3") => s.format = FileFormat::Mp3,
            ("format", "ogg") => s.format = FileFormat::Ogg,
            ("bits", "16") => s.depth = BitDepth::Bits16,
            ("bits", "24") => s.depth = BitDepth::Bits24,
            ("scope", "all") => s.only_selected = false,
            ("scope", "selected") => s.only_selected = true,
            ("numbering", "off") => s.numbering_enabled = false,
            ("numbering", "before") => {
                s.numbering_enabled = true;
                s.number_position = NumberPosition::BeforeName;
            }
            ("numbering", "after") => {
                s.numbering_enabled = true;
                s.number_position = NumberPosition::AfterName;
            }
            ("start", _) => s.number_start = number(value)?,
            ("digits", _) => s.number_padding = number(value)?,
            _ => return Err(format!("bad value '{value}' for {key}")),
        }
        Ok(())
    }

    fn state(&self) -> serde_json::Value {
        let app = self.harness.state();
        let project = app.document().project();
        let rate = project.sample_rate() as f64;
        let seconds = |frames: u64| frames as f64 / rate;
        let selection = app.selection();
        let view = app.timeline().view();

        let clips: Vec<_> = project
            .clips()
            .iter()
            .map(|clip| {
                let (lo, hi) = clip_min_max(project, clip, 0, clip.len());
                let rect = app.timeline().clip_rect(clip);
                json!({
                    "name": clip.name,
                    "start": seconds(clip.start),
                    "end": seconds(clip.end()),
                    "frames": clip.len(),
                    "segments": clip.segments.len(),
                    "peak": lo.abs().max(hi.abs()),
                    "selected": selection.clips.contains(&clip.id),
                    "rect": [rect.left(), rect.top(), rect.right(), rect.bottom()],
                })
            })
            .collect();

        let dialog = app.export_dialog();
        let files: Vec<_> = dialog
            .plan(project, &selection.clips)
            .iter()
            .map(|file| {
                json!({
                    "name": file.file_name,
                    "problem": file.problem.map(|p| match p {
                        FileProblem::Duplicate => "duplicate",
                        FileProblem::Exists => "exists",
                    }),
                    "sanitized": file.sanitized,
                })
            })
            .collect();
        let report = dialog.report().map(|r| {
            json!({ "written": r.written, "failed": r.failed, "clipped": r.clipped })
        });

        let (dialog_name, amplify_db, amplify_peak) = app.dialog_state();
        let (auto_split, silence) = app.silence_state();
        json!({
            "silence": {
                "on": auto_split,
                "db": silence.threshold_db,
                "seconds": silence.min_silence_seconds,
            },
            "about_open": app.about_is_open(),
            "project": {
                "path": app.project_path(),
                "dirty": app.is_dirty(),
                "prompt": app.prompt_name(),
                "closing": app.is_closing(),
                "title": app.window_title(),
            },
            "recording": {
                "active": app.recorder().is_recording(),
                "finishing": app.recorder().is_finishing(),
                "seconds": app.recorder().seconds(),
                "live_levels": app.recorder().with_live_levels(|levels| levels.len()),
            },
            "dialog": { "open": dialog_name, "db": amplify_db, "peak": amplify_peak },
            "playback": {
                "playing": app.player().is_playing(),
                "position": app.player().position().map(seconds),
                "peak": self.last_peak,
            },
            "export": {
                "open": dialog.open,
                "running": dialog.is_running(),
                "conflict": dialog.awaiting_conflict(),
                "files": files,
                "report": report,
            },
            "sample_rate": project.sample_rate(),
            "clips": clips,
            "cursor": seconds(selection.cursor),
            "range": selection.range.map(|r| [seconds(r.start), seconds(r.end)]),
            "status": app.selection_summary(),
            "error": app.error(),
            "importing": app.is_importing(),
            "undo": app.document().undo_label(),
            "redo": app.document().redo_label(),
            "view": {
                "from": view.scroll / rate,
                "frames_per_point": view.frames_per_point,
                "visible_seconds": app.timeline().visible_seconds(),
                "lane_height": app.timeline().lane_height(),
                "overview_shown": app.timeline().overview_shown(),
                "settings": [app.timeline().settings.visible_seconds, app.timeline().settings.clip_height],
            },
            "blocked_dialogs": app.blocked_dialogs(),
        })
    }
}

/// Runs lines as they arrive, so stdin can be a live pipe. Stops at the first
/// error, because later commands would be acting on the wrong state.
fn run_script(input: impl BufRead) -> Result<(), String> {
    let mut bridge = None;
    for (index, line) in input.lines().enumerate() {
        let fail = |message: String| format!("line {}: {message}", index + 1);
        let line = line.map_err(|e| fail(e.to_string()))?;
        let Some(command) = parse(&line).map_err(fail)? else {
            continue;
        };
        bridge
            .get_or_insert_with(Bridge::new)
            .run(command)
            .map_err(fail)?;
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => run_script(std::io::stdin().lock()),
        [flag] if flag == "--help" || flag == "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        [path] => std::fs::File::open(path)
            .map_err(|e| format!("{path}: {e}"))
            .and_then(|file| run_script(std::io::BufReader::new(file))),
        _ => Err(format!("too many arguments\n\n{USAGE}")),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };

    #[test]
    fn tokens_respect_quotes_and_comments() {
        assert_eq!(tokenize("clip \"line 1\" shift # note").unwrap(), ["clip", "line 1", "shift"]);
        assert_eq!(tokenize("clip \"#1 line\"").unwrap(), ["clip", "#1 line"]);
        assert_eq!(tokenize("clip \"\"").unwrap(), ["clip", ""]);
        assert!(tokenize("   # only a comment").unwrap().is_empty());
        assert!(tokenize("clip \"oops").is_err());
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("size 800 300").unwrap(), Some(Command::Size(800.0, 300.0)));
        assert_eq!(
            parse("range 1.5 3").unwrap(),
            Some(Command::Range(1.5, 3.0))
        );
        assert_eq!(
            parse("clip intro ctrl+shift").unwrap(),
            Some(Command::Clip("intro".to_owned(), CTRL | Modifiers::SHIFT))
        );
        assert_eq!(
            parse("click 10 20").unwrap(),
            Some(Command::Click(pos2(10.0, 20.0), Modifiers::NONE))
        );
    }

    #[test]
    fn keys_parse_in_any_case() {
        assert_eq!(parse_key("ctrl+z").unwrap(), (CTRL, egui::Key::Z));
        assert_eq!(parse_key("ctrl+shift+Z").unwrap(), (CTRL | Modifiers::SHIFT, egui::Key::Z));
        assert_eq!(parse_key("escape").unwrap(), (Modifiers::NONE, egui::Key::Escape));
        assert!(parse_key("ctrl+notakey").is_err());
        assert!(parse_key("hyper+z").is_err());
    }

    #[test]
    fn bad_commands_explain_themselves() {
        assert!(parse("nonsense").unwrap_err().contains("nonsense"));
        assert!(parse("size 10").is_err());
        assert!(parse("size a b").unwrap_err().contains("'a'"));
        assert!(parse("import").unwrap_err().contains("path"));
        assert!(parse("clip a shift extra").is_err());
    }
}
