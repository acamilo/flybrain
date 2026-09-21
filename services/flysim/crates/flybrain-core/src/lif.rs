//! Leaky integrate-and-fire network over a connectome dataset, stepped in 1-ms ticks.
//!
//! Bit-exact port of `model/lif.ts`. Every float store is written as an `f64` expression rounded
//! once with `as f32`, because that is what a `Float32Array` store does in JavaScript; see
//! [`crate::jsmath`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::dataset::BrainDataset;
use crate::error::{bail, Result};
use crate::jsmath::{exp, fround, js_max};
use crate::ordered::NumberMap;
use crate::plasticity::{PlasticityConfig, PlasticityState, RewardModulatedStdp};
use crate::pool::{SharedSlice, WorkerPool};
use crate::retina::{project_frame, RetinaColumns, RetinaConfig, DEFAULT_RETINA_CONFIG};
use crate::rng::Xorshift32;
use crate::version::version_for;

/// The same tick on the GPU, bit-exact with this kernel. Behind the `cuda` feature.
#[cfg(feature = "cuda")]
pub mod cuda;

/// Version of the default configuration; kept verbatim so existing checkpoints stay loadable.
pub const NEURAL_KERNEL_VERSION: &str = "lif-1ms-f64-v2";

/// Rate roles tracked by default, on top of every `command_*` role the dataset declares.
/// Prefix of the macro-type populations, tracked by default like the `command_*` buttons.
///
/// `docs/design/macros.md` section 12: a macro is a button, pressed by its own population, so its
/// rate is read exactly as a button's is. A dataset that predates the roles has none of them, and
/// a checkpoint that predates them restores them at zero (`import_state`).
const MACRO_RATE_ROLE_PREFIX: &str = "macro_";

const DEFAULT_EXTRA_RATE_ROLES: [&str; 6] = [
    "steer_left",
    "steer_right",
    "forward",
    "backward",
    "proboscis",
    "reward_pam",
];

/// Rate roles are packed into a `u64` bitmask, one bit per role.
///
/// 64 rather than 32 since `docs/design/macros.md` section 11: the eight `command_*` buttons and
/// the six historical motor/reward roles are fourteen, and the twenty-two `macro_<type>`
/// populations bring the default set to thirty-six. A role's bit index only decides which counter
/// a spike lands in, so the rates are untouched and the kernel version does not move; the
/// TypeScript oracle carries the same 64 in two 32-bit words (`model/lif.ts`).
pub const MAX_RATE_ROLES: usize = 64;

/// Role driven by `stimulate()` (the reward pulse) and the drive it receives per tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Stimulation {
    pub role: String,
    pub drive: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifConfig {
    /// Membrane decay time constant in milliseconds; decay per tick is `fround(exp(-1/decayMs))`.
    pub decay_ms: f64,
    /// Spike threshold in membrane units.
    pub threshold: f64,
    /// Ticks a neuron stays refractory after spiking.
    pub refractory_ms: u32,
    /// Multiplier applied to dataset weights when a spike propagates.
    pub synapse_scale: f64,
    /// Upper bound of the per-neuron random baseline drive.
    pub baseline_max: f64,
    /// Random membrane kicks per tick.
    pub noise_kicks: usize,
    /// Membrane increment per noise kick.
    pub noise_amount: f64,
    /// Exponential-moving-average coefficient for rate estimates.
    pub rate_alpha: f64,
    /// Lower clamp on the membrane after inhibitory input.
    pub membrane_floor: f64,
    /// Seed of the deterministic noise generator.
    pub seed: i32,
    pub stimulation: Stimulation,
    /// Roles whose population rate is tracked. `None` defaults to every `command_*` role plus the
    /// historical motor/reward roles, in dataset role order.
    pub rate_roles: Option<Vec<String>>,
    /// Retina gain and the frame size assumed by `set_visual_frame` when none is given.
    pub retina: RetinaConfig,
}

/// Original constants of the FAFB kernel.
impl Default for LifConfig {
    fn default() -> Self {
        Self {
            decay_ms: 20.0,
            threshold: 1.0,
            refractory_ms: 2,
            synapse_scale: 0.005,
            baseline_max: 0.06,
            noise_kicks: 300,
            noise_amount: 0.42,
            rate_alpha: 1.0 / 25.0,
            membrane_floor: -2.0,
            seed: 22_222,
            stimulation: Stimulation {
                role: "reward_pam".to_string(),
                drive: 0.20,
            },
            rate_roles: None,
            retina: DEFAULT_RETINA_CONFIG,
        }
    }
}

impl LifConfig {
    /// Numeric parameters of the kernel, in version-hash order. Role names never enter the hash.
    fn version_params(&self) -> [f64; 14] {
        [
            self.decay_ms,
            self.threshold,
            f64::from(self.refractory_ms),
            self.synapse_scale,
            self.baseline_max,
            self.noise_kicks as f64,
            self.noise_amount,
            self.rate_alpha,
            self.membrane_floor,
            f64::from(self.seed),
            self.stimulation.drive,
            self.retina.gain,
            f64::from(self.retina.width),
            f64::from(self.retina.height),
        ]
    }
}

/// `NEURAL_KERNEL_VERSION` for the default constants, otherwise `lif-1ms-f64-v2:<fnv1a32>`.
pub fn kernel_version(config: &LifConfig) -> String {
    version_for(
        NEURAL_KERNEL_VERSION,
        &format!("{NEURAL_KERNEL_VERSION}:"),
        &config.version_params(),
        &LifConfig::default().version_params(),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifState {
    pub membrane: Vec<f32>,
    pub refractory: Vec<u8>,
    pub last_spike_ms: Vec<f64>,
    pub visual_drive: Vec<f32>,
    pub rng: i32,
    pub reward_remaining: f64,
    pub ms: f64,
    pub population_rate: f64,
    pub rates: NumberMap,
    pub plasticity: PlasticityState,
}

/// How the per-millisecond parallel phases are partitioned.
///
/// Two phases are parallel — the neuron sweep and spike propagation — and both are partitioned into
/// fixed contiguous index ranges that depend on nothing but the dataset and the worker count. The
/// sweep's per-range spike lists are concatenated in range order, so the spike array is the
/// sequential one whatever the thread count, and propagation's shards preserve each target's own
/// addition order (see [`propagate_shard`]). Everything else stays sequential in the original
/// order: the noise kicks, the visual and stimulation drive, `plasticity.observe`, the role tally
/// and the rate EMAs.
///
/// `threads = 1` and [`SweepPlan::sequential`] both bypass the pool entirely and run the phases
/// inline, and the pool is built explicitly by [`SweepPlan::with_threads`] rather than being a
/// process-wide default, so a host that embeds the core keeps control of its own threads.
#[derive(Clone, Default)]
pub struct SweepPlan {
    pool: Option<Arc<WorkerPool>>,
    /// Below this many neurons the phases run sequentially; the results are identical either way.
    min_parallel_neurons: usize,
}

impl std::fmt::Debug for SweepPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SweepPlan")
            .field("threads", &self.threads())
            .field("min_parallel_neurons", &self.min_parallel_neurons)
            .finish()
    }
}

impl SweepPlan {
    /// Run every phase on the calling thread.
    pub fn sequential() -> Self {
        Self::default()
    }

    /// Build a persistent pool of `threads` workers, one of which is the calling thread.
    ///
    /// `threads = 1` builds no pool at all, so the one-thread mode is the sequential path plus the
    /// shard bookkeeping and is the reference the pool has to beat.
    pub fn with_threads(threads: usize) -> Result<Self> {
        if threads <= 1 {
            return Ok(Self {
                pool: None,
                min_parallel_neurons: 1,
            });
        }
        let pool = WorkerPool::new(threads, "flysim-sweep").map_err(|error| {
            crate::error::Error::new(format!("Unable to build a thread pool: {error}"))
        })?;
        Ok(Self {
            pool: Some(Arc::new(pool)),
            min_parallel_neurons: 1,
        })
    }

    /// Run the phases sequentially below this many neurons, where the fan-out costs more than it
    /// saves. Results are identical either way, so this is purely a performance knob.
    pub fn min_parallel_neurons(mut self, neurons: usize) -> Self {
        self.min_parallel_neurons = neurons;
        self
    }

    pub fn threads(&self) -> usize {
        self.pool.as_ref().map_or(1, |pool| pool.workers())
    }
}

/// Per-phase wall-clock totals in nanoseconds, plus the number of ticks they cover.
///
/// Accumulated only while [`LifNetwork::profile`] is set. Five `Instant::now()` calls per tick is
/// about 125 ns against a tick of 150 us and up, so the instrumented figures are within a tenth of
/// a percent of the uninstrumented ones — which is why `examples/ablate.rs` reads the phase split
/// straight off this instead of inferring it by subtracting whole runs of differently-behaving
/// networks from each other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhaseTimings {
    pub ticks: u64,
    /// Noise kicks, visual drive and stimulation.
    pub drive_ns: u64,
    pub sweep_ns: u64,
    pub observe_ns: u64,
    pub propagate_ns: u64,
    /// The role tally, the rate EMAs and the clock increment.
    pub rates_ns: u64,
}

impl PhaseTimings {
    pub fn total_ns(&self) -> u64 {
        self.drive_ns + self.sweep_ns + self.observe_ns + self.propagate_ns + self.rates_ns
    }
}

/// A lap timer that compiles to a single branch per phase when profiling is off.
struct PhaseClock(Option<std::time::Instant>);

impl PhaseClock {
    #[inline]
    fn start(profile: bool) -> Self {
        Self(profile.then(std::time::Instant::now))
    }

    #[inline]
    fn lap(&mut self, slot: &mut u64) {
        if let Some(previous) = self.0 {
            let now = std::time::Instant::now();
            *slot += now.duration_since(previous).as_nanos() as u64;
            self.0 = Some(now);
        }
    }
}

/// A network, its plasticity and the noise stream.
pub struct LifNetwork {
    pub config: LifConfig,
    /// Version string of this configuration; pin it in checkpoints.
    pub version: String,
    pub plasticity: RewardModulatedStdp,
    pub membrane: Vec<f32>,
    pub refractory: Vec<u8>,
    pub baseline: Vec<f32>,
    pub last_spike_ms: Vec<f64>,
    pub rates: NumberMap,
    pub role_names: Vec<String>,
    pub role_masks: Vec<u64>,
    pub ms: f64,
    pub population_rate: f64,
    pub data: Arc<BrainDataset>,
    /// Propagation shard boundaries, length `workers + 1`: worker `w` owns targets
    /// `target_shards[w]..target_shards[w + 1]`. Derived from the plan and the in-degrees.
    target_shards: Vec<u32>,
    /// `observe` shard boundaries over the plastic slots, length `workers + 1`
    /// (`RewardModulatedStdp::slot_shards`).
    slot_shards: Vec<u32>,
    /// Whether every CSR row is target-ascending, so a shard can binary-search its window.
    rows_ascending: bool,
    /// One spike count per sweep worker, written by that worker and read after the phase.
    sweep_counts: Vec<AtomicUsize>,
    /// Accumulate the per-phase timings in [`LifNetwork::timings`]. Off by default.
    pub profile: bool,
    timings: PhaseTimings,
    decay: f64,
    refractory_reset: u8,
    stimulation_targets: Vec<u32>,
    /// Sizes of the tracked roles, in role order.
    role_sizes: Vec<usize>,
    spikes: Vec<u32>,
    /// Per-range spike staging for the parallel sweep; compacted into `spikes` in range order.
    spikes_scratch: Vec<u32>,
    role_counts: Vec<u32>,
    rng: Xorshift32,
    visual_drive: Vec<f32>,
    reward_remaining: f64,
    sweep: SweepPlan,
    /// When set, [`LifNetwork::step`] runs steps 1-4 and 6 on the GPU; see [`cuda`].
    #[cfg(feature = "cuda")]
    cuda: Option<Box<cuda::CudaLif>>,
}

/// Deliberately terse: the arrays are up to 139,255 entries long, so printing them would bury
/// whatever the reader was actually looking for.
impl std::fmt::Debug for LifNetwork {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifNetwork")
            .field("version", &self.version)
            .field("dataset", &self.data.meta.dataset)
            .field("neurons", &self.membrane.len())
            .field("ms", &self.ms)
            .field("populationRate", &self.population_rate)
            .field("rewardRemaining", &self.reward_remaining)
            .field("rng", &self.rng.state())
            .field("roles", &self.role_names)
            .field("plasticEdges", &self.plasticity.edges.len())
            .field("sweep", &self.sweep)
            .finish()
    }
}

impl LifNetwork {
    pub fn new(
        data: Arc<BrainDataset>,
        config: LifConfig,
        plasticity: PlasticityConfig,
    ) -> Result<Self> {
        let version = kernel_version(&config);
        let decay = fround(exp(-1.0 / config.decay_ms));
        let plasticity = RewardModulatedStdp::new(&data, plasticity);
        let neurons = data.meta.neurons;

        let role_names: Vec<String> = match &config.rate_roles {
            Some(requested) => requested
                .iter()
                .filter(|name| data.meta.roles.contains_key(name.as_str()))
                .cloned()
                .collect(),
            None => data
                .meta
                .roles
                .keys()
                .filter(|name| {
                    name.starts_with("command_")
                        || name.starts_with(MACRO_RATE_ROLE_PREFIX)
                        || DEFAULT_EXTRA_RATE_ROLES.contains(&name.as_str())
                })
                .cloned()
                .collect(),
        };
        if role_names.len() > MAX_RATE_ROLES {
            bail!(
                "Too many tracked rate roles ({}); at most {MAX_RATE_ROLES} fit the role bitmask",
                role_names.len()
            );
        }

        let mut rates = NumberMap::new();
        let mut role_masks = vec![0u64; neurons];
        let mut role_sizes = Vec::with_capacity(role_names.len());
        for (role, name) in role_names.iter().enumerate() {
            rates.set(name, 0.0);
            let neurons_of_role = data.role(name);
            role_sizes.push(neurons_of_role.len());
            for neuron in neurons_of_role {
                role_masks[*neuron as usize] |= 1u64 << role;
            }
        }

        let plastic_slots = plasticity.gains.len() as u32;
        let mut rng = Xorshift32::new(config.seed);
        let mut baseline = vec![0.0f32; neurons];
        for slot in baseline.iter_mut() {
            *slot = (rng.next_f64() * config.baseline_max) as f32;
        }

        Ok(Self {
            membrane: vec![0.0; neurons],
            refractory: vec![0; neurons],
            baseline,
            last_spike_ms: vec![-1_000_000.0; neurons],
            visual_drive: vec![0.0; data.visual_indices.len()],
            spikes: vec![0; neurons],
            spikes_scratch: vec![0; neurons],
            role_counts: vec![0; role_names.len()],
            stimulation_targets: data.role(&config.stimulation.role).to_vec(),
            // `refractory[neuron] = refractoryMs` stores through a Uint8Array: ToUint8.
            refractory_reset: (config.refractory_ms & 0xff) as u8,
            rates,
            role_names,
            role_masks,
            role_sizes,
            decay,
            rng,
            reward_remaining: 0.0,
            ms: 0.0,
            population_rate: 0.0,
            plasticity,
            version,
            config,
            target_shards: vec![0, neurons as u32],
            slot_shards: vec![0, plastic_slots],
            rows_ascending: false,
            sweep_counts: vec![AtomicUsize::new(0)],
            profile: false,
            timings: PhaseTimings::default(),
            data,
            sweep: SweepPlan::sequential(),
            #[cfg(feature = "cuda")]
            cuda: None,
        })
    }

    /// Construct with the default configuration.
    pub fn with_defaults(data: Arc<BrainDataset>) -> Result<Self> {
        Self::new(data, LifConfig::default(), PlasticityConfig::default())
    }

    /// Choose how the per-tick parallel phases are partitioned. Results do not depend on this.
    pub fn set_sweep_plan(&mut self, sweep: SweepPlan) {
        let threads = sweep.threads();
        self.target_shards = target_shards(&self.data, threads);
        self.slot_shards = self.plasticity.slot_shards(threads);
        self.rows_ascending = threads > 1 && rows_are_target_ascending(&self.data);
        self.sweep_counts = (0..threads).map(|_| AtomicUsize::new(0)).collect();
        self.sweep = sweep;
        // Spike hook: with the `cuda` feature compiled in *and* `FLY_LIF_CUDA=1` in the
        // environment, configuring a sweep plan also attaches the GPU backend. That is how the
        // existing golden suite is run through the GPU without editing a single test. Both
        // conditions are off by default, and with the feature absent this line does not exist.
        #[cfg(feature = "cuda")]
        self.attach_cuda_if_requested();
    }

    pub fn sweep_plan(&self) -> &SweepPlan {
        &self.sweep
    }

    /// The last tick's spike list occupies the first `n` entries, where `n` is the count
    /// [`LifNetwork::step`] returned for a one-tick call. Read-only; exposed so the CUDA
    /// equivalence tests can compare the list itself and not just its effects.
    pub fn spike_buffer(&self) -> &[u32] {
        &self.spikes
    }

    pub fn visual_drive(&self) -> &[f32] {
        &self.visual_drive
    }

    pub fn reward_remaining(&self) -> f64 {
        self.reward_remaining
    }

    pub fn rng_state(&self) -> i32 {
        self.rng.state()
    }

    /// Project one RGBA frame onto the retina columns; the size defaults to `config.retina`.
    pub fn set_visual_frame(&mut self, rgba: &[u8], width: u32, height: u32) {
        let count = self.data.visual_indices.len();
        if self.visual_drive.len() != count {
            self.visual_drive = vec![0.0; count];
        }
        project_frame(
            rgba,
            width,
            height,
            RetinaColumns {
                xy: &self.data.visual_xy,
                hemisphere: &self.data.visual_hemisphere,
                count,
            },
            self.config.retina.gain,
            &mut self.visual_drive,
        );
    }

    /// `set_visual_frame` at the configured retina size.
    pub fn set_visual_frame_default_size(&mut self, rgba: &[u8]) {
        let (width, height) = (self.config.retina.width, self.config.retina.height);
        self.set_visual_frame(rgba, width, height);
    }

    /// Drive the stimulation role for `duration_ms` ticks; overlapping pulses take the maximum.
    pub fn stimulate(&mut self, duration_ms: f64) {
        self.reward_remaining = js_max(self.reward_remaining, duration_ms);
    }

    /// Historical name of [`LifNetwork::stimulate`].
    pub fn reward(&mut self, duration_ms: f64) {
        self.stimulate(duration_ms);
    }

    /// Run `milliseconds` ticks and return the total spike count.
    pub fn step(&mut self, milliseconds: u64) -> u64 {
        // The GPU backend runs the same ticks in batches and replays the host-side phases; with the
        // feature off this compiles to nothing and the loop below is the whole method.
        #[cfg(feature = "cuda")]
        if self.cuda.is_some() {
            return self.step_on_cuda(milliseconds);
        }
        let mut total = 0u64;
        for _ in 0..milliseconds {
            total += self.step_one() as u64;
        }
        total
    }

    fn step_one(&mut self) -> usize {
        let neurons = self.data.meta.neurons;
        let threshold = self.config.threshold;
        let synapse_scale = self.config.synapse_scale;
        let noise_amount = self.config.noise_amount;
        let rate_alpha = self.config.rate_alpha;
        let membrane_floor = self.config.membrane_floor;
        let decay = self.decay;
        let mut clock = PhaseClock::start(self.profile);

        // 1. Noise. Indices can repeat, so 300 kicks are 300 draws, not 300 distinct neurons.
        for _ in 0..self.config.noise_kicks {
            let neuron = (self.rng.next_uint() as usize) % neurons;
            self.membrane[neuron] = (f64::from(self.membrane[neuron]) + noise_amount) as f32;
        }

        // 2. Visual drive.
        for (index, column) in self.data.visual_indices.iter().enumerate() {
            let neuron = *column as usize;
            self.membrane[neuron] =
                (f64::from(self.membrane[neuron]) + f64::from(self.visual_drive[index])) as f32;
        }

        // 3. Stimulation.
        if self.reward_remaining > 0.0 {
            let drive = self.config.stimulation.drive;
            for neuron in &self.stimulation_targets {
                let neuron = *neuron as usize;
                self.membrane[neuron] = (f64::from(self.membrane[neuron]) + drive) as f32;
            }
            self.reward_remaining -= 1.0;
        }

        clock.lap(&mut self.timings.drive_ns);

        // 4. Integrate and fire, in ascending neuron index order.
        let spike_count = self.sweep(neurons, decay, threshold);
        clock.lap(&mut self.timings.sweep_ns);

        // 5. Observe pairs, before lastSpikeMs is updated for this tick.
        self.role_counts.fill(0);
        // Sharded by slot range when there is a pool, which is bit-identical to the sequential
        // walk for any worker count (`plasticity::observe_shard`).
        let pool = self
            .sweep
            .pool
            .clone()
            .filter(|_| neurons >= self.sweep.min_parallel_neurons);
        let shards = pool
            .as_deref()
            .map(|pool| (pool, self.slot_shards.as_slice()));
        self.plasticity.observe_sharded(
            &self.spikes,
            spike_count,
            &self.last_spike_ms,
            self.ms,
            shards,
        );
        clock.lap(&mut self.timings.observe_ns);

        // 6. Propagate, per spiking neuron in the order recorded (ascending index).
        //
        // The spike stamp and the role tally are hoisted out of the edge walk. Neither is read by
        // propagation — `observe` has already run against the *previous* stamps — so doing them
        // first changes nothing, and it leaves the edge walk as one self-contained function that a
        // worker can run over a slice of `membrane`.
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
        // The unsharded walk is written out here rather than as a one-shard call, so the compiler
        // sees `offset = 0` and `limit = neurons` as constants and folds the window search away.
        if !self.propagate_sharded(neurons, spike_count, synapse_scale, membrane_floor) {
            propagate_shard::<true>(
                &mut self.membrane,
                0,
                neurons,
                &self.spikes[..spike_count],
                &self.data.indptr,
                &self.data.targets,
                &self.data.weights,
                &self.plasticity,
                synapse_scale,
                membrane_floor,
            );
        }
        clock.lap(&mut self.timings.propagate_ns);

        // 7. Rate EMAs.
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

        // 8. ms++
        self.ms += 1.0;
        clock.lap(&mut self.timings.rates_ns);
        self.timings.ticks += 1;
        spike_count
    }

    /// Per-phase wall-clock totals since the last [`LifNetwork::reset_timings`].
    ///
    /// All zero unless `profile` was set; see [`PhaseTimings`].
    pub fn timings(&self) -> PhaseTimings {
        self.timings
    }

    pub fn reset_timings(&mut self) {
        self.timings = PhaseTimings::default();
    }

    /// Spike propagation, sharded by target range.
    ///
    /// Each worker owns a contiguous, disjoint slice of `membrane` and walks the whole spike list,
    /// applying the window of each source's CSR row that lands in its own range. See
    /// [`propagate_shard`] for why that is bit-identical to the sequential source-major walk.
    ///
    /// The shard bounds are balanced by in-degree rather than by neuron count, because in-degree
    /// spans 0..5,080 and an even index split leaves one worker with several times the work.
    /// Returns false when no pool is configured, so the caller runs the sequential walk.
    fn propagate_sharded(
        &mut self,
        neurons: usize,
        spike_count: usize,
        synapse_scale: f64,
        membrane_floor: f64,
    ) -> bool {
        let Some(pool) = self
            .sweep
            .pool
            .clone()
            .filter(|_| neurons >= self.sweep.min_parallel_neurons)
        else {
            return false;
        };

        let spikes = &self.spikes[..spike_count];
        let bounds = &self.target_shards;
        let indptr = &self.data.indptr;
        let targets = &self.data.targets;
        let weights = &self.data.weights;
        let plasticity = &self.plasticity;
        let windowed = self.rows_ascending;
        let membrane = SharedSlice::new(&mut self.membrane);
        pool.broadcast(&|worker| {
            let (offset, limit) = (bounds[worker] as usize, bounds[worker + 1] as usize);
            // SAFETY: `bounds` is ascending, so worker `w` owns `bounds[w]..bounds[w + 1]` and no
            // two workers' ranges overlap. `broadcast` runs each index exactly once and returns
            // only after every borrow here has been dropped.
            let shard = unsafe { membrane.range(offset, limit) };
            if windowed {
                propagate_shard::<true>(
                    shard,
                    offset,
                    limit,
                    spikes,
                    indptr,
                    targets,
                    weights,
                    plasticity,
                    synapse_scale,
                    membrane_floor,
                );
            } else {
                propagate_shard::<false>(
                    shard,
                    offset,
                    limit,
                    spikes,
                    indptr,
                    targets,
                    weights,
                    plasticity,
                    synapse_scale,
                    membrane_floor,
                );
            }
        });
        true
    }

    /// The neuron update sweep.
    fn sweep(&mut self, neurons: usize, decay: f64, threshold: f64) -> usize {
        let refractory_reset = self.refractory_reset;

        let Some(pool) = self
            .sweep
            .pool
            .clone()
            .filter(|_| neurons >= self.sweep.min_parallel_neurons)
        else {
            return sweep_range(
                0,
                &mut self.membrane,
                &mut self.refractory,
                &self.baseline,
                decay,
                threshold,
                refractory_reset,
                &mut self.spikes,
            );
        };

        // Fixed contiguous ranges: the chunk length depends only on the neuron count and the
        // worker count, each range writes its spikes into its own slice of a scratch buffer, and
        // the slices are compacted in range order. The spike array is therefore the sequential
        // one for every thread count.
        let workers = pool.workers();
        let chunk = neurons.div_ceil(workers).max(1);
        let baseline = &self.baseline;
        let counts = &self.sweep_counts;
        let membrane = SharedSlice::new(&mut self.membrane);
        let refractory = SharedSlice::new(&mut self.refractory);
        let scratch = SharedSlice::new(&mut self.spikes_scratch);
        pool.broadcast(&|worker| {
            let offset = (chunk * worker).min(neurons);
            let limit = (offset + chunk).min(neurons);
            // SAFETY: the ranges are `chunk`-aligned and disjoint by construction, one per worker,
            // and `broadcast` runs each index exactly once.
            let count = unsafe {
                sweep_range(
                    offset,
                    membrane.range(offset, limit),
                    refractory.range(offset, limit),
                    &baseline[offset..limit],
                    decay,
                    threshold,
                    refractory_reset,
                    scratch.range(offset, limit),
                )
            };
            counts[worker].store(count, Ordering::Relaxed);
        });

        let mut total = 0usize;
        for worker in 0..workers {
            let count = self.sweep_counts[worker].load(Ordering::Relaxed);
            // Clamped, because a pool with more workers than chunks leaves the tail empty.
            let from = (chunk * worker).min(neurons);
            self.spikes[total..total + count]
                .copy_from_slice(&self.spikes_scratch[from..from + count]);
            total += count;
        }
        total
    }

    pub fn export_state(&self) -> LifState {
        LifState {
            membrane: self.membrane.clone(),
            refractory: self.refractory.clone(),
            last_spike_ms: self.last_spike_ms.clone(),
            visual_drive: self.visual_drive.clone(),
            rng: self.rng.state(),
            reward_remaining: self.reward_remaining,
            ms: self.ms,
            population_rate: self.population_rate,
            rates: self.rates.clone(),
            plasticity: self.plasticity.export_state(),
        }
    }

    /// Import plasticity first, then validate before writing anything.
    pub fn import_state(&mut self, state: &LifState) -> Result<()> {
        if state.membrane.len() != self.membrane.len()
            || state.refractory.len() != self.refractory.len()
            || state.last_spike_ms.len() != self.last_spike_ms.len()
        {
            bail!("Brain checkpoint dimensions do not match the loaded dataset");
        }
        let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
        let finite32 = |values: &[f32]| values.iter().all(|value| value.is_finite());
        if state.visual_drive.len() != self.data.visual_indices.len()
            || !is_safe_integer(state.ms)
            || state.ms < 0.0
            // `!Number.isInteger(state.rng)` needs no runtime check: the state's type is i32.
            || !state.population_rate.is_finite()
            || !state.reward_remaining.is_finite()
            || state.reward_remaining < 0.0
            || state.rates.values().any(|value| !value.is_finite())
            || !finite32(&state.membrane)
            || !finite(&state.last_spike_ms)
            || !finite32(&state.visual_drive)
        {
            bail!("Invalid neural checkpoint values");
        }
        self.plasticity.import_state(Some(&state.plasticity))?;
        self.membrane.copy_from_slice(&state.membrane);
        self.refractory.copy_from_slice(&state.refractory);
        self.last_spike_ms.copy_from_slice(&state.last_spike_ms);
        self.visual_drive = state.visual_drive.clone();
        self.rng.set_state(state.rng);
        self.reward_remaining = state.reward_remaining;
        self.ms = state.ms;
        self.population_rate = state.population_rate;
        // Missing rate roles import as 0.
        for name in &self.role_names {
            let value = state.rates.get(name).unwrap_or(0.0);
            self.rates.set(name, value);
        }
        #[cfg(feature = "cuda")]
        if let Some(backend) = self.cuda.as_mut() {
            backend.mark_host_dirty();
        }
        Ok(())
    }
}

/// Propagation shard boundaries for `workers` workers: `workers + 1` neuron indices, ascending,
/// starting at 0 and ending at the neuron count.
///
/// Split so each worker receives about the same number of *incoming* edges, not the same number of
/// neurons: in-degree runs from 0 to 5,080 on `fafb-v783`, so an even index split would leave one
/// worker several times behind the rest and the whole phase waits for it. The split depends only on
/// the dataset and the worker count, and results do not depend on it at all — a different split
/// only moves which worker applies an edge, never the order a target sees its own edges in.
fn target_shards(data: &BrainDataset, workers: usize) -> Vec<u32> {
    let neurons = data.meta.neurons;
    let mut bounds = vec![0u32; workers.max(1) + 1];
    let last = bounds.len() - 1;
    bounds[last] = neurons as u32;
    if workers <= 1 {
        return bounds;
    }
    let mut in_degree = vec![0u32; neurons];
    for target in &data.targets {
        in_degree[*target as usize] += 1;
    }
    let total = data.targets.len() as u64;
    if total == 0 {
        // An edge-free connectome: split the index range evenly. Nothing is applied either way.
        let chunk = neurons.div_ceil(workers);
        for (worker, bound) in bounds.iter_mut().enumerate().take(workers).skip(1) {
            *bound = (chunk * worker).min(neurons) as u32;
        }
        return bounds;
    }
    let mut cumulative = 0u64;
    let mut next = 1usize;
    for (neuron, degree) in in_degree.iter().copied().enumerate() {
        if next >= workers {
            break;
        }
        cumulative += u64::from(degree);
        while next < workers && cumulative * workers as u64 >= total * next as u64 {
            bounds[next] = neuron as u32 + 1;
            next += 1;
        }
    }
    for bound in bounds.iter_mut().take(workers).skip(next) {
        *bound = neurons as u32;
    }
    bounds
}

/// `js_max(floor, value)`, with the common case first.
///
/// `js_max` is NaN-propagating and has a signed-zero rule, both of which matter and neither of
/// which is reachable when the value clears the floor; that is every edge but a handful, so test
/// for it once and delegate the rest.
#[inline]
fn floor_clamp(value: f64, floor: f64) -> f64 {
    if value > floor {
        value
    } else {
        js_max(floor, value)
    }
}

/// Apply the edges of one tick's spikes that land in `membrane`, which covers global neuron
/// indices `offset..limit`.
///
/// # Determinism
///
/// **For a fixed target the sequence of additions is the sequential one, whatever the shard
/// bounds.** The sequential kernel visits `(source, edge)` in source-major order and, within a
/// source, in ascending CSR slot order. A shard walks the same spike list in the same order and the
/// same rows in the same order, and applies exactly the edges whose target lies in
/// `offset..limit` — a subsequence of the sequential edge sequence that keeps every pair's relative
/// order. Restricting that subsequence to one target therefore yields the same sequence of
/// `(weight, gain)` values in the same order as the sequential run restricted to that target, and
/// `membrane[target]` depends on nothing else: each step reads only its own previous value, so the
/// f32 rounding and the per-edge floor clamp happen at the same points with the same operands.
/// Targets in different shards never interact, so the whole array matches bit for bit.
///
/// Note that this argument does not need the row to contain a target at most once, nor the shards
/// to be balanced, nor the worker count to be stable — only that every shard traverses the spike
/// list and each row in the sequential order.
///
/// `WINDOWED` is a pure optimization on top of that. When the rows are target-ascending the shard's
/// edges are one contiguous window, found by binary search, so each edge is read by exactly one
/// worker; otherwise every worker reads every row and filters per edge. Both apply the same set of
/// edges in the same order.
// `inline(always)` and not `inline`: the unsharded caller passes `offset = 0` and
// `limit = neurons`, and only after inlining can the window search fold away. Leaving it to the
// inliner's judgement costs 8% of the sequential real-time factor, measured.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn propagate_shard<const WINDOWED: bool>(
    membrane: &mut [f32],
    offset: usize,
    limit: usize,
    spikes: &[u32],
    indptr: &[u32],
    targets: &[u32],
    weights: &[i16],
    plasticity: &RewardModulatedStdp,
    synapse_scale: f64,
    membrane_floor: f64,
) {
    if membrane.is_empty() {
        return;
    }
    let words = plasticity.slot_words();
    for source in spikes.iter().copied() {
        let source = source as usize;
        let row_from = indptr[source] as usize;
        let row_to = indptr[source + 1] as usize;
        let row = &targets[row_from..row_to];
        let Some(last) = row.last() else {
            continue;
        };
        let (from, to) = if WINDOWED {
            // Target-ascending rows, so the shard's edges are one contiguous window. An unsharded
            // walk answers both bounds with one comparison instead of a binary search.
            let from = if (row[0] as usize) >= offset {
                row_from
            } else {
                row_from + row.partition_point(|target| (*target as usize) < offset)
            };
            let to = if (*last as usize) < limit {
                row_to
            } else {
                row_from + row.partition_point(|target| (*target as usize) < limit)
            };
            (from, to)
        } else {
            (row_from, row_to)
        };
        // One presence word covers 64 consecutive edge indices, so it is loaded per run of the
        // row rather than per edge; a median row is a single run.
        let mut edge = from;
        while edge < to {
            let word = words[edge >> 6];
            let run_end = to.min(((edge >> 6) + 1) << 6);
            for edge in edge..run_end {
                if !WINDOWED && !(offset..limit).contains(&(targets[edge] as usize)) {
                    continue;
                }
                let gain = plasticity.gain_in_word(edge, word);
                // The floor is applied per edge, not once per tick: the clamp order is part of
                // the numerics.
                let target = targets[edge] as usize - offset;
                let value =
                    f64::from(membrane[target]) + f64::from(weights[edge]) * gain * synapse_scale;
                membrane[target] = floor_clamp(value, membrane_floor) as f32;
            }
            edge = run_end;
        }
    }
}

/// Whether every CSR row is target-ascending, which is what lets a propagation shard binary-search
/// its window instead of filtering the whole row. True for `fafb-v783`; the synthetic fixtures the
/// golden tests carry are not all sorted, hence the check rather than the assumption.
fn rows_are_target_ascending(data: &BrainDataset) -> bool {
    (0..data.meta.neurons).all(|source| {
        let from = data.indptr[source] as usize;
        let to = data.indptr[source + 1] as usize;
        data.targets[from..to]
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    })
}

/// `Number.isSafeInteger`.
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// Neurons per block in the sweep's fast path. A block of 8 f64 voltages is two AVX2 registers or
/// four SSE2 ones, and at the measured rates about three blocks in four contain no special neuron.
const SWEEP_BLOCK: usize = 8;

/// One neuron of the integrate-and-fire sweep, written exactly as `model/lif.ts` has it.
///
/// The one and only definition of the arithmetic. The blocked fast path in [`sweep_range`] must
/// produce the same bits, so it computes the same `f64` expression and rounds it once.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn sweep_one(
    neuron: usize,
    offset: usize,
    membrane: &mut [f32],
    refractory: &mut [u8],
    baseline: &[f32],
    decay: f64,
    threshold: f64,
    refractory_reset: u8,
    spikes: &mut [u32],
    count: &mut usize,
) {
    if refractory[neuron] > 0 {
        // A refractory neuron still leaks but receives no baseline drive.
        refractory[neuron] -= 1;
        membrane[neuron] = (f64::from(membrane[neuron]) * decay) as f32;
        return;
    }
    let voltage = f64::from(membrane[neuron]) * decay + f64::from(baseline[neuron]);
    if voltage >= threshold {
        // A spiking neuron resets to exactly 0 rather than subtracting the threshold.
        membrane[neuron] = 0.0;
        refractory[neuron] = refractory_reset;
        spikes[*count] = (offset + neuron) as u32;
        *count += 1;
    } else {
        membrane[neuron] = voltage as f32;
    }
}

/// One contiguous range of the integrate-and-fire sweep.
///
/// `offset` is the range's first neuron index, so recorded spikes carry global indices.
///
/// # Why this is blocked
///
/// Written one neuron at a time the loop cannot vectorize: the refractory test and the spike test
/// both branch, and the spike branch appends to `spikes`, which is a loop-carried dependency. But
/// at the default constants only about 2.5% of neurons are refractory and 1.2% spike, so a block
/// of eight has no special neuron roughly three times in four. So: compute the whole block's
/// voltages branchlessly, reduce "is any neuron here special" to one bool, and in the common case
/// store eight f32 and touch nothing else. A block that does contain a special neuron falls back
/// to [`sweep_one`] for all eight.
///
/// Bit-exactness: the fast path evaluates the same `m as f64 * decay + b as f64` and rounds it
/// once with `as f32`, which is what [`sweep_one`] does in its non-refractory, non-spiking branch —
/// the only branch a block on the fast path can take. Computing a voltage for a neuron that turns
/// out to be refractory is harmless because the block is then discarded and recomputed. No
/// `mul_add`, no reassociation, no fast-math: the vector forms of `mulpd` and `addpd` are the same
/// IEEE operations as the scalar ones, and the golden tests re-check that on every run.
#[allow(clippy::too_many_arguments)]
#[inline]
fn sweep_range(
    offset: usize,
    membrane: &mut [f32],
    refractory: &mut [u8],
    baseline: &[f32],
    decay: f64,
    threshold: f64,
    refractory_reset: u8,
    spikes: &mut [u32],
) -> usize {
    let neurons = membrane.len();
    let mut count = 0usize;
    let mut base = 0usize;
    while base + SWEEP_BLOCK <= neurons {
        let mut voltage = [0.0f64; SWEEP_BLOCK];
        let mut refractories = [0u8; SWEEP_BLOCK];
        let mut special = false;
        {
            let block = base..base + SWEEP_BLOCK;
            let membranes = &membrane[block.clone()];
            let baselines = &baseline[block.clone()];
            let current = &refractory[block];
            for index in 0..SWEEP_BLOCK {
                let value = f64::from(membranes[index]) * decay + f64::from(baselines[index]);
                voltage[index] = value;
                refractories[index] = current[index];
                // Non-short-circuiting on purpose: `||` would serialize the block.
                special |= (current[index] != 0) | (value >= threshold);
            }
        }
        if special {
            // Only the special lanes need the scalar path. A lane that is neither refractory nor
            // spiking takes `sweep_one`'s else branch, which is this same store.
            for index in 0..SWEEP_BLOCK {
                if refractories[index] != 0 || voltage[index] >= threshold {
                    sweep_one(
                        base + index,
                        offset,
                        membrane,
                        refractory,
                        baseline,
                        decay,
                        threshold,
                        refractory_reset,
                        spikes,
                        &mut count,
                    );
                } else {
                    membrane[base + index] = voltage[index] as f32;
                }
            }
        } else {
            for (slot, value) in membrane[base..base + SWEEP_BLOCK]
                .iter_mut()
                .zip(voltage.iter().copied())
            {
                *slot = value as f32;
            }
        }
        base += SWEEP_BLOCK;
    }
    for neuron in base..neurons {
        sweep_one(
            neuron,
            offset,
            membrane,
            refractory,
            baseline,
            decay,
            threshold,
            refractory_reset,
            spikes,
            &mut count,
        );
    }
    count
}
