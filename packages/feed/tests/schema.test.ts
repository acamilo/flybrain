import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import Ajv2020 from 'ajv/dist/2020.js';
import { FakeFlysim, type FakeScenario } from '../src/fake/simulator';

const here = fileURLToPath(new URL('.', import.meta.url));
const schema = JSON.parse(readFileSync(join(here, '../src/schema.json'), 'utf8')) as object;

const ajv = new Ajv2020({ strict: true });
const validate = ajv.compile(schema);

const SCENARIOS: FakeScenario[] = ['boot', 'running', 'stuck', 'milestone', 'shop', 'center'];
const SNAPSHOTS_PER_SCENARIO = 100;

for (const scenario of SCENARIOS) {
  test(`fake flysim (${scenario}) produces schema-valid FeedHeaders for ${SNAPSHOTS_PER_SCENARIO} snapshots`, () => {
    const sim = new FakeFlysim({ scenario, seed: 0xc0ffee });

    for (let i = 0; i < SNAPSHOTS_PER_SCENARIO; i++) {
      const { header } = sim.tick(33);
      const valid = validate(header);
      assert.ok(valid, `snapshot ${i} for scenario "${scenario}" failed schema validation: ${ajv.errorsText(validate.errors)}`);
    }
  });
}

test('schema rejects a header with an unknown property', () => {
  const sim = new FakeFlysim({ scenario: 'running', seed: 1 });
  const { header } = sim.tick(33);
  const invalid = { ...header, extraField: 'nope' };
  assert.equal(validate(invalid), false);
});

test('schema rejects a bad status enum value', () => {
  const sim = new FakeFlysim({ scenario: 'running', seed: 1 });
  const { header } = sim.tick(33);
  const invalid = { ...header, status: 'sleeping' };
  assert.equal(validate(invalid), false);
});

test('milestone.total is the ladder length, emitted as 38 and optional in the schema', () => {
  const sim = new FakeFlysim({ scenario: 'milestone', seed: 7 });
  const { header } = sim.tick(33);

  assert.equal(header.milestone.total, 38, 'the fake ladder is the real one\'s length');
  assert.ok(header.milestone.rank >= 0);
  assert.ok(
    header.milestone.rank < (header.milestone.total as number),
    'rank is 0..total-1',
  );

  // Optional, so a producer that predates it stays valid...
  const { total, ...withoutTotal } = header.milestone;
  assert.equal(validate({ ...header, milestone: withoutTotal }), true);

  // ...but when present it is an integer of at least 1, and `rank` no longer has
  // the old hard ceiling of 15, which the 38-rung ladder passes.
  for (const bad of [0, -1, 1.5, '38', null]) {
    assert.equal(
      validate({ ...header, milestone: { ...header.milestone, total: bad } }),
      false,
      `total: ${JSON.stringify(bad)}`,
    );
  }
  assert.equal(validate({ ...header, milestone: { ...header.milestone, rank: 37 } }), true);
});

test('the milestone scenario climbs the whole ladder and never reports a rank past it', () => {
  const sim = new FakeFlysim({ scenario: 'milestone', seed: 3 });
  let top = 0;
  // One rung per 90 simulated seconds, so 37 climbs need about an hour. Ticked at
  // 1 s rather than the wire's 33 ms to keep this under a second of real time; the
  // scenario steps at most one rung per tick, so anything below 90 s is exact.
  for (let i = 0; i < 4_000; i++) {
    const { header } = sim.tick(1_000);
    const { rank, total } = header.milestone;
    assert.ok(total !== undefined && rank < total, `rank ${rank} of ${total}`);
    top = Math.max(top, rank);
  }
  assert.equal(top, 37, 'the milestone scenario should reach the top rung');
});
