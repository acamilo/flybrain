/**
 * The brain map, on a 2D canvas, in three tiers (design A5). No WebGL anywhere.
 *
 *   1. a static base bitmap of all 139,255 neurons plus the faint edge scatter, rasterised once
 *      in a worker and handed over as an `ImageBitmap`;
 *   2. a density accumulator over the spike bitset (252x185 cells — `MAP_GRID_WIDTH` and
 *      `MAP_GRID_HEIGHT` in `geometry.ts`, which take a different divisor per axis because 370 is
 *      not a multiple of 4), decayed with a 110 ms time constant, expanded into an `ImageData`,
 *      tinted per cell class and blitted additively;
 *   3. at most `MAX_SPRITES` radial-gradient sprites on the brightest cells, so individual
 *      neurons still read as flaring without paying per-neuron draw costs.
 *
 * The backing store is the tab pane, exactly: 1008x370, allocated once. Layout v1 sized it for a
 * promotion that animated the map over the whole rail; layout v2 makes the map a tab, so there is
 * no promotion, no transform and no second size — the map is always drawn at the size it is seen
 * at, which is also the only size its base bitmap is ever rasterised for.
 *
 * On top of the three tiers, one moment effect: the reward flare, which spreads outward from the
 * PAM cluster's own centroid (computed in the worker from the dataset's roles and positions, not
 * hand-placed) and briefly holds every recent spike at full glow.
 */
import { MAP_GRID_HEIGHT, MAP_GRID_WIDTH, MAP_HERO_HEIGHT, MAP_HERO_WIDTH } from '@/lib/geometry';
import { clamp01 } from '@/motion/lerp';
import type { BrainBaseFit } from '@/workers/brain-base.worker';
import { CLASS_OUTPUT, CLASS_SENSORY, accumulateBitset, decayAccumulator, topCells } from './accumulator';
import type { PaintSurface } from './loop';

/** Hard cap on sprites per frame (design A5). */
export const MAX_SPRITES = 256;

/** Decay time constant of the accumulator. */
export const DECAY_TAU_MS = 110;

/**
 * The reward flare (`docs/design/animation.md`): "reward flare spreads outward from the PAM
 * cluster position over 400 ms", and the badge's version holds every recent spike at full glow
 * for 500 ms before decaying.
 */
export const FLARE_SPREAD_MS = 400;
export const FLARE_GLOW_MS = 500;

/** Device pixels per accumulator cell, per axis (1008/252 = 4, 370/185 = 2). */
const CELL_WIDTH = MAP_HERO_WIDTH / MAP_GRID_WIDTH;
const CELL_HEIGHT = MAP_HERO_HEIGHT / MAP_GRID_HEIGHT;

/** Cell value at which a sprite is drawn on top of the blit. */
export const SPRITE_THRESHOLD = 3;

const SPRITE_SIZE = 16;

export interface BrainMapColors {
  sensory: [number, number, number];
  internal: [number, number, number];
  output: [number, number, number];
}

/**
 * What the map can report about itself, for `window.__stage.brainmap()`.
 *
 * The map's geometry lives in three places that have to agree — the canvas's backing store, the
 * CSS box it is presented in, and the fit the worker built the LUT with — and none of them is
 * visible from outside. When the CONNECTOME tab looks wrong on air, these are the numbers that
 * say whether it is the fit, the sizing, or the drawing, and `tests/e2e/connectome.spec.ts` reads
 * them exactly as an operator reads them off the live page over CDP.
 */
export interface BrainMapStats {
  canvas: { width: number; height: number; cssWidth: number; cssHeight: number; dpr: number };
  grid: { width: number; height: number; cells: number };
  fit: BrainBaseFit | null;
  neuronCount: number;
  /** Cells at or over `SPRITE_THRESHOLD` right now. Above `MAX_SPRITES` the sprite pass is capped. */
  cellsOverThreshold: number;
  /** Sprites drawn on the last frame. */
  sprites: number;
  /**
   * The grid rows the last frame's sprites landed on, and the fraction of the map they span.
   *
   * This is the readout the top-band regression is measured by: a saturated sprite pass that
   * selects by scan order collapses `span` to a couple of rows near `min`, whatever the map is
   * actually doing (`accumulator.ts`'s `topCells`).
   */
  hotRows: { min: number; max: number; span: number } | null;
}

export class BrainMapSurface implements PaintSurface {
  readonly name = 'brainmap';

  private readonly ctx: CanvasRenderingContext2D;
  private readonly grid: OffscreenCanvas | HTMLCanvasElement;
  private readonly gridCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;
  private readonly gridImage: ImageData;
  private readonly accum = new Float32Array(MAP_GRID_WIDTH * MAP_GRID_HEIGHT);
  private readonly hotCells = new Uint32Array(MAX_SPRITES);

  private base: ImageBitmap | null = null;
  private lut: Uint32Array | null = null;
  private cellClasses: Uint8Array | null = null;
  private neuronCount = 0;
  /** Centroid of the PAM cluster in canvas pixels, computed in the worker from roles+positions. */
  private pam: { x: number; y: number } | null = null;
  /** The worker's fit, kept only so `stats()` can report it. */
  private fit: BrainBaseFit | null = null;
  private flareStartedMs = Number.NEGATIVE_INFINITY;
  private flareStrength = 0;
  private sprite: OffscreenCanvas | HTMLCanvasElement | null = null;
  private colors: BrainMapColors = {
    sensory: [79, 195, 247],
    internal: [133, 155, 140],
    output: [255, 176, 32],
  };
  private lastDrawMs = Number.NEGATIVE_INFINITY;
  private pendingSpikes: Uint8Array | null = null;
  private spritesLastFrame = 0;

  constructor(private readonly canvas: HTMLCanvasElement) {
    canvas.width = MAP_HERO_WIDTH;
    canvas.height = MAP_HERO_HEIGHT;

    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('brain map canvas: no 2d context');
    this.ctx = ctx;
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, canvas.width, canvas.height);

    const { scratch, scratchCtx } = makeScratch(MAP_GRID_WIDTH, MAP_GRID_HEIGHT);
    this.grid = scratch;
    this.gridCtx = scratchCtx;
    this.gridImage = scratchCtx.createImageData(MAP_GRID_WIDTH, MAP_GRID_HEIGHT);
  }

  setColors(colors: BrainMapColors): void {
    this.colors = colors;
    this.sprite = buildSprite(colors.output);
  }

  /** Hand over the worker's output: the base bitmap, the LUT, the cell classes and the PAM centroid. */
  setBase(
    base: ImageBitmap,
    lut: Uint32Array,
    cellClasses: Uint8Array,
    neuronCount: number,
    pam: { x: number; y: number } | null = null,
    fit: BrainBaseFit | null = null,
  ): void {
    this.base = base;
    this.lut = lut;
    this.cellClasses = cellClasses;
    this.neuronCount = neuronCount;
    this.pam = pam;
    this.fit = fit;
    if (!this.sprite) this.sprite = buildSprite(this.colors.output);
  }

  /** The map's own geometry and saturation, for the tests and the operator surface. */
  stats(): BrainMapStats {
    let cellsOverThreshold = 0;
    for (let cell = 0; cell < this.accum.length; cell++) {
      if ((this.accum[cell] as number) >= SPRITE_THRESHOLD) cellsOverThreshold += 1;
    }

    let hotRows: BrainMapStats['hotRows'] = null;
    if (this.spritesLastFrame > 0) {
      let min = MAP_GRID_HEIGHT;
      let max = 0;
      for (let i = 0; i < this.spritesLastFrame; i++) {
        const row = Math.floor((this.hotCells[i] as number) / MAP_GRID_WIDTH);
        if (row < min) min = row;
        if (row > max) max = row;
      }
      hotRows = { min, max, span: (max - min + 1) / MAP_GRID_HEIGHT };
    }

    const rect = this.canvas.getBoundingClientRect();
    return {
      canvas: {
        width: this.canvas.width,
        height: this.canvas.height,
        cssWidth: rect.width,
        cssHeight: rect.height,
        dpr: typeof devicePixelRatio === 'number' ? devicePixelRatio : 1,
      },
      grid: { width: MAP_GRID_WIDTH, height: MAP_GRID_HEIGHT, cells: this.accum.length },
      fit: this.fit,
      neuronCount: this.neuronCount,
      cellsOverThreshold,
      sprites: this.spritesLastFrame,
      hotRows,
    };
  }

  /**
   * Start a reward flare from the PAM cluster.
   *
   * `strength` scales with the reward's own value, so a +0.05 exploration tick is a ripple and a
   * badge is a wave. With no PAM centroid (a dataset with no `reward_pam` role) only the glow
   * boost runs, which is the honest degradation: there is no cluster to spread from.
   */
  flare(nowMs: number, strength = 1): void {
    this.flareStartedMs = nowMs;
    this.flareStrength = clamp01(strength);
  }

  /** The PAM centroid, for the tests and the operator surface. */
  pamCentroid(): { x: number; y: number } | null {
    return this.pam;
  }

  /** True while a flare is still animating, so the loop keeps repainting an otherwise idle map. */
  flaring(nowMs: number): boolean {
    const age = nowMs - this.flareStartedMs;
    return age >= 0 && age < Math.max(FLARE_SPREAD_MS, FLARE_GLOW_MS);
  }

  /** True once the base bitmap has arrived; `data-ready` waits on this. */
  ready(): boolean {
    return this.base !== null;
  }

  /** Queue one snapshot's spikes. Accumulated on the next draw, so a burst costs one pass. */
  ingestSpikes(bitset: Uint8Array): void {
    this.pendingSpikes = bitset;
  }

  /** Sprites drawn on the last frame, for the metrics readout. */
  sprites(): number {
    return this.spritesLastFrame;
  }

  draw(nowMs: number): void {
    const base = this.base;
    const lut = this.lut;
    const cellClasses = this.cellClasses;
    if (!base || !lut || !cellClasses) return;

    const dtMs = this.lastDrawMs === Number.NEGATIVE_INFINITY ? 33 : nowMs - this.lastDrawMs;
    this.lastDrawMs = nowMs;

    if (this.pendingSpikes) {
      accumulateBitset(this.pendingSpikes, lut, this.accum, this.neuronCount);
      this.pendingSpikes = null;
    }
    decayAccumulator(this.accum, dtMs, DECAY_TAU_MS);

    const ctx = this.ctx;
    ctx.globalCompositeOperation = 'source-over';
    ctx.drawImage(base, 0, 0);

    // The flare's glow boost: every recent spike at (up to) full glow, decaying over 500 ms.
    const glowAge = nowMs - this.flareStartedMs;
    const boost =
      glowAge >= 0 && glowAge < FLARE_GLOW_MS
        ? 1 + 2.2 * this.flareStrength * (1 - glowAge / FLARE_GLOW_MS)
        : 1;

    // Expand the accumulator into the grid ImageData with the per-cell class tint.
    const data = this.gridImage.data;
    const { sensory, internal, output } = this.colors;
    for (let cell = 0; cell < this.accum.length; cell++) {
      const value = this.accum[cell] as number;
      const offset = cell * 4;
      if (value <= 0) {
        data[offset] = 0;
        data[offset + 1] = 0;
        data[offset + 2] = 0;
        data[offset + 3] = 255;
        continue;
      }
      // Compress the range: a cell holding hundreds of spikes should not clip everything around it.
      const level = Math.min(1, (Math.log1p(value) / Math.log1p(24)) * boost);
      const cls = cellClasses[cell] as number;
      const tint = cls === CLASS_OUTPUT ? output : cls === CLASS_SENSORY ? sensory : internal;
      data[offset] = (tint[0] * level) | 0;
      data[offset + 1] = (tint[1] * level) | 0;
      data[offset + 2] = (tint[2] * level) | 0;
      data[offset + 3] = 255;
    }
    this.gridCtx.putImageData(this.gridImage, 0, 0);

    ctx.globalCompositeOperation = 'lighter';
    ctx.imageSmoothingEnabled = true;
    ctx.drawImage(
      this.grid as CanvasImageSource,
      0,
      0,
      MAP_GRID_WIDTH,
      MAP_GRID_HEIGHT,
      0,
      0,
      MAP_HERO_WIDTH,
      MAP_HERO_HEIGHT,
    );

    // Tier 3: bounded sprite pass on the brightest cells.
    const sprite = this.sprite;
    if (sprite) {
      const count = topCells(this.accum, SPRITE_THRESHOLD, this.hotCells);
      this.spritesLastFrame = count;
      const half = SPRITE_SIZE / 2;
      for (let i = 0; i < count; i++) {
        const cell = this.hotCells[i] as number;
        const cx = (cell % MAP_GRID_WIDTH) * CELL_WIDTH;
        const cy = Math.floor(cell / MAP_GRID_WIDTH) * CELL_HEIGHT;
        ctx.drawImage(sprite as CanvasImageSource, cx - half, cy - half);
      }
    } else {
      this.spritesLastFrame = 0;
    }

    this.drawFlareRing(nowMs);

    ctx.globalCompositeOperation = 'source-over';
  }

  /**
   * The flare's expanding ring, from the PAM centroid outward over 400 ms.
   *
   * A stroked arc with a fading alpha rather than a radial-gradient fill: a gradient the width of
   * the map costs a new `createRadialGradient` per frame, and the ring is what reads at broadcast
   * scale anyway — the spread is the information, not the fill.
   */
  private drawFlareRing(nowMs: number): void {
    const pam = this.pam;
    if (!pam) return;
    const age = nowMs - this.flareStartedMs;
    if (age < 0 || age >= FLARE_SPREAD_MS) return;

    const t = age / FLARE_SPREAD_MS;
    const reach = Math.hypot(MAP_HERO_WIDTH, MAP_HERO_HEIGHT) * 0.55;
    const [r, g, b] = this.colors.output;
    const ctx = this.ctx;
    ctx.globalCompositeOperation = 'lighter';
    ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${(0.5 * this.flareStrength * (1 - t)).toFixed(3)})`;
    ctx.lineWidth = 4 + 10 * (1 - t);
    ctx.beginPath();
    ctx.arc(pam.x, pam.y, reach * t, 0, Math.PI * 2);
    ctx.stroke();
  }

  /** Flat fill for the cold-open state, before the base bitmap exists. */
  drawPlaceholder(background: string): void {
    this.ctx.globalCompositeOperation = 'source-over';
    this.ctx.fillStyle = background;
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
  }
}

/**
 * A 16x16 radial-gradient sprite, built once.
 *
 * Pre-rendered rather than drawn as a gradient per spike: `createRadialGradient` plus a fill is
 * tens of microseconds, and 256 of those per frame at 30 Hz is real money on a Haswell core with
 * no GPU.
 */
function buildSprite(tint: [number, number, number]): OffscreenCanvas | HTMLCanvasElement {
  const { scratch, scratchCtx } = makeScratch(SPRITE_SIZE, SPRITE_SIZE, true);
  const half = SPRITE_SIZE / 2;
  const gradient = scratchCtx.createRadialGradient(half, half, 0, half, half, half);
  const [r, g, b] = tint;
  gradient.addColorStop(0, `rgba(${r}, ${g}, ${b}, 0.85)`);
  gradient.addColorStop(0.45, `rgba(${r}, ${g}, ${b}, 0.25)`);
  gradient.addColorStop(1, `rgba(${r}, ${g}, ${b}, 0)`);
  scratchCtx.fillStyle = gradient;
  scratchCtx.fillRect(0, 0, SPRITE_SIZE, SPRITE_SIZE);
  return scratch;
}

function makeScratch(
  width: number,
  height: number,
  alpha = false,
): {
  scratch: OffscreenCanvas | HTMLCanvasElement;
  scratchCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;
} {
  if (typeof OffscreenCanvas !== 'undefined') {
    const scratch = new OffscreenCanvas(width, height);
    const scratchCtx = scratch.getContext('2d', { alpha });
    if (scratchCtx) return { scratch, scratchCtx };
  }
  const scratch = document.createElement('canvas');
  scratch.width = width;
  scratch.height = height;
  const scratchCtx = scratch.getContext('2d', { alpha });
  if (!scratchCtx) throw new Error('brain map: no scratch 2d context');
  return { scratch, scratchCtx };
}
