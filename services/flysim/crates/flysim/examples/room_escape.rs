//! How long does the readout take to walk out of a room?
//!
//! `docs/design/room-escape.md` §1, "Straighter runs": the Game Boy preset commits to a direction
//! for 400 ms, one overworld step is about 270 ms, so a run is one to two steps and the walker
//! jitters in place. This example measures the fix before it is baked in: the *real* decoder, the
//! *real* Pokémon adapter and the real cartridge, with the exclusive group's direction hold,
//! decision period and fatigue gain overridden per run.
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo run --release -p flysim --example room_escape
//! ```
//!
//! Without the ROM it prints how to run it and exits 0. The cartridge is never copied into this
//! repository; it is read from the path in `FLY_ROM` and nothing else.
//!
//! ## What it reports
//!
//! For each `(hold, fatigue gain)` candidate and each of two starting rooms — Red's bedroom
//! (`REDS_HOUSE_2F`) and the ground floor (`REDS_HOUSE_1F`) — `FLY_ESCAPE_SEEDS` seeded runs of
//! `FLY_ESCAPE_MINUTES` brain minutes each, unthrottled, reporting the fraction of runs that leave
//! the starting map at all and the median brain time to leave over the runs that did. Runs are
//! independent, so they are spread across `FLY_ESCAPE_JOBS` threads.
//!
//! Rates are a seeded uniform random source, not a brain: this measures the *readout*, and a
//! random walker is the only way to measure it without 139,255 neurons of confound. That is also
//! why the chosen value has to be confirmed against the real brain, which `FLY_ESCAPE_BRAIN`
//! does: `FLY_ESCAPE_BRAIN_RUNS` runs through `NeuralAgent` (warm-up included) on the dataset at
//! `FLY_DATASET`, with the adapter's payouts reinforcing as they would on the stream.
//!
//! None of this is evidence about the fly. A random walker that leaves a room faster tells you the
//! readout stopped cancelling itself out, and nothing more.
//!
//! ## Starting states
//!
//! `FLY_ESCAPE_STATE` optionally points at a binjgb save state to start every bedroom run from.
//! Without it the example boots the cartridge itself: Start and A on a slow alternation through
//! the intro and the naming screens, then B until the adapter reports a stable, unscripted,
//! dialogue-free overworld sample in the bedroom. That state is then shared by every run, so the
//! intro is emulated once rather than 160 times.
//!
//! The ground-floor state is produced the same way and cached: one run walks down the stairs and
//! its state is written to `FLY_ESCAPE_STATE_1F` (default: `room-escape-1f.state` beside the
//! working directory), which later invocations reuse.
//!
//! ## v0.1.1: starting from the live fly
//!
//! `FLY_ESCAPE_CHECKPOINT` points at a `FLYSIM01` checkpoint -- the release box's own durable
//! state, pulled read-only -- and takes over the whole run: the random-walker sweep is skipped and
//! every candidate in [`CANDIDATES`] is measured forward from that exact state instead, with
//! `FLY_ESCAPE_BRAIN` supplying the connectome. It needs the real thing because the question
//! v0.1.1 asks is not "can a walker leave a room" (v0.1.0 measured that: always, at every hold
//! value) but "why did *this* fly, 43 brain minutes into a run with a full reward ledger and a
//! settled network, stay on the ground floor for forty more".
//!
//! What it reports per run, on top of the leave time: the direction-hold histogram, the mean
//! same-direction run length in decisions, the share of decisions the incumbent won by hysteresis
//! rather than by score, the holds over which the player never moved at all, and every frame and
//! decision spent standing on a tile one press from the exit. Those tiles are not hardcoded:
//! [`survey`] walks the map with real presses first and finds them.
//!
//! ```sh
//! FLY_ROM=... FLY_ESCAPE_CHECKPOINT=/path/to/525.checkpoint \
//!   FLY_ESCAPE_BRAIN=data/fafb-v783 FLY_ESCAPE_BRAIN_RUNS=5 FLY_ESCAPE_MINUTES=10 \
//!   FLY_ESCAPE_BRAIN_THREADS=3 FLY_ESCAPE_BRAIN_PARALLEL=5 \
//!   cargo run --release -p flysim --example room_escape
//! ```
//!
//! `FLY_ESCAPE_CANDIDATES` selects a subset by name, `FLY_ESCAPE_TARGET_MINUTES` sets the bar the
//! summary line scores against (5, from `docs/design/room-escape.md` section 3),
//! `FLY_ESCAPE_RUN_ON` spends the whole budget instead of stopping at the exit, and
//! `FLY_ESCAPE_SKIP_SWEEP` skips the random-walker tables on a fresh-boot run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use flybrain_core::agent::{
    AgentConfig, GAMEBOY_MS_PER_FRAME, NeuralAgent,
};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::{GAMEBOY_BUTTONS, gameboy_decoder_config, to_button_mask};
use flybrain_core::decoder::{DecoderConfig, PopulationDecoder};
use flybrain_core::lif::SweepPlan;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::{GameAdapter, MemoryReader};
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, SUPPORTED_ROM};
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, buttons};
use flysim::frame::{FrameObserver, FramePhase, LegacyFrame, Parts};

/// `constants/map_constants.asm`: Red's bedroom and the ground floor of his house.
const REDS_HOUSE_2F: u32 = 0x26;
const REDS_HOUSE_1F: u32 = 0x25;

const ROLES: [&str; 8] = [
    "command_0",
    "command_1",
    "command_2",
    "command_3",
    "command_4",
    "command_5",
    "command_6",
    "command_7",
];
/// A flat calibration point in the middle of the range below, so scores straddle 1.
const BASELINE_HZ: f64 = 30.0;
const MAX_HZ: f64 = 60.0;

/// Sixty seeds, the first twenty of which are the design's twenty.
///
/// Twenty is what `docs/design/room-escape.md` section 1 asks for and it is the default. It turned
/// out not to be enough to separate the candidates: every one of them leaves both rooms in every
/// one of twenty runs, so the leave fraction saturates and the median time to leave moves by more
/// between neighbouring hold values than it does across the whole range. `FLY_ESCAPE_SEEDS=60`
/// triples the power at no cost but wall time; the report carries both.
const SEEDS: [i32; 60] = [
    20260916, 4242, 7, 31337, 99991, 1, 2718281, 57721, 16180, 14142, 22360, 26457, 30000, 8675309,
    525600, 112358, 999, 4177, 60103, 271828, 13, 97, 1009, 2047, 3301, 4099, 5041, 6007, 7919,
    8191, 9001, 10007, 11111, 12289, 13331, 14407, 15551, 16661, 17389, 18433, 19553, 20611, 21701,
    22801, 23917, 24989, 26041, 27109, 28201, 29333, 30403, 31511, 32609, 33713, 34819, 35911,
    37013, 38119, 39217, 40321,
];

/// The candidates from `docs/design/room-escape.md` §1: hold = decision, fatigue gain 0.08 (today)
/// against 0.04 (the design's proposal).
const HOLDS: [f64; 4] = [400.0, 600.0, 800.0, 1000.0];
const FATIGUES: [f64; 2] = [0.08, 0.04];

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn env_list(name: &str, default: &[f64]) -> Vec<f64> {
    match std::env::var(name) {
        Ok(text) => text.split(',').filter_map(|piece| piece.trim().parse().ok()).collect(),
        Err(_) => default.to_vec(),
    }
}

/// The Game Boy preset with the exclusive group's timings overridden.
///
/// Every other channel — A, B, Start, Select, the hysteresis, the fatigue decay and the clear
/// lockout — is the preset's own, because those are not what is being measured.
fn tuned(
    hold_ms: f64,
    decision_ms: f64,
    fatigue_gain: f64,
    blocked_fatigue: f64,
    hysteresis: f64,
) -> DecoderConfig {
    let mut config = gameboy_decoder_config();
    let group = config.exclusive.as_mut().expect("the Game Boy preset has an exclusive group");
    group.hold_ms = hold_ms;
    group.decision_ms = decision_ms;
    group.fatigue_gain = fatigue_gain;
    group.hysteresis = hysteresis;
    group.blocked_fatigue = blocked_fatigue;
    group.blocked_ms = if blocked_fatigue > 0.0 { hold_ms } else { 0.0 };
    config
}

/// The `xorshift` generator the workspace's fixtures use.
fn xorshift(seed: i32) -> impl FnMut() -> f64 {
    let mut state = if seed == 0 { 1 } else { seed };
    move || {
        state ^= state << 13;
        state ^= ((state as u32) >> 17) as i32;
        state ^= state << 5;
        f64::from(state as u32) / 4_294_967_296.0
    }
}

fn emulator(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge")
}

/// A calibrated decoder: the flat baseline every run scores against.
fn calibrated(config: DecoderConfig) -> PopulationDecoder {
    let mut decoder = PopulationDecoder::new(config).expect("a valid preset");
    decoder.calibrate(&NumberMap::from_pairs(ROLES.iter().map(|role| (*role, BASELINE_HZ))));
    decoder
}

/// Boot the cartridge to a playable bedroom and return the save state.
///
/// The button pattern is `tests/rom.rs`'s: Start and A on a slow alternation, because a held
/// button is ignored and each press has to be released, then B alone — the intro's A-mashing
/// leaves a text box open and until it is dismissed the player cannot move at all, and B advances
/// text without starting a conversation with whatever the player is facing.
fn boot_to_bedroom(rom: &[u8]) -> (Vec<u8>, f64) {
    let mut emulator = emulator(rom);
    let mut adapter = PokemonRedReward::new();
    let mut ms = 0.0;

    for frame in 0..6_000u32 {
        let mask = match frame % 32 {
            0..=7 => buttons::START,
            16..=23 => buttons::A,
            _ => buttons::NONE,
        };
        emulator.set_buttons(mask);
        emulator.run_frame().expect("a frame should complete");
        ms += GAMEBOY_MS_PER_FRAME;
        adapter.sample(&mut emulator, ms);
        if adapter.mode() == "OVERWORLD" && frame > 3_000 {
            break;
        }
    }
    for frame in 0..12_000u32 {
        let mask = if frame % 24 < 8 { buttons::B } else { buttons::NONE };
        emulator.set_buttons(mask);
        emulator.run_frame().expect("a frame should complete");
        ms += GAMEBOY_MS_PER_FRAME;
        adapter.sample(&mut emulator, ms);
        if adapter.safe_for_snapshot() {
            break;
        }
    }
    assert!(
        adapter.safe_for_snapshot(),
        "the intro never reached a stable dialogue-free overworld sample (mode {})",
        adapter.mode()
    );
    assert_eq!(
        adapter.map_id(),
        Some(REDS_HOUSE_2F),
        "a cold boot should end in Red's bedroom"
    );
    (emulator.export_state().expect("state export"), ms)
}

/// One measured run.
struct Run {
    /// Brain milliseconds at which the starting map was first left, or `None`.
    left_ms: Option<f64>,
    /// Brain milliseconds at which a map outside Red's house was first reached, or `None`.
    ///
    /// v0.1.0's tables reported `left_ms` only, which on the ground floor is the staircase: one
    /// step, and it lands in the other room of the same house. This column is the one the v0.1.1
    /// question is about, and it is here so that the walker says what the *best case* is for any
    /// policy that chooses directions without a plan.
    out_ms: Option<f64>,
    /// Maps visited, in order, starting with the room the run started in.
    maps: Vec<u32>,
    /// Distinct player coordinates the adapter saw over the whole budget, which for a fresh
    /// adapter is how much ground this run covered. Uncensored: unlike the time to leave, it is
    /// measured over the same brain minutes for every run, so it is the metric that discriminates
    /// once every candidate escapes every room.
    tiles: usize,
    /// A save state captured on the map named by [`Capture`].
    captured: Option<Vec<u8>>,
}

/// Where and when to export a starting state for a later sweep.
///
/// `after_ms` exists because the first safe sample after a warp is *on* the destination warp tile:
/// a ground-floor state captured there would sit on the staircase, and "leave the map" from the
/// staircase means one step back up. Waiting a few seconds puts the capture somewhere in the room
/// instead, so the sweep measures finding an exit rather than standing on one.
#[derive(Clone, Copy)]
struct Capture {
    map: u32,
    after_ms: f64,
}

/// Drive `config` from a seeded uniform random rate source and watch for a map change.
#[allow(clippy::too_many_arguments)]
fn walk(
    rom: &[u8],
    start: &[u8],
    start_map: u32,
    seed: i32,
    config: DecoderConfig,
    minutes: f64,
    capture: Option<Capture>,
    stop_on_leave: bool,
) -> Run {
    let mut emulator = emulator(rom);
    emulator.import_state(start).expect("the starting state should import");
    let mut adapter = PokemonRedReward::new();
    let mut decoder = calibrated(config);
    let mut random = xorshift(seed);

    let mut run =
        Run { left_ms: None, out_ms: None, maps: vec![start_map], tiles: 0, captured: None };
    let until = minutes * 60_000.0;
    let mut ms = 0.0;
    let mut arrived_ms = None;
    while ms < until {
        ms += GAMEBOY_MS_PER_FRAME;
        let mut rates = NumberMap::new();
        for role in ROLES {
            rates.set(role, random() * MAX_HZ);
        }
        let active = decoder.decode(&rates, ms, adapter.boot());
        emulator.set_buttons(to_button_mask(&active) as u8);
        emulator.run_frame().expect("a frame should complete");
        adapter.sample(&mut emulator, ms);
        run.tiles = adapter.progress().unique_locations;

        let Some(map) = adapter.map_id() else { continue };
        if run.maps.last() != Some(&map) {
            run.maps.push(map);
        }
        if map != start_map && run.left_ms.is_none() {
            run.left_ms = Some(ms);
        }
        if map != REDS_HOUSE_1F && map != REDS_HOUSE_2F && run.out_ms.is_none() {
            run.out_ms = Some(ms);
        }
        if let Some(capture) = capture
            && capture.map == map
            && run.captured.is_none()
        {
            let arrived = *arrived_ms.get_or_insert(ms);
            if ms - arrived >= capture.after_ms && adapter.safe_for_snapshot() {
                run.captured = Some(emulator.export_state().expect("state export"));
            }
        }
        if stop_on_leave && run.left_ms.is_some() && (capture.is_none() || run.captured.is_some())
        {
            break;
        }
    }
    run
}

/// Arithmetic mean, or `None` for no samples.
fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Quartile-free median: the mean of the two central values for an even count, as a spreadsheet
/// would compute it. Empty input has no median.
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite times"));
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

struct Job {
    room: &'static str,
    start_map: u32,
    hold: f64,
    fatigue: f64,
    hysteresis: f64,
    seed: i32,
}

#[derive(Default)]
struct Cell {
    left: Vec<f64>,
    out: Vec<f64>,
    runs: usize,
    tiles: Vec<f64>,
    /// Distinct maps each run reached beyond the one it started in.
    reached: Vec<f64>,
    maps: BTreeMap<u32, usize>,
}

/// Run every job across `threads` worker threads, each with its own emulator.
fn sweep(
    rom: &[u8],
    states: &BTreeMap<&'static str, Vec<u8>>,
    jobs: &[Job],
    minutes: f64,
    threads: usize,
) -> BTreeMap<(String, u64, u64, u64), Cell> {
    let next = AtomicUsize::new(0);
    let mut results: Vec<Vec<(usize, Run)>> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..threads.max(1) {
            handles.push(scope.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else { break };
                    let run = walk(
                        rom,
                        &states[job.room],
                        job.start_map,
                        job.seed,
                        tuned(job.hold, job.hold, job.fatigue, 0.0, job.hysteresis),
                        minutes,
                        None,
                        false,
                    );
                    mine.push((index, run));
                }
                mine
            }));
        }
        for handle in handles {
            results.push(handle.join().expect("a worker thread"));
        }
    });

    let mut cells: BTreeMap<(String, u64, u64, u64), Cell> = BTreeMap::new();
    for (index, run) in results.into_iter().flatten() {
        let job = &jobs[index];
        let key = (
            job.room.to_string(),
            job.hold as u64,
            (job.fatigue * 1000.0) as u64,
            (job.hysteresis * 1000.0) as u64,
        );
        let cell = cells.entry(key).or_default();
        cell.runs += 1;
        cell.tiles.push(run.tiles as f64);
        let distinct: std::collections::BTreeSet<u32> =
            run.maps.iter().skip(1).copied().collect();
        cell.reached.push(distinct.len() as f64);
        if let Some(ms) = run.left_ms {
            cell.left.push(ms);
        }
        if let Some(ms) = run.out_ms {
            cell.out.push(ms);
        }
        for map in distinct {
            *cell.maps.entry(map).or_insert(0) += 1;
        }
    }
    cells
}

fn print_table(title: &str, cells: &BTreeMap<(String, u64, u64, u64), Cell>, room: &str) {
    println!("\n### {title}\n");
    println!(
        "| hold = decision | fatigue gain | left the map | median to leave | mean to leave | \
         out of the house | median to leave it | median tiles | mean tiles | median maps |"
    );
    println!(
        "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for ((cell_room, hold, fatigue, hysteresis), cell) in cells {
        if cell_room != room {
            continue;
        }
        let mut left = cell.left.clone();
        let median_ms = median(&mut left)
            .map(|ms| format!("{:.1} s", ms / 1000.0))
            .unwrap_or_else(|| "—".to_string());
        let mut out = cell.out.clone();
        let median_out = median(&mut out)
            .map(|ms| format!("{:.1} s", ms / 1000.0))
            .unwrap_or_else(|| "—".to_string());
        let mut tiles = cell.tiles.clone();
        let median_tiles =
            median(&mut tiles).map(|count| format!("{count:.1}")).unwrap_or_else(|| "—".to_string());
        let mut reached = cell.reached.clone();
        let median_maps = median(&mut reached)
            .map(|count| format!("{count:.1}"))
            .unwrap_or_else(|| "—".to_string());
        println!(
            "| {hold} ms | {:.2} | {:.2} | {}/{} | {median_ms} | {} | {}/{} | {median_out} | \
             {median_tiles} | {} | {median_maps} |",
            *fatigue as f64 / 1000.0,
            *hysteresis as f64 / 1000.0,
            cell.left.len(),
            cell.runs,
            mean(&cell.left).map(|ms| format!("{:.1} s", ms / 1000.0)).unwrap_or_else(|| "—".to_string()),
            cell.out.len(),
            cell.runs,
            mean(&cell.tiles).map(|count| format!("{count:.1}")).unwrap_or_else(|| "—".to_string()),
        );
    }
    let maps: BTreeMap<u32, usize> = cells
        .iter()
        .filter(|((cell_room, _, _, _), _)| cell_room == room)
        .flat_map(|(_, cell)| cell.maps.iter())
        .fold(BTreeMap::new(), |mut all, (map, count)| {
            *all.entry(*map).or_insert(0) += count;
            all
        });
    let maps: Vec<String> = maps.iter().map(|(map, count)| format!("{map} in {count}")).collect();
    println!("\nMaps reached across every run in this table: {}.", maps.join(", "));
}

// -------------------------------------------------------------------------------------------
// The real brain
// -------------------------------------------------------------------------------------------

/// Where one brain run starts.
enum Start<'a> {
    /// A save state and the map it stands in: a fresh fly, warmed up here.
    Fresh { state: &'a [u8], map: u32, warmup_ms: u64 },
    /// A `FLYSIM01` checkpoint: the live fly, its emulator, its reward ledger and its ratchet,
    /// with the network's noise stream displaced by `rng` so the runs are independent.
    Live { checkpoint: &'a flysim::store::Checkpoint, rng: i32 },
}

/// The four exclusive channels, in preset order. The index is the `command_N` role.
const DIRECTIONS: [&str; 4] = ["up", "down", "left", "right"];

/// Everything one instrumented brain run reports.
///
/// The list is the v0.1.1 diagnosis list in `docs/design/room-escape.md` section 3: leave or no
/// leave, time to leave, coverage, the direction-hold histogram, how often hysteresis rather than
/// the argmax decided the winner, and what the fly did while it stood on a tile one press from the
/// exit.
#[derive(Debug, Clone, Default)]
struct Trace {
    /// Brain ms after the run's own start at which the starting map was first left.
    left_ms: Option<f64>,
    /// Brain ms at which a map outside the starting *building* was first reached. On the ground
    /// floor, leaving the map by the staircase is not leaving the house; this column is.
    out_ms: Option<f64>,
    /// Maps visited in order, starting with the one the run started in.
    maps: Vec<u32>,
    /// The adapter's lifetime unique-location count at the end of the run.
    tiles: usize,
    /// New unique locations this run added to that count.
    new_tiles: usize,
    /// Exclusive-group decisions taken.
    decisions: u64,
    /// Decisions won, per direction.
    wins: BTreeMap<&'static str, u64>,
    /// Decisions at which the fatigue-adjusted argmax lost to the incumbent under hysteresis.
    hysteresis_holds: u64,
    /// Length, in consecutive decisions, of every completed same-direction run.
    runs: Vec<f64>,
    /// Holds over which the player's position never changed, per direction: a wall bump.
    blocked_holds: BTreeMap<&'static str, u64>,
    /// Longest single stretch with no change of position, in brain ms.
    longest_still_ms: f64,
    /// Frames spent standing on a tile from which one press leaves the map, per tile.
    exit_tile_frames: BTreeMap<(u32, u32), u64>,
    /// Decisions taken while standing on such a tile.
    exit_decisions: u64,
    /// ...of which the winner was a direction that leaves from that tile.
    exit_decisions_taken: u64,
    /// Recoveries the ratchet fired during the run, and whether its attempt budget is now spent.
    recoveries: u64,
    budget_spent: bool,
    /// Reward paid during the run.
    reward: f64,
}

impl Trace {
    fn mean_run(&self) -> Option<f64> {
        mean(&self.runs)
    }

    fn win_share(&self, direction: &str) -> f64 {
        if self.decisions == 0 {
            return 0.0;
        }
        self.wins.get(direction).copied().unwrap_or(0) as f64 / self.decisions as f64
    }

    fn blocked(&self) -> u64 {
        self.blocked_holds.values().sum()
    }
}

/// The tiles a map's [`survey`] found reachable, and which presses leave it from each of them.
type Survey = (BTreeSet<(u32, u32)>, BTreeMap<(u32, u32), Vec<&'static str>>);

/// The starting map's reachable tiles, and which presses leave it -- measured, not assumed.
///
/// A breadth-first walk of the map with real button presses on throwaway emulators: from each
/// tile, hold each direction until the coordinates change or 48 frames pass, and record either the
/// tile reached or the map escaped to. Nothing neural is involved and the measured run's own
/// emulator is untouched, so this is instrumentation rather than a hint. It is what makes the
/// report's coverage column mean something ("32 of 48 tiles", not "71 locations") and what lets it
/// count the decisions that *could* have ended the run.
fn survey(rom: &[u8], state: &[u8]) -> Survey {
    let read = |probe: &mut Emulator| {
        (
            u32::from(probe.read8(ram::wCurMap)),
            u32::from(probe.read8(ram::wXCoord)),
            u32::from(probe.read8(ram::wYCoord)),
        )
    };
    let step = |probe: &mut Emulator, mask: u8| {
        let before = read(probe);
        let mut moved = false;
        probe.set_buttons(mask);
        for _ in 0..48 {
            probe.run_frame().expect("a frame should complete");
            if read(probe) != before {
                moved = true;
                break;
            }
        }
        probe.set_buttons(buttons::NONE);
        for _ in 0..10 {
            probe.run_frame().expect("a frame should complete");
        }
        (read(probe), moved)
    };
    let restore = |state: &[u8]| {
        let mut probe = emulator(rom);
        probe.import_state(state).expect("a surveyed state should import");
        probe
    };

    let mut first = restore(state);
    let (start_map, x, y) = read(&mut first);
    let mut reachable = BTreeSet::new();
    let mut exits: BTreeMap<(u32, u32), Vec<&'static str>> = BTreeMap::new();
    let mut states: BTreeMap<(u32, u32), Vec<u8>> = BTreeMap::new();
    reachable.insert((x, y));
    states.insert((x, y), state.to_vec());
    let mut queue = vec![(x, y)];
    while let Some(tile) = queue.pop() {
        for (name, mask) in [
            ("up", buttons::UP),
            ("down", buttons::DOWN),
            ("left", buttons::LEFT),
            ("right", buttons::RIGHT),
        ] {
            let mut probe = restore(&states[&tile]);
            let ((map, x, y), moved) = step(&mut probe, mask);
            if !moved {
                continue;
            }
            if map != start_map {
                exits.entry(tile).or_default().push(name);
                continue;
            }
            if reachable.insert((x, y)) {
                states.insert((x, y), probe.export_state().expect("state export"));
                queue.push((x, y));
            }
        }
    }
    (reachable, exits)
}

/// The room-escape instrumentation inside the stream's frame: the decoder as it stood before
/// the decode and after it, read at the one point between the two.
struct Escape<'a> {
    /// The readout before this frame's decode.
    before: Option<flybrain_core::decoder::DecoderState>,
    hold_start: (Option<(u32, u32, u32)>, f64),
    run_winner: Option<String>,
    run_length: f64,
    start_map: u32,
    exits: &'a BTreeMap<(u32, u32), Vec<&'static str>>,
    hold_ms: f64,
    trace: Trace,
}

impl FrameObserver for Escape<'_> {
    fn after(&mut self, phase: FramePhase, agent: &mut NeuralAgent) {
        if phase == FramePhase::Ticked {
            self.before = Some(agent.decoder.export_state());
        }
    }

    fn before_execute(&mut self, frame: &LegacyFrame, parts: &mut Parts<'_>, _active: &[String]) {
        let Some(before) = self.before.take() else { return };
        let agent = &*parts.agent;
        let after = agent.decoder.export_state();
        if after.next_decision == before.next_decision {
            return;
        }
        let ms = agent.network.ms;
        let location = frame.location;
        let trace = &mut self.trace;
        trace.decisions += 1;
        let winner = after.current.clone().expect("a decision names a winner");
        // The raw argmax, recomputed from the decoder's own inputs: the rates the decode saw, the
        // calibrated baseline, and the fatigue as it stood *before* the decision. Comparing it
        // with the winner is what "the incumbent won by hysteresis" means.
        let adjusted = |channel: &str| {
            let index = DIRECTIONS.iter().position(|name| *name == channel).expect("a direction");
            let role = ROLES[index];
            let rate = agent.network.rates.get_or_zero(role);
            let base = after.baseline.get_or_zero(role);
            (rate + 1.0) / (base + 1.0) / (1.0 + before.fatigue.get_or_zero(channel))
        };
        let mut argmax = DIRECTIONS[0];
        for channel in DIRECTIONS.iter().skip(1) {
            if adjusted(channel) > adjusted(argmax) {
                argmax = channel;
            }
        }
        if argmax != winner {
            trace.hysteresis_holds += 1;
        }
        if let Some(name) = DIRECTIONS.iter().find(|name| **name == winner) {
            *trace.wins.entry(name).or_insert(0) += 1;
        }
        if self.run_winner.as_deref() == Some(winner.as_str()) {
            self.run_length += 1.0;
        } else {
            if self.run_length > 0.0 {
                trace.runs.push(self.run_length);
            }
            self.run_winner = Some(winner.clone());
            self.run_length = 1.0;
        }

        // Was the hold that just ended a wall bump? The location now against the location at the
        // previous decision, for the direction that was held in between.
        if let (Some(previous), Some(held)) = (self.hold_start.0, before.current.as_deref())
            && ms - self.hold_start.1 >= self.hold_ms
            && location == Some(previous)
            && let Some(name) = DIRECTIONS.iter().find(|name| **name == held)
        {
            *trace.blocked_holds.entry(name).or_insert(0) += 1;
        }
        self.hold_start = (location, ms);

        // The decision this whole exercise is about: standing on a tile one press from leaving,
        // did the readout choose that press? Only on the starting map: the bedroom has walkable
        // tiles at the same coordinates and they are not these exits.
        if let Some(leaving) = location
            .filter(|(map, _, _)| *map == self.start_map)
            .and_then(|(_, x, y)| self.exits.get(&(x, y)))
        {
            trace.exit_decisions += 1;
            if leaving.iter().any(|direction| *direction == winner) {
                trace.exit_decisions_taken += 1;
            }
        }
    }
}

/// One instrumented brain run: the real network, the real readout, the real adapter, and -- from a
/// live checkpoint -- the real reward ledger and the real ratchet.
///
/// ## How the runs are made independent
///
/// The network is deterministic and so is binjgb, so N runs from one state are one run reported N
/// times. [`Start::Fresh`] varies the warm-up length, as v0.1.0's confirmation table did: it is
/// how long the fly settles before its resting rates are calibrated, and on the stream that is
/// wherever the process happened to start. A *restore* has no warm-up at all -- `simloop.rs` skips
/// it deliberately, because a checkpoint already carries a settled network and a calibrated
/// readout -- so [`Start::Live`] instead displaces `LifState::rng`, the xorshift state behind the
/// per-tick noise kicks. That is the only thing it changes: the membranes, the learned gains, the
/// eligibility traces, the brain clock, the decoder's fatigue, holds and baseline, the emulator and
/// the reward ledger are all the live fly's to the byte. Five noise streams from one state is the
/// same kind of variation five warm-ups are, and on a restored run it is the only arbitrary
/// quantity left.
#[allow(clippy::too_many_arguments)]
fn brain_trace(
    rom: &[u8],
    data: &Arc<flybrain_core::dataset::BrainDataset>,
    start: Start<'_>,
    config: DecoderConfig,
    minutes: f64,
    threads: usize,
    house: &[u32],
    exits: &BTreeMap<(u32, u32), Vec<&'static str>>,
    stop_on_exit: bool,
) -> Trace {
    let mut emulator = emulator(rom);
    let mut adapter = PokemonRedReward::new();
    let mut agent_config = AgentConfig::with_decoder(config.clone());
    let hold_ms =
        config.exclusive.as_ref().expect("the Game Boy preset has an exclusive group").hold_ms;
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut trace = Trace::default();
    if let Start::Fresh { warmup_ms, .. } = &start {
        agent_config.warmup_ms = *warmup_ms;
    }

    let mut agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("a valid agent");
    if threads > 1 {
        agent.set_sweep_plan(SweepPlan::with_threads(threads).expect("a sweep plan"));
    }
    // The stream's own frame (`flysim::frame::LegacyFrame`), in raw mode: no macro layer.
    let mut frame = LegacyFrame::new();
    let start_map = match &start {
        Start::Fresh { state, map, .. } => {
            emulator.import_state(state).expect("the starting state should import");
            frame.frame_buffer.copy_from_slice(emulator.framebuffer());
            agent.warmup(Some(&frame.frame_buffer)).expect("warm-up");
            *map
        }
        Start::Live { checkpoint, rng } => {
            let mut checkpoint = (*checkpoint).clone();
            checkpoint.agent.network.rng = *rng;
            frame
                .restore(
                    &mut Parts {
                        agent: &mut agent,
                        emulator: &mut emulator,
                        adapter: &mut adapter,
                        ratchet: &mut ratchet,
                        macros: None,
                    },
                    &checkpoint,
                )
                .expect("the checkpoint should restore");
            u32::from(emulator.read8(ram::wCurMap))
        }
    };

    trace.maps.push(start_map);
    let began_ms = agent.network.ms;
    let until = began_ms + minutes * 60_000.0;
    let mut escape = Escape {
        before: None,
        hold_start: (frame.location, began_ms),
        run_winner: None,
        run_length: 0.0,
        start_map,
        exits,
        hold_ms,
        trace,
    };
    // Reported on its own: the longest stretch with no movement at all, whatever was held.
    let mut still_since_ms = began_ms;
    let tiles_at_start = adapter.progress().unique_locations;
    escape.trace.tiles = tiles_at_start;

    while agent.network.ms < until {
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: None,
        };
        let location_before = frame.location;
        let transition = frame.transition(&mut parts, &mut escape).expect("a frame");
        let ms = transition.ms;
        let trace = &mut escape.trace;
        trace.reward += transition.evaluated.rewards.iter().map(|event| event.value).sum::<f64>();
        trace.tiles = transition.evaluated.progress.unique_locations;

        let location = frame.location;
        if location != location_before {
            still_since_ms = ms;
        }
        trace.longest_still_ms = trace.longest_still_ms.max(ms - still_since_ms);
        if let Some(tile) = location
            .filter(|(map, _, _)| *map == start_map)
            .map(|(_, x, y)| (x, y))
            .filter(|tile| exits.contains_key(tile))
        {
            *trace.exit_tile_frames.entry(tile).or_insert(0) += 1;
        }

        // The ratchet, with the adapter's own policy and the checkpoint's own budget.
        let progress = transition.evaluated.progress;
        let boundary = frame.boundary(&mut parts, &progress, ms).expect("the boundary");
        if boundary.rollback.is_some() {
            escape.trace.recoveries += 1;
            still_since_ms = ms;
            escape.hold_start = (frame.location, ms);
        }
        let trace = &mut escape.trace;

        let Some(map) = adapter.map_id() else { continue };
        if trace.maps.last() != Some(&map) {
            trace.maps.push(map);
        }
        if map != start_map && trace.left_ms.is_none() {
            trace.left_ms = Some(ms - began_ms);
        }
        if !house.contains(&map) && trace.out_ms.is_none() {
            trace.out_ms = Some(ms - began_ms);
            if stop_on_exit {
                break;
            }
        }
    }
    let Escape { mut trace, run_length, .. } = escape;
    if run_length > 0.0 {
        trace.runs.push(run_length);
    }
    trace.new_tiles = trace.tiles.saturating_sub(tiles_at_start);
    trace.budget_spent = ratchet.state.attempts >= ratchet.policy().max_attempts;
    trace
}

/// One readout candidate, named for the report.
#[derive(Clone, Copy)]
struct Candidate {
    name: &'static str,
    hold: f64,
    fatigue: f64,
    /// Fatigue forced onto a blocked direction; 0 is the rule switched off.
    blocked_fatigue: f64,
    /// The lead a challenger needs over the incumbent. 1.0 is no commitment bonus at all.
    hysteresis: f64,
}

impl Candidate {
    fn config(&self) -> DecoderConfig {
        tuned(self.hold, self.hold, self.fatigue, self.blocked_fatigue, self.hysteresis)
    }
}

/// The candidates `docs/design/room-escape.md` section 3 lists, in its order of preference.
///
/// The last three are the hysteresis axis, which the first four do not touch: with the live fly's
/// four direction scores inside a 8.6% spread, a 15% lead is unreachable and the incumbent can only
/// ever be displaced by fatigue (`infra/docs/room-escape.md`). 1.05 is a commitment bonus the score
/// spread can actually overcome; 1.00 is no bonus at all, so the hold alone carries the commitment.
const CANDIDATES: [Candidate; 7] = [
    Candidate { name: "v0.1.0", hold: 800.0, fatigue: 0.04, blocked_fatigue: 0.0, hysteresis: 1.15 },
    Candidate {
        name: "fatigue-0.08",
        hold: 800.0,
        fatigue: 0.08,
        blocked_fatigue: 0.0,
        hysteresis: 1.15,
    },
    Candidate {
        name: "blocked-0.35",
        hold: 800.0,
        fatigue: 0.04,
        blocked_fatigue: 0.35,
        hysteresis: 1.15,
    },
    Candidate { name: "both", hold: 800.0, fatigue: 0.08, blocked_fatigue: 0.35, hysteresis: 1.15 },
    Candidate {
        name: "hysteresis-1.05",
        hold: 800.0,
        fatigue: 0.08,
        blocked_fatigue: 0.0,
        hysteresis: 1.05,
    },
    Candidate {
        name: "hysteresis-1.00",
        hold: 800.0,
        fatigue: 0.08,
        blocked_fatigue: 0.0,
        hysteresis: 1.00,
    },
    Candidate {
        name: "hold-600",
        hold: 600.0,
        fatigue: 0.08,
        blocked_fatigue: 0.0,
        hysteresis: 1.05,
    },
];

/// `FLY_ESCAPE_CANDIDATES` is a comma-separated list of [`CANDIDATES`] names, or of
/// `hold/fatigue/blocked/hysteresis` tuples for a candidate the list does not already hold. The
/// tuple form is what a follow-up sweep uses without a recompile, and its name in the report is the
/// tuple itself. The hysteresis is optional and defaults to the preset's 1.15.
fn candidates_from_env(default: &[Candidate]) -> Vec<Candidate> {
    let Ok(text) = std::env::var("FLY_ESCAPE_CANDIDATES") else {
        return default.to_vec();
    };
    text.split(',')
        .map(str::trim)
        .map(|name| {
            if let Some(candidate) = CANDIDATES.iter().find(|candidate| candidate.name == name) {
                return *candidate;
            }
            let parts: Vec<f64> =
                name.split('/').filter_map(|piece| piece.trim().parse().ok()).collect();
            assert!(
                parts.len() == 3 || parts.len() == 4,
                "unknown candidate {name}: name one of CANDIDATES, or a \
                 hold/fatigue/blocked[/hysteresis] tuple"
            );
            Candidate {
                name: String::leak(name.to_string()),
                hold: parts[0],
                fatigue: parts[1],
                blocked_fatigue: parts[2],
                hysteresis: parts.get(3).copied().unwrap_or(1.15),
            }
        })
        .collect()
}
fn print_trace_table(name: &str, traces: &[Trace], minutes: f64, target_ms: f64) {
    println!("\n**{name}**\n");
    println!(
        "| run | left the map | out of the house | new tiles | decisions | up | down | left | \
         right | mean run | hysteresis | blocked holds | longest still | exit decisions | \
         recoveries | reward |"
    );
    println!(
        "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | \
         ---: | ---: | ---: | ---: |"
    );
    let percent =
        |part: u64, whole: u64| if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 };
    for (index, trace) in traces.iter().enumerate() {
        let time = |value: Option<f64>| {
            value.map(|ms| format!("{:.1} s", ms / 1000.0)).unwrap_or_else(|| "no".to_string())
        };
        println!(
            "| {} | {} | {} | {} | {} | {:.0}% | {:.0}% | {:.0}% | {:.0}% | {} | {:.0}% | {} \
             ({:.0}%) | {:.1} s | {}/{} | {}{} | {:.2} |",
            index + 1,
            time(trace.left_ms),
            time(trace.out_ms),
            trace.new_tiles,
            trace.decisions,
            trace.win_share("up") * 100.0,
            trace.win_share("down") * 100.0,
            trace.win_share("left") * 100.0,
            trace.win_share("right") * 100.0,
            trace.mean_run().map(|value| format!("{value:.2}")).unwrap_or_else(|| "—".to_string()),
            percent(trace.hysteresis_holds, trace.decisions),
            trace.blocked(),
            percent(trace.blocked(), trace.decisions),
            trace.longest_still_ms / 1000.0,
            trace.exit_decisions_taken,
            trace.exit_decisions,
            trace.recoveries,
            if trace.budget_spent { "*" } else { "" },
            trace.reward,
        );
    }
    let out: Vec<f64> = traces.iter().filter_map(|trace| trace.out_ms).collect();
    let within = out.iter().filter(|ms| **ms <= target_ms).count();
    let mut sorted = out.clone();
    println!(
        "\n{}/{} runs left the house inside {minutes} brain minutes, {within}/{} inside {:.0}; \
         median {}.",
        out.len(),
        traces.len(),
        traces.len(),
        target_ms / 60_000.0,
        median(&mut sorted)
            .map(|ms| format!("{:.1} s", ms / 1000.0))
            .unwrap_or_else(|| "—".to_string())
    );
    let tiles: BTreeSet<&(u32, u32)> =
        traces.iter().flat_map(|trace| trace.exit_tile_frames.keys()).collect();
    if tiles.is_empty() {
        println!("No run ever stood on a tile one press from the exit.");
    } else {
        let frames: Vec<String> = tiles
            .iter()
            .map(|tile| {
                let total: u64 =
                    traces.iter().filter_map(|trace| trace.exit_tile_frames.get(tile)).sum();
                format!("({}, {}): {total}", tile.0, tile.1)
            })
            .collect();
        println!("Frames stood on an exit tile, summed over the runs: {}.", frames.join(", "));
    }
    let maps: BTreeSet<u32> =
        traces.iter().flat_map(|trace| trace.maps.iter().skip(1)).copied().collect();
    let maps: Vec<String> = maps.iter().map(u32::to_string).collect();
    println!("Maps reached beyond the starting one: {}.", maps.join(", "));
}

/// The live-fly diagnosis: `FLY_ESCAPE_CHECKPOINT` plus `FLY_ESCAPE_BRAIN`.
fn live(rom: &[u8], checkpoint_path: &std::ffi::OsStr, minutes: f64, target_ms: f64) {
    let dataset = PathBuf::from(
        std::env::var_os("FLY_ESCAPE_BRAIN")
            .expect("FLY_ESCAPE_CHECKPOINT needs FLY_ESCAPE_BRAIN: a checkpoint is a brain"),
    );
    let checkpoint = flysim::store::load(std::path::Path::new(checkpoint_path))
        .expect("FLY_ESCAPE_CHECKPOINT should be a FLYSIM01 envelope");
    let runs = env_usize("FLY_ESCAPE_BRAIN_RUNS", 5);
    let brain_threads = env_usize("FLY_ESCAPE_BRAIN_THREADS", 4);
    let parallel = env_usize("FLY_ESCAPE_BRAIN_PARALLEL", 1).max(1);
    let stop_on_exit = std::env::var_os("FLY_ESCAPE_RUN_ON").is_none();
    let candidates = candidates_from_env(&CANDIDATES);

    let data = Arc::new(
        load_brain_dataset_from_dir(&dataset).expect("FLY_ESCAPE_BRAIN should be a brain dataset"),
    );
    let mut probe = emulator(rom);
    probe.import_state(&checkpoint.runtime.emulator).expect("the emulator state should import");
    let start_map = u32::from(probe.read8(ram::wCurMap));
    let (reachable, exits) = survey(rom, &checkpoint.runtime.emulator);
    let house: Vec<u32> = if start_map == REDS_HOUSE_1F || start_map == REDS_HOUSE_2F {
        vec![REDS_HOUSE_1F, REDS_HOUSE_2F]
    } else {
        vec![start_map]
    };

    println!("## The live fly, from {}\n", std::path::Path::new(checkpoint_path).display());
    println!(
        "Generation {}, brain clock {:.1} min, emulator frame {}, ratchet best rank {} \
         ({} recoveries, {} attempts, coverage {}).",
        checkpoint.runtime.generation,
        checkpoint.agent.network.ms / 60_000.0,
        checkpoint.runtime.emulator_frame,
        checkpoint.runtime.ratchet.best,
        checkpoint.runtime.ratchet.recoveries,
        checkpoint.runtime.ratchet.attempts,
        checkpoint.runtime.ratchet.coverage,
    );
    println!(
        "Standing on map {start_map} at ({}, {}); the decoder is holding {:?}.",
        probe.read8(ram::wXCoord),
        probe.read8(ram::wYCoord),
        checkpoint.agent.decoder.current,
    );
    println!("\nScores as the checkpoint stands, `(rate + 1) / (baseline + 1)`:\n");
    println!("| channel | rate | baseline | score | fatigue | adjusted |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: |");
    for (index, channel) in DIRECTIONS.iter().enumerate() {
        let role = ROLES[index];
        let rate = checkpoint.agent.network.rates.get_or_zero(role);
        let base = checkpoint.agent.decoder.baseline.get_or_zero(role);
        let fatigue = checkpoint.agent.decoder.fatigue.get_or_zero(channel);
        let score = (rate + 1.0) / (base + 1.0);
        println!(
            "| {channel} | {rate:.3} | {base:.3} | {score:.4} | {fatigue:.4} | {:.4} |",
            score / (1.0 + fatigue)
        );
    }
    println!(
        "\n{} reachable tiles on map {start_map}, {} of them one press from leaving:",
        reachable.len(),
        exits.len()
    );
    for (tile, directions) in &exits {
        println!("- ({}, {}) pressing {}", tile.0, tile.1, directions.join(" or "));
    }
    let visited: BTreeSet<(u32, u32)> = checkpoint.runtime.reward["tiles"]
        .as_array()
        .map(|tiles| {
            tiles
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(|key| {
                    let mut parts = key.split(':');
                    let map: u32 = parts.next()?.parse().ok()?;
                    if map != start_map {
                        return None;
                    }
                    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let never: Vec<(u32, u32)> = reachable.difference(&visited).copied().collect();
    println!(
        "\nThe live tile ledger holds {} of those {}. Never stood on: {never:?}",
        visited.len(),
        reachable.len()
    );

    for candidate in &candidates {
        let config = candidate.config();
        let started = std::time::Instant::now();
        let next = AtomicUsize::new(0);
        let mut collected: Vec<(usize, Trace)> = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..parallel {
                let config = config.clone();
                let (data, checkpoint, exits, house, next) =
                    (&data, &checkpoint, &exits, &house, &next);
                handles.push(scope.spawn(move || {
                    let mut mine = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= runs {
                            break;
                        }
                        // The noise stream, displaced per run: see [`brain_trace`].
                        let rng =
                            checkpoint.agent.network.rng.wrapping_add(SEEDS[index % SEEDS.len()]);
                        let trace = brain_trace(
                            rom,
                            data,
                            Start::Live { checkpoint, rng: if rng == 0 { 1 } else { rng } },
                            config.clone(),
                            minutes,
                            brain_threads,
                            house,
                            exits,
                            stop_on_exit,
                        );
                        eprintln!("{} live run {} done", candidate.name, index + 1);
                        mine.push((index, trace));
                    }
                    mine
                }));
            }
            for handle in handles {
                collected.extend(handle.join().expect("a worker thread"));
            }
        });
        collected.sort_by_key(|(index, _)| *index);
        let traces: Vec<Trace> = collected.into_iter().map(|(_, trace)| trace).collect();
        print_trace_table(candidate.name, &traces, minutes, target_ms);
        eprintln!("{} took {:.1} s wall", candidate.name, started.elapsed().as_secs_f64());
    }
}

fn main() {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        println!(
            "FLY_ROM is not set, so there is no room to escape.\n\
             \n  FLY_ROM=\"$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb\" \\\n    \
             cargo run --release -p flysim --example room_escape\n\n\
             See docs/design/room-escape.md §1 and the module comment."
        );
        return;
    };
    let rom = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("FLY_ROM is {path:?} but could not be read: {error}"));
    {
        let check = emulator(&rom);
        assert_eq!(
            check.rom_sha256(),
            SUPPORTED_ROM,
            "FLY_ROM is not the cartridge the adapter is pinned to"
        );
    }

    let minutes = env_f64("FLY_ESCAPE_MINUTES", 10.0);
    let seeds = env_usize("FLY_ESCAPE_SEEDS", SEEDS.len()).clamp(1, SEEDS.len());
    let holds = env_list("FLY_ESCAPE_HOLDS", &HOLDS);
    let fatigues = env_list("FLY_ESCAPE_FATIGUES", &FATIGUES);
    let hystereses = env_list("FLY_ESCAPE_HYSTERESES", &[1.15]);
    let threads = env_usize("FLY_ESCAPE_JOBS", 8);
    let target_ms = env_f64("FLY_ESCAPE_TARGET_MINUTES", 5.0) * 60_000.0;
    // Brain milliseconds to wander the ground floor before its starting state is captured, so the
    // capture is not standing on the staircase it just arrived by. See [`Capture`].
    let capture_delay = env_f64("FLY_ESCAPE_CAPTURE_DELAY", 15_000.0);
    let state_1f = PathBuf::from(
        std::env::var_os("FLY_ESCAPE_STATE_1F").unwrap_or_else(|| "room-escape-1f.state".into()),
    );

    // v0.1.1: the live fly, forward from the checkpoint the release box wrote.
    if let Some(checkpoint) = std::env::var_os("FLY_ESCAPE_CHECKPOINT") {
        live(&rom, &checkpoint, minutes, target_ms);
        return;
    }

    let skip_sweep = std::env::var_os("FLY_ESCAPE_SKIP_SWEEP").is_some();
    if !skip_sweep {
        println!(
            "room escape: {minutes} brain minutes x {seeds} seeds x {} holds x {} fatigue gains \
             x {} hystereses x 2 rooms, on {} threads, no brain",
            holds.len(),
            fatigues.len(),
            hystereses.len(),
            threads
        );
    }
    println!("channels: {}", GAMEBOY_BUTTONS.join(", "));

    let started = std::time::Instant::now();
    let bedroom = match std::env::var_os("FLY_ESCAPE_STATE") {
        Some(path) => {
            println!("bedroom: restored from {path:?}");
            std::fs::read(&path).expect("FLY_ESCAPE_STATE should be readable")
        }
        None => {
            let (state, ms) = boot_to_bedroom(&rom);
            println!(
                "bedroom: booted the cartridge in {:.1} s wall, {:.1} s of game time",
                started.elapsed().as_secs_f64(),
                ms / 1000.0
            );
            state
        }
    };

    // The ground floor, walked to once and cached: a starting state, not a measurement.
    let ground = if state_1f.is_file() {
        println!("house 1F: reusing {}", state_1f.display());
        std::fs::read(&state_1f).expect("the cached 1F state should be readable")
    } else {
        let descent = std::time::Instant::now();
        let mut found = None;
        for seed in SEEDS {
            let run = walk(
                &rom,
                &bedroom,
                REDS_HOUSE_2F,
                seed,
                tuned(800.0, 800.0, 0.04, 0.0, 1.15),
                minutes,
                Some(Capture { map: REDS_HOUSE_1F, after_ms: capture_delay }),
                true,
            );
            if let Some(state) = run.captured {
                println!(
                    "house 1F: walked down the stairs on seed {seed} in {:.1} s wall \
                     (left the bedroom at {:.1} s of brain time)",
                    descent.elapsed().as_secs_f64(),
                    run.left_ms.unwrap_or(f64::NAN) / 1000.0
                );
                found = Some(state);
                break;
            }
        }
        let state = found.expect("no seed reached the ground floor; raise FLY_ESCAPE_MINUTES");
        std::fs::write(&state_1f, &state).expect("writing the 1F state");
        println!("house 1F: saved to {}", state_1f.display());
        state
    };

    let mut states: BTreeMap<&'static str, Vec<u8>> = BTreeMap::new();
    states.insert("bedroom", bedroom);
    states.insert("house 1F", ground);

    if !skip_sweep {
        let mut jobs = Vec::new();
        for room in ["bedroom", "house 1F"] {
            let start_map = if room == "bedroom" { REDS_HOUSE_2F } else { REDS_HOUSE_1F };
            for hold in &holds {
                for fatigue in &fatigues {
                    for seed in SEEDS.iter().take(seeds) {
                        for hysteresis in &hystereses {
                            jobs.push(Job {
                                room,
                                start_map,
                                hold: *hold,
                                fatigue: *fatigue,
                                hysteresis: *hysteresis,
                                seed: *seed,
                            });
                        }
                    }
                }
            }
        }
        println!("\nsweeping {} runs...", jobs.len());
        let sweeping = std::time::Instant::now();
        let cells = sweep(&rom, &states, &jobs, minutes, threads);
        println!("swept {} runs in {:.1} s wall", jobs.len(), sweeping.elapsed().as_secs_f64());

        print_table("From Red's bedroom (map 38)", &cells, "bedroom");
        print_table("From the ground floor (map 37)", &cells, "house 1F");
        println!(
            "\nThis measures the readout, not the fly. A random walker is not a brain and a run \
             that leaves a room faster is not learning."
        );
    }

    // The confirmation run, opt-in because it needs the dataset and a quarter of an hour.
    let Some(dataset) = std::env::var_os("FLY_ESCAPE_BRAIN") else {
        println!(
            "\nFLY_ESCAPE_BRAIN is not set, so the chosen value was not confirmed against the \
             real brain.\n  FLY_ESCAPE_BRAIN=data/fafb-v783 FLY_ESCAPE_CANDIDATES=v0.1.0 \
             ... --example room_escape"
        );
        return;
    };
    let dataset = PathBuf::from(dataset);
    let runs = env_usize("FLY_ESCAPE_BRAIN_RUNS", 5);
    let brain_threads = env_usize("FLY_ESCAPE_BRAIN_THREADS", 4);
    // `FLY_ESCAPE_BRAIN_ROOM` is `bedroom` (the default, and v0.1.0's confirmation room) or `1F`.
    let room = std::env::var("FLY_ESCAPE_BRAIN_ROOM").unwrap_or_else(|_| "bedroom".to_string());
    let bedroom = room == "bedroom";
    let start_map = if bedroom { REDS_HOUSE_2F } else { REDS_HOUSE_1F };
    let state = &states[if bedroom { "bedroom" } else { "house 1F" }];
    let (reachable, exits) = survey(&rom, state);
    let house = [REDS_HOUSE_1F, REDS_HOUSE_2F];
    let data = Arc::new(
        load_brain_dataset_from_dir(&dataset).expect("FLY_ESCAPE_BRAIN should be a brain dataset"),
    );
    println!(
        "\n## Confirmation: the real brain from a fresh boot in the {room} ({} reachable tiles, \
         {} of them one press from leaving)\n",
        reachable.len(),
        exits.len()
    );
    for candidate in candidates_from_env(&CANDIDATES[..1]) {
        let config = candidate.config();
        let mut traces = Vec::new();
        for index in 0..runs {
            let brain = std::time::Instant::now();
            // 2,500 ms is `DEFAULT_WARMUP_MS`; the step is deliberately not a round number so the
            // runs do not land on a harmonic of anything in the kernel.
            let warmup_ms = 2_500 + index as u64 * 313;
            traces.push(brain_trace(
                &rom,
                &data,
                Start::Fresh { state, map: start_map, warmup_ms },
                config.clone(),
                minutes,
                brain_threads,
                &house,
                &exits,
                std::env::var_os("FLY_ESCAPE_RUN_ON").is_none(),
            ));
            eprintln!(
                "{} fresh run {} took {:.1} s wall",
                candidate.name,
                index + 1,
                brain.elapsed().as_secs_f64()
            );
        }
        print_trace_table(candidate.name, &traces, minutes, target_ms);
    }
    println!(
        "\nThe brain is a handful of runs, on one cartridge, from one room. It confirms the \
         readout is not broken; it is not a benchmark."
    );
}
