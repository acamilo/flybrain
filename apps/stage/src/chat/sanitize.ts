/**
 * Chat re-validation, on the page, at the point of render (defence in depth).
 *
 * The service is the authority. `docs/feed-protocol.md` says `header.chat[].text` has already been
 * through the shared sanitizer and `by` through `validateDisplayName`, and
 * `services/flysim/crates/flysim/src/chat.rs` enforces byte-identical rules in Rust. This module
 * runs **the same shared implementation again** — `sanitizeChatText` from `@flybrain/feed`, not a
 * local copy of its rules — and drops anything that fails.
 *
 * Why bother, when the service already did it: this is the only text on a 24/7 broadcast that
 * originates with a stranger. The Nothing, Forever precedent (a 14-day ban for generated text) is
 * about what reaches the frame, not about whose bug let it through, and a page that re-validates
 * cannot be made to render a slur by a service regression, a replayed fixture, or a future
 * transport nobody has written yet. Calling the shared function rather than reimplementing it is
 * what makes the second check free of the usual cost of defence in depth — there is no second set
 * of rules to drift.
 *
 * Drop, never repair: a line that fails is not truncated or masked, it is not shown. A missing
 * line is invisible; a half-cleaned one is a liability. The one thing this module accepts from the
 * sanitizer is its *cleaning* (NFC, folded whitespace), because that is what the service already
 * put in the header.
 *
 * Pure, so `tests/unit/chat-render.test.ts` can drive the whole table.
 */
import { CHAT_MAX_TEXT_LENGTH, sanitizeChatText, validateDisplayName, type ChatLine } from '@flybrain/feed';

export { CHAT_MAX_TEXT_LENGTH };

/** True when a display name is one the page will render, by the bridge's own rule. */
export function isSafeChatName(name: unknown): name is string {
  return typeof name === 'string' && validateDisplayName(name) === name;
}

/** The line's text as it will be rendered, or null when any rule refuses it. */
export function safeChatText(text: unknown): string | null {
  return sanitizeChatText(text);
}

/**
 * Re-validate one line. Returns a fresh object with only the fields the panel renders, or null.
 *
 * A fresh object, not the input: whatever else the service may have put on that line, nothing
 * beyond `id`, `wallMs`, `by`, `text` and `bot` can reach a component from here.
 */
export function sanitizeChatLine(line: unknown): ChatLine | null {
  if (typeof line !== 'object' || line === null) return null;
  const candidate = line as Partial<ChatLine>;
  if (typeof candidate.id !== 'number' || !Number.isFinite(candidate.id)) return null;
  if (!isSafeChatName(candidate.by)) return null;
  const text = safeChatText(candidate.text);
  if (text === null) return null;
  return {
    id: candidate.id,
    wallMs: typeof candidate.wallMs === 'number' && Number.isFinite(candidate.wallMs) ? candidate.wallMs : 0,
    by: candidate.by,
    text,
    ...(candidate.bot === true ? { bot: true } : {}),
  };
}

/**
 * The last `keep` renderable lines of a chat ring, oldest first.
 *
 * Oldest first because that is the reading order on screen and the newest line is the one that
 * slides in at the bottom. The header already arrives oldest-first; sorting by id rather than
 * trusting the order costs nothing and makes the panel independent of that promise.
 */
export function sanitizeChatRing(lines: unknown, keep: number): ChatLine[] {
  if (!Array.isArray(lines)) return [];
  const out: ChatLine[] = [];
  for (const line of lines) {
    const safe = sanitizeChatLine(line);
    if (safe) out.push(safe);
  }
  out.sort((a, b) => a.id - b.id);
  return out.length > keep ? out.slice(out.length - keep) : out;
}
