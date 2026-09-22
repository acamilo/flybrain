import assert from 'node:assert/strict';
import test from 'node:test';

import { readRational } from '../src/common';
import * as fixtures from '../src/fixtures';
import {
  RATIONAL_ZERO,
  U64_MAX,
  addRational,
  compareRational,
  divideFloor,
  multiplyRational,
  reduced,
  requirePositiveRational,
  subtractRational,
  validateRational,
} from '../src/scalar';

test('the accumulator produces the fixture tick counts and remainders', () => {
  const file = fixtures.load('rational.json');
  for (const item of fixtures.section(file, 'accumulator')) {
    const name = fixtures.field(item, 'name');
    const step = readRational(fixtures.member(item, 'stepDuration'));
    const tick = readRational(fixtures.member(item, 'tickDuration'));
    let accumulator = RATIONAL_ZERO;
    let total = 0n;
    fixtures.section(item, 'steps').forEach((expected, index) => {
      accumulator = addRational(accumulator, step);
      const { ticks, remainder } = divideFloor(accumulator, tick);
      accumulator = remainder;
      total += BigInt(ticks);
      assert.equal(ticks, fixtures.field(expected, 'ticks'), `${name}: ticks at step ${index}`);
      assert.deepEqual(
        remainder,
        readRational(fixtures.member(expected, 'remainder')),
        `${name}: remainder at step ${index}`,
      );
      assert.ok(compareRational(remainder, tick) < 0, `${name}: remainder below one tick`);
    });
    assert.equal(total.toString(), fixtures.field(item, 'totalTicks'), `${name}: total ticks`);
  }
});

test('checked arithmetic reduces or refuses', () => {
  const file = fixtures.load('rational.json');
  for (const item of fixtures.section(file, 'add')) {
    const left = readRational(fixtures.member(item, 'a'));
    const right = readRational(fixtures.member(item, 'b'));
    const record = item as Record<string, unknown>;
    if (record.sum !== undefined) {
      assert.deepEqual(addRational(left, right), readRational(record.sum as never));
    } else {
      assert.throws(() => addRational(left, right));
    }
  }
  for (const item of fixtures.section(file, 'subtract')) {
    const left = readRational(fixtures.member(item, 'a'));
    const right = readRational(fixtures.member(item, 'b'));
    const record = item as Record<string, unknown>;
    if (record.difference !== undefined) {
      assert.deepEqual(subtractRational(left, right), readRational(record.difference as never));
    } else {
      assert.throws(() => subtractRational(left, right));
    }
  }
  for (const item of fixtures.section(file, 'multiply')) {
    const value = readRational(fixtures.member(item, 'a'));
    const factor = BigInt(fixtures.field(item, 'k'));
    const record = item as Record<string, unknown>;
    if (record.product !== undefined) {
      assert.deepEqual(multiplyRational(value, factor), readRational(record.product as never));
    } else {
      assert.throws(() => multiplyRational(value, factor));
    }
  }
  for (const item of fixtures.section(file, 'compare')) {
    const left = readRational(fixtures.member(item, 'a'));
    const right = readRational(fixtures.member(item, 'b'));
    const expected = { less: -1, equal: 0, greater: 1 }[fixtures.field(item, 'ordering')];
    assert.equal(compareRational(left, right), expected);
  }
});

test('zero has exactly one encoding and durations must be positive', () => {
  validateRational(RATIONAL_ZERO);
  assert.throws(() => validateRational({ numerator: '0', denominator: '2' }), /0\/1/);
  assert.throws(() => validateRational({ numerator: '1', denominator: '0' }), /positive/);
  assert.throws(() => validateRational({ numerator: '2', denominator: '4' }), /reduced/);
  assert.throws(() => requirePositiveRational(RATIONAL_ZERO, 'worldTime'));
  assert.throws(() => divideFloor(RATIONAL_ZERO, RATIONAL_ZERO), /positive/);
});

test('reduction refuses a result that does not fit U64', () => {
  const big = { numerator: U64_MAX.toString(), denominator: '1' };
  assert.throws(() => multiplyRational(big, 2n), /does not fit U64/);
  assert.throws(() => addRational(big, big), /does not fit U64/);
  assert.deepEqual(reduced(U64_MAX * 2n, 2n), big);
});
