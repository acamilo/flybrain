/**
 * The rig math the design's verification list points at now that the fly no longer taps
 * anything: "gait speed rises with forward drive and stride asymmetry follows steer"
 * (`docs/design/fly-avatar.md`). This replaces the old Playwright check that a leg landed on the
 * button cap the bitmask named, which stopped applying once the Game Boy and its taps were
 * removed — the legs only walk now.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { FlyRig, idleDrives, type FlyDrives, type FlyFrame } from '../../src/fly/rig';

/** Every leg's tarsus tip, in `legs[leg][3]` (coxa, trochanter, knee, tip). */
function tips(frame: FlyFrame): [number, number, number][] {
  return frame.legs.map((joints) => joints[3] as [number, number, number]);
}

test('gait phase advances faster with a stronger forward drive', () => {
  const slow = new FlyRig();
  const fast = new FlyRig();
  const slowDrives: FlyDrives = { ...idleDrives(), forward: 0.2 };
  const fastDrives: FlyDrives = { ...idleDrives(), forward: 1 };

  let now = 0;
  for (let i = 0; i < 10; i++) {
    now += 33;
    slow.advance(slowDrives, now);
    fast.advance(fastDrives, now);
  }

  assert.ok(slow.gaitPhase > 0, 'the slow fly should still be walking, just less far into its cycle');
  assert.ok(
    fast.gaitPhase > slow.gaitPhase,
    `a stronger forward drive (phase ${fast.gaitPhase.toFixed(3)}) should outrun a weaker one (${slow.gaitPhase.toFixed(3)})`,
  );
});

test('a resting fly (forward and backward both zero) does not shuffle its feet', () => {
  const idle = new FlyRig();
  const idleFrame = idle.advance(idleDrives(), 33);
  const restingTips = tips(idleFrame);

  // A second idle frame, clock still moving: nothing about the legs should have changed, because
  // both forward and backward are zero (idle: no gait motion — the design's "subtle breathing
  // only", applied to the limbs).
  const stillFrame = idle.advance(idleDrives(), 66);
  assert.deepEqual(tips(stillFrame), restingTips, 'a resting fly should not shuffle its feet');
});

test('stride asymmetry follows differential steering, and vanishes when steering is even', () => {
  const straight = new FlyRig();
  const turning = new FlyRig();
  const straightDrives: FlyDrives = { ...idleDrives(), forward: 1 };
  const turningDrives: FlyDrives = { ...idleDrives(), forward: 1, steerRight: 1 };

  // Track each leg's peak-to-peak swing along z (the fore-aft stride axis) over a full gait cycle.
  const straightRange = [0, 0, 0, 0, 0, 0].map(() => ({ min: Infinity, max: -Infinity }));
  const turningRange = [0, 0, 0, 0, 0, 0].map(() => ({ min: Infinity, max: -Infinity }));

  let now = 0;
  for (let i = 0; i < 60; i++) {
    now += 33;
    const straightTips = tips(straight.advance(straightDrives, now));
    const turningTips = tips(turning.advance(turningDrives, now));
    for (let leg = 0; leg < 6; leg++) {
      const sz = straightTips[leg]?.[2] as number;
      const sRange = straightRange[leg] as { min: number; max: number };
      sRange.min = Math.min(sRange.min, sz);
      sRange.max = Math.max(sRange.max, sz);

      const tz = turningTips[leg]?.[2] as number;
      const tRange = turningRange[leg] as { min: number; max: number };
      tRange.min = Math.min(tRange.min, tz);
      tRange.max = Math.max(tRange.max, tz);
    }
  }

  const swing = (range: { min: number; max: number }[]) => range.map((r) => r.max - r.min);
  const straightSwing = swing(straightRange);
  const turningSwing = swing(turningRange);

  // Walking straight: every leg on a side should swing about the same as its mirror.
  const straightLeftRight = [0, 1].map((pair) => Math.abs((straightSwing[pair] ?? 0) - (straightSwing[pair + 1] ?? 0)));
  for (const delta of straightLeftRight) {
    assert.ok(delta < 0.01, `walking straight should not already be asymmetric (delta ${delta.toFixed(4)})`);
  }

  // Turning: left legs (even index) and right legs (odd index) should now swing by visibly
  // different amounts — the outer side taking the longer stride.
  let asymmetric = 0;
  for (let pair = 0; pair < 3; pair++) {
    const left = turningSwing[pair * 2] ?? 0;
    const right = turningSwing[pair * 2 + 1] ?? 0;
    if (Math.abs(left - right) > 0.02) asymmetric++;
  }
  assert.ok(asymmetric > 0, 'differential steering should make at least one leg pair stride asymmetrically');
});
