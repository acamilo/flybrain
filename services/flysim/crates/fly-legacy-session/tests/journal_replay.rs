//! SERVE-01: the sugar journal a killed session-runtime service wrote, replayed.
//!
//! A `flysim-session` process restores a seed checkpoint, admits sugar through `POST /stimulate`
//! (each admission journaled, frame-stamped, before the frame that applies it), writes its hot
//! checkpoints every `hot_seconds`, and is killed with `SIGKILL`. Its successor restores the
//! newest hot checkpoint. This test takes the seed, the killed process's journal segment and that
//! hot checkpoint, and replays the segment's inputs from the seed in the **legacy** loop
//! (`flysim::frame::LegacyFrame`, FND-01's harness) up to the checkpoint's frame. The replay must
//! land on the checkpoint exactly: the network, the plasticity, the decoder, the remainder, the
//! emulator, the frame on screen, the adapter's ledger and the ratchet. That is the journal's whole
//! promise -- the seed plus the journal is the run -- and it holds across the two runtimes.
//!
//! Gated on `FLY_ROM` and `FLY_SERVE01_REPLAY_DIR`, a directory holding `seed.checkpoint`,
//! `target.checkpoint` (the hot checkpoint the successor restored) and `hot/` (the journal
//! files). `FLY_MACRO_MODE` is the run's mode (default `macros`).

use std::path::PathBuf;
use std::sync::Arc;

use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{FrameObserver, LegacyFrame, Parts};
use flysim::macros::macro_layer;
use flysim::snapshot::MacroMode;

struct Quiet;
impl FrameObserver for Quiet {}

#[test]
fn a_killed_session_runs_journal_replays_onto_its_hot_checkpoint_in_the_legacy_loop() {
    let Some(rom) = std::env::var_os("FLY_ROM").map(PathBuf::from) else {
        eprintln!("skipping: FLY_ROM is not set");
        return;
    };
    let Some(dir) = std::env::var_os("FLY_SERVE01_REPLAY_DIR").map(PathBuf::from) else {
        eprintln!("skipping: FLY_SERVE01_REPLAY_DIR is not set");
        return;
    };
    let Some(dataset) = fly_session::legacy_parity::fafb_dir() else {
        eprintln!("skipping: no data/fafb-v783");
        return;
    };
    let mode = match std::env::var("FLY_MACRO_MODE").as_deref() {
        Ok("raw") => MacroMode::Raw,
        _ => MacroMode::Macros,
    };
    let seed = flysim::store::decode(&std::fs::read(dir.join("seed.checkpoint")).unwrap())
        .expect("the seed");
    let target = flysim::store::decode(&std::fs::read(dir.join("target.checkpoint")).unwrap())
        .expect("the target");
    let segments = flysim::journal::read_segments(&dir.join("hot")).expect("the journal");
    // The killed process: the segment whose boot header names the seed's generation.
    let segment = segments
        .iter()
        .find(|s| {
            s.boot.as_ref().and_then(|b| b["generation"].as_u64())
                == Some(seed.runtime.generation)
                && s.boot.as_ref().and_then(|b| b["runtime"].as_str()) == Some("fly-session")
        })
        .expect("a fly-session segment that booted from the seed");
    let boot = segment.boot.as_ref().unwrap();
    assert_eq!(
        boot["startFrame"].as_str(),
        Some(seed.runtime.emulator_frame.to_string().as_str()),
        "the boot header starts at the seed's frame"
    );
    let inputs: Vec<(u64, f64, f64)> = segment
        .inputs
        .iter()
        .filter(|line| line["kind"] == "sugar")
        .map(|line| {
            (
                line["frame"].as_str().unwrap().parse().unwrap(),
                line["brainMs"].as_f64().unwrap(),
                line["durationMs"].as_f64().unwrap(),
            )
        })
        .collect();
    let applied: Vec<_> = inputs
        .iter()
        .filter(|(frame, _, _)| *frame < target.runtime.emulator_frame)
        .collect();
    eprintln!(
        "replaying {} of the segment's {} sugar inputs from frame {} to frame {} ({} frames); \
         {} segments in the journal",
        applied.len(),
        inputs.len(),
        seed.runtime.emulator_frame,
        target.runtime.emulator_frame,
        target.runtime.emulator_frame - seed.runtime.emulator_frame,
        segments.len()
    );
    assert!(!applied.is_empty(), "the run admitted sugar before its last hot checkpoint");

    let rom = std::fs::read(&rom).unwrap();
    let data = Arc::new(load_brain_dataset_from_dir(&dataset).expect("the dataset"));
    let mut emulator =
        Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES).expect("binjgb");
    let mut adapter = PokemonRedReward::new();
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_owned();
    config.macros.mode = mode;
    let (channels, hold_ms) = fly_legacy_session::composition::channels_and_hold(mode);
    let channels: Vec<&str> = channels.iter().map(String::as_str).collect();
    let mut agent_config =
        AgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
    agent_config.warmup_ms = config.loop_.warmup_ms;
    let mut agent = NeuralAgent::new(Arc::clone(&data), agent_config).expect("an agent");
    // The sweep's thread count does not change a number (the live loop runs 3, the harnesses 1).
    let threads = std::env::var("FLY_SERVE01_REPLAY_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3usize);
    if threads > 1 {
        agent.set_sweep_plan(
            flybrain_core::lif::SweepPlan::with_threads(threads).expect("a sweep plan"),
        );
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
            &seed,
        )
        .expect("the seed restores");
    let mut macros = macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
    if let Some(layer) = macros.as_mut() {
        let _ = layer.observe(&mut emulator, &AdapterLedger(&adapter), agent.network.ms);
    }
    let started = std::time::Instant::now();
    let mut stamped = 0;
    while frame.frame_counter < target.runtime.emulator_frame {
        for (at, brain_ms, duration) in &inputs {
            if *at == frame.frame_counter {
                assert_eq!(
                    agent.network.ms, *brain_ms,
                    "the journal's brainMs at frame {at}"
                );
                agent.network.stimulate(*duration);
                stamped += 1;
            }
        }
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: macros.as_mut(),
        };
        let transition = frame.transition(&mut parts, &mut Quiet).expect("a frame");
        frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .expect("the boundary");
    }
    assert_eq!(stamped, applied.len(), "every input before the checkpoint was applied");
    assert_eq!(frame.frame_counter, target.runtime.emulator_frame);

    let mut state = agent.export_state();
    state.remainder = frame.remainder;
    assert!(state == target.agent, "the agent state (network, plasticity, decoder, remainder)");
    assert_eq!(
        emulator.export_state().expect("the emulator"),
        target.runtime.emulator,
        "the emulator state"
    );
    assert_eq!(frame.frame_buffer, target.runtime.framebuffer, "the frame on screen");
    assert_eq!(adapter.export_state(), target.runtime.reward, "the adapter's ledger");
    assert_eq!(ratchet.state, target.runtime.ratchet, "the ratchet");
    eprintln!(
        "IDENTICAL: {} frames, {stamped} journaled sugar inputs, agent/emulator/frame/adapter/ratchet \
         equal to the killed process's hot generation {} ({:.1?})",
        target.runtime.emulator_frame - seed.runtime.emulator_frame,
        target.runtime.generation,
        started.elapsed()
    );
}
