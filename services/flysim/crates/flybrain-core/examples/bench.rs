//! Real-time factor of the neural core on `data/fafb-v783`, per thread count.
//!
//! Loads the shipped connectome, warms up 2500 ms, then runs five rounds of 300 frames of noise
//! input through `NeuralAgent` and reports the fastest round's milliseconds of brain time per wall
//! millisecond. Rounds matter: a shared machine moves a single round by more than the differences
//! this table is read for.
//!
//! The default cargo profile targets x86-64-v2 for the deployment host, so build this one for the
//! machine you are measuring:
//!
//! ```sh
//! RUSTFLAGS="-C target-cpu=native" cargo run --release --example bench
//! ```
//!
//! Optional arguments: a dataset directory, then a comma-separated thread-count list. A thread
//! count of 0 means the sequential sweep, which is the baseline the pool has to beat.
//!
//! `examples/ablate.rs` breaks one tick down by phase, which is what to reach for when these
//! numbers are lower than expected.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use flybrain_core::agent::{AgentConfig, NeuralAgent, RewardEvent, TickOptions};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::gameboy::gameboy_decoder_config;
use flybrain_core::lif::SweepPlan;

const FRAME_WIDTH: usize = 160;
const FRAME_HEIGHT: usize = 144;
const WARMUP_MS: u64 = 2500;
const FRAMES: usize = 300;
/// Rounds of `FRAMES` per thread count; the fastest round is the reported figure.
const ROUNDS: usize = 5;

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
        None => vec![0, 1, 2, 4, 6, 8],
    };

    println!("dataset: {}", dir.display());
    let load = Instant::now();
    let data = Arc::new(load_brain_dataset_from_dir(&dir)?);
    println!(
        "loaded {} neurons, {} edges, {} retina columns in {:.2} s",
        data.meta.neurons,
        data.meta.edges,
        data.visual_indices.len(),
        load.elapsed().as_secs_f64()
    );

    let frames = frame_pool(16, 783);
    println!();
    println!("threads   warmup s   best s   brain ms/wall ms   frames/s");

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

        let warmup = Instant::now();
        agent.warmup(Some(&frames[0]))?;
        let warmup_seconds = warmup.elapsed().as_secs_f64();

        // `ROUNDS` rounds of `FRAMES` frames each; the fastest round is reported. A shared machine
        // moves a single round by 10% or more, which is larger than the differences being read off
        // this table, so one round is not a measurement.
        let mut total_steps = 0u64;
        let mut best_factor = 0.0f64;
        let mut best_seconds = f64::INFINITY;
        let mut frame = 0usize;
        for _ in 0..ROUNDS {
            let run = Instant::now();
            let mut steps = 0u64;
            for _ in 0..FRAMES {
                frame += 1;
                let rewards = if frame.is_multiple_of(40) {
                    vec![RewardEvent::new(1.0)]
                } else {
                    Vec::new()
                };
                let result = agent.tick(
                    &frames[frame % frames.len()],
                    &TickOptions {
                        rewards: &rewards,
                        boot: true,
                        learn: true,
                    },
                )?;
                steps += result.steps;
            }
            let seconds = run.elapsed().as_secs_f64();
            total_steps += steps;
            best_factor = best_factor.max(steps as f64 / (seconds * 1000.0));
            best_seconds = best_seconds.min(seconds);
        }
        let label = if count == 0 {
            "seq".to_string()
        } else {
            count.to_string()
        };
        println!(
            "{label:>7}   {warmup_seconds:>8.2}   {best_seconds:>5.2}   {best_factor:>16.3}   {:>8.1}",
            FRAMES as f64 / best_seconds
        );
        // Sanity: the warm-up plus the frames must have advanced the clock as expected.
        assert_eq!(agent.network.ms, WARMUP_MS as f64 + total_steps as f64);
    }
    Ok(())
}

/// `<repo>/data/fafb-v783`, relative to this crate.
fn default_dataset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join("data/fafb-v783")
}
