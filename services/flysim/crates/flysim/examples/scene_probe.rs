//! Drive macros mode from a checkpoint until the scene detector sticks, and dump every input of
//! the branch it stuck on.
//!
//! `infra/docs/macros-traps.md` (2026-09-17): with the walk fixes in, the fly reaches the mart and
//! then spends the rest of the run in `Scene::Dialog` — 41,012 frames of 71,673 — while the reward
//! adapter's own mode reads `OVERWORLD`. Two readings fit that: a text box whose text one A press
//! and one B press cannot advance, or a `dialog` reading that is wrong. They are told apart by the
//! bytes, so this prints the bytes.
//!
//! The readout is `tests/rom_macros_mode.rs`'s stub — one macro population hot per hold, rotating
//! — so this needs no connectome and runs in seconds. What it measures is the detector, and the
//! detector cannot tell what is driving it.
//!
//! **What it found, and its limit.** Driven by the stub the scene never holds one non-overworld
//! reading for 600 frames in 200,000, so the residual needed the real connectome to reproduce and
//! `examples/trap_hunt.rs`'s per-frame counters are the measurement that settled it (0 frames the
//! corner test would have called `dialog` without a border, 315 frames where the map drew all four
//! corners with the font flag clear). This is the tool for dumping every byte of the branch at one
//! spot; the hunt is the tool for counting.
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-timeouts.checkpoint \
//!   cargo run --release -p flysim --example scene_probe
//! ```
//!
//! | env | default | meaning |
//! | --- | --- | --- |
//! | `FLY_ROM` | — | the cartridge; without it this prints instructions |
//! | `FLY_TRAP_CHECKPOINT` | `FLY_MACRO_CHECKPOINT` | the `FLYSIM01` checkpoint to start from |
//! | `FLY_PROBE_FRAMES` | 200000 | frames to drive before giving up |
//! | `FLY_PROBE_STUCK` | 600 | consecutive frames in one non-overworld scene that count as stuck |

use std::collections::BTreeMap;

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, scene, state};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter,
    MemoryReader,
};
use flysim::config::Config;
use flysim::macros::macro_layer;
use flysim::snapshot::MacroMode;

#[path = "support/ledgers.rs"]
mod ledgers;

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const BURST_MS: f64 = 100.0;
const HOLDS_PER_SLOT: usize = 3;
const HOT: f64 = 16.0;
const REST: f64 = 10.0;
const SEED: u32 = 20_260_916;

fn rates(hot: Option<&str>) -> NumberMap {
    let mut rates = NumberMap::new();
    for channel in flybrain_gb::macro_channels("pokemon-red") {
        rates.set(channel, REST);
    }
    for bucket in 0..8 {
        rates.set(&format!("motor_{bucket}"), REST);
    }
    if let Some(hot) = hot {
        rates.set(hot, HOT);
    }
    rates
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Every byte the dialog branch of [`scene::detect`] rests on, plus the tiles it reads.
fn dump(gb: &mut Emulator, label: &str) {
    let text = state::text_box(gb);
    let corners: Vec<String> = [(0u16, 12u16), (19, 12), (0, 17), (19, 17)]
        .into_iter()
        .map(|(x, y)| {
            format!("({x},{y})={:#04x}", gb.read8(ram::wTileMap + y * 20 + x))
        })
        .collect();
    println!("\n## {label}");
    println!("- scene: `{:?}`", scene::detect(gb));
    println!("- text box: open={} waiting={}", text.open, text.waiting);
    println!("- box corners (the `waiting` test): {}", corners.join(" "));
    println!("- `{}`", scene::why_unknown(gb));
    println!("- start menu: {:?}", state::start_menu(gb).is_some());
    println!("- submenu: {:?}", state::submenu(gb));
    println!("- pc: {:?} shop: {:?}", state::pc(gb).is_some(), state::shop(gb).is_some());
    println!("- controllable: {}", state::controllable(gb));
    println!("- player: {:?}", state::player(gb));
    println!("- sprites the macros can see: {:?}", state::npcs(gb));
    println!("\n```");
    for y in 10..18u16 {
        let row: Vec<String> = (0..20u16)
            .map(|x| format!("{:02x}", gb.read8(ram::wTileMap + y * 20 + x)))
            .collect();
        println!("row {y:2}  {}", row.join(" "));
    }
    println!("```");
}

/// What the save says about the plot, and who is standing on this map.
///
/// The question row 28 of `infra/docs/macros-traps.md` asks is "what unlocks the road north", and
/// the cartridge answers it in two places: the event bitsets, and the map's own object list. Both
/// are read here rather than recalled — the macros never learn any of it, but the *investigation*
/// has to know which script is talking.
fn cartridge(gb: &mut Emulator, adapter: &PokemonRedReward, label: &str) {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
    let progress = adapter.progress();
    println!(
        "\n## {label}\n\n- rank {} ({}), badges {}, unique tiles {}",
        progress.rank, progress.rank_label, progress.counter, progress.unique_locations
    );
    let set: Vec<&str> = flybrain_gb::pokemon_red::symbols::EVENTS
        .iter()
        .filter(|(_, bit)| {
            gb.read8(ram::wEventFlags + (bit >> 3)) & (1 << (bit & 7)) != 0
        })
        .map(|(name, _)| *name)
        .collect();
    println!("- {} named events set", set.len());
    for name in &set {
        println!("  - {name}");
    }
    let ledger = AdapterLedger(adapter);
    let mut state = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let state: &mut dyn MacroState = &mut state;
    println!("\n- objective: {:?}", state.objective());
    println!("- map size: {:?}", state.map_size());
    println!("- warps: {:?}", state.warps());
    println!("- connections: {:?}", state.connections());
    println!("- signs: {:?}", state.signs());
    println!("- people and objects on this map:");
    for npc in state.npcs() {
        println!(
            "  - slot {:2} picture {:#04x} at ({:2}, {:2}) facing {:?}{}",
            npc.slot,
            npc.picture,
            npc.x,
            npc.y,
            npc.facing,
            if npc.person() { "" } else { "  (an object, not a person)" }
        );
    }
}

/// Everything the overworld pad rests on, for the map the checkpoint is standing on.
///
/// The 2026-09-17 rank-9 stall had a pad of one button — `GO FRONTIER` — in a ten-by-eight gate
/// house, so the question is which candidate list is empty and which is not. Each list is printed
/// whole rather than summarised: the frontier tiles with the ground they border, the exits with
/// the way they are classified and the map each resolves to, and the first hop the map graph
/// answers toward the objective.
fn pad(gb: &mut Emulator, adapter: &PokemonRedReward, label: &str) {
    use flybrain_gb::pokemon_red::macros::cartridge::{MacroState, TargetKey};
    use flybrain_gb::pokemon_red::macros::{geography, palette, path, plan};
    use flybrain_gb::pokemon_red::macros::state::Walkable;

    // Why the grid could not be decoded, read before the state borrows the emulator: it is the
    // same call `MacroState::map_grid` makes, and the only reading that can say *which* of section
    // 15's refusals a frame is.
    let refusal = flybrain_gb::pokemon_red::state::map_grid(gb).err();
    // Which tile the decode and the screen disagree about, when that is the refusal. The label
    // alone cannot tell "the grid is wrong on this map" from "this frame was mid-warp", and the
    // two want opposite fixes (`docs/design/macros.md` section 15). Read here, beside the refusal
    // itself, because everything below holds a borrow of the emulator.
    let disagreement = match refusal {
        Some(flybrain_gb::pokemon_red::state::GridRefusal::ScreenDisagrees) => {
            flybrain_gb::pokemon_red::state::grid_disagreement(gb)
        }
        _ => Vec::new(),
    };
    let ledger = AdapterLedger(adapter);
    let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let state: &mut dyn MacroState = &mut poke;
    let scene = state.scene();
    let Some(player) = state.player() else {
        println!("\n## {label}\n\n- no loaded map");
        return;
    };
    let size = state.map_size().expect("a loaded map has a size");
    println!("\n## {label}\n");
    println!("- scene `{scene:?}`, player {player:?}, map {}x{}", size.width, size.height);
    println!("- objective: {:?}", state.objective());
    if let Some(objective) = state.objective() {
        println!(
            "- `next_hop({:?}, {:#04x})` = {:?}, neighbours {:?}",
            geography::region_at(player.map, player.y),
            objective.map,
            geography::next_hop(geography::region_at(player.map, player.y), objective.map),
            geography::neighbours(player.map)
        );
    }
    let plan = plan::plan_for(scene, state);
    let names: Vec<&str> =
        plan.slots.iter().flatten().map(|spec| spec.name).collect();
    println!("- the pad: {names:?}");
    // The whole-map grid (`docs/design/macros.md` section 15), which is what the walks plan over
    // now. Three numbers read a stalled walk: how much of the map is ground, how much of that the
    // fly can actually get to from where it stands, and how much of *that* it has never stood on.
    // A fly with 600 walkable tiles and 4 reachable ones is fenced in and no re-plan will help.
    match state.map_grid() {
        None => println!(
            "- the map grid: none ({})",
            refusal.map_or("unknown", |refusal| refusal.label())
        ),
        Some(grid) => {
            let unstood = grid
                .walkable_tiles()
                .into_iter()
                .filter(|(x, y)| !state.tile_visited(*x, *y))
                .count();
            println!(
                "- the map grid: {}x{} walkable {} reachable {} unstood {} unknown {}",
                grid.width(),
                grid.height(),
                grid.walkable_count(),
                grid.reachable_from(player.x, player.y),
                unstood,
                grid.unknown_count()
            );
        }
    }
    // Which tile the decode and the screen disagree about, when that is the refusal. The label
    // alone cannot tell "the grid is wrong on this map" from "this frame was mid-warp", and the
    // two want opposite fixes (`docs/design/macros.md` section 15).
    if !disagreement.is_empty() {
        println!("- the decode against the screen, tile by tile:");
        for (x, y, decoded, screen) in disagreement {
            println!(
                "  - ({x:2}, {y:2}) decoded {decoded:?} screen {screen:?}{}",
                if decoded.is_some() && screen.is_some() && decoded != screen {
                    "   <- the disagreement"
                } else {
                    ""
                }
            );
        }
    }
    // The `v` column is the *adapter's* lifetime exploration ledger and nothing else. The
    // session's own stood ledger (`docs/design/macros.md` section 12.7) is owned by the driver's
    // palette, which this probe does not reach into, so a doormat the running fly has already
    // marked still prints as unrecorded here. That is the point of the column: it shows what the
    // reward ledger can and cannot answer.
    // The grid's reading of the ground where there is one, the window's otherwise, said out loud
    // so the map below cannot be mistaken for the other reading.
    let grid = state.map_grid();
    println!(
        "\n### The ground, as the {} reads it (`v` = the adapter's lifetime ledger only)\n\n```",
        if grid.is_some() { "map grid" } else { "ten-by-nine window" }
    );
    for y in 0..size.height {
        let row: Vec<String> = (0..size.width)
            .map(|x| {
                let answer = match grid.as_deref() {
                    Some(grid) => grid.walkable(x, y),
                    None => state.walkable(x, y),
                };
                let walk = match answer {
                    Walkable::Yes => '.',
                    Walkable::No => '#',
                    Walkable::Unknown => '?',
                };
                let seen = if state.tile_visited(x, y) { 'v' } else { '-' };
                let here = player.x == x && player.y == y;
                format!("{}{}{}", walk, seen, if here { '@' } else { ' ' })
            })
            .collect();
        println!("y{y:2}  {}", row.join(""));
    }
    println!("```\n");
    println!("- walkable and never stood on: {}", {
        let mut n = 0;
        for y in 0..size.height {
            for x in 0..size.width {
                let answer = match grid.as_deref() {
                    Some(grid) => grid.walkable(x, y),
                    None => state.walkable(x, y),
                };
                if answer == Walkable::Yes && !state.tile_visited(x, y) {
                    n += 1;
                }
            }
        }
        n
    });
    println!("- `path::frontier`: {:?}", path::frontier(state));
    println!("- `frontier_aims`: {:?}", palette::frontier_aims(state));
    println!("\n### The ways out\n");
    for exit in path::exits(state) {
        println!(
            "- {:?} at ({:2},{:2}) press {:?} way {:?} into {:?} -> destination {:?} \
             (map_visited {:?}, exit_visited {}, blocked {})",
            exit.id,
            exit.tile.x,
            exit.tile.y,
            exit.press,
            exit.way,
            exit.into,
            exit.destination(player.map),
            exit.destination(player.map).map(|map| state.map_visited(map)),
            state.exit_visited(exit.id),
            state.blocked(TargetKey::Exit(exit.id)),
        );
    }
    for way in [path::Way::Exit, path::Way::Passage, path::Way::Route] {
        let ways = palette::ways(state, way);
        println!(
            "- `ways({way:?})` = {:?}",
            ways.iter().map(|exit| exit.id).collect::<Vec<_>>()
        );
    }
    // Section 13's two errands, which is what row 54's `GO HEAL` cycle turns on: which building
    // the errand names, whether the ledgers have paid it, and what the walk would aim at. An aim
    // with no press settles where it stands, so an aim on the fly's own tile is a macro that
    // completes without moving.
    println!("\n### The errands\n");
    println!("- `area_here` = {:?}", palette::area_here(state));
    for kind in [geography::Amenity::Mart, geography::Amenity::Center] {
        let at = geography::amenity_of(palette::area_here(state).unwrap_or(0), kind);
        let paid = at.is_some_and(|map| state.map_visited(map));
        println!(
            "- {kind:?}: building {:?} (map_visited {paid}), `errand` = {:?}, `amenity_goals` = {:?}",
            at,
            palette::errand(state, kind),
            palette::amenity_goals(state, kind),
        );
    }
    println!("- `counter_pending` = {}", palette::counter_pending(state));
    println!("- `heal_goals` = {:?}", palette::heal_goals(state));
    println!("- `errand_place` = {:?}", palette::errand_place(state));
    println!("- `stranded` = {}", palette::stranded(state));
    println!();
    println!("- `objective_goals` = {:?}", palette::objective_goals(state));
    println!("- `objective_targets` = {:?}", palette::objective_targets(state));
    println!("- `untalked_people` = {:?}", palette::untalked_people(state));
    println!("- `untalked_objects` = {:?}", palette::untalked_objects(state));
}

/// The battle the fly cannot get out of: every move with its PP, the party, and the pad.
///
/// `FLY_PROBE_CATCH=noattack` drives until the fly's own turn has had `ATTACK` *off* the pad for a
/// run of frames and then prints this. `ATTACK`'s precondition over the top-level menu is
/// `best_move(..).is_some()` and `best_move` skips a move with no PP, so a turn with nothing left
/// to attack with takes FIGHT off the pad — and the move list `ATTACK` would confirm Struggle from
/// is only ever reached *through* FIGHT.
fn battle_dump(gb: &mut Emulator, adapter: &PokemonRedReward, pad: &[String], label: &str) {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;

    let ledger = AdapterLedger(adapter);
    let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let state: &mut dyn MacroState = &mut poke;
    println!("\n## {label}\n");
    println!("- scene: `{:?}`", state.scene());
    println!("- the pad: {pad:?}");
    println!("- player: {:?}", state.player());
    let battle = state.battle();
    println!("- battle: {:?}", battle.as_ref().map(|battle| (battle.kind, battle.own_turn, battle.forced_switch, battle.menu)));
    println!("- enemy: {:?}", battle.as_ref().and_then(|battle| battle.enemy));
    if let Some(mon) = battle.as_ref().and_then(|battle| battle.own) {
        println!("- the Pokémon that is out: slot {} species {:#04x} level {} hp {}/{} status {:?}", mon.slot, mon.species, mon.level, mon.hp, mon.max_hp, mon.status);
        for (index, slot) in mon.moves.iter().enumerate() {
            println!("  - move {index}: {slot:?}");
        }
    }
    let party = state.party();
    println!("- party of {}:", party.mons.len());
    for mon in &party.mons {
        println!(
            "  - slot {} species {:#04x} level {} hp {}/{} moves {:?}",
            mon.slot, mon.species, mon.level, mon.hp, mon.max_hp, mon.moves
        );
    }
    println!("- bag: {:?}", state.bag());
    println!("- money: {}", state.money());
}

/// One line of the dialogue box, decoded through `constants/charmap.asm`.
///
/// The survey below is about *which box* is open, and the only thing on screen that says so is the
/// text in it: `wTextBoxID` is `$01` for every ordinary `TX_FAR` box the nurse draws, so the id
/// cannot tell the welcome from the prompt from the closing line. The tiles can.
fn box_line(gb: &mut Emulator, y: u16) -> String {
    (1..19u16)
        .map(|x| match gb.read8(ram::wTileMap + y * 20 + x) {
            0x7f => ' ',
            byte @ 0x80..=0x99 => (b'A' + (byte - 0x80)) as char,
            byte @ 0xa0..=0xb9 => (b'a' + (byte - 0xa0)) as char,
            0xba => 'e',
            0xe3 => '-',
            0xe6 => '?',
            0xe7 => '!',
            0xe8 => '.',
            0xef => 'M',
            0xee => '\u{25bc}',
            byte @ 0xf6..=0xff => (b'0' + (byte - 0xf6)) as char,
            _ => '.',
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The top-right corner of the screen, where a two-option menu's own little box is drawn: the
/// tiles at (11..20, 6..11) reduced to which of them hold a text-box frame tile.
fn corner_box(gb: &mut Emulator) -> String {
    let mut out = String::new();
    for y in 6..12u16 {
        for x in 11..20u16 {
            let byte = gb.read8(ram::wTileMap + y * 20 + x);
            out.push(match byte {
                0x79 | 0x7b | 0x7d | 0x7e => '+',
                0x7a | 0x7c => '|',
                0x7f => '_',
                _ => '.',
            });
        }
        out.push('/');
    }
    out
}

/// Everything that tells one of the nurse's boxes from another, on one line.
fn nurse_frame(gb: &mut Emulator) -> String {
    let text = state::text_box(gb);
    format!(
        "{:?} open={} waiting={} cursor=({},{},{},{},{:#04x}) yesno={} | {} | {}",
        scene::detect(gb),
        text.open,
        text.waiting,
        gb.read8(ram::wTopMenuItemY),
        gb.read8(ram::wTopMenuItemX),
        gb.read8(ram::wCurrentMenuItem),
        gb.read8(ram::wMaxMenuItem),
        gb.read8(ram::wMenuWatchedKeys),
        corner_box(gb),
        box_line(gb, 14),
        box_line(gb, 16),
    )
}

/// The Pokémon Center nurse's whole conversation, box by box, with raw presses (row 41).
///
/// The rung-10 loop of 2026-09-22 was `YES` 2,142 macro starts on one tile of map `0x3a`, so the
/// question the fix turns on is **which box each A press answers**. `wTextBoxID` cannot say --
/// every box the nurse draws is `$01` -- and `docs/design/macros-wram.md` says outright that there
/// is no "a choice is open" flag, so the reading has to be surveyed: leave the box with B, then
/// pulse A and print every state the conversation passes through, with the two-option menu's own
/// geometry beside it. The party is printed first because `HEAL`'s precondition is the party and
/// the loop's premise is that it is already full.
fn nurse_survey(gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64) {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;

    println!("\n## The party at the checkpoint\n");
    {
        let ledger = AdapterLedger(adapter);
        let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
        let state: &mut dyn MacroState = &mut poke;
        for mon in &state.party().mons {
            println!(
                "- slot {} species {:#04x} level {} hp {}/{} status {:?}",
                mon.slot, mon.species, mon.level, mon.hp, mon.max_hp, mon.status
            );
        }
        println!(
            "- `party_needs_rest` = {}, `party_rested` = {}",
            flybrain_gb::pokemon_red::macros::palette::party_needs_rest(state),
            flybrain_gb::pokemon_red::macros::palette::party_rested(state),
        );
    }

    let pulse = |gb: &mut Emulator, adapter: &mut PokemonRedReward, mask: u8, ms: &mut f64| {
        for phase in 0..16 {
            gb.set_buttons(if phase < 8 { mask } else { 0 });
            gb.run_frame().expect("a frame should complete");
            *ms += MS_PER_FRAME;
            adapter.sample(gb, *ms);
        }
    };

    println!("\n## The nurse's conversation, one raw A pulse at a time\n");
    println!("- at the checkpoint: {}", nurse_frame(gb));
    for _ in 0..20 {
        if scene::detect(gb) == scene::Scene::Overworld {
            break;
        }
        pulse(gb, adapter, flybrain_gb::buttons::B, ms);
    }
    println!("- after B until the box closes: {}", nurse_frame(gb));
    println!("\n```");
    let mut last = String::new();
    for index in 0..env_usize("FLY_PROBE_PULSES", 120) {
        pulse(gb, adapter, flybrain_gb::buttons::A, ms);
        let now = nurse_frame(gb);
        if now != last {
            println!("A#{index:<3} {now}");
            last = now;
        }
    }
    println!("```");
}

/// The survey the whole-map grid's mid-step refusal turns on (`infra/docs/macros-traps.md` row 54).
///
/// Section 15 checks the decode against the screen buffer over the fly's own tile and its four
/// neighbours, and the residual of 2026-09-22 measured that check refusing on **every frame the
/// fly is mid-step**: standing still Pewter City decoded on 118 of 120 frames, and the one frame
/// the survey caught disagreed by exactly one tile row in the direction of travel. A walk planned
/// on such a frame is planned over the ten-by-nine window, which is the oscillation of row 23.
///
/// Two things have to be measured before that can be fixed honestly, and neither can be argued
/// from the disassembly alone:
///
/// 1. **when `wXCoord` / `wYCoord` change** — at the start of a step or at the end of it. That
///    decides whether the screen is behind the coordinates or the coordinates ahead of the screen.
/// 2. **which byte says "a step is in progress"**. `docs/design/macros-wram.md` says the reviewed
///    symbol list carries none, so every plausible candidate is dumped across a whole step and the
///    one that tracks it is the reading.
///
/// It holds one direction from the checkpoint and prints a line per frame: the coordinates, the
/// grid's verdict, the tiles the cross-check disagreed on, and the candidates.
fn step_survey(gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64) {
    use flybrain_gb::pokemon_red::macros::state::Walkable;

    let frames = env_usize("FLY_PROBE_STEP_FRAMES", 96);
    let step = |gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64, mask: u8| {
        gb.set_buttons(mask);
        gb.run_frame().expect("a frame should complete");
        *ms += MS_PER_FRAME;
        adapter.sample(gb, *ms);
    };

    // Somewhere the fly is its own master and the map is on screen, so that a refusal below is
    // about the step and not about a text box.
    for _ in 0..600 {
        if scene::detect(gb) == scene::Scene::Overworld && state::controllable(gb) {
            break;
        }
        step(gb, adapter, ms, flybrain_gb::buttons::B);
    }

    let Some(here) = state::player(gb) else {
        println!("\nNo player at the checkpoint, so there is no step to survey.");
        return;
    };
    println!("\n## The mid-step survey, on map {:#04x} from ({}, {})\n", here.map, here.x, here.y);

    // A direction with walkable ground on the other side of it, so the hold is a step rather than
    // a turn into a wall.
    let facings = [
        (flybrain_gb::buttons::DOWN, 0i16, 1i16, "DOWN"),
        (flybrain_gb::buttons::UP, 0, -1, "UP"),
        (flybrain_gb::buttons::LEFT, -1, 0, "LEFT"),
        (flybrain_gb::buttons::RIGHT, 1, 0, "RIGHT"),
    ];
    let mut chosen = None;
    for (mask, dx, dy, name) in facings {
        let (Ok(x), Ok(y)) =
            (u8::try_from(i16::from(here.x) + dx), u8::try_from(i16::from(here.y) + dy))
        else {
            continue;
        };
        if state::walkable(gb, x, y) == Walkable::Yes {
            chosen = Some((mask, name));
            break;
        }
    }
    let Some((mask, name)) = chosen else {
        println!("Every neighbour of the fly is a wall, so there is no step to survey.");
        return;
    };
    println!("Holding {name} for {frames} frames.\n");
    println!("```");
    println!(
        "frame  coords    grid                            s1+1,+3,+5,+7,+8,+9  scy scx  cfc5 d730 d736"
    );
    for frame in 0..frames {
        let sprite: Vec<String> = [1u16, 3, 5, 7, 8, 9]
            .into_iter()
            .map(|offset| format!("{:02x}", gb.read8(ram::wSpriteStateData1 + offset)))
            .collect();
        let scy = gb.read8(0xff42);
        let scx = gb.read8(0xff43);
        let cfc5 = gb.read8(0xcfc5);
        let d730 = gb.read8(ram::wStatusFlags5);
        let d736 = gb.read8(ram::wMovementFlags);
        let verdict = match state::map_grid(gb) {
            Ok(_) => "ok".to_string(),
            Err(refusal) => {
                let shown: Vec<String> = state::grid_disagreement(gb)
                    .into_iter()
                    .filter(|(_, _, decoded, screen)| decoded != screen)
                    .map(|(x, y, decoded, screen)| format!("({x},{y}){decoded:?}/{screen:?}"))
                    .collect();
                format!("{} {}", refusal.label(), shown.join(" "))
            }
        };
        let coords = state::player(gb)
            .map(|player| format!("({:>2},{:>2})", player.x, player.y))
            .unwrap_or_else(|| "  none  ".to_string());
        println!(
            "{frame:>5}  {coords}  {verdict:<30}  {}  {scy:>3} {scx:>3}  {cfc5:02x}   {d730:02x}   {d736:02x}",
            sprite.join(",")
        );
        step(gb, adapter, ms, mask);
    }
    println!("```");
}

/// Ground truth for "this menu is accepting input", measured rather than read off a flag.
///
/// The emulator exports its own state, one directional pulse is issued into it, and
/// `wCurrentMenuItem` is read: `HandleMenuInput` moves the cursor on UP and DOWN before it even
/// looks at `wMenuWatchedKeys`, so a cursor that moves is a menu that is running its input loop and
/// a cursor that does not is a menu nobody is reading. The state goes straight back afterwards, so
/// the run this is measured inside is not perturbed by the measurement.
fn press_honoured(gb: &mut Emulator) -> bool {
    let save = gb.export_state().expect("the emulator should export its own state");
    let before = gb.read8(ram::wCurrentMenuItem);
    let mask = if before < gb.read8(ram::wMaxMenuItem) {
        flybrain_gb::buttons::DOWN
    } else {
        flybrain_gb::buttons::UP
    };
    // Released first, and that is not cosmetic. `JoypadLowSensitivity` acts on a key's *edge*, so a
    // direction the fly is already holding when this pulse begins produces no press at all and the
    // frame reads as refused for a reason that is the measurement's and not the cartridge's. The
    // first survey of row 50 measured 187 such frames before this line existed.
    for phase in 0..ACCEPT_PULSE {
        gb.set_buttons(if (8..22).contains(&phase) { mask } else { 0 });
        gb.run_frame().expect("a frame should complete");
    }
    let moved = gb.read8(ram::wCurrentMenuItem) != before;
    gb.import_state(&save).expect("the emulator should take its own state back");
    moved
}

/// Frames of the rollback pulse [`press_honoured`] issues: released, held, released.
const ACCEPT_PULSE: usize = 30;

/// Whether the cursor bytes say "the move list", which is all the seam read before row 50.
fn move_cursor_geometry(gb: &mut Emulator) -> bool {
    gb.read8(ram::wTopMenuItemY) == 12 && gb.read8(ram::wTopMenuItemX) == 5
}

/// Every byte of WRAM and HRAM, as two sets of values per address.
///
/// The question row 50 asks is "which byte flips exactly when a press is honoured", and the honest
/// way to answer it is not to nominate candidates but to let every address answer: an address whose
/// values on accepting frames never once overlap its values on refusing frames *is* the reading,
/// and one that overlaps is not, however plausible its name.
struct Separator {
    seen: BTreeMap<u16, [[u64; 4]; 2]>,
    counts: [usize; 2],
}

impl Separator {
    fn new() -> Self {
        Self { seen: BTreeMap::new(), counts: [0, 0] }
    }

    fn observe(&mut self, gb: &mut Emulator, honoured: bool) {
        let class = usize::from(honoured);
        self.counts[class] += 1;
        for address in (0xc000u16..0xe000).chain(0xff80u16..0xffff) {
            let value = gb.read8(address);
            let bits = self.seen.entry(address).or_insert([[0; 4]; 2]);
            bits[class][usize::from(value) / 64] |= 1u64 << (u32::from(value) % 64);
        }
    }

    /// The addresses whose two value sets never overlap, smallest sets first.
    fn disjoint(&self) -> Vec<(u16, Vec<u8>, Vec<u8>)> {
        let mut out: Vec<(u16, Vec<u8>, Vec<u8>)> = self
            .seen
            .iter()
            .filter(|(_, bits)| {
                (0..4).all(|word| bits[0][word] & bits[1][word] == 0)
                    && bits[0].iter().any(|word| *word != 0)
                    && bits[1].iter().any(|word| *word != 0)
            })
            .map(|(address, bits)| (*address, values(&bits[0]), values(&bits[1])))
            .collect();
        out.sort_by_key(|(address, refused, honoured)| {
            (refused.len() + honoured.len(), *address)
        });
        out
    }
}

/// A 256-bit set back as the byte values in it, capped so a report line stays a line.
fn values(bits: &[u64; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in 0..=255u16 {
        if bits[usize::from(value) / 64] & (1u64 << (u32::from(value) % 64)) != 0 {
            out.push(value as u8);
        }
        if out.len() >= 9 {
            break;
        }
    }
    out
}

/// What the seam makes of this battle frame, in the shape the pad is dealt from.
fn battle_reading(gb: &mut Emulator, adapter: &PokemonRedReward) -> Option<(String, bool, bool)> {
    use flybrain_gb::pokemon_red::macros::state::BattleMenu;

    let ledger = AdapterLedger(adapter);
    let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let battle = flybrain_gb::pokemon_red::macros::state::GameState::battle(&mut poke)?;
    let name = match battle.menu {
        BattleMenu::None => "none".to_string(),
        BattleMenu::Main { cursor } => format!("main[{cursor}]"),
        BattleMenu::Moves { cursor: Some(slot), count } => format!("moves[{slot}/{count}]"),
        BattleMenu::Moves { cursor: None, count } => format!("moves[?/{count}]"),
        BattleMenu::Party { cursor } => format!("party[{cursor}]"),
        BattleMenu::Bag { cursor, count } => format!("bag[{cursor}/{count}]"),
    };
    Some((name, battle.own_turn, battle.forced_switch))
}

/// Whether the move list's own box is on screen, by the two tiles only it draws.
///
/// `MoveSelectionMenu`'s regular menu is a `TextBoxBorder` at (4, 12) fourteen wide, with the
/// junction tile written over (10, 12) afterwards. A plain battle text box is the full width of the
/// screen, so (10, 12) is a horizontal run and (4, 13) is inside it; the top-level battle menu's
/// own box starts at column 8. Either mark alone is ambiguous; together they are the move list.
fn move_box_drawn(gb: &mut Emulator) -> bool {
    let corner = gb.read8(ram::wTileMap + 12 * 20 + 10);
    let wall = gb.read8(ram::wTileMap + 13 * 20 + 4);
    matches!(corner, 0x79 | 0x7b | 0x7d | 0x7e) && wall == 0x7c
}

/// The whole screen as border tiles, the menu cursor and "some text", one row per line.
fn screen_rows(gb: &mut Emulator) -> String {
    (0..18u16)
        .map(|y| {
            let row: String = (0..20u16)
                .map(|x| match gb.read8(ram::wTileMap + y * 20 + x) {
                    0x7f => '.',
                    0x79 | 0x7b | 0x7d | 0x7e => '+',
                    0x7a => '-',
                    0x7c => '|',
                    0xed => '>',
                    _ => 'x',
                })
                .collect();
            format!("    {y:>2} {row}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tiles of the six rows a battle's bottom boxes are drawn in, as one line.
fn box_rows(gb: &mut Emulator) -> String {
    (12..18u16)
        .map(|y| {
            (0..20u16)
                .map(|x| match gb.read8(ram::wTileMap + y * 20 + x) {
                    0x7f => '.',
                    0x79 | 0x7b | 0x7d | 0x7e => '+',
                    0x7a => '-',
                    0x7c => '|',
                    0xed => '>',
                    _ => 'x',
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Row 50's survey: which reading says a battle menu is accepting input, and which only says it is
/// drawn.
///
/// `infra/docs/macros-traps.md` row 50: `MOVE n` reports `blocked` 890 times in 1,431 macros, every
/// one of them on a move list the seam could place a cursor in. Two readings fit that -- a list
/// that is up and busy, or cursor bytes that outlive the list they were written for -- and they are
/// told apart by pressing at it, so this presses at it: every battle frame is classified by whether
/// a real directional press moves the cursor, and every byte of WRAM and HRAM is asked whether it
/// separates the two classes.
fn accept_survey(
    gb: &mut Emulator,
    adapter: &mut PokemonRedReward,
    ms: &mut f64,
    layer: &mut flysim::macros::MacroLayer,
    decoder: &mut PopulationDecoder,
    channels: &[String],
    hold_ms: f64,
) {
    let budget = env_usize("FLY_PROBE_FRAMES", 200_000);
    let samples = env_usize("FLY_PROBE_SAMPLES", 3_000);
    let trace = env_usize("FLY_PROBE_TRACE", 160);
    let mut next_burst = *ms;
    let mut burst = 0usize;
    let mut separators: BTreeMap<&'static str, Separator> = BTreeMap::new();
    let mut tally: BTreeMap<(String, bool), [usize; 2]> = BTreeMap::new();
    let mut shown: BTreeMap<(String, bool), String> = BTreeMap::new();
    let mut traced = 0usize;
    let mut tested = 0usize;
    // [box not drawn, box drawn] x [press refused, press honoured], over every frame whose cursor
    // bytes say "the move list" -- which is the whole of what the seam read before row 50.
    let mut readings = [[0usize; 2]; 2];

    println!("\n## Row 50: every battle frame, pressed at\n");
    println!("```");
    println!(
        "frame  seam                turn  honoured  ccyx/cur/max/keys  d125 cf94 cd6c cfc4  boxes"
    );
    for _ in 0..budget {
        let bursting = *ms < next_burst + BURST_MS;
        let hot = bursting.then(|| channels[(burst / HOLDS_PER_SLOT) % channels.len()].as_str());
        if *ms >= next_burst + hold_ms {
            next_burst = *ms;
            burst += 1;
        }
        let bound = layer.bound_channels();
        let active = decoder.decode_bound(&rates(hot), *ms, false, None, Some(&bound));
        let mask = {
            let ledger = AdapterLedger(adapter);
            layer.decide(&active, 0, *ms, gb, &ledger).mask
        };
        gb.set_buttons(mask as u8);
        gb.run_frame().expect("a frame should complete");
        *ms += MS_PER_FRAME;
        adapter.sample(gb, *ms);
        {
            let ledger = AdapterLedger(adapter);
            let _ = layer.observe(gb, &ledger, *ms);
        }

        let Some((name, own_turn, forced)) = battle_reading(gb, adapter) else { continue };
        let geom = move_cursor_geometry(gb);
        if (name == "none" && !geom) || forced {
            continue;
        }
        if tested >= samples {
            break;
        }
        tested += 1;
        let honoured = press_honoured(gb);
        let drawn = move_box_drawn(gb);
        if geom {
            readings[usize::from(drawn)][usize::from(honoured)] += 1;
        }
        let key = (format!("{name} drawn={drawn}"), own_turn);
        tally.entry(key.clone()).or_insert([0, 0])[usize::from(honoured)] += 1;
        let kind = if name.starts_with("moves") {
            "the move list"
        } else if name.starts_with("main") {
            "the top-level menu"
        } else if name.starts_with("bag") {
            "the bag"
        } else {
            "the party list"
        };
        separators.entry(kind).or_insert_with(Separator::new).observe(gb, honoured);
        let boxes = box_rows(gb);
        shown.entry((name.clone(), honoured)).or_insert_with(|| screen_rows(gb));
        if traced < trace {
            traced += 1;
            println!(
                "{tested:>5}  {name:<18}  {:<4}  {:<8}  {:>2},{:>2},{:>2},{:>2},{:#04x}  \
                 {:02x}   {:02x}   {:02x}   {:02x}    {boxes}",
                own_turn,
                honoured,
                gb.read8(ram::wTopMenuItemY),
                gb.read8(ram::wTopMenuItemX),
                gb.read8(ram::wCurrentMenuItem),
                gb.read8(ram::wMaxMenuItem),
                gb.read8(ram::wMenuWatchedKeys),
                gb.read8(ram::wTextBoxID),
                gb.read8(ram::wListMenuID),
                gb.read8(ram::wNumMovesMinusOne),
                gb.read8(ram::wFontLoaded),
            );
        }
    }
    println!("```");

    let (stale, live) = (readings[0], readings[1]);
    println!("\n## The move list, by which reading says it is up\n");
    println!("| the reading | press refused | press honoured |");
    println!("| --- | ---: | ---: |");
    println!(
        "| the cursor bytes alone (what the seam read before row 50) | {} | {} |",
        stale[0] + live[0],
        stale[1] + live[1],
    );
    println!("| the cursor bytes **and** the box on screen | {} | {} |", live[0], live[1]);
    println!("| the cursor bytes with no box drawn | {} | {} |", stale[0], stale[1]);

    println!("\n## What the seam reads against what the cartridge honours\n");
    println!("| the seam's menu | `own_turn` | press refused | press honoured |");
    println!("| --- | --- | ---: | ---: |");
    for ((name, own_turn), counts) in &tally {
        println!("| `{name}` | {own_turn} | {} | {} |", counts[0], counts[1]);
    }

    for (kind, separator) in &separators {
        println!(
            "\n## The bytes that separate a honoured press from a refused one, on {kind}\n\n\
             {} refusing frames, {} accepting.\n",
            separator.counts[0], separator.counts[1]
        );
        let disjoint = separator.disjoint();
        if disjoint.is_empty() {
            println!("No single byte of WRAM or HRAM separates the two classes here.");
            continue;
        }
        println!("| address | when refused | when honoured |");
        println!("| ---: | --- | --- |");
        for (address, refused, honoured) in disjoint.iter().take(40) {
            println!(
                "| `{address:#06x}` | {} | {} |",
                refused.iter().map(|v| format!("{v:02x}")).collect::<Vec<_>>().join(" "),
                honoured.iter().map(|v| format!("{v:02x}")).collect::<Vec<_>>().join(" "),
            );
        }
        println!("\n{} addresses separate in all.", disjoint.len());
    }

    println!("\n## One screen of each class\n\n```");
    for ((name, honoured), boxes) in &shown {
        println!("{name} honoured={honoured}\n{boxes}");
    }
    println!("```");
}

/// The screen as text, for a survey that has to read what the cartridge actually drew.
///
/// Red's charmap: `$80`-`$99` are `A`-`Z`, `$a0`-`$b9` are `a`-`z`, `$f6`-`$ff` are `0`-`9`, and
/// `$7f` is a space. Everything else prints as `.`, which is enough to tell a clerk's text box
/// from an item list.
fn screen_text(gb: &mut Emulator) -> Vec<String> {
    (0..18u16)
        .map(|y| {
            (0..20u16)
                .map(|x| match gb.read8(ram::wTileMap + y * 20 + x) {
                    0x7f => ' ',
                    byte @ 0x80..=0x99 => (b'A' + (byte - 0x80)) as char,
                    byte @ 0xa0..=0xb9 => (b'a' + (byte - 0xa0)) as char,
                    byte @ 0xf6..=0xff => (b'0' + (byte - 0xf6)) as char,
                    0xe7 => '!',
                    0xe8 => '?',
                    0xf3 => '$',
                    _ => '.',
                })
                .collect()
        })
        .collect()
}

/// Every byte the mart's reading rests on, as one line.
///
/// The raw half is read first, because the seam borrows the emulator: `wListMenuID` and
/// `wTextBoxID` are the two bytes [`state::shop`] decides on, and `wCurrentMenuItem` /
/// `wMaxMenuItem` are the cursor a purchase navigates by.
fn counter_line(gb: &mut Emulator, adapter: &PokemonRedReward) -> String {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
    let raw = format!(
        "list={:#04x} textbox={:#04x} cur={} max={} font={:#04x} watched={:#04x} \
         top=({},{}) joy={:#04x}",
        gb.read8(ram::wListMenuID),
        gb.read8(ram::wTextBoxID),
        gb.read8(ram::wCurrentMenuItem),
        gb.read8(ram::wMaxMenuItem),
        gb.read8(ram::wFontLoaded),
        gb.read8(ram::wMenuWatchedKeys),
        gb.read8(ram::wTopMenuItemY),
        gb.read8(ram::wTopMenuItemX),
        gb.read8(ram::wJoyIgnore),
    );
    let text = flybrain_gb::pokemon_red::state::text_box(gb);
    let yes_no = flybrain_gb::pokemon_red::state::yes_no_prompt(gb);
    let ledger = AdapterLedger(adapter);
    let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let state: &mut dyn MacroState = &mut poke;
    format!(
        "{:?} shop={:?} box(open={} waiting={}) yes_no={} money={} | {raw}",
        state.scene(),
        state.shop().map(|shop| (shop.screen, shop.cursor.current, shop.cursor.max)),
        text.open,
        text.waiting,
        yes_no,
        state.money()
    )
}

/// Row 55's counter survey: what the mart's seam reads, what the pad offers, and what a real
/// `BUY …` does frame by frame.
///
/// `FLY_PROBE_CATCH=shop`. The live loop was `BUY ANTIDOTE start` / `BUY ANTIDOTE blocked` every
/// 0.8 s for ten brain minutes in the Pewter mart, with nothing else starting, so the three
/// questions are which reading puts the button on the pad, which step of the script gives up, and
/// what the counter's own cursor reports while it does. All three are printed rather than
/// reasoned about: the script navigates by reading the cursor, so the cursor is the evidence.
///
/// `FLY_PROBE_SHOP_ITEM` names the item by its `BUY …` button (`antidote` by default) so the same
/// survey can be pointed at whichever purchase the loop is on.
fn shop_survey(gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64) {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
    use flybrain_gb::pokemon_red::macros::palette;
    use flybrain_gb::pokemon_red::macros::{MacroKind, MacroMachine, Palette};
    use flybrain_gb::pokemon_red::state::PokeState;

    let want = std::env::var("FLY_PROBE_SHOP_ITEM").unwrap_or_else(|_| "antidote".to_string());
    let wanted = match want.as_str() {
        "potion" => MacroKind::BuyPotion,
        "ball" => MacroKind::BuyBall,
        "repel" => MacroKind::BuyRepel,
        _ => MacroKind::BuyAntidote,
    };

    println!("\n## The counter as the seam reads it, sixty frames with nothing pressed\n");
    let mut last = String::new();
    for frame in 0..60 {
        let line = counter_line(gb, adapter);
        if line != last {
            println!("- frame {frame:3}: {line}");
            last = line;
        }
        gb.set_buttons(flybrain_gb::buttons::NONE);
        gb.run_frame().expect("a frame should complete");
        *ms += MS_PER_FRAME;
        adapter.sample(gb, *ms);
    }

    println!("\n## Every button the shop scene deals, and the halves of each precondition\n");
    {
        let ledger = AdapterLedger(adapter);
        let mut poke = PokeState::with_ledger(gb, &ledger);
        let state: &mut dyn MacroState = &mut poke;
        let scene = state.scene();
        let palette = Palette::for_scene(scene, state);
        let names: Vec<&str> = palette.slots.iter().flatten().map(|spec| spec.name).collect();
        println!("- scene `{scene:?}`, the pad: {names:?}");
        println!("- `shop_screen` = {:?}", palette::shop_screen(state));
        println!("- `listing` = {:?}", palette::listing(state));
        println!("- `inside_mart` = {}", palette::inside_mart(state));
        for kind in [
            MacroKind::BuyPotion,
            MacroKind::BuyBall,
            MacroKind::BuyAntidote,
            MacroKind::BuyRepel,
        ] {
            let purchase = kind.purchase();
            let stocked = purchase.map(|(id, _)| state.shop_stock().iter().position(|s| *s == id));
            let rich = purchase.map(|(_, cost)| state.money() >= cost);
            println!(
                "- {:<12} item {:?}, its index in the stock {stocked:?}, money allows {rich:?}, \
                 precondition {}",
                kind.name(),
                purchase,
                palette::precondition(kind, state),
            );
        }
    }

    println!("\n## `{}`, frame by frame, three times over\n", wanted.name());
    for attempt in 1..=3 {
        let slot = {
            let ledger = AdapterLedger(adapter);
            let mut poke = PokeState::with_ledger(gb, &ledger);
            let state: &mut dyn MacroState = &mut poke;
            let scene = state.scene();
            let palette = Palette::for_scene(scene, state);
            palette.slot(flybrain_gb::pokemon_red::macros::MacroId(wanted.slot())).map(|_| {
                (palette, flybrain_gb::pokemon_red::macros::MacroId(wanted.slot()))
            })
        };
        let Some((palette, slot)) = slot else {
            println!("- attempt {attempt}: the button is not on the pad, so nothing is pressed.");
            return;
        };
        let mut machine = MacroMachine::new(SEED);
        let started = {
            let ledger = AdapterLedger(adapter);
            let mut poke = PokeState::with_ledger(gb, &ledger);
            machine.start(&palette, slot, &mut poke)
        };
        println!("\n### Attempt {attempt}: `start` = {started:?}");
        println!("- frame   0: {}", counter_line(gb, adapter));
        let mut frame = 0u32;
        loop {
            let mask = {
                let ledger = AdapterLedger(adapter);
                let mut poke = PokeState::with_ledger(gb, &ledger);
                machine.step(&mut poke)
            };
            let Some(mask) = mask else { break };
            gb.set_buttons(mask);
            gb.run_frame().expect("a frame should complete");
            *ms += MS_PER_FRAME;
            adapter.sample(gb, *ms);
            frame += 1;
            let line = counter_line(gb, adapter);
            if line != last || frame.is_multiple_of(20) {
                println!("- frame {frame:3}: mask {mask:#06x}  {line}");
                last = line;
            }
            if frame > 700 {
                println!("- (over seven hundred frames, which the cap forbids)");
                break;
            }
        }
        println!("- outcome after {frame} frames: {:?}", machine.outcome());
        let mut entries = Vec::new();
        while let Some(entry) = machine.take_blocked() {
            entries.push(entry);
        }
        println!("- the blocked ledger entries it earned: {entries:?}");
    }

    println!("\n## What an A press at the counter really opens, pulsed by hand\n");
    let pulse = |gb: &mut Emulator, adapter: &mut PokemonRedReward, mask: u8, ms: &mut f64| {
        for phase in 0..16 {
            gb.set_buttons(if phase < 8 { mask } else { 0 });
            gb.run_frame().expect("a frame should complete");
            *ms += MS_PER_FRAME;
            adapter.sample(gb, *ms);
        }
    };
    for step in 1..=40 {
        pulse(gb, adapter, flybrain_gb::buttons::A, ms);
        println!("- A pulse {step:2}: {}", counter_line(gb, adapter));
    }
    println!("\n## And backing out of whatever that left, with B\n");
    for step in 1..=10 {
        pulse(gb, adapter, flybrain_gb::buttons::B, ms);
        println!("- B pulse {step:2}: {}", counter_line(gb, adapter));
    }
    println!("\n## The live buy list: BUY chosen from the counter menu, then read\n");
    // Get to the BUY / SELL / QUIT menu -- the one screen in the mart whose cursor is accepting
    // input with no dialogue box drawn -- put the cursor on BUY, confirm, and read what opens.
    // This is the list `shop_plan` navigates, and what `wMaxMenuItem` reports on it is the whole
    // question row 55 turns on.
    for _ in 0..40 {
        let menu = {
            let text = flybrain_gb::pokemon_red::state::text_box(gb);
            !text.waiting && gb.read8(ram::wMaxMenuItem) == 2 && gb.read8(ram::wTopMenuItemY) == 4
        };
        if menu {
            break;
        }
        pulse(gb, adapter, flybrain_gb::buttons::A, ms);
    }
    println!("- at the counter menu: {}", counter_line(gb, adapter));
    for row in screen_text(gb) {
        println!("      |{row}|");
    }
    for _ in 0..4 {
        if gb.read8(ram::wCurrentMenuItem) == 0 {
            break;
        }
        pulse(gb, adapter, flybrain_gb::buttons::UP, ms);
    }
    println!("- cursor on BUY: {}", counter_line(gb, adapter));
    pulse(gb, adapter, flybrain_gb::buttons::A, ms);
    let mut seen = String::new();
    for frame in 0..90 {
        let line = counter_line(gb, adapter);
        if line != seen {
            println!("- {frame:3} frames after confirming BUY: {line}");
            for row in screen_text(gb) {
                println!("      |{row}|");
            }
            seen = line;
        }
        gb.set_buttons(flybrain_gb::buttons::NONE);
        gb.run_frame().expect("a frame should complete");
        *ms += MS_PER_FRAME;
        adapter.sample(gb, *ms);
    }
    println!("\n### Walking that list down, one pulse at a time\n");
    for step in 1..=9 {
        pulse(gb, adapter, flybrain_gb::buttons::DOWN, ms);
        println!("- DOWN {step}: {}", counter_line(gb, adapter));
        for row in screen_text(gb) {
            println!("      |{row}|");
        }
    }

    println!("\n## The counter reopened from the overworld: every screen the mart draws\n");
    // Out of the counter, face the clerk again and press A: the one sequence that shows what
    // `wMaxMenuItem` really reports on the BUY / SELL / QUIT menu and on the *live* buy list,
    // which is the question the fix turns on.
    for step in 1..=30 {
        pulse(gb, adapter, flybrain_gb::buttons::A, ms);
        let line = counter_line(gb, adapter);
        println!("- A pulse {step:2}: {line}");
        if step % 10 == 0 {
            for down in 1..=3 {
                pulse(gb, adapter, flybrain_gb::buttons::DOWN, ms);
                println!("  - DOWN {down}: {}", counter_line(gb, adapter));
            }
        }
    }
}

/// Everything that tells one box of a conversation from another, on one line (row 56).
///
/// The two readings side by side: what the seam makes of the frame ([`state::yes_no_prompt`], and
/// the scene the pad is dealt for), and **where the cartridge actually drew a box**
/// ([`state::drawn_boxes`]). A prompt Red draws somewhere other than the nurse's corner reads
/// `yesno=false` on the left of that line and shows up as a rectangle on the right of it, which is
/// the whole question row 56 asks.
fn dialog_frame(gb: &mut Emulator) -> String {
    let text = state::text_box(gb);
    let boxes: Vec<String> = state::drawn_boxes(gb)
        .into_iter()
        .map(|(left, top, right, bottom)| format!("({left},{top})-({right},{bottom})"))
        .collect();
    format!(
        "{:?} open={} waiting={} yesno={} cursor=({},{},{},{},{:#04x}) textbox={:#04x} \
         boxes=[{}] | {} | {}",
        scene::detect(gb),
        text.open,
        text.waiting,
        state::yes_no_prompt(gb),
        gb.read8(ram::wTopMenuItemY),
        gb.read8(ram::wTopMenuItemX),
        gb.read8(ram::wCurrentMenuItem),
        gb.read8(ram::wMaxMenuItem),
        gb.read8(ram::wMenuWatchedKeys),
        gb.read8(ram::wTextBoxID),
        boxes.join(" "),
        box_line(gb, 14),
        box_line(gb, 16),
    )
}

/// The pad the palette deals for the frame that is up, by name.
fn dialog_pad(gb: &mut Emulator, adapter: &PokemonRedReward) -> String {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
    use flybrain_gb::pokemon_red::macros::plan;
    let ledger = AdapterLedger(adapter);
    let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
    let state: &mut dyn MacroState = &mut poke;
    let scene = state.scene();
    let plan = plan::plan_for(scene, state);
    let names: Vec<&str> = plan.slots.iter().flatten().map(|spec| spec.name).collect();
    format!("{names:?}")
}

/// Row 56's conversation survey: which box the fly is answering in a gym, and where Red draws it.
///
/// `FLY_PROBE_CATCH=dialog`. The live loop was map 54, scene `dialog`, `YES` 64 / `NO` 62 /
/// `NEXT` 59 / `TALK` 6 over ten brain minutes with no walk macro dealt at all, and three readings
/// fit that: a conversation that re-offers its choice for ever, a talked ledger that never records
/// the person, or a prompt the seam cannot see. They are told apart by walking the conversation
/// press by press and printing both readings of every frame, so this walks it.
///
/// `FLY_PROBE_ANSWER` picks the button the survey presses on a frame where a box **is** drawn
/// somewhere: `a` (the default) or `b`. Everything else gets an A, because A is what advances a
/// plain box. The pad is printed beside each frame, so a row where the pad is `NEXT, YES, NO` on a
/// frame with a two-option box on screen is the trap said out loud.
fn dialog_survey(gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64) {
    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
    use flybrain_gb::pokemon_red::macros::palette;

    let pulse = |gb: &mut Emulator, adapter: &mut PokemonRedReward, mask: u8, ms: &mut f64| {
        for phase in 0..16 {
            gb.set_buttons(if phase < 8 { mask } else { 0 });
            gb.run_frame().expect("a frame should complete");
            *ms += MS_PER_FRAME;
            adapter.sample(gb, *ms);
        }
    };

    println!("\n## What the fly is standing on, and what it is facing\n");
    {
        let ledger = AdapterLedger(adapter);
        let mut poke = flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
        let state: &mut dyn MacroState = &mut poke;
        println!("- player: {:?}", state.player());
        println!("- objective: {:?}", state.objective());
        println!("- facing: {:?}", palette::facing_target(state));
        println!("- `facing_untalked` = {}", palette::facing_untalked(state));
        println!("- untalked people: {:?}", palette::untalked_people(state));
        println!("- untalked objects: {:?}", palette::untalked_objects(state));
        println!("- `yes_no_prompt` = {}", state.yes_no_prompt());
    }
    println!("\n## The screen at the checkpoint\n\n```\n{}\n```\n", screen_rows(gb));
    println!("```");
    for row in screen_text(gb) {
        println!("{row}");
    }
    println!("```\n");
    println!("- the frame: {}", dialog_frame(gb));
    println!("- the pad: {}", dialog_pad(gb, adapter));

    let answer = std::env::var("FLY_PROBE_ANSWER").unwrap_or_else(|_| "a".to_string());
    let answer_mask =
        if answer == "b" { flybrain_gb::buttons::B } else { flybrain_gb::buttons::A };

    println!(
        "\n## The conversation, one raw pulse at a time (a box on screen gets `{answer}`)\n"
    );
    println!("```");
    let mut last = String::new();
    let mut boxes_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes: BTreeMap<(bool, bool, bool), u64> = BTreeMap::new();
    for index in 0..env_usize("FLY_PROBE_PULSES", 200) {
        let choice = if state::drawn_boxes(gb).iter().any(|(_, top, _, _)| *top < 12) {
            answer_mask
        } else {
            flybrain_gb::buttons::A
        };
        pulse(gb, adapter, choice, ms);
        let drawn = state::drawn_boxes(gb);
        for (left, top, right, bottom) in &drawn {
            *boxes_seen.entry(format!("({left},{top})-({right},{bottom})")).or_default() += 1;
        }
        *classes
            .entry((
                drawn.iter().any(|(_, top, _, _)| *top < 12),
                gb.read8(ram::wTextBoxID) == 0x14,
                state::yes_no_prompt(gb),
            ))
            .or_default() += 1;
        let now = format!("{} pad={}", dialog_frame(gb), dialog_pad(gb, adapter));
        if now != last {
            println!("#{index:<3} {now}");
            last = now;
        }
    }
    println!("```\n");
    println!("Every rectangle the cartridge drew over the survey, and on how many frames:\n");
    for (figure, count) in &boxes_seen {
        println!("- `{figure}` on {count} frames");
    }
    println!("\n{}", separator_table(&classes));
}

/// `FLY_PROBE_CATCH=route`. The live trap was map 2, scene `overworld`, a pad of `GO ROUTE` alone,
/// refused about 740 times per ten brain minutes for hours with no button pressed. The ledgers
/// that dealt that pad are session state and a restore starts them empty, so this *earns* them:
/// it drives the real [`PokemonPalette`] from the checkpoint, choosing uniformly among whatever the
/// scene binds once per hold (the brain's hold, not a ranking), and prints every pad it deals and
/// every refusal with its reason. When the pad is down to one button that keeps refusing, it reads
/// the frame the way the macros do -- ledgers included -- and says which list emptied why, and
/// whether the route search can reach any of the one button's goals.
///
/// `FLY_PROBE_FRAMES` bounds the drive; `FLY_PROBE_RNG` changes the choices; `FLY_PROBE_PREFER`
/// (comma-separated names) presses those buttons whenever they are dealt.
///
/// [`PokemonPalette`]: flybrain_gb::pokemon_red::macros::PokemonPalette
fn route_survey(gb: &mut Emulator, adapter: &mut PokemonRedReward, ms: &mut f64) {
    use flybrain_gb::MacroPalette;
    use flybrain_gb::pokemon_red::macros::cartridge::{FACINGS, MacroState, TalkTarget, TargetKey};
    use flybrain_gb::pokemon_red::macros::path::Way;
    use flybrain_gb::pokemon_red::macros::{PokemonPalette, Tile, palette, path};

    let budget = env_usize("FLY_PROBE_FRAMES", 240_000);
    let mut rng = env_usize("FLY_PROBE_RNG", 20_260_923) as u32 | 1;
    let hold_frames = 48usize;
    let trace_frames = env_usize("FLY_PROBE_TRACE_FRAMES", 0);
    // `FLY_PROBE_CATCH_AFTER=0` reads the frame at once, before any choice.
    let catch_after = env_usize("FLY_PROBE_CATCH_AFTER", 20) as u32;
    let prefer: Vec<String> = std::env::var("FLY_PROBE_PREFER")
        .map(|value| value.split(',').map(|name| name.trim().to_string()).collect())
        .unwrap_or_default();
    let mut macros = PokemonPalette::new(SEED);
    if let Some(seeded) = ledgers::seed(&mut macros, gb, adapter, *ms, "FLY_PROBE_SEED") {
        println!("\n- seeded: {seeded}");
    }
    let mut running = false;
    let mut since_decision = hold_frames;
    let mut last_pad = String::new();
    let mut refusals: BTreeMap<String, u64> = BTreeMap::new();
    let mut outcomes: BTreeMap<String, u64> = BTreeMap::new();
    let mut single_refusals = 0u32;
    let mut caught_at: Option<usize> = None;
    // `FLY_PROBE_CATCH_MAP=54` with `FLY_PROBE_CATCH_ENTRIES=4` reads the frame the fly is given
    // the buttons back on its fourth arrival on map 54 (row 58: the pad in and out of one door).
    let catch_map: Option<u8> =
        std::env::var("FLY_PROBE_CATCH_MAP").ok().and_then(|value| value.parse().ok());
    let catch_entries = env_usize("FLY_PROBE_CATCH_ENTRIES", 4);
    let mut entries = 0usize;
    let mut arrived_at = 0usize;
    let mut last_map: Option<u8> = None;

    // `FLY_PROBE_HOLD=right:96,up:32` holds raw directions first and prints where the fly is
    // every eight frames: what the cartridge does with a press, before any macro is asked.
    if let Ok(holds) = std::env::var("FLY_PROBE_HOLD") {
        println!("\n## Raw holds before the drive\n\n```");
        for hold in holds.split(',') {
            let (name, frames) = hold.split_once(':').unwrap_or((hold, "32"));
            let mask = match name {
                "right" => flybrain_gb::buttons::RIGHT,
                "left" => flybrain_gb::buttons::LEFT,
                "up" => flybrain_gb::buttons::UP,
                "down" => flybrain_gb::buttons::DOWN,
                "a" => flybrain_gb::buttons::A,
                "b" => flybrain_gb::buttons::B,
                _ => 0,
            };
            for index in 0..frames.parse::<usize>().unwrap_or(32) {
                gb.set_buttons(mask);
                gb.run_frame().expect("a frame should complete");
                *ms += MS_PER_FRAME;
                adapter.sample(gb, *ms);
                if index % 8 == 7 {
                    println!(
                        "{name:>5} +{index:<3} {:?} scene {:?} sim={} flags5={:#04x} joyignore={:#04x}",
                        state::player(gb).map(|p| (p.map, p.x, p.y, p.facing)),
                        scene::detect(gb),
                        gb.read8(ram::wSimulatedJoypadStatesIndex),
                        gb.read8(ram::wStatusFlags5),
                        gb.read8(ram::wJoyIgnore),
                    );
                }
            }
        }
        println!("```");
    }
    println!("\n## The drive: the real palette, one uniform choice per hold\n\n```");
    for frame in 0..budget {
        macros.clock(*ms);
        let observed = {
            let ledger = AdapterLedger(&*adapter);
            macros.observe(gb, &ledger)
        };
        let names: Vec<&str> = observed.bindings.iter().map(|binding| binding.name).collect();
        let player = state::player(gb);
        let pad = format!("{:?} {names:?}", observed.scene);
        if pad != last_pad {
            println!("f{frame:<6} {:?} pad {pad}", player.map(|p| (p.map, p.x, p.y)));
            if let Some(line) = battle_line(gb) {
                println!("        {line}");
            }
            last_pad = pad;
        }
        let mut mask = 0u8;
        if running {
            let ledger = AdapterLedger(&*adapter);
            match macros.step(gb, &ledger) {
                Some(held) => mask = held,
                None => running = false,
            }
        } else if since_decision >= hold_frames && !observed.bindings.is_empty() {
            since_decision = 0;
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            // `FLY_PROBE_PREFER` names buttons to press whenever they are dealt, first one first:
            // the live brain's measured favourites, so the survey can earn the ledgers the live
            // session earned. A survey's driver, never the fly's: nothing in the crate reads it.
            let preferred = prefer
                .iter()
                .find_map(|want| observed.bindings.iter().find(|binding| binding.name == want));
            let binding = preferred
                .unwrap_or(&observed.bindings[rng as usize % observed.bindings.len()]);
            let ledger = AdapterLedger(&*adapter);
            match macros.start(binding.slot, gb, &ledger) {
                flybrain_gb::Started::Running(_) => {
                    running = true;
                    if let Some(held) = macros.step(gb, &ledger) {
                        mask = held;
                    } else {
                        running = false;
                    }
                }
                flybrain_gb::Started::Refused { name, reason } => {
                    let key = format!("{} {reason}", name.unwrap_or("-"));
                    *refusals.entry(key.clone()).or_default() += 1;
                    if names.len() == 1 && name.is_some() {
                        single_refusals += 1;
                    } else {
                        single_refusals = 0;
                    }
                    if refusals[&key] <= 3 || single_refusals == 1 {
                        println!(
                            "f{frame:<6} {:?} refused {key} (pad {names:?})",
                            player.map(|p| (p.map, p.x, p.y))
                        );
                    }
                }
            }
        }
        if let Some((name, outcome)) = macros.take_finished() {
            *outcomes
                .entry(format!(
                    "{name} {outcome:?} on {:?}",
                    state::player(gb).map(|p| p.map)
                ))
                .or_default() += 1;
            if !matches!(outcome, flybrain_gb::Outcome::Done) {
                println!(
                    "f{frame:<6} {:?} {name} {outcome:?}",
                    state::player(gb).map(|p| (p.map, p.x, p.y))
                );
            }
        }
        since_decision += 1;
        if frame < trace_frames {
            println!(
                "  t{frame:<5} {:?} mask {mask:#04x} running {:?} marks {:?} stood {} | {}",
                state::player(gb).map(|p| (p.map, p.x, p.y, p.facing)),
                macros.running(),
                macros.fences().1,
                macros.stood(),
                scene::why_unknown(gb)
            );
        }
        gb.set_buttons(mask);
        gb.run_frame().expect("a frame should complete");
        *ms += MS_PER_FRAME;
        adapter.sample(gb, *ms);
        if single_refusals >= catch_after {
            caught_at = Some(frame);
            break;
        }
        if let (Some(want), Some(player)) = (catch_map, player) {
            if player.map == want && last_map != Some(want) {
                entries += 1;
                arrived_at = frame;
            }
            last_map = Some(player.map);
            // Forty frames in: the first frames on a new map byte still carry the old map's
            // warps (the tear 12.16 names), and a reading there says nothing about the room.
            if player.map == want
                && entries >= catch_entries
                && frame >= arrived_at + 40
                && !running
                && matches!(observed.scene, flybrain_gb::SceneId::Overworld)
                && !observed.bindings.is_empty()
            {
                caught_at = Some(frame);
                break;
            }
        }
    }
    println!("```\n");
    let progress = adapter.progress();
    println!(
        "- at the end: rank {} ({}), badges {}, unique tiles {}",
        progress.rank, progress.rank_label, progress.counter, progress.unique_locations
    );
    println!("- refusals: {refusals:?}");
    println!("- outcomes: {outcomes:?}");
    let Some(frame) = caught_at else {
        println!("- pushed tiles at the end (no window): {:?}", macros.fences().0);
        println!("\nThe pad never came down to one refusing button in {budget} frames.");
        return;
    };
    println!(
        "\n## Caught on frame {frame} ({:.1} brain minutes): {}\n",
        frame as f64 * MS_PER_FRAME / 60_000.0,
        if single_refusals >= catch_after {
            "one button, refused twenty holds running".to_string()
        } else {
            format!("arrival {entries} on map {catch_map:?}")
        }
    );
    let (pushed, frontiers) = macros.fences();
    println!("- pushed tiles (no window): {pushed:?}");
    println!("- frontier marks (no window): {frontiers:?}");
    let ledger = AdapterLedger(&*adapter);
    macros.inspect(gb, &ledger, |state: &mut dyn MacroState| {
        let player = state.player().expect("a loaded map");
        println!("- player: {player:?}");
        println!("- objective: {:?}", state.objective());
        println!("- `objective_goals`: {:?}", palette::objective_goals(state));
        println!("- `objective_targets`: {:?}", palette::objective_targets(state));
        let drawn = path::person_targets(state);
        for (tile, target) in drawn.iter().copied().chain(path::offscreen_person_targets(state)) {
            println!(
                "  - person {target:?} at ({:2},{:2}) {}: talked {}, blocked {}, reached {}",
                tile.x,
                tile.y,
                if drawn.contains(&(tile, target)) { "drawn" } else { "off the screen" },
                state.talked(target),
                state.blocked(TargetKey::Thing(target)),
                state.reached(TargetKey::Thing(target))
            );
        }
        println!("- `untalked_people`: {:?}", palette::untalked_people(state));
        println!("- `untalked_objects`: {:?}", palette::untalked_objects(state));
        println!("- `facing_untalked`: {}", palette::facing_untalked(state));
        println!("- `frontier_aims`: {} tiles", palette::frontier_aims(state).len());
        println!("- `frontier_exhausted`: {}", state.frontier_exhausted());
        println!("- `stranded`: {}", palette::stranded(state));
        for way in [Way::Exit, Way::Passage, Way::Route] {
            println!("- `ways({way:?})`: {:?}", palette::ways(state, way).iter().map(|exit| (exit.id, exit.tile)).collect::<Vec<_>>());
        }
        println!("\n### Every way out, and whether the route search reaches it from here\n");
        for exit in path::exits(state) {
            let reach = path::route(state, &[exit.tile]).map(|route| route.goal.is_some());
            println!(
                "- {:?} at ({:2},{:2}) way {:?} -> {:?}: map_visited {:?}, blocked {}, reachable {:?}",
                exit.id,
                exit.tile.x,
                exit.tile.y,
                exit.way,
                exit.destination(player.map),
                exit.destination(player.map).map(|map| state.map_visited(map)),
                state.blocked(TargetKey::Exit(exit.id)),
                reach
            );
        }
        // A room small enough to print whole is printed whole, with its people on it (row 58:
        // the gym's leader is twelve rows from the door).
        let size = state.map_size().expect("a loaded map");
        let whole = size.width <= 24 && size.height <= 24;
        let people: Vec<(Tile, TalkTarget)> = path::person_targets(state)
            .into_iter()
            .chain(path::offscreen_person_targets(state))
            .collect();
        let (rows, columns) = if whole {
            (0..=size.height - 1, 0..=size.width - 1)
        } else {
            (
                player.y.saturating_sub(3)..=player.y.saturating_add(3),
                player.x.saturating_sub(6)..=player.x.saturating_add(6),
            )
        };
        let grid = state.map_grid();
        println!(
            "\n### The fly's {} (pushed = `P`, player = `@`, a person = `N`; grid {})\n\n```",
            if whole { "whole map" } else { "own neighbourhood" },
            grid.is_some()
        );
        for y in rows {
            let row: String = columns
                .clone()
                .map(|x| {
                    let walk = match grid.as_deref() {
                        Some(grid) => grid.walkable(x, y),
                        None => state.walkable(x, y),
                    };
                    if x == player.x && y == player.y {
                        '@'
                    } else if people.iter().any(|(tile, _)| *tile == Tile::new(x, y)) {
                        'N'
                    } else if state.pushed_tile(x, y) {
                        'P'
                    } else {
                        match walk {
                            flybrain_gb::pokemon_red::macros::state::Walkable::Yes => '.',
                            flybrain_gb::pokemon_red::macros::state::Walkable::No => '#',
                            flybrain_gb::pokemon_red::macros::state::Walkable::Unknown => '?',
                        }
                    }
                })
                .collect();
            println!("y{y:2} x{:2}..  {row}", if whole { 0 } else { player.x.saturating_sub(6) });
        }
        println!("```");
        for (tile, target) in &people {
            let aims: Vec<Tile> = FACINGS.iter().filter_map(|facing| tile.step(*facing)).collect();
            let reach = path::route(state, &aims).map(|route| route.goal);
            println!("- a route to {target:?} at ({:2},{:2}): {reach:?}", tile.x, tile.y);
        }
    });
}

/// How the candidate readings of "a two-option box is up" separate the frames of a survey.
///
/// Three columns, because three things could say it and only a measurement says which: the
/// cartridge's own `wTextBoxID`, the seam's pinned-geometry [`state::yes_no_prompt`], and the
/// generalised reading -- a complete border drawn around the cursor the game parked, wherever on
/// screen that is. A row where the box is drawn and a column reads `false` is that column missing
/// the prompt.
fn separator_table(classes: &BTreeMap<(bool, bool, bool), u64>) -> String {
    let mut out = String::from(
        "| a box drawn above the dialogue box | `wTextBoxID` = `$14` | `yes_no_prompt` | frames |\n         | --- | --- | --- | ---: |\n",
    );
    for ((drawn, textbox, prompt), frames) in classes {
        out.push_str(&format!("| {drawn} | {textbox} | {prompt} | {frames} |\n"));
    }
    out
}

fn main() {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        println!("FLY_ROM is not set, so there is nothing to probe.");
        return;
    };
    let rom = std::fs::read(&path).expect("FLY_ROM should be readable");
    let Some(checkpoint_path) = std::env::var_os("FLY_TRAP_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_MACRO_CHECKPOINT"))
    else {
        println!("FLY_TRAP_CHECKPOINT is not set: a probe is about a state the stream was in.");
        return;
    };
    let checkpoint = flysim::store::load(std::path::Path::new(&checkpoint_path))
        .expect("FLY_TRAP_CHECKPOINT should be a FLYSIM01 envelope");

    let mut gb = Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    let mut adapter = PokemonRedReward::new();
    // `FLY_PROBE_RATCHET=1` starts from the ratchet's best snapshot instead: where a "Stuck"
    // rollback puts the fly, which is where a live session that spent its rollbacks resumed from.
    if std::env::var("FLY_PROBE_RATCHET").is_ok_and(|value| value == "1") {
        gb.import_state(&checkpoint.runtime.ratchet_game).expect("the ratchet's snapshot");
    } else {
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
    }
    adapter.import_state(&checkpoint.runtime.reward).expect("the checkpoint's reward ledger");

    let channels = flybrain_gb::macro_channels("pokemon-red");
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset.macros.as_ref().expect("the preset has a macro group").hold_ms;
    let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
    decoder.calibrate(&rates(None));
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = MacroMode::Macros;
    config.validate().expect("pokemon-red has a palette in macros mode");
    let mut layer = macro_layer(&config, hold_ms, SEED).expect("a layer in macros mode");
    let mut ms = 0.0;
    let _ = layer.observe(&mut gb, &AdapterLedger(&adapter), ms);

    println!("# Scene probe\n\nFrom `{}`.", std::path::Path::new(&checkpoint_path).display());
    cartridge(&mut gb, &adapter, "The save at the checkpoint");
    dump(&mut gb, "At the checkpoint");
    pad(&mut gb, &adapter, "The pad at the checkpoint");

    let catch_nurse = std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "nurse");
    if catch_nurse {
        nurse_survey(&mut gb, &mut adapter, &mut ms);
        return;
    }

    // Row 54's mid-step survey: hold one direction and watch the grid's cross-check, the
    // coordinates and every candidate for "a step is in progress" across a whole step.
    if std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "step") {
        step_survey(&mut gb, &mut adapter, &mut ms);
        return;
    }

    // Row 50's survey: drive real battles and press at every battle menu the seam reads, to tell a
    // list that is accepting input from cursor bytes that outlived their list.
    if std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "accept") {
        let channels: Vec<String> = channels.iter().map(|name| (*name).to_string()).collect();
        accept_survey(
            &mut gb,
            &mut adapter,
            &mut ms,
            &mut layer,
            &mut decoder,
            &channels,
            hold_ms,
        );
        return;
    }

    // Row 55's counter survey: what the mart's seam reads while a `BUY ...` runs, and what an A
    // press at the counter really opens.
    if std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "shop") {
        shop_survey(&mut gb, &mut adapter, &mut ms);
        return;
    }

    // Row 56's conversation survey: which box a gym's guide draws, where he draws it, and what
    // the dialog pad makes of it press by press.
    if std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "dialog") {
        dialog_survey(&mut gb, &mut adapter, &mut ms);
        return;
    }

    // Row 57's pad survey: earn the session's ledgers from the checkpoint with the real palette,
    // and read the frame the pad comes down to one refusing button on.
    if std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "route") {
        route_survey(&mut gb, &mut adapter, &mut ms);
        return;
    }

    let budget = env_usize("FLY_PROBE_FRAMES", 200_000);
    let stuck_after = env_usize("FLY_PROBE_STUCK", 600);
    let mut next_burst = ms;
    let mut burst = 0usize;
    let mut run: (&'static str, usize) = ("", 0);
    let mut stuck_at = None;
    // The push-back's own signature: `wSimulatedJoypadStatesIndex` non-zero means the cartridge is
    // walking the player with joypad states of its own (`infra/docs/macros-traps.md` row 28).
    let catch_script = std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "script");
    // The survey method (`docs/design/macros-wram.md`): drive until the fly is standing on a named
    // map and print that map's own tables, because a warp's destination byte is ground truth about
    // which map id is on the other side of which door.
    let survey: Option<u8> = std::env::var("FLY_PROBE_MAP")
        .ok()
        .and_then(|value| value.trim().parse().ok());
    // A turn the fly cannot attack on: `ATTACK` off the pad while a battle menu is accepting
    // input. `FLY_PROBE_STUCK` frames in a row of it and everything gets dumped.
    let catch_noattack =
        std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "noattack");
    // The same turn asked about without reference to the pad: the fly's own turn with no move that
    // has PP. `noattack` is row 34's *symptom* and goes quiet once the row is fixed, which is what
    // makes it the after-measurement; this is the *state*, and it is what a checkpoint is made from.
    let catch_nopp = std::env::var("FLY_PROBE_CATCH").is_ok_and(|value| value == "nopp");
    let mut noattack = 0usize;
    let mut before = (0u8, 0u8, 0u8);
    let mut surveyed = 0usize;
    for frame in 0..budget {
        let bursting = ms < next_burst + BURST_MS;
        let hot = bursting.then(|| channels[(burst / HOLDS_PER_SLOT) % channels.len()]);
        if ms >= next_burst + hold_ms {
            next_burst = ms;
            burst += 1;
        }
        let bound = layer.bound_channels();
        let active = decoder.decode_bound(&rates(hot), ms, false, None, Some(&bound));
        let mask = {
            let ledger = AdapterLedger(&adapter);
            layer.decide(&active, 0, ms, &mut gb, &ledger).mask
        };
        gb.set_buttons(mask as u8);
        gb.run_frame().expect("a frame should complete");
        ms += MS_PER_FRAME;
        adapter.sample(&mut gb, ms);
        {
            let ledger = AdapterLedger(&adapter);
            let _ = layer.observe(&mut gb, &ledger, ms);
        }
        if catch_script
            && gb.read8(ram::wSimulatedJoypadStatesIndex) != 0
            && gb.read8(ram::wCurMap) == 1
        {
            println!(
                "\nThe cartridge took the joypad at frame {frame} ({:.2} brain minutes). \
                 The frame before, the player was on map {:#04x} at ({}, {}).",
                ms / 60_000.0,
                before.0,
                before.1,
                before.2
            );
            cartridge(&mut gb, &adapter, "The save when the script fired");
            dump(&mut gb, "The frame the script took over");
            // Ride it out and see where the player ends up, pressing nothing.
            for _ in 0..240 {
                gb.set_buttons(0);
                gb.run_frame().expect("a frame should complete");
                ms += MS_PER_FRAME;
                adapter.sample(&mut gb, ms);
            }
            dump(&mut gb, "240 frames later, with nothing pressed");
            return;
        }
        if catch_noattack || catch_nopp {
            let own_turn = matches!(
                scene::detect(&mut gb),
                scene::Scene::Battle { own_turn: true, forced_switch: false }
            );
            let pad = layer.bound_channels();
            let caught = if catch_nopp {
                use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
                let ledger = AdapterLedger(&adapter);
                let mut poke =
                    flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut gb, &ledger);
                let state: &mut dyn MacroState = &mut poke;
                // Not `best_move(..).is_none()`: that is also true on a battle's opening frames,
                // before the engine has copied the active Pokemon into `wBattleMon*` (row 30b),
                // and this catch saved that frame the first time it was run. The condition is the
                // state itself -- a Pokemon that *is* out, with every move it has at 0 PP.
                own_turn
                    && state.battle().and_then(|battle| battle.own).is_some_and(|mon| {
                        let mut moves = mon.moves.iter().flatten().filter(|entry| entry.id != 0);
                        moves.clone().next().is_some() && moves.all(|entry| entry.pp == 0)
                    })
            } else {
                own_turn && !pad.iter().any(|channel| channel.ends_with("attack"))
            };
            if caught {
                noattack += 1;
            } else {
                noattack = 0;
            }
            if noattack >= stuck_after {
                println!(
                    "\nThe fly's own turn matched for {stuck_after} consecutive frames, at frame \
                     {frame} ({:.2} brain minutes). Catch: {}.",
                    ms / 60_000.0,
                    if catch_nopp { "no move with PP" } else { "no `ATTACK` on the pad" }
                );
                battle_dump(&mut gb, &adapter, &pad, "The turn it could not attack on");
                // `FLY_PROBE_SAVE` writes this exact state as a `FLYSIM01` checkpoint, so the ROM
                // test for row 34 can resume into the battle instead of playing 99 brain minutes
                // to reach it. The agent half is the source checkpoint's, unchanged -- this is the
                // release box's own run carried forward by the stub, not a synthesised save -- and
                // `.local/` is not tracked, exactly as every other checkpoint in this workspace.
                if let Some(save) = std::env::var_os("FLY_PROBE_SAVE") {
                    let mut runtime = checkpoint.runtime.clone();
                    runtime.emulator = gb.export_state().expect("the emulator should export");
                    runtime.reward = adapter.export_state();
                    runtime.framebuffer = gb.framebuffer().to_vec();
                    runtime.emulator_frame = frame as u64;
                    let bytes = flysim::store::encode(&checkpoint.agent, &runtime)
                        .expect("the envelope should encode");
                    flysim::store::write_atomic(std::path::Path::new(&save), &bytes)
                        .expect("the checkpoint should be writable");
                    println!(
                        "\nWrote this state to `{}` ({} bytes).",
                        std::path::Path::new(&save).display(),
                        bytes.len()
                    );
                }
                // The survey method on the one question the fix turns on: **what does the
                // cartridge do when FIGHT is chosen and no move has PP?** Back out to the
                // top-level menu with B, put its cursor on FIGHT, confirm, and watch whether the
                // move list opens at all. `CheckPlayerHasUsableMoves` is claimed to skip it and
                // set Struggle; this is that claim, measured through the seam the macros read.
                println!("\n## Choosing FIGHT with no PP, with raw presses\n");
                let menu = |gb: &mut Emulator, adapter: &PokemonRedReward| {
                    use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
                    let ledger = AdapterLedger(adapter);
                    let mut poke =
                        flybrain_gb::pokemon_red::state::PokeState::with_ledger(gb, &ledger);
                    let state: &mut dyn MacroState = &mut poke;
                    (
                        state.battle().map(|battle| battle.menu),
                        state.battle().and_then(|battle| battle.own).map(|mon| mon.hp),
                        state.battle().and_then(|battle| battle.enemy).map(|mon| mon.hp),
                    )
                };
                let pulse = |gb: &mut Emulator, adapter: &mut PokemonRedReward, mask: u8, ms: &mut f64| {
                    for phase in 0..16 {
                        gb.set_buttons(if phase < 8 { mask } else { 0 });
                        gb.run_frame().expect("a frame should complete");
                        *ms += MS_PER_FRAME;
                        adapter.sample(gb, *ms);
                    }
                };
                for _ in 0..12 {
                    if matches!(menu(&mut gb, &adapter).0, Some(flybrain_gb::pokemon_red::macros::state::BattleMenu::Main { .. })) {
                        break;
                    }
                    pulse(&mut gb, &mut adapter, flybrain_gb::buttons::B, &mut ms);
                }
                println!("- after backing out with B: {:?}", menu(&mut gb, &adapter).0);
                for _ in 0..6 {
                    if matches!(
                        menu(&mut gb, &adapter).0,
                        Some(flybrain_gb::pokemon_red::macros::state::BattleMenu::Main { cursor: 0 })
                    ) {
                        break;
                    }
                    pulse(&mut gb, &mut adapter, flybrain_gb::buttons::UP, &mut ms);
                    pulse(&mut gb, &mut adapter, flybrain_gb::buttons::LEFT, &mut ms);
                }
                let before = menu(&mut gb, &adapter);
                println!("- cursor on FIGHT: {:?}, own hp {:?}, enemy hp {:?}", before.0, before.1, before.2);
                pulse(&mut gb, &mut adapter, flybrain_gb::buttons::A, &mut ms);
                let mut seen: Vec<String> = Vec::new();
                let mut move_list_frames = 0usize;
                for _ in 0..30 {
                    let now = menu(&mut gb, &adapter);
                    let label = format!("{:?}", now.0);
                    if matches!(now.0, Some(flybrain_gb::pokemon_red::macros::state::BattleMenu::Moves { .. })) {
                        move_list_frames += 1;
                    }
                    if seen.last() != Some(&label) {
                        seen.push(label);
                    }
                    pulse(&mut gb, &mut adapter, flybrain_gb::buttons::NONE, &mut ms);
                }
                let after = menu(&mut gb, &adapter);
                println!("- menus after confirming FIGHT, in order: {seen:?}");
                println!("- pulses with the move list open: {move_list_frames}");
                println!(
                    "- own hp {:?} -> {:?}, enemy hp {:?} -> {:?}  (Struggle recoils on its user)",
                    before.1, after.1, before.2, after.2
                );
                battle_dump(&mut gb, &adapter, &layer.bound_channels(), "After confirming FIGHT");
                return;
            }
        }
        if survey == Some(gb.read8(ram::wCurMap)) && state::controllable(&mut gb) {
            surveyed += 1;
        } else {
            surveyed = 0;
        }
        // Sixty frames on the map, because `wCurMap` changes several frames before the map header
        // it names is loaded: a dump taken on the first frame prints the *previous* map's size and
        // warp table under the new map's id.
        if let Some(map) = survey
            && surveyed >= 60
        {
            println!("\nThe fly reached map {map:#04x} at frame {frame}.");
            // Stand still for a while and count how many of those frames the whole-map grid can be
            // decoded on. A walking fly is mid-step on most frames -- `wYCoord` is the tile it is
            // walking *to* while the background is still scrolling -- and the screen buffer the
            // cross-check reads is the one that is a tile behind, so "is the grid refused on this
            // map" and "is the grid refused while the fly is moving" are different questions with
            // different fixes (`docs/design/macros.md` section 15).
            let mut refusals: BTreeMap<&'static str, usize> = BTreeMap::new();
            for _ in 0..env_usize("FLY_PROBE_SETTLE", 120) {
                gb.set_buttons(flybrain_gb::buttons::NONE);
                gb.run_frame().expect("a frame should complete");
                ms += MS_PER_FRAME;
                adapter.sample(&mut gb, ms);
                let label = match flybrain_gb::pokemon_red::state::map_grid(&mut gb) {
                    Ok(_) => "decoded",
                    Err(refusal) => refusal.label(),
                };
                *refusals.entry(label).or_insert(0) += 1;
            }
            println!("\nStanding still on it, frame by frame: {refusals:?}");
            cartridge(&mut gb, &adapter, "The save on the surveyed map");
            pad(&mut gb, &adapter, "The pad on the surveyed map");
            return;
        }
        before = (
            gb.read8(ram::wCurMap),
            gb.read8(ram::wXCoord),
            gb.read8(ram::wYCoord),
        );
        let name = layer.scene_name();
        if name == run.0 {
            run.1 += 1;
        } else {
            run = (name, 1);
        }
        // The scene-run detector is the default catch; a named `FLY_PROBE_CATCH` is looking for
        // something else and a battle's own text would end the run before it got there.
        if !catch_noattack && !catch_nopp && !catch_script && name != "overworld"
            && run.1 >= stuck_after
        {
            stuck_at = Some((frame, name));
            break;
        }
    }

    match stuck_at {
        Some((frame, name)) => {
            println!(
                "\nStuck in `{name}` for {stuck_after} consecutive frames, at frame {frame} \
                 ({:.1} brain minutes). Adapter mode `{}`.",
                ms / 60_000.0,
                adapter.mode()
            );
            dump(&mut gb, "Where it stuck");
            // Does anything move it? The pad's own two presses, then the ones it has no button
            // for, each pulsed the way every script in this workspace pulses.
            for (label, mask) in [
                ("after 20 A pulses", flybrain_gb::buttons::A),
                ("after 20 B pulses", flybrain_gb::buttons::B),
                ("after 20 START pulses", flybrain_gb::buttons::START),
            ] {
                for pulse in 0..20 {
                    for phase in 0..16 {
                        gb.set_buttons(if phase < 8 { mask } else { 0 });
                        gb.run_frame().expect("a frame should complete");
                        ms += MS_PER_FRAME;
                        adapter.sample(&mut gb, ms);
                    }
                    let _ = pulse;
                }
                dump(&mut gb, label);
                if scene::detect(&mut gb) == scene::Scene::Overworld {
                    println!("\n**{label} left it.**");
                    break;
                }
            }
        }
        None => println!(
            "\nNever stuck in one non-overworld scene for {stuck_after} frames in {budget}."
        ),
    }
}

/// Row 60: the battle bytes a `MOVE n` button's effect rests on, in one line -- the fly's moves
/// with PP and whether the cartridge would answer each with nothing, both sides' stat stages
/// (7 is normal, 1 is -6), the enemy's stats, status and HP.
fn battle_line(gb: &mut Emulator) -> Option<String> {
    let battle = state::battle(gb)?;
    let own = battle.own?;
    let moves: Vec<String> = own
        .moves
        .iter()
        .flatten()
        .map(|entry| {
            let nothing = state::move_without_effect(gb, entry.id);
            let row = state::move_data(gb, entry.id).map(|data| (data.effect, data.power));
            format!("{:#04x} pp{} row{row:?} nothing={nothing:?}", entry.id, entry.pp)
        })
        .collect();
    let stages = |gb: &mut Emulator, base: u16| -> Vec<u8> { (0..6).map(|i| gb.read8(base + i)).collect() };
    let own_stages = stages(gb, ram::wPlayerMonStatMods);
    let enemy_stages = stages(gb, ram::wEnemyMonStatMods);
    let enemy_stats: Vec<u16> = (0..4)
        .map(|i| u16::from(gb.read8(ram::wEnemyMonAttack + 2 * i)) * 256 + u16::from(gb.read8(ram::wEnemyMonAttack + 2 * i + 1)))
        .collect();
    Some(format!(
        "menu={:?} own hp {}/{} moves [{}] stages {own_stages:?} | enemy {:?} stages {enemy_stages:?} stats {enemy_stats:?} status {:#04x}",
        battle.menu,
        own.hp,
        own.max_hp,
        moves.join(", "),
        battle.enemy.map(|enemy| (enemy.species, enemy.level, enemy.hp, enemy.max_hp)),
        gb.read8(ram::wEnemyMonStatus),
    ))
}
