import assert from 'node:assert/strict';
import test from 'node:test';
import { HealthState, Metrics, startHealthServer, type HealthServerHandle } from '../src/health';
import { FakeSimClient } from './helpers';

async function withServer(
  run: (handle: HealthServerHandle, state: HealthState, metrics: Metrics) => Promise<void>,
  simOverrides?: Partial<FakeSimClient>,
): Promise<void> {
  const sim = Object.assign(new FakeSimClient(), simOverrides);
  const state = new HealthState();
  const metrics = new Metrics();
  const handle = await startHealthServer({ host: '127.0.0.1', port: 0, sim, state, metrics });
  try {
    await run(handle, state, metrics);
  } finally {
    await handle.close();
  }
}

void test('/health reports sim reachability from a live healthz check', async () => {
  await withServer(async (handle) => {
    const response = await fetch(`http://127.0.0.1:${handle.port}/health`);
    assert.equal(response.status, 200);
    const body = (await response.json()) as { simReachable: boolean };
    assert.equal(body.simReachable, true);
  });
});

void test('/health reflects an unreachable sim', async () => {
  const sim = new FakeSimClient();
  sim.healthzResult = { ok: false, kind: 'timeout' };
  const state = new HealthState();
  const metrics = new Metrics();
  const handle = await startHealthServer({ host: '127.0.0.1', port: 0, sim, state, metrics });
  try {
    const response = await fetch(`http://127.0.0.1:${handle.port}/health`);
    const body = (await response.json()) as { simReachable: boolean };
    assert.equal(body.simReachable, false);
  } finally {
    await handle.close();
  }
});

void test('/health includes token expiry, subscriptions and last redemption outcome from HealthState', async () => {
  await withServer(async (handle, state) => {
    state.tokenExpiry = { bot: '2026-10-01T00:00:00.000Z', broadcaster: null };
    state.subscriptions = [{ type: 'channel.chat.message', status: 'enabled' }];
    state.eventSubConnected = true;
    state.lastRedemptionOutcome = { status: 'FULFILLED', atIso: '2026-09-15T00:00:00.000Z', by: 'alex' };

    const response = await fetch(`http://127.0.0.1:${handle.port}/health`);
    const body = (await response.json()) as Record<string, unknown>;
    assert.deepEqual(body.tokenExpiry, { bot: '2026-10-01T00:00:00.000Z', broadcaster: null });
    assert.deepEqual(body.subscriptions, [{ type: 'channel.chat.message', status: 'enabled' }]);
    assert.equal(body.eventSubConnected, true);
    assert.deepEqual(body.lastRedemptionOutcome, { status: 'FULFILLED', atIso: '2026-09-15T00:00:00.000Z', by: 'alex' });
  });
});

void test('/metrics serves Prometheus text with counters and the latency gauge', async () => {
  await withServer(async (handle, _state, metrics) => {
    metrics.increment('flybridge_commands_served_total{command="fly"}', 3);
    metrics.observeSimCallLatencyMs(12);
    metrics.observeSimCallLatencyMs(8);

    const response = await fetch(`http://127.0.0.1:${handle.port}/metrics`);
    assert.equal(response.status, 200);
    assert.match(response.headers.get('content-type') ?? '', /text\/plain/);
    const text = await response.text();
    assert.match(text, /flybridge_commands_served_total\{command="fly"\} 3/);
    assert.match(text, /flybridge_sim_call_latency_ms_avg 10\.00/);
  });
});

void test('unknown routes return 404', async () => {
  await withServer(async (handle) => {
    const response = await fetch(`http://127.0.0.1:${handle.port}/nope`);
    assert.equal(response.status, 404);
  });
});
