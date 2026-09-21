/**
 * Which notice, if any, this process start owes chat — and the rate limit that stops a restart
 * loop from becoming a spam loop.
 *
 * `docs/design/stage-bridge.md` B2 gives the bridge a startup notice, and `src/index.ts` posted
 * one unconditionally. That was fine while a restart meant a human had typed one. Now
 * `src/subscription-health.ts` exits the process on its own to recover a dead chat subscription,
 * so a bad half hour on Twitch's side could put a dozen identical "flybridge is online" lines in
 * chat. The deliverable is one notice per ten minutes, at most, across restarts — which means the
 * clock cannot live in the process. It lives in a small JSON file in the same
 * `StateDirectory=flybridge` as `tokens.json` and the redemption intent log.
 *
 * The same file carries the recovery marker: the self-healing exit path writes
 * `pendingRecovery` before it calls `process.exit`, and the next start that is allowed to post
 * says "reconnected" instead of "online". That is the only way the new process can know it is a
 * recovery — the exit and the restart share nothing else, and systemd's `NRestarts` was 0 during
 * the very incident that motivated this, so it is not a source either.
 */
import { readFile } from 'node:fs/promises';
import { atomicWriteJson } from './atomic-file';
import type { Clock } from './ratelimit';
import { systemClock } from './ratelimit';

/** The notices this gate can authorise. Both are `TemplateId`s in `src/templates.ts`. */
export type NoticeKind = 'startup' | 'recovered';

export interface PendingRecovery {
  atIso: string;
  /** The first loss reason from `ChatSubscriptionLostReport`, for the journal and for `/health`. */
  reason: string;
}

export interface NoticeState {
  /** Epoch ms of the last notice actually put in chat, or `null` if none ever was. */
  lastPostedAtMs: number | null;
  /** Written by the self-healing exit, consumed by the next notice that posts. */
  pendingRecovery: PendingRecovery | null;
}

const EMPTY_STATE: NoticeState = { lastPostedAtMs: null, pendingRecovery: null };

/** Default minimum gap between any two notices. `NOTICE_MIN_INTERVAL_MS`. */
export const DEFAULT_NOTICE_MIN_INTERVAL_MS = 10 * 60 * 1000;

/**
 * A recovery marker older than this is reported as a plain startup instead.
 *
 * Without the bound, a self-heal at 02:00 followed by a hand-typed restart at 09:00 would tell
 * chat the bridge had just reconnected, which is false. Not an env knob: there is no operator
 * decision in it.
 */
export const RECOVERY_NOTICE_MAX_AGE_MS = 60 * 60 * 1000;

export interface NoticeLogOptions {
  path: string;
  minIntervalMs: number;
  /**
   * `FEATURE_QUIET`: when true, `decide()` authorises nothing at all, so neither the startup nor
   * the recovery notice is ever posted (the operator 2026-09-17: the bridge "doesn't greet"). The gate
   * lives here rather than in `src/index.ts` so the suppression is unit-testable and so the
   * recovery marker is left in the state file instead of being consumed unread.
   */
  quiet?: boolean;
  clock?: Clock;
  /** Test seams. Default to `node:fs/promises` and `src/atomic-file.ts`. */
  readJson?: (path: string) => Promise<string>;
  writeJson?: (path: string, data: unknown) => Promise<void>;
}

/** What `decide()` concluded, including the reason it concluded nothing, for the journal. */
export type NoticeDecision =
  | { post: NoticeKind; recovery: PendingRecovery | null }
  /** `quiet` distinguishes "silenced by FEATURE_QUIET" from "too soon since the last notice". */
  | { post: null; suppressedForMs: number; quiet?: boolean };

export class NoticeLog {
  private readonly path: string;
  private readonly minIntervalMs: number;
  private readonly quiet: boolean;
  private readonly clock: Clock;
  private readonly readJson: (path: string) => Promise<string>;
  private readonly writeJson: (path: string, data: unknown) => Promise<void>;
  private state: NoticeState = EMPTY_STATE;

  constructor(options: NoticeLogOptions) {
    this.path = options.path;
    this.minIntervalMs = options.minIntervalMs;
    this.quiet = options.quiet ?? false;
    this.clock = options.clock ?? systemClock;
    this.readJson = options.readJson ?? ((path) => readFile(path, 'utf8'));
    this.writeJson = options.writeJson ?? atomicWriteJson;
  }

  /**
   * Read the state file. A missing or corrupt file is not an error: the bridge must start and
   * post its notice on a fresh container, and a truncated file is worth one log line and a clean
   * slate, never a refusal to run the chat bridge.
   */
  async load(): Promise<void> {
    let raw: string;
    try {
      raw = await this.readJson(this.path);
    } catch {
      this.state = EMPTY_STATE;
      return;
    }
    try {
      const parsed = JSON.parse(raw) as Partial<NoticeState>;
      this.state = {
        lastPostedAtMs: typeof parsed.lastPostedAtMs === 'number' ? parsed.lastPostedAtMs : null,
        pendingRecovery: isPendingRecovery(parsed.pendingRecovery) ? parsed.pendingRecovery : null,
      };
    } catch {
      console.error(`flybridge: ${this.path} is not valid JSON; treating the notice log as empty`);
      this.state = EMPTY_STATE;
    }
  }

  /** The loaded state, for `/health` and for tests. */
  get current(): NoticeState {
    return this.state;
  }

  decide(): NoticeDecision {
    if (this.quiet) return { post: null, suppressedForMs: 0, quiet: true };
    const now = this.clock.now();
    const last = this.state.lastPostedAtMs;
    if (last !== null && now - last < this.minIntervalMs) {
      return { post: null, suppressedForMs: this.minIntervalMs - (now - last) };
    }
    const recovery = this.state.pendingRecovery;
    if (recovery !== null && now - Date.parse(recovery.atIso) <= RECOVERY_NOTICE_MAX_AGE_MS) {
      return { post: 'recovered', recovery };
    }
    return { post: 'startup', recovery: null };
  }

  /** Record that a notice reached chat: starts the ten-minute clock and consumes the marker. */
  async markPosted(): Promise<void> {
    this.state = { lastPostedAtMs: this.clock.now(), pendingRecovery: null };
    await this.writeJson(this.path, this.state);
  }

  /**
   * Record a self-healing exit, so the next start that is allowed to speak says "reconnected".
   * Called from the `onUnhealthy` path in `src/index.ts` immediately before `process.exit`, which
   * is why it is the only write here that must be awaited before the process goes away.
   */
  async markSelfHealExit(reason: string): Promise<void> {
    this.state = {
      lastPostedAtMs: this.state.lastPostedAtMs,
      pendingRecovery: { atIso: new Date(this.clock.now()).toISOString(), reason },
    };
    await this.writeJson(this.path, this.state);
  }
}

function isPendingRecovery(value: unknown): value is PendingRecovery {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Partial<PendingRecovery>;
  return typeof candidate.atIso === 'string' && typeof candidate.reason === 'string';
}
