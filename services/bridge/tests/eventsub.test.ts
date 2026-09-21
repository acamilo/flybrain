/**
 * `startEventSub` wiring (`src/eventsub.ts`), through the `createListener` seam: no twurple, no
 * Twitch, no sockets.
 *
 * Two properties are asserted here, both of them direct consequences of the 2026-09-16 the release container
 * outage (`src/subscription-health.ts`):
 *
 *  1. TRANSPORT COUNT. One Twitch account holding both roles gets ONE listener, because
 *     `EventSubWsListener` opens a socket per auth user id and two sockets from one account spent
 *     two of Twitch's three per-user websocket transports. Two accounts still get two listeners.
 *  2. SELF-HEALING. A `channel.chat.message` create failure with no confirmation inside the grace
 *     period reaches `onUnhealthy`; a confirmation inside it does not.
 *  3. QUIET MODE (`FEATURE_QUIET`). Follows and raids stay subscribed and counted but say nothing,
 *     while commands still answer and viewer chat still reaches the on-screen panel.
 */
import assert from 'node:assert/strict';
import type { ApiClient } from '@twurple/api';
import type {
  EventSubChannelChatMessageEvent,
  EventSubChannelFollowEvent,
  EventSubChannelRaidEvent,
} from '@twurple/eventsub-base';
import test from 'node:test';
import { createSend } from '../src/chat';
import { startEventSub, type EventSubListenerLike, type EventSubSubscriptionLike } from '../src/eventsub';
import { ExplainerPoster } from '../src/explainer';
import { HealthState, Metrics } from '../src/health';
import { OnscreenChat } from '../src/onscreen-chat';
import { FakeClock, RateLimiter } from '../src/ratelimit';
import {
  ChatSubscriptionHealth,
  type ChatSubscriptionLostReport,
  type Timers,
} from '../src/subscription-health';
import { FakeChatSender, FakeSimClient } from './helpers';

const BROADCASTER_ID = '<broadcaster-id>';
/** The first live channel runs both roles on this one account (docs/design/stage-bridge.md B1). */
const SAME_ACCOUNT_BOT_ID = BROADCASTER_ID;
/** The planned separate bot account. */
const SEPARATE_BOT_ID = '999000111';

class ManualTimers implements Timers {
  private next = 1;
  private readonly pending = new Map<number, () => void>();

  set(callback: () => void): unknown {
    const handle = this.next++;
    this.pending.set(handle, callback);
    return handle;
  }

  clear(handle: unknown): void {
    this.pending.delete(handle as number);
  }

  fire(): void {
    for (const [handle, callback] of [...this.pending.entries()]) {
      this.pending.delete(handle);
      callback();
    }
  }
}

/**
 * A recording stand-in for `EventSubWsListener`, implementing only `EventSubListenerLike`. Its
 * `emit*` methods are how a test plays Twitch: "your socket dropped", "the create failed", "the
 * create succeeded".
 */
class FakeListener implements EventSubListenerLike {
  readonly apiClient: ApiClient;
  starts = 0;
  stops = 0;
  readonly created: string[] = [];

  private chatHandlers: ((event: EventSubChannelChatMessageEvent) => void)[] = [];
  private followHandlers: ((event: EventSubChannelFollowEvent) => void)[] = [];
  private raidHandlers: ((event: EventSubChannelRaidEvent) => void)[] = [];
  private connectHandlers: ((userId: string) => void)[] = [];
  private disconnectHandlers: ((userId: string, error?: Error) => void)[] = [];
  private revokeHandlers: ((subscription: EventSubSubscriptionLike, status: string) => void)[] = [];
  private successHandlers: ((subscription: EventSubSubscriptionLike) => void)[] = [];
  private failureHandlers: ((subscription: EventSubSubscriptionLike, error: Error) => void)[] = [];

  constructor(apiClient: ApiClient) {
    this.apiClient = apiClient;
  }

  onUserSocketConnect(handler: (userId: string) => void): unknown {
    this.connectHandlers.push(handler);
    return null;
  }

  onUserSocketDisconnect(handler: (userId: string, error?: Error) => void): unknown {
    this.disconnectHandlers.push(handler);
    return null;
  }

  onRevoke(handler: (subscription: EventSubSubscriptionLike, status: string) => void): unknown {
    this.revokeHandlers.push(handler);
    return null;
  }

  onSubscriptionCreateSuccess(handler: (subscription: EventSubSubscriptionLike) => void): unknown {
    this.successHandlers.push(handler);
    return null;
  }

  onSubscriptionCreateFailure(handler: (subscription: EventSubSubscriptionLike, error: Error) => void): unknown {
    this.failureHandlers.push(handler);
    return null;
  }

  onChannelChatMessage(
    broadcasterId: string,
    userId: string,
    handler: (event: EventSubChannelChatMessageEvent) => void,
  ): EventSubSubscriptionLike {
    this.chatHandlers.push(handler);
    return this.record(`channel.chat.message.${broadcasterId}.${userId}`);
  }

  onChannelFollow(
    broadcasterId: string,
    moderatorId: string,
    handler: (event: EventSubChannelFollowEvent) => void,
  ): EventSubSubscriptionLike {
    this.followHandlers.push(handler);
    return this.record(`channel.follow.${broadcasterId}.${moderatorId}`);
  }

  onChannelRaidTo(
    broadcasterId: string,
    handler: (event: EventSubChannelRaidEvent) => void,
  ): EventSubSubscriptionLike {
    this.raidHandlers.push(handler);
    return this.record(`channel.raid.to.${broadcasterId}`);
  }

  onChannelRedemptionAddForReward(broadcasterId: string, rewardId: string): EventSubSubscriptionLike {
    return this.record(`channel.channel_points_custom_reward_redemption.add.${broadcasterId}.${rewardId}`);
  }

  start(): void {
    this.starts += 1;
  }

  stop(): void {
    this.stops += 1;
  }

  /** One chat message from a viewer. Only the four fields the handler reads are populated. */
  emitChat(messageText: string, chatter = 'a_viewer'): void {
    const event = {
      chatterId: '424242',
      chatterName: chatter,
      chatterDisplayName: chatter,
      messageText,
    } as unknown as EventSubChannelChatMessageEvent;
    for (const handler of this.chatHandlers) handler(event);
  }

  emitFollow(userDisplayName: string): void {
    for (const handler of this.followHandlers) {
      handler({ userDisplayName } as unknown as EventSubChannelFollowEvent);
    }
  }

  emitRaid(raidingBroadcasterDisplayName: string, viewers: number): void {
    for (const handler of this.raidHandlers) {
      handler({ raidingBroadcasterDisplayName, viewers } as unknown as EventSubChannelRaidEvent);
    }
  }

  emitConnect(userId: string): void {
    for (const handler of this.connectHandlers) handler(userId);
  }

  emitDisconnect(userId: string, error?: Error): void {
    for (const handler of this.disconnectHandlers) handler(userId, error);
  }

  emitRevoke(id: string, status: string): void {
    for (const handler of this.revokeHandlers) handler({ id }, status);
  }

  emitCreateSuccess(id: string): void {
    for (const handler of this.successHandlers) handler({ id });
  }

  emitCreateFailure(id: string, error: Error): void {
    for (const handler of this.failureHandlers) handler({ id }, error);
  }

  private record(id: string): EventSubSubscriptionLike {
    this.created.push(id);
    return { id };
  }
}

/** Distinct sentinels: the assertions below care WHICH client a listener was handed. */
const BOT_CLIENT = { role: 'bot' } as unknown as ApiClient;
const BROADCASTER_CLIENT = { role: 'broadcaster' } as unknown as ApiClient;
const SHARED_CLIENT = { role: 'shared' } as unknown as ApiClient;

interface Harness {
  listeners: FakeListener[];
  timers: ManualTimers;
  reports: ChatSubscriptionLostReport[];
  health: HealthState;
  metrics: Metrics;
  /** Every chat line this bridge would have sent to Twitch, rendered. */
  sender: FakeChatSender;
  /** Records `POST /chat` (the on-screen panel) and `POST /stimulate` calls. */
  sim: FakeSimClient;
  result: ReturnType<typeof startEventSub>;
}

/** Let the `void handle*()` promises the handlers kick off settle before asserting. */
async function flush(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

function start(options: {
  botUserId: string;
  sharedEventSubApiClient?: ApiClient | null;
  quiet?: boolean;
  /** 0 lifts the 5 s global reply bucket, for a test that fires several commands at once. */
  globalReplyMs?: number;
}): Harness {
  const created: FakeListener[] = [];
  const timers = new ManualTimers();
  const reports: ChatSubscriptionLostReport[] = [];
  const health = new HealthState();
  const metrics = new Metrics();
  const sim = new FakeSimClient();
  const config = { gameTitle: 'Pokemon Red', featureQuiet: options.quiet ?? false };
  const sender = new FakeChatSender();
  const send = createSend(sender);

  const chatHealth = new ChatSubscriptionHealth({
    graceMs: 60_000,
    timers,
    clock: new FakeClock(),
    onUnhealthy: (report) => reports.push(report),
    log: () => undefined,
  });

  const result = startEventSub({
    botApiClient: BOT_CLIENT,
    broadcasterApiClient: BROADCASTER_CLIENT,
    sharedEventSubApiClient: options.sharedEventSubApiClient,
    botUserId: options.botUserId,
    broadcasterUserId: BROADCASTER_ID,
    send,
    sim,
    rateLimiter: new RateLimiter({
      perUserPerCommandMs: 60_000,
      globalReplyMs: options.globalReplyMs ?? 5_000,
      chatMessagesPer30s: 15,
      perCommandCooldownMs: {},
    }),
    config,
    health,
    metrics,
    explainer: new ExplainerPoster({ send, config, intervalMs: 1, silenceMs: 1 }),
    onscreen: new OnscreenChat({ sim, config: { featureOnscreenChat: true, botUser: '<twitch-channel>' }, metrics }),
    redemptions: null,
    chatHealth,
    createListener: ({ apiClient }) => {
      const listener = new FakeListener(apiClient);
      created.push(listener);
      return listener;
    },
  });

  return { listeners: created, timers, reports, health, metrics, sender, sim, result };
}

void test('one Twitch account for both roles collapses to ONE listener, on the shared client', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });

  assert.equal(h.listeners.length, 1, 'one websocket transport, not two');
  assert.equal(h.result.listenerCount, 1);
  assert.equal(h.result.collapsed, true);
  assert.equal(h.result.bot, h.result.broadcaster, 'both roles point at the same listener');
  assert.equal(h.listeners[0]!.apiClient, SHARED_CLIENT);
  assert.equal(h.listeners[0]!.starts, 1, 'started exactly once');
  assert.equal(h.health.eventSubListenerCount, 0, 'index.ts publishes the count, not startEventSub');

  // Chat, follow and raid all live on that one listener.
  assert.deepEqual(h.listeners[0]!.created, [
    `channel.chat.message.${BROADCASTER_ID}.${SAME_ACCOUNT_BOT_ID}`,
    `channel.follow.${BROADCASTER_ID}.${BROADCASTER_ID}`,
    `channel.raid.to.${BROADCASTER_ID}`,
  ]);
  assert.equal(h.result.chatSubscriptionId, `channel.chat.message.${BROADCASTER_ID}.${SAME_ACCOUNT_BOT_ID}`);
});

void test('separate bot and broadcaster accounts keep TWO listeners, one per role token', () => {
  const h = start({ botUserId: SEPARATE_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });

  assert.equal(h.listeners.length, 2);
  assert.equal(h.result.listenerCount, 2);
  assert.equal(h.result.collapsed, false);
  assert.notEqual(h.result.bot, h.result.broadcaster);
  assert.equal(h.listeners[0]!.apiClient, BOT_CLIENT, 'chat on the bot token');
  assert.equal(h.listeners[1]!.apiClient, BROADCASTER_CLIENT, 'follows and raids on the broadcaster token');
  assert.deepEqual(h.listeners[0]!.created, [`channel.chat.message.${BROADCASTER_ID}.${SEPARATE_BOT_ID}`]);
  assert.deepEqual(h.listeners[1]!.created, [
    `channel.follow.${BROADCASTER_ID}.${BROADCASTER_ID}`,
    `channel.raid.to.${BROADCASTER_ID}`,
  ]);
  assert.equal(h.listeners[0]!.starts, 1);
  assert.equal(h.listeners[1]!.starts, 1);
});

void test('one account with no shared client falls back to two listeners rather than dropping a role', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: null });
  assert.equal(h.listeners.length, 2);
  assert.equal(h.result.listenerCount, 2);
});

void test('the watchdog is armed from startup and disarmed by the first confirmation', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  assert.equal(h.health.chatSubscriptionHealthy, false, 'nothing is confirmed until Twitch says so');

  h.listeners[0]!.emitConnect(BROADCASTER_ID);
  assert.equal(h.health.eventSubConnected, true);
  // A connected socket is NOT a working chat subscription — that distinction is the whole outage.
  assert.equal(h.health.chatSubscriptionHealthy, false);

  h.listeners[0]!.emitCreateSuccess(h.result.chatSubscriptionId);
  assert.equal(h.health.chatSubscriptionHealthy, true);
  assert.equal(h.metrics.get('flybridge_eventsub_chat_subscription_confirmed_total'), 1);

  h.timers.fire();
  assert.deepEqual(h.reports, [], 'a confirmed subscription must never exit the process');
});

void test('a disconnect then a 429 transport-limit create failure, with no confirmation, asks for the exit', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  const listener = h.listeners[0]!;

  listener.emitConnect(BROADCASTER_ID);
  listener.emitCreateSuccess(h.result.chatSubscriptionId);
  assert.equal(h.health.chatSubscriptionHealthy, true);

  // 11:45:34 UTC: code 1006 on both sockets.
  listener.emitDisconnect(BROADCASTER_ID, new Error('Connection closed abnormally (1006)'));
  assert.equal(h.health.eventSubConnected, false);
  assert.equal(h.metrics.get('flybridge_eventsub_reconnects_total'), 1);

  // 11:45:35 and 11:45:45 UTC: twurple re-creates, Twitch refuses.
  const limit = Object.assign(new Error('number of websocket transports limit exceeded'), { statusCode: 429 });
  listener.emitCreateFailure(h.result.chatSubscriptionId, limit);
  listener.emitCreateFailure(h.result.chatSubscriptionId, limit);
  assert.equal(h.metrics.get('flybridge_eventsub_transport_limit_total'), 2);
  assert.equal(h.metrics.get('flybridge_eventsub_subscription_create_failures_total'), 2);
  assert.equal(h.reports.length, 0, 'still inside the grace period');
  assert.equal(h.health.chatSubscriptionHealthy, false, '/health says chat is dead while the grace runs');

  // 11:46:34 UTC: the grace period is up and nothing has confirmed.
  h.timers.fire();
  assert.equal(h.reports.length, 1);
  assert.equal(h.reports[0]!.transportLimitHit, true);
  assert.equal(h.health.chatSubscriptionHealthy, false);
});

void test('a confirmation after the reconnect, inside the grace period, does not exit', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  const listener = h.listeners[0]!;

  listener.emitConnect(BROADCASTER_ID);
  listener.emitCreateSuccess(h.result.chatSubscriptionId);
  listener.emitDisconnect(BROADCASTER_ID, new Error('Connection closed abnormally (1006)'));
  listener.emitCreateFailure(h.result.chatSubscriptionId, new Error('number of websocket transports limit exceeded'));
  // The healthy path: the next attempt lands.
  listener.emitConnect(BROADCASTER_ID);
  listener.emitCreateSuccess(h.result.chatSubscriptionId);

  h.timers.fire();
  assert.deepEqual(h.reports, []);
  assert.equal(h.health.chatSubscriptionHealthy, true);
  assert.equal(h.health.eventSubConnected, true);
});

void test('a create failure on a subscription that is not chat does not arm the watchdog', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  const listener = h.listeners[0]!;
  listener.emitCreateSuccess(h.result.chatSubscriptionId);

  listener.emitCreateFailure(`channel.follow.${BROADCASTER_ID}.${BROADCASTER_ID}`, new Error('missing scope'));
  h.timers.fire();

  assert.deepEqual(h.reports, [], 'losing follows must not restart the chat bridge');
  assert.equal(h.metrics.get('flybridge_eventsub_subscription_create_failures_total'), 1);
  assert.equal(h.metrics.get('flybridge_eventsub_chat_subscription_lost_total'), 0);
  assert.equal(h.health.chatSubscriptionHealthy, true);
});

void test('Twitch revoking the chat subscription arms the watchdog and shows up in /health', () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  const listener = h.listeners[0]!;
  listener.emitCreateSuccess(h.result.chatSubscriptionId);

  listener.emitRevoke(h.result.chatSubscriptionId, 'authorization_revoked');

  assert.equal(
    h.health.subscriptions.find((s) => s.type === h.result.chatSubscriptionId)?.status,
    'authorization_revoked',
  );
  h.timers.fire();
  assert.equal(h.reports.length, 1);
  assert.match(h.reports[0]!.reasons[0]!, /revoked by Twitch/);
});

void test('with two listeners, only the chat one arms the watchdog on a socket drop', () => {
  const h = start({ botUserId: SEPARATE_BOT_ID, sharedEventSubApiClient: null });
  const [chatListener, broadcasterListener] = [h.listeners[0]!, h.listeners[1]!];
  chatListener.emitConnect(SEPARATE_BOT_ID);
  broadcasterListener.emitConnect(BROADCASTER_ID);
  chatListener.emitCreateSuccess(h.result.chatSubscriptionId);

  broadcasterListener.emitDisconnect(BROADCASTER_ID, new Error('1006'));
  h.timers.fire();
  assert.deepEqual(h.reports, [], 'the redemptions socket dropping does not kill chat');
  assert.equal(h.health.eventSubConnected, false, 'but /health still reports the bridge as degraded');
});

void test('stop() stops every distinct listener exactly once, and the collapsed one only once', () => {
  const collapsed = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  collapsed.result.stop();
  assert.equal(collapsed.listeners[0]!.stops, 1);

  const split = start({ botUserId: SEPARATE_BOT_ID, sharedEventSubApiClient: null });
  split.result.stop();
  assert.equal(split.listeners[0]!.stops, 1);
  assert.equal(split.listeners[1]!.stops, 1);
  // Shutdown must not race a self-healing exit.
  split.timers.fire();
  assert.deepEqual(split.reports, []);
});

// -- Quiet mode (FEATURE_QUIET) ----------------------------------------------------------------

void test('quiet mode keeps the follow and raid subscriptions, counts them, and says nothing', async () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT, quiet: true });
  const listener = h.listeners[0]!;

  // The subscriptions are still there — /health and the metrics must not change shape just
  // because the bridge stopped greeting.
  assert.deepEqual(listener.created, [
    `channel.chat.message.${BROADCASTER_ID}.${SAME_ACCOUNT_BOT_ID}`,
    `channel.follow.${BROADCASTER_ID}.${BROADCASTER_ID}`,
    `channel.raid.to.${BROADCASTER_ID}`,
  ]);

  listener.emitFollow('fly_fan_42');
  listener.emitRaid('another_streamer', 12);
  await flush();

  assert.deepEqual(h.sender.sent, [], 'no follow or raid thanks in quiet mode');
  assert.equal(h.metrics.get('flybridge_follows_total'), 1);
  assert.equal(h.metrics.get('flybridge_raids_total'), 1);
});

void test('follows and raids are thanked, and counted, when quiet mode is off', async () => {
  const h = start({ botUserId: SAME_ACCOUNT_BOT_ID, sharedEventSubApiClient: SHARED_CLIENT });
  h.listeners[0]!.emitFollow('fly_fan_42');
  h.listeners[0]!.emitRaid('another_streamer', 12);
  await flush();

  assert.equal(h.sender.sent.length, 2);
  assert.match(h.sender.sent[0]!, /Thanks for the follow, fly_fan_42/);
  assert.match(h.sender.sent[1]!, /Thanks for the raid, another_streamer/);
  assert.equal(h.metrics.get('flybridge_follows_total'), 1);
  assert.equal(h.metrics.get('flybridge_raids_total'), 1);
});

void test('quiet mode still answers every command and still forwards chat to the panel', async () => {
  const h = start({
    botUserId: SAME_ACCOUNT_BOT_ID,
    sharedEventSubApiClient: SHARED_CLIENT,
    quiet: true,
    globalReplyMs: 0,
  });
  const listener = h.listeners[0]!;

  for (const [index, command] of ['!fly', '!brain', '!how', '!stuck', '!sugar'].entries()) {
    // A fresh chatter id per command would be simpler, but the per-user-per-command bucket is
    // keyed by (user, command), so one viewer can use all five.
    listener.emitChat(command, `viewer${String(index)}`);
  }
  await flush();

  assert.equal(h.sender.sent.length, 5, 'spoken to, so it speaks: one reply per command');
  assert.match(h.sender.sent[0]!, /simulated fly brain/);
  assert.match(h.sender.sent[2]!, /!fly what this is/);
  assert.match(h.sender.sent[3]!, /Current milestone/);
  assert.match(h.sender.sent[4]!, /Sugar from/);
  assert.equal(h.sim.stimulateCalls.length, 1, '!sugar still reaches the sim');

  // Commands are not chatter and never go on the panel; a plain viewer line does.
  listener.emitChat('the fly just walked into a wall', 'chatty_viewer');
  await flush();
  assert.equal(h.sim.chatCalls.length, 1);
  assert.equal(h.sim.chatCalls[0]!.text, 'the fly just walked into a wall');
});
