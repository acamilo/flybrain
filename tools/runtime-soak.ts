/**
 * runtime-soak: drive a running fly service (flysim or flysim-session) the way the stage and the
 * bridge do, for a wall-clock soak, and summarise what they saw (SERVE-01).
 *
 *   npx tsx tools/runtime-soak.ts --minutes 35 --out soak.json \
 *     [--feed ws://127.0.0.1:7400/feed] [--control http://127.0.0.1:7401] \
 *     [--metrics http://127.0.0.1:9101] [--state DIR --hot DIR]
 *
 * The clients are the real ones, not re-implementations:
 *
 * - **feed**: two WebSocket clients sending the stage's `hello` (one wanting every attachment,
 *   one header-only), each message decoded by the stage's own `decodeFeedMessage` (shape checks
 *   included) and every header validated against `packages/feed/src/schema.json`. Counted:
 *   messages, decode and schema failures, `seq` gaps, stalls longer than the stage's 2 s STALE
 *   banner, statuses seen, the frame advancing.
 * - **control**: the bridge's own `HttpSimClient` (`services/bridge/src/sim.ts`): `/healthz`
 *   every 2 s, `/status` every 5 s, `/events` paged every 3 s with the bridge's cursor, sugar
 *   (`/stimulate`) every 20 s alternating `chat`/`points`, an on-screen chat line every 15 s; plus
 *   `/reward` (403 expected), a forced `/checkpoint` every 10 minutes and one pause/resume.
 * - **store**: every 30 s, the hot and durable directories' checkpoint files and the metrics
 *   listener's checkpoint series, so the hot/durable cadence is observed from outside.
 *
 * Exit 0 when nothing failed; the JSON summary says what was seen either way.
 */
import { readdirSync, statSync, writeFileSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import Ajv2020 from 'ajv/dist/2020.js';
import { decodeFeedMessage } from '../apps/stage/src/feed/decode';
import { HttpSimClient } from '../services/bridge/src/sim';

function arg(name: string, fallback: string): string {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 && process.argv[index + 1] ? process.argv[index + 1] : fallback;
}

const minutes = Number(arg('minutes', '30'));
const feedUrl = arg('feed', 'ws://127.0.0.1:7400/feed');
const controlUrl = arg('control', 'http://127.0.0.1:7401');
const metricsUrl = arg('metrics', 'http://127.0.0.1:9101');
const stateDir = arg('state', '');
const hotDir = arg('hot', '');
const out = arg('out', 'soak.json');
const endAt = Date.now() + minutes * 60_000;

const schema = JSON.parse(
  readFileSync(new URL('../packages/feed/src/schema.json', import.meta.url), 'utf8'),
);
const ajv = new Ajv2020({ strict: false });
const validate = ajv.compile(schema);

interface FeedStats {
  wants: string[];
  connects: number;
  messages: number;
  decodeErrors: string[];
  schemaErrors: string[];
  seqGaps: number;
  stalls: number;
  longestSilenceMs: number;
  statuses: Record<string, number>;
  firstFrame: number | null;
  lastFrame: number | null;
  frameBackwards: number;
  /** Each reconnect after the service went away: the last frame before, the first after. */
  restarts: { lastFrame: number | null; firstFrame: number | null; gapMs: number }[];
  withFrame: number;
  audioSamples: number;
  lastHeader: unknown;
}

function feedClient(wants: string[]): FeedStats {
  const stats: FeedStats = {
    wants,
    connects: 0,
    messages: 0,
    decodeErrors: [],
    schemaErrors: [],
    seqGaps: 0,
    stalls: 0,
    longestSilenceMs: 0,
    statuses: {},
    firstFrame: null,
    lastFrame: null,
    frameBackwards: 0,
    restarts: [],
    withFrame: 0,
    audioSamples: 0,
    lastHeader: null,
  };
  let lastSeq: number | null = null;
  let lastAt = Date.now();
  const open = () => {
    if (Date.now() >= endAt) return;
    const socket = new WebSocket(feedUrl);
    socket.binaryType = 'arraybuffer';
    socket.onopen = () => {
      stats.connects += 1;
      if (stats.connects > 1) {
        stats.restarts.push({ lastFrame: stats.lastFrame, firstFrame: null, gapMs: Date.now() - lastAt });
        // A new process: its frame restarts from its checkpoint, so "backwards" is per process.
        stats.lastFrame = null;
      }
      lastSeq = null;
      socket.send(JSON.stringify({ protocol: 1, client: 'stage', wants }));
    };
    socket.onmessage = (message) => {
      const now = Date.now();
      const silence = now - lastAt;
      lastAt = now;
      stats.longestSilenceMs = Math.max(stats.longestSilenceMs, silence);
      if (silence > 2000 && stats.messages > 0) stats.stalls += 1;
      stats.messages += 1;
      try {
        const decoded = decodeFeedMessage(new Uint8Array(message.data as ArrayBuffer));
        const header = decoded.header;
        if (!validate(header) && stats.schemaErrors.length < 20) {
          stats.schemaErrors.push(ajv.errorsText(validate.errors));
        }
        if (lastSeq !== null && header.seq !== lastSeq + 1) stats.seqGaps += 1;
        lastSeq = header.seq;
        stats.statuses[header.status] = (stats.statuses[header.status] ?? 0) + 1;
        if (header.status === 'running' || header.status === 'recovering') {
          if (stats.firstFrame === null) stats.firstFrame = header.frame;
          const restart = stats.restarts[stats.restarts.length - 1];
          if (restart && restart.firstFrame === null) restart.firstFrame = header.frame;
          if (stats.lastFrame !== null && header.frame < stats.lastFrame) stats.frameBackwards += 1;
          stats.lastFrame = header.frame;
        }
        if (decoded.frame) stats.withFrame += 1;
        if (decoded.audio) stats.audioSamples += decoded.audio.length;
        stats.lastHeader = header;
      } catch (error) {
        if (stats.decodeErrors.length < 20) stats.decodeErrors.push(String(error));
      }
    };
    socket.onclose = () => setTimeout(open, 500);
    socket.onerror = () => {};
    const closer = setInterval(() => {
      if (Date.now() >= endAt) {
        clearInterval(closer);
        socket.onclose = null;
        socket.close();
      }
    }, 1000);
  };
  open();
  return stats;
}

interface Tally {
  count: number;
  outcomes: Record<string, number>;
  samples: unknown[];
}
function tally(): Tally {
  return { count: 0, outcomes: {}, samples: [] };
}
function note(t: Tally, outcome: string, sample?: unknown) {
  t.count += 1;
  t.outcomes[outcome] = (t.outcomes[outcome] ?? 0) + 1;
  if (sample !== undefined && t.samples.length < 5) t.samples.push(sample);
}
function outcomeOf(result: { ok: boolean; kind?: string; status?: number }): string {
  if (result.ok) return 'ok';
  return result.kind === 'http_error' ? `http_${result.status}` : String(result.kind);
}

const sim = new HttpSimClient({ baseUrl: controlUrl, timeoutMs: 3000 });
const control = {
  healthz: tally(),
  status: tally(),
  events: tally(),
  stimulate: tally(),
  chat: tally(),
  reward: tally(),
  checkpoint: tally(),
  pause: tally(),
  resume: tally(),
};
let eventCursor = 0;
const eventKinds: Record<string, number> = {};
let eventIdGaps = 0;
let lastEventId = 0;

async function post(path: string, body?: unknown): Promise<{ status: number; json: unknown }> {
  const response = await fetch(`${controlUrl}${path}`, {
    method: 'POST',
    headers: body === undefined ? {} : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  });
  return { status: response.status, json: await response.json().catch(() => null) };
}

interface StoreSample {
  atMs: number;
  hot: { name: string; mtimeMs: number; size: number }[];
  durable: { name: string; mtimeMs: number; size: number }[];
  metrics: Record<string, number>;
}
const store: StoreSample[] = [];

function listing(dir: string) {
  if (!dir) return [];
  try {
    return readdirSync(dir)
      .filter((name) => name.endsWith('.checkpoint') || name === 'manifest.json' || name.startsWith('sugar-journal'))
      .map((name) => {
        const st = statSync(join(dir, name));
        return { name, mtimeMs: Math.round(st.mtimeMs), size: st.size };
      })
      .sort((a, b) => a.name.localeCompare(b.name));
  } catch {
    return [];
  }
}

async function metrics(): Promise<Record<string, number>> {
  try {
    const text = await (await fetch(`${metricsUrl}/metrics`, { signal: AbortSignal.timeout(3000) })).text();
    const out: Record<string, number> = {};
    for (const line of text.split('\n')) {
      if (!line || line.startsWith('#')) continue;
      const [name, value] = line.split(' ');
      if (name && !name.includes('{')) out[name] = Number(value);
    }
    return out;
  } catch {
    return {};
  }
}

function every(ms: number, task: () => Promise<void>) {
  const timer = setInterval(() => {
    if (Date.now() >= endAt) {
      clearInterval(timer);
      return;
    }
    task().catch(() => {});
  }, ms);
}

const full = feedClient(['frame', 'audio', 'spikes']);
const bare = feedClient([]);

every(2000, async () => note(control.healthz, outcomeOf(await sim.healthz())));
every(5000, async () => {
  const result = await sim.status();
  note(control.status, outcomeOf(result));
});
every(3000, async () => {
  const result = await sim.events(eventCursor, 100);
  note(control.events, outcomeOf(result));
  if (result.ok) {
    for (const event of result.data.events) {
      if (lastEventId && event.id !== lastEventId + 1) eventIdGaps += 1;
      lastEventId = event.id;
      eventKinds[event.kind] = (eventKinds[event.kind] ?? 0) + 1;
      eventCursor = Math.max(eventCursor, event.id);
    }
  }
});
let sugarTurn = 0;
every(20_000, async () => {
  sugarTurn += 1;
  const source = sugarTurn % 2 ? 'chat' : 'points';
  const result = await sim.stimulate({ by: `soak_viewer_${sugarTurn % 7}`, source });
  note(control.stimulate, outcomeOf(result), result.ok ? undefined : result);
});
let chatTurn = 0;
every(15_000, async () => {
  chatTurn += 1;
  const result = await sim.chat({ by: `soak_viewer_${chatTurn % 5}`, text: `soak line ${chatTurn}` });
  note(control.chat, outcomeOf(result));
});
every(10 * 60_000, async () => {
  const { status, json } = await post('/checkpoint');
  note(control.checkpoint, `http_${status}`, json);
});
every(30_000, async () => {
  store.push({ atMs: Date.now(), hot: listing(hotDir), durable: listing(stateDir), metrics: await metrics() });
});
setTimeout(async () => {
  const { status } = await post('/reward', { value: 1, by: 'soak', source: 'operator' });
  note(control.reward, `http_${status}`);
  const paused = await post('/pause');
  note(control.pause, `http_${paused.status}`, paused.json);
  await new Promise((resolve) => setTimeout(resolve, 3000));
  const resumed = await post('/resume');
  note(control.resume, `http_${resumed.status}`, resumed.json);
}, 60_000);

setTimeout(async () => {
  const finalStatus = await sim.status();
  const summary = {
    feedUrl,
    controlUrl,
    minutes,
    startedAt: new Date(endAt - minutes * 60_000).toISOString(),
    feed: { full: { ...full, lastHeader: undefined }, headerOnly: { ...bare, lastHeader: undefined } },
    control,
    events: { kinds: eventKinds, lastId: lastEventId, idGaps: eventIdGaps },
    finalStatus: finalStatus.ok ? finalStatus.data : finalStatus,
    store,
  };
  writeFileSync(out, JSON.stringify(summary, null, 2));
  const failures = [
    full.decodeErrors.length + bare.decodeErrors.length > 0 && 'feed decode errors',
    full.schemaErrors.length + bare.schemaErrors.length > 0 && 'header schema errors',
    full.frameBackwards > 0 && 'the frame went backwards',
    (control.healthz.outcomes.ok ?? 0) < control.healthz.count * 0.97 && 'healthz below 97%',
    control.reward.outcomes.http_403 !== 1 && '/reward was not 403',
    eventIdGaps > 0 && 'event id gaps',
  ].filter(Boolean);
  console.log(JSON.stringify({ failures, messages: full.messages, frames: [full.firstFrame, full.lastFrame] }));
  process.exit(failures.length ? 1 : 0);
}, minutes * 60_000 + 5000);
