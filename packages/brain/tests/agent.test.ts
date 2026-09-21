import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  DEFAULT_STIMULATION_MS,
  GAMEBOY_MS_PER_FRAME,
  NeuralAgent,
  type AgentState,
  type RewardEvent,
} from '../src/agent/agent';
import {
  AGENT_CHUNK_NAMES,
  agentFromChunks,
  agentToChunks,
  decodeEnvelope,
  encodeEnvelope,
  type AgentManifest,
} from '../src/agent/envelope';
import type { BrainDataset } from '../src/dataset/format';
import { loadBrainDatasetFromDir } from '../src/dataset/load-node';
import type { DecoderConfig } from '../src/readout/decoder';
import { gameboyDecoderConfig, toButtonMask } from '../src/readout/presets/gameboy';
import { toyDataset, xorshift } from './fixtures/toy-dataset';
import { MotorDecoder } from './legacy/decoder';
import { FlyBrain as LegacyFlyBrain } from './legacy/lif';

const FRAME_WIDTH = 160;
const FRAME_HEIGHT = 144;
const MAGIC = 'FLYBRAIN';

/**
 * Synthetic connectome big enough for the loop to be worth measuring.
 *
 * The shared toy dataset has four neurons and one command role, so a Game Boy readout on it can
 * never fire a pulse channel and the exclusive group has one real candidate. This fixture keeps
 * the same shape (CSR, roles, retina columns) but sizes the populations so that: the noise budget
 * of 300 kicks spread over 4096 neurons leaves the network sub-threshold and the connectome
 * actually drives it, all eight `command_*` roles exist with distinct neurons, and every
 * kenyon -> mbon edge is positive so plasticity has slots to move.
 */
function syntheticConnectome(seed = 20260915): BrainDataset {
  const random = xorshift(seed);
  const neurons = 4096;
  const kenyon = { from: 0, to: 1024 };
  const mbon = { from: 1024, to: 1280 };
  const roles: Record<string, number[]> = {
    kenyon: range(kenyon.from, kenyon.to),
    mbon: range(mbon.from, mbon.to),
    reward_pam: range(1280, 1344),
  };
  for (let command = 0; command < 8; command++) roles[`command_${command}`] = range(1344 + command * 32, 1376 + command * 32);
  const visual = range(1600, 2400);

  const indptr = new Uint32Array(neurons + 1);
  const targets: number[] = [];
  const weights: number[] = [];
  for (let source = 0; source < neurons; source++) {
    indptr[source] = targets.length;
    const isKenyon = source >= kenyon.from && source < kenyon.to;
    for (let edge = 0; edge < 10; edge++) {
      // Four of every Kenyon cell's ten edges land on an MBON with a positive weight: those are
      // the ones the plasticity rule is allowed to select.
      const forced = isKenyon && edge < 4;
      const target = forced
        ? mbon.from + Math.floor(random() * (mbon.to - mbon.from))
        : Math.floor(random() * neurons);
      const magnitude = 1 + Math.floor(random() * 40);
      targets.push(target);
      weights.push(!forced && random() < 0.2 ? -magnitude : magnitude);
    }
  }
  indptr[neurons] = targets.length;

  const visualXY = new Float32Array(visual.length * 2);
  for (let column = 0; column < visual.length; column++) {
    visualXY[column * 2] = (column % 40) * 7.5;
    visualXY[column * 2 + 1] = Math.floor(column / 40) * 5.25;
  }
  return {
    meta: {
      schemaVersion: 1, dataset: 'synthetic', neurons, edges: targets.length, roles,
      visual: { population: 'synthetic-retina', count: visual.length },
    },
    indptr,
    targets: Uint32Array.from(targets),
    weights: Int16Array.from(weights),
    visualIndices: Uint32Array.from(visual),
    visualHemisphere: Uint8Array.from(visual.map((_, column) => column % 2)),
    visualXY,
  };
}

function range(from: number, to: number): number[] {
  return Array.from({ length: to - from }, (_, index) => from + index);
}

/** Deterministic RGBA frames, cycled by the loops below so both sides see the same images. */
function framePool(count: number, seed = 4242): Uint8Array[] {
  const random = xorshift(seed);
  return Array.from({ length: count }, () => {
    const rgba = new Uint8Array(FRAME_WIDTH * FRAME_HEIGHT * 4);
    for (let index = 0; index < rgba.length; index++) rgba[index] = Math.floor(random() * 256);
    return rgba;
  });
}

/** Rewards, boot mode and manual-input frames, on prime-ish periods so they interleave. */
function rewardsFor(frame: number): RewardEvent[] {
  if (frame % 137 === 0) return [{ value: -0.4, stimulationMs: 40 }, { value: 0.25 }];
  if (frame % 50 === 0) return [{ value: 0.6 }];
  return [];
}
const bootFor = (frame: number): boolean => Math.floor(frame / 120) % 2 === 0;
const learnFor = (frame: number): boolean => frame % 90 !== 0;

/**
 * The Game Boy preset with the exclusive group's timings pinned to `MotorDecoder`'s own hardcoded
 * constants: a 400 ms hold, a 400 ms decision period and a 0.08 fatigue gain.
 *
 * The oracle below proves that `NeuralAgent` reproduces the original worker loop frame by frame,
 * and the loop it is compared against holds `MotorDecoder`, whose timings are not configurable.
 * `docs/design/room-escape.md` sections 1 and 3 moved the live preset's hold, fatigue gain and
 * hysteresis and switched on its blocked-direction cooldown, any of which would make the oracle fail
 * for a reason that has nothing to do with the agent's glue. The live numbers are pinned in
 * `tests/readout.test.ts` instead, and `tools/golden.ts`'s `agent` scenario carries them into the
 * Rust port.
 */
function legacyGameboyConfig(): DecoderConfig {
  const config = gameboyDecoderConfig();
  return {
    ...config,
    exclusive: {
      ...config.exclusive!,
      decisionMs: 400,
      holdMs: 400,
      fatigueGain: 0.08,
      hysteresis: 1.15,
      blockedFatigue: 0,
      blockedMs: 0,
    },
  };
}

function agentFor(data: BrainDataset): NeuralAgent {
  return new NeuralAgent(data, { decoder: legacyGameboyConfig(), frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT } });
}

/** The original worker's initialize + per-frame sequence, written out by hand. */
class LegacyLoop {
  readonly brain: LegacyFlyBrain;
  readonly decoder = new MotorDecoder();
  remainder = 0;

  constructor(data: BrainDataset, firstFrame: Uint8Array) {
    this.brain = new LegacyFlyBrain(data);
    this.brain.plasticity.enabled = false;
    this.brain.step(2_500);
    this.brain.plasticity.enabled = true;
    this.decoder.calibrate(this.brain.rates);
    this.brain.setVisualFrame(firstFrame);
  }

  /** Returns the button mask and the number of 1-ms steps taken. */
  frame(image: Uint8Array, events: RewardEvent[], boot: boolean, learn: boolean): { mask: number; steps: number } {
    this.remainder += GAMEBOY_MS_PER_FRAME;
    const steps = Math.floor(this.remainder);
    this.remainder -= steps;
    this.brain.step(steps);
    const mask = this.decoder.decode(this.brain.rates, this.brain.ms, boot);
    this.brain.setVisualFrame(image);
    for (const event of events) this.brain.reward(event.stimulationMs ?? DEFAULT_STIMULATION_MS);
    if (learn) this.brain.plasticity.reinforce(events.reduce((sum, event) => sum + event.value, 0), this.brain.ms);
    return { mask, steps };
  }
}

function cloneState(state: AgentState): AgentState {
  const { network } = state;
  return {
    ...state,
    network: {
      ...network,
      membrane: network.membrane.slice(),
      refractory: network.refractory.slice(),
      lastSpikeMs: network.lastSpikeMs.slice(),
      visualDrive: network.visualDrive.slice(),
      rates: { ...network.rates },
      plasticity: {
        ...network.plasticity,
        gains: network.plasticity.gains.slice(),
        traces: network.plasticity.traces.slice(),
        touched: network.plasticity.touched.slice(),
      },
    },
    decoder: {
      ...state.decoder,
      baseline: { ...state.decoder.baseline },
      heldUntil: { ...state.decoder.heldUntil },
      nextAllowed: { ...state.decoder.nextAllowed },
      fatigue: { ...state.decoder.fatigue },
    },
  };
}

// --- Oracle: the agent loop against the original worker's glue, frame by frame.

test('ORACLE: NeuralAgent reproduces the original worker loop over 600 frames', () => {
  const data = syntheticConnectome();
  const frames = framePool(16);
  const legacy = new LegacyLoop(data, frames[0]!);
  const agent = agentFor(data);
  agent.warmup(frames[0]!);

  // Warm-up must land on identical state before a single frame is ticked.
  assert.deepEqual(agent.network.exportState(), legacy.brain.exportState());
  assert.equal(agent.network.ms, 2_500);

  let fired = 0;
  let directions = 0;
  for (let frame = 1; frame <= 600; frame++) {
    const image = frames[frame % frames.length]!;
    const events = rewardsFor(frame);
    const boot = bootFor(frame);
    const learn = learnFor(frame);
    const expected = legacy.frame(image, events, boot, learn);
    const result = agent.tick(image, { rewards: events, boot, learn });
    assert.equal(result.steps, expected.steps, `step count differs at frame ${frame}`);
    assert.equal(toButtonMask(result.active), expected.mask, `button mask differs at frame ${frame}`);
    if (result.active.some(channel => ['a', 'b', 'start', 'select'].includes(channel))) fired++;
    if (result.active.some(channel => ['up', 'down', 'left', 'right'].includes(channel))) directions++;
  }

  // A run that never fires a pulse or never holds a direction would pass vacuously.
  assert.ok(fired > 0, 'no pulse channel ever fired');
  assert.ok(directions > 0, 'no direction was ever held');
  assert.deepEqual(agent.network.exportState(), legacy.brain.exportState());
  assert.ok(agent.plasticity.statistics().updates > 0, 'plasticity never updated');
  assert.ok(agent.plasticity.statistics().changed > 0, 'no gain ever moved');

  const before = legacy.decoder.exportState();
  const after = agent.exportState();
  assert.deepEqual(after.decoder.baseline, before.baseline);
  assert.deepEqual(after.decoder.heldUntil, before.heldUntil);
  assert.deepEqual(after.decoder.nextAllowed, before.nextAllowed);
  assert.deepEqual(after.decoder.fatigue, before.fatigue);
  assert.equal(after.decoder.current, before.direction);
  assert.equal(after.decoder.nextDecision, before.nextDirectionDecision);
  assert.equal(after.remainder, legacy.remainder);
  assert.equal(after.warmedUp, true);
  assert.equal(after.version, 1);
});

test('one Game Boy frame is 70224 dot clocks and the loop never drifts', () => {
  assert.equal(GAMEBOY_MS_PER_FRAME, 1000 / (4_194_304 / 70_224));
  const data = toyDataset();
  const agent = new NeuralAgent(data, { decoder: gameboyDecoderConfig(), frame: { width: 8, height: 4 }, warmupMs: 10 });
  assert.equal(agent.msPerFrame, GAMEBOY_MS_PER_FRAME);
  assert.equal(agent.warmupMs, 10);
  assert.equal(agent.ready, false);
  const blank = new Uint8Array(8 * 4 * 4);
  agent.warmup();
  assert.equal(agent.ready, true);
  let steps = 0;
  for (let frame = 0; frame < 1000; frame++) steps += agent.tick(blank).steps;
  // 1000 frames at 59.7275 fps is 16742.7 ms: the fractional remainder must be carried, not lost.
  assert.equal(steps, Math.floor(1000 * GAMEBOY_MS_PER_FRAME));
  assert.equal(agent.network.ms, 10 + steps);
  const remainder = agent.exportState().remainder;
  assert.ok(remainder >= 0 && remainder < 1);
});

test('warmup calibrates once and tick before warmup is refused', () => {
  const data = toyDataset();
  const agent = new NeuralAgent(data, { decoder: gameboyDecoderConfig(), frame: { width: 4, height: 4 }, warmupMs: 50 });
  const blank = new Uint8Array(4 * 4 * 4);
  assert.throws(() => agent.tick(blank), /Warm up the agent/);
  agent.warmup();
  assert.throws(() => agent.warmup(), /already warmed up/);
  // A wrong-sized frame is refused before the network advances, not half way through the tick.
  const before = agent.exportState();
  assert.throws(() => agent.tick(new Uint8Array(3)), /RGBA bytes/);
  assert.throws(() => agent.resetTransients(new Uint8Array(3)), /RGBA bytes/);
  assert.deepEqual(agent.exportState(), before);

  // Calibration happens on settled rates with plasticity re-enabled. Roles the dataset does not
  // declare calibrate to zero, which is what makes their score exactly 1 and keeps them silent.
  assert.equal(agent.plasticity.enabled, true);
  assert.deepEqual(agent.exportState().decoder.baseline, {
    command_0: agent.network.rates.command_0,
    command_1: 0, command_2: 0, command_3: 0, command_4: 0, command_5: 0, command_6: 0, command_7: 0,
  });
  assert.ok(agent.network.rates.command_0! > 0);
});

test('the frame size defaults to the kernel retina and is validated', () => {
  const data = toyDataset();
  const agent = new NeuralAgent(data, { decoder: gameboyDecoderConfig() });
  assert.deepEqual(agent.frame, { width: 160, height: 144 });
  assert.throws(() => new NeuralAgent(data, { decoder: gameboyDecoderConfig(), frame: { width: 0, height: 4 } }), /frame size/);
  assert.throws(() => new NeuralAgent(data, { decoder: gameboyDecoderConfig(), warmupMs: -1 }), /warmupMs/);
  assert.throws(() => new NeuralAgent(data, { decoder: gameboyDecoderConfig(), msPerFrame: 0 }), /msPerFrame/);
});

// --- The game-recovery hook.

test('resetTransients drops holds and traces but keeps the RNG, clock and gains', () => {
  const data = syntheticConnectome();
  const frames = framePool(8, 77);
  const agent = agentFor(data);
  agent.warmup(frames[0]!);
  for (let frame = 1; frame <= 60; frame++) {
    agent.tick(frames[frame % frames.length]!, { rewards: rewardsFor(frame), boot: bootFor(frame) });
  }

  const before = agent.exportState();
  assert.ok(before.network.plasticity.traces.some(value => value !== 0), 'no eligibility to clear');
  assert.ok(Object.values(before.decoder.heldUntil).some(value => value > 0), 'no hold to clear');

  agent.resetTransients(frames[3]!);
  const after = agent.exportState();

  // Kept: everything the rollback did not invalidate.
  assert.equal(after.network.ms, before.network.ms);
  assert.equal(after.network.rng, before.network.rng);
  assert.equal(after.network.populationRate, before.network.populationRate);
  assert.deepEqual(after.network.rates, before.network.rates);
  assert.deepEqual(after.network.membrane, before.network.membrane);
  assert.deepEqual(after.network.plasticity.gains, before.network.plasticity.gains);
  assert.equal(after.network.plasticity.updates, before.network.plasticity.updates);
  assert.deepEqual(after.decoder.baseline, before.decoder.baseline);
  assert.equal(after.remainder, before.remainder);

  // Dropped: the timeline that no longer exists.
  assert.ok(after.network.plasticity.traces.every(value => value === 0));
  assert.ok(after.network.plasticity.touched.every(value => value === before.network.ms));
  assert.equal(after.network.plasticity.signal, 0);
  assert.ok(Object.values(after.decoder.heldUntil).every(value => value === 0));
  assert.ok(Object.values(after.decoder.nextAllowed).every(value => value === before.network.ms + 480));
  assert.equal(after.decoder.current, null);
  assert.ok(Object.values(after.decoder.fatigue).every(value => value === 0));
  assert.equal(after.decoder.nextDecision, before.network.ms);

  // The replacement image became the visual drive.
  assert.notDeepEqual(after.network.visualDrive, before.network.visualDrive);
});

// --- Checkpoints.

test('an exported agent resumes bit-exactly in a fresh agent', () => {
  const data = syntheticConnectome();
  const frames = framePool(8, 909);
  const source = agentFor(data);
  source.warmup(frames[0]!);
  for (let frame = 1; frame <= 200; frame++) {
    source.tick(frames[frame % frames.length]!, { rewards: rewardsFor(frame), boot: bootFor(frame), learn: learnFor(frame) });
  }

  const restored = agentFor(data);
  assert.equal(restored.ready, false);
  restored.importState(source.exportState());
  assert.equal(restored.ready, true);
  assert.deepEqual(restored.exportState(), source.exportState());
  assert.equal(restored.compatibility(), source.compatibility());

  for (let frame = 201; frame <= 320; frame++) {
    const image = frames[frame % frames.length]!;
    const options = { rewards: rewardsFor(frame), boot: bootFor(frame), learn: learnFor(frame) };
    const expected = source.tick(image, options);
    const actual = restored.tick(image, options);
    assert.deepEqual(actual, expected, `diverged at frame ${frame}`);
  }
  assert.deepEqual(restored.exportState(), source.exportState());
  assert.deepEqual(restored.snapshot(), source.snapshot());
});

test('snapshot reports the network without exposing its arrays', () => {
  const data = syntheticConnectome();
  const frames = framePool(4, 31);
  const agent = agentFor(data);
  agent.warmup(frames[0]!);
  agent.tick(frames[1]!, { rewards: [{ value: 1 }] });
  const snapshot = agent.snapshot();
  assert.equal(snapshot.ms, agent.network.ms);
  assert.equal(snapshot.populationRate, agent.network.populationRate);
  assert.deepEqual(snapshot.rates, agent.network.rates);
  assert.notEqual(snapshot.rates, agent.network.rates);
  assert.equal(snapshot.spikeTimes.length, agent.network.lastSpikeMs.length);
  assert.deepEqual(snapshot.spikeTimes, Float32Array.from(agent.network.lastSpikeMs));
  assert.equal(snapshot.learning.version, 'fly-kc-mbon-rstdp-v2');
  assert.ok(snapshot.learning.synapses > 0);
});

test('a rejected checkpoint leaves the agent exactly as it was', () => {
  const data = syntheticConnectome();
  const frames = framePool(8, 1234);
  const agent = agentFor(data);
  agent.warmup(frames[0]!);
  for (let frame = 1; frame <= 100; frame++) agent.tick(frames[frame % frames.length]!, { rewards: rewardsFor(frame) });
  const early = agent.exportState();
  for (let frame = 101; frame <= 200; frame++) agent.tick(frames[frame % frames.length]!, { rewards: rewardsFor(frame) });
  const current = agent.exportState();

  const reject = (state: AgentState, pattern: RegExp) => {
    assert.throws(() => agent.importState(state), pattern);
    assert.deepEqual(agent.exportState(), current, 'a failed import must not change the agent');
  };

  // Version and warm-up flag.
  reject({ ...cloneState(early), version: 2 as unknown as 1 }, /checkpoint version/);
  reject({ ...cloneState(early), warmedUp: 'yes' as unknown as boolean }, /warm-up flag/);

  // A remainder outside [0, 1) would desynchronize the network from the environment clock.
  reject({ ...cloneState(early), remainder: 1.25 }, /frame remainder/);
  reject({ ...cloneState(early), remainder: -0.5 }, /frame remainder/);
  reject({ ...cloneState(early), remainder: Number.NaN }, /frame remainder/);

  // Corrupt plasticity: rejected by the network before anything is written.
  const badGains = cloneState(early);
  badGains.network.plasticity.gains[0] = 5;
  reject(badGains, /plasticity values/);
  const badTopology = cloneState(early);
  badTopology.network.plasticity.topology += 1;
  reject(badTopology, /plasticity topology/);

  // Corrupt readout only: the network half imports cleanly first, so this is the case that needs
  // the rollback. Without it the agent would keep a frame-100 network under a frame-200 readout.
  const badDecoder = cloneState(early);
  badDecoder.decoder.heldUntil.up = Number.POSITIVE_INFINITY;
  reject(badDecoder, /decoder checkpoint/);
  const badFatigue = cloneState(early);
  badFatigue.decoder.fatigue.left = 4;
  reject(badFatigue, /decoder fatigue/);

  // The valid state it rolled back from still loads.
  agent.importState(early);
  assert.deepEqual(agent.exportState(), early);
});

// --- Envelope.

/** Encodes a manifest verbatim, so decode's own structural checks can be exercised. */
function encodeRaw(magic: string, manifest: object, chunks: Uint8Array[]): ArrayBuffer {
  const magicBytes = new TextEncoder().encode(magic);
  const manifestBytes = new TextEncoder().encode(JSON.stringify(manifest));
  const total = magicBytes.length + 8 + manifestBytes.length + chunks.reduce((sum, chunk) => sum + 4 + chunk.byteLength, 0);
  const output = new Uint8Array(total);
  const view = new DataView(output.buffer);
  let offset = 0;
  output.set(magicBytes, offset); offset += magicBytes.length;
  view.setUint32(offset, manifestBytes.length, true); offset += 4;
  output.set(manifestBytes, offset); offset += manifestBytes.length;
  for (const chunk of chunks) {
    view.setUint32(offset, chunk.byteLength, true); offset += 4;
    output.set(chunk, offset); offset += chunk.byteLength;
  }
  view.setUint32(offset, crc32(output.subarray(0, offset)), true);
  return output.buffer;
}

/** Independent CRC32, so the test does not trust the implementation it is checking. */
function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

test('an envelope round-trips its manifest and chunks', () => {
  const chunks = { alpha: Uint8Array.of(1, 2, 3), beta: new Uint8Array(0), gamma: Uint8Array.from({ length: 300 }, (_, i) => i & 0xff) };
  const buffer = encodeEnvelope(MAGIC, { note: 'hello', count: 7 }, chunks);
  const decoded = decodeEnvelope(buffer, MAGIC);
  assert.equal(decoded.manifest.schemaVersion, 2);
  assert.equal(decoded.manifest.note, 'hello');
  assert.equal(decoded.manifest.count, 7);
  assert.deepEqual(decoded.manifest.chunks, ['alpha', 'beta', 'gamma']);
  assert.deepEqual({ ...decoded.chunks }, chunks);
  // Chunk names cannot reach a prototype key.
  assert.equal(Object.getPrototypeOf(decoded.chunks), null);
  // The footer is a CRC32 over everything before it.
  const bytes = new Uint8Array(buffer);
  assert.equal(new DataView(buffer).getUint32(bytes.length - 4, true), crc32(bytes.subarray(0, -4)));
  assert.equal(new TextDecoder().decode(bytes.subarray(0, 8)), MAGIC);
});

test('an envelope rejects a foreign magic, a bad schema and a bad chunk name', () => {
  const buffer = encodeEnvelope(MAGIC, {}, { alpha: Uint8Array.of(9) });
  assert.throws(() => decodeEnvelope(buffer, 'OTHERMAG'), /Not a OTHERMAG envelope/);
  assert.throws(() => decodeEnvelope(new Uint8Array(3).buffer, MAGIC), /Not a FLYBRAIN envelope/);
  assert.throws(() => encodeEnvelope('', {}, {}), /magic/);
  assert.throws(() => encodeEnvelope(MAGIC, {}, { 'bad-name': Uint8Array.of(1) }), /chunk name/);

  assert.throws(() => decodeEnvelope(encodeRaw(MAGIC, { schemaVersion: 1, chunks: [] }, []), MAGIC), /envelope schema: 1/);
  assert.throws(() => decodeEnvelope(encodeRaw(MAGIC, { schemaVersion: 2, chunks: ['bad-name'] }, [Uint8Array.of(1)]), MAGIC), /Invalid envelope chunks/);
  assert.throws(() => decodeEnvelope(encodeRaw(MAGIC, { schemaVersion: 2, chunks: ['a', 'a'] }, [Uint8Array.of(1), Uint8Array.of(2)]), MAGIC), /Invalid envelope chunks/);
  assert.throws(() => decodeEnvelope(encodeRaw(MAGIC, { schemaVersion: 2, chunks: [7] }, [Uint8Array.of(1)]), MAGIC), /Invalid envelope chunks/);
  assert.throws(() => decodeEnvelope(encodeRaw(MAGIC, { schemaVersion: 2, chunks: 'alpha' }, []), MAGIC), /Invalid envelope chunks/);
});

test('an envelope rejects corruption, truncation and trailing bytes', () => {
  const chunks = { alpha: Uint8Array.from({ length: 64 }, (_, i) => i), beta: Uint8Array.of(5, 6) };
  const original = new Uint8Array(encodeEnvelope(MAGIC, { note: 'x' }, chunks));

  const flipped = original.slice();
  flipped[flipped.length - 10] ^= 0x01;
  assert.throws(() => decodeEnvelope(flipped.buffer as ArrayBuffer, MAGIC), /checksum mismatch/);

  const flippedManifest = original.slice();
  flippedManifest[20] ^= 0x20;
  assert.throws(() => decodeEnvelope(flippedManifest.buffer as ArrayBuffer, MAGIC), /checksum mismatch|JSON|Unsupported|Invalid/);

  const trailing = new Uint8Array(original.length + 3);
  trailing.set(original);
  assert.throws(() => decodeEnvelope(trailing.buffer, MAGIC), /checksum mismatch/);

  // Trailing data with a valid footer: only the length bookkeeping can catch it.
  const padded = new Uint8Array(original.length + 4);
  padded.set(original.subarray(0, original.length - 4));
  new DataView(padded.buffer).setUint32(padded.length - 4, crc32(padded.subarray(0, -4)), true);
  assert.throws(() => decodeEnvelope(padded.buffer, MAGIC), /trailing data/);

  const shortChunk = original.slice(0, original.length - 8);
  assert.throws(() => decodeEnvelope(shortChunk.buffer as ArrayBuffer, MAGIC), /truncated|checksum mismatch/);

  const manifestBytes = new TextEncoder().encode(JSON.stringify({ schemaVersion: 2, chunks: ['alpha'] }));
  const truncatedManifest = new Uint8Array(MAGIC.length + 4 + manifestBytes.length - 5);
  truncatedManifest.set(new TextEncoder().encode(MAGIC));
  new DataView(truncatedManifest.buffer).setUint32(MAGIC.length, manifestBytes.length, true);
  assert.throws(() => decodeEnvelope(truncatedManifest.buffer, MAGIC), /manifest is truncated/);

  // A chunk header that claims more bytes than the file holds.
  const overlong = encodeRaw(MAGIC, { schemaVersion: 2, chunks: ['alpha', 'beta'] }, [Uint8Array.of(1, 2, 3)]);
  assert.throws(() => decodeEnvelope(overlong, MAGIC), /truncated/);
});

test('agentToChunks and agentFromChunks survive a full envelope round trip', () => {
  const data = syntheticConnectome();
  const frames = framePool(8, 5150);
  const source = agentFor(data);
  source.warmup(frames[0]!);
  for (let frame = 1; frame <= 150; frame++) {
    source.tick(frames[frame % frames.length]!, { rewards: rewardsFor(frame), boot: bootFor(frame), learn: learnFor(frame) });
  }

  const { manifest, chunks } = agentToChunks(source.exportState());
  assert.deepEqual(Object.keys(chunks), [...AGENT_CHUNK_NAMES]);
  assert.equal(chunks.membrane!.byteLength, data.meta.neurons * 4);
  assert.equal(chunks.lastSpikeMs!.byteLength, data.meta.neurons * 8);
  assert.equal(chunks.visualDrive!.byteLength, data.visualIndices.length * 4);

  const buffer = encodeEnvelope(MAGIC, { ...manifest, compatibility: source.compatibility() }, chunks);
  const decoded = decodeEnvelope(buffer, MAGIC);
  assert.equal(decoded.manifest.compatibility, source.compatibility());
  const state = agentFromChunks(decoded.manifest as unknown as AgentManifest, decoded.chunks);

  const restored = agentFor(data);
  restored.importState(state);
  assert.deepEqual(restored.exportState(), source.exportState());
  for (let frame = 151; frame <= 200; frame++) {
    const image = frames[frame % frames.length]!;
    const options = { rewards: rewardsFor(frame), boot: bootFor(frame), learn: learnFor(frame) };
    assert.deepEqual(restored.tick(image, options), source.tick(image, options), `diverged at frame ${frame}`);
  }
  assert.deepEqual(restored.exportState(), source.exportState());
});

test('agentFromChunks rejects an incomplete or misaligned checkpoint', () => {
  const data = syntheticConnectome();
  const agent = agentFor(data);
  agent.warmup();
  const { manifest, chunks } = agentToChunks(agent.exportState());

  assert.throws(() => agentFromChunks({ ...manifest, agentVersion: 2 as unknown as 1 }, chunks), /checkpoint version/);
  assert.throws(() => agentFromChunks({ ...manifest, decoder: undefined as unknown as AgentManifest['decoder'] }, chunks), /manifest is incomplete/);
  for (const name of AGENT_CHUNK_NAMES) {
    const missing = { ...chunks };
    delete missing[name];
    assert.throws(() => agentFromChunks(manifest, missing), new RegExp(`missing ${name}`));
  }
  const partial = { ...chunks, membrane: chunks.membrane!.subarray(0, chunks.membrane!.byteLength - 1) };
  assert.throws(() => agentFromChunks(manifest, partial), /partial element/);
});

// --- Real dataset, when the artifacts are present in this worktree.

const dataDir = join(fileURLToPath(new URL('../../../', import.meta.url)), 'data', 'fafb-v783');

test('NeuralAgent runs on the real FAFB v783 dataset', {
  skip: existsSync(join(dataDir, 'meta.json')) ? false : 'data/fafb-v783/meta.json is absent in this worktree',
}, async () => {
  const data = await loadBrainDatasetFromDir(dataDir);
  const agent = agentFor(data);
  const frames = framePool(4, 783);
  agent.warmup(frames[0]!);
  assert.equal(agent.network.ms, 2_500);
  assert.ok(agent.network.populationRate > 0);

  let held = 0;
  for (let frame = 1; frame <= 30; frame++) {
    const result = agent.tick(frames[frame % frames.length]!, { rewards: frame % 10 === 0 ? [{ value: 1 }] : [] });
    assert.ok(Number.isFinite(result.spikes) && result.spikes >= 0);
    held += result.active.length;
  }
  const snapshot = agent.snapshot();
  assert.ok(Object.values(snapshot.rates).every(Number.isFinite));
  assert.ok(Object.keys(snapshot.rates).length >= 8);
  assert.ok(Number.isFinite(snapshot.populationRate) && snapshot.populationRate > 0);
  assert.ok(held > 0, 'the readout never held a channel');
  assert.equal(snapshot.learning.synapses, 16_384);

  const compatibility = agent.compatibility();
  assert.ok(compatibility.includes('lif-1ms-f64-v2'), compatibility);
  assert.ok(compatibility.includes('fly-kc-mbon-rstdp-v2'), compatibility);
  assert.ok(compatibility.includes(data.fingerprint!), compatibility);
  assert.equal(compatibility.split('/').length, 3);
});
