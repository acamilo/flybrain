/**
 * The label table must cover the dataset, not the other way round.
 *
 * `src/lib/labels.ts` is the only place human copy for a circuit lives, and the authority for
 * which circuits exist is the dataset's own `meta.json` plus its `circuit-roles.json` sidecar. If
 * a dataset rebuild adds or renames a role, this test fails rather than the page silently
 * dropping a bar — which is the failure the design asks for explicitly.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { BAR_ROLES, CIRCUIT_GROUPS, DATASET_COPY, ROLE_LABELS, ROTATING_CARDS } from '../../src/lib/labels';

const here = dirname(fileURLToPath(import.meta.url));
const datasetDir = resolve(here, '../../../../data/fafb-v783');

interface Meta {
  neurons: number;
  roles: Record<string, number[]>;
}

const meta = JSON.parse(readFileSync(resolve(datasetDir, 'meta.json'), 'utf8')) as Meta;
const circuits = JSON.parse(readFileSync(resolve(datasetDir, 'circuit-roles.json'), 'utf8')) as {
  roles: Record<string, number[]>;
};

test('every role in meta.json has a label', () => {
  const missing = Object.keys(meta.roles).filter((role) => !(role in ROLE_LABELS));
  assert.deepEqual(missing, [], `roles without a label: ${missing.join(', ')}`);
});

test('every role in circuit-roles.json has a label', () => {
  const missing = Object.keys(circuits.roles).filter((role) => !(role in ROLE_LABELS));
  assert.deepEqual(missing, [], `sidecar roles without a label: ${missing.join(', ')}`);
});

test('no label describes a role the dataset does not have', () => {
  const known = new Set([...Object.keys(meta.roles), ...Object.keys(circuits.roles)]);
  const extra = Object.keys(ROLE_LABELS).filter((role) => !known.has(role));
  assert.deepEqual(extra, [], `labels for roles that do not exist: ${extra.join(', ')}`);
});

test('the circuit panel has six groups and every bar reads a real role', () => {
  // Design A2 fixes the count at six; the pitch of the panel depends on it.
  assert.equal(CIRCUIT_GROUPS.length, 6);
  for (const role of BAR_ROLES) {
    assert.ok(role in meta.roles, `bar role ${role} is not in the dataset`);
    assert.ok(role in ROLE_LABELS, `bar role ${role} has no label`);
  }
});

test('bar roles are unique and every group has at least one bar', () => {
  assert.equal(new Set(BAR_ROLES).size, BAR_ROLES.length, 'a role is bound to two bars');
  for (const group of CIRCUIT_GROUPS) {
    assert.ok(group.bars.length > 0, `group ${group.id} has no bars`);
    assert.ok(group.fullScaleHz > 0, `group ${group.id} has no full scale`);
  }
});

test('the legs group is present, and the card is where its honest line lives', () => {
  const legs = CIRCUIT_GROUPS.find((group) => group.id === 'legs');
  assert.ok(legs, 'the legs group is missing');

  // The row is just LEGS now (the copy direction: no parentheticals, no captions), so the claim
  // that they drive nothing has to be somewhere. It is in the scaffolding card.
  const scaffolding = ROTATING_CARDS.find((card) => card.title === 'scaffolding');
  assert.ok(scaffolding, 'there is no scaffolding card');
  assert.match(scaffolding.line, /legs/i, 'the scaffolding card no longer mentions the legs');

  // And that claim is only honest because these roles really are tiny.
  for (const bar of legs.bars) {
    const count = meta.roles[bar.role]?.length ?? 0;
    assert.ok(count > 0 && count <= 8, `${bar.role} has ${count} neurons, which breaks the claim`);
  }
});

test('nothing in the dataset-level copy names a game', () => {
  // The game-agnostic rule: no game vocabulary outside src/games.
  const corpus = JSON.stringify([ROLE_LABELS, CIRCUIT_GROUPS, ROTATING_CARDS, DATASET_COPY]).toLowerCase();
  for (const word of ['pokemon', 'pokémon', 'pokedex', 'pokédex', 'badge', 'gym', 'mario', 'platformer']) {
    assert.ok(!corpus.includes(word), `dataset-level copy mentions "${word}"`);
  }
});

test('the rotating card covers the four things the page has to say', () => {
  // Real, scaffolding, sugar, credit — one short line each. This is the *only* place on the page
  // allowed to explain anything (the copy pass), so what is in it is load-bearing.
  assert.deepEqual(
    ROTATING_CARDS.map((card) => card.title),
    ['real', 'scaffolding', 'sugar', 'credit'],
  );
  for (const card of ROTATING_CARDS) {
    assert.ok(card.line.length > 0, `${card.title} has no line`);
    assert.ok(card.line.length <= 100, `${card.title} is ${card.line.length} chars, which is not one short line`);
  }

  const credit = ROTATING_CARDS.find((card) => card.title === 'credit');
  assert.match(credit?.line ?? '', /FlyWire/, 'the credit card must name FlyWire');
  assert.match(credit?.line ?? '', /CC BY-NC/, 'the credit card must carry the licence');
});

test('no on-screen string is a sentence of explanation', () => {
  // The register, as a test: panel titles and readout labels are short, and none of them is
  // punctuated prose. The rotating card is exempt — it is the designated place for sentences.
  const onScreen = [
    ...Object.values(DATASET_COPY),
    ...CIRCUIT_GROUPS.map((group) => group.label),
    ...Object.values(ROLE_LABELS).map((role) => role.label),
  ];
  for (const text of onScreen) {
    assert.ok(text.length <= 24, `"${text}" is too long for a label`);
    assert.ok(!/[.!]/.test(text), `"${text}" reads as a sentence`);
    assert.ok(!/\(/.test(text), `"${text}" has a parenthetical`);
  }
});
