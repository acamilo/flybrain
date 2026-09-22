import assert from 'node:assert/strict';
import test from 'node:test';

import * as fixtures from '../src/fixtures';
import { readArtifactRef } from '../src/reader';
import {
  artifactIdentity,
  isBusCallId,
  isDigest,
  isDomainRequestId,
  isId,
  ownerTokenKind,
  parseU64,
} from '../src/scalar';
import { readAssetRef } from '../src/workers';

test('U64 boundaries reject from the fixture', () => {
  const file = fixtures.load('boundaries.json');
  for (const item of fixtures.section(file, 'u64')) {
    const record = item as Record<string, unknown>;
    assert.equal(
      parseU64(record.text) !== undefined,
      record.accept,
      `${String(record.text)}: ${String(record.reason)}`,
    );
  }
});

test('the Id and Digest encodings are the ones the bus uses', () => {
  for (const id of ['a', 'fly-a', '0', 'a.b_c-d', 'a'.repeat(64)]) {
    assert.ok(isId(id), `${id} is an Id`);
  }
  for (const id of ['', 'A', '-a', '.a', 'a b', 'fly/a', 'a'.repeat(65)]) {
    assert.ok(!isId(id), `${id} is not an Id`);
  }
  assert.ok(isDigest('a'.repeat(64)));
  assert.ok(!isDigest('A'.repeat(64)), 'digests are lowercase');
  assert.ok(!isDigest('a'.repeat(63)), 'digests are 64 hex digits');
  assert.ok(!isDigest('g'.repeat(64)), 'digests are hexadecimal');
});

test('the four identities never accept each other spellings', () => {
  const file = fixtures.load('identities.json');
  for (const item of fixtures.cases(file)) {
    const record = item as Record<string, unknown>;
    const text = record.text as string;
    assert.equal(isBusCallId(text), record.busCallId, `busCallId ${text}`);
    assert.equal(isDomainRequestId(text), record.domainRequestId, `domainRequestId ${text}`);
    assert.equal(ownerTokenKind(text) ?? null, record.ownerToken, `owner token ${text}`);
    const accepted = [
      isBusCallId(text),
      isDomainRequestId(text),
      ownerTokenKind(text) !== undefined,
    ].filter(Boolean).length;
    assert.ok(accepted <= 1, `${text} is accepted by more than one identity type`);
  }
});

test('an artifact identity is the naming half of an ArtifactRef, and an asset is neither', () => {
  const file = fixtures.load('identities.json');
  const artifact = fixtures.member(file, 'artifact');
  const reference = readArtifactRef(fixtures.member(artifact, 'ref'));
  assert.deepEqual(artifactIdentity(reference), fixtures.member(artifact, 'identity'));
  const asset = readAssetRef(fixtures.member(file, 'asset'));
  assert.notEqual(
    asset.id,
    reference.artifactId,
    'the fixture asset and artifact are deliberately different things',
  );
});
