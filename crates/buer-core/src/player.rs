//! Playing a pattern: turning a position on the timeline into a stream of events.
//!
//! The position is *read* from the host rather than integrated from a per-sample increment. A
//! counter of our own drifts against the host over a long take, and — much worse — knows nothing
//! about the host seeking, looping or being scrubbed. Reading `pos_beats` every block costs nothing
//! and is right by construction; a jump larger than a block is simply a seek, and is handled as one.
//!
//! Events come out in tick order, offs before ons at the same tick. Both matter: CLAP wants an
//! output queue sorted by time, and a note that is re-struck on the lane it is already sounding on
//! would otherwise cut itself off a sample after it started.

use crate::mpeout::{MpeOut, Out, Voice, MEMBERS};
use crate::pattern::{Bank, Pattern};

/// How many notes can sound at once. One per member channel: a sixteenth would have to share, and
/// two notes on one channel cannot be bent apart.
pub const MAX_VOICES: usize = MEMBERS;

/// A position further than this from where the last block ended is the host having moved, not time
/// having passed. A block is at most a few thousand samples, so a beat is far outside anything a
/// block could cover and well inside anything worth calling a seek.
const SEEK_TICKS: f64 = crate::pattern::TICKS_PER_BEAT as f64;

/// Where the timeline is at the start of a block.
#[derive(Copy, Clone, Debug)]
pub struct Clock {
    pub playing: bool,
    /// Absolute position in ticks at the first sample of the block. Where it comes from — the host's
    /// transport or buer's own free-running counter — is the plugin's business, not the player's.
    pub ticks: f64,
    pub ticks_per_sample: f64,
}

#[derive(Copy, Clone, Debug)]
struct Held {
    voice: Voice,
    /// The tick within the pattern that this note stops on.
    end: f64,
    /// When it started, so the oldest can be stolen when they run out.
    age: u64,
}

pub struct Player {
    slot: usize,
    /// A slot chosen while playing, waiting for the loop point. Switching there rather than at once
    /// is what keeps a change of pattern from cutting a note in half.
    pending: Option<usize>,
    /// Index of the next note-on in the pattern's sorted list.
    cursor: usize,
    /// Position within the pattern, in ticks.
    position: f64,
    /// Where the last block ended, absolutely, so a seek can be told from time passing.
    expected: f64,
    running: bool,
    sounding: [Option<Held>; MAX_VOICES],
    age: u64,
    /// What share of its written length a note actually sounds for.
    gate: f32,
    mpe: MpeOut,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            slot: 0,
            pending: None,
            cursor: 0,
            position: 0.0,
            expected: f64::NAN,
            running: false,
            sounding: [None; MAX_VOICES],
            age: 0,
            gate: 1.0,
            mpe: MpeOut::new(),
        }
    }

    pub fn mpe(&mut self) -> &mut MpeOut {
        &mut self.mpe
    }

    /// What share of its written length a note sounds for. Applied when the note starts, so
    /// changing it does not shorten something already sounding.
    pub fn set_gate(&mut self, gate: f32) {
        self.gate = gate.clamp(0.01, 4.0);
    }

    /// Which pattern is sounding. Not the one asked for — that only takes effect at the loop point.
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Position within the pattern, in ticks. What the editor draws its playhead at.
    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Ask for a pattern. It takes effect at the next loop point while playing, and at once while
    /// stopped, where there is nothing to cut in half.
    pub fn select(&mut self, slot: usize) {
        if self.running {
            self.pending = (slot != self.slot).then_some(slot);
        } else {
            self.slot = slot;
            self.pending = None;
            self.cursor = 0;
        }
    }

    /// Stop everything that is sounding and forget where we were.
    pub fn silence(&mut self, emit: &mut dyn FnMut(u32, Out)) {
        for held in self.sounding.iter_mut() {
            if let Some(held) = held.take() {
                self.mpe.note_off(held.voice, &mut |event| emit(0, event));
            }
        }
        self.mpe.reset();
        self.running = false;
        self.expected = f64::NAN;
    }

    /// Advance by one block, emitting `(sample offset, event)` in order.
    pub fn process(
        &mut self,
        bank: &Bank,
        clock: Clock,
        samples: u32,
        looping: bool,
        emit: &mut dyn FnMut(u32, Out),
    ) {
        if !clock.playing {
            if self.running {
                self.silence(emit);
                self.position = 0.0;
                self.cursor = 0;
                if let Some(slot) = self.pending.take() {
                    self.slot = slot;
                }
            }
            return;
        }

        let pattern = bank.pattern(self.slot);
        if pattern.length == 0 || clock.ticks_per_sample <= 0.0 {
            return;
        }

        // A start, or the host having moved somewhere else, both mean the same thing: nothing that
        // is sounding belongs where we now are.
        let jumped = !self.running || (clock.ticks - self.expected).abs() > SEEK_TICKS;
        if jumped {
            self.silence(emit);
            self.running = true;
            self.mpe.announce(&mut |event| emit(0, event));
            self.seek(pattern, clock.ticks, looping);
        }

        let span = samples as f64 * clock.ticks_per_sample;
        self.expected = clock.ticks + span;

        // Playing once means playing once: past the end of the pattern there is nothing left to do
        // but hold the position at the end so the playhead does not lie about it.
        if !looping && clock.ticks >= pattern.length as f64 {
            self.stop_sounding(emit, 0);
            self.position = pattern.length as f64;
            return;
        }

        let mut sample = 0.0f64;
        let mut remaining = span;
        while remaining > 0.0 {
            let length = pattern.length as f64;
            let to_boundary = length - self.position;
            let step = remaining.min(to_boundary);
            let end = self.position + step;

            self.segment(pattern, end, sample, clock.ticks_per_sample, samples, emit);

            sample += step / clock.ticks_per_sample;
            remaining -= step;
            self.position = end;

            if self.position >= length - f64::EPSILON {
                // The loop point. Everything sounding ends here — a note is clipped to the pattern,
                // so there is never one that wanted to carry on — and a waiting pattern takes over.
                let offset = (sample.round() as u32).min(samples.saturating_sub(1));
                self.stop_sounding(emit, offset);
                self.position = 0.0;
                self.cursor = 0;
                if let Some(slot) = self.pending.take() {
                    self.slot = slot;
                    // The new pattern may be a different length, so nothing below may keep using
                    // the old one. Picking it up on the next block costs at most one buffer and
                    // saves re-deriving every local here.
                    return;
                }
                if !looping {
                    return;
                }
            }
        }
    }

    /// Put the cursor and the position where an absolute tick says they should be.
    fn seek(&mut self, pattern: &Pattern, ticks: f64, looping: bool) {
        let length = pattern.length as f64;
        self.position = if looping {
            ticks.rem_euclid(length)
        } else {
            ticks.min(length)
        };
        self.cursor = pattern.cursor_at(self.position as u32);
    }

    /// Emit everything that falls before `to`, in order.
    fn segment(
        &mut self,
        pattern: &Pattern,
        to: f64,
        sample_base: f64,
        ticks_per_sample: f64,
        samples: u32,
        emit: &mut dyn FnMut(u32, Out),
    ) {
        let from = self.position;
        loop {
            let next_off = self
                .sounding
                .iter()
                .enumerate()
                .filter_map(|(index, held)| held.map(|held| (held.end, index)))
                .filter(|(end, _)| *end < to)
                .min_by(|a, b| a.0.total_cmp(&b.0));
            let next_on = pattern
                .notes()
                .get(self.cursor)
                .filter(|note| (note.start as f64) < to)
                .map(|note| note.start as f64);

            // An off at the same tick as an on goes first, or a lane being re-struck cuts itself.
            let take_off = match (next_off, next_on) {
                (Some((end, _)), Some(start)) => end <= start,
                (Some(_), None) => true,
                _ => false,
            };

            if take_off {
                let (end, index) = next_off.expect("checked above");
                let offset = Self::offset(end, from, sample_base, ticks_per_sample, samples);
                let held = self.sounding[index].take().expect("checked above");
                self.mpe
                    .note_off(held.voice, &mut |event| emit(offset, event));
            } else if let Some(start) = next_on {
                let note = pattern.notes()[self.cursor];
                self.cursor += 1;
                let offset = Self::offset(start, from, sample_base, ticks_per_sample, samples);
                // Gated, but never past the loop point: a note allowed to ring across the wrap
                // would meet its own retrigger on the same lane, and one of the two would be lost.
                let gated = (note.length as f64 * self.gate as f64).max(1.0);
                let end = (note.start as f64 + gated).min(pattern.length as f64);
                self.start(note.lane, note.velocity, end, offset, emit);
            } else {
                return;
            }
        }
    }

    fn start(
        &mut self,
        lane: u8,
        velocity: u8,
        end: f64,
        offset: u32,
        emit: &mut dyn FnMut(u32, Out),
    ) {
        let free = self.sounding.iter().position(Option::is_none).or_else(|| {
            // Out of voices. The oldest note is the one least likely to still be wanted, and
            // dropping the new one instead would make a dense pattern quietly lose its top line.
            self.sounding
                .iter()
                .enumerate()
                .filter_map(|(index, held)| held.map(|held| (held.age, index)))
                .min_by_key(|(age, _)| *age)
                .map(|(_, index)| index)
        });
        let Some(slot) = free else { return };

        if let Some(stolen) = self.sounding[slot].take() {
            self.mpe
                .note_off(stolen.voice, &mut |event| emit(offset, event));
        }

        self.age += 1;
        let voice = self
            .mpe
            .note_on(lane, velocity, &mut |event| emit(offset, event));
        self.sounding[slot] = Some(Held {
            voice,
            end,
            age: self.age,
        });
    }

    fn stop_sounding(&mut self, emit: &mut dyn FnMut(u32, Out), offset: u32) {
        for held in self.sounding.iter_mut() {
            if let Some(held) = held.take() {
                self.mpe
                    .note_off(held.voice, &mut |event| emit(offset, event));
            }
        }
    }

    fn offset(tick: f64, from: f64, sample_base: f64, ticks_per_sample: f64, samples: u32) -> u32 {
        let sample = sample_base + (tick - from).max(0.0) / ticks_per_sample;
        (sample.round().max(0.0) as u32).min(samples.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{Note, Pattern, TICKS_PER_BEAT};

    /// A block of `samples` at 120 bpm and 48 kHz, starting at `ticks`.
    fn clock(ticks: f64) -> Clock {
        Clock {
            playing: true,
            ticks,
            // 120 bpm, 48 kHz: two beats a second, so 3840 ticks a second.
            ticks_per_sample: 2.0 * crate::pattern::TICKS_PER_BEAT as f64 / 48_000.0,
        }
    }

    fn one_bar(notes: &[Note]) -> Bank {
        let mut bank = Bank::default();
        let mut pattern = Pattern::empty("1", 1, 4);
        for note in notes {
            pattern.insert(*note);
        }
        *bank.pattern_mut(0) = pattern;
        bank
    }

    /// Run `blocks` blocks of 512 samples and collect everything emitted, as (absolute sample,
    /// event).
    fn run(player: &mut Player, bank: &Bank, blocks: usize, looping: bool) -> Vec<(u64, Out)> {
        let mut events = Vec::new();
        let samples = 512u32;
        let tps = clock(0.0).ticks_per_sample;
        for block in 0..blocks {
            let base = block as u64 * samples as u64;
            let ticks = block as f64 * samples as f64 * tps;
            player.process(
                bank,
                clock(ticks),
                samples,
                looping,
                &mut |offset, event| events.push((base + offset as u64, event)),
            );
        }
        events
    }

    /// Every note-on, as (sample, note number, velocity).
    fn note_ons(events: &[(u64, Out)]) -> Vec<(u64, u8, u8)> {
        events
            .iter()
            .filter_map(|(at, event)| match event {
                Out::NoteOn { note, velocity, .. } => {
                    Some((*at, *note, (velocity * 127.0).round() as u8))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_note_on_the_downbeat_lands_on_the_first_sample() {
        let bank = one_bar(&[Note::new(0, 480, 120, 100)]);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 1, true);
        assert_eq!(note_ons(&events), vec![(0, 60, 100)]);
    }

    #[test]
    fn a_note_lands_on_the_sample_its_tick_falls_on() {
        // One beat in, at 1920 ticks a second and 48 kHz, is 24000 samples.
        let bank = one_bar(&[Note::new(TICKS_PER_BEAT, 480, 120, 100)]);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 60, true);
        let ons = note_ons(&events);
        assert_eq!(ons.len(), 1);
        assert!(
            (ons[0].0 as i64 - 24_000).abs() <= 1,
            "landed at {}",
            ons[0].0
        );
    }

    #[test]
    fn an_off_precedes_an_on_on_the_same_lane_at_the_same_tick() {
        let bank = one_bar(&[
            Note::new(0, TICKS_PER_BEAT, 120, 100),
            Note::new(TICKS_PER_BEAT, TICKS_PER_BEAT, 120, 100),
        ]);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 60, true);
        let order: Vec<_> = events
            .iter()
            .filter(|(_, e)| matches!(e, Out::NoteOn { .. } | Out::NoteOff { .. }))
            .map(|(at, e)| (*at, matches!(e, Out::NoteOff { .. })))
            .collect();
        assert_eq!(order[0], (0, false));
        assert!(order[1].1, "the off does not come first: {order:?}");
        assert!(!order[2].1);
        assert_eq!(order[1].0, order[2].0);
    }

    #[test]
    fn events_come_out_in_time_order() {
        let mut notes = Vec::new();
        for i in 0..16 {
            notes.push(Note::new(i * 240, 200, 120 + (i % 5) as u8, 100));
        }
        let bank = one_bar(&notes);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 60, true);
        assert!(events.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    #[test]
    fn the_loop_wraps_without_dropping_a_note_off() {
        let bank = one_bar(&[Note::new(0, 480, 120, 100)]);
        let mut player = Player::new();
        // A bar of 4/4 at 120 bpm is two seconds, which at 48 kHz and 512 samples a block is 187.5
        // blocks. Four seconds and a bit covers the downbeat three times.
        let mut events = run(&mut player, &bank, 380, true);
        let ons = |events: &[(u64, Out)]| {
            events
                .iter()
                .filter(|(_, e)| matches!(e, Out::NoteOn { .. }))
                .count()
        };
        let offs = |events: &[(u64, Out)]| {
            events
                .iter()
                .filter(|(_, e)| matches!(e, Out::NoteOff { .. }))
                .count()
        };
        assert_eq!(ons(&events), 3, "the pattern did not loop");
        assert_eq!(offs(&events), 2, "a note ended that had not started");

        // And the one still sounding is accounted for rather than left hanging.
        player.silence(&mut |_, event| events.push((0, event)));
        assert_eq!(offs(&events), ons(&events));
    }

    #[test]
    fn playing_once_plays_once() {
        let bank = one_bar(&[Note::new(0, 480, 120, 100)]);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 380, false);
        assert_eq!(
            events
                .iter()
                .filter(|(_, e)| matches!(e, Out::NoteOn { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn stopping_the_transport_stops_every_note() {
        let bank = one_bar(&[Note::new(0, TICKS_PER_BEAT * 4, 120, 100)]);
        let mut player = Player::new();
        let mut events = Vec::new();
        player.process(&bank, clock(0.0), 512, true, &mut |_, e| events.push(e));
        assert!(events.iter().any(|e| matches!(e, Out::NoteOn { .. })));
        events.clear();
        player.process(
            &bank,
            Clock {
                playing: false,
                ..clock(100.0)
            },
            512,
            true,
            &mut |_, e| events.push(e),
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Out::NoteOff { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn the_host_jumping_somewhere_else_does_not_leave_a_note_hanging() {
        let bank = one_bar(&[Note::new(0, TICKS_PER_BEAT * 4, 120, 100)]);
        let mut player = Player::new();
        let mut events = Vec::new();
        player.process(&bank, clock(0.0), 512, true, &mut |_, e| events.push(e));
        events.clear();
        // Straight to the middle of the next bar.
        player.process(&bank, clock(5760.0), 512, true, &mut |_, e| events.push(e));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Out::NoteOff { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn seeking_into_the_middle_of_a_pattern_starts_from_there() {
        let bank = one_bar(&[
            Note::new(0, 240, 120, 100),
            Note::new(TICKS_PER_BEAT * 3, 240, 130, 100),
        ]);
        let mut player = Player::new();
        let mut events = Vec::new();
        // Start playing from just before the second note.
        let ticks = TICKS_PER_BEAT as f64 * 3.0 - 10.0;
        player.process(&bank, clock(ticks), 512, true, &mut |_, e| events.push(e));
        let notes: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Out::NoteOn { note, .. } => Some(*note),
                _ => None,
            })
            .collect();
        assert_eq!(notes, vec![65]);
    }

    #[test]
    fn switching_pattern_waits_for_the_loop_point() {
        let mut bank = one_bar(&[Note::new(0, 240, 120, 100)]);
        let mut second = Pattern::empty("2", 1, 4);
        second.insert(Note::new(0, 240, 130, 100));
        *bank.pattern_mut(1) = second;

        let mut player = Player::new();
        let mut events = Vec::new();
        player.process(&bank, clock(0.0), 512, true, &mut |_, e| events.push(e));
        player.select(1);
        assert_eq!(player.slot(), 0, "the pattern changed under the notes");

        // Play on to just past the loop point.
        let tps = clock(0.0).ticks_per_sample;
        let mut notes = Vec::new();
        for block in 1..400 {
            let ticks = block as f64 * 512.0 * tps;
            player.process(&bank, clock(ticks), 512, true, &mut |_, e| {
                if let Out::NoteOn { note, .. } = e {
                    notes.push(note)
                }
            });
        }
        assert_eq!(player.slot(), 1);
        assert!(notes.contains(&65), "the second pattern never played");
    }

    #[test]
    fn choosing_a_pattern_while_stopped_takes_effect_at_once() {
        let mut player = Player::new();
        player.select(3);
        assert_eq!(player.slot(), 3);
    }

    #[test]
    fn more_notes_at_once_than_there_are_channels_steals_rather_than_dropping() {
        let mut notes = Vec::new();
        for lane in 100..120u8 {
            notes.push(Note::new(0, TICKS_PER_BEAT, lane, 100));
        }
        let bank = one_bar(&notes);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 1, true);
        let ons = events
            .iter()
            .filter(|(_, e)| matches!(e, Out::NoteOn { .. }))
            .count();
        assert_eq!(ons, 20, "a note was silently dropped");
        // And the last note in is one of the ones still sounding.
        let last: Vec<_> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Out::NoteOn { note, .. } => Some(*note),
                _ => None,
            })
            .collect();
        assert_eq!(*last.last().unwrap(), 59);
    }

    #[test]
    fn the_gate_shortens_a_note_without_moving_it() {
        let bank = one_bar(&[Note::new(0, TICKS_PER_BEAT * 2, 120, 100)]);
        let mut player = Player::new();
        player.set_gate(0.5);
        let events = run(&mut player, &bank, 120, true);
        let off = events
            .iter()
            .find(|(_, e)| matches!(e, Out::NoteOff { .. }))
            .expect("the note never ended");
        // A beat at 120 bpm is half a second, which is 24000 samples at 48 kHz.
        assert!((off.0 as i64 - 24_000).abs() <= 2, "ended at {}", off.0);
    }

    #[test]
    fn a_gate_past_the_loop_point_still_stops_at_it() {
        let bank = one_bar(&[Note::new(0, TICKS_PER_BEAT * 4, 120, 100)]);
        let mut player = Player::new();
        player.set_gate(4.0);
        // Just past one bar, so the note has met the loop point and the next pass has begun.
        let events = run(&mut player, &bank, 200, true);
        let ons = events
            .iter()
            .filter(|(_, e)| matches!(e, Out::NoteOn { .. }))
            .count();
        let offs = events
            .iter()
            .filter(|(_, e)| matches!(e, Out::NoteOff { .. }))
            .count();
        assert_eq!(ons, 2);
        assert_eq!(offs, 1, "the note rang across the wrap");
    }

    #[test]
    fn a_quarter_tone_pattern_leaves_as_bends() {
        let bank = one_bar(&[Note::new(0, 240, 121, 100)]);
        let mut player = Player::new();
        let events = run(&mut player, &bank, 1, true);
        let bends: Vec<_> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Out::Bend { value, .. } => Some(*value),
                _ => None,
            })
            .collect();
        assert_eq!(bends.len(), 1);
        assert!(bends[0] > 0.5);
    }
}
