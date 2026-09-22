import assert from 'node:assert/strict';
import test from 'node:test';

import * as fixtures from '../src/fixtures';
import * as seed from '../src/seed';

test('every vector derives its recorded seed', () => {
  const file = fixtures.load('seed-vectors.json');
  assert.equal((file as Record<string, unknown>).algorithm, seed.ALGORITHM);
  const vectors = fixtures.section(file, 'vectors');
  for (const item of vectors) {
    const master = seed.masterSeed(fixtures.field(item, 'masterSeed'));
    const agentId = fixtures.field(item, 'agentId');
    assert.equal(
      Buffer.from(seed.material(master, agentId)).toString('utf8'),
      fixtures.field(item, 'material'),
      'the hashed material is part of the specification',
    );
    assert.equal(seed.materialDigest(master, agentId), fixtures.field(item, 'materialDigest'));
    assert.equal(
      seed.agentSeed(master, agentId),
      (item as Record<string, unknown>).seed,
      `seed for ${agentId} under master ${master}`,
    );
  }
  assert.ok(vectors.length >= 20, 'keep the vector table broad');
});

test('one composition gets independent seeds', () => {
  const file = fixtures.load('seed-vectors.json');
  const composition = fixtures.member(file, 'composition');
  const master = seed.masterSeed(fixtures.field(composition, 'masterSeed'));
  const ids = fixtures.section(composition, 'agentIds') as string[];
  const seeds = seed.compositionSeeds(master, ids);
  assert.deepEqual(seeds, fixtures.section(composition, 'seeds'));
  assert.equal(new Set(seeds).size, seeds.length, 'per-agent seeds are independent');
  assert.ok(
    seeds.every((value) => value !== 0),
    'a zero seed would stall an xorshift generator',
  );
});

test('a different master seed or agent id derives a different seed', () => {
  assert.notEqual(seed.agentSeed(0n, 'fly-a'), seed.agentSeed(1n, 'fly-a'));
  assert.notEqual(seed.agentSeed(0n, 'fly-a'), seed.agentSeed(0n, 'fly-b'));
  assert.equal(seed.agentSeed(7n, 'fly-a'), seed.agentSeed(7n, 'fly-a'));
});

test('invalid inputs are refused rather than normalized', () => {
  const file = fixtures.load('seed-vectors.json');
  for (const item of fixtures.section(file, 'invalid')) {
    const master = seed.masterSeed(fixtures.field(item, 'masterSeed'));
    const agentId = fixtures.optionalField(item, 'agentId');
    if (agentId !== undefined) {
      assert.throws(() => seed.agentSeed(master, agentId), `${agentId} must be refused`);
    } else {
      const ids = fixtures.section(item, 'agentIds') as string[];
      assert.throws(() => seed.compositionSeeds(master, ids));
    }
  }
});
