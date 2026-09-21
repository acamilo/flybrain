//! Ticks per second of the CUDA LIF backend, against the CPU kernel on the same box.
//!
//! Steps the network in fixed batches with the drive phases live (a new visual frame every 17
//! ticks, a stimulation pulse every 250) and reports ticks per second, the real-time factor that
//! implies at 1 kHz, and the GPU time per tick from CUDA events. Both backends run the same
//! sequence, so the only difference is where the tick executes.
//!
//! `nsys` is not installed on the container, so the kernel figure is `cudaEventElapsedTime` around
//! each synchronised batch: it covers the whole batch's kernels including the launch gaps, which
//! is the number a sim loop actually pays.
//!
//! ```sh
//! cargo build --release --features cuda --example cuda_bench
//! ./target/release/examples/cuda_bench ../../data/fafb-v783 20000 4
//! ```
//!
//! Optional arguments: a dataset directory, the ticks per measured run, the CPU thread count.

use std::sync::Arc;
use std::time::Instant;

use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::{LifConfig, LifNetwork, SweepPlan};
use flybrain_core::plasticity::PlasticityConfig;

#[cfg(feature = "cuda")]
use flybrain_core::lif::cuda::{CudaLif, PHASE_NAMES};

const FRAME_WIDTH: u32 = 160;
const FRAME_HEIGHT: u32 = 144;
const FRAME_TICKS: u64 = 17;
const STIMULATE_EVERY: u64 = 250;
const WARMUP_TICKS: u64 = 2_500;

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

/// Step `ticks` milliseconds in `batch`-tick calls, driving the retina and the reward the way the
/// agent loop does. Returns wall seconds.
fn run(network: &mut LifNetwork, ticks: u64, batch: u64, frames: &[Vec<u8>]) -> f64 {
    let started = Instant::now();
    let mut done = 0u64;
    let mut frame = 0usize;
    while done < ticks {
        let now = done;
        if now.is_multiple_of(FRAME_TICKS) {
            network.set_visual_frame(&frames[frame % frames.len()], FRAME_WIDTH, FRAME_HEIGHT);
            frame += 1;
        }
        if now.is_multiple_of(STIMULATE_EVERY) {
            network.stimulate(12.0);
        }
        let step = batch.min(ticks - done);
        network.step(step);
        done += step;
    }
    started.elapsed().as_secs_f64()
}

#[cfg(not(feature = "cuda"))]
fn main() {
    eprintln!("cuda_bench: build with --features cuda");
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
        .unwrap_or(20_000);
    let threads: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);

    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    let frames = frame_pool(24, 9_781);
    println!(
        "dataset {} neurons {} edges {}, {ticks} ticks per run",
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

    println!();
    println!("backend           batch   ticks/s   realtime   kernel us/tick");

    for count in [1usize, threads] {
        let mut network = build();
        if count > 1 {
            network.set_sweep_plan(SweepPlan::with_threads(count).expect("a pool"));
        }
        run(&mut network, WARMUP_TICKS, 17, &frames);
        let seconds = run(&mut network, ticks, 17, &frames);
        let rate = ticks as f64 / seconds;
        println!(
            "cpu {count:>2} thread{}   17   {rate:8.0}   {:7.3}x   {:>14}",
            if count == 1 { " " } else { "s" },
            rate / 1000.0,
            "-"
        );
    }

    for batch in [1usize, 17, 32] {
        let mut network = build();
        let mut backend = CudaLif::with_max_ticks(&network, 0, batch).expect("the CUDA backend");
        backend.sync_host_each_batch = false;
        let bytes = backend.device_bytes;
        network.attach_cuda(backend);
        run(&mut network, WARMUP_TICKS, batch as u64, &frames);
        if let Some(backend) = network.cuda_mut() {
            backend.kernel_ms = 0.0;
            backend.kernel_ticks = 0;
        }
        let seconds = run(&mut network, ticks, batch as u64, &frames);
        let rate = ticks as f64 / seconds;
        let backend = network.cuda().expect("attached");
        let kernel_us = if backend.kernel_ticks > 0 {
            backend.kernel_ms * 1000.0 / backend.kernel_ticks as f64
        } else {
            f64::NAN
        };
        println!(
            "cuda            {batch:>5}   {rate:8.0}   {:7.3}x   {kernel_us:14.1}",
            rate / 1000.0
        );
        if batch == 32 {
            println!();
            println!(
                "device memory allocated by the backend: {:.1} MiB",
                bytes as f64 / (1024.0 * 1024.0)
            );
        }
    }

    // How many edges a tick actually touches, which is the whole pull-versus-push argument: the
    // CPU push and the bucket kernels walk only the rows of spiking neurons, a pull over the
    // transposed CSR walks all of them.
    {
        let mut network = build();
        network.set_sweep_plan(SweepPlan::with_threads(threads).expect("a pool"));
        run(&mut network, WARMUP_TICKS, 17, &frames);
        let mut spiking = 0u64;
        let mut touched = 0u64;
        let samples = 200u64;
        for _ in 0..samples {
            let count = network.step(1) as usize;
            spiking += count as u64;
            for source in &network.spike_buffer()[..count] {
                let source = *source as usize;
                touched += u64::from(data.indptr[source + 1] - data.indptr[source]);
            }
        }
        println!();
        println!(
            "per tick, averaged over {samples} ticks: {:.0} spikes ({:.3} % of neurons), {:.0} \
             outgoing edges touched, {:.1} % of the {} in the connectome",
            spiking as f64 / samples as f64,
            100.0 * spiking as f64 / samples as f64 / data.meta.neurons as f64,
            touched as f64 / samples as f64,
            100.0 * touched as f64 / samples as f64 / data.targets.len() as f64,
            data.targets.len()
        );
    }

    // Where the GPU time goes, one tick per batch so each kernel can be bracketed on its own.
    let phase_ticks = 2_000u64;
    let mut network = build();
    let mut backend = CudaLif::with_max_ticks(&network, 0, 1).expect("the CUDA backend");
    backend.sync_host_each_batch = false;
    network.attach_cuda(backend);
    run(&mut network, WARMUP_TICKS, 1, &frames);
    if let Some(backend) = network.cuda_mut() {
        backend.profile_phases = true;
        backend.phase_ms = Default::default();
        backend.kernel_ms = 0.0;
        backend.kernel_ticks = 0;
    }
    run(&mut network, phase_ticks, 1, &frames);
    let backend = network.cuda().expect("attached");
    println!();
    println!("per-kernel GPU time over {phase_ticks} ticks, us/tick:");
    for (name, ms) in PHASE_NAMES.iter().zip(backend.phase_ms.iter()) {
        println!("  {name:<14} {:8.1}", ms * 1000.0 / phase_ticks as f64);
    }
    println!(
        "  {:<14} {:8.1}",
        "total",
        backend.phase_ms.iter().sum::<f64>() * 1000.0 / phase_ticks as f64
    );
}
