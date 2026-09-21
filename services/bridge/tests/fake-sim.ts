/**
 * A minimal, fully scriptable fake of flysim's control API (`docs/control-api.md`), for tests
 * that need precise control over 429/500/timeout responses that `packages/feed`'s fake
 * simulator (built for realistic feed data, not fault injection) doesn't offer. Started on an
 * ephemeral port so tests can run in parallel.
 */
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import type {
  ChatRequest,
  EventsResponse,
  HealthzResponse,
  StatusResponse,
  StimulateRequest,
} from '@flybrain/feed';

export type ScriptedStimulateResponse =
  | { kind: 'accept'; eventId?: number }
  | { kind: 'rate_limited'; retryAfterMs: number }
  | { kind: 'forbidden'; error?: string }
  | { kind: 'error'; status: number; error?: string }
  | { kind: 'timeout'; delayMs?: number };

/** `POST /chat` outcomes (`docs/control-api.md`): 202, 403 kill switch, 422 refused, 429 limited. */
export type ScriptedChatResponse =
  | { kind: 'accept'; eventId?: number }
  | { kind: 'refused'; reason: string }
  | { kind: 'rate_limited'; retryAfterMs: number }
  | { kind: 'disabled' }
  | { kind: 'error'; status: number; error?: string }
  | { kind: 'timeout'; delayMs?: number };

export interface FakeSimHandle {
  port: number;
  baseUrl: string;
  requests: { path: string; body: unknown }[];
  queueStimulateResponse(response: ScriptedStimulateResponse): void;
  queueChatResponse(response: ScriptedChatResponse): void;
  setStatus(status: StatusResponse): void;
  setHealthy(healthy: boolean): void;
  close(): Promise<void>;
}

const DEFAULT_STATUS: StatusResponse = {
  protocol: 1,
  seq: 0,
  wallMs: 0,
  status: 'running',
  realtimeFactor: 1,
  uptimeSeconds: 0,
  runSeconds: 0,
  brainMs: 0,
  frame: 0,
  buttons: 0,
  rates: {},
  populationRate: 0,
  spikeCount: 0,
  learning: { enabled: false, updates: 0, changed: 0, synapses: 0, signal: 0 },
  game: {
    mode: 'BOOT',
    semanticRewards: true,
    map: null,
    badges: 0,
    uniqueLocations: 0,
    rewardTotal: 0,
    rewardCounts: { story: 0, explore: 0, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
  },
  milestone: { rank: 0, label: 'Booting', next: 'Left the bedroom', sinceSeconds: 0, attempts: 0 },
  sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
  version: { kernel: 'test', plasticity: 'test', adapter: 'test', binjgb: 'test', dataset: 'test' },
  checkpoint: { latestWallMs: 0, generation: 0 },
};

export async function startFakeSim(): Promise<FakeSimHandle> {
  const stimulateQueue: ScriptedStimulateResponse[] = [];
  const chatQueue: ScriptedChatResponse[] = [];
  const requests: { path: string; body: unknown }[] = [];
  let status: StatusResponse = { ...DEFAULT_STATUS };
  let healthy = true;
  let nextEventId = 1;

  const server = http.createServer((req, res) => {
    void handle(req, res);
  });

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');

    if (req.method === 'GET' && url.pathname === '/status') {
      requests.push({ path: '/status', body: undefined });
      sendJson(res, 200, status);
      return;
    }

    if (req.method === 'GET' && url.pathname === '/healthz') {
      requests.push({ path: '/healthz', body: undefined });
      if (healthy) sendJson(res, 200, { status: 'ok' } satisfies HealthzResponse);
      else sendJson(res, 503, { error: 'loop has not advanced' });
      return;
    }

    if (req.method === 'GET' && url.pathname === '/events') {
      requests.push({ path: '/events', body: undefined });
      sendJson(res, 200, { events: [] } satisfies EventsResponse);
      return;
    }

    if (req.method === 'POST' && url.pathname === '/chat') {
      const body = (await readJsonBody(req)) as ChatRequest;
      requests.push({ path: '/chat', body });
      const scripted = chatQueue.shift() ?? { kind: 'accept' as const };
      switch (scripted.kind) {
        case 'accept':
          sendJson(res, 202, { eventId: scripted.eventId ?? nextEventId++ });
          return;
        case 'refused':
          sendJson(res, 422, { error: `chat line refused: ${scripted.reason}` });
          return;
        case 'rate_limited':
          sendJson(res, 429, { retryAfterMs: scripted.retryAfterMs });
          return;
        case 'disabled':
          sendJson(res, 403, { error: 'chat is disabled (chat.enabled = false)' });
          return;
        case 'error':
          sendJson(res, scripted.status, { error: scripted.error ?? 'error' });
          return;
        case 'timeout': {
          const timer = setTimeout(() => {
            try {
              sendJson(res, 202, { eventId: nextEventId++ });
            } catch {
              // client already gave up
            }
          }, scripted.delayMs ?? 60_000);
          timer.unref();
          return;
        }
      }
    }

    if (req.method === 'POST' && url.pathname === '/stimulate') {
      const body = (await readJsonBody(req)) as StimulateRequest;
      requests.push({ path: '/stimulate', body });
      const scripted = stimulateQueue.shift() ?? { kind: 'accept' };
      await respondStimulate(res, scripted);
      return;
    }

    sendJson(res, 404, { error: `no such route: ${req.method ?? ''} ${url.pathname}` });
  }

  async function respondStimulate(res: ServerResponse, scripted: ScriptedStimulateResponse): Promise<void> {
    switch (scripted.kind) {
      case 'accept':
        sendJson(res, 202, { eventId: scripted.eventId ?? nextEventId++ });
        return;
      case 'rate_limited':
        sendJson(res, 429, { retryAfterMs: scripted.retryAfterMs });
        return;
      case 'forbidden':
        sendJson(res, 403, { error: scripted.error ?? 'forbidden' });
        return;
      case 'error':
        sendJson(res, scripted.status, { error: scripted.error ?? 'error' });
        return;
      case 'timeout': {
        // Deliberately never respond within any sane client timeout. Unref'd so it never keeps
        // the test process alive; the response is simply dropped when the server closes.
        const timer = setTimeout(() => {
          try {
            sendJson(res, 202, { eventId: nextEventId++ });
          } catch {
            // client already gave up; nothing to do
          }
        }, scripted.delayMs ?? 60_000);
        timer.unref();
        return;
      }
    }
  }

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });

  const address = server.address();
  const port = typeof address === 'object' && address !== null ? address.port : 0;

  return {
    port,
    baseUrl: `http://127.0.0.1:${port}`,
    requests,
    queueStimulateResponse(response: ScriptedStimulateResponse): void {
      stimulateQueue.push(response);
    },
    queueChatResponse(response: ScriptedChatResponse): void {
      chatQueue.push(response);
    },
    setStatus(newStatus: StatusResponse): void {
      status = newStatus;
    },
    setHealthy(value: boolean): void {
      healthy = value;
    },
    close(): Promise<void> {
      return new Promise((resolve) => server.close(() => resolve()));
    },
  };
}

function sendJson(res: ServerResponse, statusCode: number, body: unknown): void {
  const bytes = Buffer.from(JSON.stringify(body));
  res.writeHead(statusCode, { 'content-type': 'application/json', 'content-length': bytes.byteLength });
  res.end(bytes);
}

function readJsonBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => {
      if (chunks.length === 0) {
        resolve(undefined);
        return;
      }
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString('utf8')));
      } catch (cause) {
        reject(new Error(`invalid JSON body: ${(cause as Error).message}`));
      }
    });
    req.on('error', reject);
  });
}
