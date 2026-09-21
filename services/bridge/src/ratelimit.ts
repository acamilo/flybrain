/**
 * Token-bucket rate limiting for chat commands (`docs/design/stage-bridge.md` B2).
 *
 * Three independent limits, all must pass for a command to fire:
 *  - per-user, per-command: one allowed every `perUserPerCommandMs` (default 60 s).
 *  - global reply: at most one reply every `globalReplyMs` across all commands (default 5 s).
 *  - per-command global cooldown: a command-specific floor before *anyone* can trigger it again
 *    (used for `!sugar`, which also consults the sim's own cooldown so the answer stays truthful
 *    — see `src/commands.ts`).
 *
 * Time comes from an injected `Clock` rather than real timers, so tests are deterministic without
 * `setTimeout`/`setInterval` mocking (`tests/ratelimit.test.ts` drives a `FakeClock`).
 */
import type { RateLimitSettings } from './config';

export interface Clock {
  now(): number;
}

export const systemClock: Clock = { now: () => Date.now() };

/** A controllable clock for tests. Time only moves when `advance` is called. */
export class FakeClock implements Clock {
  private currentMs: number;

  constructor(startMs = 0) {
    this.currentMs = startMs;
  }

  now(): number {
    return this.currentMs;
  }

  advance(ms: number): void {
    this.currentMs += ms;
  }
}

export type RateLimitReason = 'user' | 'global' | 'cooldown';

export type RateLimitDecision = { allowed: true } | { allowed: false; reason: RateLimitReason; retryAfterMs: number };

export class RateLimiter {
  private readonly userLast = new Map<string, number>();
  private readonly commandLast = new Map<string, number>();
  private globalLast = -Infinity;

  constructor(
    private readonly settings: RateLimitSettings,
    private readonly clock: Clock = systemClock,
  ) {}

  /**
   * Check whether `userId` may run `command` right now, without recording anything. Callers that
   * only want to check (e.g. `!sugar` truthfully reporting a cooldown before also asking the sim)
   * should use this; `tryConsume` both checks and records atomically.
   */
  check(userId: string, command: string): RateLimitDecision {
    const now = this.clock.now();

    const cooldownMs = this.settings.perCommandCooldownMs[command] ?? 0;
    if (cooldownMs > 0) {
      const lastCommand = this.commandLast.get(command);
      if (lastCommand !== undefined) {
        const retryAfterMs = lastCommand + cooldownMs - now;
        if (retryAfterMs > 0) return { allowed: false, reason: 'cooldown', retryAfterMs };
      }
    }

    const userKey = `${userId}:${command}`;
    const lastUser = this.userLast.get(userKey);
    if (lastUser !== undefined) {
      const retryAfterMs = lastUser + this.settings.perUserPerCommandMs - now;
      if (retryAfterMs > 0) return { allowed: false, reason: 'user', retryAfterMs };
    }

    const retryAfterGlobalMs = this.globalLast + this.settings.globalReplyMs - now;
    if (retryAfterGlobalMs > 0) return { allowed: false, reason: 'global', retryAfterMs: retryAfterGlobalMs };

    return { allowed: true };
  }

  /** Record that `userId` ran `command` at the current clock time, consuming all three buckets. */
  record(userId: string, command: string): void {
    const now = this.clock.now();
    this.userLast.set(`${userId}:${command}`, now);
    this.commandLast.set(command, now);
    this.globalLast = now;
  }

  /** Check and, if allowed, record in one call. */
  tryConsume(userId: string, command: string): RateLimitDecision {
    const decision = this.check(userId, command);
    if (decision.allowed) this.record(userId, command);
    return decision;
  }
}
