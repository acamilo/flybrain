//! The damage reward against the real cartridge, from the live row 67 checkpoint.
//!
//! Gated on `FLY_ROM` *and* on the checkpoint, the way every ROM test in this workspace is, and
//! skips cleanly without either:
//!
//! ```sh
//! FLY_ROM="$HOME/roms/pokemon-red.gb" \
//!   FLY_ROW67_CHECKPOINT=.local/checkpoints/<the row 67 Pewter checkpoint> \
//!   cargo test --release -p flysim --test rom_damage -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! The synthetic traces in `pokemon_red/tests.rs` and `pokemon_red/damage.rs` write
//! `wEnemyMonHP`, `hWhoseTurn` and `wPlayerMoveNum` from the disassembly. They cannot say that
//! `hWhoseTurn` is at `$FFF3` on this cartridge, that it reads 0 on the frame the fly's BUBBLE
//! lands and 1 on the frame the enemy's attack does, or that the enemy's HP drops on a frame the
//! adapter samples with the move that did it still in `wPlayerMoveNum`. This test does, with the
//! shipping adapter sampling once a frame -- and it restores the checkpoint's own `v7` reward
//! ledger, so it is also the `v7` -> `v8` migration on the live run's state.
//!
//! ## How the fly is moved
//!
//! Not by a brain: the shipping palette (`PokemonPalette`) deals the pad, and a harness picks. Out
//! of battle the pick is uniform over what the pad deals, which is what `row67_the_gym_trainer_is_
//! beaten_from_what_the_pad_deals` (`rom_macros_mode.rs`) showed reaches the gym's Jr. Trainer.
//! In battle it rotates the three moves -- TAIL WHIP, BUBBLE, TACKLE -- so each is used, and the
//! test asserts what each one was paid.

use std::collections::BTreeMap;

use flybrain_gb::adapter::RewardEvent;
use flybrain_gb::compatibility::{RestoreDecision, accepted_adapters, decide};
use flybrain_gb::pokemon_red::macros::PokemonPalette;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{MIGRATES_FROM, PokemonRedReward, REWARD_ADAPTER, state};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter,
    MacroPalette, MemoryReader, Started,
};

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const SEED: u32 = 20_260_929;
/// `constants/move_constants.asm`.
const TACKLE: u8 = 0x21;
const TAIL_WHIP: u8 = 0x27;
const BUBBLE: u8 = 0x91;

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_ROW67_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

fn hp(gb: &mut Emulator, address: u16) -> u16 {
    u16::from(gb.read8(address)) << 8 | u16::from(gb.read8(address + 1))
}

/// The live `v7` checkpoint is refused by this build without the opt-in and restored with it, by
/// the same decision `Sim::try_restore` makes, and its reward ledger imports.
#[test]
fn the_live_v7_checkpoint_migrates_to_v8() {
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_ROW67_CHECKPOINT");
        return;
    };
    let live = checkpoint.runtime.compatibility.clone();
    let segments: Vec<&str> = live.split('/').collect();
    assert_eq!(segments[1], "pokered-unique8-v7", "the live run is v7: {live}");
    // This build's string is the live one with the adapter segment moved and nothing else: the
    // dataset, kernel, plasticity, emulator, symbols and state format are the same build inputs.
    let mut ours = segments.clone();
    ours[1] = REWARD_ADAPTER;
    let current = ours.join("/");
    assert!(matches!(decide(&live, &current, MIGRATES_FROM, &[]), RestoreDecision::Refuse(_)));
    assert_eq!(
        decide(&live, &current, MIGRATES_FROM, &accepted_adapters(Some("pokered-unique8-v7"))),
        RestoreDecision::MigrateAdapter { from: "pokered-unique8-v7".to_string() }
    );
    let mut adapter = PokemonRedReward::new();
    adapter.import_state(&checkpoint.runtime.reward).expect("a v7 ledger is a v8 ledger");
    let state = adapter.export_state();
    assert_eq!(state["damageCounts"], serde_json::json!({}));
    assert_eq!(state["counts"]["damage"], serde_json::json!(0));
    for kind in ["milestone", "map", "trainer", "battle", "talk", "item", "catch"] {
        assert_eq!(
            state["counts"][kind], checkpoint.runtime.reward["counts"][kind],
            "counts.{kind} survives"
        );
    }
    assert_eq!(state["seen"], checkpoint.runtime.reward["seen"], "the ledger survives");
    assert_eq!(adapter.progress().rank, 10, "PEWTER CITY");
}

/// BUBBLE and TACKLE are paid when they land, TAIL WHIP never is, and the enemy's own hits land
/// while `hWhoseTurn` reads 1.
#[test]
fn damage_pays_on_bubble_and_tackle_and_not_on_tail_whip() {
    let (Some(rom), Some(checkpoint)) = (rom(), checkpoint()) else {
        eprintln!("skipped: needs FLY_ROM and FLY_ROW67_CHECKPOINT");
        return;
    };
    let mut gb = Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
    let mut adapter = PokemonRedReward::new();
    adapter.import_state(&checkpoint.runtime.reward).expect("a v7 ledger is a v8 ledger");

    let mut palette = PokemonPalette::new(SEED);
    let rotation = ["MOVE 2", "MOVE 3", "MOVE 1"];
    let mut next_move = 0usize;
    let hold_frames = 48u32;
    let mut since_decision = hold_frames;
    let mut running = false;
    let mut rng = 7u32;
    let mut ms = 0.0;

    // Per move id: moves started, enemy HP drops seen, damage events paid, HP paid for.
    let mut chosen: BTreeMap<u8, u32> = BTreeMap::new();
    let mut drops: BTreeMap<u8, u32> = BTreeMap::new();
    let mut paid: BTreeMap<u8, Vec<RewardEvent>> = BTreeMap::new();
    let mut enemy_drop_turns: BTreeMap<u8, u32> = BTreeMap::new();
    let mut own_drop_turns: BTreeMap<u8, u32> = BTreeMap::new();
    let mut last_enemy: Option<(u8, u8, u16)> = None;
    let mut last_own: Option<u16> = None;
    let mut trainer_battles = 0u32;
    let mut fighting = gb.read8(ram::wIsInBattle);
    // Frames from the `MOVE n` start to the payout it earned: how old the choice's trace is.
    let mut chose_at: Option<u32> = None;
    let mut delays: Vec<u32> = Vec::new();

    for frame in 0..90_000u32 {
        palette.clock(ms);
        let observed = {
            let ledger = AdapterLedger(&adapter);
            palette.observe(&mut gb, &ledger)
        };
        let mut mask = 0u8;
        {
            let ledger = AdapterLedger(&adapter);
            if running {
                match palette.step(&mut gb, &ledger) {
                    Some(held) => mask = held,
                    None => running = false,
                }
            } else if since_decision >= hold_frames && !observed.bindings.is_empty() {
                since_decision = 0;
                let moves: Vec<_> =
                    observed.bindings.iter().filter(|b| b.name.starts_with("MOVE ")).collect();
                let binding = if moves.is_empty() {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    &observed.bindings[(rng >> 8) as usize % observed.bindings.len()]
                } else {
                    let want = rotation[next_move % rotation.len()];
                    let pick = moves.iter().find(|b| b.name == want).unwrap_or(&moves[0]);
                    next_move += 1;
                    pick
                };
                if let Some(n) = binding.name.strip_prefix("MOVE ").and_then(|n| n.parse::<u16>().ok())
                {
                    let id = gb.read8(ram::wBattleMonMoves + n - 1);
                    *chosen.entry(id).or_default() += 1;
                    chose_at = Some(frame);
                }
                if let Started::Running(_) = palette.start(binding.slot, &mut gb, &ledger) {
                    running = true;
                    match palette.step(&mut gb, &ledger) {
                        Some(held) => mask = held,
                        None => running = false,
                    }
                }
            }
        }
        since_decision += 1;
        gb.set_buttons(mask);
        gb.run_frame().expect("a frame should complete");
        ms += MS_PER_FRAME;
        let events = adapter.sample(&mut gb, ms);

        let in_battle = gb.read8(ram::wIsInBattle);
        if fighting == 0 && in_battle == 2 {
            trainer_battles += 1;
        }
        fighting = in_battle;
        let whose = gb.read8(state::poke::H_WHOSE_TURN);
        let move_id = gb.read8(ram::wPlayerMoveNum);
        if in_battle == 2 {
            let enemy = (
                gb.read8(ram::wEnemyMonPartyPos),
                gb.read8(ram::wEnemyMonSpecies),
                hp(&mut gb, ram::wEnemyMonHP),
            );
            if let Some(before) = last_enemy
                && (before.0, before.1) == (enemy.0, enemy.1)
                && enemy.2 < before.2
            {
                *enemy_drop_turns.entry(whose).or_default() += 1;
                if whose == 0 {
                    *drops.entry(move_id).or_default() += 1;
                }
            }
            last_enemy = Some(enemy);
            let own = hp(&mut gb, ram::wBattleMonHP);
            if let Some(before) = last_own
                && own < before
            {
                *own_drop_turns.entry(whose).or_default() += 1;
            }
            last_own = Some(own);
        } else {
            last_enemy = None;
            last_own = None;
        }
        for event in events.into_iter().filter(|event| event.kind == "damage") {
            assert_eq!(whose, 0, "a damage payout on the enemy's turn: {event:?}");
            assert_eq!(in_battle, 2, "the walk to the gym has no grass: {event:?}");
            paid.entry(move_id).or_default().push(event);
            delays.extend(chose_at.map(|at| frame - at));
        }
        let hits = |id: u8| paid.get(&id).map_or(0, Vec::len);
        if hits(BUBBLE) >= 2
            && hits(TACKLE) >= 2
            && chosen.get(&TAIL_WHIP).copied().unwrap_or(0) >= 3
            && own_drop_turns.values().sum::<u32>() >= 2
        {
            break;
        }
    }

    let summary: BTreeMap<u8, Vec<(String, f64)>> = paid
        .iter()
        .map(|(id, events)| {
            (*id, events.iter().map(|event| (event.label.clone(), event.value)).collect())
        })
        .collect();
    eprintln!(
        "{:.1} brain minutes, {trainer_battles} trainer battle(s). moves chosen {chosen:?}; \
         enemy HP drops on the fly's turn by move {drops:?}; enemy drops by hWhoseTurn \
         {enemy_drop_turns:?}; own drops by hWhoseTurn {own_drop_turns:?}; damage paid by move \
         {summary:?}; frames from the MOVE press to its payout {delays:?}",
        ms / 60_000.0
    );
    assert!(trainer_battles > 0, "the run never reached the trainer's battle");
    assert!(chosen.get(&TAIL_WHIP).copied().unwrap_or(0) >= 3, "TAIL WHIP was not used");
    assert!(!paid.contains_key(&TAIL_WHIP), "TAIL WHIP was paid: {summary:?}");
    for id in [BUBBLE, TACKLE] {
        let events = paid.get(&id).map(Vec::as_slice).unwrap_or_default();
        assert!(events.len() >= 2, "move {id:#04x} was not paid twice: {summary:?}");
        for event in events {
            assert!(event.value > 0.0 && event.value <= 0.25 + 1e-12, "{event:?}");
            assert_eq!(event.stimulation_ms, 80);
        }
    }
    // The address, pinned on the cartridge: the fly's hits land on 0, the enemy's on 1.
    assert_eq!(enemy_drop_turns.get(&1), None, "the enemy lost HP on its own turn");
    assert!(own_drop_turns.get(&1).copied().unwrap_or(0) >= 2, "{own_drop_turns:?}");
    assert_eq!(own_drop_turns.get(&0), None, "the fly lost HP on its own turn: {own_drop_turns:?}");
    // Every drop on the fly's turn was paid, and only those.
    for (id, count) in &drops {
        assert_eq!(
            paid.get(id).map_or(0, Vec::len) as u32,
            *count,
            "move {id:#04x}: every drop it caused is one payout"
        );
    }
}
