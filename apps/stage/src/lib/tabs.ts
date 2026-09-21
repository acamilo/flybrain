/**
 * Which tab the slot is showing, and why (`docs/stream-mvp-plan.md`, "Rail layout v2": "three
 * tabs auto-cycling … slow cadence steered by activity, big moments pre-empt for 9 s").
 *
 * Five tabs since 2026-09-17: DESCRIBE joined the rotation (`docs/design/describe-tab.md`) and
 * MACROS joined it with section 14's decided layout (`docs/design/macros.md`), which put the fly's
 * whole thirty-one-button keyboard on the rail because it does not fit under the game at the 24 px
 * floor. Both change the cadence in one way only — a full turn of the rotation is five dwells
 * rather than three — because nothing steers to either and no moment focuses them.
 *
 * Three inputs, in descending authority:
 *
 *   1. **Focus.** A moment takes the slot for its hold and then hands it back to whatever was up
 *      before. This is what replaced layout v1's promotion of the brain map over the rail.
 *   2. **Event steering.** A rung change goes to LADDER and a reward goes to CONNECTOME, because
 *      those are the tabs that show the thing that just happened. Gated by `STEER_DWELL_MS` so a
 *      reward every twenty seconds cannot make the slot flicker.
 *   3. **The cycle.** Otherwise the slot advances on its own every 45 to 60 s. Sustained
 *      steering — "SENSES while walking with high command activity" — is applied *at* a cycle
 *      boundary rather than immediately, for the same reason: a condition that is true for
 *      minutes at a time must bias the rotation, not own it.
 *
 * Pure and clock-injected: no timers, no `Math.random`, so `tests/unit/tabs.test.ts` drives the
 * whole state machine and a fixture seek reproduces the same tab at the same virtual time.
 */

export type TabId = 'senses' | 'connectome' | 'ladder' | 'describe' | 'macros';

/**
 * Rotation order, and the order the tab strip renders.
 *
 * DESCRIBE is fourth (`docs/design/describe-tab.md`: "Fourth tab on the rail after
 * SENSES / CONNECTOME / LADDER") and MACROS last. Neither is steered to and no moment focuses
 * either: DESCRIBE says what the stream *is*, which is never what just happened, and MACROS says
 * what the fly *can* press, which changes with the scene rather than with an event — the button
 * that fired is already lit under the game. So both arrive only on the cycle's own turn.
 */
export const TABS: readonly TabId[] = ['senses', 'connectome', 'ladder', 'describe', 'macros'];

/** On-screen names. Caps are applied by CSS, not here. */
export const TAB_LABELS: Record<TabId, string> = {
  senses: 'senses',
  connectome: 'connectome',
  ladder: 'ladder',
  describe: 'describe',
  macros: 'macros',
};

/** Auto-cycle bounds from the locked layout. */
export const CYCLE_MIN_MS = 45_000;
export const CYCLE_MAX_MS = 60_000;

/** No steer may interrupt a tab that has been up for less than this. */
export const STEER_DWELL_MS = 12_000;

/**
 * The new-chatter switch, both numbers, in the one place that owns cadence.
 *
 * The operator, 2026-09-17: "when new user joins chat, switch to it for a few sec, cool down timer." A
 * display name nobody on this page has seen before takes the slot to DESCRIBE for
 * `NEW_CHATTER_HOLD_MS`, then hands it back to whatever was up — the same focus-and-return a big
 * moment uses, so it is the rail's existing tab-change motion and not a second kind of switch.
 *
 * `NEW_CHATTER_COOLDOWN_MS` is why it is watchable rather than annoying: a raid or a busy evening
 * is a dozen new names a minute, and without a cooldown the slot would sit on DESCRIBE and the
 * rest of the rail would never be seen. Two minutes means at most one interruption per rotation
 * and a half. Names that arrive inside the cooldown still count as seen
 * (`src/lib/chatters.ts`), so the switch is "somebody new turned up recently", not a queue.
 *
 * Documented in `docs/design/describe-tab.md` (2026-09-17). Whoever tunes these tunes them here.
 */
export const NEW_CHATTER_HOLD_MS = 8_000;
export const NEW_CHATTER_COOLDOWN_MS = 120_000;

/**
 * Command-channel activity above which, while walking, SENSES is the interesting tab.
 *
 * The signal is the mean bar fill of the drive group, which sits near `1 / headroom` (about 0.67)
 * at rest (`src/lib/circuit-scale.ts`), so the threshold has to be above that to mean anything.
 */
export const WALKING_ACTIVITY = 0.72;

/** Why the slot is showing what it is showing. Surfaced on `window.__stage` for the tests. */
export type TabReason = 'seed' | 'cycle' | 'steer' | 'focus' | 'return' | 'forced';

/** One frame of the world, as the controller sees it. */
export interface TabSignals {
  /** True while the adapter reports the walking/overworld mode. */
  walking: boolean;
  /** 0..1 mean fill of the drive bars. */
  commandActivity: number;
  /** A reward event landed since the last update. */
  reward: boolean;
  /** The milestone rank changed since the last update. */
  rankChanged: boolean;
  /**
   * A chat line from a display name this page had not seen before landed since the last update
   * (`src/lib/chatters.ts` decides what counts). Optional so a caller that does not watch chat —
   * `tests/unit/tabs.test.ts`'s older cases, and any future one — reads as "no".
   */
  newChatter?: boolean;
}

export interface TabControllerOptions {
  /** Start here. Defaults to SENSES. */
  initial?: TabId;
  /** Pin the slot and disable cycling and steering (`?tab=`). */
  forced?: TabId | null;
  minMs?: number;
  maxMs?: number;
  steerDwellMs?: number;
  /** How long a new chatter holds DESCRIBE. Defaults to `NEW_CHATTER_HOLD_MS`. */
  newChatterHoldMs?: number;
  /** How long after a switch further new names are ignored. Defaults to `NEW_CHATTER_COOLDOWN_MS`. */
  newChatterCooldownMs?: number;
  /** Seed for the dwell sequence. Same seed, same cadence — which is what makes it testable. */
  seed?: number;
}

export class TabController {
  private tab: TabId;
  private reasonValue: TabReason;
  private changedAtMs: number | null = null;
  private dwellMs: number;
  private focus: { tab: TabId; untilMs: number; returnTo: TabId } | null = null;
  private seed: number;
  /** Clock value of the last new-chatter switch; -Infinity means "never". */
  private chatterSwitchMs = Number.NEGATIVE_INFINITY;
  private readonly minMs: number;
  private readonly maxMs: number;
  private readonly steerDwellMs: number;
  private readonly holdMs: number;
  private readonly cooldownMs: number;
  private readonly forced: TabId | null;

  constructor(options: TabControllerOptions = {}) {
    this.forced = options.forced ?? null;
    this.tab = this.forced ?? options.initial ?? 'senses';
    this.reasonValue = this.forced ? 'forced' : 'seed';
    this.minMs = options.minMs ?? CYCLE_MIN_MS;
    this.maxMs = options.maxMs ?? CYCLE_MAX_MS;
    this.steerDwellMs = options.steerDwellMs ?? STEER_DWELL_MS;
    this.holdMs = options.newChatterHoldMs ?? NEW_CHATTER_HOLD_MS;
    this.cooldownMs = options.newChatterCooldownMs ?? NEW_CHATTER_COOLDOWN_MS;
    this.seed = (options.seed ?? 20260915) | 0;
    this.dwellMs = this.nextDwell();
  }

  get current(): TabId {
    return this.tab;
  }

  get reason(): TabReason {
    return this.reasonValue;
  }

  /** Clock value of the last change, for the crossfade. */
  get changedAt(): number | null {
    return this.changedAtMs;
  }

  /** True while a moment owns the slot. */
  get focused(): boolean {
    return this.focus !== null;
  }

  /** True when the slot is pinned by `?tab=`. */
  get isForced(): boolean {
    return this.forced !== null;
  }

  /** Clock value of the last new-chatter switch, or null before the first. For `window.__stage`. */
  get lastChatterSwitch(): number | null {
    return this.chatterSwitchMs === Number.NEGATIVE_INFINITY ? null : this.chatterSwitchMs;
  }

  /**
   * Give `tab` the slot until `untilMs`, then hand it back.
   *
   * Idempotent for the same tab, and that is load-bearing: the director calls this on *every*
   * frame a moment is on stage, so a version that re-`set`s each time would reset `changedAt`
   * sixty times a second and the crossfade would never get past its first frame. Extending the
   * deadline is all a repeat call may do.
   */
  focusOn(tab: TabId, untilMs: number, nowMs: number): void {
    if (this.forced) return;
    const focus = this.focus;
    if (focus && focus.tab === tab) {
      focus.untilMs = Math.max(focus.untilMs, untilMs);
      return;
    }
    const returnTo = focus?.returnTo ?? this.tab;
    this.focus = { tab, untilMs, returnTo };
    this.set(tab, nowMs, 'focus');
  }

  /** Advance one frame. Returns the tab to show. */
  update(nowMs: number, signals: TabSignals): TabId {
    if (this.changedAtMs === null) this.changedAtMs = nowMs;
    if (this.forced) return this.tab;

    // A new chatter, before the focus check: it *becomes* a focus, on the same machinery a moment
    // uses, so the hold expires and the slot returns to the tab that was up. Never while a moment
    // already owns the slot — a badge or a rollback is the more interesting thing on screen and
    // that hold is short — and the cooldown is not spent in that case either, so the next new
    // name after the moment still gets its switch.
    if (
      signals.newChatter === true &&
      this.focus === null &&
      nowMs - this.chatterSwitchMs >= this.cooldownMs
    ) {
      this.chatterSwitchMs = nowMs;
      this.focusOn('describe', nowMs + this.holdMs, nowMs);
      return this.tab;
    }

    const focus = this.focus;
    if (focus) {
      if (nowMs < focus.untilMs) {
        if (this.tab !== focus.tab) this.set(focus.tab, nowMs, 'focus');
        return this.tab;
      }
      this.focus = null;
      this.set(focus.returnTo, nowMs, 'return');
      return this.tab;
    }

    const steered = this.steerTarget(signals);
    if (steered && steered !== this.tab && nowMs - this.changedAtMs >= this.steerDwellMs) {
      this.set(steered, nowMs, 'steer');
      return this.tab;
    }

    if (nowMs - this.changedAtMs >= this.dwellMs) {
      this.set(this.nextTab(signals), nowMs, 'cycle');
      this.dwellMs = this.nextDwell();
    }
    return this.tab;
  }

  /** The event steer for this frame, or null. Rung change out-ranks a reward. */
  private steerTarget(signals: TabSignals): TabId | null {
    if (signals.rankChanged) return 'ladder';
    if (signals.reward) return 'connectome';
    return null;
  }

  /**
   * Where the cycle goes next: the sustained bias if it applies and is not already up, else the
   * next tab in rotation.
   */
  private nextTab(signals: TabSignals): TabId {
    if (signals.walking && signals.commandActivity >= WALKING_ACTIVITY && this.tab !== 'senses') {
      return 'senses';
    }
    const index = TABS.indexOf(this.tab);
    return TABS[(index + 1) % TABS.length] as TabId;
  }

  private set(tab: TabId, nowMs: number, reason: TabReason): void {
    this.tab = tab;
    this.reasonValue = reason;
    this.changedAtMs = nowMs;
  }

  /** A dwell in `[minMs, maxMs]`, from the seeded sequence. */
  private nextDwell(): number {
    this.seed = (this.seed + 0x6d2b79f5) | 0;
    let t = this.seed;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    const unit = ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    return this.minMs + unit * (this.maxMs - this.minMs);
  }
}

/** Parse a `?tab=` value. Anything unrecognised means "do not pin", never a crash on air. */
export function parseTab(value: string | null | undefined): TabId | null {
  if (!value) return null;
  return (TABS as readonly string[]).includes(value) ? (value as TabId) : null;
}
