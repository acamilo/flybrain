import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';
import type { BrainDataset, BrainMetadata } from '../src/dataset/format';
import { DEFAULT_LIF_CONFIG, FlyBrain, LifNetwork, MAX_RATE_ROLES, NEURAL_KERNEL_VERSION, ROLE_MASK_WORDS, kernelVersion } from '../src/model/lif';
import { DEFAULT_RETINA_CONFIG, projectFrame } from '../src/model/retina';
import { Xorshift32 } from '../src/model/rng';
import { toyDataset, xorshift } from './fixtures/toy-dataset';
import { FlyBrain as LegacyFlyBrain } from './legacy/lif';
import type { Plasticity as LegacyPlasticity } from './legacy/plasticity';
import type { RewardModulatedStdp } from '../src/model/plasticity';

/** Toy connectome with a single retina column on neuron 0, as the original warm-up test used. */
function visualToyDataset(): BrainDataset {
  const data = toyDataset();
  data.visualIndices = Uint32Array.of(0);
  data.visualHemisphere = Uint8Array.of(0);
  data.visualXY = Float32Array.of(0, 0);
  return data;
}

/** Deterministic RGBA frame. */
function randomFrame(width: number, height: number, seed: number): Uint8Array {
  const random = xorshift(seed);
  const rgba = new Uint8Array(width * height * 4);
  for (let i = 0; i < rgba.length; i++) rgba[i] = Math.floor(random() * 256);
  return rgba;
}

/** Gray frame of 4x4 blocks, so nearest-neighbour rounding differences stay inside a block. */
function blockFrame(width: number, height: number): Uint8Array {
  const rgba = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const value = (Math.floor(x * 4 / width) * 4 + Math.floor(y * 4 / height)) * 15 + 3;
      const offset = (y * width + x) * 4;
      rgba[offset] = value; rgba[offset + 1] = value; rgba[offset + 2] = value; rgba[offset + 3] = 255;
    }
  }
  return rgba;
}

/** One causal pre(0)->post(1) pairing 10 ms apart, as in the original unit tests. */
function causal(p: RewardModulatedStdp | LegacyPlasticity, ms: number) {
  p.observe(Uint32Array.of(1), 1, Float64Array.of(ms - 10, -1e6, -1e6, -1e6), ms);
}

test('Xorshift32 reproduces the original kernel stream bit-exactly', () => {
  let legacy = 22_222;
  const legacyUint = () => { let value = legacy; value ^= value << 13; value ^= value >>> 17; value ^= value << 5; legacy = value; return value >>> 0; };
  const rng = new Xorshift32();
  assert.equal(rng.state, 22_222);
  for (let i = 0; i < 10_000; i++) {
    assert.equal(rng.nextUint(), legacyUint());
    assert.equal(rng.state, legacy);
  }
  const checkpoint = rng.state;
  const draws = [rng.next(), rng.next(), rng.next()];
  rng.state = checkpoint;
  assert.deepEqual([rng.next(), rng.next(), rng.next()], draws);
  assert.ok(draws.every(value => value >= 0 && value < 1));
  assert.notEqual(new Xorshift32(7).nextUint(), new Xorshift32().nextUint());
});

test('projectFrame matches the original 160x144 retina projection', () => {
  const xy = Float32Array.of(120, -40, 300, 900, -55, 12, 4, 4, 1000, 1000, 0, 0, 640, 480, 17, 900);
  const hemisphere = Uint8Array.of(0, 1, 0, 1, 1, 0, 1, 0);
  const data = toyDataset();
  data.visualIndices = Uint32Array.of(0, 1, 2, 3, 0, 1, 2, 3);
  data.visualHemisphere = hemisphere;
  data.visualXY = xy;
  const frame = randomFrame(DEFAULT_RETINA_CONFIG.width, DEFAULT_RETINA_CONFIG.height, 3);

  const legacy = new LegacyFlyBrain(data);
  legacy.setVisualFrame(frame);
  const expected = legacy.exportState().visualDrive;

  const out = new Float32Array(8);
  projectFrame(frame, DEFAULT_RETINA_CONFIG.width, DEFAULT_RETINA_CONFIG.height,
    { xy, hemisphere, count: 8 }, DEFAULT_RETINA_CONFIG.gain, out);
  assert.deepEqual(out, expected);
  assert.ok(out.some(value => value !== out[0]), 'frame must actually vary across columns');

  const brain = new LifNetwork(data);
  brain.setVisualFrame(frame);
  assert.deepEqual(brain.exportState().visualDrive, expected);
});

test('retina projection is resolution independent: 320x288 maps to the same relative pixels', () => {
  const xy = Float32Array.of(0, 0, 0.1, 0.35, 0.6, 0.85, 1, 1, 0.35, 0.6, 0.85, 0.1);
  const hemisphere = Uint8Array.of(1, 1, 0, 1, 0, 1);
  const columns = { xy, hemisphere, count: 6 };
  const small = new Float32Array(6);
  const large = new Float32Array(6);
  projectFrame(blockFrame(160, 144), 160, 144, columns, DEFAULT_RETINA_CONFIG.gain, small);
  projectFrame(blockFrame(320, 288), 320, 288, columns, DEFAULT_RETINA_CONFIG.gain, large);
  assert.deepEqual(large, small);
  assert.equal(new Set(small).size, 6, 'columns must land in six distinct blocks');

  // Gain scales the drive linearly and the frame size is a per-call override on the network.
  const doubled = new Float32Array(6);
  projectFrame(blockFrame(320, 288), 320, 288, columns, DEFAULT_RETINA_CONFIG.gain * 2, doubled);
  for (let i = 0; i < 6; i++) assert.equal(doubled[i], Math.fround(small[i] * 2));

  const data = toyDataset();
  data.visualIndices = Uint32Array.of(0, 1, 2, 3, 0, 1);
  data.visualHemisphere = hemisphere;
  data.visualXY = xy;
  const brain = new LifNetwork(data);
  brain.setVisualFrame(blockFrame(320, 288), 320, 288);
  assert.deepEqual(brain.exportState().visualDrive, small);
});

test('LifNetwork is bit-exact with the legacy FlyBrain over 3000 ms on the toy dataset', () => {
  const legacy = new LegacyFlyBrain(visualToyDataset());
  const next = new LifNetwork(visualToyDataset());
  const compare = (label: string) => {
    assert.deepEqual(next.exportState(), legacy.exportState(), label);
    assert.deepEqual(next.rates, legacy.rates, label);
    assert.equal(next.populationRate, legacy.populationRate, label);
    assert.equal(next.ms, legacy.ms, label);
    assert.deepEqual(next.membrane, legacy.membrane, label);
    assert.deepEqual(next.baseline, legacy.baseline, label);
    assert.deepEqual(next.refractory, legacy.refractory, label);
    assert.deepEqual(next.roleNames, legacy.roleNames, label);
    assert.deepEqual(next.plasticity.statistics(), legacy.plasticity.statistics(), label);
  };
  compare('construction');
  assert.equal(legacy.step(100), next.step(100));
  compare('after 100 ms');

  for (let round = 0; round < 6; round++) {
    const frame = randomFrame(160, 144, round + 1);
    legacy.setVisualFrame(frame); next.setVisualFrame(frame);
    assert.equal(legacy.step(300), next.step(300), `round ${round} visual drive`);
    if (round % 2 === 0) { legacy.reward(120); next.stimulate(120); }
    assert.equal(legacy.step(100), next.step(100), `round ${round} stimulation`);
    causal(legacy.plasticity, legacy.ms); causal(next.plasticity, next.ms);
    const reward = round % 2 === 0 ? -0.25 : 0.5;
    legacy.plasticity.reinforce(reward, legacy.ms); next.plasticity.reinforce(reward, next.ms);
    compare(`round ${round}`);
  }

  assert.equal(legacy.step(500), next.step(500));
  compare('after 3000 ms');
  assert.equal(next.ms, 3000);
  assert.ok(next.populationRate > 0, 'the network must actually spike');
  assert.ok(next.plasticity.statistics().changed > 0, 'reinforcement must actually move gains');
  assert.ok(next.rates.command_0 >= 0);

  // A fresh network resumes the legacy checkpoint exactly.
  const resumed = new LifNetwork(visualToyDataset());
  resumed.importState(legacy.exportState());
  assert.deepEqual(resumed.exportState(), legacy.exportState());
  resumed.step(250); legacy.step(250);
  assert.deepEqual(resumed.exportState(), legacy.exportState());
});

test('reward is an alias of stimulate and drives the configured role', () => {
  assert.equal(FlyBrain, LifNetwork);
  const stimulated = new LifNetwork(toyDataset());
  const rewarded = new LifNetwork(toyDataset());
  stimulated.stimulate(50); rewarded.reward(50);
  stimulated.step(10); rewarded.step(10);
  assert.deepEqual(rewarded.exportState(), stimulated.exportState());
  assert.equal(stimulated.exportState().rewardRemaining, 40);
  // Overlapping pulses take the maximum remainder.
  stimulated.stimulate(10); assert.equal(stimulated.exportState().rewardRemaining, 40);
  stimulated.stimulate(200); assert.equal(stimulated.exportState().rewardRemaining, 200);

  // The default role is absent from the toy dataset, so stimulation is a no-op there; a
  // configured role that exists moves the membrane.
  const driven = new LifNetwork(toyDataset(), { stimulation: { role: 'motor', drive: 0.5 }, noiseKicks: 0, baselineMax: 0 });
  driven.stimulate(1); driven.step(1);
  assert.ok(driven.membrane[3] > 0.4 && driven.membrane[0] === 0);
  assert.equal(driven.exportState().rewardRemaining, 0);
});

test('default configuration keeps the historical kernel version; numeric changes derive a new one', () => {
  assert.equal(kernelVersion(), NEURAL_KERNEL_VERSION);
  assert.equal(kernelVersion({ ...DEFAULT_LIF_CONFIG }), NEURAL_KERNEL_VERSION);
  assert.equal(new LifNetwork(toyDataset()).version, NEURAL_KERNEL_VERSION);
  const versions = new Set<string>();
  const patches: Partial<typeof DEFAULT_LIF_CONFIG>[] = [
    { decayMs: 25 }, { threshold: 0.9 }, { refractoryMs: 3 }, { synapseScale: 0.006 }, { baselineMax: 0.05 },
    { noiseKicks: 200 }, { noiseAmount: 0.5 }, { rateAlpha: 1 / 50 }, { membraneFloor: -3 }, { seed: 1 },
    { stimulation: { role: 'reward_pam', drive: 0.3 } },
    { retina: { gain: 0.3, width: 160, height: 144 } },
    { retina: { gain: 0.20, width: 320, height: 144 } },
    { retina: { gain: 0.20, width: 160, height: 288 } },
  ];
  for (const patch of patches) {
    const version = kernelVersion(patch);
    assert.notEqual(version, NEURAL_KERNEL_VERSION);
    assert.match(version, /^lif-1ms-f64-v2:[0-9a-f]{8}$/);
    versions.add(version);
  }
  assert.equal(versions.size, patches.length, 'each numeric parameter must reach the version hash');
  // Role names are dataset labels, not kernel constants: they never move the version.
  assert.equal(kernelVersion({ stimulation: { role: 'other', drive: 0.20 }, rateRoles: ['command_0'] }), NEURAL_KERNEL_VERSION);
  assert.equal(new LifNetwork(toyDataset(), { decayMs: 25 }).version, kernelVersion({ decayMs: 25 }));
});

test('non-default constants change the dynamics, not only the version string', () => {
  // Weak noise keeps the four-neuron toy network near threshold; saturated drive would hide the
  // threshold and baseline parameters, which only matter while spiking is not already maximal.
  const base = { noiseKicks: 2, noiseAmount: 0.2 };
  const reference = new LifNetwork(visualToyDataset(), base);
  reference.step(600);
  for (const patch of [{ decayMs: 25 }, { threshold: 0.5 }, { refractoryMs: 5 }, { synapseScale: 0.05 },
    { baselineMax: 0.2 }, { noiseKicks: 6 }, { noiseAmount: 0.4 }, { rateAlpha: 1 / 5 }, { seed: 99 }]) {
    const variant = new LifNetwork(visualToyDataset(), { ...base, ...patch });
    variant.step(600);
    assert.notDeepEqual(variant.exportState(), reference.exportState(), JSON.stringify(patch));
  }
  // membraneFloor only bites on inhibition, so compare the clamped neuron directly.
  const shallow = new LifNetwork(visualToyDataset(), { membraneFloor: -0.01, noiseKicks: 0, baselineMax: 0 });
  const deep = new LifNetwork(visualToyDataset(), { membraneFloor: -50, noiseKicks: 0, baselineMax: 0 });
  for (const brain of [shallow, deep]) { brain.membrane[0] = 10; brain.step(2); }
  assert.ok(shallow.membrane[2] > deep.membrane[2]);
});

test('rate roles default to the dataset order and are capped at the bitmask width', () => {
  const data = toyDataset();
  data.meta.roles = { mbon: [1, 2], command_3: [3], steer_left: [0], kenyon: [0], command_0: [3], proboscis: [1] };
  assert.deepEqual(new LifNetwork(data).roleNames, ['command_3', 'steer_left', 'command_0', 'proboscis']);
  // A `macro_<type>` population is tracked by default like a `command_*` button
  // (`docs/design/macros.md` section 12: a macro is a button), in the dataset's own order.
  const withMacros = toyDataset();
  withMacros.meta.roles = { macro_talk: [1], command_0: [3], kenyon: [0], macro_go_out: [2] };
  assert.deepEqual(new LifNetwork(withMacros).roleNames, ['macro_talk', 'command_0', 'macro_go_out']);
  // An explicit list keeps its own order and drops names the dataset does not declare.
  assert.deepEqual(new LifNetwork(data, { rateRoles: ['mbon', 'absent', 'command_0'] }).roleNames, ['mbon', 'command_0']);
  assert.deepEqual(new LifNetwork(data, { rateRoles: [] }).rates, {});

  const wide = toyDataset();
  wide.meta.roles = {};
  for (let i = 0; i < MAX_RATE_ROLES; i++) wide.meta.roles[`command_${i}`] = [i % 4];
  const packed = new LifNetwork(wide);
  assert.equal(packed.roleNames.length, MAX_RATE_ROLES);
  assert.ok(packed.roleMasks instanceof Uint32Array);
  assert.equal(packed.roleMasks.length, wide.meta.neurons * ROLE_MASK_WORDS);
  // The top role lives in the last word's top bit, which is what the second word bought
  // (`docs/design/macros.md` section 11 needs thirty-six roles, not fourteen).
  assert.notEqual(packed.roleMasks[3 * ROLE_MASK_WORDS + ROLE_MASK_WORDS - 1] & (1 << 31), 0, 'the last role must occupy the top bit');
  packed.step(200);
  assert.ok(Object.values(packed.rates).every(Number.isFinite));
  assert.ok(packed.rates[`command_${MAX_RATE_ROLES - 1}`] > 0);

  wide.meta.roles[`command_${MAX_RATE_ROLES}`] = [0];
  assert.throws(() => new LifNetwork(wide), new RegExp(String(MAX_RATE_ROLES)));
});

// --- Oracle on the real FAFB v783 artifacts, when the dataset is present in the worktree.

function loadRealDataset(base: string): BrainDataset {
  const meta = JSON.parse(readFileSync(join(base, 'meta.json'), 'utf8')) as BrainMetadata;
  const circuits = JSON.parse(readFileSync(join(base, 'circuit-roles.json'), 'utf8')) as { neurons: number; roles: Record<string, number[]> };
  if (circuits.neurons !== meta.neurons) throw new Error('Circuit roles do not match connectome');
  meta.roles = { ...meta.roles, ...circuits.roles };
  const bytes = (name: string) => new Uint8Array(gunzipSync(readFileSync(join(base, name))));
  const data: BrainDataset = {
    meta,
    indptr: new Uint32Array(bytes('indptr.binz').buffer),
    targets: new Uint32Array(bytes('targets.binz').buffer),
    weights: new Int16Array(bytes('weights.binz').buffer),
    visualIndices: new Uint32Array(bytes('visual-indices.binz').buffer),
    visualHemisphere: bytes('visual-hemisphere.binz'),
    visualXY: new Float32Array(bytes('visual-xy.binz').buffer),
  };
  if (data.indptr.length !== meta.neurons + 1 || data.targets.length !== meta.edges || data.weights.length !== meta.edges) {
    throw new Error('FlyWire artifact lengths do not match metadata');
  }
  return data;
}

const realBase = join(fileURLToPath(new URL('../../../', import.meta.url)), 'data', 'fafb-v783');
const realDatasetPresent = existsSync(join(realBase, 'meta.json'));

test('LifNetwork is bit-exact with the legacy FlyBrain on the real FAFB v783 dataset', {
  skip: realDatasetPresent ? false : 'data/fafb-v783/meta.json is absent in this worktree (dataset branch not merged)',
}, () => {
  const data = loadRealDataset(realBase);
  const legacy = new LegacyFlyBrain(data);
  const next = new LifNetwork(data);
  assert.equal(next.plasticity.edges.length, 16_384);
  assert.deepEqual(next.plasticity.edges, legacy.plasticity.edges);
  assert.equal(next.plasticity.exportState().topology, legacy.plasticity.exportState().topology);
  assert.equal(next.plasticity.exportState().version, legacy.plasticity.exportState().version);
  // The prototype predates the macro populations (`docs/design/macros.md` section 11), so it
  // tracks fourteen roles where this network tracks forty-five. Everything else about the two is
  // still asserted identical, which is the point of the test: the extra roles are counters over
  // neurons that were always in the dataset and they change nothing that spikes.
  const isMacro = (name: string) => name.startsWith('macro_');
  assert.deepEqual(next.roleNames.filter(name => !isMacro(name)), legacy.roleNames);
  // Thirty-one since sections 13 and 14; twenty-two when section 11 shipped.
  assert.equal(next.roleNames.filter(isMacro).length, 31);

  const frame = randomFrame(160, 144, 99);
  legacy.setVisualFrame(frame); next.setVisualFrame(frame);
  assert.equal(legacy.step(100), next.step(100));
  legacy.reward(120); next.stimulate(120);
  assert.equal(legacy.step(100), next.step(100));
  legacy.plasticity.reinforce(1, legacy.ms); next.plasticity.reinforce(1, next.ms);

  const withoutMacros = (rates: Record<string, number>) =>
    Object.fromEntries(Object.entries(rates).filter(([name]) => !isMacro(name)));
  assert.deepEqual({ ...next.exportState(), rates: withoutMacros(next.exportState().rates) }, legacy.exportState());
  assert.deepEqual(withoutMacros(next.rates), legacy.rates);
  // And the new roles are live rather than zero-filled padding.
  assert.ok(next.roleNames.filter(isMacro).some(name => next.rates[name]! > 0));
  assert.equal(next.populationRate, legacy.populationRate);
  assert.equal(next.ms, 200);
  assert.ok(next.populationRate > 0);
});
