//! The legacy profile `gameboy-legacy-fafb-v783-v1` pins identities this service computes.
//! `fly-session-types` records them as constants (`legacy-gameboy-v1` section 2); this test is
//! where they are recomputed from the committed dataset and the service's own defaults, so the
//! profile cannot silently describe another fly.

mod common;

use std::sync::Arc;

use fly_session_types::gameboy;
use fly_session_types::scalar::RationalNs;
use flybrain_core::agent::{AgentConfig, DEFAULT_WARMUP_MS, GAMEBOY_MS_PER_FRAME, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::{GAMEBOY_BUTTON_BITS, gameboy_decoder_config_with_macros};

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
