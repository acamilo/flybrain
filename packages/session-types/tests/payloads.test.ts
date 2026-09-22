import assert from 'node:assert/strict';
import test from 'node:test';

import { canonicalize, parseStrict, requireEnvelopeFit, sha256Hex } from '../src/canonical';
import { MAX_TYPED_VALUE_BYTES } from '../src/scalar';
import { readTypedValue } from '../src/common';
import * as fixtures from '../src/fixtures';
import { READERS, roundTrip } from './readers';

test('every valid case round trips and canonicalizes to its recorded bytes', () => {
  const file = fixtures.load('valid.json');
  const cases = fixtures.cases(file);
  for (const item of cases) {
    const name = fixtures.field(item, 'name');
    const typeName = fixtures.field(item, 'type');
    const value = fixtures.member(item, 'value');
    let written: unknown;
    try {
      written = roundTrip(typeName, value);
    } catch (error) {
      assert.fail(`${name} (${typeName}) must be accepted: ${String(error)}`);
    }
    const canonicalIn = canonicalize(value);
    assert.equal(
      canonicalize(written),
      canonicalIn,
      `${name}: reading and writing must preserve every field`,
    );
    assert.equal(canonicalIn, fixtures.field(item, 'canonical'), `${name}: canonical JSON`);
    assert.equal(sha256Hex(canonicalIn), fixtures.field(item, 'digest'), `${name}: digest`);
  }
  assert.ok(cases.length >= 70, 'the valid fixture should stay broad');
});

test('every type this package reads appears in the valid fixture', () => {
  const covered = fixtures
    .cases(fixtures.load('valid.json'))
    .map((item) => fixtures.field(item, 'type'));
  const missing = Object.keys(READERS).filter((typeName) => !covered.includes(typeName));
  assert.deepEqual(missing, [], 'every readable type needs at least one accepted fixture');
});

test('every invalid case is refused', () => {
  const file = fixtures.load('invalid.json');
  const cases = fixtures.cases(file);
  for (const item of cases) {
    const name = fixtures.field(item, 'name');
    const typeName = fixtures.field(item, 'type');
    const reason = fixtures.field(item, 'reason');
    assert.throws(
      () => roundTrip(typeName, fixtures.member(item, 'value')),
      `${name} (${typeName}) must be refused: ${reason}`,
    );
  }
  assert.ok(cases.length >= 80, 'the invalid fixture should stay broad');
});

test('every raw byte case is refused before or during validation', () => {
  for (const item of fixtures.cases(fixtures.load('raw.json'))) {
    const name = fixtures.field(item, 'name');
    const typeName = fixtures.field(item, 'type');
    const reason = fixtures.field(item, 'reason');
    const bytes = fixtures.decodeBase64(fixtures.field(item, 'base64'));
    // parseStrict is the only door into the readers, so a byte sequence that does not parse
    // never reaches validation.
    assert.throws(
      () => roundTrip(typeName, parseStrict(bytes)),
      `${name} must be refused: ${reason}`,
    );
  }
});

test('generated boundary cases land on the right side of every limit', () => {
  const file = fixtures.load('generated.json');
  const padSchema = fixtures.member(file, 'padSchema');
  for (const item of fixtures.cases(file)) {
    const name = fixtures.field(item, 'name');
    const kind = fixtures.field(item, 'kind');
    const expectAccept = fixtures.field(item, 'expect') === 'accept';
    const record = item as Record<string, unknown>;
    const attempt = () => {
      switch (kind) {
        case 'padded-typed-value': {
          const pad = 'a'.repeat(record.padCharacters as number);
          readTypedValue({ schema: padSchema, value: { pad } });
          return;
        }
        case 'padded-request': {
          const pad = 'a'.repeat(record.padCharacters as number);
          const total = record.envelopeTotal as number;
          const body = roundTrip('SessionRpcRequest', {
            requestId: 'req-1',
            scope: null,
            params: { pad },
          });
          requireEnvelopeFit(body, total - canonicalize(body).length);
          return;
        }
        case 'error-message':
        case 'error-message-astral': {
          const character = kind === 'error-message' ? 'x' : '\u{10400}';
          const message = character.repeat(record.codePoints as number);
          roundTrip('SessionRpcFailure', {
            type: 'error',
            requestId: 'req-41',
            workerId: 'fly-a',
            incarnationId: 'inc-1',
            scope: null,
            error: { code: 'INTERNAL', message, mutation: 'unknown' },
          });
          return;
        }
        default:
          throw new Error(`unknown generated case kind "${kind}"`);
      }
    };
    if (expectAccept) {
      assert.doesNotThrow(attempt, `${name} must be accepted`);
    } else {
      assert.throws(attempt, `${name} must be refused`);
    }
  }
});

test('a typed value at the cap is accepted and one byte more is not', () => {
  const schema = { id: 'pad.v1', version: 1, digest: sha256Hex('pad.v1') };
  const overhead = canonicalize({ schema, value: { pad: '' } }).length;
  const atCap = readTypedValue({
    schema,
    value: { pad: 'a'.repeat(MAX_TYPED_VALUE_BYTES - overhead) },
  });
  assert.equal(canonicalize(atCap).length, MAX_TYPED_VALUE_BYTES);
  assert.throws(() =>
    readTypedValue({ schema, value: { pad: 'a'.repeat(MAX_TYPED_VALUE_BYTES - overhead + 1) } }),
  );
});
