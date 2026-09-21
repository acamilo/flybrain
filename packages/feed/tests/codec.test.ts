import assert from 'node:assert/strict';
import test from 'node:test';
import { FeedCodecError, decodeSnapshot, encodeSnapshot } from '../src/codec';
import type { AttachmentKind, FeedHeader } from '../src/types';

function sampleHeader(attachments: AttachmentKind[]): FeedHeader {
  return {
    protocol: 1,
    seq: 42,
    wallMs: 1_700_000_000_000,
    status: 'running',
    realtimeFactor: 1.0,
    uptimeSeconds: 120.5,
    runSeconds: 120.5,
    brainMs: 120_500,
    frame: 3600,
    buttons: 0b0001_0001,
    rates: { command_0: 12.5, command_1: 6.2, reward_pam: 2.1 },
    populationRate: 13.2,
    spikeCount: 3,
    learning: { enabled: true, updates: 10, changed: 4, synapses: 4_200_000, signal: 0.12 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 5,
      rewardTotal: 0.42,
      rewardCounts: { story: 0, explore: 3, area: 1, pokedex: 0, trainer: 0, wildwin: 1, badge: 0 },
    },
    milestone: { rank: 2, label: 'Left the house', next: 'Reached Route 1', sinceSeconds: 30, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 1 },
    events: [{ id: 1, wallMs: 1_700_000_000_000, brainMs: 120_000, kind: 'reward', label: 'Explored a new room', value: 0.05, rewardKind: 'explore' }],
    attachments,
  };
}

test('encodeSnapshot/decodeSnapshot round trip with all three attachments', () => {
  const header = sampleHeader(['frame', 'audio', 'spikes']);
  const frame = new Uint8Array(92_160).fill(7);
  const audio = new Uint8Array(11_760).fill(9);
  const spikes = new Uint8Array(17_407).fill(0);
  spikes[0] = 0b0000_0011;

  const encoded = encodeSnapshot(header, { frame, audio, spikes });
  const decoded = decodeSnapshot(encoded);

  assert.deepEqual(decoded.header, header);
  assert.equal(decoded.attachments.size, 3);
  assert.deepEqual(decoded.attachments.get('frame'), frame);
  assert.deepEqual(decoded.attachments.get('audio'), audio);
  assert.deepEqual(decoded.attachments.get('spikes'), spikes);
});

test('encodeSnapshot/decodeSnapshot round trip with no attachments', () => {
  const header = sampleHeader([]);
  const encoded = encodeSnapshot(header, {});
  const decoded = decodeSnapshot(encoded);

  assert.deepEqual(decoded.header, header);
  assert.equal(decoded.attachments.size, 0);
});

test('encodeSnapshot honours header.attachments order regardless of object key order', () => {
  const header = sampleHeader(['spikes', 'frame']);
  const spikes = new Uint8Array([1, 2, 3]);
  const frame = new Uint8Array([4, 5, 6]);

  // Provide attachments in the opposite key order; encoding must still follow header.attachments.
  const encoded = encodeSnapshot(header, { frame, spikes });
  const decoded = decodeSnapshot(encoded);

  assert.deepEqual([...decoded.attachments.keys()], ['spikes', 'frame']);
  assert.deepEqual(decoded.attachments.get('spikes'), spikes);
  assert.deepEqual(decoded.attachments.get('frame'), frame);
});

test('encodeSnapshot rejects a header.protocol other than 1', () => {
  const header = { ...sampleHeader([]), protocol: 2 as unknown as 1 };
  assert.throws(() => encodeSnapshot(header, {}), FeedCodecError);
});

test('encodeSnapshot rejects a mismatch between header.attachments and provided attachments', () => {
  const header = sampleHeader(['frame']);
  assert.throws(() => encodeSnapshot(header, { frame: new Uint8Array(1), audio: new Uint8Array(1) }), FeedCodecError);
  assert.throws(() => encodeSnapshot(header, {}), FeedCodecError);
});

test('decodeSnapshot rejects a bad protocol', () => {
  const header = sampleHeader([]);
  const badHeaderBytes = new TextEncoder().encode(JSON.stringify({ ...header, protocol: 7 }));
  const buffer = new Uint8Array(4 + badHeaderBytes.length);
  new DataView(buffer.buffer).setUint32(0, badHeaderBytes.length, true);
  buffer.set(badHeaderBytes, 4);

  assert.throws(() => decodeSnapshot(buffer), /protocol/);
});

test('decodeSnapshot rejects truncation in the header length prefix', () => {
  const tooShort = new Uint8Array([1, 2, 3]); // fewer than 4 bytes for the u32 length prefix
  assert.throws(() => decodeSnapshot(tooShort), FeedCodecError);
});

test('decodeSnapshot rejects truncation in the header body', () => {
  const buffer = new Uint8Array(4 + 2);
  new DataView(buffer.buffer).setUint32(0, 100, true); // claims 100 bytes of header, only 2 present
  assert.throws(() => decodeSnapshot(buffer), /truncated/);
});

test('decodeSnapshot rejects truncation in an attachment body', () => {
  const header = sampleHeader(['frame']);
  const encoded = encodeSnapshot(header, { frame: new Uint8Array(10) });
  const truncated = encoded.subarray(0, encoded.length - 5); // cut off part of the attachment bytes

  assert.throws(() => decodeSnapshot(truncated), /truncated/);
});

test('decodeSnapshot rejects trailing bytes after the last attachment', () => {
  const header = sampleHeader(['frame']);
  const encoded = encodeSnapshot(header, { frame: new Uint8Array(10) });
  const withTrailingJunk = new Uint8Array(encoded.length + 3);
  withTrailingJunk.set(encoded);

  assert.throws(() => decodeSnapshot(withTrailingJunk), /trailing/);
});

test('decodeSnapshot rejects an attachment list with duplicate kinds', () => {
  const header = { ...sampleHeader(['frame']), attachments: ['frame', 'frame'] as AttachmentKind[] };
  const headerBytes = new TextEncoder().encode(JSON.stringify(header));
  const frameBytes = new Uint8Array(10);
  const buffer = new Uint8Array(4 + headerBytes.length + 4 + frameBytes.length);
  const view = new DataView(buffer.buffer);
  let offset = 0;
  view.setUint32(offset, headerBytes.length, true);
  offset += 4;
  buffer.set(headerBytes, offset);
  offset += headerBytes.length;
  view.setUint32(offset, frameBytes.length, true);
  offset += 4;
  buffer.set(frameBytes, offset);

  assert.throws(() => decodeSnapshot(buffer), /more than once/);
});

test('decodeSnapshot rejects malformed JSON in the header', () => {
  const badJson = new TextEncoder().encode('{not json');
  const buffer = new Uint8Array(4 + badJson.length);
  new DataView(buffer.buffer).setUint32(0, badJson.length, true);
  buffer.set(badJson, 4);

  assert.throws(() => decodeSnapshot(buffer), /JSON/);
});
