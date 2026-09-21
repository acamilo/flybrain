//! Reward-modulated STDP, bit-exact with `model/plasticity.ts`.
//!
//! Pre-post spike timing writes an eligibility trace; a caller-supplied scalar reward turns that
//! trace into a change in a per-edge gain. Nothing else in the library learns.

use crate::bitset::{BitSet, RankedBitSet};
use crate::dataset::BrainDataset;
use crate::error::{bail, Result};
use crate::jsmath::{exp, js_max, js_min, tanh};
use crate::pool::{SharedSlice, WorkerPool};
use crate::version::{fnv1a32_step, version_for};

/// Version of the default configuration; kept verbatim so existing checkpoints stay loadable.
pub const PLASTICITY_VERSION: &str = "fly-kc-mbon-rstdp-v2";

/// Prefix of a derived version string. Deliberately not the full default string.
const PLASTICITY_VERSION_PREFIX: &str = "rstdp-v2:";

#[derive(Debug, Clone, PartialEq)]
pub struct PlasticityConfig {
    /// Anatomical role a plastic edge's source must belong to.
    pub pre_role: String,
    /// Anatomical role a plastic edge's target must belong to.
    pub post_role: String,
    /// Maximum number of plastic edges (strongest positive weights win).
    pub budget: usize,
    /// Eligibility-trace decay time constant in milliseconds.
    pub trace_ms: f64,
    /// Spike-pair exponential time constant in milliseconds.
    pub pair_ms: f64,
    /// Largest spike interval that still pairs, in milliseconds.
    pub pair_window_ms: f64,
    /// Eligibility added for a causal (pre before post) pair at dt = 0.
    pub potentiation: f64,
    /// Eligibility subtracted for an anti-causal (post before pre) pair at dt = 0.
    pub depression: f64,
    /// Gain step per unit of modulator times eligibility.
    pub learning_rate: f64,
    /// Restoring pull back towards a gain of 1, applied on reinforcement only.
    pub restoring: f64,
    /// Lower gain clamp.
    pub min_gain: f64,
    /// Upper gain clamp.
    pub max_gain: f64,
}

/// Original constants: strongest 16,384 positive KC->MBON edges, gains clamped to [0.9, 1.1].
impl Default for PlasticityConfig {
    fn default() -> Self {
        Self {
            pre_role: "kenyon".to_string(),
            post_role: "mbon".to_string(),
            budget: 16_384,
            trace_ms: 5000.0,
            pair_ms: 20.0,
            pair_window_ms: 100.0,
            potentiation: 0.1,
            depression: 0.05,
            learning_rate: 0.002,
            restoring: 0.0001,
            min_gain: 0.9,
            max_gain: 1.1,
        }
    }
}

impl PlasticityConfig {
    /// Numeric parameters that define the learning rule, in version-hash order.
    ///
    /// Role names and budget are deliberately absent: they change which edges are selected, which
    /// the topology hash already covers.
    fn version_params(&self) -> [f64; 9] {
        [
            self.trace_ms,
            self.pair_ms,
            self.pair_window_ms,
            self.potentiation,
            self.depression,
            self.learning_rate,
            self.restoring,
            self.min_gain,
            self.max_gain,
        ]
    }
}

/// `PLASTICITY_VERSION` for the default rule constants, otherwise `rstdp-v2:<fnv1a32>`.
pub fn plasticity_version(config: &PlasticityConfig) -> String {
    version_for(
        PLASTICITY_VERSION,
        PLASTICITY_VERSION_PREFIX,
        &config.version_params(),
        &PlasticityConfig::default().version_params(),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct LearningStats {
    pub version: String,
    pub enabled: bool,
    pub synapses: usize,
    /// Count of selected edges (hash group 1). Historical field name.
    pub mushroom: u64,
    /// Always 0: no second edge group exists. Kept for checkpoint/UI compatibility.
    pub output: u64,
    pub updates: f64,
    pub changed: u64,
    pub mean_change: f64,
    pub max_change: f64,
    pub signal: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlasticityState {
    pub version: String,
    pub topology: u32,
    pub enabled: bool,
    pub updates: f64,
    pub signal: f64,
    pub gains: Vec<f32>,
    pub traces: Vec<f32>,
    pub touched: Vec<f64>,
}

/// Phenomenological three-factor plasticity; anatomical sites, not fitted dopamine compartments.
#[derive(Debug, Clone)]
pub struct RewardModulatedStdp {
    pub config: PlasticityConfig,
    /// Which base edges are plastic, and the slot of each.
    ///
    /// This was a dense `Vec<i32>` of "slot, or -1" with one entry per base edge: 10.8 MB for
    /// `fafb-v783`, read once per propagated edge, which is enough traffic on its own to evict the
    /// membrane arrays from L2. The ranked bitset is 506 KB and answers "not plastic" — the
    /// overwhelming majority — from a single word. A rank *is* the old slot number, because the
    /// selected edges are numbered in ascending edge order either way.
    slots: RankedBitSet,
    pub edges: Vec<u32>,
    pub gains: Vec<f32>,
    pub traces: Vec<f32>,
    pub touched: Vec<f64>,
    /// Version string of this configuration; written into exported state.
    pub version: String,
    pub enabled: bool,
    sources: Vec<u32>,
    /// `data.targets[edges[slot]]`, cached so `observe` never touches the base CSR arrays.
    slot_targets: Vec<u32>,
    selected: Vec<u8>,
    /// `incoming[neuron]` and `outgoing.get(neuron)` from the original, flattened into CSR over
    /// neurons. Both preserve ascending edge order, which keeps pairing order identical to the
    /// original base-edge traversal; an absent entry is an empty range, matching the original's
    /// `if (!outgoing) continue`.
    incoming: SlotIndex,
    outgoing: SlotIndex,
    /// `exp(-dt / pair_ms)` for every integer `dt` the pairing window admits, and
    /// `exp(-gap / trace_ms)` for every short integer gap. `observe` calls `exp` about 7,800 times
    /// per tick on `fafb-v783` -- two per pairing -- and every one of those arguments is an integer
    /// number of milliseconds divided by a fixed constant, so almost all of them are one of a few
    /// hundred values. See [`table_exp`]: this is a lookup that returns identical bits, not an
    /// approximation, and the fallback path is the original call.
    pair_decay: Vec<f64>,
    trace_decay: Vec<f64>,
    topology: u32,
    updates: f64,
    signal: f64,
}

/// Per-neuron slot lists in CSR form, plus a bit per neuron saying whether its list is non-empty.
///
/// The bitset is the whole point of the type. `observe` runs for every spiking neuron — about
/// 1,730 per tick on `fafb-v783` — and asking "does this neuron have plastic edges?" through
/// `offsets` costs two random loads from a 557 KB array, which is four L3 hits per spike between
/// the two indices. Almost every answer is no: only 4,730 of 139,255 neurons carry a plastic edge.
/// A 17 KB bitset gives the same answer out of L1.
#[derive(Debug, Clone, Default)]
struct SlotIndex {
    offsets: Vec<u32>,
    slots: Vec<u32>,
    present: BitSet,
}

impl SlotIndex {
    /// Build from `(neuron, slot)` pairs already in ascending slot order.
    fn build(neurons: usize, pairs: impl Iterator<Item = (u32, u32)> + Clone) -> Self {
        let mut counts = vec![0u32; neurons + 1];
        let mut present = BitSet::new(neurons);
        for (neuron, _) in pairs.clone() {
            counts[neuron as usize + 1] += 1;
            present.insert(neuron as usize);
        }
        for index in 1..counts.len() {
            counts[index] += counts[index - 1];
        }
        let offsets = counts;
        let mut cursor = offsets.clone();
        let mut slots = vec![0u32; offsets[neurons] as usize];
        for (neuron, slot) in pairs {
            let at = &mut cursor[neuron as usize];
            slots[*at as usize] = slot;
            *at += 1;
        }
        Self {
            offsets,
            slots,
            present,
        }
    }

    #[inline]
    fn get(&self, neuron: u32) -> &[u32] {
        let from = self.offsets[neuron as usize] as usize;
        let to = self.offsets[neuron as usize + 1] as usize;
        &self.slots[from..to]
    }

    #[inline]
    fn words(&self) -> &[u64] {
        self.present.words()
    }
}

/// Entries in the trace-decay table: `exp(-gap / trace_ms)` for gaps of 0..1023 ms.
///
/// 8 KB. Traces are touched about a quarter of a tick apart on average, so the gap is nearly always
/// a few milliseconds; a longer gap falls through to `exp` and costs what it always did.
const TRACE_TABLE: usize = 1024;

/// Upper bound on the pairing-window table, so a pathological `pair_window_ms` cannot allocate.
const MAX_PAIR_TABLE: usize = 4096;

/// `exp(-argument / scale)`, read from `table` when `argument` is an integer the table covers.
///
/// `table[k]` is built as `exp(-(k as f64) / scale)`, the same expression with the same operands,
/// so a hit returns the same bits the call would — this is a lookup, not an approximation. Both of
/// the kernel's `exp` arguments are integer millisecond differences, because `ms` is an integer
/// tick counter and `touched` and `lastSpikeMs` only ever hold an `ms`; the integer test is there
/// because `observe` and `reinforce` are public and a caller may pass anything.
#[inline]
fn table_exp(table: &[f64], argument: f64, scale: f64) -> f64 {
    let index = argument as usize;
    if index as f64 == argument {
        if let Some(value) = table.get(index) {
            return *value;
        }
    }
    exp(-argument / scale)
}

/// The pairing-window constants `observe` reads, grouped so a shard takes one argument.
struct PairWindow {
    pair_ms: f64,
    pair_window_ms: f64,
    potentiation: f64,
    depression: f64,
    trace_ms: f64,
}

/// The read-only slot indices and decay tables `observe` reads, likewise.
struct SlotArrays<'a> {
    incoming: &'a SlotIndex,
    outgoing: &'a SlotIndex,
    sources: &'a [u32],
    slot_targets: &'a [u32],
    pair_decay: &'a [f64],
    trace_decay: &'a [f64],
}

/// The slots in `list` that lie in `offset..limit`, as one contiguous window.
///
/// Every per-neuron list is built in ascending slot order ([`SlotIndex::build`] is fed pairs in
/// slot order), so the shard's slots are a contiguous run found by binary search rather than a
/// per-slot range test. `SHARDED` is false for the unsharded walk, where the window is the whole
/// list and the search folds away.
#[inline(always)]
fn slot_window<const SHARDED: bool>(list: &[u32], offset: usize, limit: usize) -> &[u32] {
    if !SHARDED {
        return list;
    }
    let from = list.partition_point(|slot| (*slot as usize) < offset);
    let to = list.partition_point(|slot| (*slot as usize) < limit);
    &list[from..to]
}

/// Observe this tick's spike pairs for the slots in `offset..limit`, whose traces and touch
/// stamps are `traces` and `touched`.
///
/// # Determinism
///
/// **For a fixed slot the sequence of trace writes is the sequential one, whatever the shard
/// bounds** — the same argument [`crate::lif`]'s `propagate_shard` rests on. The sequential walk
/// visits the spike list in order and, per spiking neuron, its arriving slots then its leaving
/// slots, each in ascending slot order. A shard walks the same spike list in the same order and
/// the same lists in the same order, and writes exactly the slots in `offset..limit` — a
/// subsequence of the sequential write sequence, which preserves the relative order of every pair
/// it keeps. Restricted to one slot it is therefore the sequential sequence for that slot, and a
/// trace write depends on nothing but that slot's own previous value and touch stamp: the lazy
/// decay, the clamp and the single rounding to f32 happen at the same points with the same
/// operands. Slots in different shards never interact and every slot is in exactly one shard.
///
/// `last_spike` is read, never written — the kernel stamps this tick's spikes after `observe`
/// returns — so the shards need no ordering between them at all.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn observe_shard<const SHARDED: bool>(
    traces: &mut [f32],
    touched: &mut [f64],
    offset: usize,
    limit: usize,
    spikes: &[u32],
    last_spike: &[f64],
    ms: f64,
    index: &SlotArrays<'_>,
    window: &PairWindow,
) {
    if traces.is_empty() {
        return;
    }
    let (incoming_words, outgoing_words) = (index.incoming.words(), index.outgoing.words());
    for neuron in spikes.iter().copied() {
        // Most spiking neurons carry no plastic edge in either direction, and both bitsets fit
        // in L1, so the common case is two loads from the same pair of cache lines and no
        // touch of `offsets`, `slots`, `sources` or `slot_targets` at all.
        let word = neuron as usize >> 6;
        let bit = 1u64 << (neuron & 63);
        let (arriving, leaving) = (incoming_words[word], outgoing_words[word]);
        if (arriving | leaving) & bit == 0 {
            continue;
        }
        // Causal: each slot arriving at the spiking neuron.
        if arriving & bit != 0 {
            let list = slot_window::<SHARDED>(index.incoming.get(neuron), offset, limit);
            for slot in list.iter().copied() {
                let dt = ms - last_spike[index.sources[slot as usize] as usize];
                if dt > 0.0 && dt <= window.pair_window_ms {
                    let pair =
                        window.potentiation * table_exp(index.pair_decay, dt, window.pair_ms);
                    write_trace(
                        traces,
                        touched,
                        index.trace_decay,
                        window.trace_ms,
                        slot as usize - offset,
                        ms,
                        pair,
                    );
                }
            }
        }
        // Anti-causal: each slot leaving the spiking neuron.
        if leaving & bit != 0 {
            let list = slot_window::<SHARDED>(index.outgoing.get(neuron), offset, limit);
            for slot in list.iter().copied() {
                let dt = ms - last_spike[index.slot_targets[slot as usize] as usize];
                if dt > 0.0 && dt <= window.pair_window_ms {
                    let pair = -window.depression * table_exp(index.pair_decay, dt, window.pair_ms);
                    write_trace(
                        traces,
                        touched,
                        index.trace_decay,
                        window.trace_ms,
                        slot as usize - offset,
                        ms,
                        pair,
                    );
                }
            }
        }
    }
}

/// The one lazy trace update: advance the trace to `ms`, add `pair`, clamp, stamp.
///
/// A free function rather than a method so `observe` can hold the slot indices and the trace
/// arrays borrowed at the same time.
#[inline]
fn write_trace(
    traces: &mut [f32],
    touched: &mut [f64],
    decay_table: &[f64],
    trace_ms: f64,
    slot: usize,
    ms: f64,
    pair: f64,
) {
    let decay = table_exp(decay_table, js_max(0.0, ms - touched[slot]), trace_ms);
    let value = f64::from(traces[slot]) * decay + pair;
    traces[slot] = js_max(-1.0, js_min(1.0, value)) as f32;
    touched[slot] = ms;
}

impl RewardModulatedStdp {
    pub fn new(data: &BrainDataset, config: PlasticityConfig) -> Self {
        let version = plasticity_version(&config);
        let pre: std::collections::HashSet<u32> =
            data.role(&config.pre_role).iter().copied().collect();
        let post: std::collections::HashSet<u32> =
            data.role(&config.post_role).iter().copied().collect();

        // Candidates: positive, non-self edges from a preRole neuron to a postRole neuron.
        let mut candidates: Vec<(u32, u32)> = Vec::new();
        for source in 0..data.meta.neurons as u32 {
            let from = data.indptr[source as usize] as usize;
            let to = data.indptr[source as usize + 1] as usize;
            for edge in from..to {
                let target = data.targets[edge];
                if data.weights[edge] <= 0 || source == target {
                    continue;
                }
                if pre.contains(&source) && post.contains(&target) {
                    candidates.push((edge as u32, source));
                }
            }
        }

        // Fixed deterministic budget: strongest anatomical pre->post connections.
        //
        // `(a, b) => weights[b] - weights[a] || a - b` is a total order (edge indices are
        // unique), so a stable sort with the same comparator reproduces V8's result exactly.
        candidates.sort_by(|a, b| {
            let weight_a = f64::from(data.weights[a.0 as usize]);
            let weight_b = f64::from(data.weights[b.0 as usize]);
            match weight_b.partial_cmp(&weight_a) {
                Some(std::cmp::Ordering::Equal) | None => a.0.cmp(&b.0),
                Some(order) => order,
            }
        });
        candidates.truncate(config.budget);
        candidates.sort_by_key(|candidate| candidate.0);

        let count = candidates.len();
        // `candidates` is edge-sorted and edge indices are unique, so the k-th set bit belongs to
        // the k-th slot: a rank in this set is exactly the slot the dense array used to store.
        let slots = RankedBitSet::from_ascending(
            data.meta.edges,
            candidates.iter().map(|(edge, _)| *edge as usize),
        );
        let mut hash: u32 = 2_166_136_261;
        for (edge, source) in candidates.iter().copied() {
            // A negative Int16 weight sign-extends into the int32 XOR, as in JavaScript.
            for value in [
                edge as i32,
                source as i32,
                data.targets[edge as usize] as i32,
                i32::from(data.weights[edge as usize]),
                1,
            ] {
                hash = fnv1a32_step(hash, value);
            }
        }

        let pair_span = if config.pair_window_ms >= 0.0 {
            (config.pair_window_ms as usize).min(MAX_PAIR_TABLE)
        } else {
            0
        };
        let pair_decay = (0..=pair_span)
            .map(|dt| exp(-(dt as f64) / config.pair_ms))
            .collect();
        let trace_decay = (0..TRACE_TABLE)
            .map(|gap| exp(-(gap as f64) / config.trace_ms))
            .collect();

        let neurons = data.meta.neurons;
        // `candidates` is edge-sorted, so building in slot order keeps each per-neuron list in
        // ascending edge order.
        let incoming = SlotIndex::build(
            neurons,
            candidates
                .iter()
                .enumerate()
                .map(|(slot, (edge, _))| (data.targets[*edge as usize], slot as u32)),
        );
        let outgoing = SlotIndex::build(
            neurons,
            candidates
                .iter()
                .enumerate()
                .map(|(slot, (_, source))| (*source, slot as u32)),
        );

        Self {
            slots,
            edges: candidates.iter().map(|candidate| candidate.0).collect(),
            sources: candidates.iter().map(|candidate| candidate.1).collect(),
            slot_targets: candidates
                .iter()
                .map(|candidate| data.targets[candidate.0 as usize])
                .collect(),
            // Every candidate belongs to hash group 1; there is no second edge group.
            selected: vec![1; count],
            gains: vec![1.0; count],
            traces: vec![0.0; count],
            touched: vec![0.0; count],
            incoming,
            outgoing,
            pair_decay,
            trace_decay,
            topology: hash,
            updates: 0.0,
            signal: 0.0,
            enabled: true,
            version,
            config,
        }
    }

    /// Effective multiplier for a base edge; 1 for every non-plastic edge.
    #[inline]
    pub fn gain(&self, edge: usize) -> f64 {
        match self.slots.rank_of(edge) {
            None => 1.0,
            Some(slot) => f64::from(self.gains[slot as usize]),
        }
    }

    /// Plastic slot of a base edge, or `None` for the immutable majority.
    #[inline]
    pub fn slot_of(&self, edge: usize) -> Option<u32> {
        self.slots.rank_of(edge)
    }

    /// The plastic-edge presence bitset, so a loop over a run of consecutive edge indices — a CSR
    /// row — loads one word and asks [`RewardModulatedStdp::gain_in_word`] per edge.
    #[inline]
    pub fn slot_words(&self) -> &[u64] {
        self.slots.words()
    }

    /// [`RewardModulatedStdp::gain`] for `edge`, given the already-loaded bitset word `edge >> 6`.
    #[inline]
    pub fn gain_in_word(&self, edge: usize, word: u64) -> f64 {
        if word & (1u64 << (edge & 63)) == 0 {
            return 1.0;
        }
        f64::from(self.gains[self.slots.rank_within(edge, word) as usize])
    }

    /// Bytes the plastic-edge index occupies; quoted by the crate README.
    pub fn slot_index_bytes(&self) -> usize {
        self.slots.heap_bytes()
    }

    pub fn topology(&self) -> u32 {
        self.topology
    }

    pub fn updates(&self) -> f64 {
        self.updates
    }

    pub fn signal(&self) -> f64 {
        self.signal
    }

    pub fn clear_eligibility(&mut self, ms: f64) {
        self.traces.fill(0.0);
        self.touched.fill(ms);
        self.signal = 0.0;
    }

    /// Called once per tick by the kernel, *before* this tick's spikes are written into
    /// `last_spike`, so a pair needs a strictly positive `dt`.
    pub fn observe(&mut self, spikes: &[u32], count: usize, last_spike: &[f64], ms: f64) {
        self.observe_sharded(spikes, count, last_spike, ms, None);
    }

    /// Slot-range boundaries for `workers` workers, length `workers + 1`.
    ///
    /// An even split of the slot range. Slots are numbered in ascending base-edge order, which is
    /// source-major, so a range is a contiguous run of pre-role neurons' edges; the long per-neuron
    /// lists belong to the post-role neurons and are spread across the whole range, which is where
    /// nearly all of the work is. Balance is not a correctness property either way — see
    /// [`observe_shard`].
    pub fn slot_shards(&self, workers: usize) -> Vec<u32> {
        let slots = self.gains.len();
        let workers = workers.max(1);
        let chunk = slots.div_ceil(workers).max(1);
        (0..=workers)
            .map(|worker| (chunk * worker).min(slots) as u32)
            .collect()
    }

    /// [`RewardModulatedStdp::observe`], optionally sharded by slot range over a worker pool.
    ///
    /// `shards` is a pool and the boundaries [`RewardModulatedStdp::slot_shards`] returned for it.
    /// The result is identical to the sequential run for any worker count; the argument is in
    /// [`observe_shard`].
    pub fn observe_sharded(
        &mut self,
        spikes: &[u32],
        count: usize,
        last_spike: &[f64],
        ms: f64,
        shards: Option<(&WorkerPool, &[u32])>,
    ) {
        if !self.enabled {
            return;
        }
        let Self {
            config,
            traces,
            touched,
            sources,
            slot_targets,
            incoming,
            outgoing,
            pair_decay,
            trace_decay,
            ..
        } = self;
        let window = PairWindow {
            pair_ms: config.pair_ms,
            pair_window_ms: config.pair_window_ms,
            potentiation: config.potentiation,
            depression: config.depression,
            trace_ms: config.trace_ms,
        };
        let spikes = &spikes[..count];
        let slots = traces.len();
        let index = SlotArrays {
            incoming,
            outgoing,
            sources,
            slot_targets,
            pair_decay,
            trace_decay,
        };

        let Some((pool, bounds)) =
            shards.filter(|(pool, bounds)| bounds.len() == pool.workers() + 1)
        else {
            observe_shard::<false>(
                traces, touched, 0, slots, spikes, last_spike, ms, &index, &window,
            );
            return;
        };
        let traces = SharedSlice::new(traces);
        let touched = SharedSlice::new(touched);
        pool.broadcast(&|worker| {
            let (from, to) = (bounds[worker] as usize, bounds[worker + 1] as usize);
            // SAFETY: `bounds` is ascending and ends at `slots`, so worker `w` owns
            // `bounds[w]..bounds[w + 1]` and no two workers' ranges overlap. `broadcast` runs each
            // index exactly once and returns only after every borrow here has been dropped.
            let (traces, touched) = unsafe { (traces.range(from, to), touched.range(from, to)) };
            observe_shard::<true>(
                traces,
                touched,
                from,
                to,
                spikes,
                last_spike,
                ms,
                &index,
                &window,
            );
        });
    }

    pub fn reinforce(&mut self, reward: f64, ms: f64) {
        if !self.enabled || !reward.is_finite() || reward == 0.0 {
            return;
        }
        let learning_rate = self.config.learning_rate;
        let restoring = self.config.restoring;
        let min_gain = self.config.min_gain;
        let max_gain = self.config.max_gain;
        let trace_ms = self.config.trace_ms;
        self.signal = tanh(reward);
        let signal = self.signal;
        let Self {
            traces,
            touched,
            gains,
            trace_decay,
            ..
        } = self;
        let mut changed = false;
        for slot in 0..gains.len() {
            write_trace(traces, touched, trace_decay, trace_ms, slot, ms, 0.0);
            let old = gains[slot];
            // Small restoring term limits long-run drift; excitation never changes sign.
            let next = f64::from(old) + learning_rate * signal * f64::from(traces[slot])
                - restoring * (f64::from(old) - 1.0);
            gains[slot] = js_max(min_gain, js_min(max_gain, next)) as f32;
            changed |= gains[slot] != old;
        }
        if changed {
            self.updates += 1.0;
        }
    }

    pub fn statistics(&self) -> LearningStats {
        let mut changed = 0u64;
        let mut sum = 0.0f64;
        let mut max = 0.0f64;
        let mut mushroom = 0u64;
        for index in 0..self.gains.len() {
            let delta = (f64::from(self.gains[index]) - 1.0).abs();
            if delta > 0.000001 {
                changed += 1;
            }
            sum += delta;
            max = js_max(max, delta);
            mushroom += u64::from(self.selected[index]);
        }
        let synapses = self.edges.len();
        LearningStats {
            version: self.version.clone(),
            enabled: self.enabled,
            synapses,
            mushroom,
            output: synapses as u64 - mushroom,
            updates: self.updates,
            changed,
            mean_change: sum / if synapses == 0 { 1.0 } else { synapses as f64 },
            max_change: max,
            signal: self.signal,
        }
    }

    pub fn export_state(&self) -> PlasticityState {
        PlasticityState {
            version: self.version.clone(),
            topology: self.topology,
            enabled: self.enabled,
            updates: self.updates,
            signal: self.signal,
            gains: self.gains.clone(),
            traces: self.traces.clone(),
            touched: self.touched.clone(),
        }
    }

    /// Validate everything before mutating anything.
    pub fn import_state(&mut self, state: Option<&PlasticityState>) -> Result<()> {
        let Some(state) = state else {
            self.gains.fill(1.0);
            self.traces.fill(0.0);
            self.touched.fill(0.0);
            self.updates = 0.0;
            self.signal = 0.0;
            return Ok(());
        };
        if state.version != self.version || state.topology != self.topology {
            bail!("Incompatible plasticity topology/version");
        }
        if state.gains.len() != self.gains.len()
            || state.traces.len() != self.traces.len()
            || state.touched.len() != self.touched.len()
        {
            bail!("Invalid plasticity dimensions");
        }
        if state.updates.fract() != 0.0
            || !state.updates.is_finite()
            || state.updates < 0.0
            || !state.signal.is_finite()
            || state.signal.abs() > 1.0
        {
            bail!("Invalid plasticity metadata");
        }
        // 0.100001 for the default clamps: the widest legal displacement plus a float32 tolerance.
        let radius = js_max(1.0 - self.config.min_gain, self.config.max_gain - 1.0) + 0.000001;
        for index in 0..self.gains.len() {
            let gain = f64::from(state.gains[index]);
            let trace = f64::from(state.traces[index]);
            let touched = state.touched[index];
            if !gain.is_finite()
                || (gain - 1.0).abs() > radius
                || !trace.is_finite()
                || trace.abs() > 1.0
                || !touched.is_finite()
                || touched < 0.0
            {
                bail!("Invalid plasticity values");
            }
        }
        self.gains.copy_from_slice(&state.gains);
        self.traces.copy_from_slice(&state.traces);
        self.touched.copy_from_slice(&state.touched);
        self.enabled = state.enabled;
        self.updates = state.updates;
        self.signal = state.signal;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table hit has to be the same bits as the call it replaces, and a miss has to fall through.
    #[test]
    fn the_exp_tables_are_a_lookup_not_an_approximation() {
        for scale in [20.0f64, 5000.0, 1.0 / 3.0] {
            let table: Vec<f64> = (0..256).map(|k| exp(-(k as f64) / scale)).collect();
            for k in 0..256 {
                assert_eq!(
                    table_exp(&table, k as f64, scale).to_bits(),
                    exp(-(k as f64) / scale).to_bits(),
                    "integer {k} at scale {scale}"
                );
            }
            // Past the end, and non-integers inside it, go to `exp`.
            for argument in [256.0, 1e9, 0.5, 12.25, -3.0, f64::NAN, f64::INFINITY] {
                let want = exp(-argument / scale);
                let got = table_exp(&table, argument, scale);
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "non-hit {argument} at scale {scale}"
                );
            }
        }
    }
}
