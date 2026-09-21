import assert from 'node:assert/strict';
import test from 'node:test';
import { FlyBrain, LifNetwork } from '../src/model/lif';
import {
  DEFAULT_PLASTICITY_CONFIG,
  PLASTICITY_VERSION,
  Plasticity,
  RewardModulatedStdp,
  plasticityVersion,
} from '../src/model/plasticity';
import { toyDataset } from './fixtures/toy-dataset';

/** One causal pre(0)->post(1) pairing 10 ms apart, as in the original unit tests. */
function causal(p: RewardModulatedStdp, ms: number) {
  p.observe(Uint32Array.of(1), 1, Float64Array.of(ms - 10, -1e6, -1e6, -1e6), ms);
}

test('aliases point at the generalized classes', () => {
  assert.equal(Plasticity, RewardModulatedStdp);
  assert.equal(FlyBrain, LifNetwork);
});

// --- Ported from fly-plays-pokemon tests/unit/plasticity.test.ts (game-rollback test excluded:
// --- it exercises the reward adapter, which is not part of this library).

test('causal eligibility is necessary; zero reward leaves exact weights; anatomical scope', () => {
  const p = new RewardModulatedStdp(toyDataset()); assert.deepEqual([...p.edges], [0]);
  p.reinforce(1, 100); assert.equal(p.gain(0), 1);
  causal(p, 110); p.reinforce(0, 120); assert.equal(p.gain(0), 1);
  p.reinforce(1, 130); assert.ok(p.gain(0) > 1);
  for (const edge of [1, 2, 3, 4]) assert.equal(p.gain(edge), 1);
  const old = p.exportState(); p.reinforce(0, 140); assert.deepEqual(p.exportState(), old);
});

test('eligibility decays over five seconds; anti-causal pairing depresses; bounds preserve sign', () => {
  const immediate = new RewardModulatedStdp(toyDataset()), delayed = new RewardModulatedStdp(toyDataset());
  causal(immediate, 100); causal(delayed, 100); immediate.reinforce(1, 100); delayed.reinforce(1, 5100);
  assert.ok(delayed.gain(0) > 1 && delayed.gain(0) < immediate.gain(0));
  const p = new RewardModulatedStdp(toyDataset());
  p.observe(Uint32Array.of(0), 1, Float64Array.of(-1e6, 90, -1e6, -1e6), 100); p.reinforce(1, 100); assert.ok(p.gain(0) < 1);
  for (let i = 0; i < 20000; i++) { causal(p, 200 + i); p.reinforce(i < 10000 ? 10 : -10, 200 + i); assert.ok(p.gain(0) >= 0.899999 && p.gain(0) <= 1.100001); }
});

test('exact neural continuation includes RNG, eligibility, gains and long-run Float64 pairing', () => {
  const a = new LifNetwork(toyDataset()); a.ms = 2 ** 26; a.step(15); causal(a.plasticity, a.ms); a.plasticity.reinforce(1, a.ms);
  const b = new LifNetwork(toyDataset()); b.importState(a.exportState());
  a.step(25); b.step(25); a.plasticity.reinforce(0.5, a.ms); b.plasticity.reinforce(0.5, b.ms);
  assert.deepEqual(b.exportState(), a.exportState()); assert.ok(a.lastSpikeMs instanceof Float64Array);
});

test('invalid imports reject before neural/plastic mutation', () => {
  const a = new LifNetwork(toyDataset()); const original = a.exportState();
  assert.throws(() => a.importState({ ...original, ms: NaN }), /Invalid/);
  assert.deepEqual(a.exportState(), original);
  for (const patch of [{ version: 'wrong' }, { topology: 0 }, { gains: Float32Array.of(NaN) }, { traces: Float32Array.of(2) }, { touched: Float64Array.of(-1) }]) {
    assert.throws(() => a.plasticity.importState({ ...original.plasticity, ...patch }), /Invalid|Incompatible/);
    assert.deepEqual(a.exportState(), original);
  }
});

test('warm-up without a framebuffer keeps visual neurons finite', () => {
  const data = toyDataset(); data.visualIndices = Uint32Array.of(0); data.visualHemisphere = Uint8Array.of(0); data.visualXY = Float32Array.of(0, 0);
  const brain = new LifNetwork(data); brain.step(100);
  assert.ok(brain.membrane.every(Number.isFinite));
  new LifNetwork(data).importState(brain.exportState());
});

test('sparse observation is exactly equivalent to base-edge traversal without reading base CSR', () => {
  const data = toyDataset(); data.meta.roles.kenyon = [0, 1]; data.meta.roles.mbon = [1, 2, 3];
  const p = new RewardModulatedStdp(data);
  const reference = new RewardModulatedStdp(data);
  const indptr = data.indptr;
  Object.defineProperty(data, 'indptr', { get() { throw new Error('observe traversed base CSR'); } });
  const spikes = Uint32Array.of(0, 1, 2, 3);
  for (const ms of [100, 110, 250, 5000]) {
    const last = Float64Array.of(ms - 10, ms - 20, ms - 30, ms - 40);
    const state = reference.exportState();
    const pair = (slot: number, amount: number) => {
      state.traces[slot] = Math.max(-1, Math.min(1, state.traces[slot] * Math.exp(-Math.max(0, ms - state.touched[slot]) / 5000) + amount));
      state.touched[slot] = ms;
    };
    for (const neuron of spikes) {
      for (let source = 0; source < data.meta.neurons; source++) {
        for (let edge = indptr[source]; edge < indptr[source + 1]; edge++) {
          const slot = reference.slots[edge];
          if (slot >= 0 && data.targets[edge] === neuron) pair(slot, 0.1 * Math.exp(-(ms - last[source]) / 20));
        }
      }
      for (let edge = indptr[neuron]; edge < indptr[neuron + 1]; edge++) {
        const slot = reference.slots[edge];
        if (slot >= 0) pair(slot, -0.05 * Math.exp(-(ms - last[data.targets[edge]]) / 20));
      }
    }
    reference.importState(state);
    p.observe(spikes, spikes.length, last, ms);
    p.reinforce(0.5, ms); reference.reinforce(0.5, ms);
    assert.deepEqual(p.exportState(), reference.exportState());
  }
});

// --- Configuration coverage.

test('default configuration keeps the historical version; numeric changes derive a new one', () => {
  assert.equal(plasticityVersion(), PLASTICITY_VERSION);
  assert.equal(plasticityVersion({ ...DEFAULT_PLASTICITY_CONFIG }), PLASTICITY_VERSION);
  assert.equal(new RewardModulatedStdp(toyDataset()).exportState().version, PLASTICITY_VERSION);
  const versions = new Set<string>();
  for (const patch of [{ traceMs: 4000 }, { pairMs: 30 }, { pairWindowMs: 50 }, { potentiation: 0.2 }, { depression: 0.1 },
    { learningRate: 0.004 }, { restoring: 0.0002 }, { minGain: 0.8 }, { maxGain: 1.2 }]) {
    const version = plasticityVersion(patch);
    assert.notEqual(version, PLASTICITY_VERSION);
    assert.match(version, /^rstdp-v2:[0-9a-f]{8}$/);
    versions.add(version);
  }
  assert.equal(versions.size, 9, 'each numeric parameter must reach the version hash');
  // Role names and budget select edges (covered by the topology hash) and never move the version.
  assert.equal(plasticityVersion({ preRole: 'mbon', postRole: 'motor', budget: 4 }), PLASTICITY_VERSION);
  assert.equal(new RewardModulatedStdp(toyDataset(), { traceMs: 4000 }).exportState().version, plasticityVersion({ traceMs: 4000 }));
});

test('a default-version state is incompatible with a non-default configuration', () => {
  const state = new RewardModulatedStdp(toyDataset()).exportState();
  const rule = new RewardModulatedStdp(toyDataset(), { learningRate: 0.004 });
  assert.throws(() => rule.importState(state), /Incompatible/);
  // Same rule constants, different site: the topology hash rejects it even though versions match.
  const site = new RewardModulatedStdp(toyDataset(), { postRole: 'motor' });
  assert.equal(site.exportState().version, PLASTICITY_VERSION);
  assert.throws(() => site.importState(state), /Incompatible/);
  // And through the network, which delegates the check.
  const brain = new LifNetwork(toyDataset(), { plasticity: { traceMs: 4000 } });
  assert.throws(() => brain.importState(new LifNetwork(toyDataset()).exportState()), /Incompatible/);
});

test('pre/post roles and budget choose which edges are plastic', () => {
  // Toy connectome edges: 0: 0->1 (10), 1: 0->2 (-5), 2: 0->3 (20), 3: 1->3 (10), 4: 2->1 (5).
  assert.deepEqual([...new RewardModulatedStdp(toyDataset()).edges], [0]);
  assert.deepEqual([...new RewardModulatedStdp(toyDataset(), { postRole: 'motor' }).edges], [2]);
  assert.deepEqual([...new RewardModulatedStdp(toyDataset(), { preRole: 'mbon', postRole: 'motor' }).edges], [3]);
  assert.deepEqual([...new RewardModulatedStdp(toyDataset(), { preRole: 'mbon', postRole: 'mbon' }).edges], [4]);
  assert.deepEqual([...new RewardModulatedStdp(toyDataset(), { preRole: 'kenyon', postRole: 'missing' }).edges], []);
  // Budget keeps the strongest candidates, edge index breaking ties, and is re-sorted by edge.
  const wide = toyDataset(); wide.meta.roles.kenyon = [0, 1, 2]; wide.meta.roles.mbon = [1, 2, 3];
  assert.deepEqual([...new RewardModulatedStdp(wide).edges], [0, 2, 3, 4]);
  assert.deepEqual([...new RewardModulatedStdp(wide, { budget: 2 }).edges], [0, 2]);
  const topologies = new Set([toyDataset(), toyDataset(), wide].map((data, index) =>
    new RewardModulatedStdp(data, index === 1 ? { postRole: 'motor' } : {}).exportState().topology));
  assert.equal(topologies.size, 3);
});

test('statistics keep the historical site field names', () => {
  const p = new RewardModulatedStdp(toyDataset());
  const before = p.statistics();
  assert.deepEqual(before, { version: PLASTICITY_VERSION, enabled: true, synapses: 1, mushroom: 1, output: 0,
    updates: 0, changed: 0, meanChange: 0, maxChange: 0, signal: 0 });
  causal(p, 100); p.reinforce(1, 100);
  const after = p.statistics();
  assert.equal(after.mushroom, p.edges.length);
  assert.equal(after.output, 0);
  assert.equal(after.updates, 1);
  assert.equal(after.changed, 1);
  assert.ok(after.maxChange > 0 && after.signal === Math.tanh(1));
});

test('clamps follow the configured gain bounds', () => {
  const p = new RewardModulatedStdp(toyDataset(), { minGain: 0.5, maxGain: 3, learningRate: 1 });
  for (let i = 0; i < 50; i++) { causal(p, 100 + i); p.reinforce(10, 100 + i); }
  assert.equal(p.gain(0), 3);
  const state = p.exportState();
  assert.doesNotThrow(() => new RewardModulatedStdp(toyDataset(), { minGain: 0.5, maxGain: 3, learningRate: 1 }).importState(state));
  // The default radius (0.100001) rejects a gain of 3.
  assert.throws(() => new RewardModulatedStdp(toyDataset()).importState({ ...state, version: PLASTICITY_VERSION }), /Invalid plasticity values/);
});

test('disabled plasticity ignores observation and reinforcement', () => {
  const p = new RewardModulatedStdp(toyDataset());
  p.enabled = false;
  causal(p, 100); p.reinforce(1, 100);
  assert.equal(p.gain(0), 1);
  assert.equal(p.statistics().updates, 0);
  p.enabled = true;
  causal(p, 200); p.reinforce(1, 200);
  assert.ok(p.gain(0) > 1);
  p.clearEligibility(200);
  assert.ok(p.traces.every(value => value === 0));
  assert.equal(p.statistics().signal, 0);
});
