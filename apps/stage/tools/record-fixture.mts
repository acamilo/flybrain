/**
 * Record a `.flyfeed` fixture from a running feed server.
 *
 * ```sh
 * # from a fake flysim this tool starts itself
 * npx tsx tools/record-fixture.mts --name steady --scenario running --seconds 120
npx tsx tools/record-fixture.mts --name macros --mode macros --offline --seed 777 --seconds 100
npx tsx tools/record-fixture.mts --name shop --mode macros --scenario shop --offline --seed 777 --seconds 40
npx tsx tools/record-fixture.mts --name center --mode macros --scenario center --offline --seed 777 --seconds 40
npx tsx tools/record-fixture.mts --name bigpad --mode macros --full-pad --offline --seconds 40
 *
 * # or from a real one that is already running
 * npx tsx tools/record-fixture.mts --name live --url ws://127.0.0.1:7400/feed --seconds 60
 * ```
 *
 * ## Why this tool rewrites headers
 *
 * A full-fidelity recording is not committable. Measured on this machine, one snapshot from the
 * fake flysim gzips to 9.7 KB (frame 0.6 KB, spikes 3.0 KB, audio 4.9 KB, header 0.3 KB), so
 * 120 s at 30 Hz is 35 MB — and most of that is audio and spike bits, which are close to
 * incompressible. So the recorder can keep a *stride* of each attachment kind, dropping the rest
 * from that snapshot's `header.attachments` exactly as if the client had never asked for them,
 * and the policy is written into the manifest so nobody has to guess later:
 *
 *   - `frame`: every snapshot. The game is the centrepiece and frames are cheap.
 *   - `spikes`: every third snapshot (10 Hz). The brain map's accumulator decays with a 110 ms
 *     time constant, so 10 Hz is visually indistinguishable from 30 Hz. Snapshots without the
 *     attachment carry `spikeCount: 0` per the contract, and the page holds the last real value.
 *   - `audio`: the first 12 s only. Enough to exercise the ring buffer, the drift servo and the
 *     underrun path; nothing visual depends on it.
 *
 * `--spikes-stride 1 --audio-seconds 0` records at full fidelity for a local (uncommitted) run.
 */
import { createWriteStream } from 'node:fs';
import { mkdir, stat } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { PassThrough } from 'node:stream';
import { fileURLToPath } from 'node:url';
import { createGzip } from 'node:zlib';

import {
  decodeSnapshot,
  encodeFlyfeedHeader,
  encodeFlyfeedRecord,
  encodeSnapshot,
  type AttachmentKind,
  type ClientHello,
  type FeedHeader,
  type FlyfeedManifest,
} from '@flybrain/feed';
import type { MacroMode } from '@flybrain/feed';
import { startFakeServer, type FakeScenario } from '@flybrain/feed/fake';
import { WebSocket } from 'ws';

const here = dirname(fileURLToPath(import.meta.url));
const defaultOutDir = resolve(here, '../public/fixtures');

interface Options {
  name: string;
  scenario: FakeScenario;
  /** `[macros] mode` for the fake it starts: `raw` (the default) or `macros`. */
  macroMode: MacroMode;
  /**
   * Deal the fake's widest pad in every scene instead of the scene's own.
   *
   * For the fixtures the *screen* needs: `docs/design/macros.md` section 14 sizes the strip under
   * the game for fourteen macro cells in two columns and the MACROS tab for every type there is,
   * and no scene the fake walks deals more than six (`packages/feed/src/fake/palette.ts`).
   */
  fullPad: boolean;
  seconds: number;
  /** Let the simulation run this long before recording starts (to reach a scripted event). */
  skipSeconds: number;
  /** Connect to an already-running feed instead of starting a fake one. */
  url: string | null;
  /**
   * Generate from the fake simulator directly instead of recording over a socket: exactly 30 Hz,
   * `wallMs` on the same grid, reproducible from the seed. See {@link generateOffline}.
   */
  offline: boolean;
  /** Control API base for `--stimulate-at`, when connecting to an existing server. */
  controlUrl: string | null;
  /** Offsets into the recording, in seconds, at which to POST /stimulate. */
  stimulateAt: number[];
  frameStride: number;
  spikesStride: number;
  /** Keep audio for the first N seconds of the recording; 0 keeps none, Infinity keeps all. */
  audioSeconds: number;
  seed: number;
  outDir: string;
  gzip: boolean;
}

function parseArgs(argv: string[]): Options {
  const options: Options = {
    name: 'steady',
    scenario: 'running',
    macroMode: 'raw',
    fullPad: false,
    seconds: 120,
    skipSeconds: 0,
    url: null,
    offline: false,
    controlUrl: null,
    stimulateAt: [],
    frameStride: 1,
    spikesStride: 3,
    audioSeconds: 12,
    seed: 20260915,
    outDir: defaultOutDir,
    gzip: true,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = () => {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      return value;
    };
    switch (arg) {
      case '--name': options.name = next(); break;
      case '--scenario': {
        const value = next();
        // `shop` and `center` pin the palette's scene walk (`docs/design/macros.md` section 13):
        // the mart and the Pokémon Center are two screens the random walk reaches rarely and
        // never for long, and a review still needs a still of each at a fixed seek time.
        if (
          value !== 'boot' &&
          value !== 'running' &&
          value !== 'stuck' &&
          value !== 'milestone' &&
          value !== 'shop' &&
          value !== 'center'
        ) {
          throw new Error(`--scenario must be boot|running|stuck|milestone|shop|center, got ${value}`);
        }
        options.scenario = value;
        break;
      }
      case '--mode': {
        const value = next();
        if (value !== 'raw' && value !== 'macros') throw new Error(`--mode must be raw|macros, got ${value}`);
        options.macroMode = value;
        break;
      }
      case '--seconds': options.seconds = Number.parseFloat(next()); break;
      case '--skip': options.skipSeconds = Number.parseFloat(next()); break;
      case '--url': options.url = next(); break;
      case '--offline': options.offline = true; break;
      case '--full-pad': options.fullPad = true; break;
      case '--control-url': options.controlUrl = next(); break;
      case '--stimulate-at':
        options.stimulateAt = next().split(',').map((part) => Number.parseFloat(part.trim()));
        break;
      case '--frame-stride': options.frameStride = Number.parseInt(next(), 10); break;
      case '--spikes-stride': options.spikesStride = Number.parseInt(next(), 10); break;
      case '--audio-seconds': options.audioSeconds = Number.parseFloat(next()); break;
      case '--seed': options.seed = Number.parseInt(next(), 10); break;
      case '--out-dir': options.outDir = resolve(next()); break;
      case '--no-gzip': options.gzip = false; break;
      default: throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

/** Keep or drop each attachment for one snapshot, and fix the header up to match. */
function applyPolicy(
  message: Uint8Array,
  index: number,
  elapsedMs: number,
  options: Options,
): Uint8Array {
  const { header, attachments } = decodeSnapshot(message);

  const keep = (kind: AttachmentKind): boolean => {
    if (!attachments.has(kind)) return false;
    if (kind === 'frame') return options.frameStride > 0 && index % options.frameStride === 0;
    if (kind === 'spikes') return options.spikesStride > 0 && index % options.spikesStride === 0;
    return elapsedMs <= options.audioSeconds * 1000;
  };

  const kinds = header.attachments.filter(keep);
  if (kinds.length === header.attachments.length) return message;

  const kept: Partial<Record<AttachmentKind, Uint8Array>> = {};
  for (const kind of kinds) kept[kind] = attachments.get(kind);

  const rewritten: FeedHeader = {
    ...header,
    attachments: kinds,
    // The contract: `spikeCount` is the set bits in the spikes attachment, 0 when it is omitted.
    spikeCount: kinds.includes('spikes') ? header.spikeCount : 0,
  };
  return encodeSnapshot(rewritten, kept);
}

/** What a recording produced, however it was produced. */
interface Tally {
  snapshots: number;
  firstWallMs: number;
  lastWallMs: number;
  rawBytes: number;
  writtenBytes: number;
}

/**
 * Generate a fixture by driving the fake simulator directly, with no socket and no wall clock.
 *
 * Why this exists: a recording taken over the socket is *not* reproducible from its seed. The fake
 * consumes randomness once per tick, the tick interval over a real socket wanders by a millisecond
 * or two, and the fake's scene and macro timers are in *milliseconds* — so the tick on which a
 * scene changes moves, the RNG draw that picks the next scene lands somewhere else, and two runs of
 * `--seed 20260921` produce different runs. That is fine for a fixture nobody has to reason about
 * and useless for one whose seek times the screenshot tests name: the macros fixture has to hold
 * an overworld, a battle, a running macro and an outcome at known seconds.
 *
 * So: exactly 30 ticks a second of exactly 1000/30 ms, and `wallMs` rewritten onto that same grid
 * (the player derives its schedule from the headers, `packages/feed/src/fixture.ts`). Same seed,
 * same file, on any machine. `--offline` is therefore the default for a committed fixture and the
 * socket path stays for recording a *real* flysim.
 */
async function generateOffline(options: Options, body: PassThrough): Promise<Tally> {
  const { FakeFlysim } = await import('@flybrain/feed/fake');
  const sim = new FakeFlysim({
    scenario: options.scenario,
    seed: options.seed,
    macroMode: options.macroMode,
    fullPad: options.fullPad,
  });

  const dtMs = 1000 / 30;
  const base = Date.now();
  const tally: Tally = { snapshots: 0, firstWallMs: base, lastWallMs: base, rawBytes: 0, writtenBytes: 0 };

  for (let i = 0; i < Math.round(options.skipSeconds * 30); i += 1) sim.tick(dtMs);

  const stimulateTicks = new Set(options.stimulateAt.map((offset) => Math.round(offset * 30)));
  const total = Math.round(options.seconds * 30);

  for (let i = 0; i < total; i += 1) {
    const wallMs = Math.round(base + i * dtMs);
    if (stimulateTicks.has(i)) {
      const result = sim.stimulate({ by: `fixture_${options.name}`, source: 'chat' }, wallMs);
      console.log(`  t=${(i / 30).toFixed(1)}s stimulate -> ${result.ok ? 'accepted' : 'rate limited'}`);
    }

    const snapshot = sim.tick(dtMs);
    // Every timestamp in the snapshot goes onto the same grid, so nothing inside a header
    // disagrees with the header's own clock.
    const header: FeedHeader = {
      ...snapshot.header,
      wallMs,
      events: snapshot.header.events.map((event) => ({ ...event, wallMs })),
      ...(snapshot.header.chat === undefined
        ? {}
        : { chat: snapshot.header.chat.map((line) => ({ ...line, wallMs: Math.min(line.wallMs, wallMs) })) }),
    };

    const message = encodeSnapshot(header, snapshot.attachments);
    tally.rawBytes += message.byteLength;
    const filtered = applyPolicy(message, i, i * dtMs, options);
    tally.writtenBytes += filtered.byteLength;
    if (i === 0) tally.firstWallMs = wallMs;
    tally.lastWallMs = wallMs;
    tally.snapshots += 1;
    body.write(encodeFlyfeedRecord(filtered));
  }

  return tally;
}

async function main(): Promise<void> {
  const options = parseArgs(process.argv.slice(2));

  let feedUrl = options.url;
  let controlUrl = options.controlUrl;
  let closeFake: (() => Promise<void>) | null = null;

  if (options.offline && options.url) throw new Error('--offline and --url are mutually exclusive');

  if (!feedUrl && !options.offline) {
    const fake = await startFakeServer({
      feedPort: 0,
      controlPort: 0,
      scenario: options.scenario,
      seed: options.seed,
      macroMode: options.macroMode,
      fullPad: options.fullPad,
    });
    feedUrl = `ws://127.0.0.1:${fake.feedPort}/feed`;
    controlUrl = `http://127.0.0.1:${fake.controlPort}`;
    closeFake = fake.close;
    console.log(
      `started a fake flysim: ${feedUrl} (scenario ${options.scenario}, macros ${options.macroMode}, ` +
        `seed ${options.seed})`,
    );
  }

  if (options.skipSeconds > 0 && !options.offline) {
    console.log(`letting the simulation run for ${options.skipSeconds}s before recording`);
    await sleep(options.skipSeconds * 1000);
  }

  await mkdir(options.outDir, { recursive: true });
  const extension = options.gzip ? '.flyfeed.gz' : '.flyfeed';
  const outPath = resolve(options.outDir, `${options.name}${extension}`);

  const body = new PassThrough();
  const sink = options.gzip
    ? pipeline(body, createGzip({ level: 9 }), createWriteStream(outPath))
    : pipeline(body, createWriteStream(outPath));

  const manifest: FlyfeedManifest = {
    name: options.name,
    protocol: 1,
    recordedAt: new Date().toISOString(),
    source: options.url
      ? `feed ${options.url}`
      : `fake-flysim${options.offline ? ' offline' : ''} scenario=${options.scenario} ` +
        `macros=${options.macroMode} seed=${options.seed} skip=${options.skipSeconds}s`,
    snapshotCount: 0,
    durationMs: 0,
    hz: 30,
    attachmentPolicy: {
      frame: { stride: options.frameStride },
      spikes: { stride: options.spikesStride },
      audio: { stride: 1, seconds: options.audioSeconds },
    },
    notes: [],
  };

  // The manifest is written first and its counts are not yet known, so they are patched into the
  // notes at the end. Rewriting the head would mean buffering the whole (hundreds of MB) file.
  const placeholder: FlyfeedManifest = { ...manifest, notes: ['counts in the trailing note record'] };
  body.write(encodeFlyfeedHeader(placeholder));

  let snapshots = 0;
  let firstWallMs = 0;
  let lastWallMs = 0;
  let rawBytes = 0;
  let writtenBytes = 0;

  if (options.offline) {
    const tally = await generateOffline(options, body);
    snapshots = tally.snapshots;
    firstWallMs = tally.firstWallMs;
    lastWallMs = tally.lastWallMs;
    rawBytes = tally.rawBytes;
    writtenBytes = tally.writtenBytes;
  } else {

  if (feedUrl === null) throw new Error('no feed URL to record from');

  const wants: AttachmentKind[] = ['frame', 'audio', 'spikes'];
  const socket = new WebSocket(feedUrl);
  socket.binaryType = 'arraybuffer';

  let startedAt = 0;

  const stimulations = [...options.stimulateAt].sort((a, b) => a - b);
  const timers: ReturnType<typeof setTimeout>[] = [];

  await new Promise<void>((resolveRecording, rejectRecording) => {
    socket.on('open', () => {
      const hello: ClientHello = { protocol: 1, client: 'test', wants };
      socket.send(JSON.stringify(hello));
      startedAt = Date.now();

      for (const offset of stimulations) {
        timers.push(
          setTimeout(() => {
            void stimulate(controlUrl, `fixture_${options.name}`).then((ok) => {
              console.log(`  t=${offset.toFixed(1)}s POST /stimulate -> ${ok}`);
            });
          }, offset * 1000),
        );
      }

      timers.push(
        setTimeout(() => {
          socket.close();
        }, options.seconds * 1000),
      );
    });

    socket.on('message', (data: ArrayBuffer) => {
      const message = new Uint8Array(data);
      rawBytes += message.byteLength;
      const elapsedMs = Date.now() - startedAt;
      const filtered = applyPolicy(message, snapshots, elapsedMs, options);
      writtenBytes += filtered.byteLength;

      const { header } = decodeSnapshot(filtered);
      if (snapshots === 0) firstWallMs = header.wallMs;
      lastWallMs = header.wallMs;
      snapshots += 1;

      body.write(encodeFlyfeedRecord(filtered));
    });

    socket.on('error', rejectRecording);
    socket.on('close', () => resolveRecording());
  });

  for (const timer of timers) clearTimeout(timer);
  }

  body.end();
  await sink;
  await closeFake?.();

  const durationMs = lastWallMs - firstWallMs;
  const size = (await stat(outPath)).size;

  console.log(`
wrote ${outPath}
  snapshots      ${snapshots}
  duration       ${(durationMs / 1000).toFixed(1)}s
  wire bytes     ${(rawBytes / 1e6).toFixed(1)} MB at full fidelity
  after policy   ${(writtenBytes / 1e6).toFixed(1)} MB
  on disk        ${(size / 1e6).toFixed(2)} MB ${options.gzip ? '(gzip -9)' : '(uncompressed)'}
  policy         frame stride ${options.frameStride}, spikes stride ${options.spikesStride}, audio first ${options.audioSeconds}s
`);

  if (snapshots === 0) throw new Error('recorded no snapshots');

  // The reader checks `snapshotCount` against the records, so a file written with a placeholder
  // count has to be rewritten with the real one. Small files only; a full-fidelity recording is
  // streamed and left with the placeholder note instead.
  await rewriteManifest(outPath, { ...manifest, snapshotCount: snapshots, durationMs }, options.gzip);
}

async function rewriteManifest(path: string, manifest: FlyfeedManifest, gzip: boolean): Promise<void> {
  const { readFile, writeFile } = await import('node:fs/promises');
  const { gunzipSync, gzipSync } = await import('node:zlib');
  const { readFlyfeedManifest } = await import('@flybrain/feed');

  const onDisk = await readFile(path);
  const plain = gzip ? gunzipSync(onDisk) : onDisk;
  const { bodyOffset } = readFlyfeedManifest(new Uint8Array(plain));

  const head = encodeFlyfeedHeader(manifest);
  const rebuilt = Buffer.concat([Buffer.from(head), plain.subarray(bodyOffset)]);
  await writeFile(path, gzip ? gzipSync(rebuilt, { level: 9 }) : rebuilt);
}

async function stimulate(controlUrl: string | null, by: string): Promise<string> {
  if (!controlUrl) return 'skipped (no control URL)';
  try {
    const response = await fetch(`${controlUrl}/stimulate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ by, source: 'chat' }),
    });
    return `${response.status}`;
  } catch (error) {
    return `failed: ${(error as Error).message}`;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms));
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
