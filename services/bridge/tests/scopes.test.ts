import assert from 'node:assert/strict';
import test from 'node:test';
import { assertRequiredScopes, MissingScopesError, SCOPE_TABLE } from '../src/scopes';

const ALWAYS_ON_BOT_SCOPES = ['user:read:chat', 'user:write:chat', 'user:bot'];
const ALWAYS_ON_BROADCASTER_SCOPES = ['channel:bot', 'moderator:read:followers'];

void test('passes when all always-on B1 scopes are granted and both features are off', () => {
  assert.doesNotThrow(() =>
    assertRequiredScopes(
      { featureRedemptions: false, featurePredictions: false },
      { bot: ALWAYS_ON_BOT_SCOPES, broadcaster: ALWAYS_ON_BROADCASTER_SCOPES },
    ),
  );
});

void test('throws MissingScopesError naming every missing scope, not just the first', () => {
  assert.throws(
    () =>
      assertRequiredScopes(
        { featureRedemptions: false, featurePredictions: false },
        { bot: [], broadcaster: [] },
      ),
    (error: unknown) => {
      assert.ok(error instanceof MissingScopesError);
      const scopes = error.missing.map((m) => m.scope);
      assert.deepEqual(scopes.sort(), [...ALWAYS_ON_BOT_SCOPES, ...ALWAYS_ON_BROADCASTER_SCOPES].sort());
      return true;
    },
  );
});

void test('redemption scopes are only required when featureRedemptions is on', () => {
  assert.doesNotThrow(() =>
    assertRequiredScopes(
      { featureRedemptions: false, featurePredictions: false },
      { bot: ALWAYS_ON_BOT_SCOPES, broadcaster: ALWAYS_ON_BROADCASTER_SCOPES },
    ),
  );

  assert.throws(
    () =>
      assertRequiredScopes(
        { featureRedemptions: true, featurePredictions: false },
        { bot: ALWAYS_ON_BOT_SCOPES, broadcaster: ALWAYS_ON_BROADCASTER_SCOPES },
      ),
    (error: unknown) => {
      assert.ok(error instanceof MissingScopesError);
      const scopes = error.missing.map((m) => m.scope).sort();
      assert.deepEqual(scopes, ['channel:manage:broadcast', 'channel:manage:redemptions', 'channel:read:redemptions']);
      return true;
    },
  );
});

void test('prediction scope is only required when featurePredictions is on', () => {
  assert.throws(
    () =>
      assertRequiredScopes(
        { featureRedemptions: false, featurePredictions: true },
        { bot: ALWAYS_ON_BOT_SCOPES, broadcaster: ALWAYS_ON_BROADCASTER_SCOPES },
      ),
    (error: unknown) => {
      assert.ok(error instanceof MissingScopesError);
      assert.deepEqual(
        error.missing.map((m) => m.scope),
        ['channel:manage:predictions'],
      );
      return true;
    },
  );
});

void test('the error message names the token kind and what each missing scope is needed for', () => {
  try {
    assertRequiredScopes({ featureRedemptions: false, featurePredictions: false }, { bot: [], broadcaster: [] });
    assert.fail('expected assertRequiredScopes to throw');
  } catch (error) {
    assert.ok(error instanceof MissingScopesError);
    assert.match(error.message, /bot token missing "user:read:chat"/);
    assert.match(error.message, /channel\.chat\.message over EventSub WS/);
  }
});

void test('SCOPE_TABLE matches the B1 design doc table (every scope, correct token)', () => {
  const byScope = new Map(SCOPE_TABLE.map((req) => [req.scope, req.token]));
  assert.equal(byScope.get('user:read:chat'), 'bot');
  assert.equal(byScope.get('user:write:chat'), 'bot');
  assert.equal(byScope.get('user:bot'), 'bot');
  assert.equal(byScope.get('channel:bot'), 'broadcaster');
  assert.equal(byScope.get('channel:read:redemptions'), 'broadcaster');
  assert.equal(byScope.get('channel:manage:redemptions'), 'broadcaster');
  assert.equal(byScope.get('moderator:read:followers'), 'broadcaster');
  assert.equal(byScope.get('channel:manage:broadcast'), 'broadcaster');
  assert.equal(byScope.get('channel:manage:predictions'), 'broadcaster');
});
