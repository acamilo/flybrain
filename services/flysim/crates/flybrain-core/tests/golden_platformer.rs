//! Golden scenario `platformer`: the platformer decoder preset, mask for mask.
//!
//! Two presets that agree on a random walk can still disagree by a millisecond, so the generator
//! quantizes both halves of the input: the clock advances by the preset's own timings and each
//! rate lands exactly on a threshold. Rates, timestamps, boot flags and clear points travel as
//! chunks, so this test decodes the oracle's input rather than a reconstruction of it, and a
//! difference can only be the decoder or the preset's numbers.
//!
//! The parameter assertions at the end are the point of the file as much as the masks are: they
//! pin every value in `docs/design/platformer.md` §4 against the TypeScript twin.

mod common;

use common::{assert_f64_exact, golden};
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_core::decoder::platformer::platformer_decoder_config;
use flybrain_core::decoder::{DecoderConfig, PopulationDecoder};
use flybrain_core::json::JsonValue;
use flybrain_core::ordered::NumberMap;

/// The configuration the oracle recorded, field by field, against the Rust preset.
fn assert_config_matches(config: &DecoderConfig, want: &JsonValue) {
    let group = config.exclusive.as_ref().expect("the preset has an exclusive group");
    let want_group = want.get("exclusive").expect("config.exclusive");
    let want_channels = want_group
        .get("channels")
        .and_then(JsonValue::as_object)
        .expect("config.exclusive.channels");
    assert_eq!(
        group
            .channels
            .iter()
            .map(|(channel, role)| (channel.clone(), role.clone()))
            .collect::<Vec<_>>(),
        want_channels
            .iter()
            .map(|(channel, role)| (channel.clone(), role.as_str().unwrap().to_string()))
            .collect::<Vec<_>>(),
        "channel order decides argmax ties, so it is part of the preset"
    );
    for (field, got) in [
        ("decisionMs", group.decision_ms),
        ("holdMs", group.hold_ms),
        ("hysteresis", group.hysteresis),
        ("fatigueGain", group.fatigue_gain),
        ("fatigueDecay", group.fatigue_decay),
    ] {
        assert_f64_exact(
            field,
            got,
            want_group.get(field).and_then(JsonValue::as_f64).unwrap(),
        );
    }

    let want_pulses = want.get("pulses").and_then(JsonValue::as_array).expect("config.pulses");
    assert_eq!(config.pulses.len(), want_pulses.len());
    for (pulse, want) in config.pulses.iter().zip(want_pulses) {
        let label = &pulse.channel;
        assert_eq!(
            pulse.channel.as_str(),
            want.get("channel").and_then(JsonValue::as_str).unwrap()
        );
        assert_eq!(
            pulse.role.as_str(),
            want.get("role").and_then(JsonValue::as_str).unwrap(),
            "{label}: role"
        );
        for (field, got) in [
            ("holdMs", pulse.hold_ms),
            ("cooldownMs", pulse.cooldown_ms),
            ("threshold", pulse.threshold),
        ] {
            assert_f64_exact(
                &format!("{label}: {field}"),
                got,
                want.get(field).and_then(JsonValue::as_f64).unwrap(),
            );
        }
        match (&pulse.boot, want.get("boot")) {
            (Some(boot), Some(want_boot)) => {
                assert_f64_exact(
                    &format!("{label}: boot.cooldownMs"),
                    boot.cooldown_ms,
                    want_boot.get("cooldownMs").and_then(JsonValue::as_f64).unwrap(),
                );
                assert_f64_exact(
                    &format!("{label}: boot.threshold"),
                    boot.threshold,
                    want_boot.get("threshold").and_then(JsonValue::as_f64).unwrap(),
                );
            }
            (None, None) => {}
            (got, want) => panic!("{label}: boot variant differs: {got:?} vs {want:?}"),
        }
        match (&pulse.throttle_group, want.get("throttleGroup")) {
            (Some(group), Some(JsonValue::String(want))) => assert_eq!(group, want, "{label}"),
            (None, None) => {}
            (got, want) => panic!("{label}: throttle group differs: {got:?} vs {want:?}"),
        }
    }
    assert_f64_exact(
        "clearLockoutMs",
        config.clear_lockout_ms,
        want.get("clearLockoutMs").and_then(JsonValue::as_f64).unwrap(),
    );
}

#[test]
fn the_platformer_preset_decodes_the_oracle_sequence_mask_for_mask() {
    let golden = golden("platformer");
    let config = platformer_decoder_config();
    assert_config_matches(&config, golden.at("config"));

    let roles = golden.strings("script.roles");
    let baseline_rate = golden.number("script.baselineRate");
    let steps = golden.number("script.steps") as usize;
    let clear_every = golden.number("script.clearEvery") as usize;

    let rates_flat = golden.f64_chunk("rates");
    let clock = golden.f64_chunk("clock");
    let boot = golden.chunk("boot").to_vec();
    let clear = golden.chunk("clear").to_vec();
    assert_eq!(rates_flat.len(), steps * roles.len());
    assert_eq!(clock.len(), steps);
    assert_eq!(boot.len(), steps);

    let mut decoder = PopulationDecoder::new(config).expect("the preset is a valid configuration");
    assert_eq!(
        decoder.channel_names().to_vec(),
        golden.strings("channelNames"),
        "decode order is part of the protocol"
    );
    decoder.calibrate(&NumberMap::from_pairs(
        roles.iter().map(|role| (role.as_str(), baseline_rate)),
    ));

    let want_masks: Vec<u32> = golden.numbers("masks").iter().map(|mask| *mask as u32).collect();
    assert_eq!(want_masks.len(), steps);

    for step in 0..steps {
        let now_ms = clock[step];
        if clear[step] == 1 {
            assert!(step > 0 && step.is_multiple_of(clear_every));
            decoder.clear_holds(now_ms);
        }
        let mut rates = NumberMap::new();
        for (index, role) in roles.iter().enumerate() {
            rates.set(role, rates_flat[step * roles.len() + index]);
        }
        let active = decoder.decode(&rates, now_ms, boot[step] == 1);
        assert_eq!(
            to_button_mask(&active),
            want_masks[step],
            "mask differs at step {step} (t={now_ms}, boot={})",
            boot[step]
        );
    }

    // The final state, so a divergence that happens to produce the same masks still fails.
    let want = golden.at("final");
    let state = decoder.export_state();
    assert_eq!(f64::from(state.version), want.get("version").and_then(JsonValue::as_f64).unwrap());
    assert!(state.calibrated);
    assert_f64_exact(
        "final.nextDecision",
        state.next_decision,
        want.get("nextDecision").and_then(JsonValue::as_f64).unwrap(),
    );
    match want.get("current") {
        Some(JsonValue::String(channel)) => {
            assert_eq!(state.current.as_deref(), Some(channel.as_str()))
        }
        _ => assert_eq!(state.current, None),
    }
    for (field, map) in [
        ("baseline", &state.baseline),
        ("heldUntil", &state.held_until),
        ("nextAllowed", &state.next_allowed),
        ("fatigue", &state.fatigue),
    ] {
        let expected = want.get(field).unwrap().as_object().unwrap();
        assert_eq!(
            map.keys().cloned().collect::<Vec<_>>(),
            expected.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            "final.{field} key order"
        );
        for (key, value) in expected {
            assert_f64_exact(
                &format!("final.{field}.{key}"),
                map.get(key).unwrap(),
                value.as_f64().unwrap(),
            );
        }
    }

    // A sequence that never held a direction or never fired a pulse would pass vacuously.
    let directions = 0b1111u32;
    let action = 0b0011_0000u32;
    let system = 0b1100_0000u32;
    assert!(want_masks.iter().any(|mask| mask & directions != 0), "no direction was ever held");
    assert!(want_masks.iter().any(|mask| mask & action != 0), "A and B never fired");
    assert!(want_masks.iter().any(|mask| mask & system != 0), "Start/Select never fired in boot");
    // And the soft-reset combination must be unreachable: A, B, Start and Select in one frame
    // resets Super Mario Land (bank0.asm:1075).
    assert!(
        want_masks.iter().all(|mask| mask & 0b1111_0000 != 0b1111_0000),
        "the soft-reset button combination was decoded"
    );
    assert!(
        want_masks.iter().all(|mask| mask & system != system),
        "Start and Select were held together, so the throttle group is not doing its job"
    );
}
