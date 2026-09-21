import assert from 'node:assert/strict';
import test from 'node:test';
import { WebSocket } from 'ws';
import { decodeSnapshot } from '../src/codec';
import type { FeedEvent, FeedHeader } from '../src/types';
import { startFakeServer, type FakeServerHandle } from '../src/fake/server';

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Connects, sends the client hello, and records every decoded snapshot header for the life of
 * the test (a listener attached only around individual awaits would miss snapshots published
 * while an HTTP call is in flight).
 */
class FeedRecorder {
  readonly headers: FeedHeader[] = [];
  private readonly socket: WebSocket;

  private constructor(socket: WebSocket) {
    this.socket = socket;
    socket.on('message', (data: Buffer) => {
      const { header } = decodeSnapshot(new Uint8Array(data));
      this.headers.push(header);
    });
  }

  static async connect(feedPort: number, wants: Array<'frame' | 'audio' | 'spikes'>): Promise<FeedRecorder> {
    const socket = new WebSocket(`ws://127.0.0.1:${feedPort}/feed`);
    await new Promise<void>((resolve, reject) => {
      socket.once('open', () => {
        socket.send(JSON.stringify({ protocol: 1, client: 'test', wants }));
        resolve();
      });
      socket.once('error', reject);
    });
    return new FeedRecorder(socket);
  }

  async waitForCount(count: number, timeoutMs = 5000): Promise<FeedHeader[]> {
    const start = Date.now();
    while (this.headers.length < count) {
      if (Date.now() - start > timeoutMs) {
        throw new Error(`timed out waiting for ${count} snapshots, got ${this.headers.length}`);
      }
      await sleep(15);
    }
    return this.headers.slice(0, count);
  }

  async waitForEvent(predicate: (event: FeedEvent) => boolean, timeoutMs = 5000): Promise<FeedEvent> {
    const start = Date.now();
    for (;;) {
      const found = this.headers.flatMap((h) => h.events).find(predicate);
      if (found) return found;
      if (Date.now() - start > timeoutMs) {
        throw new Error('timed out waiting for a matching FeedEvent');
      }
      await sleep(15);
    }
  }

  close(): void {
    this.socket.close();
  }
}

async function postJson(controlPort: number, path: string, body: unknown): Promise<{ status: number; json: unknown }> {
  const response = await fetch(`http://127.0.0.1:${controlPort}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const json = await response.json();
  return { status: response.status, json };
}

test('fake server: feed connection, 5 snapshots, stimulate visible in a later snapshot, 7th within a minute is rate limited', async () => {
  let handle: FakeServerHandle | undefined;
  let recorder: FeedRecorder | undefined;

  try {
    handle = await startFakeServer({ feedPort: 0, controlPort: 0, scenario: 'running', seed: 7 });
    recorder = await FeedRecorder.connect(handle.feedPort, ['frame', 'audio', 'spikes']);

    const firstFive = await recorder.waitForCount(5);
    assert.equal(firstFive.length, 5);
    for (const header of firstFive) {
      assert.equal(header.protocol, 1);
      assert.ok(header.seq > 0);
      assert.deepEqual([...header.attachments].sort(), ['audio', 'frame', 'spikes']);
    }
    for (let i = 1; i < firstFive.length; i++) {
      assert.ok((firstFive[i] as FeedHeader).seq > (firstFive[i - 1] as FeedHeader).seq);
    }

    // Accepted stimulate calls, spaced so each pulse (50ms) fully expires before the next call —
    // isolates the 6-per-minute global limit from the "no overlap with an active pulse" rule.
    for (let i = 0; i < 6; i++) {
      const result = await postJson(handle.controlPort, '/stimulate', { durationMs: 50, by: `viewer-${i}`, source: 'chat' });
      assert.equal(result.status, 202, `stimulate #${i + 1} expected 202, got ${result.status}: ${JSON.stringify(result.json)}`);
      assert.ok(typeof (result.json as { eventId: number }).eventId === 'number');
      await sleep(120);
    }

    // The 7th call within the same minute is rejected by the global rate limit.
    const seventh = await postJson(handle.controlPort, '/stimulate', { durationMs: 50, by: 'viewer-6', source: 'chat' });
    assert.equal(seventh.status, 429);
    assert.ok(typeof (seventh.json as { retryAfterMs: number }).retryAfterMs === 'number');

    // A sugar event from one of the accepted calls shows up in a subsequent snapshot.
    const sugarEvent = await recorder.waitForEvent((e) => e.kind === 'sugar');
    assert.equal(typeof sugarEvent.by, 'string');
    assert.ok((sugarEvent.by ?? '').startsWith('viewer-'));
  } finally {
    recorder?.close();
    await handle?.close();
  }
});

test('fake server: /reward is 403 unless started with allowReward', async () => {
  let handle: FakeServerHandle | undefined;
  try {
    handle = await startFakeServer({ feedPort: 0, controlPort: 0, scenario: 'running', seed: 1, allowReward: false });
    const result = await postJson(handle.controlPort, '/reward', { value: 0.1, by: 'operator', source: 'operator' });
    assert.equal(result.status, 403);
  } finally {
    await handle?.close();
  }
});

test('fake server: /reward succeeds when started with allowReward', async () => {
  let handle: FakeServerHandle | undefined;
  try {
    handle = await startFakeServer({ feedPort: 0, controlPort: 0, scenario: 'running', seed: 1, allowReward: true });
    const result = await postJson(handle.controlPort, '/reward', { value: 0.1, by: 'operator', source: 'operator' });
    assert.equal(result.status, 202);
  } finally {
    await handle?.close();
  }
});

test('fake server: GET /status, /healthz, /events and POST /checkpoint, /pause, /resume', async () => {
  let handle: FakeServerHandle | undefined;
  try {
    handle = await startFakeServer({ feedPort: 0, controlPort: 0, scenario: 'running', seed: 3 });

    const status = await fetch(`http://127.0.0.1:${handle.controlPort}/status`);
    assert.equal(status.status, 200);
    const statusBody = (await status.json()) as { protocol: number; version: { kernel: string } };
    assert.equal(statusBody.protocol, 1);
    assert.ok(statusBody.version.kernel.length > 0);

    const healthz = await fetch(`http://127.0.0.1:${handle.controlPort}/healthz`);
    assert.equal(healthz.status, 200);

    const checkpoint = await postJson(handle.controlPort, '/checkpoint', {});
    assert.equal(checkpoint.status, 200);
    assert.equal(typeof (checkpoint.json as { generation: number }).generation, 'number');

    const paused = await postJson(handle.controlPort, '/pause', {});
    assert.equal(paused.status, 200);
    assert.equal((paused.json as { status: string }).status, 'paused');

    const resumed = await postJson(handle.controlPort, '/resume', {});
    assert.equal(resumed.status, 200);

    const events = await fetch(`http://127.0.0.1:${handle.controlPort}/events?since=0&limit=10`);
    assert.equal(events.status, 200);
    const eventsBody = (await events.json()) as { events: unknown[] };
    assert.ok(Array.isArray(eventsBody.events));
  } finally {
    await handle?.close();
  }
});

test('fake server: honours the client hello wants list (no unwanted attachments sent)', async () => {
  let handle: FakeServerHandle | undefined;
  let recorder: FeedRecorder | undefined;
  try {
    handle = await startFakeServer({ feedPort: 0, controlPort: 0, scenario: 'running', seed: 9 });
    recorder = await FeedRecorder.connect(handle.feedPort, ['frame']);
    const [header] = await recorder.waitForCount(1);
    assert.deepEqual(header?.attachments, ['frame']);
  } finally {
    recorder?.close();
    await handle?.close();
  }
});
