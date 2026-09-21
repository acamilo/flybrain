//! Golden scenario `math`: the transcendental arguments the 1-ms kernel actually produces.
//!
//! This is the first test to look at when a state golden diverges by one or two ulp. `Math.exp`
//! and `Math.tanh` in V8 are the fdlibm ports in `src/base/ieee754.cc`, and neither the platform
//! libm nor `libm::tanh` reproduces them; see `src/jsmath.rs`.

mod common;

use common::{assert_f64_eq, assert_f64_exact, golden, xorshift};
use flybrain_core::dataset::sha256_hex;
use flybrain_core::envelope::f64_bytes;
use flybrain_core::jsmath::{exp, fround, tanh};

#[test]
fn spike_pair_decay_is_bit_exact_over_the_whole_window() {
    let golden = golden("math");
    let want = golden.f64_chunk("expPair");
    assert_eq!(want.len(), 100, "the pairing window is 0 < dt <= 100 ms");
    let got: Vec<f64> = (1..=100).map(|dt| exp(-f64::from(dt) / 20.0)).collect();
    assert_f64_eq("exp(-dt/20)", &got, &want);
}

#[test]
fn eligibility_trace_decay_is_bit_exact_over_two_million_milliseconds() {
    let golden = golden("math");
    let count = golden.number("expTraceCount") as u64;
    let mut bytes = Vec::with_capacity(count as usize * 8);
    for k in 0..count {
        bytes.extend_from_slice(&exp(-(k as f64) / 5000.0).to_le_bytes());
    }
    assert_eq!(
        sha256_hex(&bytes),
        golden.text("expTraceDigest"),
        "exp(-k/5000) for k in 0..{count} differs from the oracle; \
         the platform f64::exp is not interchangeable with V8's"
    );
}

#[test]
fn membrane_decay_constants_are_bit_exact_after_fround() {
    let golden = golden("math");
    for (decay_ms, want) in golden
        .at("froundDecay")
        .as_object()
        .expect("froundDecay")
        .iter()
    {
        let decay_ms: f64 = decay_ms.parse().expect("a numeric key");
        assert_f64_exact(
            &format!("fround(exp(-1/{decay_ms}))"),
            fround(exp(-1.0 / decay_ms)),
            want.as_f64().expect("a number"),
        );
    }
}

#[test]
fn the_modulator_is_bit_exact_over_seeded_reward_sums() {
    let golden = golden("math");
    let inputs = golden.f64_chunk("tanhInput");
    let want = golden.f64_chunk("tanhOutput");
    assert_eq!(inputs.len(), golden.number("tanhCount") as usize);

    // The generator's own sums, recomputed here: if these do not match, the divergence is the
    // seeded sequence rather than tanh, and the next assertion would mislead.
    let catalog = [1.0, 0.05, 0.2, 0.5, 0.5, 0.1, 3.0, -0.4, 0.25, 0.6, -0.5];
    let mut random = xorshift(20260915);
    let mut sums = Vec::with_capacity(inputs.len());
    for _ in 0..inputs.len() {
        let mut sum = 0.0;
        let terms = 1 + (random() * 4.0).floor() as usize;
        for _ in 0..terms {
            sum += catalog[(random() * catalog.len() as f64).floor() as usize];
        }
        sums.push(sum);
    }
    assert_f64_eq("reward sums", &sums, &inputs);

    let got: Vec<f64> = inputs.iter().copied().map(tanh).collect();
    assert_f64_eq("tanh(reward)", &got, &want);
    // Guard against a vacuous pass: the sums must actually span both signs and both tanh branches.
    assert!(want.iter().any(|value| *value > 0.9));
    assert!(want.iter().any(|value| *value < 0.0));
    assert!(f64_bytes(&want).len() == want.len() * 8);
}
