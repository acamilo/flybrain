/**
 * The rail's own motion constants: the durations that belong to a panel rather than to a moment.
 *
 * `src/motion/moments.ts` owns every duration a *moment* has (enter, hold, leave, per type) and
 * `src/motion/catalogue.ts` owns what each moment looks like. What is left over is the handful of
 * timings that are properties of the rail itself — the tab crossfade, the flash envelope, how long
 * a spine rung pulses — and the caption band's one word per moment type. They live here so the
 * director is wiring rather than a table, and so `src/theme/motion.css` has exactly one set of
 * numbers to agree with.
 */
import type { MomentType } from './moments';
import { MOMENT_TIMING } from './moments';

/** Tab content crossfade. The locked layout's own number. */
export const CROSSFADE_MS = 300;

/** Flash envelope from `docs/design/animation.md`: 120 ms up, 600 ms down. */
export const FLASH_UP_MS = 120;
export const FLASH_DOWN_MS = 600;

/** Arrival duration for a slide: a ticker row, a chat line, the caption band. */
export const ARRIVAL_MS = 240;

/** The spine's new rung "pulses twice then fills amber": two 600 ms pulses. */
export const SPINE_PULSE_MS = 1200;

/** The badge count's 1.15x bounce. */
export const BOUNCE_MS = 320;

/** The day-rollover slide, which is the whole moment: "DAY N slides across the strip once, 2 s". */
export const DAY_SLIDE_MS = MOMENT_TIMING.dayRollover.enterMs + MOMENT_TIMING.dayRollover.holdMs;

/** The rollback's horizontal wipe over the game canvas: the rollback moment's own arrival. */
export const REWIND_MS = MOMENT_TIMING.rollback.enterMs;

/**
 * HERE FOR thresholds, in seconds: 1 h, 3 h, 6 h.
 *
 * Crossing one pulses the number and steps its colour warmer, which is the catalogue's last row.
 * It is not a queued moment: the threshold is a property of a value that is on screen all the
 * time, so a page that loads into a fly which has been stuck for four hours must already be warm
 * rather than waiting for a crossing that happened before it started.
 */
export const HERE_FOR_THRESHOLDS: readonly number[] = [3600, 3 * 3600, 6 * 3600];

/** Warmth tier (0..3) for a time at the current rung. */
export function hereForTier(seconds: number): number {
  if (!Number.isFinite(seconds)) return 0;
  let tier = 0;
  for (const threshold of HERE_FOR_THRESHOLDS) {
    if (seconds >= threshold) tier += 1;
  }
  return tier;
}

/**
 * The caption band's first word, per moment type.
 *
 * Caps, one word, in the pixel face: it is a label for the line beside it, which carries the
 * feed's own text. The types with no caption (`sugar`, `reward`, `modeChange`) still have an entry
 * so the map is total and the band can never render `undefined` on air.
 */
export const CAPTION_HEADLINE: Record<MomentType, string> = {
  badge: 'BADGE',
  milestone: 'MILESTONE',
  rollback: 'REWIND',
  sugar: 'SUGAR',
  reward: 'REWARD',
  dayRollover: 'DAY',
  modeChange: 'MODE',
};
