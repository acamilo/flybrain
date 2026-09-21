/**
 * EventSub WebSocket wiring (`docs/design/stage-bridge.md` B1/B2): `channel.chat.message`,
 * `channel.follow`, `channel.raid`, and — behind `FEATURE_REDEMPTIONS` — the Sugar reward's
 * `channel.channel_points_custom_reward_redemption.add`. PubSub is dead; this is EventSub WS
 * only (`@twurple/eventsub-ws`).
 *
 * This module is the twurple-facing glue: it translates EventSub events into calls on the
 * twurple-free pieces (`commands.ts`, `redemptions.ts`, `chat.ts`'s `send`, and now
 * `subscription-health.ts`) and updates `HealthState`/`Metrics` for `/health` and `/metrics`.
 * Everything it calls into *is* unit-tested with fakes; the wiring itself is tested through the
 * `createListener` seam (`tests/eventsub.test.ts`) and exercised end to end via
 * `tools/mock-twitch.sh` (Twitch CLI).
 *
 * ## Transport budget (2026-09-16 incident, see `src/subscription-health.ts`)
 *
 * Twitch allows THREE websocket transports per user. `EventSubWsListener` opens one socket per
 * distinct auth user id, so two listeners for two roles that are the same Twitch account — which
 * is what the first live channel is — spent two of the three, and a single 1006 disconnect made
 * twurple re-create both at once, tipping over the limit and leaving the channel with no chat
 * subscription for an hour. So: when the two roles resolve to one user id, ONE listener carries
 * everything, using the scope-routing auth provider from `src/auth.ts`. Two accounts (the planned
 * separate bot account) still get two listeners, because then the per-user budgets are separate
 * and one socket per account is already the minimum.
 */
import type { ApiClient } from '@twurple/api';
import type {
  EventSubChannelChatMessageEvent,
  EventSubChannelFollowEvent,
  EventSubChannelRaidEvent,
} from '@twurple/eventsub-base';
import { EventSubWsListener } from '@twurple/eventsub-ws';
import { handleCommand, parseCommand } from './commands';
import type { BridgeConfig } from './config';
import type { Send } from './chat';
import { validateDisplayName } from './names';
import type { RateLimiter } from './ratelimit';
import type { RedemptionManager } from './redemptions';
import type { SimClient } from './sim';
import { HealthState, Metrics } from './health';
import type { ExplainerPoster } from './explainer';
import type { OnscreenChat } from './onscreen-chat';
import type { ChatSubscriptionHealth } from './subscription-health';
import { isWebsocketTransportLimitError } from './subscription-health';

/** What one created subscription looks like from here: an id, which is all this module uses. */
export interface EventSubSubscriptionLike {
  readonly id: string;
}

/**
 * The slice of `EventSubWsListener` this module actually touches.
 *
 * Narrow on purpose: `EventSubWsListener` structurally satisfies it (its event binders are
 * `(handler) => Listener`, its `onChannel*` methods return an `EventSubSubscription` whose `id`
 * is a string), and a test can implement it in thirty lines instead of faking a twurple class.
 * That is what makes "one listener when the roles are one account, two when they are not"
 * assertable without a Twitch connection.
 */
export interface EventSubListenerLike {
  onUserSocketConnect(handler: (userId: string) => void): unknown;
  onUserSocketDisconnect(handler: (userId: string, error?: Error) => void): unknown;
  onRevoke(handler: (subscription: EventSubSubscriptionLike, status: string) => void): unknown;
  onSubscriptionCreateSuccess(handler: (subscription: EventSubSubscriptionLike) => void): unknown;
  onSubscriptionCreateFailure(handler: (subscription: EventSubSubscriptionLike, error: Error) => void): unknown;
  onChannelChatMessage(
    broadcasterId: string,
    userId: string,
    handler: (event: EventSubChannelChatMessageEvent) => void,
  ): EventSubSubscriptionLike;
  onChannelFollow(
    broadcasterId: string,
    moderatorId: string,
    handler: (event: EventSubChannelFollowEvent) => void,
  ): EventSubSubscriptionLike;
  onChannelRaidTo(broadcasterId: string, handler: (event: EventSubChannelRaidEvent) => void): EventSubSubscriptionLike;
  onChannelRedemptionAddForReward(
    broadcasterId: string,
    rewardId: string,
    handler: (event: { id: string; rewardId: string; userDisplayName: string }) => void,
  ): EventSubSubscriptionLike;
  start(): void;
  stop(): void;
}

export interface CreateListenerOptions {
  apiClient: ApiClient;
  url?: string;
}

export interface EventSubDeps {
  /** Carries the BOT token. `channel.chat.message` is authorized by the bot user. */
  botApiClient: ApiClient;
  /** Carries the BROADCASTER token: follows, raids, Channel Points redemptions. */
  broadcasterApiClient: ApiClient;
  /**
   * Set ONLY when both roles are the same Twitch account: an `ApiClient` over
   * `AuthSetup.sharedEventSubAuthProvider`, which routes each call to the role token that carries
   * the scopes it asked for. When present, one listener (one websocket transport) serves both
   * roles. Absent or null means two listeners, which is correct for two accounts.
   */
  sharedEventSubApiClient?: ApiClient | null;
  botUserId: string;
  broadcasterUserId: string;
  send: Send;
  sim: SimClient;
  rateLimiter: RateLimiter;
  config: Pick<BridgeConfig, 'gameTitle' | 'featureQuiet'>;
  health: HealthState;
  metrics: Metrics;
  explainer: ExplainerPoster;
  /** Forwards AutoMod-passed chat to flysim's `POST /chat` (`src/onscreen-chat.ts`). */
  onscreen: OnscreenChat;
  /** Present, and already `start()`-ed, only when `FEATURE_REDEMPTIONS` is on. */
  redemptions: RedemptionManager | null;
  /** The self-healing watchdog (`src/subscription-health.ts`). */
  chatHealth: ChatSubscriptionHealth;
  /** Overrides the WebSocket URL — used by `tools/mock-twitch.sh` against `twitch event websocket start-server`. */
  url?: string;
  /** Test seam. Defaults to constructing a real `EventSubWsListener`. */
  createListener?: (options: CreateListenerOptions) => EventSubListenerLike;
}

/** What `startEventSub` hands back. */
export interface EventSubListeners {
  /** The listener carrying `channel.chat.message`. Same object as `broadcaster` when collapsed. */
  bot: EventSubListenerLike;
  /** The listener carrying follows, raids and redemptions. Same object as `bot` when collapsed. */
  broadcaster: EventSubListenerLike;
  /** Distinct listeners, and therefore distinct websocket transports: 1 collapsed, 2 otherwise. */
  listenerCount: 1 | 2;
  /** True when one listener serves both roles. */
  collapsed: boolean;
  /** The `channel.chat.message` subscription id the watchdog watches. */
  chatSubscriptionId: string;
  stop: () => void;
}

export function startEventSub(deps: EventSubDeps): EventSubListeners {
  const create = deps.createListener ?? ((options) => new EventSubWsListener(options));
  const sameAccount = deps.botUserId === deps.broadcasterUserId;
  const sharedApiClient = sameAccount ? (deps.sharedEventSubApiClient ?? null) : null;

  if (sameAccount && sharedApiClient === null) {
    // Defensive: `buildAuthProvider` builds the routing provider for exactly this case, so this
    // means someone wired `startEventSub` by hand. Two transports for one account is the shape
    // that produced the 2026-09-16 outage, so say so rather than silently regressing.
    console.error(
      'flybridge: bot and broadcaster are the same Twitch account but no sharedEventSubApiClient was ' +
        'given; falling back to TWO EventSub websockets, which doubles this account’s transport count',
    );
  }

  let botListener: EventSubListenerLike;
  let broadcasterListener: EventSubListenerLike;
  let distinct: EventSubListenerLike[];
  if (sharedApiClient !== null) {
    const shared = create({ apiClient: sharedApiClient, url: deps.url });
    botListener = shared;
    broadcasterListener = shared;
    distinct = [shared];
  } else {
    botListener = create({ apiClient: deps.botApiClient, url: deps.url });
    broadcasterListener = create({ apiClient: deps.broadcasterApiClient, url: deps.url });
    distinct = [botListener, broadcasterListener];
  }

  // Subscriptions first, so the handlers below can compare against the chat subscription's id
  // instead of reconstructing twurple's `channel.chat.message.<broadcaster>.<user>` id format.
  // Nothing is sent to Twitch until `start()` at the bottom, so ordering is free here.
  const chatSubscription = botListener.onChannelChatMessage(deps.broadcasterUserId, deps.botUserId, (event) => {
    void handleChatMessage(event, deps);
  });

  const followSubscription = broadcasterListener.onChannelFollow(
    deps.broadcasterUserId,
    deps.broadcasterUserId,
    (event) => {
      void handleFollow(event, deps);
    },
  );

  const raidSubscription = broadcasterListener.onChannelRaidTo(deps.broadcasterUserId, (event) => {
    void handleRaid(event, deps);
  });

  deps.health.subscriptions = [chatSubscription, followSubscription, raidSubscription].map((subscription) => ({
    type: subscription.id,
    status: 'enabled',
  }));

  if (deps.redemptions) {
    const rewardId = deps.redemptions.currentRewardId;
    if (rewardId) {
      const redemptions = deps.redemptions;
      const redemptionSubscription = broadcasterListener.onChannelRedemptionAddForReward(
        deps.broadcasterUserId,
        rewardId,
        (event) => {
          void redemptions.handleRedemptionAdd(event.id, event.rewardId, event.userDisplayName).then((status) => {
            deps.health.lastRedemptionOutcome = {
              status,
              atIso: new Date().toISOString(),
              by: validateDisplayName(event.userDisplayName),
            };
            deps.metrics.increment(
              status === 'FULFILLED' ? 'flybridge_redemptions_fulfilled_total' : 'flybridge_redemptions_refunded_total',
            );
          });
        },
      );
      deps.health.subscriptions.push({ type: redemptionSubscription.id, status: 'enabled' });
    } else {
      console.error('flybridge: FEATURE_REDEMPTIONS is on but the Sugar reward has no id yet; skipping subscription');
    }
  }

  const chatSubscriptionId = chatSubscription.id;

  // `eventSubConnected` is an AND over every socket: a bridge with chat but no redemptions, or the
  // reverse, is degraded, and /health should not call that connected. With one collapsed listener
  // there is exactly one term.
  const connected = new Map<EventSubListenerLike, boolean>(distinct.map((listener) => [listener, false]));
  const publishConnected = (): void => {
    deps.health.eventSubConnected = [...connected.values()].every(Boolean);
  };
  publishConnected();

  // `/health` must go false the MOMENT chat is lost, not only when the watchdog gives up: the
  // whole point of reporting it separately from `eventSubConnected` is that a probe can see the
  // gap between "sockets up" and "chat working" while it is open.
  const publishChatHealth = (): void => {
    deps.health.chatSubscriptionHealthy = deps.chatHealth.healthy;
  };

  const wire = (listener: EventSubListenerLike, which: string): void => {
    const carriesChat = listener === botListener;

    listener.onUserSocketConnect(() => {
      connected.set(listener, true);
      publishConnected();
    });

    listener.onUserSocketDisconnect((userId, error) => {
      connected.set(listener, false);
      publishConnected();
      deps.metrics.increment('flybridge_eventsub_reconnects_total');
      if (error) console.error(`flybridge: EventSub ${which} socket for ${userId} disconnected: ${error.message}`);
      if (carriesChat) {
        // twurple marks every subscription for this user dropped and re-creates them on the next
        // `session_welcome`. Arm the grace timer here: if the re-create never lands (the 429
        // transport-limit case), nothing else will ever tell us.
        deps.metrics.increment('flybridge_eventsub_chat_subscription_lost_total');
        deps.chatHealth.noteLost(`EventSub ${which} socket disconnected${error ? `: ${error.message}` : ''}`);
        publishChatHealth();
      }
    });

    listener.onRevoke((subscription, status) => {
      console.error(`flybridge: EventSub subscription ${subscription.id} revoked (${status})`);
      deps.health.subscriptions = deps.health.subscriptions.map((s) =>
        s.type === subscription.id ? { ...s, status } : s,
      );
      if (subscription.id === chatSubscriptionId) {
        deps.metrics.increment('flybridge_eventsub_chat_subscription_lost_total');
        deps.chatHealth.noteLost(`chat subscription revoked by Twitch (${status})`);
        publishChatHealth();
      }
    });

    listener.onSubscriptionCreateSuccess((subscription) => {
      if (subscription.id !== chatSubscriptionId) return;
      // The ONLY confirmation that chat works. See `src/subscription-health.ts` for why
      // `subscription.verified` and `onSubscriptionActivate` are not usable for this.
      deps.metrics.increment('flybridge_eventsub_chat_subscription_confirmed_total');
      if (!deps.chatHealth.healthy) {
        console.log(`flybridge: channel.chat.message subscription confirmed (${subscription.id})`);
      }
      deps.chatHealth.noteConfirmed();
      publishChatHealth();
    });

    listener.onSubscriptionCreateFailure((subscription, error) => {
      console.error(`flybridge: EventSub subscription create failure for ${subscription.id}: ${error.message}`);
      deps.metrics.increment('flybridge_eventsub_subscription_create_failures_total');
      if (isWebsocketTransportLimitError(error)) {
        deps.metrics.increment('flybridge_eventsub_transport_limit_total');
      }
      if (subscription.id === chatSubscriptionId) {
        deps.metrics.increment('flybridge_eventsub_chat_subscription_lost_total');
        deps.chatHealth.noteCreateFailure(error);
        publishChatHealth();
      }
    });
  };

  if (distinct.length === 1) {
    wire(distinct[0]!, 'shared');
  } else {
    wire(botListener, 'bot');
    wire(broadcasterListener, 'broadcaster');
  }

  // Arm the watchdog before the first connection attempt, so a bridge that never manages to
  // create the chat subscription AT ALL — a cold start straight into the 429, which is exactly
  // what a crash-restart during the 2026-09-16 window would have hit — exits and retries instead
  // of sitting there `active (running)` with a dead channel. `noteConfirmed()` disarms it.
  deps.chatHealth.noteLost('startup: chat subscription not confirmed yet');
  publishChatHealth();

  for (const listener of distinct) listener.start();

  return {
    bot: botListener,
    broadcaster: broadcasterListener,
    listenerCount: distinct.length === 1 ? 1 : 2,
    collapsed: distinct.length === 1,
    chatSubscriptionId,
    stop: () => {
      deps.chatHealth.stop();
      for (const listener of distinct) listener.stop();
    },
  };
}

async function handleChatMessage(event: EventSubChannelChatMessageEvent, deps: EventSubDeps): Promise<void> {
  deps.explainer.noteChatActivity();

  // On-screen chat first, and independently of the command path: `channel.chat.message` only
  // fires for messages AutoMod let through (a held message produces no event at all), and
  // `OnscreenChat` decides what a viewer line is — commands, bots and hostile text are dropped
  // there, not here. This is the only place a message body is read for anything but `parseCommand`.
  void deps.onscreen.forwardViewerMessage({
    login: event.chatterName,
    displayName: event.chatterDisplayName,
    messageText: event.messageText,
  });

  const command = parseCommand(event.messageText);
  if (!command) return;

  deps.metrics.increment(`flybridge_commands_served_total{command="${command}"}`);
  await handleCommand(
    command,
    { id: event.chatterId, displayName: event.chatterDisplayName },
    { sim: deps.sim, send: deps.send, rateLimiter: deps.rateLimiter, config: deps.config },
  );
}

async function handleFollow(event: EventSubChannelFollowEvent, deps: EventSubDeps): Promise<void> {
  const by = validateDisplayName(event.userDisplayName);
  console.log(`flybridge: follow from ${by}`);
  deps.metrics.increment('flybridge_follows_total');
  // Quiet mode keeps the subscription and the count, and drops only the chat line: a follow is
  // not somebody speaking to the bot (`FEATURE_QUIET`, src/config.ts).
  if (deps.config.featureQuiet) return;
  // TODO(control-api): docs/control-api.md has no ticker/viewer endpoint for follows or raids —
  // only /stimulate, /reward, /checkpoint, /pause, /events, /status, /healthz. B1 replies in
  // chat only; showing follows/raids on the broadcast ticker needs a contract addition first
  // (see docs/design/stage-bridge.md section C: viewer names reach the ticker only via
  // /stimulate or a future /ticker-equivalent endpoint, never ad hoc).
  await deps.send('followThanks', { by });
}

async function handleRaid(event: EventSubChannelRaidEvent, deps: EventSubDeps): Promise<void> {
  const by = validateDisplayName(event.raidingBroadcasterDisplayName);
  console.log(`flybridge: raid from ${by} with ${event.viewers} viewers`);
  deps.metrics.increment('flybridge_raids_total');
  if (deps.config.featureQuiet) return;
  // TODO(control-api): same missing ticker endpoint as handleFollow above.
  await deps.send('raidThanks', { by, viewers: event.viewers });
}
