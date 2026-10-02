//! `LifNetwork::tick_spikes` (PERF-01): after a one-tick `step`, the list is exactly the neurons
//! that tick stamped in `last_spike_ms`, so the union of the lists over a window is the window
//! bitset a scan of `last_spike_ms` gives. GPU-02: `step_frame` gathers the same union in one call.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::visual_toy_dataset;
use flybrain_core::lif::{LifConfig, LifNetwork};
use flybrain_core::plasticity::PlasticityConfig;

#[test]
fn a_ticks_list_is_the_neurons_it_stamped_and_windows_agree_with_the_scan() {
    let config = LifConfig { noise_amount: 40.0, ..LifConfig::default() };
    let mut network =
        LifNetwork::new(Arc::new(visual_toy_dataset()), config, PlasticityConfig::default())
            .expect("a network");
    network.stimulate(200.0);
    let neurons = network.last_spike_ms.len();
    let mut spiking_ticks = 0;
    for window in 0..200 {
        let since = network.ms;
        let mut union = vec![0u8; neurons.div_ceil(8)];
        for _ in 0..(16 + window % 2) {
            let before = network.ms;
            let count = network.step(1) as usize;
            let list: BTreeSet<u32> = network
                .tick_spikes(count)
                .expect("the CPU kernel keeps its list")
                .iter()
                .copied()
                .collect();
            assert_eq!(list.len(), count, "a neuron spikes at most once a tick");
            let stamped: BTreeSet<u32> = (0..neurons as u32)
                .filter(|&n| network.last_spike_ms[n as usize] == before)
                .collect();
            assert_eq!(list, stamped);
            spiking_ticks += usize::from(count > 0);
            for n in list {
                union[n as usize >> 3] |= 1 << (n & 7);
            }
        }
        let mut scan = vec![0u8; neurons.div_ceil(8)];
        for (n, last) in network.last_spike_ms.iter().enumerate() {
            if *last >= since {
                scan[n >> 3] |= 1 << (n & 7);
            }
        }
        assert_eq!(union, scan);
    }
    assert!(spiking_ticks > 0, "the fixture must spike for the test to mean anything");
}

/// GPU-02: `step_frame` is the session's old per-frame loop -- one-tick `step` calls with each
/// tick's list OR-ed into the bitset -- bit for bit, in the state and in the bitset, at one and
/// at several sweep threads.
#[test]
fn a_frame_step_is_the_one_tick_loop_bit_for_bit() {
    use flybrain_core::lif::SweepPlan;
    let config = LifConfig { noise_amount: 40.0, ..LifConfig::default() };
    for threads in [1, 3] {
        let build = || {
            let mut network = LifNetwork::new(
                Arc::new(visual_toy_dataset()),
                config.clone(),
                PlasticityConfig::default(),
            )
            .expect("a network");
            network.configure_sweep(SweepPlan::with_threads(threads).expect("a plan"));
            network
        };
        let (mut looped, mut framed) = (build(), build());
        let neurons = looped.last_spike_ms.len();
        let mut spiking_frames = 0;
        for frame in 0..120u64 {
            if frame % 25 == 0 {
                looped.stimulate(30.0);
                framed.stimulate(30.0);
            }
            if frame % 10 == 9 {
                let ms = looped.ms;
                looped.plasticity.reinforce(0.5, ms);
                framed.plasticity.reinforce(0.5, ms);
            }
            let ticks = 16 + frame % 2;
            let mut want = vec![0u8; neurons.div_ceil(8)];
            let mut want_total = 0u64;
            for _ in 0..ticks {
                let count = looped.step(1) as usize;
                want_total += count as u64;
                for &n in looped.tick_spikes(count).expect("the CPU kernel keeps its list") {
                    want[n as usize >> 3] |= 1 << (n & 7);
                }
            }
            let mut got = vec![0u8; neurons.div_ceil(8)];
            let total = framed.step_frame(ticks, &mut got).expect("the CPU cannot fail");
            assert_eq!(total, want_total, "frame {frame} at {threads} threads");
            assert_eq!(got, want, "frame {frame} at {threads} threads");
            assert_eq!(framed.export_state(), looped.export_state());
            spiking_frames += usize::from(total > 0);
        }
        assert!(spiking_frames > 0, "the fixture must spike for the test to mean anything");
        assert!(framed.host_state_current());
        assert!(framed.take_backend_fault().is_none());
    }
}

#[test]
fn a_frame_step_refuses_a_bitset_too_small_for_the_network() {
    let mut network = LifNetwork::new(
        Arc::new(visual_toy_dataset()),
        LifConfig::default(),
        PlasticityConfig::default(),
    )
    .expect("a network");
    let neurons = network.last_spike_ms.len();
    let mut short = vec![0u8; neurons.div_ceil(8) - 1];
    assert!(network.step_frame(17, &mut short).is_err());
    assert_eq!(network.ms, 0.0, "a refused frame steps nothing");
}

/// The deployment switch: only `FLY_LIF_CUDA=1` asks for the GPU, as before GPU-02, and a device
/// that is not an ordinal is refused rather than read as 0.
#[test]
fn the_backend_switch_reads_like_the_old_env_hook() {
    use flybrain_core::lif::LifBackend;
    assert_eq!(LifBackend::from_values(None, None), Ok(LifBackend::Cpu));
    assert_eq!(LifBackend::from_values(Some("0"), Some("3")), Ok(LifBackend::Cpu));
    assert_eq!(LifBackend::from_values(Some("yes"), None), Ok(LifBackend::Cpu));
    assert_eq!(LifBackend::from_values(Some("1"), None), Ok(LifBackend::Cuda { device: 0 }));
    assert_eq!(
        LifBackend::from_values(Some("1"), Some("1")),
        Ok(LifBackend::Cuda { device: 1 })
    );
    assert!(LifBackend::from_values(Some("1"), Some("GPU-abc")).is_err());
    assert_eq!(LifBackend::Cuda { device: 1 }.label(), "cuda:1");
    assert_eq!(LifBackend::Cpu.label(), "cpu");
}

/// Without the feature, asking for the GPU is an error, never a CPU run under a GPU label; the CPU
/// backend is accepted and changes nothing.
#[cfg(not(feature = "cuda"))]
#[test]
fn a_cpu_build_refuses_the_gpu_backend() {
    use flybrain_core::lif::LifBackend;
    let mut network = LifNetwork::new(
        Arc::new(visual_toy_dataset()),
        LifConfig::default(),
        PlasticityConfig::default(),
    )
    .expect("a network");
    assert!(network.set_backend(LifBackend::Cuda { device: 0 }).is_err());
    assert!(network.set_backend(LifBackend::Cpu).is_ok());
    assert_eq!(network.backend(), LifBackend::Cpu);
    assert!(!LifBackend::cuda_compiled());
}
