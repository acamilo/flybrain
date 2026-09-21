/**
 * The LADDER tab's fallback for a rung name that still does not fit its column at the 24 px type
 * floor, even with the extra width the 2026-09-16 pass gave it back from the dropped thumbnail
 * (`src/lib/geometry.ts`'s `LADDER_STATS_WIDTH`).
 *
 * `docs/design/gameboy-theme.md`'s "Selection cursor marks the current rung" makes a rung's name
 * the one piece of state a viewer hunts for, so an ellipsis that hides the tail of "Viridian
 * Forest" is the wrong failure — a slow, stepped horizontal pan is the honest one: nothing is ever
 * permanently hidden, it is just not all on screen at once. `src/panels/tabs/LadderTab.tsx` is the
 * one place this drives a real DOM element; these are the pure numbers, kept separate so they can
 * be tested without a browser.
 */

/** The pixel-motion grid every stepped transition on the page snaps to (`--pixel-step`). */
export const MARQUEE_STEP_PX = 8;

/** How long the name sits fully visible at each end of its pan before reversing. */
export const MARQUEE_HOLD_MS = 1200;

/** How long one 8 px step of the pan takes. */
export const MARQUEE_STEP_MS = 160;

/**
 * How far a name has to travel to bring its clipped tail fully into view, rounded up to a whole
 * step so the pan always lands on the pixel grid.
 *
 * `contentWidth` is the label's own unclipped width (an element's `scrollWidth`) and `boxWidth`
 * its visible column (`clientWidth`). Zero when the name already fits — the caller's cue not to
 * marquee it at all.
 */
export function marqueeDistance(contentWidth: number, boxWidth: number, step = MARQUEE_STEP_PX): number {
  const overflow = contentWidth - boxWidth;
  if (overflow <= 0) return 0;
  return Math.ceil(overflow / step) * step;
}

/**
 * Total time for one full pass: out, hold at the far end, back, hold at the start — the shape a
 * CSS `animation` keyed on this duration plays on `infinite alternate`.
 */
export function marqueeCycleMs(
  distance: number,
  stepPx = MARQUEE_STEP_PX,
  stepMs = MARQUEE_STEP_MS,
  holdMs = MARQUEE_HOLD_MS,
): number {
  if (distance <= 0) return 0;
  const steps = distance / stepPx;
  return steps * stepMs * 2 + holdMs * 2;
}
