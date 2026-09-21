import type { BrainDataset } from '../dataset/format';
import { versionFor } from './version';

/** Version of the default configuration; kept verbatim so existing checkpoints stay loadable. */
export const PLASTICITY_VERSION = 'fly-kc-mbon-rstdp-v2';

export interface PlasticityConfig {
  /** Anatomical role a plastic edge's source must belong to. */
  preRole: string;
  /** Anatomical role a plastic edge's target must belong to. */
  postRole: string;
  /** Maximum number of plastic edges (strongest positive weights win). */
  budget: number;
  /** Eligibility-trace decay time constant in milliseconds. */
  traceMs: number;
  /** Spike-pair exponential time constant in milliseconds. */
  pairMs: number;
  /** Largest spike interval that still pairs, in milliseconds. */
  pairWindowMs: number;
  /** Eligibility added for a causal (pre before post) pair at dt = 0. */
  potentiation: number;
  /** Eligibility subtracted for an anti-causal (post before pre) pair at dt = 0. */
  depression: number;
  /** Gain step per unit of modulator times eligibility. */
  learningRate: number;
  /** Restoring pull back towards a gain of 1, applied on reinforcement only. */
  restoring: number;
  /** Lower gain clamp. */
  minGain: number;
  /** Upper gain clamp. */
  maxGain: number;
}

/** Original constants: strongest 16,384 positive KC->MBON edges, gains clamped to [0.9, 1.1]. */
export const DEFAULT_PLASTICITY_CONFIG: PlasticityConfig = {
  preRole: 'kenyon', postRole: 'mbon', budget: 16_384,
  traceMs: 5000, pairMs: 20, pairWindowMs: 100,
  potentiation: 0.1, depression: 0.05, learningRate: 0.002, restoring: 0.0001,
  minGain: 0.9, maxGain: 1.1,
};

/**
 * Numeric parameters that define the learning rule, in version-hash order. Role names and budget
 * are deliberately absent: they change which edges are selected, which the topology hash already
 * covers, and both are dataset-specific rather than rule-defining.
 */
const VERSION_PARAMS = [
  'traceMs', 'pairMs', 'pairWindowMs', 'potentiation', 'depression', 'learningRate', 'restoring', 'minGain', 'maxGain',
] as const satisfies readonly (keyof PlasticityConfig)[];

/** `PLASTICITY_VERSION` for the default rule constants, otherwise `rstdp-v2:<fnv1a32>`. */
export function plasticityVersion(config: Partial<PlasticityConfig> = {}): string {
  const merged = { ...DEFAULT_PLASTICITY_CONFIG, ...config };
  return versionFor(
    PLASTICITY_VERSION,
    'rstdp-v2:',
    VERSION_PARAMS.map((key) => merged[key]),
    VERSION_PARAMS.map((key) => DEFAULT_PLASTICITY_CONFIG[key]),
  );
}

export interface LearningStats {
  version: string; enabled: boolean; synapses: number;
  /**
   * Count of selected edges (hash group 1). Historical field name: with a single plastic site all
   * selected edges belong to that site.
   */
  mushroom: number;
  /** Always 0: no second edge group exists. Kept for checkpoint/UI compatibility. */
  output: number;
  updates: number; changed: number; meanChange: number; maxChange: number; signal: number;
}

export interface PlasticityState {
  version: string; topology: number; enabled: boolean; updates: number; signal: number;
  gains: Float32Array; traces: Float32Array; touched: Float64Array;
}

/** Phenomenological three-factor plasticity; anatomical sites, not fitted dopamine compartments. */
export class RewardModulatedStdp {
  readonly config: PlasticityConfig;
  /** Base edge index -> plastic slot, or -1 for the immutable majority. */
  readonly slots: Int32Array;
  readonly edges: Uint32Array;
  readonly gains: Float32Array;
  readonly traces: Float32Array;
  readonly touched: Float64Array;
  /** Version string of this configuration; written into exported state. */
  readonly version: string;
  private readonly sources: Uint32Array;
  private readonly selected: Uint8Array;
  private readonly incoming: number[][];
  private readonly outgoing: Map<number, number[]> = new Map();
  private readonly topology: number;
  private updates = 0;
  private signal = 0;
  enabled = true;

  constructor(private readonly data: BrainDataset, config: Partial<PlasticityConfig> = {}) {
    this.config = { ...DEFAULT_PLASTICITY_CONFIG, ...config };
    this.version = plasticityVersion(this.config);
    const pre = new Set(data.meta.roles[this.config.preRole] ?? []);
    const post = new Set(data.meta.roles[this.config.postRole] ?? []);
    const candidates: [number, number, number][] = [];
    for (let source = 0; source < data.meta.neurons; source++) {
      for (let edge = data.indptr[source]; edge < data.indptr[source + 1]; edge++) {
        const target = data.targets[edge];
        if (data.weights[edge] <= 0 || source === target) continue;
        const site = pre.has(source) && post.has(target);
        if (site) candidates.push([edge, source, 1]);
      }
    }
    // Fixed deterministic budget: strongest anatomical pre->post connections.
    const chosen = candidates
      .sort((a, b) => data.weights[b[0]] - data.weights[a[0]] || a[0] - b[0])
      .slice(0, this.config.budget)
      .sort((a, b) => a[0] - b[0]);
    this.slots = new Int32Array(data.meta.edges).fill(-1);
    this.edges = Uint32Array.from(chosen.map(row => row[0]));
    this.sources = Uint32Array.from(chosen.map(row => row[1]));
    this.selected = Uint8Array.from(chosen.map(row => row[2]));
    this.gains = new Float32Array(chosen.length).fill(1);
    this.traces = new Float32Array(chosen.length);
    this.touched = new Float64Array(chosen.length);
    this.incoming = Array.from({ length: data.meta.neurons }, () => []);
    let hash = 2166136261;
    chosen.forEach(([edge, source, group], slot) => {
      this.slots[edge] = slot;
      this.incoming[data.targets[edge]].push(slot);
      // chosen is edge-sorted: preserve the former base-CSR traversal order.
      const outgoing = this.outgoing.get(source) ?? [];
      outgoing.push(slot);
      this.outgoing.set(source, outgoing);
      for (const value of [edge, source, data.targets[edge], data.weights[edge], group]) hash = Math.imul(hash ^ value, 16777619) >>> 0;
    });
    this.topology = hash;
  }

  /** Effective multiplier for a base edge; 1 for every non-plastic edge. */
  gain(edge: number): number { const slot = this.slots[edge]; return slot < 0 ? 1 : this.gains[slot]; }

  clearEligibility(ms: number): void { this.traces.fill(0); this.touched.fill(ms); this.signal = 0; }

  private trace(slot: number, ms: number, pair: number): void {
    const decay = Math.exp(-Math.max(0, ms - this.touched[slot]) / this.config.traceMs);
    this.traces[slot] = Math.max(-1, Math.min(1, this.traces[slot] * decay + pair));
    this.touched[slot] = ms;
  }

  observe(spikes: Uint32Array, count: number, lastSpike: Float64Array, ms: number): void {
    if (!this.enabled) return;
    const { pairMs, pairWindowMs, potentiation, depression } = this.config;
    for (let i = 0; i < count; i++) {
      const neuron = spikes[i];
      for (const slot of this.incoming[neuron]) {
        const dt = ms - lastSpike[this.sources[slot]];
        if (dt > 0 && dt <= pairWindowMs) this.trace(slot, ms, potentiation * Math.exp(-dt / pairMs));
      }
      const outgoing = this.outgoing.get(neuron);
      if (!outgoing) continue;
      for (const slot of outgoing) {
        const dt = ms - lastSpike[this.data.targets[this.edges[slot]]];
        if (dt > 0 && dt <= pairWindowMs) this.trace(slot, ms, -depression * Math.exp(-dt / pairMs));
      }
    }
  }

  reinforce(reward: number, ms: number): void {
    if (!this.enabled || !Number.isFinite(reward) || reward === 0) return;
    const { learningRate, restoring, minGain, maxGain } = this.config;
    this.signal = Math.tanh(reward);
    let changed = false;
    for (let slot = 0; slot < this.edges.length; slot++) {
      this.trace(slot, ms, 0);
      const old = this.gains[slot];
      // Small restoring term limits long-run drift; excitation never changes sign.
      const next = old + learningRate * this.signal * this.traces[slot] - restoring * (old - 1);
      this.gains[slot] = Math.max(minGain, Math.min(maxGain, next));
      changed ||= this.gains[slot] !== old;
    }
    if (changed) this.updates++;
  }

  statistics(): LearningStats {
    let changed = 0, sum = 0, max = 0, mushroom = 0;
    for (let i = 0; i < this.gains.length; i++) {
      const delta = Math.abs(this.gains[i] - 1);
      if (delta > 0.000001) changed++;
      sum += delta; max = Math.max(max, delta); mushroom += this.selected[i];
    }
    return { version: this.version, enabled: this.enabled, synapses: this.edges.length, mushroom, output: this.edges.length - mushroom,
      updates: this.updates, changed, meanChange: sum / (this.edges.length || 1), maxChange: max, signal: this.signal };
  }

  exportState(): PlasticityState {
    return { version: this.version, topology: this.topology, enabled: this.enabled, updates: this.updates, signal: this.signal,
      gains: this.gains.slice(), traces: this.traces.slice(), touched: this.touched.slice() };
  }

  importState(state?: PlasticityState): void {
    if (!state) { this.gains.fill(1); this.traces.fill(0); this.touched.fill(0); this.updates = 0; this.signal = 0; return; }
    if (state.version !== this.version || state.topology !== this.topology) throw new Error('Incompatible plasticity topology/version');
    if (state.gains.length !== this.gains.length || state.traces.length !== this.traces.length || state.touched.length !== this.touched.length) throw new Error('Invalid plasticity dimensions');
    if (typeof state.enabled !== 'boolean' || !Number.isInteger(state.updates) || state.updates < 0 || !Number.isFinite(state.signal) || Math.abs(state.signal) > 1) throw new Error('Invalid plasticity metadata');
    // 0.100001 for the default clamps: the widest legal displacement plus a float32 tolerance.
    const radius = Math.max(1 - this.config.minGain, this.config.maxGain - 1) + 0.000001;
    for (let i = 0; i < this.gains.length; i++) {
      if (!Number.isFinite(state.gains[i]) || Math.abs(state.gains[i] - 1) > radius || !Number.isFinite(state.traces[i]) || Math.abs(state.traces[i]) > 1 || !Number.isFinite(state.touched[i]) || state.touched[i] < 0) throw new Error('Invalid plasticity values');
    }
    this.gains.set(state.gains); this.traces.set(state.traces); this.touched.set(state.touched);
    this.enabled = state.enabled; this.updates = state.updates; this.signal = state.signal;
  }
}

/** Historical name of {@link RewardModulatedStdp}. */
export { RewardModulatedStdp as Plasticity };
