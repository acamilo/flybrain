/**
 * The animation engine's public surface: `docs/design/animation.md`, implemented.
 *
 * Six files, in dependency order — nothing here imports a panel, `App.tsx` or the paint loop, so
 * the engine can be wired into the rail later without this directory changing:
 *
 *   `lerp.ts`       `Smoothed` / `SmoothedRecord` / `Tween`, frame-rate independent
 *   `moments.ts`    the priority queue, the 9 s hold, and the feed -> moment mapping
 *   `particles.ts`  the pooled, additive, 400-particle canvas layer
 *   `catalogue.ts`  one data row per moment: regions, emitters, tween, sound tier
 *   `sfx-tiers.ts`  sound tiers -> the existing bank in `src/audio/sfx.ts`
 *   `engine.ts`     the four of them wired together, as `tick(nowMs)` + `draw(ctx)`
 *
 * The dev harness (`harness.tsx`, at `/motion-harness/` in `vite dev`) is deliberately *not*
 * exported here: it is a page, not a module, and nothing in the broadcast build should be able to
 * import it by accident.
 */
export {
  approach,
  clamp01,
  DEFAULT_TAU_MS,
  EASINGS,
  easing,
  HALF_LIFE_TO_TAU,
  mix,
  Smoothed,
  SmoothedRecord,
  Tween,
  type EasingFn,
  type EasingName,
  type SmoothedOptions,
  type TweenOptions,
} from './lerp';

export {
  DAY_SECONDS,
  DEFAULT_MAX_PENDING,
  dayNumber,
  formatRewardValue,
  MOMENT_PRIORITY,
  MOMENT_TIMING,
  MOMENT_TYPES,
  MomentQueue,
  momentTotalMs,
  PREEMPT_LEAVE_MS,
  REWARD_TIER_BOUNDS,
  REWARD_TIER_INTENSITY,
  rewardTier,
  triggerFromEvent,
  TriggerMapper,
  type ActiveMoment,
  type MomentPhase,
  type MomentQueueOptions,
  type MomentSnapshot,
  type MomentTiming,
  type MomentTrigger,
  type MomentType,
  type RewardValueTier,
} from './moments';

export {
  ALPHA_STEPS,
  DECAY_MS,
  DIR,
  MAX_COLOURS,
  MAX_PARTICLES,
  ParticleField,
  type ParticleFieldOptions,
  type ParticlePainter,
  type Point,
  type Rect,
} from './particles';

export {
  MOMENT_CATALOGUE,
  MOTION_ANCHORS,
  MOTION_COLOURS,
  MOTION_REGIONS,
  MOTION_SEED,
  recipeFor,
  regionsUnion,
  resolveMotionColours,
  type ColourToken,
  type EmitterSpec,
  type MomentRecipe,
  type MotionAnchor,
  type MotionPalette,
  type MotionRegion,
  type TabFocus,
} from './catalogue';

export { NEEDS_SAMPLE, SFX_TIERS, sfxCue, type SfxCue, type SfxTier } from './sfx-tiers';

export { MotionEngine, type MotionEngineOptions, type MotionSurface, type QueuedCue } from './engine';
