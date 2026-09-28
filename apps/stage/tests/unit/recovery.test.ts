/**
 * The recovery splash's model, poller and server route.
 *
 * The one property every test here protects: the notice file can never break the stream. A
 * missing, malformed, stale or hostile file is no splash, and a good one shows exactly the
 * contract's lifetimes (countdown until acting, acting while flysim is down, done ~8 s, failed
 * briefly) from the file alone.
 */
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { parseStageOptions } from '../../src/lib/query';
import {
  RECOVERY_ACTING_MAX_S,
  RECOVERY_DONE_S,
  RECOVERY_FAILED_S,
  RECOVERY_STALE_S,
  cleanText,
  formatCountdown,
  formatStuck,
  parseRecoveryBody,
  parseRecoveryNotice,
  recoveryView,
} from '../../src/lib/recovery';
import { RecoveryPoller } from '../../src/lib/recovery-poll';

const T = 1_790_629_095;

function notice(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    v: 1,
    id: `${T}-reset`,
    phase: 'countdown',
    action: 'reset',
    fromRung: 12,
    fromLabel: 'MT. MOON',
    toRung: 11,
    toLabel: 'PEWTER CITY',
    reason: 'unrewarded',
    loop: ['GO OBJECTIVE', 'GO WARP'],
    stuckSeconds: 1800,
    announcedAt: T,
    executeAt: T + 60,
    updatedAt: T,
    ...overrides,
  };
}

const view = (raw: Record<string, unknown>, now: number) => recoveryView(parseRecoveryNotice(raw), now);

test('the contract example parses', () => {
  const parsed = parseRecoveryNotice(notice());
  assert.ok(parsed);
  assert.equal(parsed.phase, 'countdown');
  assert.equal(parsed.action, 'reset');
  assert.equal(parsed.toLabel, 'PEWTER CITY');
  assert.equal(parsed.fromLabel, 'MT. MOON');
  assert.deepEqual(parsed.loop, ['GO OBJECTIVE', 'GO WARP']);
});

test('anything malformed is no notice at all', () => {
  const bad: unknown[] = [
    null,
    42,
    'x',
    [],
    notice({ v: 2 }),
    notice({ v: '1' }),
    notice({ id: '' }),
    notice({ id: 7 }),
    notice({ phase: 'panic' }),
    notice({ action: 'reboot' }),
    notice({ executeAt: 'soon' }),
    notice({ updatedAt: Number.NaN }),
    notice({ announcedAt: undefined }),
  ];
  for (const raw of bad) assert.equal(parseRecoveryNotice(raw), null, JSON.stringify(raw));
  for (const body of ['', '{', 'null', '{"v":1}', 'x'.repeat(20_000)]) assert.equal(parseRecoveryBody(body), null);
  assert.ok(parseRecoveryBody(JSON.stringify(notice())));
});

test('optional fields degrade the copy, not the notice', () => {
  const bare = view({ v: 1, id: 'a', phase: 'countdown', action: 'reset', announcedAt: T, executeAt: T + 30, updatedAt: T }, T);
  assert.ok(bare);
  assert.equal(bare.body, 'Rewinding to the last milestone in');
  assert.equal(bare.loop, '');
  assert.equal(bare.stuck, '');
});

test('helper strings are clipped to a closed character set', () => {
  assert.equal(cleanText('<img src=x onerror=alert(1)>', 24), 'img srcx onerroralert1');
  assert.equal(cleanText('S.S. ANNE', 24), 'S.S. ANNE');
  assert.equal(cleanText('  ROUTE\n\t22  ', 24), 'ROUTE 22');
  assert.equal(cleanText('A'.repeat(100), 24).length, 24);
  assert.equal(cleanText(12, 24), '');
  const parsed = parseRecoveryNotice(notice({ loop: ['go objective', 5, '', 'A', 'B', 'C', 'D'], toLabel: 'pewter\u0000city' }));
  assert.ok(parsed);
  assert.deepEqual(parsed.loop, ['GO OBJECTIVE', 'A', 'B', 'C']);
  assert.equal(parsed.toLabel, 'PEWTERCITY');
  // A restart has no destination, whatever the file says.
  assert.equal(parseRecoveryNotice(notice({ action: 'restart' }))?.toLabel, '');
});

test('countdown: the text box with M:SS to executeAt, reset and restart copy differ', () => {
  const reset = view(notice(), T + 18);
  assert.ok(reset);
  assert.equal(reset.layout, 'box');
  assert.equal(reset.countdown, '0:42');
  assert.equal(reset.headline, 'The fly is stuck in a loop!');
  assert.equal(reset.body, 'Rewinding to PEWTER CITY in');
  assert.equal(reset.loop, 'GO OBJECTIVE > GO WARP');
  assert.equal(reset.stuck, 'STUCK 30 MIN');

  const restart = view(notice({ action: 'restart' }), T);
  assert.ok(restart);
  assert.equal(restart.body, 'Shaking it off in');
  assert.equal(restart.countdown, '1:00');

  // Past executeAt but not yet acting: say "now", never a negative clock.
  const late = view(notice(), T + 75);
  assert.ok(late);
  assert.equal(late.countdown, '0:00');
  assert.equal(late.body, 'Rewinding to PEWTER CITY now');
  assert.equal(late.busy, true);
});

test('acting: covers the game while flysim is down, and gives up after a cap', () => {
  const acting = notice({ phase: 'acting', updatedAt: T + 60 });
  const reset = view(acting, T + 90);
  assert.ok(reset);
  assert.equal(reset.layout, 'cover');
  assert.equal(reset.headline, 'Rewinding');
  assert.equal(reset.body, 'Back to PEWTER CITY');
  const restart = view({ ...acting, action: 'restart' }, T + 90);
  assert.equal(restart?.headline, 'Shaking it off');
  assert.equal(view(acting, T + 60 + RECOVERY_ACTING_MAX_S + 1), null);
});

test('done shows about 8 s, failed a little longer, then both clear', () => {
  const done = notice({ phase: 'done', updatedAt: T + 120 });
  assert.equal(view(done, T + 120)?.headline, 'Back at PEWTER CITY!');
  assert.equal(view({ ...done, action: 'restart' }, T + 121)?.headline, 'All shaken off!');
  assert.ok(view(done, T + 120 + RECOVERY_DONE_S));
  assert.equal(view(done, T + 120 + RECOVERY_DONE_S + 1), null);

  const failed = notice({ phase: 'failed', updatedAt: T + 120 });
  assert.equal(view(failed, T + 125)?.body, 'A human will take a look');
  assert.equal(view(failed, T + 120 + RECOVERY_FAILED_S + 1), null);
});

test('a notice older than 15 minutes, or from the far future, is ignored', () => {
  assert.ok(view(notice(), T + RECOVERY_STALE_S));
  assert.equal(view(notice(), T + RECOVERY_STALE_S + 1), null);
  assert.equal(view(notice(), T - 3600), null);
});

test('formatting', () => {
  assert.equal(formatCountdown(59.2), '1:00');
  assert.equal(formatCountdown(5), '0:05');
  assert.equal(formatCountdown(725), '12:05');
  assert.equal(formatCountdown(-3), '0:00');
  assert.equal(formatStuck(null), '');
  assert.equal(formatStuck(30), '');
  assert.equal(formatStuck(1800), 'STUCK 30 MIN');
  assert.equal(formatStuck(7200), 'STUCK 2 H');
  assert.equal(formatStuck(3900), 'STUCK 1 H 5 MIN');
});

test('the page polls in live mode, not in player mode, unless told', () => {
  assert.equal(parseStageOptions('?mode=live').recovery, true);
  assert.equal(parseStageOptions('').recovery, false);
  assert.equal(parseStageOptions('?recovery=1').recovery, true);
  assert.equal(parseStageOptions('?mode=live&recovery=0').recovery, false);
});

test('the poller turns every failure into "no notice" and reports changes once', async () => {
  const seen: (string | null)[] = [];
  let reply: () => Promise<Response> = () => Promise.resolve(new Response(JSON.stringify(notice()), { status: 200 }));
  const poller = new RecoveryPoller({
    fetch: () => reply(),
    onChange: (n) => seen.push(n === null ? null : n.phase),
  });

  assert.equal((await poller.poll())?.phase, 'countdown');
  await poller.poll(); // unchanged: no second callback
  reply = () => Promise.resolve(new Response(null, { status: 204 }));
  assert.equal(await poller.poll(), null);
  reply = () => Promise.resolve(new Response('{"v":1,', { status: 200 }));
  assert.equal(await poller.poll(), null);
  reply = () => Promise.resolve(new Response('not found', { status: 404 }));
  assert.equal(await poller.poll(), null);
  reply = () => Promise.reject(new Error('ECONNREFUSED'));
  assert.equal(await poller.poll(), null);
  reply = () => Promise.resolve(new Response(JSON.stringify(notice({ phase: 'acting' })), { status: 200 }));
  assert.equal((await poller.poll())?.phase, 'acting');

  assert.deepEqual(seen, ['countdown', null, 'acting']);
});

test('the poller survives a throwing listener', async () => {
  const poller = new RecoveryPoller({
    fetch: () => Promise.resolve(new Response(JSON.stringify(notice()), { status: 200 })),
    onChange: () => {
      throw new Error('boom');
    },
  });
  const warn = console.warn;
  console.warn = () => {};
  try {
    assert.equal((await poller.poll())?.phase, 'countdown');
  } finally {
    console.warn = warn;
  }
});

test('flystage-web serves the notice file at /recovery-notice.json, 204 without one', async () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const serve = resolve(here, '../../../../infra/config/serve.mjs');
  const dir = await mkdtemp(join(tmpdir(), 'recovery-notice-'));
  const file = join(dir, 'recovery-notice.json');
  const child = spawn(process.execPath, [serve, dir, '127.0.0.1', '0'], {
    env: { ...process.env, FLY_RECOVERY_NOTICE: file },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  try {
    const base = await new Promise<string>((done, fail) => {
      let out = '';
      child.stdout.on('data', (chunk: Buffer) => {
        out += chunk.toString();
        const match = /(http:\/\/127\.0\.0\.1:\d+)\//.exec(out);
        if (match?.[1]) done(match[1]);
      });
      child.on('exit', () => fail(new Error(`serve.mjs exited: ${out}`)));
    });

    const missing = await fetch(`${base}/recovery-notice.json`);
    assert.equal(missing.status, 204);
    assert.equal(missing.headers.get('cache-control'), 'no-store');

    const body = JSON.stringify(notice());
    await writeFile(file, body);
    const present = await fetch(`${base}/recovery-notice.json?t=1`);
    assert.equal(present.status, 200);
    assert.equal(await present.text(), body);

    await writeFile(file, 'x'.repeat(20_000));
    assert.equal((await fetch(`${base}/recovery-notice.json`)).status, 204, 'oversized files are not served');

    // The static half still works, and the notice is not reachable any other way.
    await writeFile(join(dir, 'index.html'), '<p>stage</p>');
    assert.equal(await (await fetch(`${base}/`)).text(), '<p>stage</p>');
  } finally {
    child.kill('SIGTERM');
    await rm(dir, { recursive: true, force: true });
  }
});
