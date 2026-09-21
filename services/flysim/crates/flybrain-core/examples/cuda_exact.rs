//! Is the CUDA LIF tick bit-exact with the CPU kernel? One tick at a time, on the real dataset.
//!
//! Builds two identical networks from the same seeded state, steps both one millisecond at a time,
//! and after *every* tick compares the membrane and refractory arrays bit for bit, the spike list
//! element for element, and the rest of the observable state: the RNG, the clock, the reward
//! countdown, the population rate, every tracked role rate, and the whole plasticity state
//! (gains, eligibility traces and their touch stamps). The first difference stops the run and is
//! printed with its neuron index and both raw bit patterns.
//!
//! All three drive phases are exercised, because a comparison that only ran the sweep and the
//! propagation would miss exactly the repeated-index ordering the GPU has to reproduce:
//!
//!   * a new visual frame every 17 ticks, the cadence the agent uses;
//!   * a stimulation pulse every 250 ticks, so `reward_remaining` is positive for a run of ticks;
//!   * a reward every 200 ticks, which moves the plastic gains off 1.0 and puts the transposed
//!     array's gain lookup on the propagation path.
//!
//! ```sh
//! cargo build --release --features cuda --example cuda_exact
//! FLY_LIF_CUDA=0 ./target/release/examples/cuda_exact ../../data/fafb-v783 10000
//! ```
//!
//! Optional arguments: a dataset directory, then the tick count, then the CPU thread count for the
//! reference run (default 4).

use std::sync::Arc;
use std::time::Instant;

use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::{LifConfig, LifNetwork, SweepPlan};
use flybrain_core::plasticity::PlasticityConfig;

#[cfg(feature = "cuda")]
use flybrain_core::lif::cuda::CudaLif;

const FRAME_WIDTH: u32 = 160;
const FRAME_HEIGHT: u32 = 144;
const FRAME_TICKS: u64 = 17;
const STIMULATE_EVERY: u64 = 250;
const REWARD_EVERY: u64 = 200;

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
            (0..(FRAME_WIDTH * FRAME_HEIGHT * 4) as usize)
                .map(|_| (random() * 256.0).floor() as u8)
                .collect()
        })
        .collect()
}

/// A difference, with enough context to localize it in the kernel.
struct Divergence {
    tick: u64,
    what: String,
}

fn compare_f32(what: &str, tick: u64, cpu: &[f32], gpu: &[f32]) -> Option<Divergence> {
    for (index, (left, right)) in cpu.iter().zip(gpu.iter()).enumerate() {
        if left.to_bits() != right.to_bits() {
            return Some(Divergence {
                tick,
                what: format!(
                    "{what}[{index}]: cpu {left:e} (0x{:08x}) vs gpu {right:e} (0x{:08x})",
                    left.to_bits(),
                    right.to_bits()
                ),
            });
        }
    }
    None
}

fn compare_f64(what: &str, tick: u64, cpu: &[f64], gpu: &[f64]) -> Option<Divergence> {
    for (index, (left, right)) in cpu.iter().zip(gpu.iter()).enumerate() {
        if left.to_bits() != right.to_bits() {
            return Some(Divergence {
                tick,
                what: format!(
                    "{what}[{index}]: cpu {left:e} (0x{:016x}) vs gpu {right:e} (0x{:016x})",
                    left.to_bits(),
                    right.to_bits()
                ),
            });
        }
    }
    None
}

fn scalar(what: &str, tick: u64, cpu: f64, gpu: f64) -> Option<Divergence> {
    (cpu.to_bits() != gpu.to_bits()).then(|| Divergence {
        tick,
        what: format!("{what}: cpu {cpu:e} vs gpu {gpu:e}"),
    })
}

#[cfg(not(feature = "cuda"))]
fn main() {
    eprintln!("cuda_exact: build with --features cuda");
    std::process::exit(2);
}

#[cfg(feature = "cuda")]
fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .unwrap_or_else(|| "../../data/fafb-v783".to_string());
    let ticks: u64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);
    let threads: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);

    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    println!(
        "dataset {} neurons {} edges {}",
        data.meta.dataset,
        data.meta.neurons,
        data.targets.len()
    );

    let build = || {
        LifNetwork::new(
            Arc::clone(&data),
            LifConfig::default(),
            PlasticityConfig::default(),
        )
        .expect("a network")
    };
    let mut cpu = build();
    let mut gpu = build();
    cpu.set_sweep_plan(SweepPlan::with_threads(threads).expect("a pool"));

    let backend = CudaLif::new(&gpu, 0).expect("the CUDA backend must start");
    println!(
        "cuda backend up: {:.1} MiB of device memory, max {} ticks per call",
        backend.device_bytes as f64 / (1024.0 * 1024.0),
        backend.max_ticks()
    );
    gpu.attach_cuda(backend);

    let frames = frame_pool(24, 9_781);
    let started = Instant::now();
    let mut divergence: Option<Divergence> = None;
    let mut total_spikes = 0u64;
    let mut stimulated_ticks = 0u64;
    let mut visual_ticks = 0u64;

    for tick in 0..ticks {
        if tick % FRAME_TICKS == 0 {
            let frame = &frames[(tick / FRAME_TICKS) as usize % frames.len()];
            cpu.set_visual_frame(frame, FRAME_WIDTH, FRAME_HEIGHT);
            gpu.set_visual_frame(frame, FRAME_WIDTH, FRAME_HEIGHT);
        }
        if tick % STIMULATE_EVERY == 0 {
            cpu.stimulate(12.0);
            gpu.stimulate(12.0);
        }
        if tick > 0 && tick % REWARD_EVERY == 0 {
            let reward = 0.75;
            let ms = cpu.ms;
            cpu.plasticity.reinforce(reward, ms);
            let ms = gpu.ms;
            gpu.plasticity.reinforce(reward, ms);
        }

        if cpu.reward_remaining() > 0.0 {
            stimulated_ticks += 1;
        }
        if cpu.visual_drive().iter().any(|drive| *drive != 0.0) {
            visual_ticks += 1;
        }

        let cpu_spikes = cpu.step(1) as usize;
        let gpu_spikes = gpu.step(1) as usize;
        total_spikes += cpu_spikes as u64;

        if cpu_spikes != gpu_spikes {
            divergence = Some(Divergence {
                tick,
                what: format!("spike count: cpu {cpu_spikes} vs gpu {gpu_spikes}"),
            });
            break;
        }
        let cpu_list = &cpu.spike_buffer()[..cpu_spikes];
        let gpu_list = &gpu.spike_buffer()[..gpu_spikes];
        if cpu_list != gpu_list {
            let at = cpu_list
                .iter()
                .zip(gpu_list.iter())
                .position(|(left, right)| left != right)
                .unwrap_or(0);
            divergence = Some(Divergence {
                tick,
                what: format!(
                    "spike list entry {at}: cpu {} vs gpu {}",
                    cpu_list[at], gpu_list[at]
                ),
            });
            break;
        }

        let left = cpu.export_state();
        let right = gpu.export_state();
        divergence = compare_f32("membrane", tick, &left.membrane, &right.membrane)
            .or_else(|| {
                left.refractory
                    .iter()
                    .zip(right.refractory.iter())
                    .position(|(a, b)| a != b)
                    .map(|index| Divergence {
                        tick,
                        what: format!(
                            "refractory[{index}]: cpu {} vs gpu {}",
                            left.refractory[index], right.refractory[index]
                        ),
                    })
            })
            .or_else(|| {
                compare_f64(
                    "lastSpikeMs",
                    tick,
                    &left.last_spike_ms,
                    &right.last_spike_ms,
                )
            })
            .or_else(|| scalar("ms", tick, left.ms, right.ms))
            .or_else(|| {
                scalar(
                    "populationRate",
                    tick,
                    left.population_rate,
                    right.population_rate,
                )
            })
            .or_else(|| {
                scalar(
                    "rewardRemaining",
                    tick,
                    left.reward_remaining,
                    right.reward_remaining,
                )
            })
            .or_else(|| {
                (left.rng != right.rng).then(|| Divergence {
                    tick,
                    what: format!("rng: cpu {} vs gpu {}", left.rng, right.rng),
                })
            })
            .or_else(|| {
                cpu.role_names.iter().find_map(|name| {
                    scalar(
                        &format!("rate {name}"),
                        tick,
                        left.rates.get_or_zero(name),
                        right.rates.get_or_zero(name),
                    )
                })
            })
            .or_else(|| {
                compare_f32(
                    "plasticity.gains",
                    tick,
                    &left.plasticity.gains,
                    &right.plasticity.gains,
                )
            })
            .or_else(|| {
                compare_f32(
                    "plasticity.traces",
                    tick,
                    &left.plasticity.traces,
                    &right.plasticity.traces,
                )
            })
            .or_else(|| {
                compare_f64(
                    "plasticity.touched",
                    tick,
                    &left.plasticity.touched,
                    &right.plasticity.touched,
                )
            });
        if divergence.is_some() {
            break;
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    match divergence {
        None => {
            let gains = &cpu.plasticity.gains;
            let moved = gains.iter().filter(|gain| **gain != 1.0).count();
            let spread = gains
                .iter()
                .map(|gain| (f64::from(*gain) - 1.0).abs())
                .fold(0.0f64, f64::max);
            println!(
                "PASS: {ticks} ticks bit-exact, {total_spikes} spikes, {elapsed:.1} s wall \
                 (one-tick batches, both sides compared in full every tick)"
            );
            println!(
                "  coverage: {visual_ticks} ticks with visual drive, {stimulated_ticks} with \
                 stimulation, {moved}/{} plastic gains off 1.0 (max |gain - 1| {spread:.6}), so \
                 the plastic-gain lookup was on the propagation path",
                gains.len()
            );
        }
        Some(divergence) => {
            println!(
                "FAIL: first divergence at tick {} after {:.1} s",
                divergence.tick, elapsed
            );
            println!("  {}", divergence.what);
            std::process::exit(1);
        }
    }
}
