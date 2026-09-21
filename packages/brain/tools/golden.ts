/**
 * Golden-file generator: the TypeScript oracle dumps state, the Rust port compares it.
 *
 * Run from the repository root:
 *
 * ```sh
 * npx tsx packages/brain/tools/golden.ts
 * ```
 *
 * Writes `services/flysim/golden/*.flygold`, one file per scenario, using the library's own
 * checkpoint envelope (`agent/envelope.ts`) with the magic `FLYGOLD1`: a JSON manifest holding the
 * scenario definition and every scalar, plus named binary chunks holding the arrays verbatim. That
 * format is exact (no float reformatting), compact, and reusing it means the Rust envelope port is
 * exercised by every golden test.
 *
 * Pass scenario names as arguments to regenerate only those files, which is how a change to one
 * preset lands without rewriting the others:
 *
 * ```sh
 * npx tsx packages/brain/tools/golden.ts platformer
 * ```
 *
 * Five scenarios:
 *
 * - `math`   the transcendental arguments the kernel actually produces, so a libm difference
 *            between V8 and Rust is caught on its own rather than as a mystery state divergence.
 * - `toy`    the four-neuron fixture plus retina columns, 3,000 ms with a frame, a stimulation
 *            pulse and two reinforcements; full arrays at 1,000, 2,000 and 3,000 ms.
 * - `agent`  the 4,096-neuron synthetic connectome from `tests/agent.test.ts` through
 *            `NeuralAgent` and the Game Boy readout for 600 frames; per-frame button masks, array
 *            digests at frames 200 and 400, full arrays at 600.
 * - `real`   `data/fafb-v783` for 200 ms with a frame, a stimulation pulse and a reinforcement;
 *            digests and per-millisecond spike counts only, because the arrays are 139k long.
 * - `platformer` the platformer decoder preset over a seeded rate sequence, with the clock and the
 *            rates carried as chunks so the Rust twin decodes the same input without reimplementing
 *            the generator. Masks per step plus the final decoder state.
 *
 * Scenario fixtures travel in the golden file rather than being re-derived on the Rust side: the
 * toy connectome as manifest JSON, the synthetic connectome as chunks. Only the seeded RGBA frame
 * generator is reimplemented in Rust, and the manifest carries a SHA-256 of the frame pool so a
 * mistake there fails as itself.
 */
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { NeuralAgent, type RewardEvent } from '../src/agent/agent';
import { encodeEnvelope } from '../src/agent/envelope';
import type { BrainDataset } from '../src/dataset/format';
import { loadBrainDatasetFromDir } from '../src/dataset/load-node';
import { LifNetwork, kernelVersion, type LifState } from '../src/model/lif';
import { plasticityVersion } from '../src/model/plasticity';
import { PopulationDecoder } from '../src/readout/decoder';
import { gameboyDecoderConfig, toButtonMask } from '../src/readout/presets/gameboy';
import { platformerDecoderConfig } from '../src/readout/presets/platformer';
import { toyDataset, xorshift } from '../tests/fixtures/toy-dataset';

const MAGIC = 'FLYGOLD1';
const REPO_ROOT = join(fileURLToPath(new URL('../../../', import.meta.url)));
const GOLDEN_DIR = join(REPO_ROOT, 'services', 'flysim', 'golden');
const FRAME_WIDTH = 160;
const FRAME_HEIGHT = 144;

// --- Plumbing ------------------------------------------------------------------------------------

type Chunks = Record<string, Uint8Array>;

/** Raw little-endian bytes of a typed array, as the array itself holds them. */
function raw(array: ArrayBufferView): Uint8Array {
  return new Uint8Array(array.buffer as ArrayBuffer, array.byteOffset, array.byteLength).slice();
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function write(name: string, manifest: object, chunks: Chunks): void {
  const buffer = encodeEnvelope(MAGIC, manifest, chunks);
  const path = join(GOLDEN_DIR, `${name}.flygold`);
  writeFileSync(path, new Uint8Array(buffer));
  const bytes = new Uint8Array(buffer).length;
  console.log(`${name}.flygold  ${bytes.toLocaleString()} bytes, ${Object.keys(chunks).length} chunks`);
}

/** Deterministic RGBA frames, the generator `tests/agent.test.ts` and the bench both use. */
function framePool(count: number, seed: number): Uint8Array[] {
  const random = xorshift(seed);
  return Array.from({ length: count }, () => {
    const rgba = new Uint8Array(FRAME_WIDTH * FRAME_HEIGHT * 4);
    for (let index = 0; index < rgba.length; index++) rgba[index] = Math.floor(random() * 256);
    return rgba;
  });
}

/** SHA-256 over a whole frame pool, so the Rust reimplementation of the generator is checked. */
function poolDigest(frames: Uint8Array[]): string {
  const hash = createHash('sha256');
  for (const frame of frames) hash.update(frame);
  return hash.digest('hex');
}

/** The scalar half of a `LifState`, as manifest JSON. */
function networkScalars(state: LifState) {
  return {
    rng: state.rng,
    rewardRemaining: state.rewardRemaining,
    ms: state.ms,
    populationRate: state.populationRate,
    rates: { ...state.rates },
    plasticity: {
      version: state.plasticity.version,
      topology: state.plasticity.topology,
      enabled: state.plasticity.enabled,
      updates: state.plasticity.updates,
      signal: state.plasticity.signal,
    },
  };
}

/** The seven arrays of a `LifState` as chunks, suffixed so several dumps can share one envelope. */
function networkChunks(state: LifState, suffix: string): Chunks {
  return {
    [`membrane${suffix}`]: raw(state.membrane),
    [`refractory${suffix}`]: raw(state.refractory),
    [`lastSpikeMs${suffix}`]: raw(state.lastSpikeMs),
    [`visualDrive${suffix}`]: raw(state.visualDrive),
    [`gains${suffix}`]: raw(state.plasticity.gains),
    [`traces${suffix}`]: raw(state.plasticity.traces),
    [`touched${suffix}`]: raw(state.plasticity.touched),
  };
}

/** The same seven arrays as digests, for a dump too large to carry verbatim. */
function networkDigests(state: LifState) {
  return {
    membrane: sha256(raw(state.membrane)),
    refractory: sha256(raw(state.refractory)),
    lastSpikeMs: sha256(raw(state.lastSpikeMs)),
    visualDrive: sha256(raw(state.visualDrive)),
    gains: sha256(raw(state.plasticity.gains)),
    traces: sha256(raw(state.plasticity.traces)),
    touched: sha256(raw(state.plasticity.touched)),
  };
}

// --- Scenario: math ------------------------------------------------------------------------------

/**
 * The transcendental arguments the kernel produces, on their own.
 *
 * The eligibility trace decays by `exp(-k/5000)` for an integer millisecond gap `k`, a spike pair
 * weighs `exp(-dt/20)` for an integer `dt` in 1..100, the membrane decay is
 * `fround(exp(-1/decayMs))`, and the modulator is `tanh` of a sum of reward-catalog values. Those
 * are the only shapes, and all of them are dumped here.
 */
function scenarioMath(): void {
  const pairs = new Float64Array(100);
  for (let dt = 1; dt <= 100; dt++) pairs[dt - 1] = Math.exp(-dt / 20);

  // 2,000,001 doubles is 16 MB, so the trace decay travels as a digest rather than a chunk.
  const traceHash = createHash('sha256');
  const scratch = Buffer.alloc(8);
  for (let k = 0; k <= 2_000_000; k++) {
    scratch.writeDoubleLE(Math.exp(-k / 5000));
    traceHash.update(scratch);
  }

  // Sums of the Pokemon catalog's values, which is what `reinforce` is actually handed.
  const catalog = [1, 0.05, 0.2, 0.5, 0.5, 0.1, 3, -0.4, 0.25, 0.6, -0.5];
  const random = xorshift(20260915);
  const rewards = new Float64Array(4096);
  const modulators = new Float64Array(4096);
  for (let index = 0; index < rewards.length; index++) {
    let sum = 0;
    const terms = 1 + Math.floor(random() * 4);
    for (let term = 0; term < terms; term++) sum += catalog[Math.floor(random() * catalog.length)]!;
    rewards[index] = sum;
    modulators[index] = Math.tanh(sum);
  }

  const decays: Record<string, number> = {};
  for (const decayMs of [5, 10, 20, 25, 30, 50, 100]) {
    decays[String(decayMs)] = Math.fround(Math.exp(-1 / decayMs));
  }

  write(
    'math',
    {
      scenario: 'math',
      note: 'exp and tanh arguments the 1-ms kernel produces; see jsmath.rs',
      froundDecay: decays,
      expTraceDigest: traceHash.digest('hex'),
      expTraceCount: 2_000_001,
      tanhCount: rewards.length,
    },
    { expPair: raw(pairs), tanhInput: raw(rewards), tanhOutput: raw(modulators) },
  );
}

// --- Scenario: toy -------------------------------------------------------------------------------

/**
 * The shared four-neuron fixture, plus the two retina columns and the `reward_pam` role it needs
 * to exercise visual drive and the stimulation pulse.
 *
 * `toyDataset()` declares no retina and no stimulation role, so a default-config run would leave
 * both paths dead. Adding them keeps the configuration default (the role name is a dataset label,
 * not a kernel constant) while making steps 2 and 3 of the tick observable.
 */
function goldenToyDataset(): BrainDataset {
  const data = toyDataset();
  data.meta.roles.reward_pam = [2];
  data.meta.visual = { population: 'test', count: 2 };
  data.visualIndices = Uint32Array.of(0, 3);
  data.visualHemisphere = Uint8Array.of(0, 1);
  data.visualXY = Float32Array.of(0, 0, 12.5, 7.25);
  return data;
}

/** The connectome as manifest JSON: small enough that the Rust side can rebuild it exactly. */
function datasetJson(data: BrainDataset) {
  return {
    meta: {
      schemaVersion: data.meta.schemaVersion,
      dataset: data.meta.dataset,
      neurons: data.meta.neurons,
      edges: data.meta.edges,
      roles: Object.fromEntries(Object.entries(data.meta.roles).map(([name, list]) => [name, [...list]])),
      visual: { ...data.meta.visual },
    },
    indptr: [...data.indptr],
    targets: [...data.targets],
    weights: [...data.weights],
    visualIndices: [...data.visualIndices],
    visualHemisphere: [...data.visualHemisphere],
    visualXY: [...data.visualXY],
  };
}

function scenarioToy(): void {
  const data = goldenToyDataset();
  const network = new LifNetwork(data);
  const frames = framePool(1, 7);
  const frame = frames[0]!;

  // 3,000 ms: a frame at 100, a 120 ms pulse from 500, reinforce(1) at 700, reinforce(-0.5) at
  // 2,000, and a state dump at each kilosecond boundary.
  const spikes: number[] = [];
  const dumps: unknown[] = [];
  let chunks: Chunks = { frame: frame.slice() };
  const suffixes = ['A', 'B', 'C'];
  let dumped = 0;

  for (let ms = 0; ms < 3000; ms++) {
    if (ms === 100) network.setVisualFrame(frame, FRAME_WIDTH, FRAME_HEIGHT);
    if (ms === 500) network.stimulate(120);
    spikes.push(network.step(1));
    if (network.ms === 700) network.plasticity.reinforce(1, network.ms);
    if (network.ms === 2000) network.plasticity.reinforce(-0.5, network.ms);
    if (network.ms === 1000 || network.ms === 2000 || network.ms === 3000) {
      const state = network.exportState();
      const suffix = suffixes[dumped++]!;
      dumps.push({ atMs: network.ms, suffix, network: networkScalars(state), digests: networkDigests(state) });
      chunks = { ...chunks, ...networkChunks(state, suffix) };
    }
  }

  write(
    'toy',
    {
      scenario: 'toy',
      dataset: datasetJson(data),
      frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT, seed: 7, digest: poolDigest(frames) },
      script: {
        totalMs: 3000,
        frameAtMs: 100,
        stimulateAtMs: 500,
        stimulationMs: 120,
        reinforce: [
          { atMs: 700, reward: 1 },
          { atMs: 2000, reward: -0.5 },
        ],
        dumpAtMs: [1000, 2000, 3000],
      },
      kernelVersion: network.version,
      plasticityVersion: network.plasticity.version,
      roleNames: [...network.roleNames],
      baselineDigest: sha256(raw(network.baseline)),
      plasticEdges: [...network.plasticity.edges],
      spikesPerMs: spikes,
      totalSpikes: spikes.reduce((sum, value) => sum + value, 0),
      dumps,
    },
    chunks,
  );
}

// --- Scenario: agent -----------------------------------------------------------------------------

function range(from: number, to: number): number[] {
  return Array.from({ length: to - from }, (_, index) => from + index);
}

/**
 * The 4,096-neuron synthetic connectome from `tests/agent.test.ts`, verbatim.
 *
 * Kept identical because it is sized so the loop is worth measuring: 300 noise kicks over 4,096
 * neurons leave the network sub-threshold and the connectome actually drives it, all eight
 * `command_*` roles exist with distinct neurons, and every Kenyon-to-MBON edge is positive so
 * plasticity has slots to move.
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
      const forced = isKenyon && edge < 4;
      const target = forced
        ? mbon.from + Math.floor(random() * (mbon.to - mbon.from))
        : Math.floor(random() * neurons);
      const magnitude = 1 + Math.floor(random() * 40);
      targets.push(target);
      // Short-circuit: a forced edge never draws the sign sample, so it consumes two draws and a
      // free edge consumes three. The Rust side receives these arrays rather than re-deriving them.
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

/** The per-frame schedule; the Rust side applies the same three rules. */
const rewardsFor = (frame: number): RewardEvent[] => (frame % 40 === 0 ? [{ value: 1 }] : []);
const bootFor = (frame: number): boolean => Math.floor(frame / 120) % 2 === 0;
const learnFor = (frame: number): boolean => frame % 90 !== 0;

function scenarioAgent(): void {
  const data = syntheticConnectome();
  const frames = framePool(16, 4242);
  const agent = new NeuralAgent(data, {
    decoder: gameboyDecoderConfig(),
    frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT },
  });
  agent.warmup(frames[0]!);

  const warmup = agent.exportState();
  const masks: number[] = [];
  const steps: number[] = [];
  const spikes: number[] = [];
  const dumps: unknown[] = [];
  let chunks: Chunks = {
    indptr: raw(data.indptr),
    targets: raw(data.targets),
    weights: raw(data.weights),
    visualIndices: raw(data.visualIndices),
    visualHemisphere: raw(data.visualHemisphere),
    visualXY: raw(data.visualXY),
  };

  for (let frame = 1; frame <= 600; frame++) {
    const image = frames[frame % frames.length]!;
    const result = agent.tick(image, { rewards: rewardsFor(frame), boot: bootFor(frame), learn: learnFor(frame) });
    masks.push(toButtonMask(result.active));
    steps.push(result.steps);
    spikes.push(result.spikes);
    if (frame === 200 || frame === 400) {
      const state = agent.exportState();
      dumps.push({
        atFrame: frame,
        remainder: state.remainder,
        network: networkScalars(state.network),
        decoder: state.decoder,
        digests: networkDigests(state.network),
      });
    }
  }

  const final = agent.exportState();
  chunks = { ...chunks, ...networkChunks(final.network, 'Z') };
  dumps.push({
    atFrame: 600,
    suffix: 'Z',
    remainder: final.remainder,
    network: networkScalars(final.network),
    decoder: final.decoder,
    digests: networkDigests(final.network),
  });

  write(
    'agent',
    {
      scenario: 'agent',
      meta: {
        schemaVersion: data.meta.schemaVersion,
        dataset: data.meta.dataset,
        neurons: data.meta.neurons,
        edges: data.meta.edges,
        roles: Object.fromEntries(Object.entries(data.meta.roles).map(([name, list]) => [name, [...list]])),
        visual: { ...data.meta.visual },
      },
      frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT, count: frames.length, seed: 4242, digest: poolDigest(frames) },
      script: {
        frames: 600,
        rewardEveryFrames: 40,
        rewardValue: 1,
        bootRule: 'floor(frame / 120) % 2 === 0',
        learnRule: 'frame % 90 !== 0',
        warmupMs: agent.warmupMs,
        msPerFrame: agent.msPerFrame,
        dumpAtFrames: [200, 400, 600],
      },
      kernelVersion: agent.network.version,
      plasticityVersion: agent.plasticity.version,
      compatibility: agent.compatibility(),
      roleNames: [...agent.network.roleNames],
      plasticEdgeCount: agent.plasticity.edges.length,
      plasticEdgesDigest: sha256(raw(agent.plasticity.edges)),
      topology: final.network.plasticity.topology,
      warmup: { network: networkScalars(warmup.network), decoder: warmup.decoder, digests: networkDigests(warmup.network) },
      masks,
      steps,
      spikes,
      learning: agent.plasticity.statistics(),
      dumps,
    },
    chunks,
  );
}

// --- Scenario: real -----------------------------------------------------------------------------

/**
 * `data/fafb-v783` for 200 ms. 139,255 neurons make every array too large to commit, so this
 * scenario carries digests, the per-millisecond spike counts, and the plastic-edge selection.
 */
async function scenarioReal(): Promise<void> {
  const dir = join(REPO_ROOT, 'data', 'fafb-v783');
  const data = await loadBrainDatasetFromDir(dir);
  const network = new LifNetwork(data);
  const frames = framePool(1, 99);
  const frame = frames[0]!;

  network.setVisualFrame(frame, FRAME_WIDTH, FRAME_HEIGHT);
  const spikes: number[] = [];
  for (let ms = 0; ms < 100; ms++) spikes.push(network.step(1));
  network.stimulate(120);
  for (let ms = 0; ms < 100; ms++) spikes.push(network.step(1));
  network.plasticity.reinforce(1, network.ms);

  const state = network.exportState();
  // A few hundred sampled values, so a digest mismatch can be localized without the full arrays.
  const stride = Math.floor(data.meta.neurons / 256);
  const sampled: Record<string, number[]> = {
    membraneIndices: [],
    membrane: [],
    lastSpikeMs: [],
    refractory: [],
  };
  for (let index = 0; index < data.meta.neurons; index += stride) {
    sampled.membraneIndices!.push(index);
    sampled.membrane!.push(state.membrane[index]!);
    sampled.lastSpikeMs!.push(state.lastSpikeMs[index]!);
    sampled.refractory!.push(state.refractory[index]!);
  }
  const slotStride = Math.floor(network.plasticity.edges.length / 256);
  const sampledSlots: Record<string, number[]> = { slots: [], gains: [], traces: [], touched: [] };
  for (let slot = 0; slot < network.plasticity.edges.length; slot += slotStride) {
    sampledSlots.slots!.push(slot);
    sampledSlots.gains!.push(state.plasticity.gains[slot]!);
    sampledSlots.traces!.push(state.plasticity.traces[slot]!);
    sampledSlots.touched!.push(state.plasticity.touched[slot]!);
  }

  write(
    'real',
    {
      scenario: 'real',
      datasetDir: 'data/fafb-v783',
      fingerprint: data.fingerprint,
      metaDigest: sha256(new TextEncoder().encode(JSON.stringify(data.meta))),
      meta: {
        schemaVersion: data.meta.schemaVersion,
        dataset: data.meta.dataset,
        neurons: data.meta.neurons,
        edges: data.meta.edges,
        roleSizes: Object.fromEntries(Object.entries(data.meta.roles).map(([name, list]) => [name, list.length])),
        roleOrder: Object.keys(data.meta.roles),
        visual: { ...data.meta.visual },
      },
      frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT, seed: 99, digest: poolDigest(frames) },
      script: { stepMs: [100, 100], stimulateAfterMs: 100, stimulationMs: 120, reinforce: 1 },
      kernelVersion: network.version,
      plasticityVersion: network.plasticity.version,
      defaultKernelVersion: kernelVersion(),
      defaultPlasticityVersion: plasticityVersion(),
      roleNames: [...network.roleNames],
      baselineDigest: sha256(raw(network.baseline)),
      plasticEdgeCount: network.plasticity.edges.length,
      plasticEdgesDigest: sha256(raw(network.plasticity.edges)),
      topology: state.plasticity.topology,
      spikesPerMs: spikes,
      totalSpikes: spikes.reduce((sum, value) => sum + value, 0),
      network: networkScalars(state),
      digests: networkDigests(state),
      sampled,
      sampledSlots,
      learning: network.plasticity.statistics(),
    },
    {},
  );
}

// --- Scenario: platformer ------------------------------------------------------------------------

/**
 * The platformer decoder preset over a seeded rate sequence.
 *
 * The sequence is built to land *on* the preset's edges rather than near them, because that is what
 * distinguishes 250 ms from 249 ms: the clock advances by increments drawn from the preset's own
 * timings (55, 200, 250, 300, 420, 600 ms and a few small values), and each role's rate is drawn
 * from a ladder whose scores against the flat baseline of 5 are exactly 1, 1.05, 1.1, 2 and well
 * above — the two pulse thresholds and the system threshold. `boot` alternates every 500 steps so
 * both Start/Select variants are exercised, and `clearHolds` runs every 1,000 steps so the 300 ms
 * lockout is in the comparison too.
 *
 * Rates and timestamps travel as chunks: the Rust twin decodes the same input without porting the
 * generator, so a mask difference can only be the decoder or the preset.
 */
function scenarioPlatformer(): void {
  const config = platformerDecoderConfig();
  const roles = Array.from({ length: 8 }, (_, index) => `command_${index}`);
  const baselineRate = 5;
  const baseline = Object.fromEntries(roles.map((role) => [role, baselineRate]));
  // Scores are (rate + 1) / (baseline + 1), so these are exactly 1, 1.05, 1.1, 1.1667, 1.25, 2,
  // 2.1667, 5.1667 and 10.1667.
  const ladder = [5, 5.3, 5.6, 6, 6.5, 11, 12, 30, 60];
  const steps = 4000;
  const clearEvery = 1000;
  const bootEvery = 500;
  const increments = [1, 5, 25, 55, 100, 200, 250, 300, 420, 600];

  const random = xorshift(20260915);
  const decoder = new PopulationDecoder(config);
  decoder.calibrate(baseline);

  const rateValues = new Float64Array(steps * roles.length);
  const clock = new Float64Array(steps);
  const bootFlags = new Uint8Array(steps);
  const clearFlags = new Uint8Array(steps);
  const masks: number[] = [];
  let nowMs = 0;

  for (let step = 0; step < steps; step += 1) {
    nowMs += increments[Math.floor(random() * increments.length)]!;
    if (step > 0 && step % clearEvery === 0) {
      decoder.clearHolds(nowMs);
      clearFlags[step] = 1;
    }
    const boot = Math.floor(step / bootEvery) % 2 === 0;
    const rates: Record<string, number> = {};
    for (const [index, role] of roles.entries()) {
      const rate = ladder[Math.floor(random() * ladder.length)]!;
      rates[role] = rate;
      rateValues[step * roles.length + index] = rate;
    }
    clock[step] = nowMs;
    bootFlags[step] = boot ? 1 : 0;
    masks.push(toButtonMask(decoder.decode(rates, nowMs, boot)));
  }

  write(
    'platformer',
    {
      scenario: 'platformer',
      preset: 'platformer',
      config,
      script: {
        steps,
        seed: 20260915,
        roles,
        baselineRate,
        ladder,
        increments,
        clearEvery,
        bootEvery,
      },
      channelNames: [...decoder.channelNames],
      masks,
      final: decoder.exportState(),
    },
    { rates: raw(rateValues), clock: raw(clock), boot: bootFlags.slice(), clear: clearFlags.slice() },
  );
}

/**
 * A restore from a checkpoint that predates a channel group: the calibration rule for it.
 *
 * `docs/readout.md`, "Channels added after a checkpoint was written". The state is exported from a
 * decoder built on the plain Game Boy preset — no macro group, so no baseline for any macro role —
 * and imported into one built on the preset *with* the group. The first decode after that restore
 * calibrates the missing roles from its own rates, so every macro channel scores exactly 1.0 on
 * that decision instead of competing on its raw rate.
 *
 * The rates are the live ones of 2026-09-17 (27 to 145 Hz across the macro roles, `macro_talk` the
 * quiet one at 41) followed by a ladder that lifts one macro role at a time above the rest the
 * restore measured, so the golden pins both halves: the tie at 1.0, and the group deciding on merit
 * afterwards. Scores travel for every step, because a score is what the rule changes.
 */
function scenarioRestore(): void {
  const macroRoles = ['macro_talk', 'macro_frontier', 'macro_objective', 'macro_npc', 'macro_next'];
  const plain = gameboyDecoderConfig();
  const withMacros = gameboyDecoderConfig(macroRoles);
  const directionRoles = Object.values(plain.exclusive!.channels);
  const pulseRoles = (plain.pulses ?? []).map((pulse) => pulse.role);
  const buttonRoles = [...new Set([...directionRoles, ...pulseRoles])];
  const baselineRate = 12;
  const baseline: Record<string, number> = Object.fromEntries(
    buttonRoles.map((role) => [role, baselineRate]),
  );

  // The checkpoint: calibrated and decoded once, on the preset that has no macro group.
  const before = new PopulationDecoder(plain);
  before.calibrate(baseline);
  before.decode(baseline, 0, false);
  const checkpoint = before.exportState();

  const after = new PopulationDecoder(withMacros);
  // Calibrated on the macro preset first, so the test is the *restore* overwriting it rather than a
  // decoder that was never calibrated at all: the live box had both.
  after.calibrate({ ...baseline, ...Object.fromEntries(macroRoles.map((role) => [role, 5])) });
  after.importState(checkpoint);
  const pending = [...after.pendingBaselineRoles];

  // Step 0 is the live reading; the rest lift one macro role at a time to twice what step 0
  // measured for it, in role order, then drop everything back to the live reading.
  const live: Record<string, number> = {
    ...baseline,
    macro_talk: 41,
    macro_frontier: 54,
    macro_objective: 65,
    macro_npc: 107,
    macro_next: 145,
  };
  const steps = 1 + macroRoles.length * 2;
  const roles = [...buttonRoles, ...macroRoles];
  const rateValues = new Float64Array(steps * roles.length);
  const clock = new Float64Array(steps);
  const winners: (string | null)[] = [];
  const macroWinners: (string | null)[] = [];
  const scoreValues = new Float64Array(steps * after.channelNames.length);
  let nowMs = 0;

  for (let step = 0; step < steps; step += 1) {
    const rates: Record<string, number> = { ...live };
    if (step > 0) {
      const role = macroRoles[Math.floor((step - 1) / 2)]!;
      if ((step - 1) % 2 === 0) rates[role] = live[role]! * 2;
    }
    for (const [index, role] of roles.entries()) {
      rateValues[step * roles.length + index] = rates[role]!;
    }
    clock[step] = nowMs;
    after.decode(rates, nowMs, false);
    const scores = after.lastScores;
    for (const [index, channel] of after.channelNames.entries()) {
      scoreValues[step * after.channelNames.length + index] = scores[channel]!;
    }
    winners.push(after.exportState().current ?? null);
    macroWinners.push(after.macroWinner);
    nowMs += 1000;
  }

  write(
    'restore',
    {
      scenario: 'restore',
      preset: 'gameboy',
      config: withMacros,
      script: { steps, macroRoles, buttonRoles, baselineRate, live, roles },
      checkpoint,
      pending,
      channelNames: [...after.channelNames],
      baselines: after.baselines,
      winners,
      macroWinners,
      final: after.exportState(),
    },
    { rates: raw(rateValues), clock: raw(clock), scores: raw(scoreValues) },
  );
}

// --- Non-default configurations ------------------------------------------------------------------

/**
 * Version strings for configurations that are not default, so the FNV-1a-32 hash, the parameter
 * order and the JavaScript number formatting inside it are all pinned.
 */
function scenarioVersions(): void {
  const lif = [
    { label: 'default', patch: {} },
    { label: 'decayMs25', patch: { decayMs: 25 } },
    { label: 'threshold0_9', patch: { threshold: 0.9 } },
    { label: 'refractory3', patch: { refractoryMs: 3 } },
    { label: 'synapseScale0_006', patch: { synapseScale: 0.006 } },
    { label: 'baselineMax0_05', patch: { baselineMax: 0.05 } },
    { label: 'noiseKicks200', patch: { noiseKicks: 200 } },
    { label: 'noiseAmount0_5', patch: { noiseAmount: 0.5 } },
    { label: 'rateAlpha1over50', patch: { rateAlpha: 1 / 50 } },
    { label: 'membraneFloorMinus3', patch: { membraneFloor: -3 } },
    { label: 'seed1', patch: { seed: 1 } },
    { label: 'stimulationDrive0_3', patch: { stimulation: { role: 'reward_pam', drive: 0.3 } } },
    { label: 'retinaGain0_3', patch: { retina: { gain: 0.3, width: 160, height: 144 } } },
    { label: 'retinaWidth320', patch: { retina: { gain: 0.20, width: 320, height: 144 } } },
    { label: 'retinaHeight288', patch: { retina: { gain: 0.20, width: 160, height: 288 } } },
    { label: 'roleNamesOnly', patch: { stimulation: { role: 'other', drive: 0.20 }, rateRoles: ['command_0'] } },
  ];
  const plasticity = [
    { label: 'default', patch: {} },
    { label: 'traceMs4000', patch: { traceMs: 4000 } },
    { label: 'pairMs30', patch: { pairMs: 30 } },
    { label: 'pairWindow50', patch: { pairWindowMs: 50 } },
    { label: 'potentiation0_2', patch: { potentiation: 0.2 } },
    { label: 'depression0_1', patch: { depression: 0.1 } },
    { label: 'learningRate0_004', patch: { learningRate: 0.004 } },
    { label: 'restoring0_0002', patch: { restoring: 0.0002 } },
    { label: 'minGain0_8', patch: { minGain: 0.8 } },
    { label: 'maxGain1_2', patch: { maxGain: 1.2 } },
    { label: 'siteAndBudgetOnly', patch: { preRole: 'mbon', postRole: 'motor', budget: 4 } },
  ];

  write(
    'versions',
    {
      scenario: 'versions',
      lif: lif.map(({ label, patch }) => ({ label, version: kernelVersion(patch) })),
      plasticity: plasticity.map(({ label, patch }) => ({ label, version: plasticityVersion(patch) })),
    },
    {},
  );
}

// --- Entry point ---------------------------------------------------------------------------------

async function main(): Promise<void> {
  mkdirSync(GOLDEN_DIR, { recursive: true });
  // No arguments regenerates everything; naming scenarios regenerates only those, so one preset's
  // golden file can be rewritten without touching the others.
  const only = new Set(process.argv.slice(2));
  const wanted = (name: string): boolean => only.size === 0 || only.has(name);
  const unknown = [...only].filter((name) => !SCENARIOS.includes(name));
  if (unknown.length > 0) throw new Error(`unknown scenario(s): ${unknown.join(', ')}; known: ${SCENARIOS.join(', ')}`);

  if (wanted('math')) scenarioMath();
  if (wanted('versions')) scenarioVersions();
  if (wanted('toy')) scenarioToy();
  if (wanted('agent')) scenarioAgent();
  if (wanted('platformer')) scenarioPlatformer();
  if (wanted('restore')) scenarioRestore();
  if (wanted('real')) await scenarioReal();
}

const SCENARIOS = ['math', 'versions', 'toy', 'agent', 'platformer', 'restore', 'real'];

await main();
