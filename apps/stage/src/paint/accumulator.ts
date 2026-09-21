/**
 * The brain map's density accumulator (design A5, tier 2).
 *
 * The naive design — one glow sprite per spiked neuron — does not survive arithmetic: at a
 * plausible 2-4 Hz mean population rate over 139,255 neurons each 33 ms snapshot carries 9,000 to
 * 18,000 spikes, and 14,000 `drawImage` calls is 30-80 ms of a 33 ms budget. So spikes are
 * accumulated into a coarse grid instead: one integer increment each, about 0.05 ms for the whole
 * bitset, and the grid is then expanded, tinted and blitted twice.
 *
 * Everything here is a pure function over typed arrays, so `tests/unit/accumulator.test.ts` can
 * check it against a brute-force implementation, which is the only way to be sure a bit-twiddling
 * hot loop is right.
 */

/** Neuron class as `classifyByRoles` labels them: 0 sensory, 1 internal, 2 output. */
export const CLASS_SENSORY = 0;
export const CLASS_INTERNAL = 1;
export const CLASS_OUTPUT = 2;

/**
 * Sentinel LUT value for a neuron that falls outside the fitted extent.
 *
 * Never a real cell index (the grid never reaches 2^32-1 cells), so `accumulateBitset` and
 * `buildCellClasses` can skip it with a plain equality check. A neuron mapping here is dropped
 * from the map entirely rather than clamped onto an edge cell — see `buildGridLut`.
 */
export const OUT_OF_BOUNDS = 0xffffffff;

/**
 * Map every neuron to a grid cell.
 *
 * `positions` is the normalized, flattened position array (`normalizePositions` output: xyz
 * triples with z = 0). `halfExtentX`/`halfExtentY` are the same fitted half-extent the base
 * raster was rasterised with (`lib/fit.ts`'s `fitHalfExtent`) — the same one scale for both axes,
 * so the LUT can never disagree with the dots it is meant to sit on top of.
 *
 * A neuron outside that box maps to `OUT_OF_BOUNDS` rather than being clamped into the nearest
 * edge cell: fitting the grid to these neurons' own extent means none should ever fall outside
 * it, and a clamp would turn the one case that isn't supposed to happen into a silent pileup of
 * glow along the first or last row or column instead of just dropping the odd point out.
 *
 * Deviation from A5, with reason: A5 specifies a `Uint16Array` LUT. The grid is 304x228 = 69,312
 * cells, which overflows a `Uint16Array`, so the LUT is a `Uint32Array` (557 KB instead of
 * 278 KB, once, in a worker).
 */
export function buildGridLut(
  positions: Float32Array,
  gridWidth: number,
  gridHeight: number,
  halfExtentX: number,
  halfExtentY: number,
): Uint32Array {
  const count = Math.floor(positions.length / 3);
  const lut = new Uint32Array(count);
  const lastX = gridWidth - 1;
  const lastY = gridHeight - 1;

  for (let i = 0; i < count; i++) {
    const nx = (positions[i * 3] as number) / halfExtentX;
    const ny = (positions[i * 3 + 1] as number) / halfExtentY;
    if (nx < -1 || nx > 1 || ny < -1 || ny > 1) {
      lut[i] = OUT_OF_BOUNDS;
      continue;
    }
    const px = Math.min(lastX, Math.round(((nx + 1) / 2) * lastX));
    // Screen y grows downward; the point cloud's y grows upward.
    const py = Math.min(lastY, Math.round(((1 - ny) / 2) * lastY));
    lut[i] = py * gridWidth + px;
  }
  return lut;
}

/**
 * Add one snapshot's spikes into `accum`.
 *
 * Skips whole zero bytes, which is most of them: a 2,000-spike snapshot over 139,255 neurons
 * leaves roughly 98 percent of the 17,407 bytes empty. Returns the number of set bits, so the
 * caller can compare it against `header.spikeCount` and notice a mismatched dataset.
 */
export function accumulateBitset(
  bitset: Uint8Array,
  lut: Uint32Array,
  accum: Float32Array,
  neuronCount: number,
): number {
  let spikes = 0;
  const bytes = Math.min(bitset.length, (neuronCount + 7) >> 3);

  for (let byteIndex = 0; byteIndex < bytes; byteIndex++) {
    const byte = bitset[byteIndex] as number;
    if (byte === 0) continue;
    const base = byteIndex << 3;
    for (let bit = 0; bit < 8; bit++) {
      if ((byte & (1 << bit)) === 0) continue;
      const neuron = base + bit;
      if (neuron >= neuronCount) break;
      const cell = lut[neuron] as number;
      if (cell !== OUT_OF_BOUNDS) accum[cell] = (accum[cell] as number) + 1;
      spikes += 1;
    }
  }
  return spikes;
}

/**
 * Exponential decay with time constant `tauMs`.
 *
 * `factor = exp(-dt / tau)`, computed once for the whole array. Values below a floor are snapped
 * to zero so the grid does not carry denormals forever.
 */
export function decayAccumulator(accum: Float32Array, dtMs: number, tauMs: number): void {
  if (dtMs <= 0) return;
  const factor = Math.exp(-dtMs / tauMs);
  for (let i = 0; i < accum.length; i++) {
    const value = (accum[i] as number) * factor;
    accum[i] = value < 0.01 ? 0 : value;
  }
}

/**
 * Histogram resolution of the sprite selection: one bucket per whole spike over the threshold,
 * with the top bucket open-ended. 64 buckets covers a cell holding `threshold + 63` spikes, which
 * is past the point where the tint saturates anyway (`brainmap.ts` normalises on `log1p(24)`).
 */
export const SPRITE_BUCKETS = 64;

/**
 * Stride of the top-up sweep, in cells.
 *
 * It only has to be large and not a divisor of the grid width, so that consecutive visits land
 * far apart on the map: 97 over a 251-wide grid advances 97 columns, wraps, and precesses across
 * the rows, so the first sweep samples the whole map rather than filling one row at a time.
 */
const TOP_UP_STRIDE = 97;

/**
 * Pick the brightest cells above `threshold`, capped at `out.length`.
 *
 * Sort-free (design A5: "sort-free selection of cells above a threshold") but deliberately *not*
 * scan-ordered, which is the bug this replaces. The obvious one-pass "collect until full" loop is
 * only correct while fewer cells than the cap clear the threshold. Above that it spends the whole
 * sprite budget on the lowest cell indices, and cell indices are row-major: at the live simulator's
 * rate — around 30,000 spikes per snapshot, the fake sim's default since 2026-09-16 (it used to
 * default to 2,000, which never exercised this path) — 9,800 of the 46,184 cells clear the
 * threshold every frame, so all 256 sprites landed in grid rows 14 to 42 and drew a bright bar
 * across the top of the brain with nothing below it. Which is what "still clamped" looked like on
 * air, though nothing was clamped: the fit is fine (`lib/fit.ts`) and no neuron is out of bounds.
 *
 * So: histogram the cells over the threshold by whole spikes, walk down from the brightest bucket
 * while its cells still fit the cap, take everything above that cutoff, and spend whatever budget
 * is left on the cutoff bucket itself in a strided sweep, so the leftovers are spread over the
 * whole map instead of over its first rows. Three O(cells) passes and a 64-entry histogram, no
 * sort, no per-cell allocation, and the selection is within one whole spike of the true brightest
 * `cap` cells.
 */
export function topCells(accum: Float32Array, threshold: number, out: Uint32Array): number {
  const cap = out.length;
  const cells = accum.length;
  if (cap === 0) return 0;

  // The histogram is 256 bytes, allocated per call rather than kept as module state so this stays
  // a pure function over its arguments — at 30 Hz that is not a cost worth a hidden scratch array.
  const histogram = new Int32Array(SPRITE_BUCKETS);
  let over = 0;
  for (let i = 0; i < cells; i++) {
    const value = accum[i] as number;
    if (value < threshold) continue;
    histogram[bucketOf(value, threshold)] += 1;
    over += 1;
  }

  // Under the cap every cell over the threshold gets a sprite, so the order it finds them in
  // cannot bias anything. This is the quiet case: an idle network, and most of a fixture.
  if (over <= cap) {
    let found = 0;
    for (let i = 0; i < cells && found < cap; i++) {
      if ((accum[i] as number) >= threshold) {
        out[found] = i;
        found += 1;
      }
    }
    return found;
  }

  // Brightest bucket down, while the cells in the buckets kept so far still fit.
  let keep = SPRITE_BUCKETS;
  let fitting = 0;
  while (keep > 0 && fitting + (histogram[keep - 1] as number) <= cap) {
    fitting += histogram[keep - 1] as number;
    keep -= 1;
  }

  let found = 0;
  for (let i = 0; i < cells && found < cap; i++) {
    const value = accum[i] as number;
    if (value >= threshold && bucketOf(value, threshold) >= keep) {
      out[found] = i;
      found += 1;
    }
  }

  // `keep - 1` is the first bucket that would have overflowed the cap. Spend the rest of the
  // budget on it in strided sweeps: every cell is still visited exactly once across the sweeps,
  // but the first sweep alone samples the whole map, so a partially-taken bucket is taken evenly.
  if (found < cap && keep > 0) {
    const target = keep - 1;
    for (let offset = 0; offset < TOP_UP_STRIDE && found < cap; offset++) {
      for (let i = offset; i < cells && found < cap; i += TOP_UP_STRIDE) {
        const value = accum[i] as number;
        if (value >= threshold && bucketOf(value, threshold) === target) {
          out[found] = i;
          found += 1;
        }
      }
    }
  }
  return found;
}

/** Bucket index for a cell value, clamped into the open-ended top bucket. */
function bucketOf(value: number, threshold: number): number {
  const index = Math.floor(value - threshold);
  if (index <= 0) return 0;
  return index >= SPRITE_BUCKETS ? SPRITE_BUCKETS - 1 : index;
}

/**
 * Per-cell dominant neuron class, for the colour ramp.
 *
 * Output neurons win over sensory, which win over internal — the same precedence
 * `classifyByRoles` uses when a neuron appears in more than one role, so the map's colours agree
 * with the point cloud's.
 */
export function buildCellClasses(lut: Uint32Array, classes: Uint8Array, cellCount: number): Uint8Array {
  const cells = new Uint8Array(cellCount).fill(CLASS_INTERNAL);
  const seen = new Uint8Array(cellCount);
  for (let neuron = 0; neuron < lut.length; neuron++) {
    const cell = lut[neuron] as number;
    if (cell === OUT_OF_BOUNDS) continue;
    const cls = classes[neuron] as number;
    if (seen[cell] === 0) {
      cells[cell] = cls;
      seen[cell] = 1;
      continue;
    }
    const current = cells[cell] as number;
    if (rank(cls) > rank(current)) cells[cell] = cls;
  }
  return cells;
}

function rank(cls: number): number {
  if (cls === CLASS_OUTPUT) return 2;
  if (cls === CLASS_SENSORY) return 1;
  return 0;
}
