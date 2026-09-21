//! Sixty seconds of brain time per thread count, with resident memory.
//!
//! `examples/bench.rs` reports the fastest of several short rounds, which is the right number for
//! comparing two builds. This one reports the opposite: one long unbroken run per thread count, no
//! best-of, so a cost that only shows up over a minute — a pool that stops waking promptly, a
//! buffer that grows, a working set that stops fitting — has somewhere to show up. It also prints
//! the slowest single frame, because a stream drops on the worst frame and not on the mean.
//!
//! Sixty brain-seconds is 3,591 frames of 16 or 17 ticks, the same fractional-remainder cadence
//! the agent uses in production.
//!
//! ```sh
//! RUSTFLAGS="-C target-cpu=native" cargo build --release --example soak
//! taskset -c 0,2,4,6,8,10,12,14 ./target/release/examples/soak
//! ```
//!
//! Optional arguments: a dataset directory, then a comma-separated thread-count list (0 means the
//! sequential path), then the brain seconds to run.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use flybrain_core::agent::{AgentConfig, NeuralAgent, RewardEvent, TickOptions};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::gameboy_decoder_config;
use flybrain_core::lif::SweepPlan;

const FRAME_WIDTH: usize = 160;
const FRAME_HEIGHT: usize = 144;
/// The Game Boy frame period the agent's tick accumulator produces, 16.742 ms.
const MS_PER_FRAME: f64 = 1000.0 / (4_194_304.0 / 70_224.0);

/// The test suite's frame generator: deterministic RGBA noise.
fn frame_pool(count: usize, seed: i32) -> Vec<Vec<u8>> {
    let mut state = if seed == 0 { 1 } else { seed };
    let mut random = move || {
        state ^= state << 13;
        state ^= ((state as u32) >> 17) as i32;
        state ^= state << 5;
        f64::from(state as u32) / 4_294_967_296.0
    };
    (0..count)
        .map(|_| {
            (0..FRAME_WIDTH * FRAME_HEIGHT * 4)
                .map(|_| (random() * 256.0).floor() as u8)
                .collect()
        })
        .collect()
}

/// Current and peak resident set size in MiB, from `/proc/self/status`.
///
/// `None` where that file does not exist, so the example still runs and simply omits the column.
fn resident_mib() -> Option<(f64, f64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<f64> {
        status
            .lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse::<f64>()
            .ok()
            .map(|kib| kib / 1024.0)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_dataset_dir);
    let threads: Vec<usize> = match args.next() {
        Some(list) => list
            .split(',')
            .filter_map(|value| value.trim().parse().ok())
            .collect(),
        None => vec![1, 2, 4, 6],
    };
    let brain_seconds: f64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60.0);
    let frames = (brain_seconds * 1000.0 / MS_PER_FRAME).round() as usize;

    let data = Arc::new(load_brain_dataset_from_dir(&dir)?);
    let pool = frame_pool(16, 783);
    println!(
        "soak: {brain_seconds:.0} s of brain time per thread count ({frames} frames), \
         {} neurons, {} edges",
        data.meta.neurons, data.meta.edges
    );
    if let Some((resident, _)) = resident_mib() {
        println!("resident after load: {resident:.0} MiB");
    }
    println!();
    println!("threads   brain s   wall s   brain ms/wall ms   frames/s   worst frame ms   RSS MiB   peak MiB");

    for count in threads {
        let mut agent = NeuralAgent::new(
            Arc::clone(&data),
            AgentConfig::with_decoder(gameboy_decoder_config()),
        )?;
        agent.set_sweep_plan(if count == 0 {
            SweepPlan::sequential()
        } else {
            SweepPlan::with_threads(count)?.min_parallel_neurons(4096)
        });
        agent.warmup(Some(&pool[0]))?;

        let run = Instant::now();
        let mut steps = 0u64;
        let mut worst = 0.0f64;
        for frame in 1..=frames {
            let rewards = if frame.is_multiple_of(40) {
                vec![RewardEvent::new(1.0)]
            } else {
                Vec::new()
            };
            let started = Instant::now();
            let result = agent.tick(
                &pool[frame % pool.len()],
                &TickOptions {
                    rewards: &rewards,
                    boot: true,
                    learn: true,
                },
            )?;
            worst = worst.max(started.elapsed().as_secs_f64() * 1000.0);
            steps += result.steps;
        }
        let seconds = run.elapsed().as_secs_f64();
        let (resident, peak) = resident_mib().unwrap_or((f64::NAN, f64::NAN));
        let label = if count == 0 {
            "seq".to_string()
        } else {
            count.to_string()
        };
        println!(
            "{label:>7}   {:>7.1}   {seconds:>6.1}   {:>16.3}   {:>8.1}   {worst:>14.2}   \
             {resident:>7.0}   {peak:>8.0}",
            steps as f64 / 1000.0,
            steps as f64 / (seconds * 1000.0),
            frames as f64 / seconds,
        );
    }
    Ok(())
}

/// `<repo>/data/fafb-v783`, relative to this crate.
fn default_dataset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join("data/fafb-v783")
}
