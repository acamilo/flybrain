/**
 * `buildAuthProvider`'s same-account handling and the scope-routing provider it builds
 * (`src/auth.ts`).
 *
 * The routing provider is what lets ONE EventSub websocket carry both roles' subscriptions when
 * both roles are one Twitch account — the transport saving in `src/eventsub.ts`. It must route by
 * the SCOPES a Helix call asks for, because the user id says nothing when the roles share an
 * account, and getting that wrong reproduces the missing-scope failure the long comment on
 * `buildAuthProvider` is about from the other direction.
 */
import assert from 'node:assert/strict';
import test from 'node:test';
import type { AccessTokenMaybeWithUserId, AccessTokenWithUserId, AuthProvider } from '@twurple/auth';
import { buildAuthProvider, createRoleRoutingAuthProvider, type TokensFile } from '../src/auth';

const CLIENT_ID = 'test-client-id';
const BOT_SCOPES = ['user:read:chat', 'user:write:chat', 'user:bot'];
const BROADCASTER_SCOPES = [
  'channel:bot',
  'moderator:read:followers',
  'channel:read:redemptions',
  'channel:manage:redemptions',
];

/** A provider that only records what it was asked for and hands back a labelled token. */
function stubProvider(label: string): AuthProvider & { calls: Array<Array<string[] | undefined>> } {
  const calls: Array<Array<string[] | undefined>> = [];
  return {
    calls,
    clientId: CLIENT_ID,
    getCurrentScopesForUser: () => [],
    getAccessTokenForUser: async (
      user,
      ...scopeSets: Array<string[] | undefined>
    ): Promise<AccessTokenWithUserId | null> => {
      calls.push(scopeSets);
      return {
        accessToken: label,
        refreshToken: null,
        scope: [],
        expiresIn: null,
        obtainmentTimestamp: 0,
        userId: String(user),
      };
    },
    getAnyAccessToken: async (): Promise<AccessTokenMaybeWithUserId> => ({
      accessToken: `${label}-any`,
      refreshToken: null,
      scope: [],
      expiresIn: null,
      obtainmentTimestamp: 0,
      userId: undefined,
    }),
  };
}

function routing(): {
  provider: AuthProvider;
  bot: ReturnType<typeof stubProvider>;
  broadcaster: ReturnType<typeof stubProvider>;
} {
  const bot = stubProvider('bot-token');
  const broadcaster = stubProvider('broadcaster-token');
  const provider = createRoleRoutingAuthProvider({
    clientId: CLIENT_ID,
    bot: { provider: bot, scopes: BOT_SCOPES },
    broadcaster: { provider: broadcaster, scopes: BROADCASTER_SCOPES },
  });
  return { provider, bot, broadcaster };
}

async function tokenFor(provider: AuthProvider, ...scopeSets: Array<string[] | undefined>): Promise<string> {
  const token = await provider.getAccessTokenForUser('<broadcaster-id>', ...scopeSets);
  assert.ok(token !== null);
  return token.accessToken;
}

void test('channel.chat.message routes to the bot token (user:read:chat)', async () => {
  const { provider } = routing();
  assert.equal(await tokenFor(provider, ['user:read:chat']), 'bot-token');
});

void test('channel.follow v2 routes to the broadcaster token (moderator:read:followers)', async () => {
  const { provider } = routing();
  assert.equal(await tokenFor(provider, ['moderator:read:followers']), 'broadcaster-token');
});

void test('the Channel Points redemption subscription routes to the broadcaster token', async () => {
  const { provider } = routing();
  assert.equal(
    await tokenFor(provider, ['channel:read:redemptions', 'channel:manage:redemptions']),
    'broadcaster-token',
  );
});

void test('channel.raid, which needs no scope, gets the broadcaster token', async () => {
  const { provider } = routing();
  assert.equal(await tokenFor(provider, undefined), 'broadcaster-token');
  assert.equal(await tokenFor(provider), 'broadcaster-token');
});

void test('the requested scope set is passed through, so the role provider can refresh for it', async () => {
  const { provider, bot } = routing();
  await tokenFor(provider, ['user:read:chat']);
  assert.deepEqual(bot.calls, [[['user:read:chat']]]);
});

void test('a scope neither token carries falls back to the broadcaster rather than returning null', async () => {
  const { provider } = routing();
  assert.equal(await tokenFor(provider, ['channel:manage:predictions']), 'broadcaster-token');
});

void test('getCurrentScopesForUser reports the union of both role tokens', () => {
  const { provider } = routing();
  const scopes = provider.getCurrentScopesForUser('<broadcaster-id>');
  for (const scope of [...BOT_SCOPES, ...BROADCASTER_SCOPES]) assert.ok(scopes.includes(scope), scope);
  assert.equal(scopes.length, BOT_SCOPES.length + BROADCASTER_SCOPES.length);
});

void test('refreshAccessTokenForUser is deliberately not implemented', () => {
  const { provider } = routing();
  // A user id cannot say which role to refresh when both roles are one account, so guessing is
  // worse than letting @twurple/api surface the 401. See src/auth.ts.
  assert.equal(provider.refreshAccessTokenForUser, undefined);
});

function tokensFile(botUserId: string, broadcasterUserId: string): TokensFile {
  const token = (userId: string, scope: string[]) => ({
    userId,
    accessToken: `access-${userId}-${scope[0] ?? 'none'}`,
    refreshToken: `refresh-${userId}`,
    scope,
    expiresIn: 14_400,
    // Not expired: a real `RefreshingAuthProvider` would otherwise try to refresh against Twitch.
    obtainmentTimestamp: Date.now(),
  });
  return {
    bot: token(botUserId, BOT_SCOPES),
    broadcaster: token(broadcasterUserId, BROADCASTER_SCOPES),
  };
}

const CONFIG = { twitchClientId: CLIENT_ID, twitchClientSecret: 'secret', tokensFile: '/tmp/unused-tokens.json' };

void test('buildAuthProvider builds the shared EventSub provider only for one account', () => {
  const same = buildAuthProvider(CONFIG, tokensFile('<broadcaster-id>', '<broadcaster-id>'));
  assert.equal(same.sameAccount, true);
  assert.notEqual(same.sharedEventSubAuthProvider, null);
  assert.notEqual(same.botAuthProvider, same.broadcasterAuthProvider, 'still one provider per role');

  const split = buildAuthProvider(CONFIG, tokensFile('999000111', '<broadcaster-id>'));
  assert.equal(split.sameAccount, false);
  assert.equal(split.sharedEventSubAuthProvider, null);
});

void test('the shared provider routes to the real per-role providers by scope', async () => {
  const { sharedEventSubAuthProvider } = buildAuthProvider(CONFIG, tokensFile('<broadcaster-id>', '<broadcaster-id>'));
  assert.ok(sharedEventSubAuthProvider !== null);
  // The stored tokens are unexpired, so the real `RefreshingAuthProvider`s hand back exactly what
  // tokens.json held — which is how we can tell which role answered.
  const chat = await sharedEventSubAuthProvider.getAccessTokenForUser('<broadcaster-id>', ['user:read:chat']);
  const follows = await sharedEventSubAuthProvider.getAccessTokenForUser('<broadcaster-id>', [
    'moderator:read:followers',
  ]);
  assert.equal(chat?.accessToken, 'access-<broadcaster-id>-user:read:chat');
  assert.equal(follows?.accessToken, 'access-<broadcaster-id>-channel:bot');
});
