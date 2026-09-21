/**
 * The engine: the one object the page holds, and the only file that knows the queue, the catalogue
 * and the particle pool all exist.
 *
 * It exists to satisfy one constraint from `src/paint/loop.ts`, which is not allowed to change: the
 * loop owns the single `requestAnimationFrame` for the whole page and calls surfaces that are
 * "dirty". So the engine exposes exactly the two hooks that fits — `tick(nowMs)` and `draw(ctx)` —
 * plus `surface(ctx)`, which packages them as the loop's own `PaintSurface` shape so wiring it in is
 * one line in `App.tsx` later and nothing in this directory has to know how the loop is structured.
 *
 * The split of responsibilities, top to bottom:
 *
 *   `TriggerMapper`  what happened   (feed header + events -> triggers)
 *   `MomentQueue`    what is on stage (priority, coalescing, the 9 s hold, phase progress)
 *   `MOMENT_CATALOGUE` what it looks like (regions, emitters, sound tier)
 *   `ParticleField`  the pixels       (pooled, additive, 400 max)
 *
 * The engine is the wiring between those four and owns nothing else. In particular it does not play
 * sound: the `AudioContext` and the master gain belong to `src/audio/engine.ts`, which is created by
 * `App` after a user gesture, so the engine *queues* cues and the page drains them with `takeCue()`.
 * That also keeps the whole engine testable in Node with no Web Audio at all.
 */
import type { FeedEvent, FeedHeader } from '@flybrain/feed';

import {
  type EmitterSpec,
  MOTION_ANCHORS,
  MOTION_REGIONS,
  MOTION_SEED,
  type MomentRecipe,
  type MotionAnchor,
  type MotionPalette,
  type MotionRegion,
  recipeFor,
  resolveMotionColours,
} from './catalogue';
import {
  type ActiveMoment,
  type MomentSnapshot,
  MomentQueue,
  type MomentTrigger,
  TriggerMapper,
} from './moments';
import { ParticleField, type ParticlePainter } from './particles';
import { type SfxCue, type SfxTier, sfxCue } from './sfx-tiers';

/** The loop's surface shape (`src/paint/loop.ts`), restated so nothing here imports the loop. */
export interface MotionSurface {
  readonly name: string;
  draw(nowMs: number, dtMs: number): void;
}

/** One queued sound request, drained by whoever owns the `AudioContext`. */
export interface QueuedCue {
  tier: SfxTier;
  cue: SfxCue;
  /** Moment id it belongs to, for a log line or a test. */
  momentId: number;
}

export interface MotionEngineOptions {
  clock?: () => number;
  /** Particle pool size. Defaults to the design's 400. */
  capacity?: number;
  /** RNG seed, so a screenshot run is reproducible. */
  seed?: number;
  /** Colour overrides. Defaults to the theme's CSS variables, or the token fallbacks with no DOM. */
  palette?: MotionPalette;
  /** Anchor overrides, for when the v2 panels can report their real element boxes. */
  anchors?: Partial<Record<MotionAnchor, { x: number; y: number }>>;
  /** Scale every hold — 0.1 for a demo reel, 1 on air. */
  holdScale?: number;
  /**
   * Longest frame the simulation will integrate in one step. A backgrounded tab or a GC pause can
   * hand the loop a 2 s `dt`, and integrating that would teleport every particle off screen; the
   * broadcast page's own dropped-frame threshold is 33.4 ms, so 100 ms is three of those.
   */
  maxStepMs?: number;
}

export class MotionEngine {
  readonly queue: MomentQueue;
  readonly field: ParticleField;
  readonly mapper = new TriggerMapper();

  private readonly clock: () => number;
  private readonly palette: MotionPalette;
  /**
   * Emission points, mutable.
   *
   * Not `readonly`: the catalogue's defaults are derived from the region boxes, and the page
   * replaces them with the measured centres of the elements they name once the panels have laid
   * out — the spine's current rung in particular moves every time the fly gains a rung. See
   * `setAnchors`.
   */
  private anchors: Record<MotionAnchor, { x: number; y: number }>;
  private readonly maxStepMs: number;

  private lastTickMs: number | null = null;
  /** Which emitter specs of the moment on stage have already fired. */
  private firedMask: boolean[] = [];
  /** Identity of the burst the mask belongs to: a coalesce bumps `count` and re-arms the emitters. */
  private firedKey = '';
  private readonly cues: QueuedCue[] = [];

  constructor(options: MotionEngineOptions = {}) {
    this.clock = options.clock ?? (() => performance.now());
    this.queue = new MomentQueue({ clock: this.clock, holdScale: options.holdScale });
    this.field = new ParticleField({
      capacity: options.capacity,
      seed: options.seed ?? MOTION_SEED,
    });
    this.palette = options.palette ?? resolveMotionColours();
    this.anchors = { ...MOTION_ANCHORS, ...options.anchors };
    this.maxStepMs = options.maxStepMs ?? 100;
  }

  /** One feed snapshot in: events and header deltas both become moments. */
  ingest(header: FeedHeader, nowMs: number = this.clock()): void {
    for (const trigger of this.mapper.observe(header)) this.queue.enqueue(trigger, nowMs);
  }

  /** Ask for a moment directly — a `FeedEvent`, or a trigger the harness built by hand. */
  enqueue(input: MomentTrigger | FeedEvent, nowMs: number = this.clock()): number | null {
    return this.queue.enqueue(input, nowMs);
  }

  /**
   * Advance the moment state machine and the particle simulation to `nowMs`.
   *
   * Safe to call every frame whether or not anything is happening: with no moment and no live
   * particles it is a handful of comparisons.
   */
  tick(nowMs: number = this.clock()): void {
    const previous = this.lastTickMs;
    this.lastTickMs = nowMs;
    // A negative step means the clock moved backwards, which a fixture seek does on purpose
    // (`src/feed/fixture.ts`); treat it as a fresh start rather than integrating backwards.
    const raw = previous === null ? 0 : nowMs - previous;
    const dtMs = raw < 0 ? 0 : Math.min(raw, this.maxStepMs);

    this.queue.tick(nowMs);
    const active = this.queue.active(nowMs);
    if (active) this.fireDue(active);
    else this.firedKey = '';

    this.field.tick(dtMs);
  }

  /** Paint the particle layer. Returns the number of draw calls, 0 when nothing is alive. */
  draw(ctx: ParticlePainter): number {
    return this.field.draw(ctx);
  }

  /**
   * Package `tick` and `draw` as one paint surface for the rAF loop.
   *
   * The loop hands its surfaces `(nowMs, dtMs)` and each surface owns its own context, so the
   * context is bound here rather than passed per frame.
   */
  surface(ctx: ParticlePainter, name = 'motion'): MotionSurface {
    return {
      name,
      draw: (nowMs: number) => {
        this.tick(nowMs);
        this.draw(ctx);
      },
    };
  }

  /** True when this frame has something to paint — the loop's dirty check. */
  get dirty(): boolean {
    return !this.field.idle || this.queue.getSnapshot().active !== null;
  }

  /** The moment on stage, with progress at `nowMs`. Reused object; read it, do not keep it. */
  active(nowMs: number = this.clock()): ActiveMoment | null {
    return this.queue.active(nowMs);
  }

  /** The React-facing snapshot. */
  snapshot(): MomentSnapshot {
    return this.queue.getSnapshot();
  }

  subscribe(listener: () => void): () => void {
    return this.queue.subscribe(listener);
  }

  /** Recipe of whatever is on stage, for a renderer that wants its regions. */
  activeRecipe(nowMs: number = this.clock()): MomentRecipe | null {
    const active = this.queue.active(nowMs);
    return active ? recipeFor(active.type) : null;
  }

  /** Take the next queued sound cue, or null. Drained by the page's audio engine. */
  takeCue(): QueuedCue | null {
    return this.cues.shift() ?? null;
  }

  /** How many cues are waiting. */
  get cueCount(): number {
    return this.cues.length;
  }

  /**
   * Replace some or all of the emission points with measured ones.
   *
   * Called every frame by the rail's director with the element centres it can see, so a recipe
   * that says "sparks from the new rung" fires from the rung the viewer is looking at rather than
   * from the arithmetic guess the catalogue ships as a default.
   */
  setAnchors(anchors: Partial<Record<MotionAnchor, { x: number; y: number }>>): void {
    this.anchors = { ...this.anchors, ...anchors };
  }

  /** Where a region is, for a renderer or the harness. */
  region(which: MotionRegion): { x: number; y: number; width: number; height: number } {
    return MOTION_REGIONS[which];
  }

  /**
   * Drop every moment, particle and cue.
   *
   * The particle field is *reseeded*, not just cleared: a reset means "replay from here", and a
   * fixture seek or a screenshot run that replays the same moment has to produce the same burst,
   * or the contact sheets would show six frames of six different explosions.
   */
  reset(): void {
    this.queue.reset();
    this.mapper.reset();
    this.field.clear();
    this.field.reseed();
    this.cues.length = 0;
    this.firedKey = '';
    this.firedMask = [];
    this.lastTickMs = null;
  }

  /**
   * Fire the emitters of the moment on stage whose delay has elapsed, once each.
   *
   * Keyed by moment id *and* coalesce count, so a second sugar folding into the first re-fires the
   * burst — which is the visible half of "equal priority coalesces": the caption does not change but
   * the sparks say it happened again.
   */
  private fireDue(active: ActiveMoment): void {
    const recipe = recipeFor(active.type);
    const key = `${String(active.id)}:${String(active.count)}`;
    if (key !== this.firedKey) {
      this.firedKey = key;
      this.firedMask = recipe.emitters.map(() => false);
      const cue = sfxCue(recipe.sfx);
      if (cue.sfx) this.cues.push({ tier: recipe.sfx, cue, momentId: active.id });
    }

    for (let i = 0; i < recipe.emitters.length; i += 1) {
      if (this.firedMask[i]) continue;
      const spec = recipe.emitters[i] as EmitterSpec;
      if (active.elapsedMs < (spec.delayMs ?? 0)) continue;
      this.fire(spec, active.trigger.intensity);
      this.firedMask[i] = true;
    }
  }

  /** One catalogue row's emitter call, with the trigger's intensity scaling its count. */
  private fire(spec: EmitterSpec, intensity: number): void {
    const scale = intensity <= 0 ? 0 : Math.min(1, intensity);
    const count = (base: number) => Math.max(1, Math.round(base * scale));
    switch (spec.emitter) {
      case 'sparks': {
        const at = this.anchors[spec.at];
        this.field.sparks(at.x, at.y, count(spec.count), this.palette[spec.colour], spec.dir, spec.spread, {
          speed: spec.speed,
        });
        return;
      }
      case 'fountain':
        this.field.fountain(MOTION_REGIONS[spec.over], count(spec.count), this.palette[spec.colour]);
        return;
      case 'shockwave': {
        const at = this.anchors[spec.at];
        this.field.shockwave(at.x, at.y, this.palette[spec.colour], { radius: spec.radius });
        return;
      }
      case 'streamField':
        this.field.streamField(MOTION_REGIONS[spec.over], spec.dir, count(spec.count), this.palette[spec.colour], {
          speed: spec.speed,
        });
        return;
      case 'drift': {
        const from = this.anchors[spec.from];
        const to = this.anchors[spec.to];
        this.field.drift(from, to, count(spec.count), this.palette[spec.colour]);
        return;
      }
    }
  }
}
