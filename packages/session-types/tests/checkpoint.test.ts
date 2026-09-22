import assert from 'node:assert/strict';
import test from 'node:test';

import { canonicalize, parseStrict } from '../src/canonical';
import * as checkpoint from '../src/checkpoint';
import * as fixtures from '../src/fixtures';

function envelopeBytes(): Uint8Array {
  const file = fixtures.load('checkpoint-envelope.json');
  const envelope = fixtures.member(file, 'envelope');
  return fixtures.decodeBase64(fixtures.field(envelope, 'base64'));
}

test('the fixture envelope decodes to its recorded layout', () => {
  const file = fixtures.load('checkpoint-envelope.json');
  const bytes = envelopeBytes();
  const envelope = checkpoint.decode(bytes);
  checkpoint.validateManifest(envelope);

  const layout = fixtures.member(fixtures.member(file, 'envelope'), 'layout') as Record<
    string,
    unknown
  >;
  assert.equal(Buffer.from(bytes.subarray(0, 8)).toString('ascii'), checkpoint.MAGIC);
  assert.equal(String(bytes.length), layout.totalBytes);
  assert.equal(String(envelope.layout.tableOffset), layout.tableOffset);
  assert.equal(envelope.layout.manifestBytes, layout.manifestBytes);
  const entries = layout.entries as Record<string, unknown>[];
  assert.equal(envelope.layout.entries.length, entries.length);
  envelope.layout.entries.forEach((entry, index) => {
    const recorded = entries[index]!;
    assert.equal(entry.name, recorded.name);
    assert.equal(String(entry.offset), recorded.offset);
    assert.equal(String(entry.byteLength), recorded.byteLength);
    assert.equal(entry.digest, recorded.digest);
    assert.equal(entry.offset % 8, 0, 'payloads start on an eight-byte boundary');
  });

  for (const payload of fixtures.section(file, 'payloads')) {
    const name = fixtures.field(payload, 'name');
    const expected = fixtures.decodeBase64(fixtures.field(payload, 'base64'));
    const found = envelope.payloads.find((candidate) => candidate.name === name);
    assert.ok(found, `payload ${name} must be present`);
    assert.deepEqual(found.bytes, expected, `payload ${name} must come back byte for byte`);
  }
  assert.equal(canonicalize(envelope.manifest), canonicalize(fixtures.member(file, 'manifest')));
});

test('every recorded corruption is refused', () => {
  const file = fixtures.load('checkpoint-envelope.json');
  const bytes = envelopeBytes();
  for (const item of fixtures.section(file, 'corruption')) {
    const name = fixtures.field(item, 'name');
    const offset = (item as Record<string, unknown>).offset as number;
    const corrupted = Uint8Array.from(bytes);
    corrupted[offset] = (corrupted[offset]! ^ 0x01) & 0xff;
    assert.throws(() => checkpoint.decode(corrupted), `${name} must be refused`);
  }
  assert.throws(() => checkpoint.decode(bytes.subarray(0, bytes.length - 1)));
  assert.throws(() => checkpoint.decode(bytes.subarray(0, 8)));
});

test('a FLYSIM01 envelope is not read as a session checkpoint', () => {
  const manifest = Buffer.from('{"schemaVersion":2,"chunks":["agent"]}', 'utf8');
  const length = Buffer.alloc(4);
  length.writeUInt32LE(manifest.length, 0);
  const chunkLength = Buffer.alloc(4);
  chunkLength.writeUInt32LE(64, 0);
  const legacy = Buffer.concat([
    Buffer.from('FLYSIM01', 'ascii'),
    length,
    manifest,
    chunkLength,
    Buffer.alloc(64),
    Buffer.alloc(4), // the CRC32 footer
  ]);
  assert.throws(() => checkpoint.decode(legacy), /magic/);
});

test('the layout is deterministic and the manifest is canonical', () => {
  const manifest = parseStrict('{"b":2,"a":1}');
  const payloads = [
    { name: 'one', bytes: new TextEncoder().encode('first') },
    { name: 'two', bytes: new Uint8Array(9) },
  ];
  const bytes = checkpoint.encode(manifest, payloads);
  assert.deepEqual(bytes, checkpoint.encode(manifest, payloads));
  const envelope = checkpoint.decode(bytes);
  const start = checkpoint.HEADER_BYTES;
  const end = start + envelope.layout.manifestBytes;
  assert.equal(Buffer.from(bytes.subarray(start, end)).toString('utf8'), '{"a":1,"b":2}');
  assert.throws(() =>
    checkpoint.encode(manifest, [
      { name: 'one', bytes: new Uint8Array() },
      { name: 'one', bytes: new Uint8Array() },
    ]),
  );
  assert.throws(() => checkpoint.encode(manifest, [{ name: 'One', bytes: new Uint8Array() }]));
});

test('an envelope written here is read by the same rules the Rust crate wrote its fixture with', () => {
  const file = fixtures.load('checkpoint-envelope.json');
  const manifest = fixtures.member(file, 'manifest');
  const payloads = fixtures
    .section(file, 'payloads')
    .map((payload) => ({
      name: fixtures.field(payload, 'name'),
      bytes: fixtures.decodeBase64(fixtures.field(payload, 'base64')),
    }));
  assert.deepEqual(
    Uint8Array.from(checkpoint.encode(manifest, payloads)),
    Uint8Array.from(envelopeBytes()),
    'the two implementations produce the same bytes for the same inputs',
  );
});

test('a manifest missing a required field is not a complete checkpoint', () => {
  const file = fixtures.load('checkpoint-envelope.json');
  const full = fixtures.member(file, 'manifest') as Record<string, unknown>;
  for (const field of checkpoint.REQUIRED_MANIFEST_FIELDS) {
    const manifest = { ...full };
    delete manifest[field];
    const envelope = checkpoint.decode(checkpoint.encode(manifest, []));
    assert.throws(() => checkpoint.validateManifest(envelope), `without ${field}`);
  }
});
