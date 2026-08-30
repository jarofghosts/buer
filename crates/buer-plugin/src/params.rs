//! The parameters, and the bank that rides along beside them.
//!
//! What is a parameter and what is not is decided by one question: would a host want to record it?
//! The pattern being played, yes — that is how a bank becomes an arrangement. The notes in it, no;
//! nothing automates a note. So the notes are a persisted field with a hook on restore, and the
//! slot is an `IntParam`.

use buer_core::generate::Spec;
use buer_core::mpeout::{OutputMode, Zone};
use buer_core::scala::{KeyboardMap, ScalaScale, ScalaTuning};
use buer_core::{Bank, SLOTS};
use nih_plug::params::persist::PersistentField;
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;

use crate::shared::Shared;

/// The steps the editor offers for how large it draws itself. Changing step asks the host for a
/// window the same amount larger or smaller, so the interface keeps filling the one it is given.
pub const UI_SCALES: [f32; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5];

/// The window a fresh instance opens at, in points, laid out for a scale of 1. Named because the
/// editor also reads it: a window still exactly this size is one nobody has chosen, and so one it
/// may size for the scale it is about to draw at.
pub const DEFAULT_WINDOW: (u32, u32) = (1000, 720);

/// How much of the keyboard down the left of the roll is named.
///
/// Twenty-four rows to the octave is a lot of rows to count, and naming every one of them is a lot
/// of text beside a grid you are trying to read. So it is a choice, and it is remembered.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LaneNames {
    /// Only each c, which is enough to find your place.
    #[default]
    Octaves,
    /// Every semitone. The quarter tones between them stay blank, so the twelve you already know
    /// are still the ones that stand out.
    Notes,
    /// Every lane, quarter tones included.
    Lanes,
}

impl LaneNames {
    /// Whether this lane gets a name, given that there is room for one.
    pub fn names(self, lane: buer_core::Lane) -> bool {
        match self {
            LaneNames::Octaves => buer_core::pitch::degree(lane) == 0,
            LaneNames::Notes => !buer_core::pitch::is_quarter(lane),
            LaneNames::Lanes => true,
        }
    }

    pub const ALL: [(&'static str, LaneNames); 3] = [
        ("octaves", LaneNames::Octaves),
        ("notes", LaneNames::Notes),
        ("lanes", LaneNames::Lanes),
    ];
}

/// Which dialect the quarter tone leaves in.
#[derive(Enum, Debug, PartialEq, Eq, Clone, Copy)]
pub enum PitchOutParam {
    /// A channel per note and a bend on it. Arrives everywhere, which is why it is the default.
    #[id = "mpe"]
    #[name = "mpe"]
    Mpe,
    /// A clap tuning note expression. Exact, and understood by fewer instruments.
    #[id = "clap"]
    #[name = "clap"]
    Clap,
    /// Both, for a host that passes one dialect and drops the other. An instrument that honours
    /// both plays a semitone sharp, not a quarter tone.
    #[id = "both"]
    #[name = "both"]
    Both,
}

impl From<PitchOutParam> for OutputMode {
    fn from(value: PitchOutParam) -> Self {
        match value {
            PitchOutParam::Mpe => OutputMode::Mpe,
            PitchOutParam::Clap => OutputMode::Clap,
            PitchOutParam::Both => OutputMode::Both,
        }
    }
}

#[derive(Enum, Debug, PartialEq, Eq, Clone, Copy)]
pub enum ZoneParam {
    #[id = "lower"]
    #[name = "lower"]
    Lower,
    #[id = "upper"]
    #[name = "upper"]
    Upper,
}

impl From<ZoneParam> for Zone {
    fn from(value: ZoneParam) -> Self {
        match value {
            ZoneParam::Lower => Zone::Lower,
            ZoneParam::Upper => Zone::Upper,
        }
    }
}

#[derive(Enum, Debug, PartialEq, Eq, Clone, Copy)]
pub enum ClockParam {
    /// Follow the host's transport.
    #[id = "host"]
    #[name = "host"]
    Host,
    /// Run at buer's own tempo, whatever the host is doing.
    #[id = "free"]
    #[name = "free"]
    Free,
}

#[derive(Params)]
pub struct BuerParams {
    /// The pattern to play. A parameter rather than a button so that a host can record and automate
    /// a change of pattern, which is the only thing that makes a bank more than a filing cabinet.
    #[id = "pattern"]
    pub pattern: IntParam,

    #[id = "pitchout"]
    pub pitch_out: EnumParam<PitchOutParam>,
    #[id = "mpezone"]
    pub mpe_zone: EnumParam<ZoneParam>,
    /// The member channels' bend range, in semitones. MPE's own default is ±48 and most instruments
    /// assume it; a narrower range places a quarter tone more precisely but agrees with less.
    #[id = "bendrng"]
    pub bend_range: IntParam,

    #[id = "clock"]
    pub clock: EnumParam<ClockParam>,
    /// The tempo free-running mode runs at.
    #[id = "freebpm"]
    pub free_tempo: FloatParam,
    /// Free-running mode's transport. A parameter and not a button: the audio thread cannot write to
    /// its own parameters, so a button would need a flag of its own and would then disagree with
    /// whatever the host thinks the value is.
    #[id = "play"]
    pub play: BoolParam,

    #[id = "loop"]
    pub looping: BoolParam,
    /// What share of its written length a note actually sounds for.
    #[id = "gate"]
    pub gate: FloatParam,
    /// Shifts everything by this many quarter tones. Odd numbers move by a quarter tone, which is
    /// the point of counting it in lanes rather than semitones.
    #[id = "transpos"]
    pub transpose: IntParam,
    /// Whether note input is passed through to the output. Setting a note output port makes most
    /// hosts route the keyboard through the plugin and expect it back out, so this defaults on.
    #[id = "thru"]
    pub pass_through: BoolParam,

    #[persist = "bank"]
    pub bank: BankSlot,

    /// The randomise panel's settings. Not parameters: nothing automates a seed, and putting nine
    /// generator controls in the host's parameter list would bury the four that matter.
    #[persist = "random"]
    pub random: RwLock<Spec>,
    /// The loaded scale file, as text, so a project carries its own tuning.
    #[persist = "scale"]
    pub scale: RwLock<StoredScale>,

    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,
    #[persist = "uiscale"]
    pub ui_scale: RwLock<f32>,
    /// Whether [`Self::ui_scale`] is a value someone picked, rather than the one it starts at.
    /// Until it is, the editor follows the host's own scaling.
    #[persist = "uiscaleset"]
    pub ui_scale_set: RwLock<bool>,
    /// How much of the keyboard is named. A view setting, but one worth outliving the window.
    #[persist = "names"]
    pub lane_names: RwLock<LaneNames>,
}

impl BuerParams {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            pattern: IntParam::new(
                "pattern",
                1,
                IntRange::Linear {
                    min: 1,
                    max: SLOTS as i32,
                },
            ),
            pitch_out: EnumParam::new("pitch out", PitchOutParam::Mpe),
            mpe_zone: EnumParam::new("mpe zone", ZoneParam::Lower),
            bend_range: IntParam::new("bend range", 48, IntRange::Linear { min: 1, max: 96 })
                .with_unit(" st")
                .with_value_to_string(Arc::new(|value| format!("±{value}")))
                // Every parameter that formats its value has to be able to read one back: CLAP asks
                // for text-to-value on all of them or none, and a plugin that answers for some is a
                // plugin whose typed-in values silently do nothing.
                .with_string_to_value(Arc::new(|text| {
                    parse_number(text).map(|value| value.abs().round() as i32)
                })),
            clock: EnumParam::new("clock", ClockParam::Host),
            free_tempo: FloatParam::new(
                "free tempo",
                120.0,
                FloatRange::Linear {
                    min: 20.0,
                    max: 300.0,
                },
            )
            .with_unit(" bpm")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            play: BoolParam::new("play", false),
            looping: BoolParam::new("loop", true),
            gate: FloatParam::new(
                "gate",
                1.0,
                FloatRange::Linear {
                    min: 0.05,
                    max: 1.5,
                },
            )
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            transpose: IntParam::new("transpose", 0, IntRange::Linear { min: -48, max: 48 })
                .with_value_to_string(Arc::new(|value| {
                    let semitones = value as f32 / 2.0;
                    format!("{semitones:+} st")
                }))
                // Semitones on the way out, so semitones on the way in: two lanes to the semitone.
                .with_string_to_value(Arc::new(|text| {
                    parse_number(text).map(|semitones| (semitones * 2.0).round() as i32)
                })),
            pass_through: BoolParam::new("pass through", true),
            bank: BankSlot::new(shared),
            random: RwLock::new(Spec::default()),
            scale: RwLock::new(StoredScale::default()),
            editor_state: EguiState::from_size(DEFAULT_WINDOW.0, DEFAULT_WINDOW.1),
            ui_scale: RwLock::new(UI_SCALES[0]),
            ui_scale_set: RwLock::new(false),
            lane_names: RwLock::new(LaneNames::default()),
        }
    }

    pub fn ui_scale(&self) -> f32 {
        *self.ui_scale.read()
    }

    pub fn ui_scale_is_set(&self) -> bool {
        *self.ui_scale_set.read()
    }

    pub fn set_ui_scale(&self, scale: f32) {
        *self.ui_scale.write() = scale;
        *self.ui_scale_set.write() = true;
    }
}

/// The first number in a string, sign and decimal point included, ignoring whatever unit or
/// decoration is written around it.
///
/// Written by hand rather than reached for from `formatters` because the strings here carry a `±`
/// or a leading `+`, and a plain `parse` refuses both.
fn parse_number(text: &str) -> Option<f32> {
    let mut number = String::new();
    for character in text.trim().chars() {
        match character {
            '-' | '+' if number.is_empty() => number.push(character),
            '0'..='9' | '.' => number.push(character),
            _ if number.is_empty() => continue,
            _ => break,
        }
    }
    number.parse().ok()
}

/// A `.scl` scale and its optional `.kbm` map, as the text they arrived as.
///
/// The text rather than the parsed scale, for the reason mater keeps it that way: a project that
/// carries its own tuning has to carry something a person can read, and re-parsing on load is free.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct StoredScale {
    pub name: String,
    pub scl: String,
    pub kbm_name: String,
    pub kbm: String,
}

impl StoredScale {
    pub fn is_empty(&self) -> bool {
        self.scl.trim().is_empty()
    }

    /// Parse it, or say why it will not parse.
    pub fn tuning(&self) -> Result<Option<ScalaTuning>, String> {
        if self.is_empty() {
            return Ok(None);
        }
        let scale =
            ScalaScale::parse(&self.scl).map_err(|error| format!("{}: {error}", self.name))?;
        let keymap = if self.kbm.trim().is_empty() {
            None
        } else {
            Some(
                KeyboardMap::parse(&self.kbm)
                    .map_err(|error| format!("{}: {error}", self.kbm_name))?,
            )
        };
        Ok(Some(ScalaTuning::new(scale, keymap)))
    }
}

/// The pattern bank, in the plugin's state, so an instance is a self-contained sequence.
///
/// A hand-written [`PersistentField`] rather than a plain mutex for the reason mater's sample slot
/// is one: `set` is the hook. A host restoring its project fires it, and the bank reaches the audio
/// thread before the next block rather than after somebody clicks something.
pub struct BankSlot {
    stored: Mutex<Bank>,
    shared: Arc<Shared>,
}

impl BankSlot {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            stored: Mutex::new(Bank::default()),
            shared,
        }
    }

    /// Take an edited bank and publish it. Main thread only.
    pub fn store(&self, bank: Bank) {
        let published = Arc::new(bank.clone());
        *self.stored.lock() = bank;
        self.shared.publish_bank(published);
    }

    pub fn snapshot(&self) -> Bank {
        self.stored.lock().clone()
    }
}

impl<'a> PersistentField<'a, Bank> for BankSlot {
    fn set(&self, new_value: Bank) {
        let mut bank = new_value;
        // Ids are not written to the file, so every note in a bank read off disk claims to be note
        // zero until this runs.
        bank.normalise();
        let notes: usize = bank.patterns.iter().map(|p| p.notes().len()).sum();
        let used = bank.patterns.iter().filter(|p| !p.is_empty()).count();
        let published = Arc::new(bank.clone());
        *self.stored.lock() = bank;
        self.shared.publish_restored_bank(published);
        self.shared.set_status(if notes == 0 {
            "restored an empty bank".to_string()
        } else {
            format!("restored {used} patterns, {notes} notes")
        });
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&Bank) -> R,
    {
        f(&self.stored.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buer_core::{Note, TICKS_PER_BEAT};

    #[test]
    fn naming_the_octaves_names_only_the_cs() {
        // c4 is lane 120, c#4 is 122, and 121 is the quarter tone between them.
        assert!(LaneNames::Octaves.names(120));
        assert!(!LaneNames::Octaves.names(121));
        assert!(!LaneNames::Octaves.names(122));
    }

    #[test]
    fn naming_the_notes_leaves_the_quarter_tones_blank() {
        assert!(LaneNames::Notes.names(120));
        assert!(!LaneNames::Notes.names(121));
        assert!(LaneNames::Notes.names(122));
    }

    #[test]
    fn naming_the_lanes_names_all_of_them() {
        for lane in 0..24 {
            assert!(LaneNames::Lanes.names(lane));
        }
    }

    #[test]
    fn a_value_typed_back_in_is_the_value_that_was_shown() {
        let shared = Arc::new(Shared::default());
        let params = BuerParams::new(shared);
        for value in [-48, -25, -1, 0, 1, 24, 48] {
            let shown = params
                .transpose
                .normalized_value_to_string(params.transpose.preview_normalized(value), false);
            let back = params
                .transpose
                .string_to_normalized_value(&shown)
                .expect("transpose could not read its own output");
            assert_eq!(params.transpose.preview_plain(back), value, "{shown:?}");
        }
        for value in [1, 2, 12, 48, 96] {
            let shown = params
                .bend_range
                .normalized_value_to_string(params.bend_range.preview_normalized(value), false);
            let back = params
                .bend_range
                .string_to_normalized_value(&shown)
                .expect("bend range could not read its own output");
            assert_eq!(params.bend_range.preview_plain(back), value, "{shown:?}");
        }
    }

    #[test]
    fn a_number_is_found_whatever_is_written_round_it() {
        assert_eq!(parse_number("±48 st"), Some(48.0));
        assert_eq!(parse_number("+12 st"), Some(12.0));
        assert_eq!(parse_number("-0.5 st"), Some(-0.5));
        assert_eq!(parse_number("nothing here"), None);
    }

    #[test]
    fn a_restored_bank_hands_its_notes_names_of_their_own() {
        let shared = Arc::new(Shared::default());
        let slot = BankSlot::new(shared.clone());

        let mut bank = Bank::default();
        let pattern = bank.pattern_mut(0);
        pattern.insert(Note::new(0, 480, 120, 100));
        pattern.insert(Note::new(TICKS_PER_BEAT, 480, 121, 100));
        // As a file would leave them.
        for pattern in &mut bank.patterns {
            for note in pattern.notes().iter() {
                let _ = note;
            }
        }

        PersistentField::set(&slot, bank);
        let restored = slot.snapshot();
        let ids: Vec<_> = restored.pattern(0).notes().iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn restoring_a_bank_says_so_and_hands_it_straight_to_the_audio_thread() {
        let shared = Arc::new(Shared::default());
        let slot = BankSlot::new(shared.clone());
        let mut bank = Bank::default();
        bank.pattern_mut(0).insert(Note::new(0, 480, 120, 100));

        PersistentField::set(&slot, bank);
        assert!(shared.handoff.lock().incoming_bank.is_some());
        assert_eq!(shared.status(), "restored 1 patterns, 1 notes");
    }

    #[test]
    fn an_edit_does_not_look_like_a_restore() {
        let shared = Arc::new(Shared::default());
        let slot = BankSlot::new(shared.clone());
        let before = shared.generation.load(std::sync::atomic::Ordering::Acquire);
        slot.store(Bank::default());
        assert_eq!(
            shared.generation.load(std::sync::atomic::Ordering::Acquire),
            before
        );
    }
}
