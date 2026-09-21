/**
 * Loads `tokens.json` (written by `tools/authorize.mts`) and builds the `RefreshingAuthProvider`
 * with both identities registered (`docs/design/stage-bridge.md` B1): the bot account (chat) and
 * the broadcaster account (redemptions, predictions, follows). Persists refreshed tokens back to
 * the same file, atomically, 0600.
 */
import { readFile } from 'node:fs/promises';
import type { AccessTokenMaybeWithUserId, AccessTokenWithUserId, AuthProvider } from '@twurple/auth';
import { RefreshingAuthProvider } from '@twurple/auth';
import { getTokenInfo } from '@twurple/auth';
import { atomicWriteJson } from './atomic-file';
import type { BridgeConfig } from './config';
import { assertRequiredScopes, type GrantedScopes } from './scopes';

/** One stored OAuth token, shaped like `@twurple/auth`'s `AccessToken` plus the user id. */
export interface StoredToken {
  userId: string;
  accessToken: string;
  refreshToken: string | null;
  scope: string[];
  expiresIn: number | null;
  obtainmentTimestamp: number;
}

export interface TokensFile {
  bot: StoredToken;
  broadcaster: StoredToken;
}

export async function loadTokensFile(path: string): Promise<TokensFile> {
  let raw: string;
  try {
    raw = await readFile(path, 'utf8');
  } catch (cause) {
    throw new Error(
      `could not read TOKENS_FILE at ${path}: ${(cause as Error).message}. Run tools/authorize.mts first.`,
    );
  }
  const parsed = JSON.parse(raw) as Partial<TokensFile>;
  if (!parsed.bot || !parsed.broadcaster) {
    throw new Error(`${path} is missing "bot" and/or "broadcaster" — re-run tools/authorize.mts`);
  }
  return parsed as TokensFile;
}

export interface AuthSetup {
  /** Carries the bot token only: chat sends and `channel.chat.message`. */
  botAuthProvider: RefreshingAuthProvider;
  /** Carries the broadcaster token only: follows, raids, Channel Points. */
  broadcasterAuthProvider: RefreshingAuthProvider;
  botUserId: string;
  broadcasterUserId: string;
  /** True when one Twitch account holds both roles — see the comment on `buildAuthProvider`. */
  sameAccount: boolean;
  /**
   * Non-null ONLY when both roles are the same Twitch account: a provider that fronts the two
   * per-role providers and picks between them by the scopes each call asks for, so a single
   * EventSub listener can carry both roles' subscriptions on ONE websocket transport
   * (`src/eventsub.ts`, `createRoleRoutingAuthProvider` below). `null` when the accounts differ,
   * because then twurple's own per-user socket keying already gives one socket per account and
   * there is nothing to merge.
   */
  sharedEventSubAuthProvider: AuthProvider | null;
}

/**
 * Build ONE `RefreshingAuthProvider` PER ROLE, each holding exactly one token.
 *
 * This used to be a single provider with both tokens added to it, which is correct only while the
 * bot and the broadcaster are different Twitch accounts. They are not required to be: the first
 * live channel (`<twitch-channel>`, 2026-09-16) runs both roles on ONE account, so the two
 * `addUser` calls carried the SAME user id — and twurple keys its token map by user id, so the
 * second call silently REPLACED the first. The surviving token was the broadcaster's, which holds
 * `channel:bot`/`moderator:read:followers`/`channel:*:redemptions` and none of
 * `user:read:chat`/`user:write:chat`/`user:bot`. The startup scope assertion still passed, because
 * `assertStartupScopes` calls `getTokenInfo` on each stored token separately and never asks the
 * provider what it would actually hand out; the failure landed later, on the first chat send and
 * on the `channel.chat.message` subscription, as a missing-scope error.
 *
 * One provider per role cannot collide: each holds a single user id and a single token, whether or
 * not the two roles are the same account. The cost is one `ApiClient` and one EventSub WebSocket
 * per role (`src/eventsub.ts`), which is what Twitch's own model expects anyway — a subscription
 * is authorized by the token of the user it names.
 */
export function buildAuthProvider(config: Pick<BridgeConfig, 'twitchClientId' | 'twitchClientSecret' | 'tokensFile'>, tokens: TokensFile): AuthSetup {
  const newProvider = (): RefreshingAuthProvider => {
    const provider = new RefreshingAuthProvider({
      clientId: config.twitchClientId,
      clientSecret: config.twitchClientSecret,
    });
    provider.onRefresh((userId, newTokenData) => {
      void persistRefreshedToken(config.tokensFile, tokens, userId, newTokenData).catch((cause: unknown) => {
        console.error(`flybridge: failed to persist refreshed token for ${userId}: ${String(cause)}`);
      });
    });
    return provider;
  };

  const botAuthProvider = newProvider();
  botAuthProvider.addUser(tokens.bot.userId, toAccessToken(tokens.bot), ['chat']);

  const broadcasterAuthProvider = newProvider();
  broadcasterAuthProvider.addUser(tokens.broadcaster.userId, toAccessToken(tokens.broadcaster), ['broadcaster']);

  const sameAccount = tokens.bot.userId === tokens.broadcaster.userId;

  return {
    botAuthProvider,
    broadcasterAuthProvider,
    botUserId: tokens.bot.userId,
    broadcasterUserId: tokens.broadcaster.userId,
    sameAccount,
    sharedEventSubAuthProvider: sameAccount
      ? createRoleRoutingAuthProvider({
          clientId: config.twitchClientId,
          bot: { provider: botAuthProvider, scopes: tokens.bot.scope },
          broadcaster: { provider: broadcasterAuthProvider, scopes: tokens.broadcaster.scope },
        })
      : null,
  };
}

export interface RoleRoutingRole {
  provider: AuthProvider;
  /** The scopes this role's stored token actually carries, from `tokens.json`. */
  scopes: readonly string[];
}

export interface RoleRoutingAuthProviderOptions {
  clientId: string;
  bot: RoleRoutingRole;
  broadcaster: RoleRoutingRole;
}

/**
 * An `AuthProvider` that fronts the two per-role providers and routes each request to whichever
 * role's token carries the SCOPES the request asked for.
 *
 * WHY THIS EXISTS. `EventSubWsListener` opens one websocket per distinct auth user id, so two
 * listeners for one Twitch account meant two transports against Twitch's per-user limit of three
 * — the doubling that turned a single 1006 disconnect into the hour of dead chat on 2026-09-16
 * (see `src/subscription-health.ts`). One listener means one transport, and one listener takes
 * exactly one `ApiClient`, hence one `AuthProvider`. It cannot be a `RefreshingAuthProvider`
 * holding both tokens: that provider keys its token map by user id, so the second `addUser` for
 * the same account silently replaces the first — the bug the long comment on `buildAuthProvider`
 * above is about, and which must not be re-introduced from the other end.
 *
 * WHY ROUTING BY SCOPE WORKS. Every Helix call twurple makes to create an EventSub subscription
 * goes through `BaseApiClient` with `forceType: 'user'` and the endpoint's required scope set, and
 * `BaseApiClient` hands that scope set straight to `AuthProvider.getAccessTokenForUser`
 * (@twurple/api 8.1.4). So the request says what it needs: `channel.chat.message` asks for
 * `user:read:chat` (bot), `channel.follow` v2 for `moderator:read:followers` (broadcaster), the
 * Channel Points redemption for `channel:{read,manage}:redemptions` (broadcaster). `channel.raid`
 * asks for no scope at all — it needs only *a* user token — and gets the default role.
 *
 * WHAT IS DELIBERATELY NOT IMPLEMENTED. `refreshAccessTokenForUser` is optional on the interface
 * and is omitted: its only argument is a user id, which says nothing about the role when both
 * roles are one account, so any implementation would be a coin flip that could hand back a token
 * missing the scope the caller needed. Leaving it out means `BaseApiClient` skips the pre-emptive
 * refresh and surfaces a 401 instead of guessing — and it costs nothing, because
 * `RefreshingAuthProvider.getAccessTokenForUser` already refreshes an expired or
 * scope-insufficient token before returning it, and this provider delegates every call to it.
 */
export function createRoleRoutingAuthProvider(options: RoleRoutingAuthProviderOptions): AuthProvider {
  const { clientId, bot, broadcaster } = options;

  const covers = (granted: readonly string[], requested: readonly string[]): boolean =>
    requested.every((scope) => granted.includes(scope));

  const pick = (scopeSets: Array<string[] | undefined>): AuthProvider => {
    const requested = scopeSets.flatMap((set) => set ?? []);
    // No scopes requested: any user token for this account will do (`channel.raid`). The
    // broadcaster is the default because it owns the channel-level subscriptions.
    if (requested.length === 0) return broadcaster.provider;
    if (covers(bot.scopes, requested)) return bot.provider;
    if (covers(broadcaster.scopes, requested)) return broadcaster.provider;
    // Neither token covers it, which is a missing-scope misconfiguration `assertRequiredScopes`
    // should already have refused at startup. Delegate anyway, so the error that reaches the log
    // is twurple's own named missing-scope error rather than a null token from here.
    console.error(
      `flybridge: no stored token carries all of [${requested.join(', ')}]; routing to the broadcaster token`,
    );
    return broadcaster.provider;
  };

  return {
    clientId,
    // The union, because that is what this provider can actually supply for the account. Callers
    // use it as a capability check, and answering with one role's scopes would under-report.
    getCurrentScopesForUser: (): string[] => [...new Set([...bot.scopes, ...broadcaster.scopes])],
    getAccessTokenForUser: async (
      user,
      ...scopeSets: Array<string[] | undefined>
    ): Promise<AccessTokenWithUserId | null> => await pick(scopeSets).getAccessTokenForUser(user, ...scopeSets),
    getAnyAccessToken: async (user?): Promise<AccessTokenMaybeWithUserId> =>
      await broadcaster.provider.getAnyAccessToken(user),
  };
}

function toAccessToken(token: StoredToken): {
  accessToken: string;
  refreshToken: string | null;
  scope: string[];
  expiresIn: number | null;
  obtainmentTimestamp: number;
} {
  return {
    accessToken: token.accessToken,
    refreshToken: token.refreshToken,
    scope: token.scope,
    expiresIn: token.expiresIn,
    obtainmentTimestamp: token.obtainmentTimestamp,
  };
}

/**
 * Persist a refreshed token back into `tokens.json`.
 *
 * The role is identified by the token's SCOPE SET, not by its user id: when both roles are the
 * same account (the `<twitch-channel>` case) the id says nothing, and writing the refreshed
 * broadcaster token over the `bot` entry would destroy the chat scopes on disk — a corruption that
 * survives a restart, unlike the in-memory collision `buildAuthProvider` describes. Scopes are
 * preserved exactly across a refresh, so they are a reliable discriminator.
 */
async function persistRefreshedToken(
  tokensFilePath: string,
  tokens: TokensFile,
  userId: string,
  newToken: { accessToken: string; refreshToken: string | null; scope: string[]; expiresIn: number | null; obtainmentTimestamp: number },
): Promise<void> {
  const key = roleForRefreshedToken(tokens, userId, newToken.scope);
  if (!key) {
    console.error(
      `flybridge: refreshed token for ${userId} matches neither stored role by scope set; not persisting`,
    );
    return;
  }
  tokens[key] = { userId, ...newToken };
  await atomicWriteJson(tokensFilePath, tokens);
}

function roleForRefreshedToken(
  tokens: TokensFile,
  userId: string,
  scope: readonly string[],
): keyof TokensFile | null {
  const sameSet = (a: readonly string[], b: readonly string[]): boolean =>
    a.length === b.length && [...a].sort().join(' ') === [...b].sort().join(' ');
  if (sameSet(tokens.bot.scope, scope)) return 'bot';
  if (sameSet(tokens.broadcaster.scope, scope)) return 'broadcaster';
  // Different accounts: the id is unambiguous, so fall back to it.
  if (tokens.bot.userId !== tokens.broadcaster.userId) {
    return tokens.bot.userId === userId ? 'bot' : 'broadcaster';
  }
  return null;
}

/**
 * Startup scope assertion via `getTokenInfo` (`docs/design/stage-bridge.md` B1): refuses to
 * start with a named missing scope rather than failing at the first subscription.
 */
export async function assertStartupScopes(
  config: Pick<BridgeConfig, 'twitchClientId' | 'featureRedemptions' | 'featurePredictions'>,
  tokens: TokensFile,
): Promise<void> {
  const [botInfo, broadcasterInfo] = await Promise.all([
    getTokenInfo(tokens.bot.accessToken, config.twitchClientId),
    getTokenInfo(tokens.broadcaster.accessToken, config.twitchClientId),
  ]);
  const granted: GrantedScopes = { bot: botInfo.scopes, broadcaster: broadcasterInfo.scopes };
  assertRequiredScopes(config, granted);
}
