/**
 * The fly strip's renderer, and the `?fly=` switch that picks one.
 *
 * Two renderers draw the same rig (`src/fly/rig.ts`):
 *
 * - `webgl` — three.js, one WebGL context, low-poly meshes with flat shading. The design's
 *   primary, and the default.
 * - `paper` — the same joints projected by hand into a 2D canvas and filled as flat polygons.
 *   It exists because the capture host may have no usable WebGL at all; the design calls it the
 *   "paper fly" and accepts that it is plainer.
 *
 * `off` renders nothing and creates no context, which is the honest escape hatch if the fly ever
 * costs more than it is worth on air.
 *
 * `webgl` is loaded through a dynamic import so that a page running `paper` or `off` never parses
 * three.js at all — the fallback exists for the weakest host on the list, and handing it half a
 * megabyte of dead module would be a strange way to help it.
 */
import type { CanvasPalette } from '@/theme/colors';
import type { FlyDrives, FlyFrame } from './rig';

export type FlyMode = 'off' | 'webgl' | 'paper';

export interface FlyRenderer {
  readonly mode: FlyMode;
  /** Draw one posed frame. Called at most 30 times a second by the paint loop. */
  draw(frame: FlyFrame): void;
  dispose(): void;
}

export interface FlyRendererOptions {
  canvas: HTMLCanvasElement;
  width: number;
  height: number;
  palette: CanvasPalette;
}

/**
 * Build the renderer for a mode. Resolves to null for `off`, and also if no renderer can start —
 * a broadcast page drops the fly rather than failing to paint.
 *
 * `webgl` falls back to `paper` by itself when the GL context cannot be created. Measured on the
 * P0 spike (2026-09-15, run 2): the capture container's Chromium runs with `--disable-gpu
 * --disable-software-rasterizer` (`infra/config/chromium-flags`), so `getContext('webgl')` returns
 * null, `new WebglFly(...)` throws, and the page went to air at the default `?fly=webgl` with
 * **no fly at all** — `__stage.fly()` reporting `{mode: 'webgl', rendering: false, legTips: []}`.
 * That is the exact host the paper fly was written for, so it is taken automatically instead of
 * requiring an operator to have already known to put `&fly=paper` in the kiosk URL. `?fly=paper`
 * and `?fly=off` are unchanged, and a host that does have WebGL still gets the WebGL fly, so the
 * "exactly one GL context at `?fly=webgl`, zero at `?fly=paper`/`off`" count in
 * `tests/e2e/structure.spec.ts` still holds.
 */
export async function createFlyRenderer(mode: FlyMode, options: FlyRendererOptions): Promise<FlyRenderer | null> {
  if (mode === 'webgl') {
    try {
      const { WebglFly } = await import('./webgl');
      return new WebglFly(options);
    } catch (error) {
      console.warn(`fly renderer webgl unavailable, falling back to paper: ${(error as Error).message}`);
      mode = 'paper';
    }
  }
  if (mode === 'paper') {
    try {
      const { PaperFly } = await import('./paper');
      return new PaperFly(options);
    } catch (error) {
      console.warn(`fly renderer paper unavailable: ${(error as Error).message}`);
    }
  }
  return null;
}

export type { FlyDrives, FlyFrame };
