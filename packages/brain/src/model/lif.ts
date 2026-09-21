import type { BrainDataset } from '../dataset/format';
import { RewardModulatedStdp, type PlasticityConfig, type PlasticityState } from './plasticity';
import { DEFAULT_RETINA_CONFIG, projectFrame, type RetinaConfig } from './retina';
import { Xorshift32 } from './rng';
import { versionFor } from './version';

/** Version of the default configuration; kept verbatim so existing checkpoints stay loadable. */
export const NEURAL_KERNEL_VERSION = 'lif-1ms-f64-v2';

/** Rate roles tracked by default, on top of every `command_*` role the dataset declares. */
const DEFAULT_EXTRA_RATE_ROLES = ['steer_left', 'steer_right', 'forward', 'backward', 'proboscis', 'reward_pam'];

/**
 * Prefix of the macro-type populations, tracked by default like the `command_*` buttons.
 *
 * `docs/design/macros.md` section 12: a macro is a button, pressed by its own population, so its
 * rate is read exactly as a button's is. A dataset that predates the roles simply has none, and a
 * checkpoint that predates them restores them at zero (`importState`).
 */
const MACRO_RATE_ROLE_PREFIX = 'macro_';

/**
 * Rate roles are packed into a per-neuron bitmask of {@link ROLE_MASK_WORDS} 32-bit words, one
 * bit per role.
 *
 * 64 rather than 32 since `docs/design/macros.md` section 11: the eight `command_*` buttons and
 * the six historical motor/reward roles are fourteen, and the twenty-two `macro_<type>`
 * populations bring the default set to thirty-six. The rates themselves are unchanged — a role's
 * bit index only decides which counter a spike lands in — so a wider mask is capacity, not
 * semantics.
 */
export const MAX_RATE_ROLES = 64;

/** 32-bit words per neuron in the role bitmask. */
export const ROLE_MASK_WORDS = MAX_RATE_ROLES / 32;

export interface LifConfig {
  /** Membrane decay time constant in milliseconds; decay per tick is fround(exp(-1/decayMs)). */
  decayMs: number;
  /** Spike threshold in membrane units. */
  threshold: number;
  /** Ticks a neuron stays refractory after spiking. */
  refractoryMs: number;
  /** Multiplier applied to dataset weights when a spike propagates. */
  synapseScale: number;
  /** Upper bound of the per-neuron random baseline drive. */
  baselineMax: number;
  /** Random membrane kicks per tick. */
  noiseKicks: number;
  /** Membrane increment per noise kick. */
  noiseAmount: number;
  /** Exponential-moving-average coefficient for rate estimates. */
  rateAlpha: number;
  /** Lower clamp on the membrane after inhibitory input. */
  membraneFloor: number;
  /** Seed of the deterministic noise generator. */
  seed: number;
  /** Role driven by `stimulate()` (the reward pulse) and the drive it receives per tick. */
  stimulation: { role: string; drive: number };
  /**
   * Roles whose population rate is tracked. Defaults to every `command_*` role plus the
   * historical motor/reward roles, in dataset role order. Names absent from the dataset are
   * dropped; at most {@link MAX_RATE_ROLES} roles may be tracked.
   */
  rateRoles?: string[];
  /** Retina gain and the frame size assumed by `setVisualFrame` when none is given. */
  retina: RetinaConfig;
}

/** Original constants of the FAFB kernel. */
export const DEFAULT_LIF_CONFIG: LifConfig = {
  decayMs: 20, threshold: 1, refractoryMs: 2, synapseScale: 0.005,
  baselineMax: 0.06, noiseKicks: 300, noiseAmount: 0.42, rateAlpha: 1 / 25,
  membraneFloor: -2, seed: 22_222,
  stimulation: { role: 'reward_pam', drive: 0.20 },
  retina: { ...DEFAULT_RETINA_CONFIG },
};

/** Numeric parameters of the kernel, in version-hash order. Role names never enter the hash. */
function versionParams(config: LifConfig): number[] {
  return [
    config.decayMs, config.threshold, config.refractoryMs, config.synapseScale,
    config.baselineMax, config.noiseKicks, config.noiseAmount, config.rateAlpha,
    config.membraneFloor, config.seed, config.stimulation.drive,
    config.retina.gain, config.retina.width, config.retina.height,
  ];
}

/** `NEURAL_KERNEL_VERSION` for the default constants, otherwise `lif-1ms-f64-v2:<fnv1a32>`. */
export function kernelVersion(config: Partial<LifConfig> = {}): string {
  const merged = { ...DEFAULT_LIF_CONFIG, ...config };
  return versionFor(NEURAL_KERNEL_VERSION, `${NEURAL_KERNEL_VERSION}:`, versionParams(merged), versionParams(DEFAULT_LIF_CONFIG));
}

export interface LifState {
  membrane: Float32Array;
  refractory: Uint8Array;
  lastSpikeMs: Float64Array;
  visualDrive: Float32Array;
  rng: number;
  rewardRemaining: number;
  ms: number;
  populationRate: number;
  rates: Record<string, number>;
  plasticity: PlasticityState;
}

/**
 * Leaky integrate-and-fire network over a connectome dataset, stepped in 1-ms ticks.
 *
 * Membrane state is Float32; spike and eligibility timestamps are Float64 milliseconds so that
 * 1-ms differences survive long sessions. Noise is a deterministic xorshift stream, so a
 * checkpoint plus the dataset reproduces a run exactly.
 */
export class LifNetwork {
  readonly config: LifConfig;
  /** Version string of this configuration; pin it in checkpoints. */
  readonly version: string;
  readonly plasticity: RewardModulatedStdp;
  readonly membrane: Float32Array;
  readonly refractory: Uint8Array;
  readonly baseline: Float32Array;
  readonly lastSpikeMs: Float64Array;
  readonly rates: Record<string, number> = {};
  readonly roleNames: string[];
  /** Role membership per neuron, {@link ROLE_MASK_WORDS} words each, low word first. */
  readonly roleMasks: Uint32Array;
  private readonly decay: number;
  private readonly stimulationTargets: number[];
  private readonly spikes: Uint32Array;
  private readonly roleCounts: Uint32Array;
  private readonly rng: Xorshift32;
  private visualDrive: Float32Array;
  private rewardRemaining = 0;
  ms = 0;
  populationRate = 0;

  constructor(readonly data: BrainDataset, config: Partial<LifConfig> & { plasticity?: Partial<PlasticityConfig> } = {}) {
    const { plasticity, ...kernel } = config;
    this.config = { ...DEFAULT_LIF_CONFIG, ...kernel };
    this.version = kernelVersion(this.config);
    this.decay = Math.fround(Math.exp(-1 / this.config.decayMs));
    this.plasticity = new RewardModulatedStdp(data, plasticity);
    const n = data.meta.neurons;
    this.membrane = new Float32Array(n);
    this.refractory = new Uint8Array(n);
    this.baseline = new Float32Array(n);
    this.visualDrive = new Float32Array(data.visualIndices.length);
    this.lastSpikeMs = new Float64Array(n).fill(-1_000_000);
    this.spikes = new Uint32Array(n);
    this.stimulationTargets = data.meta.roles[this.config.stimulation.role] ?? [];
    const requested = this.config.rateRoles;
    this.roleNames = requested
      ? requested.filter((name) => name in data.meta.roles)
      : Object.keys(data.meta.roles).filter(
        (name) =>
          name.startsWith('command_')
          || name.startsWith(MACRO_RATE_ROLE_PREFIX)
          || DEFAULT_EXTRA_RATE_ROLES.includes(name),
      );
    if (this.roleNames.length > MAX_RATE_ROLES) {
      throw new Error(`Too many tracked rate roles (${this.roleNames.length}); at most ${MAX_RATE_ROLES} fit the role bitmask`);
    }
    this.roleMasks = new Uint32Array(n * ROLE_MASK_WORDS);
    this.roleCounts = new Uint32Array(this.roleNames.length);
    for (let role = 0; role < this.roleNames.length; role++) {
      this.rates[this.roleNames[role]] = 0;
      const word = role >>> 5;
      const bit = 1 << (role & 31);
      for (const neuron of data.meta.roles[this.roleNames[role]]) this.roleMasks[neuron * ROLE_MASK_WORDS + word] |= bit;
    }
    this.rng = new Xorshift32(this.config.seed);
    for (let i = 0; i < n; i++) this.baseline[i] = this.rng.next() * this.config.baselineMax;
  }

  /** Project one RGBA frame onto the retina columns; the size defaults to `config.retina`. */
  setVisualFrame(rgba: Uint8Array, width = this.config.retina.width, height = this.config.retina.height): void {
    const count = this.data.visualIndices.length;
    if (this.visualDrive.length !== count) this.visualDrive = new Float32Array(count);
    projectFrame(
      rgba, width, height,
      { xy: this.data.visualXY, hemisphere: this.data.visualHemisphere, count },
      this.config.retina.gain,
      this.visualDrive,
    );
  }

  /** Drive the stimulation role for `durationMs` ticks; overlapping pulses take the maximum. */
  stimulate(durationMs = 120): void {
    this.rewardRemaining = Math.max(this.rewardRemaining, durationMs);
  }

  /** Historical name of {@link LifNetwork.stimulate}. */
  reward(durationMs = 120): void {
    this.stimulate(durationMs);
  }

  step(milliseconds: number): number {
    let totalSpikes = 0;
    for (let tick = 0; tick < milliseconds; tick++) totalSpikes += this.stepOne();
    return totalSpikes;
  }

  private stepOne(): number {
    const { indptr, targets, weights, visualIndices, meta } = this.data;
    const { threshold, refractoryMs, synapseScale, noiseKicks, noiseAmount, rateAlpha, membraneFloor } = this.config;
    const decay = this.decay;
    const n = meta.neurons;
    for (let i = 0; i < noiseKicks; i++) this.membrane[this.rng.nextUint() % n] += noiseAmount;
    for (let i = 0; i < visualIndices.length; i++) this.membrane[visualIndices[i]] += this.visualDrive[i];
    if (this.rewardRemaining > 0) {
      for (const neuron of this.stimulationTargets) this.membrane[neuron] += this.config.stimulation.drive;
      this.rewardRemaining--;
    }

    let spikeCount = 0;
    for (let neuron = 0; neuron < n; neuron++) {
      if (this.refractory[neuron] > 0) {
        this.refractory[neuron]--;
        this.membrane[neuron] *= decay;
        continue;
      }
      const voltage = this.membrane[neuron] * decay + this.baseline[neuron];
      if (voltage >= threshold) {
        this.membrane[neuron] = 0;
        this.refractory[neuron] = refractoryMs;
        this.spikes[spikeCount++] = neuron;
      } else {
        this.membrane[neuron] = voltage;
      }
    }

    this.roleCounts.fill(0);
    this.plasticity.observe(this.spikes, spikeCount, this.lastSpikeMs, this.ms);
    for (let spike = 0; spike < spikeCount; spike++) {
      const source = this.spikes[spike];
      this.lastSpikeMs[source] = this.ms;
      for (let word = 0; word < ROLE_MASK_WORDS; word++) {
        let mask = this.roleMasks[source * ROLE_MASK_WORDS + word];
        for (let role = word * 32; mask; role++, mask >>>= 1) if (mask & 1) this.roleCounts[role]++;
      }
      for (let edge = indptr[source]; edge < indptr[source + 1]; edge++) {
        const target = targets[edge];
        this.membrane[target] = Math.max(membraneFloor, this.membrane[target] + weights[edge] * this.plasticity.gain(edge) * synapseScale);
      }
    }
    for (let role = 0; role < this.roleNames.length; role++) {
      const name = this.roleNames[role];
      const size = meta.roles[name].length;
      const instantaneous = size ? this.roleCounts[role] * 1000 / size : 0;
      this.rates[name] += (instantaneous - this.rates[name]) * rateAlpha;
    }
    this.populationRate += (spikeCount * 1000 / n - this.populationRate) * rateAlpha;
    this.ms++;
    return spikeCount;
  }

  exportState(): LifState {
    return {
      membrane: this.membrane.slice(),
      refractory: this.refractory.slice(),
      lastSpikeMs: this.lastSpikeMs.slice(),
      visualDrive: this.visualDrive.slice(),
      rng: this.rng.state,
      rewardRemaining: this.rewardRemaining,
      ms: this.ms,
      populationRate: this.populationRate,
      rates: { ...this.rates },
      plasticity: this.plasticity.exportState(),
    };
  }

  importState(state: Omit<LifState, 'plasticity'> & { plasticity?: PlasticityState }): void {
    if (state.membrane.length !== this.membrane.length || state.refractory.length !== this.refractory.length || state.lastSpikeMs.length !== this.lastSpikeMs.length) {
      throw new Error('Brain checkpoint dimensions do not match the loaded dataset');
    }
    if (state.visualDrive.length !== this.data.visualIndices.length || !Number.isSafeInteger(state.ms) || state.ms < 0 || !Number.isInteger(state.rng) || !Number.isFinite(state.populationRate) || !Number.isFinite(state.rewardRemaining) || state.rewardRemaining < 0 || Object.values(state.rates).some(value => !Number.isFinite(value)) || [state.membrane, state.lastSpikeMs, state.visualDrive].some(values => values.some(value => !Number.isFinite(value)))) throw new Error('Invalid neural checkpoint values');
    this.plasticity.importState(state.plasticity);
    this.membrane.set(state.membrane);
    this.refractory.set(state.refractory);
    this.lastSpikeMs.set(state.lastSpikeMs);
    this.visualDrive = state.visualDrive.slice();
    this.rng.state = state.rng;
    this.rewardRemaining = state.rewardRemaining;
    this.ms = state.ms;
    this.populationRate = state.populationRate;
    for (const name of this.roleNames) this.rates[name] = state.rates[name] ?? 0;
  }
}

/** Historical name of {@link LifNetwork}. */
export { LifNetwork as FlyBrain };
