/**
 * The stall meter and the rollback budget.
 *
 * Both are derived on the page rather than carried by the feed, and both are readouts a viewer is
 * meant to watch fill — so the arithmetic has to be right at the edges: before the first event,
 * across a rank-up, and past the end of the window.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ATTEMPTS_PER_RUNG,
  LIFETIME_BUDGET,
  RollbackCounter,
  STALL_WINDOW_SECONDS,
  StallMeter,
  isExploreEvent,
} from '../../src/lib/stall';

test('the window is the one docs/design/ladder.md fixes', () => {
  assert.equal(STALL_WINDOW_SECONDS, 120);
  assert.equal(ATTEMPTS_PER_RUNG, 3);
  assert.equal(LIFETIME_BUDGET, 36);
});

test('exploration is what resets the window, and only exploration', () => {
  assert.equal(isExploreEvent({ kind: 'reward', rewardKind: 'explore' }), true);
  assert.equal(isExploreEvent({ kind: 'reward', rewardKind: 'area' }), true);
  // A badge is progress, not exploration: the fly can win a gym without finding a new room, and
  // the stall meter is about "is it still looking".
  assert.equal(isExploreEvent({ kind: 'reward', rewardKind: 'badge' }), false);
  assert.equal(isExploreEvent({ kind: 'reward', rewardKind: 'wildwin' }), false);
  assert.equal(isExploreEvent({ kind: 'sugar' }), false);
  assert.equal(isExploreEvent({ kind: 'reward' }), false, 'a reward with no kind is not exploration');
});

test('it measures from the first snapshot, not from page load', () => {
  // A page open for ten minutes with no feed has not watched the fly stall for ten minutes.
  const stall = new StallMeter();
  assert.equal(stall.secondsSince(600_000), 0, 'unarmed');

  stall.arm(600_000);
  assert.equal(stall.secondsSince(600_000), 0);
  assert.equal(stall.secondsSince(630_000), 30);
  // Arming twice does not restart it: every snapshot calls `arm`.
  stall.arm(630_000);
  assert.equal(stall.secondsSince(630_000), 30);
});

test('the fraction fills over 120 s and then saturates', () => {
  const stall = new StallMeter();
  stall.arm(0);
  assert.equal(stall.fraction(0), 0);
  assert.equal(stall.fraction(60_000), 0.5);
  assert.equal(stall.fraction(120_000), 1);
  assert.equal(stall.fraction(600_000), 1, 'a stall does not get worse than stalled');
  assert.equal(stall.stalled(119_999), false);
  assert.equal(stall.stalled(120_000), true);
});

test('an exploration event puts the window back to zero', () => {
  const stall = new StallMeter();
  stall.arm(0);
  assert.equal(stall.fraction(90_000), 0.75);
  stall.noteExplore(90_000);
  assert.equal(stall.fraction(90_000), 0);
  assert.equal(stall.fraction(120_000), 0.25);
});

test('a reset forgets everything, which is what a fixture seek needs', () => {
  const stall = new StallMeter();
  stall.arm(0);
  stall.noteExplore(10_000);
  stall.reset();
  assert.equal(stall.secondsSince(50_000), 0, 'nothing happened yet');
  stall.arm(50_000);
  assert.equal(stall.fraction(110_000), 0.5);
});

test('lifetime rollbacks count what this page has seen, and say when', () => {
  const rollbacks = new RollbackCounter();
  assert.equal(rollbacks.sinceLoad, 0);
  assert.equal(rollbacks.secondsSinceLast(1000), null, 'no rollback is not "0 s ago"');

  rollbacks.note(1000);
  assert.equal(rollbacks.sinceLoad, 1);
  assert.equal(rollbacks.secondsSinceLast(61_000), 60);

  rollbacks.note(61_000);
  assert.equal(rollbacks.sinceLoad, 2);
  assert.equal(rollbacks.secondsSinceLast(61_000), 0);

  rollbacks.reset();
  assert.equal(rollbacks.sinceLoad, 0);
  assert.equal(rollbacks.secondsSinceLast(61_000), null);
});

test('a clock that moves backwards never produces a negative readout', () => {
  // A fixture seek freezes and rewinds the page's clock on purpose (`src/feed/fixture.ts`), and
  // "-4 s ago" on a broadcast is worse than a stale number.
  const stall = new StallMeter();
  stall.arm(10_000);
  assert.equal(stall.secondsSince(5000), 0);

  const rollbacks = new RollbackCounter();
  rollbacks.note(10_000);
  assert.equal(rollbacks.secondsSinceLast(5000), 0);
});
