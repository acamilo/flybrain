/**
 * The startup/recovery notice gate (`src/notice.ts`): one notice per ten minutes ACROSS process
 * restarts, and "reconnected" rather than "online" when the previous process exited itself.
 *
 * The file is faked through the `readJson`/`writeJson` seams, so these tests touch no disk and a
 * "restart" is just a second `NoticeLog` over the same in-memory blob.
 */
import assert from 'node:assert/strict';
import test from 'node:test';
import {
  DEFAULT_NOTICE_MIN_INTERVAL_MS,
  NoticeLog,
  RECOVERY_NOTICE_MAX_AGE_MS,
  type NoticeKind,
} from '../src/notice';
import { FakeClock } from '../src/ratelimit';

const PATH = '/var/lib/flybridge/notice-state.json';

/** A one-file in-memory filesystem, shared across the `NoticeLog`s a test creates. */
class FakeFile {
  contents: string | null = null;
  writes = 0;

  readonly read = async (path: string): Promise<string> => {
    assert.equal(path, PATH);
    if (this.contents === null) throw Object.assign(new Error('ENOENT'), { code: 'ENOENT' });
    return this.contents;
  };

  readonly write = async (path: string, data: unknown): Promise<void> => {
    assert.equal(path, PATH);
    this.contents = JSON.stringify(data);
    this.writes += 1;
  };
}

function log(file: FakeFile, clock: FakeClock, minIntervalMs = DEFAULT_NOTICE_MIN_INTERVAL_MS): NoticeLog {
  return new NoticeLog({ path: PATH, minIntervalMs, clock, readJson: file.read, writeJson: file.write });
}

/** One process lifetime: load, decide, and post if allowed. Returns what it posted. */
async function boot(file: FakeFile, clock: FakeClock, minIntervalMs?: number): Promise<NoticeKind | null> {
  const notices = log(file, clock, minIntervalMs);
  await notices.load();
  const decision = notices.decide();
  if (decision.post !== null) await notices.markPosted();
  return decision.post;
}

void test('a first start with no state file posts the startup notice', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  assert.equal(await boot(file, clock), 'startup');
  assert.equal(file.writes, 1);
});

void test('a restart inside the ten-minute window posts nothing at all', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  assert.equal(await boot(file, clock), 'startup');

  clock.advance(9 * 60 * 1000);
  const notices = log(file, clock);
  await notices.load();
  const decision = notices.decide();
  assert.equal(decision.post, null);
  assert.equal(decision.post === null ? decision.suppressedForMs : -1, 60 * 1000);
  assert.equal(file.writes, 1, 'a suppressed notice does not touch the file');
});

void test('a restart after the window posts again', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  await boot(file, clock);
  clock.advance(DEFAULT_NOTICE_MIN_INTERVAL_MS + 1);
  assert.equal(await boot(file, clock), 'startup');
});

void test('a self-healing exit makes the next allowed notice a recovery notice, once', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  await boot(file, clock);

  // The watchdog gives up an hour later.
  clock.advance(60 * 60 * 1000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('EventSub shared socket disconnected: 1006');

  // systemd restarts it 15 s later.
  clock.advance(15_000);
  assert.equal(await boot(file, clock), 'recovered');

  // A later restart is a plain startup again: the marker was consumed.
  clock.advance(DEFAULT_NOTICE_MIN_INTERVAL_MS + 1);
  assert.equal(await boot(file, clock), 'startup');
});

void test('a restart loop posts one recovery notice, not one per attempt', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  const posted: (NoticeKind | null)[] = [];

  // Twitch is refusing the subscription. Six exits, 15 s apart, plus the grace each time.
  for (let attempt = 0; attempt < 6; attempt += 1) {
    posted.push(await boot(file, clock));
    const dying = log(file, clock);
    await dying.load();
    await dying.markSelfHealExit('create failed: HTTP 429, Twitch per-user websocket transport limit exceeded');
    clock.advance(60_000 + 15_000);
  }

  assert.deepEqual(
    posted,
    ['startup', null, null, null, null, null],
    'the first start speaks; the restarts stay quiet until the ten minutes are up',
  );
});

void test('a recovery marker older than the max age reports as a plain startup', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('EventSub shared socket disconnected: 1006');

  // A self-heal at 02:00 and a hand-typed restart at 09:00 must not tell chat it just reconnected.
  clock.advance(RECOVERY_NOTICE_MAX_AGE_MS + 1);
  assert.equal(await boot(file, clock), 'startup');
});

void test('the marker survives a suppressed restart and is still reported once the window clears', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  await boot(file, clock);

  clock.advance(60_000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('create failed: HTTP 429, Twitch per-user websocket transport limit exceeded');

  clock.advance(15_000);
  assert.equal(await boot(file, clock), null, 'inside the ten minutes');

  clock.advance(DEFAULT_NOTICE_MIN_INTERVAL_MS);
  assert.equal(await boot(file, clock), 'recovered');
});

void test('markSelfHealExit does not reset the ten-minute clock', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  await boot(file, clock);
  const postedAt = (JSON.parse(file.contents!) as { lastPostedAtMs: number }).lastPostedAtMs;

  clock.advance(30_000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('whatever');
  assert.equal((JSON.parse(file.contents!) as { lastPostedAtMs: number }).lastPostedAtMs, postedAt);
});

void test('a corrupt state file is treated as empty rather than refusing to start', async () => {
  const file = new FakeFile();
  file.contents = '{ this is not json';
  const clock = new FakeClock(1_700_000_000_000);
  assert.equal(await boot(file, clock), 'startup');
});

void test('a state file with the wrong shape is ignored field by field', async () => {
  const file = new FakeFile();
  file.contents = JSON.stringify({ lastPostedAtMs: 'yesterday', pendingRecovery: { atIso: 42 } });
  const clock = new FakeClock(1_700_000_000_000);
  const notices = log(file, clock);
  await notices.load();
  assert.deepEqual(notices.current, { lastPostedAtMs: null, pendingRecovery: null });
  assert.equal(notices.decide().post, 'startup');
});

void test('the loaded pendingRecovery is readable for /health', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('EventSub shared socket disconnected: 1006');

  const restarted = log(file, clock);
  await restarted.load();
  assert.equal(restarted.current.pendingRecovery?.reason, 'EventSub shared socket disconnected: 1006');
  assert.equal(restarted.current.pendingRecovery?.atIso, new Date(1_700_000_000_000).toISOString());
});

// -- Quiet mode (FEATURE_QUIET) -----------------------------------------------------------------

void test('quiet mode suppresses the startup notice and writes nothing', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  const notices = new NoticeLog({
    path: PATH,
    minIntervalMs: DEFAULT_NOTICE_MIN_INTERVAL_MS,
    quiet: true,
    clock,
    readJson: file.read,
    writeJson: file.write,
  });
  await notices.load();

  const decision = notices.decide();
  assert.equal(decision.post, null);
  assert.equal(decision.post === null && decision.quiet, true, 'reported as quiet, not as rate-limited');
  assert.equal(file.writes, 0);
});

void test('quiet mode suppresses the RECOVERY notice too, and leaves the marker in the file', async () => {
  const file = new FakeFile();
  const clock = new FakeClock(1_700_000_000_000);
  const dying = log(file, clock);
  await dying.load();
  await dying.markSelfHealExit('EventSub shared socket disconnected: 1006');
  const writesAfterExit = file.writes;

  const restarted = new NoticeLog({
    path: PATH,
    minIntervalMs: DEFAULT_NOTICE_MIN_INTERVAL_MS,
    quiet: true,
    clock,
    readJson: file.read,
    writeJson: file.write,
  });
  await restarted.load();

  assert.equal(restarted.decide().post, null, 'a self-heal restart says nothing in quiet mode');
  assert.equal(file.writes, writesAfterExit, 'and consumes nothing');
  // /health still gets to say this process is a recovery.
  assert.equal(restarted.current.pendingRecovery?.reason, 'EventSub shared socket disconnected: 1006');
});
