/**
 * The stage: one page, one rAF loop, one feed.
 *
 * Everything is wired here rather than inside the panels, because the panels must not own
 * anything that ticks: React commits at 4 Hz, the paint loop runs at 60, and the feed arrives at
 * 30. See `src/feed/store.ts` for that split and `src/paint/loop.ts` for the instrumentation.
 * Rail layout v2's choreography — tabs, moments, lerped readouts — is one object too
 * (`src/motion/director.ts`), so this file stays wiring rather than becoming the animation.
 *
 * `data-ready="1"` goes on `<html>` only after `document.fonts.ready` *and* the brain map's base
 * bitmap, because both the capture launcher and every test wait on it. A broadcast page that
 * paints once and runs for weeks will happily broadcast a fallback font forever if nobody gates
 * the first frame.
 */
import { useEffect, useMemo, useRef } from 'react';

import { loadCompressed } from '@flybrain/brain/browser';
import { GAMEBOY_BUTTONS } from '@flybrain/brain';
import type { BrainBaseRequest, BrainBaseResponse } from '@/workers/brain-base.worker';
import BrainBaseWorker from '@/workers/brain-base.worker?worker';
import { AudioEngine } from '@/audio/engine';
import { FixturePlayer } from '@/feed/fixture';
import { FeedSocket } from '@/feed/socket';
import type { FeedSource } from '@/feed/source';
import { AFTERGLOW_MS, FeedIngest, hot, useStage } from '@/feed/store';
import { createFlyRenderer, type FlyFrame, type FlyRenderer } from '@/fly';
import { readFlyDrives } from '@/fly/drives';
import { FlyRig, idleDrives } from '@/fly/rig';
import { resolveGame } from '@/games';
import { CIRCUIT_DECISION_THRESHOLD, circuitFraction, thresholdRateHz } from '@/lib/circuit-scale';
import { BAR_ROLES, CIRCUIT_GROUPS, MACRO_CIRCUIT } from '@/lib/labels';
import {
  FLY_CANVAS_HEIGHT,
  FLY_CANVAS_WIDTH,
  GAME_HEIGHT,
  GAME_WIDTH,
  LAYOUT,
  MACRO_CELL_GAP,
  MACRO_CELL_HEIGHT,
  MACRO_CELL_ROWS,
  MACRO_CELL_WIDTH,
  MACRO_PALETTE_PAD,
  MAP_GRID_HEIGHT,
  MAP_GRID_WIDTH,
  MAP_HERO_HEIGHT,
  MAP_HERO_WIDTH,
  RETINA_CANVAS_HEIGHT,
  RETINA_CANVAS_WIDTH,
  NO_CONTENT_ZONE,
  boxStyle,
} from '@/lib/geometry';
import { stageOptions } from '@/lib/query';
import { Director, FLY_HEAD_ANCHOR } from '@/motion/director';
import { MotionEngine } from '@/motion/engine';
import type { MomentType } from '@/motion/moments';
import { MOTION_SEED, resolveMotionColours } from '@/motion/catalogue';
import { RailSignals } from '@/motion/rail-signals';
import { BAR_STEP, quantizePixels } from '@/motion/lerp';
import { DIR } from '@/motion/particles';
import { BrainMapSurface } from '@/paint/brainmap';
import { GameSurface } from '@/paint/game';
import { PaintLoop, type TimeFn } from '@/paint/loop';
import { RetinaSurface } from '@/paint/retina';
import { readCanvasPalette } from '@/theme/colors';
import { ChatPanel } from '@/panels/ChatPanel';
import { EventsTicker } from '@/panels/EventsTicker';
import { FlyStrip } from '@/panels/FlyStrip';
import { GamePanel } from '@/panels/GamePanel';
import { MomentLayer } from '@/panels/MomentLayer';
import { ProgressCluster } from '@/panels/ProgressCluster';
import { StaleBanner } from '@/panels/StaleBanner';
import { TabSlot } from '@/panels/TabSlot';
import { TitleStrip } from '@/panels/TitleStrip';

/** Where the dataset artifacts are served from (dev middleware and build copy both use this). */
const DATASET_BASE = '/data/fafb-v783';

/** The brain map repaints at up to 30 Hz; its 110 ms decay does not need 60. */
const MAP_INTERVAL_MS = 33;

/** The fly is capped at 30 fps, per `docs/design/fly-avatar.md`. */
const FLY_INTERVAL_MS = 33;

/**
 * How long a finished macro's outcome stays on its cell.
 *
 * `docs/design/macros.md` section 6: "then shows its outcome for a beat (done / blocked /
 * timeout)". A second, on the simulation clock, which is long enough to read a seven-character
 * word at a glance and short enough that the cell is back to its gloss before the fly's next
 * decision (the sim consults the channels again at most once per `holdMs`, 800 ms).
 */
const MACRO_OUTCOME_MS = 1000;

/** How new a running macro has to be for its cell to throw sparks: two snapshots' worth. */
const MACRO_BURST_MS = 200;

/** If the base bitmap never arrives, go ready anyway after this long, and say so. */
const READY_TIMEOUT_MS = 15_000;

export function App() {
  const options = useMemo(() => stageOptions(), []);
  const game = useMemo(() => resolveGame(options.game), [options.game]);

  const gameCanvas = useRef<HTMLCanvasElement | null>(null);
  const retinaCanvas = useRef<HTMLCanvasElement | null>(null);
  const mapCanvas = useRef<HTMLCanvasElement | null>(null);
  const flyCanvas = useRef<HTMLCanvasElement | null>(null);
  const particleCanvas = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = options.theme;
    root.dataset.res = options.res;
    root.dataset.game = game.id;
  }, [options.theme, options.res, game.id]);

  useEffect(() => {
    const root = document.documentElement;
    const stage = document.getElementById('stage') ?? root;
    const palette = readCanvasPalette(root);
    // One authoring resolution, and it is the native broadcast size: the backing stores are
    // already 1:1 with the encoded frame, so there is no multiplier to apply. `?res=720` only ever
    // scales *down* (thumbnails and the downscale tests), which needs no extra backing pixels.
    const backingScale = 1;

    const gameEl = gameCanvas.current;
    const retinaEl = retinaCanvas.current;
    const mapEl = mapCanvas.current;
    if (!gameEl || !retinaEl || !mapEl) return;

    const gameSurface = new GameSurface(gameEl, GAME_WIDTH, GAME_HEIGHT, backingScale);
    const retinaSurface = new RetinaSurface(retinaEl, RETINA_CANVAS_WIDTH, RETINA_CANVAS_HEIGHT, backingScale);
    const mapSurface = new BrainMapSurface(mapEl);

    const backgroundCss = `rgb(${palette.background[0]} ${palette.background[1]} ${palette.background[2]})`;
    gameSurface.drawPlaceholder(backgroundCss);
    mapSurface.drawPlaceholder(backgroundCss);
    retinaSurface.setColors(palette.sensory, palette.panel);
    retinaSurface.clear();
    mapSurface.setColors({ sensory: palette.sensory, internal: palette.internal, output: palette.output });

    // -- The animation engine, and the rail's own derived readouts --------------------------------
    const motion = new MotionEngine({
      seed: MOTION_SEED,
      palette: resolveMotionColours(root),
      // The fly is inside a canvas, so its head is the one emission point with no element to
      // measure; every other anchor is replaced from the DOM each frame by the director.
      anchors: { flyHead: FLY_HEAD_ANCHOR },
    });
    const signals = new RailSignals();

    const ingest = new FeedIngest(game, motion, signals);
    const engine = options.audio ? new AudioEngine(options.gains) : null;
    void engine?.start();

    const director = new Director({
      root: stage as HTMLElement,
      game,
      engine: motion,
      signals,
      palette,
      map: mapSurface,
      gameSurface,
      particleCtx: particleCanvas.current?.getContext('2d') ?? null,
      forcedTab: options.tab,
      playSfx: engine ? (name, gain) => engine.playSfx(name, gain) : null,
    });

    // -- Button afterglow, as a plain row of eight indicators -------------------------------------
    // The row is DOM, not canvas (`src/panels/FlyStrip.tsx`): one cell per `GAMEBOY_BUTTONS`
    // entry, each mutated only through `data-down`/`data-glow` so nothing here re-renders at
    // 30 Hz. Lit on the rising edge and for 250 ms past the *falling* edge, which is what keeps
    // an 85 ms A press visible.
    const buttonCells = new Map<string, HTMLElement>();
    for (const button of GAMEBOY_BUTTONS) {
      const cell = document.querySelector<HTMLElement>(`[data-button="${button}"]`);
      if (cell) buttonCells.set(button, cell);
    }
    const buttonPainted = new Map<string, string>();
    const paintButtons = (nowMs: number): void => {
      for (const [button, cell] of buttonCells) {
        const state = hot.buttonStates[button];
        if (!state) continue;
        const glow = state.down || nowMs - state.upAtMs < AFTERGLOW_MS;
        const key = `${state.down ? 'd' : '-'}${glow ? 'g' : '-'}`;
        if (buttonPainted.get(button) === key) continue;
        buttonPainted.set(button, key);
        cell.dataset.down = state.down ? '1' : '0';
        cell.dataset.glow = glow ? '1' : '0';
      }
    };

    // -- The macro cells -------------------------------------------------------------------------
    // Two places draw them since section 14's decided layout: the strip under the game, which is
    // the pad now, and the MACROS tab, which is the whole keyboard. React owns both lots of text,
    // because it changes once per scene (`src/panels/MacroPalette.tsx`,
    // `src/panels/tabs/MacrosTab.tsx`); what is painted here is which cell is lit and what its
    // outcome was, on the same 30 Hz path as the button afterglow above, for both at once — a
    // macro that is running is running in both places, and matching by name makes that one loop
    // rather than two. The DOM is re-queried whenever React re-deals, since the cells are keyed on
    // the scene and a cached node list goes stale on every scene change.
    const paletteRoot = document.querySelector<HTMLElement>('[data-testid="macro-palette"]');
    const boardRoot = document.querySelector<HTMLElement>('[data-testid="macros-tab"]');
    let paletteCells: HTMLElement[] = [];
    let paletteKey = '';
    const refreshPaletteCells = (): void => {
      if (!paletteRoot) return;
      const key = `${paletteRoot.dataset.mode ?? ''}:${paletteRoot.dataset.scene ?? ''}`;
      const stale = paletteCells.length === 0 || !(paletteCells[0] as HTMLElement).isConnected;
      if (key === paletteKey && !stale) return;
      paletteKey = key;
      paletteCells = [paletteRoot, boardRoot].flatMap((element) =>
        element === null ? [] : [...element.querySelectorAll<HTMLElement>('[data-macro-row]')],
      );
    };

    // Sparks from the chosen cell, the moment it lights: the rail's own burst, aimed with
    // arithmetic rather than a measured box, because the palette's geometry is fixed
    // (`src/lib/geometry.ts`) and `PARTICLE_CLIP` already covers the fly strip.
    //
    // Aimed by `data-macro-slot`, the cell's place *in the strip*, rather than by its type: the
    // strip packs the pad from the top into two columns of seven, so type 29 can be its third
    // cell. A macro that is only on the board — the pad is allowed to be wider than the strip —
    // has no slot and gets no burst, because the burst belongs to the strip's geometry and the
    // board's tab may not even be up.
    const motionColours = resolveMotionColours(root);
    const cellCentre = (slot: number): { x: number; y: number } => ({
      x:
        LAYOUT.macroPalette.x +
        MACRO_PALETTE_PAD +
        Math.floor(slot / MACRO_CELL_ROWS) * (MACRO_CELL_WIDTH + MACRO_CELL_GAP) +
        MACRO_CELL_WIDTH / 2,
      y:
        LAYOUT.macroPalette.y +
        MACRO_PALETTE_PAD +
        (slot % MACRO_CELL_ROWS) * (MACRO_CELL_HEIGHT + MACRO_CELL_GAP) +
        MACRO_CELL_HEIGHT / 2,
    });

    let lastMacroKey = '';
    const paintPalette = (): void => {
      refreshPaletteCells();
      if (paletteCells.length === 0) return;

      // Read the two fields straight off the header rather than through `paletteView`: this runs
      // 60 times a second and the view allocates all thirty-one cells, which the *commit* path
      // (4 Hz, where the cells are what React needs) can afford and this one should not.
      const game = hot.header?.game;
      const inPalette = game?.macroMode === 'macros';
      const macro = (inPalette ? game?.macro : null) ?? null;
      // The outcome beat runs on the *simulation* clock, not the page's: a held fixture seek
      // freezes the feed, so a page clock would tick the beat away under a screenshot.
      const brainMs = hot.header?.brainMs ?? 0;
      const reported = (inPalette ? game?.macroOutcome : null) ?? null;
      const outcome = reported !== null && brainMs - reported.atMs <= MACRO_OUTCOME_MS ? reported : null;

      // A cell is matched by the macro's *name*, not by the wire slot or by its place on screen:
      // a type is a channel (`docs/design/macros.md` section 12), so the name is what identifies
      // it — and an outcome the feed is still holding from the scene before then lights nothing,
      // rather than lighting whichever cell inherited its slot. It is also what makes the strip
      // and the board one loop: the same macro matches its cell in each.
      let liveSlot = -1;
      for (const cell of paletteCells) {
        const name = cell.dataset.macroName ?? '';
        const live = macro !== null && name !== '' && macro.name === name ? '1' : '0';
        if (live === '1' && cell.dataset.macroSlot !== undefined) liveSlot = Number(cell.dataset.macroSlot);
        if (cell.dataset.live !== live) cell.dataset.live = live;

        // A cell that is running again shows the run, not the last result.
        const word = outcome !== null && name !== '' && outcome.name === name && live === '0' ? outcome.outcome : '';
        if (cell.dataset.outcome !== word) {
          cell.dataset.outcome = word;
          const label = cell.querySelector<HTMLElement>('.macro-cell__outcome');
          if (label) label.textContent = word.toUpperCase();
        }
      }

      // `sinceMs` counts from the start, so the start's own clock value identifies the run: a
      // second `GO EXIT` on the same slot is a different burst. The burst only fires for a macro
      // that started *just now*, which is what keeps it off two frames it does not belong to: the
      // page connecting in the middle of a long macro, and a fixture seek, whose silent catch-up
      // lands on a snapshot mid-run and would otherwise freeze a burst into every screenshot.
      const key = macro === null ? '' : `${macro.slot}:${macro.name}:${Math.round(brainMs - macro.sinceMs)}`;
      if (key !== lastMacroKey) {
        if (key !== '' && macro !== null && liveSlot >= 0 && macro.sinceMs <= MACRO_BURST_MS) {
          const at = cellCentre(liveSlot);
          motion.field.sparks(at.x, at.y, 10, motionColours.amber, DIR.right, Math.PI / 2.5, { speed: 0.14 });
        }
        lastMacroKey = key;
      }
    };

    // -- The fly ---------------------------------------------------------------------------------
    const rig = new FlyRig();
    const flyDrives = idleDrives();
    let fly: FlyRenderer | null = null;
    let lastFlyFrame: FlyFrame | null = null;
    let lastFlyMs = Number.NEGATIVE_INFINITY;
    let lastFlyClockMs = Number.NEGATIVE_INFINITY;
    let flyStoppedDrawn = false;
    let flyAccepted = -1;

    if (options.fly !== 'off' && flyCanvas.current) {
      void createFlyRenderer(options.fly, {
        canvas: flyCanvas.current,
        width: FLY_CANVAS_WIDTH,
        height: FLY_CANVAS_HEIGHT,
        palette,
      }).then((renderer) => {
        if (disposed) {
          renderer?.dispose();
          return;
        }
        fly = renderer;
      });
    }

    // `time` is passed in rather than wrapping the whole call, so the `fly` histogram holds one
    // sample per *drawn* frame. Timing the skipped frames too would bury the real cost under a
    // pile of zeroes and report a p50 of 0 ms for a renderer doing real work at 30 fps.
    const paintFly = (nowMs: number, time: TimeFn): void => {
      if (!fly) return;
      // 30 fps while the clock moves, plus a frame whenever a snapshot lands. When the clock has
      // stopped — a fixture held on a seek target — draw exactly one more frame and then nothing:
      // the rig zeroes its gait phase on a stopped clock, so that one frame is the same fly every
      // time, which is what makes the screenshot tests reproducible.
      const stopped = nowMs === lastFlyClockMs;
      lastFlyClockMs = nowMs;
      const due = stopped
        ? !flyStoppedDrawn
        : nowMs - lastFlyMs >= FLY_INTERVAL_MS || hot.accepted !== flyAccepted;
      flyStoppedDrawn = stopped;
      if (!due) return;
      lastFlyMs = nowMs;
      flyAccepted = hot.accepted;
      const sugar = motion.snapshot().active?.type === 'sugar' ? 1 : 0;
      const frame = rig.advance(readFlyDrives(sugar, flyDrives), nowMs);
      lastFlyFrame = frame;
      time('fly', () => fly?.draw(frame));
    };

    // -- Circuit bars -------------------------------------------------------------------------
    // Each bar scales against its own adaptive reference (`hot.circuitReferenceHz`, kept warm by
    // `FeedIngest`, see `src/lib/circuit-scale.ts`), not a fixed Hz ceiling: a fixed scale is what
    // pegged every bar at 100% on the first live run, whose per-role rates run far above whatever
    // the fixture generator produced (`infra/docs/p0-local-encoded-frame.png`).
    //
    // The fills are also quantised to the theme's 8 px cells (`docs/design/gameboy-theme.md`:
    // "Bars are chunky: 12 px tall, hard edges, filled in 8 px steps (quantized), no gradients"),
    // which is why each bar's track width is measured once here: the quantum is 8 px of the
    // element's own width, and a bar in a flex row has no width this file could derive.
    const barFills = new Map<string, HTMLElement>();
    const barTracks = new Map<string, number>();
    const barPeaks = new Map<string, HTMLElement>();
    const barThresholds = new Map<string, HTMLElement>();
    const fullScale = new Map<string, number>();
    const decisionThreshold = new Map<string, number>();
    for (const group of CIRCUIT_GROUPS) {
      for (const bar of group.bars) {
        fullScale.set(bar.role, group.fullScaleHz);
        const threshold = CIRCUIT_DECISION_THRESHOLD[group.id];
        if (threshold !== undefined) decisionThreshold.set(bar.role, threshold);
      }
    }
    for (const role of BAR_ROLES) {
      const fill = document.querySelector<HTMLElement>(`[data-bar="${role}"]`);
      const peak = document.querySelector<HTMLElement>(`[data-peak="${role}"]`);
      const threshold = document.querySelector<HTMLElement>(`[data-threshold="${role}"]`);
      if (fill) {
        barFills.set(role, fill);
        barTracks.set(role, fill.parentElement?.clientWidth ?? 0);
      }
      if (peak) barPeaks.set(role, peak);
      if (threshold) barThresholds.set(role, threshold);
    }
    /**
     * The MACROS row's bars, which come and go with the scene.
     *
     * The six fixed groups are queried once above; these are re-queried whenever React re-renders
     * the row (`src/panels/tabs/SensesTab.tsx`), keyed on the channel tags it drew. Everything
     * else is the same arithmetic: the fill scales against the role's own adaptive reference and is
     * quantised to the theme's 8 px cells.
     */
    let macroBars: { role: string; fill: HTMLElement; peak: HTMLElement | null; track: number }[] = [];
    let macroBarKey = '\u0000';
    const refreshMacroBars = (): void => {
      const rows = [...document.querySelectorAll<HTMLElement>('[data-macro-bar]')];
      const key = rows.map((row) => row.dataset.macroBar ?? '').join(',');
      const stale = macroBars.length > 0 && !(macroBars[0] as { fill: HTMLElement }).fill.isConnected;
      if (key === macroBarKey && !stale) return;
      macroBarKey = key;
      macroBars = [];
      for (const row of rows) {
        const fill = row.querySelector<HTMLElement>('[data-bar]');
        const role = fill?.dataset.bar;
        if (!fill || !role) continue;
        macroBars.push({
          role,
          fill,
          peak: row.querySelector<HTMLElement>('[data-peak]'),
          track: fill.parentElement?.clientWidth ?? 0,
        });
      }
    };

    const barPainted = new Map<string, number>();
    const paintBars = (): void => {
      for (const [role, element] of barFills) {
        const reference = hot.circuitReferenceHz[role] ?? fullScale.get(role) ?? 30;
        const value = circuitFraction(hot.rates[role] ?? 0, reference);
        // Re-measure while the track reads zero: the effect that caches these can run before the
        // slot's panes have been laid out, and an unmeasured track would quietly mean no steps.
        let track = barTracks.get(role) ?? 0;
        if (track === 0) {
          track = element.parentElement?.clientWidth ?? 0;
          barTracks.set(role, track);
        }
        const quantised = quantizePixels(value, track, BAR_STEP);
        if (barPainted.get(role) === quantised) continue;
        barPainted.set(role, quantised);
        element.style.transform = `scaleX(${quantised})`;
      }
      for (const [role, element] of barPeaks) {
        const reference = hot.circuitReferenceHz[role] ?? fullScale.get(role) ?? 30;
        const peak = circuitFraction(hot.peaks[role]?.value ?? 0, reference);
        element.style.transform = `translateX(${(peak * 100).toFixed(1)}%)`;
      }
      for (const [role, element] of barThresholds) {
        const threshold = decisionThreshold.get(role);
        if (threshold === undefined) continue;
        const reference = hot.circuitReferenceHz[role] ?? fullScale.get(role) ?? 30;
        const median = hot.circuitMedianHz[role] ?? 0;
        const fraction = circuitFraction(thresholdRateHz(threshold, median), reference);
        element.style.transform = `translateX(${(fraction * 100).toFixed(1)}%)`;
      }

      refreshMacroBars();
      for (const bar of macroBars) {
        const reference = hot.circuitReferenceHz[bar.role] ?? MACRO_CIRCUIT.fullScaleHz;
        const value = circuitFraction(hot.rates[bar.role] ?? 0, reference);
        if (bar.track === 0) bar.track = bar.fill.parentElement?.clientWidth ?? 0;
        const quantised = quantizePixels(value, bar.track, BAR_STEP);
        if (barPainted.get(bar.role) !== quantised) {
          barPainted.set(bar.role, quantised);
          bar.fill.style.transform = `scaleX(${quantised})`;
        }
        if (bar.peak) {
          const peak = circuitFraction(hot.peaks[bar.role]?.value ?? 0, reference);
          bar.peak.style.transform = `translateX(${(peak * 100).toFixed(1)}%)`;
        }
      }
    };

    // -- Sound effects the moments do not own -------------------------------------------------
    // Every moment's sound comes from the engine's cue queue, drained by the director. What is
    // left here is the two alarms, which are states rather than moments: the stuck-o-meter
    // crossing its threshold, and the feed going stale.
    let sfxArmed = false;
    let stuckFired = false;
    let staleFired = false;
    const fireSfx = (): void => {
      if (!engine) return;
      const state = useStage.getState();

      if (!sfxArmed) {
        // Arm after the first frame so a seek's replayed history is silent.
        stuckFired = state.milestone.sinceSeconds >= game.stuckAlarmSeconds;
        staleFired = state.stale;
        sfxArmed = true;
        return;
      }

      const stuck = state.milestone.sinceSeconds >= game.stuckAlarmSeconds;
      if (stuck && !stuckFired) engine.playSfx('stuck');
      stuckFired = stuck;

      if (state.stale && !staleFired) engine.playSfx('stale');
      staleFired = state.stale;
    };

    // -- The loop -----------------------------------------------------------------------------
    let lastMapMs = Number.NEGATIVE_INFINITY;
    let mapDirty = false;
    let source: FeedSource | null = null;

    const loop = new PaintLoop((rawNowMs, dtMs, time) => {
      if (source) time('pump', () => source?.pump(rawNowMs));

      // A fixture held on a seek target freezes the clock, so everything that is a function of
      // elapsed time stops with it and a screenshot is reproducible.
      const nowMs = source?.clock ? source.clock(rawNowMs) : rawNowMs;

      if (hot.frameDirty && hot.frame) {
        const frame = hot.frame;
        hot.frameDirty = false;
        time('game', () => gameSurface.drawFrame(frame, rawNowMs));
        if (retinaSurface.ready()) time('retina', () => retinaSurface.drawFrame(frame));
      } else if (gameSurface.rewinding(rawNowMs)) {
        // The rollback wipe is 400 ms of animation over a 30 Hz picture, so the game canvas is
        // repainted on the frames between snapshots for its duration and on no others.
        time('game', () => gameSurface.redraw(rawNowMs));
      }

      if (hot.spikesDirty && hot.spikes) {
        mapSurface.ingestSpikes(hot.spikes);
        hot.spikesDirty = false;
        mapDirty = true;
      }

      // Redraw at up to 30 Hz while the feed is moving, and not at all when it is not: the
      // accumulator's decay would otherwise keep fading a frozen frame under a screenshot. A
      // flare is animation rather than data, so it keeps the map repainting for its 500 ms.
      const feedIdle = nowMs - hot.lastSnapshotMs > 500;
      const flaring = mapSurface.flaring(rawNowMs);
      if (mapSurface.ready() && (mapDirty || flaring || !feedIdle) && nowMs - lastMapMs >= MAP_INTERVAL_MS) {
        lastMapMs = nowMs;
        mapDirty = false;
        time('brainmap', () => mapSurface.draw(rawNowMs));
      }

      time('buttons', () => paintButtons(nowMs));
      time('palette', () => paintPalette());
      time('bars', () => paintBars());
      paintFly(nowMs, time);
      time('motion', () => director.frame(nowMs, rawNowMs, dtMs));

      if (engine && hot.audioQueue.length > 0) {
        const chunks = hot.audioQueue.splice(0, hot.audioQueue.length);
        time('audio', () => {
          for (const chunk of chunks) engine.push(chunk);
        });
      }

      time('commit', () => ingest.commit(nowMs));
      time('sfx', () => fireSfx());
    });

    // -- Dataset, worker, feed ----------------------------------------------------------------
    let disposed = false;
    const worker = new BrainBaseWorker();
    let readyTimer: ReturnType<typeof setTimeout> | null = null;

    const markReady = (degraded: boolean): void => {
      if (disposed || root.dataset.ready === '1') return;
      if (degraded) root.dataset.degraded = '1';
      root.dataset.ready = '1';
    };

    worker.addEventListener('message', (event: MessageEvent<BrainBaseResponse>) => {
      const message = event.data;
      if (message.type === 'error') {
        console.warn(`brain map base failed: ${message.message}`);
        markReady(true);
        return;
      }
      mapSurface.setBase(
        message.bitmap,
        message.lut,
        message.cellClasses,
        message.neuronCount,
        message.pam,
        message.fit,
      );
      // On a paused fixture there is no later snapshot to trigger a repaint, so ask for one.
      mapDirty = true;
      void document.fonts.ready.then(() => markReady(false));
      worker.terminate();
    });

    const request: BrainBaseRequest = {
      type: 'load',
      base: DATASET_BASE,
      width: MAP_HERO_WIDTH,
      height: MAP_HERO_HEIGHT,
      gridWidth: MAP_GRID_WIDTH,
      gridHeight: MAP_GRID_HEIGHT,
      colors: { sensory: palette.sensory, internal: palette.internal, output: palette.output },
    };
    worker.postMessage(request);
    readyTimer = setTimeout(() => markReady(true), READY_TIMEOUT_MS);

    void (async () => {
      try {
        const response = await fetch(`${DATASET_BASE}/meta.json`);
        if (response.ok) {
          const meta = (await response.json()) as {
            neurons: number;
            edges: number;
            dataset: string;
            visual: { count: number };
          };
          if (!disposed) {
            useStage.getState().setDataset({
              neurons: meta.neurons,
              edges: meta.edges,
              name: /v\d+/.exec(meta.dataset)?.[0] ?? meta.dataset,
            });
          }
        }
      } catch (error) {
        console.warn(`dataset metadata unavailable: ${(error as Error).message}`);
      }

      try {
        const [xy, hemisphere] = await Promise.all([
          loadCompressed(`${DATASET_BASE}/visual-xy.binz`, (buffer) => new Float32Array(buffer)),
          loadCompressed(`${DATASET_BASE}/visual-hemisphere.binz`, (buffer) => new Uint8Array(buffer)),
        ]);
        if (!disposed) {
          retinaSurface.setColumns({ xy, hemisphere, count: hemisphere.length });
          // Same race as the brain map: the columns can land after the last snapshot of a seek.
          if (hot.frame) retinaSurface.drawFrame(hot.frame);
        }
      } catch (error) {
        console.warn(`retina columns unavailable: ${(error as Error).message}`);
      }
    })();

    const player =
      options.mode === 'player'
        ? new FixturePlayer(`/fixtures/${options.fixture}.flyfeed.gz`, ingest, {
            seekSeconds: options.seekSeconds,
            autoplay: options.autoplay,
            loop: options.loop,
          })
        : null;
    source = player ?? new FeedSocket(options.feedUrl, ingest);

    void source.start().catch((error: unknown) => {
      console.error(`feed source failed: ${(error as Error).message}`);
    });

    loop.start();

    // -- Test and operator surface ------------------------------------------------------------
    window.__stage = {
      options,
      game: game.id,
      metrics: () => loop.metrics(),
      audio: () => engine?.state() ?? null,
      manifest: () => player?.manifest() ?? null,
      seek: (seconds: number) => {
        director.reset();
        player?.seek(seconds);
      },
      stopFeed: () => source?.stop(),
      sprites: () => mapSurface.sprites(),
      health: () => ({ gaps: hot.gaps, decodeErrors: hot.decodeErrors, accepted: hot.accepted }),
      state: () => useStage.getState(),
      gameScale: () => gameSurface.scale(),
      motion: () => director.state(),
      pam: () => mapSurface.pamCentroid(),
      // The brain map's own geometry and saturation. The one way to tell, from outside the page,
      // whether the CONNECTOME tab's canvas, its LUT and the worker's fit still agree.
      brainmap: () => mapSurface.stats(),
      // The one deliberate way to drive the catalogue by hand: the moment mockups and the e2e
      // moment assertions fire a trigger rather than waiting for a fixture to contain one.
      fire: (type, label = '', detail = '') =>
        motion.enqueue({ type, label, detail, intensity: 1, source: 'event' }),
      /**
       * Replace the held feed's chat ring, the same way `fire` replaces a moment: by hand, for the
       * chat mockup and the wrapping assertions, because the committed fixtures were recorded off a
       * live bridge and none of them happens to contain a 200-character line.
       *
       * Written into `hot.header`, not into the store, because the paint loop's commit rebuilds
       * `chat` from the header every time it runs and a `setState` here would last 250 ms. The
       * lines still go through `sanitizeChatRing` on the way to the panel, so this cannot put
       * anything on screen that a real line could not.
       *
       * The wall time is a day old on purpose: a line older than the page's connect time is
       * history rather than an arrival, so an injected name cannot pin the DESCRIBE tab
       * (`src/lib/chatters.ts`) and move a screenshot that was meant to be about chat.
       */
      chat: (lines) => {
        const header = hot.header;
        if (!header) return 0;
        const wallMs = Date.now() - 86_400_000;
        header.chat = lines.map((line, index) => ({
          id: index + 1,
          wallMs,
          by: line.by,
          text: line.text,
          ...(line.bot === true ? { bot: true } : {}),
        }));
        // Forced, because a fixture held on a seek target freezes the clock the commit gate reads:
        // without this the panel would not see the new ring until something else opened the gate.
        ingest.commit(hot.lastSnapshotMs, true);
        return useStage.getState().chat.length;
      },
      fly: () => ({
        // The renderer that is actually drawing, which is not always the one that was asked for:
        // `webgl` falls back to `paper` on a host with no usable GL context (`src/fly/index.ts`).
        // `requested` keeps the query parameter visible so the two can be told apart on air.
        mode: fly ? fly.mode : options.fly,
        requested: options.fly,
        rendering: fly !== null,
        gaitPhase: rig.gaitPhase,
        // Extension of the proboscis in rig units, which a sugar event visibly grows.
        proboscis: lastFlyFrame
          ? Math.hypot(
              lastFlyFrame.proboscis.b[0] - lastFlyFrame.proboscis.a[0],
              lastFlyFrame.proboscis.b[1] - lastFlyFrame.proboscis.a[1],
              lastFlyFrame.proboscis.b[2] - lastFlyFrame.proboscis.a[2],
            )
          : 0,
        // The tip of every leg as last drawn, so a test can check the fly is still whenever the
        // clock is (`tests/e2e/behaviour.spec.ts`).
        legTips: (lastFlyFrame?.legs ?? []).map((joints) => joints[3] as [number, number, number]),
      }),
    };

    return () => {
      disposed = true;
      if (readyTimer !== null) clearTimeout(readyTimer);
      loop.stop();
      source?.stop();
      fly?.dispose();
      worker.terminate();
      void engine?.stop();
      delete root.dataset.ready;
    };
    // The stage is built once. Every option is a page-load-time decision by design: the capture
    // launcher restarts the page to change anything.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div id="stage">
      {/* Declared, and asserted by the structural test: no text or readout may land here. */}
      <div data-nocontent="1" style={boxStyle(NO_CONTENT_ZONE)} aria-hidden />

      <TitleStrip game={game} />
      <GamePanel canvasRef={gameCanvas} />
      <FlyStrip mode={options.fly} canvasRef={flyCanvas} />

      <ProgressCluster game={game} />
      <TabSlot game={game} retinaRef={retinaCanvas} mapRef={mapCanvas} />
      <EventsTicker />
      <ChatPanel source={options.chat} />

      <MomentLayer particleRef={particleCanvas} />
      <StaleBanner />
    </div>
  );
}

declare global {
  interface Window {
    __stage?: {
      options: ReturnType<typeof stageOptions>;
      game: string;
      metrics: () => ReturnType<PaintLoop['metrics']>;
      audio: () => ReturnType<AudioEngine['state']> | null;
      manifest: () => ReturnType<FixturePlayer['manifest']> | null;
      seek: (seconds: number) => void;
      stopFeed: () => void;
      sprites: () => number;
      health: () => { gaps: number; decodeErrors: number; accepted: number };
      state: () => ReturnType<typeof useStage.getState>;
      gameScale: () => number;
      motion: () => ReturnType<Director['state']>;
      pam: () => { x: number; y: number } | null;
      brainmap: () => ReturnType<BrainMapSurface['stats']>;
      fire: (type: MomentType, label?: string, detail?: string) => number | null;
      /** Replace the held feed's chat ring by hand; returns how many lines the panel accepted. */
      chat: (lines: readonly { by: string; text: string; bot?: boolean }[]) => number;
      fly: () => {
        mode: string;
        requested: string;
        rendering: boolean;
        gaitPhase: number;
        proboscis: number;
        legTips: [number, number, number][];
      };
    };
  }
}
