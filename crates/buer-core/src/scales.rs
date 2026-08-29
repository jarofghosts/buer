//! Which lanes a note may land on: the built-in scales, a Scala file, and the toggles beside the
//! roll, and how the three compose.
//!
//! The composition rule is that **the toggles are the truth and a scale is stamped into them**.
//! Choosing "rast" writes rast's lanes into the mask; a toggle then edits that mask directly.
//!
//! The alternative — keeping the three as layers and intersecting them live — was tried on paper
//! and rejected for one decisive reason: with a live intersection, turning a lane *on* by hand does
//! nothing whenever the chosen scale excludes it. A toggle that visibly moves and changes nothing is
//! the most confusing thing a per-lane control can do, and no caption fixes it. Stamping costs one
//! thing — choosing another scale discards the hand edits — and the undo stack pays that back.

use crate::pattern::{Lane, LaneMask, LANES_PER_OCTAVE};
use crate::scala::ScalaTuning;

/// The scales that ship with buer, as degrees in quarter tones above the tonic.
///
/// The odd numbers are the quarter tones: 7 is a neutral third, three and a half semitones up. The
/// maqam rows are the ajnas as 24-EDO expresses them, which is an approximation the theory itself
/// makes — a rast third sits nearer 355 cents than 350 — and it is the approximation this grid can
/// hold. Anything finer wants a `.scl`, which is what the second source is for.
pub const BUILTIN: &[(&str, &[u8])] = &[
    (
        "chromatic 24",
        &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
        ],
    ),
    ("chromatic 12", &[0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22]),
    ("major (ajam)", &[0, 4, 8, 10, 14, 18, 22]),
    ("minor (nahawand)", &[0, 4, 6, 10, 14, 16, 20]),
    ("dorian", &[0, 4, 6, 10, 14, 18, 20]),
    ("phrygian (kurd)", &[0, 2, 6, 10, 14, 16, 20]),
    ("hijaz", &[0, 2, 8, 10, 14, 16, 20]),
    ("rast", &[0, 4, 7, 10, 14, 18, 21]),
    ("bayati", &[0, 3, 6, 10, 14, 16, 20]),
    ("saba", &[0, 3, 6, 8, 14, 16, 20]),
    ("sikah", &[0, 3, 7, 10, 14, 17, 20]),
    ("huzam", &[0, 3, 4, 10, 14, 17, 20]),
    ("nawa athar", &[0, 4, 6, 12, 14, 16, 22]),
    ("mohajira", &[0, 3, 7, 10, 14, 17, 21]),
    ("neutral pentatonic", &[0, 7, 10, 14, 21]),
    ("whole quarter tone", &[0, 3, 6, 9, 12, 15, 18, 21]),
];

/// The mask a built-in scale makes, rooted on a lane of the octave.
pub fn builtin(index: usize, root: Lane) -> LaneMask {
    let Some((_, degrees)) = BUILTIN.get(index) else {
        return LaneMask::ALL;
    };
    from_degrees(degrees, root)
}

/// A mask from degrees in quarter tones above a root.
pub fn from_degrees(degrees: &[u8], root: Lane) -> LaneMask {
    let mut mask = LaneMask::NONE;
    for degree in degrees {
        mask.set(degree.wrapping_add(root) % LANES_PER_OCTAVE, true);
    }
    // A scale always contains its own root, so a mask can never come out empty and leave a
    // generator hunting for a lane it may use.
    mask.set(root % LANES_PER_OCTAVE, true);
    mask
}

/// Which lanes a Scala scale reaches, and how far the furthest of its degrees had to move.
///
/// The grid is fixed at fifty cents; a scale file is not. Every degree lands on the nearest lane —
/// nothing is dropped — and the worst move is reported so the editor can say what the scale cost.
///
/// Dropping the degrees that fit badly was the first attempt, and it does not work: on a fifty-cent
/// grid no pitch is ever more than twenty-five cents from a lane, so "near enough to a lane" admits
/// everything, and any tighter threshold is a number picked out of the air that quietly loses a
/// just third at 386 cents while keeping a 22-EDO degree at 164. Saying how far the scale moved is
/// both honest and more use: `24edo.scl: every degree landed exactly` against
/// `22edo.scl: landed, the worst degree moved 23 cents`.
pub fn lanes_from_scala(tuning: &ScalaTuning, root: Lane) -> (LaneMask, f64) {
    let scale = &tuning.scale;
    let mut mask = LaneMask::NONE;
    mask.set(root % LANES_PER_OCTAVE, true);
    let mut worst = 0.0f64;

    for degree in 1..=scale.degrees() {
        let folded = scale.degree_cents(degree).rem_euclid(1200.0);
        let nearest = (folded / 50.0).round();
        worst = worst.max((folded - nearest * 50.0).abs());
        let index = (nearest as i64).rem_euclid(LANES_PER_OCTAVE as i64) as u8;
        mask.set(index.wrapping_add(root) % LANES_PER_OCTAVE, true);
    }

    (mask, worst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scala::ScalaScale;

    #[test]
    fn every_built_in_scale_contains_its_own_root() {
        for (index, (name, _)) in BUILTIN.iter().enumerate() {
            for root in 0..LANES_PER_OCTAVE {
                let mask = builtin(index, root);
                assert!(mask.contains(root), "{name} rooted on {root}");
                assert!(!mask.is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn the_twelve_tone_scales_use_no_quarter_tones() {
        for name in ["chromatic 12", "major (ajam)", "minor (nahawand)", "dorian"] {
            let index = BUILTIN.iter().position(|(n, _)| *n == name).unwrap();
            let mask = builtin(index, 0);
            for lane in 0..LANES_PER_OCTAVE {
                if lane % 2 == 1 {
                    assert!(!mask.contains(lane), "{name} reaches lane {lane}");
                }
            }
        }
    }

    #[test]
    fn rast_has_a_neutral_third_and_a_neutral_seventh() {
        let index = BUILTIN.iter().position(|(n, _)| *n == "rast").unwrap();
        let mask = builtin(index, 0);
        // Seven quarter tones is three and a half semitones; twenty-one is ten and a half.
        assert!(mask.contains(7));
        assert!(mask.contains(21));
        assert!(!mask.contains(8), "rast has no major third");
    }

    #[test]
    fn rooting_a_scale_moves_every_degree_with_it() {
        let index = BUILTIN.iter().position(|(n, _)| *n == "rast").unwrap();
        let on_c = builtin(index, 0);
        let on_d = builtin(index, 4);
        for lane in 0..LANES_PER_OCTAVE {
            assert_eq!(on_c.contains(lane), on_d.contains(lane + 4));
        }
    }

    #[test]
    fn a_scale_wraps_round_the_octave_rather_than_off_the_end() {
        let mask = from_degrees(&[0, 22], 4);
        assert!(mask.contains(4));
        // 22 above lane 4 is lane 26, which is lane 2.
        assert!(mask.contains(2));
    }

    #[test]
    fn a_twenty_four_tone_scala_scale_lands_on_every_lane() {
        // A period is what makes a scala line cents rather than a ratio, so `50` would be fifty
        // to one rather than half a semitone.
        let text = (1..=24)
            .map(|degree| format!("{:.1}", degree as f64 * 50.0))
            .collect::<Vec<_>>()
            .join("\n");
        let scale = ScalaScale::parse(&format!("24-edo\n24\n{text}\n")).unwrap();
        let tuning = ScalaTuning::new(scale, None);
        let (mask, worst) = lanes_from_scala(&tuning, 0);
        assert_eq!(worst, 0.0);
        assert_eq!(mask.count(), 24);
    }

    #[test]
    fn a_scale_that_does_not_fit_the_grid_says_how_far_it_had_to_move() {
        // 22-edo: most degrees fall between lanes.
        let text = (1..=22)
            .map(|degree| format!("{}", degree as f64 * 1200.0 / 22.0))
            .collect::<Vec<_>>()
            .join("\n");
        let scale = ScalaScale::parse(&format!("22-edo\n22\n{text}\n")).unwrap();
        let tuning = ScalaTuning::new(scale, None);
        let (_, worst) = lanes_from_scala(&tuning, 0);
        // A 22-EDO step is 54.5 cents, so its degrees walk right across the fifty-cent grid.
        assert!(worst > 20.0, "the worst degree only moved {worst} cents");
    }

    #[test]
    fn a_neutral_third_a_few_cents_off_still_lands_on_its_lane() {
        // Rast's third is nearer 355 cents than 350, and belongs on lane 7 all the same.
        let scale = ScalaScale::parse("rast-ish\n2\n355.0\n1200.0\n").unwrap();
        let tuning = ScalaTuning::new(scale, None);
        let (mask, worst) = lanes_from_scala(&tuning, 0);
        assert!(worst < 6.0, "moved {worst} cents");
        assert!(mask.contains(7));
    }
}
