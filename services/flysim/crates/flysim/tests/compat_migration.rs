//! A `v7` (and a `v6`) checkpoint restored under `v8`: accepted with the opt-in, refused without
//! it.
//!
//! The unit tests in `flybrain-gb` cover the decision function and the adapter's own state
//! migration separately. This is the two of them against one artefact: a real `FLYSIM01`
//! envelope carrying a `pokered-unique8-v7` (or `-v6`) compatibility string and that version's
//! reward ledger — written, encoded, decoded, and then put through exactly what
//! `Sim::try_restore` puts a candidate through, and then samples of a game: one in which items
//! were already taken (`v6`), one in the middle of the row 67 battle (`v7`).
//!
//! No ROM and no dataset, deliberately. Building a `Sim` would need both, and neither is part of
//! the question: what decides a restore is the compatibility string and `import_state`, and what
//! decides the seed is the first sample's read of the cartridge's own item bits.
//!
//! (`v5` -> `v6`, the catch rule's migration, was this same file, and `v6` -> `v7` the engagement
//! rules'. `v8` reads both `v7` and `v6`: the chain composes, and `v5` stays refused.)

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

/// A `v7` reward ledger: the `v6` one after the engagement rules' first sample (the item seed and
/// a conversation), with a battle in flight -- the row 67 Jr. Trainer's Diglett at 19 of 31 --
/// and **no** `damageCounts`, `counts.damage` or `battle.damage`.
fn v7_reward() -> serde_json::Value {
    let mut reward = v6_reward();
    let seen = reward["seen"].as_array_mut().unwrap();
    for key in ["items:seeded", "item:42", "talk:54:sprite:1"] {
        seen.push(serde_json::json!(key));
    }
    reward["counts"]["talk"] = serde_json::json!(1);
    reward["counts"]["item"] = serde_json::json!(0);
    reward["mode"] = serde_json::json!("BATTLE");
    reward["battle"] = serde_json::json!({
        "key": "54:59:11", "wild": false, "sawLiving": true, "ko": false,
        "speciesAtStart": 1, "captured": null, "capturedNew": false
    });
    reward
}

fn v6_checkpoint() -> Vec<u8> {
    checkpoint_of("pokered-unique8-v6", v6_reward())
}

fn v7_checkpoint() -> Vec<u8> {
    checkpoint_of("pokered-unique8-v7", v7_reward())
}

fn checkpoint_of(adapter: &str, reward: serde_json::Value) -> Vec<u8> {
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
        reinforcements: None,
        generation: 41,
        wall_ms: 1_790_000_000_000,
        rom_sha256: flybrain_gb::pokemon_red::SUPPORTED_ROM.to_string(),
        emulator_frame: 1_000_000,
        compatibility: compatibility(adapter),
        speed: 1.0,
        buttons: 0,
        rank_since_ms: 1_000.0,
        last_event_id: 4_242,
        reward,
        ratchet: flybrain_gb::RatchetState { best: 3, attempts: 1, recoveries: 4, ..Default::default() },
        emulator: vec![3; 64],
        framebuffer: vec![0; 32],
        ratchet_game: vec![1],
        ratchet_frame: vec![2],
    };
    store::encode(&agent, &runtime).expect("the fixture encodes")
}

#[test]
fn a_v7_or_v6_checkpoint_is_refused_under_v8_without_its_own_opt_in() {
    let adapter = PokemonRedReward::new();
    let current = compatibility(adapter.id());
    for (bytes, own) in
        [(v7_checkpoint(), "pokered-unique8-v7"), (v6_checkpoint(), "pokered-unique8-v6")]
    {
        let checkpoint = store::decode(&bytes).expect("the fixture decodes");
        assert_ne!(checkpoint.runtime.compatibility, current, "v8 is not {own}");
        let other = if own.ends_with("v7") { "pokered-unique8-v6" } else { "pokered-unique8-v7" };
        for opt_in in
            [None, Some(""), Some("pokered-unique8-v5"), Some("some-other-adapter"), Some(other)]
        {
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
                "{own} with FLY_ACCEPT_ADAPTERS={opt_in:?} must not migrate"
            );
        }
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
fn a_v6_checkpoint_restores_under_v8_with_the_new_ledgers_empty_and_the_items_seeded() {
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
    adapter.import_state(&checkpoint.runtime.reward).expect("a v6 ledger is a valid v8 ledger");

    let after = adapter.export_state();
    assert_eq!(after["counts"]["talk"], serde_json::json!(0), "no conversation was ever paid");
    assert_eq!(after["counts"]["item"], serde_json::json!(0), "nor any item");
    assert_eq!(after["counts"]["damage"], serde_json::json!(0), "nor any damage");
    assert_eq!(after["damageCounts"], serde_json::json!({}));

    // And nothing else moved: every field the v6 state carried round-trips to the same value;
    // v7 added no field (its ledgers are keys in `seen`) and v8 adds one, `damageCounts`.
    //
    // `counts` is the one field that is not byte-identical, and it is not a change of meaning:
    // it serializes every kind in the catalog, so a v8 state lists `talk`, `item` and `damage`
    // where a v6 state had nothing to list. Every kind the v6 state did carry keeps its number.
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
            assert_eq!(added, vec!["talk", "item", "damage"], "three more kinds and no others");
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
    assert_eq!(added, vec!["damageCounts"], "v8 adds one field");

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
    let accepted = accepted_adapters(Some("pokered-unique8-v7"));
    let current = compatibility(adapter.id());

    // A v7 string whose state format also moved: a different build, not a rule change.
    let other_abi = compatibility("pokered-unique8-v7").replace("199616", "199617");
    assert!(matches!(
        decide(&other_abi, &current, adapter.migrates_from(), &accepted),
        RestoreDecision::Refuse(_)
    ));

    // And an identical string needs no opt-in at all.
    assert_eq!(
        decide(&current, &current, adapter.migrates_from(), &[]),
        RestoreDecision::Exact
    );

    // And v5 -> v8 is not a migration this adapter wrote, whatever the operator names.
    assert!(matches!(
        decide(
            &compatibility("pokered-unique8-v5"),
            &current,
            adapter.migrates_from(),
            &accepted_adapters(Some("pokered-unique8-v5,pokered-unique8-v6,pokered-unique8-v7")),
        ),
        RestoreDecision::Refuse(_)
    ));
}

/// The enemy battle struct and the turn, as `LoadEnemyMonData` and `ExecutePlayerMove` leave them.
fn battle_wram(hp: u8, fly_turn: bool) -> Wram {
    let mut wram = Wram(vec![0; 0x10000]);
    wram.0[ram::wStatusFlags6 as usize] = 1;
    wram.0[ram::wPartyCount as usize] = 1;
    wram.0[ram::wCurMap as usize] = 54;
    wram.0[ram::wCurMapWidth as usize] = 5;
    wram.0[ram::wCurMapHeight as usize] = 7;
    wram.0[ram::wXCoord as usize] = 4;
    wram.0[ram::wYCoord as usize] = 10;
    wram.0[ram::wIsInBattle as usize] = 2;
    wram.0[ram::wEnemyMonPartyPos as usize] = 0;
    wram.0[ram::wEnemyMonSpecies as usize] = 59;
    wram.0[ram::wEnemyMonLevel as usize] = 11;
    wram.0[(ram::wEnemyMonHP + 1) as usize] = hp;
    wram.0[(ram::wEnemyMonMaxHP + 1) as usize] = 31;
    wram.0[flybrain_gb::pokemon_red::state::poke::H_WHOSE_TURN as usize] = u8::from(!fly_turn);
    wram.0[ram::wPlayerMoveNum as usize] = 0x91;
    wram
}

#[test]
fn a_v7_checkpoint_taken_mid_battle_restores_under_v8_and_pays_only_what_follows() {
    let checkpoint = store::decode(&v7_checkpoint()).expect("the fixture decodes");
    let mut adapter = PokemonRedReward::new();
    let current = compatibility(adapter.id());
    assert_eq!(
        decide(
            &checkpoint.runtime.compatibility,
            &current,
            adapter.migrates_from(),
            &accepted_adapters(Some("pokered-unique8-v7")),
        ),
        RestoreDecision::MigrateAdapter { from: "pokered-unique8-v7".to_string() }
    );
    adapter.import_state(&checkpoint.runtime.reward).expect("a v7 ledger is a valid v8 ledger");

    // Every field v7 carried survives, and v8 adds `damageCounts` and the battle's `damage`.
    let before = v7_reward();
    let after = adapter.export_state();
    for (key, value) in before.as_object().unwrap() {
        match key.as_str() {
            "counts" => {
                for (kind, count) in value.as_object().unwrap() {
                    assert_eq!(&after["counts"][kind], count, "counts.{kind}");
                }
                assert_eq!(after["counts"]["damage"], serde_json::json!(0));
            }
            "battle" => {
                for (field, v) in value.as_object().unwrap() {
                    assert_eq!(&after["battle"][field], v, "battle.{field}");
                }
                assert_eq!(after["battle"]["damage"]["restored"], serde_json::json!(true));
            }
            _ => assert_eq!(&after[key], value, "{key} must survive the migration byte for byte"),
        }
    }
    assert_eq!(after["damageCounts"], serde_json::json!({}));

    // The restored battle: Diglett stands at 19 of 31. The 12 HP it lost under v7 are not paid;
    // its own turn's poison tick is not paid; BUBBLE's next hit is.
    assert!(adapter.sample(&mut battle_wram(19, true), 2_000.0).is_empty(), "no back pay");
    assert!(adapter.sample(&mut battle_wram(17, false), 2_017.0).is_empty(), "not its turn");
    let hit = adapter.sample(&mut battle_wram(8, true), 2_034.0);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].kind, "damage");
    assert_eq!(hit[0].label, "HIT #59 FOR 9 HP");
    assert!((hit[0].value - 0.20 * 9.0 / 31.0).abs() < 1e-12);
    assert_eq!(adapter.progress().rank, 3, "the rank is untouched");
}
