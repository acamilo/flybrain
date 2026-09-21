import type { BrainDataset } from '../../src/dataset/format';
import { Plasticity, type PlasticityState } from './plasticity';

const DECAY = Math.fround(Math.exp(-1 / 20));
const THRESHOLD = 1;
const REFRACTORY_MS = 2;
const SYNAPSE_SCALE = 0.005;
const BASELINE_MAX = 0.06;
const NOISE_KICKS = 300;
const NOISE_AMOUNT = 0.42;
const RATE_ALPHA = 1 / 25;

export class FlyBrain {
  readonly plasticity: Plasticity;
  readonly membrane: Float32Array;
  readonly refractory: Uint8Array;
  readonly baseline: Float32Array;
  readonly lastSpikeMs: Float64Array;
  readonly rates: Record<string, number> = {};
  readonly roleNames: string[];
  readonly roleMasks: Uint16Array;
  private readonly spikes: Uint32Array;
  private readonly roleCounts: Uint32Array;
  private rng = 22_222;
  private visualDrive = new Float32Array(0);
  private rewardRemaining = 0;
  ms = 0;
  populationRate = 0;

  constructor(readonly data: BrainDataset) {
    this.plasticity = new Plasticity(data);
    const n = data.meta.neurons;
    this.membrane = new Float32Array(n);
    this.refractory = new Uint8Array(n);
    this.baseline = new Float32Array(n);
    this.visualDrive = new Float32Array(data.visualIndices.length);
    this.lastSpikeMs = new Float64Array(n).fill(-1_000_000);
    this.spikes = new Uint32Array(n);
    this.roleNames = Object.keys(data.meta.roles).filter((name) =>
      name.startsWith('command_') || ['steer_left', 'steer_right', 'forward', 'backward', 'proboscis', 'reward_pam'].includes(name),
    );
    this.roleMasks = new Uint16Array(n);
    this.roleCounts = new Uint32Array(this.roleNames.length);
    for (let role = 0; role < this.roleNames.length; role++) {
      this.rates[this.roleNames[role]] = 0;
      for (const neuron of data.meta.roles[this.roleNames[role]]) this.roleMasks[neuron] |= 1 << role;
    }
    for (let i = 0; i < n; i++) this.baseline[i] = this.random() * BASELINE_MAX;
  }

  setVisualFrame(rgba: Uint8Array): void {
    const count = this.data.visualIndices.length;
    if (this.visualDrive.length !== count) this.visualDrive = new Float32Array(count);
    const xy = this.data.visualXY;
    let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (let i = 0; i < count; i++) {
      minX = Math.min(minX, xy[i * 2]); maxX = Math.max(maxX, xy[i * 2]);
      minY = Math.min(minY, xy[i * 2 + 1]); maxY = Math.max(maxY, xy[i * 2 + 1]);
    }
    for (let i = 0; i < count; i++) {
      let normalizedX = (xy[i * 2] - minX) / (maxX - minX || 1);
      if (this.data.visualHemisphere[i] === 0) normalizedX = 1 - normalizedX;
      const normalizedY = (xy[i * 2 + 1] - minY) / (maxY - minY || 1);
      const x = Math.max(0, Math.min(159, Math.round(normalizedX * 159)));
      const y = Math.max(0, Math.min(143, Math.round(normalizedY * 143)));
      const offset = (y * 160 + x) * 4;
      const luminance = (rgba[offset] * 0.2126 + rgba[offset + 1] * 0.7152 + rgba[offset + 2] * 0.0722) / 255;
      this.visualDrive[i] = luminance * 0.20;
    }
  }

  reward(durationMs = 120): void {
    this.rewardRemaining = Math.max(this.rewardRemaining, durationMs);
  }

  step(milliseconds: number): number {
    let totalSpikes = 0;
    for (let tick = 0; tick < milliseconds; tick++) totalSpikes += this.stepOne();
    return totalSpikes;
  }

  private stepOne(): number {
    const { indptr, targets, weights, visualIndices, meta } = this.data;
    const n = meta.neurons;
    for (let i = 0; i < NOISE_KICKS; i++) this.membrane[this.randomUint() % n] += NOISE_AMOUNT;
    for (let i = 0; i < visualIndices.length; i++) this.membrane[visualIndices[i]] += this.visualDrive[i];
    if (this.rewardRemaining > 0) {
      for (const neuron of meta.roles.reward_pam ?? []) this.membrane[neuron] += 0.20;
      this.rewardRemaining--;
    }

    let spikeCount = 0;
    for (let neuron = 0; neuron < n; neuron++) {
      if (this.refractory[neuron] > 0) {
        this.refractory[neuron]--;
        this.membrane[neuron] *= DECAY;
        continue;
      }
      const voltage = this.membrane[neuron] * DECAY + this.baseline[neuron];
      if (voltage >= THRESHOLD) {
        this.membrane[neuron] = 0;
        this.refractory[neuron] = REFRACTORY_MS;
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
      let mask = this.roleMasks[source];
      for (let role = 0; mask; role++, mask >>>= 1) if (mask & 1) this.roleCounts[role]++;
      for (let edge = indptr[source]; edge < indptr[source + 1]; edge++) {
        const target = targets[edge];
        this.membrane[target] = Math.max(-2, this.membrane[target] + weights[edge] * this.plasticity.gain(edge) * SYNAPSE_SCALE);
      }
    }
    for (let role = 0; role < this.roleNames.length; role++) {
      const name = this.roleNames[role];
      const size = meta.roles[name].length;
      const instantaneous = size ? this.roleCounts[role] * 1000 / size : 0;
      this.rates[name] += (instantaneous - this.rates[name]) * RATE_ALPHA;
    }
    this.populationRate += (spikeCount * 1000 / n - this.populationRate) * RATE_ALPHA;
    this.ms++;
    return spikeCount;
  }

  private randomUint(): number {
    let value = this.rng;
    value ^= value << 13; value ^= value >>> 17; value ^= value << 5;
    this.rng = value;
    return value >>> 0;
  }

  private random(): number {
    return this.randomUint() / 0x1_0000_0000;
  }

  exportState() {
    return {
      membrane: this.membrane.slice(),
      refractory: this.refractory.slice(),
      lastSpikeMs: this.lastSpikeMs.slice(),
      visualDrive: this.visualDrive.slice(),
      rng: this.rng,
      rewardRemaining: this.rewardRemaining,
      ms: this.ms,
      populationRate: this.populationRate,
      rates: { ...this.rates },
      plasticity: this.plasticity.exportState(),
    };
  }

  importState(state: Omit<ReturnType<FlyBrain['exportState']>, 'plasticity'> & { plasticity?: PlasticityState }): void {
    if (state.membrane.length !== this.membrane.length || state.refractory.length !== this.refractory.length || state.lastSpikeMs.length !== this.lastSpikeMs.length) {
      throw new Error('Brain checkpoint dimensions do not match the loaded dataset');
    }
    if (state.visualDrive.length !== this.data.visualIndices.length || !Number.isSafeInteger(state.ms) || state.ms < 0 || !Number.isInteger(state.rng) || !Number.isFinite(state.populationRate) || !Number.isFinite(state.rewardRemaining) || state.rewardRemaining < 0 || Object.values(state.rates).some(value => !Number.isFinite(value)) || [state.membrane, state.lastSpikeMs, state.visualDrive].some(values => values.some(value => !Number.isFinite(value)))) throw new Error('Invalid neural checkpoint values');
    this.plasticity.importState(state.plasticity);
    this.membrane.set(state.membrane);
    this.refractory.set(state.refractory);
    this.lastSpikeMs.set(state.lastSpikeMs);
    this.visualDrive = state.visualDrive.slice();
    this.rng = state.rng;
    this.rewardRemaining = state.rewardRemaining;
    this.ms = state.ms;
    this.populationRate = state.populationRate;
    for (const name of this.roleNames) this.rates[name] = state.rates[name] ?? 0;
  }
}
