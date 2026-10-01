//! PERF-02: pinned sweep workers compute what floating ones do.
//!
//! `pool::place_workers` confines the calling thread to the host CPUs and pins every worker of the
//! pools created afterwards to a CPU of its own. It is process-wide and once only, so this file
//! is its own test binary with a single test. On the committed `data/fafb-v783` it runs a pinned
//! 4-worker network against the sequential one, spikes and full exported state (plastic traces
//! included), and checks the workers really run where the placement says.
//!
//! Skips with a message when the dataset is absent or the process has fewer than four CPUs.

mod common;

use std::sync::Arc;

use common::{frame_pool, real_dataset_dir};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::{LifConfig, LifNetwork, SweepPlan};
use flybrain_core::plasticity::PlasticityConfig;
use flybrain_core::pool::{WorkerPool, place_workers};

#[test]
fn a_pinned_pool_reproduces_the_sequential_run() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    // The placement must come before any pool exists.
    let Some(placement) = place_workers(4) else {
        eprintln!("skipped: fewer than four cpus to place four workers on");
        return;
    };
    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    let frames = frame_pool(1, 99, 160, 144);

    let run = |sweep: SweepPlan| {
        let mut network = LifNetwork::new(
            Arc::clone(&data),
            LifConfig::default(),
            PlasticityConfig::default(),
        )
        .expect("a default network");
        network.set_sweep_plan(sweep);
        network.set_visual_frame(&frames[0], 160, 144);
        let mut spikes = Vec::new();
        for _ in 0..60 {
            spikes.push(network.step(1));
        }
        network.stimulate(120.0);
        for _ in 0..60 {
            spikes.push(network.step(1));
        }
        let ms = network.ms;
        network.plasticity.reinforce(1.0, ms);
        // A gap of milliseconds between steps, as a host's transition work makes, so the workers
        // park and wake on their pinned cpus as they do in the service.
        std::thread::sleep(std::time::Duration::from_millis(20));
        for _ in 0..40 {
            spikes.push(network.step(1));
        }
        (spikes, network.export_state())
    };

    let reference = run(SweepPlan::sequential());
    assert!(reference.0.iter().sum::<u64>() > 0);

    #[cfg(target_os = "linux")]
    {
        // The spawned workers run on the placement's cpus, one each. A worker pins itself as it
        // starts, so the check follows a dispatch, which every worker has run.
        let pool = WorkerPool::new(4, "flysim-sweep").expect("a pool");
        pool.broadcast(&|_| {});
        let mut seen = Vec::new();
        for task in std::fs::read_dir("/proc/self/task").expect("the task list") {
            let task = task.expect("a task").path();
            let comm = std::fs::read_to_string(task.join("comm")).unwrap_or_default();
            if !comm.starts_with("flysim-sweep-") {
                continue;
            }
            let status = std::fs::read_to_string(task.join("status")).expect("the task status");
            let list = status
                .lines()
                .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
                .expect("Cpus_allowed_list")
                .trim()
                .to_owned();
            seen.push(list);
        }
        seen.sort();
        let mut want: Vec<String> = placement.workers.iter().map(usize::to_string).collect();
        want.sort();
        assert_eq!(
            seen, want,
            "the sweep workers are not on the placement's cpus"
        );
    }
    let got = run(SweepPlan::with_threads(4).expect("a pool"));
    assert_eq!(
        got.0, reference.0,
        "spike counts differ with pinned workers"
    );
    assert_eq!(got.1, reference.1, "state differs with pinned workers");
}
