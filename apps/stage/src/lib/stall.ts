/**
 * The stall meter and the rollback budget: the two readouts on the LADDER tab that turn "nothing
 * is happening" into the thing people came to watch.
 *
 * The research ranks the stuck-o-meter as the highest value per hour and the cheapest thing on
 * the page, and `docs/design/ladder.md` fixes the numbers: a stall is 120 s without new
 * exploration, attempts per rung stay 3, and the lifetime budget is 36 for the 38-rung ladder.
 *
 * Two things the feed does not carry, and therefore have to be derived here:
 *
 *   - **time since the last explore event.** `milestone.sinceSeconds` is time at the rung, which
 *     is hours, not the tens of seconds the stall window measures. So the meter watches
 *     `events[]` for `explore`/`area` rewards.
 *   - **lifetime rollbacks.** `milestone.attempts` resets at every rung ("rollbacks since
 *     reaching this rank"), so the lifetime figure is a count of `recovery` events since the page
 *     loaded, and it says so on screen rather than pretending to know the run's whole history.
 *
 * Pure and clock-injected, like everything else that has to survive a fixture seek.
 */

/** The stall window from `docs/design/ladder.md`. */
export const STALL_WINDOW_SECONDS = 120;

/** Recovery attempts allowed per rung. */
export const ATTEMPTS_PER_RUNG = 3;

/** Lifetime recovery budget for the 38-rung ladder. */
export const LIFETIME_BUDGET = 36;

/** Reward kinds that count as exploration for the stall window. */
const EXPLORE_KINDS: ReadonlySet<string> = new Set(['explore', 'area']);

/** True when a feed event resets the stall window. */
export function isExploreEvent(event: { kind: string; rewardKind?: string }): boolean {
  return event.kind === 'reward' && event.rewardKind !== undefined && EXPLORE_KINDS.has(event.rewardKind);
}

/**
 * Time since the last exploration, as a fraction of the 120 s window.
 *
 * Armed on the first snapshot rather than at construction, so a page that has been open for ten
 * minutes with no feed does not claim the fly has been stalled for ten minutes.
 */
export class StallMeter {
  private lastExploreMs: number | null = null;
  private armedMs: number | null = null;

  /** Forget everything. A seek replays the recording from the top. */
  reset(): void {
    this.lastExploreMs = null;
    this.armedMs = null;
  }

  /** Called on every snapshot, whether or not it carried events. */
  arm(nowMs: number): void {
    if (this.armedMs === null) this.armedMs = nowMs;
  }

  /** Called for each exploration event. */
  noteExplore(nowMs: number): void {
    this.arm(nowMs);
    this.lastExploreMs = nowMs;
  }

  /** Seconds since the last exploration, or since the meter was armed. */
  secondsSince(nowMs: number): number {
    const since = this.lastExploreMs ?? this.armedMs;
    if (since === null) return 0;
    return Math.max(0, (nowMs - since) / 1000);
  }

  /** 0..1 through the stall window. Saturates at 1; a stall does not get worse than stalled. */
  fraction(nowMs: number, windowSeconds = STALL_WINDOW_SECONDS): number {
    if (windowSeconds <= 0) return 0;
    return Math.min(1, this.secondsSince(nowMs) / windowSeconds);
  }

  /** True once the window has run out: what colours the meter. */
  stalled(nowMs: number, windowSeconds = STALL_WINDOW_SECONDS): boolean {
    return this.fraction(nowMs, windowSeconds) >= 1;
  }
}

/**
 * Lifetime rollbacks, counted from `recovery` events.
 *
 * `sinceLoad` is deliberately named: the feed carries no lifetime total, so this counts what this
 * page has seen and the panel labels it that way.
 */
export class RollbackCounter {
  private count = 0;
  private lastMs: number | null = null;

  reset(): void {
    this.count = 0;
    this.lastMs = null;
  }

  note(nowMs: number): void {
    this.count += 1;
    this.lastMs = nowMs;
  }

  get sinceLoad(): number {
    return this.count;
  }

  /** Seconds since the last rollback, or null when there has not been one. */
  secondsSinceLast(nowMs: number): number | null {
    if (this.lastMs === null) return null;
    return Math.max(0, (nowMs - this.lastMs) / 1000);
  }
}
