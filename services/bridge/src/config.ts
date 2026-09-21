/**
 * Env-driven configuration for flybridge, validated once at startup.
 *
 * Three ways to supply the Twitch client id/secret:
 *  - Production (systemd), the shape infra actually installs: `LoadCredentialEncrypted=twitch-app:...`
 *    drops ONE decrypted file named `twitch-app` under `$CREDENTIALS_DIRECTORY` holding two lines,
 *    `<client_id>\n<client_secret>\n` — see `infra/06-secrets.sh`, which writes exactly that, and
 *    `docs/design/infra.md` section 3's secrets table, which names the single
 *    `/etc/fly/creds/twitch-app.cred`. Never logged.
 *  - Production (systemd), the split shape: `LoadCredential=twitch-client-id:...` /
 *    `twitch-client-secret:...` drops two files named `twitch-client-id` / `twitch-client-secret`
 *    (`docs/design/stage-bridge.md` section B1). Preferred when both are present.
 *  - Dev: `TWITCH_CLIENT_ID` / `TWITCH_CLIENT_SECRET` env vars directly.
 *
 * 06-secrets.sh's own header flagged this as an unreconciled deviation between the two design docs
 * ("flybridge's own implementation ... will need to parse that shape, or the two docs need
 * reconciling before P3"). Both shapes are read here, so neither doc has to lose.
 *
 * Every other setting is plain env, with defaults matching the design doc and
 * `docs/control-api.md`. `loadConfig()` collects every problem before throwing, rather than
 * failing on the first one, so a misconfigured deploy gets one clear error list instead of a
 * fix-one-rerun-find-the-next loop.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { DEFAULT_NOTICE_MIN_INTERVAL_MS } from './notice';
import { DEFAULT_EVENTSUB_GRACE_MS } from './subscription-health';

/** Game-specific words (channel, bot account, title used in templates) never belong in code. */
export interface BridgeConfig {
  /** Twitch application client id. Never logged. */
  twitchClientId: string;
  /** Twitch application client secret. Never logged. */
  twitchClientSecret: string;
  /** Path to the persisted refresh-token store written by `tools/authorize.mts`. */
  tokensFile: string;
  /** Broadcaster channel login (no leading `#`). */
  channel: string;
  /** Bot account login used to send chat messages. */
  botUser: string;
  /** Base URL of flysim's localhost control API (`docs/control-api.md`). */
  simControlUrl: string;
  /** Per-call timeout for the control API client (`src/sim.ts`). */
  simTimeoutMs: number;
  /** Game title substituted into templates (`docs/stream-mvp-plan.md`: keep game words out of code). */
  gameTitle: string;
  /** Whether to create/subscribe to the Sugar Channel Points reward (`src/redemptions.ts`). */
  featureRedemptions: boolean;
  /** Whether Predictions are enabled (`src/predictions.ts`, disabled in B1: Affiliate-only). */
  featurePredictions: boolean;
  /**
   * Whether AutoMod-passed chat is forwarded to flysim's `POST /chat` for the on-screen CHAT
   * panel (`src/onscreen-chat.ts`). Default **true**. flysim has its own independent kill switch
   * (`[chat] enabled`), so turning the panel off can be done from either end.
   */
  featureOnscreenChat: boolean;
  /**
   * Quiet mode (`FEATURE_QUIET`, default **false**): the bridge speaks only when spoken to.
   *
   * With it on, nothing the bridge says is self-initiated — no startup or recovery notice
   * (`src/notice.ts`), no explainer rotation (`src/explainer.ts`), no follow or raid thanks
   * (`src/eventsub.ts`). The `channel.follow` and `channel.raid` subscriptions stay, so
   * `flybridge_follows_total` / `flybridge_raids_total` keep counting and `/health` keeps
   * reporting the same subscription list; only the chat line is dropped.
   *
   * What it does NOT touch: the five commands (`!fly !brain !how !stuck !sugar`), the Sugar
   * redemption replies, and forwarding viewer chat to the on-screen CHAT panel. A viewer who
   * asks still gets an answer.
   */
  featureQuiet: boolean;
  /** Path to the redemption intent log replayed at startup (`src/redemptions.ts`). */
  redemptionStateFile: string;
  /**
   * How long the `channel.chat.message` EventSub subscription may stay unconfirmed before the
   * bridge logs one line and exits non-zero so systemd restarts it with a fresh websocket
   * transport (`src/subscription-health.ts`). Default 60_000 ms, `EVENTSUB_GRACE_MS`.
   *
   * Lower it and a slow reconnect becomes an unnecessary restart; raise it and the channel stays
   * dead longer. 60 s is comfortably above a healthy reconnect (measured at well under a second
   * on 2026-09-16, both before the outage and on the manual restart that ended it) and well
   * inside what a viewer will tolerate.
   */
  eventsubGraceMs: number;
  /** Path to the notice log that rate-limits startup/recovery chat notices across restarts. */
  noticeStateFile: string;
  /**
   * Minimum gap between two startup/recovery notices, across process restarts
   * (`src/notice.ts`). Default 600_000 ms, `NOTICE_MIN_INTERVAL_MS`.
   */
  noticeMinIntervalMs: number;
  /** `node:http` health/metrics server bind (`src/health.ts`). */
  healthHost: string;
  healthPort: number;
  /**
   * How often the explainer rotation posts, absent chat activity resetting the silence clock.
   * `0` (`EXPLAINER_INTERVAL_MS=0`) turns the rotation off on its own, independently of
   * `featureQuiet` — see `explainerEnabled` in `src/explainer.ts`.
   */
  explainerIntervalMs: number;
  /** Chat must have been silent this long before the explainer poster skips a post. */
  explainerSilenceMs: number;
  rateLimits: RateLimitSettings;
}

export interface RateLimitSettings {
  /** Per-user, per-command token bucket interval. Default 60_000 ms (1 per 60 s per command). */
  perUserPerCommandMs: number;
  /** Global reply bucket: at most one reply per this many ms across all commands. Default 5_000. */
  globalReplyMs: number;
  /** Per-command global cooldown, ms, before that command can fire again for anyone. */
  perCommandCooldownMs: Record<string, number>;
  /** Self-imposed chat send ceiling, messages per 30 s, below Twitch's moderator limit of 100/30s. */
  chatMessagesPer30s: number;
}

const DEFAULT_SIM_CONTROL_URL = 'http://127.0.0.1:7401';
const DEFAULT_TOKENS_FILE = '/var/lib/flybridge/tokens.json';
const DEFAULT_REDEMPTION_STATE_FILE = '/var/lib/flybridge/redemption-state.json';
const DEFAULT_NOTICE_STATE_FILE = '/var/lib/flybridge/notice-state.json';
const DEFAULT_HEALTH_HOST = '127.0.0.1';
const DEFAULT_HEALTH_PORT = 7410;
const DEFAULT_SIM_TIMEOUT_MS = 2_000;
const DEFAULT_EXPLAINER_INTERVAL_MS = 20 * 60 * 1000;
const DEFAULT_EXPLAINER_SILENCE_MS = 20 * 60 * 1000;
const DEFAULT_PER_USER_PER_COMMAND_MS = 60_000;
const DEFAULT_GLOBAL_REPLY_MS = 5_000;
const DEFAULT_CHAT_MESSAGES_PER_30S = 15;
const DEFAULT_SUGAR_COOLDOWN_MS = 10_000;

/** Thrown by `loadConfig` with every validation problem collected, not just the first. */
export class ConfigError extends Error {
  readonly problems: readonly string[];

  constructor(problems: readonly string[]) {
    super(`invalid flybridge configuration:\n  - ${problems.join('\n  - ')}`);
    this.name = 'ConfigError';
    this.problems = problems;
  }
}

/** Minimal environment shape `loadConfig` reads from. Matches `process.env`. */
export type Env = Record<string, string | undefined>;

export interface LoadConfigOptions {
  env?: Env;
  /** Override for reading credential files, for tests. Defaults to `node:fs`'s `readFileSync`. */
  readCredentialFile?: (path: string) => string;
}

/**
 * Load and validate configuration from environment variables (and, when `CREDENTIALS_DIRECTORY`
 * is set, systemd credential files). Throws `ConfigError` listing every problem found.
 */
export function loadConfig(options: LoadConfigOptions = {}): BridgeConfig {
  const env = options.env ?? process.env;
  const readCredentialFile = options.readCredentialFile ?? ((path: string) => readFileSync(path, 'utf8'));
  const problems: string[] = [];

  const { twitchClientId, twitchClientSecret } = readAppCredentials(env, readCredentialFile, problems);

  const channel = requireNonEmpty(env.CHANNEL, 'CHANNEL', problems);
  const botUser = requireNonEmpty(env.BOT_USER, 'BOT_USER', problems);
  const gameTitle = requireNonEmpty(env.GAME_TITLE, 'GAME_TITLE', problems);

  const simControlUrl = env.SIM_CONTROL_URL?.trim() || DEFAULT_SIM_CONTROL_URL;
  validateUrl(simControlUrl, 'SIM_CONTROL_URL', problems);

  const tokensFile = env.TOKENS_FILE?.trim() || DEFAULT_TOKENS_FILE;
  const redemptionStateFile = env.REDEMPTION_STATE_FILE?.trim() || DEFAULT_REDEMPTION_STATE_FILE;
  const noticeStateFile = env.NOTICE_STATE_FILE?.trim() || DEFAULT_NOTICE_STATE_FILE;

  const healthHost = env.HEALTH_HOST?.trim() || DEFAULT_HEALTH_HOST;
  const healthPort = parsePositiveInt(env.HEALTH_PORT, DEFAULT_HEALTH_PORT, 'HEALTH_PORT', problems);
  const simTimeoutMs = parsePositiveInt(env.SIM_TIMEOUT_MS, DEFAULT_SIM_TIMEOUT_MS, 'SIM_TIMEOUT_MS', problems);
  // Non-negative, not positive: 0 is the documented "no explainer rotation" value, and it has to
  // be reachable without also setting FEATURE_QUIET (which silences everything else too).
  const explainerIntervalMs = parseNonNegativeInt(
    env.EXPLAINER_INTERVAL_MS,
    DEFAULT_EXPLAINER_INTERVAL_MS,
    'EXPLAINER_INTERVAL_MS',
    problems,
  );
  const explainerSilenceMs = parsePositiveInt(
    env.EXPLAINER_SILENCE_MS,
    DEFAULT_EXPLAINER_SILENCE_MS,
    'EXPLAINER_SILENCE_MS',
    problems,
  );

  const eventsubGraceMs = parsePositiveInt(
    env.EVENTSUB_GRACE_MS,
    DEFAULT_EVENTSUB_GRACE_MS,
    'EVENTSUB_GRACE_MS',
    problems,
  );
  const noticeMinIntervalMs = parsePositiveInt(
    env.NOTICE_MIN_INTERVAL_MS,
    DEFAULT_NOTICE_MIN_INTERVAL_MS,
    'NOTICE_MIN_INTERVAL_MS',
    problems,
  );

  const featureRedemptions = parseBoolean(env.FEATURE_REDEMPTIONS, false, 'FEATURE_REDEMPTIONS', problems);
  const featurePredictions = parseBoolean(env.FEATURE_PREDICTIONS, false, 'FEATURE_PREDICTIONS', problems);
  // On by default: the CHAT panel is part of the locked rail layout, and flysim's own
  // `[chat] enabled` is the switch an operator reaches for in a hurry.
  const featureOnscreenChat = parseBoolean(env.FEATURE_ONSCREEN_CHAT, true, 'FEATURE_ONSCREEN_CHAT', problems);
  // Off by default: a bridge that says nothing unprompted is a per-channel choice, not the shape
  // docs/design/stage-bridge.md B2 describes.
  const featureQuiet = parseBoolean(env.FEATURE_QUIET, false, 'FEATURE_QUIET', problems);

  const perUserPerCommandMs = parsePositiveInt(
    env.RATE_LIMIT_PER_USER_PER_COMMAND_MS,
    DEFAULT_PER_USER_PER_COMMAND_MS,
    'RATE_LIMIT_PER_USER_PER_COMMAND_MS',
    problems,
  );
  const globalReplyMs = parsePositiveInt(
    env.RATE_LIMIT_GLOBAL_REPLY_MS,
    DEFAULT_GLOBAL_REPLY_MS,
    'RATE_LIMIT_GLOBAL_REPLY_MS',
    problems,
  );
  const chatMessagesPer30s = parsePositiveInt(
    env.RATE_LIMIT_CHAT_MESSAGES_PER_30S,
    DEFAULT_CHAT_MESSAGES_PER_30S,
    'RATE_LIMIT_CHAT_MESSAGES_PER_30S',
    problems,
  );
  const sugarCooldownMs = parsePositiveInt(
    env.RATE_LIMIT_SUGAR_COOLDOWN_MS,
    DEFAULT_SUGAR_COOLDOWN_MS,
    'RATE_LIMIT_SUGAR_COOLDOWN_MS',
    problems,
  );

  if (problems.length > 0) throw new ConfigError(problems);

  return {
    twitchClientId,
    twitchClientSecret,
    tokensFile,
    channel,
    botUser,
    simControlUrl,
    simTimeoutMs,
    gameTitle,
    featureRedemptions,
    featurePredictions,
    featureOnscreenChat,
    featureQuiet,
    redemptionStateFile,
    eventsubGraceMs,
    noticeStateFile,
    noticeMinIntervalMs,
    healthHost,
    healthPort,
    explainerIntervalMs,
    explainerSilenceMs,
    rateLimits: {
      perUserPerCommandMs,
      globalReplyMs,
      chatMessagesPer30s,
      perCommandCooldownMs: {
        fly: 0,
        brain: 0,
        how: 0,
        stuck: 0,
        sugar: sugarCooldownMs,
      },
    },
  };
}

interface AppCredentials {
  twitchClientId: string;
  twitchClientSecret: string;
}

/**
 * Resolve the app id/secret. Under `CREDENTIALS_DIRECTORY` the split `twitch-client-id` /
 * `twitch-client-secret` pair wins when both are readable; otherwise the combined two-line
 * `twitch-app` file (`infra/06-secrets.sh`) is parsed. A problem is only recorded when NEITHER
 * shape is present, and it names both, so a misconfigured deploy is told what to install rather
 * than which of two files it happened to look for first.
 */
function readAppCredentials(
  env: Env,
  readCredentialFile: (path: string) => string,
  problems: string[],
): AppCredentials {
  const credentialsDirectory = env.CREDENTIALS_DIRECTORY?.trim();
  if (!credentialsDirectory) {
    return {
      twitchClientId: requireNonEmpty(env.TWITCH_CLIENT_ID, 'TWITCH_CLIENT_ID', problems),
      twitchClientSecret: requireNonEmpty(env.TWITCH_CLIENT_SECRET, 'TWITCH_CLIENT_SECRET', problems),
    };
  }

  const split = readSplitAppCredentials(credentialsDirectory, readCredentialFile);
  if (split) return split;

  const combinedName = 'twitch-app';
  let combinedRaw: string;
  try {
    combinedRaw = readCredentialFile(join(credentialsDirectory, combinedName));
  } catch (cause) {
    problems.push(
      `CREDENTIALS_DIRECTORY has neither the split credentials "twitch-client-id"/"twitch-client-secret" ` +
        `nor the combined "${combinedName}" (two lines, <client_id> then <client_secret>, as ` +
        `infra/06-secrets.sh installs it): ${(cause as Error).message}`,
    );
    return { twitchClientId: '', twitchClientSecret: '' };
  }

  const lines = combinedRaw
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  if (lines.length !== 2) {
    problems.push(
      `credential "${combinedName}" in CREDENTIALS_DIRECTORY must hold exactly two non-empty lines ` +
        `(<client_id> then <client_secret>), found ${lines.length}`,
    );
    return { twitchClientId: '', twitchClientSecret: '' };
  }
  return { twitchClientId: lines[0]!, twitchClientSecret: lines[1]! };
}

function readSplitAppCredentials(
  credentialsDirectory: string,
  readCredentialFile: (path: string) => string,
): AppCredentials | null {
  const read = (name: string): string | null => {
    try {
      const value = readCredentialFile(join(credentialsDirectory, name)).trim();
      return value.length > 0 ? value : null;
    } catch {
      return null;
    }
  };
  const id = read('twitch-client-id');
  const secret = read('twitch-client-secret');
  return id !== null && secret !== null ? { twitchClientId: id, twitchClientSecret: secret } : null;
}

function requireNonEmpty(value: string | undefined, name: string, problems: string[]): string {
  const trimmed = value?.trim();
  if (!trimmed) {
    problems.push(`${name} is required (set it directly, or CREDENTIALS_DIRECTORY for credential files)`);
    return '';
  }
  return trimmed;
}

function validateUrl(value: string, name: string, problems: string[]): void {
  try {
    // eslint-disable-next-line no-new
    new URL(value);
  } catch {
    problems.push(`${name} must be a valid URL, got ${JSON.stringify(value)}`);
  }
}

function parsePositiveInt(
  value: string | undefined,
  fallback: number,
  name: string,
  problems: string[],
): number {
  if (value === undefined || value.trim() === '') return fallback;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0 || String(parsed) !== value.trim()) {
    problems.push(`${name} must be a positive integer, got ${JSON.stringify(value)}`);
    return fallback;
  }
  return parsed;
}

/** Like `parsePositiveInt`, but `0` is a legal value: an interval of zero means "never". */
function parseNonNegativeInt(
  value: string | undefined,
  fallback: number,
  name: string,
  problems: string[],
): number {
  if (value === undefined || value.trim() === '') return fallback;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed < 0 || String(parsed) !== value.trim()) {
    problems.push(`${name} must be a non-negative integer, got ${JSON.stringify(value)}`);
    return fallback;
  }
  return parsed;
}

function parseBoolean(value: string | undefined, fallback: boolean, name: string, problems: string[]): boolean {
  if (value === undefined || value.trim() === '') return fallback;
  const normalized = value.trim().toLowerCase();
  if (['1', 'true', 'yes', 'on'].includes(normalized)) return true;
  if (['0', 'false', 'no', 'off'].includes(normalized)) return false;
  problems.push(`${name} must be one of true/false/1/0/yes/no/on/off, got ${JSON.stringify(value)}`);
  return fallback;
}
