/**
 * The decoupling that makes a 30 Hz feed paintable at 60 Hz without React in the way (design A6).
 *
 * Three clocks, on purpose:
 *   - **ingest, 30 Hz.** `ingest()` writes typed arrays and scalars into `hot`, a plain mutable
 *     object with no subscribers. Nothing re-renders.
 *   - **paint, 60 Hz.** The rAF loop reads `hot` directly and repaints only dirty surfaces.
 *   - **React, 4 Hz.** `commit()` copies the handful of values the DOM shows into a zustand store,
 *     coalesced to 250 ms, plus immediate pushes for the ticker and moment overlay, which are
 *     event-driven and must not wait for the next commit tick.
 *
 * Every method takes `nowMs` rather than calling `performance.now()`, because the fixture player
 * seeks by replaying the recording against virtual time: with an injected clock, a seek to t=95
 * leaves the ticker, the afterglow and the moment overlay in exactly the state they would have
 * been in had the page watched those 95 seconds live.
 */
import { GAMEBOY_BUTTONS, GAMEBOY_BUTTON_BITS } from '@flybrain/brain';
import {
  paletteView,
  type FeedHeader,
  type FeedMilestone,
  type FeedStatus,
  type GameMode,
  type GameScene,
  type MacroMode,
  type PaletteCell,
  type RewardKind,
} from '@flybrain/feed';
import { create } from 'zustand';
import { sanitizeChatRing } from '@/chat/sanitize';
import type { ChatLine } from '@/chat/types';
import type { GameConfig } from '@/games';
import { CircuitScale, RunningMedian } from '@/lib/circuit-scale';
import { CHAT_LINES } from '@/lib/geometry';
import { BAR_ROLES, CIRCUIT_GROUPS, MACRO_BAR_ROLES, MACRO_CIRCUIT } from '@/lib/labels';
import { TickerQueue, type TickerItem } from '@/lib/ticker';
import type { MotionEngine } from '@/motion/engine';
import type { MomentSnapshot } from '@/motion/moments';
import type { RailSignals } from '@/motion/rail-signals';
import type { DecodedSnapshot } from './decode';

/** Silence after which the page stops claiming the numbers are live (design A6). */
export const STALE_AFTER_MS = 2000;

/** Cold-store commit interval. */
export const COMMIT_MS = 250;

/** Afterglow after the falling edge of a button (design A3). */
export const AFTERGLOW_MS = 250;

/** Peak-hold decay on a circuit bar. */
export const PEAK_HOLD_MS = 600;

/** Per-button edge state the paint loop turns into an afterglow class. */
export interface ButtonState {
  /** True while the mask bit is set. */
  down: boolean;
  /** Clock value of the last rising edge, or -Infinity. */
  downAtMs: number;
  /** Clock value of the last falling edge, or -Infinity. */
  upAtMs: number;
}

/**
 * The hot store. Mutable, unobserved, read by the paint loop every frame.
 *
 * Typed arrays here are views into the last message (or, for audio, owned copies), so nothing is
 * allocated per snapshot beyond the audio chunk.
 */
export interface HotStore {
  header: FeedHeader | null;
  /** Clock value of the last accepted snapshot; drives the stale banner. */
  lastSnapshotMs: number;
  /** Monotonic count of accepted snapshots, for the dropped-frame check. */
  accepted: number;
  /** Snapshots whose `seq` skipped, i.e. the service dropped them rather than queueing. */
  gaps: number;
  /** Decode failures since load. Surfaced in the honesty panel rather than hidden. */
  decodeErrors: number;

  frame: Uint8Array | null;
  frameDirty: boolean;
  spikes: Uint8Array | null;
  spikesDirty: boolean;

  buttons: number;
  buttonStates: Record<string, ButtonState>;

  rates: Record<string, number>;
  /** Peak-hold value per role, decayed by the paint loop. */
  peaks: Record<string, { value: number; atMs: number }>;
  /**
   * Per-role adaptive reference (Hz) each circuit bar's fill scales against — see
   * `src/lib/circuit-scale.ts`. Updated once per snapshot, not once per animation frame, so a
   * fixture's silent seek catch-up (`src/feed/fixture.ts`) builds the same reference a viewer
   * watching live would have settled into.
   */
  circuitReferenceHz: Record<string, number>;
  /**
   * Per-role running median (Hz): the display's stand-in "resting level" for the threshold tick,
   * since the feed does not carry the decoder's real calibration baseline (`docs/readout.md`).
   */
  circuitMedianHz: Record<string, number>;
  populationRate: number;
  /**
   * The same adaptive reference, for the whole-brain rate: the fly's breathing and wing tremor
   * scale against it rather than against a hard-coded Hz (`docs/design/fly-avatar.md`).
   */
  populationReferenceHz: number;
  /** Last reported spike count from a snapshot that actually carried the bitset. */
  spikeCount: number;

  /** Audio chunks waiting for the engine. Drained, not accumulated. */
  audioQueue: Float32Array[];
}

function freshButtonStates(): Record<string, ButtonState> {
  const states: Record<string, ButtonState> = {};
  for (const button of GAMEBOY_BUTTONS) {
    states[button] = { down: false, downAtMs: Number.NEGATIVE_INFINITY, upAtMs: Number.NEGATIVE_INFINITY };
  }
  return states;
}

/** Seed Hz per bar role, from the group table (`src/lib/labels.ts`), for the adaptive trackers. */
const CIRCUIT_SEED_HZ = new Map<string, number>();
for (const group of CIRCUIT_GROUPS) {
  for (const bar of group.bars) CIRCUIT_SEED_HZ.set(bar.role, group.fullScaleHz);
}
// All 31 macro channels are tracked too, though only the scene's bound ones have a rate bar at any
// moment (`docs/design/macros.md` section 12): a channel the scene binds again a minute later has
// to come back with the reference it had, not with a cold one.
for (const role of MACRO_BAR_ROLES) CIRCUIT_SEED_HZ.set(role, MACRO_CIRCUIT.fullScaleHz);

/** Every role with an adaptive reference: the fixed bars plus the macro channels. */
const SCALED_ROLES: readonly string[] = [...BAR_ROLES, ...MACRO_BAR_ROLES];

function freshCircuitScales(): Map<string, CircuitScale> {
  const scales = new Map<string, CircuitScale>();
  for (const role of SCALED_ROLES) scales.set(role, new CircuitScale(CIRCUIT_SEED_HZ.get(role) ?? 10));
  return scales;
}

function freshCircuitMedians(): Map<string, RunningMedian> {
  const medians = new Map<string, RunningMedian>();
  for (const role of SCALED_ROLES) medians.set(role, new RunningMedian(CIRCUIT_SEED_HZ.get(role) ?? 10));
  return medians;
}

function freshCircuitReferenceHz(): Record<string, number> {
  const out: Record<string, number> = {};
  for (const role of SCALED_ROLES) out[role] = CIRCUIT_SEED_HZ.get(role) ?? 10;
  return out;
}

/** Seed for the whole-brain rate's own envelope. A resting fly brain sits in this neighbourhood. */
const POPULATION_SEED_HZ = 10;

/** Module-singleton per-role trackers. Not part of `HotStore` itself: the paint loop only ever
 *  reads the plain Hz numbers those trackers publish into `hot.circuitReferenceHz` /
 *  `circuitMedianHz`, never the tracker instances. */
let circuitScales = freshCircuitScales();
let circuitMedians = freshCircuitMedians();
let populationScale = new CircuitScale(POPULATION_SEED_HZ);

/** The one hot store instance. Deliberately a module singleton: there is one stage per page. */
export const hot: HotStore = {
  header: null,
  lastSnapshotMs: Number.NEGATIVE_INFINITY,
  accepted: 0,
  gaps: 0,
  decodeErrors: 0,
  frame: null,
  frameDirty: false,
  spikes: null,
  spikesDirty: false,
  buttons: 0,
  buttonStates: freshButtonStates(),
  rates: {},
  peaks: {},
  circuitReferenceHz: freshCircuitReferenceHz(),
  circuitMedianHz: freshCircuitReferenceHz(),
  populationRate: 0,
  populationReferenceHz: POPULATION_SEED_HZ,
  spikeCount: 0,
  audioQueue: [],
};

/** What React renders. Nothing here changes more than 4 times a second except ticker/moment. */
export interface ColdState {
  connection: 'idle' | 'connecting' | 'open' | 'closed';
  status: FeedStatus;
  stale: boolean;
  mode: GameMode;
  realtimeFactor: number;
  runSeconds: number;
  uptimeSeconds: number;
  populationRate: number;
  spikeCount: number;
  badges: number;
  uniqueLocations: number;
  rewardTotal: number;
  rewardCounts: Record<RewardKind, number> | null;
  semanticRewards: boolean;
  /**
   * The scene's macros, as `paletteView` normalizes them (`docs/design/macros.md` sections 6,
   * 12 and 14): one cell per macro type in the contract's order, bound or not.
   *
   * Here rather than in `hot` because it is React's to draw: the rows change once per scene, not
   * once per snapshot, and the SENSES panel's MACROS row is drawn from the same list. What the
   * *paint loop* needs — which cell is lit, what its outcome was, and each channel's rate — it
   * reads straight off `hot`, the same split the button row's afterglow uses.
   */
  scene: GameScene;
  macroMode: MacroMode;
  palette: readonly PaletteCell[];
  learning: { enabled: boolean; updates: number; changed: number; synapses: number; signal: number };
  /** The header's milestone object verbatim, so a protocol field cannot go missing here. */
  milestone: FeedMilestone;
  sugar: { active: boolean; remainingMs: number; cooldownMs: number; lastBy: string | null; todayCount: number };
  ticker: readonly TickerItem[];
  /** The moment on stage, as the caption band renders it (`src/motion/moments.ts`). */
  moment: MomentSnapshot['active'];
  /**
   * The last seven chat lines, re-validated on render.
   *
   * Empty both when the feed carries no `chat` (an older service, or the kill switch) and when
   * every line it carried failed re-validation, and the panel renders nothing in either case —
   * which is the same on-screen outcome and deliberately indistinguishable.
   */
  chat: readonly ChatLine[];
  /** Feed gaps and decode errors, shown in the honesty panel. */
  health: { gaps: number; decodeErrors: number };
  /**
   * Set once the dataset metadata has loaded.
   *
   * `neurons` and `edges` are `meta.json`'s own counts and `name` its version (`v783`). The
   * DESCRIBE tab quotes all three (`src/panels/tabs/DescribeTab.tsx`), which is why `edges` is
   * here: the doc's rule is that a number on that tab comes from the dataset rather than from a
   * sentence someone typed, so the card cannot outlive a connectome rebuild.
   */
  dataset: { neurons: number; edges: number; name: string } | null;

  setConnection: (connection: ColdState['connection']) => void;
  setDataset: (dataset: { neurons: number; edges: number; name: string }) => void;
}

const INITIAL_COLD = {
  connection: 'idle',
  status: 'booting',
  stale: false,
  mode: 'BOOT',
  realtimeFactor: 0,
  runSeconds: 0,
  uptimeSeconds: 0,
  populationRate: 0,
  spikeCount: 0,
  badges: 0,
  uniqueLocations: 0,
  rewardTotal: 0,
  rewardCounts: null,
  semanticRewards: true,
  scene: 'unknown',
  macroMode: 'raw',
  palette: paletteView(null).cells,
  learning: { enabled: false, updates: 0, changed: 0, synapses: 0, signal: 0 },
  milestone: { rank: 0, label: '', next: '', sinceSeconds: 0, attempts: 0 } as FeedMilestone,
  sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
  ticker: [] as readonly TickerItem[],
  moment: null as MomentSnapshot['active'],
  chat: [] as readonly ChatLine[],
  health: { gaps: 0, decodeErrors: 0 },
  dataset: null,
} satisfies Omit<ColdState, 'setConnection' | 'setDataset'>;

export const useStage = create<ColdState>()((set) => ({
  ...INITIAL_COLD,
  setConnection: (connection) => set({ connection }),
  setDataset: (dataset) => set({ dataset }),
}));

/**
 * Ingest and commit. One instance, created by `App` once the game config is known.
 *
 * `silent` ingestion is what a seek uses: state advances, the ticker and moment overlay evolve,
 * but audio is not queued (nobody wants 95 seconds of fast-forwarded sound) and no surface is
 * marked dirty until the last snapshot of the seek.
 */
export class FeedIngest {
  private readonly ticker: TickerQueue;
  private lastSeq = -1;
  private lastCommitMs = Number.NEGATIVE_INFINITY;
  private lastTickerVersion = -1;
  private lastMomentId = 0;
  private lastCircuitMedianMs: number | null = null;

  /**
   * The motion engine and the rail's derived signals, if they are wired.
   *
   * Both optional so the store stays testable on its own, and a seam so every decision about
   * *what a moment means* lives in `src/motion/` rather than in the ingest path: this file hands
   * over the snapshot and its clock and asks nothing else.
   */
  constructor(
    game: GameConfig,
    private readonly motion: MotionEngine | null = null,
    private readonly signals: RailSignals | null = null,
  ) {
    this.ticker = new TickerQueue(game);
  }

  /** Reset every derived piece of state. Used when a fixture seeks or loops. */
  reset(): void {
    this.lastSeq = -1;
    // Both commit gates reopen: a seek replays history against a virtual clock, and the next
    // commit has to happen whatever that clock says relative to the last one.
    this.lastCommitMs = Number.NEGATIVE_INFINITY;
    this.lastTickerVersion = -1;
    this.lastMomentId = 0;
    this.motion?.reset();
    this.signals?.reset();
    hot.buttonStates = freshButtonStates();
    hot.audioQueue.length = 0;
    hot.gaps = 0;
    // A fixture seek replays from the top against a virtual clock (`src/feed/fixture.ts`), so
    // the circuit bar trackers reset with everything else rather than carrying a stale reference
    // in from whatever the page had loaded before.
    circuitScales = freshCircuitScales();
    circuitMedians = freshCircuitMedians();
    populationScale = new CircuitScale(POPULATION_SEED_HZ);
    hot.circuitReferenceHz = freshCircuitReferenceHz();
    hot.circuitMedianHz = freshCircuitReferenceHz();
    hot.populationReferenceHz = POPULATION_SEED_HZ;
    this.lastCircuitMedianMs = null;
  }

  /** Count a message that failed to decode. The page keeps running; the honesty panel says so. */
  noteDecodeError(): void {
    hot.decodeErrors += 1;
  }

  ingest(snapshot: DecodedSnapshot, nowMs: number, options: { silent?: boolean } = {}): void {
    const { header } = snapshot;
    const silent = options.silent ?? false;

    if (this.lastSeq >= 0 && header.seq > this.lastSeq + 1) hot.gaps += header.seq - this.lastSeq - 1;
    this.lastSeq = header.seq;

    hot.header = header;
    hot.lastSnapshotMs = nowMs;
    hot.accepted += 1;
    hot.rates = header.rates;
    hot.populationRate = header.populationRate;

    // A snapshot without the spikes attachment reports spikeCount 0 per the contract, so the
    // readout holds the last real measurement rather than blinking to zero (the recorder strides
    // spikes down to 10 Hz in the committed fixtures).
    if (snapshot.spikes) {
      hot.spikes = snapshot.spikes;
      hot.spikesDirty = !silent;
      hot.spikeCount = header.spikeCount;
    }

    if (snapshot.frame) {
      hot.frame = snapshot.frame;
      hot.frameDirty = !silent;
    }

    if (snapshot.audio && !silent) hot.audioQueue.push(snapshot.audio);

    this.updateButtons(header.buttons, nowMs);
    this.updatePeaks(header.rates, nowMs);
    this.updateCircuitScales(header.rates, nowMs);
    populationScale.observe(header.populationRate, nowMs);
    hot.populationReferenceHz = populationScale.referenceHz;

    for (const event of header.events) this.ticker.push(event, nowMs);
    // The engine reads the events *and* the header deltas itself (`TriggerMapper`), so it is given
    // the whole snapshot rather than a replay of the loop above.
    this.motion?.ingest(header, nowMs);
    this.signals?.observe(header, nowMs);
    this.ticker.tick(nowMs);
  }

  /** Called by the paint loop every frame. Commits the cold store at most every `COMMIT_MS`. */
  commit(nowMs: number, force = false): void {
    this.ticker.tick(nowMs);
    this.decayPeaks(nowMs);

    const tickerChanged = this.ticker.version() !== this.lastTickerVersion;
    // A clock that jumps *backwards* also means "commit now". A fixture held on a seek target
    // freezes its clock at the virtual time of the snapshot it landed on, which is earlier than
    // the real clock the loop was using before the seek finished loading — so a plain
    // `elapsed >= COMMIT_MS` gate stays shut forever and the page renders its initial state. That
    // is what the cold-open fixture caught: every readout stuck at zero on a feed that had
    // already delivered sixteen snapshots.
    const elapsed = nowMs - this.lastCommitMs;
    const due = force || !(elapsed >= 0 && elapsed < COMMIT_MS);

    // A moment must reach the DOM the frame it starts, not up to 250 ms later: the caption band,
    // the rail flash and the tab focus all key off it, and the SFX fires immediately. A rollback
    // is the case that proves it — its ticker row is queued behind the dwell gate, so nothing
    // else on this path would have opened the commit gate for it.
    const moment = this.motion?.snapshot().active ?? null;
    const momentChanged = (moment?.id ?? 0) !== this.lastMomentId;

    if (!due && !tickerChanged && !momentChanged) return;

    const header = hot.header;
    const stale = header !== null && nowMs - hot.lastSnapshotMs > STALE_AFTER_MS;
    const palette = paletteView(header);

    if (tickerChanged) this.lastTickerVersion = this.ticker.version();
    if (due) this.lastCommitMs = nowMs;
    this.lastMomentId = moment?.id ?? 0;

    useStage.setState({
      ...(header
        ? {
            status: header.status,
            mode: header.game.mode,
            realtimeFactor: header.realtimeFactor,
            runSeconds: header.runSeconds,
            uptimeSeconds: header.uptimeSeconds,
            populationRate: header.populationRate,
            spikeCount: hot.spikeCount,
            badges: header.game.badges,
            uniqueLocations: header.game.uniqueLocations,
            rewardTotal: header.game.rewardTotal,
            rewardCounts: header.game.rewardCounts,
            semanticRewards: header.game.semanticRewards,
            scene: palette.scene,
            macroMode: palette.mode,
            palette: palette.cells,
            learning: header.learning,
            milestone: header.milestone,
            sugar: header.sugar,
            chat: sanitizeChatRing((header as FeedHeader & { chat?: unknown }).chat, CHAT_LINES),
          }
        : {}),
      stale,
      ticker: [...this.ticker.items()],
      moment,
      health: { gaps: hot.gaps, decodeErrors: hot.decodeErrors },
    });
  }

  /** The ticker, for tests and for the panel that needs the queued count. */
  queue(): TickerQueue {
    return this.ticker;
  }

  private updateButtons(mask: number, nowMs: number): void {
    for (const button of GAMEBOY_BUTTONS) {
      const bit = GAMEBOY_BUTTON_BITS[button];
      const down = (mask & bit) !== 0;
      const state = hot.buttonStates[button];
      if (!state) continue;
      if (down && !state.down) state.downAtMs = nowMs;
      if (!down && state.down) state.upAtMs = nowMs;
      state.down = down;
    }
    hot.buttons = mask;
  }

  private updatePeaks(rates: Record<string, number>, nowMs: number): void {
    for (const [role, value] of Object.entries(rates)) {
      const peak = hot.peaks[role];
      if (!peak || value >= peak.value) {
        hot.peaks[role] = { value, atMs: nowMs };
      }
    }
  }

  /** Advance each bar role's adaptive reference and resting-level median by one snapshot. */
  private updateCircuitScales(rates: Record<string, number>, nowMs: number): void {
    for (const role of SCALED_ROLES) {
      const value = rates[role] ?? 0;

      const scale = circuitScales.get(role);
      if (scale) {
        scale.observe(value, nowMs);
        hot.circuitReferenceHz[role] = scale.referenceHz;
      }

      const median = circuitMedians.get(role);
      if (median) {
        // The median only needs the *elapsed* time between snapshots, not the absolute virtual
        // clock, so it advances correctly whether it is fed live snapshots roughly every 33 ms or
        // a seek's silent catch-up stream.
        const dtMs = this.lastCircuitMedianMs === null ? 0 : nowMs - this.lastCircuitMedianMs;
        median.observe(value, dtMs);
        hot.circuitMedianHz[role] = median.medianHz;
      }
    }
    this.lastCircuitMedianMs = nowMs;
  }

  /** Linear decay of the peak-hold tick so a spike stays visible after the value drops. */
  private decayPeaks(nowMs: number): void {
    for (const [role, peak] of Object.entries(hot.peaks)) {
      const age = nowMs - peak.atMs;
      if (age <= 0) continue;
      const current = hot.rates[role] ?? 0;
      if (age >= PEAK_HOLD_MS) {
        hot.peaks[role] = { value: current, atMs: nowMs };
        continue;
      }
      const decayed = peak.value + (current - peak.value) * (age / PEAK_HOLD_MS);
      if (decayed <= current) hot.peaks[role] = { value: current, atMs: nowMs };
      else hot.peaks[role] = { value: decayed, atMs: peak.atMs };
    }
  }
}
