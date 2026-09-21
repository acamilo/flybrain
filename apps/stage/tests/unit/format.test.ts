/**
 * Readout formatting. Boring on its own, load-bearing in aggregate: these strings are the
 * broadcast, and one of them is the chokepoint for viewer display names.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  dayNumber,
  formatClock,
  formatCooldown,
  formatCount,
  formatCounters,
  formatDuration,
  formatDurationCompact,
  formatHz,
  formatRealtime,
  formatReward,
  safeDisplayName,
} from '../../src/lib/format';
import { LAYOUT, MAP_GRID_HEIGHT, MAP_GRID_WIDTH, MAP_HERO_HEIGHT, MAP_HERO_WIDTH } from '../../src/lib/geometry';

test('durations are coarse above a minute and never ragged', () => {
  assert.equal(formatDuration(0), '0 s');
  assert.equal(formatDuration(48), '48 s');
  assert.equal(formatDuration(64), '1 m 04 s');
  assert.equal(formatDuration(3600), '1 h 00 m');
  assert.equal(formatDuration(13_260), '3 h 41 m');
  assert.equal(formatDuration(-1), '—');
  assert.equal(formatDuration(Number.NaN), '—');
});

test('the compact duration fits a hero box', () => {
  assert.equal(formatDurationCompact(95), '1m35s');
  assert.equal(formatDurationCompact(13_260), '3h41m');
  assert.equal(formatDurationCompact(2), '2s');
  // HERE FOR is Press Start 2P at 33 px — one em per character, so 33 px each — in a cluster
  // column whose floor is five characters and whose worst case the rung line has to afford. Six
  // characters is 198 px and is the ceiling this form must never exceed.
  for (const seconds of [0, 9, 59, 60, 599, 3599, 3600, 86_399, 359_999]) {
    assert.ok(formatDurationCompact(seconds).length <= 6, `${seconds} -> ${formatDurationCompact(seconds)}`);
  }
});

test('the clock is fixed width so it cannot reflow the panel', () => {
  assert.equal(formatClock(0), '00:00:00');
  assert.equal(formatClock(95), '00:01:35');
  assert.equal(formatClock(375_555), '104:19:15');
  assert.equal(formatClock(-5), '--:--:--');
});

test('day 1 is the first 24 hours', () => {
  assert.equal(dayNumber(0), 1);
  assert.equal(dayNumber(86_399), 1);
  assert.equal(dayNumber(86_400), 2);
  assert.equal(dayNumber(-1), 1);
});

test('rates, counts and realtime read as words a viewer can parse', () => {
  assert.equal(formatHz(13.24), '13.2');
  assert.equal(formatHz(Number.NaN), '—');
  assert.equal(formatCount(139_255), '139,255');
  assert.equal(formatRealtime(0.98), '0.98x');
});

test('rewards are always signed, with the tier visible in the precision', () => {
  assert.equal(formatReward(0.05), '+0.05');
  assert.equal(formatReward(3), '+3.0');
  assert.equal(formatReward(-0.2), '−0.20');
});

test('the ladder panel counters read as one terse line, denominator only when the config has one', () => {
  const counters = [
    { field: 'badges', label: 'badges', outOf: 8 },
    { field: 'uniqueLocations', label: 'places' },
  ];
  assert.equal(
    formatCounters(counters, { badges: 3, uniqueLocations: 214 }),
    '3/8 badges · 214 places',
  );
  assert.equal(formatCounters(counters, { badges: 0, uniqueLocations: 0 }), '0/8 badges · 0 places');
  assert.equal(formatCounters([], { badges: 3 }), '', 'no counters configured -> no line');
});

test('a cooldown never shows 0 while it is still blocking', () => {
  assert.equal(formatCooldown(0), 'ready');
  assert.equal(formatCooldown(1), '1 s');
  assert.equal(formatCooldown(4200), '5 s');
});

test('display names are the chokepoint: anything unexpected becomes "a viewer"', () => {
  assert.equal(safeDisplayName('alex'), 'alex');
  assert.equal(safeDisplayName('Fly_Fan_99'), 'Fly_Fan_99');
  assert.equal(safeDisplayName('日本語名'), '日本語名', 'the bridge allows any letter');

  assert.equal(safeDisplayName(''), 'a viewer');
  assert.equal(safeDisplayName(null), 'a viewer');
  assert.equal(safeDisplayName(undefined), 'a viewer');
  assert.equal(safeDisplayName('a'.repeat(26)), 'a viewer', 'over the 25 character limit');
  assert.equal(safeDisplayName('fixture_big-moment'), 'a viewer', 'a hyphen is not in the grammar');
  assert.equal(safeDisplayName('<script>alert(1)</script>'), 'a viewer');
  assert.equal(safeDisplayName('two words'), 'a viewer');
});

test('the brain map is drawn at exactly the size it is seen at', () => {
  // Layout v1 allocated the backing store for a promotion that animated the map over the whole
  // rail, so the inset and the hero had to share a uniform scale. Layout v2 makes the map a tab:
  // there is one size, it is the tab pane's, and a rescale per frame would be a bug rather than a
  // feature.
  assert.equal(MAP_HERO_WIDTH, LAYOUT.tabContent.width);
  assert.equal(MAP_HERO_HEIGHT, LAYOUT.tabContent.height);
  // 1004x368, not layout v1's 1008x370: the dialogue-box frame of
  // `docs/design/gameboy-theme.md` is 4 px rather than 2, and the pane is what is inside it.
  assert.equal(MAP_HERO_WIDTH, 1004);
  assert.equal(MAP_HERO_HEIGHT, 368);
});

test('the accumulator grid divides the canvas exactly, per axis', () => {
  // A fractional cell would put the sprite pass a subpixel off the cell it belongs to. The pane's
  // height is not a multiple of 4, so the two axes take different divisors rather than one shared.
  assert.equal(MAP_HERO_WIDTH % MAP_GRID_WIDTH, 0);
  assert.equal(MAP_HERO_HEIGHT % MAP_GRID_HEIGHT, 0);
  assert.equal(MAP_HERO_WIDTH / MAP_GRID_WIDTH, 4);
  assert.equal(MAP_HERO_HEIGHT / MAP_GRID_HEIGHT, 2);
});
