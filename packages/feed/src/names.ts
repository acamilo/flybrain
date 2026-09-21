/**
 * The single chokepoint for viewer display names crossing from Twitch into the feed.
 *
 * See `docs/design/stage-bridge.md` section C, "How viewer names reach the ticker": Twitch ->
 * flybridge -> `POST /stimulate` on flysim -> flysim emits a `viewer`/`sugar` event in the next
 * snapshot's `events[]` -> flystage renders it. `validateDisplayName` runs in flybridge before
 * the sim call, and flystage re-validates on render as defence in depth. Only login/display
 * names ever cross this boundary, never message bodies.
 */

/** Letters, digits and underscore only, 1 to 25 characters. Anything else falls back to the literal `"a viewer"`. */
const DISPLAY_NAME_PATTERN = /^[\p{L}\p{N}_]{1,25}$/u;

/** Fallback for a name that fails validation, or is missing entirely. */
export const FALLBACK_DISPLAY_NAME = 'a viewer';

/**
 * Validate a Twitch display name for use in templates, feed events and anything rendered on
 * screen. Returns the name unchanged when it matches `^[\p{L}\p{N}_]{1,25}$`, else the literal
 * `"a viewer"`. Never throws.
 */
export function validateDisplayName(name: string | null | undefined): string {
  if (typeof name !== 'string') return FALLBACK_DISPLAY_NAME;
  return DISPLAY_NAME_PATTERN.test(name) ? name : FALLBACK_DISPLAY_NAME;
}
