import assert from 'node:assert/strict';
import test from 'node:test';
import { classifyByRoles, normalizePositions } from '../src/view/layout';

/** Maximum distance of any point from the origin, over a flat xyz triple array. */
function maxRadius(points: Float32Array): number {
  let radius = 0;
  for (let i = 0; i < points.length; i += 3) radius = Math.max(radius, Math.hypot(points[i], points[i + 1], points[i + 2]));
  return radius;
}

/** Mean of one component (0 = x, 1 = y, 2 = z) over a flat xyz triple array. */
function mean(points: Float32Array, component: number): number {
  let total = 0;
  for (let i = component; i < points.length; i += 3) total += points[i];
  return total / (points.length / 3);
}

test('normalizePositions centers the cloud, scales the max radius to 1.3 and flattens z', () => {
  const source = new Float32Array([
    1000, 2000, -500,
    1400, 2000, -500,
    1200, 2600, -500,
    1200, 1400, -500,
  ]);
  const normalized = normalizePositions(source);
  assert.equal(normalized.length, source.length);
  assert.ok(Math.abs(mean(normalized, 0)) < 1e-6, `x mean ${mean(normalized, 0)}`);
  assert.ok(Math.abs(mean(normalized, 1)) < 1e-6, `y mean ${mean(normalized, 1)}`);
  assert.ok(Math.abs(maxRadius(normalized) - 1.3) < 1e-5, `max radius ${maxRadius(normalized)}`);
  for (let i = 2; i < normalized.length; i += 3) assert.equal(normalized[i], 0);
});

test('normalizePositions scales by the 3D radius and only then flattens z', () => {
  // Centroid (1.5, 0, 2), both points at 3D radius 2.5, so the scale is 1.3 / 2.5 = 0.52 and the
  // flattened points land at x = +-1.5 * 0.52 = +-0.78 -- inside 1.3, because depth is discarded.
  const normalized = normalizePositions(new Float32Array([0, 0, 0, 3, 0, 4]));
  assert.ok(Math.abs(normalized[0] + 0.78) < 1e-6, `x0 ${normalized[0]}`);
  assert.ok(Math.abs(normalized[3] - 0.78) < 1e-6, `x1 ${normalized[3]}`);
  assert.equal(normalized[2], 0);
  assert.equal(normalized[5], 0);
});

test('normalizePositions preserves relative geometry up to the uniform scale', () => {
  // The centroid is already the origin here, so the scale is exactly 1.3 / hypot(2, 4).
  const source = new Float32Array([0, 0, 0, 2, 0, 0, 0, 4, 0, -2, -4, 0]);
  const scale = 1.3 / Math.hypot(2, 4);
  const normalized = normalizePositions(source);
  for (let i = 0; i < source.length; i += 3) {
    assert.ok(Math.abs(normalized[i] - source[i] * scale) < 1e-6, `x at ${i}: ${normalized[i]}`);
    assert.ok(Math.abs(normalized[i + 1] - source[i + 1] * scale) < 1e-6, `y at ${i}: ${normalized[i + 1]}`);
  }
  assert.ok(Math.abs(maxRadius(normalized) - 1.3) < 1e-6);
});

test('classifyByRoles labels sensory 0, output 2 and everything else 1', () => {
  const roles = {
    sensory: [0, 1],
    motor: [4],
    descending: [5, 6],
    kenyon: [2, 3],
  };
  const classes = classifyByRoles(roles, 8);
  assert.ok(classes instanceof Uint8Array);
  assert.deepEqual(Array.from(classes), [0, 0, 1, 1, 2, 2, 2, 1]);
});

test('classifyByRoles applies the output roles last, so an overlap is drawn as output', () => {
  const classes = classifyByRoles({ sensory: [0, 1], motor: [1] }, 3);
  assert.deepEqual(Array.from(classes), [0, 2, 1]);
});

test('classifyByRoles honours custom role names and tolerates missing roles', () => {
  const roles = { photoreceptor: [0], command_0: [2], sensory: [1], motor: [1] };
  const classes = classifyByRoles(roles, 4, { sensory: ['photoreceptor'], output: ['command_0', 'absent'] });
  assert.deepEqual(Array.from(classes), [0, 1, 2, 1]);
  assert.deepEqual(Array.from(classifyByRoles({}, 3)), [1, 1, 1]);
});
