/**
 * How a NAMED CIRCUITS bar decides what "full" means, without ever being told the feed's
 * absolute Hz range.
 *
 * The bug this replaces: the panel used to divide every role's rate by a fixed `fullScaleHz`
 * picked against the fixture generator's own numbers (`src/lib/labels.ts`). The first live run
 * (`infra/docs/p0-local-encoded-frame.png`) showed every command bar pegged at 100% because the
 * real per-role rates run far above what the fixture ever produced — a fixed ceiling can always
 * be exceeded by a feed nobody measured yet.
 *
 * The fix is the same idea `docs/readout.md` already uses for the decoder itself: score a rate
 * against a *reference* recorded from the feed, not against a constant. `PopulationDecoder`
 * scores `(rate + 1) / (baseline + 1)` against a baseline captured once at calibration. A bar
 * cannot do that — the page never sees the decoder's calibration baseline — so `CircuitScale`
 * keeps its own slow-moving reference per role instead: an envelope that climbs toward a
 * sustained high rate over `attackHalfLifeMs` and relaxes back down over `releaseHalfLifeMs`.
 *
 * Two time constants, not one, is what makes a burst read as a burst instead of just being
 * "the new normal" a frame later: a fast pulse (an A/B press, well under a second) barely moves
 * an envelope with a multi-second attack, so it still reads near full scale while it lasts, and
 * the envelope only relaxes back toward the lower steady rate afterward, over tens of seconds
 * — which is also why a bar sits mid-scale rather than pinned at 100% once the reference has
 * caught up: `headroom` keeps the fill at `1 / headroom` when the rate exactly equals its own
 * reference, leaving room above for the next burst to still be visible as one.
 */

/** How far above its own reference a role's rate reads as "full", so steady state has headroom
 *  left for a real burst to still stand out. 1.5 puts a settled bar at 1/1.5 ≈ 67%. */
export const DEFAULT_HEADROOM = 1.5;

/** How fast the reference climbs toward a sustained higher rate. */
export const DEFAULT_ATTACK_HALF_LIFE_MS = 4_000;

/** How fast the reference relaxes back down once the rate drops — the "decays back" half of the
 *  unit test, and roughly the 60 s memory window the fix asks for (three half-lives is ~87%). */
export const DEFAULT_RELEASE_HALF_LIFE_MS = 20_000;

/** Never let a reference (or the median below) collapse toward zero during a quiet boot. */
export const DEFAULT_FLOOR_HZ = 1;

export interface CircuitScaleOptions {
  attackHalfLifeMs?: number;
  releaseHalfLifeMs?: number;
  headroom?: number;
  floorHz?: number;
}

/** Clamp a raw Hz value to a `[0, 1]` bar fill against a reference, `headroom` included. Pure —
 *  used for the live rate, the peak-hold dot and the threshold tick alike, so all three read off
 *  the same scale. */
export function circuitFraction(valueHz: number, referenceHz: number, headroom: number = DEFAULT_HEADROOM): number {
  const safeReference = Math.max(referenceHz, 1e-6) * headroom;
  const fraction = Math.max(0, valueHz) / safeReference;
  return Math.max(0, Math.min(1, fraction));
}

/**
 * Per-role adaptive envelope. One instance per bar role, fed every snapshot (not every animation
 * frame) so a fixture seek's silent catch-up replay builds the same reference a live viewer would
 * have watched settle in real time (`src/feed/fixture.ts`'s seek contract).
 */
export class CircuitScale {
  private reference: number;
  private lastMs: number | null = null;
  private readonly attackHalfLifeMs: number;
  private readonly releaseHalfLifeMs: number;
  private readonly headroom: number;
  private readonly floorHz: number;

  constructor(seedHz: number, options: CircuitScaleOptions = {}) {
    this.attackHalfLifeMs = options.attackHalfLifeMs ?? DEFAULT_ATTACK_HALF_LIFE_MS;
    this.releaseHalfLifeMs = options.releaseHalfLifeMs ?? DEFAULT_RELEASE_HALF_LIFE_MS;
    this.headroom = options.headroom ?? DEFAULT_HEADROOM;
    this.floorHz = options.floorHz ?? DEFAULT_FLOOR_HZ;
    this.reference = Math.max(seedHz, this.floorHz);
  }

  /** Advance the envelope to `nowMs` given the latest rate. Call once per role per snapshot. */
  observe(valueHz: number, nowMs: number): void {
    const value = Math.max(0, valueHz);
    if (this.lastMs !== null) {
      const dtMs = Math.max(0, nowMs - this.lastMs);
      const halfLife = value >= this.reference ? this.attackHalfLifeMs : this.releaseHalfLifeMs;
      const decay = halfLife > 0 ? Math.pow(0.5, dtMs / halfLife) : 0;
      this.reference = value + (this.reference - value) * decay;
    }
    this.lastMs = nowMs;
    this.reference = Math.max(this.reference, this.floorHz);
  }

  /** `observe` then read the fraction back in one call — the common case for a live bar. */
  update(valueHz: number, nowMs: number): number {
    this.observe(valueHz, nowMs);
    return this.toFraction(valueHz);
  }

  /** Project any Hz value (the peak-hold dot, an inferred threshold) against the *current*
   *  reference without advancing it. */
  toFraction(valueHz: number): number {
    return circuitFraction(valueHz, this.reference, this.headroom);
  }

  get referenceHz(): number {
    return this.reference;
  }
}

/**
 * A cheap streaming approximation of the running median, used as the display's stand-in for the
 * decoder's real calibration baseline (`docs/readout.md`'s `(rate + 1) / (baseline + 1)` score),
 * which the feed protocol does not carry to the page (`docs/feed-protocol.md`'s `FeedHeader` has
 * no baseline field). It is not an exact order statistic — it nudges toward the value at a fixed
 * Hz-per-second rate rather than maintaining a sorted window — but it converges to the true
 * median of a role's rate over tens of seconds, which is precise enough for a faint tick mark.
 * Documented here and in `docs/readout.md` per the fix's own "document it" instruction.
 */
export class RunningMedian {
  private median: number;
  private readonly stepHzPerMs: number;
  private readonly floorHz: number;

  constructor(seedHz: number, stepHzPerSecond = 2, floorHz: number = DEFAULT_FLOOR_HZ) {
    this.floorHz = floorHz;
    this.median = Math.max(seedHz, floorHz);
    this.stepHzPerMs = stepHzPerSecond / 1000;
  }

  observe(valueHz: number, dtMs: number): void {
    const value = Math.max(0, valueHz);
    const step = this.stepHzPerMs * Math.max(0, dtMs);
    if (value > this.median) this.median = Math.min(value, this.median + step);
    else if (value < this.median) this.median = Math.max(value, this.median - step);
    this.median = Math.max(this.median, this.floorHz);
  }

  get medianHz(): number {
    return this.median;
  }
}

/**
 * Invert the decoder's own score formula (`docs/readout.md`: `score = (rate + 1) / (baseline +
 * 1)`) to find the rate a role would need to cross a given decision threshold, using the running
 * median in place of the real baseline. This is what the faint threshold tick is positioned at.
 */
export function thresholdRateHz(decisionThreshold: number, medianHz: number): number {
  return Math.max(0, decisionThreshold * (Math.max(0, medianHz) + 1) - 1);
}

/**
 * The decoder's decision threshold (`docs/readout.md`), by `CircuitGroup.id`, for the two groups
 * the readout actually gates on a fixed score: A/B at 1, Start/Select at 1.35 after boot. The
 * drive D-pad is an exclusive argmax with no fixed threshold, so it is absent here.
 *
 * TODO: this belongs on `CircuitGroup` in `src/lib/labels.ts` next to `fullScaleHz` — kept here
 * instead for now because another pass is editing that file's copy concurrently with this fix.
 */
export const CIRCUIT_DECISION_THRESHOLD: Record<string, number> = {
  press: 1,
  menu: 1.35,
};
