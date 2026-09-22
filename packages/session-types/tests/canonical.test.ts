import assert from 'node:assert/strict';
import test from 'node:test';

import {
  MAX_ENVELOPE_BYTES,
  canonicalize,
  digestOf,
  parseStrict,
  requireEnvelopeFit,
} from '../src/canonical';
import { bodyDigest, operationKeyDigest, readScope } from '../src/common';
import * as fixtures from '../src/fixtures';

const ASTRAL = String.fromCodePoint(0x10400);
const FULLWIDTH_A = String.fromCodePoint(0xff21);
const E_ACUTE = String.fromCodePoint(0xe9);

test('object keys are sorted by UTF-16 code unit', () => {
  const value: Record<string, number> = { b: 1, a: 2, A: 3 };
  value[E_ACUTE] = 4;
  value[ASTRAL] = 5;
  value[FULLWIDTH_A] = 6;
  assert.equal(
    canonicalize(value),
    `{"A":3,"a":2,"b":1,"${E_ACUTE}":4,"${ASTRAL}":5,"${FULLWIDTH_A}":6}`,
    'an astral key, whose leading surrogate is D801, sorts before U+FF21',
  );
});

test('numbers print the way ECMAScript prints them', () => {
  const file = fixtures.load('boundaries.json');
  for (const item of fixtures.section(file, 'doubles')) {
    const record = item as Record<string, unknown>;
    const value = record.value as number;
    if (record.accept === true) {
      assert.equal(canonicalize(value), record.canonical, `${value} prints as its canonical form`);
    } else {
      assert.throws(() => canonicalize(value), `${value} must be refused`);
    }
  }
});

test('strings are escaped the way JSON.stringify escapes them', () => {
  const bell = String.fromCharCode(7);
  const del = String.fromCharCode(0x7f);
  const value = { s: ['q"', 'b\\', 't\t', 'n\n', bell, del].join(' ') };
  assert.equal(
    canonicalize(value),
    JSON.stringify(value),
    'for a single-key object the two agree exactly, escape for escape',
  );
  assert.ok(canonicalize(value).includes('\\u0007'), 'a control character uses lowercase \\u');
  assert.ok(canonicalize(value).includes(del), 'DEL is not an escape in JSON');
});

test('canonical form does not depend on the input formatting', () => {
  const compact = '{"b":[1,2,{"y":true,"x":null}],"a":"z"}';
  const pretty = '{\n  "a"  :  "z",\n  "b": [ 1, 2, { "x": null, "y": true } ]\n}';
  assert.equal(canonicalize(parseStrict(compact)), canonicalize(parseStrict(pretty)));
  assert.equal(digestOf(parseStrict(compact)), digestOf(parseStrict(pretty)));
});

test('duplicate keys and invalid UTF-8 never parse', () => {
  assert.throws(() => parseStrict('{"a":1,"a":2}'), /duplicate key/);
  assert.throws(() => parseStrict('{"a":{"b":1,"b":2}}'), /duplicate key/);
  assert.throws(() => parseStrict(new Uint8Array([0x7b, 0x22, 0xff, 0x22, 0x7d])), /UTF-8/);
  assert.throws(() => parseStrict('{"a":1} {"b":2}'), /trailing data/);
  assert.throws(() => parseStrict('{"a":NaN}'));
  assert.throws(() => parseStrict('{"a":Infinity}'));
  assert.throws(() => parseStrict('{"a":1'));
  assert.throws(() => parseStrict(''));
});

test('an envelope over 64 KiB is refused', () => {
  assert.throws(() => requireEnvelopeFit({ pad: 'a'.repeat(MAX_ENVELOPE_BYTES) }, 0));
  const small = { pad: 'a' };
  const length = canonicalize(small).length;
  assert.equal(requireEnvelopeFit(small, MAX_ENVELOPE_BYTES - length), MAX_ENVELOPE_BYTES);
  assert.throws(() => requireEnvelopeFit(small, MAX_ENVELOPE_BYTES - length + 1));
});

test('operation keys match the fixture and separate the operations they should', () => {
  const file = fixtures.load('operations.json');
  const digests: [string, string][] = [];
  for (const item of fixtures.section(file, 'keys')) {
    const name = fixtures.field(item, 'name');
    const digest = operationKeyDigest({
      scope: readScope(fixtures.member(item, 'scope')),
      method: fixtures.field(item, 'method'),
      workerId: fixtures.field(item, 'workerId'),
    });
    assert.equal(digest, fixtures.field(item, 'digest'), `${name}: operation key digest`);
    digests.push([name, digest]);
  }
  digests.forEach(([name, digest], index) => {
    for (const [otherName, other] of digests.slice(index + 1)) {
      assert.notEqual(digest, other, `${name} and ${otherName} are different operations`);
    }
  });
});

test('canonical bodies match the fixture', () => {
  const file = fixtures.load('operations.json');
  for (const item of fixtures.section(file, 'bodies')) {
    const scopeValue = fixtures.member(item, 'scope');
    const digest = bodyDigest(
      fixtures.field(item, 'method'),
      scopeValue === null ? null : readScope(scopeValue),
      fixtures.member(item, 'params'),
    );
    assert.equal(digest, fixtures.field(item, 'digest'), fixtures.field(item, 'name'));
  }
});

test('operation pairs agree with the fixture about sameness', () => {
  const file = fixtures.load('operations.json');
  for (const item of fixtures.section(file, 'pairs')) {
    const name = fixtures.field(item, 'name');
    const reason = fixtures.field(item, 'reason');
    const worker = fixtures.field(item, 'workerId');
    const rightWorker = fixtures.optionalField(item, 'rightWorkerId') ?? worker;
    const side = (key: string, workerId: string): [string, string] => {
      const value = fixtures.member(item, key);
      const method = fixtures.field(value, 'method');
      const scope = readScope(fixtures.member(value, 'scope'));
      const params = fixtures.member(value, 'params');
      return [operationKeyDigest({ scope, method, workerId }), bodyDigest(method, scope, params)];
    };
    const [leftKey, leftBody] = side('left', worker);
    const [rightKey, rightBody] = side('right', rightWorker);
    const record = item as Record<string, unknown>;
    assert.equal(leftKey === rightKey, record.sameKey, `${name}: key sameness. ${reason}`);
    assert.equal(leftBody === rightBody, record.sameBody, `${name}: body sameness. ${reason}`);
  }
});

test('a domain body can never carry a bus identity', () => {
  const file = fixtures.load('operations.json');
  for (const item of fixtures.section(file, 'rejected')) {
    assert.throws(
      () =>
        bodyDigest(
          fixtures.field(item, 'method'),
          readScope(fixtures.member(item, 'scope')),
          fixtures.member(item, 'params'),
        ),
      `${fixtures.field(item, 'name')} must be refused`,
    );
  }
});
