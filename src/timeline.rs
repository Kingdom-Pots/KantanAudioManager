//! The track view: ruler, clips with waveforms, selection and zoom.

use eframe::egui::{
    self,
    Align2, Color32, CornerRadius, FontId, Modifiers, Painter, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, pos2, vec2,
};

use egui_phosphor::regular as icons;

use crate::peaks::clip_min_max;
use crate::{Clip, ClipId, Frames, Project, TimeRange};

const RULER_HEIGHT: f32 = 24.0;
const SCROLLBAR_HEIGHT: f32 = 14.0;
const CLIP_HEADER_HEIGHT: f32 = 22.0;
const LANE_MARGIN: f32 = 8.0;
/// Closest zoom: each frame is 16 points wide.
const MIN_FRAMES_PER_POINT: f64 = 1.0 / 16.0;
const MIN_TICK_SPACING: f64 = 90.0;
/// How close, on screen, a dragged edge must be to snap.
const SNAP_POINTS: f64 = 8.0;
const GHOST_OPACITY: f32 = 0.65;
const DRAGGED_ORIGINAL_OPACITY: f32 = 0.35;

const BACKGROUND: Color32 = Color32::from_rgb(24, 26, 30);
const RULER: Color32 = Color32::from_rgb(34, 37, 43);
const TICK: Color32 = Color32::from_rgb(70, 75, 85);
const LABEL: Color32 = Color32::from_rgb(170, 176, 186);
const CLIP_BODY_SELECTED: Color32 = Color32::from_rgb(46, 66, 98);
const LIVE_BODY: Color32 = Color32::from_rgb(64, 32, 36);
const LIVE_HEADER: Color32 = Color32::from_rgb(170, 50, 45);
const LIVE_WAVE: Color32 = Color32::from_rgb(245, 130, 120);
const CLIP_BODY: Color32 = Color32::from_rgb(38, 46, 58);
const CLIP_HEADER: Color32 = Color32::from_rgb(58, 70, 88);
const CLIP_HEADER_SELECTED: Color32 = Color32::from_rgb(52, 120, 200);
const CLIP_EDGE: Color32 = Color32::from_rgb(135, 150, 172);
const CLIP_BORDER_SELECTED: Color32 = Color32::from_rgb(110, 175, 245);
const WAVEFORM: Color32 = Color32::from_rgb(120, 190, 235);
const WAVEFORM_OVER: Color32 = Color32::from_rgb(235, 90, 80);
// Premultiplied: white at about 18% opacity.
const RANGE: Color32 = Color32::from_rgba_premultiplied(45, 45, 45, 45);
const PLAYHEAD: Color32 = Color32::from_rgb(110, 225, 130);
const CURSOR: Color32 = Color32::from_rgb(245, 245, 245);
const SCROLL_TRACK: Color32 = Color32::from_rgb(34, 37, 43);
const SCROLL_THUMB: Color32 = Color32::from_rgb(85, 92, 104);

/// What part of the timeline is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// Frame at the left edge of the lane.
    pub scroll: f64,
    pub frames_per_point: f64,
}

impl View {
    pub fn frame_at(&self, offset: f64) -> f64 {
        self.scroll + offset * self.frames_per_point
    }

    pub fn offset_of(&self, frame: f64) -> f64 {
        (frame - self.scroll) / self.frames_per_point
    }

    /// Keeps the view on the timeline and within the zoom limits.
    pub fn clamp(&mut self, width: f64, extent: f64) {
        let widest = (extent / width.max(1.0)).max(MIN_FRAMES_PER_POINT);
        self.frames_per_point = self.frames_per_point.clamp(MIN_FRAMES_PER_POINT, widest);
        let last = (extent - width * self.frames_per_point).max(0.0);
        self.scroll = self.scroll.clamp(0.0, last);
    }

    /// Zooms so the frame under `anchor` stays where it is.
    pub fn zoom(&mut self, factor: f64, anchor: f64, width: f64, extent: f64) {
        let frame = self.frame_at(anchor);
        self.frames_per_point /= factor;
        self.clamp(width, extent);
        self.scroll = frame - anchor * self.frames_per_point;
        self.clamp(width, extent);
    }

    pub fn fit(&mut self, width: f64, extent: f64) {
        self.scroll = 0.0;
        self.frames_per_point = extent / width.max(1.0);
        self.clamp(width, extent);
    }
}

/// How far the timeline can be viewed: a little past the last clip, and never
/// less than ten seconds so a first short clip doesn't fill the window.
fn extent(project: &Project) -> f64 {
    let end = project.clips().last().map_or(0, Clip::end) as f64;
    (end * 1.02).max(project.sample_rate() as f64 * 10.0)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub clips: Vec<ClipId>,
    anchor: Option<ClipId>,
    pub range: Option<TimeRange>,
    pub cursor: Frames,
}

impl Selection {
    /// Handles clicking a clip's header. Ctrl toggles it, and shift selects
    /// everything between it and the last clip clicked.
    pub fn click_clip(&mut self, project: &Project, clip: &Clip, modifiers: Modifiers) {
        self.range = None;
        self.cursor = clip.start;

        let anchor_index = self.anchor.and_then(|a| index_of(project, a));
        match (modifiers.shift, anchor_index, index_of(project, clip.id)) {
            (true, Some(from), Some(to)) => {
                self.clips = project.clips()[from.min(to)..=from.max(to)]
                    .iter()
                    .map(|c| c.id)
                    .collect();
            }
            _ if modifiers.command => {
                if self.clips.contains(&clip.id) {
                    self.clips.retain(|&id| id != clip.id);
                } else {
                    self.clips.push(clip.id);
                }
                self.anchor = Some(clip.id);
            }
            _ => {
                self.clips = vec![clip.id];
                self.anchor = Some(clip.id);
            }
        }
    }

    /// Handles clicking a clip's body. It selects the clip and places the cursor
    /// at the click, so splitting and playing start from there. Shift extends a
    /// range from the cursor instead, and ctrl toggles the clip.
    pub fn click_body(&mut self, project: &Project, clip: &Clip, frame: Frames, modifiers: Modifiers) {
        if modifiers.shift {
            self.click_position(frame, true);
        } else {
            self.click_clip(project, clip, modifiers);
            self.cursor = frame;
        }
    }

    /// Handles clicking somewhere other than a clip's header. Shift extends a
    /// range from the cursor, and a plain click moves the cursor.
    pub fn click_position(&mut self, frame: Frames, shift: bool) {
        self.clips.clear();
        self.anchor = None;
        if shift {
            self.set_range(self.cursor, frame);
        } else {
            self.range = None;
            self.cursor = frame;
        }
    }

    /// Sets the range selection, which replaces any selected clips.
    pub fn set_range(&mut self, a: Frames, b: Frames) {
        self.clips.clear();
        self.anchor = None;
        self.range = (a != b).then(|| TimeRange::new(a, b));
    }

    pub fn select_all(&mut self, project: &Project) {
        self.range = None;
        self.clips = project.clips().iter().map(|c| c.id).collect();
        self.anchor = self.clips.first().copied();
    }

    pub fn clear(&mut self) {
        self.clips.clear();
        self.anchor = None;
        self.range = None;
    }

    /// Forgets clips that an undo or redo removed.
    pub fn retain_existing(&mut self, project: &Project) {
        self.clips.retain(|&id| project.clip(id).is_some());
        if self.anchor.is_some_and(|id| project.clip(id).is_none()) {
            self.anchor = None;
        }
    }
}

fn index_of(project: &Project, id: ClipId) -> Option<usize> {
    project.clips().iter().position(|c| c.id == id)
}

/// Formats a position as `m:ss.mmm`.
pub fn format_time(frames: Frames, sample_rate: u32) -> String {
    let millis = frames * 1000 / sample_rate as u64;
    format!("{}:{:02}.{:03}", millis / 60_000, millis / 1000 % 60, millis % 1000)
}

/// The smallest round time step whose ticks are at least `MIN_TICK_SPACING`
/// apart at this zoom, in frames.
fn tick_step(frames_per_point: f64, sample_rate: u32) -> f64 {
    const SECONDS: [f64; 21] = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0,
        60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
    ];
    let wanted = MIN_TICK_SPACING * frames_per_point / sample_rate as f64;
    let seconds = SECONDS
        .iter()
        .copied()
        .find(|&s| s >= wanted)
        .unwrap_or_else(|| (wanted / 3600.0).ceil() * 3600.0);
    seconds * sample_rate as f64
}

/// The take being recorded, drawn where it will land.
pub struct LivePreview<'a> {
    pub name: &'a str,
    pub start: Frames,
    pub len: Frames,
    /// One (min, max) per `frames_per_level` timeline frames.
    pub levels: &'a [(f32, f32)],
    pub frames_per_level: f64,
}

/// Something the user asked for on the track, for the app to carry out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimelineAction {
    /// A clip, the requested start, and the snapping distance in frames.
    Move(ClipId, Frames, Frames),
    Rename(ClipId),
    Split(Frames),
    Merge,
    Amplify,
    Delete,
    Export,
    SplitOnSilence,
}

#[derive(Clone, Copy)]
enum Drag {
    None,
    Range { start: Frames },
    Clip { id: ClipId, grab: Frames, start: Frames },
    Scrollbar { grab: f64 },
}

/// Screen areas of the timeline.
struct Areas {
    ruler: Rect,
    lane: Rect,
    scrollbar: Rect,
}

impl Areas {
    fn new(rect: Rect) -> Self {
        let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_HEIGHT));
        let scrollbar = Rect::from_min_max(
            pos2(rect.left(), rect.bottom() - SCROLLBAR_HEIGHT),
            rect.max,
        );
        let lane = Rect::from_min_max(pos2(rect.left(), ruler.bottom()), pos2(rect.right(), scrollbar.top()));
        Self { ruler, lane, scrollbar }
    }

    fn clip_rect(&self, view: &View, clip: &Clip) -> Rect {
        let left = self.lane.left() + view.offset_of(clip.start as f64) as f32;
        let right = self.lane.left() + view.offset_of(clip.end() as f64) as f32;
        Rect::from_min_max(
            pos2(left, self.lane.top() + LANE_MARGIN),
            pos2(right.max(left + 1.0), self.lane.bottom() - LANE_MARGIN),
        )
    }
}

pub struct Timeline {
    view: View,
    drag: Drag,
    fit_requested: bool,
    /// Lane width from the last frame, for zoom requests made outside it.
    width: f64,
    extent: f64,
    /// Screen area from the last frame.
    rect: Rect,
    action: Option<TimelineAction>,
    /// Where the right-click menu was opened.
    menu_frame: Frames,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            view: View { scroll: 0.0, frames_per_point: 100.0 },
            drag: Drag::None,
            fit_requested: true,
            width: 1.0,
            extent: 1.0,
            rect: Rect::NOTHING,
            action: None,
            menu_frame: 0,
        }
    }
}

impl Timeline {
    /// Abandons a drag in progress, returning whether there was one.
    pub fn cancel_drag(&mut self) -> bool {
        !matches!(std::mem::replace(&mut self.drag, Drag::None), Drag::None)
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Where a clip is on screen as of the last frame. It can be partly or
    /// entirely outside the lane when zoomed in.
    pub fn clip_rect(&self, clip: &Clip) -> Rect {
        Areas::new(self.rect).clip_rect(&self.view, clip)
    }

    /// A point on the part of a clip's header that is on screen, if any.
    pub fn clip_header_point(&self, clip: &Clip) -> Option<Pos2> {
        let lane = Areas::new(self.rect).lane;
        let rect = self.clip_rect(clip);
        let left = rect.left().max(lane.left());
        let right = rect.right().min(lane.right());
        (left < right).then(|| pos2((left + right) / 2.0, rect.top() + CLIP_HEADER_HEIGHT / 2.0))
    }

    /// The screen position of a timeline frame, halfway down the waveform lane.
    pub fn lane_point(&self, frame: f64) -> Pos2 {
        let lane = Areas::new(self.rect).lane;
        let x = lane.left() + self.view.offset_of(frame) as f32;
        pos2(x, lane.top() + lane.height() * 0.6)
    }

    /// The middle of the scrollbar's thumb as of the last frame.
    pub fn scrollbar_thumb_center(&self) -> Pos2 {
        self.scrollbar_thumb(&Areas::new(self.rect)).center()
    }

    pub fn request_fit(&mut self) {
        self.fit_requested = true;
    }

    /// Zooms around the middle of the view, for the toolbar buttons.
    pub fn zoom_centered(&mut self, factor: f64) {
        self.view.zoom(factor, self.width / 2.0, self.width, self.extent);
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        selection: &mut Selection,
        playhead: Option<Frames>,
        live: Option<LivePreview<'_>>,
    ) -> Option<TimelineAction> {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let areas = Areas::new(rect);
        self.rect = rect;

        self.width = areas.lane.width() as f64;
        self.extent = extent(project);
        if let Some(live) = &live {
            self.extent = self.extent.max((live.start + live.len) as f64 * 1.02);
        }
        if std::mem::take(&mut self.fit_requested) {
            self.view.fit(self.width, self.extent);
        }
        self.handle_zoom_and_scroll(ui, &response, &areas);
        self.view.clamp(self.width, self.extent);
        self.handle_pointer(ui, &response, &areas, project, selection);
        self.context_menu(&response, project, selection);
        self.update_pointer_icon(ui, &response, &areas, project);
        let hint = self.hover_hint(&response, &areas, project);
        if let Some(hint) = hint {
            response.clone().on_hover_text_at_pointer(hint);
        }
        if let Some(playhead) = playhead {
            self.follow(playhead as f64);
        }
        if let Some(live) = &live {
            self.follow_recording((live.start + live.len) as f64);
        }
        self.view.clamp(self.width, self.extent);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        let dragged = match self.drag {
            Drag::Clip { id, .. } => Some(id),
            _ => None,
        };
        for clip in project.clips() {
            // The original stays dimmed in place while its ghost is dragged.
            let mut clip_painter = painter.clone();
            if dragged == Some(clip.id) {
                clip_painter.set_opacity(DRAGGED_ORIGINAL_OPACITY);
            }
            self.paint_clip(&clip_painter, &areas, project, clip, selection.clips.contains(&clip.id));
        }
        if let Some(live) = &live {
            self.paint_live(&painter, &areas, live);
        }
        self.paint_selection(&painter, &areas, selection);
        self.paint_ruler(&painter, &areas, project.sample_rate());
        if let Some(playhead) = playhead {
            let x = areas.lane.left() + self.view.offset_of(playhead as f64) as f32;
            painter.line_segment(
                [pos2(x, areas.ruler.top()), pos2(x, areas.lane.bottom())],
                Stroke::new(1.5, PLAYHEAD),
            );
        }
        self.paint_scrollbar(&painter, &areas);
        self.paint_clip_drag(&painter, &areas, project);
        self.action.take()
    }

    /// Shows what a drag would do: move a clip, or select a range.
    fn update_pointer_icon(&self, ui: &Ui, response: &Response, areas: &Areas, project: &Project) {
        let icon = if matches!(self.drag, Drag::Clip { .. }) {
            Some(egui::CursorIcon::Grabbing)
        } else {
            response.hover_pos().and_then(|pos| {
                if self.clip_header_at(areas, project, pos).is_some() {
                    Some(egui::CursorIcon::Grab)
                } else if areas.lane.contains(pos) {
                    Some(egui::CursorIcon::Text)
                } else {
                    None
                }
            })
        };
        if let Some(icon) = icon {
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Explains what the part of the track under the pointer does.
    fn hover_hint(&self, response: &Response, areas: &Areas, project: &Project) -> Option<&'static str> {
        if !matches!(self.drag, Drag::None) {
            return None;
        }
        let pos = response.hover_pos()?;
        if areas.scrollbar.contains(pos) {
            Some("Drag to scroll along the track")
        } else if areas.ruler.contains(pos) {
            Some("Click to move the cursor here. Ctrl+Scroll zooms.")
        } else if self.clip_header_at(areas, project, pos).is_some() {
            Some("Drag to move the clip. Click to select it, with Ctrl or Shift to add more. Double-click to rename. Right-click for more.")
        } else if self.clip_at(areas, project, pos).is_some() {
            Some("Click to select the clip and place the cursor. Drag to select a range. Right-click for more.")
        } else {
            Some("Click to place the cursor. Drag to select a range.")
        }
    }

    fn context_menu(&mut self, response: &Response, project: &Project, selection: &Selection) {
        let frame = self.menu_frame;
        let mut action = None;
        response.context_menu(|ui| {
            let inside = project.clips().iter().any(|c| frame > c.start && frame < c.end());
            let single = match selection.clips.as_slice() {
                [id] => Some(*id),
                _ => None,
            };
            let has_target = selection.range.is_some() || !selection.clips.is_empty();
            let mut item = |ui: &mut Ui, enabled: bool, icon: &str, label: &str, key: &str, tip: &str, chosen: TimelineAction| {
                let button = egui::Button::new(format!("{icon}  {label}")).shortcut_text(key);
                if ui.add_enabled(enabled, button).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                    action = Some(chosen);
                    ui.close();
                }
            };
            item(ui, inside, icons::SCISSORS, "Split here", "Ctrl+I", "Cut the clip in two at this point. Right-click inside a clip to use it.", TimelineAction::Split(frame));
            item(ui, selection.clips.len() >= 2, icons::ARROWS_MERGE, "Merge", "Ctrl+J", "Join the selected clips into one. Select two or more side by side.", TimelineAction::Merge);
            let rename = "Change the clip's name, which is also its exported filename. Select one clip.";
            item(ui, single.is_some(), icons::PENCIL_SIMPLE, "Rename", "F2", rename, single.map_or(TimelineAction::Merge, TimelineAction::Rename));
            item(ui, !selection.clips.is_empty(), icons::WAVEFORM, "Split at silences", "", "Cut the selected clips into one clip per line at pauses. Set what counts as a pause by right-clicking the Split on silence button.", TimelineAction::SplitOnSilence);
            item(ui, has_target, icons::SPEAKER_HIGH, "Amplify", "Ctrl+L", "Make the selected clips or range louder or quieter.", TimelineAction::Amplify);
            item(ui, has_target, icons::TRASH, "Delete", "Del", "Remove the selected clips or range. The space stays empty.", TimelineAction::Delete);
            ui.separator();
            item(ui, true, icons::EXPORT, "Export", "Ctrl+E", "Export clips to audio files.", TimelineAction::Export);
        });
        if action.is_some() {
            self.action = action;
        }
    }

    /// Draws a see-through copy of the dragged clip, following the pointer.
    fn paint_clip_drag(&self, painter: &Painter, areas: &Areas, project: &Project) {
        let Drag::Clip { id, start, .. } = self.drag else { return };
        let Some(clip) = project.clip(id) else { return };
        // The ghost shows where the clip will really land.
        let start = project.landing(id, start, self.snap_frames()).unwrap_or(start);
        let moved = Clip { start, ..clip.clone() };

        // A guide marks an edge that lines up with another clip.
        for frame in [start, moved.end()] {
            let aligned = project
                .clips()
                .iter()
                .filter(|c| c.id != id)
                .any(|c| c.start == frame || c.end() == frame);
            if aligned {
                let x = areas.lane.left() + self.view.offset_of(frame as f64) as f32;
                painter.line_segment(
                    [pos2(x, areas.lane.top()), pos2(x, areas.lane.bottom())],
                    Stroke::new(1.0, PLAYHEAD),
                );
            }
        }
        let mut ghost = painter.with_clip_rect(areas.lane);
        ghost.set_opacity(GHOST_OPACITY);
        self.paint_clip(&ghost, areas, project, &moved, true);
        painter.with_clip_rect(areas.lane).rect_stroke(
            areas.clip_rect(&self.view, &moved),
            CornerRadius::same(3),
            Stroke::new(1.5, CURSOR),
            StrokeKind::Inside,
        );
    }

    /// Keeps the growing end of a recording on screen, sliding smoothly.
    fn follow_recording(&mut self, end: f64) {
        let visible = self.width * self.view.frames_per_point;
        if end > self.view.scroll + visible * 0.95 {
            self.view.scroll = end - visible * 0.95;
        } else if end < self.view.scroll {
            self.view.scroll = (end - visible * 0.5).max(0.0);
        }
    }

    fn paint_live(&self, painter: &Painter, areas: &Areas, live: &LivePreview<'_>) {
        let clip_area = |start: f64, end: f64| {
            let left = areas.lane.left() + self.view.offset_of(start) as f32;
            let right = areas.lane.left() + self.view.offset_of(end) as f32;
            Rect::from_min_max(
                pos2(left, areas.lane.top() + LANE_MARGIN),
                pos2(right.max(left + 1.0), areas.lane.bottom() - LANE_MARGIN),
            )
        };
        let rect = clip_area(live.start as f64, (live.start + live.len) as f64);
        let painter = painter.with_clip_rect(areas.lane);
        let radius = CornerRadius::same(3);
        painter.rect_filled(rect, radius, LIVE_BODY);
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), CLIP_HEADER_HEIGHT));
        painter.rect_filled(header, radius, LIVE_HEADER);
        painter.with_clip_rect(header.intersect(areas.lane)).text(
            pos2(rect.left().max(areas.lane.left()) + 6.0, header.center().y),
            Align2::LEFT_CENTER,
            live.name,
            FontId::proportional(13.0),
            Color32::WHITE,
        );

        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        let center = body.center().y;
        let half_height = body.height() / 2.0 - 3.0;
        let first = body.left().max(areas.lane.left()).floor() as i32;
        let last = body.right().min(areas.lane.right()).ceil() as i32;
        for x in first..last {
            let offset = (x as f32 - areas.lane.left()) as f64;
            let from = (self.view.frame_at(offset) - live.start as f64).max(0.0) / live.frames_per_level;
            let to = (self.view.frame_at(offset + 1.0) - live.start as f64) / live.frames_per_level;
            let (from, to) = (from as usize, (to.ceil() as usize).max(from as usize + 1));
            let Some(levels) = live.levels.get(from..to.min(live.levels.len())) else { continue };
            if levels.is_empty() {
                continue;
            }
            let (lo, hi) = levels.iter().fold((f32::MAX, f32::MIN), |(a, b), &(l, h)| (a.min(l), b.max(h)));
            let top = center - hi.clamp(-1.0, 1.0) * half_height;
            let bottom = center - lo.clamp(-1.0, 1.0) * half_height;
            let (top, bottom) = if bottom - top < 2.0 {
                let middle = ((top + bottom) / 2.0).round();
                (middle - 1.0, middle + 1.0)
            } else {
                (top, bottom)
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(x as f32, top), pos2(x as f32 + 1.0, bottom)),
                CornerRadius::ZERO,
                LIVE_WAVE,
            );
        }
        painter.rect_stroke(rect, radius, Stroke::new(1.5, LIVE_WAVE), StrokeKind::Inside);
    }

    /// Turns the page when the playhead leaves the view.
    fn follow(&mut self, frame: f64) {
        if frame < self.view.scroll || frame > self.view.frame_at(self.width) {
            self.view.scroll = frame;
        }
    }

    fn handle_zoom_and_scroll(&mut self, ui: &Ui, response: &Response, areas: &Areas) {
        if !response.hovered() {
            return;
        }
        let (zoom, scroll, pointer) = ui.input(|i| {
            (i.zoom_delta() as f64, i.smooth_scroll_delta, i.pointer.hover_pos())
        });
        if zoom != 1.0 {
            let anchor = pointer.map_or(self.width / 2.0, |p| (p.x - areas.lane.left()) as f64);
            self.view.zoom(zoom, anchor, self.width, self.extent);
        }
        self.view.scroll -= (scroll.x + scroll.y) as f64 * self.view.frames_per_point;
    }

    fn handle_pointer(
        &mut self,
        ui: &Ui,
        response: &Response,
        areas: &Areas,
        project: &Project,
        selection: &mut Selection,
    ) {
        let modifiers = ui.input(|i| i.modifiers);
        let frame_at = |view: &View, x: f32| {
            view.frame_at((x - areas.lane.left()) as f64).max(0.0).round() as Frames
        };

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(areas.lane.center());
            self.drag = if areas.scrollbar.contains(origin) {
                Drag::Scrollbar { grab: self.scrollbar_grab(areas, origin.x) }
            } else if let Some(clip) = self.clip_header_at(areas, project, origin) {
                let grabbed = frame_at(&self.view, origin.x);
                Drag::Clip { id: clip.id, grab: grabbed.saturating_sub(clip.start), start: clip.start }
            } else {
                Drag::Range { start: frame_at(&self.view, origin.x) }
            };
        }

        if response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            match self.drag {
                Drag::None => {}
                Drag::Range { start } => {
                    self.scroll_past_edges(ui, areas, pointer.x);
                    let x = pointer.x.clamp(areas.lane.left(), areas.lane.right());
                    selection.set_range(start, frame_at(&self.view, x));
                }
                Drag::Scrollbar { grab } => self.drag_scrollbar(areas, pointer.x, grab),
                Drag::Clip { id, grab, .. } => {
                    self.scroll_past_edges(ui, areas, pointer.x);
                    let start = frame_at(&self.view, pointer.x).saturating_sub(grab);
                    self.drag = Drag::Clip { id, grab, start };
                }
            }
        }

        if response.drag_stopped() {
            if let Drag::Clip { id, start, .. } = self.drag {
                self.action = Some(TimelineAction::Move(id, start, self.snap_frames()));
                if !selection.clips.contains(&id)
                    && let Some(clip) = project.clip(id)
                {
                    let cursor = selection.cursor;
                    selection.click_clip(project, clip, Modifiers::NONE);
                    selection.cursor = cursor;
                }
            }
            self.drag = Drag::None;
        }

        if response.secondary_clicked()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.menu_frame = frame_at(&self.view, pointer.x);
            // The menu acts on the selection, so a clip outside it joins by replacing it.
            let in_range = selection.range.is_some_and(|r| self.menu_frame >= r.start && self.menu_frame < r.end);
            if let Some(clip) = self.clip_at(areas, project, pointer)
                && !selection.clips.contains(&clip.id)
                && !in_range
            {
                selection.click_body(project, clip, self.menu_frame, Modifiers::NONE);
            }
        }

        if response.double_clicked()
            && let Some(pointer) = response.interact_pointer_pos()
            && let Some(clip) = self.clip_header_at(areas, project, pointer)
        {
            self.action = Some(TimelineAction::Rename(clip.id));
        }

        if response.clicked() {
            let Some(pointer) = response.interact_pointer_pos() else { return };
            if areas.scrollbar.contains(pointer) {
                let thumb = self.scrollbar_thumb(areas);
                self.drag_scrollbar(areas, pointer.x, thumb.width() as f64 / 2.0);
            } else if let Some(clip) = self.clip_header_at(areas, project, pointer) {
                selection.click_clip(project, clip, modifiers);
            } else if let Some(clip) = self.clip_at(areas, project, pointer) {
                selection.click_body(project, clip, frame_at(&self.view, pointer.x), modifiers);
            } else {
                selection.click_position(frame_at(&self.view, pointer.x), modifiers.shift);
            }
        }
    }

    fn snap_frames(&self) -> Frames {
        (SNAP_POINTS * self.view.frames_per_point) as Frames
    }

    /// Scrolls while a drag is held past either edge of the lane.
    fn scroll_past_edges(&mut self, ui: &Ui, areas: &Areas, x: f32) {
        let past_left = (areas.lane.left() - x).max(0.0);
        let past_right = (x - areas.lane.right()).max(0.0);
        if past_left > 0.0 || past_right > 0.0 {
            self.view.scroll += (past_right - past_left) as f64 * self.view.frames_per_point * 0.2;
            ui.ctx().request_repaint();
        }
    }

    fn clip_at<'a>(&self, areas: &Areas, project: &'a Project, pos: Pos2) -> Option<&'a Clip> {
        if !areas.lane.contains(pos) {
            return None;
        }
        project.clips().iter().find(|clip| areas.clip_rect(&self.view, clip).contains(pos))
    }

    fn clip_header_at<'a>(&self, areas: &Areas, project: &'a Project, pos: Pos2) -> Option<&'a Clip> {
        if !areas.lane.contains(pos) {
            return None;
        }
        project.clips().iter().find(|clip| {
            let rect = areas.clip_rect(&self.view, clip);
            rect.x_range().contains(pos.x)
                && pos.y >= rect.top()
                && pos.y < rect.top() + CLIP_HEADER_HEIGHT
        })
    }

    fn scrollbar_thumb(&self, areas: &Areas) -> Rect {
        let track = areas.scrollbar;
        let width = track.width() as f64;
        let visible = self.width * self.view.frames_per_point;
        let thumb_width = (visible / self.extent * width).clamp(24.0_f64.min(width), width);
        let travel = width - thumb_width;
        let scrollable = (self.extent - visible).max(1.0);
        let left = track.left() as f64 + self.view.scroll / scrollable * travel;
        Rect::from_min_size(
            pos2(left as f32, track.top()),
            vec2(thumb_width as f32, track.height()),
        )
    }

    fn scrollbar_grab(&self, areas: &Areas, x: f32) -> f64 {
        let thumb = self.scrollbar_thumb(areas);
        if thumb.x_range().contains(x) {
            (x - thumb.left()) as f64
        } else {
            thumb.width() as f64 / 2.0
        }
    }

    fn drag_scrollbar(&mut self, areas: &Areas, x: f32, grab: f64) {
        let thumb = self.scrollbar_thumb(areas);
        let travel = (areas.scrollbar.width() - thumb.width()) as f64;
        if travel <= 0.0 {
            return;
        }
        let visible = self.width * self.view.frames_per_point;
        let fraction = (x as f64 - areas.scrollbar.left() as f64 - grab) / travel;
        self.view.scroll = fraction * (self.extent - visible);
    }

    fn paint_clip(&self, painter: &Painter, areas: &Areas, project: &Project, clip: &Clip, selected: bool) {
        let rect = areas.clip_rect(&self.view, clip);
        if rect.right() < areas.lane.left() || rect.left() > areas.lane.right() {
            return;
        }

        let radius = CornerRadius::same(3);
        painter.rect_filled(rect, radius, if selected { CLIP_BODY_SELECTED } else { CLIP_BODY });

        let header = Rect::from_min_size(rect.min, vec2(rect.width(), CLIP_HEADER_HEIGHT));
        let header_color = if selected { CLIP_HEADER_SELECTED } else { CLIP_HEADER };
        painter.rect_filled(header, radius, header_color);
        // Keep the name visible while its clip is scrolled partly off screen.
        let name_x = rect.left().max(areas.lane.left()) + 6.0;
        painter.with_clip_rect(header.intersect(areas.lane)).text(
            pos2(name_x, header.center().y),
            Align2::LEFT_CENTER,
            &clip.name,
            FontId::proportional(13.0),
            Color32::WHITE,
        );

        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        self.paint_waveform(painter, areas, project, clip, body);

        // A border on every clip keeps touching clips distinct, such as after a split.
        let border = if selected {
            Stroke::new(2.0, CLIP_BORDER_SELECTED)
        } else {
            Stroke::new(1.0, CLIP_EDGE)
        };
        painter.rect_stroke(rect, radius, border, StrokeKind::Inside);
    }

    fn paint_waveform(&self, painter: &Painter, areas: &Areas, project: &Project, clip: &Clip, body: Rect) {
        let center = body.center().y;
        let half_height = body.height() / 2.0 - 3.0;
        if half_height <= 0.0 {
            return;
        }

        let first = body.left().max(areas.lane.left()).floor() as i32;
        let last = body.right().min(areas.lane.right()).ceil() as i32;
        for x in first..last {
            let offset = (x as f32 - areas.lane.left()) as f64;
            let from = self.view.frame_at(offset).floor();
            let to = self.view.frame_at(offset + 1.0).floor().max(from + 1.0);
            let local_start = from - clip.start as f64;
            let local_end = to - clip.start as f64;
            if local_end <= 0.0 || local_start >= clip.len() as f64 {
                continue;
            }
            let start = local_start.max(0.0) as Frames;
            let (lo, hi) = clip_min_max(project, clip, start, local_end as Frames - start);

            let over = lo < -1.0 || hi > 1.0;
            let top = center - hi.clamp(-1.0, 1.0) * half_height;
            let bottom = center - lo.clamp(-1.0, 1.0) * half_height;
            // Quiet or steady audio still draws a visible line, on whole pixels so it stays crisp.
            let (top, bottom) = if bottom - top < 2.0 {
                let middle = ((top + bottom) / 2.0).round();
                (middle - 1.0, middle + 1.0)
            } else {
                (top, bottom)
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(x as f32, top), pos2(x as f32 + 1.0, bottom)),
                CornerRadius::ZERO,
                if over { WAVEFORM_OVER } else { WAVEFORM },
            );
        }
    }

    fn paint_selection(&self, painter: &Painter, areas: &Areas, selection: &Selection) {
        let lane = areas.lane;
        let x_of = |frame: Frames| lane.left() + self.view.offset_of(frame as f64) as f32;

        if let Some(range) = selection.range {
            let rect = Rect::from_min_max(pos2(x_of(range.start), lane.top()), pos2(x_of(range.end), lane.bottom()));
            painter.with_clip_rect(lane).rect_filled(rect, CornerRadius::ZERO, RANGE);
        }

        if selection.range.is_none() {
            let x = x_of(selection.cursor);
            if lane.x_range().contains(x) {
                painter.line_segment(
                    [pos2(x, areas.ruler.top()), pos2(x, lane.bottom())],
                    Stroke::new(1.0, CURSOR),
                );
            }
        }
    }

    fn paint_ruler(&self, painter: &Painter, areas: &Areas, sample_rate: u32) {
        let ruler = areas.ruler;
        painter.rect_filled(ruler, CornerRadius::ZERO, RULER);

        let step = tick_step(self.view.frames_per_point, sample_rate);
        let right = self.view.frame_at(self.width);
        let mut tick = (self.view.scroll / step).floor() * step;
        while tick <= right {
            let x = ruler.left() + self.view.offset_of(tick) as f32;
            painter.line_segment(
                [pos2(x, ruler.bottom() - 8.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, TICK),
            );
            painter.text(
                pos2(x + 4.0, ruler.center().y - 2.0),
                Align2::LEFT_CENTER,
                format_time(tick.round() as Frames, sample_rate),
                FontId::monospace(11.0),
                LABEL,
            );
            tick += step;
        }
    }

    fn paint_scrollbar(&self, painter: &Painter, areas: &Areas) {
        painter.rect_filled(areas.scrollbar, CornerRadius::ZERO, SCROLL_TRACK);
        let thumb = self.scrollbar_thumb(areas).shrink2(vec2(0.0, 2.0));
        painter.rect_filled(thumb, CornerRadius::same(4), SCROLL_THUMB);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f64 = 1000.0;
    const EXTENT: f64 = 1_000_000.0;

    #[test]
    fn view_conversions_round_trip() {
        let view = View { scroll: 500.0, frames_per_point: 2.5 };
        assert_eq!(view.frame_at(100.0), 750.0);
        assert_eq!(view.offset_of(750.0), 100.0);
    }

    #[test]
    fn zoom_keeps_the_anchored_frame_in_place() {
        let mut view = View { scroll: 100_000.0, frames_per_point: 100.0 };
        let before = view.frame_at(300.0);
        view.zoom(2.0, 300.0, WIDTH, EXTENT);
        assert_eq!(view.frames_per_point, 50.0);
        assert!((view.frame_at(300.0) - before).abs() < 1e-6);
    }

    #[test]
    fn view_cannot_leave_the_timeline_or_zoom_past_its_limits() {
        let mut view = View { scroll: 0.0, frames_per_point: 100.0 };

        view.zoom(0.0001, 0.0, WIDTH, EXTENT);
        assert_eq!(view.frames_per_point, EXTENT / WIDTH);
        assert_eq!(view.scroll, 0.0);

        view.zoom(1e9, 0.0, WIDTH, EXTENT);
        assert_eq!(view.frames_per_point, MIN_FRAMES_PER_POINT);

        view.scroll = 1e12;
        view.clamp(WIDTH, EXTENT);
        assert_eq!(view.scroll, EXTENT - WIDTH * MIN_FRAMES_PER_POINT);

        view.scroll = -5.0;
        view.clamp(WIDTH, EXTENT);
        assert_eq!(view.scroll, 0.0);
    }

    #[test]
    fn fit_shows_the_whole_timeline() {
        let mut view = View { scroll: 123.0, frames_per_point: 1.0 };
        view.fit(WIDTH, EXTENT);
        assert_eq!(view.scroll, 0.0);
        assert_eq!(view.frame_at(WIDTH), EXTENT);
    }

    #[test]
    fn times_format_as_minutes_seconds_millis() {
        assert_eq!(format_time(0, 48000), "0:00.000");
        assert_eq!(format_time(72_000, 48000), "0:01.500");
        assert_eq!(format_time(48000 * 125, 48000), "2:05.000");
    }

    #[test]
    fn ticks_are_round_and_never_crowded() {
        for frames_per_point in [0.1, 1.0, 37.0, 500.0, 12_345.0, 1e7] {
            let step = tick_step(frames_per_point, 48000);
            assert!(step / frames_per_point >= MIN_TICK_SPACING);
        }
        assert_eq!(tick_step(48000.0 / 100.0, 48000), 48000.0);
    }

    fn project_with_clips(count: usize) -> (Project, Vec<ClipId>) {
        let mut project = Project::new(100);
        let ids = (0..count)
            .map(|i| {
                let source = project.add_source(1, vec![0.0; 10]).unwrap();
                project.append_clip(&format!("clip {i}"), source).unwrap()
            })
            .collect();
        (project, ids)
    }

    const CTRL: Modifiers = Modifiers::COMMAND;
    const SHIFT: Modifiers = Modifiers::SHIFT;

    fn click(selection: &mut Selection, project: &Project, id: ClipId, modifiers: Modifiers) {
        selection.click_clip(project, project.clip(id).unwrap(), modifiers);
    }

    #[test]
    fn clicking_a_clip_replaces_the_selection_and_moves_the_cursor() {
        let (p, ids) = project_with_clips(3);
        let mut s = Selection::default();
        click(&mut s, &p, ids[0], Modifiers::NONE);
        click(&mut s, &p, ids[2], Modifiers::NONE);
        assert_eq!(s.clips, [ids[2]]);
        assert_eq!(s.cursor, p.clip(ids[2]).unwrap().start);
    }

    #[test]
    fn ctrl_click_toggles_clips() {
        let (p, ids) = project_with_clips(3);
        let mut s = Selection::default();
        click(&mut s, &p, ids[0], Modifiers::NONE);
        click(&mut s, &p, ids[2], CTRL);
        assert_eq!(s.clips, [ids[0], ids[2]]);
        click(&mut s, &p, ids[0], CTRL);
        assert_eq!(s.clips, [ids[2]]);
    }

    #[test]
    fn shift_click_selects_a_run_in_either_direction() {
        let (p, ids) = project_with_clips(5);
        let mut s = Selection::default();
        click(&mut s, &p, ids[1], Modifiers::NONE);
        click(&mut s, &p, ids[3], SHIFT);
        assert_eq!(s.clips, [ids[1], ids[2], ids[3]]);
        click(&mut s, &p, ids[0], SHIFT);
        assert_eq!(s.clips, [ids[0], ids[1]]);
    }

    #[test]
    fn shift_click_with_no_anchor_selects_just_that_clip() {
        let (p, ids) = project_with_clips(3);
        let mut s = Selection::default();
        click(&mut s, &p, ids[1], SHIFT);
        assert_eq!(s.clips, [ids[1]]);
    }

    #[test]
    fn clip_and_range_selections_replace_each_other() {
        let (p, ids) = project_with_clips(2);
        let mut s = Selection::default();
        click(&mut s, &p, ids[0], Modifiers::NONE);

        s.set_range(30, 10);
        assert!(s.clips.is_empty());
        assert_eq!(s.range, Some(TimeRange { start: 10, end: 30 }));

        click(&mut s, &p, ids[0], Modifiers::NONE);
        assert_eq!(s.range, None);
    }

    #[test]
    fn empty_range_means_no_selection() {
        let mut s = Selection::default();
        s.set_range(10, 30);
        s.set_range(10, 10);
        assert_eq!(s.range, None);
    }

    #[test]
    fn shift_clicking_a_position_extends_from_the_cursor() {
        let mut s = Selection::default();
        s.click_position(100, false);
        assert_eq!((s.cursor, s.range), (100, None));
        s.click_position(40, true);
        assert_eq!(s.range, Some(TimeRange { start: 40, end: 100 }));
        assert_eq!(s.cursor, 100);
        s.click_position(70, false);
        assert_eq!((s.cursor, s.range), (70, None));
    }

    #[test]
    fn select_all_and_clear() {
        let (p, ids) = project_with_clips(3);
        let mut s = Selection::default();
        s.select_all(&p);
        assert_eq!(s.clips, ids);
        s.clear();
        assert!(s.clips.is_empty());
    }

    #[test]
    fn undone_clips_drop_out_of_the_selection() {
        let (mut p, ids) = project_with_clips(3);
        let mut s = Selection::default();
        s.select_all(&p);
        p.delete_clips(&ids[1..]).unwrap();
        s.retain_existing(&p);
        assert_eq!(s.clips, [ids[0]]);
    }
}
