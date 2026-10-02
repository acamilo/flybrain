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
//! reference run (default 4), then the mode: `tick` (the default, above) or `frame`.
//!
//! `frame` is the session runtime's shape since GPU-02: both sides step a game frame (16 or 17
//! ticks) per [`LifNetwork::step_frame`] call, the GPU with device-owned state, so `membrane` and
//! `refractory` stay on the card. After every frame the frame's spike bitset and everything the
//! host keeps (the spike stamps, the RNG, the clock, the rates, the plasticity) are compared;
//! `membrane` and `refractory` only at the checkpoints the session would take: every 60 frames
//! through `export_state` while the host copy is stale (the download path), and every 300 frames
//! after `sync_host` (the capture path), plus a checkpoint round trip (the GPU network imports the
//! CPU's exported state halfway). The device-owned run's PCIe traffic per frame is printed.
//! `FLY_LIF_CUDA_DEVICE` picks the card in both modes (default 0).

use std::sync::Arc;
use std::time::Instant;

use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::{LifBackend, LifConfig, LifNetwork, LifState, SweepPlan};
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


/// Every field of two exported states, in a fixed order; the first difference.
fn compare_states(
    tick: u64,
    role_names: &[String],
    left: &LifState,
    right: &LifState,
) -> Option<Divergence> {
    compare_f32("membrane", tick, &left.membrane, &right.membrane)
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
            role_names.iter().find_map(|name| {
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
    let mode = args.next().unwrap_or_else(|| "tick".to_string());
    let device = match LifBackend::from_values(
        Some("1"),
        std::env::var("FLY_LIF_CUDA_DEVICE").ok().as_deref(),
    ) {
        Ok(LifBackend::Cuda { device }) => device,
        other => panic!("FLY_LIF_CUDA_DEVICE: {other:?}"),
    };

    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    println!(
        "dataset {} neurons {} edges {}",
        data.meta.dataset,
        data.meta.neurons,
        data.targets.len()
    );
    match mode.as_str() {
        "tick" => {}
        "frame" => return frame_mode(&data, ticks, threads, device),
        other => panic!("mode {other}: expected tick or frame"),
    }

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

    let backend = CudaLif::new(&gpu, device).expect("the CUDA backend must start");
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
        divergence = compare_states(tick, &cpu.role_names, &left, &right);
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

/// The session's shape: a frame per call, device-owned state, full state only at checkpoints.
#[cfg(feature = "cuda")]
fn frame_mode(data: &Arc<flybrain_core::dataset::BrainDataset>, ticks: u64, threads: usize, device: usize) {
    const EXPORT_EVERY: u64 = 60;
    const SYNC_EVERY: u64 = 300;
    let build = || {
        LifNetwork::new(
            Arc::clone(data),
            LifConfig::default(),
            PlasticityConfig::default(),
        )
        .expect("a network")
    };
    let mut cpu = build();
    let mut gpu = build();
    cpu.configure_sweep(SweepPlan::with_threads(threads).expect("a pool"));
    gpu.set_backend(LifBackend::Cuda { device })
        .expect("the CUDA backend must start");
    println!("frame mode: gpu backend {}, device-owned state", gpu.backend().label());

    let frames = frame_pool(24, 9_781);
    let neurons = data.meta.neurons;
    let count = ticks.div_ceil(FRAME_TICKS);
    let started = Instant::now();
    let mut divergence: Option<Divergence> = None;
    let mut total_spikes = 0u64;
    let mut gpu_ns = 0u128;
    let mut cpu_ns = 0u128;
    let mut tick = 0u64;
    let mut exports = 0u64;
    let mut syncs = 0u64;
    for frame in 0..count {
        let image = &frames[frame as usize % frames.len()];
        cpu.set_visual_frame(image, FRAME_WIDTH, FRAME_HEIGHT);
        gpu.set_visual_frame(image, FRAME_WIDTH, FRAME_HEIGHT);
        if frame % (STIMULATE_EVERY / FRAME_TICKS) == 0 {
            cpu.stimulate(12.0);
            gpu.stimulate(12.0);
        }
        if frame > 0 && frame % (REWARD_EVERY / FRAME_TICKS) == 0 {
            let ms = cpu.ms;
            cpu.plasticity.reinforce(0.75, ms);
            gpu.plasticity.reinforce(0.75, ms);
        }
        if frame == count / 2 {
            // A checkpoint carried from the CPU into the GPU network mid-run.
            let state = cpu.export_state();
            gpu.import_state(&state).expect("the CPU's state imports");
        }
        // The session's tick count alternates 16 and 17 (548625/32768 ms per frame).
        let step = if frame % 2 == 0 { FRAME_TICKS } else { FRAME_TICKS - 1 };
        let mut want = vec![0u8; neurons.div_ceil(8)];
        let mut got = vec![0u8; neurons.div_ceil(8)];
        let at = Instant::now();
        let cpu_spikes = cpu.step_frame(step, &mut want).expect("the CPU");
        cpu_ns += at.elapsed().as_nanos();
        let at = Instant::now();
        let gpu_spikes = gpu.step_frame(step, &mut got).expect("the GPU frame");
        gpu_ns += at.elapsed().as_nanos();
        if let Some(fault) = gpu.take_backend_fault() {
            panic!("the GPU backend detached itself: {fault}");
        }
        total_spikes += cpu_spikes;
        tick += step;
        if cpu_spikes != gpu_spikes || want != got {
            let at = want.iter().zip(&got).position(|(a, b)| a != b);
            divergence = Some(Divergence {
                tick,
                what: format!(
                    "frame {frame}: spikes cpu {cpu_spikes} vs gpu {gpu_spikes}, first bitset byte \
                     differing {at:?}"
                ),
            });
            break;
        }
        // What the host keeps, every frame, without touching the device's arrays.
        let left = cpu.export_state();
        let host = {
            let mut copy = left.clone();
            copy.last_spike_ms = gpu.last_spike_ms.clone();
            copy.ms = gpu.ms;
            copy.population_rate = gpu.population_rate;
            copy.reward_remaining = gpu.reward_remaining();
            copy.rng = gpu.rng_state();
            copy.rates = gpu.rates.clone();
            copy.plasticity = gpu.plasticity.export_state();
            copy.visual_drive = gpu.visual_drive().to_vec();
            copy
        };
        divergence = compare_states(tick, &cpu.role_names, &left, &host);
        if divergence.is_some() {
            break;
        }
        if frame % EXPORT_EVERY == EXPORT_EVERY - 1 {
            assert!(!gpu.host_state_current(), "device-owned: the host lags between syncs");
            divergence = compare_states(tick, &cpu.role_names, &left, &gpu.export_state());
            exports += 1;
        }
        if divergence.is_none() && frame % SYNC_EVERY == SYNC_EVERY - 1 {
            gpu.sync_host().expect("the capture sync");
            assert!(gpu.host_state_current());
            divergence = compare_f32("membrane (synced)", tick, &cpu.membrane, &gpu.membrane)
                .or_else(|| {
                    (cpu.refractory != gpu.refractory).then(|| Divergence {
                        tick,
                        what: "refractory (synced) differs".to_string(),
                    })
                });
            syncs += 1;
        }
        if divergence.is_some() {
            break;
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let backend = gpu.cuda().expect("still attached");
    let (up, down) = (backend.uploaded_bytes, backend.downloaded_bytes);
    match divergence {
        None => {
            println!(
                "PASS: {count} frames ({tick} ticks) bit-exact, {total_spikes} spikes, {elapsed:.1} s \
                 wall (frame per call, device-owned state; host state every frame, {exports} \
                 exports through the device download, {syncs} host syncs, one CPU->GPU import)"
            );
            println!(
                "  step_frame: cpu ({threads} threads) {:.3} ms/frame, gpu {:.3} ms/frame; PCIe \
                 {:.1} KB/frame up, {:.1} KB/frame down (syncs and the import included)",
                cpu_ns as f64 / 1e6 / count as f64,
                gpu_ns as f64 / 1e6 / count as f64,
                up as f64 / 1024.0 / count as f64,
                down as f64 / 1024.0 / count as f64,
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
