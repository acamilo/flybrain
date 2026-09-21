/**
 * Retina projection: an RGBA image of any size onto a population of input columns.
 *
 * Columns carry 2D coordinates in arbitrary dataset units. Each call re-derives the column
 * bounding box (columns are immutable in practice, but the original kernel recomputed the bounds
 * per frame and the arithmetic is reproduced here), normalizes every column into [0, 1], mirrors
 * the X axis for hemisphere 0, and samples one pixel with nearest-neighbour rounding.
 */

export interface RetinaConfig {
  /** Membrane drive per unit luminance. */
  gain: number;
  /** Default frame width in pixels. */
  width: number;
  /** Default frame height in pixels. */
  height: number;
}

/** Original constants: a Game Boy sized frame at 0.20 drive per unit luminance. */
export const DEFAULT_RETINA_CONFIG: RetinaConfig = { gain: 0.20, width: 160, height: 144 };

/** Retina column geometry, normally a view onto a dataset's visual arrays. */
export interface RetinaColumns {
  /** Interleaved x,y coordinates in dataset units, length >= 2 * count. */
  xy: Float32Array;
  /** 0 = left (mirrored on X), 1 = right. */
  hemisphere: Uint8Array;
  /** Number of columns to project. */
  count: number;
}

/** Rec. 709 luminance weights, matching the original kernel. */
const RED = 0.2126;
const GREEN = 0.7152;
const BLUE = 0.0722;

/**
 * Project one RGBA frame onto `out` (drive per column). `out` must have at least `columns.count`
 * entries; entries beyond the column count are left untouched.
 */
export function projectFrame(
  rgba: Uint8Array,
  width: number,
  height: number,
  columns: { xy: Float32Array; hemisphere: Uint8Array; count: number },
  gain: number,
  out: Float32Array,
): void {
  const { xy, hemisphere, count } = columns;
  let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
  for (let i = 0; i < count; i++) {
    minX = Math.min(minX, xy[i * 2]); maxX = Math.max(maxX, xy[i * 2]);
    minY = Math.min(minY, xy[i * 2 + 1]); maxY = Math.max(maxY, xy[i * 2 + 1]);
  }
  const lastX = width - 1;
  const lastY = height - 1;
  for (let i = 0; i < count; i++) {
    let normalizedX = (xy[i * 2] - minX) / (maxX - minX || 1);
    if (hemisphere[i] === 0) normalizedX = 1 - normalizedX;
    const normalizedY = (xy[i * 2 + 1] - minY) / (maxY - minY || 1);
    const x = Math.max(0, Math.min(lastX, Math.round(normalizedX * lastX)));
    const y = Math.max(0, Math.min(lastY, Math.round(normalizedY * lastY)));
    const offset = (y * width + x) * 4;
    const luminance = (rgba[offset] * RED + rgba[offset + 1] * GREEN + rgba[offset + 2] * BLUE) / 255;
    out[i] = luminance * gain;
  }
}
