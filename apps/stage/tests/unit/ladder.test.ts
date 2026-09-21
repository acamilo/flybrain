import assert from 'node:assert/strict';
import test from 'node:test';

import { pokemonRed } from '../../src/games/pokemon-red';
import { platformer } from '../../src/games/platformer';
import { DEFAULT_RUNG_COUNT, ladderColumns, rungCount, rungLabels } from '../../src/lib/ladder';

test('the spine draws the rung count the feed reports', () => {
  // The whole point: the service's ladder grew from 16 rungs to 38 and the page follows
  // without a release, even though this game's config still lists 16 fallback labels.
  assert.equal(rungCount(38, pokemonRed.milestoneLadder.length), 38);
  assert.equal(rungCount(16, pokemonRed.milestoneLadder.length), 16);
  assert.equal(rungCount(1, pokemonRed.milestoneLadder.length), 1);
  assert.equal(rungCount(200, pokemonRed.milestoneLadder.length), 200);
});

test('a feed with no total falls back to the per-game ladder', () => {
  // A fixture recorded before `milestone.total` existed, or an older flysim, takes this path.
  // Pokemon Red's config lists all 38 rung *names* — the LADDER tab spells the ladder out and the
  // header carries only the current one — so its fallback is 38 rather than `DEFAULT_RUNG_COUNT`.
  assert.equal(rungCount(undefined, pokemonRed.milestoneLadder.length), 38);
  // And a different config keeps its own length rather than inheriting Pokemon Red's: Super Mario
  // Land's adapter ladder is 16 rungs (`docs/design/platformer.md` section 3).
  assert.equal(platformer.milestoneLadder.length, 16);
  assert.equal(rungCount(undefined, platformer.milestoneLadder.length), 16);
  assert.equal(rungCount(undefined, 8), 8, 'any config length is honoured as-is');
  // `DEFAULT_RUNG_COUNT` is the last resort, for a config with no ladder at all.
  assert.equal(rungCount(undefined, 0), DEFAULT_RUNG_COUNT);
});

test('the ladder tab spells out every rung the feed counts', () => {
  // The names are the config's and the count is the feed's, so the two have to be reconciled:
  // a service with more rungs than this build knows names for shows numbered blanks, and one with
  // fewer shows only as many as it has.
  assert.deepEqual(rungLabels(3, pokemonRed.milestoneLadder), ['Boot screen', 'Bedroom', 'Downstairs']);
  assert.equal(rungLabels(38, pokemonRed.milestoneLadder).length, 38);
  assert.equal(rungLabels(38, pokemonRed.milestoneLadder).at(-1), 'Champion');

  const padded = rungLabels(40, pokemonRed.milestoneLadder);
  assert.equal(padded.length, 40);
  assert.equal(padded[38], '', 'a rung the config has no name for is blank, not missing');

  // Index for index with the service's own ratchet: rung 11 is the first badge in both.
  assert.equal(pokemonRed.milestoneLadder[11], 'Boulder Badge');
  assert.equal(pokemonRed.milestoneLadder.length, 38);
});

test('the rungs are dealt into columns top to bottom, not left to right', () => {
  // A reader following 0, 1, 2 down the first column is following the fly's own path; row-major
  // would put rung 1 next to rung 14.
  const columns = ladderColumns([0, 1, 2, 3, 4, 5, 6, 7], 3);
  assert.deepEqual(columns, [[0, 1, 2], [3, 4, 5], [6, 7]]);

  const long = ladderColumns(Array.from({ length: 38 }, (_, index) => index), 3);
  assert.equal(long.length, 3);
  assert.equal(long[0]?.length, 13, 'ceil(38/3) rows per column, which is what the pane fits');
  assert.deepEqual(long[0]?.slice(0, 3), [0, 1, 2]);
  assert.equal(long[2]?.length, 12);

  // Degenerate inputs never produce an empty layout on air.
  assert.deepEqual(ladderColumns([], 3), [[], [], []]);
  assert.deepEqual(ladderColumns([1, 2], 0), [[1, 2]]);
});

test('a nonsense total from the wire never erases the spine', () => {
  // `total` sizes a render loop on a page that has to keep painting on air, so every value
  // that is not a positive integer falls through to the config, and then to a floor.
  for (const bad of [0, -1, -38, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(rungCount(bad, 8), 8, `total: ${bad}`);
  }
  for (const bad of [undefined, 0, Number.NaN]) {
    assert.equal(rungCount(bad, 0), DEFAULT_RUNG_COUNT, `ladder: 0, total: ${bad}`);
  }
  assert.ok(rungCount(undefined, 0) >= 1, 'the count is never zero');
});
