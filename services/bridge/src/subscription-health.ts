/**
 * "Is chat actually working?" — the one invariant flybridge cannot be allowed to lose silently.
 *
 * ## The incident this exists for (the release container, 2026-09-16 11:45:34 UTC, v0.1.2/v0.1.3)
 *
 * Both EventSub websockets (the bot listener and the broadcaster listener, which on the first
 * live channel are the SAME Twitch account) dropped with code 1006. twurple reconnected at once
 * and, on `session_welcome`, re-created every subscription for that user — which is correct, and
 * which is also where it ended. Twitch answered the `channel.chat.message` creation with
 *
 *     HTTP 429 {"error":"Too Many Requests","status":429,
 *               "message":"number of websocket transports limit exceeded"}
 *
 * twice (11:45:35 and 11:45:45), because the two stale transports from one account had not been
 * reaped yet and two fresh ones from the same account doubled the count against the per-user
 * websocket transport limit of 3. The bot socket then closed with 4003 "connection unused". After
 * that the process stayed up, `systemctl status` said `active (running)` with `NRestarts=0`, and
 * the bridge had NO chat subscription at all for an hour — on-stream chat and every command dead
 * — until a hand-typed `systemctl restart flybridge` at 12:47:13, after which the subscriptions
 * were created inside a second.
 *
 * ## Why twurple does not recover on its own
 *
 * `EventSubSubscription._subscribeAndSave()` (@twurple/eventsub-base 8.1.4) fires the create
 * request, and on rejection logs it and emits `onSubscriptionCreateFailure`. There is no retry,
 * no backoff and no further attempt: the subscription object simply stays unsubscribed for the
 * life of the process. Nothing in the library turns that into a non-zero exit, and nothing in
 * `/health` was watching it either, so systemd — the only thing that could have fixed it — was
 * never told.
 *
 * ## What this module does
 *
 * Tracks one fact: has the `channel.chat.message` subscription been CONFIRMED CREATED since the
 * last time it was lost? Arm the grace timer whenever it is lost (socket disconnect, create
 * failure, revocation, or simply "startup, not created yet"); disarm it on confirmation. If the
 * grace period (default 60 s, `EVENTSUB_GRACE_MS`) elapses with no confirmation, log ONE line
 * naming every reason and hand control back to systemd by exiting non-zero.
 *
 * Exiting is the recovery, not a surrender: a fresh process gets a fresh websocket transport, and
 * `infra/units/flybridge.service`'s `RestartSec=15` is long enough for Twitch to have reaped the
 * stale ones — which is also the 429 backoff asked for, since retrying the create in-process
 * against a limit that only time can clear would just burn the same error on a tighter loop.
 * `StartLimitIntervalSec=0` on the unit keeps repeated exits from parking it.
 *
 * ## Why the twurple hooks and not the Helix API
 *
 * `onSubscriptionCreateSuccess` / `onSubscriptionCreateFailure` / `onUserSocketDisconnect` /
 * `onRevoke` already say everything needed, so there is no `GET eventsub/subscriptions` poll
 * here. Two traps in that library worth writing down:
 *
 *  - `EventSubSubscription.verified` is USELESS on a websocket listener. `_verify()` is only
 *    called by the webhook/conduit listeners, which are not in `@twurple/eventsub-ws` at all, so
 *    `verified` is permanently `false` for every WS subscription. Confirming from it would have
 *    exited a perfectly healthy bridge every 60 s.
 *  - `onSubscriptionActivate` is NOT a confirmation either: `_subscribeAndSave()` emits it
 *    BEFORE it sends the create request, so it fires on the way into the 429 as well.
 */
import type { Clock } from './ratelimit';
import { systemClock } from './ratelimit';

/**
 * Exit code used when chat is confirmed dead. 75 is sysexits.h's `EX_TEMPFAIL`: "the request
 * could not be completed, try again later", which is exactly the contract with systemd here.
 * Non-zero is the part that matters (`Restart=always` restarts on a clean exit too, but a
 * distinct code makes `systemctl status` and the journal say why).
 */
export const CHAT_SUBSCRIPTION_LOST_EXIT_CODE = 75;

/** Default grace period before a missing chat subscription becomes an exit. `EVENTSUB_GRACE_MS`. */
export const DEFAULT_EVENTSUB_GRACE_MS = 60_000;

/** How many distinct loss reasons the report keeps, so one flapping socket cannot grow the log line. */
const MAX_REPORTED_REASONS = 8;

/**
 * The timer seam. Tests pass a manual implementation instead of mocking global timers, matching
 * how `src/ratelimit.ts` injects a `Clock`.
 */
export interface Timers {
  set(callback: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

export const systemTimers: Timers = {
  set: (callback, ms) => setTimeout(callback, ms),
  clear: (handle) => {
    clearTimeout(handle as ReturnType<typeof setTimeout>);
  },
};

/** What `onUnhealthy` is handed: everything the one log line and the runbook need. */
export interface ChatSubscriptionLostReport {
  graceMs: number;
  /** Distinct loss reasons in the order they were seen, oldest first. */
  reasons: readonly string[];
  /** True if any create failure was Twitch's per-user websocket transport limit (the 2026-09-16 case). */
  transportLimitHit: boolean;
  /** Wall-clock ms of the first loss in this armed window. */
  lostSinceMs: number;
}

export interface ChatSubscriptionHealthOptions {
  /** Grace period, ms. `BridgeConfig.eventsubGraceMs`. */
  graceMs: number;
  /**
   * Called exactly once, when the grace period has elapsed with no confirmation. Production
   * passes a function that persists the recovery marker and then `process.exit`s; tests pass a
   * recorder.
   */
  onUnhealthy: (report: ChatSubscriptionLostReport) => void;
  timers?: Timers;
  clock?: Clock;
  /** Where the one clear line goes. Defaults to `console.error`. */
  log?: (line: string) => void;
}

/**
 * The `channel.chat.message` subscription's health, as a small state machine with one timer.
 *
 * Deliberately knows nothing about twurple: `src/eventsub.ts` translates the library's events
 * into `noteConfirmed` / `noteLost` / `noteCreateFailure`, which is what makes this unit-testable
 * without a Twitch connection.
 */
export class ChatSubscriptionHealth {
  private readonly graceMs: number;
  private readonly timers: Timers;
  private readonly clock: Clock;
  private readonly log: (line: string) => void;
  private readonly onUnhealthy: (report: ChatSubscriptionLostReport) => void;

  private confirmed = false;
  private timerHandle: unknown = null;
  private reasons: string[] = [];
  private transportLimitHit = false;
  private lostSinceMs = 0;
  /** Set once `onUnhealthy` has fired, so a flapping socket cannot fire it twice. */
  private tripped = false;
  /** Set by `stop()` during shutdown: a deliberate teardown must not look like a dead channel. */
  private stopped = false;

  constructor(options: ChatSubscriptionHealthOptions) {
    this.graceMs = options.graceMs;
    this.timers = options.timers ?? systemTimers;
    this.clock = options.clock ?? systemClock;
    this.log = options.log ?? ((line) => console.error(line));
    this.onUnhealthy = options.onUnhealthy;
  }

  /** Whether the chat subscription is currently believed to be live. `/health` reports this. */
  get healthy(): boolean {
    return this.confirmed;
  }

  /** Whether the grace timer is currently running. */
  get armed(): boolean {
    return this.timerHandle !== null;
  }

  /**
   * Twitch accepted the `channel.chat.message` subscription. Disarms the grace timer.
   *
   * The only confirmation source: `EventSubBase.onSubscriptionCreateSuccess`, which fires from
   * `_registerTwitchSubscription` after the create request resolved.
   */
  noteConfirmed(): void {
    if (this.stopped) return;
    this.confirmed = true;
    this.reasons = [];
    this.transportLimitHit = false;
    this.disarm();
  }

  /**
   * The chat subscription is not (or is no longer) known to be live. Arms the grace timer.
   *
   * The timer is NOT restarted if it is already running: the grace is measured from the FIRST
   * loss, because a socket that disconnects every 20 s would otherwise postpone recovery forever
   * while chat stayed dead the whole time.
   */
  noteLost(reason: string): void {
    if (this.stopped || this.tripped) return;
    this.confirmed = false;
    if (this.reasons.length < MAX_REPORTED_REASONS && !this.reasons.includes(reason)) {
      this.reasons.push(reason);
    }
    if (this.timerHandle !== null) return;
    this.lostSinceMs = this.clock.now();
    this.timerHandle = this.timers.set(() => {
      this.timerHandle = null;
      this.trip();
    }, this.graceMs);
  }

  /**
   * A subscription create attempt failed. Records the 429 transport-limit case specially — it is
   * the one failure that time, and only time, fixes, so it is worth naming in the exit line and
   * counting in `/metrics`.
   */
  noteCreateFailure(error: unknown): void {
    const transportLimit = isWebsocketTransportLimitError(error);
    if (transportLimit) this.transportLimitHit = true;
    this.noteLost(
      transportLimit
        ? 'create failed: HTTP 429, Twitch per-user websocket transport limit exceeded'
        : `create failed: ${errorMessage(error)}`,
    );
  }

  /** Shutdown. Cancels the grace timer so SIGTERM does not race an exit-75. */
  stop(): void {
    this.stopped = true;
    this.disarm();
  }

  private disarm(): void {
    if (this.timerHandle === null) return;
    this.timers.clear(this.timerHandle);
    this.timerHandle = null;
  }

  private trip(): void {
    if (this.stopped || this.confirmed || this.tripped) return;
    this.tripped = true;
    const report: ChatSubscriptionLostReport = {
      graceMs: this.graceMs,
      reasons: [...this.reasons],
      transportLimitHit: this.transportLimitHit,
      lostSinceMs: this.lostSinceMs,
    };
    this.log(formatLostLine(report));
    this.onUnhealthy(report);
  }
}

/**
 * The one line the journal gets. Everything an operator needs is on it, because
 * `infra/docs/runbook.md`'s "chat dead, bridge active" entry tells them to grep for exactly this.
 */
export function formatLostLine(report: ChatSubscriptionLostReport): string {
  const reasons = report.reasons.length > 0 ? report.reasons.join('; ') : 'no reason recorded';
  const limitNote = report.transportLimitHit
    ? ' Twitch refused the subscription with its per-user websocket transport limit, which only ' +
      'time clears, so restarting with a single fresh transport is the fix.'
    : '';
  return (
    `flybridge: FATAL: the channel.chat.message EventSub subscription has not been confirmed for ` +
    `${String(report.graceMs)} ms — chat and every command are dead (reasons: ${reasons}).${limitNote}` +
    ` Exiting ${String(CHAT_SUBSCRIPTION_LOST_EXIT_CODE)} so systemd restarts flybridge with a new websocket transport.`
  );
}

/**
 * Twitch's per-user websocket transport limit, as it arrives through twurple.
 *
 * `@twurple/api-call`'s `HttpStatusCodeError` carries `statusCode` and puts the response body in
 * `message`, so the status is checked structurally when it is there and the body text is matched
 * either way — the same error reaches here as a plain `Error` from the mock API and from a
 * `fetch` wrapper, and a bridge that only recognised one shape would mis-report the reason on the
 * other.
 */
export function isWebsocketTransportLimitError(error: unknown): boolean {
  const message = errorMessage(error);
  if (!/websocket transports? limit exceeded/i.test(message)) return false;
  const statusCode = (error as { statusCode?: unknown } | null | undefined)?.statusCode;
  return statusCode === undefined || statusCode === 429;
}

export interface SelfHealExitDeps {
  /** The notice log, so the next start can tell chat it reconnected (`src/notice.ts`). */
  notices: { markSelfHealExit(reason: string): Promise<void> };
  /** Called before exiting, for `/health` bookkeeping. */
  beforeExit?: () => void;
  /** Seams for the child-process test in `tests/selfheal-exit.test.ts`. */
  exit?: (code: number) => void;
  log?: (line: string) => void;
}

/**
 * The production `onUnhealthy`: persist the recovery marker, then exit non-zero.
 *
 * Extracted from `src/index.ts` so the real thing — marker write included — can be driven by a
 * child process in a test rather than re-implemented there. The marker write is best-effort: a
 * failure to write it must not stop the exit, because the exit is the recovery and a missing
 * marker only costs chat a "reconnected" line.
 */
export function createSelfHealExit(deps: SelfHealExitDeps): (report: ChatSubscriptionLostReport) => void {
  const exit = deps.exit ?? ((code: number) => process.exit(code));
  const log = deps.log ?? ((line: string) => console.error(line));
  return (report) => {
    deps.beforeExit?.();
    void deps.notices
      .markSelfHealExit(report.reasons[0] ?? 'chat subscription unconfirmed')
      .catch((cause: unknown) => {
        log(`flybridge: could not write the notice log before exiting: ${String(cause)}`);
      })
      .finally(() => {
        exit(CHAT_SUBSCRIPTION_LOST_EXIT_CODE);
      });
  };
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
