/**
 * `src/paint/retina.ts`'s `fillScale`: the fix for the squashed-eye bug (2026-09-16, the operator: "the
 * retina seems squashed" — both eyes rendered as tall narrow leaves).
 *
 * The investigation started from the hypothesis that `visual-xy.binz`'s x,y were an axial hex
 * lattice needing a hex-to-Cartesian transform. Checked directly against the committed dataset —
 * every point's full radius-2 integer neighbourhood is populated, including `(1,1)`/`(-1,-1)`,
 * which a real six-neighbour hex lattice never has alongside the true axial pair `(1,-1)`/`(-1,1)`
 * — that hypothesis is wrong: it is a plain rectangular index grid, just one where x (18 unique
 * steps on the committed dataset) and y (60) are quantised at different grains with no shared
 * physical unit. Preserving their raw ratio (`src/lib/fit.ts`'s shared letterboxed fit, built for
 * the connectome map's one calibrated space) faithfully reproduces the squash rather than fixing
 * it, so the real fix is independent per-axis scaling: each eye's x and y stretched to its own
 * half of the panel separately, with no shared factor and no letterbox margin.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { fillScale } from '../../src/paint/retina';

test('each axis is stretched to the box independently, whatever the source aspect', () => {
  // A source cloud 17 wide by 59 tall (the committed dataset's measured eye span) into a
  // 260x316 half-cell: two different scale factors, not one shared "tighter axis" pick.
  const fit = fillScale(17, 59, 260, 316);
  assert.ok(Math.abs(fit.scaleX - 260 / 17) < 1e-9);
  assert.ok(Math.abs(fit.scaleY - 316 / 59) < 1e-9);
  // The whole point: these are not equal, because a shared scale is exactly the bug.
  assert.notEqual(fit.scaleX, fit.scaleY);
});

test('a stretched cloud exactly fills its box on both axes, no letterbox margin', () => {
  const spanX = 17;
  const spanY = 59;
  const boxW = 260;
  const boxH = 316;
  const fit = fillScale(spanX, spanY, boxW, boxH);
  assert.ok(Math.abs(spanX * fit.scaleX - boxW) < 1e-9, 'x should fill the box width exactly');
  assert.ok(Math.abs(spanY * fit.scaleY - boxH) < 1e-9, 'y should fill the box height exactly');
});

test('a square source cloud into a square box uses one equal scale on both axes', () => {
  const fit = fillScale(40, 40, 200, 200);
  assert.equal(fit.scaleX, fit.scaleY);
});

test('a zero span (a single column) does not divide by zero', () => {
  const fit = fillScale(0, 59, 260, 316);
  assert.ok(Number.isFinite(fit.scaleX));
  assert.ok(fit.scaleX > 0);
});
