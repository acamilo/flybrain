/**
 * The reward ticker's queue: a human-paced feed, not a log (design A3).
 *
 * The audit finding this fixes: on the old page a gym badge and a +0.05 exploration tick got the
 * same 2 s tint, and the reward label was sticky forever. So:
 *
 *   - **minimum dwell.** A promoted row stays up at least `minDwellMs` (4 s) before the next one
 *     is allowed in, however fast events arrive. A 30 Hz feed cannot flicker the panel.
 *   - **dedupe.** Repeated events of the same kind inside that kind's `dedupeMs` window collapse
 *     into one row with a count ("3 new places"), instead of three identical rows.
 *   - **tiers.** A `moment` event (a badge) jumps the dwell gate, because making the viewer wait
 *     4 s to be told about the badge is exactly the wrong pacing. Quiet and notable events queue.
 *
 * Pure and clock-injected: every method takes `nowMs`, so `tests/unit/ticker.test.ts` drives it
 * without timers and the fixture player can replay it against virtual time.
 */
import type { FeedEvent, FeedEventKind, RewardKind } from '@flybrain/feed';
import type { GameConfig, RewardTier } from '@/games';

/** Presentation tier of a row. Non-reward events get `system`. */
export type TickerTier = RewardTier | 'system';

/** One row in the ticker. */
export interface TickerItem {
  /** Feed event id of the first event folded into this row. Stable React key. */
  id: number;
  kind: FeedEventKind;
  rewardKind?: RewardKind;
  tier: TickerTier;
  /** The text as rendered. Constant copy from the game config, or the feed's own label. */
  label: string;
  /** Summed reward value across folded events, when the kind carries one. */
  amount?: number;
  /** How many events are folded into this row. 1 unless deduped. */
  count: number;
  /** Viewer display name for sugar/viewer events. */
  by?: string;
  /** When the row was created. */
  createdMs: number;
  /** When the row became visible, or null while it is still queued. */
  promotedMs: number | null;
}

export interface TickerOptions {
  /** Rows on screen. Design A2: 3. */
  visibleRows?: number;
  /** Minimum time a promoted row holds the panel before the next promotion. */
  minDwellMs?: number;
  /** Hard cap on queued-but-not-yet-shown rows. */
  maxQueue?: number;
}

export const DEFAULT_TICKER_OPTIONS = {
  visibleRows: 3,
  minDwellMs: 4000,
  maxQueue: 12,
} as const;

/**
 * Events that never reach the ticker.
 *
 * `checkpoint`: a 5 s heartbeat is not news.
 *
 * `viewer`: every accepted chat line logs one (`packages/feed/src/fake/simulator.ts`, and the real
 * service does the same), and rail layout v2 gives chat its own panel — so a `viewer` event in
 * EVENTS is the same line twice, once with its text and once as the word "chat". Sugar is its own
 * kind and still lands here, which is the viewer action EVENTS is actually about.
 */
const HIDDEN_KINDS: ReadonlySet<FeedEventKind> = new Set<FeedEventKind>(['checkpoint', 'viewer']);

export class TickerQueue {
  private readonly visibleRows: number;
  private readonly minDwellMs: number;
  private readonly maxQueue: number;
  private readonly game: GameConfig;

  private visible: TickerItem[] = [];
  private pending: TickerItem[] = [];
  private lastPromotionMs = Number.NEGATIVE_INFINITY;
  /** Bumped whenever the visible list changes, so the store can push to React on change only. */
  private revision = 0;

  constructor(game: GameConfig, options: TickerOptions = {}) {
    this.game = game;
    this.visibleRows = options.visibleRows ?? DEFAULT_TICKER_OPTIONS.visibleRows;
    this.minDwellMs = options.minDwellMs ?? DEFAULT_TICKER_OPTIONS.minDwellMs;
    this.maxQueue = options.maxQueue ?? DEFAULT_TICKER_OPTIONS.maxQueue;
  }

  /** Rows currently on screen, newest first. */
  items(): readonly TickerItem[] {
    return this.visible;
  }

  /** Rows waiting for the dwell gate. Exposed for the panel's "+2 more" affordance and tests. */
  queued(): readonly TickerItem[] {
    return this.pending;
  }

  /** Changes when the visible list changes. */
  version(): number {
    return this.revision;
  }

  /** Feed one event in. Returns true when it produced or folded into a row. */
  push(event: FeedEvent, nowMs: number): boolean {
    if (HIDDEN_KINDS.has(event.kind)) return false;

    const tier = this.tierOf(event);
    const label = this.labelOf(event);

    const folded = this.fold(event, nowMs);
    if (folded) return true;

    const item: TickerItem = {
      id: event.id,
      kind: event.kind,
      tier,
      label,
      count: 1,
      createdMs: nowMs,
      promotedMs: null,
      ...(event.rewardKind === undefined ? {} : { rewardKind: event.rewardKind }),
      ...(event.value === undefined ? {} : { amount: event.value }),
      ...(event.by === undefined ? {} : { by: event.by }),
    };

    if (tier === 'moment') {
      // Jump the gate: the badge is the reason anyone is watching.
      this.promote(item, nowMs);
      return true;
    }

    this.pending.push(item);
    if (this.pending.length > this.maxQueue) this.pending.splice(0, this.pending.length - this.maxQueue);
    this.tick(nowMs);
    return true;
  }

  /** Advance the dwell gate. Call once per paint; cheap and idempotent. */
  tick(nowMs: number): void {
    if (this.pending.length === 0) return;
    if (nowMs - this.lastPromotionMs < this.minDwellMs) return;
    const next = this.pending.shift();
    if (next) this.promote(next, nowMs);
  }

  private promote(item: TickerItem, nowMs: number): void {
    item.promotedMs = nowMs;
    this.visible.unshift(item);
    if (this.visible.length > this.visibleRows) this.visible.length = this.visibleRows;
    this.lastPromotionMs = nowMs;
    this.revision += 1;
  }

  /**
   * Fold a repeat into an existing row when the kind allows it.
   *
   * The window is measured from the row's creation, so a steady trickle of exploration ticks
   * collapses into one row that counts up and then ages out, rather than a row that never dies.
   */
  private fold(event: FeedEvent, nowMs: number): boolean {
    if (event.kind !== 'reward' || !event.rewardKind) return false;
    const copy = this.game.rewardCopy[event.rewardKind];
    if (!copy || copy.dedupeMs <= 0) return false;

    for (const item of [...this.visible, ...this.pending]) {
      if (item.rewardKind !== event.rewardKind) continue;
      if (nowMs - item.createdMs > copy.dedupeMs) continue;
      item.count += 1;
      if (event.value !== undefined) item.amount = (item.amount ?? 0) + event.value;
      item.label = copy.collapsedNoun ? `${item.count} ${copy.collapsedNoun}` : copy.label;
      this.revision += 1;
      return true;
    }
    return false;
  }

  private tierOf(event: FeedEvent): TickerTier {
    if (event.kind === 'reward' && event.rewardKind) {
      return this.game.rewardCopy[event.rewardKind]?.tier ?? 'quiet';
    }
    if (event.kind === 'milestone' || event.kind === 'sugar') return 'moment';
    if (event.kind === 'recovery') return 'notable';
    return 'system';
  }

  /**
   * Copy for a row.
   *
   * Reward rows use the game config's constant string, never the service's free text, so the
   * page's vocabulary is the page's own. Non-reward rows use the feed's `label`, which the
   * service generates from its own templates and never from chat.
   */
  private labelOf(event: FeedEvent): string {
    if (event.kind === 'reward' && event.rewardKind) {
      return this.game.rewardCopy[event.rewardKind]?.label ?? event.label;
    }
    return event.label;
  }
}
