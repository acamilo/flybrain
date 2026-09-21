/**
 * Rasterise the static brain map once, off the main thread (design A5, tier 1).
 *
 * 139,255 direct pixel writes plus the edge scatter is roughly 2-3 ms of arithmetic, against
 * 15-30 ms for the same number of `fillRect` calls — and doing it in a worker means the first
 * paint of the page never waits for it. The worker also builds the two lookup tables the
 * per-frame accumulator needs, because it is the only place that has the positions loaded.
 *
 * It sends back an `ImageBitmap` plus the LUT and the per-cell class array, all transferred, so
 * nothing is copied and the worker can be terminated immediately afterwards.
 *
 * Reuses, rather than reimplements: `loadCompressed` from `@flybrain/brain/browser` and
 * `normalizePositions` / `classifyByRoles` from `@flybrain/brain/view/layout`. Not
 * `loadBrainDataset` (it would also pull 9.5 MB of connectivity the display never touches) and
 * not `view/connectome.ts` (it imports three).
 */
import { loadCompressed } from '@flybrain/brain/browser';
import { classifyByRoles, normalizePositions } from '@flybrain/brain/view/layout';
import { fitHalfExtent, pointCloudExtent, project } from '@/lib/fit';
import { OUT_OF_BOUNDS, buildCellClasses, buildGridLut } from '@/paint/accumulator';

export interface BrainBaseRequest {
  type: 'load';
  /** URL prefix serving one `data/<dataset>` directory. */
  base: string;
  width: number;
  height: number;
  gridWidth: number;
  gridHeight: number;
  /** Per-class RGB for the base point cloud. */
  colors: { sensory: [number, number, number]; internal: [number, number, number]; output: [number, number, number] };
}

export interface BrainBaseReady {
  type: 'ready';
  bitmap: ImageBitmap;
  lut: Uint32Array;
  cellClasses: Uint8Array;
  neuronCount: number;
  /**
   * Centroid of the `reward_pam` cluster in canvas pixels, or null when the dataset has no such
   * role.
   *
   * Computed here rather than hand-placed because this is the only place that has the positions
   * and the role index loaded, and because a hard-coded coordinate would silently point at the
   * wrong part of the brain the first time the dataset is rebuilt. The reward flare spreads from
   * it (`docs/design/animation.md`).
   */
  pam: { x: number; y: number } | null;
  /** Wall time spent rasterising, reported so the milestone's measurement is real. */
  rasterMs: number;
  /**
   * The fit the base raster, the density LUT and the PAM centroid all share.
   *
   * Reported rather than kept private because it is the only place these numbers exist, and
   * "the map looks wrong" is otherwise unanswerable from outside the worker: `__stage.brainmap()`
   * surfaces them so a test — or an operator on the live page over CDP — can check the canvas the
   * LUT was built for against the canvas it is being drawn on, and see whether the fit dropped
   * any neuron at all.
   */
  fit: BrainBaseFit;
}

/** What `fitHalfExtent` resolved to, and what it was asked to fit into. */
export interface BrainBaseFit {
  width: number;
  height: number;
  gridWidth: number;
  gridHeight: number;
  /** Half-extent of the point cloud itself, after `normalizePositions`. */
  maxAbsX: number;
  maxAbsY: number;
  /** Half-extent the fit maps onto the canvas edges. Never smaller than `maxAbs`. */
  halfExtentX: number;
  halfExtentY: number;
  /** Neurons the LUT dropped as out of bounds. Zero unless the fit and the positions disagree. */
  outOfBounds: number;
}

export interface BrainBaseFailed {
  type: 'error';
  message: string;
}

export type BrainBaseResponse = BrainBaseReady | BrainBaseFailed;

/** Brightness one neuron contributes to its pixel. Additive, so dense regions saturate. */
const NEURON_WEIGHT = 0.8;
/** Brightness one edge endpoint contributes. Deliberately faint: it is texture, not data. */
const EDGE_WEIGHT = 0.09;

/**
 * The dopamine cluster's role key.
 *
 * It is in `meta.json`'s `roles` (307 neurons), not in the `circuit-roles.json` sidecar, which
 * only carries the five broad classes the point cloud is tinted by.
 */
const PAM_ROLE = 'reward_pam';

const scope = self as unknown as Worker;

scope.addEventListener('message', (event: MessageEvent<BrainBaseRequest>) => {
  const request = event.data;
  if (request?.type !== 'load') return;
  void run(request).catch((error: unknown) => {
    const message: BrainBaseFailed = { type: 'error', message: (error as Error).message };
    scope.postMessage(message);
  });
});

async function run(request: BrainBaseRequest): Promise<void> {
  const { base, width, height, gridWidth, gridHeight, colors } = request;

  // Two role tables, because they hold different things: `circuit-roles.json` carries the broad
  // classes the point cloud is coloured by (sensory / motor / kenyon / …) and `meta.json` carries
  // the fine-grained readout roles, which is where `reward_pam` lives.
  const [rawPositions, edgeIndices, roles, metaRoles] = await Promise.all([
    loadCompressed(`${base}/positions.binz`, (buffer) => new Float32Array(buffer)),
    loadCompressed(`${base}/viewer-edges.binz`, (buffer) => new Uint32Array(buffer)),
    fetchRoles(`${base}/circuit-roles.json`),
    fetchRoles(`${base}/meta.json`),
  ]);

  const started = performance.now();

  const positions = normalizePositions(rawPositions);
  const neuronCount = Math.floor(positions.length / 3);
  const classes = classifyByRoles(roles, neuronCount);

  // Fit the cloud to the canvas instead of using the old viewer's fixed orthographic box: the
  // audit found the brain panel wasting a quarter of its area because the camera never adapted.
  // One uniform scale for x and y (`fitHalfExtent`), shared with the density-accumulator LUT
  // below and with the PAM centroid, so all three ever agree on where a neuron sits.
  const { maxAbsX, maxAbsY } = pointCloudExtent(positions);
  const halfExtent = fitHalfExtent(maxAbsX, maxAbsY, width, height);

  const intensity = new Float32Array(width * height);
  const pixelClass = new Uint8Array(width * height);

  const plot = (x: number, y: number, weight: number, cls: number | null): void => {
    const offset = y * width + x;
    intensity[offset] = (intensity[offset] as number) + weight;
    if (cls !== null && cls > (pixelClass[offset] as number)) pixelClass[offset] = cls;
  };

  // Outside the fitted box (there should be none, since `halfExtent` comes from these same
  // neurons' own extent) is dropped, never clamped onto the first/last row or column.
  const toPixel = (index: number): { x: number; y: number } | null =>
    project(positions[index * 3] as number, positions[index * 3 + 1] as number, halfExtent, width, height);

  // The edge scatter first, so neurons draw over it.
  for (let i = 0; i < edgeIndices.length; i++) {
    const neuron = edgeIndices[i] as number;
    if (neuron >= neuronCount) continue;
    const point = toPixel(neuron);
    if (point) plot(point.x, point.y, EDGE_WEIGHT, null);
  }

  for (let neuron = 0; neuron < neuronCount; neuron++) {
    const point = toPixel(neuron);
    if (point) plot(point.x, point.y, NEURON_WEIGHT, classes[neuron] as number);
  }

  const rgba = new Uint8ClampedArray(width * height * 4);
  for (let offset = 0, p = 0; offset < intensity.length; offset++, p += 4) {
    const value = intensity[offset] as number;
    if (value <= 0) {
      rgba[p + 3] = 255;
      continue;
    }
    // Compress a wide dynamic range: the central brain is thousands of neurons deep.
    const level = Math.min(1, Math.log1p(value) / Math.log1p(7));
    const cls = pixelClass[offset] as number;
    const tint = cls === 2 ? colors.output : cls === 0 ? colors.sensory : colors.internal;
    rgba[p] = tint[0] * level;
    rgba[p + 1] = tint[1] * level;
    rgba[p + 2] = tint[2] * level;
    rgba[p + 3] = 255;
  }

  const lut = buildGridLut(positions, gridWidth, gridHeight, halfExtent.x, halfExtent.y);
  const cellClasses = buildCellClasses(lut, classes, gridWidth * gridHeight);

  let outOfBounds = 0;
  for (let i = 0; i < lut.length; i++) if (lut[i] === OUT_OF_BOUNDS) outOfBounds += 1;

  // The PAM cluster's centre of mass, in the same pixel space the base bitmap was rasterised in
  // (the same `halfExtent`, via the same `toPixel`), so the flare starts exactly where the dots
  // it flares from are drawn.
  const pamIndices = metaRoles[PAM_ROLE] ?? roles[PAM_ROLE] ?? [];
  let pam: { x: number; y: number } | null = null;
  if (pamIndices.length > 0) {
    let sumX = 0;
    let sumY = 0;
    let counted = 0;
    for (const index of pamIndices) {
      if (index < 0 || index >= neuronCount) continue;
      const point = toPixel(index);
      if (!point) continue;
      sumX += point.x;
      sumY += point.y;
      counted += 1;
    }
    if (counted > 0) pam = { x: sumX / counted, y: sumY / counted };
  }

  const rasterMs = performance.now() - started;

  const bitmap = await createImageBitmap(new ImageData(rgba, width, height));

  const fit: BrainBaseFit = {
    width,
    height,
    gridWidth,
    gridHeight,
    maxAbsX,
    maxAbsY,
    halfExtentX: halfExtent.x,
    halfExtentY: halfExtent.y,
    outOfBounds,
  };

  const message: BrainBaseReady = { type: 'ready', bitmap, lut, cellClasses, neuronCount, pam, rasterMs, fit };
  scope.postMessage(message, [bitmap, lut.buffer, cellClasses.buffer]);
}

/** Read a `{ roles: { <name>: number[] } }` document. Both role tables have that shape. */
async function fetchRoles(url: string): Promise<Record<string, number[]>> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`unable to load roles from ${url}: HTTP ${response.status}`);
  const body = (await response.json()) as { roles?: Record<string, number[]> };
  return body.roles ?? {};
}
