//! Golden scenario `restore`: the calibration rule for channels added after a checkpoint.
//!
//! `docs/readout.md`, "Channels added after a checkpoint was written". The oracle exports a state
//! from a decoder built on the plain Game Boy preset — no macro group, so no baseline for any
//! macro role — and imports it into one built on the preset *with* the group. The first decode
//! after that restore calibrates the missing roles from its own rates, so every macro channel
//! scores exactly 1.0 on that decision instead of competing on its raw rate.
//!
//! The rates are the live ones of 2026-09-17 (27 to 145 Hz across the macro roles, `macro_talk`
//! the quiet one at 41, and `TALK` never chosen in forty-eight minutes), followed by a ladder that
//! lifts one macro role at a time above the rest the restore measured. The **scores** travel for
//! every step and every channel, because a score is what the rule changes: a mask comparison alone
//! would pass on a decoder that got the normalization wrong and the argmax right.

mod common;

use common::{assert_f64_exact, golden};
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::decoder::{DecoderState, PopulationDecoder};
use flybrain_core::json::JsonValue;
use flybrain_core::ordered::NumberMap;

/// The oracle's exported `DecoderState`, as this crate's own type.
fn state_from(json: &JsonValue) -> DecoderState {
    let map = |path: &str| -> NumberMap {
        let mut out = NumberMap::new();
        if let Some(object) = json.get(path).and_then(JsonValue::as_object) {
            for (key, value) in object {
                out.set(key, value.as_f64().unwrap_or_else(|| panic!("{path}.{key}")));
            }
        }
        out
    };
    DecoderState {
        version: json.get("version").and_then(JsonValue::as_f64).expect("version") as u32,
        calibrated: matches!(json.get("calibrated"), Some(JsonValue::Bool(true))),
        baseline: map("baseline"),
        held_until: map("heldUntil"),
        next_allowed: map("nextAllowed"),
        next_decision: json.get("nextDecision").and_then(JsonValue::as_f64).expect("nextDecision"),
        current: json.get("current").and_then(JsonValue::as_str).map(str::to_string),
        fatigue: map("fatigue"),
        macro_next_decision: json
            .get("macroNextDecision")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.0),
        macro_current: json.get("macroCurrent").and_then(JsonValue::as_str).map(str::to_string),
        macro_fatigue: map("macroFatigue"),
    }
}

fn want_channel(json: Option<&JsonValue>) -> Option<String> {
    match json {
        Some(JsonValue::String(channel)) => Some(channel.clone()),
        _ => None,
    }
}

#[test]
fn a_restore_calibrates_the_roles_its_checkpoint_never_had_score_for_score() {
    let golden = golden("restore");
    let macro_roles = golden.strings("script.macroRoles");
    let button_roles = golden.strings("script.buttonRoles");
    let roles = golden.strings("script.roles");
    let baseline_rate = golden.number("script.baselineRate");
    let steps = golden.number("script.steps") as usize;

    let macro_names: Vec<&str> = macro_roles.iter().map(String::as_str).collect();
    let config = gameboy_decoder_config_with_macros(&macro_names);
    let mut decoder = PopulationDecoder::new(config).expect("the preset is a valid configuration");
    let channels = golden.strings("channelNames");
    assert_eq!(decoder.channel_names().to_vec(), channels, "decode order is part of the protocol");

    // Calibrated on the macro preset first, so what is under test is the *restore* overwriting it
    // rather than a decoder that was never calibrated at all: the live box had both.
    let mut rested = NumberMap::new();
    for role in &button_roles {
        rested.set(role, baseline_rate);
    }
    for role in &macro_roles {
        rested.set(role, 5.0);
    }
    decoder.calibrate(&rested);

    decoder
        .import_state(&state_from(golden.at("checkpoint")))
        .expect("a checkpoint from before the macro group must load");
    assert_eq!(
        decoder.pending_baseline_roles().to_vec(),
        golden.strings("pending"),
        "the roles the checkpoint had no baseline for, in channel order"
    );

    let rates_flat = golden.f64_chunk("rates");
    let clock = golden.f64_chunk("clock");
    let scores_flat = golden.f64_chunk("scores");
    assert_eq!(rates_flat.len(), steps * roles.len());
    assert_eq!(scores_flat.len(), steps * channels.len());
    assert_eq!(clock.len(), steps);

    let want_winners: Vec<Option<String>> = golden
        .at("winners")
        .as_array()
        .expect("winners")
        .iter()
        .map(|entry| want_channel(Some(entry)))
        .collect();
    let want_macro_winners: Vec<Option<String>> = golden
        .at("macroWinners")
        .as_array()
        .expect("macroWinners")
        .iter()
        .map(|entry| want_channel(Some(entry)))
        .collect();

    for step in 0..steps {
        let mut rates = NumberMap::new();
        for (index, role) in roles.iter().enumerate() {
            rates.set(role, rates_flat[step * roles.len() + index]);
        }
        decoder.decode(&rates, clock[step], false);
        for (index, channel) in channels.iter().enumerate() {
            assert_f64_exact(
                &format!("step {step}: score[{channel}]"),
                decoder.last_scores().get_or_zero(channel),
                scores_flat[step * channels.len() + index],
            );
        }
        assert_eq!(
            decoder.export_state().current,
            want_winners[step],
            "step {step}: the direction group"
        );
        assert_eq!(
            decoder.macro_current().map(str::to_string),
            want_macro_winners[step],
            "step {step}: the macro group"
        );
    }

    // The baselines the restore settled on, role by role, and the final state.
    let want_baselines = golden.at("baselines").as_object().expect("baselines");
    for (role, value) in want_baselines {
        assert_f64_exact(
            &format!("baseline[{role}]"),
            decoder.baselines().get_or_zero(role),
            value.as_f64().unwrap(),
        );
    }
    assert!(decoder.pending_baseline_roles().is_empty(), "nothing is left waiting");
    let want = state_from(golden.at("final"));
    assert_eq!(decoder.export_state(), want, "the whole state, not only the winners");
}
