//! GPU-02 notes round (N2, operator decision 2026-10-02): a GPU failure on the live brain falls
//! back to the CPU and the session keeps going from the last checkpoint, with no loop re-attaching
//! a dead device.
//!
//! The fault-injection hook (`flybrain_core::lif::cuda::inject`, or `FLY_LIF_CUDA_FAULT` for a
//! worker process) raises a CUDA error where a real one would surface. Against the CPU reference
//! on the real connectome:
//!
//! - **Permanent, mid-run** (the device held the only current state): the agent fails, a
//!   replacement worker restores the last checkpoint on the CPU and the records are those of the CPU
//!   reference with a restore at that checkpoint -- in-process, and as a process, where the
//!   replacement is a new process that must not re-attach the GPU (no second failure).
//! - **Init failure** (a lost device at a restart): `Agent.Initialize` succeeds on the CPU, with
//!   a warning and a counted fallback, and a restore into a replacement stays on the CPU.
//! - **Transient** (the host state was current: the first batch): the frame finishes on the CPU in
//!   the call, bit-exact, and no recovery is needed; the fallback is counted and the GPU is not
//!   re-attached by a later restore.
//!
//! Needs `--features cuda` and `data/fafb-v783`. The cases that run a real device also need
//! `FLY_GPU02_CUDA=1` and a visible card; the init-failure case needs neither.
#![cfg(feature = "cuda")]

use std::sync::Arc;

use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity::{
    self, DirectSource, LegacyRig, LegacyScript, ParityRecord, ReferenceSource, RewardEvent,
    RigAgent, ScriptStep, fafb_dir, frame_pool,
};
use fly_session::types::id;
use fly_session_types::gameboy::{Location, ReadoutContext};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::cuda::inject;
use flybrain_core::lif::health;

fn pokered_channels() -> Vec<String> {
    let file = fly_session_types::fixtures::load("gameboy-decoder-config.json").expect("vectors");
    file["cases"][1]["macroChannels"]
        .as_array()
        .expect("the Pokemon Red macro group")
        .iter()
        .map(|c| c.as_str().expect("a channel").to_owned())
        .collect()
}

/// Frames with sugar and reward pulses, a checkpoint every 25 steps, an optional restore.
fn script(frames: usize, restore_at: Option<usize>) -> LegacyScript {
    let channels = pokered_channels();
    let bound = |k: usize| -> Vec<String> {
        let take: &[usize] = match k % 120 {
            0..20 => &[],
            20..50 => &[0, 1, 9],
            _ => &[1, 4, 5, 6],
        };
        take.iter().map(|i| channels[*i].clone()).collect()
    };
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
        if restore_at == Some(k) {
            steps.push(ScriptStep::Restore);
        }
    }
    let last = steps.len() - 1;
    let checkpoints = (0..steps.len())
        .filter(|i| i % 25 == 24)
        .chain([last])
        .collect();
    LegacyScript {
        name: "gpu02-fallback".to_owned(),
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

fn agent(dir: &std::path::Path, script: &LegacyScript) -> RigAgent {
    RigAgent {
        agent_id: id("fly-a"),
        port_id: id("p1"),
        dataset_dir: dir.to_path_buf(),
        profile: LegacyProfileKind::Production,
        macro_channels: script.macro_channels.clone(),
        worker_threads: 1,
    }
}

fn reset() {
    inject::clear();
    health::reset_for_tests();
}

struct Outcome {
    records: Vec<ParityRecord>,
    recovered: Vec<usize>,
    fallbacks: u64,
}

async fn run(dir: &std::path::Path, script: &LegacyScript, mode: ExecutionMode) -> Outcome {
    let root = tempfile::tempdir().expect("tmp");
    let mut rig = LegacyRig::start(root.path(), mode, &[agent(dir, script)])
        .await
        .expect("the rig");
    let (records, recovered) =
        legacy_parity::run_on_worker_recovering(&mut rig, &id("fly-a"), script)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
    let fallbacks = rig.gpu_fallbacks();
    rig.stop().await;
    Outcome {
        records,
        recovered,
        fallbacks,
    }
}

fn reference(dir: &std::path::Path, script: &LegacyScript) -> Vec<ParityRecord> {
    let dataset = Arc::new(load_brain_dataset_from_dir(dir).expect("the dataset loads"));
    DirectSource { dataset }
        .records(script)
        .expect("the CPU reference runs")
}

/// The records of the CPU reference with a restore after step `at`, minus that restore's own
/// record (the recovery has none), against what the recovered run recorded.
fn assert_recovered_equals_reference(
    dir: &std::path::Path,
    script: &LegacyScript,
    at: usize,
    got: &[ParityRecord],
    case: &str,
) {
    let alt = legacy_parity::script_with_restore_after(script, at);
    let mut want = reference(dir, &alt);
    want.remove(at + 2); // records[0] is the first record, step i is records[i + 1]
    assert_eq!(want.len(), got.len(), "{case}: record count");
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        let (mut w, mut g) = (w.clone(), g.clone());
        w.index = 0;
        g.index = 0;
        assert_eq!(w, g, "{case}: record {i} differs from the CPU reference");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gpu_failure_falls_back_to_the_cpu_and_the_session_continues() {
    let Some(dir) = fafb_dir() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let device = std::env::var_os("FLY_GPU02_CUDA").is_some();
    let frames = 120;
    // SAFETY: the only test in this binary; no worker (thread or child) exists yet.
    unsafe { std::env::set_var("FLY_LIF_CUDA", "1") };
    let plain = script(frames, None);

    // Init failure: a restart whose CUDA init fails. No device needed.
    reset();
    inject::fail_attach(true);
    let with_restore = script(frames, Some(60));
    let got = run(&dir, &with_restore, ExecutionMode::InProcess).await;
    assert!(got.recovered.is_empty(), "nothing failed mid-run");
    assert_eq!(got.fallbacks, 1, "one failed attach; the restore did not try the GPU again");
    legacy_parity::compare(&reference(&dir, &with_restore), &got.records)
        .unwrap_or_else(|e| panic!("init failure: {e}"));
    assert!(health::gpu_unusable());
    eprintln!("init failure: continued on the CPU, {} records identical", got.records.len());

    if !device {
        eprintln!("skipping the device cases: FLY_GPU02_CUDA is not set");
        reset();
        return;
    }

    // Control: the GPU, no fault, no fallback.
    reset();
    let started = inject::batches();
    let got = run(&dir, &plain, ExecutionMode::InProcess).await;
    // A game frame is one batch; the warm-up before the first frame is many. The faults below
    // go off `frames - 50` batches from the run's end: mid-run, never in the warm-up.
    let run_batches = inject::batches() - started;
    assert!(run_batches > frames as u64);
    assert_eq!(got.fallbacks, 0, "the GPU attached");
    legacy_parity::compare(&reference(&dir, &plain), &got.records)
        .unwrap_or_else(|e| panic!("control: {e}"));

    // Transient: the first batch (the warm-up) fails with the host state current. The frame
    // finishes on the CPU in the call: no recovery, bit-exact, and the GPU stays out.
    reset();
    inject::fail_batch_after(0);
    let got = run(&dir, &with_restore, ExecutionMode::InProcess).await;
    assert!(got.recovered.is_empty(), "the call finished on the CPU");
    assert_eq!(got.fallbacks, 1, "counted once; the restore did not re-attach the GPU");
    legacy_parity::compare(&reference(&dir, &with_restore), &got.records)
        .unwrap_or_else(|e| panic!("transient: {e}"));
    eprintln!("transient: finished on the CPU in place, {} records identical", got.records.len());

    // Permanent, mid-run, in-process: the device held the only current state.
    reset();
    inject::fail_batch_after(run_batches - 50);
    let got = run(&dir, &plain, ExecutionMode::InProcess).await;
    assert_eq!(got.recovered.len(), 1, "one recovery, no loop");
    assert_eq!(got.fallbacks, 1);
    assert_recovered_equals_reference(&dir, &plain, got.recovered[0], &got.records, "permanent in-process");
    eprintln!(
        "permanent in-process: recovered from the checkpoint after step {}, {} records identical",
        got.recovered[0],
        got.records.len()
    );

    // Permanent, mid-run, as a process: the replacement is a new process, which must read the
    // failure from the launcher's marker. With the fault armed in every process at the same
    // batch, a replacement that re-attached the GPU would fail again about 50 frames after the
    // restore (its warm-up is the same length, then 50 frames to the same batch).
    reset();
    unsafe { std::env::set_var("FLY_LIF_CUDA_FAULT", format!("batch={}", run_batches - 50)) };
    let got = run(&dir, &plain, ExecutionMode::Process).await;
    unsafe { std::env::remove_var("FLY_LIF_CUDA_FAULT") };
    assert_eq!(got.recovered.len(), 1, "the replacement process stayed on the CPU");
    assert_eq!(got.fallbacks, 1);
    assert_recovered_equals_reference(&dir, &plain, got.recovered[0], &got.records, "permanent process");
    eprintln!(
        "permanent process: recovered from the checkpoint after step {}, {} records identical",
        got.recovered[0],
        got.records.len()
    );
    reset();
}
