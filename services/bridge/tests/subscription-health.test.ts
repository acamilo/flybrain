/**
 * The self-healing watchdog's state machine (`src/subscription-health.ts`), driven by a manual
 * timer and a `FakeClock` — no real `setTimeout`, no Twitch, no process exit.
 *
 * The scenario every test here is a slice of is the 2026-09-16 the release container outage: socket disconnect,
 * then a 429 "websocket transports limit exceeded" on the re-create, then silence.
 */
import assert from 'node:assert/strict';
import test from 'node:test';
import { FakeClock } from '../src/ratelimit';
import {
  CHAT_SUBSCRIPTION_LOST_EXIT_CODE,
  ChatSubscriptionHealth,
  createSelfHealExit,
  formatLostLine,
  isWebsocketTransportLimitError,
  type ChatSubscriptionLostReport,
  type Timers,
} from '../src/subscription-health';

const GRACE_MS = 60_000;

/** A `Timers` whose one pending callback fires only when `fire()` is called. */
class ManualTimers implements Timers {
  private next = 1;
  readonly pending = new Map<number, { callback: () => void; ms: number }>();

  set(callback: () => void, ms: number): unknown {
    const handle = this.next++;
    this.pending.set(handle, { callback, ms });
    return handle;
  }

  clear(handle: unknown): void {
    this.pending.delete(handle as number);
  }

  /** Fire every pending callback, as the event loop would once the delay elapsed. */
  fire(): void {
    for (const [handle, entry] of [...this.pending.entries()]) {
      this.pending.delete(handle);
      entry.callback();
    }
  }

  get pendingCount(): number {
    return this.pending.size;
  }
}

interface Harness {
  health: ChatSubscriptionHealth;
  timers: ManualTimers;
  clock: FakeClock;
  reports: ChatSubscriptionLostReport[];
  logs: string[];
}

function harness(graceMs = GRACE_MS): Harness {
  const timers = new ManualTimers();
  const clock = new FakeClock();
  const reports: ChatSubscriptionLostReport[] = [];
  const logs: string[] = [];
  const health = new ChatSubscriptionHealth({
    graceMs,
    timers,
    clock,
    onUnhealthy: (report) => reports.push(report),
    log: (line) => logs.push(line),
  });
  return { health, timers, clock, reports, logs };
}

/** The Twitch 429 body, verbatim from the journal on 2026-09-16 11:45:35 UTC. */
function transportLimitError(): Error {
  const error = new Error(
    'Encountered HTTP status code 429: Too Many Requests\n\nURL: https://api.twitch.tv/helix/eventsub/subscriptions\n' +
      'Method: POST\nBody:\n{"error":"Too Many Requests","status":429,' +
      '"message":"number of websocket transports limit exceeded"}',
  );
  Object.assign(error, { statusCode: 429 });
  return error;
}

void test('a create failure with no confirmation inside the grace period asks for a non-zero exit', () => {
  const { health, timers, reports, logs } = harness();

  health.noteLost('EventSub shared socket disconnected: 1006');
  health.noteCreateFailure(transportLimitError());
  assert.equal(reports.length, 0, 'nothing should fire before the grace period elapses');
  assert.equal(health.armed, true);

  timers.fire();

  assert.equal(reports.length, 1);
  const report = reports[0]!;
  assert.equal(report.graceMs, GRACE_MS);
  assert.equal(report.transportLimitHit, true);
  assert.deepEqual(report.reasons, [
    'EventSub shared socket disconnected: 1006',
    'create failed: HTTP 429, Twitch per-user websocket transport limit exceeded',
  ]);
  assert.equal(logs.length, 1, 'exactly one clear line in the journal');
  assert.match(logs[0]!, /channel\.chat\.message/);
  assert.match(logs[0]!, /websocket transport limit/);
  assert.match(logs[0]!, new RegExp(`Exiting ${String(CHAT_SUBSCRIPTION_LOST_EXIT_CODE)}`));
  assert.notEqual(CHAT_SUBSCRIPTION_LOST_EXIT_CODE, 0);
});

void test('the grace timer set by the first loss also covers a create failure that follows it', () => {
  const { health, timers } = harness();
  health.noteLost('EventSub shared socket disconnected: 1006');
  assert.equal(timers.pendingCount, 1);
  health.noteCreateFailure(transportLimitError());
  health.noteCreateFailure(transportLimitError());
  // One timer, measured from the FIRST loss: a socket that flaps must not be able to postpone
  // recovery indefinitely while chat stays dead the whole time.
  assert.equal(timers.pendingCount, 1);
});

void test('a confirmation inside the grace period does not exit', () => {
  const { health, timers, reports, logs } = harness();

  health.noteLost('startup: chat subscription not confirmed yet');
  health.noteCreateFailure(transportLimitError());
  health.noteConfirmed();

  assert.equal(health.healthy, true);
  assert.equal(health.armed, false, 'confirmation disarms the grace timer');
  assert.equal(timers.pendingCount, 0);

  timers.fire(); // nothing pending; also proves a stale callback cannot fire late
  assert.deepEqual(reports, []);
  assert.deepEqual(logs, []);
});

void test('a reconnect after a confirmed period re-arms, and a second confirmation disarms again', () => {
  const { health, timers, reports } = harness();
  health.noteLost('startup: chat subscription not confirmed yet');
  health.noteConfirmed();
  assert.equal(health.healthy, true);

  health.noteLost('EventSub shared socket disconnected: 1006');
  assert.equal(health.healthy, false);
  assert.equal(timers.pendingCount, 1);

  health.noteConfirmed();
  assert.equal(timers.pendingCount, 0);
  timers.fire();
  assert.deepEqual(reports, []);
});

void test('onUnhealthy fires at most once, however much the socket flaps afterwards', () => {
  const { health, timers, reports } = harness();
  health.noteLost('EventSub shared socket disconnected: 1006');
  timers.fire();
  assert.equal(reports.length, 1);

  health.noteLost('EventSub shared socket disconnected again');
  health.noteCreateFailure(transportLimitError());
  timers.fire();
  assert.equal(reports.length, 1, 'the process is already on its way out');
});

void test('stop() cancels the grace timer, so SIGTERM does not race a self-healing exit', () => {
  const { health, timers, reports } = harness();
  health.noteLost('EventSub shared socket disconnected: 1006');
  health.stop();
  assert.equal(timers.pendingCount, 0);
  timers.fire();
  assert.deepEqual(reports, []);
});

void test('the report records the wall-clock time of the first loss', () => {
  const { health, timers, clock, reports } = harness();
  clock.advance(1_700_000);
  health.noteLost('EventSub shared socket disconnected: 1006');
  clock.advance(30_000);
  health.noteCreateFailure(transportLimitError());
  timers.fire();
  assert.equal(reports[0]!.lostSinceMs, 1_700_000);
});

void test('a non-429 create failure is recorded verbatim and does not claim the transport limit', () => {
  const { health, timers, reports } = harness();
  health.noteCreateFailure(new Error('subscription missing required scope user:read:chat'));
  timers.fire();
  assert.equal(reports[0]!.transportLimitHit, false);
  assert.deepEqual(reports[0]!.reasons, ['create failed: subscription missing required scope user:read:chat']);
});

void test('isWebsocketTransportLimitError recognises the Twitch 429 and nothing else', () => {
  assert.equal(isWebsocketTransportLimitError(transportLimitError()), true);
  // The same body arriving as a plain Error, which is what the Twitch CLI mock API produces.
  assert.equal(
    isWebsocketTransportLimitError(new Error('number of websocket transports limit exceeded')),
    true,
  );
  assert.equal(isWebsocketTransportLimitError(new Error('429 Too Many Requests')), false);
  assert.equal(isWebsocketTransportLimitError(new Error('missing scope')), false);
  assert.equal(isWebsocketTransportLimitError('websocket transports limit exceeded'), true);
  assert.equal(isWebsocketTransportLimitError(undefined), false);
  // A 500 whose body happens to quote the phrase is not the transport limit.
  const misleading = Object.assign(new Error('websocket transports limit exceeded'), { statusCode: 500 });
  assert.equal(isWebsocketTransportLimitError(misleading), false);
});

void test('formatLostLine names the subscription, the grace, every reason and the exit code', () => {
  const line = formatLostLine({
    graceMs: 60_000,
    reasons: ['EventSub shared socket disconnected: 1006'],
    transportLimitHit: false,
    lostSinceMs: 0,
  });
  assert.match(line, /channel\.chat\.message/);
  assert.match(line, /60000 ms/);
  assert.match(line, /EventSub shared socket disconnected: 1006/);
  assert.match(line, /Exiting 75/);
  assert.doesNotMatch(line, /transport limit/);
});

void test('createSelfHealExit writes the recovery marker before exiting non-zero', async () => {
  const marked: string[] = [];
  const exits: number[] = [];
  const onUnhealthy = createSelfHealExit({
    notices: {
      markSelfHealExit: async (reason) => {
        marked.push(reason);
      },
    },
    exit: (code) => exits.push(code),
  });

  onUnhealthy({
    graceMs: GRACE_MS,
    reasons: ['EventSub shared socket disconnected: 1006', 'create failed: HTTP 429'],
    transportLimitHit: true,
    lostSinceMs: 0,
  });
  // The marker write is a promise; let it settle.
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(marked, ['EventSub shared socket disconnected: 1006']);
  assert.deepEqual(exits, [CHAT_SUBSCRIPTION_LOST_EXIT_CODE]);
});

void test('createSelfHealExit still exits when the marker cannot be written', async () => {
  const exits: number[] = [];
  const logs: string[] = [];
  const onUnhealthy = createSelfHealExit({
    notices: {
      markSelfHealExit: async () => {
        throw new Error('EROFS');
      },
    },
    exit: (code) => exits.push(code),
    log: (line) => logs.push(line),
  });

  onUnhealthy({ graceMs: GRACE_MS, reasons: [], transportLimitHit: false, lostSinceMs: 0 });
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(exits, [CHAT_SUBSCRIPTION_LOST_EXIT_CODE]);
  assert.equal(logs.length, 1);
  assert.match(logs[0]!, /could not write the notice log/);
});
