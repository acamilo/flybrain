//! Raw against palette against plan: the measurement `docs/design/macros.md` section 7 asks for.
//!
//! > From the same archived checkpoint, 6 brain hours each, unthrottled, on the dev box: raw mode
//! > vs macros mode, reporting rungs reached, time to each rung, macro outcome counts, and reward
//! > per brain hour. Ship macros mode as default only if it reaches rung 6 or higher in fewer
//! > brain hours in 3 of 3 runs. The decoder preset is not touched in either arm.
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   FLY_MACRO_BRAIN=data/fafb-v783 FLY_MACRO_HOURS=6 \
//!   cargo run --release -p flysim --example palette_bench
//! ```
//!
//! Without the ROM it prints how to run it and ledger 0. The cartridge is never copied into this
//! repository; it is read from the path in `FLY_ROM` and nothing else.
//!
//! ## The three arms
//!
//! Every arm is the sim loop's own frame order and the sim loop's own parts: the real connectome
//! through `NeuralAgent`, the real Game Boy readout preset with nothing overridden, the real
//! Pokémon adapter paying the real reward catalog, and the real ratchet with the adapter's
//! recovery policy. The *only* difference between them is `flysim::macros::MacroLayer`: the raw
//! arm has none, so `to_button_mask` goes to the emulator exactly as it does on the stream today;
//! the macros arm has one, and the readout it reads is the same decode plus the macro group
//! (section 12). The macros arm's report adds one line for that: the macros it started, by slot.
//!
//! That is deliberately the whole difference. If the two arms differed in the readout as well,
//! nothing the table said could be attributed to the palette.
//!
//! ## Where a run starts
//!
//! - **`FLY_MACRO_CHECKPOINT`** points at a `FLYSIM01` checkpoint — the release box's own durable
//!   state, pulled read-only — and both arms start from it: the same network, the same emulator,
//!   the same reward ledger, the same ratchet. This is what section 7 means by "the same archived
//!   checkpoint", and it is the arm comparison the gate is about.
//! - **without it**, a fresh boot: the harness drives the intro with scripted Start/A/B presses
//!   (the pattern `tests/rom.rs` and `examples/room_escape.rs` use), caches the resulting state,
//!   and both arms start from that one identical playable frame with a freshly warmed-up brain.
//!   The fly does not spend its measured hours on the naming screens, and both arms get the same
//!   cartridge state to the byte. A fresh-boot run is a smoke test of the harness and of palette
//!   mode, **not** the gate: one seed from one state is one run, and section 7 wants three.
//!
//! ## What it reports
//!
//! A markdown table per arm plus one comparison table: the rungs reached with the brain time at
//! each, macro outcome counts, reward per brain hour, recoveries, coverage, and the scene
//! histogram for the macros arm. `FLY_MACRO_DIGEST` adds a trajectory digest, which is the
//! reproducibility check: two runs of one arm on one machine must print the same digest.
//!
//! ## Knobs
//!
//! | env | default | meaning |
//! | --- | --- | --- |
//! | `FLY_ROM` | — | the cartridge; without it this prints instructions and ledger 0 |
//! | `FLY_MACRO_BRAIN` | `FLY_DATASET`, else `data/fafb-v783` | the connectome |
//! | `FLY_MACRO_HOURS` | 6 | brain hours per arm |
//! | `FLY_MACRO_ARMS` | `raw,palette` | which arms to run: `raw`, `palette`, `plan` |
//! | `FLY_MACRO_CHECKPOINT` | — | start both arms from this `FLYSIM01` checkpoint |
//! | `FLY_MACRO_STATE` | `palette-bench-boot.state` | where the booted cartridge state is cached |
//! | `FLY_MACRO_THREADS` | 4 | sweep threads per arm |
//! | `FLY_MACRO_SEED` | 20260916 | seeds the palette and, on a fresh boot, the warm-up |
//! | `FLY_MACRO_SERIAL` | unset | run the arms one after another instead of side by side |
//! | `FLY_MACRO_DIGEST` | unset | print the trajectory digest |
//!
//! None of this is evidence about the fly. It measures what a scene-appropriate action palette
//! does to a run, and nothing more.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flybrain_core::agent::{
    AgentConfig, NeuralAgent, RewardEvent as NeuralReward, TickOptions,
};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::{gameboy_decoder_config_with_macros, to_button_mask};
use flybrain_core::lif::SweepPlan;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::recovery::{NeuralRecovery, recover_game};
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, buttons};
use flysim::config::Config;
use flysim::macros::{MacroLayer, OutcomeCounts, Silence, macro_layer};
use flysim::snapshot::MacroMode;

/// `constants/map_constants.asm`: Red's bedroom, where a cold boot ends up.
const REDS_HOUSE_2F: u32 = 0x26;

/// One brain hour in milliseconds.
const HOUR_MS: f64 = 3_600_000.0;

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn emulator(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge")
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

/// Where both arms start.
enum Start<'a> {
    /// A cartridge state, a fresh brain, and a warm-up.
    Fresh { state: &'a [u8], warmup_ms: u64 },
    /// A `FLYSIM01` checkpoint: the live fly, its emulator, its ledger and its ratchet.
    Live { checkpoint: &'a flysim::store::Checkpoint },
}

/// One measured arm.
#[derive(Debug, Default, Clone)]
struct Arm {
    mode: &'static str,
    /// Brain milliseconds at which each rung was first reached, with its label.
    rungs: Vec<(u32, &'static str, f64)>,
    counts: OutcomeCounts,
    /// Lifetime payout sum over the run.
    reward: f64,
    /// Payout count per adapter kind.
    payouts: BTreeMap<&'static str, u64>,
    brain_ms: f64,
    frames: u64,
    recoveries: u64,
    /// Distinct player positions the adapter saw, which is how much ground the run covered.
    new_tiles: usize,
    /// Frames spent in each scene, macros mode only.
    scenes: BTreeMap<&'static str, u64>,
    /// Frames on which a macro owned the buttons.
    macro_frames: u64,
    /// Macros started per slot of the scene's palette, macros mode only: which of the buttons on
    /// the pad the fly actually pressed, which is the shape of the arm section 12 gates on.
    by_rank: BTreeMap<u8, u64>,
    /// Frames on which nothing at all was pressed.
    idle_frames: u64,
    /// Those frames by cause, macros mode only (`flysim::macros::Silence`).
    ///
    /// The smoke run found 63% of macro frames pressing nothing and could not say which of the
    /// doctrine's several ways of doing nothing they were. This is that split.
    silence: BTreeMap<&'static str, u64>,
    /// FNV-1a over every frame's `(buttons, map, x, y)`: two runs of one arm on one machine
    /// print the same digest, and a change that perturbs the trajectory changes it.
    digest: u64,
    wall_seconds: f64,
}

impl Arm {
    fn rung_reached(&self) -> u32 {
        self.rungs.iter().map(|(rank, _, _)| *rank).max().unwrap_or(0)
    }

    fn hours(&self) -> f64 {
        self.brain_ms / HOUR_MS
    }

    fn reward_per_hour(&self) -> f64 {
        if self.hours() > 0.0 { self.reward / self.hours() } else { 0.0 }
    }

    /// Brain hours at which `rank` was first reached, if it was.
    fn at(&self, rank: u32) -> Option<f64> {
        self.rungs
            .iter()
            .find(|(reached, _, _)| *reached == rank)
            .map(|(_, _, ms)| *ms / HOUR_MS)
    }
}

fn hash(digest: u64, value: u64) -> u64 {
    let mut digest = digest;
    for byte in value.to_le_bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(0x100_0000_01b3);
    }
    digest
}

/// Boot the cartridge to a playable frame in Red's bedroom and return the save state.
///
/// The button pattern is `tests/rom.rs`'s and `room_escape.rs`'s: Start and A on a slow
/// alternation, because a held button is ignored and each press has to be released, then B alone
/// until the adapter reports a stable, unscripted, dialogue-free overworld sample.
fn boot_to_bedroom(rom: &[u8]) -> Vec<u8> {
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
        ms += flysim::config::GAMEBOY_MS_PER_FRAME;
        adapter.sample(&mut emulator, ms);
        if adapter.mode() == "OVERWORLD" && frame > 3_000 {
            break;
        }
    }
    for frame in 0..12_000u32 {
        let mask = if frame % 24 < 8 { buttons::B } else { buttons::NONE };
        emulator.set_buttons(mask);
        emulator.run_frame().expect("a frame should complete");
        ms += flysim::config::GAMEBOY_MS_PER_FRAME;
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
    assert_eq!(adapter.map_id(), Some(REDS_HOUSE_2F), "a cold boot should end in Red's bedroom");
    emulator.export_state().expect("state export")
}

/// One arm: `hours` brain hours of the sim loop's frame order, unthrottled.
///
/// The order is `simloop.rs`'s (steps 2 to 10), as `NeuralAgent::tick` expresses it: the frame and
/// the payouts handed to a tick are the ones the previous tick's buttons produced. The macro layer
/// is consulted at exactly the two points the loop consults it — after the decode, before the
/// buttons reach the emulator, and after the frame and its payouts — so the arm measures the
/// wiring under test rather than a second implementation of it.
fn run_arm(
    rom: &[u8],
    data: &Arc<flybrain_core::dataset::BrainDataset>,
    start: &Start<'_>,
    mode: MacroMode,
    hours: f64,
    threads: usize,
    seed: u32,
) -> Arm {
    let began_wall = std::time::Instant::now();
    let mut emulator = emulator(rom);
    let mut adapter = PokemonRedReward::new();
    // The macro group only in the arm that uses it, exactly as `simloop::decoder_for` does.
    let channels = if mode.dealt() { flybrain_gb::macro_channels("pokemon-red") } else { Vec::new() };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .expect("the preset has a group")
        .hold_ms;
    let blocked_ms = preset.exclusive.as_ref().expect("the preset has an exclusive group").blocked_ms;
    let mut agent_config = AgentConfig::with_decoder(preset);
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut arm = Arm { mode: mode.as_str(), ..Arm::default() };

    match start {
        Start::Fresh { state, warmup_ms } => {
            emulator.import_state(state).expect("the booted state should import");
            agent_config.warmup_ms = *warmup_ms;
        }
        Start::Live { checkpoint } => {
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
        }
    }

    let mut agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("a valid agent");
    if threads > 1 {
        agent.set_sweep_plan(SweepPlan::with_threads(threads).expect("a sweep plan"));
    }
    match start {
        Start::Fresh { .. } => {
            agent.warmup(Some(emulator.framebuffer())).expect("warm-up");
        }
        Start::Live { checkpoint } => {
            agent.import_state(&checkpoint.agent).expect("the checkpoint's agent should import");
            let (width, height) = (agent.frame.width, agent.frame.height);
            agent.network.set_visual_frame(&checkpoint.runtime.framebuffer, width, height);
        }
    }

    // The layer under test, built the way the sim loop builds it: from the configuration, so raw
    // mode gets no layer at all rather than a disabled one.
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = mode;
    let mut macros: Option<MacroLayer> = macro_layer(&config, hold_ms, seed);
    assert_eq!(
        macros.is_some(),
        mode.dealt(),
        "the layer exists in the two dealt modes and nowhere else"
    );

    let began_ms = agent.network.ms;
    let until = began_ms + hours * HOUR_MS;
    let mut frame = emulator.framebuffer().to_vec();
    let mut payouts: Vec<flybrain_gb::RewardEvent> = Vec::new();
    let mut location = adapter.location();
    let mut blocked_since_ms = began_ms;
    let mut held_channel: Option<String> = agent.decoder.current().map(str::to_string);
    let mut rank = adapter.progress().rank;
    let tiles_at_start = adapter.progress().unique_locations;
    arm.rungs.push((rank, adapter.progress().rank_label, 0.0));

    // The scene has not been observed yet, so the first frame is decided on an empty palette,
    // which presses nothing. That is one frame, and it is the honest starting state.
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

        // The blocked-direction cooldown's input, as `simloop.rs` computes it.
        let ms = agent.network.ms;
        let blocked = (blocked_ms > 0.0 && ms - blocked_since_ms >= blocked_ms)
            .then(|| agent.decoder.current().map(str::to_string))
            .flatten();
        // The scene's own macro buttons, from the palette the previous frame's `observe` dealt:
        // the same mask the sim loop passes (`docs/design/macros.md` section 12). `None` in the
        // raw arm, which has no layer and no macro group at all.
        let bound = macros.as_ref().map(MacroLayer::bound_channels);
        let result = agent
            .tick_bound(&frame, &options, blocked.as_deref(), bound.as_deref())
            .expect("a tick");
        let held = agent.decoder.current().map(str::to_string);
        if held != held_channel {
            held_channel = held;
            blocked_since_ms = ms;
        }

        // Step 4, with the layer in the middle of it in macros mode and absent in raw mode.
        let ms = agent.network.ms;
        let mut mask = to_button_mask(&result.active);
        if let Some(layer) = macros.as_mut() {
            let ledger = AdapterLedger(&adapter);
            let decision = layer.decide(&result.active, mask, ms, &mut emulator, &ledger);
            mask = decision.mask;
            if let Some(silence) = decision.silence {
                *arm.silence.entry(silence.label()).or_insert(0) += 1;
            }
            for event in decision.events.iter().filter(|event| event.outcome.is_none()) {
                *arm.by_rank.entry(event.slot).or_insert(0) += 1;
            }
            if layer.running().is_some() {
                arm.macro_frames += 1;
            }
        }
        if mask == 0 {
            arm.idle_frames += 1;
        }
        emulator.set_buttons(mask as u8);
        emulator.run_frame().expect("a frame should complete");
        arm.frames += 1;
        frame.copy_from_slice(emulator.framebuffer());

        // Steps 7 to 9, then the scene.
        payouts = adapter.sample(&mut emulator, ms);
        for event in &payouts {
            arm.reward += event.value;
            *arm.payouts.entry(event.kind).or_insert(0) += 1;
        }
        if let Some(layer) = macros.as_mut() {
            let ledger = AdapterLedger(&adapter);
            let _ = layer.observe(&mut emulator, &ledger, agent.network.ms);
            *arm.scenes.entry(layer.scene_name()).or_insert(0) += 1;
        }

        let now = adapter.location();
        if now.is_some() && now != location {
            location = now;
            blocked_since_ms = ms;
        }
        arm.digest = hash(arm.digest, u64::from(mask));
        if let Some((map, x, y)) = location {
            arm.digest = hash(arm.digest, u64::from(map) << 32 | u64::from(x) << 16 | u64::from(y));
        }

        // Step 10: the ratchet, with the adapter's own policy.
        let progress = adapter.progress();
        if progress.rank != rank {
            rank = progress.rank;
            arm.rungs.push((rank, progress.rank_label, ms - began_ms));
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
            arm.recoveries += 1;
            location = adapter.location();
            held_channel = None;
            blocked_since_ms = ms;
            // The sim loop abandons a running macro on a rollback, and so does this.
            if let Some(layer) = macros.as_mut() {
                layer.cancel(ms);
            }
        }
    }

    arm.brain_ms = agent.network.ms - began_ms;
    arm.new_tiles = adapter.progress().unique_locations.saturating_sub(tiles_at_start);
    arm.counts = macros.as_ref().map(MacroLayer::counts).unwrap_or_default();
    arm.wall_seconds = began_wall.elapsed().as_secs_f64();
    arm
}

fn print_arm(arm: &Arm, digest: bool) {
    println!("### {} mode\n", arm.mode.to_uppercase());
    println!(
        "{:.2} brain hours over {} frames in {:.0} s wall ({:.2}x realtime).\n",
        arm.hours(),
        arm.frames,
        arm.wall_seconds,
        if arm.wall_seconds > 0.0 { arm.brain_ms / 1000.0 / arm.wall_seconds } else { 0.0 }
    );
    println!("| rung | label | brain hours |");
    println!("| ---: | --- | ---: |");
    for (rank, label, ms) in &arm.rungs {
        println!("| {rank} | {label} | {:.3} |", ms / HOUR_MS);
    }
    println!();
    println!("| measure | value |");
    println!("| --- | ---: |");
    println!("| rung reached | {} |", arm.rung_reached());
    println!("| reward total | {:.3} |", arm.reward);
    println!("| reward per brain hour | {:.3} |", arm.reward_per_hour());
    println!("| new locations | {} |", arm.new_tiles);
    println!("| recoveries | {} |", arm.recoveries);
    println!(
        "| frames with nothing pressed | {} ({:.0}%) |",
        arm.idle_frames,
        percent(arm.idle_frames, arm.frames)
    );
    if arm.mode != "raw" {
        println!(
            "| frames a macro owned the buttons | {} ({:.0}%) |",
            arm.macro_frames,
            percent(arm.macro_frames, arm.frames)
        );
        println!("| macros started | {} |", arm.counts.started);
        println!("| done | {} |", arm.counts.done);
        println!("| blocked | {} |", arm.counts.blocked);
        println!("| timeout | {} |", arm.counts.timeout);
        println!("| refused | {} |", arm.counts.refused);
    }
    if digest {
        println!("| trajectory digest | `{:016x}` |", arm.digest);
    }
    println!();
    if !arm.payouts.is_empty() {
        let payouts: Vec<String> =
            arm.payouts.iter().map(|(kind, count)| format!("{kind} {count}")).collect();
        println!("Payouts by kind: {}.\n", payouts.join(", "));
    }
    if arm.mode != "raw" {
        // Every cause named whether or not the run hit it, so a zero is a measurement rather than
        // a gap in the table.
        println!("| silent frame | frames | share of silent |");
        println!("| --- | ---: | ---: |");
        let silent = arm.idle_frames.max(1);
        for cause in Silence::ALL {
            let frames = arm.silence.get(cause.label()).copied().unwrap_or(0);
            println!(
                "| {} | {frames} | {:.0}% |",
                cause.label(),
                frames as f64 / silent as f64 * 100.0
            );
        }
        println!();
    }
    // Which of the scene's buttons the fly actually pressed, by the slot the palette dealt them
    // into: section 12's gate is raw against macros, and this is the shape of the macros arm.
    if arm.mode == "macros" && !arm.by_rank.is_empty() {
        let started = arm.by_rank.values().sum::<u64>().max(1);
        let ranks: Vec<String> = arm
            .by_rank
            .iter()
            .map(|(rank, count)| {
                format!("slot {rank} {count} ({:.0}%)", *count as f64 / started as f64 * 100.0)
            })
            .collect();
        println!("Macros chosen by slot: {}.\n", ranks.join(", "));
    }
    if !arm.scenes.is_empty() {
        let total = arm.frames.max(1);
        let scenes: Vec<String> = arm
            .scenes
            .iter()
            .map(|(scene, frames)| {
                format!("{scene} {:.0}%", *frames as f64 / total as f64 * 100.0)
            })
            .collect();
        println!("Frames by scene: {}.\n", scenes.join(", "));
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 }
}

fn print_comparison(arms: &[Arm]) {
    println!("### Both arms\n");
    println!("| measure | {} |", arms.iter().map(|arm| arm.mode).collect::<Vec<_>>().join(" | "));
    println!("| --- | {} |", arms.iter().map(|_| "---:").collect::<Vec<_>>().join(" | "));
    let row = |name: &str, cells: Vec<String>| println!("| {name} | {} |", cells.join(" | "));
    row("brain hours", arms.iter().map(|arm| format!("{:.2}", arm.hours())).collect());
    row("rung reached", arms.iter().map(|arm| arm.rung_reached().to_string()).collect());
    row(
        "reward per brain hour",
        arms.iter().map(|arm| format!("{:.3}", arm.reward_per_hour())).collect(),
    );
    row("new locations", arms.iter().map(|arm| arm.new_tiles.to_string()).collect());
    row("recoveries", arms.iter().map(|arm| arm.recoveries.to_string()).collect());
    row("macros started", arms.iter().map(|arm| arm.counts.started.to_string()).collect());
    // Section 7's gate is rung 6 in fewer brain hours, so the brain hours at each rung either arm
    // reached is the column that decides it.
    let top = arms.iter().map(Arm::rung_reached).max().unwrap_or(0);
    println!();
    println!("| rung | label | {} |", arms.iter().map(|arm| arm.mode).collect::<Vec<_>>().join(" | "));
    println!("| ---: | --- | {} |", arms.iter().map(|_| "---:").collect::<Vec<_>>().join(" | "));
    for rank in 0..=top {
        let label = arms
            .iter()
            .flat_map(|arm| arm.rungs.iter())
            .find(|(reached, _, _)| *reached == rank)
            .map(|(_, label, _)| *label)
            .unwrap_or("-");
        let cells: Vec<String> = arms
            .iter()
            .map(|arm| arm.at(rank).map_or("—".to_string(), |hours| format!("{hours:.3}")))
            .collect();
        println!("| {rank} | {label} | {} |", cells.join(" | "));
    }
    println!();
}

fn main() {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        println!(
            "FLY_ROM is not set, so there is nothing to measure.\n\
             \n  FLY_ROM=/path/to/pokemon-red.gb FLY_MACRO_BRAIN=data/fafb-v783 \\\n    \
             FLY_MACRO_HOURS=6 cargo run --release -p flysim --example palette_bench\n\n\
             docs/design/macros.md section 7 is the gate this measures against: macros mode\n\
             ships as the default only if it reaches rung 6 or higher in fewer brain hours in\n\
             3 of 3 runs from the same archived checkpoint."
        );
        return;
    };
    let rom = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("FLY_ROM is {path:?} but could not be read: {error}"));

    let brain = std::env::var_os("FLY_MACRO_BRAIN")
        .or_else(|| std::env::var_os("FLY_DATASET"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/fafb-v783"));
    let data = Arc::new(
        load_brain_dataset_from_dir(&brain)
            .unwrap_or_else(|error| panic!("loading the connectome at {brain:?}: {error}")),
    );

    let hours = env_f64("FLY_MACRO_HOURS", 6.0);
    let threads = env_usize("FLY_MACRO_THREADS", 4);
    let seed = env_usize("FLY_MACRO_SEED", 20_260_916) as u32;
    let digest = std::env::var_os("FLY_MACRO_DIGEST").is_some();
    let serial = std::env::var_os("FLY_MACRO_SERIAL").is_some();
    let wanted = std::env::var("FLY_MACRO_ARMS").unwrap_or_else(|_| "raw,macros".to_string());
    let modes: Vec<MacroMode> = wanted
        .split(',')
        .filter_map(|name| match name.trim() {
            "raw" => Some(MacroMode::Raw),
            // The two names section 12 replaced still name the macros arm, for one release.
            "macros" | "palette" | "plan" => Some(MacroMode::Macros),
            "" => None,
            other => panic!("FLY_MACRO_ARMS: {other:?} is not an arm (raw or macros)"),
        })
        .collect();
    assert!(!modes.is_empty(), "FLY_MACRO_ARMS selected no arms");

    // Either the archived checkpoint both arms share, or one booted cartridge state they share.
    let checkpoint = std::env::var_os("FLY_MACRO_CHECKPOINT").map(|path| {
        flysim::store::load(Path::new(&path))
            .expect("FLY_MACRO_CHECKPOINT should be a FLYSIM01 envelope")
    });
    let booted = if checkpoint.is_some() {
        Vec::new()
    } else {
        let cache = std::env::var_os("FLY_MACRO_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("palette-bench-boot.state"));
        match std::fs::read(&cache) {
            // Progress notes go to stderr so that stdout is the report. binjgb prints the
            // cartridge header to stdout itself on every boot and that cannot be silenced
            // without patching a vendored source (`crates/flybrain-gb/README.md`), so stdout is
            // markdown plus that, and nothing else this example can help.
            Ok(state) => {
                eprintln!("Cartridge state from {}.", cache.display());
                state
            }
            Err(_) => {
                let state = boot_to_bedroom(&rom);
                let _ = std::fs::write(&cache, &state);
                eprintln!("Booted the intro and cached the state at {}.", cache.display());
                state
            }
        }
    };
    let start = match &checkpoint {
        Some(checkpoint) => Start::Live { checkpoint },
        None => Start::Fresh { state: &booted, warmup_ms: flybrain_core::agent::DEFAULT_WARMUP_MS },
    };

    println!("# Macro palette: {}\n", wanted);
    println!(
        "{hours} brain hours per arm, unthrottled, {threads} sweep threads, seed {seed}. \
         Connectome {}.",
        brain.display()
    );
    match &checkpoint {
        Some(checkpoint) => println!(
            "Both arms start from the archived checkpoint: generation {}, {:.2} brain hours in, \
             frame {}, best rung {}.\n",
            checkpoint.runtime.generation,
            checkpoint.agent.network.ms / HOUR_MS,
            checkpoint.runtime.emulator_frame,
            checkpoint.runtime.ratchet.best
        ),
        None => println!(
            "Fresh boot: the harness drove the intro, both arms start from that one playable \
             frame in Red's bedroom with a warmed-up brain. **Not the gate** — section 7 wants \
             three runs from an archived checkpoint.\n"
        ),
    }
    println!("The decoder preset is untouched in both arms; the only difference is the macro layer.\n");

    let arms: Vec<Arm> = if serial || modes.len() == 1 {
        modes
            .iter()
            .map(|mode| run_arm(&rom, &data, &start, *mode, hours, threads, seed))
            .collect()
    } else {
        // Independent runs, so they go side by side: this is a workstation with cores to spare
        // and the arms share nothing but the read-only dataset and the starting state.
        std::thread::scope(|scope| {
            let handles: Vec<_> = modes
                .iter()
                .map(|mode| {
                    let (rom, data, start, mode) = (&rom, &data, &start, *mode);
                    scope.spawn(move || run_arm(rom, data, start, mode, hours, threads, seed))
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().expect("an arm")).collect()
        })
    };

    for arm in &arms {
        print_arm(arm, digest);
    }
    if arms.len() > 1 {
        print_comparison(&arms);
    }
    println!(
        "Section 7's gate: macros mode ships as the default only if it reaches rung 6 or higher \
         in fewer brain hours in 3 of 3 runs from the same archived checkpoint. One run is not \
         that. None of this is evidence about the fly."
    );
}
