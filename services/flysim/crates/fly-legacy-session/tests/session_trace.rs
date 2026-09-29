//! TASK-01's pre-shadow proof: a whole new-runtime session -- the legacy agent worker (AGENT-01),
//! the legacy environment worker (ENV-01) and the `pokered-macros-v1` task and executor in the
//! coordinator -- reproduces the legacy loop's `FLY_TRACE` (FND-01) frame by frame: ticks, clock,
//! remainder, rates, spikes, decision, joypad mask, macro events, frame, work RAM, rewards, rank,
//! slot saves and rollbacks. Where the legacy side is a host run in this process, the ledgers
//! (adapter, ratchet, executor) are compared after every boundary too.
//!
//! Every run is an explicit ROM job (the operator's rom env script, never a download):
//!
//! - `toy_raw_from_power_on` (`FLY_ROM`): the committed toy connectome in raw mode from power-on,
//!   with sugar admitted through the coordinator's admission queue.
//! - `fafb_service_trace_checkpoints` (`FLY_TASK01_FAFB=1`, `FLY_ENV01_TRACE_DIR`,
//!   `FLY_DOOR_CHECKPOINT`): the checkpoints of the service's own traces (the FND-01 review's
//!   `rollback`, `climb` and `r58`: a ratchet rollback, a slot save, two real rewards), for each
//!   trace's length, against today's legacy loop; the session imported from the same FLYSIM01
//!   files. `FLY_TASK01_TRACE_LIMIT` caps each.
//! - `fafb_reward_segments` (`FLY_TASK01_FAFB=1`): host runs of `LegacyFrame` from stream
//!   checkpoints, with sugar, in two arms. The brain arm: the adapter ledger thinned so rewards
//!   fire where the fly already is (new area, exits, new ground, wild wins). The driver arm: the
//!   brain ticks and decodes as always but the executor is handed the ROM tests' rotation driver's
//!   decision (`fly_legacy_session::driver`), which walks, talks, fights and picks up, so real
//!   rewards reach `Agent.Commit`. `FLY_TASK01_SEGMENTS` picks the checkpoints (`name=path,...`),
//!   `FLY_TASK01_FRAMES` the length.
//!
//! Execution modes: in-process, then process (the workspace's `fly-session` binary); a build
//! without it skips process mode and says so.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use fly_legacy_session::admission::LegacyAdmission;
use fly_legacy_session::composition::{LegacyConfig, LegacySession, channels_and_hold};
use fly_legacy_session::driver::{DecisionDriver, RotationDriver};
use fly_legacy_session::task::ledgers_of;
use fly_legacy_session::trace::{self, Agreement};
use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity;
use fly_session::types::id;
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::{BrainDataset, load_brain_dataset_from_dir};
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{LegacyFrame, Parts};
use flysim::macros::macro_layer;
use flysim::snapshot::MacroMode;
use flysim::trace::FrameTrace;

fn rom_path() -> Option<PathBuf> {
    match std::env::var_os("FLY_ROM") {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
            None
        }
    }
}

fn fafb() -> Option<PathBuf> {
    if std::env::var_os("FLY_TASK01_FAFB").is_none() {
        eprintln!("skipping: FLY_TASK01_FAFB is not set");
        return None;
    }
    let dir = legacy_parity::fafb_dir();
    if dir.is_none() {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
    }
    dir
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn modes() -> Vec<ExecutionMode> {
    let mut modes = vec![ExecutionMode::InProcess];
    let program = fly_session::launcher::default_worker_program();
    if program.is_file() {
        modes.push(ExecutionMode::Process);
    } else {
        eprintln!(
            "process mode skipped: no worker program at {}",
            program.display()
        );
    }
    modes
}

/// What each frame admits at its top, in admission order, and whether the rotation driver hands
/// the executor its decisions.
#[derive(Clone, Debug, Default)]
struct Inputs {
    /// `(frame index from 1, duration ms)`.
    sugar: Vec<(usize, f64)>,
    driver: bool,
}

/// The legacy loop's one look into a decision: the driver's, when the run has one.
struct Driven(Option<RotationDriver>);

impl flysim::frame::FrameObserver for Driven {
    fn readout(&mut self, ms: f64, bound: Option<&[String]>, active: &mut Vec<String>) {
        if let Some(driver) = self.0.as_mut() {
            driver.readout(ms, bound.unwrap_or(&[]), active);
        }
    }
}

/// How a run starts.
#[derive(Clone)]
enum Start {
    PowerOn,
    /// A FLYSIM01 file.
    Checkpoint(Vec<u8>),
}

/// The run's `FLY_TRACE` behaviours and, when it has them, the ledgers after every boundary.
struct Run {
    behaviours: Vec<Value>,
    ledgers: Vec<String>,
    /// The bound set of each transition's decision (session runs only).
    bounds: Vec<Vec<String>>,
}

fn decoder_hold(mode: MacroMode) -> (Vec<&'static str>, f64) {
    let channels = if mode.dealt() {
        flybrain_gb::macro_channels("pokemon-red")
    } else {
        Vec::new()
    };
    let (_, hold) = channels_and_hold(mode);
    (channels, hold)
}

/// The legacy loop in this process: `LegacyFrame` with `FLY_TRACE` on, admitting the inputs at
/// the top of each frame as the service's command drain does, the ledgers read after each
/// boundary.
fn run_legacy(
    rom: &[u8],
    data: &Arc<BrainDataset>,
    start: &Start,
    mode: MacroMode,
    frames: usize,
    inputs: &Inputs,
    trace_path: &Path,
) -> Run {
    let mut emulator =
        Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES).expect("binjgb");
    let mut adapter = PokemonRedReward::new();
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_owned();
    config.macros.mode = mode;
    let (channels, hold_ms) = decoder_hold(mode);
    let mut agent_config = AgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
    agent_config.warmup_ms = config.loop_.warmup_ms;
    let mut agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("an agent");
    let mut frame =
        LegacyFrame::new().with_trace(Some(FrameTrace::create(trace_path).expect("a trace")));
    match start {
        Start::PowerOn => {
            frame
                .initialize(&mut emulator, &mut agent)
                .expect("the first frame and the warm-up");
        }
        Start::Checkpoint(bytes) => {
            let checkpoint = flysim::store::decode(bytes).expect("FLYSIM01");
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
                .expect("the checkpoint restores");
        }
    }
    let mut macros = macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
    if let Some(layer) = macros.as_mut() {
        let _ = layer.observe(&mut emulator, &AdapterLedger(&adapter), agent.network.ms);
    }
    let mut ledgers = Vec::new();
    let mut observer = Driven(inputs.driver.then(RotationDriver::new));
    for k in 1..=frames {
        // The command drain: the sugar admitted for this frame, in admission order.
        for (_, duration) in inputs.sugar.iter().filter(|(at, _)| *at == k) {
            agent.network.stimulate(*duration);
            if let Some(trace) = frame.trace_mut() {
                trace.sugar(*duration);
            }
        }
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: macros.as_mut(),
        };
        let transition = frame
            .transition(&mut parts, &mut observer)
            .expect("a frame");
        frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .expect("the boundary");
        ledgers.push(ledgers_of(&adapter, &ratchet, macros.as_ref()));
    }
    frame.finish_trace();
    drop(frame);
    let text = std::fs::read_to_string(trace_path).expect("the trace reads back");
    Run {
        behaviours: trace::behaviours(&text).expect("a FLY_TRACE"),
        ledgers,
        bounds: Vec::new(),
    }
}

/// The same run on the session framework.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    mode: ExecutionMode,
    rom_path: &Path,
    dataset_dir: &Path,
    profile: LegacyProfileKind,
    macro_mode: MacroMode,
    start: &Start,
    frames: usize,
    inputs: &Inputs,
) -> Run {
    let root = tempfile::tempdir().expect("tmp");
    let config = LegacyConfig {
        mode,
        rom_path: rom_path.to_owned(),
        dataset_dir: dataset_dir.to_owned(),
        profile,
        macro_mode,
        agent_id: id("fly"),
        agent_threads: 1,
        record: true,
    };
    let mut session = LegacySession::start(root.path(), config)
        .await
        .unwrap_or_else(|e| panic!("the session starts: {e}"));
    if inputs.driver {
        session.task.set_driver(Box::new(RotationDriver::new()));
    }
    session
        .bootstrap()
        .await
        .unwrap_or_else(|e| panic!("Ready(0): {e}"));
    if let Start::Checkpoint(bytes) = start {
        session
            .import_flysim01(bytes, &id("flysim01"), &id("e2"))
            .await
            .unwrap_or_else(|e| panic!("the FLYSIM01 import: {e}"));
    }
    let mut admission = LegacyAdmission::new(session.coordinator.admissions(), id("fly"));
    assert!(
        admission.reward(1.0).is_err(),
        "the operator reward pulse is refused"
    );
    let mut behaviours = Vec::new();
    let mut ledgers = Vec::new();
    let mut bounds = Vec::new();
    for k in 1..=frames {
        for (_, duration) in inputs.sugar.iter().filter(|(at, _)| *at == k) {
            admission.replay_sugar(*duration);
        }
        let step = session
            .step()
            .await
            .unwrap_or_else(|e| panic!("{} frame {k}: {e}", mode.label()));
        let details = step.details.expect("details are recorded");
        let [record] = step.records.as_slice() else {
            panic!("frame {k}: {} task records", step.records.len());
        };
        behaviours.push(
            trace::line(&details, record).unwrap_or_else(|e| panic!("frame {k}: {e}"))["behaviour"]
                .clone(),
        );
        ledgers.push(record.ledgers.clone());
        bounds.push(record.bound.clone());
    }
    let ended = session.coordinator.admissions().take_ended();
    assert_eq!(
        ended.len(),
        inputs.sugar.iter().filter(|(k, _)| *k <= frames).count(),
        "every admission ended"
    );
    assert!(
        ended
            .iter()
            .all(|(_, end)| matches!(end, fly_session::coordinator::AdmissionEnd::Applied { .. })),
        "and every one was applied"
    );
    let uncaptured = session.task.uncaptured_reads();
    assert!(
        uncaptured.is_empty(),
        "the engine read addresses the image does not capture: {:04x?}",
        uncaptured
    );
    session.stop().await;
    Run {
        behaviours,
        ledgers,
        bounds,
    }
}

fn report(what: &str, agreement: &Agreement, elapsed: std::time::Duration) {
    eprintln!(
        "{what} ({elapsed:?}): {} transitions identical ({} sugar, {} rewards {:?}, \
         {} macro events, {} pressed, {} slot saves, {} rollbacks, {} spikes)",
        agreement.transitions,
        agreement.sugar,
        agreement.rewards,
        agreement.reward_kinds,
        agreement.macro_events,
        agreement.pressed,
        agreement.saves,
        agreement.rollbacks,
        agreement.spikes
    );
}

fn compare_ledgers(legacy: &[String], session: &[String]) {
    assert_eq!(legacy.len(), session.len());
    for (k, (a, b)) in legacy.iter().zip(session).enumerate() {
        assert_eq!(a, b, "the ledgers after frame {}", k + 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toy_raw_from_power_on() {
    let Some(rom_path) = rom_path() else { return };
    let rom = std::fs::read(&rom_path).expect("the cartridge");
    let data = Arc::new(load_brain_dataset_from_dir(legacy_parity::toy::dir()).expect("the toy"));
    let frames = env_usize("FLY_TASK01_TOY_FRAMES", 600);
    let inputs = Inputs {
        sugar: vec![(40, 300.0), (frames / 2, 500.0)],
        driver: false,
    };
    let dir = tempfile::tempdir().expect("tmp");
    let legacy = run_legacy(
        &rom,
        &data,
        &Start::PowerOn,
        MacroMode::Raw,
        frames,
        &inputs,
        &dir.path().join("trace.jsonl"),
    );
    for mode in modes() {
        let started = std::time::Instant::now();
        let session = run_session(
            mode,
            &rom_path,
            &legacy_parity::toy::dir(),
            LegacyProfileKind::Toy,
            MacroMode::Raw,
            &Start::PowerOn,
            frames,
            &inputs,
        )
        .await;
        let agreement = trace::compare(&legacy.behaviours, &session.behaviours, &session.bounds)
            .unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
        compare_ledgers(&legacy.ledgers, &session.ledgers);
        assert_eq!(agreement.sugar, 2);
        report(
            &format!("toy raw, {}", mode.label()),
            &agreement,
            started.elapsed(),
        );
    }
}

/// The checkpoints of the service's own traces (the FND-01 review's `rollback` -- a ratchet
/// rollback on the first boundary, a real `talk` reward -- `climb` -- a ratchet slot save -- and
/// `r58` -- a real `talk` reward -- with their start checkpoints).
///
/// Those traces were recorded by the service as it stood on 2026-09-23. The macro engine has
/// changed since (loop-review rows 59 to 67), so from some frame on they are not what today's
/// engine does. The reference is therefore FND-01's harness at this tree -- `LegacyFrame` with
/// `FLY_TRACE` on, which FND-01 proved byte-identical to the service's loop -- run here from the
/// same checkpoint for the trace's length; the session must match it frame by frame, ledgers
/// included. How far the recorded service trace agrees with today's legacy loop is reported, not
/// asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fafb_service_trace_checkpoints() {
    let Some(rom_path) = rom_path() else { return };
    let Some(dataset) = fafb() else { return };
    let Some(dir) = std::env::var_os("FLY_ENV01_TRACE_DIR").map(PathBuf::from) else {
        eprintln!("skipping: FLY_ENV01_TRACE_DIR is not set");
        return;
    };
    let rom = std::fs::read(&rom_path).expect("the cartridge");
    let data = Arc::new(load_brain_dataset_from_dir(&dataset).expect("the dataset"));
    let limit = env_usize("FLY_TASK01_TRACE_LIMIT", usize::MAX);
    let names: Vec<String> = std::env::var("FLY_TASK01_TRACES")
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_else(|_| vec!["rollback".into(), "climb".into(), "r58".into()]);
    for name in names {
        let checkpoint = if name == "r58" {
            std::env::var_os("FLY_DOOR_CHECKPOINT").map(PathBuf::from)
        } else {
            Some(dir.join(format!("{name}.checkpoint")))
        };
        let Some(checkpoint) = checkpoint.filter(|c| c.is_file()) else {
            eprintln!("skipping {name}: no checkpoint");
            continue;
        };
        let text =
            std::fs::read_to_string(dir.join(format!("{name}.trace.jsonl"))).expect("the trace");
        let mut service = trace::behaviours(&text).expect("a FLY_TRACE");
        service.truncate(limit);
        let frames = service.len();
        let bytes = std::fs::read(&checkpoint).expect("the checkpoint");
        let start = Start::Checkpoint(bytes);
        let tmp = tempfile::tempdir().expect("tmp");
        let started = std::time::Instant::now();
        let legacy = run_legacy(
            &rom,
            &data,
            &start,
            MacroMode::Macros,
            frames,
            &Inputs::default(),
            &tmp.path().join("trace.jsonl"),
        );
        let agreed = legacy
            .behaviours
            .iter()
            .zip(&service)
            .take_while(|(today, recorded)| today == recorded)
            .count();
        eprintln!(
            "legacy loop {name}: {frames} frames in {:?}; the recorded service trace agrees with \
             today's loop for its first {agreed} transitions",
            started.elapsed()
        );
        for mode in modes() {
            let started = std::time::Instant::now();
            let session = run_session(
                mode,
                &rom_path,
                &dataset,
                LegacyProfileKind::Production,
                MacroMode::Macros,
                &start,
                frames,
                &Inputs::default(),
            )
            .await;
            let agreement =
                trace::compare(&legacy.behaviours, &session.behaviours, &session.bounds)
                    .unwrap_or_else(|e| panic!("{name}, {}: {e}", mode.label()));
            compare_ledgers(&legacy.ledgers, &session.ledgers);
            report(
                &format!("service checkpoint {name}, {}", mode.label()),
                &agreement,
                started.elapsed(),
            );
        }
    }
}

/// Thins a checkpoint's adapter ledger so the fly earns rewards: the current map is unvisited
/// (an `AREA` payout on the first sample), its exits unfound (`boundary`), its ground unwalked
/// (`exploration`), and no wild win is a replay (`battle`).
fn reward_bearing(bytes: &[u8]) -> Vec<u8> {
    let mut checkpoint = flysim::store::decode(bytes).expect("FLYSIM01");
    let reward = &mut checkpoint.runtime.reward;
    let map = reward["location"]
        .as_str()
        .and_then(|l| l.split(':').next())
        .unwrap_or("0")
        .to_owned();
    if let Some(seen) = reward["seen"].as_array_mut() {
        seen.retain(|key| {
            let key = key.as_str().unwrap_or("");
            key != format!("map:{map}") && !key.starts_with(&format!("boundary:{map}:"))
        });
    }
    if let Some(tiles) = reward["tiles"].as_array_mut() {
        tiles.retain(|tile| !tile.as_str().unwrap_or("").starts_with(&format!("{map}:")));
    }
    if let Some(counts) = reward["tileCounts"].as_object_mut() {
        counts.remove(&map);
    }
    reward["wildWins"] = serde_json::json!({});
    reward["replayBlocked"] = serde_json::json!([]);
    flysim::store::encode(&checkpoint.agent, &checkpoint.runtime).expect("encodes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fafb_reward_segments() {
    let Some(rom_path) = rom_path() else { return };
    let Some(dataset) = fafb() else { return };
    let rom = std::fs::read(&rom_path).expect("the cartridge");
    let segments: Vec<(String, PathBuf)> = match std::env::var("FLY_TASK01_SEGMENTS") {
        Ok(list) => list
            .split(',')
            .filter_map(|item| {
                item.split_once('=')
                    .map(|(n, p)| (n.to_owned(), PathBuf::from(p)))
            })
            .collect(),
        Err(_) => [
            "FLY_DOOR_CHECKPOINT",
            "FLY_CATCH_CHECKPOINT",
            "FLY_ENGAGE_CHECKPOINT",
        ]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(|p| ((*var).to_owned(), PathBuf::from(p))))
        .collect(),
    };
    let frames = env_usize("FLY_TASK01_FRAMES", 1200);
    let data = Arc::new(load_brain_dataset_from_dir(&dataset).expect("the dataset"));
    let arms: Vec<String> = std::env::var("FLY_TASK01_ARMS")
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_else(|_| vec!["brain".into(), "driver".into()]);
    for (segment, path) in segments {
        for arm in &arms {
            let name = format!("{segment} {arm}");
            let original = std::fs::read(&path).expect("the checkpoint");
            let bytes = if arm == "brain" {
                reward_bearing(&original)
            } else {
                original
            };
            let inputs = Inputs {
                sugar: vec![(10, 400.0), (frames / 2, 250.0)],
                driver: arm == "driver",
            };
            let dir = tempfile::tempdir().expect("tmp");
            let started = std::time::Instant::now();
            let start = Start::Checkpoint(bytes);
            let legacy = run_legacy(
                &rom,
                &data,
                &start,
                MacroMode::Macros,
                frames,
                &inputs,
                &dir.path().join("trace.jsonl"),
            );
            eprintln!(
                "legacy loop {name}: {frames} frames in {:?}",
                started.elapsed()
            );
            for mode in modes() {
                let started = std::time::Instant::now();
                let session = run_session(
                    mode,
                    &rom_path,
                    &dataset,
                    LegacyProfileKind::Production,
                    MacroMode::Macros,
                    &start,
                    frames,
                    &inputs,
                )
                .await;
                let agreement =
                    trace::compare(&legacy.behaviours, &session.behaviours, &session.bounds)
                        .unwrap_or_else(|e| panic!("{name}, {}: {e}", mode.label()));
                compare_ledgers(&legacy.ledgers, &session.ledgers);
                report(
                    &format!("segment {name}, {}", mode.label()),
                    &agreement,
                    started.elapsed(),
                );
            }
        }
    }
}
