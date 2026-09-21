import assert from 'node:assert/strict';
import test from 'node:test';
import { createSend } from '../src/chat';
import {
  isChannelPointsAffiliateRefusal,
  MemoryIntentStore,
  RedemptionManager,
  type RedemptionApi,
  type RedemptionReward,
  type RedemptionStatus,
} from '../src/redemptions';
import { FakeChatSender, FakeSimClient } from './helpers';

class MockRedemptionApi implements RedemptionApi {
  rewards: RedemptionReward[] = [];
  createRewardCalls = 0;
  statusUpdates: { rewardId: string; redemptionIds: string[]; status: RedemptionStatus }[] = [];
  /** Number of times updateRedemptionStatus should throw before succeeding. 0 = never fails. */
  failStatusUpdatesRemaining = 0;

  async findRewardByTitle(title: string): Promise<RedemptionReward | null> {
    return this.rewards.find((r) => r.title === title) ?? null;
  }

  async createReward(title: string): Promise<RedemptionReward> {
    this.createRewardCalls += 1;
    const reward = { id: `reward-${this.createRewardCalls}`, title };
    this.rewards.push(reward);
    return reward;
  }

  async updateRedemptionStatus(rewardId: string, redemptionIds: string[], status: RedemptionStatus): Promise<void> {
    if (this.failStatusUpdatesRemaining > 0) {
      this.failStatusUpdatesRemaining -= 1;
      throw new Error('Twitch API unavailable');
    }
    this.statusUpdates.push({ rewardId, redemptionIds, status });
  }
}

function makeManager(overrides: { api?: MockRedemptionApi; sim?: FakeSimClient; sender?: FakeChatSender } = {}) {
  const api = overrides.api ?? new MockRedemptionApi();
  const sim = overrides.sim ?? new FakeSimClient();
  const sender = overrides.sender ?? new FakeChatSender();
  const store = new MemoryIntentStore();
  const errors: string[] = [];
  const manager = new RedemptionManager({
    sim,
    api,
    store,
    send: createSend(sender),
    rewardTitle: 'Sugar',
    logError: (message) => errors.push(message),
  });
  return { manager, api, sim, sender, store, errors };
}

void test('start() creates the Sugar reward when none exists', async () => {
  const { manager, api } = makeManager();
  const reward = await manager.start();
  assert.equal(reward.title, 'Sugar');
  assert.equal(api.createRewardCalls, 1);
});

void test('start() is idempotent: finds an existing reward by title instead of creating a duplicate', async () => {
  const api = new MockRedemptionApi();
  api.rewards.push({ id: 'existing-reward', title: 'Sugar' });
  const { manager } = makeManager({ api });
  const reward = await manager.start();
  assert.equal(reward.id, 'existing-reward');
  assert.equal(api.createRewardCalls, 0);
});

void test('a successful redemption calls sim.stimulate with source points and fulfils it', async () => {
  const { manager, api, sim, sender } = makeManager();
  await manager.start();
  const status = await manager.handleRedemptionAdd('redemption-1', manager.currentRewardId!, 'fly_fan_42');
  assert.equal(status, 'FULFILLED');
  assert.deepEqual(sim.stimulateCalls[0], { by: 'fly_fan_42', source: 'points' });
  assert.deepEqual(api.statusUpdates, [{ rewardId: manager.currentRewardId, redemptionIds: ['redemption-1'], status: 'FULFILLED' }]);
  assert.match(sender.sent[0]!, /fly_fan_42/);
});

void test('validates a hostile redeemer name before it reaches the sim', async () => {
  const { manager, sim } = makeManager();
  await manager.start();
  await manager.handleRedemptionAdd('redemption-1', manager.currentRewardId!, '<script>');
  assert.deepEqual(sim.stimulateCalls[0], { by: 'a viewer', source: 'points' });
});

for (const failure of [
  { name: '429 rate_limited', result: { ok: false as const, kind: 'rate_limited' as const, retryAfterMs: 1000 } },
  { name: '500 http_error', result: { ok: false as const, kind: 'http_error' as const, status: 500 } },
  { name: 'timeout', result: { ok: false as const, kind: 'timeout' as const } },
]) {
  void test(`sim ${failure.name} refunds the redemption (CANCELED)`, async () => {
    const { manager, api, sim, sender } = makeManager();
    sim.stimulateResult = failure.result;
    await manager.start();
    const status = await manager.handleRedemptionAdd('redemption-1', manager.currentRewardId!, 'alex');
    assert.equal(status, 'CANCELED');
    assert.deepEqual(api.statusUpdates, [
      { rewardId: manager.currentRewardId, redemptionIds: ['redemption-1'], status: 'CANCELED' },
    ]);
    assert.match(sender.sent[0]!, /turned off/);
  });
}

void test('retries a failing Twitch status update up to the bound, then succeeds', async () => {
  const api = new MockRedemptionApi();
  api.failStatusUpdatesRemaining = 2; // fails twice, succeeds on the 3rd attempt
  const { manager } = makeManager({ api });
  await manager.start();
  const status = await manager.handleRedemptionAdd('redemption-1', manager.currentRewardId!, 'alex');
  assert.equal(status, 'FULFILLED');
  assert.equal(api.statusUpdates.length, 1); // only the final, successful call is recorded
});

void test('never leaves a redemption pending: if every status-update attempt fails, it stays pending in the store and is logged', async () => {
  const api = new MockRedemptionApi();
  api.failStatusUpdatesRemaining = 100; // always fails
  const { manager, store, errors } = makeManager({ api });
  await manager.start();
  await manager.handleRedemptionAdd('redemption-1', manager.currentRewardId!, 'alex');

  const persisted = await store.load();
  const intent = persisted.find((i) => i.redemptionId === 'redemption-1');
  assert.ok(intent);
  assert.equal(intent.status, 'pending');
  assert.ok(errors.some((e) => e.includes('redemption-1')));
});

void test('startup replay: a pending intent left over from a crash is force-CANCELED so nothing stays pending', async () => {
  const api = new MockRedemptionApi();
  api.rewards.push({ id: 'reward-1', title: 'Sugar' });
  const sim = new FakeSimClient();
  const sender = new FakeChatSender();
  const store = new MemoryIntentStore();
  await store.save([
    { redemptionId: 'orphaned-1', rewardId: 'reward-1', displayName: 'alex', status: 'pending', createdAtMs: 0 },
  ]);

  const manager = new RedemptionManager({ sim, api, store, send: createSend(sender), rewardTitle: 'Sugar' });
  await manager.start();

  assert.deepEqual(api.statusUpdates, [{ rewardId: 'reward-1', redemptionIds: ['orphaned-1'], status: 'CANCELED' }]);
  const persisted = await store.load();
  assert.equal(persisted.find((i) => i.redemptionId === 'orphaned-1')?.status, 'canceled');
  // The orphaned intent is resolved without ever calling the sim again (no re-fire after a restart).
  assert.equal(sim.stimulateCalls.length, 0);
});

void test('an intent already resolved before a restart is left untouched on replay', async () => {
  const api = new MockRedemptionApi();
  api.rewards.push({ id: 'reward-1', title: 'Sugar' });
  const store = new MemoryIntentStore();
  await store.save([
    { redemptionId: 'done-1', rewardId: 'reward-1', displayName: 'alex', status: 'fulfilled', createdAtMs: 0 },
  ]);
  const sim = new FakeSimClient();
  const sender = new FakeChatSender();
  const manager = new RedemptionManager({ sim, api, store, send: createSend(sender), rewardTitle: 'Sugar' });
  await manager.start();
  assert.deepEqual(api.statusUpdates, []); // never re-resolved
});

void test('isChannelPointsAffiliateRefusal recognises only the Affiliate 403', () => {
  // The exact error twurple raises, and the exact body Twitch sent on 2026-09-16.
  const affiliate = Object.assign(new Error('Encountered HTTP status code 403: Forbidden'), {
    _statusCode: 403,
    _body: '{"error":"Forbidden","status":403,"message":"The broadcaster must have partner or affiliate status."}',
  });
  assert.equal(isChannelPointsAffiliateRefusal(affiliate), true);

  // A 403 for any other reason is a real failure and must still stop startup.
  const otherForbidden = Object.assign(new Error('Encountered HTTP status code 403: Forbidden'), {
    _statusCode: 403,
    _body: '{"error":"Forbidden","status":403,"message":"Missing scope: channel:manage:redemptions"}',
  });
  assert.equal(isChannelPointsAffiliateRefusal(otherForbidden), false);

  const unauthorized = Object.assign(new Error('401'), { _statusCode: 401, _body: 'Invalid OAuth token' });
  assert.equal(isChannelPointsAffiliateRefusal(unauthorized), false);

  assert.equal(isChannelPointsAffiliateRefusal(new Error('network down')), false);
  assert.equal(isChannelPointsAffiliateRefusal(null), false);
  assert.equal(isChannelPointsAffiliateRefusal('403 partner or affiliate status'), false);
});
