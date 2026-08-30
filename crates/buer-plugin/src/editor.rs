//! The editor: a roll you draw on, with what governs it underneath.
//!
//! Most of the machinery here — the window, the scaling, the cell grid, the click-to-type readings —
//! is carried over from mater, which spent a long time getting it right, and the comments that
//! explain *why* have come with it. What is new is [`roll`], the [`icon`], and the bank strip.

pub mod icon;
pub mod roll;

use buer_core::generate::{self, Shape, Spec};
use buer_core::pattern::{Bank, LaneMask, NoteId, SLOTS};
use buer_core::{pitch, scales, TICKS_PER_BEAT};
use nih_plug::params::persist::PersistentField;
use nih_plug::prelude::*;
use nih_plug_egui::{
    create_egui_editor, egui, resizable_window::paint_resize_corner, widgets, EguiState,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::display;
use crate::midi;
use crate::params::{BuerParams, ClockParam, LaneNames, StoredScale, DEFAULT_WINDOW, UI_SCALES};
use crate::project;
use crate::shared::Shared;

/// The one accent colour: the playhead, notes, anything switched on.
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 146, 38);
/// The same hue lifted, for hover, for selection, and for text that has to stay legible on it.
pub const ACCENT_BRIGHT: egui::Color32 = egui::Color32::from_rgb(255, 187, 112);
/// And dropped, for fills that sit behind something — a note's body, a slider's bar.
pub const ACCENT_FILL: egui::Color32 = egui::Color32::from_rgb(122, 60, 10);

/// Normal text, against the near-black everything here is painted on.
///
/// egui's dark theme puts this at gray(140), which is about 5:1 — a quarter of the contrast a host
/// carries on its own chrome, and low enough that a parameter name reads as faint at any size. This
/// is about 10:1.
pub const TEXT: egui::Color32 = egui::Color32::from_gray(200);
/// What dimmed text is tinted halfway towards.
///
/// egui fades a disabled `Ui` — and anything `weak()` — halfway to `fade_out_to_color`, whose only
/// source is `noninteractive.weak_bg_fill`. At the gray(27) it ships as, that target *is* the
/// background, so a dimmed label landed at 2.2:1: greyed past reading rather than out of the way.
pub const TEXT_FADE: egui::Color32 = egui::Color32::from_gray(75);

/// Width of one column of the control grid. Every cell is a whole number of these, so however the
/// rows wrap they still line up.
const CELL_WIDTH: f32 = 215.0;
const CELL_HEIGHT: f32 = 42.0;
/// Height of the control inside a cell, and of the line above it that names it and reads its value.
/// Fixed, because clicking a reading to type an exact value swaps it for a text field, and a taller
/// field would jog every row below it.
const CONTROL_HEIGHT: f32 = 20.0;
const HEADER_HEIGHT: f32 = 18.0;
/// Room between the interface and the window's edges. The panel's own margin is no use: the
/// resizable window hands its contents the full clip rect, which is outside that margin.
const WINDOW_PADDING: f32 = 8.0;
const SECTION_GAP: f32 = 6.0;
/// The share of the window the roll takes when the panels want everything else, and the floor that
/// share is held above.
const ROLL_FRACTION: f32 = 0.7;
const ROLL_MIN_HEIGHT: f32 = 240.0;
/// One cell of the pattern bank strip.
const SLOT_WIDTH: f32 = 46.0;
const SLOT_HEIGHT: f32 = 30.0;
/// How wide the settings menu is, in columns of the control grid: enough for a row of radio buttons
/// to lie flat, and no wider than the default window has room for.
const SETTINGS_COLUMNS: usize = 3;
/// How many banks back the undo stack reaches.
const HISTORY_DEPTH: usize = 64;

/// The snap and note-length palettes, coarsest first. `1` is no snap at all.
const DIVISIONS: [(&str, u32); 8] = [
    ("1/1", TICKS_PER_BEAT * 4),
    ("1/2", TICKS_PER_BEAT * 2),
    ("1/4", TICKS_PER_BEAT),
    ("1/8", TICKS_PER_BEAT / 2),
    ("1/8t", TICKS_PER_BEAT / 3),
    ("1/16", TICKS_PER_BEAT / 4),
    ("1/16t", TICKS_PER_BEAT / 6),
    ("1/32", TICKS_PER_BEAT / 8),
];

/// Every size the editor lays out by hand, multiplied by the current ui scale.
///
/// egui's own zoom factor is no use here: this integration hands egui a screen rect derived from the
/// native scale alone, so changing the zoom would leave layout and rendering disagreeing. The style
/// and these metrics carry the scaling instead, and a point stays a pixel.
#[derive(Copy, Clone)]
pub struct Metrics {
    scale: f32,
}

impl Metrics {
    /// A cell that many columns of the grid wide.
    fn cell(self, ui: &egui::Ui, columns: usize) -> egui::Vec2 {
        egui::vec2(self.span(ui, columns), CELL_HEIGHT * self.scale)
    }

    /// How wide that many columns are, the gaps between them included.
    fn span(self, ui: &egui::Ui, columns: usize) -> f32 {
        let columns = columns.max(1) as f32;
        CELL_WIDTH * self.scale * columns + ui.spacing().item_spacing.x * (columns - 1.0)
    }

    fn columns(self, ui: &egui::Ui, width: f32) -> f32 {
        let gap = ui.spacing().item_spacing.x;
        (width + gap) / (CELL_WIDTH * self.scale + gap)
    }

    /// How many whole columns it takes to hold something this wide.
    fn columns_for(self, ui: &egui::Ui, width: f32) -> usize {
        // A hair of slack either way, so something exactly a whole number of columns wide is
        // neither pushed into one more nor cut down to one fewer by the last bit of arithmetic.
        (self.columns(ui, width) - 0.001).ceil().max(1.0) as usize
    }

    /// And how many fit within it.
    fn columns_in(self, ui: &egui::Ui, width: f32) -> usize {
        (self.columns(ui, width) + 0.001).floor().max(1.0) as usize
    }

    fn padding(self) -> egui::Margin {
        egui::Margin::same(self.at(WINDOW_PADDING) as i8)
    }

    pub fn at(self, points: f32) -> f32 {
        points * self.scale
    }

    pub fn scale(self) -> f32 {
        self.scale
    }

    /// Metrics at a given scale, for the tests that run a frame without a window.
    #[cfg(test)]
    pub fn for_test(scale: f32) -> Self {
        Self { scale }
    }
}

/// Whole-bank snapshots, because a diff is not worth what it costs here.
///
/// A busy bank is on the order of a hundred kilobytes; sixty-four of them is a few megabytes, which
/// is nothing beside what egui allocates per frame. A note-level diff would buy that back and would
/// have to get move, resize, clipping, id assignment and a whole-pattern generate all exactly right,
/// in both directions. Snapshots cannot be wrong.
#[derive(Default)]
struct History {
    past: VecDeque<(&'static str, Bank)>,
    future: Vec<(&'static str, Bank)>,
    /// The gesture in progress, and the bank as it was before it started.
    open: Option<(&'static str, Bank)>,
}

impl History {
    /// Take the before-image. A `begin` while one is open is ignored, so a re-entered gesture cannot
    /// split into two entries.
    fn begin(&mut self, label: &'static str, bank: &Bank) {
        if self.open.is_none() {
            self.open = Some((label, bank.clone()));
        }
    }

    fn end(&mut self, bank: &Bank) {
        let Some((label, before)) = self.open.take() else {
            return;
        };
        // A gesture that changed nothing leaves no step: a click that selected a note and let go
        // should not have to be undone.
        if &before == bank {
            return;
        }
        if self.past.len() >= HISTORY_DEPTH {
            self.past.pop_front();
        }
        self.past.push_back((label, before));
        self.future.clear();
    }

    /// One edit that is not a drag.
    fn once(&mut self, label: &'static str, before: &Bank, after: &Bank) {
        self.begin(label, before);
        self.end(after);
    }

    fn undo(&mut self, current: &Bank) -> Option<(&'static str, Bank)> {
        let (label, bank) = self.past.pop_back()?;
        self.future.push((label, current.clone()));
        Some((label, bank))
    }

    fn redo(&mut self, current: &Bank) -> Option<(&'static str, Bank)> {
        let (label, bank) = self.future.pop()?;
        if self.past.len() >= HISTORY_DEPTH {
            self.past.pop_front();
        }
        self.past.push_back((label, current.clone()));
        Some((label, bank))
    }

    fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.open = None;
    }
}

struct EditorState {
    /// The editor's working copy, and the one that is edited. Published to the audio thread and
    /// written back into the parameter's slot whenever it changes.
    bank: Bank,
    /// The generation the bank was last taken from, so a project loading underneath can be told
    /// from an edit made here.
    seen: usize,
    /// The slot being edited, which is often not the slot sounding.
    slot: usize,
    /// Whether the slot being edited follows the one sounding.
    follow: bool,
    view: roll::View,
    gesture: roll::Gesture,
    selection: Vec<NoteId>,
    history: History,
    snap: u32,
    /// The length a fresh note is drawn at, or `None` to take it from the snap.
    draw_length: Option<u32>,
    /// The scale the style was last built for, so it is rebuilt only when it changes.
    styled_for: Option<f32>,
    /// The height everything under the roll wanted when it was last drawn, which is what the roll
    /// gives up to it. Nothing is known about it until it has been drawn once.
    panels_height: Option<f32>,
    /// The ui scale the roll's zoom was last measured in. Kept apart from `styled_for`, which the
    /// build closure resets whenever the window reopens: the zoom is already in screen points by
    /// then, and rescaling it from 1.0 again would double it every time the editor was closed.
    view_scale: f32,
    /// Which built-in scale the chooser is on, and the lane its tonic sits on. View state: what
    /// was actually stamped is on the pattern.
    scale_choice: usize,
    scale_root: u8,
    /// Whether export writes the whole bank, and how many times a pattern is laid end to end.
    export_bank: bool,
    export_repeats: u32,
    /// Whether the settings menu is open over the window.
    settings_open: bool,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            bank: Bank::default(),
            // Nothing has been taken yet, so the first frame syncs whatever the parameter holds.
            seen: usize::MAX,
            slot: 0,
            follow: false,
            view: roll::View::default(),
            gesture: roll::Gesture::None,
            selection: Vec::new(),
            history: History::default(),
            snap: TICKS_PER_BEAT / 4,
            draw_length: None,
            styled_for: None,
            panels_height: None,
            view_scale: 1.0,
            scale_choice: 0,
            scale_root: 0,
            export_bank: false,
            export_repeats: 1,
            settings_open: false,
        }
    }
}

impl EditorState {
    fn draw_length(&self) -> u32 {
        self.draw_length.unwrap_or(self.snap.max(1))
    }
}

/// Takes the host's DPI scaling, and remembers what it is so the layout can divide it back out.
///
/// The factor is real and worth having: it is what makes the window the right size for the screen
/// and the text sharp on it. What it must not do is change how large the interface *looks*. It used
/// to, by a route nobody could see: nih-plug's egui integration refuses a scale factor while the
/// editor is open and applies it the *next* time the window opens. See [`layout_scale`].
struct HostDpi {
    inner: Box<dyn Editor>,
    shared: Arc<Shared>,
}

impl Editor for HostDpi {
    fn set_scale_factor(&self, factor: f32) -> bool {
        // Only remember what was actually taken: the integration turns a factor down while the
        // window is open, and the wrapper then keeps sizing everything by the previous one.
        let accepted = self.inner.set_scale_factor(factor);
        if accepted {
            self.shared.host_dpi.store(factor, Ordering::Relaxed);
            self.shared.host_dpi_reported.store(true, Ordering::Relaxed);
        }
        accepted
    }

    fn spawn(
        &self,
        parent: ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        self.inner.spawn(parent, context)
    }

    fn size(&self) -> (u32, u32) {
        self.inner.size()
    }

    fn param_value_changed(&self, id: &str, normalized_value: f32) {
        self.inner.param_value_changed(id, normalized_value);
    }

    fn param_modulation_changed(&self, id: &str, modulation_offset: f32) {
        self.inner.param_modulation_changed(id, modulation_offset);
    }

    fn param_values_changed(&self) {
        self.inner.param_values_changed();
    }
}

pub fn create(params: Arc<BuerParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    let egui_state = params.editor_state.clone();
    let host_scale = shared.clone();

    let inner = create_egui_editor(
        egui_state.clone(),
        EditorState::default(),
        // A reopened window is a fresh context with egui's own style, so ask for ours again.
        |_, state: &mut EditorState| state.styled_for = None,
        move |ctx, setter, state| {
            // Anything the audio thread displaced is dropped here, on the main thread.
            shared.collect_garbage();
            sync(state, &params, &shared);
            handle_dropped_files(ctx, setter, &params, &shared, state);

            let host = HostScale::read(&shared);
            let scale = layout_scale(ui_scale(&params, host, display_scale()), host.factor);
            let opening = state.styled_for.is_none();
            if state.styled_for != Some(scale) {
                apply_style(ctx, scale);
                state.styled_for = Some(scale);
            }
            if state.view_scale != scale {
                state.view.rescale(state.view_scale, scale);
                state.view_scale = scale;
            }
            let metrics = Metrics { scale };
            if opening {
                size_for_scale(ctx, &egui_state, setter, scale);
            }

            let running = shared.running.load(Ordering::Relaxed);
            if running {
                ctx.request_repaint();
            }

            window(ctx, &egui_state, setter, egui::vec2(760.0, 520.0), |ui| {
                egui::Frame::NONE
                    .inner_margin(metrics.padding())
                    .show(ui, |ui| {
                        header(ui, &params, &shared, &egui_state, setter, state, metrics);
                        ui.add_space(metrics.at(SECTION_GAP));
                        bank_strip(ui, &params, &shared, setter, state, metrics);
                        ui.add_space(metrics.at(SECTION_GAP));
                        roll_controls(ui, &params, setter, state, metrics);
                        ui.add_space(metrics.at(SECTION_GAP));

                        let height = roll_height(ui, state, metrics);
                        show_roll(ui, &params, &shared, state, metrics, height);

                        ui.add_space(metrics.at(SECTION_GAP));
                        let panels = egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.set_max_width(
                                (ui.available_width() - ui.spacing().scroll.bar_width).max(1.0),
                            );
                            transport(ui, &params, setter, metrics);
                            ui.separator();
                            randomise(ui, &params, &shared, state, metrics);
                        });
                        state.panels_height = Some(panels.content_size.y);
                    });
            });

            settings(ctx, &params, &shared, setter, state, metrics);

            keys(ctx, &params, state, &shared);
        },
    )?;

    Some(Box::new(HostDpi {
        inner,
        shared: host_scale,
    }))
}

/// Take the bank from the parameter whenever something other than this editor has replaced it.
fn sync(state: &mut EditorState, params: &Arc<BuerParams>, shared: &Arc<Shared>) {
    let generation = shared.generation.load(Ordering::Acquire);
    if generation == state.seen {
        return;
    }
    // Undoing past a project load would swap in a different project's notes under a project that
    // has already been loaded, which is not an undo of anything anybody did. The ids were handed
    // out again on the way in, so the selection has to go with it.
    state.bank = params.bank.snapshot();
    state.slot = state.bank.current.min(SLOTS - 1);
    state.history.clear();
    state.selection.clear();
    state.gesture = roll::Gesture::None;
    state.seen = generation;
}

/// Write an edited bank back, and hand it to the audio thread.
fn commit(state: &mut EditorState, params: &Arc<BuerParams>) {
    state.bank.current = state.slot;
    params.bank.store(state.bank.clone());
}

fn roll_height(ui: &egui::Ui, state: &EditorState, metrics: Metrics) -> f32 {
    let available = ui.available_height();
    let panels = state
        .panels_height
        .map(|height| height + metrics.at(SECTION_GAP))
        .unwrap_or(0.0);
    (available - panels)
        .max(metrics.at(ROLL_MIN_HEIGHT))
        .min(available.max(1.0))
        .max(available * ROLL_FRACTION)
}

fn show_roll(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &mut EditorState,
    metrics: Metrics,
    height: f32,
) {
    let sounding = shared.sounding_slot.load(Ordering::Relaxed);
    if state.follow && sounding != state.slot {
        state.slot = sounding.min(SLOTS - 1);
    }
    let playhead = shared
        .running
        .load(Ordering::Relaxed)
        .then(|| shared.playhead.load(Ordering::Relaxed))
        .filter(|_| sounding == state.slot);

    let snap = state.snap;
    let draw_length = state.draw_length();
    let names = *params.lane_names.read();
    let slot = state.slot;
    // The before-image for the undo stack, taken only on the frame a press could start a gesture.
    // Cloning the bank every frame to have one ready would be sixty copies a second of something
    // nobody asked for.
    let pressing = ui.input(|input| input.pointer.any_pressed());
    let before = pressing.then(|| state.bank.clone());

    let outcome = {
        let EditorState {
            bank,
            view,
            gesture,
            selection,
            ..
        } = state;
        let mut context = roll::Context {
            view,
            gesture,
            selection,
            snap,
            draw_length,
            playhead,
            names,
            metrics,
        };
        roll::show(ui, bank.pattern_mut(slot), &mut context, height)
    };

    if let (Some(label), Some(before)) = (outcome.began, &before) {
        state.history.begin(label, before);
    }
    if outcome.changed {
        commit(state, params);
    }
    if outcome.ended {
        let bank = state.bank.clone();
        state.history.end(&bank);
    }
    if let Some(note) = outcome.audition {
        shared.audition(note);
    }
}

/// Keys the roll answers to. Read from the context rather than from a response, so they work
/// wherever the pointer is inside the window.
fn keys(
    ctx: &egui::Context,
    params: &Arc<BuerParams>,
    state: &mut EditorState,
    shared: &Arc<Shared>,
) {
    // Somebody typing an exact value into a reading is not somebody deleting the selection, and
    // neither is somebody with the settings menu open over the roll.
    if state.settings_open || ctx.memory(|memory| memory.focused().is_some()) {
        return;
    }

    /// Only the keys this answers to. Taking a copy of the whole `InputState` instead would carry
    /// every event of the frame with it, sixty times a second, to read four booleans out of.
    struct Pressed {
        modifiers: egui::Modifiers,
        delete: bool,
        select_all: bool,
        undo: bool,
        deselect: bool,
    }
    let input = ctx.input(|input| Pressed {
        modifiers: input.modifiers,
        delete: input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace),
        select_all: input.key_pressed(egui::Key::A),
        undo: input.key_pressed(egui::Key::Z),
        deselect: input.key_pressed(egui::Key::Escape),
    });

    let modifiers = input.modifiers;
    let mut changed = None;

    if input.delete && !state.selection.is_empty() {
        {
            let before = state.bank.clone();
            let slot = state.slot;
            for id in std::mem::take(&mut state.selection) {
                state.bank.pattern_mut(slot).remove_id(id);
            }
            let after = state.bank.clone();
            state.history.once("delete notes", &before, &after);
            changed = Some("deleted");
        }
    }

    // Clicking empty ground draws a note now, so letting go of a selection needs a key of its own.
    if input.deselect {
        state.selection.clear();
    }

    if modifiers.command && input.select_all {
        state.selection = state
            .bank
            .pattern(state.slot)
            .notes()
            .iter()
            .map(|note| note.id)
            .collect();
    }

    if modifiers.command && input.undo {
        let current = state.bank.clone();
        let step = if modifiers.shift {
            state.history.redo(&current).map(|step| (step, "redid"))
        } else {
            state.history.undo(&current).map(|step| (step, "undid"))
        };
        if let Some(((label, bank), what)) = step {
            state.bank = bank;
            state.selection.clear();
            shared.set_status(format!("{what} {label}"));
            changed = Some("");
        }
    }

    if let Some(what) = changed {
        commit(state, params);
        if !what.is_empty() {
            shared.set_status(format!("{what} the selection"));
        }
    }
}

fn header(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &Arc<EguiState>,
    setter: &ParamSetter,
    editor: &mut EditorState,
    metrics: Metrics,
) {
    ui.horizontal(|ui| {
        // The mark, turning with the playhead: the wheel doing what a wheel does, and the transport
        // light the header needed anyway.
        let radius = metrics.at(11.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(radius * 2.2, radius * 2.2),
            egui::Sense::hover(),
        );
        let running = shared.running.load(Ordering::Relaxed);
        let length = editor.bank.pattern(editor.slot).length.max(1) as f32;
        let phase = if running {
            shared.playhead.load(Ordering::Relaxed) / length
        } else {
            0.0
        };
        ui.painter().extend(icon::wheel(
            rect.center(),
            radius,
            phase,
            egui::Stroke::new(
                metrics.at(1.2).max(1.0),
                if running { ACCENT_BRIGHT } else { TEXT_FADE },
            ),
        ));

        ui.label(egui::RichText::new("buer").heading().color(TEXT));

        if ui.button("save…").clicked() {
            save_project(setter, shared);
        }
        if ui.button("load…").clicked() {
            load_project(setter, shared);
        }
        if ui.button("import midi…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("midi file", &["mid", "midi"])
                .pick_file()
            {
                import_midi(&path, setter, params, shared, editor);
            }
        }
        if ui.button("export midi…").clicked() {
            export_midi(params, shared, editor);
        }

        ui.add_space(metrics.at(SECTION_GAP));
        // Selectable rather than a plain button, so the header says whether the menu is up — it is
        // drawn over this row and covers the mark, and there is no other way to tell.
        if ui
            .selectable_label(editor.settings_open, "settings…")
            .clicked()
        {
            editor.settings_open = !editor.settings_open;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui_scale_control(ui, params, shared, state, setter);
        });
    });

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(shared.status()).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("drop a .buer, .mid or .scl file anywhere").weak());
        });
    });
}

fn bank_strip(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    setter: &ParamSetter,
    state: &mut EditorState,
    metrics: Metrics,
) {
    let sounding = shared.sounding_slot.load(Ordering::Relaxed);
    let wanted = (params.pattern.value() - 1).max(0) as usize;

    ui.horizontal_wrapped(|ui| {
        for slot in 0..SLOTS {
            let size = egui::vec2(metrics.at(SLOT_WIDTH), metrics.at(SLOT_HEIGHT));
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            let editing = slot == state.slot;
            let painter = ui.painter_at(rect);

            painter.rect_filled(
                rect,
                egui::CornerRadius::same(2),
                if editing {
                    ACCENT_FILL
                } else {
                    ui.visuals().extreme_bg_color
                },
            );
            // Sounding and editing are frequently different slots, and showing only one of them is
            // the mistake to avoid: the outline is what is playing, the fill is what is being drawn
            // on.
            let outline = if slot == sounding {
                ACCENT_BRIGHT
            } else if slot == wanted {
                ACCENT
            } else {
                TEXT_FADE
            };
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(2),
                egui::Stroke::new(metrics.at(1.0).max(1.0), outline),
                egui::StrokeKind::Inside,
            );

            // The pattern's notes, as dots, so a slot can be told from an empty one at a glance.
            let pattern = state.bank.pattern(slot);
            if !pattern.is_empty() {
                let inner = rect.shrink(metrics.at(4.0));
                let length = pattern.length.max(1) as f32;
                let mut dots = Vec::with_capacity(pattern.notes().len());
                for note in pattern.notes() {
                    let x = inner.left() + inner.width() * note.start as f32 / length;
                    let y = inner.bottom() - inner.height() * note.lane as f32 / 255.0;
                    dots.push(egui::Shape::rect_filled(
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(1.5, 1.5)),
                        egui::CornerRadius::ZERO,
                        if editing { ACCENT_BRIGHT } else { ACCENT },
                    ));
                }
                painter.extend(dots);
            }

            painter.text(
                rect.left_top() + egui::vec2(metrics.at(3.0), metrics.at(2.0)),
                egui::Align2::LEFT_TOP,
                (slot + 1).to_string(),
                egui::FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size),
                if editing { ACCENT_BRIGHT } else { TEXT_FADE },
            );

            if response.clicked() {
                state.slot = slot;
                state.selection.clear();
                setter.begin_set_parameter(&params.pattern);
                setter.set_parameter(&params.pattern, slot as i32 + 1);
                setter.end_set_parameter(&params.pattern);
            }
        }

        ui.add_space(metrics.at(SECTION_GAP));
        let mut follow = state.follow;
        if ui.checkbox(&mut follow, "follow").changed() {
            state.follow = follow;
        }
        if ui.button("clear").clicked() {
            let before = state.bank.clone();
            let slot = state.slot;
            state.bank.pattern_mut(slot).clear();
            let after = state.bank.clone();
            state.history.once("clear pattern", &before, &after);
            state.selection.clear();
            commit(state, params);
        }
    });
}

fn roll_controls(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    setter: &ParamSetter,
    state: &mut EditorState,
    metrics: Metrics,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("snap").small().weak());
        for (name, ticks) in DIVISIONS {
            chip(ui, name, state.snap == ticks, || state.snap = ticks);
        }
        chip(ui, "off", state.snap == 1, || state.snap = 1);

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("length").small().weak());
        // "draw" means a click makes a note one snap long and the drag sets the rest, which is the
        // plain reading of drawing to length. Any other choice makes a click that exact length, and
        // dragging still overrides it.
        chip(ui, "draw", state.draw_length.is_none(), || {
            state.draw_length = None
        });
        for (name, ticks) in DIVISIONS {
            chip(ui, name, state.draw_length == Some(ticks), || {
                state.draw_length = Some(ticks)
            });
        }

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("names").small().weak());
        let current = *params.lane_names.read();
        for (label, option) in LaneNames::ALL {
            chip(ui, label, current == option, || {
                *params.lane_names.write() = option
            });
        }

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("bars").small().weak());
        let slot = state.slot;
        let length = state.bank.pattern(slot).length;
        let bars = (length / (TICKS_PER_BEAT * 4)).max(1);
        let mut wanted = bars;
        if ui.small_button("−").clicked() {
            wanted = bars.saturating_sub(1).max(1);
        }
        ui.label(
            egui::RichText::new(format!("{bars}"))
                .monospace()
                .color(TEXT),
        );
        if ui.small_button("+").clicked() {
            wanted = (bars + 1).min(64);
        }
        if wanted != bars {
            let before = state.bank.clone();
            state
                .bank
                .pattern_mut(slot)
                .set_length(wanted * TICKS_PER_BEAT * 4);
            let after = state.bank.clone();
            state.history.once("set pattern length", &before, &after);
            commit(state, params);
        }

        ui.add_space(metrics.at(SECTION_GAP));
        let mut looping = params.looping.value();
        if ui.checkbox(&mut looping, "loop").changed() {
            setter.begin_set_parameter(&params.looping);
            setter.set_parameter(&params.looping, looping);
            setter.end_set_parameter(&params.looping);
        }
        if params.clock.value() == ClockParam::Free {
            let mut play = params.play.value();
            if ui.checkbox(&mut play, "play").changed() {
                setter.begin_set_parameter(&params.play);
                setter.set_parameter(&params.play, play);
                setter.end_set_parameter(&params.play);
            }
        }
    });
}

/// A small selectable label, accented when chosen. [`radio`] with the circles taken off — a length
/// palette is a set of shapes, not a list of words.
fn chip(ui: &mut egui::Ui, name: &str, selected: bool, choose: impl FnOnce()) {
    let text = egui::RichText::new(name).small();
    let text = if selected {
        text.color(ACCENT_BRIGHT)
    } else {
        text.color(TEXT_FADE)
    };
    if ui.selectable_label(selected, text).clicked() && !selected {
        choose();
    }
}

/// The settings menu: what is set once for an instance and then left alone.
///
/// Both of the sections behind it used to sit under the roll, where between them they were four
/// rows of controls permanently in the way of the thing they configure. Over the window instead,
/// and only when asked for.
fn settings(
    ctx: &egui::Context,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    setter: &ParamSetter,
    state: &mut EditorState,
    metrics: Metrics,
) {
    if !state.settings_open {
        return;
    }

    let screen = ctx.screen_rect();
    let menu = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
        // An area sizes itself to whatever goes in it, and a wrapping row offered unlimited width
        // never wraps. The cell grid needs a width to wrap within, so it is handed one.
        let width = metrics
            .span(ui, SETTINGS_COLUMNS)
            .min(screen.width() - metrics.at(WINDOW_PADDING) * 2.0)
            .max(metrics.span(ui, 1));
        ui.set_max_width(width);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("settings").heading().color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("close").clicked() {
                    state.settings_open = false;
                }
            });
        });
        ui.separator();

        // At a large ui scale in a small window the two sections are taller than the screen, and a
        // menu that cannot be scrolled back to its own close button is a trap.
        egui::ScrollArea::vertical()
            .max_height(screen.height() * 0.75)
            .show(ui, |ui| {
                ui.set_max_width(width);
                output(ui, params, setter, metrics);
                ui.separator();
                scale(ui, params, shared, state, metrics);
            });
    });

    if menu.should_close() {
        state.settings_open = false;
    }
}

/// How pitch leaves the plugin.
fn output(ui: &mut egui::Ui, params: &Arc<BuerParams>, setter: &ParamSetter, metrics: Metrics) {
    ui.label(egui::RichText::new("output").strong());
    ui.horizontal_wrapped(|ui| {
        radio(ui, "pitch out", &params.pitch_out, setter, metrics);
        let rotates = params.pitch_out.value() != crate::params::PitchOutParam::Clap;
        dimmed(ui, rotates, metrics, |ui| {
            radio(ui, "mpe zone", &params.mpe_zone, setter, metrics);
        });
        dimmed(ui, rotates, metrics, |ui| {
            labelled(ui, "bend range", &params.bend_range, setter, metrics);
        });
        labelled(ui, "transpose", &params.transpose, setter, metrics);
        toggle(ui, "pass through", &params.pass_through, setter, metrics);
    });
    if params.pitch_out.value() == crate::params::PitchOutParam::Both {
        ui.label(
            egui::RichText::new(
                "both — for a host that passes one dialect and drops the other. an instrument \
                 that honours both plays a semitone sharp, not a quarter tone",
            )
            .small()
            .weak()
            .italics(),
        );
    }
}

fn transport(ui: &mut egui::Ui, params: &Arc<BuerParams>, setter: &ParamSetter, metrics: Metrics) {
    ui.label(egui::RichText::new("transport").strong());
    ui.horizontal_wrapped(|ui| {
        radio(ui, "clock", &params.clock, setter, metrics);
        let free = params.clock.value() == ClockParam::Free;
        dimmed(ui, free, metrics, |ui| {
            labelled(ui, "free tempo", &params.free_tempo, setter, metrics);
        });
        dimmed(ui, free, metrics, |ui| {
            toggle(ui, "play", &params.play, setter, metrics);
        });
        toggle(ui, "loop", &params.looping, setter, metrics);
        labelled(ui, "gate", &params.gate, setter, metrics);
    });
}

fn save_project(setter: &ParamSetter, shared: &Arc<Shared>) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("buer project", &[project::EXTENSION])
        .set_file_name(format!("untitled.{}", project::EXTENSION))
        .save_file()
    else {
        return;
    };
    let path = project::with_extension(path);
    match project::write(&path, setter.raw_context.get_state()) {
        Ok(()) => shared.set_status(format!("saved {}", project::label(&path))),
        Err(error) => shared.set_status(error),
    }
}

fn load_project(setter: &ParamSetter, shared: &Arc<Shared>) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("buer project", &[project::EXTENSION])
        .pick_file()
    else {
        return;
    };
    open_project(&path, setter, shared);
}

fn open_project(path: &Path, setter: &ParamSetter, shared: &Arc<Shared>) {
    match project::read(path) {
        Ok(state) => {
            setter.raw_context.set_state(state);
            shared.set_status(format!("loaded {}", project::label(path)));
        }
        Err(error) => shared.set_status(error),
    }
}

fn handle_dropped_files(
    ctx: &egui::Context,
    setter: &ParamSetter,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &mut EditorState,
) {
    let dropped = ctx.input(|input| input.raw.dropped_files.clone());
    for file in dropped {
        let Some(path) = file.path else { continue };
        match path
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("buer") => open_project(&path, setter, shared),
            Some("mid") | Some("midi") => import_midi(&path, setter, params, shared, state),
            Some("scl") => load_scale(&path, params, shared, false),
            Some("kbm") => load_scale(&path, params, shared, true),
            // A better answer than mater's, which takes anything it does not recognise for audio.
            _ => shared.set_status(format!(
                "don't know what to do with {}",
                project::label(&path)
            )),
        }
    }
}

fn import_midi(
    path: &Path,
    setter: &ParamSetter,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &mut EditorState,
) {
    match midi::read(path) {
        Ok(imported) => {
            // The file's own tempo becomes the free-running one, so importing and pressing play
            // gives back what the file sounded like without a host transport.
            if let Some(tempo) = imported.tempo {
                setter.begin_set_parameter(&params.free_tempo);
                setter.set_parameter(&params.free_tempo, tempo.clamp(20.0, 300.0));
                setter.end_set_parameter(&params.free_tempo);
            }
            let before = state.bank.clone();
            let filled = midi::into_bank(&imported, &mut state.bank);
            let after = state.bank.clone();
            state.history.once("import midi", &before, &after);
            state.selection.clear();
            state.slot = 0;
            commit(state, params);
            shared.set_status(midi::describe(&imported, filled));
        }
        Err(error) => shared.set_status(error),
    }
}

fn export_midi(params: &Arc<BuerParams>, shared: &Arc<Shared>, state: &EditorState) {
    let pattern = state.bank.pattern(state.slot);
    let name = if pattern.name.trim().is_empty() {
        "untitled".to_string()
    } else {
        pattern.name.clone()
    };
    let Some(path) = rfd::FileDialog::new()
        .add_filter("midi file", &["mid"])
        .set_file_name(format!("{name}.mid"))
        .save_file()
    else {
        return;
    };
    let path = if path.extension().is_some() {
        path
    } else {
        path.with_extension("mid")
    };

    let tempo = params.free_tempo.value();
    let zone = params.mpe_zone.value().into();
    let range = params.bend_range.value() as f32;
    match midi::write(
        &path,
        &state.bank,
        &midi::Export {
            slot: state.slot,
            all: state.export_bank,
            tempo,
            zone,
            range,
            repeats: state.export_repeats,
        },
    ) {
        Ok(()) => shared.set_status(format!(
            "wrote {} with the bends baked in",
            project::label(&path)
        )),
        Err(error) => shared.set_status(error),
    }
}

fn load_scale(path: &Path, params: &Arc<BuerParams>, shared: &Arc<Shared>, keymap: bool) {
    let Ok(text) = std::fs::read_to_string(path) else {
        shared.set_status(format!("could not read {}", project::label(path)));
        return;
    };
    {
        let mut stored = params.scale.write();
        if keymap {
            stored.kbm_name = project::label(path);
            stored.kbm = text;
        } else {
            stored.name = project::label(path);
            stored.scl = text;
        }
    }
    // Say now whether it parses, rather than at the moment somebody presses stamp.
    let stored = params.scale.read().clone();
    match stored.tuning() {
        Ok(Some(_)) => shared.set_status(format!(
            "loaded {} — press stamp to put it on the lanes",
            project::label(path)
        )),
        Ok(None) => shared.set_status("that scale file has nothing in it"),
        Err(error) => shared.set_status(error),
    }
}

/// The scale that decides which lanes a note may land on. Behind the settings menu, because the
/// column of toggles down the left of the roll is the same constraint and is always there.
fn scale(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &mut EditorState,
    metrics: Metrics,
) {
    let slot = state.slot;

    ui.label(egui::RichText::new("scale").strong());
    ui.horizontal_wrapped(|ui| {
        let named = {
            let pattern = state.bank.pattern(slot);
            if pattern.scale.is_empty() {
                "by hand".to_string()
            } else {
                pattern.scale.clone()
            }
        };
        ui.label(egui::RichText::new(format!("lanes: {named}")).small());

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("root").small().weak());
        let root = state.scale_root;
        if ui.small_button("−").clicked() {
            state.scale_root = (root + LANES - 1) % LANES;
        }
        ui.label(
            egui::RichText::new(pitch::describe(120 + state.scale_root))
                .monospace()
                .color(TEXT),
        );
        if ui.small_button("+").clicked() {
            state.scale_root = (root + 1) % LANES;
        }

        ui.add_space(metrics.at(SECTION_GAP));
        let chosen = state.scale_choice.min(scales::BUILTIN.len().saturating_sub(1));
        egui::ComboBox::from_id_salt("built-in scale")
            .selected_text(scales::BUILTIN[chosen].0)
            .show_ui(ui, |ui| {
                for (index, (name, _)) in scales::BUILTIN.iter().enumerate() {
                    if ui
                        .selectable_label(index == chosen, *name)
                        .clicked()
                    {
                        state.scale_choice = index;
                    }
                }
            });
        // Stamping rather than intersecting live, so a toggle always changes what it looks like it
        // changes. See `buer_core::scales`.
        if ui.button("stamp").clicked() {
            let before = state.bank.clone();
            let mask = scales::builtin(state.scale_choice, state.scale_root);
            let name = scales::BUILTIN[state.scale_choice].0.to_string();
            {
                let pattern = state.bank.pattern_mut(slot);
                pattern.constraint = mask;
                pattern.scale = format!("{name} on {}", pitch::describe(120 + state.scale_root));
            }
            let after = state.bank.clone();
            state.history.once("stamp a scale", &before, &after);
            commit(state, params);
        }

        ui.add_space(metrics.at(SECTION_GAP));
        if ui.button("load .scl…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("scala scale", &["scl"])
                .pick_file()
            {
                load_scale(&path, params, shared, false);
            }
        }
        let stored: StoredScale = params.scale.read().clone();
        let loaded = !stored.is_empty();
        if ui
            .add_enabled(loaded, egui::Button::new("stamp .scl"))
            .clicked()
        {
            match stored.tuning() {
                Ok(Some(tuning)) => {
                    let (mask, worst) = scales::lanes_from_scala(&tuning, state.scale_root);
                    let before = state.bank.clone();
                    {
                        let pattern = state.bank.pattern_mut(slot);
                        pattern.constraint = mask;
                        pattern.scale = stored.name.clone();
                    }
                    let after = state.bank.clone();
                    state.history.once("stamp a scale", &before, &after);
                    commit(state, params);
                    shared.set_status(if worst < 1.0 {
                        format!("{}: every degree landed exactly", stored.name)
                    } else {
                        format!(
                            "{}: landed, the worst degree moved {worst:.0} cents onto its lane",
                            stored.name
                        )
                    });
                }
                Ok(None) => shared.set_status("no scale is loaded"),
                Err(error) => shared.set_status(error),
            }
        }
        if loaded {
            ui.label(egui::RichText::new(&stored.name).small().weak());
        }
        if ui.button("all lanes").clicked() {
            let before = state.bank.clone();
            {
                let pattern = state.bank.pattern_mut(slot);
                pattern.constraint = LaneMask::ALL;
                pattern.scale.clear();
            }
            let after = state.bank.clone();
            state.history.once("clear the lanes", &before, &after);
            commit(state, params);
        }
    });
}

/// The generators that write into whatever lanes [`scale`] left open.
fn randomise(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &mut EditorState,
    metrics: Metrics,
) {
    let slot = state.slot;

    ui.label(egui::RichText::new("randomise").strong());

    let mut spec = params.random.read().clone();
    let mut edited = false;

    ui.horizontal_wrapped(|ui| {
        let free = matches!(spec.shape, Shape::Free { .. });
        if ui.selectable_label(free, "free").clicked() && !free {
            spec.shape = Shape::Free {
                density: 0.5,
                rest: 0.15,
            };
            edited = true;
        }
        if ui.selectable_label(!free, "euclidean").clicked() && free {
            spec.shape = Shape::Euclid {
                steps: 16,
                pulses: 7,
                rotation: 0,
            };
            edited = true;
        }

        ui.add_space(metrics.at(SECTION_GAP));
        match &mut spec.shape {
            Shape::Free { density, rest } => {
                ui.label(egui::RichText::new("density").small().weak());
                edited |= ui.add(egui::Slider::new(density, 0.0..=1.0).show_value(false)).changed();
                ui.label(egui::RichText::new("rests").small().weak());
                edited |= ui.add(egui::Slider::new(rest, 0.0..=1.0).show_value(false)).changed();
            }
            Shape::Euclid {
                steps,
                pulses,
                rotation,
            } => {
                ui.label(egui::RichText::new("steps").small().weak());
                edited |= ui.add(egui::DragValue::new(steps).range(1..=64)).changed();
                ui.label(egui::RichText::new("pulses").small().weak());
                edited |= ui.add(egui::DragValue::new(pulses).range(0..=64)).changed();
                ui.label(egui::RichText::new("rotate").small().weak());
                edited |= ui.add(egui::DragValue::new(rotation).range(0..=63)).changed();
                // The rhythm itself, so the numbers are not the only way to read it.
                let mask = generate::euclid(*steps, (*pulses).min(*steps), *rotation);
                let drawn: String = (0..(*steps).min(64))
                    .map(|step| if mask & (1 << step) != 0 { 'x' } else { '.' })
                    .collect();
                ui.label(egui::RichText::new(drawn).monospace().color(ACCENT));
            }
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("grid").small().weak());
        for (name, ticks) in DIVISIONS {
            let selected = spec.grid == ticks;
            if ui
                .selectable_label(selected, egui::RichText::new(name).small())
                .clicked()
                && !selected
            {
                spec.grid = ticks;
                edited = true;
            }
        }

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("range").small().weak());
        edited |= ui
            .add(egui::DragValue::new(&mut spec.low).range(0..=255).custom_formatter(
                |value, _| pitch::describe(value as u8),
            ))
            .changed();
        edited |= ui
            .add(egui::DragValue::new(&mut spec.high).range(0..=255).custom_formatter(
                |value, _| pitch::describe(value as u8),
            ))
            .changed();

        ui.add_space(metrics.at(SECTION_GAP));
        ui.label(egui::RichText::new("velocity").small().weak());
        edited |= ui
            .add(egui::DragValue::new(&mut spec.velocity.0).range(1..=127))
            .changed();
        edited |= ui
            .add(egui::DragValue::new(&mut spec.velocity.1).range(1..=127))
            .changed();
    });

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("seed").small().weak());
        ui.label(
            egui::RichText::new(format!("{:08x}", spec.seed))
                .monospace()
                .color(TEXT),
        );
        // A new seed is rolled here and nowhere else: generation stays a pure function of the spec,
        // so the number on screen is always the number that made what you are looking at.
        if ui.button("⚄").on_hover_text("roll a new seed").clicked() {
            spec.seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos() as u64)
                .unwrap_or(0x5eed);
            edited = true;
        }

        ui.add_space(metrics.at(SECTION_GAP));
        if ui.button("generate").clicked() {
            let before = state.bank.clone();
            let mask = state.bank.pattern(slot).constraint;
            let length = state.bank.pattern(slot).length;
            let notes = generate::generate(
                length,
                &Spec {
                    allowed: mask,
                    ..spec.clone()
                },
            );
            state.bank.pattern_mut(slot).replace(notes);
            let after = state.bank.clone();
            state.history.once("generate", &before, &after);
            state.selection.clear();
            commit(state, params);
            shared.set_status(format!(
                "generated {} notes from seed {:08x}",
                state.bank.pattern(slot).notes().len(),
                spec.seed
            ));
        }
    });

    if edited {
        *params.random.write() = spec;
    }
}

/// Quarter tones in an octave, as the editor counts roots.
const LANES: u8 = 24;

/// What the host has said about scaling, if it has said anything.
///
/// Both halves are needed: `factor` starts at 1.0, which is also what a host scaling by 100 % would
/// send, so the number alone cannot tell a host that means it from one that has never spoken.
#[derive(Copy, Clone)]
struct HostScale {
    factor: f32,
    reported: bool,
}

impl HostScale {
    fn read(shared: &Shared) -> Self {
        Self {
            factor: shared.host_dpi.load(Ordering::Relaxed),
            reported: shared.host_dpi_reported.load(Ordering::Relaxed),
        }
    }
}

/// How large the interface should draw itself, in the units the `ui scale` readout is in.
///
/// Three sources, in order of how much they know. A scale set by hand is exactly itself, on any
/// host, for good. Failing that the host's own factor, because a host scaling its interface by 200 %
/// has sized this window for a 200 % interface. Failing *that* — a host that never reports one,
/// which is not rare — the desktop's own scaling.
fn ui_scale(params: &BuerParams, host: HostScale, display: f32) -> f32 {
    if params.ui_scale_is_set() {
        params.ui_scale()
    } else if host.reported && host.factor > 0.0 {
        host.factor
    } else {
        display
    }
}

/// What the desktop says it is scaled by, held to the range the steps offer.
fn display_scale() -> f32 {
    display::system_scale()
        .map(|scale| scale.clamp(UI_SCALES[0], UI_SCALES[UI_SCALES.len() - 1]))
        .unwrap_or(UI_SCALES[0])
}

/// What one laid-out point is worth, with the host's DPI scaling divided back out of `ui scale`.
///
/// The host's factor multiplies every point on its way to the screen, so leaving it in would make
/// the two scales compound: a host at 200 % would draw a 100 % interface at double size, and the
/// size would change whenever the host announced a new factor.
fn layout_scale(ui_scale: f32, host_dpi: f32) -> f32 {
    if host_dpi > 0.0 {
        ui_scale / host_dpi
    } else {
        ui_scale
    }
}

/// Rebuild the style for a given scale.
///
/// Everything here is set from a constant rather than adjusted from what is already there, so
/// applying it repeatedly cannot compound.
fn apply_style(ctx: &egui::Context, scale: f32) {
    ctx.all_styles_mut(|style| {
        style.text_styles = text_styles(scale);
        style.spacing = spacing(scale);
        paint(&mut style.visuals, scale);
    });
}

fn text_styles(scale: f32) -> BTreeMap<egui::TextStyle, egui::FontId> {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::{FontId, TextStyle};

    [
        (TextStyle::Small, FontId::new(10.5 * scale, Proportional)),
        (TextStyle::Body, FontId::new(13.5 * scale, Proportional)),
        (TextStyle::Button, FontId::new(13.5 * scale, Proportional)),
        (TextStyle::Heading, FontId::new(18.0 * scale, Proportional)),
        (TextStyle::Monospace, FontId::new(13.0 * scale, Monospace)),
    ]
    .into()
}

/// egui's default spacing, scaled. The fields left alone are widths of things we do not use.
fn spacing(scale: f32) -> egui::style::Spacing {
    let base = egui::style::Spacing::default();
    let margin = |margin: egui::Margin| egui::Margin::same((f32::from(margin.left) * scale) as i8);

    egui::style::Spacing {
        item_spacing: base.item_spacing * scale,
        window_margin: margin(base.window_margin),
        menu_margin: margin(base.menu_margin),
        button_padding: base.button_padding * scale,
        indent: base.indent * scale,
        interact_size: base.interact_size * scale,
        slider_width: base.slider_width * scale,
        slider_rail_height: base.slider_rail_height * scale,
        combo_width: base.combo_width * scale,
        text_edit_width: base.text_edit_width * scale,
        icon_width: base.icon_width * scale,
        icon_width_inner: base.icon_width_inner * scale,
        icon_spacing: base.icon_spacing * scale,
        scroll: egui::style::ScrollStyle {
            bar_width: base.scroll.bar_width * scale,
            handle_min_length: base.scroll.handle_min_length * scale,
            ..base.scroll
        },
        ..base
    }
}

/// Repaint egui's blues — selection, links, the text cursor — in the accent orange.
fn paint(visuals: &mut egui::Visuals, scale: f32) {
    visuals.selection.bg_fill = ACCENT_FILL;
    visuals.selection.stroke = egui::Stroke::new(scale, ACCENT_BRIGHT);
    visuals.hyperlink_color = ACCENT;
    visuals.text_cursor.stroke = egui::Stroke::new(2.0 * scale, ACCENT);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(scale, ACCENT);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(scale, ACCENT_BRIGHT);
    visuals.resize_corner_size = 12.0 * scale;
    // Text last, and both of these rather than one: lifting normal text without lifting what dimmed
    // text fades towards would only widen the gap between them.
    visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    visuals.widgets.noninteractive.weak_bg_fill = TEXT_FADE;
}

/// A resizable window, in place of nih-plug's.
///
/// nih-plug's `ResizableWindow` asks the host for a size in points and then resizes the drawing
/// surface to that many *pixels*, so at anything above 100 % the interface is painted into a corner
/// of its own window. This is the same idea, done in the units the rest of the integration uses.
fn window(
    ctx: &egui::Context,
    state: &Arc<EguiState>,
    setter: &ParamSetter,
    min_size: egui::Vec2,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let rect = ui.clip_rect();
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
        add_contents(&mut content);

        let corner_size = egui::Vec2::splat(ui.visuals().resize_corner_size);
        let corner_rect = egui::Rect::from_min_size(rect.max - corner_size, corner_size);
        let corner = ui.interact(
            corner_rect,
            egui::Id::new("resize corner"),
            egui::Sense::drag(),
        );

        if let Some(pointer) = corner.interact_pointer_pos() {
            if corner.dragged() {
                resize(
                    ctx,
                    state,
                    setter,
                    (pointer - rect.min + corner_size / 2.0).max(min_size),
                );
            }
        }

        paint_resize_corner(ui, &corner);
    });
}

fn resize(ctx: &egui::Context, state: &Arc<EguiState>, setter: &ParamSetter, size: egui::Vec2) {
    let asked = (
        size.x.round().max(1.0) as u32,
        size.y.round().max(1.0) as u32,
    );
    if state.size() == asked {
        return;
    }

    // `EguiState` keeps its size to itself. The persistence trait, which is how a host restoring a
    // project sets it, is the way in.
    if let Ok(resized) = Arc::try_unwrap(EguiState::from_size(asked.0, asked.1)) {
        PersistentField::set(state, resized);
    }
    setter.raw_context.request_resize();
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
}

/// How large the interface draws itself, in steps. Laid out right to left, so it reads
/// `ui scale [−] 125 % [+]`.
fn ui_scale_control(
    ui: &mut egui::Ui,
    params: &Arc<BuerParams>,
    shared: &Arc<Shared>,
    state: &Arc<EguiState>,
    setter: &ParamSetter,
) {
    // The size being drawn, not what the layout is working in: the host's DPI scaling has been
    // divided out of that one, and reporting it would show 100 % on a host set to 200 %.
    let host = HostScale::read(shared);
    let scale = ui_scale(params, host, display_scale());
    // The nearest step, so a state saved by a future version with other steps still lands somewhere.
    let step = UI_SCALES
        .iter()
        .position(|&candidate| candidate >= scale)
        .unwrap_or(UI_SCALES.len() - 1);

    let larger = ui.add_enabled(step + 1 < UI_SCALES.len(), egui::Button::new("+"));
    if larger.clicked() {
        rescale(ui.ctx(), params, state, setter, scale, UI_SCALES[step + 1]);
    }

    ui.label(egui::RichText::new(format!("{:.0} %", scale * 100.0)).monospace())
        .on_hover_text(format!(
            "{}\n{}",
            if params.ui_scale_is_set() {
                "how large the interface draws itself, saved with the instance"
            } else {
                "how large the interface draws itself — following the scaling below until you \
                 set it here, after which it is yours"
            },
            match (host.reported, display::system_scale()) {
                (true, _) => format!("the host reports {:.0} % scaling", host.factor * 100.0),
                (false, Some(display)) => format!(
                    "the host reports no scaling of its own; the desktop's is {:.0} %",
                    display * 100.0
                ),
                (false, None) => "neither the host nor the desktop reports any scaling".to_string(),
            }
        ));

    let smaller = ui.add_enabled(step > 0, egui::Button::new("−"));
    if smaller.clicked() {
        rescale(ui.ctx(), params, state, setter, scale, UI_SCALES[step - 1]);
    }

    ui.label(egui::RichText::new("ui scale").weak());
}

/// Draw the interface at a new size, and ask for a window that fits it.
fn rescale(
    ctx: &egui::Context,
    params: &Arc<BuerParams>,
    state: &Arc<EguiState>,
    setter: &ParamSetter,
    from: f32,
    to: f32,
) {
    params.set_ui_scale(to);
    resize(ctx, state, setter, rescaled(state.size(), from, to));
}

/// The window that holds an interface redrawn from one scale at another: the same window, by the
/// ratio between them.
fn rescaled(size: (u32, u32), from: f32, to: f32) -> egui::Vec2 {
    let (width, height) = (size.0 as f32, size.1 as f32);
    if from <= 0.0 {
        return egui::vec2(width, height);
    }
    let ratio = to / from;
    egui::vec2(width * ratio, height * ratio)
}

/// Size a window nobody has chosen yet for the scale it is about to be drawn at.
///
/// Only a window still at exactly [`DEFAULT_WINDOW`] is touched — any other size was dragged there
/// or restored from a project, and is theirs to keep. And only on opening, so that dragging a
/// window down to that size is not undone under the pointer.
fn size_for_scale(ctx: &egui::Context, state: &Arc<EguiState>, setter: &ParamSetter, scale: f32) {
    if state.size() == DEFAULT_WINDOW {
        resize(ctx, state, setter, rescaled(DEFAULT_WINDOW, 1.0, scale));
    }
}

fn cell(
    ui: &mut egui::Ui,
    label: &str,
    columns: usize,
    metrics: Metrics,
    reading: impl FnOnce(&mut egui::Ui),
    control: impl FnOnce(&mut egui::Ui),
) {
    let size = metrics.cell(ui, columns);
    ui.allocate_ui(size, |ui| {
        ui.vertical(|ui| {
            line(ui, egui::vec2(size.x, metrics.at(HEADER_HEIGHT)), |ui| {
                ui.label(egui::RichText::new(label).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), reading);
            });
            line(ui, egui::vec2(size.x, metrics.at(CONTROL_HEIGHT)), control);
        });
    });
}

/// One line of a cell, held at the size it is given however little goes in it — a checkbox is
/// narrow, and the cell still has to hold its column's width.
fn line(ui: &mut egui::Ui, size: egui::Vec2, contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
        |ui| {
            ui.set_min_size(size);
            contents(ui);
        },
    );
}

/// Grey a cell out where its parameter has nothing to do.
///
/// [`egui::Ui::add_enabled_ui`] on its own is not enough inside a wrapping row: the scope it opens
/// takes whatever is left of the line and lays the cell out within that, so a cell that reaches the
/// scope with too little room left runs off the edge instead of moving down.
fn dimmed(ui: &mut egui::Ui, enabled: bool, metrics: Metrics, add: impl FnOnce(&mut egui::Ui)) {
    // What is left of the line, which is not what `available_width` reports in a wrapping row: that
    // one answers with the width of a whole row, on the grounds that a wrap is always available.
    if ui.available_rect_before_wrap().width() < metrics.span(ui, 1) {
        ui.end_row();
    }
    ui.add_enabled_ui(enabled, add);
}

/// One parameter as a slider, with its value in the caption line above.
fn labelled<'a>(
    ui: &mut egui::Ui,
    label: &str,
    param: &'a impl Param,
    setter: &'a ParamSetter,
    metrics: Metrics,
) {
    let width = metrics.span(ui, 1);
    cell(
        ui,
        label,
        1,
        metrics,
        |ui| reading(ui, param, setter),
        |ui| {
            ui.add(
                widgets::ParamSlider::for_param(param, setter)
                    .without_value()
                    .with_width(width),
            );
        },
    );
}

/// A switch in the same cell shape as [`labelled`].
///
/// On and off are states, not values you slide between, so they get a checkbox — and the accent
/// colour, so a row of them can be read at a glance. The word goes in the caption line where every
/// other parameter's value is, so that the columns read straight down.
fn toggle(
    ui: &mut egui::Ui,
    label: &str,
    param: &BoolParam,
    setter: &ParamSetter,
    metrics: Metrics,
) {
    let current = param.value();
    cell(
        ui,
        label,
        1,
        metrics,
        |ui| {
            let text = egui::RichText::new(if current { "on" } else { "off" }).small();
            ui.label(if current {
                text.color(ACCENT)
            } else {
                text.weak()
            });
        },
        |ui| {
            let mut value = current;
            if current {
                let widgets = &mut ui.visuals_mut().widgets;
                widgets.inactive.fg_stroke.color = ACCENT;
                widgets.hovered.fg_stroke.color = ACCENT;
                widgets.active.fg_stroke.color = ACCENT_BRIGHT;
            }
            if ui.add(egui::Checkbox::without_text(&mut value)).changed() {
                setter.begin_set_parameter(param);
                setter.set_parameter(param, value);
                setter.end_set_parameter(param);
            }
        },
    );
}

/// A parameter's current value, beside its name, and the way to type an exact one in.
fn reading<P: Param>(ui: &mut egui::Ui, param: &P, setter: &ParamSetter) {
    // Keyed by the parameter rather than by where its cell landed, so that a row rewrapping under
    // the pointer cannot hand a half-typed value to whatever takes its place.
    let entry = egui::Id::new(("value entry", param.name()));
    let field = entry.with("field");
    // Monospace, so that a value changing under the pointer does not shuffle the name beside it.
    let font = egui::FontId::monospace(egui::TextStyle::Small.resolve(ui.style()).size);
    let room = ui.available_width();

    let Some(mut typed) = ui.memory(|memory| memory.data.get_temp::<String>(entry)) else {
        let text = param.to_string();
        let width = ui.fonts(|fonts| {
            fonts
                .layout_no_wrap(text.clone(), font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        });
        let response = ui.add(
            egui::Label::new(egui::RichText::new(&text).font(font))
                .sense(egui::Sense::click())
                .truncate(),
        );
        // Only where it has actually been cut short. A tooltip repeating what is already on screen
        // is noise, and every parameter in the window would carry one.
        let response = if width > room {
            response.on_hover_text(text)
        } else {
            response
        };

        if response.clicked() {
            ui.memory_mut(|memory| {
                memory.data.insert_temp(entry, param.to_string());
                // The field itself does not exist until the next frame, which egui allows.
                memory.request_focus(field);
            });
        }
        return;
    };

    let response = ui.add(
        egui::TextEdit::singleline(&mut typed)
            .id(field)
            .desired_width(room)
            .font(font),
    );

    if response.lost_focus() {
        // Enter commits, anything else — escape, or the pointer going elsewhere — abandons it.
        if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            if let Some(normalized) = param.string_to_normalized_value(&typed) {
                setter.begin_set_parameter(param);
                setter.set_parameter_normalized(param, normalized);
                setter.end_set_parameter(param);
            }
        }
        ui.memory_mut(|memory| memory.data.remove::<String>(entry));
    } else {
        ui.memory_mut(|memory| memory.data.insert_temp(entry, typed));
    }
}

/// An enum parameter as one radio button per variant.
///
/// Every alternative is named on screen, so the choice can be read and made without being opened or
/// dragged: a set of named modes is not a value you slide along.
fn radio<T: Enum + PartialEq + Copy + 'static>(
    ui: &mut egui::Ui,
    label: &str,
    param: &EnumParam<T>,
    setter: &ParamSetter,
    metrics: Metrics,
) {
    let names = T::variants();
    let columns = radio_columns(ui, names, metrics);

    cell(
        ui,
        label,
        columns,
        metrics,
        |_| {},
        |ui| {
            // A wrapping row wraps text by default, which would break a variant's name across two
            // lines rather than move the whole button down to the next one.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

            let current = param.value();
            let plain = ui.style().clone();
            for (index, name) in names.iter().enumerate() {
                let variant = T::from_index(index);
                let selected = variant == current;
                if selected {
                    let widgets = &mut ui.visuals_mut().widgets;
                    widgets.inactive.fg_stroke.color = ACCENT;
                    widgets.hovered.fg_stroke.color = ACCENT_BRIGHT;
                    widgets.active.fg_stroke.color = ACCENT_BRIGHT;
                }
                let response = ui.add(egui::RadioButton::new(selected, *name));
                if selected {
                    // Or the accent would carry over to every variant drawn after it.
                    ui.set_style(plain.clone());
                }

                if response.clicked() && !selected {
                    setter.begin_set_parameter(param);
                    setter.set_parameter(param, variant);
                    setter.end_set_parameter(param);
                }
            }
        },
    );
}

fn radio_columns(ui: &egui::Ui, names: &[&str], metrics: Metrics) -> usize {
    // Out to the right edge of what is on screen, not of the layout: at a large ui scale the row is
    // wider than the window, and a variant beyond the edge cannot be read or clicked.
    let room = (ui.clip_rect().right() - ui.max_rect().left() - ui.spacing().scroll.bar_width)
        .min(ui.max_rect().width())
        .max(metrics.span(ui, 1));
    metrics
        .columns_for(ui, radio_row_width(ui, names))
        .min(metrics.columns_in(ui, room))
}

/// How wide a row of radio buttons wants to be, by the same arithmetic the widget itself does.
fn radio_row_width(ui: &egui::Ui, names: &[&str]) -> f32 {
    let spacing = ui.spacing();
    let font = egui::TextStyle::Button.resolve(ui.style());

    let variants: f32 = names
        .iter()
        .map(|name| {
            let text = ui.fonts(|fonts| {
                fonts
                    .layout_no_wrap((*name).to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                    .size()
                    .x
            });
            (spacing.icon_width + spacing.icon_spacing + text).max(spacing.interact_size.x)
        })
        .sum();

    variants + spacing.item_spacing.x * (names.len().saturating_sub(1)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use buer_core::Note;

    #[test]
    fn a_gesture_that_changed_nothing_leaves_no_undo_step() {
        let bank = Bank::default();
        let mut history = History::default();
        history.begin("move note", &bank);
        history.end(&bank);
        assert!(history.undo(&bank).is_none());
    }

    #[test]
    fn a_drag_is_one_undo_step_however_many_frames_it_took() {
        let before = Bank::default();
        let mut after = before.clone();
        after
            .pattern_mut(0)
            .insert(Note::new(0, 480, 120, 100));

        let mut history = History::default();
        history.begin("draw note", &before);
        // Every frame of the drag tries again, and must not add a step of its own.
        history.begin("draw note", &after);
        history.begin("draw note", &after);
        history.end(&after);

        assert_eq!(history.past.len(), 1);
        let (label, back) = history.undo(&after).unwrap();
        assert_eq!(label, "draw note");
        assert_eq!(back, before);
    }

    #[test]
    fn undo_and_redo_walk_back_and_forth_over_the_same_steps() {
        let first = Bank::default();
        let mut second = first.clone();
        second.pattern_mut(0).insert(Note::new(0, 480, 120, 100));
        let mut third = second.clone();
        third.pattern_mut(0).insert(Note::new(480, 480, 121, 100));

        let mut history = History::default();
        history.once("a", &first, &second);
        history.once("b", &second, &third);

        let (_, back) = history.undo(&third).unwrap();
        assert_eq!(back, second);
        let (_, back) = history.undo(&second).unwrap();
        assert_eq!(back, first);
        assert!(history.undo(&first).is_none());
        let (_, forward) = history.redo(&first).unwrap();
        assert_eq!(forward, second);
    }

    #[test]
    fn a_new_edit_after_an_undo_throws_the_redo_away() {
        let first = Bank::default();
        let mut second = first.clone();
        second.pattern_mut(0).insert(Note::new(0, 480, 120, 100));

        let mut history = History::default();
        history.once("a", &first, &second);
        history.undo(&second);
        assert_eq!(history.future.len(), 1);

        let mut other = first.clone();
        other.pattern_mut(1).insert(Note::new(0, 480, 130, 100));
        history.once("b", &first, &other);
        assert!(history.future.is_empty());
    }

    #[test]
    fn the_stack_does_not_grow_without_end() {
        let mut history = History::default();
        let mut bank = Bank::default();
        for step in 0..HISTORY_DEPTH * 2 {
            let before = bank.clone();
            bank.pattern_mut(0)
                .insert(Note::new(step as u32 * 10, 480, 120, 100));
            let after = bank.clone();
            history.once("draw note", &before, &after);
        }
        assert_eq!(history.past.len(), HISTORY_DEPTH);
    }

    #[test]
    fn the_hosts_scaling_does_not_change_how_large_the_interface_looks() {
        // A 100 % interface on a host at 200 % lays out at half a point each, which the host's own
        // factor then doubles back to the size it was asked for.
        assert_eq!(layout_scale(1.0, 2.0), 0.5);
        assert_eq!(layout_scale(2.0, 2.0), 1.0);
    }

    #[test]
    fn a_silent_host_leaves_the_layout_alone() {
        assert_eq!(layout_scale(1.5, 1.0), 1.5);
        assert_eq!(layout_scale(1.5, 0.0), 1.5);
    }

    #[test]
    fn a_window_redrawn_at_another_scale_grows_by_the_ratio_between_them() {
        assert_eq!(rescaled((1000, 720), 1.0, 2.0), egui::vec2(2000.0, 1440.0));
        assert_eq!(rescaled((1000, 720), 2.0, 1.0), egui::vec2(500.0, 360.0));
        // A scale of zero is not one the steps offer, but dividing by it would hand back nan.
        assert_eq!(rescaled((1000, 720), 0.0, 2.0), egui::vec2(1000.0, 720.0));
    }
}
