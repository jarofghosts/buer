//! Making a pattern out of nothing.
//!
//! Two shapes, sharing everything else. [`Shape::Free`] walks the grid rolling dice; [`Shape::Euclid`]
//! spaces a number of pulses as evenly as they will go. They share the pitch set, the register, the
//! velocity range and the seed, because those are the same questions either way and splitting them
//! would mean setting them twice.
//!
//! Generation is a pure function of its [`Spec`]. The seed is *in* the spec and is never rolled in
//! here: the number shown beside the button has to be the number that made what you are looking at,
//! or "keep this one and nudge the density" is impossible.

use crate::pattern::{Lane, LaneMask, Note, MIN_LENGTH, TICKS_PER_BEAT};
use crate::rng::Rng;

/// How the notes are spaced.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Shape {
    /// Roll for a note on every step of the grid.
    Free {
        /// How often a step gets a note at all, 0..1.
        density: f32,
        /// And how often one that would have is left out anyway, 0..1. Density says how busy; this
        /// says how ragged, and a pattern wants both.
        rest: f32,
    },
    /// Spread `pulses` over `steps` as evenly as whole steps allow.
    Euclid {
        steps: u32,
        pulses: u32,
        rotation: u32,
    },
}

impl Default for Shape {
    fn default() -> Self {
        Shape::Free {
            density: 0.5,
            rest: 0.15,
        }
    }
}

/// Everything a generated pattern is made from.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Spec {
    pub shape: Shape,
    /// Which lanes a note may land on, already composed from the scale and the toggles.
    pub allowed: LaneMask,
    /// The register, in lanes. Inclusive at both ends.
    pub low: Lane,
    pub high: Lane,
    pub velocity: (u8, u8),
    /// The step the free shape walks, and the length a euclid pulse is measured against.
    pub grid: u32,
    /// Candidate note lengths in ticks, with weights. An empty list means one grid step.
    pub lengths: Vec<(u32, u8)>,
    pub seed: u64,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            shape: Shape::default(),
            allowed: LaneMask::ALL,
            // Two octaves either side of middle c, which is where a hand goes.
            low: 108,
            high: 156,
            velocity: (70, 110),
            grid: TICKS_PER_BEAT / 4,
            lengths: vec![
                (TICKS_PER_BEAT / 4, 4),
                (TICKS_PER_BEAT / 2, 3),
                (TICKS_PER_BEAT, 2),
                (TICKS_PER_BEAT / 8, 1),
            ],
            seed: 0x1a2b_3c4d,
        }
    }
}

/// Generate a pattern's worth of notes.
pub fn generate(length: u32, spec: &Spec) -> Vec<Note> {
    let mut rng = Rng::from_seed(spec.seed);
    let grid = spec.grid.max(MIN_LENGTH);
    let steps: Vec<u32> = match spec.shape {
        Shape::Free { density, rest } => (0..length / grid)
            .filter(|_| rng.chance(density) && !rng.chance(rest))
            .map(|step| step * grid)
            .collect(),
        Shape::Euclid {
            steps,
            pulses,
            rotation,
        } => {
            let steps = steps.clamp(1, 64);
            let pattern = euclid(steps, pulses.min(steps), rotation);
            let step = (length / steps).max(1);
            (0..steps)
                .filter(|index| pattern & (1 << index) != 0)
                .map(|index| index * step)
                .collect()
        }
    };

    let mut notes = Vec::with_capacity(steps.len());
    for start in steps {
        let lane = pick_lane(&mut rng, spec);
        let velocity = rng.between(
            spec.velocity.0.min(spec.velocity.1) as u32,
            spec.velocity.0.max(spec.velocity.1) as u32,
        )
        .clamp(1, 127) as u8;
        let length_ticks = pick_length(&mut rng, spec, grid);
        notes.push(Note::new(start, length_ticks, lane, velocity));
    }
    notes
}

/// A lane in the register that the mask allows.
///
/// Chosen by picking among the allowed lanes rather than by rolling until one fits: a mask with one
/// bit set and a register of two octaves would otherwise reject forty-seven draws out of forty-eight.
fn pick_lane(rng: &mut Rng, spec: &Spec) -> Lane {
    let (low, high) = if spec.low <= spec.high {
        (spec.low, spec.high)
    } else {
        (spec.high, spec.low)
    };
    let allowed = if spec.allowed.is_empty() {
        LaneMask::ALL
    } else {
        spec.allowed
    };

    let mut candidates = 0u32;
    for lane in low..=high {
        if allowed.contains(lane) {
            candidates += 1;
        }
    }
    if candidates == 0 {
        return low;
    }
    let wanted = rng.below(candidates);
    let mut seen = 0;
    for lane in low..=high {
        if allowed.contains(lane) {
            if seen == wanted {
                return lane;
            }
            seen += 1;
        }
    }
    low
}

fn pick_length(rng: &mut Rng, spec: &Spec, grid: u32) -> u32 {
    if spec.lengths.is_empty() {
        return grid;
    }
    let weights: Vec<u8> = spec.lengths.iter().map(|(_, weight)| *weight).collect();
    let index = rng.weighted(&weights);
    spec.lengths
        .get(index)
        .map(|(length, _)| *length)
        .unwrap_or(grid)
        .max(MIN_LENGTH)
}

/// Bjorklund's algorithm, as a bitmask of which steps carry a pulse.
///
/// The pulses come out spaced as evenly as whole steps allow, which is the pattern that turns up in
/// so much of the world's music: three in eight is the tresillo, five in eight the cinquillo, five
/// in twelve a South African bell. Bit *n* set means step *n* sounds.
///
/// This is the real pairing algorithm and not the shorter `(n · pulses) mod steps < pulses`, which
/// is easy to reach for and agrees with it on many inputs but not all — it turns E(5,8) into a
/// rotation of the cinquillo rather than the cinquillo. Where the two disagree it is this one that
/// gives the rhythm people actually name.
pub fn euclid(steps: u32, pulses: u32, rotation: u32) -> u64 {
    let steps = steps.clamp(1, 64);
    let pulses = pulses.min(steps);
    if pulses == 0 {
        return 0;
    }

    // Groups of pulses, and groups of rests. Each pass hangs as many rest-groups as it can onto the
    // ends of the pulse-groups, and repeats on what is left over; the sequence falls out of the
    // flattening when there is at most one group left over.
    let mut ones: Vec<Vec<bool>> = (0..pulses).map(|_| vec![true]).collect();
    let mut zeros: Vec<Vec<bool>> = (0..steps - pulses).map(|_| vec![false]).collect();

    while zeros.len() > 1 {
        let pairs = ones.len().min(zeros.len());
        let mut paired = Vec::with_capacity(pairs);
        for index in 0..pairs {
            let mut group = ones[index].clone();
            group.extend_from_slice(&zeros[index]);
            paired.push(group);
        }
        let left_over = if ones.len() > pairs {
            ones[pairs..].to_vec()
        } else {
            zeros[pairs..].to_vec()
        };
        ones = paired;
        zeros = left_over;
    }

    let mut mask = 0u64;
    let mut step = 0u32;
    for group in ones.iter().chain(zeros.iter()) {
        for &sounds in group {
            if sounds {
                mask |= 1 << ((step + rotation) % steps);
            }
            step += 1;
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(mask: u64, steps: u32) -> String {
        (0..steps)
            .map(|step| if mask & (1 << step) != 0 { 'x' } else { '.' })
            .collect()
    }

    #[test]
    fn euclid_three_in_eight_is_the_tresillo() {
        assert_eq!(bits(euclid(8, 3, 0), 8), "x..x..x.");
    }

    #[test]
    fn euclid_five_in_eight_is_the_cinquillo() {
        assert_eq!(bits(euclid(8, 5, 0), 8), "x.xx.xx.");
    }

    #[test]
    fn euclid_spreads_its_pulses_as_evenly_as_whole_steps_allow() {
        assert_eq!(bits(euclid(16, 4, 0), 16), "x...x...x...x...");
        assert_eq!(bits(euclid(4, 2, 0), 4), "x.x.");
        assert_eq!(bits(euclid(12, 5, 0), 12), "x..x.x..x.x.");
    }

    #[test]
    fn rotation_moves_the_pulses_without_changing_how_many_there_are() {
        for rotation in 0..8 {
            let mask = euclid(8, 3, rotation);
            assert_eq!(mask.count_ones(), 3, "at rotation {rotation}");
        }
        assert_eq!(bits(euclid(8, 3, 1), 8), ".x..x..x");
    }

    #[test]
    fn every_step_sounds_when_every_step_is_a_pulse_and_none_when_none_is() {
        assert_eq!(bits(euclid(8, 8, 0), 8), "xxxxxxxx");
        assert_eq!(bits(euclid(8, 0, 0), 8), "........");
        assert_eq!(euclid(64, 64, 0), u64::MAX);
    }

    #[test]
    fn the_same_seed_gives_the_same_pattern_twice() {
        let spec = Spec::default();
        assert_eq!(generate(7680, &spec), generate(7680, &spec));
    }

    #[test]
    fn another_seed_gives_another_pattern() {
        let a = Spec::default();
        let b = Spec {
            seed: a.seed + 1,
            ..a.clone()
        };
        assert_ne!(generate(7680, &a), generate(7680, &b));
    }

    #[test]
    fn no_generated_note_falls_outside_the_chosen_lanes() {
        let allowed = crate::scales::builtin(
            crate::scales::BUILTIN
                .iter()
                .position(|(name, _)| *name == "rast")
                .unwrap(),
            0,
        );
        let spec = Spec {
            allowed,
            shape: Shape::Free {
                density: 1.0,
                rest: 0.0,
            },
            ..Spec::default()
        };
        let notes = generate(7680 * 4, &spec);
        assert!(!notes.is_empty());
        for note in notes {
            assert!(allowed.contains(note.lane), "lane {}", note.lane);
            assert!((spec.low..=spec.high).contains(&note.lane));
        }
    }

    #[test]
    fn an_empty_lane_mask_generates_notes_rather_than_silence() {
        let spec = Spec {
            allowed: LaneMask::NONE,
            shape: Shape::Free {
                density: 1.0,
                rest: 0.0,
            },
            ..Spec::default()
        };
        assert!(!generate(7680, &spec).is_empty());
    }

    #[test]
    fn a_density_of_zero_generates_nothing_and_one_fills_the_grid() {
        let empty = Spec {
            shape: Shape::Free {
                density: 0.0,
                rest: 0.0,
            },
            ..Spec::default()
        };
        assert!(generate(7680, &empty).is_empty());

        let full = Spec {
            shape: Shape::Free {
                density: 1.0,
                rest: 0.0,
            },
            grid: TICKS_PER_BEAT,
            ..Spec::default()
        };
        assert_eq!(generate(7680, &full).len(), 4);
    }

    #[test]
    fn every_generated_note_starts_inside_the_pattern() {
        let spec = Spec {
            shape: Shape::Euclid {
                steps: 16,
                pulses: 7,
                rotation: 3,
            },
            ..Spec::default()
        };
        let length = 7680;
        let notes = generate(length, &spec);
        assert_eq!(notes.len(), 7);
        for note in notes {
            assert!(note.start < length, "starts at {}", note.start);
        }
    }

    #[test]
    fn a_register_of_one_lane_still_generates() {
        let spec = Spec {
            low: 120,
            high: 120,
            allowed: LaneMask::NONE,
            shape: Shape::Free {
                density: 1.0,
                rest: 0.0,
            },
            ..Spec::default()
        };
        let notes = generate(7680, &spec);
        assert!(!notes.is_empty());
        assert!(notes.iter().all(|note| note.lane == 120));
    }

    #[test]
    fn a_register_given_the_wrong_way_round_is_taken_the_right_way_round() {
        let spec = Spec {
            low: 150,
            high: 110,
            shape: Shape::Free {
                density: 1.0,
                rest: 0.0,
            },
            ..Spec::default()
        };
        for note in generate(7680, &spec) {
            assert!((110..=150).contains(&note.lane));
        }
    }
}
