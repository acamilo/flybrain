//! TASK-01 review N5: the session runtime's cost per frame against the legacy loop's, on the same
//! box, from the same checkpoint, in the service configuration (no trace, no step details).
//!
//! `FLY_TASK01_BENCH=1` (plus rom-env and `data/fafb-v783`): the legacy loop (`LegacyFrame`, the
//! service's frame, trace off) and the session (agent, world and task under the coordinator,
//! details off) each run `FLY_TASK01_BENCH_FRAMES` unpaced frames after a warm-up, with
//! `FLY_TASK01_BENCH_THREADS` sweep threads (the release container runs 3). Reported: ms per frame,
//! the realtime factor at 1x (one frame is 16.74 ms of game time), and the session's per-method
//! latencies from the coordinator's metrics. Then the session runs paced at 1x for the same
//! number of frames and reports the realtime factor it held.

use std::sync::Arc;
use std::time::Instant;

use fly_legacy_session::composition::{LegacyConfig, LegacySession, channels_and_hold};
use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity;
use fly_session::types::id;
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::lif::SweepPlan;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{LegacyFrame, Parts};
use flysim::macros::macro_layer;
use flysim::snapshot::MacroMode;

const FRAME_MS: f64 = 1000.0 * 70_224.0 / 4_194_304.0;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_against_legacy_per_frame() {
    if std::env::var_os("FLY_TASK01_BENCH").is_none() {
        eprintln!("skipping: FLY_TASK01_BENCH is not set");
        return;
    }
    let (Some(rom_path), Some(dataset), Some(checkpoint_path)) = (
        std::env::var_os("FLY_ROM").map(std::path::PathBuf::from),
        legacy_parity::fafb_dir(),
        std::env::var_os("FLY_DOOR_CHECKPOINT").map(std::path::PathBuf::from),
    ) else {
        eprintln!("skipping: FLY_ROM, data/fafb-v783 or FLY_DOOR_CHECKPOINT missing");
        return;
    };
    let frames = env_usize("FLY_TASK01_BENCH_FRAMES", 1200);
    let warmup = env_usize("FLY_TASK01_BENCH_WARMUP", 120);
    let threads = env_usize("FLY_TASK01_BENCH_THREADS", 3);
    let rom = std::fs::read(&rom_path).unwrap();
    let bytes = std::fs::read(&checkpoint_path).unwrap();
    let checkpoint = flysim::store::decode(&bytes).unwrap();

    // ---- The legacy loop: the service's frame, trace off.
    let data = Arc::new(load_brain_dataset_from_dir(&dataset).unwrap());
    let mut emulator = Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES).unwrap();
    let mut adapter = PokemonRedReward::new();
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_owned();
    config.macros.mode = MacroMode::Macros;
    let (channels, hold_ms) = channels_and_hold(MacroMode::Macros);
    let channels: Vec<&str> = channels.iter().map(String::as_str).collect();
    let mut agent_config = AgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
    agent_config.warmup_ms = config.loop_.warmup_ms;
    let mut agent = NeuralAgent::new(Arc::clone(&data), agent_config).unwrap();
    if threads > 1 {
        agent.set_sweep_plan(SweepPlan::with_threads(threads).unwrap());
    }
    let mut frame = LegacyFrame::new();
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
        .unwrap();
    let mut macros = macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
    if let Some(layer) = macros.as_mut() {
        let _ = layer.observe(&mut emulator, &AdapterLedger(&adapter), agent.network.ms);
    }
    let mut legacy_ms = 0.0;
    for n in 0..warmup + frames {
        let started = Instant::now();
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: macros.as_mut(),
        };
        let transition = frame.transition(&mut parts, &mut ()).unwrap();
        frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .unwrap();
        if n >= warmup {
            legacy_ms += started.elapsed().as_secs_f64() * 1000.0;
        }
    }
    drop(agent);
    let legacy_per = legacy_ms / frames as f64;
    eprintln!(
        "legacy loop: {legacy_per:.2} ms/frame over {frames} frames ({threads} threads), realtime factor at 1x {:.2}",
        (FRAME_MS / legacy_per).min(1.0)
    );

    // ---- The session, unpaced then paced, in each mode.
    for mode in [ExecutionMode::InProcess, ExecutionMode::Process] {
        for paced in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut session = LegacySession::start(
                root.path(),
                LegacyConfig {
                    mode,
                    rom_path: rom_path.clone(),
                    dataset_dir: dataset.clone(),
                    profile: LegacyProfileKind::Production,
                    macro_mode: MacroMode::Macros,
                    agent_id: id("fly"),
                    agent_threads: threads.max(1),
                    record: false,
                },
            )
            .await
            .unwrap();
            session.bootstrap().await.unwrap();
            session.import_flysim01(&bytes, &id("e2")).await.unwrap();
            if !paced {
                session.coordinator.disable_pacing();
            }
            for _ in 0..warmup {
                session.step().await.unwrap();
            }
            session.coordinator.metrics.clear();
            let started = Instant::now();
            for _ in 0..frames {
                session.step().await.unwrap();
            }
            let per = started.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            let factor = FRAME_MS / per;
            eprintln!(
                "session {} {}: {per:.2} ms/frame, realtime factor {:.2} ({:.2}x the legacy loop's cost)",
                mode.label(),
                if paced { "paced at 1x" } else { "unpaced" },
                if paced { factor } else { factor.min(1.0) },
                per / legacy_per
            );
            if !paced {
                for name in session.coordinator.metrics.names() {
                    let p = session.coordinator.metrics.percentiles(&name).unwrap();
                    eprintln!(
                        "  {name:<24} p50 {:>8.0} us  p95 {:>8.0} us  n {}",
                        p.p50_us(),
                        p.p95_us(),
                        session.coordinator.metrics.count(&name)
                    );
                }
            }
            session.stop().await;
        }
    }
}
