import assert from 'node:assert/strict';
import test from 'node:test';
import { HttpSimClient } from '../src/sim';
import { startFakeSim, type FakeSimHandle } from './fake-sim';

async function withFakeSim(run: (sim: FakeSimHandle) => Promise<void>): Promise<void> {
  const fakeSim = await startFakeSim();
  try {
    await run(fakeSim);
  } finally {
    await fakeSim.close();
  }
}

void test('stimulate: 202 accept maps to ok:true with the eventId', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.queueStimulateResponse({ kind: 'accept', eventId: 42 });
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.stimulate({ by: 'alex', source: 'chat' });
    assert.deepEqual(result, { ok: true, data: { eventId: 42 } });
  });
});

void test('stimulate: 429 maps to rate_limited with retryAfterMs', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.queueStimulateResponse({ kind: 'rate_limited', retryAfterMs: 3_500 });
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.stimulate({ by: 'alex', source: 'chat' });
    assert.deepEqual(result, { ok: false, kind: 'rate_limited', retryAfterMs: 3_500 });
  });
});

void test('stimulate: 403 maps to forbidden', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.queueStimulateResponse({ kind: 'forbidden', error: 'reward is disabled' });
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.stimulate({ by: 'alex', source: 'chat' });
    assert.deepEqual(result, { ok: false, kind: 'forbidden', error: 'reward is disabled' });
  });
});

void test('stimulate: 500 maps to http_error with the status', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.queueStimulateResponse({ kind: 'error', status: 500, error: 'boom' });
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.stimulate({ by: 'alex', source: 'chat' });
    assert.deepEqual(result, { ok: false, kind: 'http_error', status: 500, error: 'boom' });
  });
});

void test('stimulate: a slow server past the client timeout maps to kind:timeout', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.queueStimulateResponse({ kind: 'timeout', delayMs: 500 });
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 50 });
    const result = await client.stimulate({ by: 'alex', source: 'chat' });
    assert.deepEqual(result, { ok: false, kind: 'timeout' });
  });
});

void test('stimulate: connection refused maps to network_error', async () => {
  // Nothing listening on this port.
  const client = new HttpSimClient({ baseUrl: 'http://127.0.0.1:1', timeoutMs: 1000 });
  const result = await client.stimulate({ by: 'alex', source: 'chat' });
  assert.equal(result.ok, false);
  if (!result.ok) assert.equal(result.kind, 'network_error');
});

void test('status: returns the full StatusResponse on 200', async () => {
  await withFakeSim(async (fakeSim) => {
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.status();
    assert.equal(result.ok, true);
    if (result.ok) {
      assert.equal(result.data.status, 'running');
      assert.equal(result.data.milestone.label, 'Booting');
    }
  });
});

void test('healthz: 503 maps to http_error 503', async () => {
  await withFakeSim(async (fakeSim) => {
    fakeSim.setHealthy(false);
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.healthz();
    assert.equal(result.ok, false);
    if (!result.ok) {
      assert.equal(result.kind, 'http_error');
      assert.equal((result as { status: number }).status, 503);
    }
  });
});

void test('healthz: 200 maps to ok', async () => {
  await withFakeSim(async (fakeSim) => {
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.healthz();
    assert.deepEqual(result, { ok: true, data: { status: 'ok' } });
  });
});

void test('events: forwards since/limit as query params and returns the page', async () => {
  await withFakeSim(async (fakeSim) => {
    const client = new HttpSimClient({ baseUrl: fakeSim.baseUrl, timeoutMs: 1000 });
    const result = await client.events(10, 5);
    assert.deepEqual(result, { ok: true, data: { events: [] } });
    assert.equal(fakeSim.requests.at(-1)?.path, '/events');
  });
});
