/**
 * The NAMED CIRCUITS bar scale: `CircuitScale` has to do what a fixed Hz ceiling cannot — read
 * "how far above resting" without ever being told the feed's absolute Hz range (the bug behind
 * `infra/docs/p0-local-encoded-frame.png`, every bar pegged at 100%).
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  circuitFraction,
  CircuitScale,
  DEFAULT_HEADROOM,
  RunningMedian,
  thresholdRateHz,
} from '../../src/lib/circuit-scale';

test('a steady rate settles at a mid-range fill, not pegged at full', () => {
  const scale = new CircuitScale(5, { attackHalfLifeMs: 1000, releaseHalfLifeMs: 5000 });

  let fill = 0;
  let nowMs = 0;
  // Feed the same rate for a long stretch, well past several attack half-lives, so the reference
  // has fully caught up to it.
  for (let i = 0; i < 200; i++) {
    nowMs += 100;
    fill = scale.update(40, nowMs);
  }

  // The reference has converged on the steady value, so the fill is exactly the headroom
  // fraction — comfortably inside the 30-90% band the fix asks for, not pinned at 100%.
  const expected = 1 / DEFAULT_HEADROOM;
  assert.ok(Math.abs(fill - expected) < 0.01, `steady fill ${fill} was not near ${expected}`);
  assert.ok(fill > 0.3 && fill < 0.9, `steady fill ${fill} left the 30-90% band`);
});

test('a burst well above the settled rate reads as near full scale', () => {
  const scale = new CircuitScale(5, { attackHalfLifeMs: 4000, releaseHalfLifeMs: 20000 });

  let nowMs = 0;
  for (let i = 0; i < 50; i++) {
    nowMs += 100;
    scale.update(10, nowMs); // settle at a low steady rate first
  }

  // A fast burst: a handful of frames at a much higher rate, well under one attack half-life, the
  // way an 85 ms A/B press or a short D-pad hold would look.
  let burstFill = 0;
  for (let i = 0; i < 3; i++) {
    nowMs += 33;
    burstFill = scale.update(60, nowMs);
  }

  assert.ok(burstFill > 0.9, `burst fill ${burstFill} did not read as near full`);
});

test('after a burst ends, the fill decays back down toward the steady baseline over time', () => {
  const scale = new CircuitScale(5, { attackHalfLifeMs: 4000, releaseHalfLifeMs: 20000 });

  let nowMs = 0;
  // Long enough (ten attack half-lives) that the reference has fully converged on the steady
  // rate before the burst, so the burst is what pushes it above 10, not settling noise.
  for (let i = 0; i < 400; i++) {
    nowMs += 100;
    scale.update(10, nowMs);
  }
  for (let i = 0; i < 3; i++) {
    nowMs += 33;
    scale.update(60, nowMs);
  }

  // The burst ends; the rate drops straight back to the pre-burst steady value.
  nowMs += 33;
  const rightAfter = scale.update(10, nowMs);

  // The reference is still elevated from the burst, so the bar reads compressed immediately
  // after — not simply back at the pre-burst steady fraction yet.
  const steadyFraction = 10 / (10 * DEFAULT_HEADROOM);
  assert.ok(rightAfter < steadyFraction, `fill right after the burst (${rightAfter}) should read low`);

  // Given a long stretch of quiet afterward (several release half-lives), the reference relaxes
  // back down and the fill climbs back toward the steady fraction.
  let later = rightAfter;
  for (let i = 0; i < 3000; i++) {
    nowMs += 100;
    later = scale.update(10, nowMs);
  }
  assert.ok(later > rightAfter, 'the fill never recovered after the burst passed');
  assert.ok(Math.abs(later - steadyFraction) < 0.02, `fill did not settle back near ${steadyFraction}, got ${later}`);
});

test('circuitFraction is pure, clamped and scale-agnostic', () => {
  assert.equal(circuitFraction(0, 10), 0);
  assert.equal(circuitFraction(-5, 10), 0);
  assert.equal(circuitFraction(1000, 10), 1, 'a huge value clamps rather than overflowing the bar');
  assert.ok(circuitFraction(10, 10, 1) <= 1 && circuitFraction(10, 10, 1) > 0.99);
});

test('the reference never collapses toward zero while the feed is quiet at boot', () => {
  const scale = new CircuitScale(5, { floorHz: 2 });
  let nowMs = 0;
  for (let i = 0; i < 500; i++) {
    nowMs += 100;
    scale.update(0, nowMs);
  }
  assert.ok(scale.referenceHz >= 2, `reference collapsed to ${scale.referenceHz}`);
});

test('the running median tracks the typical rate, not a single sample', () => {
  const median = new RunningMedian(5, 10);
  let value = 5;
  for (let i = 0; i < 2000; i++) {
    // Alternate around a true median of 8: mostly small perturbations, with the occasional spike
    // that a real median should shrug off.
    value = i % 50 === 0 ? 80 : i % 2 === 0 ? 7 : 9;
    median.observe(value, 50);
  }
  assert.ok(
    median.medianHz >= 7 && median.medianHz <= 9,
    `median settled at ${median.medianHz}, outside the [7, 9] equilibrium band`,
  );
});

test('thresholdRateHz inverts the decoder score formula from docs/readout.md', () => {
  // score = (rate + 1) / (baseline + 1)  =>  rate = score * (baseline + 1) - 1
  assert.equal(thresholdRateHz(1, 9), 9, 'a threshold of 1 is exactly the baseline');
  assert.ok(thresholdRateHz(1.35, 9) > 9, 'a threshold above 1 sits above the baseline');
  assert.equal(thresholdRateHz(1, 0), 0);
  // A negative "median" (never produced by RunningMedian, which floors itself) is clamped to 0
  // before the formula runs, not passed through negative.
  assert.equal(thresholdRateHz(2, -5), 1, 'a negative baseline is treated as 0, not negative');
  assert.equal(thresholdRateHz(0, 9), 0, 'a threshold of 0 never returns a negative rate either');
});
