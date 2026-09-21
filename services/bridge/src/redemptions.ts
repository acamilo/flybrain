/**
 * Sugar Channel Points redemption handling (`docs/design/stage-bridge.md` B3), behind
 * `FEATURE_REDEMPTIONS`.
 *
 * 1. Idempotent reward creation: look up "Sugar" by title first, create only if missing, so
 *    re-running startup never creates a duplicate reward.
 * 2. On `channel.channel_points_custom_reward_redemption.add` (wired in `src/eventsub.ts`),
 *    `POST /stimulate` on flysim with `source: 'points'`. Accepted -> `FULFILLED`. Rejected,
 *    failed, or timed out -> `CANCELED` (refunds the viewer's points).
 * 3. Every redemption is persisted as a pending intent before the sim call and resolved after,
 *    via `IntentStore`. On startup, anything still `pending` (a crash mid-flight) is force-
 *    resolved to `CANCELED` so nothing stays pending forever.
 *
 * `RedemptionApi` is a small interface so tests exercise the state machine with a mock, never a
 * real `@twurple/api` client.
 */
import { readFile } from 'node:fs/promises';
import type { ApiClient } from '@twurple/api';
import { atomicWriteJson } from './atomic-file';
import { validateDisplayName } from './names';
import type { Send } from './chat';
import type { SimClient } from './sim';

export interface RedemptionReward {
  id: string;
  title: string;
}

export type RedemptionStatus = 'FULFILLED' | 'CANCELED';

/** Small interface over the Twitch Helix calls redemption handling needs. Never import twurple in tests. */
export interface RedemptionApi {
  findRewardByTitle(title: string): Promise<RedemptionReward | null>;
  createReward(title: string): Promise<RedemptionReward>;
  updateRedemptionStatus(rewardId: string, redemptionIds: string[], status: RedemptionStatus): Promise<void>;
}

export interface RedemptionIntent {
  redemptionId: string;
  rewardId: string;
  displayName: string;
  status: 'pending' | 'fulfilled' | 'canceled';
  createdAtMs: number;
}

export interface IntentStore {
  load(): Promise<RedemptionIntent[]>;
  save(intents: RedemptionIntent[]): Promise<void>;
}

/** Reads/writes the intent log as JSON at a fixed path (`BridgeConfig.redemptionStateFile`). */
export class FileIntentStore implements IntentStore {
  constructor(private readonly path: string) {}

  async load(): Promise<RedemptionIntent[]> {
    try {
      const raw = await readFile(this.path, 'utf8');
      const parsed = JSON.parse(raw) as unknown;
      return Array.isArray(parsed) ? (parsed as RedemptionIntent[]) : [];
    } catch (cause) {
      if ((cause as NodeJS.ErrnoException).code === 'ENOENT') return [];
      throw cause;
    }
  }

  async save(intents: RedemptionIntent[]): Promise<void> {
    await atomicWriteJson(this.path, intents);
  }
}

/** An in-memory store, for tests. */
export class MemoryIntentStore implements IntentStore {
  private intents: RedemptionIntent[] = [];

  async load(): Promise<RedemptionIntent[]> {
    return this.intents.map((intent) => ({ ...intent }));
  }

  async save(intents: RedemptionIntent[]): Promise<void> {
    this.intents = intents.map((intent) => ({ ...intent }));
  }
}

/**
 * Real Twitch Helix implementation of `RedemptionApi`. `is_user_input_required: false` (no free
 * text to render on screen), a global cooldown and a per-stream max, per B3 item 1.
 */
export class TwurpleRedemptionApi implements RedemptionApi {
  constructor(
    private readonly apiClient: ApiClient,
    private readonly broadcasterUserId: string,
    private readonly options: { costPoints: number; globalCooldownSeconds: number; maxPerStream: number | null } = {
      costPoints: 200,
      globalCooldownSeconds: 60,
      maxPerStream: null,
    },
  ) {}

  async findRewardByTitle(title: string): Promise<RedemptionReward | null> {
    const rewards = await this.apiClient.channelPoints.getCustomRewards(this.broadcasterUserId, true);
    const match = rewards.find((reward) => reward.title === title);
    return match ? { id: match.id, title: match.title } : null;
  }

  async createReward(title: string): Promise<RedemptionReward> {
    const reward = await this.apiClient.channelPoints.createCustomReward(this.broadcasterUserId, {
      title,
      cost: this.options.costPoints,
      userInputRequired: false,
      globalCooldown: this.options.globalCooldownSeconds,
      maxRedemptionsPerStream: this.options.maxPerStream,
      isEnabled: true,
    });
    return { id: reward.id, title: reward.title };
  }

  async updateRedemptionStatus(rewardId: string, redemptionIds: string[], status: RedemptionStatus): Promise<void> {
    await this.apiClient.channelPoints.updateRedemptionStatusByIds(
      this.broadcasterUserId,
      rewardId,
      redemptionIds,
      status,
    );
  }
}

/**
 * Whether an error is Twitch refusing the Channel Points API because the channel is not Affiliate
 * or Partner yet.
 *
 * Channel Points are **not** available on a plain account, whatever the token's scopes say: a
 * brand-new channel can be granted `channel:read:redemptions` and `channel:manage:redemptions`,
 * pass the startup scope assertion, and still get
 *
 *   GET channel_points/custom_rewards -> 403
 *   {"error":"Forbidden","status":403,"message":"The broadcaster must have partner or affiliate status."}
 *
 * on the first call. Measured on `<twitch-channel>` (`broadcaster_type: ""`) on 2026-09-16, where
 * it took the whole bridge down in a 10 s restart loop and the channel had no chat bot at all.
 *
 * Duck-typed on purpose (`_statusCode`/`_body`, the fields twurple's `HttpStatusCodeError`
 * carries) so this module stays twurple-free and testable without a Twitch client. The message
 * match is required, not just the 403: a 403 with any other body is a real failure — a revoked
 * token, a scope the assertion did not cover — and must not be swallowed.
 */
export function isChannelPointsAffiliateRefusal(error: unknown): boolean {
  if (typeof error !== 'object' || error === null) return false;
  const candidate = error as { _statusCode?: unknown; _body?: unknown; message?: unknown };
  if (candidate._statusCode !== 403) return false;
  const text = `${String(candidate._body ?? '')} ${String(candidate.message ?? '')}`;
  return /partner or affiliate status/i.test(text);
}

const MAX_STATUS_UPDATE_ATTEMPTS = 3;

export interface RedemptionManagerOptions {
  sim: SimClient;
  api: RedemptionApi;
  store: IntentStore;
  send: Send;
  rewardTitle: string;
  /** Injectable for tests; defaults to `console.error`. */
  logError?: (message: string) => void;
}

export class RedemptionManager {
  private rewardId: string | null = null;
  private readonly intents = new Map<string, RedemptionIntent>();

  constructor(private readonly options: RedemptionManagerOptions) {}

  /** Idempotent reward creation, then replay any intents left pending from a previous run. */
  async start(): Promise<RedemptionReward> {
    const existing = await this.options.api.findRewardByTitle(this.options.rewardTitle);
    const reward = existing ?? (await this.options.api.createReward(this.options.rewardTitle));
    this.rewardId = reward.id;

    const loaded = await this.options.store.load();
    for (const intent of loaded) this.intents.set(intent.redemptionId, intent);

    const pending = loaded.filter((intent) => intent.status === 'pending');
    for (const intent of pending) {
      // A crash between accepting the redemption and resolving it. Refund by default: we cannot
      // safely re-fire a sim stimulation after an unknown-length restart gap.
      await this.resolve(intent, 'CANCELED');
    }

    return reward;
  }

  get currentRewardId(): string | null {
    return this.rewardId;
  }

  /** Handle one `channel.channel_points_custom_reward_redemption.add` event. */
  async handleRedemptionAdd(redemptionId: string, rewardId: string, displayName: string): Promise<RedemptionStatus> {
    const intent: RedemptionIntent = {
      redemptionId,
      rewardId,
      displayName,
      status: 'pending',
      createdAtMs: Date.now(),
    };
    this.intents.set(redemptionId, intent);
    await this.persist();

    const by = validateDisplayName(displayName);
    const result = await this.options.sim.stimulate({ by, source: 'points' });

    if (result.ok) {
      await this.resolve(intent, 'FULFILLED');
      await this.options.send('sugarAccepted', { by });
      return 'FULFILLED';
    }

    await this.resolve(intent, 'CANCELED');
    await this.options.send('sugarDisabled', {});
    return 'CANCELED';
  }

  private async resolve(intent: RedemptionIntent, status: RedemptionStatus): Promise<void> {
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_STATUS_UPDATE_ATTEMPTS; attempt++) {
      try {
        await this.options.api.updateRedemptionStatus(intent.rewardId, [intent.redemptionId], status);
        intent.status = status === 'FULFILLED' ? 'fulfilled' : 'canceled';
        this.intents.set(intent.redemptionId, intent);
        await this.persist();
        return;
      } catch (cause) {
        lastError = cause;
      }
    }
    // Every attempt failed. Leave the intent `pending` in the store (already persisted) so the
    // next startup's replay tries again rather than silently losing the refund.
    const log = this.options.logError ?? ((message: string) => console.error(message));
    log(
      `redemption ${intent.redemptionId}: failed to set status ${status} after ${MAX_STATUS_UPDATE_ATTEMPTS} attempts: ${String(lastError)}`,
    );
  }

  private async persist(): Promise<void> {
    await this.options.store.save([...this.intents.values()]);
  }
}
