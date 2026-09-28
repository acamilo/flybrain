import assert from 'node:assert/strict';
import test from 'node:test';
import { redactSecrets, safeErrorText } from '../src/redact';

void test('redactSecrets strips OAuth secrets from a twurple HttpStatusCodeError-shaped message', () => {
  const message =
    'Encountered HTTP status code 400: Bad Request\n\nURL: token?grant_type=refresh_token&client_id=abc' +
    '&client_secret=s3cr3t&refresh_token=r3fr3sh\nMethod: POST';
  const out = redactSecrets(message);
  assert.doesNotMatch(out, /s3cr3t|r3fr3sh/);
  assert.match(out, /client_secret=<redacted>&refresh_token=<redacted>/);
  assert.match(out, /client_id=abc/, 'the client id is not a secret and stays readable');
});

void test('redactSecrets strips Authorization header values', () => {
  assert.equal(redactSecrets('Authorization: OAuth abcdef0123456789'), 'Authorization: OAuth <redacted>');
});

void test('safeErrorText redacts the cause chain too', () => {
  const error = new Error('outer', { cause: new Error('URL: token?client_secret=s3cr3t') });
  assert.doesNotMatch(safeErrorText(error), /s3cr3t/);
});
