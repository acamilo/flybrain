/**
 * The particle layer: `docs/design/animation.md`'s addendum, "particles, on a 2D canvas layer over
 * the rail (no WebGL) … at most 400 live, pooled, additive blending, decay 600 to 1200 ms".
 *
 * Every constraint in that sentence is load-bearing, and each one shapes the code:
 *
 *   - **Pooled, 400 max.** Capacity is allocated once, in typed arrays, structure-of-arrays: `x`
 *     in one `Float32Array`, `y` in the next. There is no `Particle` class and no object literal
 *     anywhere in `tick`, `draw` or any emitter, so the steady state allocates nothing and the
 *     garbage collector never has an opinion about a 24/7 broadcast. A dead particle is removed by
 *     swapping the last live one into its slot (`swap-remove`), which keeps the live range dense
 *     and makes `tick` a flat loop over `0..live`.
 *   - **Additive.** `globalCompositeOperation = 'lighter'`, so overlapping sparks bloom instead of
 *     stacking opaque discs, which is what makes a warm colour read as light on the dark stage
 *     without a `filter: blur` — a property the design's rules forbid outright.
 *   - **A 2D canvas layer over the rail.** So the whole system is one `tick(dtMs)` and one
 *     `draw(ctx)`, the two hooks `src/paint/loop.ts` already knows how to call, and `draw` returns
 *     without touching the context at all when nothing is alive: an idle stage pays nothing, and
 *     `tests/unit/motion-particles.test.ts` asserts exactly that.
 *   - **Decay 600 to 1200 ms.** `DECAY_MS`, and every emitter's lifetimes land inside it.
 *
 * Draw batching. Per-particle `globalAlpha` would mean one `fill()` per particle; instead alpha is
 * quantised into `ALPHA_STEPS` brightness buckets and every particle in the same (colour, bucket,
 * kind) group goes into one path with one `fill()`. At 400 live that is at most a couple of dozen
 * draw calls rather than 400, which is what keeps the layer inside the frame budget with the game,
 * the retina, the brain map and the fly all painting in the same 16.7 ms.
 *
 * Determinism. The jitter comes from a seeded `mulberry32`, not `Math.random`, so a screenshot run
 * (`tools/motion-strip.mts`) produces the same frame twice and a unit test can assert a position.
 */

/** Hard cap from the design addendum. */
export const MAX_PARTICLES = 400;

/** The design's decay window: nothing lives less than 600 ms or longer than 1200 ms. */
export const DECAY_MS = { min: 600, max: 1200 } as const;

/** How many colours one field can batch. Beyond this, later colours reuse the first slot. */
export const MAX_COLOURS = 8;

/** Brightness buckets used to batch fills. Six steps is invisible at these sizes and sub-second lives. */
export const ALPHA_STEPS = 6;

const KIND_DOT = 0;
const KIND_LINE = 1;
const KIND_RING = 2;
const KIND_COUNT = 3;

/** An absolute box, the same shape as `Box` in `src/lib/geometry.ts`. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

/**
 * The slice of `CanvasRenderingContext2D` the layer actually uses.
 *
 * Declared structurally so a unit test can hand `draw` a recording stub without a DOM, a real
 * context still satisfies it, and adding a call here is a deliberate act rather than something
 * that slips in because the whole 2D API was in scope.
 */
export interface ParticlePainter {
  globalAlpha: number;
  globalCompositeOperation: string;
  fillStyle: string | CanvasGradient | CanvasPattern;
  strokeStyle: string | CanvasGradient | CanvasPattern;
  lineWidth: number;
  save(): void;
  restore(): void;
  beginPath(): void;
  closePath(): void;
  moveTo(x: number, y: number): void;
  lineTo(x: number, y: number): void;
  arc(x: number, y: number, radius: number, startAngle: number, endAngle: number): void;
  fill(): void;
  stroke(): void;
}

/** Directions, in canvas radians (y grows downward), for emitter callers that want a name. */
export const DIR = {
  up: -Math.PI / 2,
  down: Math.PI / 2,
  left: Math.PI,
  right: 0,
} as const;

/** `mulberry32`: 32 bits of state, uniform enough for jitter, identical on every machine. */
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export interface ParticleFieldOptions {
  /** Live cap. Never above `MAX_PARTICLES`; lower is legal for a test or a reduced budget. */
  capacity?: number;
  /** RNG seed. Same seed, same burst, forever. */
  seed?: number;
}

/**
 * One pooled particle field. One instance per canvas layer; the stage has a single layer over the
 * rail, and the harness uses one too.
 */
export class ParticleField {
  readonly capacity: number;

  // Structure of arrays. Position in stage pixels, velocity in pixels per millisecond.
  private readonly px: Float32Array;
  private readonly py: Float32Array;
  private readonly vx: Float32Array;
  private readonly vy: Float32Array;
  /** Seek target, for `drift`. Ignored when `seek` is 0. */
  private readonly tx: Float32Array;
  private readonly ty: Float32Array;
  private readonly seek: Float32Array;
  private readonly ageMs: Float32Array;
  private readonly lifeMs: Float32Array;
  /** Dot radius, line half-length, or ring end radius, by kind. */
  private readonly size: Float32Array;
  /** Downward acceleration, px/ms². */
  private readonly gravity: Float32Array;
  /** Velocity decay rate, 1/ms: `v *= exp(-drag * dt)`, so it is frame-rate independent. */
  private readonly drag: Float32Array;
  /** Stroke width at birth, for the stroked kinds (the ring, the streak). */
  private readonly widthPx: Float32Array;
  private readonly kind: Uint8Array;
  private readonly colour: Uint8Array;

  /** Batch occupancy, recomputed each `draw`: one counter per (kind, colour, alpha bucket). */
  private readonly batchCounts: Uint16Array;

  private liveCount = 0;
  private droppedCount = 0;
  private readonly palette: string[] = [];
  private readonly paletteIndex = new Map<string, number>();
  private random: () => number;
  private seed: number;

  constructor(options: ParticleFieldOptions = {}) {
    this.capacity = Math.max(1, Math.min(options.capacity ?? MAX_PARTICLES, MAX_PARTICLES));
    const n = this.capacity;
    this.px = new Float32Array(n);
    this.py = new Float32Array(n);
    this.vx = new Float32Array(n);
    this.vy = new Float32Array(n);
    this.tx = new Float32Array(n);
    this.ty = new Float32Array(n);
    this.seek = new Float32Array(n);
    this.ageMs = new Float32Array(n);
    this.lifeMs = new Float32Array(n);
    this.size = new Float32Array(n);
    this.gravity = new Float32Array(n);
    this.drag = new Float32Array(n);
    this.widthPx = new Float32Array(n);
    this.kind = new Uint8Array(n);
    this.colour = new Uint8Array(n);
    this.batchCounts = new Uint16Array(KIND_COUNT * MAX_COLOURS * ALPHA_STEPS);
    this.seed = options.seed ?? 0x5eed;
    this.random = mulberry32(this.seed);
  }

  /** Live particles. */
  get live(): number {
    return this.liveCount;
  }

  /**
   * Particles refused because the pool was full, since construction or the last `clear`.
   *
   * Counted per *particle*, not per emitter call: an emitter stops at its first refusal (there is no
   * point asking 599 more times) and books the rest of its request here, so the number answers "how
   * much of the design's 400 was not enough" rather than "how many bursts were unlucky".
   */
  get dropped(): number {
    return this.droppedCount;
  }

  /** True when `draw` would paint nothing. */
  get idle(): boolean {
    return this.liveCount === 0;
  }

  /** Kill everything. The pool itself is not reallocated. */
  clear(): void {
    this.liveCount = 0;
    this.droppedCount = 0;
  }

  /** Restart the jitter sequence, so a screenshot run is reproducible frame for frame. */
  reseed(seed: number = this.seed): void {
    this.seed = seed;
    this.random = mulberry32(seed);
  }

  // -------------------------------------------------------------------------------------------
  // Emitters (the addendum's five)
  // -------------------------------------------------------------------------------------------

  /**
   * A cone of sparks: the milestone's "amber sparks from the new rung" and the reward's "few
   * sparks from the ticker line scaled by value".
   *
   * `dir` is the cone's axis in canvas radians and `spread` its full width, so
   * `sparks(x, y, 8, amber, DIR.up, Math.PI / 3)` is a 60-degree fan upward.
   */
  sparks(
    x: number,
    y: number,
    count: number,
    colour: string,
    dir: number = DIR.up,
    spread: number = Math.PI / 3,
    options: { speed?: number; lifeMs?: number; size?: number; gravity?: number } = {},
  ): number {
    const colourIndex = this.intern(colour);
    const speed = options.speed ?? 0.16;
    const life = options.lifeMs ?? DECAY_MS.min;
    const size = options.size ?? 1.8;
    const gravity = options.gravity ?? 0.00016;
    let spawned = 0;
    for (let i = 0; i < count; i += 1) {
      const slot = this.take();
      if (slot < 0) {
        this.droppedCount += count - i - 1;
        break;
      }
      const angle = dir + (this.random() - 0.5) * spread;
      const velocity = speed * (0.55 + this.random() * 0.9);
      this.initDot(slot, x, y, Math.cos(angle) * velocity, Math.sin(angle) * velocity, colourIndex, {
        lifeMs: life * (0.7 + this.random() * 0.5),
        size: size * (0.7 + this.random() * 0.8),
        gravity,
        drag: 0.0016,
      });
      spawned += 1;
    }
    return spawned;
  }

  /**
   * The badge's "larger fountain over the rail": particles launched up from across the bottom edge
   * of `rect`, arcing back down under gravity, lifetimes at the top of the decay window because
   * this is the one moment allowed to fill the frame.
   */
  fountain(rect: Rect, count: number, colour: string, options: { speed?: number; lifeMs?: number } = {}): number {
    const colourIndex = this.intern(colour);
    const speed = options.speed ?? 1.3;
    const life = options.lifeMs ?? DECAY_MS.max;
    let spawned = 0;
    for (let i = 0; i < count; i += 1) {
      const slot = this.take();
      if (slot < 0) {
        this.droppedCount += count - i - 1;
        break;
      }
      const x = rect.x + this.random() * rect.width;
      const y = rect.y + rect.height;
      const lateral = (this.random() - 0.5) * 0.12;
      const rise = -speed * (0.6 + this.random() * 0.7);
      this.initDot(slot, x, y, lateral, rise, colourIndex, {
        lifeMs: life * (0.7 + this.random() * 0.3),
        size: 1.6 + this.random() * 2.2,
        // Tuned against the rail's own 944 px: at v = 1.3 px/ms and g = 0.0021 px/ms², the apex is
        // about 400 px up and about 620 ms in, so the fountain fills the lower half of the rail and
        // is falling back before its 1200 ms lifetime runs out — never above the frame.
        gravity: 0.0021,
        drag: 0.0004,
      });
      spawned += 1;
    }
    return spawned;
  }

  /** The badge's "shockwave ring from the badge count": one expanding, thinning stroke. */
  shockwave(x: number, y: number, colour = '#ffb020', options: { radius?: number; lifeMs?: number; width?: number } = {}): number {
    const slot = this.take();
    if (slot < 0) return 0;
    const colourIndex = this.intern(colour);
    this.reset(slot);
    this.kind[slot] = KIND_RING;
    this.colour[slot] = colourIndex;
    this.px[slot] = x;
    this.py[slot] = y;
    this.size[slot] = options.radius ?? 260;
    this.lifeMs[slot] = options.lifeMs ?? 700;
    this.widthPx[slot] = options.width ?? 6;
    return 1;
  }

  /**
   * The rollback's "backward-streaming line field over the game": short streaks crossing `rect`
   * along `dir`, staggered in x so the field looks continuous rather than a single rank of dashes.
   */
  streamField(
    rect: Rect,
    dir: number = DIR.left,
    count = 40,
    colour = '#7fd7ff',
    options: { speed?: number; lifeMs?: number; length?: number; width?: number } = {},
  ): number {
    const colourIndex = this.intern(colour);
    const speed = options.speed ?? 1.1;
    const life = options.lifeMs ?? DECAY_MS.min;
    const length = options.length ?? 26;
    let spawned = 0;
    for (let i = 0; i < count; i += 1) {
      const slot = this.take();
      if (slot < 0) {
        this.droppedCount += count - i - 1;
        break;
      }
      this.reset(slot);
      this.kind[slot] = KIND_LINE;
      this.colour[slot] = colourIndex;
      this.px[slot] = rect.x + this.random() * rect.width;
      this.py[slot] = rect.y + this.random() * rect.height;
      const velocity = speed * (0.7 + this.random() * 0.6);
      this.vx[slot] = Math.cos(dir) * velocity;
      this.vy[slot] = Math.sin(dir) * velocity;
      this.lifeMs[slot] = life * (0.6 + this.random() * 0.6);
      this.size[slot] = length * (0.6 + this.random() * 0.8);
      this.widthPx[slot] = options.width ?? 1.5;
      spawned += 1;
    }
    return spawned;
  }

  /**
   * The sugar's "small warm sparks that drift from the fly's head toward the sugar chip": a burst
   * that is steered, not thrown. Each particle carries the target and accelerates toward it, so
   * the swarm converges on the chip instead of merely being aimed at it.
   */
  drift(
    from: Point,
    to: Point,
    count = 14,
    colour = '#f472a8',
    options: { lifeMs?: number; spreadPx?: number; size?: number } = {},
  ): number {
    const colourIndex = this.intern(colour);
    const life = options.lifeMs ?? 900;
    const spread = options.spreadPx ?? 30;
    const size = options.size ?? 2.6;
    let spawned = 0;
    for (let i = 0; i < count; i += 1) {
      const slot = this.take();
      if (slot < 0) {
        this.droppedCount += count - i - 1;
        break;
      }
      const x = from.x + (this.random() - 0.5) * spread;
      const y = from.y + (this.random() - 0.5) * spread;
      // A wide lifetime spread is what makes the swarm arrive over a few hundred milliseconds
      // rather than all at once, so it reads as sparks drifting along a path.
      const lifeMs = life * (0.6 + this.random() * 0.8);
      const dx = to.x - x;
      const dy = to.y - y;
      // Start with about half the average speed the trip needs and let the seek term supply the
      // rest: a straight lerp from A to B reads mechanical, an accelerating swarm reads alive.
      const base = 0.5 / lifeMs;
      this.initDot(slot, x, y, dx * base, dy * base, colourIndex, {
        lifeMs,
        size: size * (0.7 + this.random() * 0.7),
        gravity: 0,
        drag: 0.0006,
      });
      this.tx[slot] = to.x;
      this.ty[slot] = to.y;
      this.seek[slot] = 0.000004 * (0.8 + this.random() * 0.5);
      spawned += 1;
    }
    return spawned;
  }

  // -------------------------------------------------------------------------------------------
  // Simulation
  // -------------------------------------------------------------------------------------------

  /** Advance every live particle by `dtMs` and retire the expired ones. */
  tick(dtMs: number): void {
    if (!(dtMs > 0)) return;
    let i = 0;
    while (i < this.liveCount) {
      const age = this.ageMs[i] as number;
      const next = age + dtMs;
      if (next >= (this.lifeMs[i] as number)) {
        this.retire(i);
        continue;
      }
      this.ageMs[i] = next;

      if (this.kind[i] === KIND_RING) {
        i += 1;
        continue;
      }

      const seek = this.seek[i] as number;
      let vx = this.vx[i] as number;
      let vy = this.vy[i] as number;
      if (seek !== 0) {
        vx += ((this.tx[i] as number) - (this.px[i] as number)) * seek * dtMs;
        vy += ((this.ty[i] as number) - (this.py[i] as number)) * seek * dtMs;
      }
      vy += (this.gravity[i] as number) * dtMs;
      const damp = Math.exp(-(this.drag[i] as number) * dtMs);
      vx *= damp;
      vy *= damp;
      this.vx[i] = vx;
      this.vy[i] = vy;
      this.px[i] = (this.px[i] as number) + vx * dtMs;
      this.py[i] = (this.py[i] as number) + vy * dtMs;
      i += 1;
    }
  }

  /**
   * Paint the whole field additively. Restores every context property it changes.
   *
   * Returns the number of draw calls issued, which is what the unit test asserts is zero while the
   * field is empty — an idle stage must not pay for the layer at all.
   */
  draw(ctx: ParticlePainter): number {
    if (this.liveCount === 0) return 0;

    const counts = this.batchCounts;
    counts.fill(0);
    for (let i = 0; i < this.liveCount; i += 1) {
      const key = this.batchOf(i);
      counts[key] = (counts[key] as number) + 1;
    }

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';
    let calls = 0;

    for (let bucket = 0; bucket < ALPHA_STEPS; bucket += 1) {
      const alpha = (bucket + 1) / ALPHA_STEPS;
      for (let colour = 0; colour < this.palette.length; colour += 1) {
        const style = this.palette[colour] as string;

        if ((counts[batchKey(KIND_DOT, colour, bucket)] as number) > 0) {
          ctx.globalAlpha = alpha;
          ctx.fillStyle = style;
          ctx.beginPath();
          for (let i = 0; i < this.liveCount; i += 1) {
            if (this.kind[i] !== KIND_DOT || this.colour[i] !== colour || this.bucketOf(i) !== bucket) continue;
            const radius = (this.size[i] as number) * (1 - 0.35 * this.ageFraction(i));
            ctx.moveTo((this.px[i] as number) + radius, this.py[i] as number);
            ctx.arc(this.px[i] as number, this.py[i] as number, radius, 0, TAU);
          }
          ctx.fill();
          calls += 1;
        }

        if ((counts[batchKey(KIND_LINE, colour, bucket)] as number) > 0) {
          ctx.globalAlpha = alpha;
          ctx.strokeStyle = style;
          // One width per batch: every streak in a field is emitted with the same width, and a
          // per-particle width would cost a `stroke()` each.
          ctx.lineWidth = this.widthPx[this.firstOf(KIND_LINE, colour, bucket)] as number;
          ctx.beginPath();
          for (let i = 0; i < this.liveCount; i += 1) {
            if (this.kind[i] !== KIND_LINE || this.colour[i] !== colour || this.bucketOf(i) !== bucket) continue;
            // The streak trails *behind* the motion, so it is drawn back along the velocity.
            const speed = Math.hypot(this.vx[i] as number, this.vy[i] as number) || 1;
            const length = this.size[i] as number;
            ctx.moveTo(this.px[i] as number, this.py[i] as number);
            ctx.lineTo(
              (this.px[i] as number) - ((this.vx[i] as number) / speed) * length,
              (this.py[i] as number) - ((this.vy[i] as number) / speed) * length,
            );
          }
          ctx.stroke();
          calls += 1;
        }

        if ((counts[batchKey(KIND_RING, colour, bucket)] as number) > 0) {
          for (let i = 0; i < this.liveCount; i += 1) {
            if (this.kind[i] !== KIND_RING || this.colour[i] !== colour || this.bucketOf(i) !== bucket) continue;
            const fraction = this.ageFraction(i);
            ctx.globalAlpha = alpha;
            ctx.strokeStyle = style;
            // Out-expo on the radius and a thinning stroke: fast out, soft settle, gone.
            ctx.lineWidth = Math.max(0.5, (this.widthPx[i] as number) * (1 - fraction));
            ctx.beginPath();
            ctx.arc(
              this.px[i] as number,
              this.py[i] as number,
              (this.size[i] as number) * (1 - Math.pow(2, -8 * fraction)),
              0,
              TAU,
            );
            ctx.stroke();
            calls += 1;
          }
        }
      }
    }

    ctx.restore();
    return calls;
  }

  /** Colours currently interned, in batch order. For tests and the harness readout. */
  colours(): readonly string[] {
    return this.palette;
  }

  // -------------------------------------------------------------------------------------------
  // Pool internals
  // -------------------------------------------------------------------------------------------

  /** Claim a slot, or -1 when the pool is full (which counts as a dropped emission). */
  private take(): number {
    if (this.liveCount >= this.capacity) {
      this.droppedCount += 1;
      return -1;
    }
    const slot = this.liveCount;
    this.liveCount += 1;
    return slot;
  }

  /** Swap-remove: the last live particle takes the dead one's slot, so the range stays dense. */
  private retire(index: number): void {
    const last = this.liveCount - 1;
    if (index !== last) {
      this.px[index] = this.px[last] as number;
      this.py[index] = this.py[last] as number;
      this.vx[index] = this.vx[last] as number;
      this.vy[index] = this.vy[last] as number;
      this.tx[index] = this.tx[last] as number;
      this.ty[index] = this.ty[last] as number;
      this.seek[index] = this.seek[last] as number;
      this.ageMs[index] = this.ageMs[last] as number;
      this.lifeMs[index] = this.lifeMs[last] as number;
      this.size[index] = this.size[last] as number;
      this.gravity[index] = this.gravity[last] as number;
      this.drag[index] = this.drag[last] as number;
      this.widthPx[index] = this.widthPx[last] as number;
      this.kind[index] = this.kind[last] as number;
      this.colour[index] = this.colour[last] as number;
    }
    this.liveCount = last;
  }

  private reset(slot: number): void {
    this.vx[slot] = 0;
    this.vy[slot] = 0;
    this.tx[slot] = 0;
    this.ty[slot] = 0;
    this.seek[slot] = 0;
    this.ageMs[slot] = 0;
    this.gravity[slot] = 0;
    this.drag[slot] = 0;
    this.widthPx[slot] = 0;
  }

  private initDot(
    slot: number,
    x: number,
    y: number,
    vx: number,
    vy: number,
    colourIndex: number,
    options: { lifeMs: number; size: number; gravity: number; drag: number },
  ): void {
    this.reset(slot);
    this.kind[slot] = KIND_DOT;
    this.colour[slot] = colourIndex;
    this.px[slot] = x;
    this.py[slot] = y;
    this.vx[slot] = vx;
    this.vy[slot] = vy;
    this.lifeMs[slot] = clampLife(options.lifeMs);
    this.size[slot] = options.size;
    this.gravity[slot] = options.gravity;
    this.drag[slot] = options.drag;
  }

  /** 0 at birth, 1 at death. */
  private ageFraction(index: number): number {
    const life = this.lifeMs[index] as number;
    return life > 0 ? Math.min(1, (this.ageMs[index] as number) / life) : 1;
  }

  /** Brightness bucket, 0 (dimmest) to `ALPHA_STEPS - 1`: linear fade over the lifetime. */
  private bucketOf(index: number): number {
    const remaining = 1 - this.ageFraction(index);
    const bucket = Math.floor(remaining * ALPHA_STEPS);
    return bucket < 0 ? 0 : bucket >= ALPHA_STEPS ? ALPHA_STEPS - 1 : bucket;
  }

  /** First live particle in a batch. Only used to pick one stroke width for a whole streak batch. */
  private firstOf(kind: number, colour: number, bucket: number): number {
    for (let i = 0; i < this.liveCount; i += 1) {
      if (this.kind[i] === kind && this.colour[i] === colour && this.bucketOf(i) === bucket) return i;
    }
    return 0;
  }

  private batchOf(index: number): number {
    return batchKey(this.kind[index] as number, this.colour[index] as number, this.bucketOf(index));
  }

  /**
   * Intern a colour string to a small index, so a particle carries one byte rather than a string
   * reference and `draw` can batch by it. A field that somehow sees more than `MAX_COLOURS`
   * distinct colours reuses the first slot rather than growing: the alternative is an unbounded
   * map on a page that runs for weeks.
   */
  private intern(colour: string): number {
    const existing = this.paletteIndex.get(colour);
    if (existing !== undefined) return existing;
    if (this.palette.length >= MAX_COLOURS) return 0;
    const index = this.palette.length;
    this.palette.push(colour);
    this.paletteIndex.set(colour, index);
    return index;
  }
}

const TAU = Math.PI * 2;

function batchKey(kind: number, colour: number, bucket: number): number {
  return (kind * MAX_COLOURS + colour) * ALPHA_STEPS + bucket;
}

/**
 * Keep a *spark's* lifetime inside the design's 600-1200 ms decay window.
 *
 * Only the dot kinds go through this. The two stroked kinds are motion rather than decay and take
 * their duration from the catalogue instead: the shockwave ring is one 700 ms expansion tied to the
 * badge count's bounce, and a rewind streak is a 400 ms wipe's worth of travel across the game
 * canvas — clamping either to 600 ms would slow a wipe the design times in its own row.
 */
function clampLife(lifeMs: number): number {
  return Math.max(DECAY_MS.min, Math.min(DECAY_MS.max, lifeMs));
}
