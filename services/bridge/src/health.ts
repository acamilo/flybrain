/**
 * `/health` (JSON) and `/metrics` (Prometheus text) on `node:http`, per `docs/design/stage-bridge.md`
 * B4: token expiry, EventSub subscription list and socket state, sim reachability, last redemption
 * outcome; commands served, replies suppressed by rate limit, redemptions fulfilled/refunded,
 * EventSub reconnects, sim call latency.
 *
 * `HealthState` is a small mutable struct other modules update as things happen (`src/eventsub.ts`
 * sets `eventSubConnected`/`subscriptions`, `src/redemptions.ts` sets `lastRedemptionOutcome`);
 * sim reachability is checked live on each `/health` request via `SimClient.healthz()` rather
 * than cached, so the answer is never stale.
 */
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import type { SimClient } from './sim';

export interface TokenExpiryInfo {
  /** ISO 8601, or `null` if unknown or the token never expires. */
  bot: string | null;
  broadcaster: string | null;
}

export interface SubscriptionInfo {
  type: string;
  status: string;
}

export interface RedemptionOutcome {
  status: 'FULFILLED' | 'CANCELED';
  atIso: string;
  by: string;
}

export class HealthState {
  tokenExpiry: TokenExpiryInfo = { bot: null, broadcaster: null };
  subscriptions: SubscriptionInfo[] = [];
  eventSubConnected = false;
  lastRedemptionOutcome: RedemptionOutcome | null = null;
  /**
   * Whether the `channel.chat.message` subscription is confirmed created (`src/eventsub.ts` sets
   * it from `onSubscriptionCreateSuccess`). `eventSubConnected` is NOT the same question: on
   * 2026-09-16 the sockets were connected and this was false for an hour, which is the whole
   * reason `src/subscription-health.ts` exists. Reported separately so a probe can tell them
   * apart.
   */
  chatSubscriptionHealthy = false;
  /** How many distinct EventSub websocket transports this process holds (1 or 2). */
  eventSubListenerCount = 0;
  /** Set at startup when the previous process exited itself to recover a dead chat subscription. */
  lastSelfHealAtIso: string | null = null;
}

/** Prometheus counters/gauges. Names are prefixed `flybridge_`. */
export class Metrics {
  private readonly counters = new Map<string, number>();
  private readonly latenciesMs: number[] = [];
  private static readonly MAX_LATENCY_SAMPLES = 200;

  increment(name: string, by = 1): void {
    this.counters.set(name, (this.counters.get(name) ?? 0) + by);
  }

  get(name: string): number {
    return this.counters.get(name) ?? 0;
  }

  observeSimCallLatencyMs(ms: number): void {
    this.latenciesMs.push(ms);
    if (this.latenciesMs.length > Metrics.MAX_LATENCY_SAMPLES) this.latenciesMs.shift();
  }

  toPrometheusText(): string {
    const lines: string[] = [];
    for (const [name, value] of [...this.counters.entries()].sort(([a], [b]) => a.localeCompare(b))) {
      lines.push(`# TYPE ${name} counter`);
      lines.push(`${name} ${value}`);
    }
    const avgLatencyMs =
      this.latenciesMs.length > 0 ? this.latenciesMs.reduce((sum, ms) => sum + ms, 0) / this.latenciesMs.length : 0;
    lines.push('# TYPE flybridge_sim_call_latency_ms_avg gauge');
    lines.push(`flybridge_sim_call_latency_ms_avg ${avgLatencyMs.toFixed(2)}`);
    return `${lines.join('\n')}\n`;
  }
}

export interface HealthServerOptions {
  host: string;
  port: number;
  sim: SimClient;
  state: HealthState;
  metrics: Metrics;
}

export interface HealthServerHandle {
  port: number;
  close(): Promise<void>;
}

export async function startHealthServer(options: HealthServerOptions): Promise<HealthServerHandle> {
  const server = http.createServer((req, res) => {
    void handleRequest(req, res, options);
  });

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(options.port, options.host, () => resolve());
  });

  const address = server.address();
  const port = typeof address === 'object' && address !== null ? address.port : options.port;

  return {
    port,
    close(): Promise<void> {
      return new Promise((resolve) => server.close(() => resolve()));
    },
  };
}

async function handleRequest(req: IncomingMessage, res: ServerResponse, options: HealthServerOptions): Promise<void> {
  const url = new URL(req.url ?? '/', 'http://127.0.0.1');

  if (req.method === 'GET' && url.pathname === '/health') {
    const healthz = await options.sim.healthz();
    sendJson(res, 200, {
      tokenExpiry: options.state.tokenExpiry,
      subscriptions: options.state.subscriptions,
      eventSubConnected: options.state.eventSubConnected,
      eventSubListenerCount: options.state.eventSubListenerCount,
      chatSubscriptionHealthy: options.state.chatSubscriptionHealthy,
      lastSelfHealAtIso: options.state.lastSelfHealAtIso,
      simReachable: healthz.ok,
      lastRedemptionOutcome: options.state.lastRedemptionOutcome,
    });
    return;
  }

  if (req.method === 'GET' && url.pathname === '/metrics') {
    const text = options.metrics.toPrometheusText();
    res.writeHead(200, { 'content-type': 'text/plain; version=0.0.4' });
    res.end(text);
    return;
  }

  res.writeHead(404, { 'content-type': 'text/plain' });
  res.end('not found');
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const bytes = Buffer.from(JSON.stringify(body));
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': bytes.byteLength });
  res.end(bytes);
}
