/**
 * One table per moment: which regions it touches, which emitters fire where, which tween, which
 * sound tier. `docs/design/animation.md` ends its catalogue with an instruction —
 *
 *   "Implement in one module (`src/moments.ts`) with the queue, and one CSS file for keyframes, so
 *    the catalogue above stays reviewable."
 *
 * — and this is the machine-readable half of that: the recipes are *data*, so the rail rework that
 * wires the panels up reads a row rather than writing a branch, and a reviewer can diff this file
 * against the design's own table line by line.
 *
 * Nothing here draws. `MOMENT_CATALOGUE` is inert; `engine.ts` is what walks a row and calls the
 * emitters in `particles.ts`.
 *
 * ## The regions
 *
 * Rail layout v2 (`docs/stream-mvp-plan.md`, locked 2026-09-15 night), read straight out of
 * `src/lib/geometry.ts` — which is what the panels are laid out from, so a moment cannot animate a
 * box the page does not actually have. The engine shipped with these as local literals because the
 * v2 panels were being rebuilt at the same time in another pass; that pass has landed, and this is
 * the "should become a thin read of `LAYOUT`" its own comment asked for.
 *
 * The arithmetic those numbers close on, unchanged:
 *
 *     left column   title 40 + game 720 + gutter 4 + fly strip 220              = 984 = 1080 - 2x48
 *     right rail    cluster 144 + slot 420 + ticker 100 + chat 244 + 3 x 12     = 944 = 720 + 4 + 220
 *     width         800 + 12 + 1012                                             = 1824 = 1920 - 2x48
 */
import { LAYOUT, RAIL_BOX } from '@/lib/geometry';
import type { Rect } from './particles';
import { DIR } from './particles';
import { MOMENT_TIMING, type MomentTiming, type MomentType } from './moments';
import type { SfxTier } from './sfx-tiers';

/** Named rail regions a moment is allowed to animate. */
export type MotionRegion =
  | 'titleStrip'
  | 'game'
  | 'flyStrip'
  | 'rail'
  | 'progressCluster'
  | 'tabSlot'
  | 'ticker'
  | 'chat';

/** Absolute boxes, stage pixels from the top left of `#stage`. */
export const MOTION_REGIONS: Record<MotionRegion, Rect> = {
  titleStrip: LAYOUT.title,
  game: LAYOUT.game,
  flyStrip: LAYOUT.flyStrip,
  rail: RAIL_BOX,
  progressCluster: LAYOUT.progress,
  tabSlot: LAYOUT.tabs,
  ticker: LAYOUT.events,
  chat: LAYOUT.chat,
};

/**
 * Emission points.
 *
 * The addendum aims particles at *things*, not at panels ("drift from the fly's head toward the
 * sugar chip", "amber sparks from the new rung", "a shockwave ring from the badge count").
 *
 * These are the *defaults*: a point derived from a region, which is what the harness and the
 * contact sheets run on because they have no panels. On the real page every one of them except
 * `flyHead` is replaced at run time with the centre of the element it names, measured from the DOM
 * and pushed in with `MotionEngine.setAnchors` (`src/motion/director.ts`) — the spine's current
 * rung moves as the fly climbs the ladder, so that one has to be re-measured rather than derived.
 * `flyHead` keeps its derived point because the fly is inside a canvas and has no element to
 * measure.
 */
export type MotionAnchor =
  | 'flyHead'
  | 'sugarChip'
  | 'badgeCount'
  | 'spineRung'
  | 'tickerLine'
  | 'tabSlotCentre'
  | 'railCentre'
  | 'titleStripCentre'
  | 'gameCentre';

function centre(box: Rect): { x: number; y: number } {
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

export const MOTION_ANCHORS: Record<MotionAnchor, { x: number; y: number }> = {
  // The fly's head is high and left of centre in the strip (`docs/design/fly-avatar.md`).
  flyHead: { x: MOTION_REGIONS.flyStrip.x + 300, y: MOTION_REGIONS.flyStrip.y + 70 },
  // SUGAR READY chip: bottom right of the compact progress cluster, per rail v2's field order.
  sugarChip: {
    x: MOTION_REGIONS.progressCluster.x + MOTION_REGIONS.progressCluster.width - 120,
    y: MOTION_REGIONS.progressCluster.y + MOTION_REGIONS.progressCluster.height - 34,
  },
  // "badges · places" sits mid-cluster.
  badgeCount: { x: MOTION_REGIONS.progressCluster.x + 640, y: MOTION_REGIONS.progressCluster.y + 88 },
  // The 38-rung spine runs across the cluster under the rung name.
  spineRung: { x: MOTION_REGIONS.progressCluster.x + 300, y: MOTION_REGIONS.progressCluster.y + 108 },
  // The ticker's top line, which is the one that just slid up.
  tickerLine: { x: MOTION_REGIONS.ticker.x + 120, y: MOTION_REGIONS.ticker.y + 30 },
  tabSlotCentre: centre(MOTION_REGIONS.tabSlot),
  railCentre: centre(MOTION_REGIONS.rail),
  titleStripCentre: centre(MOTION_REGIONS.titleStrip),
  gameCentre: centre(MOTION_REGIONS.game),
};

/**
 * Colour tokens, as CSS custom properties with a fallback.
 *
 * The canvases already resolve their colours out of the theme this way (`src/theme/colors.ts`), so
 * a theme swap moves the particles with the panels and there is still exactly one definition of
 * each colour in `src/theme/tokens.css`. The fallbacks are the same literals that file's palette
 * uses, which is also what makes the recipes testable and drawable without a DOM.
 */
export type ColourToken = 'amber' | 'sugar' | 'cool' | 'ink';

export const MOTION_COLOURS: Record<ColourToken, { cssVar: string; fallback: string }> = {
  // The stage's one accent: every reward, rung and badge is this colour.
  amber: { cssVar: '--accent', fallback: '#ffb020' },
  // Dopamine / PAM pink, used by the sugar ring and the sugar drift.
  sugar: { cssVar: '--dopamine', fallback: '#f472a8' },
  // Sensory blue: the rollback's rewind field, which is the one cold moment on the stage.
  cool: { cssVar: '--sensory', fallback: '#4fc3f7' },
  ink: { cssVar: '--ink-1', fallback: '#c8d0dc' },
};

/** Resolved colour per token. Falls back to the literals with no DOM (tests, Node tools). */
export type MotionPalette = Record<ColourToken, string>;

export function resolveMotionColours(root?: Element): MotionPalette {
  const palette: MotionPalette = {
    amber: MOTION_COLOURS.amber.fallback,
    sugar: MOTION_COLOURS.sugar.fallback,
    cool: MOTION_COLOURS.cool.fallback,
    ink: MOTION_COLOURS.ink.fallback,
  };
  const element = root ?? (typeof document === 'undefined' ? null : document.documentElement);
  if (!element || typeof getComputedStyle === 'undefined') return palette;
  const style = getComputedStyle(element);
  for (const token of Object.keys(MOTION_COLOURS) as ColourToken[]) {
    const value = style.getPropertyValue(MOTION_COLOURS[token].cssVar).trim();
    if (value) palette[token] = value;
  }
  return palette;
}

/**
 * One emitter call, as data.
 *
 * `count` is the count at full intensity; the engine multiplies it by the trigger's `intensity`, so
 * the addendum's "a few sparks from the ticker line scaled by value" is one number here and no code.
 * `delayMs` is measured from the moment's start, which is how the badge's shockwave lands *after*
 * the fountain has left the ground.
 */
export type EmitterSpec =
  | {
      emitter: 'sparks';
      at: MotionAnchor;
      count: number;
      colour: ColourToken;
      /** Cone axis, canvas radians. */
      dir: number;
      /** Cone width, radians. */
      spread: number;
      speed?: number;
      delayMs?: number;
    }
  | { emitter: 'fountain'; over: MotionRegion; count: number; colour: ColourToken; delayMs?: number }
  | { emitter: 'shockwave'; at: MotionAnchor; colour: ColourToken; radius: number; delayMs?: number }
  | {
      emitter: 'streamField';
      over: MotionRegion;
      dir: number;
      count: number;
      colour: ColourToken;
      speed?: number;
      delayMs?: number;
    }
  | { emitter: 'drift'; from: MotionAnchor; to: MotionAnchor; count: number; colour: ColourToken; delayMs?: number };

/**
 * Default RNG seed for the particle field.
 *
 * Fixed rather than time-based so the same moment produces the same burst in the harness, in the
 * contact sheets (`tools/motion-strip.mts`) and in a Playwright screenshot diff. A 24/7 broadcast
 * has no reason to want a different badge every time either: nobody sees two badges side by side.
 */
export const MOTION_SEED = 0x0f1ab1e5;

/** Which tab the slot takes focus on for the duration of a moment, per the catalogue. */
export type TabFocus = 'SENSES' | 'CONNECTOME' | 'LADDER';

export interface MomentRecipe {
  type: MomentType;
  /** Regions the moment animates. The later wiring uses this to know what to mark dirty. */
  regions: readonly MotionRegion[];
  /** Arrival / hold / exit, from `moments.ts` so the queue and the renderer cannot disagree. */
  timing: MomentTiming;
  emitters: readonly EmitterSpec[];
  sfx: SfxTier;
  /** Tab the slot is pinned to while the moment holds, if any. */
  focusTab?: TabFocus;
  /** Whether a caption band enters over the tab slot. */
  caption: boolean;
  /** The catalogue row in `docs/design/animation.md` this is a transcription of. */
  note: string;
}

/**
 * The catalogue.
 *
 * Read each row against the design's table. Where a row here is quieter than the prose, that is the
 * "never busy" half of the intensity brief: the whole-frame paint p95 has to stay under 4 ms with
 * the game, the retina, the brain map and the fly all painting too, so no moment emits more than
 * about a third of the 400-particle pool except the badge, which is allowed the frame.
 */
export const MOMENT_CATALOGUE: Record<MomentType, MomentRecipe> = {
  badge: {
    type: 'badge',
    regions: ['rail', 'progressCluster', 'tabSlot', 'ticker'],
    timing: MOMENT_TIMING.badge,
    emitters: [
      // "a larger fountain over the rail"
      { emitter: 'fountain', over: 'rail', count: 120, colour: 'amber' },
      // "plus a shockwave ring from the badge count", after the fountain has cleared the edge
      { emitter: 'shockwave', at: 'badgeCount', colour: 'amber', radius: 240, delayMs: 120 },
      // the badge count's own 1.15x bounce, punctuated
      { emitter: 'sparks', at: 'badgeCount', count: 18, colour: 'amber', dir: DIR.up, spread: Math.PI, delayMs: 120 },
    ],
    sfx: 'fanfare',
    focusTab: 'LADDER',
    caption: true,
    note: 'Badge: rail border flash, badge count bounce, connectome flare, caption band, LADDER focus 9 s.',
  },
  milestone: {
    type: 'milestone',
    regions: ['tabSlot', 'progressCluster', 'ticker'],
    timing: MOMENT_TIMING.milestone,
    emitters: [
      // "milestone spawns amber sparks from the new rung"
      { emitter: 'sparks', at: 'spineRung', count: 28, colour: 'amber', dir: DIR.up, spread: Math.PI / 2 },
      // the rung "pulses twice then fills amber": the second pulse gets its own few sparks
      { emitter: 'sparks', at: 'spineRung', count: 12, colour: 'amber', dir: DIR.up, spread: Math.PI / 3, delayMs: 260 },
    ],
    sfx: 'chime',
    focusTab: 'LADDER',
    caption: true,
    note: 'Milestone: caption band in from the left, spine rung pulses twice then fills, LADDER focus 9 s.',
  },
  rollback: {
    type: 'rollback',
    regions: ['game', 'tabSlot'],
    timing: MOMENT_TIMING.rollback,
    emitters: [
      // "rollback spawns a backward-streaming line field over the game"
      { emitter: 'streamField', over: 'game', dir: DIR.left, count: 90, colour: 'cool', speed: 1.4 },
    ],
    sfx: 'rewind',
    focusTab: 'LADDER',
    caption: true,
    note: 'Rollback: 400 ms horizontal rewind wipe with scanline flicker, "REWIND · try 3".',
  },
  sugar: {
    type: 'sugar',
    regions: ['flyStrip', 'progressCluster', 'ticker'],
    timing: MOMENT_TIMING.sugar,
    emitters: [
      // "sugar spawns a burst of small warm sparks that drift from the fly's head toward the sugar chip"
      { emitter: 'drift', from: 'flyHead', to: 'sugarChip', count: 34, colour: 'sugar' },
      // the head glow "blooms then decays with the PAM rate"
      { emitter: 'sparks', at: 'flyHead', count: 10, colour: 'sugar', dir: DIR.up, spread: Math.PI, delayMs: 60 },
    ],
    sfx: 'tone',
    caption: false,
    note: 'Sugar: proboscis extends 400 ms, head glow blooms, sugar ring fills then drains over the cooldown.',
  },
  reward: {
    type: 'reward',
    regions: ['ticker'],
    timing: MOMENT_TIMING.reward,
    emitters: [
      // "rewards spawn a few sparks from the ticker line scaled by value" — count x intensity
      // 20 at full value, 7 at the lowest tier: "a few sparks … scaled by value", with the bottom
      // of the scale still legible at the 27 px body floor.
      { emitter: 'sparks', at: 'tickerLine', count: 20, colour: 'amber', dir: DIR.up, spread: (Math.PI * 2) / 3, speed: 0.12 },
    ],
    sfx: 'tick',
    caption: false,
    note: 'Small reward: ticker line slides up 240 ms, value flashes amber 120/600 ms.',
  },
  dayRollover: {
    type: 'dayRollover',
    regions: ['titleStrip'],
    timing: MOMENT_TIMING.dayRollover,
    emitters: [
      // The strip is 40 px tall, so the only particle that fits is a thin trail behind the slide.
      { emitter: 'sparks', at: 'titleStripCentre', count: 10, colour: 'ink', dir: DIR.right, spread: Math.PI / 8, speed: 0.22 },
    ],
    sfx: 'stinger',
    caption: false,
    note: 'Day rollover: "DAY N" slides across the strip once, 2 s.',
  },
  modeChange: {
    type: 'modeChange',
    regions: ['titleStrip'],
    timing: MOMENT_TIMING.modeChange,
    emitters: [],
    sfx: 'silent',
    caption: false,
    note: 'Mode change: chip text swaps with a 200 ms vertical roll. No sound, no particles.',
  },
};

/** The recipe for a type. Total, so the engine never has to null-check a row. */
export function recipeFor(type: MomentType): MomentRecipe {
  return MOMENT_CATALOGUE[type];
}

/** Every region any moment can animate, for a renderer that wants to size one canvas layer. */
export function regionsUnion(): readonly MotionRegion[] {
  const seen = new Set<MotionRegion>();
  for (const recipe of Object.values(MOMENT_CATALOGUE)) for (const region of recipe.regions) seen.add(region);
  return [...seen];
}
