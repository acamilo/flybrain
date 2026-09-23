//! A `v6` checkpoint restored under `v7`: accepted with the opt-in, refused without it.
//!
//! The unit tests in `flybrain-gb` cover the decision function and the adapter's own state
//! migration separately. This is the two of them against one artefact: a real `FLYSIM01`
//! envelope carrying a `pokered-unique8-v6` compatibility string and a `v6` reward ledger —
//! written, encoded, decoded, and then put through exactly what `Sim::try_restore` puts a
//! candidate through, and then one sample of a game in which items were already taken.
//!
//! No ROM and no dataset, deliberately. Building a `Sim` would need both, and neither is part of
//! the question: what decides a restore is the compatibility string and `import_state`, and what
//! decides the seed is the first sample's read of the cartridge's own item bits.
//!
//! (`v5` -> `v6`, the catch rule's migration, was this same file; `v6` no longer migrates from
//! anything, so that pair is refused now like any other.)

use flybrain_gb::GameAdapter;
use flybrain_gb::MemoryReader;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::compatibility::{RestoreDecision, accepted_adapters, decide};
use flybrain_gb::pokemon_red::PokemonRedReward;
use flysim::store::{self, RuntimeState};

/// The live string's shape, with the adapter left open. The dataset fingerprint is shortened —
/// nothing here parses it, and a seven-digest one would be 455 characters of noise.
fn compatibility(adapter: &str) -> String {
    format!(
        "lif-1ms-f64-v2/{adapter}/aabbccddeeff00/fly-kc-mbon-rstdp-v2/\
         binjgb:c60e138da5a795ebb55e56b11b7e90024e41112c/\
         pokered:0cd19d3b877b7dc66d12c7050bed9a7f38154d4b/statefmt:199616-x86_64-unknown-linux-gnu"
    )
}

/// A `v6` reward ledger: `STATE_VERSION` 4, every field `v6` wrote, and **no** `talk:`,
/// `item:`, `hidden:` or `items:seeded` key in `seen`.
///
/// Written out by hand rather than exported from an adapter, because an exported one would be a
/// `v7` state with keys deleted — this is the shape the release box's checkpoints really carry,
/// field for field, including a `boundary:` key earned indoors (Red's staircase, map 38) that
/// `v7` would not have paid for and keeps anyway.
fn v6_reward() -> serde_json::Value {
    serde_json::json!({
        "version": 4,
        "seen": [
            "adventure", "map:0", "early:outside", "dex:3", "boundary:0:edge:n:near",
            "boundary:38:1:7:on"
        ],
        "tiles": ["0:5:6", "0:5:7"],
        "tileCounts": { "0": 2 },
        "wildWins": { "0:112:4": 2 },
        "catchCounts": { "176": 1 },
        "replayBlocked": [],
        "counts": {
            "milestone": 2, "exploration": 0, "map": 1, "species": 1,
            "trainer": 0, "battle": 2, "badge": 0, "boundary": 2, "catch": 1
        },
        "total": 2.45,
        "recent": [{ "kind": "species", "label": "OWNED #4", "brainMs": 1234.5, "value": 0.5 }],
        "last": { "species": { "kind": "species", "label": "OWNED #4", "brainMs": 1234.5, "value": 0.5 } },
        "initialized": true,
        "sawBoot": true,
        "location": "0:5:7",
        "stable": 9,
        "progress": 3,
        "badges": 0,
        "battle": null,
        "mode": "OVERWORLD"
    })
}

fn v6_checkpoint() -> Vec<u8> {
    use flybrain_core::decoder::DecoderState;
    use flybrain_core::lif::LifState;
    use flybrain_core::ordered::NumberMap;
    use flybrain_core::plasticity::PlasticityState;

    let agent = flybrain_core::agent::AgentState {
        version: 1,
        remainder: 0.25,
        warmed_up: true,
        network: LifState {
            membrane: vec![0.1, -0.2],
            refractory: vec![0, 1],
            last_spike_ms: vec![-1_000_000.0, 5.0],
            visual_drive: vec![0.3],
            rng: 42,
            reward_remaining: 0.0,
            ms: 1_234.5,
            population_rate: 1.0,
            rates: NumberMap::from_pairs([("forward", 1.0)]),
            plasticity: PlasticityState {
                version: "fly-kc-mbon-rstdp-v2".to_string(),
                topology: 7,
                enabled: true,
                updates: 1.0,
                signal: 0.0,
                gains: vec![1.0],
                traces: vec![0.0],
                touched: vec![0.0],
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
    };
    let runtime = RuntimeState {
        generation: 41,
        wall_ms: 1_790_000_000_000,
        rom_sha256: flybrain_gb::pokemon_red::SUPPORTED_ROM.to_string(),
        emulator_frame: 1_000_000,
        compatibility: compatibility("pokered-unique8-v6"),
        speed: 1.0,
        buttons: 0,
        rank_since_ms: 1_000.0,
        last_event_id: 4_242,
        reward: v6_reward(),
        ratchet: flybrain_gb::RatchetState { best: 3, attempts: 1, recoveries: 4, ..Default::default() },
        emulator: vec![3; 64],
        framebuffer: vec![0; 32],
        ratchet_game: vec![1],
        ratchet_frame: vec![2],
    };
    store::encode(&agent, &runtime).expect("the fixture encodes")
}

#[test]
fn a_v6_checkpoint_is_refused_under_v7_without_the_opt_in() {
    let checkpoint = store::decode(&v6_checkpoint()).expect("the fixture decodes");
    let adapter = PokemonRedReward::new();
    let current = compatibility(adapter.id());
    assert_ne!(checkpoint.runtime.compatibility, current, "v7 is not v6");

    for opt_in in [None, Some(""), Some("pokered-unique8-v5"), Some("some-other-adapter")] {
        assert!(
            matches!(
                decide(
                    &checkpoint.runtime.compatibility,
                    &current,
                    adapter.migrates_from(),
                    &accepted_adapters(opt_in),
                ),
                RestoreDecision::Refuse(_)
            ),
            "FLY_ACCEPT_ADAPTERS={opt_in:?} must not migrate anything"
        );
    }
}

/// A flat 64 KiB address space: the one thing a sample reads.
struct Wram(Vec<u8>);

impl MemoryReader for Wram {
    fn read8(&mut self, address: u16) -> u8 {
        self.0[address as usize]
    }
}

#[test]
fn a_v6_checkpoint_restores_under_v7_with_the_new_ledgers_empty_and_the_items_seeded() {
    let checkpoint = store::decode(&v6_checkpoint()).expect("the fixture decodes");
    let mut adapter = PokemonRedReward::new();
    let current = compatibility(adapter.id());

    assert_eq!(
        decide(
            &checkpoint.runtime.compatibility,
            &current,
            adapter.migrates_from(),
            &accepted_adapters(Some("pokered-unique8-v6")),
        ),
        RestoreDecision::MigrateAdapter { from: "pokered-unique8-v6".to_string() }
    );

    // The migration itself: `import_state`, exactly as `Sim::try_restore` calls it.
    adapter.import_state(&checkpoint.runtime.reward).expect("a v6 ledger is a valid v7 ledger");

    let after = adapter.export_state();
    assert_eq!(after["counts"]["talk"], serde_json::json!(0), "no conversation was ever paid");
    assert_eq!(after["counts"]["item"], serde_json::json!(0), "nor any item");

    // And nothing else moved: every field the v6 state carried round-trips to the same value,
    // and v7 adds no field at all -- its ledgers are keys in `seen`.
    //
    // `counts` is the one field that is not byte-identical, and it is not a change of meaning:
    // it serializes every kind in the catalog, so a v7 state lists `talk` and `item` where a v6
    // state had nothing to list. Every kind the v6 state did carry keeps its number.
    let before = v6_reward();
    for (key, value) in before.as_object().unwrap() {
        if key == "counts" {
            for (kind, count) in value.as_object().unwrap() {
                assert_eq!(&after["counts"][kind], count, "counts.{kind}");
            }
            let added: Vec<&String> = after["counts"]
                .as_object()
                .unwrap()
                .keys()
                .filter(|kind| !value.as_object().unwrap().contains_key(*kind))
                .collect();
            assert_eq!(added, vec!["talk", "item"], "v7 counts two more kinds and no others");
            continue;
        }
        assert_eq!(&after[key], value, "{key} must survive the migration byte for byte");
    }
    let added: Vec<&String> = after
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| !before.as_object().unwrap().contains_key(*key))
        .collect();
    assert!(added.is_empty(), "v7 adds no field: {added:?}");

    // The first sample of the restored game. Under v6 the fly took an item ball (global
    // toggleable index 0x2a) and a hidden item (index 9); v6 paid for neither. The sample seeds
    // both into the ledger and pays nothing, which is the "no retroactive payout" half.
    let mut wram = Wram(vec![0; 0x10000]);
    wram.0[ram::wStatusFlags6 as usize] = 1;
    wram.0[ram::wPartyCount as usize] = 1;
    wram.0[ram::wCurMapWidth as usize] = 10;
    wram.0[ram::wCurMapHeight as usize] = 9;
    wram.0[ram::wXCoord as usize] = 5;
    wram.0[ram::wYCoord as usize] = 7;
    wram.0[(ram::wToggleableObjectFlags + 0x2a / 8) as usize] |= 1 << (0x2a % 8);
    wram.0[(ram::wObtainedHiddenItemsFlags + 1) as usize] |= 1 << 1;
    assert!(adapter.sample(&mut wram, 2_000.0).is_empty(), "the seed pays nothing");
    let seen = adapter.export_state()["seen"].clone();
    for key in ["item:42", "hidden:9", "items:seeded"] {
        assert!(seen.as_array().unwrap().contains(&serde_json::json!(key)), "{key} seeded");
    }
    assert!(
        !seen.as_array().unwrap().iter().any(|key| key.as_str().unwrap().starts_with("talk:")),
        "the talk ledger starts empty"
    );

    // The rest of what a restore reads is untouched by the migration.
    assert_eq!(adapter.progress().rank, 3);
    assert_eq!(checkpoint.runtime.last_event_id, 4_242);
    assert_eq!(checkpoint.runtime.ratchet.best, 3);
}

#[test]
fn nothing_but_the_adapter_segment_may_differ_for_the_migration_to_apply() {
    let adapter = PokemonRedReward::new();
    let accepted = accepted_adapters(Some("pokered-unique8-v6"));
    let current = compatibility(adapter.id());

    // A v6 string whose state format also moved: a different build, not a rule change.
    let other_abi = compatibility("pokered-unique8-v6").replace("199616", "199617");
    assert!(matches!(
        decide(&other_abi, &current, adapter.migrates_from(), &accepted),
        RestoreDecision::Refuse(_)
    ));

    // And an identical string needs no opt-in at all.
    assert_eq!(
        decide(&current, &current, adapter.migrates_from(), &[]),
        RestoreDecision::Exact
    );

    // And v5 -> v7 is not a migration this adapter wrote, whatever the operator names.
    assert!(matches!(
        decide(
            &compatibility("pokered-unique8-v5"),
            &current,
            adapter.migrates_from(),
            &accepted_adapters(Some("pokered-unique8-v5,pokered-unique8-v6")),
        ),
        RestoreDecision::Refuse(_)
    ));
}
