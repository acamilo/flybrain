/**
 * Startup scope assertion (`docs/design/stage-bridge.md` B1 scope table).
 *
 * Two identities are registered on the auth provider: the bot account (chat) and the
 * broadcaster account (redemptions, predictions, follows). Rather than discovering a missing
 * scope the first time a subscription fails, flybridge calls `getTokenInfo` on both tokens at
 * startup and refuses to start, naming every missing scope, when one is absent.
 */
import type { BridgeConfig } from './config';

export type TokenKind = 'bot' | 'broadcaster';

export interface ScopeRequirement {
  scope: string;
  token: TokenKind;
  neededFor: string;
  /** Whether this scope is required given the current feature flags. Always-on scopes ignore `config`. */
  requiredWhen: (config: Pick<BridgeConfig, 'featureRedemptions' | 'featurePredictions'>) => boolean;
}

/** The B1 scope table. */
export const SCOPE_TABLE: readonly ScopeRequirement[] = [
  { scope: 'user:read:chat', token: 'bot', neededFor: 'channel.chat.message over EventSub WS', requiredWhen: () => true },
  { scope: 'user:write:chat', token: 'bot', neededFor: 'Send Chat Message', requiredWhen: () => true },
  { scope: 'user:bot', token: 'bot', neededFor: 'send as bot', requiredWhen: () => true },
  { scope: 'channel:bot', token: 'broadcaster', neededFor: 'authorize the bot on the channel', requiredWhen: () => true },
  {
    scope: 'moderator:read:followers',
    token: 'broadcaster',
    neededFor: 'channel.follow v2',
    requiredWhen: () => true,
  },
  {
    scope: 'channel:read:redemptions',
    token: 'broadcaster',
    neededFor: 'channel.channel_points_custom_reward_redemption.add',
    requiredWhen: (config) => config.featureRedemptions,
  },
  {
    scope: 'channel:manage:redemptions',
    token: 'broadcaster',
    neededFor: 'create the Sugar reward, fulfil/refund',
    requiredWhen: (config) => config.featureRedemptions,
  },
  {
    scope: 'channel:manage:broadcast',
    token: 'broadcaster',
    neededFor: 'stream markers',
    requiredWhen: (config) => config.featureRedemptions,
  },
  {
    scope: 'channel:manage:predictions',
    token: 'broadcaster',
    neededFor: 'predictions (Affiliate-gated, feature-flagged off)',
    requiredWhen: (config) => config.featurePredictions,
  },
] as const;

export class MissingScopesError extends Error {
  readonly missing: readonly ScopeRequirement[];

  constructor(missing: readonly ScopeRequirement[]) {
    const lines = missing.map((req) => `${req.token} token missing "${req.scope}" (needed for: ${req.neededFor})`);
    super(`flybridge cannot start: missing Twitch scopes:\n  - ${lines.join('\n  - ')}`);
    this.name = 'MissingScopesError';
    this.missing = missing;
  }
}

export interface GrantedScopes {
  bot: readonly string[];
  broadcaster: readonly string[];
}

/**
 * Check granted scopes against `SCOPE_TABLE`, filtered by which features are enabled. Throws
 * `MissingScopesError` naming every missing scope (not just the first) when any are absent.
 */
export function assertRequiredScopes(
  config: Pick<BridgeConfig, 'featureRedemptions' | 'featurePredictions'>,
  granted: GrantedScopes,
): void {
  const missing = SCOPE_TABLE.filter((req) => {
    if (!req.requiredWhen(config)) return false;
    const grantedForToken = req.token === 'bot' ? granted.bot : granted.broadcaster;
    return !grantedForToken.includes(req.scope);
  });
  if (missing.length > 0) throw new MissingScopesError(missing);
}
