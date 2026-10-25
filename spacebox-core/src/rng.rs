//! Small deterministic PRNG (SplitMix64) so generated galaxies are reproducible from a seed.

#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[lo, hi]`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn range_f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.f64() * (hi - lo)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.next_u64() as usize % items.len()]
    }
}

/// Deterministic hash of a seed and salts to a number in `[0, 1)`, for decisions that must be
/// reproducible without keeping generator state (AI rolls, rounding of fractional growth).
pub fn hash01(seed: u64, salt: &[u64]) -> f64 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    for s in salt {
        x = (x ^ s.wrapping_mul(0xBF58_476D_1CE4_E5B9)).rotate_left(29);
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    }
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Rounds `x >= 0` down or up at random, so the expected value is `x`.
pub fn stochastic_round(x: f64, roll: f64) -> u64 {
    let x = x.max(0.0);
    let base = x.floor();
    base as u64 + u64::from(roll < x - base)
}
