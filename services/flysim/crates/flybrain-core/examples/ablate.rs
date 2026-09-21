//! Per-phase breakdown of one 1-ms tick on `data/fafb-v783`, per thread count.
//!
//! `examples/bench.rs` reports the real-time factor; this reports where it goes. The split comes
//! from `LifNetwork::profile`, which laps an `Instant` between the five phases: at about 125 ns of
//! clock reads against a tick of 150 us and up, the instrumented total is within a tenth of a
//! percent of the uninstrumented one, and the header line prints both so a reader can check that.
//!
//! This used to infer the split by ablating one phase at a time through the configuration surface
//! and subtracting whole runs from each other. That does not work once the phases differ in cost:
//! silencing the network or removing its edges changes the spike count, which changes the sweep's
//! own cost, so the subtraction attributes the difference to the wrong phase. The `--ablate` mode
//! keeps those runs as a cross-check on the instrumentation, not as the measurement.
//!
//! Run it for the machine you are measuring, and pin it to distinct physical cores — SMT siblings
//! sharing a core make a thread-count sweep meaningless:
//!
//! ```sh
//! RUSTFLAGS="-C target-cpu=native" cargo build --release --example ablate
//! taskset -c 0,2,4,6,8,10,12,14 ./target/release/examples/ablate
//! ```
//!
//! `FLYSIM_ROUNDS` sets the rounds per configuration (default 5); the fastest is reported.
use std::sync::Arc;
use std::time::Instant;

use flybrain_core::dataset::{load_brain_dataset_from_dir, BrainDataset};
use flybrain_core::lif::{LifConfig, LifNetwork, PhaseTimings, Stimulation, SweepPlan};
use flybrain_core::plasticity::PlasticityConfig;

const TICKS: u64 = 2000;

/// Rounds per configuration; the fastest is reported. A phase difference of 20 us/tick is
/// meaningful here and a busy machine moves a single round by more than that.
fn rounds() -> usize {
    std::env::var("FLYSIM_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5)
}

fn network(data: Arc<BrainDataset>, config: LifConfig, learn: bool, threads: usize) -> LifNetwork {
    let mut network =
        LifNetwork::new(data, config, PlasticityConfig::default()).expect("a network");
    if threads > 1 {
        network.set_sweep_plan(
            SweepPlan::with_threads(threads)
                .unwrap()
                .min_parallel_neurons(4096),
        );
    }
    network.plasticity.enabled = learn;
    network.step(500); // warm the caches
    network
}

/// Fastest round's us/tick, uninstrumented.
fn wall(network: &mut LifNetwork) -> (f64, f64) {
    let mut best = f64::INFINITY;
    let mut spikes = 0u64;
    for _ in 0..rounds() {
        let start = Instant::now();
        spikes = network.step(TICKS);
        best = best.min(start.elapsed().as_secs_f64() * 1e6 / TICKS as f64);
    }
    (best, spikes as f64 / TICKS as f64)
}

/// Per-phase us/tick from the instrumented run, and its own total.
fn phases(network: &mut LifNetwork) -> PhaseTimings {
    network.profile = true;
    let mut best = f64::INFINITY;
    let mut chosen = PhaseTimings::default();
    for _ in 0..rounds() {
        network.reset_timings();
        network.step(TICKS);
        let timings = network.timings();
        let total = timings.total_ns() as f64;
        if total < best {
            best = total;
            chosen = timings;
        }
    }
    network.profile = false;
    chosen
}

fn phase_row(label: &str, network: &mut LifNetwork) {
    let (wall_us, spikes) = wall(network);
    let timings = phases(network);
    let per_tick = |value: u64| value as f64 / 1000.0 / timings.ticks as f64;
    let total = per_tick(timings.total_ns());
    println!(
        "{label:<22} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>8.1} {:>9.1} {:>7.0} {:>7.3}x",
        per_tick(timings.drive_ns),
        per_tick(timings.sweep_ns),
        per_tick(timings.observe_ns),
        per_tick(timings.propagate_ns),
        per_tick(timings.rates_ns),
        total,
        wall_us,
        spikes,
        1000.0 / wall_us,
    );
}

fn main() {
    let dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../data/fafb-v783");
    let data = Arc::new(load_brain_dataset_from_dir(&dir).unwrap());

    println!(
        "per-phase us/tick, {TICKS} ticks per round, best of {}",
        rounds()
    );
    println!(
        "{:<22} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>9} {:>7} {:>8}",
        "configuration",
        "drive",
        "sweep",
        "observe",
        "propag",
        "rates",
        "sum",
        "wall",
        "spikes",
        "x real"
    );
    for threads in [1usize, 2, 4, 6, 8] {
        let mut net = network(Arc::clone(&data), LifConfig::default(), true, threads);
        phase_row(&format!("full default, {threads}t"), &mut net);
    }
    let mut no_learn = network(Arc::clone(&data), LifConfig::default(), false, 1);
    phase_row("observe disabled, 1t", &mut no_learn);

    if std::env::args().any(|argument| argument == "--ablate") {
        println!();
        println!("cross-check: the old subtractive ablations (different dynamics, so the spike");
        println!("counts are not comparable with the rows above -- only the phase columns are)");
        let quiet = LifConfig {
            noise_kicks: 0,
            baseline_max: 0.0,
            stimulation: Stimulation {
                role: "absent".to_string(),
                drive: 0.0,
            },
            ..LifConfig::default()
        };
        let mut no_edges = (*data).clone();
        no_edges.indptr = vec![0; data.meta.neurons + 1];
        no_edges.targets.clear();
        no_edges.weights.clear();
        no_edges.meta.edges = 0;
        let no_edges = Arc::new(no_edges);
        let mut silent = network(Arc::clone(&data), quiet.clone(), true, 1);
        phase_row("silent net, 1t", &mut silent);
        let mut bare = network(Arc::clone(&no_edges), quiet, false, 1);
        phase_row("no edges, silent, 1t", &mut bare);
        for threads in [1usize, 2, 4, 8] {
            let mut driven = network(Arc::clone(&no_edges), LifConfig::default(), false, threads);
            phase_row(&format!("no edges, driven, {threads}t"), &mut driven);
        }
    }
}
