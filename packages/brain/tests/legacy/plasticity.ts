import type { BrainDataset } from '../../src/dataset/format';

export const PLASTICITY_VERSION = 'fly-kc-mbon-rstdp-v2';
const TRACE_MS = 5000;
const PAIR_MS = 20;
export interface LearningStats {
  version: string; enabled: boolean; synapses: number; mushroom: number; output: number;
  updates: number; changed: number; meanChange: number; maxChange: number; signal: number;
}
export interface PlasticityState {
  version: string; topology: number; enabled: boolean; updates: number; signal: number;
  gains: Float32Array; traces: Float32Array; touched: Float64Array;
}

/** Phenomenological three-factor plasticity; anatomical sites, not fitted dopamine compartments. */
export class Plasticity {
  readonly slots: Int32Array;
  readonly edges: Uint32Array;
  readonly gains: Float32Array;
  readonly traces: Float32Array;
  readonly touched: Float64Array;
  private readonly sources: Uint32Array;
  private readonly mushroom: Uint8Array;
  private readonly incoming: number[][];
  private readonly outgoing: Map<number, number[]> = new Map();
  private readonly topology: number;
  private updates = 0;
  private signal = 0;
  enabled = true;

  constructor(private readonly data: BrainDataset) {
    const kc = new Set(data.meta.roles.kenyon ?? []);
    const mbon = new Set(data.meta.roles.mbon ?? []);
    const candidates: [number, number, number][] = [];
    for (let source = 0; source < data.meta.neurons; source++) {
      for (let edge = data.indptr[source]; edge < data.indptr[source + 1]; edge++) {
        const target = data.targets[edge];
        if (data.weights[edge] <= 0 || source === target) continue;
        const mb = kc.has(source) && mbon.has(target);
        if (mb) candidates.push([edge, source, 1]);
      }
    }
    // Fixed deterministic budget: strongest anatomical KC→MBON connections.
    const chosen = candidates.sort((a, b) => data.weights[b[0]] - data.weights[a[0]] || a[0] - b[0]).slice(0, 16384).sort((a, b) => a[0] - b[0]);
    this.slots = new Int32Array(data.meta.edges).fill(-1);
    this.edges = Uint32Array.from(chosen.map(row => row[0]));
    this.sources = Uint32Array.from(chosen.map(row => row[1]));
    this.mushroom = Uint8Array.from(chosen.map(row => row[2]));
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

  gain(edge: number): number { const slot = this.slots[edge]; return slot < 0 ? 1 : this.gains[slot]; }

  clearEligibility(ms: number): void { this.traces.fill(0); this.touched.fill(ms); this.signal = 0; }

  private trace(slot: number, ms: number, pair: number): void {
    const decay = Math.exp(-Math.max(0, ms - this.touched[slot]) / TRACE_MS);
    this.traces[slot] = Math.max(-1, Math.min(1, this.traces[slot] * decay + pair));
    this.touched[slot] = ms;
  }

  observe(spikes: Uint32Array, count: number, lastSpike: Float64Array, ms: number): void {
    if (!this.enabled) return;
    for (let i = 0; i < count; i++) {
      const neuron = spikes[i];
      for (const slot of this.incoming[neuron]) {
        const dt = ms - lastSpike[this.sources[slot]];
        if (dt > 0 && dt <= 100) this.trace(slot, ms, 0.1 * Math.exp(-dt / PAIR_MS));
      }
      const outgoing = this.outgoing.get(neuron);
      if (!outgoing) continue;
      for (const slot of outgoing) {
        const dt = ms - lastSpike[this.data.targets[this.edges[slot]]];
        if (dt > 0 && dt <= 100) this.trace(slot, ms, -0.05 * Math.exp(-dt / PAIR_MS));
      }
    }
  }

  reinforce(reward: number, ms: number): void {
    if (!this.enabled || !Number.isFinite(reward) || reward === 0) return;
    this.signal = Math.tanh(reward);
    let changed = false;
    for (let slot = 0; slot < this.edges.length; slot++) {
      this.trace(slot, ms, 0);
      const old = this.gains[slot];
      // Small restoring term limits long-run drift; excitation never changes sign.
      const next = old + 0.002 * this.signal * this.traces[slot] - 0.0001 * (old - 1);
      this.gains[slot] = Math.max(0.9, Math.min(1.1, next));
      changed ||= this.gains[slot] !== old;
    }
    if (changed) this.updates++;
  }

  statistics(): LearningStats {
    let changed = 0, sum = 0, max = 0, mushroom = 0;
    for (let i = 0; i < this.gains.length; i++) {
      const delta = Math.abs(this.gains[i] - 1);
      if (delta > 0.000001) changed++;
      sum += delta; max = Math.max(max, delta); mushroom += this.mushroom[i];
    }
    return { version: PLASTICITY_VERSION, enabled: this.enabled, synapses: this.edges.length, mushroom, output: this.edges.length - mushroom,
      updates: this.updates, changed, meanChange: sum / (this.edges.length || 1), maxChange: max, signal: this.signal };
  }

  exportState(): PlasticityState {
    return { version: PLASTICITY_VERSION, topology: this.topology, enabled: this.enabled, updates: this.updates, signal: this.signal,
      gains: this.gains.slice(), traces: this.traces.slice(), touched: this.touched.slice() };
  }

  importState(state?: PlasticityState): void {
    if (!state) { this.gains.fill(1); this.traces.fill(0); this.touched.fill(0); this.updates = 0; this.signal = 0; return; }
    if (state.version !== PLASTICITY_VERSION || state.topology !== this.topology) throw new Error('Incompatible plasticity topology/version');
    if (state.gains.length !== this.gains.length || state.traces.length !== this.traces.length || state.touched.length !== this.touched.length) throw new Error('Invalid plasticity dimensions');
    if (typeof state.enabled !== 'boolean' || !Number.isInteger(state.updates) || state.updates < 0 || !Number.isFinite(state.signal) || Math.abs(state.signal) > 1) throw new Error('Invalid plasticity metadata');
    for (let i = 0; i < this.gains.length; i++) {
      const radius = 0.100001;
      if (!Number.isFinite(state.gains[i]) || Math.abs(state.gains[i] - 1) > radius || !Number.isFinite(state.traces[i]) || Math.abs(state.traces[i]) > 1 || !Number.isFinite(state.touched[i]) || state.touched[i] < 0) throw new Error('Invalid plasticity values');
    }
    this.gains.set(state.gains); this.traces.set(state.traces); this.touched.set(state.touched);
    this.enabled = state.enabled; this.updates = state.updates; this.signal = state.signal;
  }
}
