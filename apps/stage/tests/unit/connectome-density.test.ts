/**
 * The CONNECTOME tab's density pipeline over the *real* dataset, at the *live* spike rate.
 *
 * `accumulator.test.ts` checks each function against a brute-force implementation, and
 * `fit.test.ts` checks the fit's arithmetic. Both passed while the map on air still piled every
 * bright glow into a bar across the top of the brain, for two reasons this file exists to remove:
 *
 *   1. **Synthetic positions.** A uniform random cloud has no dense core, so nothing ever
 *      saturates. The real 139,255 FlyWire somata are two lobes and a dense central brain, and
 *      the top of those lobes is the first thing a row-major scan reaches.
 *   2. **The fixtures' spike rate.** Until 2026-09-16 the committed `.flyfeed` fixtures and the
 *      fake simulator defaulted to 2,000 spikes a snapshot, at which 25 cells clear the sprite
 *      threshold and the 256-sprite cap is never reached. The live flysim sends around 30,000, at
 *      which 9,800 cells clear it and the cap is hit on every single frame — a different code
 *      path, and the one that is on air 24 hours a day. The fixtures and the fake simulator's
 *      default now match the live rate; `FIXTURE_RATE` below stays as the low-density regression
 *      case this file was written to compare against.
 *
 * So this runs the worker's own fit (`pointCloudExtent` + `fitHalfExtent`, the same calls
 * `brain-base.worker.ts` makes) and the real accumulator over the real positions at both rates,
 * and asserts the two things a viewer would notice: nothing piles onto an edge, and the sprite
 * budget is spent on the brain rather than on its first rows.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

import { normalizePositions } from '@flybrain/brain/view/layout';

import { fitHalfExtent, pointCloudExtent } from '../../src/lib/fit';
import { MAP_GRID_HEIGHT, MAP_GRID_WIDTH, MAP_HERO_HEIGHT, MAP_HERO_WIDTH } from '../../src/lib/geometry';
import { OUT_OF_BOUNDS, accumulateBitset, buildGridLut, decayAccumulator, topCells } from '../../src/paint/accumulator';
import { DECAY_TAU_MS, MAX_SPRITES, SPRITE_THRESHOLD } from '../../src/paint/brainmap';

const here = dirname(fileURLToPath(import.meta.url));
const POSITIONS = resolve(here, '../../../../data/fafb-v783/positions.binz');

/** Snapshot cadence the stage runs the map at. */
const FRAME_MS = 33;
/** Frames to settle the accumulator into its steady state (about 9 time constants). */
const SETTLE_FRAMES = 90;

/** Spikes per snapshot: the pre-2026-09-16 fixture default, and what the live simulator sends. */
const FIXTURE_RATE = 2_000;
const LIVE_RATE = 30_000;

/** Deterministic PRNG, so a failure is reproducible. Same generator as `accumulator.test.ts`. */
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The real somata positions, normalized exactly as the worker normalizes them. */
function loadPositions(): Float32Array {
  const raw = gunzipSync(readFileSync(POSITIONS));
  const source = new Float32Array(raw.buffer, raw.byteOffset, raw.byteLength / 4);
  return normalizePositions(source);
}

/** The worker's LUT for the real dataset at the real pane size. */
function realLut(): { lut: Uint32Array; neuronCount: number; outOfBounds: number } {
  const positions = loadPositions();
  const neuronCount = Math.floor(positions.length / 3);
  const { maxAbsX, maxAbsY } = pointCloudExtent(positions);
  const halfExtent = fitHalfExtent(maxAbsX, maxAbsY, MAP_HERO_WIDTH, MAP_HERO_HEIGHT);
  const lut = buildGridLut(positions, MAP_GRID_WIDTH, MAP_GRID_HEIGHT, halfExtent.x, halfExtent.y);
  let outOfBounds = 0;
  for (let i = 0; i < lut.length; i++) if (lut[i] === OUT_OF_BOUNDS) outOfBounds += 1;
  return { lut, neuronCount, outOfBounds };
}

/** Run the accumulator to steady state at `spikesPerSnapshot`, and hand back the grid. */
function settle(lut: Uint32Array, neuronCount: number, spikesPerSnapshot: number, seed: number): Float32Array {
  const random = mulberry32(seed);
  const accum = new Float32Array(MAP_GRID_WIDTH * MAP_GRID_HEIGHT);
  const bitset = new Uint8Array(Math.ceil(neuronCount / 8));
  const probability = spikesPerSnapshot / neuronCount;

  for (let frame = 0; frame < SETTLE_FRAMES; frame++) {
    bitset.fill(0);
    for (let neuron = 0; neuron < neuronCount; neuron++) {
      if (random() < probability) bitset[neuron >> 3] = (bitset[neuron >> 3] as number) | (1 << (neuron & 7));
    }
    accumulateBitset(bitset, lut, accum, neuronCount);
    decayAccumulator(accum, FRAME_MS, DECAY_TAU_MS);
  }
  return accum;
}

function rowSums(accum: Float32Array): Float64Array {
  const rows = new Float64Array(MAP_GRID_HEIGHT);
  for (let cell = 0; cell < accum.length; cell++) {
    rows[Math.floor(cell / MAP_GRID_WIDTH)] += accum[cell] as number;
  }
  return rows;
}

function columnSums(accum: Float32Array): Float64Array {
  const cols = new Float64Array(MAP_GRID_WIDTH);
  for (let cell = 0; cell < accum.length; cell++) {
    cols[cell % MAP_GRID_WIDTH] += accum[cell] as number;
  }
  return cols;
}

function mean(values: Float64Array): number {
  let total = 0;
  for (const value of values) total += value;
  return total / values.length;
}

test('the real dataset fits the connectome pane with nothing out of bounds', () => {
  const { lut, neuronCount, outOfBounds } = realLut();

  assert.equal(neuronCount, 139_255, 'the dataset is the 139,255-neuron FlyWire cut');
  assert.equal(lut.length, neuronCount);
  // The fit is taken from these neurons' own extent, so a single dropped neuron means the fit and
  // the positions disagree — which is the bug the OUT_OF_BOUNDS sentinel exists to make visible
  // instead of piling that neuron onto row or column zero.
  assert.equal(outOfBounds, 0, 'no neuron falls outside a fit computed from these same neurons');
});

/**
 * The edge-pileup regression, over the real positions at both spike rates.
 *
 * A clamping LUT put every out-of-extent neuron on the first or last row/column, which shows up
 * here as an edge row carrying many times the mean row's density. The fit drops them instead, so
 * the edges — which the 0.97 margin guarantees hold no neuron at all — must stay empty.
 */
for (const [label, rate] of [
  ['the fixtures’ rate', FIXTURE_RATE],
  ['the live rate', LIVE_RATE],
] as const) {
  test(`no edge row or column piles up density at ${label}`, () => {
    const { lut, neuronCount } = realLut();
    const accum = settle(lut, neuronCount, rate, 20_260_916);

    const rows = rowSums(accum);
    const cols = columnSums(accum);
    const rowLimit = 3 * mean(rows);
    const colLimit = 3 * mean(cols);

    for (const row of [0, 1, MAP_GRID_HEIGHT - 2, MAP_GRID_HEIGHT - 1]) {
      assert.ok(
        (rows[row] as number) <= rowLimit,
        `edge row ${row} holds ${rows[row]}, over 3x the mean row (${rowLimit.toFixed(1)})`,
      );
    }
    for (const col of [0, 1, MAP_GRID_WIDTH - 2, MAP_GRID_WIDTH - 1]) {
      assert.ok(
        (cols[col] as number) <= colLimit,
        `edge column ${col} holds ${cols[col]}, over 3x the mean column (${colLimit.toFixed(1)})`,
      );
    }
  });
}

/**
 * The bug the operator saw on air: "activity piled along a horizontal line near the top and in the top
 * corners", with the fit already fixed and nothing out of bounds.
 *
 * At the live rate 9,800 cells clear the sprite threshold, so the 256-sprite cap binds on every
 * frame. Selecting them in scan order means selecting them in row-major order, which put all 256
 * in grid rows 14 to 42 of 184 — a bar across the top of the brain, with the dense central brain
 * below it getting none. Two assertions: the selection covers the map's active rows, and it is
 * made of the brightest cells rather than the first ones.
 */
test('a saturated sprite pass spreads over the map and takes the brightest cells', () => {
  const { lut, neuronCount } = realLut();
  const accum = settle(lut, neuronCount, LIVE_RATE, 20_260_916);

  let over = 0;
  let firstActiveRow = MAP_GRID_HEIGHT;
  let lastActiveRow = 0;
  for (let cell = 0; cell < accum.length; cell++) {
    if ((accum[cell] as number) < SPRITE_THRESHOLD) continue;
    over += 1;
    const row = Math.floor(cell / MAP_GRID_WIDTH);
    if (row < firstActiveRow) firstActiveRow = row;
    if (row > lastActiveRow) lastActiveRow = row;
  }
  assert.ok(over > MAX_SPRITES, `the live rate must saturate the cap: ${over} cells over threshold`);

  const out = new Uint32Array(MAX_SPRITES);
  const found = topCells(accum, SPRITE_THRESHOLD, out);
  assert.equal(found, MAX_SPRITES, 'a saturated pass fills the budget');

  const picked = Array.from(out.subarray(0, found));
  const rows = picked.map((cell) => Math.floor(cell / MAP_GRID_WIDTH));
  const spread = Math.max(...rows) - Math.min(...rows);
  const active = lastActiveRow - firstActiveRow;
  assert.ok(
    spread >= active / 2,
    `sprites span grid rows ${Math.min(...rows)}..${Math.max(...rows)} (${spread}) of the ` +
      `${active} active rows ${firstActiveRow}..${lastActiveRow} — a band, not the map`,
  );

  // And they are genuinely the brightest, to within the histogram's one-spike resolution: no
  // picked cell may be more than a whole spike dimmer than the true cap-th brightest cell.
  const sorted = Array.from(accum).sort((a, b) => b - a);
  const cutoff = (sorted[MAX_SPRITES - 1] as number) - 1;
  for (const cell of picked) {
    assert.ok(
      (accum[cell] as number) >= cutoff,
      `cell ${cell} holds ${accum[cell]}, below the brightest-${MAX_SPRITES} cutoff ${cutoff}`,
    );
  }
});
