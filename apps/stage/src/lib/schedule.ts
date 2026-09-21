/**
 * The narrative lane's rotation schedule (design A3, "Honesty panel").
 *
 * One panel carries two states on a fixed 240 s cycle, driven by simulated run time so the
 * rotation is deterministic in a fixture and survives a restart:
 *
 *   0 s .. 180 s   the event ticker (the default; what just happened)
 *   180 s .. 240 s the rotating card, one of `ROTATING_CARDS`, a different one each cycle
 *
 * Two states, not three: the copy pass collapsed the eight explainer cards and
 * the two-column real-vs-scaffolding panel into one rotating card of four short lines, because
 * explanations are allowed in exactly one place on this page. A3's requirement that survives is
 * the one that matters — the honest statement of what is real and what is scaffolding appears on
 * a schedule, in full view, rather than in a footnote.
 */

/** Length of one rotation cycle. */
export const CYCLE_MS = 240_000;

/** When the rotating card takes the lane. It holds until `CYCLE_MS`. */
export const CARD_START_MS = 180_000;

/** What the narrative lane should be showing. */
export type NarrativeState = { kind: 'ticker' } | { kind: 'card'; index: number };

/**
 * Resolve the lane state at `runMs` of simulated run time.
 *
 * `cardCount` is the number of cards available, so the rotation follows the card list without
 * this file knowing anything about it.
 */
export function narrativeStateAt(runMs: number, cardCount: number): NarrativeState {
  if (!Number.isFinite(runMs) || runMs < 0) return { kind: 'ticker' };

  const cycle = Math.floor(runMs / CYCLE_MS);
  const phase = runMs - cycle * CYCLE_MS;

  if (phase >= CARD_START_MS && cardCount > 0) return { kind: 'card', index: cycle % cardCount };
  return { kind: 'ticker' };
}

/**
 * Fraction 0..1 through the current lane state, for a progress hairline on the rotating card.
 * Returns 0 for the ticker state, which has no countdown worth showing.
 */
export function narrativeProgressAt(runMs: number): number {
  if (!Number.isFinite(runMs) || runMs < 0) return 0;
  const phase = runMs - Math.floor(runMs / CYCLE_MS) * CYCLE_MS;
  if (phase >= CARD_START_MS) return (phase - CARD_START_MS) / (CYCLE_MS - CARD_START_MS);
  return 0;
}
