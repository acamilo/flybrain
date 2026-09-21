/**
 * The tab controller: what the slot shows, and why.
 *
 * Every rule in `docs/stream-mvp-plan.md`'s locked rail — "three tabs auto-cycling … slow cadence
 * steered by activity, big moments pre-empt for 9 s" — is a case here. The controller is pure and
 * clock-injected precisely so that this file can drive four minutes of broadcast in a loop with no
 * timers and no DOM.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CYCLE_MAX_MS,
  CYCLE_MIN_MS,
  STEER_DWELL_MS,
  TABS,
  TabController,
  WALKING_ACTIVITY,
  parseTab,
} from '../../src/lib/tabs';

const IDLE = { walking: false, commandActivity: 0, reward: false, rankChanged: false };

test('it starts where it was told and cycles in order', () => {
  const tabs = new TabController({ initial: 'senses' });
  assert.equal(tabs.update(0, IDLE), 'senses');
  assert.equal(tabs.reason, 'seed');

  // Nothing happens inside the dwell, whatever the dwell turned out to be.
  assert.equal(tabs.update(CYCLE_MIN_MS - 1, IDLE), 'senses');
  // And by the top of the band it has moved on, in rotation order.
  assert.equal(tabs.update(CYCLE_MAX_MS + 1, IDLE), 'connectome');
  assert.equal(tabs.reason, 'cycle');
  assert.equal(tabs.update(2 * CYCLE_MAX_MS + 2, IDLE), 'ladder');
  assert.equal(tabs.update(3 * CYCLE_MAX_MS + 3, IDLE), 'describe', 'DESCRIBE is fourth');
  assert.equal(tabs.update(4 * CYCLE_MAX_MS + 4, IDLE), 'macros', 'MACROS is fifth');
  assert.equal(tabs.update(5 * CYCLE_MAX_MS + 5, IDLE), 'senses', 'the rotation wraps');
});

test('the dwell is inside the locked 45 to 60 s band, every time', () => {
  // Seeded, so the cadence is reproducible; and it has to stay inside the band for a broadcast
  // that runs for weeks, not just on the first few cycles.
  const tabs = new TabController({ seed: 7 });
  let now = 0;
  let previous = tabs.update(now, IDLE);
  let changes = 0;
  let lastChangeAt = 0;

  for (let step = 0; step < 6000 && changes < 40; step++) {
    now += 100;
    const tab = tabs.update(now, IDLE);
    if (tab !== previous) {
      const dwell = now - lastChangeAt;
      assert.ok(dwell >= CYCLE_MIN_MS, `dwell ${dwell} under the 45 s floor`);
      assert.ok(dwell <= CYCLE_MAX_MS + 100, `dwell ${dwell} over the 60 s ceiling`);
      lastChangeAt = now;
      previous = tab;
      changes += 1;
    }
  }
  assert.ok(changes > 10, 'the slot barely cycled');
});

test('a rung change steers to LADDER and a reward to CONNECTOME', () => {
  const tabs = new TabController({ initial: 'senses' });
  tabs.update(0, IDLE);

  // Not immediately: a tab that has just arrived is not interrupted, or a reward every twenty
  // seconds would make the slot flicker.
  assert.equal(tabs.update(1000, { ...IDLE, reward: true }), 'senses');

  assert.equal(tabs.update(STEER_DWELL_MS + 1, { ...IDLE, reward: true }), 'connectome');
  assert.equal(tabs.reason, 'steer');

  // A rung change out-ranks a reward on the same frame: the ladder is what changed.
  const later = STEER_DWELL_MS * 2 + 2;
  assert.equal(tabs.update(later, { ...IDLE, reward: true, rankChanged: true }), 'ladder');
});

test('walking with high command activity biases the cycle toward SENSES, without owning it', () => {
  // The sustained signal is applied *at* a cycle boundary, not immediately: a condition that is
  // true for minutes at a time must bias the rotation rather than pin the slot.
  const walking = { walking: true, commandActivity: WALKING_ACTIVITY + 0.05, reward: false, rankChanged: false };
  const tabs = new TabController({ initial: 'connectome' });
  tabs.update(0, walking);
  assert.equal(tabs.update(1000, walking), 'connectome', 'the bias does not interrupt');

  assert.equal(tabs.update(CYCLE_MAX_MS + 1, walking), 'senses');
  // And it does not then hold SENSES for ever: the next cycle moves on.
  assert.equal(tabs.update(2 * CYCLE_MAX_MS + 2, walking), 'connectome');
});

test('activity below the threshold leaves the rotation alone', () => {
  // The bar sits near 1/headroom (about 0.67) at rest, so the threshold has to be above that or
  // "high activity" would mean "a fly that is alive".
  const resting = { walking: true, commandActivity: 0.67, reward: false, rankChanged: false };
  const tabs = new TabController({ initial: 'connectome' });
  tabs.update(0, resting);
  assert.equal(tabs.update(CYCLE_MAX_MS + 1, resting), 'ladder', 'the plain rotation, not the bias');
});

test('a moment takes the slot for its hold and hands it back to what was up', () => {
  const tabs = new TabController({ initial: 'senses' });
  tabs.update(0, IDLE);

  tabs.focusOn('ladder', 9000, 1000);
  assert.equal(tabs.current, 'ladder');
  assert.equal(tabs.reason, 'focus');
  assert.equal(tabs.focused, true);

  assert.equal(tabs.update(5000, { ...IDLE, reward: true }), 'ladder', 'a focus outranks a steer');
  assert.equal(tabs.update(8999, IDLE), 'ladder');

  assert.equal(tabs.update(9001, IDLE), 'senses', 'and it goes back');
  assert.equal(tabs.reason, 'return');
  assert.equal(tabs.focused, false);
});

test('a second focus during the first still returns to where the slot started', () => {
  // A badge interrupting a milestone must not leave the slot on LADDER for ever.
  const tabs = new TabController({ initial: 'connectome' });
  tabs.update(0, IDLE);
  tabs.focusOn('ladder', 9000, 1000);
  tabs.focusOn('ladder', 14_000, 6000);
  assert.equal(tabs.update(13_999, IDLE), 'ladder');
  assert.equal(tabs.update(14_001, IDLE), 'connectome');
});

test('a pinned tab ignores the clock, the steering and the moments', () => {
  // `?tab=` is what makes a screenshot of a cycling slot possible at all.
  const tabs = new TabController({ forced: 'connectome', initial: 'senses' });
  assert.equal(tabs.current, 'connectome');
  assert.equal(tabs.isForced, true);
  assert.equal(tabs.update(0, IDLE), 'connectome');
  assert.equal(tabs.update(10 * CYCLE_MAX_MS, { walking: true, commandActivity: 1, reward: true, rankChanged: true }), 'connectome');

  tabs.focusOn('ladder', 9000, 1000);
  assert.equal(tabs.update(2000, IDLE), 'connectome', 'not even a moment moves a pinned slot');
});

test('parseTab accepts the four tabs and nothing else', () => {
  for (const tab of TABS) assert.equal(parseTab(tab), tab);
  assert.equal(parseTab('SENSES'), null, 'case matters; the query value is lower case');
  assert.equal(parseTab('ladder '), null);
  assert.equal(parseTab(''), null);
  assert.equal(parseTab(null), null);
  assert.equal(parseTab(undefined), null);
  // A bad value must fall back to cycling rather than throw on a live broadcast.
  assert.equal(parseTab('../../etc/passwd'), null);
});
