/**
 * The page's own chat re-validation.
 *
 * The service is the authority and `packages/feed/tests/chat.test.ts` is where the *rules* are
 * tested against the shared fixture both languages load. What is tested here is the render-side
 * gate: that the page really does run those rules again, that a line failing any of them is
 * dropped whole rather than repaired, and that nothing but the four fields the panel renders can
 * reach a component.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { CHAT_LINES } from '../../src/lib/geometry';
import { isSafeChatName, safeChatText, sanitizeChatLine, sanitizeChatRing } from '../../src/chat/sanitize';

function line(overrides: Record<string, unknown> = {}) {
  return { id: 1, wallMs: 1_700_000_000_000, by: 'moth_lord', text: 'he has been in there two hours', ...overrides };
}

test('a good line survives, with only the fields the panel renders', () => {
  const safe = sanitizeChatLine(line({ bot: true, extra: 'dropped', html: '<b>no</b>' }));
  assert.deepEqual(safe, {
    id: 1,
    wallMs: 1_700_000_000_000,
    by: 'moth_lord',
    text: 'he has been in there two hours',
    bot: true,
  });
});

test('the render gate really is the shared sanitizer, not a lookalike', () => {
  // One case per rule in `packages/feed/src/chat.ts`, so a regression that bypassed the shared
  // implementation here would fail rather than pass quietly.
  assert.equal(safeChatText('a zero​width space'), null, 'zero-width');
  assert.equal(safeChatText('emoji \u{1F41D}'), null, 'emoji are not in the charset');
  assert.equal(safeChatText('bell '), null, 'control characters');
  assert.equal(safeChatText('look at twitch.tv'), null, 'a TLD-like token');
  assert.equal(safeChatText('https://example.com'), null, 'a scheme');
  assert.equal(safeChatText('www.example'), null, 'a www host');
  assert.equal(safeChatText('a'.repeat(201)), null, 'over 200 code points');
  assert.equal(safeChatText('   '), null, 'nothing left after trimming');
  assert.equal(safeChatText(''), null);
  assert.equal(safeChatText(42), null, 'a non-string');
  // And the cleaning the service already applied is accepted rather than re-done differently.
  assert.equal(safeChatText('two   spaces'), 'two spaces');
  assert.equal(safeChatText(' padded '), 'padded');
  assert.equal(safeChatText('e.g. 3.14 is fine'), 'e.g. 3.14 is fine');
});

test('a bad name drops the line rather than falling back to "a viewer"', () => {
  // The sugar panel's `safeDisplayName` substitutes a literal because it has a sentence to
  // complete. A chat line has no such need: an unrenderable name means an unrenderable line.
  assert.equal(isSafeChatName('moth_lord'), true);
  assert.equal(isSafeChatName('two words'), false);
  assert.equal(isSafeChatName('has-hyphen'), false);
  assert.equal(isSafeChatName('a'.repeat(26)), false);
  assert.equal(isSafeChatName(''), false);
  assert.equal(isSafeChatName(null), false);

  assert.equal(sanitizeChatLine(line({ by: '<script>' })), null);
  assert.equal(sanitizeChatLine(line({ by: 42 })), null);
});

test('a line with no usable id is dropped: the id is the panel\'s React key', () => {
  assert.equal(sanitizeChatLine(line({ id: 'one' })), null);
  assert.equal(sanitizeChatLine(line({ id: Number.NaN })), null);
  assert.equal(sanitizeChatLine(line({ id: undefined })), null);
  assert.equal(sanitizeChatLine(null), null);
  assert.equal(sanitizeChatLine('a line'), null);
});

test('bot is a flag the service sets, never one a viewer can claim', () => {
  // `bot: true` renders green, which is the page saying "this is our own reply". Anything but a
  // literal `true` is not it.
  assert.equal(sanitizeChatLine(line({ bot: 'true' }))?.bot, undefined);
  assert.equal(sanitizeChatLine(line({ bot: 1 }))?.bot, undefined);
  assert.equal(sanitizeChatLine(line({ bot: true }))?.bot, true);
});

test('the ring keeps the last seven, oldest first, and drops the rest', () => {
  const ring = Array.from({ length: 12 }, (_, index) => line({ id: index + 1, text: `line ${index + 1}` }));
  const kept = sanitizeChatRing(ring, CHAT_LINES);
  assert.equal(CHAT_LINES, 7);
  assert.equal(kept.length, 7);
  assert.equal(kept[0]?.text, 'line 6');
  assert.equal(kept[6]?.text, 'line 12');
});

test('a ring sorted by id, whatever order it arrived in', () => {
  // The header promises oldest-first; sorting anyway costs nothing and makes the panel's reading
  // order independent of that promise.
  const kept = sanitizeChatRing([line({ id: 3 }), line({ id: 1 }), line({ id: 2 })], 7);
  assert.deepEqual(
    kept.map((entry) => entry.id),
    [1, 2, 3],
  );
});

test('a ring with unrenderable lines in it keeps the rest', () => {
  const kept = sanitizeChatRing(
    [line({ id: 1 }), line({ id: 2, text: 'join discord.gg now' }), line({ id: 3 }), 'nonsense', null],
    7,
  );
  assert.deepEqual(
    kept.map((entry) => entry.id),
    [1, 3],
  );
});

test('a missing or malformed ring is empty, never a throw', () => {
  // The panel renders nothing for an empty ring, which is the same outcome as a feed with no chat
  // at all — deliberately indistinguishable, and never a half-drawn panel.
  assert.deepEqual(sanitizeChatRing(undefined, 7), []);
  assert.deepEqual(sanitizeChatRing(null, 7), []);
  assert.deepEqual(sanitizeChatRing('chat', 7), []);
  assert.deepEqual(sanitizeChatRing({ 0: line() }, 7), []);
  assert.deepEqual(sanitizeChatRing([], 7), []);
});
