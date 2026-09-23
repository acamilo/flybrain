//! The legacy profile `gameboy-legacy-fafb-v783-v1` pins identities this service computes.
//! `fly-session-types` records them as constants (`legacy-gameboy-v1` section 2); this test is
//! where they are recomputed from the committed dataset and the service's own defaults, so the
//! profile cannot silently describe another fly.

mod common;

use std::sync::Arc;

use fly_session_types::gameboy;
use fly_session_types::scalar::RationalNs;
use fly_session_types::{canonical, fixtures};
use flybrain_core::agent::{AgentConfig, DEFAULT_WARMUP_MS, GAMEBOY_MS_PER_FRAME, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::{GAMEBOY_BUTTON_BITS, gameboy_decoder_config_with_macros};
use flybrain_core::decoder::{DecoderConfig, ExclusiveGroup};
use serde_json::{Value, json};

#[test]
fn the_profile_fingerprint_and_versions_are_the_ones_this_build_computes() {
    let path = common::repo_root().join("data/fafb-v783");
    if !path.join("meta.json").is_file() {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    }
    let dataset = load_brain_dataset_from_dir(&path).expect("the committed dataset loads");
    assert_eq!(
        dataset.fingerprint.as_deref(),
        Some(gameboy::FAFB_V783_FINGERPRINT),
        "the legacy profile embeds today's schema-1 fingerprint; a new one is a new profile id"
    );
    let agent = NeuralAgent::new(
        Arc::new(dataset),
        AgentConfig::with_decoder(gameboy_decoder_config_with_macros(&[])),
    )
    .expect("the default agent builds");
    assert_eq!(agent.network.version, gameboy::KERNEL_VERSION);
    assert_eq!(
        agent.network.plasticity.version,
        gameboy::PLASTICITY_VERSION
    );
    assert_eq!(
        (u64::from(agent.frame.width), u64::from(agent.frame.height)),
        (gameboy::VIEW_WIDTH, gameboy::VIEW_HEIGHT)
    );
}

#[test]
fn the_profile_clock_warmup_and_joypad_are_the_service_defaults() {
    assert_eq!(DEFAULT_WARMUP_MS, gameboy::WARMUP_MS);
    // The legacy f64 frame period is exactly the rational step duration, in ms.
    let step = gameboy::step_duration();
    let ms = RationalNs::reduced(548_625 * 1_000_000, 32_768).expect("ns");
    assert_eq!(step, ms);
    assert_eq!(GAMEBOY_MS_PER_FRAME, 548_625.0 / 32_768.0);
    let order: Vec<&str> = GAMEBOY_BUTTON_BITS.iter().map(|(name, _)| *name).collect();
    assert_eq!(order, gameboy::GAMEBOY_BUTTONS);
    for (index, (_, bit)) in GAMEBOY_BUTTON_BITS.iter().enumerate() {
        assert_eq!(*bit, 1 << index, "bit i is GAMEBOY_BUTTONS[i]");
    }
}

fn group_form(group: Option<&ExclusiveGroup>) -> Value {
    match group {
        None => Value::Null,
        Some(g) => json!({
            "channels": g.channels.iter()
                .map(|(channel, role)| json!({"channel": channel, "role": role}))
                .collect::<Vec<_>>(),
            "decisionMs": g.decision_ms,
            "holdMs": g.hold_ms,
            "hysteresis": g.hysteresis,
            "fatigueGain": g.fatigue_gain,
            "fatigueDecay": g.fatigue_decay,
            "blockedFatigue": g.blocked_fatigue,
            "blockedMs": g.blocked_ms,
        }),
    }
}

/// The canonical decoder-configuration form of legacy-gameboy-v1 section 12, built from the
/// Rust twin. `@flybrain/session-types` builds the same value from the TypeScript oracle's
/// preset (`decoderConfigForm`), and both are held to `fixtures/gameboy-decoder-config.json`.
fn decoder_config_form(config: &DecoderConfig) -> Value {
    json!({
        "form": "gameboy-decoder-config-v1",
        "exclusive": group_form(config.exclusive.as_ref()),
        "macros": group_form(config.macros.as_ref()),
        "pulses": config.pulses.iter().map(|p| json!({
            "channel": p.channel,
            "role": p.role,
            "holdMs": p.hold_ms,
            "cooldownMs": p.cooldown_ms,
            "threshold": p.threshold,
            "boot": p.boot.map_or(Value::Null, |b| json!({"cooldownMs": b.cooldown_ms, "threshold": b.threshold})),
            "throttleGroup": p.throttle_group.clone().map_or(Value::Null, Value::String),
        })).collect::<Vec<_>>(),
        "clearLockoutMs": config.clear_lockout_ms,
    })
}

/// The shared `decoderConfigDigest` vectors: raw mode and the Pokemon Red macro group, from
/// `gameboy_decoder_config_with_macros`. `FLY_UPDATE_FIXTURES=1` rewrites the file; otherwise
/// the checked-in values must be exactly what this build computes.
#[test]
fn the_decoder_config_digest_vectors_are_what_the_rust_preset_computes() {
    let pokered: Vec<&str> = flybrain_gb::macro_channels("pokemon-red");
    let cases: Vec<Value> = [("raw", Vec::new()), ("macros", pokered)]
        .into_iter()
        .map(|(name, channels)| {
            let form = decoder_config_form(&gameboy_decoder_config_with_macros(&channels));
            json!({
                "name": name,
                "macroChannels": channels,
                "form": form,
                "canonical": canonical::canonicalize(&form).expect("canonical"),
                "digest": canonical::digest_of(&form).expect("digest"),
            })
        })
        .collect();
    let file = json!({
        "description": "decoderConfigDigest vectors (legacy-gameboy-v1 section 12): the canonical form of gameboy_decoder_config_with_macros / gameboyDecoderConfig for raw mode and the Pokemon Red macro group. Written by FLY_UPDATE_FIXTURES=1 cargo test -p flysim --test legacy_profile_identity; both languages must reproduce every form and digest.",
        "cases": cases,
    });
    let mut text = serde_json::to_string_pretty(&file).expect("json");
    text.push('\n');
    let path = fixtures::dir().join("gameboy-decoder-config.json");
    if std::env::var_os("FLY_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &text).expect("write the fixture");
    }
    let found = std::fs::read_to_string(&path).expect("the checked-in fixture");
    assert_eq!(
        found, text,
        "gameboy-decoder-config.json is stale; rerun with FLY_UPDATE_FIXTURES=1"
    );
    // Channel order is identity: reversing the macro group moves the digest.
    let mut reversed: Vec<&str> = flybrain_gb::macro_channels("pokemon-red");
    reversed.reverse();
    let other = canonical::digest_of(&decoder_config_form(&gameboy_decoder_config_with_macros(
        &reversed,
    )))
    .expect("digest");
    assert_ne!(Some(other.as_str()), cases[1]["digest"].as_str());
}
