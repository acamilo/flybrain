/**
 * The self-healing exit, for real: spawn a process that loses its chat subscription, wait for it
 * to end, and check the status code systemd would see.
 *
 * `tests/subscription-health.test.ts` proves the state machine with fake timers and a recorded
 * `onUnhealthy`. It cannot prove that the process actually exits non-zero, which is the entire
 * recovery mechanism, so that is what this does — with real timers, a real file write and a real
 * `process.exit`. The fixture is `tests/fixtures/selfheal-exit.ts`.
 */
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { CHAT_SUBSCRIPTION_LOST_EXIT_CODE } from '../src/subscription-health';
import type { NoticeState } from '../src/notice';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(HERE, 'fixtures', 'selfheal-exit.ts');
/** Short enough to keep the suite fast, long enough that a loaded CI box is not flaky. */
const GRACE_MS = 250;

interface Run {
  code: number | null;
  signal: NodeJS.Signals | null;
  stderr: string;
}

async function runFixture(noticeStateFile: string): Promise<Run> {
  const child = spawn(
    process.execPath,
    ['--import', 'tsx', FIXTURE, String(GRACE_MS), noticeStateFile],
    { cwd: join(HERE, '..'), stdio: ['ignore', 'ignore', 'pipe'] },
  );
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk: string) => {
    stderr += chunk;
  });
  return await new Promise<Run>((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal, stderr }));
  });
}

void test('a chat subscription that never confirms ends the process with a non-zero status', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'flybridge-selfheal-'));
  try {
    const noticeStateFile = join(dir, 'notice-state.json');
    const run = await runFixture(noticeStateFile);

    assert.equal(run.signal, null, 'a clean exit, not a crash or a kill');
    assert.equal(run.code, CHAT_SUBSCRIPTION_LOST_EXIT_CODE);
    assert.notEqual(run.code, 0, 'systemd Restart=always restarts on 0 too, but the code must say why');

    // The one clear line the runbook tells an operator to grep for.
    assert.match(run.stderr, /FATAL: the channel\.chat\.message EventSub subscription has not been confirmed/);
    assert.match(run.stderr, /per-user websocket transport limit/);
    assert.match(run.stderr, new RegExp(`Exiting ${String(CHAT_SUBSCRIPTION_LOST_EXIT_CODE)}`));
    assert.equal(
      run.stderr.split('FATAL:').length - 1,
      1,
      'exactly one FATAL line, however many losses were recorded',
    );

    // The recovery marker the next start reads, so chat is told once and not on every attempt.
    const state = JSON.parse(await readFile(noticeStateFile, 'utf8')) as NoticeState;
    assert.equal(state.lastPostedAtMs, null, 'this process never posted a notice');
    assert.match(state.pendingRecovery?.reason ?? '', /socket disconnected/);
    assert.ok(Date.parse(state.pendingRecovery?.atIso ?? '') > 0);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
