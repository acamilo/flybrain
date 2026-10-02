//! GPU-02: the legacy agent worker on the CUDA LIF backend, against the CPU reference, on the real
//! connectome.
//!
//! The worker steps a frame per `step_frame` call with the state left on the device, and syncs it
//! back only for `State.Capture`. This runs a FAFB scenario with every kind of step the session
//! takes -- frames with sugar, reward pulses and reinforcement, a rollback, a restore into a
//! fresh worker (and so a fresh backend fed by an imported checkpoint), full-state checkpoints --
//! through the worker on the GPU, in-process and as a process, at one and two sweep threads, and
//! requires every record (decision, telemetry, the transition's spike bitset digest, the full
//! state digest at each checkpoint) to equal `NeuralAgent` driven directly on the CPU.
//!
//! An explicit job: `--features cuda`, `FLY_GPU02_CUDA=1`, a visible device and `data/fafb-v783`.
//! `FLY_GPU02_FRAMES` sets the length (default 300 frames, about 5,000 ticks). The worker picks the
//! GPU up from `FLY_LIF_CUDA=1`, set here before any worker starts. A host that cannot attach the
//! backend now falls back to the CPU (GPU-02 notes, `cuda_fallback.rs`), so this test asserts that
//! no fallback was recorded: a pass on the CPU would mean nothing.
#![cfg(feature = "cuda")]

use std::sync::Arc;

use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity::{
    self, DirectSource, LegacyRig, LegacyScript, ReferenceSource, RewardEvent, RigAgent,
    ScriptStep, fafb_dir, frame_pool,
};
use fly_session::types::id;
use fly_session_types::gameboy::{Location, ReadoutContext};
use flybrain_core::dataset::load_brain_dataset_from_dir;

fn pokered_channels() -> Vec<String> {
    let file = fly_session_types::fixtures::load("gameboy-decoder-config.json").expect("vectors");
    file["cases"][1]["macroChannels"]
        .as_array()
        .expect("the Pokemon Red macro group")
        .iter()
        .map(|c| c.as_str().expect("a channel").to_owned())
        .collect()
}

fn script(frames: usize) -> LegacyScript {
    let channels = pokered_channels();
    let bound = |k: usize| -> Vec<String> {
        let take: &[usize] = match k % 120 {
            0..20 => &[],
            20..50 => &[0, 1, 9],
            _ => &[1, 4, 5, 6],
        };
        take.iter().map(|i| channels[*i].clone()).collect()
    };
    let rollback_at = frames * 2 / 5;
    let restore_at = frames * 7 / 10;
    let mut steps = Vec::new();
    for k in 1..=frames {
        steps.push(ScriptStep::Frame {
            sugar: if k % 97 == 12 { vec![400.0] } else { vec![] },
            frame: k % 6,
            rewards: if k % 11 == 0 {
                vec![RewardEvent {
                    value: 0.5,
                    stimulation_ms: 120.0,
                }]
            } else {
                vec![]
            },
            next_context: ReadoutContext {
                boot: k < 10,
                bound: bound(k),
                location: (k > 5).then_some(Location {
                    area: 12,
                    x: 3 + (k / 40) as u32,
                    y: 7,
                }),
            },
        });
        if k == rollback_at {
            steps.push(ScriptStep::Rollback {
                frame: 2,
                context: ReadoutContext {
                    boot: false,
                    bound: bound(k),
                    location: None,
                },
            });
        }
        if k == restore_at {
            steps.push(ScriptStep::Restore);
        }
    }
    let last = steps.len() - 1;
    let checkpoints = (0..steps.len())
        .filter(|i| i % 50 == 49)
        .chain([last])
        .collect();
    LegacyScript {
        name: "gpu02-fafb".to_owned(),
        macro_channels: channels,
        frames: Arc::new(frame_pool(6, 783)),
        initial_frame: 0,
        initial_context: ReadoutContext {
            boot: true,
            bound: vec![],
            location: None,
        },
        steps,
        checkpoints,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gpu_worker_matches_the_cpu_reference_on_fafb() {
    if std::env::var_os("FLY_GPU02_CUDA").is_none() {
        eprintln!("skipping: FLY_GPU02_CUDA is not set");
        return;
    }
    let Some(dir) = fafb_dir() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let frames = std::env::var("FLY_GPU02_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let script = script(frames);
    let dataset = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset loads"));
    let started = std::time::Instant::now();
    let want = DirectSource { dataset }
        .records(&script)
        .expect("the CPU reference runs");
    eprintln!(
        "cpu reference: {} records in {:?}, digest {}",
        want.len(),
        started.elapsed(),
        legacy_parity::golden_json(&script, &want)["digest"]
    );
    // SAFETY: the only test in this binary, and no worker (thread or child) exists yet; every
    // worker started from here on reads the switch through `LifBackend::from_env`.
    unsafe { std::env::set_var("FLY_LIF_CUDA", "1") };
    for (mode, threads) in [
        (ExecutionMode::InProcess, 1),
        (ExecutionMode::InProcess, 2),
        (ExecutionMode::Process, 2),
    ] {
        let started = std::time::Instant::now();
        let root = tempfile::tempdir().expect("tmp");
        let agent = RigAgent {
            agent_id: id("fly-a"),
            port_id: id("p1"),
            dataset_dir: dir.clone(),
            profile: LegacyProfileKind::Production,
            macro_channels: script.macro_channels.clone(),
            worker_threads: threads,
        };
        let mut rig = LegacyRig::start(root.path(), mode, &[agent])
            .await
            .expect("the rig");
        let got = legacy_parity::run_on_worker(&mut rig, &id("fly-a"), &script)
            .await
            .unwrap_or_else(|e| panic!("{} x{threads}: {e}", mode.label()));
        assert_eq!(
            rig.gpu_fallbacks(),
            0,
            "{} x{threads}: the GPU did not attach, so this ran on the CPU",
            mode.label()
        );
        rig.stop().await;
        legacy_parity::compare(&want, &got)
            .unwrap_or_else(|e| panic!("{} x{threads}: {e}", mode.label()));
        eprintln!(
            "gpu worker {} x{threads}: {} records identical to the CPU in {:?}",
            mode.label(),
            got.len(),
            started.elapsed()
        );
    }
}
