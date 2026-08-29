//! Lanes, and what to call them.
//!
//! A lane is a quarter tone: `lane / 2` is the MIDI note it sounds from and `lane % 2` says whether
//! it is the quarter tone above that note. Everything downstream — the roll's rows, the generator's
//! pitch set, the bend that leaves as MPE — is counted in lanes, and the split into a note and a
//! bend happens once, in [`crate::mpeout`].

use crate::pattern::{Lane, LANES_PER_OCTAVE};

/// The twelve names, lowercase like everything else the interface says.
const NAMES: [&str; 12] = [
    "c", "c#", "d", "d#", "e", "f", "f#", "g", "g#", "a", "a#", "b",
];

/// Which of the twelve are black keys, so the roll can shade them.
const BLACK: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

/// The MIDI note a lane sounds from.
pub fn note(lane: Lane) -> u8 {
    lane / 2
}

/// Whether a lane is the quarter tone above its note.
pub fn is_quarter(lane: Lane) -> bool {
    lane % 2 == 1
}

/// Whether a lane's note is a black key. A quarter tone belongs to the key below it.
pub fn is_black(lane: Lane) -> bool {
    BLACK[(note(lane) % 12) as usize]
}

/// Where in the octave a lane sits, 0..24. This is what a [`crate::pattern::LaneMask`] indexes.
pub fn degree(lane: Lane) -> u8 {
    lane % LANES_PER_OCTAVE
}

/// The lane as a fractional MIDI note, which is what the pitch actually is.
pub fn to_pitch(lane: Lane) -> f32 {
    lane as f32 / 2.0
}

/// The nearest lane to a fractional MIDI note. This is how an imported note and bend become a row.
pub fn from_pitch(pitch: f32) -> Lane {
    (pitch * 2.0).round().clamp(0.0, 255.0) as Lane
}

/// How far a fractional MIDI note is from the lane it lands on, in cents. Import reports this so a
/// file that is not really 24-EDO does not come in silently wrong.
pub fn cents_from_lane(pitch: f32) -> f32 {
    (pitch - to_pitch(from_pitch(pitch))) * 100.0
}

/// What to call a lane. Middle c, MIDI 60, is `c4`, as in mater; a `+` marks the quarter tone above.
pub fn describe(lane: Lane) -> String {
    let note = note(lane);
    let octave = note as i32 / 12 - 1;
    let name = NAMES[(note % 12) as usize];
    if is_quarter(lane) {
        format!("{name}{octave}+")
    } else {
        format!("{name}{octave}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_c_is_c4_and_the_lane_above_it_is_a_quarter_tone() {
        assert_eq!(describe(120), "c4");
        assert_eq!(describe(121), "c4+");
        assert_eq!(note(121), 60);
        assert!(is_quarter(121));
    }

    #[test]
    fn a_lane_is_half_a_semitone_wide() {
        assert_eq!(to_pitch(120), 60.0);
        assert_eq!(to_pitch(121), 60.5);
    }

    #[test]
    fn a_quarter_tone_sharp_note_lands_on_its_own_lane() {
        assert_eq!(from_pitch(60.5), 121);
        assert_eq!(from_pitch(60.49), 121);
        assert_eq!(from_pitch(60.2), 120);
    }

    #[test]
    fn a_pitch_off_the_grid_reports_how_far_it_was_moved() {
        assert!((cents_from_lane(60.0) - 0.0).abs() < 0.001);
        // 60.2 rounds down to c4 and is 20 cents above it; 60.33 rounds up to c4+ and is 17
        // cents below it. The sign says which way the note was moved, which is the point.
        assert!((cents_from_lane(60.2) - 20.0).abs() < 0.5);
        assert!((cents_from_lane(60.33) + 17.0).abs() < 0.5);
    }

    #[test]
    fn a_pitch_outside_midi_is_clamped_rather_than_wrapping() {
        assert_eq!(from_pitch(-5.0), 0);
        assert_eq!(from_pitch(400.0), 255);
    }

    #[test]
    fn a_quarter_tone_belongs_to_the_key_below_it() {
        // c#4 is a black key, and so is the quarter tone above it.
        assert!(is_black(122));
        assert!(is_black(123));
        assert!(!is_black(120));
    }
}
