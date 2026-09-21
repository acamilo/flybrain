/**
 * The moment queue: "one moment at a time", from `docs/design/animation.md`.
 *
 * The design's three sentences are the whole specification, and each one is a rule here:
 *
 *   - *"A queue with priority: badge > milestone > rollback > sugar > small reward."* →
 *     `MOMENT_PRIORITY`. One number per type, all distinct, so "equal priority" and "same type"
 *     are the same statement and coalescing needs no second concept.
 *   - *"A lower-priority moment waits"* → `pending`, ordered by priority then arrival.
 *   - *"equal priority coalesces"* → a second sugar while sugar is on stage folds into the moment
 *     already playing (count goes up, the caption updates, the hold clock restarts) instead of
 *     queueing a duplicate nobody can tell apart from the first.
 *
 * And one rule the design implies rather than states: a *higher* priority moment does not wait.
 * `docs/stream-mvp-plan.md`'s rail layout v2 says "big moments pre-empt for 9 s", and the
 * existing `FeedIngest.startMoment` (`src/feed/store.ts`) already implements "a bigger moment
 * wins" for the brain-map promotion. So a badge arriving during a reward cuts the reward's exit
 * short (`PREEMPT_LEAVE_MS`) and takes the stage; the reward is *spent*, not requeued, because the
 * ticker has already told the viewer about it and replaying it 9 s later would be a lie about
 * when it happened.
 *
 * Determinism. Nothing here reads a clock it was not handed: `enqueue` and `tick` take `nowMs`,
 * the `clock` option only supplies a default for callers that have no `nowMs` in scope, and there
 * are no timers. That is the same contract as `src/lib/ticker.ts` and `src/feed/store.ts`, and it
 * is what lets `tests/unit/motion-moments.test.ts` drive nine seconds of state machine in a loop and a
 * fixture seek land mid-moment in the right pose.
 *
 * What this file is *not*: a renderer. It owns which moment is on stage, which phase it is in and
 * how far through that phase it is; `catalogue.ts` says what that should look like, and the
 * per-frame drawing is `particles.ts` plus (later) the panels themselves.
 */
import type { FeedEvent, FeedHeader, GameMode } from '@flybrain/feed';

import { EASINGS, type EasingName, Tween } from './lerp';

/**
 * The moment types.
 *
 * The first five are the design's priority list verbatim. `dayRollover` and `modeChange` are the
 * catalogue's two other data-caused transitions that occupy the frame for a while ("DAY N slides
 * across the strip once, 2 s", "chip text swaps with a 200 ms vertical roll"); the design does not
 * rank them because they cannot collide with anything interesting, so they sit at the bottom where
 * a real moment always wins.
 */
export type MomentType = 'badge' | 'milestone' | 'rollback' | 'sugar' | 'reward' | 'dayRollover' | 'modeChange';

export const MOMENT_TYPES: readonly MomentType[] = [
  'badge',
  'milestone',
  'rollback',
  'sugar',
  'reward',
  'dayRollover',
  'modeChange',
];

/**
 * Priority, high wins. Distinct per type on purpose: the design's "equal priority coalesces" then
 * means exactly "a second badge folds into the badge on stage", with no case where two different
 * kinds of moment have to argue about which caption to show.
 */
export const MOMENT_PRIORITY: Record<MomentType, number> = {
  badge: 6,
  milestone: 5,
  rollback: 4,
  sugar: 3,
  reward: 2,
  dayRollover: 1,
  modeChange: 0,
};

export interface MomentTiming {
  /** Arrival. The design's band is 240-320 ms; the rollback's wipe is its own 400 ms. */
  enterMs: number;
  /** Time fully on stage, after the arrival and before the exit. */
  holdMs: number;
  /** Exit. */
  leaveMs: number;
  enterEasing: EasingName;
  leaveEasing: EasingName;
}

/**
 * Durations, straight off the catalogue table in `docs/design/animation.md`.
 *
 * Badge and milestone are 9 s of *total stage time* — the design states that number three times
 * (holds, LADDER focus, the verification's t+9.5 s assertion), so it is the one figure in this
 * file that is not a judgement call. It is the total rather than the hold because that is what the
 * design's own verification measures ("assert the DOM state at t+100 ms and at t+9.5 s: entered,
 * then gone"): with `holdMs: 9000` the badge is still leaving at t+9.5 s, and the LADDER focus the
 * catalogue calls "9 s" lasts 9.64. So the hold is 8360 and 320 + 8360 + 320 is exactly 9000. The others are the catalogue's own motion descriptions added up:
 *
 *   - rollback: a 400 ms rewind wipe, then the best-snapshot thumbnail "blinks twice" and the
 *     caption "REWIND · try 3" has to be readable, which is 2.4 s of hold at the 27 px body floor.
 *   - sugar: the proboscis "extends over 400 ms and holds for the pulse", and a pulse is 400 ms by
 *     default and 1000 ms at the configured maximum (`docs/control-api.md`), so 1.6 s of hold
 *     covers the long pulse without outstaying it.
 *   - reward: "line slides up 240 ms, value flashes amber 120/600 ms" — 240 in, the 720 ms of
 *     flash as the hold, 240 out.
 *   - dayRollover: "slides across the strip once, 2 s", so the slide *is* the moment: 2 s split
 *     into a 300 ms arrival, a beat, and a 300 ms exit.
 *   - modeChange: "chip text swaps with a 200 ms vertical roll", and nothing holds afterwards.
 *
 * Easings are the design's rule, not a per-row choice: out-expo for arrivals ("fast in, soft
 * settle"), in-out-cubic for anything that is really a crossfade.
 */
export const MOMENT_TIMING: Record<MomentType, MomentTiming> = {
  badge: { enterMs: 320, holdMs: 8360, leaveMs: 320, enterEasing: 'outExpo', leaveEasing: 'inOutCubic' },
  milestone: { enterMs: 320, holdMs: 8360, leaveMs: 320, enterEasing: 'outExpo', leaveEasing: 'inOutCubic' },
  rollback: { enterMs: 400, holdMs: 2400, leaveMs: 300, enterEasing: 'inOutCubic', leaveEasing: 'inOutCubic' },
  sugar: { enterMs: 400, holdMs: 1600, leaveMs: 300, enterEasing: 'outExpo', leaveEasing: 'inOutCubic' },
  reward: { enterMs: 240, holdMs: 720, leaveMs: 240, enterEasing: 'outExpo', leaveEasing: 'inOutCubic' },
  dayRollover: { enterMs: 300, holdMs: 1400, leaveMs: 300, enterEasing: 'inOutCubic', leaveEasing: 'inOutCubic' },
  modeChange: { enterMs: 200, holdMs: 0, leaveMs: 200, enterEasing: 'outExpo', leaveEasing: 'inOutCubic' },
};

/** Total on-stage time of one moment type, arrival and exit included. */
export function momentTotalMs(type: MomentType): number {
  const timing = MOMENT_TIMING[type];
  return timing.enterMs + timing.holdMs + timing.leaveMs;
}

/**
 * How fast a pre-empted moment gets off the stage. Short enough to feel like the badge interrupted
 * it, long enough not to be a cut (the design forbids cuts: "tab and moment transitions are
 * interpolated … not cut").
 */
export const PREEMPT_LEAVE_MS = 120;

/** Hard cap on waiting moments, so an event storm cannot grow the queue without bound. */
export const DEFAULT_MAX_PENDING = 8;

/** Value tier of a small reward, which is what scales its spark count. */
export type RewardValueTier = 'low' | 'mid' | 'high';

/**
 * Tier boundaries in reward units.
 *
 * Calibrated against the reward generator's own ranges (`packages/feed/src/fake/simulator.ts`,
 * `REWARD_VALUE_RANGE`): explore 0.02-0.08 and wildwin 0.05-0.15 are the low tier, area 0.1-0.2
 * and pokedex 0.15-0.3 the middle, trainer 0.2-0.4 and story 0.3-0.6 the top. A badge is 1.0 and
 * never reaches here — it is its own moment.
 */
export const REWARD_TIER_BOUNDS = { low: 0.1, mid: 0.3 } as const;

/** Particle-count scale per tier, used by `catalogue.ts`. */
export const REWARD_TIER_INTENSITY: Record<RewardValueTier, number> = { low: 0.35, mid: 0.65, high: 1 };

export function rewardTier(value: number): RewardValueTier {
  if (!(value > 0)) return 'low';
  if (value < REWARD_TIER_BOUNDS.low) return 'low';
  if (value < REWARD_TIER_BOUNDS.mid) return 'mid';
  return 'high';
}

/**
 * One request for a moment: what happened, in the words the caption will use.
 *
 * Flat and optional-heavy rather than a discriminated union per type, because this is the seam the
 * later rail wiring reads as *data* — a panel asks "what is on stage, how strong is it, what does
 * the caption say" without switching on the type.
 */
export interface MomentTrigger {
  type: MomentType;
  /** Caption line. The feed's own `label` where there is one; never raw chat. */
  label: string;
  /** Optional second line: the try count, the viewer name, the new mode. */
  detail?: string;
  /** Raw value where the kind carries one (reward value, milestone rank, badge count). */
  value?: number;
  /** 0..1 strength, which is what the emitters scale their counts by. */
  intensity: number;
  /** Viewer display name, for sugar. */
  by?: string;
  /** Feed event id when the trigger came from an event rather than a header delta. */
  eventId?: number;
  /** Where the trigger came from — an `events` entry, or a counter that moved on its own. */
  source: 'event' | 'header';
}

export type MomentPhase = 'entering' | 'holding' | 'leaving';

/** The moment on stage, as the renderer sees it. Rebuilt in place every `tick`, never allocated. */
export interface ActiveMoment {
  /** Monotonic per queue. Stable across coalesces, which is what makes it a React key. */
  id: number;
  type: MomentType;
  trigger: MomentTrigger;
  /** Equal-priority triggers folded in, including the first. */
  count: number;
  phase: MomentPhase;
  /** Raw linear progress through the current phase, 0..1. */
  linear: number;
  /** Eased progress through the current phase, 0..1 (past 1 mid-flight for an overshoot easing). */
  progress: number;
  /**
   * How present the moment is, 0 offstage to 1 fully on: the arrival eased up, 1 through the
   * hold, the exit eased back down. One number a renderer can multiply an opacity or an offset by
   * without knowing which phase it is in.
   */
  presence: number;
  startedMs: number;
  /** When the exit will have finished, given no pre-emption. */
  endsMs: number;
  elapsedMs: number;
}

/** What React renders. Identity changes only when something React can see changes. */
export interface MomentSnapshot {
  active: {
    id: number;
    type: MomentType;
    phase: MomentPhase;
    label: string;
    detail: string | null;
    count: number;
    intensity: number;
  } | null;
  pending: number;
  /** Bumps on every snapshot change, so a test can assert "notified once, not per frame". */
  version: number;
}

const EMPTY_SNAPSHOT: MomentSnapshot = { active: null, pending: 0, version: 0 };

export interface MomentQueueOptions {
  /** Default clock for calls that omit `nowMs`. Never used when `nowMs` is passed. */
  clock?: () => number;
  maxPending?: number;
  /** Scale every hold, for a demo or a screenshot run. 1 in production. */
  holdScale?: number;
}

interface QueuedMoment {
  id: number;
  trigger: MomentTrigger;
  count: number;
  /** Arrival order, to break ties a stable sort would otherwise leave to the engine. */
  seq: number;
}

/**
 * The queue.
 *
 * State machine, per moment: `entering` for `enterMs` → `holding` for `holdMs` → `leaving` for
 * `leaveMs` → off, then the highest-priority pending moment starts entering on the same frame, so
 * a queued moment never costs an idle frame.
 */
export class MomentQueue {
  private readonly clock: () => number;
  private readonly maxPending: number;
  private readonly holdScale: number;

  private pendingQueue: QueuedMoment[] = [];
  private current: QueuedMoment | null = null;
  private phase: MomentPhase = 'entering';
  private phaseStartMs = 0;
  private startedMs = 0;
  private holdMs = 0;
  private leaveMs = 0;
  private enter = new Tween({ durationMs: 0 });
  private leave = new Tween({ durationMs: 0 });

  private nextId = 0;
  private nextSeq = 0;
  private version = 0;
  private snapshot: MomentSnapshot = EMPTY_SNAPSHOT;
  private readonly listeners = new Set<() => void>();
  /** Reused object; the renderer reads it every frame and must not allocate one per frame. */
  private readonly view: ActiveMoment;

  constructor(options: MomentQueueOptions = {}) {
    this.clock = options.clock ?? (() => performance.now());
    this.maxPending = options.maxPending ?? DEFAULT_MAX_PENDING;
    this.holdScale = options.holdScale ?? 1;
    this.view = {
      id: 0,
      type: 'reward',
      trigger: { type: 'reward', label: '', intensity: 0, source: 'header' },
      count: 0,
      phase: 'entering',
      linear: 0,
      progress: 0,
      presence: 0,
      startedMs: 0,
      endsMs: 0,
      elapsedMs: 0,
    };
  }

  /**
   * Ask for a moment. Accepts a `MomentTrigger` or a raw `FeedEvent` (mapped by
   * `triggerFromEvent`, which returns null for the kinds that are not moments).
   *
   * Returns the id of the moment the request landed in — the new one, the active one it coalesced
   * into, the pending one it folded into — or null when it was dropped (an unmappable event, or a
   * full queue with nothing lower-priority to evict).
   */
  enqueue(input: MomentTrigger | FeedEvent, nowMs: number = this.clock()): number | null {
    const trigger = isFeedEvent(input) ? triggerFromEvent(input) : input;
    if (!trigger) return null;

    // Equal priority (= same type) and already on stage, still on the way in or holding: fold.
    // The hold clock restarts, which is what makes three sugars in a row read as one sustained
    // sugar rather than a stutter.
    if (this.current && this.current.trigger.type === trigger.type && this.phase !== 'leaving') {
      this.current.count += 1;
      this.current.trigger = trigger;
      if (this.phase === 'holding') this.phaseStartMs = nowMs;
      this.refreshSnapshot();
      return this.current.id;
    }

    // Same type already waiting: fold there instead of queueing a duplicate.
    const waiting = this.pendingQueue.find((item) => item.trigger.type === trigger.type);
    if (waiting) {
      waiting.count += 1;
      waiting.trigger = trigger;
      this.refreshSnapshot();
      return waiting.id;
    }

    this.nextId += 1;
    const queued: QueuedMoment = { id: this.nextId, trigger, count: 1, seq: this.nextSeq++ };

    if (!this.current) {
      this.begin(queued, nowMs);
      return queued.id;
    }

    // A bigger moment wins: cut the exit of what is on stage short and queue the newcomer, which
    // will start entering on the frame that exit finishes (at most PREEMPT_LEAVE_MS away).
    if (MOMENT_PRIORITY[trigger.type] > MOMENT_PRIORITY[this.current.trigger.type]) {
      this.preempt(nowMs);
      this.push(queued);
      return queued.id;
    }

    return this.push(queued) ? queued.id : null;
  }

  /** Advance the state machine to `nowMs`. Safe to call many times with the same value. */
  tick(nowMs: number = this.clock()): void {
    // A while loop, not an if: a modeChange's 200/0/200 ms is short enough that a dropped frame
    // can span a whole moment, and the queue behind it must still drain on this frame.
    for (let guard = 0; guard < MOMENT_TYPES.length + this.maxPending + 2; guard += 1) {
      if (!this.current) {
        const next = this.shift();
        // `break`, not `return`: the snapshot below is what tells React the stage went empty, and an
        // early return here left a finished moment in the snapshot forever (caught by the engine's
        // "only dirty while there is something to paint" test).
        if (!next) break;
        this.begin(next, nowMs);
        continue;
      }

      const inPhase = nowMs - this.phaseStartMs;
      if (this.phase === 'entering') {
        if (inPhase < this.enter.durationMs) break;
        this.phase = 'holding';
        this.phaseStartMs = this.phaseStartMs + this.enter.durationMs;
        continue;
      }
      if (this.phase === 'holding') {
        if (inPhase < this.holdMs) break;
        this.phase = 'leaving';
        this.phaseStartMs = this.phaseStartMs + this.holdMs;
        this.leave.start(this.phaseStartMs);
        continue;
      }
      if (inPhase < this.leaveMs) break;
      this.current = null;
      this.phase = 'entering';
    }
    this.refreshSnapshot();
  }

  /**
   * The moment on stage with its progress evaluated at `nowMs`, or null.
   *
   * The returned object is reused between calls: read it, do not keep it. Per-frame allocation is
   * exactly what the 4 ms paint budget in `docs/design/animation.md` cannot afford.
   */
  active(nowMs: number = this.clock()): ActiveMoment | null {
    const current = this.current;
    if (!current) return null;
    const timing = MOMENT_TIMING[current.trigger.type];
    const inPhase = Math.max(0, nowMs - this.phaseStartMs);

    let linear: number;
    let progress: number;
    let presence: number;
    if (this.phase === 'entering') {
      linear = this.enter.durationMs > 0 ? Math.min(1, inPhase / this.enter.durationMs) : 1;
      progress = this.enter.value(nowMs);
      presence = EASINGS[timing.enterEasing](linear);
    } else if (this.phase === 'holding') {
      linear = this.holdMs > 0 ? Math.min(1, inPhase / this.holdMs) : 1;
      progress = linear;
      presence = 1;
    } else {
      linear = this.leaveMs > 0 ? Math.min(1, inPhase / this.leaveMs) : 1;
      progress = this.leave.value(nowMs);
      presence = 1 - EASINGS[timing.leaveEasing](linear);
    }

    const view = this.view;
    view.id = current.id;
    view.type = current.trigger.type;
    view.trigger = current.trigger;
    view.count = current.count;
    view.phase = this.phase;
    view.linear = linear;
    view.progress = progress;
    view.presence = presence;
    view.startedMs = this.startedMs;
    // Measured from the *current* phase, not from the start, because a coalesce restarts the hold
    // clock and a pre-emption shortens the exit: both move the end.
    view.endsMs =
      this.phase === 'entering'
        ? this.phaseStartMs + this.enter.durationMs + this.holdMs + this.leaveMs
        : this.phase === 'holding'
          ? this.phaseStartMs + this.holdMs + this.leaveMs
          : this.phaseStartMs + this.leaveMs;
    view.elapsedMs = Math.max(0, nowMs - this.startedMs);
    return view;
  }

  /** How many moments are waiting. */
  get pendingCount(): number {
    return this.pendingQueue.length;
  }

  /** The waiting moments, highest priority first. For tests and the honesty panel. */
  pending(): readonly { id: number; type: MomentType; count: number }[] {
    return this.pendingQueue.map((item) => ({ id: item.id, type: item.trigger.type, count: item.count }));
  }

  /**
   * React binding. The listener fires when the *snapshot* changes — a new moment, a phase change,
   * a coalesce, a change in the queue depth — and never merely because a progress value moved:
   * React renders at 4 Hz here (`src/feed/store.ts`) and the canvas layer reads `active()` at 60.
   */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Stable-identity snapshot, for `useSyncExternalStore`. */
  getSnapshot(): MomentSnapshot {
    return this.snapshot;
  }

  /** Drop everything. Used by a fixture seek or a feed reconnect. */
  reset(): void {
    this.pendingQueue = [];
    this.current = null;
    this.phase = 'entering';
    this.refreshSnapshot();
  }

  private begin(queued: QueuedMoment, nowMs: number): void {
    const timing = MOMENT_TIMING[queued.trigger.type];
    this.current = queued;
    this.phase = 'entering';
    this.startedMs = nowMs;
    this.phaseStartMs = nowMs;
    this.holdMs = Math.max(0, timing.holdMs * this.holdScale);
    this.leaveMs = timing.leaveMs;
    this.enter = new Tween({ durationMs: timing.enterMs, easing: timing.enterEasing });
    this.leave = new Tween({ durationMs: timing.leaveMs, easing: timing.leaveEasing });
    this.enter.start(nowMs);
    this.leave.reset();
    this.refreshSnapshot();
  }

  /** Send what is on stage out now, over `PREEMPT_LEAVE_MS`, whatever phase it was in. */
  private preempt(nowMs: number): void {
    if (!this.current) return;
    this.phase = 'leaving';
    this.phaseStartMs = nowMs;
    this.leaveMs = Math.min(this.leaveMs, PREEMPT_LEAVE_MS);
    this.leave = new Tween({
      durationMs: this.leaveMs,
      easing: MOMENT_TIMING[this.current.trigger.type].leaveEasing,
    });
    this.leave.start(nowMs);
  }

  /** Insert into the pending queue by priority, then arrival. Returns false when dropped. */
  private push(queued: QueuedMoment): boolean {
    this.pendingQueue.push(queued);
    this.pendingQueue.sort(
      (a, b) =>
        MOMENT_PRIORITY[b.trigger.type] - MOMENT_PRIORITY[a.trigger.type] || a.seq - b.seq,
    );
    let kept = true;
    while (this.pendingQueue.length > this.maxPending) {
      // Evict the least important, most stale waiter. A queue this deep means the page is behind
      // on something a viewer stopped caring about several seconds ago.
      const dropped = this.pendingQueue.pop();
      if (dropped === queued) kept = false;
    }
    this.refreshSnapshot();
    return kept;
  }

  private shift(): QueuedMoment | null {
    return this.pendingQueue.shift() ?? null;
  }

  private refreshSnapshot(): void {
    const current = this.current;
    const previous = this.snapshot;
    const activeChanged =
      (current === null) !== (previous.active === null) ||
      (current !== null &&
        previous.active !== null &&
        (previous.active.id !== current.id ||
          previous.active.phase !== this.phase ||
          previous.active.count !== current.count ||
          previous.active.label !== current.trigger.label ||
          previous.active.detail !== (current.trigger.detail ?? null)));
    if (!activeChanged && previous.pending === this.pendingQueue.length) return;

    this.version += 1;
    this.snapshot = {
      active: current
        ? {
            id: current.id,
            type: current.trigger.type,
            phase: this.phase,
            label: current.trigger.label,
            detail: current.trigger.detail ?? null,
            count: current.count,
            intensity: current.trigger.intensity,
          }
        : null,
      pending: this.pendingQueue.length,
      version: this.version,
    };
    for (const listener of this.listeners) listener();
  }
}

// ---------------------------------------------------------------------------------------------
// Feed -> trigger mapping
// ---------------------------------------------------------------------------------------------

function isFeedEvent(value: MomentTrigger | FeedEvent): value is FeedEvent {
  return typeof (value as FeedEvent).kind === 'string';
}

/**
 * One `FeedEvent` to a moment, or null for the kinds that are not moments.
 *
 * `checkpoint` is deliberately silent — a 5 s heartbeat is not news, the same call
 * `src/lib/ticker.ts` makes with its `HIDDEN_KINDS`. `viewer` and `system` are ticker rows, not
 * moments: they say something about the broadcast, not about the fly.
 */
export function triggerFromEvent(event: FeedEvent): MomentTrigger | null {
  switch (event.kind) {
    case 'reward': {
      const value = event.value ?? 0;
      // A gym badge arrives as a reward event with `rewardKind: 'badge'` and value 1
      // (`packages/feed/src/fake/simulator.ts`), and it is the top of the priority list, not a
      // small reward.
      if (event.rewardKind === 'badge') {
        return { type: 'badge', label: event.label, value, intensity: 1, eventId: event.id, source: 'event' };
      }
      const tier = rewardTier(value);
      return {
        type: 'reward',
        label: event.label,
        detail: formatRewardValue(value),
        value,
        intensity: REWARD_TIER_INTENSITY[tier],
        eventId: event.id,
        source: 'event',
      };
    }
    case 'milestone':
      return {
        type: 'milestone',
        label: event.label,
        value: event.value,
        intensity: 1,
        eventId: event.id,
        source: 'event',
      };
    case 'recovery':
      return {
        type: 'rollback',
        label: event.label,
        detail: event.value === undefined ? undefined : `try ${String(event.value)}`,
        value: event.value,
        intensity: 1,
        eventId: event.id,
        source: 'event',
      };
    case 'sugar':
      return {
        type: 'sugar',
        label: event.label,
        detail: event.by,
        value: event.value,
        intensity: 1,
        by: event.by,
        eventId: event.id,
        source: 'event',
      };
    default:
      return null;
  }
}

/** Reward value as the caption shows it: two decimals, signed. */
export function formatRewardValue(value: number): string {
  return `${value >= 0 ? '+' : '-'}${Math.abs(value).toFixed(2)}`;
}

/** Simulated seconds in a day, for the day counter. */
export const DAY_SECONDS = 86_400;

/** Day number from a header, 1-based. `runSeconds` where there is one, else service uptime. */
export function dayNumber(header: FeedHeader): number {
  const seconds = header.runSeconds > 0 ? header.runSeconds : header.uptimeSeconds;
  return Math.floor(Math.max(0, seconds) / DAY_SECONDS) + 1;
}

/** The header fields a trigger can be derived from by comparing two snapshots. */
interface HeaderWatch {
  rank: number;
  badges: number;
  attempts: number;
  sugarCount: number;
  mode: GameMode;
  day: number;
}

/**
 * Header deltas to moments, remembering the previous snapshot.
 *
 * Why deltas at all, when the feed carries events: `docs/feed-protocol.md` says events are
 * "usually empty" and the service may *drop* snapshots rather than queue them, so a badge can
 * arrive as a changed count with its event lost — the same reasoning behind
 * `FeedIngest.considerStateMoments` in `src/feed/store.ts`. Watching both is the difference
 * between a fanfare and a silent badge.
 *
 * The flip side is double-firing, so `observe` reads the events *first* and suppresses any delta
 * of a type an event already covered in the same snapshot.
 *
 * The first `observe` after construction or `reset` emits nothing but the events it was given: a
 * page that connects to a run already on rank 7 with 3 badges has not just earned them.
 */
export class TriggerMapper {
  private last: HeaderWatch | null = null;

  reset(): void {
    this.last = null;
  }

  /** Every moment one snapshot asks for, events and deltas, in priority-neutral arrival order. */
  observe(header: FeedHeader): MomentTrigger[] {
    const triggers: MomentTrigger[] = [];
    const covered = new Set<MomentType>();

    for (const event of header.events) {
      const trigger = triggerFromEvent(event);
      if (!trigger) continue;
      triggers.push(trigger);
      covered.add(trigger.type);
    }

    const next: HeaderWatch = {
      rank: header.milestone.rank,
      badges: header.game.badges,
      attempts: header.milestone.attempts,
      sugarCount: header.sugar.todayCount,
      mode: header.game.mode,
      day: dayNumber(header),
    };
    const last = this.last;
    this.last = next;
    if (!last) return triggers;

    if (!covered.has('badge') && next.badges > last.badges) {
      triggers.push({
        type: 'badge',
        label: `Badge ${String(next.badges)}`,
        detail: `${String(next.badges)} of 8`,
        value: next.badges,
        intensity: 1,
        source: 'header',
      });
    }

    if (!covered.has('milestone') && next.rank > last.rank) {
      triggers.push({
        type: 'milestone',
        label: header.milestone.label,
        detail: `rung ${String(next.rank)}`,
        value: next.rank,
        intensity: 1,
        source: 'header',
      });
    }

    // A rollback is what the ratchet does when the run gets stuck: the try count goes up and the
    // game is restored from the archived frame (`docs/design/ladder.md`).
    if (!covered.has('rollback') && next.attempts > last.attempts) {
      triggers.push({
        type: 'rollback',
        label: 'REWIND',
        detail: `try ${String(next.attempts)}`,
        value: next.attempts,
        intensity: 1,
        source: 'header',
      });
    }

    if (!covered.has('sugar') && next.sugarCount > last.sugarCount) {
      triggers.push({
        type: 'sugar',
        label: 'SUGAR',
        detail: header.sugar.lastBy ?? undefined,
        by: header.sugar.lastBy ?? undefined,
        intensity: 1,
        source: 'header',
      });
    }

    if (next.day > last.day) {
      triggers.push({
        type: 'dayRollover',
        label: `DAY ${String(next.day)}`,
        value: next.day,
        intensity: 0.6,
        source: 'header',
      });
    }

    // `UNKNOWN` is the adapter saying it cannot classify the frame, which is not a mode change a
    // viewer should be told about; the chip keeps the last real mode.
    if (next.mode !== last.mode && next.mode !== 'UNKNOWN' && last.mode !== 'UNKNOWN') {
      triggers.push({
        type: 'modeChange',
        label: next.mode,
        detail: last.mode,
        intensity: 0.3,
        source: 'header',
      });
    }

    return triggers;
  }
}
