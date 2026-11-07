//! The track view: ruler, clips with waveforms, selection and zoom.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Modifiers, Painter, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::peaks::clip_min_max;
use crate::{Clip, ClipId, Frames, Project, TimeRange};

const RULER_HEIGHT: f32 = 24.0;
const SCROLLBAR_HEIGHT: f32 = 14.0;
const CLIP_HEADER_HEIGHT: f32 = 22.0;
const LANE_MARGIN: f32 = 8.0;
/// Closest zoom: each frame is 16 points wide.
const MIN_FRAMES_PER_POINT: f64 = 1.0 / 16.0;
const MIN_TICK_SPACING: f64 = 90.0;

const BACKGROUND: Color32 = Color32::from_rgb(24, 26, 30);
const RULER: Color32 = Color32::from_rgb(34, 37, 43);
const TICK: Color32 = Color32::from_rgb(70, 75, 85);
const LABEL: Color32 = Color32::from_rgb(170, 176, 186);
const CLIP_BODY: Color32 = Color32::from_rgb(38, 46, 58);
const CLIP_HEADER: Color32 = Color32::from_rgb(58, 70, 88);
const CLIP_HEADER_SELECTED: Color32 = Color32::from_rgb(52, 120, 200);
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

enum Drag {
    None,
    Range { start: Frames },
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
        }
    }
}

impl Timeline {
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
    ) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let areas = Areas::new(rect);
        self.rect = rect;

        self.width = areas.lane.width() as f64;
        self.extent = extent(project);
        if std::mem::take(&mut self.fit_requested) {
            self.view.fit(self.width, self.extent);
        }
        self.handle_zoom_and_scroll(ui, &response, &areas);
        self.view.clamp(self.width, self.extent);
        self.handle_pointer(ui, &response, &areas, project, selection);
        if let Some(playhead) = playhead {
            self.follow(playhead as f64);
        }
        self.view.clamp(self.width, self.extent);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        for clip in project.clips() {
            self.paint_clip(&painter, &areas, project, clip, selection.clips.contains(&clip.id));
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
            } else if self.clip_header_at(areas, project, origin).is_some() {
                // Dragging a header will move the clip once editing is in.
                Drag::None
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
                    // Scroll when dragging past either edge of the lane.
                    let past_left = (areas.lane.left() - pointer.x).max(0.0);
                    let past_right = (pointer.x - areas.lane.right()).max(0.0);
                    if past_left > 0.0 || past_right > 0.0 {
                        self.view.scroll +=
                            (past_right - past_left) as f64 * self.view.frames_per_point * 0.2;
                        ui.ctx().request_repaint();
                    }
                    let x = pointer.x.clamp(areas.lane.left(), areas.lane.right());
                    selection.set_range(start, frame_at(&self.view, x));
                }
                Drag::Scrollbar { grab } => self.drag_scrollbar(areas, pointer.x, grab),
            }
        }

        if response.drag_stopped() {
            self.drag = Drag::None;
        }

        if response.clicked() {
            let Some(pointer) = response.interact_pointer_pos() else { return };
            if areas.scrollbar.contains(pointer) {
                let thumb = self.scrollbar_thumb(areas);
                self.drag_scrollbar(areas, pointer.x, thumb.width() as f64 / 2.0);
            } else if let Some(clip) = self.clip_header_at(areas, project, pointer) {
                selection.click_clip(project, clip, modifiers);
            } else {
                selection.click_position(frame_at(&self.view, pointer.x), modifiers.shift);
            }
        }
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
        painter.rect_filled(rect, radius, CLIP_BODY);

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

        if selected {
            painter.rect_stroke(rect, radius, Stroke::new(1.5, CLIP_BORDER_SELECTED), StrokeKind::Inside);
        }
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
            // Quiet audio still draws at least a one point line.
            let bottom = bottom.max(top + 1.0);
            painter.line_segment(
                [pos2(x as f32 + 0.5, top), pos2(x as f32 + 0.5, bottom)],
                Stroke::new(1.0, if over { WAVEFORM_OVER } else { WAVEFORM }),
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
