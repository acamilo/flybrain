/**
 * The live client, driven through an injected WebSocket.
 *
 * `?mode=live` is not reachable until a theme is chosen, but the socket is the part that has to
 * survive a 24/7 broadcast: it outlives every restart of the service underneath it. The failure
 * modes worth pinning are the ones that take hours to show up — a hello with the wrong `wants`,
 * a reconnect storm, and a decode failure that kills the handler instead of being counted.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { encodeSnapshot, type ClientHello, type FeedHeader } from '@flybrain/feed';
import { FeedSocket } from '../../src/feed/socket';
import { FeedIngest, hot, useStage } from '../../src/feed/store';
import { pokemonRed } from '../../src/games/pokemon-red';

/** The parts of WebSocket this client touches. */
class FakeSocket {
  static instances: FakeSocket[] = [];

  binaryType = 'blob';
  readyState = 0;
  sent: string[] = [];
  closed = false;

  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  /** Test helpers. */
  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }

  deliver(bytes: Uint8Array): void {
    const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    this.onmessage?.({ data: buffer } as MessageEvent<unknown>);
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.();
  }
}

function header(seq: number): FeedHeader {
  return {
    protocol: 1,
    seq,
    wallMs: 1_700_000_000_000 + seq,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: seq,
    runSeconds: seq,
    brainMs: seq * 1000,
    frame: seq,
    buttons: 0,
    rates: {},
    populationRate: 12,
    spikeCount: 0,
    learning: { enabled: true, updates: 0, changed: 0, synapses: 0, signal: 0 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 0,
      rewardTotal: 0,
      rewardCounts: { story: 0, explore: 0, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
    },
    milestone: { rank: 0, label: 'booting', next: 'next', sinceSeconds: 0, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
    events: [],
    attachments: [],
  };
}

function setup() {
  FakeSocket.instances = [];
  hot.accepted = 0;
  hot.decodeErrors = 0;
  const ingest = new FeedIngest(pokemonRed);
  ingest.reset();
  const socket = new FeedSocket('ws://127.0.0.1:7400/feed', ingest, {
    factory: (url) => new FakeSocket(url) as unknown as WebSocket,
  });
  return { ingest, socket };
}

test('the client sends exactly one message, ever: the hello', async () => {
  const { socket } = setup();
  await socket.start();

  const fake = FakeSocket.instances[0] as FakeSocket;
  assert.equal(fake.url, 'ws://127.0.0.1:7400/feed');
  assert.equal(fake.binaryType, 'arraybuffer', 'set before the socket opens');

  fake.open();
  assert.equal(fake.sent.length, 1);
  const hello = JSON.parse(fake.sent[0] as string) as ClientHello;
  assert.deepEqual(hello, { protocol: 1, client: 'stage', wants: ['frame', 'audio', 'spikes'] });

  fake.deliver(encodeSnapshot(header(1), {}));
  fake.deliver(encodeSnapshot(header(2), {}));
  assert.equal(fake.sent.length, 1, 'the page is a display: it never talks back');

  socket.stop();
});

test('snapshots reach the store, and the connection state is reported', async () => {
  const { socket } = setup();
  await socket.start();
  assert.equal(useStage.getState().connection, 'connecting');

  const fake = FakeSocket.instances[0] as FakeSocket;
  fake.open();
  assert.equal(useStage.getState().connection, 'open');

  fake.deliver(encodeSnapshot(header(7), {}));
  assert.equal(hot.header?.seq, 7);
  assert.equal(hot.accepted, 1);

  socket.stop();
});

test('a message that is not a snapshot is counted, not thrown', async () => {
  const { socket } = setup();
  await socket.start();
  const fake = FakeSocket.instances[0] as FakeSocket;
  fake.open();

  fake.deliver(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]));
  assert.equal(hot.decodeErrors, 1);
  assert.equal(hot.accepted, 0);

  // And the handler still works afterwards.
  fake.deliver(encodeSnapshot(header(9), {}));
  assert.equal(hot.header?.seq, 9);

  socket.stop();
});

test('a text message is ignored: the feed is binary', async () => {
  const { socket } = setup();
  await socket.start();
  const fake = FakeSocket.instances[0] as FakeSocket;
  fake.open();

  fake.onmessage?.({ data: 'hello?' } as MessageEvent<unknown>);
  assert.equal(hot.decodeErrors, 0);
  assert.equal(hot.accepted, 0);

  socket.stop();
});

test('a dropped socket reconnects, and reports closed while it is down', async () => {
  const { socket } = setup();
  await socket.start();
  const first = FakeSocket.instances[0] as FakeSocket;
  first.open();
  first.drop();

  assert.equal(useStage.getState().connection, 'closed');
  await new Promise((done) => setTimeout(done, 700));
  assert.ok(FakeSocket.instances.length >= 2, 'a second socket was opened');

  socket.stop();
});

test('backoff grows to a cap, with jitter, so restarts never synchronise', () => {
  const { socket } = setup();

  const at = (attempt: number) => socket.backoffFor(attempt);
  // 250 ms base doubling to a 5 s cap, each sample within +/-25 percent of the nominal value.
  for (const [attempt, nominal] of [[0, 250], [1, 500], [2, 1000], [3, 2000], [4, 4000], [5, 5000], [40, 5000]] as const) {
    for (let sample = 0; sample < 50; sample++) {
      const delay = at(attempt);
      assert.ok(delay >= nominal * 0.74, `attempt ${attempt}: ${delay} below the jitter floor`);
      assert.ok(delay <= nominal * 1.26, `attempt ${attempt}: ${delay} above the jitter ceiling`);
    }
  }

  const samples = new Set(Array.from({ length: 30 }, () => at(3)));
  assert.ok(samples.size > 5, 'the jitter is real, not a constant');

  socket.stop();
});

test('stop() is final: no reconnect, and calling it twice is safe', async () => {
  const { socket } = setup();
  await socket.start();
  const fake = FakeSocket.instances[0] as FakeSocket;
  fake.open();

  socket.stop();
  socket.stop();
  assert.equal(fake.closed, true);

  fake.drop();
  await new Promise((done) => setTimeout(done, 500));
  assert.equal(FakeSocket.instances.length, 1, 'nothing reconnected after stop');
});

test('a factory that throws schedules a retry instead of failing start()', async () => {
  const ingest = new FeedIngest(pokemonRed);
  let attempts = 0;
  const socket = new FeedSocket('ws://127.0.0.1:7400/feed', ingest, {
    factory: () => {
      attempts += 1;
      throw new Error('ECONNREFUSED');
    },
    backoffMs: 20,
  });

  await socket.start();
  assert.equal(attempts, 1);
  await new Promise((done) => setTimeout(done, 120));
  assert.ok(attempts > 1, 'it kept trying');
  socket.stop();
});
