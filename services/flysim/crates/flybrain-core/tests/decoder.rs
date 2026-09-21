//! Unit tests ported from `packages/brain/tests/readout.test.ts`.
//!
//! The decoder's observable behaviour is commitment, hysteresis, bounded habituation and shared
//! cooldowns, and none of that shows up in a state digest, so it is tested directly here.

mod common;

use flybrain_core::decoder::gameboy::{
    from_button_mask, gameboy_decoder_config, to_button_mask, BLOCKED_FATIGUE, BLOCKED_MS,
    GAMEBOY_BUTTON_BITS,
};
use flybrain_core::decoder::{
    BootVariant, DecoderConfig, DecoderState, ExclusiveGroup, PopulationDecoder, PulseChannel,
};
use flybrain_core::ordered::NumberMap;

/// The values `infra/docs/room-escape.md` chose, spelled out here so a preset edit fails a test.
const CHOSEN_HOLD_MS: f64 = 800.0;
const CHOSEN_FATIGUE_GAIN: f64 = 0.08;
const CHOSEN_HYSTERESIS: f64 = 1.05;

fn bit(name: &str) -> u32 {
    GAMEBOY_BUTTON_BITS
        .iter()
        .find(|(channel, _)| *channel == name)
        .map(|(_, bit)| *bit)
        .expect("a Game Boy channel")
}

/// Eight command roles at 5 spikes/s, the flat baseline the ported tests calibrate against.
fn flat_baseline() -> NumberMap {
    NumberMap::from_pairs((0..8).map(|index| (format!("command_{index}"), 5.0)))
}

fn rates(overrides: &[(&str, f64)]) -> NumberMap {
    let mut rates = flat_baseline();
    for (role, value) in overrides {
        rates.set(role, *value);
    }
    rates
}

/// A calibrated decoder that reports a button mask.
fn decoding(config: DecoderConfig) -> impl FnMut(&NumberMap, f64, bool) -> u32 {
    let mut decoder = PopulationDecoder::new(config).expect("a decoder");
    decoder.calibrate(&flat_baseline());
    move |rates, now_ms, boot| to_button_mask(&decoder.decode(rates, now_ms, boot))
}

/// A calibrated Game Boy decoder, on the live preset.
fn gameboy() -> impl FnMut(&NumberMap, f64, bool) -> u32 {
    decoding(gameboy_decoder_config())
}

/// The Game Boy preset with the exclusive group pinned to the prototype's `MotorDecoder`
/// constants: a 400 ms hold, a 400 ms decision period and a 0.08 fatigue gain.
///
/// `docs/design/room-escape.md` section 1 raised the preset's hold and halved its fatigue gain,
/// measured in `infra/docs/room-escape.md`. The tests ported from the prototype assert behaviour at
/// 400 ms boundaries, which is a fact about the *decoder*, not about the preset's numbers, so they
/// keep the numbers they were written against. `the_gameboy_preset_pins_the_chosen_values` is what
/// guards the live ones. This mirrors `legacyGameboyConfig()` in
/// `packages/brain/tests/readout.test.ts`, where the same split keeps the `MotorDecoder` oracle
/// honest.
fn legacy_gameboy_config() -> DecoderConfig {
    let mut config = gameboy_decoder_config();
    let group = config.exclusive.as_mut().expect("the preset has an exclusive group");
    group.hold_ms = 400.0;
    group.decision_ms = 400.0;
    group.fatigue_gain = 0.08;
    group.hysteresis = 1.15;
    group.blocked_fatigue = 0.0;
    group.blocked_ms = 0.0;
    config
}

#[test]
fn commitment_survives_reversals_and_system_channels_share_a_throttle() {
    let mut decode = decoding(legacy_gameboy_config());
    // Start fires on its strict threshold of 1.35 out of boot? No: score 4 clears it.
    assert_ne!(
        decode(
            &rates(&[("command_0", 20.0), ("command_6", 20.0)]),
            0.0,
            false
        ) & bit("start"),
        0
    );
    // 399 ms later the 400 ms commitment has not lapsed, so `up` is still held.
    assert_ne!(
        decode(
            &rates(&[("command_1", 30.0), ("command_7", 20.0)]),
            399.0,
            false
        ) & bit("up"),
        0
    );
    // At 400 ms a re-decision happens, but a 1-unit lead does not clear the 15% hysteresis.
    assert_eq!(
        decode(
            &rates(&[("command_0", 20.0), ("command_1", 21.0)]),
            400.0,
            false
        ) & 15,
        bit("up")
    );
    // A clear lead does take over.
    assert_eq!(
        decode(&rates(&[("command_1", 30.0)]), 800.0, false) & 15,
        bit("down")
    );
    // Select shares Start's 30 s cooldown, so neither fires at 2500 ms out of boot.
    assert_eq!(
        decode(
            &rates(&[("command_6", 20.0), ("command_7", 20.0)]),
            2500.0,
            false
        ) & (bit("start") | bit("select")),
        0
    );
    // Once the shared cooldown lapses, Start fires again.
    assert_ne!(
        decode(&rates(&[("command_6", 20.0)]), 30000.0, false) & bit("start"),
        0
    );

    // In boot the cooldown is 2500 ms rather than 30 s, so the second press lands.
    let mut boot = decoding(legacy_gameboy_config());
    boot(&rates(&[("command_6", 20.0)]), 0.0, true);
    assert_ne!(
        boot(&rates(&[("command_6", 20.0)]), 2500.0, true) & bit("start"),
        0
    );
}

#[test]
fn the_decoder_chooses_only_the_strongest_direction() {
    let mask = gameboy()(
        &rates(&[("command_2", 20.0), ("command_3", 15.0)]),
        100.0,
        true,
    );
    assert_ne!(mask & bit("left"), 0);
    assert_eq!(mask & bit("right"), 0, "only one direction at a time");
}

#[test]
fn action_channels_respond_to_activity_above_baseline() {
    let mask = gameboy()(&rates(&[("command_4", 10.0)]), 100.0, true);
    assert_ne!(mask & bit("a"), 0);
}

/// The preset's live numbers, which no oracle can check: the prototype had only one set.
///
/// `docs/design/room-escape.md` section 1 and the measurement in `infra/docs/room-escape.md`. One
/// overworld step in Pokemon Red is about 270 ms, so a 400 ms commitment was one to two steps and
/// the walker jittered. The hold and the decision period rise together, the fatigue gain halves so
/// a committed direction can survive a re-decision, and everything else stays the prototype's.
///
/// This is the Rust half of a pair: `packages/brain/tests/readout.test.ts` pins the same values on
/// the TypeScript twin, and `golden_agent` proves the two agree frame by frame.
#[test]
fn the_gameboy_preset_pins_the_chosen_values() {
    let config = gameboy_decoder_config();
    let group = config.exclusive.expect("an exclusive group");
    assert_eq!(group.hold_ms, CHOSEN_HOLD_MS);
    assert_eq!(group.decision_ms, CHOSEN_HOLD_MS, "hold and decision move together");
    assert_eq!(group.fatigue_gain, CHOSEN_FATIGUE_GAIN);
    assert_eq!(group.hysteresis, CHOSEN_HYSTERESIS);
    assert_eq!(group.blocked_fatigue, BLOCKED_FATIGUE);
    assert_eq!(group.blocked_ms, BLOCKED_MS);
    assert_eq!(group.fatigue_decay, 0.8, "the decay is unchanged");
    assert_eq!(
        group.channels,
        vec![
            ("up".to_string(), "command_0".to_string()),
            ("down".to_string(), "command_1".to_string()),
            ("left".to_string(), "command_2".to_string()),
            ("right".to_string(), "command_3".to_string()),
        ]
    );
    assert_eq!(config.clear_lockout_ms, 480.0);

    // And the commitment really lasts that long: `hold_ms == decision_ms` means the winner never
    // gaps and never overstays.
    let mut decode = gameboy();
    assert_eq!(decode(&rates(&[("command_0", 20.0)]), 0.0, false) & 15, bit("up"));
    assert_eq!(
        decode(&rates(&[("command_1", 60.0)]), CHOSEN_HOLD_MS - 1.0, false) & 15,
        bit("up"),
        "a clear challenger does not interrupt the commitment"
    );
    assert_eq!(
        decode(&rates(&[("command_1", 60.0)]), CHOSEN_HOLD_MS, false) & 15,
        bit("down"),
        "and takes over at the decision, not before"
    );
}

#[test]
fn button_masks_round_trip_through_channel_names() {
    assert_eq!(to_button_mask(&[]), 0);
    assert_eq!(
        to_button_mask(&["up".to_string(), "a".to_string(), "nonsense".to_string()]),
        bit("up") | bit("a"),
        "unknown names contribute nothing"
    );
    assert_eq!(
        from_button_mask(bit("select") | bit("left")),
        vec!["left".to_string(), "select".to_string()],
        "unpacked in bits key order"
    );
    assert_eq!(from_button_mask(0xff).len(), 8);
}

// --- Generic (non-preset) configurations ---------------------------------------------------------

/// Three-way exclusive group plus one pulse channel that has no boot variant.
fn generic_config() -> DecoderConfig {
    DecoderConfig {
        macros: None,
        exclusive: Some(ExclusiveGroup {
            channels: vec![
                ("x".to_string(), "rate_x".to_string()),
                ("y".to_string(), "rate_y".to_string()),
                ("z".to_string(), "rate_z".to_string()),
            ],
            decision_ms: 100.0,
            hold_ms: 100.0,
            hysteresis: 1.15,
            fatigue_gain: 0.08,
            fatigue_decay: 0.8,
            blocked_fatigue: 0.0,
            blocked_ms: 0.0,
        }),
        pulses: vec![PulseChannel {
            channel: "fire".to_string(),
            role: "rate_fire".to_string(),
            hold_ms: 20.0,
            cooldown_ms: 200.0,
            threshold: 1.0,
            boot: None,
            throttle_group: None,
        }],
        clear_lockout_ms: 50.0,
    }
}

fn generic_baseline() -> NumberMap {
    NumberMap::from_pairs([
        ("rate_x", 5.0),
        ("rate_y", 5.0),
        ("rate_z", 5.0),
        ("rate_fire", 5.0),
    ])
}

fn generic_rates(overrides: &[(&str, f64)]) -> NumberMap {
    let mut rates = generic_baseline();
    for (role, value) in overrides {
        rates.set(role, *value);
    }
    rates
}

fn calibrated(config: DecoderConfig, baseline: &NumberMap) -> PopulationDecoder {
    let mut decoder = PopulationDecoder::new(config).expect("a decoder");
    decoder.calibrate(baseline);
    decoder
}

#[test]
fn hysteresis_holds_the_incumbent_until_fatigue_lets_the_runner_up_win() {
    let mut decoder = calibrated(generic_config(), &generic_baseline());
    let tied = generic_rates(&[("rate_x", 20.0), ("rate_y", 20.0)]);

    // x wins outright: score 3.5 against 1.0 for the unstimulated channels.
    assert_eq!(
        decoder.decode(&generic_rates(&[("rate_x", 20.0)]), 0.0, true),
        vec!["x".to_string()]
    );
    // With 0.08 fatigue, x reads 3.240 against y's 3.5 — an 8% lead, under the 15% margin.
    assert_eq!(decoder.decode(&tied, 100.0, true), vec!["x".to_string()]);
    assert_eq!(decoder.export_state().current.as_deref(), Some("x"));
    // A second decision of fatigue (0.16) drops x to 3.017; y's 3.5 now clears 1.15x.
    assert_eq!(decoder.decode(&tied, 200.0, true), vec!["y".to_string()]);

    let state = decoder.export_state();
    assert_eq!(state.current.as_deref(), Some("y"));
    assert_eq!(state.fatigue.get("y"), Some(0.08));
    let fatigue_x = state.fatigue.get("x").expect("x fatigue");
    assert!(
        fatigue_x > 0.12 && fatigue_x < 0.16,
        "x fatigue decayed: {fatigue_x}"
    );
    assert_eq!(state.fatigue.get("z"), Some(0.0));

    // clearHolds resets the group and locks every channel out for clearLockoutMs.
    decoder.clear_holds(250.0);
    let cleared = decoder.export_state();
    assert_eq!(cleared.current, None);
    assert_eq!(
        cleared.fatigue,
        NumberMap::from_pairs([("x", 0.0), ("y", 0.0), ("z", 0.0)])
    );
    assert_eq!(
        cleared.held_until,
        NumberMap::from_pairs([("x", 0.0), ("y", 0.0), ("z", 0.0), ("fire", 0.0)])
    );
    assert_eq!(
        cleared.next_allowed,
        NumberMap::from_pairs([("x", 300.0), ("y", 300.0), ("z", 300.0), ("fire", 300.0)])
    );
    assert_eq!(cleared.next_decision, 250.0);
}

#[test]
fn exact_ties_keep_the_earlier_channel() {
    let mut idle = calibrated(generic_config(), &generic_baseline());
    assert_eq!(
        idle.decode(&generic_baseline(), 0.0, true),
        vec!["x".to_string()],
        "an all-rest tie keeps the first channel"
    );
    let mut contested = calibrated(generic_config(), &generic_baseline());
    assert_eq!(
        contested.decode(
            &generic_rates(&[("rate_y", 20.0), ("rate_z", 20.0)]),
            0.0,
            true
        ),
        vec!["y".to_string()],
        "only a strictly greater score displaces the running best"
    );
}

#[test]
fn a_pulse_channel_without_a_boot_variant_behaves_the_same_in_and_out_of_boot() {
    for boot in [true, false] {
        let mut decoder = calibrated(generic_config(), &generic_baseline());
        let hot = generic_rates(&[("rate_fire", 20.0)]);
        // Exclusive channels come first in the returned list, then pulses.
        assert_eq!(
            decoder.decode(&hot, 0.0, boot),
            vec!["x".to_string(), "fire".to_string()],
            "boot={boot}"
        );
        // 200 ms cooldown regardless of the boot flag.
        assert_eq!(
            decoder.decode(&hot, 100.0, boot),
            vec!["x".to_string()],
            "boot={boot}"
        );
        assert_eq!(
            decoder.decode(&hot, 200.0, boot),
            vec!["y".to_string(), "fire".to_string()],
            "boot={boot}"
        );
    }
}

#[test]
fn an_uncalibrated_decoder_reports_nothing() {
    let mut decoder = PopulationDecoder::new(generic_config()).expect("a decoder");
    assert_eq!(
        decoder.decode(
            &generic_rates(&[("rate_x", 99.0), ("rate_fire", 99.0)]),
            0.0,
            true
        ),
        Vec::<String>::new()
    );
    assert!(!decoder.export_state().calibrated);
    assert_eq!(
        decoder.channel_names(),
        ["x", "y", "z", "fire"].map(String::from),
        "channelNames is the decode order"
    );
}

#[test]
fn a_throttle_group_shares_one_cooldown_across_its_pulse_channels() {
    let pulse = |channel: &str, role: &str, group: Option<&str>| PulseChannel {
        channel: channel.to_string(),
        role: role.to_string(),
        hold_ms: 10.0,
        cooldown_ms: 100.0,
        threshold: 1.0,
        boot: None,
        throttle_group: group.map(str::to_string),
    };
    let config = DecoderConfig {
        macros: None,
        exclusive: None,
        pulses: vec![
            pulse("p1", "rate_1", Some("shared")),
            pulse("p2", "rate_2", Some("shared")),
            pulse("solo", "rate_3", None),
        ],
        clear_lockout_ms: 40.0,
    };
    let baseline = NumberMap::from_pairs([("rate_1", 0.0), ("rate_2", 0.0), ("rate_3", 0.0)]);
    let mut decoder = calibrated(config, &baseline);
    let hot = |one: f64, two: f64, three: f64| {
        NumberMap::from_pairs([("rate_1", one), ("rate_2", two), ("rate_3", three)])
    };

    // p1 fires and throttles p2 in the same pass; solo keeps its own cooldown.
    assert_eq!(
        decoder.decode(&hot(5.0, 5.0, 5.0), 0.0, true),
        vec!["p1".to_string(), "solo".to_string()]
    );
    assert_eq!(
        decoder.export_state().next_allowed,
        NumberMap::from_pairs([("p1", 100.0), ("p2", 100.0), ("solo", 100.0)])
    );
    assert_eq!(
        decoder.decode(&hot(5.0, 5.0, 5.0), 50.0, true),
        Vec::<String>::new()
    );
    // Once the shared cooldown lapses either member may claim it; p2 then throttles p1.
    assert_eq!(
        decoder.decode(&hot(0.0, 5.0, 0.0), 100.0, true),
        vec!["p2".to_string()]
    );
    assert_eq!(
        decoder.decode(&hot(5.0, 0.0, 5.0), 150.0, true),
        vec!["solo".to_string()]
    );
    assert_eq!(
        decoder.export_state().next_allowed,
        NumberMap::from_pairs([("p1", 200.0), ("p2", 200.0), ("solo", 250.0)])
    );
}

#[test]
fn a_duplicate_channel_is_refused_at_construction() {
    let config = DecoderConfig {
        macros: None,
        exclusive: Some(ExclusiveGroup {
            channels: vec![("a".to_string(), "rate_a".to_string())],
            decision_ms: 100.0,
            hold_ms: 100.0,
            hysteresis: 1.15,
            fatigue_gain: 0.08,
            fatigue_decay: 0.8,
            blocked_fatigue: 0.0,
            blocked_ms: 0.0,
        }),
        pulses: vec![PulseChannel {
            channel: "a".to_string(),
            role: "rate_b".to_string(),
            hold_ms: 10.0,
            cooldown_ms: 10.0,
            threshold: 1.0,
            boot: None,
            throttle_group: None,
        }],
        clear_lockout_ms: 0.0,
    };
    assert_eq!(
        PopulationDecoder::new(config).unwrap_err().message(),
        "Duplicate decoder channel"
    );
}

#[test]
fn invalid_checkpoints_throw_without_mutating_the_decoder() {
    let mut decoder = calibrated(generic_config(), &generic_baseline());
    decoder.decode(
        &generic_rates(&[("rate_x", 20.0), ("rate_fire", 20.0)]),
        0.0,
        true,
    );
    let valid = decoder.export_state();
    let before = decoder.export_state();

    let mut rejected: Vec<(DecoderState, &str)> = Vec::new();
    let with = |patch: &dyn Fn(&mut DecoderState)| {
        let mut state = valid.clone();
        patch(&mut state);
        state
    };
    rejected.push((with(&|state| state.version = 5), "Invalid decoder version"));
    rejected.push((with(&|state| state.version = 1), "Invalid decoder version"));
    rejected.push((
        with(&|state| state.next_decision = f64::NAN),
        "Invalid decoder checkpoint",
    ));
    rejected.push((
        with(&|state| state.next_decision = f64::INFINITY),
        "Invalid decoder checkpoint",
    ));
    rejected.push((
        with(&|state| state.baseline.set("rate_x", f64::NAN)),
        "Invalid decoder checkpoint",
    ));
    rejected.push((
        with(&|state| state.held_until = NumberMap::new()),
        "Invalid decoder checkpoint",
    ));
    rejected.push((
        with(&|state| state.next_allowed.set("fire", f64::NAN)),
        "Invalid decoder checkpoint",
    ));
    rejected.push((
        with(&|state| state.fatigue = NumberMap::new()),
        "Invalid decoder fatigue",
    ));
    rejected.push((
        with(&|state| state.fatigue.set("x", 1.5)),
        "Invalid decoder fatigue",
    ));
    rejected.push((
        with(&|state| state.fatigue.set("x", -0.1)),
        "Invalid decoder fatigue",
    ));
    rejected.push((
        with(&|state| state.fatigue.set("x", f64::NAN)),
        "Invalid decoder fatigue",
    ));
    rejected.push((
        with(&|state| state.current = Some("fire".to_string())),
        "Invalid decoder channel",
    ));
    rejected.push((
        with(&|state| state.current = Some("nope".to_string())),
        "Invalid decoder channel",
    ));

    for (state, message) in rejected {
        assert_eq!(
            decoder.import_state(&state).unwrap_err().message(),
            message,
            "accepted {state:?}"
        );
        assert_eq!(decoder.export_state(), before, "mutated on {message}");
    }

    decoder.import_state(&valid).expect("its own state");
    assert_eq!(decoder.export_state(), before);
}

#[test]
fn a_boot_variant_never_overrides_the_hold() {
    // holdMs is never overridden by the boot variant; only cooldown and threshold are.
    let config = DecoderConfig {
        macros: None,
        exclusive: None,
        pulses: vec![PulseChannel {
            channel: "p".to_string(),
            role: "rate".to_string(),
            hold_ms: 55.0,
            cooldown_ms: 30000.0,
            threshold: 1.35,
            boot: Some(BootVariant {
                cooldown_ms: 2500.0,
                threshold: 1.0,
            }),
            throttle_group: None,
        }],
        clear_lockout_ms: 0.0,
    };
    let baseline = NumberMap::from_pairs([("rate", 0.0)]);

    // A score of 1.2 clears the boot threshold of 1 but not the strict 1.35.
    let mut boot = calibrated(config.clone(), &baseline);
    assert_eq!(
        boot.decode(&NumberMap::from_pairs([("rate", 0.2)]), 0.0, true),
        vec!["p".to_string()]
    );
    assert_eq!(boot.export_state().held_until.get("p"), Some(55.0));
    assert_eq!(boot.export_state().next_allowed.get("p"), Some(2500.0));

    let mut strict = calibrated(config, &baseline);
    assert_eq!(
        strict.decode(&NumberMap::from_pairs([("rate", 0.2)]), 0.0, false),
        Vec::<String>::new()
    );
}

// --- The blocked-direction cooldown --------------------------------------------------------------

/// `generic_config` with the blocked-direction cooldown switched on.
fn blocked_config(blocked_fatigue: f64) -> DecoderConfig {
    let mut config = generic_config();
    let group = config.exclusive.as_mut().expect("an exclusive group");
    group.blocked_fatigue = blocked_fatigue;
    group.blocked_ms = group.hold_ms;
    config
}

#[test]
fn a_blocked_channel_is_fatigued_at_once_and_loses_the_next_decision() {
    // Two channels a few percent apart, which is what the live fly's direction scores look like:
    // x scores 3.5 and y 3.333. Hysteresis needs a 15% lead, so the spread cannot decide anything
    // and only habituation can. At `fatigue_gain` 0.08 that takes x four decisions to lose.
    let rates = generic_rates(&[("rate_x", 20.0), ("rate_y", 19.0)]);

    let mut patient = calibrated(blocked_config(0.0), &generic_baseline());
    let mut held = 0;
    while patient.decode(&rates, f64::from(held) * 100.0, false) == vec!["x".to_string()] {
        held += 1;
    }
    assert_eq!(held, 3, "with the rule off, x keeps the hold for three more decisions");

    let mut decoder = calibrated(blocked_config(0.35), &generic_baseline());
    assert_eq!(decoder.decode(&rates, 0.0, false), vec!["x".to_string()]);
    // The caller has now watched a whole hold go by with no movement and says so. x's score is
    // divided by 1.35, which puts y's 3.333 over the 1.15 threshold at once.
    assert_eq!(
        decoder.decode_blocked(&rates, 100.0, false, Some("x")),
        vec!["y".to_string()],
        "one report of a blocked direction hands the hold straight to the runner-up"
    );
}

#[test]
fn reporting_the_same_block_every_frame_is_one_penalty_not_a_ramp() {
    let rates = generic_rates(&[("rate_x", 20.0)]);
    let mut decoder = calibrated(blocked_config(0.35), &generic_baseline());
    decoder.decode(&rates, 0.0, false);
    // Thirty frames inside one hold, all reporting the same wall: fatigue is set, not accumulated,
    // so it cannot saturate to the cap and cannot outlive the decision that acts on it.
    for frame in 1..30 {
        decoder.decode_blocked(&rates, f64::from(frame), false, Some("x"));
    }
    assert_eq!(
        decoder.export_state().fatigue.get_or_zero("x"),
        0.35,
        "max, not +=: one wall is one penalty however many frames observe it"
    );
}

#[test]
fn a_blocked_report_never_lowers_fatigue_and_is_capped_at_one() {
    let rates = generic_rates(&[("rate_x", 20.0)]);
    let mut decoder = calibrated(blocked_config(0.35), &generic_baseline());
    // Twenty decisions of x winning drive its fatigue past the blocked value.
    for step in 0..20 {
        decoder.decode(&rates, f64::from(step) * 100.0, false);
    }
    let earned = decoder.export_state().fatigue.get_or_zero("x");
    assert!(earned > 0.35, "twenty wins at 0.08 a decision exceed 0.35, got {earned}");
    decoder.decode_blocked(&rates, 2_000.0, false, Some("x"));
    assert!(
        decoder.export_state().fatigue.get_or_zero("x") >= earned,
        "a blocked report raises fatigue or leaves it alone; it never resets it"
    );

    let mut hard = calibrated(blocked_config(4.0), &generic_baseline());
    hard.decode_blocked(&rates, 0.0, false, Some("x"));
    assert_eq!(hard.export_state().fatigue.get_or_zero("x"), 1.0, "still bounded by the cap");
}

#[test]
fn a_blocked_report_is_ignored_when_the_rule_is_off_or_the_channel_is_not_in_the_group() {
    let rates = generic_rates(&[("rate_x", 20.0)]);

    let mut off = calibrated(blocked_config(0.0), &generic_baseline());
    off.decode_blocked(&rates, 0.0, false, Some("x"));
    assert_eq!(off.export_state().fatigue.get_or_zero("x"), 0.08, "the winner's ordinary gain only");

    let mut decoder = calibrated(blocked_config(0.35), &generic_baseline());
    decoder.decode_blocked(&rates, 0.0, false, Some("fire"));
    decoder.decode_blocked(&rates, 1.0, false, Some("nonsense"));
    assert_eq!(
        decoder.export_state().fatigue.get_or_zero("x"),
        0.08,
        "a pulse channel and an unknown name both have no fatigue to raise"
    );
}

#[test]
fn decode_is_decode_blocked_with_no_block_and_the_preset_exposes_its_timing() {
    let rates = generic_rates(&[("rate_x", 20.0), ("rate_y", 8.0)]);
    let mut plain = calibrated(blocked_config(0.35), &generic_baseline());
    let mut explicit = calibrated(blocked_config(0.35), &generic_baseline());
    for step in 0..10 {
        let ms = f64::from(step) * 100.0;
        assert_eq!(plain.decode(&rates, ms, false), explicit.decode_blocked(&rates, ms, false, None));
    }
    assert_eq!(plain.export_state(), explicit.export_state());

    assert_eq!(
        PopulationDecoder::new(blocked_config(0.35)).expect("a decoder").blocked_ms(),
        100.0,
        "the caller reads its own observation window off the preset"
    );
    assert_eq!(
        PopulationDecoder::new(generic_config()).expect("a decoder").blocked_ms(),
        0.0,
        "and zero means it never reports anything"
    );
}

#[test]
fn current_names_the_channel_the_group_is_holding() {
    let mut decoder = calibrated(generic_config(), &generic_baseline());
    assert_eq!(decoder.current(), None, "nothing is held before the first decision");
    decoder.decode(&generic_rates(&[("rate_y", 20.0)]), 0.0, false);
    assert_eq!(decoder.current(), Some("y"));
    decoder.clear_holds(100.0);
    assert_eq!(decoder.current(), None, "a clear forgets the winner");
}

/// The read-only score accessor (`docs/design/macros.md` section 10).
///
/// Two claims, and the second is the one that matters: the numbers are `docs/readout.md`'s score
/// for every channel of both kinds, and *reading* them changes nothing a decode decides.
#[test]
fn the_score_accessor_reports_every_channel_and_changes_no_decision() {
    // A fresh decoder has decoded nothing, so every channel reads "at rest".
    let mut decoder = PopulationDecoder::new(gameboy_decoder_config()).expect("a decoder");
    for channel in decoder.channel_names() {
        assert_eq!(decoder.last_scores().get(channel), Some(1.0), "{channel} before any decode");
    }
    decoder.calibrate(&flat_baseline());

    // The score is `(rate + 1) / (baseline + 1)`, per channel, exclusive and pulse alike, and it
    // is the pre-fatigue score: UP wins this decision and its score is still 11/6.
    let hot = rates(&[("command_0", 10.0), ("command_4", 20.0)]);
    let active = decoder.decode(&hot, 0.0, false);
    assert_eq!(active, vec!["up".to_string(), "a".to_string()]);
    let scores = decoder.last_scores();
    assert_eq!(scores.get("up"), Some(11.0 / 6.0), "the exclusive winner, before fatigue");
    assert_eq!(scores.get("down"), Some(1.0), "a channel at its calibration rate");
    assert_eq!(scores.get("a"), Some(21.0 / 6.0), "a pulse channel");
    assert_eq!(
        scores.keys().cloned().collect::<Vec<_>>(),
        decoder.channel_names().to_vec(),
        "one entry per channel, in decode order"
    );

    // The decision path: two decoders, the same rates at the same clocks, one of them read on
    // every frame. Same channels out, same state, same fatigue -- so the accessor is not in the
    // loop, and a caller cannot perturb the readout by looking at it.
    let mut quiet = PopulationDecoder::new(gameboy_decoder_config()).expect("a decoder");
    let mut watched = PopulationDecoder::new(gameboy_decoder_config()).expect("a decoder");
    quiet.calibrate(&flat_baseline());
    watched.calibrate(&flat_baseline());
    let mut read = Vec::new();
    for frame in 0..600u32 {
        let ms = f64::from(frame) * 16.0;
        // A moving field, so the group re-decides, fatigues and hysteresis-holds through the run.
        let phase = f64::from(frame % 240) / 240.0;
        let rates = rates(&[
            ("command_0", 5.0 + 6.0 * phase),
            ("command_1", 11.0 - 6.0 * phase),
            ("command_2", 5.5),
            ("command_4", 4.0 + 4.0 * phase),
        ]);
        let blocked = (frame % 97 == 0).then_some("left");
        let expected = quiet.decode_blocked(&rates, ms, false, blocked);
        let actual = watched.decode_blocked(&rates, ms, false, blocked);
        assert_eq!(actual, expected, "frame {frame}");
        read.push(watched.last_scores().get_or_zero("up"));
        // And the accessor itself is stable between decodes: reading twice reads the same thing.
        assert_eq!(watched.last_scores().get_or_zero("up"), read[read.len() - 1]);
    }
    assert_eq!(watched.export_state(), quiet.export_state(), "identical state after 600 frames");
    assert!(read.iter().any(|score| *score > 1.0), "the run did exercise a live channel");
}

// --- The macro group (`docs/design/macros.md` sections 11 and 12) ---------------------------------

/// The four macro channels these tests use, named as the dataset names the populations.
const MACROS: [&str; 4] = ["macro_go_out", "macro_go_item", "macro_talk", "macro_menu"];

fn bound(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// The generic config plus a macro group over [`MACROS`], on the direction group's own numbers.
fn macro_config() -> DecoderConfig {
    let mut config = generic_config();
    let group = config.exclusive.clone().expect("the generic config has a group");
    config.macros = Some(ExclusiveGroup {
        channels: MACROS
            .iter()
            .map(|name| ((*name).to_string(), (*name).to_string()))
            .collect(),
        ..group
    });
    config
}

fn macro_baseline() -> NumberMap {
    let mut baseline = generic_baseline();
    for name in MACROS {
        baseline.set(name, 5.0);
    }
    baseline
}

fn macro_rates(overrides: &[(&str, f64)]) -> NumberMap {
    let mut rates = macro_baseline();
    for (role, value) in overrides {
        rates.set(role, *value);
    }
    rates
}

#[test]
fn the_macro_group_is_a_second_exclusive_group_over_its_own_channels() {
    let mut decoder = calibrated(macro_config(), &macro_baseline());
    assert_eq!(decoder.macro_channel_names(), MACROS.map(str::to_string));
    // The macro channels come last, so every index a consumer already reads is untouched.
    assert_eq!(
        decoder.channel_names(),
        ["x", "y", "z", "fire"]
            .iter()
            .chain(MACROS.iter())
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    );

    // Loudest macro population wins its own group, while the direction group decides its own
    // winner on the same decode. One channel of each, never two of either.
    let rates = macro_rates(&[("rate_y", 20.0), ("macro_talk", 20.0)]);
    let active = decoder.decode_bound(&rates, 0.0, false, None, None);
    assert_eq!(active, vec!["y".to_string(), "macro_talk".to_string()]);
    assert_eq!(decoder.current(), Some("y"));
    assert_eq!(decoder.macro_current(), Some("macro_talk"));

    // `last_scores` covers the group, before fatigue, like every other channel.
    assert_eq!(decoder.last_scores().get_or_zero("macro_talk"), 21.0 / 6.0);
    assert_eq!(decoder.last_scores().get_or_zero("macro_menu"), 1.0);
}

#[test]
fn only_the_bound_macro_channels_compete() {
    let mut decoder = calibrated(macro_config(), &macro_baseline());
    // The loudest population by far is not on this pad, so it cannot be pressed: the scene
    // decides which buttons exist (section 12). Masking, not penalising -- no score wins a
    // channel the scene has not bound.
    let rates = macro_rates(&[("macro_talk", 90.0), ("macro_go_item", 20.0)]);
    let active = decoder.decode_bound(
        &rates,
        0.0,
        false,
        None,
        Some(&bound(&["macro_go_out", "macro_go_item"])),
    );
    assert!(active.contains(&"macro_go_item".to_string()), "{active:?}");
    assert!(!active.contains(&"macro_talk".to_string()), "{active:?}");
    assert_eq!(decoder.macro_current(), Some("macro_go_item"));

    // A scene that binds nothing takes no decision at all: no winner, no hold, and the clock does
    // not move, so the frame a button appears on is a frame that can press it.
    let mut fresh = calibrated(macro_config(), &macro_baseline());
    let active = fresh.decode_bound(&rates, 0.0, false, None, Some(&[]));
    assert_eq!(active, vec!["x".to_string()], "the direction group still decides");
    assert_eq!(fresh.macro_current(), None);
    let active = fresh.decode_bound(&rates, 1.0, false, None, Some(&bound(&["macro_talk"])));
    assert!(active.contains(&"macro_talk".to_string()), "{active:?}");
}

#[test]
fn a_masked_channel_cannot_keep_the_seat_it_won() {
    let mut decoder = calibrated(macro_config(), &macro_baseline());
    let rates = macro_rates(&[("macro_talk", 90.0), ("macro_go_item", 20.0)]);
    decoder.decode_bound(&rates, 0.0, false, None, None);
    assert_eq!(decoder.macro_current(), Some("macro_talk"));

    // The scene changes and TALK is no longer a button. The incumbent's 1.05 commitment bonus is
    // not enough to keep a channel that is off the pad -- it is not in the running at all.
    let active = decoder.decode_bound(
        &rates,
        200.0,
        false,
        None,
        Some(&bound(&["macro_go_item", "macro_menu"])),
    );
    assert_eq!(decoder.macro_current(), Some("macro_go_item"));
    assert!(!active.contains(&"macro_talk".to_string()), "{active:?}");
}

#[test]
fn the_macro_group_commits_and_habituates_exactly_as_the_direction_group_does() {
    // Same numbers, from the preset itself: section 12 says a macro is a button.
    let preset = flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros(&MACROS);
    let directions = preset.exclusive.expect("the preset has a direction group");
    let macros = preset.macros.expect("the preset has a macro group");
    assert_eq!(macros.hold_ms, directions.hold_ms);
    assert_eq!(macros.decision_ms, directions.decision_ms);
    assert_eq!(macros.hysteresis, directions.hysteresis);
    assert_eq!(macros.fatigue_gain, directions.fatigue_gain);
    assert_eq!(macros.fatigue_decay, directions.fatigue_decay);
    assert_eq!(macros.blocked_fatigue, BLOCKED_FATIGUE);
    assert_eq!(macros.blocked_ms, BLOCKED_MS);
    assert_eq!(macros.hold_ms, CHOSEN_HOLD_MS);
    assert_eq!(macros.hysteresis, CHOSEN_HYSTERESIS);
    assert_eq!(macros.fatigue_gain, CHOSEN_FATIGUE_GAIN);

    // And the same behaviour: a hold that survives a reversal, then fatigue moving it on.
    let mut decoder = calibrated(macro_config(), &macro_baseline());
    let tied = macro_rates(&[("macro_go_out", 20.0), ("macro_go_item", 20.0)]);
    decoder.decode_bound(&tied, 0.0, false, None, None);
    assert_eq!(decoder.macro_current(), Some("macro_go_out"), "ties keep the earlier channel");
    let mut now = 0.0;
    let mut swapped = None;
    for _ in 0..40 {
        now += 100.0;
        decoder.decode_bound(&tied, now, false, None, None);
        if decoder.macro_current() == Some("macro_go_item") {
            swapped = Some(now);
            break;
        }
    }
    assert!(swapped.is_some(), "bounded habituation must move a tied winner on");
}

#[test]
fn the_direction_group_decodes_identically_with_and_without_a_macro_group() {
    // The one thing section 11 promised the readout: nothing about the buttons moves.
    let mut plain = calibrated(generic_config(), &generic_baseline());
    let mut with_macros = calibrated(macro_config(), &macro_baseline());
    let mut now = 0.0;
    for step in 0..200 {
        now += 17.0;
        let boot = step % 3 == 0;
        let x = 5.0 + f64::from(step % 7);
        let y = 5.0 + f64::from((step * 3) % 5);
        let plain_active = plain.decode(&generic_rates(&[("rate_x", x), ("rate_y", y)]), now, boot);
        let macro_active = with_macros.decode_bound(
            &macro_rates(&[("rate_x", x), ("rate_y", y), ("macro_talk", 40.0)]),
            now,
            boot,
            None,
            Some(&bound(&["macro_talk"])),
        );
        let buttons: Vec<String> = macro_active
            .into_iter()
            .filter(|channel| !channel.starts_with("macro_"))
            .collect();
        assert_eq!(plain_active, buttons, "step {step}");
        assert_eq!(plain.current(), with_macros.current(), "step {step}");
    }
}

#[test]
fn a_checkpoint_from_before_the_macro_group_loads_and_starts_it_rested() {
    let mut before = calibrated(generic_config(), &generic_baseline());
    before.decode(&generic_rates(&[("rate_x", 20.0)]), 0.0, false);
    let old = before.export_state();
    assert_eq!(old.version, 4, "the schema version does not move for an additive group");
    assert_eq!(old.macro_current, None);
    assert!(old.macro_fatigue.keys().next().is_none());

    // The very same checkpoint into a decoder that *has* the group: it carries no entry for any
    // macro channel and they start at zero, which is what keeps the live state loadable.
    let mut after = calibrated(macro_config(), &macro_baseline());
    after.decode_bound(&macro_rates(&[("macro_talk", 40.0)]), 0.0, false, None, None);
    after.import_state(&old).expect("a checkpoint without the macro group must load");
    assert_eq!(after.macro_current(), None);
    let state = after.export_state();
    for name in MACROS {
        assert_eq!(state.macro_fatigue.get_or_zero(name), 0.0, "{name}");
    }
    assert_eq!(state.current.as_deref(), Some("x"), "and the buttons restore as they always did");

    // The restored checkpoint carried no baseline for any macro role, so the first decode after
    // the restore calibrates them from its own rates -- the rule warm-up follows, applied to the
    // channels warm-up never saw (`docs/readout.md`). Every macro channel therefore scores exactly
    // 1.0 on that decision: at rest, which is the honest reading of a channel nobody has measured.
    // Before this, an absent baseline read as zero and the score became `rate + 1`, so the group
    // was decided by which population happened to fire fastest -- live on 2026-09-17 that was
    // 145 Hz against TALK's 41, and TALK was never chosen in forty-eight minutes.
    assert_eq!(after.pending_baseline_roles(), MACROS, "every macro role is waiting");
    after.decode_bound(&macro_rates(&[("macro_talk", 40.0)]), 100.0, false, None, None);
    assert!(after.pending_baseline_roles().is_empty(), "and calibrated by the first decode");
    for name in MACROS {
        assert_eq!(after.last_scores().get_or_zero(name), 1.0, "{name} scores at rest");
    }
    assert_eq!(after.baselines().get_or_zero("macro_talk"), 40.0, "at the rate it was measured at");

    // Round-tripping its own state keeps the group. From this decision on a macro above its own
    // baseline wins on merit, and `macro_talk`'s baseline is the 40 the restore measured.
    after.decode_bound(&macro_rates(&[("macro_talk", 80.0)]), 1000.0, false, None, None);
    let full = after.export_state();
    assert_eq!(full.macro_current.as_deref(), Some("macro_talk"));
    assert!(full.macro_fatigue.get_or_zero("macro_talk") > 0.0);
    after.import_state(&full).expect("its own state");
    assert_eq!(after.export_state(), full);

    // And a macro winner that is not a channel of this decoder is refused like any other.
    let mut invalid = full.clone();
    invalid.macro_current = Some("macro_nope".to_string());
    assert_eq!(
        after.import_state(&invalid).unwrap_err().message(),
        "Invalid decoder channel"
    );
    let mut invalid = full.clone();
    invalid.macro_fatigue.set("macro_talk", 1.5);
    assert_eq!(
        after.import_state(&invalid).unwrap_err().message(),
        "Invalid decoder fatigue"
    );
}

#[test]
fn clearing_holds_rests_both_groups() {
    let mut decoder = calibrated(macro_config(), &macro_baseline());
    decoder.decode_bound(&macro_rates(&[("macro_talk", 40.0)]), 0.0, false, None, None);
    assert_eq!(decoder.macro_current(), Some("macro_talk"));
    decoder.clear_holds(500.0);
    let state = decoder.export_state();
    assert_eq!(state.current, None);
    assert_eq!(state.macro_current, None);
    assert_eq!(state.macro_next_decision, 500.0);
    for name in MACROS {
        assert_eq!(state.macro_fatigue.get_or_zero(name), 0.0, "{name}");
        assert_eq!(state.held_until.get_or_zero(name), 0.0, "{name}");
    }
}

#[test]
fn a_restored_group_with_no_baselines_scores_every_channel_at_rest_then_on_merit() {
    // The rule on its own, without the schema assertions around it, and with the live numbers in
    // it. A checkpoint that predates a channel group leaves those roles with no baseline, and an
    // absent baseline used to read as zero: the score `(rate + 1) / (baseline + 1)` became
    // `rate + 1`, so the group was decided by raw rate. Live on 2026-09-17, forty-eight minutes in
    // Oak's lab with `TALK` never chosen, the macro rates were 27 to 145 Hz.
    let mut before = calibrated(generic_config(), &generic_baseline());
    before.decode(&generic_baseline(), 0.0, false);

    let mut after = calibrated(macro_config(), &macro_baseline());
    after.import_state(&before.export_state()).expect("a checkpoint without the group");
    assert_eq!(after.pending_baseline_roles(), MACROS);

    // The live rates, as far apart as they really were.
    let live = macro_rates(&[
        ("macro_go_out", 145.0),
        ("macro_go_item", 107.0),
        ("macro_talk", 41.0),
        ("macro_menu", 27.0),
    ]);
    after.decode_bound(&live, 0.0, false, None, None);
    for name in MACROS {
        assert_eq!(
            after.last_scores().get_or_zero(name),
            1.0,
            "{name} scores at rest, not at its raw rate"
        );
        assert_eq!(after.baselines().get_or_zero(name), live.get_or_zero(name), "{name}");
    }
    // A tie at 1.0 is decided by the group's own tie rule -- the first channel -- and not by 145 Hz.
    assert_eq!(after.macro_current(), Some(MACROS[0]));

    // And then the group works: the quietest of the four wins the moment it rises above the rest it
    // was measured at, which is the thing the bug made impossible.
    let mut louder = live.clone();
    louder.set("macro_menu", 60.0);
    after.decode_bound(&louder, 1000.0, false, None, None);
    assert_eq!(after.macro_current(), Some("macro_menu"));
}
