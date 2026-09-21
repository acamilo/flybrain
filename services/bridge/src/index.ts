/**
 * flybridge entrypoint: loads config, asserts scopes, wires chat/EventSub/redemptions/health,
 * posts the startup notice, and shuts down gracefully on SIGINT/SIGTERM.
 */
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { ApiClient } from '@twurple/api';
import { assertStartupScopes, buildAuthProvider, loadTokensFile } from './auth';
import { createSend, TwurpleChatSender } from './chat';
import { loadConfig } from './config';
import { startEventSub } from './eventsub';
import { explainerEnabled, ExplainerPoster } from './explainer';
import { HealthState, Metrics, startHealthServer } from './health';
import { NoticeLog } from './notice';
import { OnscreenChat, wrapSendWithOnscreenEcho } from './onscreen-chat';
import { createDisabledPredictionsManager } from './predictions';
import { renderTemplate } from './templates';
import { RateLimiter } from './ratelimit';
import { ChatSubscriptionHealth, createSelfHealExit } from './subscription-health';
import {
  FileIntentStore,
  isChannelPointsAffiliateRefusal,
  RedemptionManager,
  TwurpleRedemptionApi,
} from './redemptions';
import { HttpSimClient } from './sim';

const EXPLAINER_TICK_MS = 60_000;
const SUGAR_REWARD_TITLE = 'Sugar';

async function main(): Promise<void> {
  const config = loadConfig();
  const tokens = await loadTokensFile(config.tokensFile);
  await assertStartupScopes(config, tokens);

  const {
    botAuthProvider,
    broadcasterAuthProvider,
    botUserId,
    broadcasterUserId,
    sameAccount,
    sharedEventSubAuthProvider,
  } = buildAuthProvider(config, tokens);
  // One client per identity. See `buildAuthProvider` in src/auth.ts: a single provider holding both
  // tokens loses one of them whenever the two roles are the same Twitch account.
  const botApiClient = new ApiClient({ authProvider: botAuthProvider });
  const broadcasterApiClient = new ApiClient({ authProvider: broadcasterAuthProvider });
  // Same account: a THIRD client over the scope-routing provider, used only by the single
  // collapsed EventSub listener, so this account spends one of Twitch's three websocket
  // transports instead of two (src/eventsub.ts, and the incident in src/subscription-health.ts).
  // Chat sends still go through botApiClient and Channel Points through broadcasterApiClient;
  // those are plain Helix calls with no transport budget.
  const sharedEventSubApiClient =
    sharedEventSubAuthProvider === null ? null : new ApiClient({ authProvider: sharedEventSubAuthProvider });
  if (sameAccount) {
    console.log(
      `flybridge: bot and broadcaster are the same Twitch account (${botUserId}); one auth provider per role, ` +
        'and one shared EventSub websocket for both',
    );
  }

  const sim = new HttpSimClient({ baseUrl: config.simControlUrl, timeoutMs: config.simTimeoutMs });
  const sender = new TwurpleChatSender(botApiClient, botUserId, broadcasterUserId);
  const rateLimiter = new RateLimiter(config.rateLimits);
  const health = new HealthState();
  const metrics = new Metrics();

  // The bridge's own replies go on screen too, marked as the bot's: wrap `send()` once here, so
  // every template reply echoes to `POST /chat` after Twitch has it, and no call site has to know.
  const onscreen = new OnscreenChat({ sim, config, metrics });
  const send = wrapSendWithOnscreenEcho(createSend(sender), onscreen, renderTemplate);

  // Quiet mode, and `EXPLAINER_INTERVAL_MS=0`, both mean there is no rotation to run: the poster
  // is still constructed (the chat handler calls `noteChatActivity()` unconditionally) but it is
  // disabled, and no interval timer is armed at all.
  const explainerRunning = explainerEnabled(config);
  const explainer = new ExplainerPoster({
    send,
    config,
    intervalMs: config.explainerIntervalMs,
    silenceMs: config.explainerSilenceMs,
    enabled: explainerRunning,
  });
  const explainerTimer = explainerRunning ? setInterval(() => void explainer.tick(), EXPLAINER_TICK_MS) : null;
  if (config.featureQuiet) {
    console.log(
      'flybridge: FEATURE_QUIET is on — no startup/recovery notice, no explainer rotation, no follow ' +
        'or raid thanks. Commands (!fly !brain !how !stuck !sugar) and the on-screen CHAT panel are ' +
        'unaffected, and follows/raids are still subscribed and counted.',
    );
  } else if (!explainerRunning) {
    console.log('flybridge: EXPLAINER_INTERVAL_MS=0 — the explainer rotation is off');
  }

  let redemptions: RedemptionManager | null = null;
  if (config.featureRedemptions) {
    const api = new TwurpleRedemptionApi(broadcasterApiClient, broadcasterUserId);
    const store = new FileIntentStore(config.redemptionStateFile);
    const manager = new RedemptionManager({ sim, api, store, send, rewardTitle: SUGAR_REWARD_TITLE });
    try {
      await manager.start();
      redemptions = manager;
    } catch (cause) {
      // Channel Points are Affiliate-gated and the scope assertion cannot see that: the token can
      // carry channel:manage:redemptions on a channel that has no Channel Points at all. Losing
      // Sugar must not cost the channel its chat bot — the same reason fly.target Wants= this unit
      // rather than Requires= it. Every other failure still stops startup.
      if (!isChannelPointsAffiliateRefusal(cause)) throw cause;
      console.error(
        'flybridge: Channel Points are unavailable on this channel (Twitch: "The broadcaster must ' +
          'have partner or affiliate status"), so there is no Sugar reward. Continuing with ' +
          'redemptions DISABLED; chat, follows and raids are unaffected. Set FEATURE_REDEMPTIONS=0 ' +
          'to silence this, or turn it back on once the channel reaches Affiliate.',
      );
    }
  }

  const predictions = createDisabledPredictionsManager();
  await predictions.start();

  // The notice log is read before anything can post, because it also carries the marker the
  // self-healing exit leaves behind (src/notice.ts).
  const notices = new NoticeLog({
    path: config.noticeStateFile,
    minIntervalMs: config.noticeMinIntervalMs,
    quiet: config.featureQuiet,
  });
  await notices.load();
  const pendingRecovery = notices.current.pendingRecovery;
  if (pendingRecovery) {
    health.lastSelfHealAtIso = pendingRecovery.atIso;
    console.log(
      `flybridge: previous process exited to recover a dead chat subscription at ${pendingRecovery.atIso} ` +
        `(${pendingRecovery.reason})`,
    );
  }

  // The self-healing watchdog. `onUnhealthy` is the whole recovery: persist the marker so the
  // next start can tell chat, then exit non-zero and let systemd (Restart=always, RestartSec=15,
  // StartLimitIntervalSec=0 — infra/units/flybridge.service) bring up a process with a fresh
  // websocket transport. Nothing here retries in-process: a 429 "websocket transports limit
  // exceeded" is cleared by time, not by another attempt.
  const chatHealth = new ChatSubscriptionHealth({
    graceMs: config.eventsubGraceMs,
    onUnhealthy: createSelfHealExit({
      notices,
      beforeExit: () => {
        health.chatSubscriptionHealthy = false;
      },
    }),
  });

  const listeners = startEventSub({
    botApiClient,
    broadcasterApiClient,
    sharedEventSubApiClient,
    botUserId,
    broadcasterUserId,
    send,
    sim,
    rateLimiter,
    config,
    health,
    metrics,
    explainer,
    onscreen,
    redemptions,
    chatHealth,
  });
  health.eventSubListenerCount = listeners.listenerCount;

  const healthServer = await startHealthServer({
    host: config.healthHost,
    port: config.healthPort,
    sim,
    state: health,
    metrics,
  });

  console.log(
    `flybridge: EventSub subscriptions created: ${health.subscriptions.map((s) => s.type).join(', ')}`,
  );
  console.log(
    `flybridge: ${String(listeners.listenerCount)} EventSub websocket transport(s); watching ` +
      `${listeners.chatSubscriptionId} with a ${String(config.eventsubGraceMs)} ms grace`,
  );
  console.log(`flybridge: /health on http://${config.healthHost}:${String(config.healthPort)}/health`);

  // One notice per ten minutes at most, across restarts, and "reconnected" rather than "online"
  // when the previous process exited itself. The two branches are spelled out because
  // tests/templates.test.ts requires every send() call site to name a literal TemplateId.
  const decision = notices.decide();
  if (decision.post === null && decision.quiet === true) {
    console.log('flybridge: startup notice suppressed by FEATURE_QUIET');
  } else if (decision.post === null) {
    console.log(
      `flybridge: startup notice suppressed, another notice went out ${String(
        Math.round((config.noticeMinIntervalMs - decision.suppressedForMs) / 1000),
      )} s ago (one per ${String(Math.round(config.noticeMinIntervalMs / 1000))} s)`,
    );
  } else if (decision.post === 'recovered') {
    await send('recovered', { channel: config.channel, gameTitle: config.gameTitle });
    await notices.markPosted();
    console.log(`flybridge: recovery notice posted to #${config.channel} as ${config.botUser}`);
  } else {
    await send('startup', { channel: config.channel, gameTitle: config.gameTitle });
    await notices.markPosted();
    console.log(`flybridge: startup notice posted to #${config.channel} as ${config.botUser}`);
  }

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`flybridge: received ${signal}, shutting down`);
    if (explainerTimer !== null) clearInterval(explainerTimer);
    chatHealth.stop();
    listeners.stop();
    predictions.stop();
    await healthServer.close();
    process.exit(0);
  };
  process.on('SIGINT', () => void shutdown('SIGINT'));
  process.on('SIGTERM', () => void shutdown('SIGTERM'));
}

/**
 * Whether this module was executed directly, rather than imported by a test.
 *
 * Compares REAL paths, not URLs. The naive `import.meta.url === new URL(argv[1], 'file://').href`
 * is true only when the path systemd exec'd is already canonical, and in production it is not:
 * ExecStart runs `/opt/fly/current/bridge/index.js`, `/opt/fly/current` is the release symlink
 * 05-deploy.sh flips, and Node canonicalises ESM specifiers — so `import.meta.url` reported
 * `file:///opt/fly/releases/v0.1.0/bridge/index.js` while `argv[1]` was the `current` path. The
 * comparison failed, `main()` was never called, and the process exited 0 having done nothing:
 * systemd logged "Started" then "Deactivated successfully" and restarted it every 10 s forever,
 * with not one line of output to say why. Measured on the release container, 2026-09-16.
 */
const isMainModule = (() => {
  const entry = process.argv[1];
  if (entry === undefined) return false;
  const here = fileURLToPath(import.meta.url);
  if (here === entry) return true;
  try {
    return here === realpathSync(entry);
  } catch {
    return false;
  }
})();

if (isMainModule) {
  main().catch((error: unknown) => {
    console.error(error);
    process.exit(1);
  });
}
