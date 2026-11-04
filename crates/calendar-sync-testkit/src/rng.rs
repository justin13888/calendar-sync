//! The seeded generator behind every fault decision.
//!
//! `SplitMix64`, written out here rather than taken from a crate, so that a seed means the same
//! faults on every platform and in every version of this crate's dependencies.

/// A deterministic 64-bit generator. The same seed yields the same sequence everywhere.
#[derive(Clone, Debug)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. `bound` must not be zero.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        // Lemire's multiply-shift reduction; its bias is far below anything a test can observe.
        let wide = u128::from(self.next_u64()) * u128::from(bound);
        // The high half of a 128-bit product of two u64 values always fits in a u64.
        #[allow(clippy::cast_possible_truncation)]
        let high = (wide >> 64) as u64;
        high
    }

    /// An index in `0..len`. `len` must not be zero.
    pub(crate) fn index(&mut self, len: usize) -> usize {
        let bound = u64::try_from(len).unwrap_or(u64::MAX);
        usize::try_from(self.below(bound)).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::SplitMix64;

    #[test]
    fn known_sequence_for_seed_zero() {
        // Reference values of SplitMix64 for seed 0.
        let mut rng = SplitMix64::new(0);
        assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(rng.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = SplitMix64::new(7);
        for bound in 1..200 {
            assert!(rng.below(bound) < bound);
        }
    }
}
