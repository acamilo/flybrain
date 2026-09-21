import assert from 'node:assert/strict';
import test from 'node:test';

import { marqueeCycleMs, marqueeDistance, MARQUEE_HOLD_MS, MARQUEE_STEP_MS, MARQUEE_STEP_PX } from '../../src/lib/marquee';

test('a name that fits does not marquee', () => {
  assert.equal(marqueeDistance(150, 200), 0);
  assert.equal(marqueeDistance(200, 200), 0);
  assert.equal(marqueeCycleMs(0), 0);
});

test('distance rounds up to a whole pixel-grid step', () => {
  // 10 px of overflow rounds up to 16 (two 8 px steps), not 8.
  assert.equal(marqueeDistance(210, 200), 16);
  // Exactly two steps of overflow stays exact.
  assert.equal(marqueeDistance(216, 200), 16);
});

test('the cycle is symmetric: travel out and back, holding at each end', () => {
  const distance = marqueeDistance(216, 200); // 16 px, two steps
  const steps = distance / MARQUEE_STEP_PX;
  const cycle = marqueeCycleMs(distance);
  assert.equal(cycle, steps * MARQUEE_STEP_MS * 2 + MARQUEE_HOLD_MS * 2);
  assert.ok(cycle > MARQUEE_HOLD_MS * 2, 'the cycle must include travel time, not just the holds');
});

test('a longer overflow takes proportionally longer', () => {
  const short = marqueeCycleMs(marqueeDistance(220, 200));
  const long = marqueeCycleMs(marqueeDistance(300, 200));
  assert.ok(long > short, 'more distance to cover should take more time');
});
