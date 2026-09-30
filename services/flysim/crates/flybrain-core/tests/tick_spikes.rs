//! `LifNetwork::tick_spikes` (PERF-01): after a one-tick `step`, the list is exactly the neurons
//! that tick stamped in `last_spike_ms`, so the union of the lists over a window is the window
//! bitset a scan of `last_spike_ms` gives.

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
