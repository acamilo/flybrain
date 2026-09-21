/**
 * Sound tiers, and how each one lands in the SFX bank that already exists.
 *
 * `docs/design/animation.md`: "Sound is tiered with the visual: small reward tick, sugar tone,
 * milestone chime, badge fanfare, rollback rewind. Synthesized in the page, master gain from
 * config." Those five names are the tier vocabulary here (plus `stinger` for the day rollover's
 * "soft stinger" and `silent` for the rows the catalogue marks "none"), and this file is the only
 * place that knows which synthesised sample in `src/audio/sfx.ts` each tier actually plays.
 *
 * Why a table and not a direct call: the bank is six fixed recipes (`SfxName`) chosen for the
 * *panels*, and the moment catalogue is a separate list with a different shape. Keeping the mapping
 * as data means the later rail wiring passes a tier, one row changes when a sample is added, and no
 * moment ever names a sample directly.
 *
 * The engine shipped with two gaps — no rewind sweep and no stinger, so `rollback` borrowed the
 * stuck alarm and `dayRollover` the milestone chime at half level. Both samples now exist
 * (`src/audio/sfx.ts`), `NEEDS_SAMPLE` is empty, and this table was the only file that had to
 * change, which was the point of keeping the mapping as data.
 */
import type { SfxName } from '@/audio/sfx';

/** The design's sound tiers. */
export type SfxTier = 'tick' | 'tone' | 'chime' | 'fanfare' | 'rewind' | 'stinger' | 'silent';

export interface SfxCue {
  /** Bank entry to play, or null for a silent tier. */
  sfx: SfxName | null;
  /**
   * Per-cue gain, multiplied by the engine's master gain (`src/audio/engine.ts`). Only tiers that
   * are deliberately under- or over-playing their sample carry anything but 1.
   */
  gain: number;
}

/**
 * Tiers still playing a stand-in because the bank has no sample of their own.
 *
 * Empty now: `rewind` and `stinger` were the two, and `src/audio/sfx.ts` grew a rewind sweep and a
 * soft stinger when the rail wiring landed. Kept as a named export because it is the honest place
 * to record the next such gap, and `tests/unit/motion-catalogue.test.ts` asserts every tier the
 * catalogue uses resolves to a real bank entry.
 */
export const NEEDS_SAMPLE: readonly SfxTier[] = [];

export const SFX_TIERS: Record<SfxTier, SfxCue> = {
  // "small reward tick" — the bank's `reward` is exactly this: one 90 ms blip.
  tick: { sfx: 'reward', gain: 1 },
  // "sugar tone" — `sugar`, the quick sparkle that goes with the dopamine bar jumping.
  tone: { sfx: 'sugar', gain: 1 },
  // "milestone chime" — `milestone`, two notes, a step up.
  chime: { sfx: 'milestone', gain: 1 },
  // "badge fanfare" — `badge`, the loudest thing in the bank, and the rarest moment on the stage.
  fanfare: { sfx: 'badge', gain: 1 },
  // "rollback rewind" — the bank's own sweep now (a fall through two octaves), so this no longer
  // borrows the stuck-o-meter's alarm.
  rewind: { sfx: 'rewind', gain: 1 },
  // "soft stinger" for the day rollover: its own two quiet sine notes rather than the milestone
  // chime at half level. A day boundary is structure, not an achievement.
  stinger: { sfx: 'stinger', gain: 1 },
  silent: { sfx: null, gain: 0 },
};

/** Resolve a tier, defensively: an unknown tier is silence, never a throw on the broadcast page. */
export function sfxCue(tier: SfxTier): SfxCue {
  return SFX_TIERS[tier] ?? SFX_TIERS.silent;
}
