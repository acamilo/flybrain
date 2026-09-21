import assert from 'node:assert/strict';
import test from 'node:test';
import { createSend } from '../src/chat';
import { handleCommand, parseCommand, type CommandDeps } from '../src/commands';
import { RateLimiter } from '../src/ratelimit';
import { FakeChatSender, FakeSimClient } from './helpers';

function makeDeps(sim: FakeSimClient, sender: FakeChatSender): CommandDeps {
  return {
    sim,
    send: createSend(sender),
    rateLimiter: new RateLimiter({
      perUserPerCommandMs: 60_000,
      globalReplyMs: 0, // disable the global bucket for most command tests; ratelimit.test.ts covers it
      chatMessagesPer30s: 15,
      perCommandCooldownMs: { sugar: 0 },
    }),
    config: { gameTitle: 'Pokemon Red' },
  };
}

// -- parseCommand --------------------------------------------------------------------------

void test('parseCommand recognizes each of the five commands, case-insensitively', () => {
  assert.equal(parseCommand('!fly'), 'fly');
  assert.equal(parseCommand('!Brain'), 'brain');
  assert.equal(parseCommand('!HOW'), 'how');
  assert.equal(parseCommand('!stuck now please'), 'stuck');
  assert.equal(parseCommand('!sugar'), 'sugar');
});

void test('parseCommand returns null for non-commands and unknown commands', () => {
  assert.equal(parseCommand('hello there'), null);
  assert.equal(parseCommand('!unknown'), null);
  assert.equal(parseCommand(''), null);
  assert.equal(parseCommand('   '), null);
});

// -- !fly / !brain / !how ------------------------------------------------------------------

void test('!fly replies with the fly template including the configured game title', async () => {
  const sender = new FakeChatSender();
  const deps = makeDeps(new FakeSimClient(), sender);
  await handleCommand('fly', { id: 'u1', displayName: 'alex' }, deps);
  assert.equal(sender.sent.length, 1);
  assert.match(sender.sent[0]!, /Pokemon Red/);
});

void test('!brain and !how also substitute the configured game title, never a hardcoded one', async () => {
  const sender = new FakeChatSender();
  const deps = makeDeps(new FakeSimClient(), sender);
  await handleCommand('brain', { id: 'u1', displayName: 'alex' }, deps);
  await handleCommand('how', { id: 'u1', displayName: 'alex' }, { ...deps, rateLimiter: deps.rateLimiter });
  assert.match(sender.sent[0]!, /Pokemon Red/);
});

// -- !stuck ---------------------------------------------------------------------------------

void test('!stuck formats milestone.label and milestone.sinceSeconds from /status', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  const deps = makeDeps(sim, sender);
  await handleCommand('stuck', { id: 'u1', displayName: 'alex' }, deps);
  assert.equal(sender.sent.length, 1);
  assert.match(sender.sent[0]!, /Left the bedroom/);
  assert.match(sender.sent[0]!, /3m 12s/); // 192 seconds
});

void test('!stuck stays silent (no chat noise) when the sim is unreachable', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  sim.statusResult = { ok: false, kind: 'timeout' };
  const deps = makeDeps(sim, sender);
  await handleCommand('stuck', { id: 'u1', displayName: 'alex' }, deps);
  assert.equal(sender.sent.length, 0);
});

// -- !sugar -----------------------------------------------------------------------------------

void test('!sugar accepted: replies with sugarAccepted and calls sim.stimulate with a validated name and source chat', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: 'fly_fan_42' }, deps);
  assert.equal(sim.stimulateCalls.length, 1);
  assert.deepEqual(sim.stimulateCalls[0], { by: 'fly_fan_42', source: 'chat' });
  assert.match(sender.sent[0]!, /fly_fan_42/);
});

void test('!sugar validates a hostile display name to "a viewer" before it ever reaches the sim', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: '<script>alert(1)</script>' }, deps);
  assert.deepEqual(sim.stimulateCalls[0], { by: 'a viewer', source: 'chat' });
  assert.match(sender.sent[0]!, /a viewer/);
  assert.doesNotMatch(sender.sent[0]!, /script/);
});

void test('!sugar on cooldown: sim 429 maps to sugarCooldown with a truthful retry time', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  sim.stimulateResult = { ok: false, kind: 'rate_limited', retryAfterMs: 4_200 };
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: 'alex' }, deps);
  assert.match(sender.sent[0]!, /cooldown/);
  assert.match(sender.sent[0]!, /5s/); // ceil(4200ms) = 5s
});

void test('!sugar disabled: sim forbidden maps to sugarDisabled', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  sim.stimulateResult = { ok: false, kind: 'forbidden', error: 'nope' };
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: 'alex' }, deps);
  assert.match(sender.sent[0]!, /turned off/);
});

void test('!sugar disabled: sim timeout also maps to sugarDisabled', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  sim.stimulateResult = { ok: false, kind: 'timeout' };
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: 'alex' }, deps);
  assert.match(sender.sent[0]!, /turned off/);
});

// -- rate limiting integration ---------------------------------------------------------------

void test('a rate-limited command is dropped silently: no chat reply, no sim call', async () => {
  const sender = new FakeChatSender();
  const sim = new FakeSimClient();
  const deps = makeDeps(sim, sender);
  await handleCommand('sugar', { id: 'u1', displayName: 'alex' }, deps);
  assert.equal(sender.sent.length, 1);
  // same user, same command, immediately again -> blocked by the per-user 60s bucket
  await handleCommand('sugar', { id: 'u1', displayName: 'alex' }, deps);
  assert.equal(sender.sent.length, 1);
  assert.equal(sim.stimulateCalls.length, 1);
});
