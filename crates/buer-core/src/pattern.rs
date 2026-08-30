//! Notes, patterns and the bank they live in.
//!
//! Two choices here shape everything above them.
//!
//! Time is counted in ticks at [`TICKS_PER_BEAT`], and a note holds its own start and length rather
//! than sitting in a step. A step grid would make note lengths multiples of a step and would force
//! an imported file to be quantised on the way in; ticks cost nothing and keep both exact.
//!
//! Pitch is a *lane*: one of 24 equal divisions of the octave, so a quarter tone is a row of its
//! own rather than a detune hidden in a value field. The whole `u8` range is exactly the 256 lanes
//! of MIDI 0..128, which is why [`Lane`] is a `u8` and not something wider.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Ticks in a quarter note.
///
/// 1920 = 2^7 · 3 · 5, and every part of that is doing something:
///
/// - triplets and quintuplets divide exactly — a 16th triplet is 320 ticks, a 16th quintuplet 96 —
///   so a swung or tupleted phrase is not carrying a rounding error;
/// - it divides every resolution a standard MIDI file arrives at, 384 included, which 960 does not:
///   a 384-ppq file scaled to 960 lands on half ticks, and import has to land exactly;
/// - it fits an SMF's metrical header, so export writes these ticks out unscaled and a file that
///   went out and came back is the same file.
///
/// A `u32` of them reaches about two million beats, which is nine days at 120 bpm. The *position*
/// counted in them must be `f64` and never `f32`: an hour at 120 bpm is 27 million ticks, past
/// where an `f32`'s 24-bit mantissa can still tell one tick from the next.
pub const TICKS_PER_BEAT: u32 = 1920;

/// Quarter tones in an octave.
pub const LANES_PER_OCTAVE: u8 = 24;

/// The whole lane range, which is also the whole `u8` range: MIDI note 0 to 127 plus its quarter
/// tone.
pub const LANE_COUNT: u16 = 256;

/// The shortest note that can be drawn, a 64th. Short enough to be a grace note, long enough that a
/// stray click cannot leave something invisible behind.
pub const MIN_LENGTH: u32 = TICKS_PER_BEAT / 16;

/// A note's identity, unique within its pattern for as long as the session lasts.
///
/// Selection, a drag in progress and the undo stack all have to name a note across an edit that
/// re-sorts the list — dragging one note past another does exactly that — and an index cannot do
/// it. Not written to the file: nothing outside a session refers to a note by name, so ids are
/// handed out afresh on load.
pub type NoteId = u32;

/// A 24-EDO lane. `lane / 2` is the MIDI note it sounds from, `lane % 2` whether it is the quarter
/// tone above it.
pub type Lane = u8;

/// One note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    /// Zero until [`Pattern::insert`] takes the note and gives it one.
    pub id: NoteId,
    /// Ticks from the start of the pattern.
    pub start: u32,
    /// Ticks. Never below [`MIN_LENGTH`].
    pub length: u32,
    pub lane: Lane,
    /// 1..=127. Zero is a note-off in MIDI, so it is not a velocity anything can be given.
    pub velocity: u8,
}

impl Note {
    pub fn new(start: u32, length: u32, lane: Lane, velocity: u8) -> Self {
        Self {
            id: 0,
            start,
            length: length.max(MIN_LENGTH),
            lane,
            velocity: velocity.clamp(1, 127),
        }
    }

    /// The tick the note stops sounding on.
    pub fn end(&self) -> u32 {
        self.start.saturating_add(self.length)
    }

    /// Where a note sorts. Start first so the player can walk the list with a cursor; lane after it
    /// so two notes on the same tick have a stable order rather than depending on insertion.
    fn key(&self) -> (u32, Lane) {
        (self.start, self.lane)
    }
}

/// A note is written as `[start, length, lane, velocity]` rather than as four named fields.
///
/// A busy pattern is a few hundred notes; spelling out the field names would quadruple the file for
/// nothing, and a row of four numbers is still a thing you can read and edit by hand — which is the
/// only reason the file is JSON at all. The id is not written; see [`NoteId`].
#[cfg(feature = "serde")]
impl Serialize for Note {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut row = serializer.serialize_tuple(4)?;
        row.serialize_element(&self.start)?;
        row.serialize_element(&self.length)?;
        row.serialize_element(&self.lane)?;
        row.serialize_element(&self.velocity)?;
        row.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for Note {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let [start, length, lane, velocity] = <[u32; 4]>::deserialize(deserializer)?;
        Ok(Note::new(
            start,
            length,
            lane.min(255) as Lane,
            velocity.min(127) as u8,
        ))
    }
}

/// One sequence: a length to loop within, and the notes inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Pattern {
    pub name: String,
    /// Ticks. The loop point, and what a note is clipped to.
    pub length: u32,
    /// Which of the twenty-four lanes a note may land on here. Per pattern, so a bank can hold a
    /// bayati beside a rast; the scale *file* is per instance, because there is only one of it.
    #[cfg_attr(feature = "serde", serde(default = "LaneMask::all"))]
    pub constraint: LaneMask,
    /// What was last stamped into [`Self::constraint`], so the editor can name it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub scale: String,
    /// Sorted by (start, lane). [`Pattern::insert`] is the only way in, and it keeps that so the
    /// player never has to search.
    notes: Vec<Note>,
    /// The next identity to hand out. Never persisted — see [`NoteId`].
    #[cfg_attr(feature = "serde", serde(skip))]
    next_id: NoteId,
}

impl Default for Pattern {
    fn default() -> Self {
        Self::empty(String::new(), 4, 4)
    }
}

impl Pattern {
    /// An empty pattern of `bars` bars of `beats` beats.
    pub fn empty(name: impl Into<String>, bars: u32, beats: u32) -> Self {
        Self {
            name: name.into(),
            length: bars.max(1) * beats.max(1) * TICKS_PER_BEAT,
            constraint: LaneMask::ALL,
            scale: String::new(),
            notes: Vec::new(),
            next_id: 1,
        }
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Add a note, keeping the list sorted, and give it an identity. A note past the end of the
    /// pattern is dropped and one that overruns it is clipped: the loop point is the pattern, not a
    /// suggestion.
    pub fn insert(&mut self, note: Note) -> Option<NoteId> {
        if note.start >= self.length {
            return None;
        }
        let mut note = note;
        note.length = note.length.min(self.length - note.start).max(1);
        note.id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let at = self.notes.partition_point(|n| n.key() <= note.key());
        self.notes.insert(at, note);
        Some(note.id)
    }

    pub fn find(&self, id: NoteId) -> Option<&Note> {
        self.notes.iter().find(|note| note.id == id)
    }

    /// Hand every note a fresh identity. Called after a pattern is read off disk, where ids were
    /// not written and every note therefore claims to be note zero.
    pub fn reindex(&mut self) {
        self.next_id = 1;
        for note in &mut self.notes {
            note.id = self.next_id;
            self.next_id += 1;
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<Note> {
        (index < self.notes.len()).then(|| self.notes.remove(index))
    }

    /// Take a note out by name.
    pub fn remove_id(&mut self, id: NoteId) -> Option<Note> {
        let at = self.notes.iter().position(|note| note.id == id)?;
        Some(self.notes.remove(at))
    }

    /// Change a note in place, then put it back where it now belongs.
    ///
    /// The sort is by start, and an edit is free to move one, so the list has to be repaired
    /// afterwards. Doing it here is what lets everything above hold a [`NoteId`] and stop caring
    /// where in the vector its note happens to be.
    pub fn update(&mut self, id: NoteId, edit: impl FnOnce(&mut Note)) -> bool {
        let Some(at) = self.notes.iter().position(|note| note.id == id) else {
            return false;
        };
        let mut note = self.notes.remove(at);
        edit(&mut note);
        self.clamp(&mut note);
        let to = self.notes.partition_point(|n| n.key() <= note.key());
        self.notes.insert(to, note);
        true
    }

    /// Hold a note inside the pattern and inside what a note is allowed to be.
    fn clamp(&self, note: &mut Note) {
        note.start = note.start.min(self.length.saturating_sub(1));
        note.length = note
            .length
            .max(MIN_LENGTH)
            .min(self.length - note.start)
            .max(1);
        note.velocity = note.velocity.clamp(1, 127);
    }

    pub fn clear(&mut self) {
        self.notes.clear();
    }

    /// Replace every note. Used by the generators and by import, both of which produce a whole
    /// pattern rather than editing one.
    pub fn replace(&mut self, notes: impl IntoIterator<Item = Note>) {
        self.notes.clear();
        for note in notes {
            self.insert(note);
        }
    }

    /// Take everything another pattern holds, except its name.
    ///
    /// The name stays the slot's own: it becomes the track name on export, and a bank whose tracks
    /// are all called `1` is not what copying a pattern into slot five meant. The notes arrive
    /// through [`Pattern::insert`] like any others, so they are given fresh ids here rather than
    /// carrying the ones they had where they were copied from — two patterns are never asked to
    /// agree about a name, and the selection in one cannot then point into the other.
    pub fn take_contents(&mut self, from: &Pattern) {
        self.length = from.length;
        self.constraint = from.constraint;
        self.scale.clone_from(&from.scale);
        // Length first, so nothing is clipped on the way in.
        self.replace(from.notes.iter().copied());
    }

    /// Change the loop point, dropping and clipping whatever no longer fits.
    pub fn set_length(&mut self, length: u32) {
        self.length = length.max(TICKS_PER_BEAT);
        self.notes.retain(|n| n.start < self.length);
        for note in &mut self.notes {
            note.length = note.length.min(self.length - note.start).max(1);
        }
    }

    /// The index of the first note starting at or after `tick`. This is what the player seeks with
    /// after the host jumps somewhere.
    pub fn cursor_at(&self, tick: u32) -> usize {
        self.notes.partition_point(|n| n.start < tick)
    }
}

/// How many patterns one instance holds.
pub const SLOTS: usize = 16;

/// The bank of patterns, and which of them is playing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Bank {
    pub patterns: Vec<Pattern>,
    pub current: usize,
}

impl Default for Bank {
    fn default() -> Self {
        Self {
            patterns: (0..SLOTS)
                .map(|i| Pattern::empty(format!("{}", i + 1), 4, 4))
                .collect(),
            current: 0,
        }
    }
}

impl Bank {
    /// The playing pattern. Always answers: a bank restored from a shorter file is padded rather
    /// than left able to index past its own end on the audio thread.
    pub fn pattern(&self, slot: usize) -> &Pattern {
        &self.patterns[slot.min(self.patterns.len() - 1)]
    }

    pub fn pattern_mut(&mut self, slot: usize) -> &mut Pattern {
        let slot = slot.min(self.patterns.len() - 1);
        &mut self.patterns[slot]
    }

    /// Bring a bank read off disk up to [`SLOTS`], so nothing above has to check.
    pub fn normalise(&mut self) {
        for pattern in &mut self.patterns {
            pattern.reindex();
        }
        while self.patterns.len() < SLOTS {
            let n = self.patterns.len() + 1;
            self.patterns.push(Pattern::empty(format!("{n}"), 4, 4));
        }
        self.patterns.truncate(SLOTS);
        self.current = self.current.min(SLOTS - 1);
    }
}

/// Which of the 24 quarter-tone pitch classes a generator may use, one bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LaneMask(pub u32);

impl LaneMask {
    pub const ALL: LaneMask = LaneMask((1 << 24) - 1);
    pub const NONE: LaneMask = LaneMask(0);

    /// For serde's `default`, which wants a function rather than a constant.
    pub fn all() -> LaneMask {
        Self::ALL
    }

    pub fn contains(self, lane: Lane) -> bool {
        self.0 & (1 << (lane % LANES_PER_OCTAVE)) != 0
    }

    pub fn set(&mut self, degree: u8, on: bool) {
        let bit = 1 << (degree % LANES_PER_OCTAVE);
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    pub fn is_empty(self) -> bool {
        self.0 & Self::ALL.0 == 0
    }

    pub fn count(self) -> u32 {
        (self.0 & Self::ALL.0).count_ones()
    }

    /// Compose a scale with the hand-drawn mask.
    ///
    /// An empty hand mask means "nothing has been said", not "nothing is allowed" — the toggles
    /// must not be able to leave a generator with no note it may write.
    pub fn compose(scale: LaneMask, hand: LaneMask) -> LaneMask {
        if hand.is_empty() {
            scale
        } else if scale.is_empty() {
            hand
        } else {
            LaneMask(scale.0 & hand.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_stay_sorted_however_they_arrive() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(3840, 240, 60, 100));
        pattern.insert(Note::new(0, 240, 60, 100));
        pattern.insert(Note::new(1920, 240, 61, 100));
        pattern.insert(Note::new(1920, 240, 59, 100));
        let keys: Vec<_> = pattern.notes().iter().map(|n| (n.start, n.lane)).collect();
        assert_eq!(keys, vec![(0, 60), (1920, 59), (1920, 61), (3840, 60)]);
    }

    #[test]
    fn a_note_is_clipped_to_the_pattern_rather_than_overrunning_it() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(
            TICKS_PER_BEAT * 4 - 100,
            TICKS_PER_BEAT * 4,
            60,
            100,
        ));
        assert_eq!(pattern.notes()[0].end(), pattern.length);
    }

    #[test]
    fn a_note_past_the_end_is_not_taken_at_all() {
        let mut pattern = Pattern::empty("p", 1, 4);
        assert_eq!(pattern.insert(Note::new(9000, 240, 60, 100)), None);
        assert!(pattern.is_empty());
    }

    #[test]
    fn shortening_a_pattern_drops_what_no_longer_fits() {
        let mut pattern = Pattern::empty("p", 2, 4);
        pattern.insert(Note::new(0, 240, 60, 100));
        pattern.insert(Note::new(TICKS_PER_BEAT * 5, 240, 60, 100));
        pattern.set_length(TICKS_PER_BEAT * 4);
        assert_eq!(pattern.notes().len(), 1);
    }

    #[test]
    fn the_cursor_lands_on_the_first_note_at_or_after_a_tick() {
        let mut pattern = Pattern::empty("p", 1, 4);
        pattern.insert(Note::new(0, 240, 60, 100));
        pattern.insert(Note::new(1920, 240, 60, 100));
        pattern.insert(Note::new(3840, 240, 60, 100));
        assert_eq!(pattern.cursor_at(0), 0);
        assert_eq!(pattern.cursor_at(1), 1);
        assert_eq!(pattern.cursor_at(1920), 1);
        assert_eq!(pattern.cursor_at(9000), 3);
    }

    #[test]
    fn velocity_cannot_be_set_to_a_note_off() {
        assert_eq!(Note::new(0, 240, 60, 0).velocity, 1);
        assert_eq!(Note::new(0, 240, 60, 200).velocity, 127);
    }

    #[test]
    fn an_empty_hand_mask_does_not_mean_silence() {
        let scale = LaneMask(0b1010_1010);
        assert_eq!(LaneMask::compose(scale, LaneMask::NONE), scale);
        assert_eq!(LaneMask::compose(LaneMask::NONE, scale), scale);
    }

    #[test]
    fn a_hand_mask_narrows_a_scale_rather_than_replacing_it() {
        let scale = LaneMask(0b1111);
        let hand = LaneMask(0b0110);
        assert_eq!(LaneMask::compose(scale, hand), LaneMask(0b0110));
    }

    #[test]
    fn a_note_moved_past_its_neighbour_ends_up_after_it() {
        let mut pattern = Pattern::empty("p", 1, 4);
        let first = pattern.insert(Note::new(0, 240, 60, 100)).unwrap();
        pattern.insert(Note::new(1920, 240, 60, 100));
        pattern.update(first, |note| note.start = 3840);
        let starts: Vec<_> = pattern.notes().iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![1920, 3840]);
        assert_eq!(pattern.find(first).unwrap().start, 3840);
    }

    #[test]
    fn a_note_dragged_off_the_end_is_held_inside_the_pattern() {
        let mut pattern = Pattern::empty("p", 1, 4);
        let id = pattern.insert(Note::new(0, 480, 60, 100)).unwrap();
        pattern.update(id, |note| note.start = 999_999);
        let note = *pattern.find(id).unwrap();
        assert!(note.start < pattern.length);
        assert_eq!(note.end(), pattern.length);
    }

    #[test]
    fn a_note_cannot_be_dragged_shorter_than_the_shortest_note() {
        let mut pattern = Pattern::empty("p", 1, 4);
        let id = pattern.insert(Note::new(0, 480, 60, 100)).unwrap();
        pattern.update(id, |note| note.length = 1);
        assert_eq!(pattern.find(id).unwrap().length, MIN_LENGTH);
    }

    #[test]
    fn every_note_is_given_a_name_of_its_own() {
        let mut pattern = Pattern::empty("p", 1, 4);
        let a = pattern.insert(Note::new(0, 240, 60, 100)).unwrap();
        let b = pattern.insert(Note::new(0, 240, 61, 100)).unwrap();
        assert_ne!(a, b);
        assert_eq!(pattern.find(a).unwrap().lane, 60);
        assert_eq!(pattern.find(b).unwrap().lane, 61);
    }

    #[test]
    fn a_name_still_finds_its_note_after_the_list_has_been_re_sorted() {
        let mut pattern = Pattern::empty("p", 1, 4);
        let id = pattern.insert(Note::new(1920, 240, 60, 100)).unwrap();
        // Inserting before it moves it down the vector; the name must not follow the index.
        pattern.insert(Note::new(0, 240, 60, 100));
        assert_eq!(pattern.find(id).unwrap().start, 1920);
    }

    #[test]
    fn a_copied_pattern_arrives_whole_but_under_the_name_it_is_copied_onto() {
        let mut from = Pattern::empty("rast", 2, 4);
        from.constraint = LaneMask(0b1010_1101);
        from.scale = "rast on c".to_string();
        from.insert(Note::new(0, 240, 60, 100));
        from.insert(Note::new(1920, 240, 67, 90));

        let mut onto = Pattern::empty("5", 4, 4);
        onto.insert(Note::new(0, 240, 40, 64));
        onto.take_contents(&from);

        assert_eq!(onto.name, "5");
        assert_eq!(onto.length, from.length);
        assert_eq!(onto.constraint, from.constraint);
        assert_eq!(onto.scale, "rast on c");
        let notes: Vec<_> = onto
            .notes()
            .iter()
            .map(|n| (n.start, n.length, n.lane, n.velocity))
            .collect();
        assert_eq!(notes, vec![(0, 240, 60, 100), (1920, 240, 67, 90)]);
    }

    #[test]
    fn a_copied_pattern_longer_than_the_one_it_lands_on_keeps_every_note() {
        // The length has to arrive before the notes do, or `insert` clips them to the old one.
        let mut from = Pattern::empty("long", 4, 4);
        from.insert(Note::new(TICKS_PER_BEAT * 15, 240, 60, 100));
        let mut onto = Pattern::empty("short", 1, 4);
        onto.take_contents(&from);
        assert_eq!(onto.notes().len(), 1);
        assert_eq!(onto.notes()[0].start, TICKS_PER_BEAT * 15);
    }

    #[test]
    fn a_pasted_note_is_given_a_name_of_its_own() {
        let mut from = Pattern::empty("from", 1, 4);
        from.insert(Note::new(0, 240, 60, 100));
        let mut onto = Pattern::empty("onto", 1, 4);
        // Two notes already handed out here, so the ids cannot line up by accident.
        onto.insert(Note::new(0, 240, 40, 64));
        onto.insert(Note::new(240, 240, 41, 64));
        onto.take_contents(&from);

        let id = onto.notes()[0].id;
        assert_ne!(id, 0);
        assert_eq!(onto.find(id).unwrap().lane, 60);
    }

    #[test]
    fn a_bank_read_off_disk_short_is_padded_to_the_full_set_of_slots() {
        let mut bank = Bank {
            patterns: vec![Pattern::empty("1", 4, 4)],
            current: 99,
        };
        bank.normalise();
        assert_eq!(bank.patterns.len(), SLOTS);
        assert_eq!(bank.current, SLOTS - 1);
    }
}
