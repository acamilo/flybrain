/**
 * Every number on the rail, lerped, written by the paint loop.
 *
 * `docs/design/animation.md`'s addendum: "Every displayed number and bar is lerped toward its
 * target each frame (rates, Hz, counters, spine fill, sugar ring), never snapped; time constants
 * 120 to 300 ms so motion is continuous between 30 Hz feed updates."
 *
 * React cannot do that. It commits at 4 Hz by design (`src/feed/store.ts`), so a number it owns
 * moves four times a second in visible steps. So the components render the *structure* and leave
 * the numeric elements empty, marked `data-readout="<key>"`, and this module owns their text: one
 * `Smoothed` per key, stepped every animation frame, written only when the formatted string
 * actually changes (which is what keeps a 60 Hz loop from doing 20 DOM writes a frame for a
 * number that has not moved a digit).
 *
 * Three numbers are deliberately *not* lerped, and they are the discrete ones: the rung index,
 * the try count and the rollback budgets. A spine that reads "RUNG 4.6/37" for a quarter of a
 * second is not smooth, it is wrong; a step that happens at most once an hour does not need
 * easing.
 *
 * The same pass writes the two lerped *shapes* — the sugar cooldown ring and the stall bar — and
 * the spine's per-rung fill, because they are the same problem: a value between two feed frames.
 */
import {
  dayNumber,
  formatClock,
  formatCount,
  formatCounters,
  formatDurationCompact,
  formatHz,
  formatMinutesSeconds,
} from '@/lib/format';
import type { GameConfig } from '@/games';
import { DATASET_COPY } from '@/lib/labels';
import { ATTEMPTS_PER_RUNG, LIFETIME_BUDGET, STALL_WINDOW_SECONDS } from '@/lib/stall';
import { BAR_STEP, PIXEL_STEP, quantizePixels, Smoothed } from './lerp';

/**
 * Time constants, from the addendum's 120-300 ms band.
 *
 * Three rather than one: a bar and a ring have to feel keyed to the data, a rate reads best with a
 * little inertia, and a counter or a clock is meant to be seen crawling.
 */
const TAU = { fast: 120, mid: 200, slow: 300 } as const;

/** Everything the readouts need, gathered once per frame by the director. */
export interface ReadoutInputs {
  hz: number;
  hereForSeconds: number;
  tries: number;
  runSeconds: number;
  badges: number;
  places: number;
  rewardTotal: number;
  spikes: number;
  /** 0..1 of the sugar cooldown still to run. */
  sugarFraction: number;
  stallSeconds: number;
  /** 0..1 through the stall window. */
  stallFraction: number;
  rollbacksThisRung: number;
  rollbacksLifetime: number;
  /** Seconds since the last rollback, or null when there has not been one. */
  rollbackAgeSeconds: number | null;
  /** 0-based current rung, for the spine fill. */
  rank: number;
  /** Rungs in the spine. */
  total: number;
  /** `RUNG 12/37`. Discrete, so it is written as given. */
  rungText: string;
}

/** Circumference of the sugar chip's ring, from `src/panels/ProgressCluster.tsx`. */
const RING_CIRCUMFERENCE = 2 * Math.PI * 10;

/**
 * The spine cell's height and the floor its fill grows from, mirroring `.ladder-rung` in
 * `src/theme/panels.css`.
 *
 * 16 px and a 0.5 floor, so the growing half is 8 px: on the theme's 4 px grid that is exactly
 * three poses — 8, 12 and 16 px — and a rank-up steps through them rather than sweeping, which is
 * `docs/design/gameboy-theme.md`'s sprite motion. The numbers are here as well as in the CSS
 * because CSS cannot quantise a `calc`, so the quantising is this side's job and the two have to
 * agree; a change to the cell's height belongs in both.
 */
const SPINE_CELL_PX = 16;
const SPINE_FLOOR = 0.5;

/** A missing value. An em dash, not a zero: "no rollbacks yet" is not "0 s ago". */
const NONE = '—';

export class Readouts {
  private readonly hz = new Smoothed(0, TAU.mid);
  private readonly hereFor = new Smoothed(0, TAU.slow);
  private readonly tries = new Smoothed(1, TAU.slow);
  private readonly run = new Smoothed(0, TAU.slow);
  private readonly badges = new Smoothed(0, TAU.slow);
  private readonly places = new Smoothed(0, TAU.slow);
  private readonly spikes = new Smoothed(0, TAU.mid);
  private readonly sugar = new Smoothed(0, TAU.fast);
  private readonly stall = new Smoothed(0, TAU.slow);
  private readonly stallBar = new Smoothed(0, TAU.fast);
  private readonly rollbackAge = new Smoothed(0, TAU.slow);

  /** Per-rung lerped fill, so a rank-up lights its rung over ~300 ms instead of snapping. */
  private rungFill = new Float32Array(0);

  private readonly elements = new Map<string, HTMLElement>();
  private readonly painted = new Map<string, string>();
  private rungCells: HTMLElement[] = [];
  private sugarRing: SVGCircleElement | null = null;
  private stallFill: HTMLElement | null = null;
  /** Measured width of the stall bar's track, for the 8 px fill quantisation. */
  private stallTrackPx = 0;

  constructor(
    private readonly root: ParentNode,
    private readonly game: GameConfig,
  ) {}

  /** Jump every value to its target with no motion: a fixture seek did not watch it happen. */
  snap(inputs: ReadoutInputs): void {
    this.hz.snap(inputs.hz);
    this.hereFor.snap(inputs.hereForSeconds);
    this.tries.snap(inputs.tries);
    this.run.snap(inputs.runSeconds);
    this.badges.snap(inputs.badges);
    this.places.snap(inputs.places);
    this.spikes.snap(inputs.spikes);
    this.sugar.snap(inputs.sugarFraction);
    this.stall.snap(inputs.stallSeconds);
    this.stallBar.snap(inputs.stallFraction);
    this.rollbackAge.snap(inputs.rollbackAgeSeconds ?? 0);
    this.rungFill = new Float32Array(0);
  }

  /** One frame: step every value, then write what changed. */
  update(inputs: ReadoutInputs, dtMs: number): void {
    this.hz.to(inputs.hz);
    this.hereFor.to(inputs.hereForSeconds);
    this.tries.to(inputs.tries);
    this.run.to(inputs.runSeconds);
    this.badges.to(inputs.badges);
    this.places.to(inputs.places);
    this.spikes.to(inputs.spikes);
    this.sugar.to(inputs.sugarFraction);
    this.stall.to(inputs.stallSeconds);
    this.stallBar.to(inputs.stallFraction);
    this.rollbackAge.to(inputs.rollbackAgeSeconds ?? 0);

    this.hz.tick(dtMs);
    this.hereFor.tick(dtMs);
    this.tries.tick(dtMs);
    this.run.tick(dtMs);
    this.badges.tick(dtMs);
    this.places.tick(dtMs);
    this.spikes.tick(dtMs);
    this.sugar.tick(dtMs);
    this.stall.tick(dtMs);
    this.stallBar.tick(dtMs);
    this.rollbackAge.tick(dtMs);

    const run = this.run.value;
    this.write('rung', inputs.rungText);
    // The number only. "Hz" is a static word, so `ProgressCluster` renders it beside this element
    // in the body face and it costs 3 characters of Press Start 2P rather than paying a full em
    // each — 27 px that the rung line in the same grid row needs more. Same split as the clock's
    // day, and for the same reason.
    this.write('hz', formatHz(this.hz.value));
    this.write('hereFor', formatDurationCompact(this.hereFor.value));
    this.write('try', `try ${Math.max(1, Math.round(this.tries.value))}`);
    // Two writes, not one, and the split is a type decision rather than a layout one: the clock's
    // eight digits tick every second and have to be tabular, which on this page means Press Start
    // 2P at one em per character (`.num`, `src/index.css`); "· day 3" is two words and a numeral
    // that changes once a day, and paying a full em for each of its characters cost the footer
    // 113 px it does not have now that the body face is Silkscreen. So the digits are `clock` and
    // the words are `day`, in the body face beside it.
    this.write('clock', formatClock(run));
    this.write('day', `· ${DATASET_COPY.dayPrefix} ${dayNumber(run)}`);
    this.write(
      'counters',
      formatCounters(this.game.counters, {
        badges: Math.round(this.badges.value),
        uniqueLocations: Math.round(this.places.value),
        rewardTotal: inputs.rewardTotal,
      }),
    );
    this.write('spikes', formatCount(this.spikes.value));
    this.write('rollbacks', `${inputs.rollbacksThisRung}/${ATTEMPTS_PER_RUNG}`);
    this.write('lifetime', `${inputs.rollbacksLifetime}/${LIFETIME_BUDGET}`);
    this.write(
      'rollbackAge',
      inputs.rollbackAgeSeconds === null
        ? NONE
        : `${formatDurationCompact(this.rollbackAge.value)} ${DATASET_COPY.agoSuffix}`,
    );
    // No spaces around the slash: "0:00 / 2:00" is 11 characters of mono, which wraps in the
    // LADDER tab's narrow stats column and puts "2:00" on a line of its own.
    this.write('stall', `${formatMinutesSeconds(this.stall.value)}/${formatMinutesSeconds(STALL_WINDOW_SECONDS)}`);

    this.paintSugarRing();
    this.paintStallBar();
    this.paintSpine(inputs, dtMs);
  }

  private paintSugarRing(): void {
    if (!this.sugarRing?.isConnected) {
      this.sugarRing = this.root.querySelector<SVGCircleElement>('[data-sugar-ring]');
    }
    const ring = this.sugarRing;
    if (!ring) return;
    const offset = (RING_CIRCUMFERENCE * this.sugar.value).toFixed(2);
    if (this.painted.get('#ring') === offset) return;
    this.painted.set('#ring', offset);
    ring.setAttribute('stroke-dashoffset', offset);
  }

  /**
   * The stall meter, quantised to the theme's 8 px bar cells.
   *
   * It is a bar, so `docs/design/gameboy-theme.md`'s "filled in 8 px steps (quantized)" applies to
   * it as much as to the circuit bars. The track's width is measured rather than derived: it is a
   * flex division of the LADDER pane's stats column, and re-deriving that here is how a padding
   * change puts the steps half a cell off.
   */
  private paintStallBar(): void {
    if (!this.stallFill?.isConnected) {
      this.stallFill = this.root.querySelector<HTMLElement>('[data-stall-fill]');
      this.stallTrackPx = this.stallFill?.parentElement?.clientWidth ?? 0;
    }
    const fill = this.stallFill;
    if (!fill) return;
    const fraction = quantizePixels(this.stallBar.value, this.stallTrackPx, BAR_STEP);
    const value = fraction.toFixed(4);
    if (this.painted.get('#stall') === value) return;
    this.painted.set('#stall', value);
    fill.style.transform = `scaleX(${value})`;
  }

  /**
   * The spine's fill, per rung.
   *
   * A rung's target is 1 once reached and a dim floor otherwise, and the value between is a real
   * lerp — so a rank-up is a rung lighting up over a third of a second rather than a frame. Only
   * rungs whose quantised value changed are written, which is normally none of them.
   *
   * What lands on the element is `--fill-scale`, the `scaleY` itself, already snapped to the pixel
   * grid: the growing half of the 16 px cell is 8 px, so the pose is one of 8, 12 or 16 px and the
   * CSS is a bare `scaleY(var(--fill-scale))` rather than a `calc` it could not have quantised.
   */
  private paintSpine(inputs: ReadoutInputs, dtMs: number): void {
    if (this.rungCells.length !== inputs.total || !this.rungCells[0]?.isConnected) {
      this.rungCells = [...this.root.querySelectorAll<HTMLElement>('[data-rung]')];
      this.rungFill = new Float32Array(this.rungCells.length);
      // Seed settled, so the first painted frame is not a spine filling from nothing.
      for (let i = 0; i < this.rungFill.length; i++) this.rungFill[i] = i <= inputs.rank ? 1 : 0;
    }

    for (let i = 0; i < this.rungCells.length; i++) {
      const target = i <= inputs.rank ? 1 : 0;
      const before = this.rungFill[i] as number;
      const after = target + (before - target) * Math.exp(-Math.max(0, dtMs) / TAU.slow);
      this.rungFill[i] = Math.abs(after - target) < 0.002 ? target : after;

      const grown = quantizePixels(this.rungFill[i] as number, SPINE_CELL_PX * (1 - SPINE_FLOOR), PIXEL_STEP);
      const scale = String(SPINE_FLOOR + (1 - SPINE_FLOOR) * grown);
      const key = `#rung${i}`;
      if (this.painted.get(key) === scale) continue;
      this.painted.set(key, scale);
      const cell = this.rungCells[i];
      if (cell) cell.style.setProperty('--fill-scale', scale);
    }
  }

  /** Write one readout's text, if it changed. */
  private write(key: string, text: string): void {
    if (this.painted.get(key) === text) return;
    const element = this.lookup(key);
    if (!element) return;
    this.painted.set(key, text);
    element.textContent = text;
  }

  private lookup(key: string): HTMLElement | null {
    const cached = this.elements.get(key);
    if (cached?.isConnected) return cached;
    const found = this.root.querySelector<HTMLElement>(`[data-readout="${key}"]`);
    if (found) this.elements.set(key, found);
    else this.elements.delete(key);
    return found;
  }
}
