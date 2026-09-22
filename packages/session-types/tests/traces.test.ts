import assert from 'node:assert/strict';
import test from 'node:test';

import * as fixtures from '../src/fixtures';
import {
  behaviourDiff,
  behaviourDigest,
  behaviourEquals,
  readTransitionTrace,
  runsEqual,
} from '../src/trace';

test('every variant compares the way the fixture says', () => {
  const file = fixtures.load('traces.json');
  const baseline = readTransitionTrace(fixtures.member(file, 'baseline'));
  for (const item of fixtures.section(file, 'variants')) {
    const name = fixtures.field(item, 'name');
    const variant = readTransitionTrace(fixtures.member(item, 'trace'));
    const expected = (item as Record<string, unknown>).behaviourEquals as boolean;
    const diff = behaviourDiff(baseline, variant);
    assert.equal(
      behaviourEquals(baseline, variant),
      expected,
      `${name}: behaviour equality. differences: ${JSON.stringify(diff)}`,
    );
    assert.equal(diff.length === 0, expected, `${name}: the diff is empty exactly when equal`);
    const needle = fixtures.optionalField(item, 'diffContains');
    if (needle !== undefined) {
      assert.ok(
        diff.some((line) => line.includes(needle)),
        `${name}: the diff should name ${needle}, got ${JSON.stringify(diff)}`,
      );
    }
    if (expected) {
      assert.equal(
        behaviourDigest(baseline.behaviour),
        behaviourDigest(variant.behaviour),
        `${name}: equal behaviour has one digest`,
      );
    }
  }
});

test('a whole run compares transition by transition', () => {
  const file = fixtures.load('traces.json');
  const baseline = readTransitionTrace(fixtures.member(file, 'baseline'));
  const variants = fixtures.section(file, 'variants');
  const reversed = readTransitionTrace(fixtures.member(variants[0] as never, 'trace'));
  const changed = readTransitionTrace(
    fixtures.member(
      variants.find((item) => fixtures.field(item, 'name') === 'one extra neural tick') as never,
      'trace',
    ),
  );
  assert.ok(runsEqual([baseline, baseline], [reversed, baseline]));
  assert.ok(!runsEqual([baseline], [changed]));
  assert.ok(!runsEqual([baseline], [baseline, baseline]));
});

test('operational metadata is recorded and excluded', () => {
  const file = fixtures.load('traces.json');
  const baseline = readTransitionTrace(fixtures.member(file, 'baseline'));
  assert.equal(baseline.operational.busCallIds.length, 3);
  assert.equal(baseline.operational.prepareRequestIds.length, 2);
  assert.equal(baseline.operational.deliveryIds.length, 2);
  const retried = readTransitionTrace(
    fixtures.member(
      fixtures
        .section(file, 'variants')
        .find(
          (item) =>
            fixtures.field(item, 'name') ===
            'a safe retry with fresh bus callIds, delivery ids and wall time',
        ) as never,
      'trace',
    ),
  );
  assert.notDeepEqual(baseline.operational, retried.operational);
  assert.ok(behaviourEquals(baseline, retried));
});
