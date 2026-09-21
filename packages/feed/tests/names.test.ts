import assert from 'node:assert/strict';
import test from 'node:test';
import { FALLBACK_DISPLAY_NAME, validateDisplayName } from '../src/names';

void test('validateDisplayName accepts letters, digits and underscore up to 25 chars', () => {
  assert.equal(validateDisplayName('fly_fan_42'), 'fly_fan_42');
  assert.equal(validateDisplayName('a'), 'a');
  assert.equal(validateDisplayName('A1_2b3'), 'A1_2b3');
  assert.equal(validateDisplayName('a'.repeat(25)), 'a'.repeat(25));
});

void test('validateDisplayName accepts unicode letters and digits (\\p{L}\\p{N})', () => {
  assert.equal(validateDisplayName('Étoile'), 'Étoile');
  assert.equal(validateDisplayName('雨宮'), '雨宮');
  assert.equal(validateDisplayName('Ω'), 'Ω');
});

void test('validateDisplayName falls back on names that are too long', () => {
  assert.equal(validateDisplayName('a'.repeat(26)), FALLBACK_DISPLAY_NAME);
});

void test('validateDisplayName falls back on empty string', () => {
  assert.equal(validateDisplayName(''), FALLBACK_DISPLAY_NAME);
});

void test('validateDisplayName falls back on null/undefined', () => {
  assert.equal(validateDisplayName(null), FALLBACK_DISPLAY_NAME);
  assert.equal(validateDisplayName(undefined), FALLBACK_DISPLAY_NAME);
});

void test('validateDisplayName falls back on hostile / disallowed characters', () => {
  const hostile = [
    'foo bar', // space
    'foo\nbar', // newline
    '<script>alert(1)</script>',
    'foo\tbar', // tab
    'foo-bar', // hyphen not allowed
    'foo.bar',
    'foo@bar',
    '"; DROP TABLE viewers; --',
    'foo/bar',
    'foo\\bar',
    '👍viewer',
    '한국어 이름 공백', // spaces
  ];
  for (const name of hostile) {
    assert.equal(validateDisplayName(name), FALLBACK_DISPLAY_NAME, `expected fallback for ${JSON.stringify(name)}`);
  }
});

void test('validateDisplayName never throws on weird input types cast through null/undefined path', () => {
  // @ts-expect-error -- exercising the runtime guard for non-string input from untyped callers
  assert.equal(validateDisplayName(12345), FALLBACK_DISPLAY_NAME);
});
