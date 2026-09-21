/**
 * One `requestAnimationFrame` loop for the whole page (design A5, "repaint policy").
 *
 * Every surface is repainted only when it is dirty: the game and the retina on a new frame
 * (30 Hz), the brain map at up to 30 Hz (its 110 ms decay does not need 60), the button afterglow
 * every frame because it is the one thing that must look continuous, and React at 4 Hz through
 * the store's coalescing commit.
 *
 * The loop also measures itself. Every stage is timed with `performance.now()` into a ring of the
 * last 600 samples, which is what produces the p50/p95/max figures the milestone asks for, and
 * `window.__stage.metrics()` prints them. This is the instrumentation A5's phase-0 measurement
 * list needs, present from the first commit rather than bolted on later.
 */

export interface PaintSurface {
  readonly name: string;
  draw(nowMs: number, dtMs: number): void;
}

export interface StageTiming {
  count: number;
  meanMs: number;
  p50Ms: number;
  p95Ms: number;
  maxMs: number;
}

const RING = 600;

class Ring {
  private readonly values = new Float64Array(RING);
  private length = 0;
  private cursor = 0;
  private total = 0;
  private max = 0;
  private everything = 0;

  push(value: number): void {
    this.values[this.cursor] = value;
    this.cursor = (this.cursor + 1) % RING;
    if (this.length < RING) this.length += 1;
    this.total += value;
    this.everything += 1;
    if (value > this.max) this.max = value;
  }

  stats(): StageTiming {
    if (this.length === 0) return { count: 0, meanMs: 0, p50Ms: 0, p95Ms: 0, maxMs: 0 };
    const sorted = Array.from(this.values.subarray(0, this.length)).sort((a, b) => a - b);
    const at = (fraction: number) =>
      sorted[Math.min(sorted.length - 1, Math.floor(fraction * sorted.length))] ?? 0;
    return {
      count: this.everything,
      meanMs: this.total / this.everything,
      p50Ms: at(0.5),
      p95Ms: at(0.95),
      maxMs: this.max,
    };
  }
}

export class PaintLoop {
  private readonly stages = new Map<string, Ring>();
  private handle: number | null = null;
  private lastFrameMs = Number.NEGATIVE_INFINITY;
  private frames = 0;
  private longFrames = 0;
  private running = false;

  constructor(private readonly onFrame: (nowMs: number, dtMs: number, time: TimeFn) => void) {}

  start(): void {
    if (this.running) return;
    this.running = true;
    const step = (nowMs: number) => {
      if (!this.running) return;
      const dtMs = this.lastFrameMs === Number.NEGATIVE_INFINITY ? 16.7 : nowMs - this.lastFrameMs;
      this.lastFrameMs = nowMs;
      this.frames += 1;
      // A frame is "long" when it misses two 60 Hz vsyncs, which is the dropped-frame signal
      // A5's phase-0 list asks for.
      if (dtMs > 33.4) this.longFrames += 1;

      const frameStart = performance.now();
      this.onFrame(nowMs, dtMs, this.time);
      this.record('frame', performance.now() - frameStart);

      this.handle = requestAnimationFrame(step);
    };
    this.handle = requestAnimationFrame(step);
  }

  stop(): void {
    this.running = false;
    if (this.handle !== null) cancelAnimationFrame(this.handle);
    this.handle = null;
  }

  /** Time one stage. Returns whatever the body returns, so it wraps an expression cleanly. */
  private readonly time: TimeFn = (name, body) => {
    const started = performance.now();
    try {
      return body();
    } finally {
      this.record(name, performance.now() - started);
    }
  };

  private record(name: string, ms: number): void {
    let ring = this.stages.get(name);
    if (!ring) {
      ring = new Ring();
      this.stages.set(name, ring);
    }
    ring.push(ms);
  }

  /** Per-stage timings plus the frame accounting. */
  metrics(): { stages: Record<string, StageTiming>; frames: number; longFrames: number } {
    const stages: Record<string, StageTiming> = {};
    for (const [name, ring] of this.stages) stages[name] = ring.stats();
    return { stages, frames: this.frames, longFrames: this.longFrames };
  }
}

export type TimeFn = <T>(name: string, body: () => T) => T;
