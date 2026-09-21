/**
 * `lib/fit.ts`: the one uniform, letterboxed scale the connectome map's base raster, LUT and
 * PAM centroid all share (`workers/brain-base.worker.ts`).
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { fitHalfExtent, project } from '../../src/lib/fit';

test('a wide canvas letterboxes a square extent left and right, filling the tight axis', () => {
  // The connectome pane: 1008x370, a square-ish point cloud (`normalizePositions` produces one).
  const halfExtent = fitHalfExtent(1.3, 1.3, 1008, 370);

  // Height is the binding axis: the halfExtent on y should sit just outside 1.3 (margin only),
  // while x's halfExtent is inflated well past 1.3 — the letterboxing on the wide axis.
  assert.ok(halfExtent.y > 1.3 && halfExtent.y < 1.35, `halfExtent.y ${halfExtent.y} should hug 1.3`);
  assert.ok(halfExtent.x > halfExtent.y, 'the wide axis is the one with slack');

  const top = project(0, 1.3, halfExtent, 1008, 370);
  const bottom = project(0, -1.3, halfExtent, 1008, 370);
  assert.ok(top && bottom, 'the extreme points that define the fit must land inside it');
  assert.ok((top as { y: number }).y < 10, 'the top extreme sits just inside the top edge');
  assert.ok((bottom as { y: number }).y > 360, 'the bottom extreme sits just inside the bottom edge');

  const left = project(-1.3, 0, halfExtent, 1008, 370);
  const right = project(1.3, 0, halfExtent, 1008, 370);
  assert.ok(left && right, 'the x extremes are still inside the fitted (letterboxed) box');
  // The letterboxed margins on the wide axis should be roughly symmetric and substantial.
  const leftMargin = (left as { x: number }).x;
  const rightMargin = 1007 - (right as { x: number }).x;
  assert.ok(leftMargin > 250 && rightMargin > 250, 'a square in a wide box leaves wide side margins');
});

test('a point outside the fitted box is dropped, never clamped', () => {
  const halfExtent = fitHalfExtent(1, 1, 100, 100);
  assert.equal(project(1.5, 0, halfExtent, 100, 100), null);
  assert.equal(project(0, -1.5, halfExtent, 100, 100), null);
  assert.equal(project(2, 2, halfExtent, 100, 100), null);
});

test('a square canvas fitting a square extent uses the same scale on both axes', () => {
  const halfExtent = fitHalfExtent(1.3, 0.4, 500, 500);
  // x is the binding axis (bigger extent, same canvas span), so x's halfExtent hugs 1.3 while y's
  // is inflated in exactly the same proportion — one scale, not two.
  const scaleFromX = 250 / halfExtent.x;
  const scaleFromY = 250 / halfExtent.y;
  assert.ok(Math.abs(scaleFromX - scaleFromY) < 1e-9, 'x and y must resolve to the same scale');
});
