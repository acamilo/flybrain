//! The per-millisecond LIF tick on the GPU, bit-exact with the CPU kernel `lif-1ms-f64-v2`.
//!
//! [`CudaLif`] runs steps 1-4 and 6 of [`super::LifNetwork::step_one`] — the noise kicks, the
//! visual drive, the stimulation drive, the integrate-and-fire sweep and spike propagation — for a
//! whole batch of ticks in one call, and hands back one ascending spike list per tick. Everything
//! else stays on the host, unchanged, replayed per tick against those lists: `plasticity.observe`,
//! the `last_spike_ms` stamps, the role tally, the rate EMAs and the clock.
//!
//! # Why this can be bit-exact
//!
//! The per-tick arithmetic is pure IEEE `f64` add and multiply with one `f32` rounding at each
//! store, and the only transcendental (`exp(-1/decayMs)`) is evaluated once on the host at config
//! time. So the two things that normally make a GPU port inexact do not apply, provided:
//!
//! 1. **No contraction.** `cuda/lif.cu` is compiled with `--fmad=false` and no fast math, so every
//!    multiply and add rounds separately, as Rust's do. `cuda/build-ptx.sh` checks the emitted PTX
//!    contains no `fma.` instruction at all.
//! 2. **Summation order.** Propagation pushes each spiking source's row into per-target buckets
//!    and then gives each target one thread, which sorts its own bucket on the base edge index and
//!    sums it sequentially. That index order *is* the CPU's order, because the sequential kernel
//!    walks spiking sources ascending and each row ascending and the base edge index is
//!    source-major then slot. A target's membrane depends on nothing but its own previous value
//!    and its own sequence of addends, so the f32 rounding and the per-edge floor clamp land at the
//!    same points with the same operands. Bucket placement uses atomics and is therefore
//!    arbitrary, which is exactly what the sort undoes; no value is ever reduced across threads.
//! 3. **Repeated indices.** The 300 noise draws can repeat, and repeats are 300 sequential f32
//!    stores on the CPU. The host groups the draws by neuron and one thread applies a neuron's
//!    kicks in draw order, which keeps the sequence and makes distinct neurons independent. The
//!    visual columns and the stimulation targets get the same treatment, so a dataset that lists a
//!    neuron twice is handled rather than silently reordered.
//! 4. **Spike order.** The list is built by a ballot prefix inside each block plus a scan of the
//!    per-block counts, so it is ascending by neuron index — the order `observe`, the role tally
//!    and propagation all assume.
//!
//! The RNG stays on the host: the draws depend on nothing the GPU computes, so a whole batch of
//! them is drawn up front and uploaded once.
//!
//! # State ownership
//!
//! Between batches the device owns `membrane` and `refractory`. With
//! [`CudaLif::sync_host_each_batch`] set (the default) they are copied back at the end of every
//! batch, which keeps `export_state`, the status endpoints and every existing test correct with no
//! further changes. Clear it for throughput work and call [`CudaLif::sync_to_host`] explicitly
//! before a checkpoint.

use std::sync::Arc;

use cudarc::driver::sys::CUevent_flags;
use cudarc::driver::{
    CudaContext, CudaEvent, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::Ptx;

use crate::error::{Error, Result};

use super::LifNetwork;

/// Threads per block, and therefore neurons per block in the sweep and the compaction. Must match
/// `LIF_BLOCK` in `cuda/lif.cu`, because the spike bitset words are per-warp ballots.
const BLOCK: u32 = 256;

/// Threads in the single-block scan of the per-block spike counts.
const SCAN_THREADS: u32 = 1024;

/// Kernel groups [`CudaLif::profile_phases`] times separately.
pub const PHASES: usize = 12;

/// Names of the [`PHASES`] timed kernel groups, in launch order.
pub const PHASE_NAMES: [&str; PHASES] = [
    "noise",
    "visual",
    "stimulate",
    "sweep",
    "spike scan",
    "spike compact",
    "bucket count",
    "bucket scan",
    "bucket exscan",
    "bucket rebase",
    "bucket place",
    "bucket apply",
];

/// Ticks per call, and therefore the depth of the preallocated spike buffer. The sim steps 16 or
/// 17 ticks per game frame, so this is comfortably more than one frame.
const DEFAULT_MAX_TICKS: usize = 32;

/// PTX for `sm_75` (Turing), compiled from `cuda/lif.cu` by `cuda/build-ptx.sh` and committed, so
/// neither the build nor the run needs a CUDA toolkit.
const LIF_PTX: &str = include_str!("../../cuda/lif.ptx");

fn driver_error(what: &str, error: impl std::fmt::Display) -> Error {
    Error::new(format!("CUDA {what} failed: {error}"))
}

/// Per-neuron work list for a phase whose target indices may repeat.
///
/// `neuron[g]` is the neuron group `g` writes, and the group's addends are
/// `pos[ptr[g]..ptr[g + 1]]` in the order the sequential kernel applies them. For the noise kicks
/// and the stimulation pulse every addend is the same constant, so only the count is needed and
/// `pos` is unused.
struct Grouped {
    neuron: Vec<u32>,
    ptr: Vec<u32>,
    pos: Vec<u32>,
}

/// Group `indices` by neuron, keeping each neuron's first appearance as its group order and its
/// appearances in ascending order within the group.
fn group_indices(indices: &[u32], neurons: usize) -> Grouped {
    let mut slot_of = vec![u32::MAX; neurons];
    let mut neuron = Vec::new();
    let mut lists: Vec<Vec<u32>> = Vec::new();
    for (position, index) in indices.iter().copied().enumerate() {
        let existing = slot_of[index as usize];
        if existing == u32::MAX {
            slot_of[index as usize] = neuron.len() as u32;
            neuron.push(index);
            lists.push(vec![position as u32]);
        } else {
            lists[existing as usize].push(position as u32);
        }
    }
    let mut ptr = Vec::with_capacity(neuron.len() + 1);
    let mut pos = Vec::with_capacity(indices.len());
    ptr.push(0);
    for list in &lists {
        pos.extend_from_slice(list);
        ptr.push(pos.len() as u32);
    }
    Grouped { neuron, ptr, pos }
}

/// The GPU backend for one [`LifNetwork`].
pub struct CudaLif {
    #[allow(dead_code)]
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    noise_kernel: CudaFunction,
    visual_kernel: CudaFunction,
    stimulate_kernel: CudaFunction,
    sweep_kernel: CudaFunction,
    scan_kernel: CudaFunction,
    compact_kernel: CudaFunction,
    bucket_count_kernel: CudaFunction,
    bucket_place_kernel: CudaFunction,
    bucket_scan_kernel: CudaFunction,
    exscan_kernel: CudaFunction,
    bucket_rebase_kernel: CudaFunction,
    bucket_apply_kernel: CudaFunction,

    neurons: usize,
    blocks: u32,
    max_ticks: usize,
    noise_kicks: usize,

    membrane: CudaSlice<f32>,
    refractory: CudaSlice<u8>,
    baseline: CudaSlice<f32>,
    csr_indptr: CudaSlice<u32>,
    csr_targets: CudaSlice<u32>,
    csr_weights: CudaSlice<i16>,
    plastic_words: CudaSlice<u64>,
    plastic_prefix: CudaSlice<u32>,
    gains: CudaSlice<f32>,
    bucket_count: CudaSlice<u32>,
    bucket_offset: CudaSlice<u32>,
    bucket_cursor: CudaSlice<u32>,
    bucket_edge: CudaSlice<u64>,
    bucket_block_sums: CudaSlice<u32>,
    bucket_block_bases: CudaSlice<u32>,
    visual_neuron: CudaSlice<u32>,
    visual_ptr: CudaSlice<u32>,
    visual_pos: CudaSlice<u32>,
    visual_drive: CudaSlice<f32>,
    stimulation_neuron: CudaSlice<u32>,
    stimulation_count: CudaSlice<u32>,
    spiked_bits: CudaSlice<u32>,
    block_counts: CudaSlice<u32>,
    block_offsets: CudaSlice<u32>,
    noise_neuron: CudaSlice<u32>,
    noise_count: CudaSlice<u32>,
    tick_counts: CudaSlice<u32>,
    tick_base: CudaSlice<u32>,
    cursor: CudaSlice<u32>,
    spikes: CudaSlice<u32>,

    /// Timing events, created once with timing enabled and re-recorded per batch.
    batch_start: CudaEvent,
    batch_end: CudaEvent,
    /// Thirteen marks around the twelve kernel groups, for [`CudaLif::profile_phases`].
    phase_events: Vec<CudaEvent>,
    phase_launched: usize,

    visual_groups: u32,
    stimulation_groups: u32,
    host_noise_neuron: Vec<u32>,
    host_noise_count: Vec<u32>,
    noise_group_count: Vec<u32>,
    noise_seen: Vec<u32>,
    host_tick_counts: Vec<u32>,
    host_spikes: Vec<u32>,
    stimulation_schedule: Vec<bool>,
    host_dirty: bool,

    /// Copy `membrane` and `refractory` back at the end of every batch. On by default, so the rest
    /// of the crate sees the same host state it would have seen from the CPU kernel.
    pub sync_host_each_batch: bool,
    /// Device bytes allocated by this backend.
    pub device_bytes: usize,
    /// Accumulated GPU time of the batched kernels, from CUDA events.
    pub kernel_ms: f64,
    /// Ticks those events cover.
    pub kernel_ticks: u64,
    /// Time each kernel separately. Only meaningful at one tick per batch, because the marks are
    /// read back after the batch synchronises. Costs eight extra events per tick.
    pub profile_phases: bool,
    /// Per-kernel milliseconds, in launch order; see [`PHASE_NAMES`]. A phase that did not run in
    /// a tick contributes nothing.
    pub phase_ms: [f64; PHASES],
}

impl std::fmt::Debug for CudaLif {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CudaLif")
            .field("neurons", &self.neurons)
            .field("maxTicks", &self.max_ticks)
            .field("deviceBytes", &self.device_bytes)
            .finish()
    }
}

impl CudaLif {
    /// Build the backend for `network` on device `ordinal`, uploading the dataset and the current
    /// membrane and refractory state.
    pub fn new(network: &LifNetwork, ordinal: usize) -> Result<Self> {
        Self::with_max_ticks(network, ordinal, DEFAULT_MAX_TICKS)
    }

    pub fn with_max_ticks(network: &LifNetwork, ordinal: usize, max_ticks: usize) -> Result<Self> {
        let max_ticks = max_ticks.max(1);
        let neurons = network.data.meta.neurons;
        if neurons == 0 {
            return Err(Error::new("The CUDA backend needs at least one neuron"));
        }
        if neurons > 0x7fff_ffff {
            return Err(Error::new(
                "The CUDA backend packs the plastic flag into bit 31 of a source index, so it is \
                 limited to 2^31 neurons",
            ));
        }

        let context = CudaContext::new(ordinal).map_err(|error| {
            driver_error(
                "context creation (is libcuda present and a device visible?)",
                error,
            )
        })?;
        let stream = context.default_stream();
        let module = context
            .load_module(Ptx::from_src(LIF_PTX))
            .map_err(|error| driver_error("PTX load", error))?;
        let function = |name: &str| -> Result<CudaFunction> {
            module
                .load_function(name)
                .map_err(|error| driver_error(&format!("lookup of kernel {name}"), error))
        };

        let data = &network.data;
        let edges = data.targets.len();

        // The plastic-edge bitset over *base* edge indices, with a per-word exclusive count so a
        // plastic edge can find its gain slot without a side table: the rank of a base edge among
        // the plastic ones is precisely `RewardModulatedStdp::slot_of`, so the gains upload needs
        // no permutation.
        let words = edges.div_ceil(64).max(1);
        let mut plastic_words = vec![0u64; words];
        for edge in 0..edges {
            if network.plasticity.slot_of(edge).is_some() {
                plastic_words[edge >> 6] |= 1u64 << (edge & 63);
            }
        }
        let mut plastic_prefix = vec![0u32; words];
        let mut seen = 0u32;
        for (word, bits) in plastic_words.iter().copied().enumerate() {
            plastic_prefix[word] = seen;
            seen += bits.count_ones();
        }
        debug_assert_eq!(seen as usize, network.plasticity.gains.len());

        let visual = group_indices(&data.visual_indices, neurons);
        let stimulation = group_indices(&network.stimulation_targets, neurons);
        let stimulation_counts: Vec<u32> = (0..stimulation.neuron.len())
            .map(|group| stimulation.ptr[group + 1] - stimulation.ptr[group])
            .collect();

        let blocks = (neurons as u32).div_ceil(BLOCK);
        let noise_kicks = network.config.noise_kicks;
        let bits = neurons.div_ceil(32).max(1);

        let mut bytes = 0usize;
        macro_rules! upload {
            ($values:expr) => {{
                let values = $values;
                bytes += std::mem::size_of_val(&values[..]);
                stream
                    .clone_htod(&values)
                    .map_err(|error| driver_error("host-to-device copy", error))?
            }};
        }
        macro_rules! zeros {
            ($kind:ty, $count:expr) => {{
                let count: usize = $count;
                bytes += count * std::mem::size_of::<$kind>();
                stream
                    .alloc_zeros::<$kind>(count)
                    .map_err(|error| driver_error("device allocation", error))?
            }};
        }

        let gains_len = network.plasticity.gains.len().max(1);
        let backend = Self {
            membrane: upload!(network.membrane.clone()),
            refractory: upload!(network.refractory.clone()),
            baseline: upload!(network.baseline.clone()),
            csr_indptr: upload!(data.indptr.clone()),
            csr_targets: upload!(if data.targets.is_empty() {
                vec![0u32; 1]
            } else {
                data.targets.clone()
            }),
            csr_weights: upload!(if data.weights.is_empty() {
                vec![0i16; 1]
            } else {
                data.weights.clone()
            }),
            plastic_words: upload!(plastic_words),
            plastic_prefix: upload!(plastic_prefix),
            gains: zeros!(f32, gains_len),
            bucket_count: zeros!(u32, neurons),
            bucket_offset: zeros!(u32, neurons),
            bucket_cursor: zeros!(u32, neurons),
            bucket_edge: zeros!(u64, edges.max(1)),
            bucket_block_sums: zeros!(u32, blocks as usize),
            bucket_block_bases: zeros!(u32, blocks as usize),
            visual_neuron: upload!(if visual.neuron.is_empty() {
                vec![0u32; 1]
            } else {
                visual.neuron.clone()
            }),
            visual_ptr: upload!(visual.ptr.clone()),
            visual_pos: upload!(if visual.pos.is_empty() {
                vec![0u32; 1]
            } else {
                visual.pos.clone()
            }),
            visual_drive: zeros!(f32, data.visual_indices.len().max(1)),
            stimulation_neuron: upload!(if stimulation.neuron.is_empty() {
                vec![0u32; 1]
            } else {
                stimulation.neuron.clone()
            }),
            stimulation_count: upload!(if stimulation_counts.is_empty() {
                vec![0u32; 1]
            } else {
                stimulation_counts
            }),
            spiked_bits: zeros!(u32, bits),
            block_counts: zeros!(u32, blocks as usize),
            block_offsets: zeros!(u32, blocks as usize),
            noise_neuron: zeros!(u32, (max_ticks * noise_kicks).max(1)),
            noise_count: zeros!(u32, (max_ticks * noise_kicks).max(1)),
            tick_counts: zeros!(u32, max_ticks),
            tick_base: zeros!(u32, max_ticks),
            cursor: zeros!(u32, 1),
            spikes: zeros!(u32, max_ticks * neurons),

            noise_kernel: function("lif_noise")?,
            visual_kernel: function("lif_visual")?,
            stimulate_kernel: function("lif_stimulate")?,
            sweep_kernel: function("lif_sweep")?,
            scan_kernel: function("lif_scan")?,
            compact_kernel: function("lif_compact")?,
            bucket_count_kernel: function("lif_bucket_count")?,
            bucket_place_kernel: function("lif_bucket_place")?,
            bucket_scan_kernel: function("lif_bucket_scan")?,
            exscan_kernel: function("lif_exscan")?,
            bucket_rebase_kernel: function("lif_bucket_rebase")?,
            bucket_apply_kernel: function("lif_bucket_apply")?,

            neurons,
            blocks,
            max_ticks,
            noise_kicks,
            visual_groups: visual.neuron.len() as u32,
            stimulation_groups: stimulation.neuron.len() as u32,
            host_noise_neuron: vec![0; max_ticks * noise_kicks],
            host_noise_count: vec![0; max_ticks * noise_kicks],
            noise_group_count: vec![0; max_ticks],
            noise_seen: vec![u32::MAX; neurons],
            host_tick_counts: vec![0; max_ticks],
            host_spikes: Vec::new(),
            stimulation_schedule: vec![false; max_ticks],
            host_dirty: false,
            batch_start: context
                .new_event(Some(CUevent_flags::CU_EVENT_DEFAULT))
                .map_err(|error| driver_error("event creation", error))?,
            batch_end: context
                .new_event(Some(CUevent_flags::CU_EVENT_DEFAULT))
                .map_err(|error| driver_error("event creation", error))?,
            phase_events: (0..13)
                .map(|_| {
                    context
                        .new_event(Some(CUevent_flags::CU_EVENT_DEFAULT))
                        .map_err(|error| driver_error("event creation", error))
                })
                .collect::<Result<Vec<_>>>()?,
            phase_launched: 0,
            profile_phases: false,
            phase_ms: [0.0; PHASES],
            sync_host_each_batch: true,
            device_bytes: bytes,
            kernel_ms: 0.0,
            kernel_ticks: 0,
            context,
            stream,
        };
        Ok(backend)
    }

    /// Ticks this backend accepts per call.
    pub fn max_ticks(&self) -> usize {
        self.max_ticks
    }

    /// The host copies of `membrane` and `refractory` have been written; re-upload them before the
    /// next batch. Called by [`LifNetwork::import_state`].
    pub fn mark_host_dirty(&mut self) {
        self.host_dirty = true;
    }

    /// Copy `membrane` and `refractory` from the device into the network's host arrays.
    pub fn sync_to_host(&mut self, network: &mut LifNetwork) -> Result<()> {
        self.stream
            .memcpy_dtoh(&self.membrane, &mut network.membrane[..])
            .map_err(|error| driver_error("membrane download", error))?;
        self.stream
            .memcpy_dtoh(&self.refractory, &mut network.refractory[..])
            .map_err(|error| driver_error("refractory download", error))?;
        self.stream
            .synchronize()
            .map_err(|error| driver_error("synchronize", error))
    }

    fn upload_state(&mut self, network: &LifNetwork) -> Result<()> {
        self.stream
            .memcpy_htod(&network.membrane[..], &mut self.membrane)
            .map_err(|error| driver_error("membrane upload", error))?;
        self.stream
            .memcpy_htod(&network.refractory[..], &mut self.refractory)
            .map_err(|error| driver_error("refractory upload", error))?;
        self.host_dirty = false;
        Ok(())
    }

    /// Run `ticks` GPU ticks and replay the host-side phases against the spike lists they produce.
    fn run_batch(&mut self, network: &mut LifNetwork, ticks: usize) -> Result<usize> {
        debug_assert!(ticks <= self.max_ticks);
        let neurons = self.neurons;

        if self.host_dirty || self.sync_host_each_batch {
            self.upload_state(network)?;
        }

        // 1. The noise draws for the whole batch. They depend on nothing the GPU computes, so the
        //    RNG stays here and advances exactly as `step_one` advances it.
        for tick in 0..ticks {
            let base = tick * self.noise_kicks;
            let mut groups = 0usize;
            for _ in 0..self.noise_kicks {
                let neuron = (network.rng.next_uint() as usize) % neurons;
                let slot = self.noise_seen[neuron];
                if slot == u32::MAX {
                    self.noise_seen[neuron] = groups as u32;
                    self.host_noise_neuron[base + groups] = neuron as u32;
                    self.host_noise_count[base + groups] = 1;
                    groups += 1;
                } else {
                    self.host_noise_count[base + slot as usize] += 1;
                }
            }
            for group in 0..groups {
                self.noise_seen[self.host_noise_neuron[base + group] as usize] = u32::MAX;
            }
            self.noise_group_count[tick] = groups as u32;
        }

        // 2. The stimulation schedule, which `reward_remaining` decides tick by tick.
        for tick in 0..ticks {
            let active = network.reward_remaining > 0.0;
            self.stimulation_schedule[tick] = active;
            if active {
                network.reward_remaining -= 1.0;
            }
        }

        // Uploads: the batch's noise groups, this frame's visual drive, the plastic gains permuted
        // into the transposed array's rank order.
        let used = ticks * self.noise_kicks;
        if used > 0 {
            let mut view = self.noise_neuron.slice_mut(0..used);
            self.stream
                .memcpy_htod(&self.host_noise_neuron[..used], &mut view)
                .map_err(|error| driver_error("noise upload", error))?;
            let mut view = self.noise_count.slice_mut(0..used);
            self.stream
                .memcpy_htod(&self.host_noise_count[..used], &mut view)
                .map_err(|error| driver_error("noise upload", error))?;
        }
        if !network.visual_drive.is_empty() {
            let mut view = self.visual_drive.slice_mut(0..network.visual_drive.len());
            self.stream
                .memcpy_htod(&network.visual_drive[..], &mut view)
                .map_err(|error| driver_error("visual drive upload", error))?;
        }
        if !network.plasticity.gains.is_empty() {
            let mut view = self.gains.slice_mut(0..network.plasticity.gains.len());
            self.stream
                .memcpy_htod(&network.plasticity.gains[..], &mut view)
                .map_err(|error| driver_error("gain upload", error))?;
        }
        self.stream
            .memset_zeros(&mut self.cursor)
            .map_err(|error| driver_error("cursor reset", error))?;

        self.batch_start
            .record(&self.stream)
            .map_err(|error| driver_error("event record", error))?;
        for tick in 0..ticks {
            self.launch_tick(network, tick)?;
        }
        self.batch_end
            .record(&self.stream)
            .map_err(|error| driver_error("event record", error))?;
        self.stream
            .synchronize()
            .map_err(|error| driver_error("synchronize", error))?;
        self.kernel_ms += f64::from(
            self.batch_start
                .elapsed_ms(&self.batch_end)
                .map_err(|error| driver_error("event elapsed", error))?,
        );
        self.kernel_ticks += ticks as u64;
        if self.profile_phases && self.phase_launched == PHASES + 1 {
            for phase in 0..PHASES {
                self.phase_ms[phase] += f64::from(
                    self.phase_events[phase]
                        .elapsed_ms(&self.phase_events[phase + 1])
                        .map_err(|error| driver_error("phase event elapsed", error))?,
                );
            }
        }

        // Download the counts, then the spike lists in one packed transfer.
        let counts = self.tick_counts.slice(0..ticks);
        self.stream
            .memcpy_dtoh(&counts, &mut self.host_tick_counts[..ticks])
            .map_err(|error| driver_error("spike count download", error))?;
        let total: usize = self.host_tick_counts[..ticks]
            .iter()
            .map(|count| *count as usize)
            .sum();
        if self.host_spikes.len() < total {
            self.host_spikes.resize(total, 0);
        }
        if total > 0 {
            let view = self.spikes.slice(0..total);
            self.stream
                .memcpy_dtoh(&view, &mut self.host_spikes[..total])
                .map_err(|error| driver_error("spike download", error))?;
        }
        self.stream
            .synchronize()
            .map_err(|error| driver_error("synchronize", error))?;

        // The host phases, per tick, in the sequential kernel's order.
        let mut at = 0usize;
        let mut spikes = 0usize;
        for tick in 0..ticks {
            let count = self.host_tick_counts[tick] as usize;
            network.spikes[..count].copy_from_slice(&self.host_spikes[at..at + count]);
            at += count;
            spikes += count;
            network.replay_host_phases(count);
        }

        if self.sync_host_each_batch {
            self.sync_to_host(network)?;
        }
        Ok(spikes)
    }

    /// Record phase mark `index` when phase profiling is on.
    fn mark(&mut self, index: usize) -> Result<()> {
        if !self.profile_phases {
            return Ok(());
        }
        self.phase_events[index]
            .record(&self.stream)
            .map_err(|error| driver_error("phase event record", error))?;
        self.phase_launched = index + 1;
        Ok(())
    }

    fn launch_tick(&mut self, network: &LifNetwork, tick: usize) -> Result<()> {
        let neurons = self.neurons as u32;
        let grid = |threads: u32| LaunchConfig {
            grid_dim: (threads.div_ceil(BLOCK).max(1), 1, 1),
            block_dim: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };

        self.mark(0)?;
        let groups = self.noise_group_count[tick];
        if groups > 0 {
            let base = tick * self.noise_kicks;
            let neuron = self.noise_neuron.slice(base..base + self.noise_kicks);
            let count = self.noise_count.slice(base..base + self.noise_kicks);
            let amount = network.config.noise_amount;
            let mut launch = self.stream.launch_builder(&self.noise_kernel);
            launch
                .arg(&mut self.membrane)
                .arg(&neuron)
                .arg(&count)
                .arg(&groups)
                .arg(&amount);
            unsafe { launch.launch(grid(groups)) }
                .map_err(|error| driver_error("lif_noise launch", error))?;
        }

        self.mark(1)?;
        if self.visual_groups > 0 {
            let groups = self.visual_groups;
            let mut launch = self.stream.launch_builder(&self.visual_kernel);
            launch
                .arg(&mut self.membrane)
                .arg(&self.visual_neuron)
                .arg(&self.visual_ptr)
                .arg(&self.visual_pos)
                .arg(&self.visual_drive)
                .arg(&groups);
            unsafe { launch.launch(grid(groups)) }
                .map_err(|error| driver_error("lif_visual launch", error))?;
        }

        self.mark(2)?;
        if self.stimulation_schedule[tick] && self.stimulation_groups > 0 {
            let groups = self.stimulation_groups;
            let drive = network.config.stimulation.drive;
            let mut launch = self.stream.launch_builder(&self.stimulate_kernel);
            launch
                .arg(&mut self.membrane)
                .arg(&self.stimulation_neuron)
                .arg(&self.stimulation_count)
                .arg(&groups)
                .arg(&drive);
            unsafe { launch.launch(grid(groups)) }
                .map_err(|error| driver_error("lif_stimulate launch", error))?;
        }

        self.mark(3)?;
        let decay = network.decay;
        let threshold = network.config.threshold;
        let reset = u32::from(network.refractory_reset);
        let mut launch = self.stream.launch_builder(&self.sweep_kernel);
        launch
            .arg(&mut self.membrane)
            .arg(&mut self.refractory)
            .arg(&self.baseline)
            .arg(&neurons)
            .arg(&decay)
            .arg(&threshold)
            .arg(&reset)
            .arg(&mut self.spiked_bits)
            .arg(&mut self.block_counts);
        unsafe { launch.launch(grid(neurons)) }
            .map_err(|error| driver_error("lif_sweep launch", error))?;

        self.mark(4)?;
        let blocks = self.blocks;
        let index = tick as u32;
        let mut launch = self.stream.launch_builder(&self.scan_kernel);
        launch
            .arg(&self.block_counts)
            .arg(&mut self.block_offsets)
            .arg(&blocks)
            .arg(&mut self.tick_counts)
            .arg(&mut self.tick_base)
            .arg(&index)
            .arg(&mut self.cursor);
        unsafe {
            launch.launch(LaunchConfig {
                grid_dim: (1, 1, 1),
                block_dim: (SCAN_THREADS, 1, 1),
                shared_mem_bytes: SCAN_THREADS * 4,
            })
        }
        .map_err(|error| driver_error("lif_scan launch", error))?;

        self.mark(5)?;
        let mut launch = self.stream.launch_builder(&self.compact_kernel);
        launch
            .arg(&self.spiked_bits)
            .arg(&self.block_offsets)
            .arg(&neurons)
            .arg(&mut self.spikes);
        unsafe { launch.launch(grid(neurons)) }
            .map_err(|error| driver_error("lif_compact launch", error))?;

        self.mark(6)?;
        // Step 6, in four phases: zero the bucket sizes, count, scan, place, apply. The count and
        // place passes walk only the spiking sources' rows, which is where the win over the pull
        // formulation comes from.
        self.stream
            .memset_zeros(&mut self.bucket_count)
            .map_err(|error| driver_error("bucket reset", error))?;

        // A grid-stride loop, so the grid is sized for the usual spike count rather than the worst
        // case; 8,192 warps covers about five times the measured rate.
        let gather = LaunchConfig {
            grid_dim: (1_024, 1, 1),
            block_dim: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.bucket_count_kernel);
        launch
            .arg(&self.spikes)
            .arg(&self.tick_counts)
            .arg(&self.tick_base)
            .arg(&index)
            .arg(&self.csr_indptr)
            .arg(&self.csr_targets)
            .arg(&mut self.bucket_count);
        unsafe { launch.launch(gather) }
            .map_err(|error| driver_error("lif_bucket_count launch", error))?;

        self.mark(7)?;
        let mut launch = self.stream.launch_builder(&self.bucket_scan_kernel);
        launch
            .arg(&self.bucket_count)
            .arg(&mut self.bucket_offset)
            .arg(&mut self.bucket_block_sums)
            .arg(&neurons);
        unsafe { launch.launch(grid(neurons)) }
            .map_err(|error| driver_error("lif_bucket_scan launch", error))?;

        self.mark(8)?;
        let mut launch = self.stream.launch_builder(&self.exscan_kernel);
        launch
            .arg(&self.bucket_block_sums)
            .arg(&mut self.bucket_block_bases)
            .arg(&blocks);
        unsafe {
            launch.launch(LaunchConfig {
                grid_dim: (1, 1, 1),
                block_dim: (SCAN_THREADS, 1, 1),
                shared_mem_bytes: SCAN_THREADS * 4,
            })
        }
        .map_err(|error| driver_error("lif_exscan launch", error))?;

        self.mark(9)?;
        let mut launch = self.stream.launch_builder(&self.bucket_rebase_kernel);
        launch
            .arg(&mut self.bucket_offset)
            .arg(&mut self.bucket_cursor)
            .arg(&self.bucket_block_bases)
            .arg(&neurons);
        unsafe { launch.launch(grid(neurons)) }
            .map_err(|error| driver_error("lif_bucket_rebase launch", error))?;

        self.mark(10)?;
        let mut launch = self.stream.launch_builder(&self.bucket_place_kernel);
        launch
            .arg(&self.spikes)
            .arg(&self.tick_counts)
            .arg(&self.tick_base)
            .arg(&index)
            .arg(&self.csr_indptr)
            .arg(&self.csr_targets)
            .arg(&self.csr_weights)
            .arg(&self.plastic_words)
            .arg(&self.plastic_prefix)
            .arg(&mut self.bucket_cursor)
            .arg(&mut self.bucket_edge);
        unsafe { launch.launch(gather) }
            .map_err(|error| driver_error("lif_bucket_place launch", error))?;

        self.mark(11)?;
        let synapse_scale = network.config.synapse_scale;
        let membrane_floor = network.config.membrane_floor;
        let mut launch = self.stream.launch_builder(&self.bucket_apply_kernel);
        launch
            .arg(&mut self.membrane)
            .arg(&self.bucket_count)
            .arg(&self.bucket_offset)
            .arg(&mut self.bucket_edge)
            .arg(&self.gains)
            .arg(&neurons)
            .arg(&synapse_scale)
            .arg(&membrane_floor);
        unsafe { launch.launch(grid(neurons)) }
            .map_err(|error| driver_error("lif_bucket_apply launch", error))?;
        self.mark(12)?;
        Ok(())
    }
}

impl LifNetwork {
    /// Move the per-millisecond tick onto `backend`. [`LifNetwork::step`] then runs steps 1-4 and 6
    /// on the GPU and the rest here, unchanged.
    pub fn attach_cuda(&mut self, backend: CudaLif) {
        self.cuda = Some(Box::new(backend));
    }

    /// Take the backend back off, leaving the network on the CPU kernel.
    pub fn detach_cuda(&mut self) -> Option<CudaLif> {
        self.cuda.take().map(|backend| *backend)
    }

    pub fn cuda(&self) -> Option<&CudaLif> {
        self.cuda.as_deref()
    }

    pub fn cuda_mut(&mut self) -> Option<&mut CudaLif> {
        self.cuda.as_deref_mut()
    }

    /// Attach a backend when `FLY_LIF_CUDA=1`, on device `FLY_LIF_CUDA_DEVICE` (default 0).
    ///
    /// Panics on a CUDA failure rather than falling back to the CPU: a run that asked for the GPU
    /// and silently got the CPU would report a bit-exactness pass that means nothing.
    pub(super) fn attach_cuda_if_requested(&mut self) {
        if self.cuda.is_some() || std::env::var("FLY_LIF_CUDA").as_deref() != Ok("1") {
            return;
        }
        let ordinal = std::env::var("FLY_LIF_CUDA_DEVICE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        match CudaLif::new(self, ordinal) {
            Ok(backend) => self.attach_cuda(backend),
            Err(error) => panic!("FLY_LIF_CUDA=1 but the CUDA backend would not start: {error}"),
        }
    }

    /// The batched GPU path behind [`LifNetwork::step`].
    pub(super) fn step_on_cuda(&mut self, milliseconds: u64) -> u64 {
        let mut backend = self.cuda.take().expect("a CUDA backend is attached");
        let mut total = 0u64;
        let mut left = milliseconds;
        while left > 0 {
            let ticks = left.min(backend.max_ticks() as u64) as usize;
            match backend.run_batch(self, ticks) {
                Ok(spikes) => total += spikes as u64,
                Err(error) => panic!("CUDA LIF batch failed: {error}"),
            }
            left -= ticks as u64;
        }
        self.cuda = Some(backend);
        total
    }

    /// Steps 5, 6 (the spike stamp and role tally only), 7 and 8 of `step_one`, against a spike
    /// list the GPU produced. Byte-for-byte the same statements, in the same order.
    fn replay_host_phases(&mut self, spike_count: usize) {
        let neurons = self.data.meta.neurons;
        let rate_alpha = self.config.rate_alpha;

        self.role_counts.fill(0);
        self.plasticity
            .observe(&self.spikes, spike_count, &self.last_spike_ms, self.ms);

        let ms = self.ms;
        for source in self.spikes[..spike_count].iter().copied() {
            let source = source as usize;
            self.last_spike_ms[source] = ms;
            let mut mask = self.role_masks[source];
            while mask != 0 {
                self.role_counts[mask.trailing_zeros() as usize] += 1;
                mask &= mask - 1;
            }
        }

        for (role, name) in self.role_names.iter().enumerate() {
            let size = self.role_sizes[role];
            let instantaneous = if size != 0 {
                f64::from(self.role_counts[role]) * 1000.0 / size as f64
            } else {
                0.0
            };
            let current = self.rates.get_or_zero(name);
            self.rates
                .set(name, current + (instantaneous - current) * rate_alpha);
        }
        self.population_rate +=
            (spike_count as f64 * 1000.0 / neurons as f64 - self.population_rate) * rate_alpha;

        self.ms += 1.0;
        self.timings.ticks += 1;
    }
}
