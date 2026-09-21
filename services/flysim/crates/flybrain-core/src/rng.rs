//! Deterministic 32-bit xorshift generator, bit-exact with `model/rng.ts`.

/// Xorshift32, the original inline kernel RNG.
///
/// The JavaScript version keeps its state as a raw `number` that the shift operators coerce to
/// int32 on every draw; after the first draw the state is therefore always an int32, and that is
/// the signed value checkpoints store verbatim. This port stores it as an `i32` from the start,
/// which is the same value for every reachable state (the default seed, 22222, is already an
/// int32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xorshift32 {
    value: i32,
}

/// The seed of the original kernel.
pub const DEFAULT_SEED: i32 = 22_222;

impl Default for Xorshift32 {
    fn default() -> Self {
        Self::new(DEFAULT_SEED)
    }
}

impl Xorshift32 {
    pub fn new(seed: i32) -> Self {
        Self { value: seed }
    }

    /// Next raw draw as an unsigned 32-bit integer (`value >>> 0` in the original).
    #[inline]
    pub fn next_uint(&mut self) -> u32 {
        // `value ^= value << 13` etc. in JS: the shifts do the int32 coercion, `>>> 17` is the
        // only unsigned one.
        let mut value = self.value;
        value ^= value << 13;
        value ^= ((value as u32) >> 17) as i32;
        value ^= value << 5;
        self.value = value;
        value as u32
    }

    /// Next draw mapped to `[0, 1)` with 2^-32 resolution.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        f64::from(self.next_uint()) / 4_294_967_296.0
    }

    /// Signed internal state, as written to and read from checkpoints.
    #[inline]
    pub fn state(&self) -> i32 {
        self.value
    }

    /// Restore a checkpointed state. A JS checkpoint holds the signed int32; anything wider is
    /// reduced the way the next shift operator would reduce it (`ToInt32`).
    #[inline]
    pub fn set_state(&mut self, value: i32) {
        self.value = value;
    }

    /// `ToInt32` of an arbitrary JSON integer, for importing a checkpoint written by a host that
    /// stored the state as an unsigned or oversized number.
    pub fn to_int32(value: f64) -> i32 {
        (value as i64 as u32) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduces_the_original_kernel_stream() {
        // The reference stream, recomputed here the way tests/model.test.ts recomputes it.
        let mut legacy: i32 = 22_222;
        let mut legacy_uint = move || {
            let mut value = legacy;
            value ^= value << 13;
            value ^= ((value as u32) >> 17) as i32;
            value ^= value << 5;
            legacy = value;
            value as u32
        };
        let mut rng = Xorshift32::default();
        assert_eq!(rng.state(), 22_222);
        for _ in 0..10_000 {
            assert_eq!(rng.next_uint(), legacy_uint());
        }
        let checkpoint = rng.state();
        let draws = [rng.next_f64(), rng.next_f64(), rng.next_f64()];
        rng.set_state(checkpoint);
        assert_eq!([rng.next_f64(), rng.next_f64(), rng.next_f64()], draws);
        assert!(draws.iter().all(|value| *value >= 0.0 && *value < 1.0));
        assert_ne!(
            Xorshift32::new(7).next_uint(),
            Xorshift32::default().next_uint()
        );
    }

    #[test]
    fn first_draws_match_node() {
        // node -e 'let v=22222; const n=()=>{v^=v<<13;v^=v>>>17;v^=v<<5;return v>>>0};
        //          console.log(n(),n(),n(),n())'
        let mut rng = Xorshift32::default();
        assert_eq!(
            [
                rng.next_uint(),
                rng.next_uint(),
                rng.next_uint(),
                rng.next_uint()
            ],
            [1_374_414_818, 2_413_927_497, 829_730_269, 1_025_812_730]
        );
    }
}
