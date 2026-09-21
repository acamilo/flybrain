/**
 * Run the agent loop on the real connectome, off a synthetic environment.
 *
 * ```sh
 * npx tsx examples/node-random-frames.ts [frames]      # from packages/brain
 * ```
 *
 * There is no emulator here: the "environment" is a deterministic noise frame that shifts a little
 * every frame, plus a reward every 40th frame. That is enough to see the whole loop work and to
 * measure how many frames per second the network sustains on this machine — a Game Boy needs
 * 59.73, so anything above that runs in real time.
 */
import { loadBrainDatasetFromDir } from '../src/dataset/load-node';
import { GAMEBOY_MS_PER_FRAME, NeuralAgent } from '../src/agent/agent';
import { gameboyDecoderConfig } from '../src/readout/presets/gameboy';

const FRAME_WIDTH = 160;
const FRAME_HEIGHT = 144;
const REWARD_EVERY = 40;
const DATA_DIR = new URL('../../../data/fafb-v783', import.meta.url).pathname;

/** Deterministic xorshift32, so two runs of this example print the same numbers. */
function xorshift(seed: number): () => number {
  let state = seed | 0 || 1;
  return () => {
    state ^= state << 13; state ^= state >>> 17; state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

/** A drifting noise frame: fresh gradient offset per frame, cheap enough not to skew the timing. */
function noiseFrame(random: () => number, frame: number): Uint8Array {
  const rgba = new Uint8Array(FRAME_WIDTH * FRAME_HEIGHT * 4);
  const phase = frame * 7;
  for (let y = 0; y < FRAME_HEIGHT; y++) {
    for (let x = 0; x < FRAME_WIDTH; x++) {
      const offset = (y * FRAME_WIDTH + x) * 4;
      const value = (x + y + phase + Math.floor(random() * 64)) & 0xff;
      rgba[offset] = value;
      rgba[offset + 1] = 255 - value;
      rgba[offset + 2] = (value * 3) & 0xff;
      rgba[offset + 3] = 255;
    }
  }
  return rgba;
}

function pad(text: string, width: number): string {
  return text.length >= width ? text : ' '.repeat(width - text.length) + text;
}

async function main(): Promise<void> {
  const frames = Number(process.argv[2] ?? 120);
  if (!Number.isInteger(frames) || frames <= 0) throw new Error(`Frame count must be a positive integer, got ${process.argv[2]}`);

  const loadStarted = performance.now();
  const dataset = await loadBrainDatasetFromDir(DATA_DIR);
  const loadMs = performance.now() - loadStarted;
  console.log(`${dataset.meta.dataset}: ${dataset.meta.neurons} neurons, ${dataset.meta.edges} edges, ${dataset.meta.visual.count} retina columns (loaded in ${loadMs.toFixed(0)} ms)`);

  const random = xorshift(20260915);
  const agent = new NeuralAgent(dataset, {
    decoder: gameboyDecoderConfig(),
    frame: { width: FRAME_WIDTH, height: FRAME_HEIGHT },
  });
  console.log(`compatibility: ${agent.compatibility()}`);
  console.log(`plastic synapses: ${agent.plasticity.statistics().synapses}, ${GAMEBOY_MS_PER_FRAME.toFixed(4)} ms per frame`);

  const warmupStarted = performance.now();
  agent.warmup(noiseFrame(random, 0));
  console.log(`warm-up: ${agent.warmupMs} ms of network time in ${(performance.now() - warmupStarted).toFixed(0)} ms wall clock\n`);

  const columns = ['frame', 'ms', 'pop rate', 'active', 'updates', 'changed', 'mean |dg|', 'signal'];
  console.log(columns.map((name, index) => pad(name, [6, 8, 9, 18, 8, 8, 10, 7][index]!)).join(' '));

  const clockStarted = agent.network.ms;
  const tickStarted = performance.now();
  for (let frame = 1; frame <= frames; frame++) {
    const rewards = frame % REWARD_EVERY === 0 ? [{ value: 1 }] : [];
    const result = agent.tick(noiseFrame(random, frame), { rewards, boot: frame <= frames / 2 });
    if (frame % 10 === 0 || frame === frames) {
      const { learning, ms, populationRate } = agent.snapshot();
      console.log([
        pad(String(frame), 6),
        pad(ms.toFixed(0), 8),
        pad(populationRate.toFixed(3), 9),
        pad(result.active.join(',') || '-', 18),
        pad(String(learning.updates), 8),
        pad(String(learning.changed), 8),
        pad(learning.meanChange.toExponential(2), 10),
        pad(learning.signal.toFixed(3), 7),
      ].join(' '));
    }
  }
  const elapsed = performance.now() - tickStarted;
  console.log(`\n${frames} frames in ${elapsed.toFixed(0)} ms = ${(frames * 1000 / elapsed).toFixed(1)} frames/sec (Game Boy needs ${(1000 / GAMEBOY_MS_PER_FRAME).toFixed(2)})`);
  console.log(`${((agent.network.ms - clockStarted) / elapsed).toFixed(2)}x real time; network clock at ${agent.network.ms} ms`);
}

await main();
