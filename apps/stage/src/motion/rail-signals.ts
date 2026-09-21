/**
 * The five things the rail needs from the feed that the motion engine does not provide.
 *
 * `MotionEngine` (`src/motion/engine.ts`) answers "what just happened, and what should it look
 * like". It deliberately does not answer these, because they are *readouts* rather than motion:
 *
 *   - **the stall meter.** Time since the last exploration, against the 120 s window of
 *     `docs/design/ladder.md`. The header's `milestone.sinceSeconds` is time at the rung, which is
 *     hours; the stall window is tens of seconds, so it has to be derived from `events[]`.
 *   - **the lifetime rollback count.** `milestone.attempts` resets at every rung ("rollbacks since
 *     reaching this rank"), so the lifetime figure is counted from `recovery` events and is
 *     labelled as what it is: what this page has seen.
 *   - **the best snapshot.** The game frame at the last rank-up, kept client-side. There is no
 *     archive on the feed, and the page already had the bytes in its hands.
 *   - **tab steering.** Whether a reward or a rung change happened since the last frame, which is
 *     what makes the slot show the tab the event belongs to (`src/lib/tabs.ts`).
 *   - **new chatters.** Whether a display name this page had not seen before said something, which
 *     takes the slot to DESCRIBE for 8 s (`src/lib/chatters.ts` decides what counts as new).
 *
 * One class rather than four, because they all want the same thing: every snapshot, with its
 * clock, and a `reset()` when a fixture seeks.
 */
import type { FeedHeader } from '@flybrain/feed';
import { hot } from '@/feed/store';
import { ChatterWatch } from '@/lib/chatters';
import { isExploreEvent, RollbackCounter, StallMeter } from '@/lib/stall';

/** The game frame captured at the last rank-up. */
export interface BestSnapshot {
  /** 160x144 RGBA, an owned copy of the framebuffer at the moment the rung was reached. */
  frame: Uint8Array;
  rank: number;
  label: string;
  /** Clock value of the capture, for the "1h18m ago" line. */
  atMs: number;
}

/** What the tab controller reads once per frame. */
export interface SteeringSignals {
  reward: boolean;
  rankChanged: boolean;
  /**
   * A display name this page had not seen before said something since the last frame, which puts
   * DESCRIBE on the slot for 8 s (`src/lib/tabs.ts`, `src/lib/chatters.ts`).
   */
  newChatter: boolean;
}

export class RailSignals {
  readonly stall = new StallMeter();
  readonly rollbacks = new RollbackCounter();

  private lastRank: number | null = null;
  private lastAttempts: number | null = null;
  private steering: SteeringSignals = { reward: false, rankChanged: false, newChatter: false };
  private snapshot: BestSnapshot | null = null;
  private version = 0;
  private readonly chatters: ChatterWatch;

  /**
   * `nowWallMs` is the page's connect time, and the only thing it is for is telling an arrival
   * from the chat ring's history (`src/lib/chatters.ts`). Injected so the unit tests can name it.
   */
  constructor(nowWallMs: number = Date.now()) {
    this.chatters = new ChatterWatch(nowWallMs);
  }

  /** A seek or a loop replays the recording from the top: none of this happened. */
  reset(): void {
    this.stall.reset();
    this.rollbacks.reset();
    this.lastRank = null;
    this.lastAttempts = null;
    this.steering = { reward: false, rankChanged: false, newChatter: false };
    this.chatters.reset();
    this.snapshot = null;
    this.version += 1;
  }

  /** One snapshot, events included. */
  observe(header: FeedHeader, nowMs: number): void {
    this.stall.arm(nowMs);

    let recoveries = 0;
    for (const event of header.events) {
      if (isExploreEvent(event)) this.stall.noteExplore(nowMs);
      if (event.kind === 'reward') this.steering.reward = true;
      if (event.kind === 'recovery') recoveries += 1;
    }

    // Chat is a tab steer like the others: a name nobody here has seen before is the one event on
    // the feed that is about the *audience* rather than the fly, and DESCRIBE is the tab that
    // answers it. Recorded before the rank check so a snapshot carrying both still counts it.
    if (this.chatters.observe(header.chat)) this.steering.newChatter = true;

    const rank = header.milestone.rank;
    if (this.lastRank !== null && rank > this.lastRank) {
      this.steering.rankChanged = true;
      this.capture(rank, header.milestone.label, nowMs);
    }
    this.lastRank = rank;

    // A rollback counts once per snapshot whether it arrived as an event or only as a changed
    // attempt count, which is the same both-sources rule the engine's trigger mapper follows.
    const attempts = header.milestone.attempts;
    const climbed = this.lastAttempts !== null && attempts > this.lastAttempts;
    this.lastAttempts = attempts;
    if (recoveries > 0 || climbed) this.rollbacks.note(nowMs);
  }

  /** Read and clear the steering flags. Once per frame. */
  takeSteering(): SteeringSignals {
    const out = this.steering;
    this.steering = { reward: false, rankChanged: false, newChatter: false };
    return out;
  }

  /** Distinct chat names this page has seen, for `window.__stage`. */
  chatterCount(): number {
    return this.chatters.count;
  }

  /** The best snapshot, or null before the first rank-up this page has seen. */
  best(): BestSnapshot | null {
    return this.snapshot;
  }

  /** Changes when `best()` does, so the thumbnail is drawn once per capture. */
  bestId(): number {
    return this.version;
  }

  /**
   * Keep the framebuffer from the rank-up.
   *
   * A copy, not the view: `hot.frame` is a window into the last wire message and the next snapshot
   * overwrites it, so a view would show the current game rather than the moment the fly earned the
   * rung. 92 KB per rung is the whole cost, and there is only ever one.
   */
  private capture(rank: number, label: string, nowMs: number): void {
    const frame = hot.frame;
    if (!frame) return;
    this.snapshot = { frame: frame.slice(), rank, label, atMs: nowMs };
    this.version += 1;
  }
}
