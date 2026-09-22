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
    gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
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
