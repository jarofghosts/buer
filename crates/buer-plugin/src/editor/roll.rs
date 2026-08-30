//! The roll: twenty-four rows to the octave, drawn by hand.
//!
//! Not an [`egui::ScrollArea`]. A scroll area would fight every drag for the pointer, and would not
//! let the grid be painted under the notes and the playhead over them. This is one allocated region
//! and a painter, the way mater draws its waveform, with the scrolling and zooming done by hand.
//!
//! Every lane is the same height. The obvious reading of "quarter-tone lanes drawn thinner" is to
//! make the row physically shorter, and it was tried and rejected: it destroys the linear lane-to-y
//! map that the keyboard, the drag arithmetic, the marquee and the velocity lane all share, and
//! replaces one division with a twenty-four-entry table and a search in each of them. A dimmer band
//! and a fainter separator make the same distinction and keep [`View::lane_at`] a division.

use buer_core::pattern::{Lane, LaneMask, Note, NoteId, Pattern, MIN_LENGTH};
use buer_core::pitch;
use nih_plug_egui::egui;

use super::{Metrics, ACCENT, ACCENT_BRIGHT, ACCENT_FILL, TEXT, TEXT_FADE};
use crate::params::LaneNames;

/// Width of the keyboard down the left edge.
pub const GUTTER: f32 = 52.0;
/// Width of the column of lane toggles left of the keyboard.
pub const MASK: f32 = 13.0;
/// Height of the bar ruler along the top.
pub const RULER: f32 = 15.0;
/// Height of the velocity lane below.
pub const VELOCITY: f32 = 40.0;

/// The zoom limits, in points before the ui scale is applied to them.
const MIN_PX_PER_TICK: f32 = 0.0015;
const MAX_PX_PER_TICK: f32 = 0.5;
const MIN_PX_PER_LANE: f32 = 4.0;
const MAX_PX_PER_LANE: f32 = 30.0;

/// One bar, in ticks. Every pattern is built in fours; when a pattern carries its own time
/// signature this is where it comes from instead.
const BAR: u32 = buer_core::TICKS_PER_BEAT * 4;
/// How close to a note's right edge the pointer must be to resize rather than move it.
const GRAB: f32 = 7.0;
/// A note narrower than this is still drawn this wide, so a grace note never vanishes. In points,
/// like the zoom limits, and scaled with them.
const MIN_NOTE_PX: f32 = 3.0;

/// The visible window onto a pattern, in the pattern's own units.
#[derive(Copy, Clone, Debug)]
pub struct View {
    /// Leftmost visible tick.
    pub tick: f32,
    /// The lane along the top edge. Lanes count upward and y counts downward, which is the one
    /// place a sign is easy to lose.
    pub top_lane: f32,
    pub px_per_tick: f32,
    pub px_per_lane: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            tick: 0.0,
            // Two octaves with middle c a third of the way down, which is where a hand goes.
            top_lane: 152.0,
            // A sixteenth is 480 ticks at 1920 ppq; this makes one about twenty points wide.
            px_per_tick: 0.042,
            // Twenty-four of these is an octave, so this is a fraction under a semitone's worth of
            // height per quarter tone — about what a twelve-lane roll gives a semitone.
            px_per_lane: 11.0,
        }
    }
}

impl View {
    pub fn x(&self, rect: egui::Rect, tick: f32) -> f32 {
        rect.left() + (tick - self.tick) * self.px_per_tick
    }

    pub fn tick_at(&self, rect: egui::Rect, x: f32) -> f32 {
        self.tick + (x - rect.left()) / self.px_per_tick
    }

    /// The top of a lane's row. The row is `[y(lane), y(lane) + px_per_lane)`.
    pub fn y(&self, rect: egui::Rect, lane: f32) -> f32 {
        rect.top() + (self.top_lane - lane) * self.px_per_lane
    }

    pub fn lane_at(&self, rect: egui::Rect, y: f32) -> Lane {
        let lane = self.top_lane - (y - rect.top()) / self.px_per_lane;
        lane.floor().clamp(0.0, 255.0) as Lane
    }

    /// The lanes that fall inside a rect, top one first.
    fn lanes_in(&self, rect: egui::Rect) -> std::ops::RangeInclusive<i32> {
        let top = self.top_lane.ceil() as i32;
        let rows = (rect.height() / self.px_per_lane).ceil() as i32 + 1;
        (top - rows).max(0)..=top.min(255)
    }

    /// Redraw the roll at a new ui scale: the same view, by the ratio between them.
    ///
    /// The zoom is held in screen points, not in laid-out ones, because that is what every drag and
    /// every hit test measures against. So it has to be rescaled by hand when the interface is,
    /// the way the window is — without this the roll is the one part of the interface that does not
    /// grow, and at 200 % it is half the size of everything around it.
    pub fn rescale(&mut self, from: f32, to: f32) {
        if from <= 0.0 || to <= 0.0 {
            return;
        }
        let ratio = to / from;
        self.px_per_tick *= ratio;
        self.px_per_lane *= ratio;
    }

    /// Zoom keeping whatever is under the pointer exactly where it is. Anything else makes zooming
    /// a hunt for the place you were looking at.
    ///
    /// The limits are scaled with everything else: they are how small a lane may be drawn before it
    /// cannot be read, and that is a size on screen.
    fn zoom_x(&mut self, rect: egui::Rect, anchor: f32, factor: f32, scale: f32) {
        let held = self.tick_at(rect, anchor);
        self.px_per_tick = (self.px_per_tick * factor)
            .clamp(MIN_PX_PER_TICK * scale, MAX_PX_PER_TICK * scale);
        self.tick = held - (anchor - rect.left()) / self.px_per_tick;
    }

    fn zoom_y(&mut self, rect: egui::Rect, anchor: f32, factor: f32, scale: f32) {
        let held = self.top_lane - (anchor - rect.top()) / self.px_per_lane;
        self.px_per_lane = (self.px_per_lane * factor)
            .clamp(MIN_PX_PER_LANE * scale, MAX_PX_PER_LANE * scale);
        self.top_lane = held + (anchor - rect.top()) / self.px_per_lane;
    }

    fn hold(&mut self, rect: egui::Rect, length: u32) {
        let visible = rect.width() / self.px_per_tick;
        self.tick = self.tick.clamp(0.0, (length as f32 - visible).max(0.0));
        let rows = rect.height() / self.px_per_lane;
        self.top_lane = self.top_lane.clamp(rows.min(255.0), 255.0);
    }
}

/// What the pointer is in the middle of doing.
#[derive(Clone, Debug, Default)]
pub enum Gesture {
    #[default]
    None,
    /// Drawing. The note exists already — it is made on the press so it can be seen and heard while
    /// the drag sets its length. `anchor` is the tick the press landed on.
    Draw { id: NoteId, anchor: u32 },
    Move {
        grab_tick: i64,
        grab_lane: i32,
        from: Vec<(NoteId, u32, Lane)>,
    },
    Resize { id: NoteId, start: u32 },
    Velocity { id: NoteId, from_y: f32, was: u8 },
    Marquee { from: egui::Pos2 },
}

/// What the roll did this frame, for the editor to act on.
#[derive(Default)]
pub struct Outcome {
    /// A gesture started, and what to call it in the undo stack.
    pub began: Option<&'static str>,
    /// A gesture finished.
    pub ended: bool,
    /// The pattern changed.
    pub changed: bool,
    /// A note to sound once, so you hear the lane you landed on.
    pub audition: Option<Note>,
}

/// Everything the roll needs that is not the pattern itself.
pub struct Context<'a> {
    pub view: &'a mut View,
    pub gesture: &'a mut Gesture,
    pub selection: &'a mut Vec<NoteId>,
    pub snap: u32,
    pub draw_length: u32,
    /// Where the playhead is, or `None` when nothing is running.
    pub playhead: Option<f32>,
    pub names: LaneNames,
    pub metrics: Metrics,
}

/// Round a tick to the snap grid. A snap of one is off, and rounds to itself.
pub fn snapped(tick: f32, snap: u32) -> u32 {
    let snap = snap.max(1) as f32;
    ((tick / snap).round().max(0.0) * snap) as u32
}

fn snapped_down(tick: f32, snap: u32) -> u32 {
    let snap = snap.max(1) as f32;
    ((tick / snap).floor().max(0.0) * snap) as u32
}

/// Draw the roll and take what the pointer does to it.
pub fn show(
    ui: &mut egui::Ui,
    pattern: &mut Pattern,
    ctx: &mut Context,
    height: f32,
) -> Outcome {
    let mut outcome = Outcome::default();
    let metrics = ctx.metrics;
    let gutter = metrics.at(GUTTER);
    let mask = metrics.at(MASK);
    let ruler = metrics.at(RULER);
    let velocity = metrics.at(VELOCITY);

    let full = ui
        .allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        )
        .0;

    let grid = egui::Rect::from_min_max(
        egui::pos2(full.left() + mask + gutter, full.top() + ruler),
        egui::pos2(full.right(), full.bottom() - velocity),
    );
    if grid.width() <= 1.0 || grid.height() <= 1.0 {
        return outcome;
    }
    let lanes = egui::Rect::from_min_max(
        egui::pos2(full.left(), grid.top()),
        egui::pos2(full.left() + mask, grid.bottom()),
    );
    let keys = egui::Rect::from_min_max(
        egui::pos2(full.left() + mask, grid.top()),
        egui::pos2(full.left() + mask + gutter, grid.bottom()),
    );
    let ruler_rect = egui::Rect::from_min_max(
        egui::pos2(grid.left(), full.top()),
        egui::pos2(grid.right(), grid.top()),
    );
    let velocity_rect = egui::Rect::from_min_max(
        egui::pos2(grid.left(), grid.bottom()),
        egui::pos2(grid.right(), full.bottom()),
    );

    scroll_and_zoom(ui, grid, ctx.view, pattern.length, metrics.scale());

    let response = ui.interact(
        grid,
        ui.id().with("roll grid"),
        egui::Sense::click_and_drag(),
    );
    let velocity_response = ui.interact(
        velocity_rect,
        ui.id().with("roll velocity"),
        egui::Sense::click_and_drag(),
    );

    handle(
        ui,
        pattern,
        ctx,
        grid,
        velocity_rect,
        &response,
        &velocity_response,
        &mut outcome,
    );

    paint_grid(ui, pattern, ctx, grid);
    paint_notes(ui, pattern, ctx, grid);
    paint_keys(ui, ctx, keys);
    if lane_column(ui, pattern, ctx, lanes) {
        outcome.began = Some("set the lanes");
        outcome.changed = true;
        outcome.ended = true;
    }
    paint_ruler(ui, pattern, ctx, ruler_rect);
    paint_velocity(ui, pattern, ctx, velocity_rect);
    paint_playhead(ui, ctx, grid, ruler_rect);

    outcome
}

fn scroll_and_zoom(ui: &egui::Ui, grid: egui::Rect, view: &mut View, length: u32, scale: f32) {
    let Some(pointer) = ui.ctx().pointer_latest_pos() else {
        view.hold(grid, length);
        return;
    };
    if !grid.contains(pointer) {
        view.hold(grid, length);
        return;
    }

    let (scroll, modifiers) = ui.input(|input| (input.smooth_scroll_delta, input.modifiers));
    if scroll != egui::Vec2::ZERO {
        let step = scroll.y + scroll.x;
        if modifiers.command && modifiers.shift {
            view.zoom_y(grid, pointer.y, (step * 0.005).exp(), scale);
        } else if modifiers.command {
            view.zoom_x(grid, pointer.x, (step * 0.005).exp(), scale);
        } else if modifiers.shift {
            view.tick -= scroll.y / view.px_per_tick;
        } else {
            view.top_lane += scroll.y / view.px_per_lane;
            view.tick -= scroll.x / view.px_per_tick;
        }
    }
    view.hold(grid, length);
}

/// Which note is under a point, and whether the pointer is on its right edge.
fn hit(pattern: &Pattern, view: &View, grid: egui::Rect, at: egui::Pos2, grab: f32) -> Option<(NoteId, bool)> {
    let lane = view.lane_at(grid, at.y);
    // Last first, so the note drawn on top is the one taken.
    pattern.notes().iter().rev().find_map(|note| {
        if note.lane != lane {
            return None;
        }
        let left = view.x(grid, note.start as f32);
        let right = (view.x(grid, note.end() as f32)).max(left + MIN_NOTE_PX);
        (at.x >= left - 1.0 && at.x <= right + 1.0).then_some((note.id, at.x >= right - grab))
    })
}

#[allow(clippy::too_many_arguments)]
fn handle(
    ui: &egui::Ui,
    pattern: &mut Pattern,
    ctx: &mut Context,
    grid: egui::Rect,
    velocity_rect: egui::Rect,
    response: &egui::Response,
    velocity_response: &egui::Response,
    outcome: &mut Outcome,
) {
    let modifiers = ui.input(|input| input.modifiers);
    let grab = ctx.metrics.at(GRAB);

    // Deleting: a right-click on a note, wherever the pointer happens to be in it.
    if response.secondary_clicked() {
        if let Some(at) = response.interact_pointer_pos() {
            if let Some((id, _)) = hit(pattern, ctx.view, grid, at, grab) {
                outcome.began = Some("delete note");
                pattern.remove_id(id);
                ctx.selection.retain(|held| *held != id);
                outcome.changed = true;
                outcome.ended = true;
            }
        }
        return;
    }

    if velocity_response.drag_started() {
        if let Some(at) = velocity_response.interact_pointer_pos() {
            if let Some(id) = velocity_under(pattern, ctx.view, velocity_rect, at) {
                let was = pattern.find(id).map(|note| note.velocity).unwrap_or(100);
                *ctx.gesture = Gesture::Velocity {
                    id,
                    from_y: at.y,
                    was,
                };
                outcome.began = Some("set velocity");
            }
        }
    }

    if response.drag_started() {
        if let Some(at) = response.interact_pointer_pos() {
            match hit(pattern, ctx.view, grid, at, grab) {
                Some((id, edge)) => {
                    if !ctx.selection.contains(&id) {
                        if modifiers.shift {
                            ctx.selection.push(id);
                        } else {
                            ctx.selection.clear();
                            ctx.selection.push(id);
                        }
                    }
                    if edge {
                        let start = pattern.find(id).map(|note| note.start).unwrap_or(0);
                        *ctx.gesture = Gesture::Resize { id, start };
                        outcome.began = Some("resize note");
                    } else {
                        let from = ctx
                            .selection
                            .iter()
                            .filter_map(|id| pattern.find(*id))
                            .map(|note| (note.id, note.start, note.lane))
                            .collect();
                        *ctx.gesture = Gesture::Move {
                            grab_tick: ctx.view.tick_at(grid, at.x) as i64,
                            grab_lane: ctx.view.lane_at(grid, at.y) as i32,
                            from,
                        };
                        outcome.began = Some("move note");
                    }
                }
                None if modifiers.shift => {
                    *ctx.gesture = Gesture::Marquee { from: at };
                    outcome.began = Some("select");
                }
                None => {
                    // Drawing. The note is made now so it can be seen and heard while the drag
                    // decides how long it is.
                    let anchor = snapped_down(ctx.view.tick_at(grid, at.x), ctx.snap);
                    let lane = ctx.view.lane_at(grid, at.y);
                    let note = Note::new(anchor, ctx.draw_length, lane, 100);
                    if let Some(id) = pattern.insert(note) {
                        ctx.selection.clear();
                        ctx.selection.push(id);
                        *ctx.gesture = Gesture::Draw { id, anchor };
                        outcome.began = Some("draw note");
                        outcome.changed = true;
                        outcome.audition = pattern.find(id).copied();
                    }
                }
            }
        }
    }

    if response.dragged() || velocity_response.dragged() {
        if let Some(at) = response
            .interact_pointer_pos()
            .or_else(|| velocity_response.interact_pointer_pos())
        {
            match ctx.gesture.clone() {
                Gesture::Draw { id, anchor } => {
                    // Alt bypasses the snap, so a length can be exact to the tick.
                    let to = ctx.view.tick_at(grid, at.x);
                    let length = if modifiers.alt {
                        (to - anchor as f32).round().max(MIN_LENGTH as f32) as u32
                    } else {
                        let end = snapped(to, ctx.snap).max(anchor + ctx.snap.max(MIN_LENGTH));
                        end - anchor
                    };
                    if pattern.update(id, |note| note.length = length) {
                        outcome.changed = true;
                    }
                }
                Gesture::Resize { id, start } => {
                    let to = ctx.view.tick_at(grid, at.x);
                    let end = if modifiers.alt {
                        to.round().max(0.0) as u32
                    } else {
                        snapped(to, ctx.snap)
                    };
                    let length = end.saturating_sub(start).max(MIN_LENGTH);
                    if pattern.update(id, |note| note.length = length) {
                        outcome.changed = true;
                    }
                }
                Gesture::Move {
                    grab_tick,
                    grab_lane,
                    from,
                } => {
                    let now_tick = ctx.view.tick_at(grid, at.x) as i64;
                    let now_lane = ctx.view.lane_at(grid, at.y) as i32;
                    // Shift locks the drag to whichever axis it has moved further along.
                    let (mut dt, mut dl) = (now_tick - grab_tick, now_lane - grab_lane);
                    if modifiers.shift {
                        if dt.unsigned_abs() as f32 * ctx.view.px_per_tick
                            > dl.unsigned_abs() as f32 * ctx.view.px_per_lane
                        {
                            dl = 0;
                        } else {
                            dt = 0;
                        }
                    }
                    let mut moved = false;
                    for (id, start, lane) in &from {
                        let want = (*start as i64 + dt).max(0) as f32;
                        let start = if modifiers.alt {
                            want as u32
                        } else {
                            snapped(want, ctx.snap)
                        };
                        let lane = (*lane as i32 + dl).clamp(0, 255) as Lane;
                        moved |= pattern.update(*id, |note| {
                            note.start = start;
                            note.lane = lane;
                        });
                    }
                    if moved {
                        outcome.changed = true;
                        if dl != 0 {
                            outcome.audition = from
                                .first()
                                .and_then(|(id, _, _)| pattern.find(*id))
                                .copied();
                        }
                    }
                }
                Gesture::Velocity { id, from_y, was } => {
                    let travel = (from_y - at.y) / velocity_rect.height().max(1.0) * 127.0;
                    let velocity = (was as f32 + travel).round().clamp(1.0, 127.0) as u8;
                    if pattern.update(id, |note| note.velocity = velocity) {
                        outcome.changed = true;
                    }
                }
                Gesture::Marquee { from } => {
                    let box_ = egui::Rect::from_two_pos(from, at);
                    ctx.selection.clear();
                    for note in pattern.notes() {
                        let left = ctx.view.x(grid, note.start as f32);
                        let right = ctx.view.x(grid, note.end() as f32);
                        let top = ctx.view.y(grid, note.lane as f32 + 1.0);
                        let bottom = top + ctx.view.px_per_lane;
                        if box_.intersects(egui::Rect::from_min_max(
                            egui::pos2(left, top),
                            egui::pos2(right.max(left + MIN_NOTE_PX), bottom),
                        )) {
                            ctx.selection.push(note.id);
                        }
                    }
                }
                Gesture::None => {}
            }
        }
    }

    if response.drag_stopped() || velocity_response.drag_stopped() {
        if !matches!(ctx.gesture, Gesture::None) {
            outcome.ended = true;
        }
        *ctx.gesture = Gesture::None;
    }

    // A click is a press and a release with no movement between them, so `drag_started` never fires
    // for one — which is why choosing a fixed length used to do nothing until you dragged. A click
    // on empty ground places a note of the chosen length; on a note, it selects it.
    if response.clicked() {
        if let Some(at) = response.interact_pointer_pos() {
            match hit(pattern, ctx.view, grid, at, grab) {
                Some((id, _)) => {
                    if !modifiers.shift {
                        ctx.selection.clear();
                    }
                    if !ctx.selection.contains(&id) {
                        ctx.selection.push(id);
                    }
                }
                None => {
                    let start = snapped_down(ctx.view.tick_at(grid, at.x), ctx.snap);
                    let lane = ctx.view.lane_at(grid, at.y);
                    let note = Note::new(start, ctx.draw_length, lane, 100);
                    if let Some(id) = pattern.insert(note) {
                        ctx.selection.clear();
                        ctx.selection.push(id);
                        outcome.began = Some("draw note");
                        outcome.changed = true;
                        outcome.ended = true;
                        outcome.audition = pattern.find(id).copied();
                    }
                }
            }
        }
    }
}

fn velocity_under(
    pattern: &Pattern,
    view: &View,
    rect: egui::Rect,
    at: egui::Pos2,
) -> Option<NoteId> {
    let mut best: Option<(f32, NoteId)> = None;
    for note in pattern.notes() {
        let x = view.x(rect, note.start as f32);
        let distance = (x - at.x).abs();
        if distance <= 6.0 && best.map(|(d, _)| distance < d).unwrap_or(true) {
            best = Some((distance, note.id));
        }
    }
    best.map(|(_, id)| id)
}

fn paint_grid(ui: &egui::Ui, pattern: &Pattern, ctx: &Context, grid: egui::Rect) {
    let painter = ui.painter_at(grid);
    let visuals = ui.visuals();
    let view = &ctx.view;
    let mut shapes = Vec::new();

    painter.rect_filled(grid, egui::CornerRadius::ZERO, visuals.extreme_bg_color);

    // The lane bands. A quarter tone gets a tint and a fainter separator; a semitone gets neither
    // and reads as the row it is.
    let quarter_tint = visuals.faint_bg_color.gamma_multiply(0.6);
    let black_tint = egui::Color32::from_black_alpha(48);
    let hair = ctx.metrics.at(1.0).max(1.0);
    for lane in view.lanes_in(grid) {
        let lane = lane as Lane;
        let top = view.y(grid, lane as f32 + 1.0);
        let row = egui::Rect::from_min_size(
            egui::pos2(grid.left(), top),
            egui::vec2(grid.width(), view.px_per_lane),
        );
        if pitch::is_black(lane) {
            shapes.push(egui::Shape::rect_filled(
                row,
                egui::CornerRadius::ZERO,
                black_tint,
            ));
        }
        if pitch::is_quarter(lane) {
            shapes.push(egui::Shape::rect_filled(
                row,
                egui::CornerRadius::ZERO,
                quarter_tint,
            ));
        }
        // A lane the scale does not reach is tinted out, so a maqam is a shape you can see before
        // a single note exists.
        if !pattern.constraint.contains(lane) {
            shapes.push(egui::Shape::rect_filled(
                row,
                egui::CornerRadius::ZERO,
                egui::Color32::from_black_alpha(96),
            ));
        }
        let line = egui::Stroke::new(
            if pitch::is_quarter(lane) { hair * 0.75 } else { hair },
            if pitch::degree(lane) == 0 {
                TEXT_FADE
            } else if pitch::is_quarter(lane) {
                TEXT_FADE.gamma_multiply(0.3)
            } else {
                TEXT_FADE.gamma_multiply(0.6)
            },
        );
        shapes.push(egui::Shape::line_segment(
            [
                egui::pos2(grid.left(), top),
                egui::pos2(grid.right(), top),
            ],
            line,
        ));
    }

    // The snap grid, the beats and the bars, at three weights — and each drawn only while its own
    // spacing is wide enough to read. Drawing a line every pixel is a wash, not a grid.
    let beat = buer_core::TICKS_PER_BEAT;
    let last = view.tick_at(grid, grid.right()).ceil().max(0.0) as u32;
    for (step, colour) in [
        (ctx.snap.max(1), TEXT_FADE.gamma_multiply(0.25)),
        (beat, TEXT_FADE.gamma_multiply(0.7)),
        (BAR, TEXT_FADE),
    ] {
        if step as f32 * view.px_per_tick < 5.0 {
            continue;
        }
        let mut tick = (view.tick / step as f32).floor().max(0.0) as u32 * step;
        while tick <= last {
            let x = view.x(grid, tick as f32);
            shapes.push(egui::Shape::line_segment(
                [egui::pos2(x, grid.top()), egui::pos2(x, grid.bottom())],
                egui::Stroke::new(hair, colour),
            ));
            tick += step;
        }
    }

    // Past the end of the pattern, dimmed: the loop point is a place, and it should look like one.
    let end = view.x(grid, pattern.length as f32);
    if end < grid.right() {
        shapes.push(egui::Shape::rect_filled(
            egui::Rect::from_min_max(egui::pos2(end, grid.top()), grid.max),
            egui::CornerRadius::ZERO,
            egui::Color32::from_black_alpha(120),
        ));
    }

    painter.extend(shapes);
}

fn paint_notes(ui: &egui::Ui, pattern: &Pattern, ctx: &Context, grid: egui::Rect) {
    let painter = ui.painter_at(grid);
    let view = &ctx.view;
    let mut shapes = Vec::new();
    let radius = egui::CornerRadius::same(2);
    let hair = ctx.metrics.at(1.0).max(1.0);

    for note in pattern.notes() {
        let left = view.x(grid, note.start as f32);
        let right = view.x(grid, note.end() as f32).max(left + MIN_NOTE_PX);
        if right < grid.left() || left > grid.right() {
            continue;
        }
        let top = view.y(grid, note.lane as f32 + 1.0);
        if top > grid.bottom() || top + view.px_per_lane < grid.top() {
            continue;
        }
        let rect = egui::Rect::from_min_max(
            egui::pos2(left, top + hair),
            egui::pos2(right, top + view.px_per_lane - hair),
        );
        // Velocity as opacity, so a phrase's shape is legible without reading the lane below.
        let weight = 0.35 + 0.65 * note.velocity as f32 / 127.0;
        shapes.push(egui::Shape::rect_filled(
            rect,
            radius,
            ACCENT_FILL.gamma_multiply(weight),
        ));
        let selected = ctx.selection.contains(&note.id);
        shapes.push(egui::Shape::rect_stroke(
            rect,
            radius,
            egui::Stroke::new(hair, if selected { ACCENT_BRIGHT } else { ACCENT }),
            egui::StrokeKind::Inside,
        ));
    }

    if let Gesture::Marquee { from } = &*ctx.gesture {
        if let Some(to) = ui.ctx().pointer_latest_pos() {
            let box_ = egui::Rect::from_two_pos(*from, to);
            shapes.push(egui::Shape::rect_stroke(
                box_,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(hair, ACCENT_BRIGHT),
                egui::StrokeKind::Inside,
            ));
        }
    }

    painter.extend(shapes);
}

/// The twenty-four lane toggles, drawn on the roll's own rows so a toggle sits beside its lane and
/// repeats up the octaves.
///
/// This is where the pitch constraint actually lives. A scale is *stamped* into it — see
/// `buer_core::scales` — so a toggle always changes what it looks like it changes.
fn lane_column(ui: &egui::Ui, pattern: &mut Pattern, ctx: &Context, rect: egui::Rect) -> bool {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, ui.visuals().faint_bg_color);

    let view = &ctx.view;
    let hair = ctx.metrics.at(1.0).max(1.0);
    let pointer = ui
        .ctx()
        .pointer_latest_pos()
        .filter(|at| rect.contains(*at));
    let clicked = ui.input(|input| input.pointer.primary_pressed());
    let mut changed = false;

    let mut shapes = Vec::new();
    for lane in view.lanes_in(rect) {
        let lane = lane as Lane;
        let top = view.y(rect, lane as f32 + 1.0);
        let row = egui::Rect::from_min_size(
            egui::pos2(rect.left() + hair, top + hair),
            egui::vec2(rect.width() - hair * 2.0, (view.px_per_lane - hair * 2.0).max(1.0)),
        );
        let on = pattern.constraint.contains(lane);
        shapes.push(egui::Shape::rect_filled(
            row,
            egui::CornerRadius::ZERO,
            if on {
                ACCENT.gamma_multiply(0.85)
            } else {
                TEXT_FADE.gamma_multiply(0.4)
            },
        ));
        if let Some(at) = pointer {
            if row.expand(hair).contains(at) {
                shapes.push(egui::Shape::rect_stroke(
                    row,
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(hair, ACCENT_BRIGHT),
                    egui::StrokeKind::Inside,
                ));
                if clicked {
                    let mut mask = pattern.constraint;
                    mask.set(pitch::degree(lane), !on);
                    // A mask with nothing in it would leave the generator with no lane it may
                    // write to, and would tint out the whole roll. Turning the last one off is
                    // read as turning them all on, which is what "no constraint" means anyway.
                    pattern.constraint = if mask.is_empty() { LaneMask::ALL } else { mask };
                    pattern.scale.clear();
                    changed = true;
                }
            }
        }
    }
    painter.extend(shapes);
    changed
}

fn paint_keys(ui: &egui::Ui, ctx: &Context, keys: egui::Rect) {
    let painter = ui.painter_at(keys);
    let view = &ctx.view;
    let visuals = ui.visuals();
    painter.rect_filled(keys, egui::CornerRadius::ZERO, visuals.faint_bg_color);
    let hair = ctx.metrics.at(1.0).max(1.0);
    let font = egui::FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size);
    // A name needs a row tall enough to hold it. Below that the choice is quietly narrowed rather
    // than obeyed, because a column of overlapping text is worse than no names at all.
    let room = view.px_per_lane >= font.size;
    let names = if room {
        ctx.names
    } else {
        LaneNames::Octaves
    };

    for lane in view.lanes_in(keys) {
        let lane = lane as Lane;
        let top = view.y(keys, lane as f32 + 1.0);
        let row = egui::Rect::from_min_size(
            egui::pos2(keys.left(), top),
            egui::vec2(keys.width(), view.px_per_lane),
        );
        // A quarter tone is drawn as a narrow stub between the keys it sits between, so the
        // keyboard still reads as a keyboard.
        let (fill, width) = if pitch::is_quarter(lane) {
            (visuals.extreme_bg_color, keys.width() * 0.45)
        } else if pitch::is_black(lane) {
            (egui::Color32::from_gray(28), keys.width() * 0.7)
        } else {
            (egui::Color32::from_gray(190), keys.width())
        };
        painter.rect_filled(
            egui::Rect::from_min_size(row.min, egui::vec2(width, row.height() - hair)),
            egui::CornerRadius::ZERO,
            fill,
        );

        if !names.names(lane) || (!room && pitch::degree(lane) != 0) {
            continue;
        }
        // A name sits on whichever key it belongs to, so it takes that key's contrast: dark on the
        // white ones, light on the black ones and on the quarter-tone stubs between them.
        let ink = if pitch::is_quarter(lane) || pitch::is_black(lane) {
            TEXT
        } else {
            egui::Color32::from_gray(24)
        };
        painter.text(
            egui::pos2(keys.right() - ctx.metrics.at(2.0), row.center().y),
            egui::Align2::RIGHT_CENTER,
            pitch::describe(lane),
            font.clone(),
            ink,
        );
    }
}

/// The bar numbers along the top, and the loop end.
///
/// Bars, not patterns. Numbering by the pattern's own length put a `2` where the second *pattern*
/// would start and left four bars of grid with nothing to count them by.
fn paint_ruler(ui: &egui::Ui, pattern: &Pattern, ctx: &Context, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let view = &ctx.view;
    painter.rect_filled(rect, egui::CornerRadius::ZERO, ui.visuals().faint_bg_color);
    let font = egui::FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size);
    let hair = ctx.metrics.at(1.0).max(1.0);

    // Every bar while they are far enough apart to number, every fourth after that.
    let spacing = BAR as f32 * view.px_per_tick;
    let every = if spacing >= 26.0 {
        1
    } else if spacing >= 8.0 {
        4
    } else {
        16
    };

    let first = (view.tick / BAR as f32).floor().max(0.0) as u32 / every * every;
    let last = view.tick_at(rect, rect.right()).ceil().max(0.0) as u32;
    let mut tick = first;
    while tick <= last {
        let x = view.x(rect, tick as f32);
        if x >= rect.left() - 1.0 {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(hair, TEXT_FADE),
            );
            painter.text(
                egui::pos2(x + ctx.metrics.at(3.0), rect.center().y),
                egui::Align2::LEFT_CENTER,
                (tick / BAR + 1).to_string(),
                font.clone(),
                TEXT_FADE,
            );
        }
        tick = tick.saturating_add(BAR * every);
    }

    // Where the pattern loops, which is the one mark on this strip that is not a bar.
    let end = view.x(rect, pattern.length as f32);
    if end >= rect.left() && end <= rect.right() {
        painter.line_segment(
            [egui::pos2(end, rect.top()), egui::pos2(end, rect.bottom())],
            egui::Stroke::new(hair * 2.0, ACCENT),
        );
    }
}

fn paint_velocity(ui: &egui::Ui, pattern: &Pattern, ctx: &Context, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let view = &ctx.view;
    painter.rect_filled(
        rect,
        egui::CornerRadius::ZERO,
        ui.visuals().extreme_bg_color,
    );
    let width = ctx.metrics.at(3.0).max(2.0);
    let mut shapes = Vec::new();
    for note in pattern.notes() {
        let x = view.x(rect, note.start as f32);
        if x < rect.left() - width || x > rect.right() {
            continue;
        }
        let height = rect.height() * note.velocity as f32 / 127.0;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - height),
            egui::pos2(x + width, rect.bottom()),
        );
        let selected = ctx.selection.contains(&note.id);
        shapes.push(egui::Shape::rect_filled(
            bar,
            egui::CornerRadius::ZERO,
            if selected { ACCENT_BRIGHT } else { ACCENT },
        ));
    }
    painter.extend(shapes);
}

fn paint_playhead(ui: &egui::Ui, ctx: &Context, grid: egui::Rect, ruler: egui::Rect) {
    let Some(position) = ctx.playhead else {
        return;
    };
    let x = ctx.view.x(grid, position);
    if x < grid.left() || x > grid.right() {
        return;
    }
    ui.painter_at(egui::Rect::from_min_max(ruler.min, grid.max))
        .line_segment(
            [egui::pos2(x, ruler.top()), egui::pos2(x, grid.bottom())],
            egui::Stroke::new(ctx.metrics.at(1.5).max(1.0), ACCENT_BRIGHT),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 400.0))
    }

    #[test]
    fn the_lane_under_a_y_is_the_lane_drawn_at_it() {
        let view = View::default();
        let rect = rect();
        for lane in 100..140u8 {
            // The middle of a lane's row must read back as that lane.
            let y = view.y(rect, lane as f32 + 1.0) + view.px_per_lane / 2.0;
            assert_eq!(view.lane_at(rect, y), lane);
        }
    }

    #[test]
    fn the_tick_under_an_x_is_the_tick_drawn_at_it() {
        let view = View::default();
        let rect = rect();
        for tick in (0..7680).step_by(240) {
            let x = view.x(rect, tick as f32);
            assert!((view.tick_at(rect, x) - tick as f32).abs() < 0.01);
        }
    }

    #[test]
    fn zooming_about_the_pointer_keeps_the_tick_under_it_still() {
        let mut view = View::default();
        let rect = rect();
        let anchor = rect.left() + 250.0;
        let held = view.tick_at(rect, anchor);
        for factor in [1.4, 0.6, 2.0, 0.3] {
            view.zoom_x(rect, anchor, factor, 1.0);
            assert!(
                (view.tick_at(rect, anchor) - held).abs() < 0.5,
                "the view slid under the pointer"
            );
        }
    }

    #[test]
    fn zooming_vertically_keeps_the_lane_under_the_pointer_still() {
        let mut view = View::default();
        let rect = rect();
        let anchor = rect.top() + 150.0;
        let held = view.top_lane - (anchor - rect.top()) / view.px_per_lane;
        for factor in [1.5, 0.5, 1.2] {
            view.zoom_y(rect, anchor, factor, 1.0);
            let now = view.top_lane - (anchor - rect.top()) / view.px_per_lane;
            assert!((now - held).abs() < 0.01);
        }
    }

    #[test]
    fn snapping_rounds_to_the_grid_and_a_snap_of_one_rounds_to_itself() {
        assert_eq!(snapped(479.0, 480), 480);
        assert_eq!(snapped(200.0, 480), 0);
        assert_eq!(snapped(1234.0, 1), 1234);
        assert_eq!(snapped_down(479.0, 480), 0);
    }

    #[test]
    fn zoom_is_held_within_what_can_be_read() {
        let mut view = View::default();
        let rect = rect();
        for _ in 0..100 {
            view.zoom_x(rect, rect.left(), 2.0, 1.0);
            view.zoom_y(rect, rect.top(), 2.0, 1.0);
        }
        assert!(view.px_per_tick <= MAX_PX_PER_TICK);
        assert!(view.px_per_lane <= MAX_PX_PER_LANE);
        for _ in 0..200 {
            view.zoom_x(rect, rect.left(), 0.5, 1.0);
            view.zoom_y(rect, rect.top(), 0.5, 1.0);
        }
        assert!(view.px_per_tick >= MIN_PX_PER_TICK);
        assert!(view.px_per_lane >= MIN_PX_PER_LANE);
    }

    #[test]
    fn the_roll_grows_with_the_rest_of_the_interface() {
        let mut view = View::default();
        let (tick, lane) = (view.px_per_tick, view.px_per_lane);
        view.rescale(1.0, 2.0);
        assert_eq!(view.px_per_lane, lane * 2.0);
        assert_eq!(view.px_per_tick, tick * 2.0);
        // And back down again, exactly, so stepping through the scales cannot drift.
        view.rescale(2.0, 1.0);
        assert_eq!(view.px_per_lane, lane);
        assert_eq!(view.px_per_tick, tick);
    }

    #[test]
    fn a_lane_may_be_zoomed_as_large_at_two_hundred_percent_as_at_one_hundred() {
        // The limits are a size on screen, so they have to scale with everything else — otherwise
        // the roll at 200 % could not be zoomed past half what it can reach at 100 %.
        let rect = rect();
        let mut view = View::default();
        view.rescale(1.0, 2.0);
        for _ in 0..100 {
            view.zoom_y(rect, rect.top(), 2.0, 2.0);
        }
        assert_eq!(view.px_per_lane, MAX_PX_PER_LANE * 2.0);
    }

    #[test]
    fn a_reopened_window_does_not_double_the_zoom() {
        // The build closure resets the style, not the view; rescaling from where the view actually
        // is means reopening at the same scale changes nothing.
        let mut view = View::default();
        let lane = view.px_per_lane;
        view.rescale(1.0, 2.0);
        view.rescale(2.0, 2.0);
        assert_eq!(view.px_per_lane, lane * 2.0);
    }

    /// Run one frame of the roll with no window, and hand back every shape it painted.
    ///
    /// egui will run headless, which makes the drawing testable at all: "is a note on screen" is
    /// otherwise a question only a person with the plugin open can answer.
    fn painted(pattern: &mut Pattern, view: View, scale: f32) -> Vec<egui::Shape> {
        painted_named(pattern, view, scale, LaneNames::Octaves)
    }

    fn painted_named(
        pattern: &mut Pattern,
        view: View,
        scale: f32,
        names: LaneNames,
    ) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };

        let mut view = view;
        let mut gesture = Gesture::None;
        let mut selection = Vec::new();
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut context = Context {
                    view: &mut view,
                    gesture: &mut gesture,
                    selection: &mut selection,
                    snap: 480,
                    draw_length: 480,
                    playhead: None,
                    names,
                    metrics: Metrics::for_test(scale),
                };
                show(ui, pattern, &mut context, 560.0);
            });
        });

        output
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .collect()
    }

    /// The filled rectangles a note is drawn as, told apart by their fill.
    ///
    /// A note is `ACCENT_FILL` shaded by its velocity; the lane toggles and the velocity bars are
    /// `ACCENT`, which is far brighter. Matching on "reddish" alone catches all three.
    fn note_rects(shapes: &[egui::Shape]) -> Vec<egui::Rect> {
        shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::Shape::Rect(rect)
                    if rect.fill.r() > rect.fill.b()
                        && (30..=ACCENT_FILL.r()).contains(&rect.fill.r()) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .collect()
    }

    /// Run the roll over several frames, feeding each one its own events.
    ///
    /// A warm-up frame comes first: egui works out what the pointer is over from the widget rects
    /// of the *previous* frame, so a press on the very first frame lands on nothing.
    fn frames(pattern: &mut Pattern, draw_length: u32, events: &[Vec<egui::Event>]) {
        let ctx = egui::Context::default();
        let mut view = View::default();
        let mut gesture = Gesture::None;
        let mut selection = Vec::new();
        let warmed: Vec<Vec<egui::Event>> = std::iter::once(Vec::new())
            .chain(events.iter().cloned())
            .collect();
        for frame in &warmed {
            let input = egui::RawInput {
                events: frame.clone(),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut context = Context {
                        view: &mut view,
                        gesture: &mut gesture,
                        selection: &mut selection,
                        snap: 480,
                        draw_length,
                        playhead: None,
                        names: LaneNames::Octaves,
                        metrics: Metrics::for_test(1.0),
                    };
                    show(ui, pattern, &mut context, 560.0);
                });
            });
        }
    }

    fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_click_with_a_length_chosen_places_a_note_without_a_drag() {
        // A click is a press and a release with nothing in between, so `drag_started` never fires
        // for one. Choosing a fixed length and clicking used to do nothing at all.
        let mut pattern = Pattern::empty("p", 4, 4);
        let at = egui::pos2(400.0, 300.0);
        frames(
            &mut pattern,
            buer_core::TICKS_PER_BEAT,
            &[
                vec![egui::Event::PointerMoved(at)],
                vec![press(at, true)],
                vec![press(at, false)],
            ],
        );
        assert_eq!(pattern.notes().len(), 1, "the click placed nothing");
        assert_eq!(
            pattern.notes()[0].length,
            buer_core::TICKS_PER_BEAT,
            "the note was not the length that was chosen"
        );
    }

    #[test]
    fn a_click_lands_on_the_snap_below_it_rather_than_the_nearest_one() {
        // A note drawn at a click starts at the gridline before the pointer, not the nearest one:
        // clicking just after a beat must not put the note before it.
        assert_eq!(snapped_down(479.0, 480), 0);
        assert_eq!(snapped_down(481.0, 480), 480);
    }

    #[test]
    fn a_click_on_a_note_selects_it_rather_than_drawing_another() {
        let mut pattern = Pattern::empty("p", 4, 4);
        // Draw one, then click it again in the same place.
        let at = egui::pos2(400.0, 300.0);
        frames(
            &mut pattern,
            480,
            &[
                vec![egui::Event::PointerMoved(at)],
                vec![press(at, true)],
                vec![press(at, false)],
                vec![press(at, true)],
                vec![press(at, false)],
            ],
        );
        assert_eq!(pattern.notes().len(), 1, "the second click drew another note");
    }

    #[test]
    fn a_pattern_with_notes_in_it_draws_them() {
        let mut pattern = Pattern::empty("p", 4, 4);
        for step in 0..30 {
            pattern.insert(Note::new(step * 480, 480, 110 + (step % 20) as u8, 100));
        }
        let shapes = painted(&mut pattern, View::default(), 1.0);
        let rects = note_rects(&shapes);
        assert!(
            rects.len() >= 20,
            "only {} note rectangles were painted",
            rects.len()
        );
        for rect in rects {
            assert!(rect.width() > 0.0 && rect.height() > 0.0, "{rect:?}");
        }
    }

    #[test]
    fn a_note_is_drawn_where_the_view_says_it_is() {
        let mut pattern = Pattern::empty("p", 4, 4);
        pattern.insert(Note::new(1920, 480, 120, 100));
        let view = View::default();
        let shapes = painted(&mut pattern, view, 1.0);
        let rects = note_rects(&shapes);
        assert_eq!(rects.len(), 1, "{rects:?}");
        // A sixteenth wide, and tall enough to see — the whole complaint about the old lane height
        // was that a note came out four pixels tall in a dark fill and read as nothing at all.
        assert!(rects[0].width() > 15.0, "a beat should be visible: {:?}", rects[0]);
        assert!(rects[0].height() > 6.0, "a lane should be visible: {:?}", rects[0]);
    }

    #[test]
    fn a_note_is_drawn_twice_as_tall_at_twice_the_scale() {
        // The complaint this answers: at 200 % the roll was the one thing in the window that did
        // not grow, and a note came out four pixels tall.
        let mut pattern = Pattern::empty("p", 4, 4);
        // Near the top of the view, so it is on screen at either scale: twice the lane height is
        // half as many lanes in the same window.
        pattern.insert(Note::new(1920, 480, 150, 100));

        let small = note_rects(&painted(&mut pattern, View::default(), 1.0))[0].height();
        let mut doubled = View::default();
        doubled.rescale(1.0, 2.0);
        let large = note_rects(&painted(&mut pattern, doubled, 2.0))[0].height();

        assert!(small >= 7.0, "a note is only {small} points tall at 100 %");
        assert!(
            large > small * 1.8,
            "a note is {large} points tall at 200 % against {small} at 100 %"
        );
    }

    /// The names painted on the keyboard, which is everything left of the grid.
    fn key_names(shapes: &[egui::Shape], scale: f32) -> Vec<String> {
        let gutter = (MASK + GUTTER) * scale;
        shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::Shape::Text(text) if text.pos.x < gutter => {
                    Some(text.galley.text().to_string())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn naming_the_octaves_names_the_cs_and_nothing_else() {
        let mut pattern = Pattern::empty("p", 4, 4);
        let names = key_names(
            &painted_named(&mut pattern, View::default(), 1.0, LaneNames::Octaves),
            1.0,
        );
        assert!(!names.is_empty(), "no names at all");
        assert!(
            names.iter().all(|name| name.starts_with('c') && !name.contains('#')),
            "{names:?}"
        );
    }

    #[test]
    fn naming_the_notes_names_every_semitone_and_no_quarter_tone() {
        let mut pattern = Pattern::empty("p", 4, 4);
        let names = key_names(
            &painted_named(&mut pattern, View::default(), 1.0, LaneNames::Notes),
            1.0,
        );
        assert!(names.iter().any(|name| name.contains('#')), "{names:?}");
        assert!(!names.iter().any(|name| name.ends_with('+')), "{names:?}");
    }

    #[test]
    fn naming_the_lanes_names_the_quarter_tones_too() {
        let mut pattern = Pattern::empty("p", 4, 4);
        let octaves = key_names(
            &painted_named(&mut pattern, View::default(), 1.0, LaneNames::Octaves),
            1.0,
        );
        let notes = key_names(
            &painted_named(&mut pattern, View::default(), 1.0, LaneNames::Notes),
            1.0,
        );
        let lanes = key_names(
            &painted_named(&mut pattern, View::default(), 1.0, LaneNames::Lanes),
            1.0,
        );
        assert!(lanes.iter().any(|name| name.ends_with('+')), "{lanes:?}");
        assert!(lanes.len() > notes.len());
        assert!(notes.len() > octaves.len());
    }

    #[test]
    fn a_row_too_short_to_hold_a_name_is_not_given_one() {
        // Zoomed out past legibility the choice is narrowed back to the octaves rather than obeyed:
        // a column of overlapping text is worse than no names at all.
        let mut pattern = Pattern::empty("p", 4, 4);
        let squashed = View {
            px_per_lane: 5.0,
            ..View::default()
        };
        let names = key_names(&painted_named(&mut pattern, squashed, 1.0, LaneNames::Lanes), 1.0);
        assert!(
            names.iter().all(|name| !name.contains('#') && !name.ends_with('+')),
            "{names:?}"
        );
    }

    #[test]
    fn an_empty_pattern_draws_no_notes() {
        let mut pattern = Pattern::empty("p", 4, 4);
        assert!(note_rects(&painted(&mut pattern, View::default(), 1.0)).is_empty());
    }

    #[test]
    fn the_ruler_numbers_bars_and_not_patterns() {
        // A four-bar pattern is four bars, whatever its length happens to be in ticks.
        assert_eq!(BAR, 7680);
        let four_bars = BAR * 4;
        let numbers: Vec<u32> = (0..four_bars).step_by(BAR as usize).map(|tick| tick / BAR + 1).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4]);
    }
}
