/**
 * The spike accumulator against a brute-force implementation.
 *
 * `accumulateBitset` is a bit-twiddling hot loop that skips whole zero bytes, which is the only
 * reason the brain map fits its frame budget — and exactly the kind of code that is wrong by one
 * bit at a boundary. So it is checked against the obvious slow version over random bitsets,
 * including at the ragged end where the neuron count is not a multiple of eight.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CLASS_INTERNAL,
  CLASS_OUTPUT,
  CLASS_SENSORY,
  OUT_OF_BOUNDS,
  accumulateBitset,
  buildCellClasses,
  buildGridLut,
  decayAccumulator,
  topCells,
} from '../../src/paint/accumulator';
import { fitHalfExtent } from '../../src/lib/fit';

/** The obvious implementation: test every bit, one at a time. */
function bruteForce(bitset: Uint8Array, lut: Uint32Array, cells: number, neuronCount: number): Float32Array {
  const accum = new Float32Array(cells);
  for (let neuron = 0; neuron < neuronCount; neuron++) {
    const byte = bitset[neuron >> 3] ?? 0;
    if ((byte & (1 << (neuron & 7))) !== 0) {
      accum[lut[neuron] as number] += 1;
    }
  }
  return accum;
}

/** Deterministic PRNG so a failure is reproducible. */
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

function randomLut(neuronCount: number, cells: number, random: () => number): Uint32Array {
  const lut = new Uint32Array(neuronCount);
  for (let i = 0; i < neuronCount; i++) lut[i] = Math.floor(random() * cells);
  return lut;
}

test('matches brute force over random bitsets, including ragged neuron counts', () => {
  const random = mulberry32(1234);
  const cells = 97;

  for (const neuronCount of [1, 7, 8, 9, 63, 64, 65, 139_255]) {
    const lut = randomLut(neuronCount, cells, random);
    const bitset = new Uint8Array(Math.ceil(neuronCount / 8));

    for (let density = 0; density < 3; density++) {
      const probability = [0.001, 0.05, 0.5][density] as number;
      bitset.fill(0);
      let expectedSpikes = 0;
      for (let neuron = 0; neuron < neuronCount; neuron++) {
        if (random() < probability) {
          bitset[neuron >> 3] = (bitset[neuron >> 3] as number) | (1 << (neuron & 7));
          expectedSpikes += 1;
        }
      }

      const accum = new Float32Array(cells);
      const spikes = accumulateBitset(bitset, lut, accum, neuronCount);
      assert.equal(spikes, expectedSpikes, `spike count for n=${neuronCount}, p=${probability}`);
      assert.deepEqual(
        Array.from(accum),
        Array.from(bruteForce(bitset, lut, cells, neuronCount)),
        `cell histogram for n=${neuronCount}, p=${probability}`,
      );
    }
  }
});

test('bits past the neuron count are ignored even when the last byte is full', () => {
  // 10 neurons in 2 bytes: bits 10..15 exist in the bitset but are not neurons.
  const neuronCount = 10;
  const lut = new Uint32Array(neuronCount);
  const accum = new Float32Array(1);
  const bitset = new Uint8Array([0xff, 0xff]);

  assert.equal(accumulateBitset(bitset, lut, accum, neuronCount), neuronCount);
  assert.equal(accum[0], neuronCount);
});

test('accumulation is additive across snapshots', () => {
  const lut = new Uint32Array([0, 0, 1]);
  const accum = new Float32Array(2);
  const bitset = new Uint8Array([0b0000_0011]);

  accumulateBitset(bitset, lut, accum, 3);
  accumulateBitset(bitset, lut, accum, 3);
  assert.deepEqual(Array.from(accum), [4, 0]);
});

test('a short bitset is tolerated rather than read out of bounds', () => {
  const lut = new Uint32Array(64);
  const accum = new Float32Array(1);
  // Claims 64 neurons, carries one byte.
  assert.equal(accumulateBitset(new Uint8Array([0xff]), lut, accum, 64), 8);
});

test('decay is exponential with the stated time constant and snaps to zero', () => {
  const accum = new Float32Array([100, 1, 0]);
  decayAccumulator(accum, 110, 110);
  assert.ok(Math.abs((accum[0] as number) - 100 / Math.E) < 1e-4, 'one tau is one e-fold');
  assert.equal(accum[2], 0);

  // Ten time constants later nothing is left holding a denormal.
  for (let i = 0; i < 10; i++) decayAccumulator(accum, 110, 110);
  assert.equal(accum[0], 0);
  assert.equal(accum[1], 0);

  const unchanged = new Float32Array([5]);
  decayAccumulator(unchanged, 0, 110);
  assert.equal(unchanged[0], 5, 'a zero dt changes nothing');
});

test('topCells picks cells over the threshold and respects the hard cap', () => {
  const accum = new Float32Array([0, 5, 2, 9, 1, 4]);
  const out = new Uint32Array(10);

  assert.equal(topCells(accum, 4, out), 3);
  assert.deepEqual(Array.from(out.subarray(0, 3)), [1, 3, 5]);

  const capped = new Uint32Array(2);
  assert.equal(topCells(accum, 1, capped), 2, 'the cap bounds the sprite pass');

  assert.equal(topCells(accum, 100, out), 0);
});

/**
 * The cap has to bind on *brightness*, not on cell index.
 *
 * Scan order and brightness order are deliberately opposed here: the two dimmest qualifying cells
 * come first. Taking the first two — what the pass used to do once the cap bound, which on air
 * meant taking the top rows of the map and nothing else — would pick cells 1 and 2.
 */
test('a capped topCells takes the brightest cells, not the first ones it walks past', () => {
  const accum = new Float32Array([0, 4, 5, 0, 40, 0, 30]);
  const capped = new Uint32Array(2);

  assert.equal(topCells(accum, 3, capped), 2);
  assert.deepEqual(Array.from(capped).sort((a, b) => a - b), [4, 6], 'the 40 and the 30');
});

/**
 * A cap that lands inside one brightness bucket still has to spend the whole budget, and spread
 * it: every one of these cells is equally bright, so the only wrong answer is a contiguous run.
 */
test('a cap inside one brightness bucket fills the budget from across the grid', () => {
  const accum = new Float32Array(4_000).fill(5);
  const out = new Uint32Array(64);

  assert.equal(topCells(accum, 3, out), 64, 'the budget is spent in full');
  const picked = Array.from(out).sort((a, b) => a - b);
  assert.equal(new Set(picked).size, 64, 'no cell is picked twice');
  assert.ok(
    (picked[63] as number) - (picked[0] as number) > accum.length / 2,
    `the picks span the grid, not a run: ${picked[0]}..${picked[63]}`,
  );
});

test('the grid LUT maps the corners of the projection to the corners of the grid', () => {
  // Four neurons at the extremes of a ±1 box, z ignored.
  const positions = new Float32Array([-1, 1, 0, 1, 1, 0, -1, -1, 0, 1, -1, 0]);
  const lut = buildGridLut(positions, 4, 4, 1, 1);

  assert.equal(lut[0], 0, 'top left');
  assert.equal(lut[1], 3, 'top right');
  assert.equal(lut[2], 12, 'bottom left');
  assert.equal(lut[3], 15, 'bottom right');
});

test('the grid LUT drops a neuron outside the extent rather than clamping it to an edge cell', () => {
  // Both points are far outside the ±1 fitted box: clamping would have wrapped them onto the
  // top-left and bottom-right corner cells (the old behaviour, asserted the other way before this
  // fix), piling density onto an edge that never actually held any of these neurons.
  const positions = new Float32Array([-9, 9, 0, 9, -9, 0]);
  const lut = buildGridLut(positions, 4, 4, 1, 1);
  assert.equal(lut[0], OUT_OF_BOUNDS);
  assert.equal(lut[1], OUT_OF_BOUNDS);
});

test('accumulateBitset and buildCellClasses skip a dropped (out-of-bounds) neuron', () => {
  // Neuron 0 is in bounds (cell 0), neuron 1 is dropped.
  const lut = new Uint32Array([0, OUT_OF_BOUNDS]);
  const accum = new Float32Array(1);
  const bitset = new Uint8Array([0b0000_0011]);

  const spikes = accumulateBitset(bitset, lut, accum, 2);
  assert.equal(spikes, 2, 'both spikes are still counted for the header comparison');
  assert.equal(accum[0], 1, 'only the in-bounds neuron reached the grid');

  const classes = new Uint8Array([CLASS_OUTPUT, CLASS_SENSORY]);
  const cells = buildCellClasses(lut, classes, 1);
  assert.equal(cells[0], CLASS_OUTPUT, 'the dropped neuron never touches a real cell');
});

/**
 * The class of bug this whole fix removes: a wide, letterboxed target (like the connectome
 * pane, 252x185 accumulator cells over a 1008x370 canvas) fit from a *square* point cloud, the
 * way `normalizePositions` produces one. `fitHalfExtent` is exactly what `brain-base.worker.ts`
 * uses to turn that square into the letterboxed extent the LUT is built against, so this test
 * exercises the same path the worker does, at the same aspect ratio.
 */
test('a square point cloud fit to a wide grid stays letterboxed, not piled onto an edge', () => {
  const width = 1008;
  const height = 370;
  const gridWidth = 252;
  const gridHeight = 185;

  const random = mulberry32(42);
  const count = 50_000;
  const positions = new Float32Array(count * 3);
  let maxAbsX = 0;
  let maxAbsY = 0;
  for (let i = 0; i < count; i++) {
    const x = (random() * 2 - 1) * 1.3;
    const y = (random() * 2 - 1) * 1.3;
    positions[i * 3] = x;
    positions[i * 3 + 1] = y;
    maxAbsX = Math.max(maxAbsX, Math.abs(x));
    maxAbsY = Math.max(maxAbsY, Math.abs(y));
  }

  const halfExtent = fitHalfExtent(maxAbsX, maxAbsY, width, height);
  const lut = buildGridLut(positions, gridWidth, gridHeight, halfExtent.x, halfExtent.y);

  const density = new Float64Array(gridWidth * gridHeight);
  let minPx = Infinity;
  let maxPx = -Infinity;
  let minPy = Infinity;
  let maxPy = -Infinity;
  let dropped = 0;
  for (let i = 0; i < count; i++) {
    const cell = lut[i] as number;
    if (cell === OUT_OF_BOUNDS) {
      dropped += 1;
      continue;
    }
    density[cell] = (density[cell] as number) + 1;
    const px = cell % gridWidth;
    const py = Math.floor(cell / gridWidth);
    minPx = Math.min(minPx, px);
    maxPx = Math.max(maxPx, px);
    minPy = Math.min(minPy, py);
    maxPy = Math.max(maxPy, py);
  }
  assert.equal(dropped, 0, 'a fit derived from these points’ own extent drops none of them');

  const rowSums = new Float64Array(gridHeight);
  for (let py = 0; py < gridHeight; py++) {
    let sum = 0;
    for (let px = 0; px < gridWidth; px++) sum += density[py * gridWidth + px] as number;
    rowSums[py] = sum;
  }
  const meanRow = count / gridHeight;
  const topRow = rowSums[0] as number;
  const bottomRow = rowSums[gridHeight - 1] as number;
  assert.ok(
    topRow <= meanRow * 3,
    `top row density ${topRow} piled up past 3x the mean ${meanRow} — a clamp, not a fit`,
  );
  assert.ok(
    bottomRow <= meanRow * 3,
    `bottom row density ${bottomRow} piled up past 3x the mean ${meanRow} — a clamp, not a fit`,
  );

  // The mapped points' own bounding box, in cell-aspect-corrected pixels (cells are 4x2, not
  // square), should read back as the same 1:1 aspect ratio the input square had — proof the fit
  // used one uniform scale rather than stretching each axis independently to fill the grid.
  const cellWidth = width / gridWidth;
  const cellHeight = height / gridHeight;
  const boxWidth = (maxPx - minPx + 1) * cellWidth;
  const boxHeight = (maxPy - minPy + 1) * cellHeight;
  const aspect = boxWidth / boxHeight;
  assert.ok(Math.abs(aspect - 1) < 0.1, `mapped bounding box aspect ${aspect} should read back as ~1:1`);
});

test('every neuron lands in a real cell for the real dataset size', () => {
  const random = mulberry32(99);
  const neuronCount = 139_255;
  const positions = new Float32Array(neuronCount * 3);
  for (let i = 0; i < neuronCount; i++) {
    positions[i * 3] = (random() * 2 - 1) * 1.3;
    positions[i * 3 + 1] = (random() * 2 - 1) * 1.3;
  }

  const gridWidth = 304;
  const gridHeight = 228;
  const lut = buildGridLut(positions, gridWidth, gridHeight, 1.3, 1.3);
  const cells = gridWidth * gridHeight;

  // 69,312 cells is past what a Uint16Array can address, which is why the LUT is Uint32Array.
  assert.ok(cells > 65_535);
  for (let i = 0; i < neuronCount; i += 997) {
    assert.ok((lut[i] as number) < cells, `neuron ${i} mapped outside the grid`);
  }
});

test('cell classes take the highest-precedence class in the cell', () => {
  //                      neuron: 0        1        2        3
  const lut = new Uint32Array([0, 0, 1, 2]);
  const classes = new Uint8Array([CLASS_INTERNAL, CLASS_OUTPUT, CLASS_SENSORY, CLASS_INTERNAL]);
  const cells = buildCellClasses(lut, classes, 3);

  assert.equal(cells[0], CLASS_OUTPUT, 'output beats internal');
  assert.equal(cells[1], CLASS_SENSORY);
  assert.equal(cells[2], CLASS_INTERNAL);
});

test('cells with no neurons default to internal rather than to sensory', () => {
  const cells = buildCellClasses(new Uint32Array([0]), new Uint8Array([CLASS_OUTPUT]), 3);
  assert.equal(cells[1], CLASS_INTERNAL);
  assert.equal(cells[2], CLASS_INTERNAL);
});
