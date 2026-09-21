/**
 * The two motion primitives every other file in `src/motion` is built out of: a value that
 * *approaches* a target, and a tween that *runs* for a fixed duration.
 *
 * Why two and not one. `docs/design/animation.md`'s addendum asks for opposite things in the same
 * breath: "every displayed number and bar is lerped toward its target each frame … never
 * snapped", which is an open-ended approach with no end time (the target moves again 33 ms later,
 * on the next feed snapshot), and "arrivals 240 to 320 ms, flashes 120 ms up and 600 ms down,
 * holds 9 s", which is a clip with a known duration and a known end state. A tween cannot express
 * the first (there is no duration; the target keeps moving) and an exponential approach cannot
 * express the second (it never *arrives*, so the "static end state so a frozen frame reads
 * correctly" rule would be violated by definition). So: `Smoothed` for live values, `Tween` for
 * moments.
 *
 * Frame-rate independence, which is the whole reason this file exists rather than a `*= 0.9` in
 * each panel: the approach is `value += (target - value) * (1 - exp(-dt / tau))`, the closed-form
 * solution of `dv/dt = (target - v) / tau`, not the usual `value += (target - value) * k`. The
 * naive form makes `k` mean "per frame", so the same code settles at two different speeds at 30
 * and 60 Hz — and this page deliberately runs a 60 Hz paint loop over a 30 Hz feed
 * (`src/paint/loop.ts`), plus a fixture player that replays recordings against virtual time
 * (`src/feed/fixture.ts`), so "per frame" is never a stable unit here. With the exponential form,
 * `exp(-a/tau) * exp(-b/tau) === exp(-(a+b)/tau)`: two 16.7 ms steps compose into exactly the
 * same result as one 33.4 ms step, which is what `tests/unit/motion-lerp.test.ts` asserts at 30 against
 * 60 Hz.
 *
 * The time-constant convention: `tau` is the *e-folding* time, so a value covers 63% of its
 * distance in one tau, 95% in three. The addendum's "time constants 120 to 300 ms" are taus in
 * this sense, and `src/lib/circuit-scale.ts` uses half-lives for the same job on a much slower
 * scale (4 s attack, 20 s release); `HALF_LIFE_TO_TAU` converts, so the two files can quote each
 * other's numbers without a second convention being invented.
 */

/** Default time constant for a live readout: the middle of the addendum's 120-300 ms band. */
export const DEFAULT_TAU_MS = 180;

/** Multiply a half-life by this to get the equivalent `tau` (`tau = t½ / ln 2`). */
export const HALF_LIFE_TO_TAU = 1 / Math.LN2;

/**
 * One frame-rate-independent step of an exponential approach. Pure.
 *
 * `tauMs <= 0` means "no smoothing", which is a snap rather than a division by zero: a caller
 * that turns smoothing off (a fixture seek landing on its target frame) should not have to
 * special-case the call.
 */
export function approach(current: number, target: number, dtMs: number, tauMs: number): number {
  if (!(dtMs > 0)) return current;
  if (!(tauMs > 0)) return target;
  return target + (current - target) * Math.exp(-dtMs / tauMs);
}

export interface SmoothedOptions {
  /** Symmetric time constant. Ignored when `riseTauMs` / `fallTauMs` are both given. */
  tauMs?: number;
  /** Time constant while the value is climbing toward its target. */
  riseTauMs?: number;
  /** Time constant while it is falling. */
  fallTauMs?: number;
  /** Distance below which `settled()` reports true. */
  epsilon?: number;
}

/**
 * A single number that chases a target.
 *
 * Asymmetric taus are here because the catalogue's flashes are asymmetric: "value flashes amber
 * 120/600 ms" is one `Smoothed` with `riseTauMs: 40, fallTauMs: 200` (a tau is roughly a third of
 * the visible duration, since three taus is 95% of the way) driven by a single `to(1)` on the
 * event and `to(0)` immediately after — not two tweens and a timer.
 */
export class Smoothed {
  private current: number;
  private goal: number;
  private readonly riseTauMs: number;
  private readonly fallTauMs: number;
  private readonly epsilon: number;

  constructor(initial = 0, options: SmoothedOptions | number = {}) {
    const resolved: SmoothedOptions = typeof options === 'number' ? { tauMs: options } : options;
    const tau = resolved.tauMs ?? DEFAULT_TAU_MS;
    this.current = initial;
    this.goal = initial;
    this.riseTauMs = resolved.riseTauMs ?? tau;
    this.fallTauMs = resolved.fallTauMs ?? tau;
    this.epsilon = resolved.epsilon ?? 1e-4;
  }

  /** Aim at a new target. Does not move the value; `tick` does that. */
  to(target: number): void {
    this.goal = target;
  }

  /** Jump straight to a value, target included. Used by a seek, a reset or a first snapshot. */
  snap(value: number): void {
    this.current = value;
    this.goal = value;
  }

  /** Advance by `dtMs` and return the new value. */
  tick(dtMs: number): number {
    const tau = this.goal >= this.current ? this.riseTauMs : this.fallTauMs;
    this.current = approach(this.current, this.goal, dtMs, tau);
    return this.current;
  }

  /** Aim and advance in one call, for a value whose target is recomputed every frame. */
  update(target: number, dtMs: number): number {
    this.to(target);
    return this.tick(dtMs);
  }

  get value(): number {
    return this.current;
  }

  get target(): number {
    return this.goal;
  }

  /** True once the value is within `epsilon` of its target — the "static end state" check. */
  settled(): boolean {
    return Math.abs(this.goal - this.current) <= this.epsilon;
  }
}

/**
 * A map of role -> smoothed number, for the whole-record case: fifteen circuit bars, a rate per
 * role, a fill per rung.
 *
 * `values` is one mutable plain object updated in place, deliberately shaped like `hot.rates` in
 * `src/feed/store.ts` so the paint loop reads it the same way and nothing is allocated per frame.
 * Roles appear on first `to()` and are never removed, so the shape is stable after the first
 * snapshot.
 */
export class SmoothedRecord {
  /** Live values, mutated in place. Read it; do not write it. */
  readonly values: Record<string, number> = {};
  private readonly lanes = new Map<string, Smoothed>();
  private readonly options: SmoothedOptions;

  constructor(options: SmoothedOptions | number = {}) {
    this.options = typeof options === 'number' ? { tauMs: options } : options;
  }

  /** Aim one role at a target, creating its lane (already at `target`) on first sight. */
  to(role: string, target: number): void {
    const lane = this.lanes.get(role);
    if (lane) {
      lane.to(target);
      return;
    }
    // A brand-new role starts *at* its first value rather than sweeping up from zero: a bar that
    // appears mid-run because the feed grew a role is not an event, and animating it would read
    // as one.
    const fresh = new Smoothed(target, this.options);
    this.lanes.set(role, fresh);
    this.values[role] = target;
  }

  /** Aim every role in `targets` (absent roles keep their current target). */
  toAll(targets: Record<string, number>): void {
    for (const role in targets) this.to(role, targets[role] as number);
  }

  /** Advance every lane by `dtMs`. */
  tick(dtMs: number): void {
    for (const [role, lane] of this.lanes) this.values[role] = lane.tick(dtMs);
  }

  /** Current value of one role, 0 for a role that has never been seen. */
  value(role: string): number {
    return this.values[role] ?? 0;
  }

  /** Jump one role to a value with no animation. */
  snap(role: string, value: number): void {
    this.to(role, value);
    (this.lanes.get(role) as Smoothed).snap(value);
    this.values[role] = value;
  }

  get size(): number {
    return this.lanes.size;
  }

  settled(): boolean {
    for (const lane of this.lanes.values()) if (!lane.settled()) return false;
    return true;
  }
}

// ---------------------------------------------------------------------------------------------
// Tweens
// ---------------------------------------------------------------------------------------------

export type EasingFn = (x: number) => number;

/** The easing names the catalogue is allowed to use. */
export type EasingName = 'linear' | 'outExpo' | 'inOutCubic' | 'outBack' | 'stepped';

const BACK_C1 = 1.70158;
const BACK_C3 = BACK_C1 + 1;

/**
 * The pixel grid the stepped easing snaps to, in authoring pixels.
 *
 * `docs/design/gameboy-theme.md`, Motion: "easing on pixel elements snaps to 4 px steps (a
 * 'stepped' easing variant in motion/lerp.ts) so movement reads as sprite motion rather than
 * smooth interpolation, except for particles and the fly, which stay smooth." Mirrors
 * `--pixel-step` in `src/theme/tokens.css`; the CSS half of the same rule is `--ease-pixel`.
 */
export const PIXEL_STEP = 4;

/** The fill step for a bar, in authoring pixels. Structure, not motion: "filled in 8 px steps". */
export const BAR_STEP = 8;

/** Default span the bare `stepped` easing assumes, in pixels: 32 px, so eight 4 px steps. */
export const STEPPED_SPAN = 32;

/**
 * Snap a value onto a grid, with both ends pinned.
 *
 * Pinned because the design's static-end-state rule applies here too: a move that ends 1.5 px short
 * of its pose because the grid did not divide the distance is a frozen frame that reads wrong.
 * `step <= 0` is "no grid", which is a caller turning quantisation off rather than a division by
 * zero.
 */
export function quantize(value: number, step: number): number {
  if (!(step > 0)) return value;
  return Math.round(value / step) * step;
}

/**
 * Snap a 0..1 fraction of a `spanPx`-wide element onto a `stepPx` grid of real pixels.
 *
 * This is the form every caller actually wants: a bar's fill and a spine cell's height are
 * fractions, and the grid is in pixels, so the quantum is `stepPx / spanPx`. 0 and 1 are exact, so
 * an empty bar is empty and a full one is full however the grid divides the span.
 */
export function quantizePixels(fraction: number, spanPx: number, stepPx: number = PIXEL_STEP): number {
  if (!(spanPx > 0) || !(stepPx > 0)) return fraction;
  if (fraction <= 0) return 0;
  if (fraction >= 1) return 1;
  const steps = spanPx / stepPx;
  return clamp01(Math.round(fraction * steps) / steps);
}

/** Out-expo, named once: `EASINGS` and `stepped`'s default both need it, and one of them is first. */
const OUT_EXPO: EasingFn = (x) => (x >= 1 ? 1 : 1 - Math.pow(2, -10 * x));

/**
 * An easing that arrives in whole pixels: `base`, quantised onto a `stepPx` grid over `spanPx`.
 *
 * The shape of the motion is unchanged — out-expo still means fast in and soft settle — but the
 * value it reports is always a multiple of one pixel step, so a translate driven by it lands on the
 * grid on every frame instead of sliding through it. Both ends stay exact, for the same
 * frozen-frame reason `quantize` pins its own.
 */
export function stepped(
  spanPx: number,
  stepPx: number = PIXEL_STEP,
  base: EasingFn = OUT_EXPO,
): EasingFn {
  return (x) => (x <= 0 ? 0 : x >= 1 ? 1 : quantizePixels(base(x), spanPx, stepPx));
}

/**
 * How many CSS `steps()` a move of `spanPx` needs to land on the pixel grid.
 *
 * CSS can only quantise *time*, so the CSS half of the stepped rule turns a distance into a step
 * count and lets the timing function do the rest. At least one, so a zero-length move is still a
 * legal timing function.
 */
export function cssSteps(spanPx: number, stepPx: number = PIXEL_STEP): number {
  return Math.max(1, Math.round(spanPx / stepPx));
}

/**
 * The easing table. The three shapes `docs/design/animation.md` names, plus `linear`: out-expo for
 * arrivals ("fast in, soft settle"), in-out-cubic for crossfades, and out-back for the badge
 * count's "1.15x bounce", which needs an overshoot past 1. Plus `stepped`, which
 * `docs/design/gameboy-theme.md` adds for pixel elements.
 *
 * Every entry returns exactly 0 at 0 and exactly 1 at 1 — no floating-point near-misses — because
 * the design's static-end-state rule means a held or frozen frame must show the finished pose,
 * not 0.9997 of it. `tests/unit/motion-lerp.test.ts` asserts that with `strictEqual`.
 */
export const EASINGS: Record<EasingName, EasingFn> = {
  linear: (x) => x,
  // 1 - 2^(-10x), with the endpoint pinned: at x = 1 the formula gives 1 - 1/1024, not 1.
  outExpo: OUT_EXPO,
  inOutCubic: (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2),
  // Both ends are pinned: the polynomial form gives 2.2e-16 at x = 0, not 0, which would leave a
  // badge count a quarter-pixel off its resting place in a frozen frame.
  outBack: (x) => (x <= 0 ? 0 : x >= 1 ? 1 : 1 + BACK_C3 * Math.pow(x - 1, 3) + BACK_C1 * Math.pow(x - 1, 2)),
  /*
   * The named `stepped` is out-expo over the 32 px a typical pixel element moves, which is eight
   * 4 px steps. A caller that knows its own distance should build its own with `stepped(spanPx)`
   * rather than take this one: the grid is in pixels, so the step count is a property of the
   * element, not of the easing.
   */
  stepped: stepped(STEPPED_SPAN),
};

/** Resolve a name or a function to a function. */
export function easing(which: EasingName | EasingFn): EasingFn {
  return typeof which === 'function' ? which : EASINGS[which];
}

export interface TweenOptions {
  durationMs: number;
  easing?: EasingName | EasingFn;
  /** Dead time before the tween starts moving, counted from `start()`. */
  delayMs?: number;
}

/**
 * A fixed-duration clip, evaluated against an injected clock.
 *
 * No timers and no internal `performance.now()`: every method takes `nowMs`, the same contract
 * `src/feed/store.ts` and `src/lib/ticker.ts` already use, which is what lets a fixture seek land
 * mid-moment with the moment in exactly the pose a live viewer would have been looking at.
 *
 * A zero (or negative) duration is a legal tween that is simply already finished — a caller
 * turning an animation off should not have to branch.
 */
export class Tween {
  readonly durationMs: number;
  readonly delayMs: number;
  private readonly ease: EasingFn;
  private startedMs: number | null = null;

  constructor(options: TweenOptions) {
    this.durationMs = options.durationMs;
    this.delayMs = options.delayMs ?? 0;
    this.ease = easing(options.easing ?? 'outExpo');
  }

  /** (Re)start from `nowMs`. Restarting is how an equal-priority coalesce resets the clock. */
  start(nowMs: number): void {
    this.startedMs = nowMs;
  }

  /** Back to "never started": `progress` reads 0 and `done` reads false. */
  reset(): void {
    this.startedMs = null;
  }

  get started(): boolean {
    return this.startedMs !== null;
  }

  get startMs(): number | null {
    return this.startedMs;
  }

  /** Raw linear progress, 0..1, delay and duration included. */
  progress(nowMs: number): number {
    if (this.startedMs === null) return 0;
    const elapsed = nowMs - this.startedMs - this.delayMs;
    // Still inside the delay. A *zero-duration* tween, by contrast, is finished the instant its
    // delay is up rather than stuck at 0 — a caller that turned an animation off by setting the
    // duration to 0 wants the end state, not the start state.
    if (elapsed < 0) return 0;
    if (!(this.durationMs > 0)) return 1;
    if (elapsed >= this.durationMs) return 1;
    return elapsed / this.durationMs;
  }

  /** Eased progress, 0..1 (or past 1 mid-flight for `outBack`). */
  value(nowMs: number): number {
    return this.ease(this.progress(nowMs));
  }

  /** Eased progress mapped onto `[from, to]`. */
  between(nowMs: number, from: number, to: number): number {
    return from + (to - from) * this.value(nowMs);
  }

  done(nowMs: number): boolean {
    return this.startedMs !== null && nowMs - this.startedMs >= this.delayMs + Math.max(0, this.durationMs);
  }

  /** Total wall time from `start()` to the finished pose. */
  get totalMs(): number {
    return this.delayMs + Math.max(0, this.durationMs);
  }
}

/** Clamp to 0..1. Used by every renderer that maps a progress onto a pixel. */
export function clamp01(value: number): number {
  return value < 0 ? 0 : value > 1 ? 1 : value;
}

/** Plain linear interpolation, for a renderer that already has its own progress. */
export function mix(from: number, to: number, t: number): number {
  return from + (to - from) * t;
}
