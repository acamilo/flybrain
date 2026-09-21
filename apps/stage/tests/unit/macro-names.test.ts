/**
 * The short-name table, against the contract's own list of types.
 *
 * Two things this has to hold, both of them about the *whole* list rather than about the five
 * names that change: every name a cell draws fits the nine characters the pad's 87 px column is
 * measured for (`src/lib/geometry.ts`), and no two types end up drawing the same string — a
 * shortening that collided would put two identical rows on the MACROS tab with no way to tell
 * which one the fly pressed.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { MACRO_TYPES } from '@flybrain/feed';
import { MACRO_SHORT_NAME_MAX } from '../../src/lib/geometry';
import { shortMacroName } from '../../src/lib/macro-names';

test('every macro name fits the cell once shortened', () => {
  const tooLong = MACRO_TYPES.map(shortMacroName).filter((name) => name.length > MACRO_SHORT_NAME_MAX);
  assert.deepEqual(tooLong, [], `names past ${MACRO_SHORT_NAME_MAX} characters: ${tooLong.join(', ')}`);
});

test('the five long names take the table entries and nothing else changes', () => {
  const changed = MACRO_TYPES.filter((name) => shortMacroName(name) !== name).map(
    (name) => `${name} -> ${shortMacroName(name)}`,
  );
  assert.deepEqual(changed, [
    'GO OBJECTIVE -> GO GOAL',
    'GO FRONTIER -> GO FRONT',
    'THROW BALL -> THROW',
    'BUY POTION -> BUY POTN',
    'BUY ANTIDOTE -> BUY ANTI',
  ]);
  // A name that fits is untouched, the same way it would be in the table.
  assert.equal(shortMacroName('GO ROUTE'), 'GO ROUTE');
  assert.equal(shortMacroName('CONFIRM'), 'CONFIRM');
});

test('no two types draw the same name', () => {
  const drawn = MACRO_TYPES.map(shortMacroName);
  assert.equal(new Set(drawn).size, drawn.length, 'two macro types would draw the same string');
});

test('a long name with no table entry is cut at the last word boundary inside the cap', () => {
  // The fallback for a producer or a contract ahead of this page. `tests/e2e/macros.spec.ts` says
  // the cell must not clip horizontally; this rule is what keeps that true.
  assert.equal(shortMacroName('SOMETHING LONGER'), 'SOMETHING');
});
