/**
 * "What the fly sees": the retina raster (design A3).
 *
 * The drive per column comes from `projectFrame` and `DEFAULT_RETINA_CONFIG` in
 * `@flybrain/brain` — the same kernel the simulation runs on the same framebuffer — so this panel
 * is the fly's actual input rather than a lookalike built from the picture.
 *
 * Panel pixel coordinates are computed once from `visual-xy.binz`, per eye, because each
 * hemisphere has its own bounding box; per frame the only work is 1,572 drive values and 1,572
 * small dot writes into one ImageData.
 *
 * **`visual-xy.binz`'s x and y are on two different, incomparable grids** (found 2026-09-16, the operator:
 * "the retina seems squashed" — both eyes rendered as tall narrow leaves in
 * `mockups/steady-t1-senses.png`). `tools/build_flywire.py` copies `column_assignment.csv`'s
 * `x`/`y` straight through; measured on the committed dataset, one eye's `x` covers 18 unit steps
 * and its `y` covers 60. That is not a hex lattice — checked directly, every point's full radius-2
 * integer neighbourhood is populated (`(1,1)` and `(-1,-1)` exist exactly as often as the true
 * axial pair `(1,-1)`/`(-1,1)`, which a real six-neighbour hex lattice would never have) — so this
 * is a plain rectangular index grid, just not one where an x-step and a y-step are the same
 * physical distance. `src/lib/fit.ts`'s shared letterboxed fit assumes they are (it is built for
 * the connectome map, whose x/y really is one calibrated space), and preserving that ratio here
 * faithfully reproduces the squash rather than fixing it: a raw eye is 17 units wide and 59 tall
 * (aspect 3.47), and no amount of centering changes that ratio.
 *
 * The fix in `setColumns` below is not `lib/fit.ts`'s shared function: each eye's x and y are
 * scaled to fill its own half of the panel *independently*, because there is no shared unit
 * between them to preserve. That is not a distortion of "the true shape" — there is no true shape
 * to distort, only two axes quantised at different grains — and it produces a plausible round,
 * slightly-taller-than-wide compound eye outline (matching the half-cell's own aspect, about 1.2)
 * rather than the needle the shared ratio drew. `tests/unit/retina-fit.test.ts` checks the scaling
 * math directly: fed a synthetic rectangle at any aspect, it asserts the two axes are stretched to
 * the box's own width and height, independently, with no shared factor.
 */
import { DEFAULT_RETINA_CONFIG, projectFrame } from '@flybrain/brain';
import { FRAME_HEIGHT, FRAME_WIDTH } from '@flybrain/feed';
import type { PaintSurface } from './loop';

/** Gap between the two eye rasters. */
const EYE_GAP = 12;

/**
 * The independent per-axis fill scale for one eye: stretch `spanX`/`spanY` to exactly `boxW`/
 * `boxH`, each axis on its own factor. No shared "tighter axis wins" scale and no letterboxing —
 * both are `lib/fit.ts`'s answer for a point cloud whose x and y are one calibrated space, which
 * the retina's is not (see the module header). A fallback of 1 for a zero span keeps a
 * single-column eye from producing `Infinity`.
 */
export function fillScale(spanX: number, spanY: number, boxW: number, boxH: number): { scaleX: number; scaleY: number } {
  return {
    scaleX: boxW / (spanX || 1),
    scaleY: boxH / (spanY || 1),
  };
}

/**
 * Side of one column's dot, in canvas pixels.
 *
 * 3, not 2: the raster canvas grew 1.5x with the 1080p canvas, and a 2 px dot in a 336x240 field
 * measured RMS 0.037 at the phone downscale — a hair over the legibility floor, against 0.085 at
 * 720p. A dot has to be scaled with its canvas or it disappears at the far end of the encoder.
 */
const DOT = 3;

export interface RetinaColumnsInput {
  /** Interleaved x,y in dataset units. */
  xy: Float32Array;
  /** 0 = left eye (mirrored on X by the kernel), 1 = right. */
  hemisphere: Uint8Array;
  count: number;
}

export class RetinaSurface implements PaintSurface {
  readonly name = 'retina';

  private readonly ctx: CanvasRenderingContext2D;
  private readonly image: ImageData;
  private columns: RetinaColumnsInput | null = null;
  private drive: Float32Array = new Float32Array(0);
  private px: Int32Array = new Int32Array(0);
  private py: Int32Array = new Int32Array(0);
  private tint: [number, number, number] = [79, 195, 247];
  private backdrop: [number, number, number] = [10, 12, 16];

  constructor(
    private readonly canvas: HTMLCanvasElement,
    cssWidth: number,
    cssHeight: number,
    backingScale: number,
  ) {
    canvas.width = Math.round(cssWidth * backingScale);
    canvas.height = Math.round(cssHeight * backingScale);
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;

    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('retina canvas: no 2d context');
    this.ctx = ctx;
    this.image = ctx.createImageData(canvas.width, canvas.height);
  }

  /** Dot colour and backdrop, read from the theme's CSS variables at startup. */
  setColors(tint: [number, number, number], backdrop: [number, number, number]): void {
    this.tint = tint;
    this.backdrop = backdrop;
  }

  /**
   * Bind the dataset's visual columns and precompute their panel coordinates.
   *
   * Each eye gets half the panel, and each of *its own* x and y is independently scaled to fill
   * that half — not one shared scale picked by whichever axis is tighter. A shared scale is right
   * for the connectome map (`lib/fit.ts`), whose x and y are one calibrated space; it is wrong
   * here, where x and y are quantised at different grains and have no shared unit to preserve (see
   * the module header). `fillScale` below is the pure half of this, tested directly.
   */
  setColumns(columns: RetinaColumnsInput): void {
    this.columns = columns;
    this.drive = new Float32Array(columns.count);
    this.px = new Int32Array(columns.count);
    this.py = new Int32Array(columns.count);

    const halfWidth = Math.floor((this.canvas.width - EYE_GAP) / 2);
    const availWidth = halfWidth - DOT;
    const availHeight = this.canvas.height - DOT;
    const bounds = [
      { minX: Infinity, maxX: -Infinity, minY: Infinity, maxY: -Infinity },
      { minX: Infinity, maxX: -Infinity, minY: Infinity, maxY: -Infinity },
    ];

    for (let i = 0; i < columns.count; i++) {
      const eye = (columns.hemisphere[i] ?? 1) === 0 ? 0 : 1;
      const box = bounds[eye] as { minX: number; maxX: number; minY: number; maxY: number };
      const x = columns.xy[i * 2] as number;
      const y = columns.xy[i * 2 + 1] as number;
      box.minX = Math.min(box.minX, x);
      box.maxX = Math.max(box.maxX, x);
      box.minY = Math.min(box.minY, y);
      box.maxY = Math.max(box.maxY, y);
    }

    // Two independent scales per eye — see `fillScale` — computed once and reused for every
    // column of that eye.
    const eyeFits = bounds.map((box) => {
      const spanX = box.maxX - box.minX || 1;
      const spanY = box.maxY - box.minY || 1;
      return fillScale(spanX, spanY, availWidth, availHeight);
    });

    for (let i = 0; i < columns.count; i++) {
      const eye = (columns.hemisphere[i] ?? 1) === 0 ? 0 : 1;
      const box = bounds[eye] as { minX: number; maxX: number; minY: number; maxY: number };
      const fit = eyeFits[eye] as { scaleX: number; scaleY: number };

      let localX = ((columns.xy[i * 2] as number) - box.minX) * fit.scaleX;
      // The left eye is mirrored, matching `projectFrame`'s own hemisphere handling.
      if (eye === 0) localX = availWidth - localX;
      const localY = ((columns.xy[i * 2 + 1] as number) - box.minY) * fit.scaleY;

      const originX = eye === 0 ? 0 : halfWidth + EYE_GAP;
      this.px[i] = originX + Math.round(localX);
      this.py[i] = Math.round(localY);
    }

    this.clear();
  }

  /** Project one framebuffer and repaint the raster. */
  drawFrame(frame: Uint8Array): void {
    const columns = this.columns;
    if (!columns) return;

    projectFrame(frame, FRAME_WIDTH, FRAME_HEIGHT, columns, DEFAULT_RETINA_CONFIG.gain, this.drive);

    const data = this.image.data;
    const width = this.canvas.width;
    const [br, bg, bb] = this.backdrop;
    data.fill(0);
    for (let i = 0; i < data.length; i += 4) {
      data[i] = br;
      data[i + 1] = bg;
      data[i + 2] = bb;
      data[i + 3] = 255;
    }

    const [tr, tg, tb] = this.tint;
    // `gain` is 0.20 per unit luminance, so full white is 0.20 of drive.
    const scale = 1 / DEFAULT_RETINA_CONFIG.gain;

    for (let i = 0; i < columns.count; i++) {
      const level = Math.max(0, Math.min(1, (this.drive[i] as number) * scale));
      const r = br + (tr - br) * level;
      const g = bg + (tg - bg) * level;
      const b = bb + (tb - bb) * level;
      const x0 = this.px[i] as number;
      const y0 = this.py[i] as number;
      for (let dy = 0; dy < DOT; dy++) {
        let offset = ((y0 + dy) * width + x0) * 4;
        for (let dx = 0; dx < DOT; dx++) {
          data[offset] = r;
          data[offset + 1] = g;
          data[offset + 2] = b;
          data[offset + 3] = 255;
          offset += 4;
        }
      }
    }

    this.ctx.putImageData(this.image, 0, 0);
  }

  /** Flat backdrop, for boot and for before the columns arrive. */
  clear(): void {
    const [br, bg, bb] = this.backdrop;
    this.ctx.fillStyle = `rgb(${br} ${bg} ${bb})`;
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
  }

  draw(): void {
    // Repainted on new frames only; see `drawFrame`.
  }

  /** True once the dataset's visual columns are bound. */
  ready(): boolean {
    return this.columns !== null;
  }
}
