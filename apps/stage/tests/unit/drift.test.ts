/**
 * The audio ring buffer's drift maths.
 *
 * Worth real tests because the failure is invisible in any short run: get a sign wrong and the
 * buffer diverges over an hour instead of converging, and the first symptom is a stream that
 * clicks once a minute three hours in.
 *
 * These pin the policy that `src/audio/engine.ts` hands to the worklet in `processorOptions`.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  DEFAULT_DRIFT_POLICY,
  dropCount,
  fillZone,
  framesFor,
  hardCorrection,
  varispeedRate,
} from '../../src/audio/drift';

const RATE = 48_000;
const policy = DEFAULT_DRIFT_POLICY;
const target = framesFor(policy.targetMs, RATE);

test('the policy matches the design: 250 ms target, 120/400 watermarks, ±0.3 percent', () => {
  assert.equal(policy.targetMs, 250);
  assert.equal(policy.lowMs, 120);
  assert.equal(policy.highMs, 400);
  assert.equal(policy.maxDrift, 0.003);
  assert.ok(policy.capacityMs > policy.highMs * 2, 'the ring must hold well more than the high watermark');
});

test('frames convert the way the 30 Hz feed expects', () => {
  assert.equal(framesFor(250, RATE), 12_000);
  // One snapshot at 30 Hz carries about 1,600 frames per the contract.
  assert.equal(framesFor(1000 / 30, RATE), 1600);
});

test('at the target fill the rate is exactly 1', () => {
  assert.equal(varispeedRate(target, policy, RATE), 1);
});

test('a full buffer speeds playback up and an empty one slows it down', () => {
  assert.ok(varispeedRate(target * 1.5, policy, RATE) > 1, 'too much buffered: consume faster');
  assert.ok(varispeedRate(target * 0.5, policy, RATE) < 1, 'too little buffered: consume slower');
});

test('the correction is clamped so it stays inaudible', () => {
  for (const fill of [0, 1, target * 0.1, target * 5, target * 100]) {
    const rate = varispeedRate(fill, policy, RATE);
    assert.ok(rate >= 1 - policy.maxDrift - 1e-12, `rate ${rate} below the clamp`);
    assert.ok(rate <= 1 + policy.maxDrift + 1e-12, `rate ${rate} above the clamp`);
  }
});

test('the servo converges rather than diverging', () => {
  // Producer runs 0.1 percent fast; the servo must stop the buffer growing without bound.
  let fill = target;
  const blockFrames = 128;
  const producedPerBlock = blockFrames * 1.001;

  let worst = fill;
  for (let block = 0; block < 200_000; block++) {
    const rate = varispeedRate(fill, policy, RATE);
    fill += producedPerBlock - blockFrames * rate;
    if (hardCorrection(fill, policy, RATE) === 'drop') fill -= dropCount(fill, policy, RATE);
    worst = Math.max(worst, fill);
  }

  const capacity = framesFor(policy.capacityMs, RATE);
  assert.ok(worst < capacity, `fill reached ${worst.toFixed(0)} frames, past the ${capacity} capacity`);
  assert.ok(fill > 0, 'and it did not drain either');
});

test('a slow producer is absorbed the same way', () => {
  let fill = target;
  const blockFrames = 128;
  let underruns = 0;

  for (let block = 0; block < 200_000; block++) {
    const rate = varispeedRate(fill, policy, RATE);
    fill += blockFrames * 0.999 - blockFrames * rate;
    if (fill <= 0) {
      underruns += 1;
      fill = target; // what the worklet does: emit silence, then carry on
    }
  }
  assert.ok(underruns < 40, `${underruns} underruns over 200k blocks is not absorption`);
});

test('gross excursions are named, and the drop count returns to the target', () => {
  assert.equal(hardCorrection(0, policy, RATE), 'insert');
  assert.equal(hardCorrection(target, policy, RATE), null);
  assert.equal(hardCorrection(target * 2, policy, RATE), null, 'exactly twice is still tolerable');
  assert.equal(hardCorrection(target * 2 + 1, policy, RATE), 'drop');

  const over = target * 3;
  assert.equal(over - dropCount(over, policy, RATE), target);
  assert.equal(dropCount(target, policy, RATE), 0);
});

test('the fill zones line up with the watermarks', () => {
  assert.equal(fillZone(framesFor(100, RATE), policy, RATE), 'low');
  assert.equal(fillZone(framesFor(250, RATE), policy, RATE), 'ok');
  assert.equal(fillZone(framesFor(500, RATE), policy, RATE), 'high');
});
