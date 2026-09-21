/**
 * The director: the one place that turns the motion engine and the tab controller into pixels on
 * the rail.
 *
 * The engine (`src/motion/engine.ts`) decides *what is on stage* and draws the particle layer.
 * It deliberately stops there — it owns no panel and no DOM — so this is the other half: the part
 * the engine's own header calls "the rail rework that wires the panels up".
 *
 * What it does, once per animation frame:
 *
 *   - hands the engine the emission points it can only get from the DOM (`setAnchors`)
 *   - runs the tab controller and paints the strip: underline, crossfade, which pane is visible
 *   - gives a moment's tab the slot for the moment's own duration, which is what replaced layout
 *     v1's promotion of the brain map over the rail
 *   - flips the attribute effects: rail flash, spine pulse, counter bounce, thumbnail blink,
 *     HERE FOR warmth and its threshold pulse, the day slide
 *   - starts the connectome flare and the game's rewind wipe
 *   - drains the engine's sound cues into the page's `AudioContext`
 *   - clears the particle canvas and lets the engine draw it
 *   - steps every lerped readout (`src/motion/readouts.ts`)
 *
 * **Two clocks.** `nowMs` is the page's data clock, which a held fixture seek freezes so a
 * screenshot is reproducible; the moment queue, the readouts and the stall meter all run on it.
 * `rawNowMs` is `performance.now()`, which never stops, and it drives the fixed-length DOM
 * animations (a 120 ms flash, a 400 ms wipe) so a frozen frame still shows a settled end state
 * rather than an animation pinned mid-flight.
 *
 * Effects are attribute flips plus CSS keyframes (`src/theme/motion.css`), never JS-driven style
 * animation: compositor-only properties, a static end state in a frozen frame, and no layout on
 * the broadcast's critical path.
 */
import type { SfxName } from '@/audio/sfx';
import { hot, useStage } from '@/feed/store';
import type { GameConfig } from '@/games';
import { circuitFraction } from '@/lib/circuit-scale';
import { rungCount } from '@/lib/ladder';
import { type Box, LAYOUT, RAIL_BOX, STAGE_WIDTH } from '@/lib/geometry';
import { CIRCUIT_GROUPS } from '@/lib/labels';
import { STALL_WINDOW_SECONDS } from '@/lib/stall';
import { TabController, TABS, type TabId } from '@/lib/tabs';
import type { BrainMapSurface } from '@/paint/brainmap';
import type { GameSurface } from '@/paint/game';
import type { CanvasPalette } from '@/theme/colors';
import { type MotionAnchor, recipeFor, type TabFocus } from './catalogue';
import type { MotionEngine } from './engine';
import { clamp01, cssSteps, EASINGS, PIXEL_STEP, quantize } from './lerp';
import type { ActiveMoment } from './moments';
import type { RailSignals } from './rail-signals';
import { BOUNCE_MS, CROSSFADE_MS, DAY_SLIDE_MS, FLASH_UP_MS, hereForTier, SPINE_PULSE_MS } from './rail-motion';
import { Readouts, type ReadoutInputs } from './readouts';

/** Roles whose mean bar fill is the "command activity" the tab steering reads. */
const DRIVE_ROLES: readonly string[] =
  CIRCUIT_GROUPS.find((group) => group.id === 'drive')?.bars.map((bar) => bar.role) ?? [];

/** The catalogue's tab names, as `src/lib/tabs.ts` spells them. */
const FOCUS_TAB: Record<TabFocus, TabId> = {
  SENSES: 'senses',
  CONNECTOME: 'connectome',
  LADDER: 'ladder',
};

/**
 * Where a moment is allowed to paint particles.
 *
 * The layer's canvas spans the whole stage, because the sugar burst "drift[s] from the fly's head
 * toward the sugar chip" (`docs/design/animation.md`, addendum) and that path starts at x=448 in
 * the left column and ends at x=1790 in the rail — a rail-sized canvas cannot draw it. What keeps
 * that from becoming a licence to paint over the broadcast is this clip: the rail and the fly
 * strip, and nothing else. The game canvas and the title strip are outside it, so the badge's
 * shockwave stops at the rail's edge instead of crossing the game, which is the one thing a
 * broadcast overlay must never do.
 */
export const PARTICLE_CLIP: readonly Box[] = [RAIL_BOX, LAYOUT.flyStrip];

/** Selectors for the elements whose measured centres replace the catalogue's default anchors. */
const ANCHOR_SELECTORS: Partial<Record<MotionAnchor, string>> = {
  sugarChip: '[data-testid="sugar-chip"]',
  badgeCount: '[data-readout="counters"]',
  tickerLine: '.ticker-row',
};

export interface DirectorOptions {
  /** Where the director queries for its elements: `#stage`. */
  root: HTMLElement;
  game: GameConfig;
  engine: MotionEngine;
  signals: RailSignals;
  palette: CanvasPalette;
  map: BrainMapSurface;
  gameSurface: GameSurface;
  /** The particle layer's context, cleared here and drawn by the engine. */
  particleCtx: CanvasRenderingContext2D | null;
  /** Pinned tab from `?tab=`, or null to cycle. */
  forcedTab: TabId | null;
  playSfx: ((name: SfxName, gain: number) => void) | null;
}

export class Director {
  private readonly tabs: TabController;
  private readonly readouts: Readouts;

  private railFlashUntil = Number.NEGATIVE_INFINITY;
  private spinePulseUntil = Number.NEGATIVE_INFINITY;
  private spinePulseRung = -1;
  private bounceUntil = Number.NEGATIVE_INFINITY;
  private hereForPulseUntil = Number.NEGATIVE_INFINITY;
  private daySlideUntil = Number.NEGATIVE_INFINITY;
  private hereForWarm = 0;
  private lastMomentKey = '';
  private firstFrame = true;
  private lastRank = -1;
  /** The tab the last frame painted, or null on a first frame and after a seek. */
  private paintedTab: TabId | null = null;
  /** Wall-clock start of the running crossfade; -Infinity means "already settled". */
  private crossfadeFromMs = Number.NEGATIVE_INFINITY;

  constructor(private readonly options: DirectorOptions) {
    this.tabs = new TabController({ forced: options.forcedTab });
    this.readouts = new Readouts(options.root, options.game);
  }

  /** What the slot is showing and why, for `window.__stage`. */
  state(): {
    tab: TabId;
    reason: string;
    focused: boolean;
    particles: number;
    moment: string | null;
    momentPhase: string | null;
    pending: number;
  } {
    const snapshot = this.options.engine.snapshot();
    return {
      tab: this.tabs.current,
      reason: this.tabs.reason,
      focused: this.tabs.focused,
      particles: this.options.engine.field.live,
      moment: snapshot.active?.type ?? null,
      momentPhase: snapshot.active?.phase ?? null,
      pending: snapshot.pending,
    };
  }

  /** A fixture seek: nothing that was in flight happened. */
  reset(): void {
    this.railFlashUntil = Number.NEGATIVE_INFINITY;
    this.spinePulseUntil = Number.NEGATIVE_INFINITY;
    this.bounceUntil = Number.NEGATIVE_INFINITY;
    this.hereForPulseUntil = Number.NEGATIVE_INFINITY;
    this.daySlideUntil = Number.NEGATIVE_INFINITY;
    this.lastMomentKey = '';
    this.firstFrame = true;
    this.paintedTab = null;
    this.crossfadeFromMs = Number.NEGATIVE_INFINITY;
  }

  /** One frame. `nowMs` is the data clock, `rawNowMs` the wall clock. */
  frame(nowMs: number, rawNowMs: number, dtMs: number): void {
    const state = useStage.getState();
    const engine = this.options.engine;

    // 1. Emission points, measured. The catalogue ships arithmetic defaults; these are the real
    //    elements, and the spine's current rung moves every time the fly gains one.
    this.updateAnchors(state.milestone.rank);

    // 2. The queue and the particle simulation, on the data clock.
    engine.tick(nowMs);
    const active = engine.active(nowMs);

    // 3. Everything a newly-arrived moment does to the DOM, once each.
    if (active) this.onMoment(active, rawNowMs, state.milestone.rank);
    else this.lastMomentKey = '';

    // 4. Sound. The engine queues cues; the page owns the AudioContext.
    for (let cue = engine.takeCue(); cue; cue = engine.takeCue()) {
      if (cue.cue.sfx) this.options.playSfx?.(cue.cue.sfx, cue.cue.gain);
    }

    // 5. The tab slot.
    const steering = this.options.signals.takeSteering();
    if (active) {
      const focus = recipeFor(active.type).focusTab;
      if (focus) this.tabs.focusOn(FOCUS_TAB[focus], active.endsMs, nowMs);
    }
    const tab = this.tabs.update(nowMs, {
      walking: state.mode === 'OVERWORLD',
      commandActivity: driveActivity(),
      reward: steering.reward,
      rankChanged: steering.rankChanged,
      newChatter: steering.newChatter,
    });
    this.paintTabs(tab, rawNowMs);

    // 6. Attribute effects with a deadline, and the two that are functions of a value.
    this.expire(rawNowMs);
    this.paintHereFor(state.milestone.sinceSeconds, rawNowMs);

    // 7. Numbers.
    const inputs = this.readInputs(nowMs, state);
    if (this.firstFrame) {
      this.readouts.snap(inputs);
      this.firstFrame = false;
    }
    this.readouts.update(inputs, dtMs);

    // 8. The particle layer, last, so it draws over everything it was aimed at. The engine's
    //    painter contract has no `clearRect`, so the clear is the page's job — and it is skipped
    //    entirely on an idle frame, which is almost all of them.
    this.paintParticles();
  }

  // -- Moments ---------------------------------------------------------------------------------

  /**
   * The DOM half of one moment, applied once.
   *
   * Keyed by id *and* coalesce count, the same key the engine uses for its emitters, so a second
   * sugar folding into the first re-pulses the ring as well as re-firing the sparks.
   */
  private onMoment(active: ActiveMoment, rawNowMs: number, rank: number): void {
    const key = `${String(active.id)}:${String(active.count)}`;
    if (key === this.lastMomentKey) return;
    this.lastMomentKey = key;

    const intensity = clamp01(active.trigger.intensity);

    switch (active.type) {
      case 'badge':
        this.railFlashUntil = rawNowMs + FLASH_UP_MS;
        this.bounceUntil = rawNowMs + BOUNCE_MS;
        this.pulseSpine(rank, rawNowMs);
        this.options.map.flare(rawNowMs, 1);
        break;
      case 'milestone':
        this.pulseSpine(rank, rawNowMs);
        break;
      case 'rollback':
        this.options.gameSurface.startRewind(rawNowMs);
        break;
      case 'sugar':
        this.options.map.flare(rawNowMs, 1);
        break;
      case 'reward':
        // The flare scales with the reward: a +0.05 exploration tick is a ripple, a story beat is
        // a wave. `intensity` is already the tier scale (`src/motion/moments.ts`).
        this.options.map.flare(rawNowMs, 0.3 + 0.7 * intensity);
        break;
      case 'dayRollover':
        this.daySlideUntil = rawNowMs + DAY_SLIDE_MS;
        this.setDayText(active.trigger.label);
        break;
      case 'modeChange':
        // The chip's vertical roll is a CSS animation keyed on the mode in `TitleStrip`, so React
        // owns it: the mode is a value it already renders, and a second driver would fight it.
        break;
    }
  }

  private pulseSpine(rank: number, rawNowMs: number): void {
    this.spinePulseRung = rank;
    this.spinePulseUntil = rawNowMs + SPINE_PULSE_MS;
  }

  /** Clear the particle layer's last frame and let the engine draw the new one. */
  private paintParticles(): void {
    const ctx = this.options.particleCtx;
    if (!ctx) return;
    const engine = this.options.engine;
    const live = engine.field.live;
    if (live === 0 && this.drawnParticles === 0) return;
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    ctx.save();
    ctx.beginPath();
    for (const box of PARTICLE_CLIP) ctx.rect(box.x, box.y, box.width, box.height);
    ctx.clip();
    engine.draw(ctx);
    ctx.restore();
    this.drawnParticles = live;
  }

  private drawnParticles = 0;

  /**
   * Push the measured emission points into the engine.
   *
   * Cheap and idempotent: three `getBoundingClientRect` calls plus one for the rung, and only when
   * the rung has actually changed. Measured rather than recomputed from `geometry.ts` because the
   * rung's x is a flex division of the spine's width, and duplicating that arithmetic here is how
   * the sparks end up two rungs off after a padding change.
   */
  private updateAnchors(rank: number): void {
    const anchors: Partial<Record<MotionAnchor, { x: number; y: number }>> = {};
    for (const [anchor, selector] of Object.entries(ANCHOR_SELECTORS) as [MotionAnchor, string][]) {
      const point = this.pointOf(selector);
      if (point) anchors[anchor] = point;
    }
    if (rank !== this.lastRank) {
      this.lastRank = rank;
      const rung = this.pointOf(`[data-rung="${rank}"]`);
      if (rung) anchors.spineRung = rung;
    }
    if (Object.keys(anchors).length > 0) this.options.engine.setAnchors(anchors);
  }

  /** Centre of an element, in stage coordinates. */
  private pointOf(selector: string): { x: number; y: number } | null {
    const element = this.options.root.querySelector(selector);
    if (!element) return null;
    const rect = element.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) return null;
    const stage = this.options.root.getBoundingClientRect();
    const scale = stage.width / STAGE_WIDTH || 1;
    return {
      x: (rect.left - stage.left + rect.width / 2) / scale,
      y: (rect.top - stage.top + rect.height / 2) / scale,
    };
  }

  // -- Attribute effects ------------------------------------------------------------------------

  private expire(rawNowMs: number): void {
    this.flag('[data-rail-flash]', 'railFlash', rawNowMs < this.railFlashUntil);
    this.flag('[data-day-slide]', 'daySlide', rawNowMs < this.daySlideUntil);

    const counters = this.options.root.querySelector<HTMLElement>('[data-readout="counters"]');
    if (counters) counters.dataset.bounce = rawNowMs < this.bounceUntil ? '1' : '0';

    const pulsing = rawNowMs < this.spinePulseUntil ? this.spinePulseRung : -1;
    const lit = this.options.root.querySelector<HTMLElement>('[data-rung][data-pulse="1"]');
    if (lit && Number(lit.dataset.rung) !== pulsing) lit.dataset.pulse = '0';
    if (pulsing >= 0) {
      const target = this.options.root.querySelector<HTMLElement>(`[data-rung="${pulsing}"]`);
      if (target) target.dataset.pulse = '1';
    }
  }

  private flag(selector: string, attribute: string, on: boolean): void {
    const element = this.options.root.querySelector<HTMLElement>(selector);
    if (!element) return;
    const value = on ? '1' : '0';
    if (element.dataset[attribute] === value) return;
    element.dataset[attribute] = value;
  }

  private setDayText(label: string): void {
    const element = this.options.root.querySelector<HTMLElement>('[data-day-text]');
    if (element) element.textContent = label;
  }

  /**
   * HERE FOR: warmer at each threshold, and a single pulse on the crossing.
   *
   * The warmth is a function of the *value*, not of an event, so a page that loads into a fly that
   * has been stuck for four hours is already warm rather than waiting for a crossing that happened
   * before it started. The pulse is the crossing, detected here rather than queued as a moment:
   * the number is on screen all the time, and the catalogue's row for it is a pulse on one
   * element, not a moment that owns the frame.
   */
  private paintHereFor(sinceSeconds: number, rawNowMs: number): void {
    const element = this.options.root.querySelector<HTMLElement>('[data-here-for]');
    if (!element) return;
    const warm = hereForTier(sinceSeconds);
    if (warm !== this.hereForWarm) {
      // Only a crossing *upward* pulses: a rank-up resets `sinceSeconds` to zero, and that is a
      // milestone's moment, not a HERE FOR one.
      if (warm > this.hereForWarm) this.hereForPulseUntil = rawNowMs + SPINE_PULSE_MS / 2;
      this.hereForWarm = warm;
      element.dataset.warm = String(warm);
    }
    const pulse = rawNowMs < this.hereForPulseUntil ? '1' : '0';
    if (element.dataset.pulse !== pulse) element.dataset.pulse = pulse;
  }

  // -- Tabs -------------------------------------------------------------------------------------

  /**
   * The tab strip and the crossfade.
   *
   * Exactly one pane carries `data-visible="1"` at any instant, including mid-crossfade: the
   * outgoing pane is `data-visible="0"` with a falling opacity, which is what the structural test
   * counts. A pane that is neither is `visibility: hidden` and therefore out of paint entirely,
   * while staying mounted so the connectome keeps its worker-rasterised base bitmap.
   */
  private paintTabs(tab: TabId, rawNowMs: number): void {
    const previous = this.paintedTab;
    if (previous !== tab) {
      // A first frame is an arrival, not a transition, so it starts already finished.
      this.crossfadeFromMs = previous === null ? Number.NEGATIVE_INFINITY : rawNowMs;
      this.paintedTab = tab;
    }

    // The wall clock, not the data clock — the rule this file's header states for every
    // fixed-length DOM animation, and the crossfade is one. On the data clock a held fixture seek
    // froze the fade at zero, which paints the *outgoing* panes at full opacity and the incoming
    // one at nothing: the brain map showing through whichever tab was pinned, which is what the
    // cold-open screenshot baselines had been quietly recording. On the wall clock the fade
    // finishes 300 ms after the change however the page's own clock is behaving, so a frozen frame
    // is a settled frame.
    const linear = clamp01((rawNowMs - this.crossfadeFromMs) / CROSSFADE_MS);
    const eased = EASINGS.inOutCubic(linear);

    for (const id of TABS) {
      const pane = this.options.root.querySelector<HTMLElement>(`[data-tab-pane="${id}"]`);
      if (!pane) continue;
      const isCurrent = id === tab;
      const opacity = isCurrent ? eased : linear < 1 ? 1 - eased : 0;
      const visible = isCurrent ? '1' : '0';

      if (pane.dataset.visible !== visible) pane.dataset.visible = visible;
      pane.style.opacity = opacity.toFixed(3);
      pane.style.visibility = opacity > 0.001 ? 'visible' : 'hidden';

      const button = this.options.root.querySelector<HTMLElement>(`[data-tab="${id}"]`);
      if (button && button.dataset.active !== visible) button.dataset.active = visible;
    }

    this.paintUnderline(tab);
  }

  /**
   * Slide the amber underline to the active tab, in 4 px steps.
   *
   * `docs/design/gameboy-theme.md` puts the tab underline on the stepped easing, and CSS can only
   * quantise *time*, so the step count has to be a function of the distance — which is why this is
   * set here rather than taken from the `--ease-pixel` token the fixed-distance elements use. The
   * strip's tabs are 170 to 290 px wide, so a cycle moves the underline anywhere from 180 to 470 px
   * and one constant would be right for none of them.
   *
   * The destination is quantised as well, so the underline comes to rest on the grid and not half a
   * pixel off it.
   */
  private paintUnderline(tab: TabId): void {
    const button = this.options.root.querySelector<HTMLElement>(`[data-tab="${tab}"]`);
    const underline = this.options.root.querySelector<HTMLElement>('[data-tab-underline]');
    const strip = button?.parentElement;
    if (!button || !underline || !strip) return;

    const stageRect = this.options.root.getBoundingClientRect();
    const scale = stageRect.width / STAGE_WIDTH || 1;
    const buttonRect = button.getBoundingClientRect();
    const left = quantize((buttonRect.left - strip.getBoundingClientRect().left) / scale, PIXEL_STEP);
    const width = quantize(buttonRect.width / scale, PIXEL_STEP);

    const transform = `translateX(${left}px)`;
    if (underline.style.transform !== transform) {
      const easing = `steps(${cssSteps(Math.abs(left - this.underlineLeft))}, end)`;
      if (underline.style.transitionTimingFunction !== easing) {
        underline.style.transitionTimingFunction = easing;
      }
      this.underlineLeft = left;
      underline.style.transform = transform;
    }
    const cssWidth = `${width}px`;
    if (underline.style.width !== cssWidth) underline.style.width = cssWidth;
  }

  /** Where the underline was last put, so the next move knows how far it is going. */
  private underlineLeft = 0;

  // -- Readouts ---------------------------------------------------------------------------------

  private readInputs(nowMs: number, state: ReturnType<typeof useStage.getState>): ReadoutInputs {
    const signals = this.options.signals;
    const total = rungCount(state.milestone.total, this.options.game.milestoneLadder.length);
    const rank = Math.max(0, Math.min(total - 1, state.milestone.rank));

    return {
      hz: state.populationRate,
      hereForSeconds: state.milestone.sinceSeconds,
      tries: state.milestone.attempts + 1,
      runSeconds: state.runSeconds,
      badges: state.badges,
      places: state.uniqueLocations,
      rewardTotal: state.rewardTotal,
      spikes: state.spikeCount,
      sugarFraction: state.sugar.cooldownMs > 0 ? Math.min(1, state.sugar.cooldownMs / 10_000) : 0,
      stallSeconds: signals.stall.secondsSince(nowMs),
      stallFraction: signals.stall.fraction(nowMs, STALL_WINDOW_SECONDS),
      rollbacksThisRung: state.milestone.attempts,
      rollbacksLifetime: signals.rollbacks.sinceLoad,
      rollbackAgeSeconds: signals.rollbacks.secondsSinceLast(nowMs),
      rank,
      total,
      // The word RUNG is the cluster's own label (`ProgressCluster`), so the line carries the
      // count alone: `12/37`.
      rungText: `${rank}/${Math.max(0, total - 1)}`,
    };
  }
}

/** Mean fill of the drive bars: the tab steering's "high command activity". */
function driveActivity(): number {
  if (DRIVE_ROLES.length === 0) return 0;
  let sum = 0;
  for (const role of DRIVE_ROLES) {
    sum += circuitFraction(hot.rates[role] ?? 0, hot.circuitReferenceHz[role] ?? 30);
  }
  return sum / DRIVE_ROLES.length;
}

/** The fly's head, in stage pixels: the sugar drift's origin, which has no element to measure. */
export const FLY_HEAD_ANCHOR = {
  x: LAYOUT.flyStrip.x + LAYOUT.flyStrip.width / 2,
  y: LAYOUT.flyStrip.y + 96,
} as const;
