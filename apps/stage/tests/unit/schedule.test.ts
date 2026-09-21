/**
 * The narrative lane's rotation. The requirement is specific — the ticker holds the lane for
 * three minutes of every four, then one rotating card takes the last minute, a different card
 * each cycle — so it is worth pinning at the boundaries rather than trusting the arithmetic by
 * eye.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { CARD_START_MS, CYCLE_MS, narrativeProgressAt, narrativeStateAt } from '../../src/lib/schedule';

const CARDS = 4;

test('the cycle is four minutes and the card takes the last sixty seconds', () => {
  assert.equal(CYCLE_MS, 240_000);
  assert.equal(CYCLE_MS - CARD_START_MS, 60_000);
});

test('the two states appear at the documented boundaries', () => {
  assert.deepEqual(narrativeStateAt(0, CARDS), { kind: 'ticker' });
  assert.deepEqual(narrativeStateAt(CARD_START_MS - 1, CARDS), { kind: 'ticker' });
  assert.deepEqual(narrativeStateAt(CARD_START_MS, CARDS), { kind: 'card', index: 0 });
  assert.deepEqual(narrativeStateAt(CYCLE_MS - 1, CARDS), { kind: 'card', index: 0 });
  assert.deepEqual(narrativeStateAt(CYCLE_MS, CARDS), { kind: 'ticker' }, 'the cycle restarts');
});

test('the ticker holds the lane for most of every cycle', () => {
  let ticker = 0;
  for (let ms = 0; ms < CYCLE_MS; ms += 1000) {
    if (narrativeStateAt(ms, CARDS).kind === 'ticker') ticker += 1000;
  }
  assert.equal(ticker, 180_000, 'three minutes of events per cycle');
});

test('one card per cycle, cycling through all of them', () => {
  const seen = new Set<number>();
  for (let cycle = 0; cycle < CARDS; cycle++) {
    const state = narrativeStateAt(cycle * CYCLE_MS + CARD_START_MS + 5, CARDS);
    assert.equal(state.kind, 'card');
    if (state.kind === 'card') seen.add(state.index);
  }
  assert.equal(seen.size, CARDS, 'every card gets its turn');
});

test('the card index stays in range whatever the card count', () => {
  for (const count of [1, 2, 7, 9, 11]) {
    for (let cycle = 0; cycle < 25; cycle++) {
      const state = narrativeStateAt(cycle * CYCLE_MS + CARD_START_MS, count);
      assert.equal(state.kind, 'card');
      if (state.kind === 'card') {
        assert.ok(state.index >= 0 && state.index < count, `index ${state.index} out of range for ${count}`);
      }
    }
  }
});

test('with no cards the lane stays on the ticker instead of rendering nothing', () => {
  assert.deepEqual(narrativeStateAt(CARD_START_MS, 0), { kind: 'ticker' });
});

test('a negative or non-finite run time falls back to the ticker', () => {
  assert.deepEqual(narrativeStateAt(-1, CARDS), { kind: 'ticker' });
  assert.deepEqual(narrativeStateAt(Number.NaN, CARDS), { kind: 'ticker' });
  assert.equal(narrativeProgressAt(Number.NaN), 0);
});

test('progress runs 0..1 within the card state and is 0 on the ticker', () => {
  assert.equal(narrativeProgressAt(1000), 0);
  assert.equal(narrativeProgressAt(CARD_START_MS), 0);
  assert.ok(Math.abs(narrativeProgressAt(CARD_START_MS + 30_000) - 0.5) < 1e-9);
  assert.ok(narrativeProgressAt(CYCLE_MS - 1) < 1);
});
