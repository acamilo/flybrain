/**
 * Uniform, aspect-preserving fit of a 2D point cloud into a rectangular canvas.
 *
 * One scale for both axes — never two — sized so the whole extent fits inside the box without
 * cropping either axis, and centered (letterboxed on whichever axis has slack). Shared by the
 * connectome's base raster, its density-accumulator LUT and its PAM-centroid flare origin
 * (`workers/brain-base.worker.ts`), and by their unit tests, so none of them can compute a
 * different scale than the others and quietly pile points onto an edge.
 *
 * The one thing this deliberately does not do is clamp: a point outside the fitted box is
 * dropped (`project` returns `null`). Fitting a point cloud to its own true extent, with margin,
 * means nothing should ever fall outside it — but "should never happen" is exactly the case a
 * clamp turns into a silent pileup on the first or last row instead of a bug report.
 */

/** Margin so the extreme point sits just inside the edge, never touching it. */
export const FIT_MARGIN = 0.97;

/** The position-space box that a uniform letterboxed fit maps onto a `width` x `height` canvas. */
export interface HalfExtent {
  /** Half-width, in the point cloud's own units, that maps exactly to the canvas's left/right edge. */
  x: number;
  /** Half-height, in the point cloud's own units, that maps exactly to the canvas's top/bottom edge. */
  y: number;
}

/**
 * The half-extents of the box that fits `maxAbsX` x `maxAbsY` inside `width` x `height`.
 *
 * `Math.min` picks whichever axis is the tighter fit, and the same resulting scale sizes both
 * halves, so whatever is plotted with them keeps its own aspect ratio — it is letterboxed on the
 * other axis, never stretched to fill it.
 */
export function fitHalfExtent(
  maxAbsX: number,
  maxAbsY: number,
  width: number,
  height: number,
  margin = FIT_MARGIN,
): HalfExtent {
  const scale = Math.min(width / (2 * (maxAbsX || 1)), height / (2 * (maxAbsY || 1))) * margin;
  return { x: width / 2 / scale, y: height / 2 / scale };
}

/**
 * The half-extents of a flattened xyz point cloud on x and y, which is what `fitHalfExtent` has
 * to be fed to fit that cloud to a canvas.
 *
 * Shared rather than inlined at the one call site (`brain-base.worker.ts`) so that the unit tests
 * can fit the real dataset the way the worker fits it — the worker itself cannot be imported into
 * a test, since it installs a `self` message listener on load. A loop this small is exactly the
 * kind that gets copied into a test, drifts, and leaves the test passing over a fit the worker no
 * longer computes.
 */
export function pointCloudExtent(positions: Float32Array): { maxAbsX: number; maxAbsY: number } {
  const count = Math.floor(positions.length / 3);
  let maxAbsX = 0;
  let maxAbsY = 0;
  for (let i = 0; i < count; i++) {
    maxAbsX = Math.max(maxAbsX, Math.abs(positions[i * 3] as number));
    maxAbsY = Math.max(maxAbsY, Math.abs(positions[i * 3 + 1] as number));
  }
  return { maxAbsX, maxAbsY };
}

/**
 * Project one centered point into pixel coordinates against `halfExtent`, or `null` when it
 * falls outside the fitted box.
 *
 * Dropped, never clamped: an out-of-range point disappears rather than piling onto the first or
 * last row or column.
 */
export function project(
  x: number,
  y: number,
  halfExtent: HalfExtent,
  width: number,
  height: number,
): { x: number; y: number } | null {
  const nx = x / halfExtent.x;
  const ny = y / halfExtent.y;
  if (nx < -1 || nx > 1 || ny < -1 || ny > 1) return null;
  return {
    x: Math.min(width - 1, Math.round(((nx + 1) / 2) * (width - 1))),
    // Screen y grows downward; the point cloud's y grows upward.
    y: Math.min(height - 1, Math.round(((1 - ny) / 2) * (height - 1))),
  };
}
