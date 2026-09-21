//! Restore ordering and fallback, over a real pair of checkpoint directories.
//!
//! `docs/design/flysim.md` section 8: the candidates are the tmpfs hot copy, then the durable
//! latest, then previous, then the milestone archives; each is checked for magic, CRC32, schema,
//! ROM hash and the exact compatibility string before anything is imported; a fallback is
//! reported; and **if every candidate fails the service exits non-zero** rather than starting a
//! fresh run over a store it could not read.
//!
//! This exercises the store and the candidate walk. The sim-level restore — agent import,
//! emulator state, adapter state, ratchet — needs a ROM and the dataset and lives in
//! `tests/integration.rs`.

mod common;

use std::path::Path;

use flysim::store::{self, Candidate, RuntimeState, Store};

const ROM: &str = "0e853043e15e0f7dd150f297be5557be9890884bff3df1e9bbd7b3e1caef219f";
const COMPATIBILITY: &str = "lif-1ms-f64-v2/pokered-unique8-v3/fingerprint/fly-kc-mbon-rstdp-v2";

fn agent_state() -> flybrain_core::agent::AgentState {
    use flybrain_core::decoder::DecoderState;
    use flybrain_core::lif::LifState;
    use flybrain_core::ordered::NumberMap;
    use flybrain_core::plasticity::PlasticityState;
    flybrain_core::agent::AgentState {
        version: 1,
        remainder: 0.25,
        warmed_up: true,
        network: LifState {
            membrane: vec![0.0; 8],
            refractory: vec![0; 8],
            last_spike_ms: vec![-1_000_000.0; 8],
            visual_drive: vec![0.0; 4],
            rng: 22_222,
            reward_remaining: 0.0,
            ms: 1_000.0,
            population_rate: 0.0,
            rates: NumberMap::from_pairs([("forward", 0.0)]),
            plasticity: PlasticityState {
                version: "fly-kc-mbon-rstdp-v2".to_string(),
                topology: 1,
                enabled: true,
                updates: 0.0,
                signal: 0.0,
                gains: vec![1.0; 4],
                traces: vec![0.0; 4],
                touched: vec![0.0; 4],
            },
        },
        decoder: DecoderState {
            version: 4,
            calibrated: true,
            baseline: NumberMap::from_pairs([("forward", 1.0)]),
            held_until: NumberMap::new(),
            next_allowed: NumberMap::new(),
            next_decision: 0.0,
            current: None,
            fatigue: NumberMap::new(),
            macro_next_decision: 0.0,
            macro_current: None,
            macro_fatigue: NumberMap::new(),
        },
    }
}

fn checkpoint(generation: u64, frame: u64, rom: &str, compatibility: &str) -> Vec<u8> {
    let runtime = RuntimeState {
        generation,
        wall_ms: 1_757_000_000_000 + generation,
        rom_sha256: rom.to_string(),
        emulator_frame: frame,
        compatibility: compatibility.to_string(),
        speed: 1.0,
        buttons: 0,
        rank_since_ms: 0.0,
        last_event_id: generation * 10,
        reward: serde_json::json!({ "version": 3 }),
        ratchet: flybrain_gb::RatchetState::default(),
        emulator: vec![1; 32],
        framebuffer: vec![2; 64],
        ratchet_game: Vec::new(),
        ratchet_frame: Vec::new(),
    };
    store::encode(&agent_state(), &runtime).unwrap()
}

/// The candidate walk `Sim::restore_or_warm_up` performs: take the first candidate that decodes
/// **and** matches this build, and report whether that meant falling back.
fn restore(hot: &Store, durable: &Store) -> Result<(Candidate, RuntimeState, bool), usize> {
    let candidates = store::restore_order(hot, durable);
    let total = candidates.len();
    for (index, candidate) in candidates.into_iter().enumerate() {
        let Ok(loaded) = store::load(&candidate.path) else {
            continue;
        };
        if loaded.runtime.rom_sha256 != ROM || loaded.runtime.compatibility != COMPATIBILITY {
            continue;
        }
        return Ok((candidate, loaded.runtime, index > 0));
    }
    Err(total)
}

fn stores(root: &Path) -> (Store, Store) {
    let hot = Store::new(root.join("hot"), 2);
    let durable = Store::new(root.join("durable"), 2);
    hot.create().unwrap();
    durable.create().unwrap();
    (hot, durable)
}

#[test]
fn the_hot_copy_is_preferred_because_it_is_the_newest_state_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(3, &checkpoint(3, 300, ROM, COMPATIBILITY), None).unwrap();
    hot.commit(9, &checkpoint(9, 900, ROM, COMPATIBILITY), None).unwrap();

    let (candidate, runtime, fell_back) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "hot latest generation 9");
    assert_eq!(runtime.emulator_frame, 900);
    assert!(!fell_back);
}

#[test]
fn a_corrupt_candidate_is_skipped_and_the_fallback_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(1, &checkpoint(1, 100, ROM, COMPATIBILITY), None).unwrap();
    durable.commit(2, &checkpoint(2, 200, ROM, COMPATIBILITY), None).unwrap();

    // A single flipped bit in the newest durable generation: the CRC32 catches it.
    let path = durable.generation_path(2);
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x80;
    std::fs::write(&path, &bytes).unwrap();

    let (candidate, runtime, fell_back) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "durable previous generation 1");
    assert_eq!(runtime.emulator_frame, 100);
    assert!(fell_back, "a fallback has to be visible in /status and the log");
}

#[test]
fn a_truncated_hot_copy_falls_through_to_the_durable_store() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(4, &checkpoint(4, 400, ROM, COMPATIBILITY), None).unwrap();
    let full = checkpoint(11, 1_100, ROM, COMPATIBILITY);
    hot.commit(11, &full, None).unwrap();
    // A host that lost power mid-write cannot produce this (the rename is atomic), but a tmpfs
    // that filled up can.
    std::fs::write(hot.generation_path(11), &full[..full.len() / 2]).unwrap();

    let (candidate, runtime, fell_back) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "durable latest generation 4");
    assert_eq!(runtime.emulator_frame, 400);
    assert!(fell_back);
}

#[test]
fn a_checkpoint_from_another_cartridge_or_another_build_is_refused_not_imported() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(1, &checkpoint(1, 100, ROM, COMPATIBILITY), None).unwrap();
    // Newer, perfectly valid, and for a different cartridge.
    durable.commit(2, &checkpoint(2, 200, &"ff".repeat(32), COMPATIBILITY), None).unwrap();
    let (candidate, _, fell_back) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "durable previous generation 1");
    assert!(fell_back);

    // The statefmt segment differs between an Emscripten and a native build, which is exactly
    // the mismatch the extra compatibility segment exists to make honest.
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    let other = format!("{COMPATIBILITY}/statefmt:199608-wasm32-unknown-emscripten");
    durable.commit(1, &checkpoint(1, 100, ROM, &other), None).unwrap();
    assert_eq!(restore(&hot, &durable).unwrap_err(), 1, "nothing loadable");
}

#[test]
fn the_milestone_archive_is_the_last_resort_and_survives_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(1, &checkpoint(1, 100, ROM, COMPATIBILITY), Some(5)).unwrap();
    for generation in 2..=8 {
        durable
            .commit(generation, &checkpoint(generation, generation * 100, ROM, COMPATIBILITY), None)
            .unwrap();
    }
    // Wreck everything the manifest points at except the archives.
    for generation in [8, 7] {
        std::fs::write(durable.generation_path(generation), b"rubbish").unwrap();
    }
    let (candidate, runtime, fell_back) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "durable archived generation 1 (rank 5)");
    assert_eq!(runtime.emulator_frame, 100);
    assert!(fell_back);

    // And the independent `milestone-<rank>.checkpoint` copy is still there behind it.
    std::fs::write(durable.generation_path(1), b"rubbish too").unwrap();
    let (candidate, runtime, _) = restore(&hot, &durable).unwrap();
    assert_eq!(candidate.origin, "durable milestone archive rank 5");
    assert_eq!(runtime.emulator_frame, 100);
}

#[test]
fn an_empty_pair_of_stores_is_a_fresh_start_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    assert_eq!(
        store::restore_order(&hot, &durable).len(),
        0,
        "no candidates means warm up, which is a different branch from every candidate failing"
    );
    assert_eq!(restore(&hot, &durable).unwrap_err(), 0);
}

#[test]
fn a_generation_number_is_never_reused_across_the_two_stores() {
    let dir = tempfile::tempdir().unwrap();
    let (hot, durable) = stores(dir.path());
    durable.commit(3, &checkpoint(3, 300, ROM, COMPATIBILITY), None).unwrap();
    hot.commit(9, &checkpoint(9, 900, ROM, COMPATIBILITY), None).unwrap();
    // What `Sim::boot` computes for the next generation.
    let next = durable.highest_generation().max(hot.highest_generation()) + 1;
    assert_eq!(next, 10);
}
