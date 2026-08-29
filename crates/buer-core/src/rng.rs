//! A small, seedable generator.
//!
//! The xorshift96 mater ports from the microGranny firmware, with a seed of our own rather than the
//! firmware's fixed one — a generated pattern has to be reproducible from the seed shown next to
//! the button, or "generate again" is the only way back to something you liked.
//!
//! Nothing here needs to be cryptographic or even especially good. It needs to be deterministic,
//! allocation-free, and the same on every platform, which rules out `rand`'s defaults as much as it
//! rules out the system generator.

#[derive(Clone, Debug)]
pub struct Rng {
    x: u32,
    y: u32,
    z: u32,
}

impl Default for Rng {
    fn default() -> Self {
        Self::from_seed(0x5eed)
    }
}

impl Rng {
    /// Seed all three words from one number.
    ///
    /// Straight assignment will not do: xorshift is stuck at zero, and neighbouring seeds that
    /// differ in one bit would produce streams that stay correlated for a while. SplitMix64 spreads
    /// one seed over the three words so that seed 1 and seed 2 have nothing to do with each other.
    pub fn from_seed(seed: u64) -> Self {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)) as u32
        };
        Self {
            // Any of the three landing on zero would take a word out of the state; one is as good a
            // substitute as anything, and the stream mixes it back in within a few draws.
            x: next().max(1),
            y: next().max(1),
            z: next().max(1),
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        self.x ^= self.x << 16;
        self.x ^= self.x >> 5;
        self.x ^= self.x << 1;

        let t = self.x;
        self.x = self.y;
        self.y = self.z;
        self.z = t ^ self.x ^ self.y;

        self.z
    }

    /// A number in `0..n`, or zero if `n` is. Multiply-shift rather than a modulo: no division, and
    /// the bias is a part in 2^32 rather than the part in 2^16 the firmware's version carries.
    pub fn below(&mut self, n: u32) -> u32 {
        ((self.next_u32() as u64 * n as u64) >> 32) as u32
    }

    /// A number in `lo..=hi`, whichever way round they were given.
    pub fn between(&mut self, lo: u32, hi: u32) -> u32 {
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        lo + self.below(hi - lo + 1)
    }

    pub fn next_f32(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }

    /// True with probability `p`. Zero never fires and one always does, both exactly.
    pub fn chance(&mut self, p: f32) -> bool {
        if p <= 0.0 {
            false
        } else if p >= 1.0 {
            true
        } else {
            self.next_f32() < p
        }
    }

    /// Pick from a weighted list, returning the index. Weights of zero are never chosen; an
    /// all-zero list falls back to the first entry rather than to nothing.
    pub fn weighted(&mut self, weights: &[u8]) -> usize {
        let total: u32 = weights.iter().map(|&w| w as u32).sum();
        if total == 0 {
            return 0;
        }
        let mut pick = self.below(total);
        for (index, &weight) in weights.iter().enumerate() {
            if pick < weight as u32 {
                return index;
            }
            pick -= weight as u32;
        }
        weights.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let mut a = Rng::from_seed(12345);
        let mut b = Rng::from_seed(12345);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn neighbouring_seeds_do_not_give_neighbouring_streams() {
        let mut a = Rng::from_seed(1);
        let mut b = Rng::from_seed(2);
        let same = (0..64).filter(|_| a.next_u32() == b.next_u32()).count();
        assert_eq!(same, 0);
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = Rng::from_seed(7);
        for _ in 0..10_000 {
            assert!(rng.below(24) < 24);
        }
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn between_is_inclusive_at_both_ends_and_takes_its_bounds_either_way_round() {
        let mut rng = Rng::from_seed(9);
        let mut low = false;
        let mut high = false;
        for _ in 0..10_000 {
            let v = rng.between(60, 62);
            assert!((60..=62).contains(&v));
            low |= v == 60;
            high |= v == 62;
        }
        assert!(low && high);
        assert_eq!(rng.between(5, 5), 5);
        for _ in 0..100 {
            assert!((3..=8).contains(&rng.between(8, 3)));
        }
    }

    #[test]
    fn a_certainty_is_not_left_to_chance() {
        let mut rng = Rng::from_seed(3);
        for _ in 0..1000 {
            assert!(rng.chance(1.0));
            assert!(!rng.chance(0.0));
        }
    }

    #[test]
    fn chance_is_roughly_the_probability_it_was_given() {
        let mut rng = Rng::from_seed(11);
        let hits = (0..10_000).filter(|_| rng.chance(0.25)).count();
        assert!((2_200..2_800).contains(&hits), "hits = {hits}");
    }

    #[test]
    fn a_weight_of_zero_is_never_chosen() {
        let mut rng = Rng::from_seed(13);
        for _ in 0..1000 {
            assert_ne!(rng.weighted(&[1, 0, 1]), 1);
        }
        assert_eq!(rng.weighted(&[0, 0, 0]), 0);
    }
}
