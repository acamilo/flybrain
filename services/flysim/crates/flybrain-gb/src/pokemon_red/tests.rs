//! Port of the prototype's `tests/unit/reward.test.ts`.
//!
//! Every test uses a synthetic WRAM trace, as the prototype's did: the traces
//! are grounded in the pret/pokered disassembly (see `docs/rewards-learning.md`)
//! rather than in a running game.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::symbols::events;
use super::*;

/// A flat 64 KiB address space plus a per-address read counter, implementing the
/// same interface [`crate::Emulator`]'s frame cache does.
struct FakeMemory {
    bytes: Vec<u8>,
    reads: BTreeMap<u16, u32>,
}

impl FakeMemory {
    fn new() -> Self {
        Self { bytes: vec![0; 65536], reads: BTreeMap::new() }
    }
}

impl MemoryReader for FakeMemory {
    fn read8(&mut self, address: u16) -> u8 {
        *self.reads.entry(address).or_insert(0) += 1;
        self.bytes[address as usize]
    }
}

struct Fixture {
    memory: FakeMemory,
    reward: PokemonRedReward,
    ms: f64,
}

impl Fixture {
    fn new() -> Self {
        let mut memory = FakeMemory::new();
        memory.set(ram::wStatusFlags6, 1);
        memory.set(ram::wPartyCount, 1);
        memory.set(ram::wCurMapWidth, 20);
        memory.set(ram::wCurMapHeight, 20);
        Self { memory, reward: PokemonRedReward::new(), ms: 0.0 }
    }

    /// A fixture whose first playable sample is in Red's bedroom, which is where a
    /// real boot lands.
    ///
    /// This matters for the ladder: the first sample baselines the current map into
    /// the lifetime ledger, so a fixture left on the default map 0 starts already
    /// credited with Pallet Town (rung 3). That is the right behaviour for a save
    /// that really is in Pallet Town, and it is what gives the ROM-gated boot test
    /// rank 1 in the bedroom, but a test about a later rung should not inherit it.
    fn booted() -> Self {
        let mut f = Self::new();
        f.memory.set(ram::wCurMap, maps::REDS_HOUSE_2F);
        f.sample();
        f
    }

    fn sample(&mut self) -> Vec<RewardEvent> {
        self.memory.reads.clear();
        self.ms += 17.0;
        let ms = self.ms;
        self.reward.sample(&mut self.memory, ms)
    }

    /// Three samples at one coordinate, which is what the stability gate needs.
    fn visit(&mut self, x: u8, y: u8) -> Vec<RewardEvent> {
        self.memory.set(ram::wXCoord, x);
        self.memory.set(ram::wYCoord, y);
        let mut events = self.sample();
        events.extend(self.sample());
        events.extend(self.sample());
        events
    }

    fn visit_x(&mut self, x: u8) -> Vec<RewardEvent> {
        self.visit(x, 0)
    }

    /// One wild battle that ends in a ball keeping the Pokémon, byte for byte as the
    /// cartridge writes it at the pinned commit.
    ///
    /// `InitBattleVariables` clears `wBattleResult`; `ItemUseBall`'s capture branch sets the
    /// Pokédex bit (for a species the player did not already own) and writes
    /// `wEnemyMonSpecies` into `wCapturedMonSpecies`; `UseBagItem`'s
    /// `.returnAfterCapturingMon` then zeroes that byte, sets `wBattleResult` to 2 and leaves
    /// the battle. `dex` is the Pokédex *number* minus one, i.e. the bit index, and `None` is a
    /// species this run already owns.
    fn catch(&mut self, species: u8, dex: Option<u16>) -> Vec<RewardEvent> {
        self.memory.set(ram::wBattleResult, 0);
        self.memory.set(ram::wIsInBattle, 1);
        self.memory.set(ram::wEnemyMonSpecies, species);
        self.memory.set(ram::wEnemyMonHP + 1, 10);
        self.memory.set(ram::wEnemyMonMaxHP + 1, 10);
        let mut events = self.sample();
        if let Some(index) = dex {
            self.memory.or(ram::wPokedexOwned + (index >> 3), 1 << (index & 7));
        }
        self.memory.set(ram::wCapturedMonSpecies, species);
        events.extend(self.sample());
        self.memory.set(ram::wCapturedMonSpecies, 0);
        self.memory.set(ram::wBattleResult, 2);
        self.memory.set(ram::wIsInBattle, 0);
        events.extend(self.sample());
        events
    }

    /// Write a warp table: `wNumberOfWarps` plus one four-byte `Y, X, warp id, map id` entry per
    /// `(x, y)`, the layout `ram/wram.asm` documents at the pinned commit.
    fn warps(&mut self, warps: &[(u8, u8)]) {
        self.memory.set(ram::wNumberOfWarps, warps.len() as u8);
        for (index, (x, y)) in warps.iter().enumerate() {
            let entry = ram::wWarpEntries + index as u16 * 4;
            self.memory.set(entry, *y);
            self.memory.set(entry + 1, *x);
            self.memory.set(entry + 2, 0);
            self.memory.set(entry + 3, 1);
        }
    }
}

impl FakeMemory {
    fn set(&mut self, address: u16, value: u8) {
        self.bytes[address as usize] = value;
    }

    fn or(&mut self, address: u16, value: u8) {
        self.bytes[address as usize] |= value;
    }
}

fn kinds(events: &[RewardEvent]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind).collect()
}

fn labels(events: &[RewardEvent]) -> Vec<&str> {
    events.iter().map(|event| event.label.as_str()).collect()
}

fn count_of_kind(events: &[RewardEvent], kind: &str) -> usize {
    events.iter().filter(|event| event.kind == kind).count()
}

fn set_event_flag(memory: &mut FakeMemory, bit: u16) {
    memory.or(ram::wEventFlags + (bit >> 3), 1 << (bit & 7));
}

#[test]
fn boot_then_playable_baselines_persistent_bits_and_reads_each_byte_once() {
    let mut f = Fixture::new();
    f.memory.set(ram::wStatusFlags6, 0);
    assert!(f.sample().is_empty());

    f.memory.set(ram::wStatusFlags6, 1);
    f.memory.set(ram::wPokedexOwned, 255);
    f.memory.set(ram::wObtainedBadges, 3);
    let events = f.sample();
    assert_eq!(labels(&events), ["ADVENTURE STARTED"]);
    assert!(
        f.memory.reads.values().all(|count| *count == 1),
        "a sample must read each address once: {:?}",
        f.memory.reads.iter().filter(|(_, count)| **count != 1).collect::<Vec<_>>()
    );
    assert!(f.sample().is_empty(), "baselined bits never pay");
}

#[test]
fn downstairs_and_outside_rise_once_persist_and_cannot_pay_again_after_rollback() {
    let mut f = Fixture::new();
    f.memory.set(ram::wCurMap, 0x26);
    f.visit_x(1);

    f.memory.set(ram::wCurMap, 0x25);
    assert_eq!(f.visit_x(1).iter().filter(|e| e.label == "DOWNSTAIRS").count(), 1);
    f.memory.set(ram::wCurMap, 0);
    assert_eq!(f.visit_x(1).iter().filter(|e| e.label == "OUTSIDE").count(), 1);

    let state = f.reward.export_state();
    f.reward.clear_transient();
    f.memory.set(ram::wCurMap, 0x25);
    assert!(f.visit_x(1).is_empty());
    f.memory.set(ram::wCurMap, 0);
    assert!(f.visit_x(1).is_empty());
    assert_eq!(
        serde_json::to_value(f.reward.statistics().counts).unwrap(),
        state["counts"]
    );

    f.memory.set(ram::wFontLoaded, 1);
    f.visit_x(1);
    assert!(!f.reward.safe(), "a loaded dialogue font is not a safe sample");
}

#[test]
fn owned_badges_trainer_and_story_flags_are_bitset_novelty_not_party_size() {
    let mut f = Fixture::new();
    f.sample();

    f.memory.set(ram::wPokedexOwned, 3);
    f.memory.set(ram::wObtainedBadges, 1);
    set_event_flag(&mut f.memory, events::EVENT_BEAT_PEWTER_GYM_TRAINER_0);
    assert_eq!(kinds(&f.sample()), ["trainer", "species", "species", "badge"]);

    f.memory.set(ram::wPartyCount, 2);
    assert!(f.sample().is_empty(), "party size is not novelty");

    set_event_flag(&mut f.memory, events::EVENT_FOLLOWED_OAK_INTO_LAB);
    assert_eq!(kinds(&f.sample()), ["milestone"]);

    // Clearing and re-setting an owned bit must not pay twice.
    f.memory.set(ram::wPokedexOwned, 0);
    f.sample();
    f.memory.set(ram::wPokedexOwned, 3);
    assert!(f.sample().is_empty());

    let mut restored = PokemonRedReward::new();
    restored.import_state(&f.reward.export_state()).unwrap();
    assert!(restored.sample(&mut f.memory, 1000.0).is_empty());
}

#[test]
fn unique_coverage_pays_every_eight_locations_with_no_oscillation_and_survives_restore() {
    let mut f = Fixture::new();
    f.sample();

    for i in 0..100 {
        assert_eq!(count_of_kind(&f.visit_x(i % 2), kind::EXPLORATION), 0);
    }
    let mut payouts = 0;
    for x in 2..8 {
        payouts += count_of_kind(&f.visit_x(x), kind::EXPLORATION);
    }
    assert_eq!(payouts, 1, "eight unique locations pay once");

    let state = f.reward.export_state();
    let mut restored = PokemonRedReward::new();
    restored.import_state(&state).unwrap();
    assert_eq!(restored.export_state(), state, "export/import is a round trip");

    for x in 0..8u8 {
        f.memory.set(ram::wXCoord, x);
        for i in 0..3 {
            assert!(
                restored.sample(&mut f.memory, 10_000.0 + f64::from(u32::from(x) * 3 + i)).is_empty(),
                "restored coverage is not re-earned"
            );
        }
    }

    for n in 8..240u32 {
        payouts += count_of_kind(
            &f.visit((n % 40) as u8, (n / 40) as u8),
            kind::EXPLORATION,
        );
    }
    assert_eq!(payouts, 25, "25 payouts per map, i.e. 200 locations");
}

#[test]
fn map_novelty_requires_stable_controllable_coordinates_and_a_valid_non_demo_map() {
    let mut f = Fixture::new();
    f.sample();
    f.memory.set(ram::wCurMap, 2);

    for address in [ram::wJoyIgnore, ram::wStatusFlags5, ram::wMovementFlags, ram::wBattleType] {
        f.memory.set(address, 1);
        assert!(f.visit_x(1).is_empty(), "gate at {address:#06x} must block");
        f.memory.set(address, 0);
    }
    assert_eq!(count_of_kind(&f.visit_x(2), kind::MAP), 1);
    assert!(f.visit_x(3).is_empty(), "a second visit is not novel");
    f.memory.set(ram::wCurMap, 255);
    assert!(f.visit_x(4).is_empty(), "map 255 is out of range");
}

#[test]
fn fresh_coverage_includes_the_imported_position_only_after_the_gates_pass() {
    let mut f = Fixture::new();
    f.sample();

    f.memory.set(ram::wJoyIgnore, 1);
    for x in 0..8 {
        assert!(f.visit_x(x).is_empty());
    }
    assert_eq!(f.reward.statistics().unique_tiles, 0);

    f.memory.set(ram::wJoyIgnore, 0);
    for x in 0..8u8 {
        let events = f.visit_x(x);
        assert_eq!(count_of_kind(&events, kind::EXPLORATION), usize::from(x == 7));
        assert_eq!(f.reward.statistics().unique_tiles, usize::from(x) + 1);
    }
    for i in 0..40 {
        assert!(f.visit_x(i % 2).is_empty(), "oscillation earns nothing");
    }
}

#[test]
fn a_wild_run_capture_or_single_faint_never_pays_a_ko_while_a_verified_ko_does() {
    for result in [0u8, 1, 2] {
        let mut f = Fixture::new();
        f.sample();
        f.memory.set(ram::wIsInBattle, 1);
        f.memory.set(ram::wEnemyMonHP + 1, 10);
        f.memory.set(ram::wEnemyMonMaxHP + 1, 10);
        f.sample();
        f.memory.set(ram::wBattleResult, result);
        assert!(f.sample().is_empty());
        f.memory.set(ram::wIsInBattle, 0);
        assert!(f.sample().is_empty(), "a living enemy at exit is not a KO");
    }

    // pret/pokered 0cd19d3: core.asm HandleEnemyMonFainted:699-713 returns
    // after a wild KO; FaintEnemyPokemon:808-817 checks party survival and
    // explicitly clears wBattleResult. InitBattleVariables:4-6 also clears it,
    // hence the living -> zero HP observation is essential, not result zero
    // alone.
    let mut f = Fixture::new();
    f.sample();
    f.memory.set(ram::wIsInBattle, 1);
    f.memory.set(ram::wEnemyMonHP + 1, 10);
    f.memory.set(ram::wEnemyMonMaxHP + 1, 10);
    f.sample();
    f.memory.set(ram::wEnemyMonHP + 1, 0);
    f.sample();
    f.memory.set(ram::wIsInBattle, 0);
    assert_eq!(kinds(&f.sample()), ["battle"]);

    f.reward.clear_transient();
    let state = f.reward.export_state();
    f.reward.import_state(&state).unwrap();
    f.memory.set(ram::wIsInBattle, 1);
    f.memory.set(ram::wEnemyMonHP + 1, 10);
    f.sample();
    f.memory.set(ram::wEnemyMonHP + 1, 0);
    f.sample();
    f.memory.set(ram::wIsInBattle, 0);
    assert!(
        f.sample().is_empty(),
        "an already-paid battle key cannot pay after rollback"
    );
    assert_eq!(f.reward.statistics().counts[kind::BATTLE], 1);
}

#[test]
fn a_catch_pays_the_new_species_amount_once_the_repeat_amount_after_and_stops_at_three() {
    let mut f = Fixture::new();
    f.sample();

    // A species this run has never owned: the cartridge sets the Pokédex bit on the way
    // through, so the existing `species` rule pays 0.50 and the new rule pays 0.30.
    let first = f.catch(0xb0, Some(3));
    assert_eq!(kinds(&first), ["species", "catch"]);
    assert_eq!(labels(&first), ["OWNED #4", "CAUGHT #176"]);
    assert!((first[0].value - 0.5).abs() < 1e-12, "the species rule is untouched");
    assert!((first[1].value - 0.30).abs() < 1e-12);

    // The same species again: a repeat, twice, and then the cap.
    for _ in 0..2 {
        let again = f.catch(0xb0, None);
        assert_eq!(kinds(&again), ["catch"]);
        assert_eq!(again[0].value, 0.10, "the repeat amount is exactly 0.10, not 0.3/3");
    }
    assert!(f.catch(0xb0, None).is_empty(), "three payouts per species is the cap");
    assert_eq!(f.reward.statistics().counts[kind::CATCH], 3);

    // Another species starts its own count, and its own 0.30.
    let other = f.catch(0x99, Some(0));
    assert_eq!(kinds(&other), ["species", "catch"]);
    assert!((other[1].value - 0.30).abs() < 1e-12);
}

#[test]
fn a_catch_of_a_species_this_run_already_owns_pays_the_repeat_amount() {
    let mut f = Fixture::new();
    f.sample();
    // Owned before the battle -- a gift, a trade, an evolution -- so no Pokédex bit is set
    // during it and the catch is not a new species.
    f.memory.or(ram::wPokedexOwned, 1);
    assert_eq!(kinds(&f.sample()), ["species"]);

    let events = f.catch(0x99, None);
    assert_eq!(kinds(&events), ["catch"]);
    assert_eq!(events[0].value, 0.10);
}

#[test]
fn nothing_but_a_wild_catch_pays_the_catch_rule() {
    // A trainer battle: balls cannot be thrown, and `wIsInBattle` is 2.
    let mut f = Fixture::new();
    f.sample();
    f.memory.set(ram::wIsInBattle, 2);
    f.memory.set(ram::wEnemyMonHP + 1, 10);
    f.memory.set(ram::wEnemyMonMaxHP + 1, 10);
    f.sample();
    f.memory.set(ram::wCapturedMonSpecies, 0xb0);
    f.sample();
    f.memory.set(ram::wCapturedMonSpecies, 0);
    f.memory.set(ram::wBattleResult, 2);
    f.memory.set(ram::wIsInBattle, 0);
    assert!(f.sample().is_empty(), "a trainer battle never pays the catch rule");

    // The Safari Zone and the old man's tutorial are excluded a step earlier: the whole
    // sample is dropped with a visible mode, so no battle is ever opened.
    for battle_type in [1u8, 2] {
        let mut f = Fixture::new();
        f.sample();
        f.memory.set(ram::wBattleType, battle_type);
        f.memory.set(ram::wIsInBattle, 1);
        assert!(f.sample().is_empty());
        f.memory.set(ram::wCapturedMonSpecies, 0xb0);
        assert!(f.sample().is_empty());
        f.memory.set(ram::wCapturedMonSpecies, 0);
        f.memory.set(ram::wBattleResult, 2);
        f.memory.set(ram::wIsInBattle, 0);
        f.memory.set(ram::wBattleType, 0);
        assert!(f.sample().is_empty());
        assert_eq!(f.reward.statistics().counts[kind::CATCH], 0);
    }

    // A ball that missed: `wCapturedMonSpecies` never leaves zero and the battle ends as a
    // run or a loss.
    let mut f = Fixture::new();
    f.sample();
    f.memory.set(ram::wIsInBattle, 1);
    f.memory.set(ram::wEnemyMonHP + 1, 10);
    f.memory.set(ram::wEnemyMonMaxHP + 1, 10);
    f.sample();
    f.memory.set(ram::wIsInBattle, 0);
    assert!(f.sample().is_empty());
}

#[test]
fn a_rollback_cannot_replay_a_catch() {
    let mut f = Fixture::new();
    f.sample();
    assert_eq!(kinds(&f.catch(0xb0, Some(3))), ["species", "catch"]);

    f.reward.clear_transient();
    let state = f.reward.export_state();
    f.reward.import_state(&state).unwrap();
    assert!(f.catch(0xb0, None).is_empty(), "an already-paid species cannot pay after rollback");
    assert_eq!(f.reward.statistics().counts[kind::CATCH], 1);
}

#[test]
fn a_v5_state_restores_under_v6_with_the_catch_counter_at_zero() {
    let mut f = Fixture::new();
    f.sample();
    f.catch(0xb0, Some(3));
    let v6 = f.reward.export_state();
    assert_eq!(v6["catchCounts"], json!({ "176": 1 }));

    // The v5 shape is this one without the counter the rule added: same `version`, same field
    // names, same meanings. That is the whole of the documented migration.
    let mut v5 = v6.clone();
    v5.as_object_mut().unwrap().remove("catchCounts");
    assert_eq!(v5["version"], json!(STATE_VERSION), "v5 and v6 states share a schema version");

    let mut restored = PokemonRedReward::new();
    restored.import_state(&v5).unwrap();
    let mut expected = v6.clone();
    expected["catchCounts"] = json!({});
    assert_eq!(restored.export_state(), expected, "the counter starts at 0, nothing else moves");

    // A genuine v5 `counts` object carries eight kinds and no `catch`, which reads as zero.
    let mut older = v5.clone();
    older["counts"].as_object_mut().unwrap().remove("catch");
    let mut restored = PokemonRedReward::new();
    restored.import_state(&older).unwrap();
    assert_eq!(restored.statistics().counts[kind::CATCH], 0);
    assert_eq!(restored.statistics().counts[kind::SPECIES], 1);
}

#[test]
fn a_repeated_wild_ko_decays_then_stops() {
    let mut f = Fixture::new();
    f.sample();
    let mut values = Vec::new();
    for _ in 0..4 {
        f.memory.set(ram::wIsInBattle, 1);
        f.memory.set(ram::wEnemyMonHP + 1, 10);
        f.memory.set(ram::wEnemyMonMaxHP + 1, 10);
        f.sample();
        f.memory.set(ram::wEnemyMonHP + 1, 0);
        f.sample();
        f.memory.set(ram::wIsInBattle, 0);
        values.extend(f.sample().iter().map(|event| event.value));
    }
    assert_eq!(values.len(), 3, "at most three KOs per map/species/level");
    assert!((values[0] - 0.1).abs() < 1e-12);
    assert!((values[1] - 0.05).abs() < 1e-12);
    assert!((values[2] - 0.1 / 3.0).abs() < 1e-12);
}

#[test]
fn an_unknown_cartridge_disables_semantics_and_old_reward_state_rebaselines() {
    let mut f = Fixture::new();
    let mut unsupported = PokemonRedReward::with_rom_hash("other");
    assert!(unsupported.sample(&mut f.memory, 0.0).is_empty());
    assert!(unsupported.statistics().mode.contains("OFF"));

    f.reward.import_state(&json!({ "version": 1 })).unwrap();
    f.memory.set(ram::wObtainedBadges, 255);
    assert!(f.sample().is_empty(), "a rebaselined adapter pays nothing for old bits");

    let mut broken = f.reward.export_state();
    broken["tiles"] = json!([Value::Null]);
    assert_eq!(
        f.reward.import_state(&broken).unwrap_err().to_string(),
        "Invalid reward checkpoint"
    );
}

#[test]
fn malformed_checkpoint_fields_are_named_in_the_error() {
    let mut reward = PokemonRedReward::new();
    let good = reward.export_state();

    for (field, wrong) in [
        ("seen", json!("not an array")),
        ("tiles", json!(5)),
        ("total", json!("not a number")),
        ("stable", json!(-1)),
        ("initialized", json!(1)),
        ("sawBoot", json!("yes")),
        ("location", json!(42)),
        ("mode", Value::Null),
        ("recent", json!({})),
        ("counts", json!([])),
        ("tileCounts", json!("not a record")),
        ("wildWins", json!(3)),
        ("catchCounts", json!(3)),
    ] {
        let mut broken = good.clone();
        broken[field] = wrong;
        assert_eq!(
            reward.import_state(&broken).unwrap_err().to_string(),
            "Invalid reward checkpoint",
            "{field}"
        );
    }

    let mut negative = good.clone();
    negative["counts"] = json!({ "badge": -1 });
    assert!(reward.import_state(&negative).is_err());

    let mut bad_event = good.clone();
    bad_event["recent"] = json!([{ "kind": "nonsense", "label": "x", "brainMs": 0, "value": 1 }]);
    assert_eq!(
        reward.import_state(&bad_event).unwrap_err().to_string(),
        "Invalid reward history"
    );

    let mut bad_ledger = good.clone();
    bad_ledger["replayBlocked"] = json!([7]);
    assert_eq!(
        reward.import_state(&bad_ledger).unwrap_err().to_string(),
        "Invalid replay ledger"
    );
}

#[test]
fn the_ladder_is_thirty_eight_rungs_with_one_condition_each() {
    assert_eq!(RANK_LADDER.len(), 38);
    assert_eq!(RUNGS.len(), RANK_LADDER.len(), "a rung per label");
    assert_eq!(RUNGS[0], Rung::Boot, "rank 0 is the floor");

    let reward = PokemonRedReward::new();
    assert_eq!(GameAdapter::rank_ladder(&reward).len(), 38);
    assert_eq!(GameAdapter::progress(&reward).rank_max, 37);

    // Every flag a rung names must be a flag the adapter actually walks, or the
    // ledger key it looks for is never written and the rung is unreachable.
    for (index, rung) in RUNGS.iter().enumerate() {
        if let Rung::Flag(name) = rung {
            assert!(
                symbols::EVENTS.iter().any(|(selected, _)| selected == name),
                "rung {index} ({}) names {name}, which is not in symbols::EVENTS",
                RANK_LADDER[index]
            );
        }
    }
}

#[test]
fn the_ladder_climbs_maps_then_flags_then_badges_through_the_early_game() {
    let mut f = Fixture::booted();
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 1, "bedroom");
    f.memory.set(ram::wCurMap, 0x25);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 2, "downstairs");
    f.memory.set(ram::wCurMap, 0x00);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 3, "Pallet Town");
    f.memory.set(ram::wCurMap, 0x28);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 4, "Oak's lab");

    set_event_flag(&mut f.memory, events::EVENT_GOT_STARTER);
    f.visit_x(2);
    assert_eq!(f.reward.rank(), 5);
    // The parcel *delivered* is the rung; merely collecting it is not.
    set_event_flag(&mut f.memory, events::EVENT_GOT_OAKS_PARCEL);
    f.visit_x(3);
    assert_eq!(f.reward.rank(), 5, "collecting the parcel is not rung 6");
    set_event_flag(&mut f.memory, events::EVENT_OAK_GOT_PARCEL);
    f.visit_x(4);
    assert_eq!(f.reward.rank(), 6);
    set_event_flag(&mut f.memory, events::EVENT_GOT_POKEDEX);
    f.visit_x(5);
    assert_eq!(f.reward.rank(), 7);

    // Viridian, the forest, Pewter: three map rungs in a row.
    for (map, expected) in [(0x01u8, 8u32), (0x33, 9), (0x02, 10)] {
        f.memory.set(ram::wCurMap, map);
        f.visit_x(6);
        assert_eq!(f.reward.rank(), expected, "map {map:#04x}");
    }

    f.memory.set(ram::wObtainedBadges, 0b0000_0001);
    f.visit_x(7);
    assert_eq!(f.reward.rank(), 11, "Boulder Badge");
    let snapshot = GameAdapter::progress(&f.reward);
    assert_eq!(snapshot.rank_label, "BOULDER BADGE");
    assert_eq!(snapshot.counter, 1);
    assert_eq!(snapshot.rank_max, 37);
}

#[test]
fn a_map_rung_needs_a_first_visit_not_the_current_position() {
    let mut f = Fixture::booted();
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 1, "bedroom");

    // Standing in Cerulean while a script runs never records the arrival, so the
    // rung is not earned even though the fly is right there...
    f.memory.set(ram::wCurMap, maps::CERULEAN_CITY);
    f.memory.set(ram::wJoyIgnore, 1);
    f.visit_x(2);
    assert_eq!(f.reward.rank(), 1, "a scripted sample earns no rung");

    // ...and once it is recorded, walking away does not take it back.
    f.memory.set(ram::wJoyIgnore, 0);
    f.visit_x(3);
    assert_eq!(f.reward.rank(), 13, "Cerulean City");
    f.memory.set(ram::wCurMap, 0x0c); // Route 1: no rung of its own.
    f.visit_x(4);
    assert_eq!(f.reward.rank(), 13, "a map with no rung does not lower the rank");
}

#[test]
fn rungs_earned_out_of_order_take_the_maximum() {
    let mut f = Fixture::booted();

    // Viridian Forest (rung 9) before Viridian City (rung 8).
    f.memory.set(ram::wCurMap, 0x33);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 9);
    f.memory.set(ram::wCurMap, 0x01);
    f.visit_x(2);
    assert_eq!(f.reward.rank(), 9, "the lower rung does not pull the rank down");

    // A flag from much further along, with nothing in between.
    set_event_flag(&mut f.memory, events::EVENT_BEAT_SILPH_CO_GIOVANNI);
    f.visit_x(3);
    assert_eq!(f.reward.rank(), 28, "Silph Co. freed");

    // And a badge count that outranks it.
    f.memory.set(ram::wObtainedBadges, 0b0011_1111);
    f.visit_x(4);
    assert_eq!(f.reward.rank(), 29, "Marsh Badge");
}

#[test]
fn every_badge_rung_lands_where_the_ladder_says() {
    let mut f = Fixture::booted();
    for (badges, expected) in
        [(1u32, 11u32), (2, 14), (3, 19), (4, 24), (5, 27), (6, 29), (7, 31), (8, 32)]
    {
        f.memory.set(ram::wObtainedBadges, ((1u16 << badges) - 1) as u8);
        f.visit_x(badges as u8);
        assert_eq!(f.reward.rank(), expected, "{badges} badges");
        assert_eq!(GameAdapter::progress(&f.reward).counter, badges);
    }
    assert_eq!(GameAdapter::progress(&f.reward).rank_label, "EARTH BADGE");
}

#[test]
fn the_elite_four_rungs_survive_the_lobby_reset_that_clears_their_flags() {
    // pret/pokered 0cd19d3: IndigoPlateauLobby_Script runs
    // `ResetEventRange INDIGO_PLATEAU_EVENTS_START, EVENT_LANCES_ROOM_LOCK_DOOR`
    // on every lobby entry once BIT_STARTED_ELITE_4 is set, so Lorelei's, Bruno's
    // and Agatha's beat flags are all cleared the moment the fly walks back in --
    // which is exactly what a blackout does. The rungs read the adapter's lifetime
    // ledger instead of the live bits, so they hold.
    let mut f = Fixture::new();
    f.sample();
    f.memory.set(ram::wCurMap, 0xae);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 33, "Indigo Plateau lobby");

    for (bit, expected) in [
        (events::EVENT_BEAT_LORELEIS_ROOM_TRAINER_0, 34),
        (events::EVENT_BEAT_BRUNOS_ROOM_TRAINER_0, 35),
        (events::EVENT_BEAT_AGATHAS_ROOM_TRAINER_0, 36),
    ] {
        set_event_flag(&mut f.memory, bit);
        f.visit_x(2);
        assert_eq!(f.reward.rank(), expected);
    }

    // The lobby reset: every Indigo Plateau bit back to zero.
    for bit in 2272..=2303u16 {
        f.memory.set(ram::wEventFlags + (bit >> 3), 0);
    }
    f.visit_x(3);
    assert_eq!(f.reward.rank(), 36, "the reset does not cost a rung");

    // And it still holds across a rollback and a checkpoint round trip.
    f.reward.clear_transient();
    let mut restored = PokemonRedReward::new();
    restored.import_state(&f.reward.export_state()).unwrap();
    f.memory.set(ram::wXCoord, 9);
    for i in 0..3 {
        restored.sample(&mut f.memory, 90_000.0 + f64::from(i));
    }
    assert_eq!(restored.rank(), 36, "the ledger came back with the checkpoint");
}

#[test]
fn the_champion_rung_comes_from_the_rival_flag_or_the_hall_of_fame_counter() {
    // HallOfFame.asm resets the whole Indigo Plateau range, EVENT_BEAT_CHAMPION_RIVAL
    // included, as the player is entered into the Hall of Fame: the flag for the last
    // rung is destroyed by the event that earns it. Either half is enough.
    let mut f = Fixture::new();
    f.sample();
    set_event_flag(&mut f.memory, events::EVENT_BEAT_CHAMPION_RIVAL);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 37, "beat the rival");
    assert_eq!(GameAdapter::progress(&f.reward).rank_label, "CHAMPION");

    f.memory.set(ram::wEventFlags + (events::EVENT_BEAT_CHAMPION_RIVAL >> 3), 0);
    f.visit_x(2);
    assert_eq!(f.reward.rank(), 37, "the Hall of Fame reset does not cost the rung");

    // A cartridge already in the Hall of Fame, seen by an adapter with an empty
    // ledger: wNumHoFTeams is the only signal left, and it is enough.
    let mut fresh = Fixture::new();
    fresh.memory.set(ram::wNumHoFTeams, 1);
    fresh.sample();
    fresh.visit_x(1);
    assert_eq!(fresh.reward.rank(), 37, "wNumHoFTeams alone");
}

#[test]
fn the_rank_never_decreases_across_a_rollback() {
    let mut f = Fixture::booted();
    f.memory.set(ram::wCurMap, 0x06);
    f.visit_x(1);
    f.memory.set(ram::wObtainedBadges, 0b0000_1111);
    f.visit_x(2);
    assert_eq!(f.reward.rank(), 24, "Rainbow Badge");

    // A rollback drops the position and the stability counter, and the cartridge
    // itself is rewound: back in Pallet Town with no badges.
    f.reward.clear_transient();
    f.memory.set(ram::wCurMap, 0x00);
    f.memory.set(ram::wObtainedBadges, 0);
    f.visit_x(3);
    assert_eq!(f.reward.rank(), 24, "lifetime rungs are not rewound");
}

#[test]
fn the_rank_and_badge_count_come_back_from_a_restore_without_an_overworld_sample() {
    // Regression: `progress` and `badges` are only assigned in `observe`'s unscripted-overworld
    // branch, and neither was carried in the exported state. A restore that landed mid-battle
    // therefore reported rank 0 and 0 badges until the fly next stood still outdoors — which
    // the local end-to-end run measured at over five minutes. On stream that is the milestone
    // ladder reading BOOT, the badge counter reading 0, and the sim loop treating the eventual
    // re-derivation as a fresh climb: a spurious milestone event, a reset stuck-o-meter and an
    // overwritten milestone archive.
    let mut f = Fixture::new();
    f.memory.set(ram::wCurMap, 0x28);
    set_event_flag(&mut f.memory, events::EVENT_GOT_STARTER);
    f.memory.set(ram::wObtainedBadges, 0b0000_0011);
    f.visit_x(1);
    assert_eq!(f.reward.rank(), 14, "Cascade Badge");

    let mut restored = PokemonRedReward::new();
    restored.import_state(&f.reward.export_state()).unwrap();
    assert_eq!(restored.rank(), 14, "the rank is restored, not re-derived");
    assert_eq!(GameAdapter::progress(&restored).counter, 2, "and so is the badge count");

    // Still true when the very next thing that happens is a battle, which is the branch that
    // never touches either field.
    f.memory.set(ram::wIsInBattle, 1);
    restored.sample(&mut f.memory, 99_000.0);
    assert_eq!(restored.rank(), 14, "a battle sample does not reset it");
    assert_eq!(GameAdapter::progress(&restored).counter, 2);
}

#[test]
fn transition_demo_and_safari_samples_are_excluded_with_a_visible_mode() {
    let mut f = Fixture::new();
    f.sample();

    f.memory.set(ram::wPartyCount, 7);
    assert!(f.sample().is_empty());
    assert_eq!(f.reward.mode(), "TRANSITION");
    f.memory.set(ram::wPartyCount, 1);

    f.memory.set(ram::wCurMapWidth, 0);
    assert!(f.sample().is_empty());
    assert_eq!(f.reward.mode(), "TRANSITION");
    f.memory.set(ram::wCurMapWidth, 20);

    f.memory.set(ram::wXCoord, 99);
    assert!(f.sample().is_empty());
    assert_eq!(f.reward.mode(), "TRANSITION", "coordinates outside the map");
    f.memory.set(ram::wXCoord, 0);

    f.memory.set(ram::wBattleType, 1);
    assert!(f.sample().is_empty());
    assert_eq!(f.reward.mode(), "DEMO / SAFARI");
    f.memory.set(ram::wBattleType, 0);

    f.memory.set(ram::wStatusFlags7, 1);
    assert!(f.sample().is_empty());
    assert_eq!(f.reward.mode(), "DEMO / SAFARI");
    f.memory.set(ram::wStatusFlags7, 0);

    f.memory.set(ram::wIsInBattle, 1);
    f.sample();
    assert_eq!(f.reward.mode(), "BATTLE");
    f.memory.set(ram::wIsInBattle, 0);
    f.sample();
    assert_eq!(f.reward.mode(), "OVERWORLD");
}

#[test]
fn the_recent_ticker_keeps_the_newest_eight_events_newest_first() {
    let mut f = Fixture::new();
    f.sample();
    for i in 0..12u16 {
        f.memory.set(ram::wPokedexOwned + (i >> 3), 0);
    }
    for i in 0..12u16 {
        f.memory.or(ram::wPokedexOwned + (i >> 3), 1 << (i & 7));
        f.sample();
    }
    let recent = f.reward.statistics().recent;
    assert_eq!(recent.len(), 8);
    assert_eq!(recent[0].label, "OWNED #12", "newest first");
    assert_eq!(recent[7].label, "OWNED #5");
}

#[test]
fn the_adapter_reports_its_identity_and_pinned_rom() {
    let reward = PokemonRedReward::new();
    assert_eq!(reward.id(), "pokered-unique8-v6");
    assert_eq!(reward.migrates_from(), ["pokered-unique8-v5"]);
    assert!(reward.rom_allowed(SUPPORTED_ROM));
    assert!(!reward.rom_allowed(
        "5ca7ba01642a3b27b0cc0b5349b52792795b62d3ed977e98a09390659af96b7b"
    ));
    assert_eq!(reward.decoder_preset(), DecoderPresetId::GameBoy);
    assert_eq!(symbols::EVENTS.len(), 369);
    assert_eq!(symbols::ram::wNumberOfWarps, 0xd3ae);
    assert_eq!(symbols::ram::wWarpEntries, 0xd3af);
    assert_eq!(symbols::ram::wCurMapConnections, 0xd370);
    // Resolved from ram/wram.asm by services/flysim/tools/resolve_wram.py, bracketed by
    // wFontLoaded and wForcePlayerToChooseMon; never written out by hand.
    assert_eq!(symbols::ram::wCapturedMonSpecies, 0xd11c);
    assert_eq!(symbols::MILESTONES.len(), 17);
}

#[test]
fn a_v3_checkpoint_rebaselines_rather_than_being_read_as_a_v4_rank() {
    // The adapter id is the hard gate -- it is compared whole, so the sim loop never
    // offers a v3 checkpoint here at all. This is the second line of defence for a
    // blob that reaches import_state anyway: its `progress` is a rank on a different
    // ladder, so none of it may be credited.
    let mut f = Fixture::new();
    f.memory.set(ram::wCurMap, 0x06);
    f.memory.set(ram::wObtainedBadges, 0b0000_1111);
    f.visit_x(1);
    let mut old = f.reward.export_state();
    assert_eq!(old["version"], json!(4));
    old["version"] = json!(3);

    let mut reward = PokemonRedReward::new();
    reward.import_state(&old).unwrap();
    assert_eq!(reward.rank(), 0, "a v3 rank is not credited");
    assert_eq!(reward.export_state()["seen"], json!([]), "nor its ledger");
}

// --- Boundary rewards (`docs/design/room-escape.md` section 2) ------------------------------------

/// Payout values of the boundary events in `events`, in order.
fn boundary_values(events: &[RewardEvent]) -> Vec<f64> {
    events
        .iter()
        .filter(|event| event.kind == kind::BOUNDARY)
        .map(|event| event.value)
        .collect()
}

#[test]
fn a_warp_pays_once_when_adjacent_and_twice_as_much_on_the_exit_itself() {
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);

    assert!(boundary_values(&f.visit(2, 4)).is_empty(), "two tiles away is not adjacent");
    assert_eq!(boundary_values(&f.visit(3, 4)), [0.05], "one tile away pays the adjacent value");
    assert_eq!(boundary_values(&f.visit(4, 4)), [0.10], "the exit tile pays twice it");
    assert_eq!(labels(&f.visit(4, 3)).len(), 0, "the four-neighbourhood is already paid");
    assert!(boundary_values(&f.visit(3, 4)).is_empty(), "and neither half pays twice");
    assert!(boundary_values(&f.visit(4, 4)).is_empty());

    // Every neighbour of the same warp is the same ledger key, so circling it earns nothing more.
    for (x, y) in [(5, 4), (4, 5), (3, 4), (4, 3)] {
        assert!(boundary_values(&f.visit(x, y)).is_empty(), "({x}, {y}) is the same warp");
    }
}

#[test]
fn adjacency_is_the_four_neighbourhood_not_the_eight() {
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);
    for (x, y) in [(3, 3), (5, 5), (3, 5), (5, 3)] {
        assert!(boundary_values(&f.visit(x, y)).is_empty(), "({x}, {y}) is diagonal");
    }
    assert_eq!(boundary_values(&f.visit(4, 3)), [0.05], "but orthogonal pays");
}

/// The macro palette's read of the same ledger (`docs/design/macros.md` section 3).
///
/// `GO EXIT` asks by exit, the ledger answers from the `boundary` keys it has already written, and
/// the point of the test is that the two agree without the key format being written out twice
/// anywhere but here.
#[test]
fn the_boundary_ledger_answers_which_exits_a_run_has_found() {
    let mut f = Fixture::booted();
    let map = maps::REDS_HOUSE_2F;
    f.warps(&[(4, 4), (7, 1)]);
    f.memory.set(ram::wCurMapConnections, connection::SOUTH);

    let stairs = MapExit::Warp { map, x: 4, y: 4 };
    let door = MapExit::Warp { map, x: 7, y: 1 };
    let south = MapExit::Edge { map, edge: MapEdge::South };
    assert!(!f.reward.exit_visited(stairs), "a fresh run has found nothing");
    assert!(!f.reward.exit_visited(door));
    assert!(!f.reward.exit_visited(south));

    // Beside the first warp: the `:near` half, which is all a town door ever records, and it is
    // enough. The other warp and the edge are untouched by it.
    assert_eq!(boundary_values(&f.visit(3, 4)), [0.05]);
    assert!(f.reward.exit_visited(stairs), "found from beside it");
    assert!(!f.reward.exit_visited(door), "and only that one");
    assert!(!f.reward.exit_visited(south));

    // Onto the second: the `:on` half, which is what arriving through a staircase records.
    assert_eq!(boundary_values(&f.visit(7, 1)), [0.10]);
    assert!(f.reward.exit_visited(door));

    // The connected edge is its own key, by compass direction rather than by tile.
    // The map is 20 blocks high, so its bottom tile row is 39.
    assert_eq!(boundary_values(&f.visit(7, 39)), [0.10]);
    assert!(f.reward.exit_visited(south));
    assert!(!f.reward.exit_visited(MapExit::Edge { map, edge: MapEdge::North }));

    // A different map's exit on the same tile is a different key, because the ledger is per map.
    assert!(!f.reward.exit_visited(MapExit::Warp { map: maps::PALLET_TOWN, x: 4, y: 4 }));

    // And the read pays nothing and records nothing: the ledger is the same size after it.
    let before = f.reward.statistics().counts[kind::BOUNDARY];
    assert!(f.reward.exit_visited(stairs));
    assert_eq!(f.reward.statistics().counts[kind::BOUNDARY], before);
}

#[test]
fn only_the_warps_the_table_declares_are_read() {
    let mut f = Fixture::booted();
    // Two entries written, one declared: the second is stale bytes from a previous map.
    f.warps(&[(4, 4), (9, 9)]);
    f.memory.set(ram::wNumberOfWarps, 1);
    assert!(boundary_values(&f.visit(9, 8)).is_empty(), "an undeclared entry is not an exit");
    assert_eq!(boundary_values(&f.visit(4, 3)), [0.05]);

    // A count past MAX_WARP_EVENTS is clamped rather than walked off the end of the array.
    f.memory.set(ram::wNumberOfWarps, 255);
    assert!(boundary_values(&f.visit(19, 19)).is_empty());
}

#[test]
fn each_warp_on_a_map_is_its_own_ledger_entry() {
    let mut f = Fixture::booted();
    f.warps(&[(2, 7), (3, 7), (7, 1)]);
    assert_eq!(boundary_values(&f.visit(2, 6)), [0.05], "next to the first door");
    assert_eq!(boundary_values(&f.visit(3, 6)), [0.05], "next to the second");
    assert_eq!(boundary_values(&f.visit(7, 2)), [0.05], "next to the stairs");
    assert_eq!(boundary_values(&f.visit(7, 1)), [0.10], "and onto them");
    assert_eq!(f.reward.statistics().counts[kind::BOUNDARY], 4);
}

#[test]
fn a_connected_map_edge_is_an_exit_and_an_unconnected_one_is_not() {
    // NORTH only, on a 20x20-block map: 40 tiles each way, so row 0 is the exit and row 1 is
    // adjacent to it. `constants/map_data_constants.asm`: EAST 1, WEST 2, SOUTH 4, NORTH 8.
    let mut f = Fixture::booted();
    f.memory.set(ram::wCurMapConnections, 8);
    assert!(boundary_values(&f.visit(5, 2)).is_empty(), "two rows in is not adjacent");
    assert_eq!(boundary_values(&f.visit(5, 1)), [0.05]);
    assert_eq!(boundary_values(&f.visit(5, 0)), [0.10]);
    // The whole row is one exit, so walking along it earns nothing more.
    assert!(boundary_values(&f.visit(9, 0)).is_empty());
    // And the other three edges are not connected, so they are walls.
    assert!(boundary_values(&f.visit(0, 5)).is_empty(), "west is unconnected");
    assert!(boundary_values(&f.visit(39, 5)).is_empty(), "east is unconnected");
    assert!(boundary_values(&f.visit(5, 39)).is_empty(), "south is unconnected");

    f.memory.set(ram::wCurMapConnections, 8 | 1);
    assert_eq!(boundary_values(&f.visit(38, 5)), [0.05], "east now is");
    assert_eq!(boundary_values(&f.visit(39, 5)), [0.10]);
}

#[test]
fn the_first_playable_sample_baselines_the_exits_it_is_already_standing_on() {
    // `docs/design/room-escape.md`'s verification list: no payout inside a map already baselined.
    let mut f = Fixture::new();
    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_1F);
    f.memory.set(ram::wXCoord, 2);
    f.memory.set(ram::wYCoord, 7);
    f.warps(&[(2, 7), (7, 1)]);
    f.memory.set(ram::wCurMapConnections, 8);
    let first = f.sample();
    assert!(boundary_values(&first).is_empty(), "standing in the door is not finding it");
    assert!(boundary_values(&f.visit(2, 7)).is_empty(), "nor is standing there for longer");
    assert!(boundary_values(&f.visit(3, 7)).is_empty(), "nor its neighbours");
    // The other exits on the same map were not baselined and are still there to be found.
    assert_eq!(boundary_values(&f.visit(7, 2)), [0.05], "the stairs are still novel");
}

#[test]
fn standing_on_a_door_pays_although_the_shared_scripted_gate_rejects_that_state() {
    // BIT_STANDING_ON_DOOR is bit 0 of wMovementFlags, inside the 0xc7 mask the map and
    // exploration rules gate on, so the boundary rule has its own gate. Everything else that
    // gate rejects it rejects too.
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);
    f.memory.set(ram::wMovementFlags, 1 << 0);
    assert_eq!(boundary_values(&f.visit(4, 4)), [0.10], "standing on a door still pays");
    assert_eq!(
        count_of_kind(&f.visit(4, 4), kind::MAP),
        0,
        "and the shared gate is untouched: no map payout from a door tile"
    );

    let mut spinning = Fixture::booted();
    spinning.warps(&[(4, 4)]);
    spinning.memory.set(ram::wMovementFlags, 1 << 7);
    assert!(boundary_values(&spinning.visit(4, 4)).is_empty(), "a spin tile blocks it");

    let mut ignored = Fixture::booted();
    ignored.warps(&[(4, 4)]);
    ignored.memory.set(ram::wJoyIgnore, 1);
    assert!(boundary_values(&ignored.visit(4, 4)).is_empty(), "an ignored joypad blocks it");
}

#[test]
fn the_same_warp_coordinates_on_another_map_are_a_different_exit() {
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);
    assert_eq!(boundary_values(&f.visit(4, 3)), [0.05]);
    f.memory.set(ram::wCurMap, maps::OAKS_LAB);
    assert_eq!(boundary_values(&f.visit(4, 3)), [0.05], "keyed by map as well as by warp");
}

#[test]
fn boundary_payouts_round_trip_through_a_checkpoint_and_survive_a_rollback() {
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);
    assert_eq!(boundary_values(&f.visit(4, 3)), [0.05]);
    assert_eq!(boundary_values(&f.visit(4, 4)), [0.10]);
    let state = f.reward.export_state();
    let seen = state["seen"].as_array().expect("a ledger").clone();
    let keys: Vec<&str> = seen.iter().filter_map(|key| key.as_str()).collect();
    assert!(keys.contains(&"boundary:38:4:4:near"), "the adjacent key is exported: {keys:?}");
    assert!(keys.contains(&"boundary:38:4:4:on"), "and the on-exit key: {keys:?}");
    assert_eq!(state["counts"]["boundary"], json!(2));

    // A rollback keeps lifetime novelty, so the same door cannot be farmed by rolling back.
    f.reward.clear_transient();
    assert!(boundary_values(&f.visit(4, 3)).is_empty());
    assert!(boundary_values(&f.visit(4, 4)).is_empty());

    // And so does a restore into a fresh adapter.
    let mut restored = PokemonRedReward::new();
    restored.import_state(&state).unwrap();
    let mut g = Fixture::new();
    g.reward = restored;
    g.memory.set(ram::wCurMap, maps::REDS_HOUSE_2F);
    g.sample();
    g.warps(&[(4, 4)]);
    assert!(boundary_values(&g.visit(4, 3)).is_empty(), "an imported ledger is still paid up");
}

#[test]
fn a_sample_with_a_warp_table_still_reads_each_address_once() {
    let mut f = Fixture::booted();
    f.warps(&[(2, 7), (3, 7), (7, 1)]);
    f.memory.set(ram::wCurMapConnections, 8 | 4);
    f.visit(7, 2);
    f.memory.reads.clear();
    f.sample();
    assert!(
        f.memory.reads.values().all(|count| *count == 1),
        "a sample must read each address once: {:?}",
        f.memory.reads.iter().filter(|(_, count)| **count != 1).collect::<Vec<_>>()
    );
    assert_eq!(f.memory.reads.get(&ram::wNumberOfWarps), Some(&1));
    assert_eq!(f.memory.reads.get(&ram::wWarpEntries), Some(&1));
    assert_eq!(f.memory.reads.get(&ram::wCurMapConnections), Some(&1));
}

#[test]
fn oscillating_on_and_off_a_door_cannot_farm_reward() {
    let mut f = Fixture::booted();
    f.warps(&[(4, 4)]);
    let mut total = 0.0;
    for step in 0..200 {
        let events = if step % 2 == 0 { f.visit(4, 3) } else { f.visit(4, 4) };
        total += boundary_values(&events).iter().sum::<f64>();
    }
    assert!(
        (total - 0.15).abs() < 1e-9,
        "0.05 + 0.10 for the lifetime of the ledger and nothing after, got {total}"
    );
}

#[test]
fn the_location_accessor_carries_the_area_as_well_as_the_tile() {
    let mut f = Fixture::new();
    assert_eq!(
        GameAdapter::location(&f.reward),
        None,
        "nothing is known before the first overworld sample"
    );

    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_1F);
    f.visit(3, 6);
    assert_eq!(GameAdapter::location(&f.reward), Some((0x25, 3, 6)));

    // The bug this accessor exists to avoid: Red's staircase is (7, 1) on both floors, so a caller
    // watching only the coordinates would read the warp between them as standing still.
    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_1F);
    f.visit(7, 1);
    let downstairs = GameAdapter::location(&f.reward);
    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_2F);
    f.visit(7, 1);
    let upstairs = GameAdapter::location(&f.reward);
    assert_eq!(downstairs, Some((0x25, 7, 1)));
    assert_eq!(upstairs, Some((0x26, 7, 1)));
    assert_ne!(downstairs, upstairs, "the same tile in two rooms is two locations");

    // Transient, like the coordinates it is read out of.
    f.reward.clear_transient();
    assert_eq!(GameAdapter::location(&f.reward), None, "a rollback forgets where the fly was");
}

/// `GO FRONTIER`'s and `GO ROUTE`'s reads of the same lifetime ledger
/// (`docs/design/macros.md` sections 9 and 9.1).
///
/// Both are read accessors over state the reward rules already write: the `exploration` rule's set
/// of distinct player coordinates, and the `map` rule's per-map key. The test walks the fly and
/// then asks, so what is pinned is that the two agree about the key format without it being
/// written out twice anywhere but here.
#[test]
fn the_exploration_ledger_answers_which_ground_and_which_maps_a_run_has_covered() {
    let mut f = Fixture::booted();
    let map = maps::REDS_HOUSE_2F;
    let tile = |x, y| MapTile { map, x, y };

    // Three stable samples on one coordinate is what the `exploration` gate needs before it
    // records anything, which is the same gate every other rule here goes through.
    f.visit(0, 0);
    assert!(f.reward.tile_visited(tile(0, 0)), "the fly has stood where it booted");
    assert!(!f.reward.tile_visited(tile(3, 6)), "and nowhere else");

    f.visit(3, 6);
    assert!(f.reward.tile_visited(tile(3, 6)));
    assert!(!f.reward.tile_visited(tile(4, 6)), "the tile beside it is still frontier");
    assert!(
        !f.reward.tile_visited(MapTile { map: maps::PALLET_TOWN, x: 3, y: 6 }),
        "the same coordinates on another map are different ground"
    );

    // The map ledger, which is `GO ROUTE`'s "a door into a building whose interior is unvisited".
    assert!(f.reward.map_visited(maps::REDS_HOUSE_2F), "the fly is standing in it");
    assert!(!f.reward.map_visited(maps::OAKS_LAB));
    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_1F);
    f.visit(2, 3);
    assert!(f.reward.map_visited(maps::REDS_HOUSE_1F));
    assert!(!f.reward.map_visited(maps::PALLET_TOWN), "and only the maps it has been on");

    // Both survive a rollback, because both are lifetime state: the frontier moves outward over a
    // run rather than resetting to wherever the ratchet dropped the fly.
    f.reward.clear_transient();
    assert!(f.reward.tile_visited(tile(3, 6)));
    assert!(f.reward.map_visited(maps::REDS_HOUSE_1F));
}

/// `GO OBJECTIVE`'s place for the next unreached rung (`docs/design/macros.md` sections 9 and 12).
///
/// What this pins is the four things a wrong table would break: that the place is the rung *above*
/// the one reached; that the early ladder's places are the maps the fly has to walk through in
/// that order; that the two places the *game* moves move with it (Oak's trigger and the parcel);
/// and that every place names a real map, with a tile nowhere, because a tile is not derivable
/// without a ROM bank this crate cannot reach.
#[test]
fn the_objective_is_the_next_unreached_rungs_place_where_the_catalog_knows_one() {
    let mut f = Fixture::booted();
    f.visit(0, 0);
    // Rank 1, in the bedroom: the next rung is the ground floor.
    assert_eq!(f.reward.rank(), 1);
    assert_eq!(f.reward.objective(), Some(MapPlace::at(maps::REDS_HOUSE_1F)));

    // Downstairs: the next rung is the town.
    f.memory.set(ram::wCurMap, maps::REDS_HOUSE_1F);
    f.visit(2, 3);
    assert_eq!(f.reward.rank(), 2);
    assert_eq!(f.reward.objective(), Some(MapPlace::at(maps::PALLET_TOWN)));

    // Outside, with Oak's script not yet fired: the next rung is the lab, but the way into it is
    // Oak stopping the fly on the path north, so the place is the town's north connection. The lab
    // itself holds nothing to take until he has walked the fly in.
    f.memory.set(ram::wCurMap, maps::PALLET_TOWN);
    f.visit(5, 6);
    assert_eq!(f.reward.rank(), 3);
    assert_eq!(
        f.reward.objective(),
        Some(MapPlace::edge(maps::PALLET_TOWN, MapEdge::North)),
        "the north exit, where Oak is waiting"
    );

    // Once he has, both lab rungs are the lab.
    set_event_flag(&mut f.memory, events::EVENT_FOLLOWED_OAK_INTO_LAB);
    f.visit(5, 7);
    assert_eq!(f.reward.objective().map(|place| place.map), Some(maps::OAKS_LAB));

    // Rung 6 is the parcel *delivered*, and the errand starts at Viridian's mart: the place is the
    // counter until the parcel is in the bag, and the lab afterwards. This is the town loop's own
    // rung -- with the lab as the answer at rank 5 the catalog told a fly standing beside the lab
    // to go back into it, for ever.
    assert_eq!(rung_place(&f.reward.seen, 6).map(|place| place.map), Some(maps::VIRIDIAN_MART));
    set_event_flag(&mut f.memory, events::EVENT_GOT_OAKS_PARCEL);
    f.visit(5, 8);
    assert_eq!(rung_place(&f.reward.seen, 6).map(|place| place.map), Some(maps::OAKS_LAB));

    // Every place is a real map id, and none of them carries a tile.
    let known: Vec<u8> = RUNG_PLACES.iter().flatten().map(|place| place.map).collect();
    assert!(known.iter().all(|map| *map <= poke_max_map()), "{known:?}");
    assert!(RUNG_PLACES.iter().flatten().all(|place| place.tile.is_none()));
    // The gyms whose ids the interior ordering derives are named; the ones it does not are not.
    assert_eq!(RUNG_PLACES[11].map(|place| place.map), Some(maps::PEWTER_GYM), "BOULDER BADGE");
    assert_eq!(RUNG_PLACES[14].map(|place| place.map), Some(maps::CERULEAN_GYM), "CASCADE BADGE");
    assert_eq!(RUNG_PLACES[19], None, "THUNDER BADGE: Vermilion Gym's id is not derived");
    // And past the top of the ladder there is no next rung at all.
    assert_eq!(RUNG_PLACES.len(), RANK_LADDER.len());
    assert_eq!(rung_place(&f.reward.seen, RANK_LADDER.len()), None);
}

/// The four interior ids `docs/design/ladder.md` verified against the decomp, which is what the
/// rest of [`maps`] is counted out from: a mis-counted interior would otherwise send
/// `GO OBJECTIVE` to somebody's kitchen.
#[test]
fn the_verified_map_anchors_are_where_the_ladder_says() {
    assert_eq!(maps::VIRIDIAN_FOREST, 0x33);
    assert_eq!(maps::MT_MOON_1F, 0x3b);
    assert_eq!(maps::ROCK_TUNNEL_1F, 0x52);
    assert_eq!(maps::INDIGO_PLATEAU_LOBBY, 0xae);
    // And the ones derived between them, by the count the module documents.
    assert_eq!(maps::VIRIDIAN_MART, 0x2a);
    assert_eq!(maps::VIRIDIAN_GYM, 0x2d);
    assert_eq!(maps::PEWTER_GYM, 0x36);
    assert_eq!(maps::CERULEAN_GYM, 0x41);
}

/// The highest real map id, as the adapter's own half-loaded-frame gate uses it.
fn poke_max_map() -> u8 {
    0xf7
}

#[test]
fn a_rung_earned_out_of_order_does_not_skip_the_ones_under_it() {
    // The live save of 2026-09-17, exactly (`infra/docs/macros-traps.md` row 28). The rank is the
    // *maximum* over satisfied rungs, and the ladder's rungs are not a chain: rung 8 is a map the
    // fly can walk to, while rungs 6 and 7 are Oak's parcel delivered and the Pokédex, a script two
    // maps south. So a fly that walks to Viridian City with the parcel still in its bag reads rank
    // 8 with rungs 6 and 7 unearned — and `rank + 1` aimed `GO OBJECTIVE` at Viridian Forest,
    // north, through a gate the cartridge keeps shut until Oak has his parcel. It walked into it
    // once per hold for eight hours.
    let mut f = Fixture::booted();
    set_event_flag(&mut f.memory, events::EVENT_FOLLOWED_OAK_INTO_LAB);
    set_event_flag(&mut f.memory, events::EVENT_GOT_STARTER);
    set_event_flag(&mut f.memory, events::EVENT_GOT_OAKS_PARCEL);
    // The run the save had behind it: every map rung up to Viridian City stood on, and the parcel
    // picked up at the mart on the way.
    for (map, x, y) in [
        (maps::REDS_HOUSE_2F, 3u8, 6u8),
        (maps::REDS_HOUSE_1F, 2, 3),
        (maps::PALLET_TOWN, 5, 6),
        (maps::OAKS_LAB, 4, 4),
        (maps::VIRIDIAN_CITY, 8, 4),
    ] {
        f.memory.set(ram::wCurMap, map);
        f.visit(x, y);
    }

    assert_eq!(f.reward.rank(), 8, "the maximum over the satisfied rungs");
    assert!(
        !f.reward.seen.contains("EVENT_OAK_GOT_PARCEL"),
        "and the parcel is carried, not delivered"
    );
    // The objective is the lowest rung *not* satisfied, which is the delivery, whose place the
    // catalog already knew.
    assert_eq!(
        f.reward.objective().map(|place| place.map),
        Some(maps::OAKS_LAB),
        "the errand under the rank, not the rung above it"
    );

    // Deliver it, and the Pokédex with it, and the objective moves on to the rung the rank named
    // all along.
    set_event_flag(&mut f.memory, events::EVENT_OAK_GOT_PARCEL);
    set_event_flag(&mut f.memory, events::EVENT_GOT_POKEDEX);
    f.visit(8, 5);
    assert_eq!(f.reward.rank(), 8, "the rank has not moved: rung 9 is a map, and it is unvisited");
    assert_eq!(
        f.reward.objective().map(|place| place.map),
        Some(maps::VIRIDIAN_FOREST),
        "now the road north is the thing to do"
    );

    // And the rank still reports what it always did, which is what the stream shows and what the
    // ratchet measures: nothing about this changed the ladder.
    f.memory.set(ram::wCurMap, maps::VIRIDIAN_FOREST);
    f.visit(3, 3);
    assert_eq!(f.reward.rank(), 9);
}
