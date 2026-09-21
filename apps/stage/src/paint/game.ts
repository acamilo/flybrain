/**
 * The game canvas: 160x144 RGBA in, an exactly integer-scaled picture out.
 *
 * The audit's finding on the old page was a 2.62x scale, which puts a different number of screen
 * rows under each source row and gives the encoder row-thickness jitter to chew on. 4x (640x576)
 * fits a 720p canvas, matches how every other Pokémon stream sizes the game, and with
 * `imageSmoothingEnabled = false` plus `image-rendering: pixelated` every source pixel becomes
 * exactly a 4x4 block.
 *
 * `?res=1080` multiplies the backing store by 1.5, so the scale is 6x and still integer.
 */
import { FRAME_HEIGHT, FRAME_WIDTH } from '@flybrain/feed';
import { clamp01 } from '@/motion/lerp';
import { REWIND_MS } from '@/motion/rail-motion';
import type { PaintSurface } from './loop';

/** Scanlines drawn across the wipe front, and how far the backward streaks reach. */
const FLICKER_LINES = 18;
const STREAK_LINES = 22;

export class GameSurface implements PaintSurface {
  readonly name = 'game';

  private readonly ctx: CanvasRenderingContext2D;
  private readonly scratch: OffscreenCanvas | HTMLCanvasElement;
  private readonly scratchCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;
  private readonly image: ImageData;

  /** The last framebuffer, so the wipe can repaint at 60 Hz between 30 Hz snapshots. */
  private lastFrame: Uint8Array | null = null;
  /** A copy of the canvas as it was when the rollback landed: what the wipe wipes away. */
  private before: OffscreenCanvas | HTMLCanvasElement | null = null;
  private beforeCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
  private rewindStartedMs = Number.NEGATIVE_INFINITY;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly cssWidth: number,
    private readonly cssHeight: number,
    backingScale: number,
  ) {
    canvas.width = Math.round(cssWidth * backingScale);
    canvas.height = Math.round(cssHeight * backingScale);
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;

    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('game canvas: no 2d context');
    ctx.imageSmoothingEnabled = false;
    this.ctx = ctx;

    const { scratch, scratchCtx } = makeScratch(FRAME_WIDTH, FRAME_HEIGHT);
    this.scratch = scratch;
    this.scratchCtx = scratchCtx;
    this.image = scratchCtx.createImageData(FRAME_WIDTH, FRAME_HEIGHT);
  }

  /** The integer scale actually in effect. The structural test asserts it is a whole number. */
  scale(): number {
    return this.canvas.width / FRAME_WIDTH;
  }

  /** Paint one framebuffer. Cheap enough (0.8-2.0 ms measured budget) to do on every new frame. */
  drawFrame(frame: Uint8Array, nowMs = Number.NEGATIVE_INFINITY): void {
    this.lastFrame = frame;
    this.image.data.set(frame);
    this.scratchCtx.putImageData(this.image, 0, 0);
    this.ctx.imageSmoothingEnabled = false;
    this.ctx.drawImage(
      this.scratch as CanvasImageSource,
      0,
      0,
      FRAME_WIDTH,
      FRAME_HEIGHT,
      0,
      0,
      this.canvas.width,
      this.canvas.height,
    );
    if (this.rewinding(nowMs)) this.overlayRewind(nowMs);
  }

  /**
   * Start the rollback wipe (`docs/design/animation.md`: "game canvas does a 400 ms horizontal
   * rewind wipe to the archived frame with a scanline flicker").
   *
   * This is the catalogue's one exception to "moments never touch the game or fly canvases", and
   * it earns it: a ratchet restore *is* a change to the game picture, and a wipe is the only way
   * to say "the last twenty minutes did not happen" without a caption claiming it.
   *
   * The canvas as it stands is copied first, so the wipe reveals the restored frame over the
   * frame the fly had got itself into.
   */
  startRewind(nowMs: number): void {
    if (!this.before || !this.beforeCtx) {
      const made = makeScratch(this.canvas.width, this.canvas.height);
      this.before = made.scratch;
      this.beforeCtx = made.scratchCtx;
    }
    this.beforeCtx.drawImage(this.canvas as CanvasImageSource, 0, 0);
    this.rewindStartedMs = nowMs;
  }

  /** True while the wipe is running. The loop repaints the game at 60 Hz for its duration. */
  rewinding(nowMs: number): boolean {
    return nowMs - this.rewindStartedMs < REWIND_MS;
  }

  /** Repaint the last frame, wipe included. Called every frame while `rewinding`. */
  redraw(nowMs: number): void {
    if (this.lastFrame) this.drawFrame(this.lastFrame, nowMs);
  }

  /**
   * The wipe itself: the old picture retreating to the left, a bright front, scanline flicker and
   * backward-streaming lines.
   *
   * All of it is `drawImage` and `fillRect` on the one canvas that is already being repainted, so
   * it costs a few tenths of a millisecond for 400 ms and nothing at all afterwards.
   */
  private overlayRewind(nowMs: number): void {
    const before = this.before;
    if (!before) return;
    const t = clamp01((nowMs - this.rewindStartedMs) / REWIND_MS);
    // Linear, deliberately, where every other arrival on the page is eased: a rewind is a
    // mechanical sweep at constant speed, and an eased front spends most of the 400 ms parked
    // against the left edge — which is exactly what the 300 ms review frame caught.
    const eased = t;
    const width = this.canvas.width;
    const height = this.canvas.height;
    const front = Math.round(width * (1 - eased));

    // What is left of the old frame, sliding out to the left.
    if (front > 0) {
      this.ctx.drawImage(before as CanvasImageSource, 0, 0, front, height, 0, 0, front, height);
    }

    // The front: a bright one-pixel-wide seam, the only hard edge in the effect.
    this.ctx.fillStyle = 'rgba(255, 255, 255, 0.55)';
    this.ctx.fillRect(Math.max(0, front - 3), 0, 3, height);

    // Scanline flicker over the whole canvas, phase-locked to the wipe so a frozen frame is
    // reproducible rather than randomly striped.
    const step = Math.max(2, Math.round(height / FLICKER_LINES));
    const phase = Math.round(eased * step * 4) % step;
    this.ctx.fillStyle = `rgba(0, 0, 0, ${(0.35 * (1 - t)).toFixed(3)})`;
    for (let y = phase; y < height; y += step) this.ctx.fillRect(0, y, width, 2);

    // Backward streaks: short bright lines trailing right of the front.
    this.ctx.fillStyle = `rgba(255, 255, 255, ${(0.18 * (1 - t)).toFixed(3)})`;
    for (let i = 0; i < STREAK_LINES; i++) {
      const y = Math.round(((i + 0.5) / STREAK_LINES) * height);
      const length = Math.round(width * 0.12 * (0.4 + ((i * 37) % 13) / 13));
      this.ctx.fillRect(front, y, length, 2);
    }
  }

  /** The boot state: a flat panel rather than a black hole, so a cold open does not look broken. */
  drawPlaceholder(background: string): void {
    this.ctx.fillStyle = background;
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
  }

  draw(): void {
    // Driven by the loop's frame-dirty check rather than by time; see `drawFrame`.
  }

  /** CSS size, for the tests that assert the 4x geometry. */
  size(): { width: number; height: number } {
    return { width: this.cssWidth, height: this.cssHeight };
  }
}

function makeScratch(
  width: number,
  height: number,
): {
  scratch: OffscreenCanvas | HTMLCanvasElement;
  scratchCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;
} {
  if (typeof OffscreenCanvas !== 'undefined') {
    const scratch = new OffscreenCanvas(width, height);
    const scratchCtx = scratch.getContext('2d', { alpha: false });
    if (scratchCtx) return { scratch, scratchCtx };
  }
  const scratch = document.createElement('canvas');
  scratch.width = width;
  scratch.height = height;
  const scratchCtx = scratch.getContext('2d', { alpha: false });
  if (!scratchCtx) throw new Error('game canvas: no scratch 2d context');
  return { scratch, scratchCtx };
}
