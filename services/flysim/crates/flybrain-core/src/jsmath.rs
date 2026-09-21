//! JavaScript arithmetic semantics.
//!
//! The TypeScript library is the oracle, so this port has to reproduce V8's numbers bit for bit,
//! not merely closely. Three classes of difference between Rust and JavaScript matter:
//!
//! 1. **Storage rounding.** JS computes everything in `f64` and rounds to `f32` only when a value
//!    is stored into a `Float32Array`. Every such store in the kernel is written here as
//!    `(<f64 expression>) as f32`, which is IEEE round-to-nearest-even, exactly what a
//!    `Float32Array` store does. Never compute in `f32` and never use `mul_add`.
//! 2. **`Math.min`/`Math.max` NaN handling.** `f64::max` returns the non-NaN operand;
//!    `Math.max` propagates NaN. [`js_max`] and [`js_min`] follow JavaScript.
//! 3. **Transcendentals.** `Math.exp` and `Math.tanh` in V8 are the fdlibm ports in
//!    `src/base/ieee754.cc`, not the platform libm. glibc's `exp` disagrees with V8 by 1 ulp on
//!    roughly 10% of the arguments this kernel uses, and glibc's `tanh` by up to 3 ulp, which is
//!    enough to move a Float32 gain and, through synaptic propagation, a spike. See [`exp`] and
//!    [`tanh`].

/// `Math.fround`: round an `f64` to the nearest `f32` and back.
#[inline]
pub fn fround(value: f64) -> f64 {
    value as f32 as f64
}

/// `Math.max(a, b)`: NaN-propagating, unlike [`f64::max`].
#[inline]
pub fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a > b {
        a
    } else if b > a {
        b
    } else if a == 0.0 && b == 0.0 {
        // Math.max(-0, +0) is +0.
        if a.is_sign_positive() {
            a
        } else {
            b
        }
    } else {
        a
    }
}

/// `Math.min(a, b)`: NaN-propagating, unlike [`f64::min`].
#[inline]
pub fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a < b {
        a
    } else if b < a {
        b
    } else if a == 0.0 && b == 0.0 {
        // Math.min(-0, +0) is -0.
        if a.is_sign_negative() {
            a
        } else {
            b
        }
    } else {
        a
    }
}

/// `Math.round`: half away from zero towards +infinity, with the `0.49999999999999994` correction
/// that a naive `floor(x + 0.5)` gets wrong.
#[inline]
pub fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if x > 0.0 && x < 0.5 {
        return 0.0;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    let rounded = (x + 0.5).floor();
    if rounded - x > 0.5 {
        rounded - 1.0
    } else {
        rounded
    }
}

/// `Math.exp`.
///
/// `libm::exp` is the MUSL port of the same fdlibm `__ieee754_exp` that V8 carries in
/// `src/base/ieee754.cc`. Verified bit-identical to V8 over every argument this kernel produces:
/// `exp(-k/5000)` for integer `k` in `0..=2_000_000` (the eligibility-trace decay), `exp(-dt/20)`
/// for integer `dt` in `1..=100` (the spike-pair window), and `exp(-1/decayMs)` for the default
/// and a spread of non-default decay constants. The platform `f64::exp` (glibc) is *not*
/// interchangeable: it differs from V8 by 1 ulp on 19,758 of those 200,001 trace-decay arguments.
#[inline]
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// `Math.tanh`, transcribed from fdlibm `s_tanh.c` as V8 carries it in `src/base/ieee754.cc`.
///
/// Neither `f64::tanh` (glibc) nor `libm::tanh` reproduces V8 here: measured over 1,400,000 seeded
/// arguments they disagree with V8 on 7,095 and 139,693 of them respectively, by up to 3 ulp.
/// fdlibm's `tanh` is a thin wrapper around `expm1`, and `libm::expm1` *is* bit-identical to V8's,
/// so only the wrapper needs transcribing; this form matches V8 on all 1,400,000.
pub fn tanh(x: f64) -> f64 {
    let jx = (x.to_bits() >> 32) as u32 as i32;
    let ix = jx & 0x7fff_ffff;

    // x is +-inf or NaN.
    if ix >= 0x7ff0_0000 {
        return if jx >= 0 {
            1.0 / x + 1.0
        } else {
            1.0 / x - 1.0
        };
    }

    let z = if ix < 0x4036_0000 {
        // |x| < 22
        if ix < 0x3e30_0000 && HUGE + x > 1.0 {
            // |x| < 2^-28: tanh(tiny) = tiny.
            return x;
        }
        if ix >= 0x3ff0_0000 {
            // |x| >= 1
            let t = libm::expm1(2.0 * x.abs());
            1.0 - 2.0 / (t + 2.0)
        } else {
            let t = libm::expm1(-2.0 * x.abs());
            -t / (t + 2.0)
        }
    } else {
        // |x| > 22: +-1, with the inexact flag raised.
        1.0 - TINY
    };

    if jx >= 0 {
        z
    } else {
        -z
    }
}

const HUGE: f64 = 1.0e300;
const TINY: f64 = 1.0e-300;

/// `String(number)`: the ECMAScript `Number::toString` algorithm.
///
/// Used only by the version hashes, which join numeric parameters with `,` and hash the resulting
/// string, so "0.005" must not become "5e-3" and "20" must not become "20.0".
pub fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let mut buffer = ryu_js::Buffer::new();
    buffer.format(value).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fround_matches_the_default_decay_constant() {
        // Math.fround(Math.exp(-1 / 20)) === 0.95122945308685303 in V8; docs/model.md quotes the
        // f64 print of that f32 as 0.951229453086853.
        assert_eq!(fround(exp(-1.0 / 20.0)), 0.951229453086853);
        assert_eq!(exp(-1.0 / 20.0), 0.951229424500714);
    }

    #[test]
    fn js_min_max_propagate_nan_unlike_std() {
        assert!(js_max(-2.0, f64::NAN).is_nan());
        assert!(js_min(1.0, f64::NAN).is_nan());
        assert_eq!((-2.0f64).max(f64::NAN), -2.0, "std::max swallows NaN");
        assert_eq!(js_max(-2.0, -1.5), -1.5);
        assert_eq!(js_min(1.0, 0.25), 0.25);
        assert!(js_max(-0.0, 0.0).is_sign_positive());
        assert!(js_min(-0.0, 0.0).is_sign_negative());
    }

    #[test]
    fn js_round_is_half_up_with_the_awkward_case() {
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(1.5), 2.0);
        assert_eq!(js_round(-0.5), 0.0);
        assert!(js_round(-0.5).is_sign_negative());
        assert_eq!(js_round(2.4), 2.0);
        assert_eq!(js_round(0.499_999_999_999_999_94), 0.0);
    }

    #[test]
    fn number_to_string_matches_javascript() {
        for (value, text) in [
            (20.0, "20"),
            (1.0, "1"),
            (0.005, "0.005"),
            (0.06, "0.06"),
            (300.0, "300"),
            (0.42, "0.42"),
            (1.0 / 25.0, "0.04"),
            (-2.0, "-2"),
            (22_222.0, "22222"),
            (0.2, "0.2"),
            (160.0, "160"),
            (144.0, "144"),
            (0.0001, "0.0001"),
            (0.000_001, "0.000001"),
            (0.000_000_1, "1e-7"),
            (1e21, "1e+21"),
            (5000.0, "5000"),
            (0.1, "0.1"),
            (0.05, "0.05"),
            (0.002, "0.002"),
            (0.9, "0.9"),
            (1.1, "1.1"),
        ] {
            assert_eq!(number_to_string(value), text, "String({value})");
        }
    }

    #[test]
    fn tanh_matches_v8_on_the_documented_values() {
        // Spot values cross-checked against Node: these are the ones the reward path produces.
        assert_eq!(tanh(1.0), 0.7615941559557649);
        assert_eq!(tanh(-0.5), -0.46211715726000974);
        assert_eq!(tanh(0.85), 0.6910694698329305);
        assert_eq!(tanh(0.0), 0.0);
        assert_eq!(tanh(f64::INFINITY), 1.0);
        assert!(tanh(f64::NAN).is_nan());
    }
}
