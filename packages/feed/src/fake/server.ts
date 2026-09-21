/**
 * Runnable fake flysim for local development and tests.
 *
 * ```sh
 * npx tsx packages/feed/src/fake/server.ts [--feed-port 7400] [--control-port 7401] \
 *   [--scenario boot|running|stuck|milestone] [--mode raw|macros] [--allow-reward] \
 *   [--seed 12345] [--spikes-per-tick 30000]
 * ```
 *
 * Serves the WebSocket feed (`docs/feed-protocol.md`) and the HTTP control API
 * (`docs/control-api.md`) with synthetic but plausible data from `./simulator.ts`. Also
 * importable as a library (`startFakeServer()`) so tests can start it on ephemeral ports without
 * spawning a child process.
 */
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import { WebSocket, WebSocketServer, type RawData } from 'ws';
import { encodeSnapshot } from '../codec';
import { CONTROL_PORT, FEED_PORT } from '../types';
import type {
  AttachmentKind,
  MacroMode,
  ChatRequest,
  ClientHello,
  ErrorResponse,
  RewardRequest,
  StimulateRequest,
} from '../types';
import { FakeFlysim, type FakeScenario } from './simulator';

export { FakeFlysim, type FakeScenario, type FakeSnapshot } from './simulator';

const RUNNING_INTERVAL_MS = 1000 / 30;
const IDLE_INTERVAL_MS = 1000 / 2;

export interface StartFakeServerOptions {
  feedPort?: number;
  controlPort?: number;
  scenario?: FakeScenario;
  allowReward?: boolean;
  seed?: number;
  /** `[chat] enabled`. False makes `POST /chat` 403 and omits `chat` from the header. */
  chatEnabled?: boolean;
  /** Scripted viewer chatter. False leaves the ring to whatever posts to `/chat`. */
  chatChatter?: boolean;
  /** Approximate spikes generated per snapshot. Defaults to the live service's measured ~30,000. */
  spikesPerTick?: number;
  /** `[macros] mode`: `raw` (the default, as in the real service) or `macros`. */
  macroMode?: MacroMode;
  /**
   * Deal the fake's widest pad in every scene, for a fixture recorded for the screen
   * (`docs/design/macros.md` section 14).
   *
   * Without it the random walk reaches nine macros in one scene and three in the next, so the
   * strip's second column and the keyboard's lit cells are spotty. With it every overworld walk
   * binds all nine of the outdoor macros at once, which is what the `bigpad` fixture was recorded
   * with and what the section-14 mockups regress.
   */
  fullPad?: boolean;
}

export interface FakeServerHandle {
  feedPort: number;
  controlPort: number;
  simulator: FakeFlysim;
  close(): Promise<void>;
}

interface ClientInfo {
  wants: Set<AttachmentKind>;
}

/** Start the fake feed + control servers. Pass `feedPort: 0` / `controlPort: 0` for ephemeral ports. */
export async function startFakeServer(options: StartFakeServerOptions = {}): Promise<FakeServerHandle> {
  const simulator = new FakeFlysim({
    scenario: options.scenario ?? 'running',
    allowReward: options.allowReward ?? false,
    seed: options.seed,
    chatEnabled: options.chatEnabled ?? true,
    chatChatter: options.chatChatter ?? true,
    spikesPerTick: options.spikesPerTick,
    macroMode: options.macroMode ?? 'raw',
    fullPad: options.fullPad ?? false,
  });

  const clients = new Map<WebSocket, ClientInfo>();

  const feedHttp = http.createServer((_req, res) => {
    res.writeHead(404, { 'content-type': 'text/plain' });
    res.end('not found');
  });
  const wss = new WebSocketServer({ server: feedHttp, path: '/feed' });

  wss.on('connection', (socket) => {
    const onMessage = (data: RawData) => {
      let hello: ClientHello;
      try {
        hello = JSON.parse(data.toString());
      } catch {
        socket.close(1002, 'hello must be JSON');
        return;
      }
      if (hello.protocol !== 1) {
        socket.close(1002, `unsupported protocol ${String(hello.protocol)}`);
        return;
      }
      clients.set(socket, { wants: new Set(hello.wants ?? []) });
      socket.off('message', onMessage);
    };
    socket.on('message', onMessage);
    socket.on('close', () => clients.delete(socket));
    socket.on('error', () => clients.delete(socket));
  });

  let lastTickAt = Date.now();
  let timer: ReturnType<typeof setTimeout> | null = null;

  const publish = (): void => {
    const now = Date.now();
    const dtMs = Math.max(1, now - lastTickAt);
    lastTickAt = now;

    const snapshot = simulator.tick(dtMs);

    for (const [socket, info] of clients) {
      if (socket.readyState !== WebSocket.OPEN) continue;

      const kinds: AttachmentKind[] = [];
      const attachmentsForClient: Partial<Record<AttachmentKind, Uint8Array>> = {};
      for (const kind of snapshot.header.attachments) {
        if (info.wants.has(kind)) {
          kinds.push(kind);
          attachmentsForClient[kind] = snapshot.attachments[kind];
        }
      }

      const headerForClient = { ...snapshot.header, attachments: kinds };
      socket.send(encodeSnapshot(headerForClient, attachmentsForClient));
    }

    const nextIntervalMs = snapshot.header.status === 'running' ? RUNNING_INTERVAL_MS : IDLE_INTERVAL_MS;
    timer = setTimeout(publish, nextIntervalMs);
  };

  const controlHttp = http.createServer((req, res) => {
    void handleControlRequest(simulator, req, res);
  });

  await Promise.all([
    listen(feedHttp, options.feedPort ?? FEED_PORT),
    listen(controlHttp, options.controlPort ?? CONTROL_PORT),
  ]);

  timer = setTimeout(publish, RUNNING_INTERVAL_MS);

  const feedPort = (feedHttp.address() as { port: number }).port;
  const controlPort = (controlHttp.address() as { port: number }).port;

  return {
    feedPort,
    controlPort,
    simulator,
    async close(): Promise<void> {
      if (timer) clearTimeout(timer);
      for (const socket of clients.keys()) socket.terminate();
      await Promise.all([closeServer(wss), closeHttp(feedHttp), closeHttp(controlHttp)]);
    },
  };
}

function listen(server: http.Server, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => resolve());
  });
}

function closeHttp(server: http.Server): Promise<void> {
  return new Promise((resolve) => server.close(() => resolve()));
}

function closeServer(server: WebSocketServer): Promise<void> {
  return new Promise((resolve) => server.close(() => resolve()));
}

// -- Control API (docs/control-api.md) -----------------------------------------------------

async function handleControlRequest(simulator: FakeFlysim, req: IncomingMessage, res: ServerResponse): Promise<void> {
  const url = new URL(req.url ?? '/', 'http://127.0.0.1');
  const method = req.method ?? 'GET';

  try {
    if (method === 'GET' && url.pathname === '/status') {
      sendJson(res, 200, simulator.status());
      return;
    }

    if (method === 'GET' && url.pathname === '/healthz') {
      if (simulator.isHealthy()) {
        sendJson(res, 200, { status: 'ok' });
      } else {
        sendJson(res, 503, { error: 'loop has not advanced in the last 2 seconds' } satisfies ErrorResponse);
      }
      return;
    }

    if (method === 'GET' && url.pathname === '/events') {
      const since = Number.parseInt(url.searchParams.get('since') ?? '0', 10) || 0;
      const limitParam = url.searchParams.get('limit');
      const limit = limitParam ? Number.parseInt(limitParam, 10) || 100 : 100;
      sendJson(res, 200, simulator.events(since, limit));
      return;
    }

    if (method === 'POST' && url.pathname === '/stimulate') {
      const body = (await readJsonBody(req)) as Partial<StimulateRequest> | undefined;
      if (!body || typeof body.by !== 'string' || !isActionSource(body.source)) {
        sendJson(res, 400, { error: 'expected { durationMs?, by, source }' } satisfies ErrorResponse);
        return;
      }
      const result = simulator.stimulate({ durationMs: body.durationMs, by: body.by, source: body.source });
      if (result.ok) {
        sendJson(res, 202, { eventId: result.eventId });
      } else {
        sendJson(res, 429, { retryAfterMs: result.retryAfterMs });
      }
      return;
    }

    if (method === 'POST' && url.pathname === '/reward') {
      const body = (await readJsonBody(req)) as Partial<RewardRequest> | undefined;
      if (!body || typeof body.value !== 'number' || typeof body.by !== 'string' || !isActionSource(body.source)) {
        sendJson(res, 400, { error: 'expected { value, by, source }' } satisfies ErrorResponse);
        return;
      }
      const result = simulator.reward({ value: body.value, by: body.by, source: body.source });
      if (!result.ok) {
        sendJson(res, 403, { error: 'reward is disabled (start the fake server with --allow-reward)' } satisfies ErrorResponse);
        return;
      }
      sendJson(res, 202, { eventId: result.eventId });
      return;
    }

    if (method === 'POST' && url.pathname === '/chat') {
      const body = (await readJsonBody(req)) as Partial<ChatRequest> | undefined;
      if (!body || typeof body.by !== 'string' || typeof body.text !== 'string') {
        sendJson(res, 400, { error: 'expected { by, text, bot? }' } satisfies ErrorResponse);
        return;
      }
      const result = simulator.chat({ by: body.by, text: body.text, bot: body.bot === true });
      if (result.ok) {
        sendJson(res, 202, { eventId: result.eventId });
        return;
      }
      switch (result.kind) {
        case 'disabled':
          sendJson(res, 403, { error: 'chat is disabled ([chat] enabled = false)' } satisfies ErrorResponse);
          return;
        case 'rate_limited':
          sendJson(res, 429, { retryAfterMs: result.retryAfterMs });
          return;
        case 'rejected':
          sendJson(res, 422, { error: `chat line refused: ${result.reason}` } satisfies ErrorResponse);
          return;
      }
    }

    if (method === 'POST' && url.pathname === '/checkpoint') {
      sendJson(res, 200, simulator.checkpoint());
      return;
    }

    if (method === 'POST' && url.pathname === '/pause') {
      sendJson(res, 200, simulator.pause());
      return;
    }

    if (method === 'POST' && url.pathname === '/resume') {
      sendJson(res, 200, simulator.resume());
      return;
    }

    sendJson(res, 404, { error: `no such route: ${method} ${url.pathname}` } satisfies ErrorResponse);
  } catch (cause) {
    sendJson(res, 400, { error: (cause as Error).message } satisfies ErrorResponse);
  }
}

function isActionSource(value: unknown): value is 'chat' | 'points' | 'operator' {
  return value === 'chat' || value === 'points' || value === 'operator';
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const bytes = Buffer.from(JSON.stringify(body));
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': bytes.byteLength });
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

// -- CLI ---------------------------------------------------------------------------------------

interface CliArgs {
  feedPort: number;
  controlPort: number;
  scenario: FakeScenario;
  allowReward: boolean;
  seed?: number;
  chatEnabled: boolean;
  chatChatter: boolean;
  spikesPerTick?: number;
  macroMode: MacroMode;
}

function parseArgs(argv: string[]): CliArgs {
  const args: CliArgs = {
    feedPort: FEED_PORT,
    controlPort: CONTROL_PORT,
    scenario: 'running',
    allowReward: false,
    chatEnabled: true,
    chatChatter: true,
    macroMode: 'raw',
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    switch (arg) {
      case '--feed-port':
        args.feedPort = Number.parseInt(argv[++i] ?? '', 10);
        break;
      case '--control-port':
        args.controlPort = Number.parseInt(argv[++i] ?? '', 10);
        break;
      case '--scenario': {
        const value = argv[++i];
        if (
          value !== 'boot' &&
          value !== 'running' &&
          value !== 'stuck' &&
          value !== 'milestone' &&
          value !== 'shop' &&
          value !== 'center'
        ) {
          throw new Error(
            `--scenario must be one of boot|running|stuck|milestone|shop|center, got ${String(value)}`,
          );
        }
        args.scenario = value;
        break;
      }
      case '--allow-reward':
        args.allowReward = true;
        break;
      case '--no-chat':
        // The kill switch, as `[chat] enabled = false` behaves in the real service.
        args.chatEnabled = false;
        break;
      case '--no-chatter':
        args.chatChatter = false;
        break;
      case '--seed':
        args.seed = Number.parseInt(argv[++i] ?? '', 10);
        break;
      case '--mode': {
        const value = argv[++i];
        if (value !== 'raw' && value !== 'macros') {
          throw new Error(`--mode must be raw|macros, got ${String(value)}`);
        }
        args.macroMode = value;
        break;
      }
      case '--spikes-per-tick':
        args.spikesPerTick = Number.parseInt(argv[++i] ?? '', 10);
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const handle = await startFakeServer(args);
  // eslint-disable-next-line no-console
  console.log(
    `fake flysim: feed ws://127.0.0.1:${handle.feedPort}/feed, control http://127.0.0.1:${handle.controlPort}, ` +
      `scenario=${args.scenario}, macros=${args.macroMode}, allowReward=${args.allowReward}, ` +
      `chat=${args.chatEnabled}`,
  );

  const shutdown = (): void => {
    void handle.close().then(() => process.exit(0));
  };
  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}

const isMainModule = (() => {
  const entry = process.argv[1];
  return entry !== undefined && import.meta.url === new URL(entry, 'file://').href;
})();

if (isMainModule) {
  main().catch((error: unknown) => {
    console.error(error);
    process.exit(1);
  });
}
