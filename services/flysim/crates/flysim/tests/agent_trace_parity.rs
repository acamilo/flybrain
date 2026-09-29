//! AGENT-01 against FND-01: the legacy agent worker, and the direct reference, checked against the
//! `FLY_TRACE` of the legacy frame the stream runs (`flysim::frame::LegacyFrame`).
//!
//! The trace records what the loop did to the brain as digests -- ticks, clock, exact remainder,
//! rates, the spike set, the decision -- but not everything a worker needs to be driven: the
//! frames (only their digests), and the `{boot, bound, location}` context the executor and the
//! adapter produced. So this test is the host: it runs `LegacyFrame` on the real emulator with the
//! trace on, records those inputs beside it as a `LegacyScript`, then drives the same script
//! through `LegacyAgentWorker` in every execution mode and through `DirectReference`, and holds
//! each record to its trace line (`fly_session::legacy_parity::check_against_trace`). The script
//! is checked against the trace as well: the same admissions, frames, reward events and rollback.
//!
//! Two runs, both explicit ROM jobs (`. bin/rom-env.sh`, never a download):
//!
//! - `toy_raw_from_power_on`: `FLY_ROM` only. The committed toy connectome in raw mode from
//!   power-on, with two sugar admissions. Cheap enough for every ROM run of the workspace.
//! - `fafb_macros_from_a_checkpoint`: also `FLY_AGENT01_FAFB=1` and `data/fafb-v783`. The live
//!   composition -- `gameboy-legacy-fafb-v783-v1`, macros mode -- restored from a stream
//!   checkpoint (`FLY_AGENT01_TRACE_CHECKPOINT`, else `FLY_DOOR_CHECKPOINT`), with sugar and one
//!   ratchet rollback onto the checkpoint's slot. The worker starts from the checkpoint's agent
//!   state (`AgentDriver::seed_from_legacy_state`).
//!
//! `FLY_AGENT01_TRACE_FRAMES` sets the length (default 600 toy, 240 FAFB). The worker program for
//! process mode is the workspace's `fly-session` binary; a build that did not produce it skips
//! that mode and says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fly_session::ExecutionMode;
use fly_session::fly_session_types::gameboy::{Location, ReadoutContext};
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity::{
    self, DirectSource, LegacyRig, LegacyScript, ParityRecord, ReferenceSource, RestoredSource,
    RewardEvent, RigAgent, ScriptStep, TraceCheck, TraceTransition, toy,
};
use fly_session::types::{digest_of_bytes, id};
use flybrain_core::agent::{AgentConfig, AgentState, NeuralAgent};
use flybrain_core::dataset::{BrainDataset, load_brain_dataset_from_dir};
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{LegacyFrame, Parts};
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;
use flysim::trace::FrameTrace;

fn rom() -> Option<Vec<u8>> {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
        return None;
    };
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("FLY_ROM {path:?}: {e}")))
}

fn frames(default: usize) -> usize {
    std::env::var("FLY_AGENT01_TRACE_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn context(adapter: &dyn GameAdapter, macros: Option<&MacroLayer>) -> ReadoutContext {
    ReadoutContext {
        boot: adapter.boot(),
        bound: macros.map(MacroLayer::bound_channels).unwrap_or_default(),
        location: adapter
            .location()
            .map(|(area, x, y)| Location { area, x, y }),
    }
}

/// Frames by content, so a still screen is stored once.
#[derive(Default)]
struct Pool {
    frames: Vec<Vec<u8>>,
    index: HashMap<String, usize>,
}

impl Pool {
    fn add(&mut self, frame: &[u8]) -> usize {
        let digest = digest_of_bytes(frame);
        *self.index.entry(digest).or_insert_with(|| {
            self.frames.push(frame.to_vec());
            self.frames.len() - 1
        })
    }
}

/// How the legacy run starts.
enum Start<'a> {
    /// `LegacyFrame::initialize`: one frame with no button, then the warm-up on it.
    PowerOn,
    /// `LegacyFrame::restore` from a stream checkpoint, as `Sim::try_restore` does.
    Checkpoint(&'a flysim::store::Checkpoint),
}

struct LegacyRun {
    script: LegacyScript,
    trace: Vec<TraceTransition>,
    /// The agent state the run started from, for a restored start.
    start: Option<AgentState>,
    rewards: Vec<&'static str>,
}

/// The stream's frame on the real emulator with `FLY_TRACE` on, recording the script beside it.
///
/// Sugar is admitted the way `Sim::stimulate` admits it (stimulate, then the trace), at the top of
/// the frame. `rollback_at` forces the ratchet's rollback at that boundary when the ratchet holds a
/// slot and did not roll back itself: `LegacyFrame::rollback` is exactly what `boundary` runs when
/// the ratchet fires, and whether it fires is the task's decision, not the agent's.
#[allow(clippy::too_many_arguments)]
fn run_legacy(
    rom: &[u8],
    data: &Arc<BrainDataset>,
    start: Start<'_>,
    mode: MacroMode,
    frames: usize,
    sugar: &[(usize, f64)],
    rollback_at: Option<usize>,
    trace_path: &Path,
) -> LegacyRun {
    let mut emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    let mut adapter = PokemonRedReward::new();
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = mode;
    let channels = if mode.dealt() {
        flybrain_gb::macro_channels("pokemon-red")
    } else {
        Vec::new()
    };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .expect("the preset has a group")
        .hold_ms;
    let mut agent_config = AgentConfig::with_decoder(preset);
    agent_config.warmup_ms = config.loop_.warmup_ms;
    assert_eq!(
        config.loop_.warmup_ms,
        fly_session::fly_session_types::gameboy::WARMUP_MS,
        "the stream's warm-up is the profile's"
    );
    let mut agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("a valid agent");
    let mut frame =
        LegacyFrame::new().with_trace(Some(FrameTrace::create(trace_path).expect("a trace file")));
    let start_state = match start {
        Start::PowerOn => {
            frame
                .initialize(&mut emulator, &mut agent)
                .expect("the first frame and the warm-up");
            None
        }
        Start::Checkpoint(checkpoint) => {
            frame
                .restore(
                    &mut Parts {
                        agent: &mut agent,
                        emulator: &mut emulator,
                        adapter: &mut adapter,
                        ratchet: &mut ratchet,
                        macros: None,
                    },
                    checkpoint,
                )
                .expect("the checkpoint should restore");
            Some(checkpoint.agent.clone())
        }
    };
    // `Sim::boot`: the macro layer is built after the restore or warm-up, seeded from the network
    // as it then stands, and observes once before the first frame.
    let mut macros = macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
    if let Some(layer) = macros.as_mut() {
        let ledger = AdapterLedger(&adapter);
        let _ = layer.observe(&mut emulator, &ledger, agent.network.ms);
    }

    let mut pool = Pool::default();
    let initial_frame = pool.add(&frame.frame_buffer);
    let initial_context = context(&adapter, macros.as_ref());
    let mut steps = Vec::new();
    let mut kinds = Vec::new();
    for k in 1..=frames {
        let mut admitted = Vec::new();
        for (_, duration) in sugar.iter().filter(|(at, _)| *at == k) {
            agent.network.stimulate(*duration);
            if let Some(trace) = frame.trace_mut() {
                trace.sugar(*duration);
            }
            admitted.push(*duration);
        }
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: macros.as_mut(),
        };
        let transition = frame.transition(&mut parts, &mut ()).expect("a frame");
        let produced = pool.add(&frame.frame_buffer);
        let rewards: Vec<RewardEvent> = transition
            .evaluated
            .rewards
            .iter()
            .map(|event| {
                kinds.push(event.kind);
                RewardEvent {
                    value: event.value,
                    stimulation_ms: f64::from(event.stimulation_ms),
                }
            })
            .collect();
        // The next context is read after the evaluation and before the ratchet: what the task
        // hands `Agent.Commit`.
        let next_context = context(&*parts.adapter, parts.macros.as_deref());
        let boundary = frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .expect("the boundary");
        steps.push(ScriptStep::Frame {
            sugar: admitted,
            frame: produced,
            rewards,
            next_context,
        });
        let mut rolled_back = boundary.rollback.is_some();
        if !rolled_back && rollback_at == Some(k) && parts.ratchet.game().is_some() {
            frame.rollback(&mut parts).expect("the forced rollback");
            rolled_back = true;
        }
        if rolled_back {
            let restored = pool.add(&frame.frame_buffer);
            steps.push(ScriptStep::Rollback {
                frame: restored,
                context: context(&*parts.adapter, parts.macros.as_deref()),
            });
        }
    }
    frame.finish_trace();
    drop(frame);
    let text = std::fs::read_to_string(trace_path).expect("the trace reads back");
    let trace = legacy_parity::read_frame_trace(&text).expect("a flysim-legacy-frame-trace-v1");
    LegacyRun {
        script: LegacyScript {
            name: format!("flysim-{}", mode.as_str()),
            macro_channels: channels.iter().map(|c| (*c).to_owned()).collect(),
            frames: Arc::new(pool.frames),
            initial_frame,
            initial_context,
            steps,
            checkpoints: Default::default(),
        },
        trace,
        start: start_state,
        rewards: kinds,
    }
}

fn worker_modes() -> Vec<ExecutionMode> {
    let mut modes = vec![ExecutionMode::InProcess, ExecutionMode::Thread];
    let program = fly_session::launcher::default_worker_program();
    if program.is_file() {
        modes.push(ExecutionMode::Process);
    } else {
        eprintln!(
            "process mode skipped: no worker program at {} (build the fly-session binary, or set {})",
            program.display(),
            fly_session::launcher::WORKER_PROGRAM_ENV
        );
    }
    modes
}

async fn on_worker(
    mode: ExecutionMode,
    run: &LegacyRun,
    dataset_dir: PathBuf,
    profile: LegacyProfileKind,
) -> Vec<ParityRecord> {
    let root = tempfile::tempdir().expect("tmp");
    let agent = RigAgent {
        agent_id: id("fly"),
        port_id: id("p1"),
        dataset_dir,
        profile,
        macro_channels: run.script.macro_channels.clone(),
        worker_threads: 1,
    };
    let mut rig = LegacyRig::start(root.path(), mode, &[agent])
        .await
        .expect("the rig");
    let records =
        legacy_parity::run_on_worker_from(&mut rig, &id("fly"), &run.script, run.start.as_ref())
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
    rig.stop().await;
    records
}

fn report(what: &str, check: &TraceCheck) {
    eprintln!(
        "{what}: {} transitions identical to FLY_TRACE ({} sugar, {} reward events, {} rollbacks, \
         {} macro decisions, {} pressed, {} spikes)",
        check.transitions,
        check.sugar,
        check.rewards,
        check.rollbacks,
        check.macro_decisions,
        check.pressed,
        check.spikes
    );
}

async fn check_all(
    run: &LegacyRun,
    reference: Vec<ParityRecord>,
    dataset_dir: PathBuf,
    profile: LegacyProfileKind,
) {
    let check = legacy_parity::check_against_trace(&run.script, &reference, &run.trace)
        .unwrap_or_else(|e| panic!("the direct reference against FLY_TRACE: {e}"));
    report("direct reference", &check);
    for mode in worker_modes() {
        let started = std::time::Instant::now();
        let got = on_worker(mode, run, dataset_dir.clone(), profile).await;
        let worker = legacy_parity::check_against_trace(&run.script, &got, &run.trace)
            .unwrap_or_else(|e| panic!("{} against FLY_TRACE: {e}", mode.label()));
        assert_eq!(worker, check);
        // And record for record, telemetry included, the worker is the reference.
        legacy_parity::compare(&reference, &got)
            .unwrap_or_else(|e| panic!("{} against the direct reference: {e}", mode.label()));
        report(
            &format!("worker {} ({:?})", mode.label(), started.elapsed()),
            &worker,
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toy_raw_from_power_on() {
    let Some(rom) = rom() else { return };
    let data = Arc::new(load_brain_dataset_from_dir(toy::dir()).expect("the toy dataset"));
    let dir = tempfile::tempdir().expect("tmp");
    let n = frames(600);
    let run = run_legacy(
        &rom,
        &data,
        Start::PowerOn,
        MacroMode::Raw,
        n,
        &[(40, 300.0), (n / 2, 500.0)],
        None,
        &dir.path().join("trace.jsonl"),
    );
    assert_eq!(run.trace.len(), n);
    let reference = DirectSource { dataset: data }
        .records(&run.script)
        .expect("the direct reference runs");
    check_all(&run, reference, toy::dir(), LegacyProfileKind::Toy).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fafb_macros_from_a_checkpoint() {
    let Some(rom) = rom() else { return };
    if std::env::var_os("FLY_AGENT01_FAFB").is_none() {
        eprintln!("skipping: FLY_AGENT01_FAFB is not set");
        return;
    }
    let Some(dataset_dir) = legacy_parity::fafb_dir() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let Some(path) = std::env::var_os("FLY_AGENT01_TRACE_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_DOOR_CHECKPOINT"))
    else {
        eprintln!("skipping: neither FLY_AGENT01_TRACE_CHECKPOINT nor FLY_DOOR_CHECKPOINT is set");
        return;
    };
    let checkpoint = flysim::store::load(Path::new(&path)).expect("a FLYSIM01 checkpoint");
    let data = Arc::new(load_brain_dataset_from_dir(&dataset_dir).expect("the dataset"));
    let dir = tempfile::tempdir().expect("tmp");
    let n = frames(240);
    let started = std::time::Instant::now();
    let run = run_legacy(
        &rom,
        &data,
        Start::Checkpoint(&checkpoint),
        MacroMode::Macros,
        n,
        &[(10, 400.0), (n / 2, 250.0)],
        Some(n / 3),
        &dir.path().join("trace.jsonl"),
    );
    eprintln!(
        "legacy loop: {n} frames from {} in {:?}; reward events {:?}",
        Path::new(&path).display(),
        started.elapsed(),
        run.rewards
    );
    assert!(
        run.script
            .steps
            .iter()
            .any(|s| matches!(s, ScriptStep::Rollback { .. })),
        "the checkpoint holds no ratchet slot to roll back onto"
    );
    let reference = RestoredSource {
        dataset: data,
        state: run.start.clone().expect("a restored start"),
    }
    .records(&run.script)
    .expect("the direct reference runs");
    check_all(&run, reference, dataset_dir, LegacyProfileKind::Production).await;
}
