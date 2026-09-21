/**
 * A process that loses its chat subscription and never gets it back — driven by
 * `tests/selfheal-exit.test.ts`, which spawns this and asserts the exit code.
 *
 * Deliberately uses the PRODUCTION pieces with nothing faked but the grace period and the state
 * file path: the real `ChatSubscriptionHealth` with real `setTimeout`, the real `NoticeLog`
 * writing a real file, and the real `createSelfHealExit` calling the real `process.exit`. The
 * unit tests cover the state machine; this covers the one thing they cannot, which is that the
 * process actually goes away with a non-zero status so systemd restarts it.
 *
 * argv: <grace-ms> <notice-state-file>
 */
import { NoticeLog } from '../../src/notice';
import { ChatSubscriptionHealth, createSelfHealExit } from '../../src/subscription-health';

const graceMs = Number(process.argv[2]);
const noticeStateFile = process.argv[3];
if (!Number.isFinite(graceMs) || !noticeStateFile) {
  throw new Error('usage: selfheal-exit.ts <grace-ms> <notice-state-file>');
}

const notices = new NoticeLog({ path: noticeStateFile, minIntervalMs: 600_000 });
await notices.load();

const health = new ChatSubscriptionHealth({
  graceMs,
  onUnhealthy: createSelfHealExit({ notices }),
});

// The 2026-09-16 sequence: socket drop, then Twitch refusing the re-create, then nothing.
health.noteLost('EventSub shared socket disconnected: Connection closed abnormally (1006)');
health.noteCreateFailure(
  Object.assign(new Error('number of websocket transports limit exceeded'), { statusCode: 429 }),
);

// Keep the process alive the way the real bridge is (health server, sockets), so the only thing
// that can end it is the watchdog.
const keepAlive = setInterval(() => undefined, 1_000);
setTimeout(
  () => {
    clearInterval(keepAlive);
    console.error('selfheal-exit fixture: the watchdog never fired');
    process.exit(0);
  },
  graceMs * 10 + 2_000,
);
