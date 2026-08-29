//! Standard MIDI files, as far as buer is concerned with them: the musical half.
//!
//! Nothing here parses or writes a file. It turns a stream of channel messages into notes on the
//! lane grid, and a pattern back into a stream of channel messages — the plugin crate wraps
//! `midly` around it. The split is deliberate: what an MPE bend *means* is a question this crate
//! already answers for the audio thread, and answering it twice is how an exported file and a
//! played one come to disagree.
//!
//! Export runs the notes through the very same [`MpeOut`] the audio thread uses, so the channel a
//! note lands on and the bend word it carries are the same in a file as they are live.

use crate::mpeout::{MpeOut, Out, OutputMode, Voice, Zone};
use crate::pattern::{Note, Pattern, TICKS_PER_BEAT};
use crate::pitch;

/// One channel message, without the timing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Message {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    /// A 14-bit pitch bend, 8192 being centre.
    Bend { channel: u8, word: u16 },
    Cc { channel: u8, cc: u8, value: u8 },
}

/// A message and the tick it lands on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Timed {
    pub tick: u32,
    pub message: Message,
}

/// What an import had to do that the file did not ask for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub notes: usize,
    /// The furthest any note had to move to reach a lane, in cents.
    pub worst_cents: f32,
    /// How many notes were bent after they had already started. buer holds a note on one lane, so
    /// the bend at the note's start is the one that was kept.
    pub bent_while_sounding: usize,
    /// Whether the ticks divided exactly, and how many did not.
    pub inexact_ticks: usize,
}

const CC_DATA_ENTRY_MSB: u8 = 6;
const CC_DATA_ENTRY_LSB: u8 = 38;
const CC_RPN_LSB: u8 = 100;
const CC_RPN_MSB: u8 = 101;
const RPN_BEND_RANGE: u16 = 0;
const RPN_MPE_CONFIGURATION: u16 = 6;
const CHANNELS: usize = 16;

/// Convert a file's own resolution to buer's, exactly where it divides and reporting where it does
/// not.
pub fn rescale(tick: u32, ppq: u32) -> (u32, bool) {
    let ppq = ppq.max(1) as u64;
    let scaled = tick as u64 * TICKS_PER_BEAT as u64;
    let exact = scaled % ppq == 0;
    (((scaled + ppq / 2) / ppq).min(u32::MAX as u64) as u32, exact)
}

/// Read one track's messages as notes.
///
/// `events` must be in tick order, which is what a file gives.
pub fn import(events: &[Timed], ppq: u32) -> (Vec<Note>, Report) {
    let mut report = Report::default();
    let mut state = BendState::default();
    // The note-on waiting to be closed, per channel and key.
    let mut open: Vec<(u8, u8, u32, u8, f32)> = Vec::new();
    let mut notes = Vec::new();

    for event in events {
        let (tick, exact) = rescale(event.tick, ppq);
        if !exact {
            report.inexact_ticks += 1;
        }

        match event.message {
            Message::Cc { channel, cc, value } => state.cc(channel, cc, value),
            Message::Bend { channel, word } => {
                state.bend[channel as usize] = word;
                // A bend on a channel that is already sounding cannot be expressed: the grid holds
                // a note on one lane for its whole length.
                if open.iter().any(|(open_channel, ..)| *open_channel == channel) {
                    report.bent_while_sounding += 1;
                }
            }
            Message::NoteOn {
                channel,
                note,
                velocity,
            } if velocity > 0 => {
                let pitch = note as f32 + state.semitones(channel);
                open.push((channel, note, tick, velocity, pitch));
            }
            // A note-on at velocity zero is a note-off, and plenty of files write them that way.
            Message::NoteOn { channel, note, .. } | Message::NoteOff { channel, note } => {
                let Some(index) = open
                    .iter()
                    .rposition(|(open_channel, open_note, ..)| {
                        *open_channel == channel && *open_note == note
                    })
                else {
                    continue;
                };
                let (_, _, start, velocity, pitch) = open.remove(index);
                let lane = pitch::from_pitch(pitch);
                report.worst_cents = report.worst_cents.max(pitch::cents_from_lane(pitch).abs());
                notes.push(Note::new(
                    start,
                    tick.saturating_sub(start).max(1),
                    lane,
                    velocity,
                ));
            }
        }
    }

    // Anything still sounding at the end of the track is given the length it has so far rather than
    // being thrown away.
    let end = events
        .last()
        .map(|event| rescale(event.tick, ppq).0)
        .unwrap_or(0);
    for (_, _, start, velocity, pitch) in open {
        notes.push(Note::new(
            start,
            end.saturating_sub(start).max(1),
            pitch::from_pitch(pitch),
            velocity,
        ));
    }

    notes.sort_by_key(|note| (note.start, note.lane));
    report.notes = notes.len();
    (notes, report)
}

/// Per-channel bend, and what a bend on that channel means.
struct BendState {
    bend: [u16; CHANNELS],
    range: [f32; CHANNELS],
    rpn: [(u8, u8); CHANNELS],
    /// Whether the file has declared an MPE zone. Until it does, every channel is plain MIDI and
    /// bends by ±2, which is what a file that says nothing means.
    zoned: bool,
}

impl Default for BendState {
    fn default() -> Self {
        Self {
            bend: [8192; CHANNELS],
            range: [crate::mpeout::MASTER_BEND_RANGE; CHANNELS],
            rpn: [(0x7F, 0x7F); CHANNELS],
            zoned: false,
        }
    }
}

impl BendState {
    fn cc(&mut self, channel: u8, cc: u8, value: u8) {
        let index = channel as usize % CHANNELS;
        match cc {
            CC_RPN_MSB => self.rpn[index].0 = value,
            CC_RPN_LSB => self.rpn[index].1 = value,
            CC_DATA_ENTRY_MSB => {
                let rpn = ((self.rpn[index].0 as u16) << 7) | self.rpn[index].1 as u16;
                match rpn {
                    RPN_BEND_RANGE => {
                        self.range[index] = value as f32;
                        // MPE's rule: a range declared on one member channel is the range for every
                        // member of the zone. Applying it only to the channel it arrived on is the
                        // usual way to read an MPE file an octave out.
                        if self.zoned && index != 0 && index != 15 {
                            for member in 1..15 {
                                self.range[member] = value as f32;
                            }
                        }
                    }
                    RPN_MPE_CONFIGURATION => {
                        if value > 0 {
                            self.zoned = true;
                            for member in 0..CHANNELS {
                                if member != index {
                                    self.range[member] = crate::mpeout::DEFAULT_MEMBER_BEND_RANGE;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            CC_DATA_ENTRY_LSB => {
                let rpn = ((self.rpn[index].0 as u16) << 7) | self.rpn[index].1 as u16;
                if rpn == RPN_BEND_RANGE {
                    self.range[index] += value as f32 / 100.0;
                }
            }
            _ => {}
        }
    }

    fn semitones(&self, channel: u8) -> f32 {
        let index = channel as usize % CHANNELS;
        let word = self.bend[index] as i32 - 8192;
        word as f32 / 8192.0 * self.range[index]
    }
}

/// Turn a pattern into the messages that play it, MPE bends included.
///
/// The allocator is the audio thread's, so the channel a note lands on and the word its bend
/// carries are the ones the plugin would have sent.
pub fn export(pattern: &Pattern, zone: Zone, range: f32, repeats: u32) -> Vec<Timed> {
    let mut mpe = MpeOut::new();
    mpe.configure(zone, OutputMode::Mpe, range);

    let mut out = Vec::new();
    let mut at = 0u32;
    let push = |tick: u32, event: Out, out: &mut Vec<Timed>| {
        let message = match event {
            Out::Bend { channel, value } => Message::Bend {
                channel,
                word: (value * 16383.0).round().clamp(0.0, 16383.0) as u16,
            },
            Out::NoteOn {
                channel,
                note,
                velocity,
                ..
            } => Message::NoteOn {
                channel,
                note,
                velocity: (velocity * 127.0).round().clamp(1.0, 127.0) as u8,
            },
            Out::NoteOff { channel, note, .. } => Message::NoteOff { channel, note },
            Out::Cc { channel, cc, value } => Message::Cc { channel, cc, value },
            // A tuning expression has no place in a MIDI file; `Mpe` mode never emits one.
            Out::Tuning { .. } => return,
        };
        out.push(Timed { tick, message });
    };

    mpe.announce(&mut |event| push(0, event, &mut out));

    for _ in 0..repeats.max(1) {
        // Offs before ons at the same tick, and everything in tick order, exactly as the player
        // does it — a file whose events arrive out of order is a file that plays wrong.
        let mut sounding: Vec<(u32, Voice)> = Vec::new();
        let mut cursor = 0usize;
        let notes = pattern.notes();
        loop {
            let next_off = sounding.iter().map(|(end, _)| *end).min();
            let next_on = notes.get(cursor).map(|note| note.start);
            let take_off = match (next_off, next_on) {
                (Some(off), Some(on)) => off <= on,
                (Some(_), None) => true,
                _ => false,
            };
            if take_off {
                let end = next_off.expect("checked above");
                let index = sounding
                    .iter()
                    .position(|(tick, _)| *tick == end)
                    .expect("checked above");
                let (_, voice) = sounding.remove(index);
                mpe.note_off(voice, &mut |event| push(at + end, event, &mut out));
            } else if let Some(start) = next_on {
                let note = notes[cursor];
                cursor += 1;
                let voice = mpe.note_on(note.lane, note.velocity, &mut |event| {
                    push(at + start, event, &mut out)
                });
                sounding.push((note.end(), voice));
            } else {
                break;
            }
        }
        at += pattern.length;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::Pattern;

    fn on(tick: u32, channel: u8, note: u8, velocity: u8) -> Timed {
        Timed {
            tick,
            message: Message::NoteOn {
                channel,
                note,
                velocity,
            },
        }
    }

    fn off(tick: u32, channel: u8, note: u8) -> Timed {
        Timed {
            tick,
            message: Message::NoteOff { channel, note },
        }
    }

    fn bend(tick: u32, channel: u8, word: u16) -> Timed {
        Timed {
            tick,
            message: Message::Bend { channel, word },
        }
    }

    fn cc(tick: u32, channel: u8, cc: u8, value: u8) -> Timed {
        Timed {
            tick,
            message: Message::Cc { channel, cc, value },
        }
    }

    #[test]
    fn a_twelve_tone_file_lands_entirely_on_even_lanes() {
        let events = vec![
            on(0, 0, 60, 100),
            off(480, 0, 60),
            on(480, 0, 64, 90),
            off(960, 0, 64),
        ];
        let (notes, report) = import(&events, 480);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|note| note.lane % 2 == 0));
        assert_eq!(report.worst_cents, 0.0);
    }

    #[test]
    fn a_file_at_three_hundred_and_eighty_four_ppq_lands_on_exact_ticks() {
        let events = vec![on(96, 0, 60, 100), off(192, 0, 60)];
        let (notes, report) = import(&events, 384);
        assert_eq!(report.inexact_ticks, 0);
        // A sixteenth at 384 ppq is 96 ticks; at 1920 it is 480.
        assert_eq!(notes[0].start, 480);
        assert_eq!(notes[0].length, 480);
    }

    #[test]
    fn a_resolution_that_does_not_divide_says_so_rather_than_pretending() {
        let events = vec![on(1, 0, 60, 100), off(2, 0, 60)];
        let (_, report) = import(&events, 1000);
        assert!(report.inexact_ticks > 0);
    }

    #[test]
    fn a_note_bent_a_quarter_tone_sharp_comes_back_on_a_quarter_tone_lane() {
        // ±2 semitones is what a file that declares nothing means, so half a semitone is a quarter
        // of the wheel above centre.
        let events = vec![
            bend(0, 0, 8192 + 2048),
            on(0, 0, 60, 100),
            off(480, 0, 60),
        ];
        let (notes, report) = import(&events, 480);
        assert_eq!(notes[0].lane, 121);
        assert!(report.worst_cents < 1.0);
    }

    #[test]
    fn an_mpe_file_is_read_against_the_range_its_configuration_message_declares() {
        let events = vec![
            // The configuration message: rpn 6 on the master, fifteen members.
            cc(0, 0, 101, 0),
            cc(0, 0, 100, 6),
            cc(0, 0, 6, 15),
            // A quarter tone at ±48 is eighty-five units.
            bend(0, 1, 8192 + 85),
            on(0, 1, 60, 100),
            off(480, 1, 60),
        ];
        let (notes, report) = import(&events, 480);
        assert_eq!(notes[0].lane, 121, "read against the wrong bend range");
        assert!(report.worst_cents < 1.0, "{} cents", report.worst_cents);
    }

    #[test]
    fn a_range_declared_on_one_member_channel_is_the_range_for_all_of_them() {
        let events = vec![
            cc(0, 0, 101, 0),
            cc(0, 0, 100, 6),
            cc(0, 0, 6, 15),
            // rpn 0 on member channel 2, saying ±12.
            cc(0, 1, 101, 0),
            cc(0, 1, 100, 0),
            cc(0, 1, 6, 12),
            // The same bend on a different member must be read the same way.
            bend(0, 5, 8192 + 341),
            on(0, 5, 60, 100),
            off(480, 5, 60),
        ];
        let (notes, _) = import(&events, 480);
        assert_eq!(notes[0].lane, 121);
    }

    #[test]
    fn a_bend_after_a_note_has_started_is_reported_rather_than_lost_in_silence() {
        let events = vec![
            on(0, 0, 60, 100),
            bend(240, 0, 8192 + 2048),
            off(480, 0, 60),
        ];
        let (notes, report) = import(&events, 480);
        assert_eq!(notes[0].lane, 120, "the note moved after it had started");
        assert_eq!(report.bent_while_sounding, 1);
    }

    #[test]
    fn a_note_on_at_no_velocity_is_a_note_off() {
        let events = vec![on(0, 0, 60, 100), on(480, 0, 60, 0)];
        let (notes, _) = import(&events, 480);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].length, 1920);
    }

    #[test]
    fn a_note_never_closed_is_kept_rather_than_thrown_away() {
        let events = vec![on(0, 0, 60, 100), on(480, 0, 64, 100), off(960, 0, 64)];
        let (notes, _) = import(&events, 480);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn the_bend_precedes_the_note_it_belongs_to_in_the_written_track() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(0, 480, 121, 100));
        let events = export(&pattern, Zone::Lower, 48.0, 1);
        let notes: Vec<_> = events
            .iter()
            .filter(|event| {
                matches!(
                    event.message,
                    Message::Bend { .. } | Message::NoteOn { .. } | Message::NoteOff { .. }
                )
            })
            .collect();
        assert!(matches!(notes[0].message, Message::Bend { word, .. } if word == 8277));
        assert!(matches!(notes[1].message, Message::NoteOn { .. }));
    }

    #[test]
    fn an_exported_track_is_in_tick_order() {
        let mut pattern = Pattern::empty("p", 1, 4);
        for step in 0..16 {
            pattern.insert(Note::new(step * 120, 200, 120 + (step % 5) as u8, 100));
        }
        let events = export(&pattern, Zone::Lower, 48.0, 2);
        assert!(events.windows(2).all(|pair| pair[0].tick <= pair[1].tick));
    }

    #[test]
    fn a_pattern_exported_and_read_back_is_the_same_pattern() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(0, 480, 120, 100));
        pattern.insert(Note::new(480, 240, 121, 64));
        pattern.insert(Note::new(960, 960, 133, 127));
        pattern.insert(Note::new(960, 480, 140, 90));

        let events = export(&pattern, Zone::Lower, 48.0, 1);
        let (notes, report) = import(&events, TICKS_PER_BEAT);

        assert_eq!(report.bent_while_sounding, 0);
        assert_eq!(report.inexact_ticks, 0);
        assert_eq!(notes.len(), pattern.notes().len());
        for (before, after) in pattern.notes().iter().zip(notes.iter()) {
            assert_eq!(before.start, after.start, "start");
            assert_eq!(before.length, after.length, "length");
            assert_eq!(before.lane, after.lane, "lane");
            assert_eq!(before.velocity, after.velocity, "velocity");
        }
    }

    #[test]
    fn repeats_lay_the_pattern_end_to_end() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(0, 480, 120, 100));
        let events = export(&pattern, Zone::Lower, 48.0, 3);
        let ons: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.message, Message::NoteOn { .. }))
            .map(|event| event.tick)
            .collect();
        assert_eq!(ons, vec![0, 7680, 15360]);
    }
}
