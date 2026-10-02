//! TASK-01 review N5: the session runtime's cost per frame against the legacy loop's, measured
//! A/B-interleaved on one box so both see the same load: chunks of `FLY_TASK01_AB_CHUNK` frames
//! of the legacy loop (`LegacyFrame`, the service's frame, trace off) alternate with chunks of the
//! session (agent, world and task under the coordinator, service configuration: no details,
//! bounded history, snapshots at 30 Hz), both from the same checkpoint with the same sweep threads.
//! A shared box's load then shifts both arms alike, and the ratio is the comparison.
//!
//! `FLY_TASK01_BENCH=1` plus rom-env and `data/fafb-v783`. Reported per execution mode: mean
//! ms/frame of each arm, the ratio, the fps each sustains unpaced, and the headroom at 1x (one
//! Game Boy frame is 16.74 ms).

use std::sync::Arc;
use std::time::Instant;

use fly_legacy_session::composition::{
    LegacyConfig, LegacySession, SessionArm, channels_and_hold,
};
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity;
use fly_session::types::id;
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::lif::SweepPlan;
use flybrain_gb::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{LegacyFrame, Parts};
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

const FRAME_MS: f64 = 1000.0 * 70_224.0 / 4_194_304.0;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Legacy {
    emulator: Emulator,
    adapter: PokemonRedReward,
    ratchet: Ratchet,
    agent: NeuralAgent,
    frame: LegacyFrame,
    macros: Option<MacroLayer>,
}

impl Legacy {
    fn new(
        rom: &[u8],
        dataset: &std::path::Path,
        checkpoint: &flysim::store::Checkpoint,
        threads: usize,
    ) -> Legacy {
        let data = Arc::new(load_brain_dataset_from_dir(dataset).unwrap());
        let mut emulator =
            Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES).unwrap();
        let mut adapter = PokemonRedReward::new();
        let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_owned();
        config.macros.mode = MacroMode::Macros;
        let (channels, hold_ms) = channels_and_hold(MacroMode::Macros);
        let channels: Vec<&str> = channels.iter().map(String::as_str).collect();
        let mut agent_config =
            AgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
        agent_config.warmup_ms = config.loop_.warmup_ms;
        let mut agent = NeuralAgent::new(data, agent_config).unwrap();
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
                checkpoint,
            )
            .unwrap();
        let mut macros = macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
        if let Some(layer) = macros.as_mut() {
            let _ = layer.observe(&mut emulator, &AdapterLedger(&adapter), agent.network.ms);
        }
        Legacy {
            emulator,
            adapter,
            ratchet,
            agent,
            frame,
            macros,
        }
    }

    fn step(&mut self) {
        let mut parts = Parts {
            agent: &mut self.agent,
            emulator: &mut self.emulator,
            adapter: &mut self.adapter,
            ratchet: &mut self.ratchet,
            macros: self.macros.as_mut(),
        };
        let transition = self.frame.transition(&mut parts, &mut ()).unwrap();
        self.frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .unwrap();
    }
}

/// The service's runtime: two Tokio workers (SERVE-01's `flysim-session`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_against_legacy_interleaved() {
    run(2).await;
}

/// PERF-01: the same with one Tokio worker, the coordinator, the router and every in-process
/// participant on one thread beside the brain's sweep pool. `FLY_PERF_WORKERS` picks which of
/// the two runs (default 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn session_against_legacy_interleaved_one_worker() {
    run(1).await;
}

async fn run(workers: usize) {
    if std::env::var_os("FLY_TASK01_BENCH").is_none() {
        eprintln!("skipping: FLY_TASK01_BENCH is not set");
        return;
    }
    if env_usize("FLY_PERF_WORKERS", 2) != workers {
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
    let frames = env_usize("FLY_TASK01_BENCH_FRAMES", 1800);
    let chunk = env_usize("FLY_TASK01_AB_CHUNK", 60);
    let threads = env_usize("FLY_TASK01_BENCH_THREADS", 3);
    let rom = std::fs::read(&rom_path).unwrap();
    let bytes = std::fs::read(&checkpoint_path).unwrap();
    let checkpoint = flysim::store::decode(&bytes).unwrap();
    // PERF-01: `FLY_PERF_ARMS` picks the session arms, comma-separated: `local` (in-process
    // over the local lane, the default in-process path), `memory` (in-process over the bus in
    // memory, `FLY_SESSION_LOCAL_LANE=0`, the TASK-01 path), and the BUS-01 socket arms
    // `bus/in-process`, `bus/thread`, `bus/process` (`process`). Default: local, memory, process.
    let arms = std::env::var("FLY_PERF_ARMS").unwrap_or_else(|_| "local,memory,process".to_owned());
    // `FLY_PERF_SERVICE=1` adds the service host's per-frame reads (SERVE-01's
    // `flysim-session`): the spike bitset of every commit and the audio chunk of every boundary.
    let service = std::env::var_os("FLY_PERF_SERVICE").is_some();
    for arm in arms.split(',') {
        let parsed = match arm {
            "memory" => SessionArm::LOCAL,
            other => SessionArm::parse(other).unwrap_or_else(|e| panic!("FLY_PERF_ARMS: {e}")),
        };
        let mode = parsed.mode;
        // SAFETY: set before the session's runtime reads it; nothing else reads the variable.
        unsafe {
            std::env::set_var(
                fly_legacy_session::composition::LOCAL_LANE_ENV,
                if arm == "memory" { "0" } else { "1" },
            );
        }
        let mut legacy = Legacy::new(&rom, &dataset, &checkpoint, threads);
        let root = tempfile::tempdir().unwrap();
        let mut session = LegacySession::start(
            root.path(),
            LegacyConfig {
                mode,
                transport: parsed.transport,
                placements: Default::default(),
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
        session.coordinator.disable_pacing();
        if service {
            session
                .coordinator
                .request_commit_attachments(&[fly_session::legacy_agent::SPIKES_ATTACHMENT]);
            session.coordinator.digest_views(false);
        }
        let audio_name =
            fly_session::media::audio_attachment(fly_session::legacy_env::AUDIO_STREAM_ID);
        // Warm both.
        for _ in 0..chunk {
            legacy.step();
            session.step().await.unwrap();
        }
        session.coordinator.metrics.clear();
        let _ = fly_session::profile::report();
        // The legacy brain's own phase clock, to set its ticks beside the session's
        // `agent.ticks` span.
        legacy.agent.network.profile = true;
        legacy.agent.network.reset_timings();
        let (mut legacy_ms, mut session_ms) = (0.0, 0.0);
        let mut done = 0;
        while done < frames {
            let at = Instant::now();
            for _ in 0..chunk {
                legacy.step();
            }
            legacy_ms += at.elapsed().as_secs_f64() * 1000.0;
            let at = Instant::now();
            for _ in 0..chunk {
                session.step().await.unwrap();
                if service {
                    let _ = session.coordinator.take_details();
                    let _ = session.coordinator.media_bytes(&audio_name).await.unwrap();
                }
            }
            session_ms += at.elapsed().as_secs_f64() * 1000.0;
            done += chunk;
        }
        let (l, s) = (legacy_ms / done as f64, session_ms / done as f64);
        eprintln!(
            "{} ({arm}{}, {workers} tokio workers): legacy {l:.2} ms/frame ({:.0} fps), session {s:.2} ms/frame ({:.0} fps); session/legacy {:.2}; headroom at 1x legacy {:.2} ms, session {:.2} ms ({done} frames each, {chunk}-frame chunks, {threads} threads)",
            mode.label(),
            if service { ", service reads" } else { "" },
            1000.0 / l,
            1000.0 / s,
            s / l,
            FRAME_MS - l,
            FRAME_MS - s
        );
        let timings = legacy.agent.network.timings();
        eprintln!(
            "  legacy ticks (phase clock) mean {:>8.0} us over {done} frames",
            timings.total_ns() as f64 / done as f64 / 1000.0
        );
        for (name, count, mean, max) in fly_session::profile::report_with_max() {
            eprintln!(
                "  span {name:<26} mean {:>8.0} us  max {:>8.0} us  n {count}",
                mean.as_secs_f64() * 1e6,
                max.as_secs_f64() * 1e6
            );
        }
        for name in session.coordinator.metrics.names() {
            let p = session.coordinator.metrics.percentiles(&name).unwrap();
            eprintln!(
                "  {name:<24} p50 {:>8.0} us  p95 {:>8.0} us  n {}",
                p.p50_us(),
                p.p95_us(),
                session.coordinator.metrics.count(&name)
            );
        }
        session.stop().await;
    }
}
