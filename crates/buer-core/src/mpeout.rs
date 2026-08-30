//! Getting a quarter tone out of the plugin.
//!
//! There are two ways to say "50 cents above this note" to a CLAP host and they are not
//! interchangeable. A note expression of type *tuning* says it exactly, in semitones, and is what a
//! host reading the CLAP dialect wants. MPE says it by giving the note a channel of its own and
//! bending that channel, which is what every MIDI-dialect host and every hardware synth wants — and
//! is the only one that survives `NoteEvent::as_midi`, which has no arm for a tuning expression.
//!
//! So both are on offer and `mpe` is the default, because it is the one that always arrives. `both`
//! exists for the awkward middle, but is not the default: nih-plug declares the note port as
//! supporting either dialect and puts every event into one output queue, so a host that reads both
//! would apply the quarter tone twice.
//!
//! This module owns the channel rotation and the arithmetic. It does not know about nih-plug — it
//! emits [`Out`], and the plugin puts a `timing` on each one and sends it.

use crate::pattern::Lane;
use crate::pitch;

/// Which dialect, or both.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Note expressions only. Exact, and silent on pitch anywhere the MIDI dialect is in use.
    Clap,
    /// A channel per note and a pitch bend on it. Arrives everywhere.
    #[default]
    Mpe,
    /// Both at once, for a host that takes the MIDI dialect but whose instrument reads expressions.
    Both,
}

impl OutputMode {
    fn rotates(self) -> bool {
        matches!(self, OutputMode::Mpe | OutputMode::Both)
    }

    fn tunes(self) -> bool {
        matches!(self, OutputMode::Clap | OutputMode::Both)
    }
}

/// Which MPE zone. There is no `Off`: with the zone off there is no way to say a quarter tone in
/// MIDI at all, and saying it is what this plugin is for. `Clap` mode is the way to send everything
/// down one channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Zone {
    /// Master on channel 1, members on 2-16.
    #[default]
    Lower,
    /// Master on channel 16, members on 15-1.
    Upper,
}

/// MPE's member default: a member channel bends by ±48 semitones.
pub const DEFAULT_MEMBER_BEND_RANGE: f32 = 48.0;
/// The ordinary MIDI default, which MPE keeps for the master channel.
pub const MASTER_BEND_RANGE: f32 = 2.0;
/// How many member channels a zone has.
pub const MEMBERS: usize = 15;

const CHANNELS: usize = 16;
const CC_DATA_ENTRY_MSB: u8 = 6;
const CC_DATA_ENTRY_LSB: u8 = 38;
const CC_RPN_LSB: u8 = 100;
const CC_RPN_MSB: u8 = 101;
const RPN_BEND_RANGE: u8 = 0;
const RPN_MPE_CONFIGURATION: u8 = 6;

/// One event on its way out, without the sample offset the plugin adds.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Out {
    /// A channel pitch bend, normalised so that 0.5 is centre — nih-plug's own convention.
    Bend {
        channel: u8,
        value: f32,
    },
    NoteOn {
        voice_id: i32,
        channel: u8,
        note: u8,
        velocity: f32,
    },
    NoteOff {
        voice_id: i32,
        channel: u8,
        note: u8,
        velocity: f32,
    },
    /// A CLAP tuning note expression, in semitones.
    Tuning {
        voice_id: i32,
        channel: u8,
        note: u8,
        semitones: f32,
    },
    Cc {
        channel: u8,
        cc: u8,
        value: u8,
    },
}

/// A note that is sounding, and everything needed to stop it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    pub voice_id: i32,
    pub channel: u8,
    pub note: u8,
}

pub struct MpeOut {
    zone: Zone,
    mode: OutputMode,
    member_range: f32,
    /// Shifts every lane on the way out, in quarter tones. Here rather than in the pattern because
    /// what is written is what the roll shows; this is what is heard.
    transpose: i16,
    /// Which lane each channel is currently sounding, if any.
    owner: [Option<Lane>; CHANNELS],
    /// When each channel was last taken and last given back, so a free channel can be chosen by
    /// least-recently-used. A channel that has been idle longest is the one whose bend has had the
    /// most time to be acted on, so reusing it is the least likely to bend a note that is still
    /// audible somewhere downstream.
    taken_at: [u64; CHANNELS],
    freed_at: [u64; CHANNELS],
    clock: u64,
    next_voice_id: i32,
}

impl Default for MpeOut {
    fn default() -> Self {
        Self::new()
    }
}

impl MpeOut {
    pub fn new() -> Self {
        Self {
            zone: Zone::default(),
            mode: OutputMode::default(),
            member_range: DEFAULT_MEMBER_BEND_RANGE,
            transpose: 0,
            owner: [None; CHANNELS],
            taken_at: [0; CHANNELS],
            freed_at: [0; CHANNELS],
            clock: 1,
            next_voice_id: 1,
        }
    }

    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    pub fn zone(&self) -> Zone {
        self.zone
    }

    pub fn member_range(&self) -> f32 {
        self.member_range
    }

    pub fn set_transpose(&mut self, quarter_tones: i16) {
        self.transpose = quarter_tones;
    }

    /// Take new settings. Returns whether anything changed, which is the plugin's cue to re-announce
    /// the zone — a receiver that was told ±48 will keep believing it otherwise.
    pub fn configure(&mut self, zone: Zone, mode: OutputMode, member_range: f32) -> bool {
        let range = member_range.clamp(1.0, 96.0);
        let changed = self.zone != zone || self.mode != mode || self.member_range != range;
        self.zone = zone;
        self.mode = mode;
        self.member_range = range;
        changed
    }

    /// The master channel of the current zone.
    pub fn master(&self) -> u8 {
        match self.zone {
            Zone::Lower => 0,
            Zone::Upper => 15,
        }
    }

    /// The member channels, in the order they are handed out.
    fn members(&self) -> impl Iterator<Item = u8> {
        let zone = self.zone;
        (0..MEMBERS as u8).map(move |i| match zone {
            Zone::Lower => 1 + i,
            Zone::Upper => 14 - i,
        })
    }

    /// Announce the zone and the bend ranges.
    ///
    /// The MPE Configuration Message is how a receiver learns the zone exists at all; without it a
    /// synth treats fifteen channels of one note each as fifteen unrelated parts. RPN 0 then says
    /// what a bend on those channels means. Both are sent on activation and again whenever the zone
    /// or the range changes.
    pub fn announce(&self, emit: &mut dyn FnMut(Out)) {
        if !self.mode.rotates() {
            return;
        }
        let master = self.master();
        self.rpn(master, RPN_MPE_CONFIGURATION, MEMBERS as u8, 0, emit);
        self.bend_range_rpn(master, MASTER_BEND_RANGE, emit);
        for channel in self.members() {
            self.bend_range_rpn(channel, self.member_range, emit);
        }
    }

    fn bend_range_rpn(&self, channel: u8, range: f32, emit: &mut dyn FnMut(Out)) {
        let semitones = range.trunc().clamp(0.0, 96.0) as u8;
        let cents = ((range - range.trunc()) * 100.0).round().clamp(0.0, 99.0) as u8;
        self.rpn(channel, RPN_BEND_RANGE, semitones, cents, emit);
    }

    fn rpn(&self, channel: u8, rpn: u8, msb: u8, lsb: u8, emit: &mut dyn FnMut(Out)) {
        emit(Out::Cc {
            channel,
            cc: CC_RPN_MSB,
            value: 0,
        });
        emit(Out::Cc {
            channel,
            cc: CC_RPN_LSB,
            value: rpn,
        });
        emit(Out::Cc {
            channel,
            cc: CC_DATA_ENTRY_MSB,
            value: msb,
        });
        emit(Out::Cc {
            channel,
            cc: CC_DATA_ENTRY_LSB,
            value: lsb,
        });
        // Park the RPN selector, so a later data entry cannot land on this parameter by accident.
        emit(Out::Cc {
            channel,
            cc: CC_RPN_MSB,
            value: 127,
        });
        emit(Out::Cc {
            channel,
            cc: CC_RPN_LSB,
            value: 127,
        });
    }

    /// The 14-bit pitch-bend word a bend of this many semitones is, on a wheel with this range.
    ///
    /// Centre is 8192 of 0..=16383, so the two halves are 8192 and 8191 units wide. The arithmetic
    /// works outward from the centre and clamps, rather than scaling the whole span, so that "no
    /// bend" is exactly 8192 at every range — which is the number a MIDI monitor shows and the one
    /// worth being able to hold the code against.
    ///
    /// A quarter tone is half a semitone and nothing else, the grid having no free cents. At MPE's
    /// default ±48 that is 85 units, `8277`, sounding 49.8 cents — a fifth of a cent flat, which
    /// nothing can hear. At the ordinary ±2 it is `10240`, exact. Send the ±48 number to a receiver
    /// configured for ±2 and the note arrives a quarter of a semitone flat instead; the range is
    /// declared on the wire for that reason, in [`MpeOut::announce`].
    pub fn bend_word(semitones: f32, range: f32) -> u16 {
        const CENTRE: i32 = 8_192;
        const MAX: i32 = 16_383;
        // A nan or a zero range would divide the whole wheel into nothing; centre is the only
        // honest answer to "how far is half a semitone on a wheel that does not bend".
        if !range.is_finite() || range <= 0.0 {
            return CENTRE as u16;
        }
        let units = (semitones / range * CENTRE as f32).round() as i32;
        (CENTRE + units).clamp(0, MAX) as u16
    }

    /// And what nih-plug's `MidiPitchBend { value }` has to be to come out as that word.
    ///
    /// The wrapper computes `(value * 16383.0).round()` — 16383, not 16384 — so this division is
    /// its exact inverse. Writing the normalised value directly instead is within a unit in the
    /// middle and drifts at the ends, and cannot be checked against a monitor at all.
    pub fn bend_value(word: u16) -> f32 {
        word as f32 / 16_383.0
    }

    /// The bend for a note this far from its key, ready to send.
    pub fn bend(&self, semitones: f32) -> f32 {
        Self::bend_value(Self::bend_word(semitones, self.member_range))
    }

    /// Start a note on a lane. The events are emitted in the order a receiver needs them.
    pub fn note_on(&mut self, lane: Lane, velocity: u8, emit: &mut dyn FnMut(Out)) -> Voice {
        let lane = (lane as i16 + self.transpose).clamp(0, 255) as Lane;
        let note = pitch::note(lane);
        let semitones = if pitch::is_quarter(lane) { 0.5 } else { 0.0 };
        let voice_id = self.take_voice_id();

        let channel = if self.mode.rotates() {
            let channel = self.claim(lane, emit);
            // The bend goes first. A synth told to bend after the note has started hears a pitch
            // jump, and one with portamento glides audibly across it.
            emit(Out::Bend {
                channel,
                value: self.bend(semitones),
            });
            channel
        } else {
            self.master()
        };

        emit(Out::NoteOn {
            voice_id,
            channel,
            note,
            velocity: velocity as f32 / 127.0,
        });

        if self.mode.tunes() {
            emit(Out::Tuning {
                voice_id,
                channel,
                note,
                semitones,
            });
        }

        Voice {
            voice_id,
            channel,
            note,
        }
    }

    pub fn note_off(&mut self, voice: Voice, emit: &mut dyn FnMut(Out)) {
        emit(Out::NoteOff {
            voice_id: voice.voice_id,
            channel: voice.channel,
            note: voice.note,
            velocity: 0.0,
        });
        self.release(voice.channel);
    }

    /// Give every channel back without emitting anything. The caller has already sent the note-offs
    /// it wanted; this is the bookkeeping half.
    pub fn reset(&mut self) {
        self.owner = [None; CHANNELS];
        self.taken_at = [0; CHANNELS];
        self.freed_at = [0; CHANNELS];
        self.clock = 1;
    }

    fn take_voice_id(&mut self) -> i32 {
        let id = self.next_voice_id;
        // Wrap rather than overflow, and skip zero so a voice id is always something a host can
        // tell from an absent one at a glance in a log.
        self.next_voice_id = self.next_voice_id.checked_add(1).unwrap_or(1);
        id
    }

    /// Find a channel for a new note: the free one idle longest, or, if every one of them is
    /// sounding, the one taken longest ago.
    fn claim(&mut self, lane: Lane, emit: &mut dyn FnMut(Out)) -> u8 {
        self.clock += 1;

        let free = self
            .members()
            .filter(|&c| self.owner[c as usize].is_none())
            .min_by_key(|&c| self.freed_at[c as usize]);

        let channel = match free {
            Some(channel) => channel,
            None => {
                // Every member channel is sounding. Something has to give, and the oldest note is
                // the one least likely to still be wanted. The player caps polyphony below this, so
                // reaching here means the host is doing something unusual.
                let oldest = self
                    .members()
                    .min_by_key(|&c| self.taken_at[c as usize])
                    .unwrap_or_else(|| self.master());
                if let Some(lane) = self.owner[oldest as usize] {
                    emit(Out::NoteOff {
                        voice_id: 0,
                        channel: oldest,
                        note: pitch::note(lane),
                        velocity: 0.0,
                    });
                }
                oldest
            }
        };

        self.owner[channel as usize] = Some(lane);
        self.taken_at[channel as usize] = self.clock;
        channel
    }

    fn release(&mut self, channel: u8) {
        self.clock += 1;
        self.owner[channel as usize] = None;
        self.freed_at[channel as usize] = self.clock;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(f: impl FnOnce(&mut dyn FnMut(Out))) -> Vec<Out> {
        let mut events = Vec::new();
        f(&mut |e| events.push(e));
        events
    }

    #[test]
    fn a_quarter_tone_leaves_as_a_bend_of_fifty_cents() {
        let mut out = MpeOut::new();
        let events = collect(|emit| {
            out.note_on(121, 100, emit);
        });
        let Out::Bend { value, .. } = events[0] else {
            panic!("the bend does not come first: {events:?}");
        };
        // Half a semitone out of a ±48 range, as the 14-bit word a monitor would show.
        let raw = (value * 16383.0).round() as i32;
        assert_eq!(raw, 8277);
        // And what that actually sounds, which is the number that matters.
        let cents = (raw - 8192) as f32 / 8192.0 * 48.0 * 100.0;
        assert!((cents - 50.0).abs() < 0.25, "{cents} cents");
    }

    #[test]
    fn a_quarter_tone_on_the_ordinary_range_is_a_quarter_of_the_wheel() {
        assert_eq!(MpeOut::bend_word(0.5, 2.0), 10240);
    }

    #[test]
    fn no_bend_is_exactly_the_centre_whatever_the_range() {
        for range in [1.0, 2.0, 12.0, 48.0, 96.0] {
            assert_eq!(MpeOut::bend_word(0.0, range), 8192, "at ±{range}");
        }
    }

    #[test]
    fn the_word_survives_the_trip_through_nih_plugs_normalised_value() {
        // The wrapper's own arithmetic, from `NoteEvent::as_midi`.
        for word in [0u16, 1, 8191, 8192, 8277, 10240, 16382, 16383] {
            let back = (MpeOut::bend_value(word) * 16383.0).round() as u16;
            assert_eq!(back, word);
        }
    }

    #[test]
    fn a_bend_further_than_the_wheel_reaches_is_clamped_rather_than_wrapped() {
        assert_eq!(MpeOut::bend_word(96.0, 2.0), 16383);
        assert_eq!(MpeOut::bend_word(-96.0, 2.0), 0);
    }

    #[test]
    fn a_natural_note_is_not_bent_at_all() {
        let out = MpeOut::new();
        assert_eq!((out.bend(0.0) * 16383.0).round() as u16, 8192);
    }

    #[test]
    fn the_bend_precedes_the_note_on() {
        let mut out = MpeOut::new();
        let events = collect(|emit| {
            out.note_on(121, 100, emit);
        });
        assert!(matches!(events[0], Out::Bend { .. }));
        assert!(matches!(events[1], Out::NoteOn { .. }));
    }

    #[test]
    fn each_sounding_note_gets_a_channel_of_its_own() {
        let mut out = MpeOut::new();
        let mut channels = Vec::new();
        for lane in 120..130 {
            let voice = collect(|emit| {
                channels.push(out.note_on(lane, 100, emit).channel);
            });
            assert!(!voice.is_empty());
        }
        let mut sorted = channels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            channels.len(),
            "channels reused: {channels:?}"
        );
        assert!(channels.iter().all(|&c| (1..=15).contains(&c)));
    }

    #[test]
    fn a_released_channel_waits_its_turn_before_being_used_again() {
        let mut out = MpeOut::new();
        // Take three, give the first one back, take another. The freed channel has been idle
        // longest of the free ones only once the untouched ones have been used, so the new note
        // must land somewhere untouched.
        let mut voices = Vec::new();
        collect(|emit| {
            for lane in 120..123 {
                voices.push(out.note_on(lane, 100, emit));
            }
            out.note_off(voices[0], emit);
        });
        let next = collect(|emit| {
            out.note_on(130, 100, emit);
        });
        let Out::NoteOn { channel, .. } = next[1] else {
            panic!("{next:?}");
        };
        assert_ne!(channel, voices[0].channel);
    }

    #[test]
    fn the_upper_zone_counts_down_from_fifteen() {
        let mut out = MpeOut::new();
        out.configure(Zone::Upper, OutputMode::Mpe, DEFAULT_MEMBER_BEND_RANGE);
        assert_eq!(out.master(), 15);
        let events = collect(|emit| {
            out.note_on(120, 100, emit);
        });
        let Out::NoteOn { channel, .. } = events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(channel, 14);
    }

    #[test]
    fn clap_mode_sends_no_bend_and_stays_on_one_channel() {
        let mut out = MpeOut::new();
        out.configure(Zone::Lower, OutputMode::Clap, DEFAULT_MEMBER_BEND_RANGE);
        let events = collect(|emit| {
            out.note_on(121, 100, emit);
            out.note_on(123, 100, emit);
        });
        assert!(!events.iter().any(|e| matches!(e, Out::Bend { .. })));
        let channels: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Out::NoteOn { channel, .. } => Some(*channel),
                _ => None,
            })
            .collect();
        assert_eq!(channels, vec![0, 0]);
        let tunings: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Out::Tuning { semitones, .. } => Some(*semitones),
                _ => None,
            })
            .collect();
        assert_eq!(tunings, vec![0.5, 0.5]);
    }

    #[test]
    fn mpe_mode_sends_no_tuning_expression() {
        let mut out = MpeOut::new();
        let events = collect(|emit| {
            out.note_on(121, 100, emit);
        });
        assert!(!events.iter().any(|e| matches!(e, Out::Tuning { .. })));
    }

    #[test]
    fn both_mode_says_it_twice() {
        let mut out = MpeOut::new();
        out.configure(Zone::Lower, OutputMode::Both, DEFAULT_MEMBER_BEND_RANGE);
        let events = collect(|emit| {
            out.note_on(121, 100, emit);
        });
        assert!(events.iter().any(|e| matches!(e, Out::Bend { .. })));
        assert!(events.iter().any(|e| matches!(e, Out::Tuning { .. })));
    }

    #[test]
    fn the_zone_is_announced_before_anything_can_be_misread() {
        let out = MpeOut::new();
        let events = collect(|emit| out.announce(emit));
        // The configuration message: RPN 6 on the master, data 15.
        assert_eq!(
            events[..4],
            [
                Out::Cc {
                    channel: 0,
                    cc: 101,
                    value: 0
                },
                Out::Cc {
                    channel: 0,
                    cc: 100,
                    value: 6
                },
                Out::Cc {
                    channel: 0,
                    cc: 6,
                    value: 15
                },
                Out::Cc {
                    channel: 0,
                    cc: 38,
                    value: 0
                },
            ]
        );
        // And every member channel is told its bend range.
        for channel in 1..=15u8 {
            assert!(
                events.contains(&Out::Cc {
                    channel,
                    cc: 6,
                    value: 48
                }),
                "channel {channel} was not told its range"
            );
        }
    }

    #[test]
    fn clap_mode_announces_nothing() {
        let mut out = MpeOut::new();
        out.configure(Zone::Lower, OutputMode::Clap, DEFAULT_MEMBER_BEND_RANGE);
        assert!(collect(|emit| out.announce(emit)).is_empty());
    }

    #[test]
    fn a_sixteenth_simultaneous_note_steals_the_oldest_rather_than_being_dropped() {
        let mut out = MpeOut::new();
        let mut first = None;
        collect(|emit| {
            for (index, lane) in (100..115).enumerate() {
                let voice = out.note_on(lane, 100, emit);
                if index == 0 {
                    first = Some(voice);
                }
            }
        });
        let events = collect(|emit| {
            out.note_on(130, 100, emit);
        });
        let first = first.unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, Out::NoteOff { channel, .. } if *channel == first.channel)));
    }

    #[test]
    fn transposing_by_one_moves_a_note_by_a_quarter_tone() {
        let mut out = MpeOut::new();
        out.set_transpose(1);
        let events = collect(|emit| {
            out.note_on(120, 100, emit);
        });
        let Out::NoteOn { note, .. } = events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(note, 60);
        let Out::Bend { value, .. } = events[0] else {
            panic!("{events:?}");
        };
        assert_eq!((value * 16383.0).round() as u16, 8277);
    }

    #[test]
    fn transposing_off_the_end_of_the_keyboard_clamps_rather_than_wrapping() {
        let mut out = MpeOut::new();
        out.set_transpose(-200);
        let events = collect(|emit| {
            out.note_on(10, 100, emit);
        });
        let Out::NoteOn { note, .. } = events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(note, 0);
    }

    #[test]
    fn every_note_gets_an_identity_of_its_own() {
        let mut out = MpeOut::new();
        let mut ids = Vec::new();
        collect(|emit| {
            for lane in 100..110 {
                ids.push(out.note_on(lane, 100, emit).voice_id);
            }
        });
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10);
        assert!(ids.iter().all(|&id| id != 0));
    }
}
