/**
 * Refresh-before-give-up on load (`prepareStartupTokens` / `assertStartupScopes` in
 * `src/auth.ts`), against a mock of Twitch's two OAuth endpoints (`/oauth2/validate`,
 * `/oauth2/token` refresh grant) installed as `globalThis.fetch`, which is what
 * `@twurple/api-call` calls.
 *
 * The incident: the bridge was down longer than an access token lives (~4 h), both stored access
 * tokens expired, and the next start validated them first, got 401 and crash-looped under
 * Restart=always — with both refresh tokens still good.
 */
import assert from 'node:assert/strict';
import { mkdtemp, readdir, readFile, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  assertStartupScopes,
  buildAuthProvider,
  loadTokensFile,
  prepareStartupTokens,
  StartupTokenError,
  type StoredToken,
  type TokensFile,
} from '../src/auth';

const CLIENT_ID = 'test-client-id';
const CLIENT_SECRET = 'test-client-secret-DO-NOT-LOG';
const BOT_SCOPES = ['user:read:chat', 'user:write:chat', 'user:bot'];
const BROADCASTER_SCOPES = ['channel:bot', 'moderator:read:followers'];
const FOUR_HOURS_S = 14_400;

interface MockTwitch {
  validateCalls: string[];
  refreshCalls: string[];
  restore: () => void;
}

/**
 * Twitch's id endpoints. `valid` maps a live access token to its owner and scopes; `refreshable`
 * maps a live refresh token to the same. A refresh mints `<refreshToken>-fresh-<n>` and makes it
 * valid; an unknown refresh token gets Twitch's real answer, 400 "Invalid refresh token".
 */
function mockTwitch(
  valid: Map<string, { userId: string; scopes: string[] }>,
  refreshable: Map<string, { userId: string; scopes: string[] }>,
): MockTwitch {
  const original = globalThis.fetch;
  const validateCalls: string[] = [];
  const refreshCalls: string[] = [];
  let minted = 0;
  const json = (status: number, body: unknown): Response =>
    new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const url = new URL(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url);
    assert.equal(url.host, 'id.twitch.tv', `unexpected request to ${url.host}`);
    if (url.pathname === '/oauth2/validate') {
      const auth = new Headers(init?.headers).get('Authorization') ?? '';
      const token = auth.replace(/^OAuth /, '');
      validateCalls.push(token);
      const owner = valid.get(token);
      if (!owner) return json(401, { status: 401, message: 'invalid access token' });
      return json(200, {
        client_id: CLIENT_ID,
        login: 'someone',
        user_id: owner.userId,
        scopes: owner.scopes,
        expires_in: FOUR_HOURS_S,
      });
    }
    if (url.pathname === '/oauth2/token' && url.searchParams.get('grant_type') === 'refresh_token') {
      assert.equal(url.searchParams.get('client_secret'), CLIENT_SECRET);
      const refreshToken = url.searchParams.get('refresh_token') ?? '';
      refreshCalls.push(refreshToken);
      const owner = refreshable.get(refreshToken);
      if (!owner) return json(400, { status: 400, message: 'Invalid refresh token' });
      minted += 1;
      const accessToken = `${refreshToken}-fresh-${String(minted)}`;
      valid.set(accessToken, owner);
      return json(200, {
        access_token: accessToken,
        refresh_token: refreshToken,
        scope: owner.scopes,
        expires_in: FOUR_HOURS_S,
        token_type: 'bearer',
      });
    }
    throw new Error(`mock Twitch: unhandled ${url.pathname}`);
  }) as typeof fetch;

  return { validateCalls, refreshCalls, restore: () => (globalThis.fetch = original) };
}

function stored(userId: string, name: string, scope: string[], obtainedMsAgo: number): StoredToken {
  return {
    userId,
    accessToken: `access-${name}`,
    refreshToken: `refresh-${name}`,
    scope,
    expiresIn: FOUR_HOURS_S,
    obtainmentTimestamp: Date.now() - obtainedMsAgo,
  };
}

const EXPIRED = 6 * 3600_000; // down for six hours
const FRESH = 60_000;

async function writeTokens(tokens: TokensFile): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'flybridge-refresh-'));
  const path = join(dir, 'tokens.json');
  await writeFile(path, JSON.stringify(tokens), { mode: 0o600 });
  return path;
}

const CONFIG_BASE = {
  twitchClientId: CLIENT_ID,
  twitchClientSecret: CLIENT_SECRET,
  featureRedemptions: false,
  featurePredictions: false,
};

async function start(path: string, lines: string[] = []) {
  const config = { ...CONFIG_BASE, tokensFile: path };
  const setup = buildAuthProvider(config, await loadTokensFile(path));
  await assertStartupScopes(config, setup, { log: (line) => lines.push(line) });
  return setup;
}

void test('expired-on-disk tokens for both roles are refreshed (not validated first) and the bridge starts', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, EXPIRED),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, EXPIRED),
  });
  const twitch = mockTwitch(
    new Map(), // neither stored access token is valid any more
    new Map([
      ['refresh-bot', { userId: '111', scopes: BOT_SCOPES }],
      ['refresh-caster', { userId: '222', scopes: BROADCASTER_SCOPES }],
    ]),
  );
  try {
    const lines: string[] = [];
    await start(path, lines);
    assert.deepEqual(twitch.refreshCalls, ['refresh-bot', 'refresh-caster']);
    // Only the refreshed tokens were ever sent to validate: no 401 was spent on the stale ones.
    assert.deepEqual(twitch.validateCalls, ['refresh-bot-fresh-1', 'refresh-caster-fresh-2']);
    assert.ok(lines.some((l) => l.includes('stored bot access token has expired')));

    const onDisk = JSON.parse(await readFile(path, 'utf8')) as TokensFile;
    assert.equal(onDisk.bot.accessToken, 'refresh-bot-fresh-1');
    assert.equal(onDisk.broadcaster.accessToken, 'refresh-caster-fresh-2');
    assert.ok(Date.now() - onDisk.bot.obtainmentTimestamp < 60_000);
  } finally {
    twitch.restore();
  }
});

void test('a 401 on validate of an unexpired token triggers exactly one refresh, then succeeds', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, FRESH),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, FRESH),
  });
  const twitch = mockTwitch(
    // The broadcaster's access token is fine; the bot's was invalidated early (e.g. superseded).
    new Map([['access-caster', { userId: '222', scopes: BROADCASTER_SCOPES }]]),
    new Map([
      ['refresh-bot', { userId: '111', scopes: BOT_SCOPES }],
      ['refresh-caster', { userId: '222', scopes: BROADCASTER_SCOPES }],
    ]),
  );
  try {
    const lines: string[] = [];
    await start(path, lines);
    assert.deepEqual(twitch.refreshCalls, ['refresh-bot'], 'one refresh, for the rejected role only');
    assert.deepEqual(twitch.validateCalls, ['access-bot', 'refresh-bot-fresh-1', 'access-caster']);
    assert.ok(lines.some((l) => l.includes('rejected the stored bot access token (401)')));
    const onDisk = JSON.parse(await readFile(path, 'utf8')) as TokensFile;
    assert.equal(onDisk.bot.accessToken, 'refresh-bot-fresh-1');
    assert.equal(onDisk.broadcaster.accessToken, 'access-caster', 'untouched role is left as it was');
  } finally {
    twitch.restore();
  }
});

void test('a revoked refresh token is fatal with the runbook pointer, one attempt, and no secret in the message', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, EXPIRED),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, EXPIRED),
  });
  const twitch = mockTwitch(
    new Map(),
    // Broadcaster's refresh token still works; the bot's was revoked.
    new Map([['refresh-caster', { userId: '222', scopes: BROADCASTER_SCOPES }]]),
  );
  const errors: string[] = [];
  const originalError = console.error;
  console.error = (...args: unknown[]) => errors.push(args.map(String).join(' '));
  try {
    const error = await start(path).then(
      () => assert.fail('startup should have failed'),
      (e: unknown) => e,
    );
    assert.ok(error instanceof StartupTokenError);
    assert.equal(error.reauthorize, true);
    assert.match(error.message, /FATAL: Twitch refused to refresh the bot token for user 111 \(HTTP 400 "Invalid refresh token"\)/);
    assert.match(error.message, /tools\/authorize\.mts/);
    assert.match(error.message, /infra\/docs\/runbook\.md "Rotate the bot token"/);
    assert.doesNotMatch(error.message, /broadcaster token/, 'the healthy role is not blamed');

    // No retry storm: one refresh request per role, and nothing validated with the dead bot token.
    assert.deepEqual(twitch.refreshCalls, ['refresh-bot', 'refresh-caster']);
    assert.deepEqual(twitch.validateCalls, ['refresh-caster-fresh-1']);

    // Nothing printed may carry a token or the client secret.
    for (const text of [error.message, ...errors]) {
      assert.doesNotMatch(text, /DO-NOT-LOG|refresh-bot|refresh-caster|client_secret=(?!<redacted>)/, text);
    }
    // The role that did refresh is still persisted.
    const onDisk = JSON.parse(await readFile(path, 'utf8')) as TokensFile;
    assert.equal(onDisk.broadcaster.accessToken, 'refresh-caster-fresh-1');
    assert.equal(onDisk.bot.accessToken, 'access-bot');
  } finally {
    console.error = originalError;
    twitch.restore();
  }
});

void test('a refreshed token that validate still rejects is fatal without a second refresh', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, FRESH),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, FRESH),
  });
  const valid = new Map([['access-caster', { userId: '222', scopes: BROADCASTER_SCOPES }]]);
  const twitch = mockTwitch(valid, new Map([['refresh-bot', { userId: '111', scopes: BOT_SCOPES }]]));
  // Make every freshly minted bot token invalid as soon as it is minted.
  const mintingFetch = globalThis.fetch;
  globalThis.fetch = (async (...args: Parameters<typeof fetch>) => {
    const response = await mintingFetch(...args);
    for (const key of [...valid.keys()]) if (key.startsWith('refresh-bot-fresh')) valid.delete(key);
    return response;
  }) as typeof fetch;
  try {
    const error = await start(path).then(
      () => assert.fail('startup should have failed'),
      (e: unknown) => e,
    );
    assert.ok(error instanceof StartupTokenError);
    assert.match(error.message, /freshly refreshed bot token .* rejected \(401\)/);
    assert.deepEqual(twitch.refreshCalls, ['refresh-bot']);
  } finally {
    twitch.restore();
  }
});

void test('both roles on ONE account: each refresh is persisted under its own role, scopes intact', async () => {
  const path = await writeTokens({
    bot: stored('333', 'bot', BOT_SCOPES, EXPIRED),
    broadcaster: stored('333', 'caster', BROADCASTER_SCOPES, EXPIRED),
  });
  const twitch = mockTwitch(
    new Map(),
    new Map([
      ['refresh-bot', { userId: '333', scopes: BOT_SCOPES }],
      ['refresh-caster', { userId: '333', scopes: BROADCASTER_SCOPES }],
    ]),
  );
  try {
    const setup = await start(path);
    assert.equal(setup.sameAccount, true);
    const onDisk = JSON.parse(await readFile(path, 'utf8')) as TokensFile;
    assert.equal(onDisk.bot.userId, '333');
    assert.equal(onDisk.broadcaster.userId, '333');
    assert.equal(onDisk.bot.accessToken, 'refresh-bot-fresh-1');
    assert.deepEqual(onDisk.bot.scope, BOT_SCOPES);
    assert.equal(onDisk.broadcaster.accessToken, 'refresh-caster-fresh-2');
    assert.deepEqual(onDisk.broadcaster.scope, BROADCASTER_SCOPES);

    // A later runtime refresh of one role must not
    // clobber the other role's entry.
    twitch.refreshCalls.length = 0;
    await setup.broadcasterAuthProvider.refreshAccessTokenForUser('333');
    await setup.tokenStore.flush();
    const after = JSON.parse(await readFile(path, 'utf8')) as TokensFile;
    assert.equal(after.bot.accessToken, 'refresh-bot-fresh-1');
    assert.equal(after.broadcaster.accessToken, 'refresh-caster-fresh-3');
  } finally {
    twitch.restore();
  }
});

void test('tokens.json is rewritten atomically at mode 0600 with no temp file left behind', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, EXPIRED),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, EXPIRED),
  });
  const before = await stat(path);
  const twitch = mockTwitch(
    new Map(),
    new Map([
      ['refresh-bot', { userId: '111', scopes: BOT_SCOPES }],
      ['refresh-caster', { userId: '222', scopes: BROADCASTER_SCOPES }],
    ]),
  );
  try {
    await start(path);
    const after = await stat(path);
    assert.equal(after.mode & 0o777, 0o600);
    // A rename puts a new inode in place; an in-place rewrite would keep the old one.
    assert.notEqual(after.ino, before.ino);
    const dir = join(path, '..');
    assert.deepEqual(await readdir(dir), ['tokens.json']);
  } finally {
    twitch.restore();
  }
});

void test('a transient failure (Twitch 5xx on refresh) is fatal for this start but not a re-authorize', async () => {
  const path = await writeTokens({
    bot: stored('111', 'bot', BOT_SCOPES, EXPIRED),
    broadcaster: stored('222', 'caster', BROADCASTER_SCOPES, FRESH),
  });
  const original = globalThis.fetch;
  globalThis.fetch = (async () =>
    new Response('upstream sad', { status: 503, headers: { 'Content-Type': 'text/plain' } })) as typeof fetch;
  try {
    const config = { ...CONFIG_BASE, tokensFile: path };
    const setup = buildAuthProvider(config, await loadTokensFile(path));
    const error = await prepareStartupTokens(config, setup, { log: () => undefined }).then(
      () => assert.fail('should fail'),
      (e: unknown) => e,
    );
    assert.ok(error instanceof StartupTokenError);
    assert.equal(error.reauthorize, false);
    assert.match(error.message, /HTTP 503/);
    assert.doesNotMatch(error.message, /DO-NOT-LOG|refresh-bot/);
  } finally {
    globalThis.fetch = original;
  }
});
