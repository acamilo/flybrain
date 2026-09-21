import assert from 'node:assert/strict';
import test from 'node:test';
import { createSend } from '../src/chat';
import { explainerEnabled, ExplainerPoster } from '../src/explainer';
import { EXPLAINER_TEMPLATE_IDS } from '../src/templates';
import { FakeClock } from '../src/ratelimit';
import { FakeChatSender } from './helpers';

const INTERVAL_MS = 20 * 60 * 1000;
const SILENCE_MS = 20 * 60 * 1000;
// Both default to the same 20-minute window (docs/design/stage-bridge.md B2), so a tick that is
// exactly due is also exactly at the silence boundary if chat activity and the last post
// coincide. Tests that want "due, but recently active" need a clear margin between the two
// anchors — this is that margin, well under either window.
const SLACK_MS = 5 * 60 * 1000;

void test('does not post before intervalMs has elapsed', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
  });
  clock.advance(INTERVAL_MS - 1);
  const posted = await poster.tick();
  assert.equal(posted, false);
  assert.equal(sender.sent.length, 0);
});

void test('posts the first rotation card once intervalMs has elapsed, with recent chat activity', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
  });
  clock.advance(SLACK_MS);
  poster.noteChatActivity();
  clock.advance(INTERVAL_MS - SLACK_MS + 1); // total elapsed since start: INTERVAL_MS + 1 (due)
  const posted = await poster.tick();
  assert.equal(posted, true);
  assert.equal(sender.sent.length, 1);
});

void test('skips posting when chat has been silent for silenceMs, so it does not shout into an empty room', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
  });
  // No noteChatActivity() calls after construction; chat goes silent from t=0.
  clock.advance(INTERVAL_MS);
  const posted = await poster.tick();
  assert.equal(posted, false);
  assert.equal(sender.sent.length, 0);
});

void test('resumes posting once chat activity returns after a silent stretch', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
  });

  clock.advance(INTERVAL_MS + SLACK_MS);
  assert.equal(await poster.tick(), false); // silent since t=0, skipped, lastPostAt reset here

  clock.advance(SLACK_MS); // still silent, some time passes before chat picks back up
  poster.noteChatActivity();
  clock.advance(INTERVAL_MS - SLACK_MS + 1); // due since the reset above, not silent since activity
  assert.equal(await poster.tick(), true);
  assert.equal(sender.sent.length, 1);
});

void test('rotates through all six explainer cards in order before repeating', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
  });

  for (let i = 0; i < EXPLAINER_TEMPLATE_IDS.length + 2; i++) {
    clock.advance(SLACK_MS);
    poster.noteChatActivity();
    clock.advance(INTERVAL_MS - SLACK_MS + 1);
    const posted = await poster.tick();
    assert.equal(posted, true, `expected card ${i} to post`);
  }

  assert.equal(sender.sent.length, EXPLAINER_TEMPLATE_IDS.length + 2);
  // The 7th post (index 6) should repeat the 1st card's text.
  assert.equal(sender.sent[0], sender.sent[EXPLAINER_TEMPLATE_IDS.length]);
});

// -- Off switches: EXPLAINER_INTERVAL_MS=0 and FEATURE_QUIET ------------------------------------

void test('explainerEnabled is false in quiet mode and false at interval 0, independently', () => {
  assert.equal(explainerEnabled({ featureQuiet: false, explainerIntervalMs: INTERVAL_MS }), true);
  assert.equal(explainerEnabled({ featureQuiet: true, explainerIntervalMs: INTERVAL_MS }), false);
  assert.equal(explainerEnabled({ featureQuiet: false, explainerIntervalMs: 0 }), false);
  assert.equal(explainerEnabled({ featureQuiet: true, explainerIntervalMs: 0 }), false);
});

void test('an interval of 0 turns the rotation off, however long the clock runs', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: 0,
    silenceMs: SILENCE_MS,
    clock,
  });

  for (let i = 0; i < 5; i++) {
    poster.noteChatActivity(); // busy chat: the silence rule is not what is stopping it
    clock.advance(INTERVAL_MS);
    assert.equal(await poster.tick(), false);
  }
  assert.equal(sender.sent.length, 0);
});

void test('an explicitly disabled poster (quiet mode) never posts, even when due with live chat', async () => {
  const clock = new FakeClock();
  const sender = new FakeChatSender();
  const poster = new ExplainerPoster({
    send: createSend(sender),
    config: { gameTitle: 'Pokemon Red' },
    intervalMs: INTERVAL_MS,
    silenceMs: SILENCE_MS,
    clock,
    enabled: false,
  });

  clock.advance(SLACK_MS);
  poster.noteChatActivity();
  clock.advance(INTERVAL_MS);
  assert.equal(await poster.tick(), false);
  assert.equal(sender.sent.length, 0);
});
