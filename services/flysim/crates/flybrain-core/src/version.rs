//! Version-string hashing for non-default kernel configurations, matching `model/version.ts`.

use crate::jsmath::number_to_string;

/// FNV-1a-32 over the UTF-16 code units of `text`, as eight lowercase hex digits.
pub fn fnv1a32_hex(text: &str) -> String {
    format!("{:08x}", fnv1a32(text))
}

/// FNV-1a-32 over the UTF-16 code units of `text`.
///
/// `Math.imul(hash ^ code, 16777619) >>> 0` in the original: a wrapping 32-bit multiply.
pub fn fnv1a32(text: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for unit in text.encode_utf16() {
        hash = (hash ^ u32::from(unit)).wrapping_mul(16_777_619);
    }
    hash
}

/// FNV-1a-32 fold of one signed 32-bit value into a running hash.
///
/// The topology hash in `model/plasticity.ts` folds numbers, not characters:
/// `hash = Math.imul(hash ^ value, 16777619) >>> 0`. `hash ^ value` is an int32 XOR, so a negative
/// weight sign-extends, which is why the caller passes `i32` rather than `u32`.
#[inline]
pub fn fnv1a32_step(hash: u32, value: i32) -> u32 {
    ((hash as i32) ^ value).wrapping_mul(16_777_619) as u32
}

/// `base` when every parameter equals its default, otherwise `prefix` plus the FNV-1a-32 hash of
/// the parameters joined with `,`. Parameter order is part of the version contract.
pub fn version_for(base: &str, prefix: &str, params: &[f64], defaults: &[f64]) -> String {
    if params.len() == defaults.len()
        && params
            .iter()
            .zip(defaults)
            // `===` on numbers: NaN never equals itself, +0 equals -0.
            .all(|(value, default)| value == default)
    {
        return base.to_string();
    }
    let joined = params
        .iter()
        .map(|value| number_to_string(*value))
        .collect::<Vec<_>>()
        .join(",");
    format!("{prefix}{}", fnv1a32_hex(&joined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a32_matches_the_reference() {
        // node -e "const h=t=>{let x=2166136261;for(const c of t)x=Math.imul(x^c.charCodeAt(0),16777619)>>>0;
        //          return x.toString(16).padStart(8,'0')};console.log(h(''),h('a'),h('hello'))"
        assert_eq!(fnv1a32_hex(""), "811c9dc5");
        assert_eq!(fnv1a32_hex("a"), "e40c292c");
        assert_eq!(fnv1a32_hex("hello"), "4f9f2cab");
    }

    #[test]
    fn version_for_keeps_the_base_when_defaults_are_unchanged() {
        let defaults = [20.0, 1.0, 2.0];
        assert_eq!(
            version_for("base", "base:", &defaults, &defaults),
            "base".to_string()
        );
        assert_eq!(
            version_for("base", "base:", &[25.0, 1.0, 2.0], &defaults),
            format!("base:{}", fnv1a32_hex("25,1,2"))
        );
    }
}
