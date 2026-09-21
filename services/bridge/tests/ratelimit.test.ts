import assert from 'node:assert/strict';
import test from 'node:test';
import { FakeClock, RateLimiter } from '../src/ratelimit';
import type { RateLimitSettings } from '../src/config';

function settings(overrides: Partial<RateLimitSettings> = {}): RateLimitSettings {
  return {
    perUserPerCommandMs: 60_000,
    globalReplyMs: 5_000,
    chatMessagesPer30s: 15,
    perCommandCooldownMs: { sugar: 10_000 },
    ...overrides,
  };
}

void test('allows the first call for a user/command', () => {
  const limiter = new RateLimiter(settings(), new FakeClock());
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
});

void test('blocks the same user repeating the same command inside the per-user window', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
  clock.advance(1_000);
  const second = limiter.tryConsume('u1', 'fly');
  assert.equal(second.allowed, false);
  if (!second.allowed) {
    assert.equal(second.reason, 'user');
    assert.equal(second.retryAfterMs, 59_000);
  }
});

void test('allows the same user again after the per-user window elapses', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
  clock.advance(60_000);
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
});

void test('a different user is not blocked by another user, but shares the global reply bucket', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
  clock.advance(1_000);
  const second = limiter.tryConsume('u2', 'fly');
  assert.equal(second.allowed, false);
  if (!second.allowed) assert.equal(second.reason, 'global');
});

void test('global reply bucket clears after globalReplyMs', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
  clock.advance(5_000);
  assert.deepEqual(limiter.tryConsume('u2', 'brain'), { allowed: true });
});

void test('per-command global cooldown blocks a different user invoking the same command', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'sugar'), { allowed: true });
  clock.advance(5_000); // past the 5s global reply bucket, before the 10s sugar cooldown
  const second = limiter.tryConsume('u2', 'sugar');
  assert.equal(second.allowed, false);
  if (!second.allowed) {
    assert.equal(second.reason, 'cooldown');
    assert.equal(second.retryAfterMs, 5_000);
  }
});

void test('commands without a configured cooldown are unaffected by the cooldown check', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings({ perCommandCooldownMs: {} }), clock);
  assert.deepEqual(limiter.tryConsume('u1', 'sugar'), { allowed: true });
  clock.advance(5_000);
  assert.deepEqual(limiter.tryConsume('u2', 'sugar'), { allowed: true });
});

void test('check() does not consume the bucket', () => {
  const clock = new FakeClock();
  const limiter = new RateLimiter(settings(), clock);
  assert.deepEqual(limiter.check('u1', 'fly'), { allowed: true });
  assert.deepEqual(limiter.check('u1', 'fly'), { allowed: true });
  assert.deepEqual(limiter.tryConsume('u1', 'fly'), { allowed: true });
  clock.advance(1_000);
  const blocked = limiter.check('u1', 'fly');
  assert.equal(blocked.allowed, false);
});
