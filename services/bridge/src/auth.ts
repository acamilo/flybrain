/**
 * Loads `tokens.json` (written by `tools/authorize.mts`) and builds the `RefreshingAuthProvider`
 * with both identities registered (`docs/design/stage-bridge.md` B1): the bot account (chat) and
 * the broadcaster account (redemptions, predictions, follows). Persists refreshed tokens back to
 * the same file, atomically, 0600 — for BOTH roles, including when they are one account.
 *
 * On load, a stored access token that has expired, or that Twitch's validate endpoint rejects with
 * 401, is refreshed with its refresh token before anything gives up (`prepareStartupTokens`). Only
 * a refresh that Twitch itself refuses is fatal, with one line pointing at the runbook.
 */
import { readFile } from 'node:fs/promises';
import type { AccessToken, AccessTokenMaybeWithUserId, AccessTokenWithUserId, AuthProvider } from '@twurple/auth';
import { accessTokenIsExpired, getTokenInfo, InvalidTokenError, RefreshingAuthProvider } from '@twurple/auth';
import { atomicWriteJson } from './atomic-file';
import type { BridgeConfig } from './config';
import { safeErrorMessage } from './redact';
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

export type TokenRole = keyof TokensFile;
const ROLES: readonly TokenRole[] = ['bot', 'broadcaster'];

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
  /** Where every refresh, by either role, is persisted; `flush()` waits for the writes. */
  tokenStore: TokenStore;
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
 * the startup scope assertion called `getTokenInfo` on each stored token separately and never asked the
 * provider what it would actually hand out; the failure landed later, on the first chat send and
 * on the `channel.chat.message` subscription, as a missing-scope error.
 *
 * One provider per role cannot collide: each holds a single user id and a single token, whether or
 * not the two roles are the same account. The cost is one `ApiClient` and one EventSub WebSocket
 * per role (`src/eventsub.ts`), which is what Twitch's own model expects anyway — a subscription
 * is authorized by the token of the user it names.
 */
export function buildAuthProvider(config: Pick<BridgeConfig, 'twitchClientId' | 'twitchClientSecret' | 'tokensFile'>, tokens: TokensFile): AuthSetup {
  const tokenStore = new TokenStore(config.tokensFile, tokens);

  // The ROLE is fixed per provider, so a refresh is persisted under the role whose provider did
  // it. This replaced guessing the role from the refreshed token's scope set, which is all a
  // single shared callback could go on when both roles are one account.
  const newProvider = (role: TokenRole): RefreshingAuthProvider => {
    const provider = new RefreshingAuthProvider({
      clientId: config.twitchClientId,
      clientSecret: config.twitchClientSecret,
    });
    provider.onRefresh((userId, newTokenData) => {
      tokenStore.update(role, userId, newTokenData);
    });
    provider.onRefreshFailure((userId, error) => {
      console.error(runtimeRefreshFailureLine(role, userId, error));
    });
    return provider;
  };

  const botAuthProvider = newProvider('bot');
  botAuthProvider.addUser(tokens.bot.userId, toAccessToken(tokens.bot), ['chat']);

  const broadcasterAuthProvider = newProvider('broadcaster');
  broadcasterAuthProvider.addUser(tokens.broadcaster.userId, toAccessToken(tokens.broadcaster), ['broadcaster']);

  const sameAccount = tokens.bot.userId === tokens.broadcaster.userId;

  return {
    botAuthProvider,
    broadcasterAuthProvider,
    tokenStore,
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

function toAccessToken(token: StoredToken): AccessToken {
  return {
    accessToken: token.accessToken,
    refreshToken: token.refreshToken,
    scope: token.scope,
    expiresIn: token.expiresIn,
    obtainmentTimestamp: token.obtainmentTimestamp,
  };
}

/**
 * The in-memory copy of `tokens.json` and its only writer at runtime.
 *
 * Every refresh, by either role, lands here keyed by ROLE (never by user id — when both roles are
 * one account the id says nothing, and writing the broadcaster token over the `bot` entry would
 * destroy the chat scopes on disk). Writes are serialized, and each writes the WHOLE current
 * state, so two refreshes that land together (both roles refreshed on load) cannot race each
 * other's renames and leave one role's new token unpersisted — the v0.2.3 incident, where only the
 * bot role's fresher token reached disk and the next start failed on the broadcaster's.
 */
export class TokenStore {
  private chain: Promise<void> = Promise.resolve();
  private lastError: unknown = null;

  constructor(
    private readonly path: string,
    private readonly tokens: TokensFile,
  ) {}

  /** The current (possibly refreshed) token for a role. */
  get(role: TokenRole): StoredToken {
    return this.tokens[role];
  }

  update(role: TokenRole, userId: string, token: AccessToken): void {
    this.tokens[role] = {
      userId,
      accessToken: token.accessToken,
      refreshToken: token.refreshToken ?? this.tokens[role].refreshToken,
      // twurple turns a refresh response without `scope` into []; never let that wipe the scopes
      // the role routing and the startup assertion read from disk.
      scope: token.scope.length > 0 ? token.scope : this.tokens[role].scope,
      expiresIn: token.expiresIn,
      obtainmentTimestamp: token.obtainmentTimestamp,
    };
    this.chain = this.chain.then(async () => {
      try {
        await atomicWriteJson(this.path, this.tokens);
      } catch (cause) {
        this.lastError = cause;
        console.error(`flybridge: failed to persist the refreshed ${role} token to ${this.path}: ${safeErrorMessage(cause)}`);
      }
    });
  }

  /** Resolves once every queued write has finished; rejects if any of them failed. */
  async flush(): Promise<void> {
    await this.chain;
    if (this.lastError !== null) {
      const cause = this.lastError;
      this.lastError = null;
      throw new Error(`could not persist refreshed tokens to ${this.path}: ${safeErrorMessage(cause)}`);
    }
  }
}

const REAUTHORIZE_HINT =
  'Restarting will not fix this: re-authorize that role with tools/authorize.mts, see ' +
  'infra/docs/runbook.md "Rotate the bot token".';

/**
 * `@twurple/api-call`'s `HttpStatusCodeError`, recognised by shape: that package is only a
 * transitive dependency here, and the esbuild bundle would inline a second copy of it, against
 * which `instanceof` never matches the one `@twurple/auth` (external) throws.
 */
interface TwitchHttpError extends Error {
  statusCode: number;
  body: string;
}

function isTwitchHttpError(error: unknown): error is TwitchHttpError {
  return (
    error instanceof Error &&
    error.name === 'HttpStatusCodeError' &&
    typeof (error as Partial<TwitchHttpError>).statusCode === 'number'
  );
}

/** Status code and Twitch's own message, never the URL (which carries the secret and the token). */
function describeTwitchError(error: unknown): string {
  if (isTwitchHttpError(error)) {
    let message = '';
    try {
      const body = JSON.parse(String(error.body)) as { message?: unknown };
      if (typeof body.message === 'string') message = body.message;
    } catch {
      // not JSON: say nothing more than the status
    }
    return `HTTP ${String(error.statusCode)}${message ? ` "${message}"` : ''}`;
  }
  return safeErrorMessage(error);
}

/** Twitch answered the refresh grant itself with a 4xx: the refresh token is dead. */
function isRefreshRefusal(error: unknown): boolean {
  return isTwitchHttpError(error) && error.statusCode >= 400 && error.statusCode < 500;
}

function refreshFailureLine(role: TokenRole, userId: string, error: unknown): string {
  return (
    `flybridge: FATAL: Twitch refused to refresh the ${role} token for user ${userId} ` +
    `(${describeTwitchError(error)}): the refresh token is revoked or invalid. ${REAUTHORIZE_HINT}`
  );
}

/** What `onRefreshFailure` logs, at startup (before the FATAL line) and at runtime alike. */
function runtimeRefreshFailureLine(role: TokenRole, userId: string, error: unknown): string {
  const base = `flybridge: refreshing the ${role} token for user ${userId} failed (${describeTwitchError(error)})`;
  return isRefreshRefusal(error)
    ? `${base}; Twitch calls as ${role} will fail until it is re-authorized (infra/docs/runbook.md "Rotate the bot token")`
    : base;
}

/**
 * A stored token that could not be brought back to life on load. Its message is the whole log line
 * and carries no token, secret or URL, so the entrypoint prints it as-is. `reauthorize` is true
 * when Twitch refused the refresh (restarting cannot help); false for a transient failure (network,
 * 5xx), which the next start retries.
 */
export class StartupTokenError extends Error {
  constructor(
    message: string,
    readonly reauthorize: boolean,
  ) {
    super(message);
    this.name = 'StartupTokenError';
  }
}

export interface PrepareStartupTokensOptions {
  /** Informational lines; defaults to `console.log`. */
  log?: (line: string) => void;
}

/**
 * Make both stored tokens usable before the bridge does anything else, and return the scopes
 * Twitch reports for each (for `assertRequiredScopes`).
 *
 * Per role, at most ONE refresh request:
 *  - expired on disk (obtainmentTimestamp + expiresIn, twurple's one-minute grace) → the role's
 *    `RefreshingAuthProvider` refreshes it inside `getAccessTokenForUser`, before any validate;
 *  - otherwise validate it; a 401 → `refreshAccessTokenForUser` once, then validate the new token.
 * Every refresh goes through the role's provider, so `onRefresh` persists it under that role, and
 * the writes are flushed before this returns. A refresh Twitch refuses (4xx: revoked or invalid
 * refresh token) is fatal with a pointer to the runbook; twurple then also caches the failure, so
 * nothing in this process asks again. Both roles are attempted before failing, so one start names
 * every dead role.
 *
 * This replaces validating the stored access tokens first, which crash-looped the bridge on 401
 * whenever it had been down longer than an access token lives (~4 h), even though both refresh
 * tokens were still good (v0.2.3 and 2026-09-28).
 */
export async function prepareStartupTokens(
  config: Pick<BridgeConfig, 'twitchClientId'>,
  setup: Pick<AuthSetup, 'botAuthProvider' | 'broadcasterAuthProvider' | 'botUserId' | 'broadcasterUserId' | 'tokenStore'>,
  options: PrepareStartupTokensOptions = {},
): Promise<GrantedScopes> {
  const log = options.log ?? ((line: string) => console.log(line));
  const roles = {
    bot: { provider: setup.botAuthProvider, userId: setup.botUserId },
    broadcaster: { provider: setup.broadcasterAuthProvider, userId: setup.broadcasterUserId },
  };

  const granted: Partial<GrantedScopes> = {};
  const failures: StartupTokenError[] = [];

  for (const role of ROLES) {
    const { provider, userId } = roles[role];
    const fail = (error: unknown, what: string): StartupTokenError =>
      isRefreshRefusal(error)
        ? new StartupTokenError(refreshFailureLine(role, userId, error), true)
        : new StartupTokenError(
            `flybridge: FATAL: ${what} for the ${role} token (user ${userId}) failed: ${describeTwitchError(error)}`,
            false,
          );

    try {
      const expiredOnDisk = accessTokenIsExpired(setup.tokenStore.get(role));
      if (expiredOnDisk) log(`flybridge: the stored ${role} access token has expired; refreshing it before validating`);
      const stored = await provider.getAccessTokenForUser(userId).catch((error: unknown) => {
        throw fail(error, 'refreshing the expired stored token');
      });
      if (stored === null) throw new StartupTokenError(`flybridge: FATAL: no ${role} token was registered`, false);
      // Nothing was spent on validate yet: twurple refreshed an expired token before returning it.
      let token: AccessToken = stored;

      let info;
      try {
        info = await getTokenInfo(token.accessToken, config.twitchClientId);
      } catch (error) {
        if (!(error instanceof InvalidTokenError)) throw fail(error, 'validating');
        if (expiredOnDisk) {
          // It was refreshed a moment ago and Twitch still says 401: a second refresh would only
          // repeat the first.
          throw new StartupTokenError(
            `flybridge: FATAL: the freshly refreshed ${role} token for user ${userId} was rejected (401). ${REAUTHORIZE_HINT}`,
            true,
          );
        }
        log(`flybridge: Twitch rejected the stored ${role} access token (401); refreshing it once`);
        token = await provider.refreshAccessTokenForUser(userId).catch((refreshError: unknown) => {
          throw fail(refreshError, 'refreshing the rejected token');
        });
        info = await getTokenInfo(token.accessToken, config.twitchClientId).catch((again: unknown) => {
          throw again instanceof InvalidTokenError
            ? new StartupTokenError(
                `flybridge: FATAL: the freshly refreshed ${role} token for user ${userId} was also rejected (401). ${REAUTHORIZE_HINT}`,
                true,
              )
            : fail(again, 'validating the refreshed token');
        });
      }
      granted[role] = info.scopes;
    } catch (error) {
      if (error instanceof StartupTokenError) failures.push(error);
      else throw error;
    }
  }

  // Persist what did refresh even if the other role is dead: its new token is still good.
  await setup.tokenStore.flush();

  if (failures.length > 0) {
    throw new StartupTokenError(
      failures.map((f) => f.message).join('\n'),
      failures.some((f) => f.reauthorize),
    );
  }
  return granted as GrantedScopes;
}

/**
 * Startup token check (`docs/design/stage-bridge.md` B1): refresh-before-give-up on both stored
 * tokens (`prepareStartupTokens`), then refuse to start with a named missing scope rather than
 * failing at the first subscription.
 */
export async function assertStartupScopes(
  config: Pick<BridgeConfig, 'twitchClientId' | 'featureRedemptions' | 'featurePredictions'>,
  setup: Parameters<typeof prepareStartupTokens>[1],
  options: PrepareStartupTokensOptions = {},
): Promise<void> {
  const granted = await prepareStartupTokens(config, setup, options);
  assertRequiredScopes(config, granted);
}
