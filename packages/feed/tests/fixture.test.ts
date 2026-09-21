import assert from 'node:assert/strict';
import { gzipSync } from 'node:zlib';
import test from 'node:test';
import { decodeSnapshot, encodeSnapshot } from '../src/codec';
import {
  FLYFEED_MAGIC,
  FLYFEED_VERSION,
  FlyfeedError,
  decodeFlyfeed,
  encodeFlyfeed,
  encodeFlyfeedHeader,
  encodeFlyfeedRecord,
  isGzip,
  iterateFlyfeedRecords,
  readFlyfeedManifest,
  type FlyfeedManifest,
} from '../src/fixture';
import type { AttachmentKind, FeedHeader } from '../src/types';

function sampleHeader(seq: number, attachments: AttachmentKind[]): FeedHeader {
  return {
    protocol: 1,
    seq,
    wallMs: 1_700_000_000_000 + seq * 33,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: seq / 30,
    runSeconds: seq / 30,
    brainMs: Math.round((seq / 30) * 1000),
    frame: seq,
    buttons: seq % 2 === 0 ? 0b0001_0000 : 0,
    rates: { command_0: 12.5, reward_pam: 2.1 },
    populationRate: 13.2,
    spikeCount: attachments.includes('spikes') ? 3 : 0,
    learning: { enabled: true, updates: seq, changed: 1, synapses: 4_200_000, signal: 0.12 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 5,
      rewardTotal: 0.42,
      rewardCounts: { story: 0, explore: 3, area: 1, pokedex: 0, trainer: 0, wildwin: 1, badge: 0 },
    },
    milestone: { rank: 3, label: 'Reached Route 1', next: 'Reached Viridian City', sinceSeconds: 90, attempts: 1 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 2 },
    events: [],
    attachments,
  };
}

function sampleManifest(overrides: Partial<FlyfeedManifest> = {}): FlyfeedManifest {
  return {
    name: 'unit',
    protocol: 1,
    recordedAt: '2026-09-15T00:00:00.000Z',
    source: 'unit test',
    snapshotCount: 0,
    durationMs: 0,
    hz: 30,
    attachmentPolicy: { frame: { stride: 1 }, spikes: { stride: 3 }, audio: { stride: 1, seconds: 12 } },
    ...overrides,
  };
}

function sampleMessages(count: number): Uint8Array[] {
  const messages: Uint8Array[] = [];
  for (let seq = 1; seq <= count; seq++) {
    const withSpikes = seq % 3 === 1;
    const kinds: AttachmentKind[] = withSpikes ? ['frame', 'spikes'] : ['frame'];
    const attachments: Partial<Record<AttachmentKind, Uint8Array>> = {
      frame: new Uint8Array([seq, seq + 1, seq + 2]),
    };
    if (withSpikes) attachments.spikes = new Uint8Array([0b0000_0111]);
    messages.push(encodeSnapshot(sampleHeader(seq, kinds), attachments));
  }
  return messages;
}

test('round-trips a manifest and its records', () => {
  const messages = sampleMessages(7);
  const file = encodeFlyfeed(sampleManifest({ durationMs: 200 }), messages);

  const { manifest, messages: decoded } = decodeFlyfeed(file);
  assert.equal(manifest.name, 'unit');
  assert.equal(manifest.snapshotCount, 7, 'encodeFlyfeed overwrites snapshotCount from the records');
  assert.equal(manifest.durationMs, 200);
  assert.deepEqual(manifest.attachmentPolicy.spikes, { stride: 3 });
  assert.equal(decoded.length, 7);

  for (let i = 0; i < messages.length; i++) {
    assert.deepEqual([...(decoded[i] as Uint8Array)], [...(messages[i] as Uint8Array)], `record ${i} bytes`);
  }
});

test('records stay decodable as feed snapshots', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(4));
  const { messages } = decodeFlyfeed(file);

  const first = decodeSnapshot(messages[0] as Uint8Array);
  assert.equal(first.header.seq, 1);
  assert.deepEqual([...(first.attachments.get('spikes') as Uint8Array)], [0b0000_0111]);

  const second = decodeSnapshot(messages[1] as Uint8Array);
  assert.equal(second.header.seq, 2);
  assert.equal(second.attachments.has('spikes'), false, 'strided-out attachment is absent');
  assert.equal(second.header.spikeCount, 0, 'spikeCount is 0 when the spikes attachment is omitted');
});

test('the streaming primitives produce the same bytes as encodeFlyfeed', () => {
  const messages = sampleMessages(5);
  const manifest = sampleManifest({ snapshotCount: 5, durationMs: 133 });

  const streamed: number[] = [...encodeFlyfeedHeader(manifest)];
  for (const message of messages) streamed.push(...encodeFlyfeedRecord(message));

  assert.deepEqual([...encodeFlyfeed(manifest, messages)], streamed);
});

test('the header carries the magic and version', () => {
  const head = encodeFlyfeedHeader(sampleManifest());
  assert.equal(String.fromCharCode(...head.subarray(0, 8)), FLYFEED_MAGIC);
  assert.equal(new DataView(head.buffer, head.byteOffset, head.byteLength).getUint32(8, true), FLYFEED_VERSION);
});

test('records are zero-copy views into the file buffer', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(3));
  const { bodyOffset } = readFlyfeedManifest(file);
  for (const record of iterateFlyfeedRecords(file, bodyOffset)) {
    assert.equal(record.buffer, file.buffer, 'record shares the file ArrayBuffer');
  }
});

test('iteration can start from the manifest offset without decoding every record', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(6));
  const { manifest, bodyOffset } = readFlyfeedManifest(file);
  assert.equal(manifest.snapshotCount, 6);

  const iterator = iterateFlyfeedRecords(file, bodyOffset);
  assert.equal(decodeSnapshot(iterator.next().value as Uint8Array).header.seq, 1);
  assert.equal(decodeSnapshot(iterator.next().value as Uint8Array).header.seq, 2);
});

test('rejects bad magic, and says so specifically for a gzipped file', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(2));
  const gz = new Uint8Array(gzipSync(file));

  assert.equal(isGzip(gz), true);
  assert.equal(isGzip(file), false);
  assert.throws(() => decodeFlyfeed(gz), (error: unknown) => {
    assert.ok(error instanceof FlyfeedError);
    assert.match((error as Error).message, /gzipped/);
    return true;
  });

  const garbage = new Uint8Array(64);
  assert.throws(() => decodeFlyfeed(garbage), /bad magic/);
});

test('rejects a truncated file, a truncated record and a miscounted manifest', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(3));

  assert.throws(() => decodeFlyfeed(file.subarray(0, 6)), /shorter than the header/);
  assert.throws(() => decodeFlyfeed(file.subarray(0, file.byteLength - 4)), /truncated record/);

  const head = encodeFlyfeedHeader(sampleManifest({ snapshotCount: 99 }));
  const messages = sampleMessages(1);
  const wrongCount = new Uint8Array(head.byteLength + 4 + (messages[0] as Uint8Array).byteLength);
  wrongCount.set(head, 0);
  wrongCount.set(encodeFlyfeedRecord(messages[0] as Uint8Array), head.byteLength);
  assert.throws(() => decodeFlyfeed(wrongCount), /claims 99 snapshots but the file holds 1/);
});

test('rejects an unsupported container version and an unsupported protocol', () => {
  const file = encodeFlyfeed(sampleManifest(), sampleMessages(1));
  const bumped = Uint8Array.from(file);
  new DataView(bumped.buffer).setUint32(8, 2, true);
  assert.throws(() => readFlyfeedManifest(bumped), /unsupported .flyfeed version 2/);

  assert.throws(
    () => encodeFlyfeedHeader(sampleManifest({ protocol: 2 as unknown as 1 })),
    /unsupported protocol 2/,
  );
});

test('an empty fixture is legal', () => {
  const { manifest, messages } = decodeFlyfeed(encodeFlyfeed(sampleManifest(), []));
  assert.equal(messages.length, 0);
  assert.equal(manifest.snapshotCount, 0);
});
