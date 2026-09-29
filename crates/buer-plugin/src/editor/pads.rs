//! The pads: the other way to put a note in, for a finger rather than a pointer.
//!
//! Twenty-four pads to the octave, laid out as the keyboard plus the row it does not have. The
//! bottom row of an octave is the twelve semitones, in order; the row above it is the twelve
//! quarter tones, each one directly above the note it sharpens. So the pad above a pad is always
//! a quarter tone up, and the pad beside it is always a semitone — which is the one relation a
//! twelve-key layout cannot show at all.
//!
//! Two things arrive here that the roll never sees.
//!
//! A pad answers on the **press**, not on the click. egui reports a click on the release, and a pad
//! that lights and sounds when the finger comes off feels broken — so the pointer is read straight
//! out of the input state, the way [`super::roll`]'s lane column reads it, rather than through a
//! `Response`.
//!
//! And a pad grid is the one part of this interface where more than one finger is on the glass at
//! once, so raw [`egui::Event::Touch`] is handled beside the pointer: every finger keeps its own
//! pad, and a chord is as many notes on one step. A backend that reports touches also reports the
//! first of them as an emulated pointer, which would enter that note twice — so on any frame with a
//! finger down the pointer is ignored and the touches speak. egui-baseview, which is what the
//! plugin window is, sends no touch events at all today: a touchscreen reaches it as the mouse the
//! system synthesises, which is exactly what the pointer path already handles. Both are here so the
//! pads work under either.

use buer_core::pattern::{Lane, Note, NoteId, Pattern, MIN_LENGTH};
use buer_core::{pitch, TICKS_PER_BEAT};
use nih_plug_egui::egui;

use super::roll::snapped;
use super::{Metrics, ACCENT, ACCENT_BRIGHT, TEXT};
use crate::shared::Captured;

/// One bar, in ticks — what the mark's position is counted in, as on the roll's ruler.
const BAR: u32 = TICKS_PER_BEAT * 4;
/// The line above the pads: the octave, the mark, and the ways to move it.
const CONTROLS: f32 = 24.0;
/// How tall a row of pads has to be before another octave is worth showing instead.
const MIN_ROW: f32 = 26.0;
/// Past this the grid is more border than pad, and a finger is no better served by more of it.
const MAX_OCTAVES: usize = 4;
/// What the pads ask for: the control line and two octaves of rows a finger can hit.
pub const WANTED: f32 = CONTROLS + 4.0 * 38.0;
/// The velocity a pad writes at, which is the one the roll draws at. Velocity is the lane under the
/// roll; a pad that also set it would be two controls in one and neither of them legible.
const VELOCITY: u8 = 100;

/// Where a press came from, so that each finger keeps its own pad and its own note.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Source {
    /// The mouse — or a touchscreen the system has turned into one, which is what the plugin
    /// window actually sees today.
    Pointer,
    Finger(egui::TouchDeviceId, egui::TouchId),
    /// A key on the note input, paired with its own release by channel and note. Not by lane: an
    /// MPE channel goes on bending while the note is held, and the lane it started on is not
    /// necessarily the one it ends on.
    Key(u8, u8),
}

/// One pad currently down.
#[derive(Copy, Clone, Debug)]
struct Held {
    source: Source,
    lane: Lane,
    /// The note this press wrote, if it wrote one. A pad pressed where a note already sits writes
    /// nothing and still sounds, and a pad pressed with record off writes nothing at all.
    note: Option<NoteId>,
    /// Where the playhead was when it went down, for a note being held to length against it.
    /// `None` for anything written at the mark, whose length is settled before it is written.
    start: Option<f32>,
}

/// Where a press puts its note, if it puts one anywhere.
#[derive(Copy, Clone, Debug)]
enum Write {
    /// Nowhere: record is off, so it only sounds.
    No,
    /// At the mark, one step long, moving the mark on when the last one lifts.
    Mark,
    /// Where the playhead is, held to length until the key comes up.
    Live(f32),
}

/// What a note is written by: how long a fresh one is, and the grid it lands on.
#[derive(Copy, Clone, Debug)]
pub struct Terms {
    /// The roll's `length`, which is the snap where that is set to `draw`.
    pub step: u32,
    /// The roll's `snap`. `1` is off, and records to the tick.
    pub snap: u32,
}

/// The pad grid's own state: where it is looking, and where it writes.
#[derive(Clone, Debug)]
pub struct Pads {
    /// The lowest octave on the grid. Middle c is `c4`, as everywhere else.
    pub octave: i32,
    /// The tick the next note is written at — the mark, drawn on the roll.
    pub entry: u32,
    held: Vec<Held>,
    /// Whether anything has been written at the mark and is waiting for the last finger to lift.
    wrote: bool,
}

impl Default for Pads {
    fn default() -> Self {
        Self {
            // Two octaves from c3, so middle c is on the grid without anybody having to find it.
            octave: 3,
            entry: 0,
            held: Vec::new(),
            wrote: false,
        }
    }
}

/// Everything the pads need that is not the pattern itself.
pub struct Context<'a> {
    pub pads: &'a mut Pads,
    pub terms: Terms,
    /// Whether what is played is written down. Off, the pads are an instrument and nothing else.
    pub record: bool,
    pub metrics: Metrics,
}

/// What the pads did this frame, in the same terms the roll reports in.
#[derive(Default)]
pub struct Outcome {
    pub began: Option<&'static str>,
    pub ended: bool,
    pub changed: bool,
    /// Notes to sound once. More than one, because more than one finger can land in a frame.
    pub auditions: Vec<Note>,
}

/// Whether a press happened this frame, so the editor can take the before-image for the undo stack
/// only when there is a gesture to open. The pointer alone is not enough: a finger is not one.
pub fn pressed(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        input.pointer.any_pressed()
            || input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Touch {
                        phase: egui::TouchPhase::Start,
                        ..
                    }
                )
            })
    })
}

/// Draw the pads and take what is pressed on them.
pub fn show(ui: &mut egui::Ui, pattern: &mut Pattern, ctx: &mut Context, height: f32) -> Outcome {
    let mut outcome = Outcome::default();
    // A pattern can be shortened, or replaced outright by a project load, under a mark that was
    // inside the one before it.
    ctx.pads.entry = ctx.pads.entry.min(pattern.length.saturating_sub(1));

    let metrics = ctx.metrics;
    let octaves = octaves_in(height - metrics.at(CONTROLS), metrics.scale());
    ctx.pads.octave = clamp_octave(ctx.pads.octave, octaves);

    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
        controls(ui, pattern, ctx, octaves);
        let rest = ui.available_height();
        if rest > 1.0 {
            grid(ui, pattern, ctx, rest, octaves, &mut outcome);
        }
    });

    outcome
}

/// The octave, the mark, and the ways to move it without writing anything.
fn controls(ui: &mut egui::Ui, pattern: &Pattern, ctx: &mut Context, octaves: usize) {
    let gap = ctx.metrics.at(super::SECTION_GAP);
    let step = ctx.terms.step.max(1);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("octave").small().weak());
        if ui.small_button("−").clicked() {
            ctx.pads.octave = clamp_octave(ctx.pads.octave - 1, octaves);
        }
        ui.label(
            egui::RichText::new(format!("{}", ctx.pads.octave))
                .monospace()
                .color(TEXT),
        );
        if ui.small_button("+").clicked() {
            ctx.pads.octave = clamp_octave(ctx.pads.octave + 1, octaves);
        }

        ui.add_space(gap);
        ui.label(egui::RichText::new("at").small().weak());
        ui.label(
            egui::RichText::new(position(ctx.pads.entry))
                .monospace()
                .color(ACCENT),
        );
        // None of these three writes anything, so none of them is an undo step: moving the mark is
        // where you are about to write, not something you did.
        if ui.small_button("◀").on_hover_text("a step back").clicked() {
            ctx.pads.back(pattern, step);
        }
        if ui
            .button("rest")
            .on_hover_text("leave this step empty and move on")
            .clicked()
        {
            ctx.pads.advance(pattern, step);
        }
        if ui
            .small_button("⏮")
            .on_hover_text("back to the start of the pattern")
            .clicked()
        {
            ctx.pads.entry = 0;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // What a pad does depends on whether record is armed, and there is no reading the
            // grid itself for the answer.
            ui.label(
                egui::RichText::new(if ctx.record {
                    "a pad sounds and writes at the mark; the roll's ruler moves it"
                } else {
                    "record is off — a pad sounds and writes nothing"
                })
                .small()
                .weak(),
            );
        });
    });
}

fn grid(
    ui: &mut egui::Ui,
    pattern: &mut Pattern,
    ctx: &mut Context,
    height: f32,
    octaves: usize,
    outcome: &mut Outcome,
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    // Claimed, so that a drag across the pads belongs to them and nothing underneath takes it. The
    // response itself is not read: a pad answers on the press, and a `Response` cannot say so until
    // the drag is decided or the button comes up.
    ui.interact(rect, ui.id().with("pads"), egui::Sense::click_and_drag());

    handle(ui, pattern, ctx, rect, octaves, outcome);
    paint(ui, pattern, ctx, rect, octaves);
}

fn handle(
    ui: &egui::Ui,
    pattern: &mut Pattern,
    ctx: &mut Context,
    rect: egui::Rect,
    octaves: usize,
    outcome: &mut Outcome,
) {
    let terms = ctx.terms;
    // With record off a pad is an instrument: it sounds, and the pattern is left alone.
    let write = if ctx.record { Write::Mark } else { Write::No };
    let octave = ctx.pads.octave;
    let lane_at = |at: egui::Pos2| lane_at(rect, octave, octaves, at);

    // The fingers, each with its own pad. Collected first because the input lock cannot be held
    // while the pattern is written.
    let touches: Vec<(Source, egui::TouchPhase, egui::Pos2)> = ui.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Touch {
                    device_id,
                    id,
                    phase,
                    pos,
                    ..
                } => Some((Source::Finger(*device_id, *id), *phase, *pos)),
                _ => None,
            })
            .collect()
    });
    for (source, phase, at) in touches {
        match phase {
            egui::TouchPhase::Start => {
                if let Some(lane) = lane_at(at) {
                    ctx.pads
                        .press(pattern, source, lane, VELOCITY, terms, write, outcome);
                }
            }
            egui::TouchPhase::Move => {
                if let Some(lane) = lane_at(at) {
                    ctx.pads.glide(pattern, source, lane, outcome);
                }
            }
            egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                ctx.pads.release(pattern, source, None, terms, outcome)
            }
        }
    }

    let (pressed, down, released, touching) = ui.input(|input| {
        (
            input.pointer.primary_pressed(),
            input.pointer.primary_down(),
            input.pointer.primary_released(),
            input.any_touches(),
        )
    });
    // A backend that reports touches reports the first of them as the pointer too, and that pointer
    // would write the same note a second time.
    if !touching {
        let at = super::roll::pointer_in(ui, rect).and_then(lane_at);
        if let Some(lane) = at {
            if pressed {
                ctx.pads.press(
                    pattern,
                    Source::Pointer,
                    lane,
                    VELOCITY,
                    terms,
                    write,
                    outcome,
                );
            } else if down {
                ctx.pads.glide(pattern, Source::Pointer, lane, outcome);
            }
        }
    }
    // The press first and the release after it, because a tap is frequently both in one frame — a
    // quick hand, or a frame that spans a while because nothing is running. The other way round,
    // the release finds nothing to let go of and the pad pressed a moment later stays down for
    // good: held forever, and the mark never moving off the step it was on.
    //
    // And released whatever else is going on: a button that comes up while a finger happens to be
    // down would otherwise be a pad nothing ever lifts.
    if released {
        ctx.pads
            .release(pattern, Source::Pointer, None, terms, outcome);
    }

    ctx.pads.settle(pattern, terms.step, outcome);
}

impl Pads {
    /// A pad going down: it sounds, and — with record armed — it writes.
    #[allow(clippy::too_many_arguments)]
    fn press(
        &mut self,
        pattern: &mut Pattern,
        source: Source,
        lane: Lane,
        velocity: u8,
        terms: Terms,
        write: Write,
        outcome: &mut Outcome,
    ) {
        if self.held.iter().any(|held| held.source == source) {
            return;
        }
        // A key is not sounded here. It came in through the plugin and went straight back out on
        // its way past — sounding it again would double every note played into an armed buer.
        if !matches!(source, Source::Key(..)) {
            outcome
                .auditions
                .push(Note::new(self.entry, terms.step, lane, velocity));
        }

        let entry = self.entry;
        let (note, start) = match write {
            Write::No => (None, None),
            Write::Mark => (
                put(
                    pattern,
                    entry,
                    lane,
                    velocity,
                    terms.step,
                    "enter notes",
                    outcome,
                ),
                None,
            ),
            // Written the moment the key goes down, at the length a fresh note is drawn at, and
            // held to its real one when the key comes up. A note that only appeared on the release
            // is a note you cannot see yourself playing.
            Write::Live(at) => (
                put(
                    pattern,
                    snapped(at, terms.snap),
                    lane,
                    velocity,
                    terms.step,
                    "record notes",
                    outcome,
                ),
                Some(at),
            ),
        };
        // Only the mark waits on a release. A live note is placed by the playhead, which moves it
        // on by itself.
        if note.is_some() && matches!(write, Write::Mark) {
            self.wrote = true;
        }
        self.held.push(Held {
            source,
            lane,
            note,
            start,
        });
    }

    /// A pad held and slid onto another: the note goes with it.
    ///
    /// It follows rather than a fresh note being written for every pad crossed. Sliding is how a
    /// pitch is found on a grid this fine, and a slide that wrote would leave a run of notes behind
    /// it — the roll's own answer, where dragging a note sounds each lane it passes through.
    fn glide(&mut self, pattern: &mut Pattern, source: Source, lane: Lane, outcome: &mut Outcome) {
        let entry = self.entry;
        let Some(held) = self.held.iter_mut().find(|held| held.source == source) else {
            return;
        };
        if held.lane == lane {
            return;
        }
        held.lane = lane;
        outcome.auditions.push(Note::new(entry, 1, lane, VELOCITY));

        // Onto a lane this step already has, the note stays where it is: moving it there would
        // stack two notes nothing can tell apart, and dropping one silently is worse.
        if let Some(id) = held.note {
            if note_at(pattern, entry, lane).is_none()
                && pattern.update(id, |note| note.lane = lane)
            {
                outcome.changed = true;
            }
        }
    }

    /// A pad coming up. `end` is where the playhead was as it did, for a note being held to length.
    fn release(
        &mut self,
        pattern: &mut Pattern,
        source: Source,
        end: Option<f32>,
        terms: Terms,
        outcome: &mut Outcome,
    ) {
        let Some(at) = self.held.iter().position(|held| held.source == source) else {
            return;
        };
        let held = self.held.remove(at);

        let (Some(id), Some(start), Some(end)) = (held.note, held.start, end) else {
            return;
        };
        // Round the length rather than the end, so a note played across the loop point keeps the
        // length it was held for instead of the negative one the arithmetic would otherwise give.
        // `Pattern::update` clips whatever is left of the pattern off it.
        let played = if end >= start {
            end - start
        } else {
            pattern.length as f32 - start + end
        };
        // At least one snap, so a key brushed on a quantised grid is a note of the grid rather
        // than the shortest thing that can be drawn. With the snap off, exactly what was played.
        let length = snapped(played, terms.snap).max(terms.snap.max(MIN_LENGTH));
        if pattern.update(id, |note| note.length = length) {
            outcome.changed = true;
        }
    }

    /// One key the audio thread caught off the note input, down or up.
    pub fn captured(
        &mut self,
        pattern: &mut Pattern,
        event: &Captured,
        slot: usize,
        terms: Terms,
        outcome: &mut Outcome,
    ) {
        let source = Source::Key(event.channel, event.note);
        // The playhead only means something for the pattern it is running through. Where the slot
        // sounding is not the slot being written into there is no playhead on this roll — the
        // editor does not draw one — and the mark is the honest answer.
        let at = event
            .at
            .filter(|_| event.slot == slot)
            .map(|tick| tick.rem_euclid(pattern.length.max(1) as f32));
        if event.on {
            let write = match at {
                Some(tick) => Write::Live(tick),
                None => Write::Mark,
            };
            self.press(
                pattern,
                source,
                event.lane,
                event.velocity,
                terms,
                write,
                outcome,
            );
        } else {
            self.release(pattern, source, at, terms, outcome);
        }
    }

    /// The lanes down right now, deduplicated — a chord as it actually sounds, whichever pads or
    /// keys put it there.
    pub fn held_lanes(&self) -> Vec<Lane> {
        let mut lanes: Vec<Lane> = self.held.iter().map(|held| held.lane).collect();
        lanes.sort_unstable();
        lanes.dedup();
        lanes
    }

    /// Let go of every key, for record being disarmed under one that is still down. The release
    /// that would have trimmed it is never coming, and it keeps the length it was written at.
    pub fn drop_keys(&mut self) {
        self.held
            .retain(|held| !matches!(held.source, Source::Key(..)));
    }

    /// Let go of everything, for the mode being switched away from under a pad that is still down.
    /// Nothing on screen can lift it after that, so it would stay held until it was pressed again.
    ///
    /// Answers whether anything was let go of, which is the editor's cue to close the undo step the
    /// press opened — the release that would have closed it is never coming.
    pub fn rest(&mut self) -> bool {
        let holding = !self.held.is_empty() || self.wrote;
        self.held.clear();
        self.wrote = false;
        holding
    }

    /// The mark moves on when the last finger lifts, not when the first one does.
    ///
    /// That is what makes a chord a chord: fingers never land on the same frame, and a mark that
    /// advanced on the first press would spread three notes over three steps.
    pub fn settle(&mut self, pattern: &Pattern, step: u32, outcome: &mut Outcome) {
        if !self.held.is_empty() || !self.wrote {
            return;
        }
        self.advance(pattern, step);
        self.wrote = false;
        outcome.ended = true;
    }

    /// One step on, wrapping at the loop point, because that is where the pattern goes too.
    fn advance(&mut self, pattern: &Pattern, step: u32) {
        let step = step.max(1);
        let next = self.entry.saturating_add(step);
        self.entry = if next >= pattern.length { 0 } else { next };
    }

    /// And one step back, wrapping onto the last whole step rather than onto the loop point, which
    /// is a place nothing can be written at.
    fn back(&mut self, pattern: &Pattern, step: u32) {
        let step = step.max(1);
        self.entry = match self.entry.checked_sub(step) {
            Some(back) => back,
            None => pattern.length.saturating_sub(1) / step * step,
        };
    }
}

/// Put one note down, unless that lane is already taken at that tick.
///
/// Two fingers on one pad, a pad pressed twice before the mark moved on, or a key retriggered
/// inside one snap is one note: a second note on the same lane and tick is one nothing downstream
/// could tell apart.
fn put(
    pattern: &mut Pattern,
    at: u32,
    lane: Lane,
    velocity: u8,
    length: u32,
    label: &'static str,
    outcome: &mut Outcome,
) -> Option<NoteId> {
    if note_at(pattern, at, lane).is_some() {
        return None;
    }
    let id = pattern.insert(Note::new(at, length, lane, velocity))?;
    outcome.began = Some(label);
    outcome.changed = true;
    Some(id)
}

fn note_at(pattern: &Pattern, tick: u32, lane: Lane) -> Option<NoteId> {
    pattern
        .notes()
        .iter()
        .find(|note| note.start == tick && note.lane == lane)
        .map(|note| note.id)
}

/// How many octaves a grid this tall can hold, at rows a finger can still hit.
fn octaves_in(height: f32, scale: f32) -> usize {
    let row = (MIN_ROW * scale).max(1.0);
    ((height / row).floor().max(0.0) as usize / 2).clamp(1, MAX_OCTAVES)
}

/// Hold the lowest octave where every pad above it is still a lane. Lane 255 is the quarter tone
/// above `g9`, so the top octave that has all twenty-four of its pads is the one starting at `c8`.
fn clamp_octave(octave: i32, octaves: usize) -> i32 {
    octave.clamp(-1, 9 - octaves as i32)
}

/// The lane a pad carries, or `None` where it would fall off the top of the range.
fn lane_of(octave: i32, row: usize, column: usize) -> Option<Lane> {
    let octave = octave + (row / 2) as i32;
    // Twenty-four lanes to the octave, `c-1` at lane zero: the quarter tone sits on the odd lane
    // above the semitone it sharpens, which is the row above it here.
    let lane = (octave + 1) * 24 + column as i32 * 2 + (row % 2) as i32;
    (0..256).contains(&lane).then_some(lane as Lane)
}

/// Where one pad is drawn. Rows count up from the bottom, so a higher pad is a higher pitch.
fn pad_rect(grid: egui::Rect, octaves: usize, row: usize, column: usize) -> egui::Rect {
    let width = grid.width() / 12.0;
    let height = grid.height() / (octaves * 2) as f32;
    egui::Rect::from_min_size(
        egui::pos2(
            grid.left() + column as f32 * width,
            grid.bottom() - (row + 1) as f32 * height,
        ),
        egui::vec2(width, height),
    )
}

/// And the lane under a point, which is the same arithmetic backwards.
fn lane_at(grid: egui::Rect, octave: i32, octaves: usize, at: egui::Pos2) -> Option<Lane> {
    if !grid.contains(at) {
        return None;
    }
    let column = ((at.x - grid.left()) / (grid.width() / 12.0)).floor() as i32;
    let row = ((grid.bottom() - at.y) / (grid.height() / (octaves * 2) as f32)).floor() as i32;
    let column = column.clamp(0, 11) as usize;
    let row = row.clamp(0, (octaves * 2 - 1) as i32) as usize;
    lane_of(octave, row, column)
}

/// A pad takes the colour of the key it is, from the roll's own keyboard — and a quarter tone takes
/// the grey halfway between the two, which is where it is.
fn pad_colours(lane: Lane, held: bool) -> (egui::Color32, egui::Color32) {
    if held {
        return (ACCENT, egui::Color32::from_gray(24));
    }
    if pitch::is_quarter(lane) {
        (egui::Color32::from_gray(109), egui::Color32::from_gray(24))
    } else if pitch::is_black(lane) {
        (egui::Color32::from_gray(28), TEXT)
    } else {
        (egui::Color32::from_gray(190), egui::Color32::from_gray(24))
    }
}

fn paint(ui: &egui::Ui, pattern: &Pattern, ctx: &Context, grid: egui::Rect, octaves: usize) {
    let painter = ui.painter_at(grid);
    painter.rect_filled(
        grid,
        egui::CornerRadius::ZERO,
        ui.visuals().extreme_bg_color,
    );

    let hair = ctx.metrics.at(1.0).max(1.0);
    let radius = egui::CornerRadius::same(2);
    let font = egui::FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size);
    let pointer = super::roll::pointer_in(ui, grid);
    let mut shapes = Vec::with_capacity(octaves * 24 * 2);
    let mut names = Vec::new();

    for row in 0..octaves * 2 {
        for column in 0..12 {
            let Some(lane) = lane_of(ctx.pads.octave, row, column) else {
                continue;
            };
            let pad = pad_rect(grid, octaves, row, column).shrink(hair);
            if pad.width() <= 1.0 || pad.height() <= 1.0 {
                continue;
            }

            let held = ctx.pads.held.iter().any(|held| held.lane == lane);
            let (fill, ink) = pad_colours(lane, held);
            shapes.push(egui::Shape::rect_filled(pad, radius, fill));

            // A lane the scale does not reach is tinted out, as it is across the whole roll. Tinted
            // rather than refused: the roll lets you draw on one, and two answers to the same
            // question would be one too many.
            if !pattern.constraint.contains(lane) {
                shapes.push(egui::Shape::rect_filled(
                    pad,
                    radius,
                    egui::Color32::from_black_alpha(96),
                ));
            }

            // What this step already holds, so a chord can be read off the pads it was played on.
            if note_at(pattern, ctx.pads.entry, lane).is_some() {
                shapes.push(egui::Shape::rect_stroke(
                    pad,
                    radius,
                    egui::Stroke::new(hair * 2.0, ACCENT),
                    egui::StrokeKind::Inside,
                ));
            }
            if pointer.is_some_and(|at| pad.contains(at)) {
                shapes.push(egui::Shape::rect_stroke(
                    pad,
                    radius,
                    egui::Stroke::new(hair, ACCENT_BRIGHT),
                    egui::StrokeKind::Inside,
                ));
            }

            // A name only where the pad can hold one; a row of clipped text is worse than none, as
            // on the roll's keyboard.
            if pad.width() > font.size * 2.6 && pad.height() > font.size * 1.3 {
                names.push((pad.center(), pitch::describe(lane), ink));
            }
        }
    }

    painter.extend(shapes);
    for (at, name, ink) in names {
        painter.text(at, egui::Align2::CENTER_CENTER, name, font.clone(), ink);
    }
}

/// The mark's position, as the ruler counts: bar and beat, and the ticks past the beat where it is
/// not on one.
fn position(tick: u32) -> String {
    let bar = tick / BAR + 1;
    let beat = tick % BAR / TICKS_PER_BEAT + 1;
    let rest = tick % TICKS_PER_BEAT;
    if rest == 0 {
        format!("{bar}.{beat}")
    } else {
        format!("{bar}.{beat}+{rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 200.0))
    }

    fn pads() -> Pads {
        Pads::default()
    }

    /// A sixteenth, snapped to sixteenths, which is what the roll starts on.
    fn terms() -> Terms {
        Terms {
            step: 480,
            snap: 480,
        }
    }

    fn key(on: bool, note: u8, at: Option<f32>) -> Captured {
        Captured {
            on,
            channel: 0,
            note,
            lane: note * 2,
            velocity: 100,
            at,
            slot: 0,
        }
    }

    #[test]
    fn a_pad_is_the_lane_it_is_drawn_on() {
        let grid = grid();
        for octaves in 1..=MAX_OCTAVES {
            for row in 0..octaves * 2 {
                for column in 0..12 {
                    let centre = pad_rect(grid, octaves, row, column).center();
                    assert_eq!(
                        lane_at(grid, 3, octaves, centre),
                        lane_of(3, row, column),
                        "octaves {octaves}, row {row}, column {column}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_pad_above_a_pad_is_a_quarter_tone_up_and_the_one_beside_it_a_semitone() {
        // Middle c is lane 120, and the pad grid starting at c4 puts it bottom left.
        assert_eq!(lane_of(4, 0, 0), Some(120));
        assert_eq!(lane_of(4, 1, 0), Some(121));
        assert_eq!(lane_of(4, 0, 1), Some(122));
        // And the octave above it is the next pair of rows up.
        assert_eq!(lane_of(4, 2, 0), Some(144));
    }

    #[test]
    fn the_grid_never_reaches_past_the_last_lane() {
        for octaves in 1..=MAX_OCTAVES {
            let octave = clamp_octave(99, octaves);
            for row in 0..octaves * 2 {
                for column in 0..12 {
                    assert!(
                        lane_of(octave, row, column).is_some(),
                        "octaves {octaves} at c{octave} lost a pad"
                    );
                }
            }
        }
        // And not off the bottom either: lane zero is c-1.
        assert_eq!(clamp_octave(-9, 2), -1);
        assert_eq!(lane_of(-1, 0, 0), Some(0));
    }

    #[test]
    fn a_chord_is_one_step_however_many_fingers_it_took() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();
        let step = 480;

        // Fingers never land on the same frame, and the mark must not move under the second one.
        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.settle(&pattern, terms().step, &mut outcome);
        assert_eq!(pads.entry, 0);
        pads.press(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(1)),
            127,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.settle(&pattern, terms().step, &mut outcome);
        assert_eq!(pads.entry, 0);

        pads.release(&mut pattern, Source::Pointer, None, terms(), &mut outcome);
        pads.settle(&pattern, terms().step, &mut outcome);
        assert_eq!(
            pads.entry, 0,
            "the mark moved while a finger was still down"
        );

        pads.release(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(1)),
            None,
            terms(),
            &mut outcome,
        );
        pads.settle(&pattern, terms().step, &mut outcome);
        assert_eq!(pads.entry, step);

        assert_eq!(pattern.notes().len(), 2);
        assert!(pattern.notes().iter().all(|note| note.start == 0));
        assert!(outcome.ended);
        assert_eq!(outcome.began, Some("enter notes"));
    }

    #[test]
    fn one_pad_under_two_fingers_is_one_note() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.press(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(1)),
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );

        assert_eq!(pattern.notes().len(), 1);
        // Both still sounded: a pad that goes down silently reads as one that missed.
        assert_eq!(outcome.auditions.len(), 2);
    }

    #[test]
    fn sliding_a_held_pad_takes_its_note_with_it() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.glide(&mut pattern, Source::Pointer, 121, &mut outcome);
        pads.glide(&mut pattern, Source::Pointer, 123, &mut outcome);

        assert_eq!(pattern.notes().len(), 1, "the slide wrote a note per pad");
        assert_eq!(pattern.notes()[0].lane, 123);
    }

    #[test]
    fn sliding_onto_a_lane_this_step_already_holds_leaves_both_notes_alone() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.press(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(1)),
            127,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.glide(&mut pattern, Source::Pointer, 127, &mut outcome);

        assert_eq!(pattern.notes().len(), 2);
        let lanes: Vec<_> = pattern.notes().iter().map(|note| note.lane).collect();
        assert_eq!(lanes, vec![120, 127]);
    }

    #[test]
    fn the_mark_wraps_at_the_loop_point_rather_than_running_past_it() {
        let mut pattern = Pattern::default();
        pattern.set_length(1920);
        let mut pads = pads();

        pads.entry = 1440;
        pads.advance(&pattern, 480);
        assert_eq!(pads.entry, 0);

        // And backwards onto the last whole step, which is a tick something can be written at —
        // the loop point itself is not.
        pads.back(&pattern, 480);
        assert_eq!(pads.entry, 1440);
        assert!(pads.entry < pattern.length);
    }

    #[test]
    fn a_pad_pressed_where_a_note_already_sits_sounds_and_writes_nothing() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.release(&mut pattern, Source::Pointer, None, terms(), &mut outcome);
        pads.settle(&pattern, terms().step, &mut outcome);
        pads.back(&pattern, 480);

        let mut again = Outcome::default();
        pads.press(
            &mut pattern,
            Source::Pointer,
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut again,
        );
        assert_eq!(pattern.notes().len(), 1);
        assert!(!again.changed);
        assert_eq!(again.auditions.len(), 1);

        // Nothing was written, so nothing is waiting on a release: the mark stays put.
        pads.release(&mut pattern, Source::Pointer, None, terms(), &mut again);
        pads.settle(&pattern, terms().step, &mut again);
        assert_eq!(pads.entry, 0);
        assert!(!again.ended);
    }

    #[test]
    fn a_shorter_grid_shows_fewer_octaves_rather_than_thinner_rows() {
        assert_eq!(octaves_in(60.0, 1.0), 1);
        assert_eq!(octaves_in(120.0, 1.0), 2);
        assert_eq!(octaves_in(1000.0, 1.0), MAX_OCTAVES);
        // At a larger ui scale a row is larger too, so the same height holds fewer of them.
        assert_eq!(octaves_in(120.0, 2.0), 1);
        // And a grid with no room at all still draws an octave rather than dividing by zero.
        assert_eq!(octaves_in(0.0, 1.0), 1);
    }

    /// Run the pads over several frames, feeding each one its own events.
    ///
    /// A warm-up frame comes first, for the same reason the roll's harness has one: egui works out
    /// what the pointer is over from the widget rects of the *previous* frame.
    fn frames(pattern: &mut Pattern, pads: &mut Pads, step: u32, events: &[Vec<egui::Event>]) {
        armed_frames(pattern, pads, step, true, events)
    }

    fn armed_frames(
        pattern: &mut Pattern,
        pads: &mut Pads,
        step: u32,
        record: bool,
        events: &[Vec<egui::Event>],
    ) {
        let ctx = egui::Context::default();
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
                        pads: &mut *pads,
                        terms: Terms { step, snap: step },
                        record,
                        metrics: Metrics::for_test(1.0),
                    };
                    show(ui, pattern, &mut context, 200.0);
                });
            });
        }
    }

    /// A point well inside the bottom row of pads, which is an octave's twelve semitones.
    fn pad(column: usize) -> egui::Pos2 {
        egui::pos2(20.0 + column as f32 * 70.0, 195.0)
    }

    fn touch(id: u64, phase: egui::TouchPhase, at: egui::Pos2) -> egui::Event {
        egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(id),
            phase,
            pos: at,
            force: None,
        }
    }

    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    #[test]
    fn a_tap_writes_a_note_and_the_mark_steps_on() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        frames(
            &mut pattern,
            &mut pads,
            480,
            &[vec![button(pad(0), true)], vec![button(pad(0), false)]],
        );

        assert_eq!(pattern.notes().len(), 1);
        let note = pattern.notes()[0];
        assert_eq!(note.start, 0);
        assert_eq!(note.length, 480);
        // The bottom row of an octave is its semitones, which are the even lanes.
        assert!(
            !pitch::is_quarter(note.lane),
            "{} is a quarter tone",
            note.lane
        );
        assert_eq!(pads.entry, 480);
    }

    #[test]
    fn a_tap_that_lands_and_lifts_inside_one_frame_still_lets_go() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        // Which is most taps: a frame spans a while when nothing is running, and both halves of a
        // quick click arrive inside it.
        frames(
            &mut pattern,
            &mut pads,
            480,
            &[vec![button(pad(0), true), button(pad(0), false)]],
        );

        assert_eq!(pattern.notes().len(), 1);
        assert_eq!(pads.entry, 480, "the pad was never let go of");
    }

    #[test]
    fn two_fingers_at_once_are_a_chord_on_one_step() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        frames(
            &mut pattern,
            &mut pads,
            480,
            &[
                // Fingers land a frame apart, which is what they do.
                vec![touch(1, egui::TouchPhase::Start, pad(0))],
                vec![touch(2, egui::TouchPhase::Start, pad(4))],
                vec![touch(1, egui::TouchPhase::End, pad(0))],
                vec![touch(2, egui::TouchPhase::End, pad(4))],
            ],
        );

        assert_eq!(pattern.notes().len(), 2);
        assert!(pattern.notes().iter().all(|note| note.start == 0));
        assert_ne!(pattern.notes()[0].lane, pattern.notes()[1].lane);
        assert_eq!(
            pads.entry, 480,
            "the chord left the mark on more than one step"
        );
    }

    #[test]
    fn a_finger_the_backend_also_reports_as_the_pointer_writes_one_note() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        frames(
            &mut pattern,
            &mut pads,
            480,
            &[
                vec![
                    touch(1, egui::TouchPhase::Start, pad(2)),
                    button(pad(2), true),
                ],
                vec![
                    touch(1, egui::TouchPhase::End, pad(2)),
                    button(pad(2), false),
                ],
            ],
        );

        assert_eq!(pattern.notes().len(), 1);
        assert_eq!(pads.entry, 480);
    }

    #[test]
    fn with_record_off_a_pad_sounds_and_writes_nothing() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        armed_frames(
            &mut pattern,
            &mut pads,
            480,
            false,
            &[vec![button(pad(0), true)], vec![button(pad(0), false)]],
        );

        assert!(pattern.is_empty());
        // And the mark stays where it is: nothing was written, so there is no step to move off.
        assert_eq!(pads.entry, 0);
    }

    #[test]
    fn a_key_played_while_something_runs_lands_at_the_playhead_and_is_held_to_length() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        // Down a hair after the second sixteenth, up a hair after the sixth: a note of four of
        // them, once the snap has had it.
        pads.captured(
            &mut pattern,
            &key(true, 60, Some(487.0)),
            0,
            terms(),
            &mut outcome,
        );
        assert_eq!(pattern.notes().len(), 1, "it appears as it is played");
        pads.captured(
            &mut pattern,
            &key(false, 60, Some(2401.0)),
            0,
            terms(),
            &mut outcome,
        );

        let note = pattern.notes()[0];
        assert_eq!(note.start, 480);
        assert_eq!(note.length, 1920);
        assert_eq!(note.lane, 120);
        assert_eq!(note.velocity, 100);
        // The playhead placed it, so the mark is not involved and does not move.
        assert_eq!(pads.entry, 0);
        // A key is already sounding on its way through, and must not be sounded twice.
        assert!(outcome.auditions.is_empty());
    }

    #[test]
    fn a_key_played_while_nothing_runs_lands_at_the_mark_and_steps_on() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.captured(&mut pattern, &key(true, 60, None), 0, terms(), &mut outcome);
        pads.captured(&mut pattern, &key(true, 64, None), 0, terms(), &mut outcome);
        pads.settle(&pattern, terms().step, &mut outcome);
        assert_eq!(pads.entry, 0, "the mark moved under a key still down");

        pads.captured(
            &mut pattern,
            &key(false, 60, None),
            0,
            terms(),
            &mut outcome,
        );
        pads.captured(
            &mut pattern,
            &key(false, 64, None),
            0,
            terms(),
            &mut outcome,
        );
        pads.settle(&pattern, terms().step, &mut outcome);

        assert_eq!(pattern.notes().len(), 2);
        assert!(pattern.notes().iter().all(|note| note.start == 0));
        assert_eq!(pads.entry, 480);
    }

    #[test]
    fn a_key_played_into_a_slot_that_is_not_sounding_lands_at_the_mark() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();
        pads.entry = 960;

        // The playhead is running through slot 2 while slot 0 is the one being written into, so
        // its position says nothing about where this note goes.
        let mut event = key(true, 60, Some(7000.0));
        event.slot = 2;
        pads.captured(&mut pattern, &event, 0, terms(), &mut outcome);

        assert_eq!(pattern.notes()[0].start, 960);
    }

    #[test]
    fn a_key_held_across_the_loop_point_keeps_the_length_it_was_held_for() {
        let mut pattern = Pattern::default();
        pattern.set_length(1920);
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.captured(
            &mut pattern,
            &key(true, 60, Some(1440.0)),
            0,
            terms(),
            &mut outcome,
        );
        // Round the loop and a little past it: a note of two sixteenths, not a negative one.
        pads.captured(
            &mut pattern,
            &key(false, 60, Some(480.0)),
            0,
            terms(),
            &mut outcome,
        );

        let note = pattern.notes()[0];
        assert_eq!(note.start, 1440);
        // 960 ticks were played, and what is left of the pattern clips it to 480.
        assert_eq!(note.length, 480);
    }

    #[test]
    fn a_key_brushed_is_a_note_of_the_grid_rather_than_the_shortest_thing_there_is() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.captured(
            &mut pattern,
            &key(true, 60, Some(0.0)),
            0,
            terms(),
            &mut outcome,
        );
        pads.captured(
            &mut pattern,
            &key(false, 60, Some(30.0)),
            0,
            terms(),
            &mut outcome,
        );
        assert_eq!(pattern.notes()[0].length, terms().step);

        // With the snap off it is exactly what was played, down to the shortest note there is.
        let exact = Terms { step: 480, snap: 1 };
        let mut pattern = Pattern::default();
        pads.captured(
            &mut pattern,
            &key(true, 62, Some(0.0)),
            0,
            exact,
            &mut outcome,
        );
        pads.captured(
            &mut pattern,
            &key(false, 62, Some(30.0)),
            0,
            exact,
            &mut outcome,
        );
        assert_eq!(pattern.notes()[0].length, MIN_LENGTH);
    }

    #[test]
    fn record_disarmed_under_a_held_key_lets_go_of_it() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.captured(
            &mut pattern,
            &key(true, 60, Some(0.0)),
            0,
            terms(),
            &mut outcome,
        );
        pads.drop_keys();

        // The release never comes, so the note keeps the length it was written at — and nothing is
        // left held, so the pad it is drawn on is not lit for good.
        pads.captured(
            &mut pattern,
            &key(false, 60, Some(9600.0)),
            0,
            terms(),
            &mut outcome,
        );
        assert_eq!(pattern.notes().len(), 1);
        assert_eq!(pattern.notes()[0].length, terms().step);
        assert!(pads.held.is_empty());
    }

    #[test]
    fn held_lanes_are_sorted_and_a_lane_under_two_fingers_is_named_once() {
        let mut pattern = Pattern::default();
        let mut pads = pads();
        let mut outcome = Outcome::default();

        pads.press(
            &mut pattern,
            Source::Pointer,
            127,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.press(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(1)),
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );
        pads.press(
            &mut pattern,
            Source::Finger(egui::TouchDeviceId(0), egui::TouchId(2)),
            120,
            VELOCITY,
            terms(),
            Write::Mark,
            &mut outcome,
        );

        assert_eq!(pads.held_lanes(), vec![120, 127]);
    }

    #[test]
    fn the_mark_reads_as_a_bar_and_a_beat() {
        assert_eq!(position(0), "1.1");
        assert_eq!(position(TICKS_PER_BEAT), "1.2");
        assert_eq!(position(BAR), "2.1");
        assert_eq!(position(BAR + TICKS_PER_BEAT / 2), "2.1+960");
    }
}
