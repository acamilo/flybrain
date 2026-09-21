/**
 * The chat text sanitizer: the single gate between a Twitch message and anything that reaches the
 * screen.
 *
 * `docs/control-api.md` (`POST /chat`) makes flysim the enforcing side — the bridge runs this
 * first so a rejected line is never sent at all, and `services/flysim/crates/flysim/src/chat.rs`
 * runs byte-identical rules again on arrival. The two implementations are pinned to each other by
 * `tests/fixtures/chat-cases.json`, which both test suites load: a rule that changes in one
 * language and not the other fails both.
 *
 * Why this exists at all: `docs/stream-mvp-plan.md` records the Nothing, Forever precedent (a
 * 14-day ban for generated text) and the operating rule that follows from it — never render raw
 * chat into the video. Chat lines reaching the page are AutoMod-passed, name-validated,
 * sanitized here, deny-list filtered, and carried in a bounded ring in the feed header. They
 * never touch the simulation.
 *
 * The rules, in the order they are applied (the order is part of the contract, because the
 * rejection reason is a metric label on the Rust side — `fly_chat_rejected_total{reason}`):
 *
 *  1. NFC-normalize, then turn tab/CR/LF into spaces.
 *  2. `control`: any remaining `\p{Cc}` code point rejects the line.
 *  3. `charset`: every code point must be `\p{Alphabetic}`, `\p{Number}`, a space (any
 *     `\p{White_Space}` is first folded to one) or one of [`ALLOWED_PUNCTUATION`] /
 *     [`ALLOWED_EXTRA_PUNCTUATION`]. This is what rejects emoji, combining marks (zalgo),
 *     zero-width characters, bidi overrides and the BOM.
 *  4. Collapse runs of spaces and trim. `empty`: nothing left.
 *  5. `too_long`: more than [`CHAT_MAX_TEXT_LENGTH`] code points.
 *  6. `url`: `://`, `www.`, or a TLD-like token (`something.tld`, two or more ASCII letters after
 *     the dot). Slightly over-eager by design: `Mr.Mime` is refused, `e.g.` and `3.14` are not.
 *
 * No emoji for now. Adding them means changing rule 3 in both languages and in the fixture.
 */

/** Longest accepted line, in Unicode code points. */
export const CHAT_MAX_TEXT_LENGTH = 200;

/** Most chat lines the feed header ever carries (`docs/feed-protocol.md`: `chat?: ChatLine[]`). */
export const CHAT_RING_MAX = 12;

/**
 * The ASCII punctuation a chat line may contain, besides letters, digits and spaces.
 *
 * Deliberately excludes `<`, `>`, `` ` ``, `\`, `|`, `^` and `$`: none of them reads as prose, and
 * every one of them is markup or shell syntax somewhere downstream.
 */
export const ALLOWED_PUNCTUATION = '!"#%&\'()*+,-./:;=?@[]_{}~';

/**
 * Non-ASCII punctuation a chat line may also contain: the typographic marks phone keyboards
 * produce by themselves, and the CJK equivalents of the ASCII stops.
 *
 * An explicit list rather than the Unicode `P*` categories, because Rust's standard library has no
 * `is_punctuation` and this set has to be identical in both languages without either side taking a
 * dependency. Symbols stay out, so emoji are still refused.
 */
export const ALLOWED_EXTRA_PUNCTUATION =
  '–—‘’“”…¡¿·、。「」！？';

/** Why a line was refused. The Rust side adds `name`, `deny_list` and `rate_limited`. */
export type ChatRejectReason = 'malformed' | 'control' | 'charset' | 'empty' | 'too_long' | 'url';

/** The outcome of [`classifyChatText`]: the cleaned line, or the reason it was refused. */
export type ChatTextResult = { ok: true; text: string } | { ok: false; reason: ChatRejectReason };

const CONTROL_CHARACTER = /\p{Cc}/u;
const WHITESPACE = /\p{White_Space}/gu;
// One code point at a time against the two punctuation strings plus these two properties, rather
// than one big character class: the Rust side has to do exactly this, and a shared character class
// would have to be spelled twice with two different escaping rules.
const LETTER_OR_DIGIT = /^[\p{Alphabetic}\p{Number}]$/u;

function isAllowed(character: string): boolean {
  return (
    character === ' ' ||
    ALLOWED_PUNCTUATION.includes(character) ||
    ALLOWED_EXTRA_PUNCTUATION.includes(character) ||
    LETTER_OR_DIGIT.test(character)
  );
}

/**
 * Sanitize one chat message. Returns the cleaned line, or `null` when any rule rejects it.
 *
 * Never throws, and never returns a partially cleaned line: a message either survives every rule
 * or is dropped whole.
 */
export function sanitizeChatText(text: unknown): string | null {
  const result = classifyChatText(text);
  return result.ok ? result.text : null;
}

/** [`sanitizeChatText`] with the rejection reason, for metrics and tests. */
export function classifyChatText(input: unknown): ChatTextResult {
  if (typeof input !== 'string') return { ok: false, reason: 'malformed' };

  const normalized = input.normalize('NFC').replace(/[\t\n\r]/g, ' ');
  if (CONTROL_CHARACTER.test(normalized)) return { ok: false, reason: 'control' };

  const spaced = normalized.replace(WHITESPACE, ' ');
  if (![...spaced].every(isAllowed)) return { ok: false, reason: 'charset' };

  const collapsed = spaced.replace(/ {2,}/g, ' ').trim();
  if (collapsed.length === 0) return { ok: false, reason: 'empty' };
  if ([...collapsed].length > CHAT_MAX_TEXT_LENGTH) return { ok: false, reason: 'too_long' };
  if (looksLikeUrl(collapsed)) return { ok: false, reason: 'url' };

  return { ok: true, text: collapsed };
}

/**
 * Whether a sanitized line advertises a link.
 *
 * ASCII-only lowercasing on purpose: `String.prototype.toLowerCase()` and Rust's
 * `to_lowercase()` disagree about a handful of non-ASCII code points (and one of them changes
 * length), and only ASCII matters for a host name.
 */
export function looksLikeUrl(text: string): boolean {
  const lower = asciiLowercase(text);
  if (lower.includes('://') || lower.includes('www.')) return true;

  for (const token of lower.split(' ')) {
    const chars = [...token];
    for (let index = 1; index < chars.length - 1; index++) {
      if (chars[index] !== '.') continue;
      if (!isAsciiAlphanumeric(chars[index - 1] as string)) continue;
      let letters = 0;
      for (let after = index + 1; after < chars.length; after++) {
        if (!isAsciiLetter(chars[after] as string)) break;
        letters += 1;
      }
      if (letters >= 2) return true;
    }
  }
  return false;
}

function asciiLowercase(text: string): string {
  return text.replace(/[A-Z]/g, (character) => String.fromCharCode(character.charCodeAt(0) + 32));
}

function isAsciiLetter(character: string): boolean {
  return character >= 'a' && character <= 'z';
}

function isAsciiAlphanumeric(character: string): boolean {
  return isAsciiLetter(character) || (character >= '0' && character <= '9');
}
