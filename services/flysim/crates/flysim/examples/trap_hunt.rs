//! Loop detector: run macros mode from a checkpoint and name every window the fly spent going
//! nowhere.
//!
//! The Viridian stall of 2026-09-17 (`infra/docs/macros-traps.md`) was two hours and seventeen
//! minutes of `GO NPC done, GO OUT done, NEXT done, GO FRONTIER done`, every three brain seconds,
//! over three tiles — and nothing in the loop, the ratchet or the feed said so out loud. This
//! example is the thing that says so, on the dev box, before a release: it runs the real sim loop
//! from a checkpoint and reports every two-brain-minute window in which
//!
//! - the fly stood on **fewer than [`MIN_TILES`] distinct `(map, tile)`s**, or
//! - **one macro sequence repeated more than [`MAX_REPEATS`] times** (any period up to
//!   [`MAX_PERIOD`] macros).
//!
//! Neither is a rule about what the fly *should* do — it is free to stand still, and silence
//! waits. Both are rules about what a *loop* looks like from outside: the macros finish, the
//! ledgers do not move, and the ground under the fly is the same three tiles.
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-loop.checkpoint \
//!   FLY_TRAP_MINUTES=30 cargo run --release -p flysim --example trap_hunt
//! ```
//!
//! Without the ROM it prints how to run it and nothing else. The cartridge is never copied into
//! this repository; it is read from the path in `FLY_ROM` and nothing else. Neither is a
//! checkpoint: `.local/` is not tracked.
//!
//! | env | default | meaning |
//! | --- | --- | --- |
//! | `FLY_ROM` | — | the cartridge; without it this prints instructions |
//! | `FLY_TRAP_CHECKPOINT` | `FLY_MACRO_CHECKPOINT` | the `FLYSIM01` checkpoint to start from |
//! | `FLY_TRAP_MINUTES` | 30 | brain minutes to run |
//! | `FLY_TRAP_STUB` | unset | `1` drives the macros from a rotating stub over [`STUB_CHANNELS`] instead of the brain's readout, so two builds with different channel lists are comparable |
//! | `FLY_TRAP_MODE` | `macros` | `macros` or `raw` |
//! | `FLY_MACRO_BRAIN` | `FLY_DATASET`, else `data/fafb-v783` | the connectome |
//! | `FLY_TRAP_THREADS` | 4 | sweep threads |
//! | `FLY_TRAP_SEED` | 20260917 | seeds the palette |
//! | `FLY_TRAP_SEED_*` | unset | rebuilds session ledgers a restore starts empty: `PUSHED`, `EXHAUSTED`, `TALKED`, `BLOCKED`, `STOOD` (`examples/support/ledgers.rs`, row 57) |
//!
//! The frame order is `simloop.rs`'s, as `examples/palette_bench.rs` expresses it, so what this
//! measures is the loop that ships rather than a second implementation of it. Without a
//! checkpoint it refuses rather than booting the intro: a trap hunt is about a state the stream
//! was actually in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flybrain_core::agent::{AgentConfig, NeuralAgent, RewardEvent as NeuralReward, TickOptions};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::{gameboy_decoder_config_with_macros, to_button_mask};
use flybrain_core::lif::SweepPlan;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::recovery::{NeuralRecovery, recover_game};
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

#[path = "support/ledgers.rs"]
mod ledgers;

/// One brain minute in milliseconds.
const MINUTE_MS: f64 = 60_000.0;

/// The window a trap is measured over: two brain minutes, which is the ratchet's own stall window
/// (120 brain seconds, `flybrain_gb::ratchet::STALL_MS`) — long enough that a walk across a town
/// fits inside it and short enough that a loop cannot hide in it.
const WINDOW_MS: f64 = 2.0 * MINUTE_MS;

/// How far one window's start is from the next. Fifteen brain seconds, so a loop that begins
/// anywhere is inside some window whole rather than split across two.
const WINDOW_STEP_MS: f64 = 15_000.0;

/// Distinct `(map, tile)`s a window must hold to count as going somewhere.
///
/// Four, which is the operator's number and the shape of the Viridian loop: the tile outside the door, the
/// doormat inside it, and one tile of frontier. Three is a loop; four is a walk.
const MIN_TILES: usize = 4;

/// Times one macro sequence may repeat inside a window before it is a loop.
const MAX_REPEATS: usize = 10;

/// The longest macro sequence a repeat is looked for at. Eight, which is longer than any scene's
/// pad, so a cycle that visits every button of a scene in turn is still caught.
const MAX_PERIOD: usize = 8;

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Every macro channel the hunt's stub rotates over, whatever the build under test has.
///
/// `FLY_TRAP_STUB=1` replaces the brain's readout with one hot macro channel per hold, rotating
/// over this list — and the list is spelled out here rather than taken from
/// `flybrain_gb::macro_channels` **so that two builds with different channel lists get the same
/// rotation**. That is the only way a before/after over a change that adds macro types can be
/// compared at all: `tools/build_flywire.py` re-deals every `macro_<type>` population whenever a
/// type is added (206 neurons thirty-one ways is not twenty-two ways plus nine), so the *brain's*
/// preference over macros is a different function in the two arms and a run driven by it cannot
/// separate "the macros got worse" from "the populations moved".
///
/// A name the build under test does not have simply wins no hold, which is the honest reading of
/// a build that does not have that button — so the list is the **union** over the builds being
/// compared rather than either one's own, or an arm loses a scene it can act in.
const STUB_CHANNELS: [&str; 32] = [
    "macro_go_objective",
    "macro_go_out",
    "macro_go_warp",
    "macro_go_route",
    "macro_go_item",
    "macro_go_npc",
    "macro_go_frontier",
    "macro_go_shop",
    "macro_go_heal",
    "macro_talk",
    "macro_menu",
    "macro_next",
    "macro_yes",
    "macro_no",
    "macro_close",
    "macro_confirm",
    "macro_back",
    // `macro_attack` is the *union*, not a mistake: it is the button the build before section 14
    // ends a battle with, and a list without it left that build's arm sitting in one unbroken
    // battle for 69,977 of 71,673 frames — a fact about the rotation, not about the macros. Each
    // arm gets every button it has, and a name the build does not have wins no hold.
    "macro_attack",
    "macro_move_1",
    "macro_move_2",
    "macro_move_3",
    "macro_move_4",
    "macro_switch",
    "macro_item",
    "macro_throw_ball",
    "macro_run",
    "macro_buy_potion",
    "macro_buy_ball",
    "macro_buy_antidote",
    "macro_buy_repel",
    "macro_heal",
    "macro_leave",
];

/// Holds one stub channel stays hot before the rotation moves on.
///
/// One, so twenty brain minutes is about twenty-four passes over the thirty-one — enough that
/// every button is offered many times and short enough that no single one owns the run.
const STUB_HOLDS_PER_CHANNEL: usize = 1;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// The neural half of a ratchet recovery, exactly as `simloop.rs` wires it.
struct AgentRecovery<'a> {
    agent: &'a mut NeuralAgent,
}

impl NeuralRecovery for AgentRecovery<'_> {
    fn clear_decoder_holds(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.decoder.clear_holds(ms);
    }

    fn clear_eligibility(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.network.plasticity.clear_eligibility(ms);
    }

    fn set_visual_frame(&mut self, frame: &[u8]) {
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent.network.set_visual_frame(frame, width, height);
    }
}

/// Where the fly stood on one frame, and what it started on it.
/// A refused macro and the `(map, x, y)` it was refused on.
type RefusedAt = (&'static str, Option<(u32, u32, u32)>);

struct Trace {
    /// `(brain ms, map, x, y)` for every frame the adapter could place the player on.
    steps: Vec<(f64, u32, u32, u32)>,
    /// `(brain ms, macro name)` for every macro that started.
    starts: Vec<(f64, &'static str)>,
    /// One entry per macro that ran to an outcome, for the walk report.
    episodes: Vec<Episode>,
    began_ms: f64,
    ended_ms: f64,
    frames: u64,
    recoveries: u64,
    rungs: Vec<(u32, &'static str, f64)>,
    outcomes: BTreeMap<&'static str, u64>,
    /// What `FLY_TRAP_SEED_*` rebuilt, when anything (row 57).
    seeded: Option<String>,
    /// `refused` outcomes by macro, and the run of one macro refused with the fly on one tile:
    /// row 57's pad was one button refused 740 times running.
    refusals: BTreeMap<&'static str, u64>,
    refusal_run: (Option<RefusedAt>, u64),
    longest_refusal_run: (u64, &'static str),
    /// Frames spent in each scene, so a window full of macros can be read back to the scene that
    /// dealt them.
    scenes: BTreeMap<&'static str, u64>,
    /// The scene and the adapter's mode on the last frame, for a run that ended somewhere odd.
    ended_in: (&'static str, String),
    /// The longest run of consecutive frames in one scene, and when it began.
    longest_scene: BTreeMap<&'static str, (u64, f64)>,
    /// Frames spent in a text box, by the map they were spent on, and the tile the fly was on.
    ///
    /// "The scene reads `dialog` and stays there" is the one trap in `infra/docs/macros-traps.md`
    /// that the hunt could report and not *locate*: a scene histogram says two thirds of a run was
    /// a text box and nothing about which box. A map and a tile name the conversation, which is
    /// what a fix has to be about.
    dialog_frames: BTreeMap<(u8, u8, u8), u64>,
    /// Macro starts in a text box, by name and map: which press the fly answered it with.
    dialog_macros: BTreeMap<(&'static str, u8), u64>,
    /// Every input of the detector's disputed branch on the last frame
    /// (`pokemon_red::scene::why_unknown`).
    ended_why: String,
    /// The whole-map grid on the last frame, as [`grid_line`] reads it
    /// (`docs/design/macros.md` section 15): how much of the map is ground, how much of it the fly
    /// could reach from where it stopped, and how much of that it had never stood on. A hunt that
    /// ends with reachable far below walkable ended fenced in, which no amount of re-planning was
    /// ever going to fix.
    ended_grid: String,
    /// How the dialog branch's two halves agreed, per frame: `wFontLoaded`'s bit against the four
    /// corners and against the whole `TextBoxBorder` (`pokemon_red::state::dialog_border`).
    ///
    /// `font_corners_no_border` is the false positive the 2026-09-17 residual has to be tested
    /// for: a frame the detector calls `dialog` on four map tiles that hold frame tile ids.
    font_corners_border: u64,
    font_corners_no_border: u64,
    font_no_corners: u64,
    corners_no_font: u64,
    /// Frames spent in each battle sub-state, by [`battle_sub_state`]'s name.
    ///
    /// The scene histogram says "battle" and a battle has five sub-states with five different
    /// pads (`docs/design/macros.md` section 12.6, 13.1), so a loop inside one of them is
    /// invisible above. This is what names it.
    battle_frames: BTreeMap<&'static str, u64>,
    /// Macro starts by battle sub-state: which button the fly pressed on which of the five pads.
    battle_starts: BTreeMap<(&'static str, &'static str), u64>,
    /// Every macro channel that was bound in each battle sub-state, over the whole run.
    ///
    /// The pad's composition, measured rather than read off the table: "which sub-state offers
    /// `BACK`" is a question about the build under test and not about the document.
    battle_pads: BTreeMap<&'static str, BTreeSet<String>>,
    wall_seconds: f64,
}

/// Which sub-state of a battle this frame is, or `None` when no battle is running.
///
/// The five the pad is dealt by: the top-level menu, the move list, the party list, the bag, the
/// forced switch, and the frames between turns where no list is accepting input.
fn battle_sub_state(emulator: &mut flybrain_gb::Emulator) -> Option<&'static str> {
    use flybrain_gb::pokemon_red::macros::state::BattleMenu;
    let battle = flybrain_gb::pokemon_red::state::battle(emulator)?;
    if battle.forced_switch {
        return Some("forced switch");
    }
    Some(match battle.menu {
        BattleMenu::Main { .. } => "main menu",
        BattleMenu::Moves { cursor: Some(_), .. } => "move list",
        BattleMenu::Moves { cursor: None, .. } => "move list, no cursor",
        BattleMenu::Party { .. } => "party list",
        BattleMenu::Bag { .. } => "bag",
        BattleMenu::None => "between turns",
    })
}

/// The macro that owns the buttons, with where it began and the ground it has covered since.
struct Running {
    name: &'static str,
    from: Option<(u32, u32, u32)>,
    tiles: BTreeSet<(u32, u32, u32)>,
    frames: usize,
    reach: u32,
}

/// One macro from its start to its outcome, as the frames underneath it saw it.
///
/// What separates the three ways a walk can spend the frame cap (`infra/docs/macros-traps.md`):
/// a walk crossing a town covers `tiles` in a straight-ish line and ends `net` tiles from where it
/// began; a walk oscillating over the walkable window's edge covers two or three tiles and ends
/// where it started; a blocked one never leaves its tile at all.
struct Episode {
    name: &'static str,
    outcome: &'static str,
    frames: usize,
    /// Distinct `(map, x, y)` the player stood on while it ran.
    tiles: usize,
    /// Manhattan distance from the first tile to the last.
    net: u32,
    /// The furthest the player ever got from the tile it started on.
    reach: u32,
}

/// One window the hunt flagged.
struct Trap {
    at_minute: f64,
    tiles: usize,
    /// The repeated sequence and how many times it ran, when that is what flagged it.
    cycle: Option<(String, usize)>,
    macros: usize,
}

/// Run `minutes` brain minutes of the sim loop's frame order from `checkpoint`, recording where
/// the fly stood and what it started.
fn run(
    rom: &[u8],
    data: &Arc<flybrain_core::dataset::BrainDataset>,
    checkpoint: &flysim::store::Checkpoint,
    mode: MacroMode,
    minutes: f64,
    threads: usize,
    seed: u32,
) -> Trace {
    let began_wall = std::time::Instant::now();
    let stub = std::env::var("FLY_TRAP_STUB").is_ok_and(|value| value == "1");
    let mut stub_hold = 0usize;
    let mut stub_next_ms = f64::NEG_INFINITY;
    let mut emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    let mut adapter = PokemonRedReward::new();
    let channels =
        if mode.dealt() { flybrain_gb::macro_channels("pokemon-red") } else { Vec::new() };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .expect("the preset has a group")
        .hold_ms;
    let blocked_ms =
        preset.exclusive.as_ref().expect("the preset has an exclusive group").blocked_ms;
    let agent_config = AgentConfig::with_decoder(preset);
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());

    emulator
        .import_state(&checkpoint.runtime.emulator)
        .expect("the checkpoint's emulator state should import");
    adapter
        .import_state(&checkpoint.runtime.reward)
        .expect("the checkpoint's reward ledger should import");
    let snapshot = (!checkpoint.runtime.ratchet_game.is_empty()).then(|| {
        flybrain_gb::ratchet::Snapshot {
            game: checkpoint.runtime.ratchet_game.clone(),
            frame: checkpoint.runtime.ratchet_frame.clone(),
        }
    });
    ratchet
        .import(Some(checkpoint.runtime.ratchet), snapshot, adapter.rank_ladder().len())
        .expect("the checkpoint's ratchet state should import");

    let mut agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("a valid agent");
    if threads > 1 {
        agent.set_sweep_plan(SweepPlan::with_threads(threads).expect("a sweep plan"));
    }
    agent.import_state(&checkpoint.agent).expect("the checkpoint's agent should import");
    let (width, height) = (agent.frame.width, agent.frame.height);
    agent.network.set_visual_frame(&checkpoint.runtime.framebuffer, width, height);

    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = mode;
    let mut macros: Option<MacroLayer> = macro_layer(&config, hold_ms, seed);
    let mut seeded_note: Option<String> = None;
    // Row 57: a trap dealt by session ledgers does not come back from a checkpoint, because a
    // restore starts them empty. `FLY_TRAP_SEED_*` rebuilds them on the real palette, so both arms
    // of a hunt start inside the state the live session was in.
    if let Some(flavour) = config.macros.mode.palette_mode() {
        let mut palette =
            flybrain_gb::pokemon_red::macros::PokemonPalette::with_mode(seed, flavour);
        let ms = agent.network.ms;
        if let Some(seeded) =
            ledgers::seed(&mut palette, &mut emulator, &adapter, ms, "FLY_TRAP_SEED")
        {
            eprintln!("seeded the session ledgers: {seeded}");
            seeded_note = Some(seeded);
            macros = Some(MacroLayer::new(Box::new(palette), hold_ms));
        }
    }

    let began_ms = agent.network.ms;
    let until = began_ms + minutes * MINUTE_MS;
    let mut frame = emulator.framebuffer().to_vec();
    let mut payouts: Vec<flybrain_gb::RewardEvent> = Vec::new();
    let mut location = adapter.location();
    let mut blocked_since_ms = began_ms;
    let mut held_channel: Option<String> = agent.decoder.current().map(str::to_string);
    let mut rank = adapter.progress().rank;
    let mut running: Option<Running> = None;
    // A periodic one-liner for a run that is going nowhere: what the fly is standing on, what it
    // faces, and which text box the detector is looking at. Off unless asked for, because it is a
    // diagnostic and the tables above are the report.
    let trace_every_ms = env_f64("FLY_TRAP_TRACE_SECONDS", 0.0) * 1000.0;
    let mut next_trace = began_ms;
    // The scene of the frames in a row, for "stuck in a text box" against "in and out of one".
    let mut scene_run: (&'static str, u64, f64) = ("", 0, began_ms);
    let mut trace = Trace {
        steps: Vec::new(),
        starts: Vec::new(),
        episodes: Vec::new(),
        began_ms,
        ended_ms: began_ms,
        frames: 0,
        recoveries: 0,
        rungs: vec![(rank, adapter.progress().rank_label, 0.0)],
        outcomes: BTreeMap::new(),
        scenes: BTreeMap::new(),
        dialog_frames: BTreeMap::new(),
        dialog_macros: BTreeMap::new(),
        ended_in: ("", String::new()),
        longest_scene: BTreeMap::new(),
        ended_why: String::new(),
        ended_grid: String::new(),
        font_corners_border: 0,
        font_corners_no_border: 0,
        font_no_corners: 0,
        corners_no_font: 0,
        battle_frames: BTreeMap::new(),
        battle_starts: BTreeMap::new(),
        battle_pads: BTreeMap::new(),
        wall_seconds: 0.0,
        seeded: seeded_note,
        refusals: BTreeMap::new(),
        refusal_run: (None, 0),
        longest_refusal_run: (0, ""),
    };

    if let Some(layer) = macros.as_mut() {
        let ledger = AdapterLedger(&adapter);
        let _ = layer.observe(&mut emulator, &ledger, agent.network.ms);
    }

    while agent.network.ms < until {
        let rewards: Vec<NeuralReward> = payouts
            .iter()
            .map(|event| {
                NeuralReward::with_stimulation(event.value, f64::from(event.stimulation_ms))
            })
            .collect();
        let options = TickOptions { rewards: &rewards, boot: adapter.boot(), learn: true };
        let ms = agent.network.ms;
        let blocked = (blocked_ms > 0.0 && ms - blocked_since_ms >= blocked_ms)
            .then(|| agent.decoder.current().map(str::to_string))
            .flatten();
        let bound = macros.as_ref().map(MacroLayer::bound_channels);
        let result = agent
            .tick_bound(&frame, &options, blocked.as_deref(), bound.as_deref())
            .expect("a tick");
        // The brain is still ticked — the frame order, the plasticity and the cost are the run's —
        // and only the *readout* is replaced, so a stub run and a brain run differ in who chooses
        // and in nothing else.
        let active: Vec<String> = if stub {
            let hot = STUB_CHANNELS[(stub_hold / STUB_HOLDS_PER_CHANNEL) % STUB_CHANNELS.len()];
            if ms >= stub_next_ms {
                stub_next_ms = ms + hold_ms;
                stub_hold += 1;
            }
            bound
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|channel| *channel == hot)
                .cloned()
                .collect()
        } else {
            result.active.clone()
        };
        let held = agent.decoder.current().map(str::to_string);
        if held != held_channel {
            held_channel = held;
            blocked_since_ms = ms;
        }

        let ms = agent.network.ms;
        let mut mask = to_button_mask(&active);
        // Read before `decide`, because `decide` is what starts the macro whose scene this is.
        let layer_scene = macros.as_ref().map_or("", MacroLayer::scene_name);
        let dialog_map = (layer_scene == "dialog" || layer_scene == "unknown")
            .then(|| flybrain_gb::pokemon_red::state::player(&mut emulator).map(|p| p.map))
            .flatten();
        let battle_sub = battle_sub_state(&mut emulator);
        if let Some(sub) = battle_sub {
            *trace.battle_frames.entry(sub).or_insert(0) += 1;
            let pad = trace.battle_pads.entry(sub).or_default();
            for channel in bound.as_deref().unwrap_or_default() {
                pad.insert(channel.clone());
            }
        }
        if let Some(layer) = macros.as_mut() {
            let ledger = AdapterLedger(&adapter);
            let decision = layer.decide(&active, mask, ms, &mut emulator, &ledger);
            mask = decision.mask;
            for event in &decision.events {
                match event.outcome {
                    None => {
                        trace.starts.push((ms, event.name));
                        // Which press answered a box, and on which map: 991 `YES` in twenty brain
                        // minutes is a fact about one conversation, and this is what says which.
                        if let Some(map) = dialog_map {
                            *trace.dialog_macros.entry((event.name, map)).or_insert(0) += 1;
                        }
                        if let Some(sub) = battle_sub {
                            *trace.battle_starts.entry((event.name, sub)).or_insert(0) += 1;
                        }
                        running = Some(Running {
                            name: event.name,
                            from: location,
                            tiles: location.into_iter().collect(),
                            frames: 0,
                            reach: 0,
                        });
                    }
                    Some(outcome) => {
                        *trace.outcomes.entry(outcome.as_str()).or_insert(0) += 1;
                        if outcome.as_str() == "refused" {
                            *trace.refusals.entry(event.name).or_insert(0) += 1;
                            let key = Some((event.name, location));
                            trace.refusal_run = if trace.refusal_run.0 == key {
                                (key, trace.refusal_run.1 + 1)
                            } else {
                                (key, 1)
                            };
                            if trace.refusal_run.1 > trace.longest_refusal_run.0 {
                                trace.longest_refusal_run = (trace.refusal_run.1, event.name);
                            }
                        }
                        if let Some(run) = running.take() {
                            let net = match (run.from, location) {
                                (Some((map, x, y)), Some((at, ax, ay))) if map == at => {
                                    ax.abs_diff(x) + ay.abs_diff(y)
                                }
                                _ => 0,
                            };
                            trace.episodes.push(Episode {
                                name: run.name,
                                outcome: outcome.as_str(),
                                frames: run.frames,
                                tiles: run.tiles.len(),
                                net,
                                reach: run.reach,
                            });
                        }
                    }
                }
            }
        }
        {
            let text = flybrain_gb::pokemon_red::state::text_box(&mut emulator);
            let (corners, border) =
                flybrain_gb::pokemon_red::state::dialog_border(&mut emulator);
            match (text.open, corners, border) {
                (true, true, true) => trace.font_corners_border += 1,
                (true, true, false) => trace.font_corners_no_border += 1,
                (true, false, _) => trace.font_no_corners += 1,
                (false, true, _) => trace.corners_no_font += 1,
                (false, false, _) => {}
            }
        }
        if let Some(layer) = macros.as_ref() {
            let name = layer.scene_name();
            *trace.scenes.entry(name).or_insert(0) += 1;
            if name == scene_run.0 {
                scene_run.1 += 1;
            } else {
                scene_run = (name, 1, ms);
            }
            let longest = trace.longest_scene.entry(name).or_insert((0, 0.0));
            if scene_run.1 > longest.0 {
                *longest = (scene_run.1, scene_run.2 - began_ms);
            }
            // Where the text box is, which is the half the scene histogram could not say.
            if name == "dialog" || name == "unknown" {
                let where_ = flybrain_gb::pokemon_red::state::player(&mut emulator)
                    .map(|player| (player.map, player.x, player.y));
                if let Some(key) = where_ {
                    *trace.dialog_frames.entry(key).or_insert(0) += 1;
                }
            }
        }
        emulator.set_buttons(mask as u8);
        emulator.run_frame().expect("a frame should complete");
        trace.frames += 1;
        frame.copy_from_slice(emulator.framebuffer());

        payouts = adapter.sample(&mut emulator, ms);
        if let Some(layer) = macros.as_mut() {
            let ledger = AdapterLedger(&adapter);
            let _ = layer.observe(&mut emulator, &ledger, agent.network.ms);
        }

        if trace_every_ms > 0.0 && ms >= next_trace {
            next_trace = ms + trace_every_ms;
            let scene = macros.as_ref().map_or("", MacroLayer::scene_name);
            use flybrain_gb::pokemon_red::macros::cartridge::{MacroState, Tile};
            // Read before the state borrows the emulator: this is the same call the state makes,
            // and the only one that can say *which* refusal a frame is.
            let refusal = flybrain_gb::pokemon_red::state::map_grid(&mut emulator).err();
            let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut emulator);
            let state: &mut dyn MacroState = &mut state;
            let player = state.player();
            let ahead = player.and_then(|player| {
                let ahead = Tile::new(player.x, player.y).step(player.facing)?;
                flybrain_gb::pokemon_red::macros::path::target_at(state, ahead)
            });
            let ground = grid_line(state, player, refusal);
            let why = flybrain_gb::pokemon_red::scene::why_unknown(&mut emulator);
            println!(
                "trace {:7.2} min  scene={scene:<9} player={player:?} ahead={ahead:?}\n    {why}\n    {ground}",
                (ms - began_ms) / MINUTE_MS
            );
        }

        let now = adapter.location();
        if now.is_some() && now != location {
            location = now;
            blocked_since_ms = ms;
        }
        if let Some((map, x, y)) = location {
            trace.steps.push((ms, map, x, y));
        }
        if let Some(run) = running.as_mut() {
            run.frames += 1;
            if let Some(at) = location {
                run.tiles.insert(at);
                if let (Some((map, x, y)), (on, ax, ay)) = (run.from, at)
                    && map == on
                {
                    run.reach = run.reach.max(ax.abs_diff(x) + ay.abs_diff(y));
                }
            }
        }

        let progress = adapter.progress();
        if progress.rank != rank {
            rank = progress.rank;
            trace.rungs.push((rank, progress.rank_label, ms - began_ms));
        }
        let safe = adapter.safe_for_snapshot();
        let capture_due = safe && u64::from(progress.rank) > ratchet.state.best;
        let captured = capture_due.then(|| flybrain_gb::ratchet::Snapshot {
            game: emulator.export_state().expect("state export"),
            frame: frame.clone(),
        });
        let recover = ratchet.observe_with_game_over(
            safe,
            u64::from(progress.rank),
            progress.unique_locations as u64,
            ms as u64,
            adapter.game_over(),
            || captured.expect("the ratchet only captures when a snapshot was prepared"),
        );
        if recover {
            let snapshot = flybrain_gb::ratchet::Snapshot {
                game: ratchet.game().expect("a recovery has a snapshot").to_vec(),
                frame: ratchet.frame().expect("a recovery has a framebuffer").to_vec(),
            };
            let restored = {
                let mut neural = AgentRecovery { agent: &mut agent };
                recover_game(&mut emulator, &mut adapter, &mut neural, &snapshot)
                    .expect("recovering the game")
            };
            frame.copy_from_slice(&restored);
            emulator.set_buttons(0);
            trace.recoveries += 1;
            location = adapter.location();
            held_channel = None;
            blocked_since_ms = ms;
            if let Some(layer) = macros.as_mut() {
                layer.cancel(ms);
            }
            running = None;
        }
    }
    trace.ended_in = (
        macros.as_ref().map_or("", MacroLayer::scene_name),
        adapter.mode().to_string(),
    );
    trace.ended_why = flybrain_gb::pokemon_red::scene::why_unknown(&mut emulator);
    trace.ended_grid = {
        use flybrain_gb::pokemon_red::macros::cartridge::MacroState;
        let refusal = flybrain_gb::pokemon_red::state::map_grid(&mut emulator).err();
        let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut emulator);
        let state: &mut dyn MacroState = &mut state;
        let player = state.player();
        grid_line(state, player, refusal)
    };
    trace.ended_ms = agent.network.ms;
    trace.wall_seconds = began_wall.elapsed().as_secs_f64();
    trace
}

/// The longest run of one repeated block in `names`, as `(block, repeats)`.
///
/// Every period up to [`MAX_PERIOD`], every offset, so a cycle that starts part-way into the
/// window is found where it starts rather than missed. A period of one is a single macro over and
/// over, which is the shape the battle-text deadlock of v0.2.4 had.
fn longest_cycle(names: &[&'static str]) -> Option<(String, usize)> {
    let mut best: Option<(String, usize)> = None;
    for period in 1..=MAX_PERIOD.min(names.len()) {
        let mut start = 0;
        while start + period <= names.len() {
            let block = &names[start..start + period];
            let mut repeats = 1;
            while start + period * (repeats + 1) <= names.len()
                && &names[start + period * repeats..start + period * (repeats + 1)] == block
            {
                repeats += 1;
            }
            if best.as_ref().is_none_or(|(_, known)| repeats > *known) {
                best = Some((block.join(", "), repeats));
            }
            start += period * repeats;
        }
    }
    best
}

/// Every window of the trace that is a trap by either rule.
fn hunt(trace: &Trace) -> Vec<Trap> {
    let mut traps = Vec::new();
    let mut at = trace.began_ms;
    while at + WINDOW_MS <= trace.ended_ms {
        let until = at + WINDOW_MS;
        let tiles: BTreeSet<(u32, u32, u32)> = trace
            .steps
            .iter()
            .filter(|(ms, ..)| *ms >= at && *ms < until)
            .map(|(_, map, x, y)| (*map, *x, *y))
            .collect();
        let names: Vec<&'static str> = trace
            .starts
            .iter()
            .filter(|(ms, _)| *ms >= at && *ms < until)
            .map(|(_, name)| *name)
            .collect();
        let cycle = longest_cycle(&names).filter(|(_, repeats)| *repeats > MAX_REPEATS);
        // A window with no macro in it at all is a fly that chose nothing, which is the doctrine
        // working rather than a trap: silence waits. Only a window that *did* things and got
        // nowhere counts.
        let stuck = tiles.len() < MIN_TILES && !names.is_empty();
        if stuck || cycle.is_some() {
            traps.push(Trap {
                at_minute: (at - trace.began_ms) / MINUTE_MS,
                tiles: tiles.len(),
                cycle,
                macros: names.len(),
            });
        }
        at += WINDOW_STEP_MS;
    }
    traps
}

/// One row per `(macro, outcome)`: how long it ran and how much ground it covered.
///
/// This is the diagnostic half of the hunt. The windows say *that* the fly is going nowhere; this
/// says which macro spent the frames and whether it was walking, oscillating or stuck: `tiles` and
/// `net` near one on a `timeout` that spent the whole cap is a walk re-planning in place, and
/// `net` in the tens is a walk the cap simply cut in half.
fn walk_report(trace: &Trace) {
    if trace.episodes.is_empty() {
        return;
    }
    let mut rows: BTreeMap<(&'static str, &'static str), Vec<&Episode>> = BTreeMap::new();
    for episode in &trace.episodes {
        rows.entry((episode.name, episode.outcome)).or_default().push(episode);
    }
    println!("\n| macro | outcome | n | mean frames | mean tiles | mean net | mean reach | max net |");
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for ((name, outcome), episodes) in &rows {
        let n = episodes.len() as f64;
        let mean = |total: usize| total as f64 / n;
        println!(
            "| {name} | {outcome} | {} | {:.0} | {:.1} | {:.1} | {:.1} | {} |",
            episodes.len(),
            mean(episodes.iter().map(|e| e.frames).sum()),
            mean(episodes.iter().map(|e| e.tiles).sum()),
            mean(episodes.iter().map(|e| e.net as usize).sum()),
            mean(episodes.iter().map(|e| e.reach as usize).sum()),
            episodes.iter().map(|e| e.net).max().unwrap_or(0),
        );
    }
}

/// The whole-map grid in one line: what a stalled walk looks like from outside.
///
/// `docs/design/macros.md` section 15. Walkable is how much of the map is ground, reachable is
/// how much of that the fly can get to from where it is standing (the directed walls respected),
/// and unstood is how much of *that* this run has never been on -- which is the frontier's own
/// candidate pool. A walk that cannot finish is one of three shapes and these numbers tell them
/// apart: fenced in (reachable far below walkable), nothing left to explore (unstood zero), or no
/// grid at all, in which case the walks are back on the ten-by-nine window and the reason is
/// named.
fn grid_line(
    state: &mut dyn flybrain_gb::pokemon_red::macros::cartridge::MacroState,
    player: Option<flybrain_gb::pokemon_red::macros::state::Player>,
    refusal: Option<flybrain_gb::pokemon_red::state::GridRefusal>,
) -> String {
    let Some(player) = player else { return "grid: no player".to_string() };
    let Some(grid) = state.map_grid() else {
        // Which of section 15's refusals this frame is, rather than a bare "none": a walk that is
        // on the window reading should say why it is.
        return format!("grid: none ({})", refusal.map_or("unknown", |refusal| refusal.label()));
    };
    let unstood = grid
        .walkable_tiles()
        .into_iter()
        .filter(|(x, y)| !state.tile_visited(*x, *y))
        .count();
    format!(
        "grid map={:#04x} {}x{} walkable={} reachable={} unstood={}",
        grid.map(),
        grid.width(),
        grid.height(),
        grid.walkable_count(),
        grid.reachable_from(player.x, player.y),
        unstood
    )
}

fn main() {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        println!(
            "FLY_ROM is not set, so there is nothing to hunt.\n\
             \n  FLY_ROM=/path/to/pokemon-red.gb \\\n    \
             FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-loop.checkpoint \\\n    \
             FLY_TRAP_MINUTES=30 cargo run --release -p flysim --example trap_hunt\n\n\
             It reports every two-brain-minute window in which the fly stood on fewer than {} \n\
             distinct (map, tile)s or repeated one macro sequence more than {} times.",
            MIN_TILES, MAX_REPEATS
        );
        return;
    };
    let rom = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("FLY_ROM is {path:?} but could not be read: {error}"));

    let Some(checkpoint_path) = std::env::var_os("FLY_TRAP_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_MACRO_CHECKPOINT"))
    else {
        println!(
            "FLY_TRAP_CHECKPOINT is not set. A trap hunt is about a state the stream was really \n\
             in, so this example does not boot the intro to invent one: point it at a FLYSIM01 \n\
             checkpoint (`.local/checkpoints/...`, never committed)."
        );
        return;
    };
    let checkpoint = flysim::store::load(Path::new(&checkpoint_path))
        .expect("FLY_TRAP_CHECKPOINT should be a FLYSIM01 envelope");

    let brain = std::env::var_os("FLY_MACRO_BRAIN")
        .or_else(|| std::env::var_os("FLY_DATASET"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/fafb-v783"));
    let data = Arc::new(
        load_brain_dataset_from_dir(&brain)
            .unwrap_or_else(|error| panic!("loading the connectome at {brain:?}: {error}")),
    );

    let minutes = env_f64("FLY_TRAP_MINUTES", 30.0);
    let threads = env_usize("FLY_TRAP_THREADS", 4);
    let seed = env_usize("FLY_TRAP_SEED", 20_260_917) as u32;
    let mode = match std::env::var("FLY_TRAP_MODE").unwrap_or_else(|_| "macros".to_string()).as_str()
    {
        "raw" => MacroMode::Raw,
        "macros" | "palette" | "plan" => MacroMode::Macros,
        other => panic!("FLY_TRAP_MODE: {other:?} is not a mode (raw or macros)"),
    };

    let trace = run(&rom, &data, &checkpoint, mode, minutes, threads, seed);
    let traps = hunt(&trace);
    let windows = {
        let mut count = 0usize;
        let mut at = trace.began_ms;
        while at + WINDOW_MS <= trace.ended_ms {
            count += 1;
            at += WINDOW_STEP_MS;
        }
        count
    };
    let ground: BTreeSet<(u32, u32, u32)> =
        trace.steps.iter().map(|(_, map, x, y)| (*map, *x, *y)).collect();

    println!("# Trap hunt: {} mode\n", mode.as_str());
    println!(
        "{:.2} brain minutes over {} frames in {:.0} s wall, from `{}`.\n",
        (trace.ended_ms - trace.began_ms) / MINUTE_MS,
        trace.frames,
        trace.wall_seconds,
        Path::new(&checkpoint_path).display()
    );
    if let Some(seeded) = &trace.seeded {
        println!("Session ledgers rebuilt before the first frame (`FLY_TRAP_SEED_*`): {seeded}.\n");
    }
    println!("| measure | value |");
    println!("| --- | ---: |");
    println!("| rung reached | {} |", trace.rungs.iter().map(|(rank, ..)| *rank).max().unwrap_or(0));
    println!("| distinct (map, tile) | {} |", ground.len());
    println!("| the map at the end | {} |", trace.ended_grid);
    println!("| macros started | {} |", trace.starts.len());
    for (outcome, count) in &trace.outcomes {
        println!("| {outcome} | {count} |");
    }
    if !trace.refusals.is_empty() {
        let by: Vec<String> =
            trace.refusals.iter().map(|(name, count)| format!("`{name}` {count}")).collect();
        println!("| refused, by macro | {} |", by.join(", "));
        println!(
            "| longest run of one macro refused on one tile | {} (`{}`) |",
            trace.longest_refusal_run.0, trace.longest_refusal_run.1
        );
    }
    println!("| recoveries | {} |", trace.recoveries);
    println!("| windows examined | {windows} |");
    println!("| windows flagged | {} |", traps.len());
    println!();
    if traps.is_empty() {
        println!(
            "No window of {:.0} brain minutes held fewer than {} distinct (map, tile)s or one \
             macro sequence more than {} times.\n",
            WINDOW_MS / MINUTE_MS,
            MIN_TILES,
            MAX_REPEATS
        );
    } else {
        println!("| at (brain min) | tiles | macros | repeated sequence |");
        println!("| ---: | ---: | ---: | --- |");
        for trap in &traps {
            let cycle = match &trap.cycle {
                Some((block, repeats)) => format!("`{block}` x{repeats}"),
                None => "—".to_string(),
            };
            println!(
                "| {:.2} | {} | {} | {} |",
                trap.at_minute, trap.tiles, trap.macros, cycle
            );
        }
        println!();
    }
    if !trace.dialog_frames.is_empty() {
        println!("\n| text box on map | tile | frames |");
        println!("| --- | --- | ---: |");
        let mut rows: Vec<((u8, u8, u8), u64)> =
            trace.dialog_frames.iter().map(|(key, n)| (*key, *n)).collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((map, x, y), frames) in rows.into_iter().take(8) {
            println!("| {map:#04x} | ({x}, {y}) | {frames} |");
        }
    }
    if !trace.dialog_macros.is_empty() {
        println!("\n| press in a text box | map | n |");
        println!("| --- | --- | ---: |");
        let mut rows: Vec<((&str, u8), u64)> =
            trace.dialog_macros.iter().map(|(key, n)| (*key, *n)).collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((name, map), n) in rows.into_iter().take(8) {
            println!("| {name} | {map:#04x} | {n} |");
        }
    }

    if !trace.battle_frames.is_empty() {
        println!("\n| battle sub-state | frames | pad |");
        println!("| --- | ---: | --- |");
        for (sub, frames) in &trace.battle_frames {
            let pad = trace
                .battle_pads
                .get(sub)
                .map(|set| {
                    set.iter()
                        .map(|c| c.strip_prefix("macro_").unwrap_or(c).to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!("| {sub} | {frames} | {pad} |");
        }
        println!("\n| macro start | battle sub-state | n |");
        println!("| --- | --- | ---: |");
        let mut rows: Vec<((&str, &str), u64)> =
            trace.battle_starts.iter().map(|(key, n)| (*key, *n)).collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((name, sub), n) in rows {
            println!("| {name} | {sub} | {n} |");
        }
    }
    println!("\n| scene | frames | longest run | run began (brain min) |");
    println!("| --- | ---: | ---: | ---: |");
    for (scene, frames) in &trace.scenes {
        let (longest, at) = trace.longest_scene.get(scene).copied().unwrap_or((0, 0.0));
        println!("| {scene} | {frames} | {longest} | {:.2} |", at / MINUTE_MS);
    }
    println!("\n| the dialog branch's two halves | frames |");
    println!("| --- | ---: |");
    println!("| font set, corners drawn, whole border drawn | {} |", trace.font_corners_border);
    println!(
        "| font set, corners drawn, **border not** (a false `dialog`) | {} |",
        trace.font_corners_no_border
    );
    println!("| font set, corners not drawn (menu or unknown) | {} |", trace.font_no_corners);
    println!("| corners drawn, font clear (harmless: overworld) | {} |", trace.corners_no_font);
    println!(
        "\nEnded in scene `{}`, adapter mode `{}`:\n\n```\n{}\n```",
        trace.ended_in.0, trace.ended_in.1, trace.ended_why
    );
    walk_report(&trace);
    for (rank, label, ms) in &trace.rungs {
        println!("- rung {rank} {label} at {:.2} brain minutes", ms / MINUTE_MS);
    }
    println!(
        "\nWindows overlap by design ({:.0} s apart over a {:.0} s window), so one loop is \
         reported by every window it fills.",
        WINDOW_STEP_MS / 1000.0,
        WINDOW_MS / 1000.0
    );
}
