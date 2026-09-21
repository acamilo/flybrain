//! The 30-minute random-walker baseline for the platformer reward scale.
//!
//! `docs/design/platformer.md` §7, "30-minute random-walker baseline": replace the network's role
//! rates with a seeded uniform random source, keep the *real* platformer decoder and the *real*
//! adapter, and run 30 brain minutes across five seeds headless. No connectome is loaded, because
//! the point is to measure the reward scale without the brain in the loop.
//!
//! ```sh
//! FLY_ROM_PLATFORMER="$HOME/roms/Super Mario Land (World) (Rev A).gb" \
//!   cargo run --release -p flysim --example random_walker
//! ```
//!
//! Without the ROM it prints how to run it and exits 0; no such cartridge exists on the machine
//! this was written on. `FLY_WALKER_MINUTES` and `FLY_WALKER_SEEDS` shorten a run while working on
//! it, and `FLY_ROM_PLATFORMER_SHA256` pins the cartridge (unset, the hash on disk is used and
//! printed).
//!
//! ## Calibration target
//!
//! **0.5 to 2.0 total reward per brain minute, dominated by `band` and `coin`, and 1-1 cleared in at
//! most one of the five seeds.** The design's rules if it misses:
//!
//! - random clears 1-1 in *every* seed -> coarsen bands to a full screen and halve `coin`;
//! - random earns under 0.1 per brain minute -> halve the band width.
//!
//! This calibrates the reward scale only. It is not evidence about the fly, and these numbers must
//! never be presented as if they were: a uniform random source is not a brain, and a run that beats
//! it is not thereby learning. That is the whole reason the baseline exists — a reward scale a random
//! walker can farm tells you nothing later.

use std::collections::BTreeMap;

use flybrain_core::agent::GAMEBOY_MS_PER_FRAME;
use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::{GAMEBOY_BUTTONS, to_button_mask};
use flybrain_core::decoder::platformer::platformer_decoder_config;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::{GameAdapter, MemoryReader};
use flybrain_gb::platformer::{PlatformerAdapter, symbols};
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};

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
const SEEDS: [i32; 5] = [20260915, 4242, 7, 31337, 99991];

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
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

#[derive(Default)]
struct Report {
    seed: i32,
    frames: u64,
    brain_ms: f64,
    levels_reached: u32,
    cleared_1_1: bool,
    furthest: BTreeMap<u32, u32>,
    deaths: u64,
    game_overs: u64,
    coins: u64,
    score_payouts: u64,
    bands: u64,
    total: f64,
    counts: BTreeMap<&'static str, u64>,
    /// Frames each channel was held, in `GAMEBOY_BUTTONS` order.
    held: [u64; 8],
    playable_frames: u64,
}

fn walk(rom: &[u8], seed: i32, minutes: f64) -> Report {
    let mut emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    let sha256 = emulator.rom_sha256();
    let mut adapter = PlatformerAdapter::with_rom_pin(Some(&sha256));
    let mut decoder = PopulationDecoder::new(platformer_decoder_config()).expect("a valid preset");
    decoder.calibrate(&NumberMap::from_pairs(
        ROLES.iter().map(|role| (*role, BASELINE_HZ)),
    ));

    let mut random = xorshift(seed);
    let mut report = Report { seed, ..Report::default() };
    let mut lives: Option<u32> = None;
    let mut was_game_over = false;
    let until = minutes * 60_000.0;
    let mut ms = 0.0;

    while ms < until {
        ms += GAMEBOY_MS_PER_FRAME;
        let mut rates = NumberMap::new();
        for role in ROLES {
            rates.set(role, random() * MAX_HZ);
        }
        let active = decoder.decode(&rates, ms, adapter.boot());
        let mask = to_button_mask(&active);
        for (index, name) in GAMEBOY_BUTTONS.iter().enumerate() {
            if active.iter().any(|channel| channel == name) {
                report.held[index] += 1;
            }
        }
        emulator.set_buttons(mask as u8);
        emulator.run_frame().expect("a frame should complete");
        report.frames += 1;
        adapter.sample(&mut emulator, ms);

        if !adapter.boot() {
            report.playable_frames += 1;
            if let Some(level) = adapter.level() {
                report.levels_reached = report.levels_reached.max(level);
                report.cleared_1_1 |= level >= 1;
                let column = emulator.read8(symbols::hram::hScreenIndex);
                let column = u32::from(column) * symbols::COLUMNS_PER_SCREEN
                    + u32::from(emulator.read8(symbols::hram::hColumnIndex));
                let furthest = report.furthest.entry(level).or_insert(0);
                *furthest = (*furthest).max(column);
            }
            let now = u32::from(emulator.read8(symbols::wram::wLives));
            if lives.is_some_and(|before| now < before) {
                report.deaths += 1;
            }
            lives = Some(now);
        }
        // A game over is one event, not one per frame it is displayed.
        let game_over = adapter.game_over();
        if game_over && !was_game_over {
            report.game_overs += 1;
            lives = None;
        }
        was_game_over = game_over;
    }

    let statistics = adapter.statistics();
    report.brain_ms = ms;
    report.total = statistics.total;
    report.bands = statistics.bands;
    report.coins = statistics.counts.get("coin").copied().unwrap_or(0);
    report.score_payouts = statistics.counts.get("score").copied().unwrap_or(0);
    report.counts = statistics.counts;
    report
}

fn main() {
    let Some(path) = std::env::var_os("FLY_ROM_PLATFORMER") else {
        println!(
            "FLY_ROM_PLATFORMER is not set, so there is nothing to walk.\n\
             \n  FLY_ROM_PLATFORMER=/path/to/sml.gb \\\n    \
             cargo run --release -p flysim --example random_walker\n\n\
             Calibration target: 0.5 to 2.0 total reward per brain minute, dominated by band and\n\
             coin, with 1-1 cleared in at most one of the five seeds. See the module comment."
        );
        return;
    };
    let rom = std::fs::read(&path).unwrap_or_else(|error| {
        panic!("FLY_ROM_PLATFORMER is {path:?} but could not be read: {error}")
    });
    let minutes = env_f64("FLY_WALKER_MINUTES", 30.0);
    let seeds = env_f64("FLY_WALKER_SEEDS", SEEDS.len() as f64).clamp(1.0, SEEDS.len() as f64)
        as usize;

    println!("random walker: {minutes} brain minutes x {seeds} seeds, platformer preset, no brain");
    let mut reports = Vec::new();
    for seed in SEEDS.iter().take(seeds) {
        let started = std::time::Instant::now();
        let report = walk(&rom, *seed, minutes);
        let per_minute = report.total / (report.brain_ms / 60_000.0);
        println!(
            "\nseed {}: {} frames in {:.1}s wall, {:.1} brain minutes",
            report.seed,
            report.frames,
            started.elapsed().as_secs_f64(),
            report.brain_ms / 60_000.0
        );
        println!(
            "  reward {:.3} total, {per_minute:.3} per brain minute; counts {:?}",
            report.total, report.counts
        );
        println!(
            "  furthest level {} (1-1 cleared: {}), bands {}, coins {}, score payouts {}",
            report.levels_reached, report.cleared_1_1, report.bands, report.coins,
            report.score_payouts
        );
        println!("  deaths {}, game overs {}", report.deaths, report.game_overs);
        for (level, column) in &report.furthest {
            let percent = symbols::level_columns(*level)
                .map(|columns| {
                    f64::from(column.saturating_sub(symbols::FIRST_COLUMN)) / f64::from(columns)
                        * 100.0
                })
                .unwrap_or(0.0);
            println!("  furthest in level {level}: column {column} ({percent:.0}%)");
        }
        let frames = report.frames.max(1) as f64;
        let channels: Vec<String> = GAMEBOY_BUTTONS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                format!("{name} {:.2}", report.held[index] as f64 / frames)
            })
            .collect();
        println!("  active-frame fraction: {}", channels.join(", "));
        println!(
            "  playable fraction: {:.2}",
            report.playable_frames as f64 / frames
        );
        reports.push((per_minute, report.cleared_1_1));
    }

    let mean: f64 = reports.iter().map(|(rate, _)| *rate).sum::<f64>() / reports.len() as f64;
    let cleared = reports.iter().filter(|(_, cleared)| *cleared).count();
    println!("\nmean reward per brain minute {mean:.3}; 1-1 cleared in {cleared}/{} seeds", reports.len());
    println!("target: 0.5 to 2.0 per minute, 1-1 cleared in at most 1 seed.");
    if mean < 0.1 {
        println!("below 0.1/min: halve the band width (docs/design/platformer.md §7).");
    } else if cleared == reports.len() {
        println!("1-1 cleared in every seed: coarsen bands to a full screen and halve coin.");
    } else if !(0.5..=2.0).contains(&mean) {
        println!("outside the target band: adjust the band width or the coin value, not the brain.");
    }
    println!("This calibrates the reward scale only. It is not evidence about the fly.");
}
