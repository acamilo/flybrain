/**
 * Pure(ish) state machine behind the fake flysim: builds one `FeedHeader` + attachment set per
 * tick, and implements the control API's stateful pieces (stimulate rate limiting, checkpoints,
 * pause/resume, event log). No networking here — `fake/server.ts` wires this into `ws` and
 * `node:http`. Kept separate so tests can drive it directly ("drive the generator directly, not
 * over the network").
 *
 * Button bit order (`GAMEBOY_BUTTON_BITS`) mirrors `packages/brain/src/readout/presets/gameboy.ts`
 * exactly: up=1<<0, down=1<<1, left=1<<2, right=1<<3, a=1<<4, b=1<<5, start=1<<6, select=1<<7.
 * `command_0..3` are the D-pad directions in that same order, `command_4`/`command_5` are A/B,
 * `command_6`/`command_7` are Start/Select — matching `gameboyDecoderConfig()`'s channel map.
 */
import { AUDIO_RATE, FRAME_HEIGHT, FRAME_WIDTH } from '../types';
import type {
  AttachmentKind,
  ChatLine,
  ChatRequest,
  CheckpointResponse,
  EventsResponse,
  FeedEvent,
  FeedGame,
  FeedHeader,
  FeedStatus,
  FeedVersions,
  RewardKind,
  RewardRequest,
  StatusResponse,
  StimulateRequest,
} from '../types';
import { MACRO_TYPES, macroRateRole, type MacroMode } from '../types';
import { CHAT_RING_MAX, classifyChatText, type ChatRejectReason } from '../chat';
import { validateDisplayName, FALLBACK_DISPLAY_NAME } from '../names';
import { FakePalette, gameModeForScene, type PinnedScene } from './palette';
import { chance, mulberry32, pick, randInt, randRange, wander, type Rng } from './prng';

/**
 * Which run this fake is.
 *
 * The first four are about the *status and the ladder*: `boot` never leaves booting, `stuck` sits
 * on rung 4, `milestone` climbs, `running` skips the boot wait. `shop` and `center` are section
 * 13's addition and are about the *scene*: they pin the palette's walk to a mart counter and to
 * the inside of a Pokémon Center, which is what makes a mockup and a Playwright baseline of
 * either pad reproducible at a fixed seek time. They run exactly like `running` otherwise.
 */
export type FakeScenario = 'boot' | 'running' | 'stuck' | 'milestone' | 'shop' | 'center';

/** The scene a scenario pins the palette to, or null for the four that pin none. */
function pinnedScene(scenario: FakeScenario): PinnedScene | null {
  return scenario === 'shop' || scenario === 'center' ? scenario : null;
}

export interface FakeFlysimOptions {
  scenario?: FakeScenario;
  /** PRNG seed; same seed + same tick sequence reproduces the same run. */
  seed?: number;
  /** `POST /reward` is 403 unless this is true (mirrors `control.allow_reward`). */
  allowReward?: boolean;
  /** Neuron count backing the `spikes` bitset. Defaults to the real dataset's 139,255. */
  neuronCount?: number;
  /** Approximate spikes generated per snapshot. Defaults to the live service's measured ~30,000. */
  spikesPerTick?: number;
  /** flysim.toml [control] defaults. */
  sugarDefaultMs?: number;
  sugarMaxMs?: number;
  sugarPerMinute?: number;
  /** flysim.toml [loop] checkpoint_seconds, in ms. */
  checkpointIntervalMs?: number;
  /** flysim.toml `[chat] enabled`. False makes `POST /chat` 403 and omits the header field. */
  chatEnabled?: boolean;
  /** flysim.toml `[chat] ring`, clamped to `CHAT_RING_MAX`. */
  chatRing?: number;
  /** `POST /chat` per-name limit, ms between accepted lines from one name. */
  chatPerNameMs?: number;
  /** `POST /chat` global limit, accepted lines per second across all names. */
  chatPerSecond?: number;
  /** Scripted viewer chatter. False leaves the ring empty until something posts to `/chat`. */
  chatChatter?: boolean;
  /**
   * `flysim.toml`'s `[macros] mode`: whether the scene's macros are on the pad.
   *
   * `raw` is the default here as it is there (`docs/design/macros.md` section 1: "Raw mode remains
   * and is the default until measured"); `macros` is section 12's other mode, where the scene
   * binds a macro channel per type it allows. In raw mode no scene is claimed and the palette is
   * empty with a null macro, which is exactly the state the page's raw layout is drawn from.
   */
  macroMode?: MacroMode;
  /**
   * Deal the fake's widest pad in every scene, for a fixture recorded for the screen.
   *
   * Without it the random walk reaches nine macros in one scene and three in the next, so the
   * strip's second column and the keyboard's lit cells are spotty. With it every overworld walk
   * binds all nine of the outdoor macros at once, which is what the `bigpad` fixture was recorded
   * with and what the section-14 mockups regress.
   */
  fullPad?: boolean;
}

export interface FakeSnapshot {
  header: FeedHeader;
  attachments: {
    frame: Uint8Array;
    audio: Uint8Array;
    spikes: Uint8Array;
  };
}

export type StimulateResult = { ok: true; eventId: number } | { ok: false; retryAfterMs: number };
export type RewardResult = { ok: true; eventId: number } | { ok: false; disabled: true };

/** `POST /chat`: 202, 403 (kill switch), 422 (a rule refused it) or 429 (a rate limit). */
export type ChatResult =
  | { ok: true; eventId: number }
  | { ok: false; kind: 'disabled' }
  | { ok: false; kind: 'rejected'; reason: ChatRejectReason | 'name' }
  | { ok: false; kind: 'rate_limited'; retryAfterMs: number };

const DIRECTIONS = ['up', 'down', 'left', 'right'] as const;
const GAMEBOY_BUTTON_BITS: Record<string, number> = {
  up: 1 << 0, down: 1 << 1, left: 1 << 2, right: 1 << 3, a: 1 << 4, b: 1 << 5, start: 1 << 6, select: 1 << 7,
};
const DIRECTION_ROLE = ['command_0', 'command_1', 'command_2', 'command_3'] as const;

/**
 * A 38-rung ladder, the same length as the real Pokémon Red one
 * (`docs/design/ladder.md`), so `milestone.total` is 38 and a consumer that sizes
 * anything off it is exercised at the real length.
 *
 * Deliberately not the real ladder's wording: the stage draws the *current* rung's
 * label from the feed and the rest from its own per-game config, and the two
 * differing is what proves the feed is the authoritative one.
 */
const MILESTONE_LADDER: string[] = [
  'Boot screen',
  'Left the bedroom',
  'Left the house',
  'Reached Pallet Town',
  "Reached Oak's lab",
  'Chose a starter',
  "Delivered Oak's parcel",
  'Received the Pokédex',
  'Reached Viridian City',
  'Entered Viridian Forest',
  'Reached Pewter City',
  'Earned Boulder Badge',
  'Reached Mt. Moon',
  'Reached Cerulean City',
  'Earned Cascade Badge',
  'Crossed Nugget Bridge',
  'Helped Bill',
  'Reached Vermilion City',
  'Learned Cut',
  'Earned Thunder Badge',
  'Entered Rock Tunnel',
  'Reached Lavender Town',
  'Reached Celadon City',
  'Found the Silph Scope',
  'Earned Rainbow Badge',
  'Found the Poké Flute',
  'Reached Fuchsia City',
  'Earned Soul Badge',
  'Freed Silph Co.',
  'Earned Marsh Badge',
  'Reached Cinnabar Island',
  'Earned Volcano Badge',
  'Earned Earth Badge',
  'Reached Indigo Plateau',
  'Beat Lorelei',
  'Beat Bruno',
  'Beat Agatha',
  'Became Champion',
];

const REWARD_LABELS: Record<RewardKind, string[]> = {
  story: ['Advanced the main story', 'Triggered a story flag', 'Talked to a key NPC'],
  explore: ['Explored a new room', 'Found a hidden path', 'Walked off the beaten trail'],
  area: ['Entered a new area', 'Crossed into unmapped territory', 'Discovered a new route'],
  pokedex: ['Registered a new Pokédex entry', 'Saw a new species', 'Caught a new species'],
  trainer: ['Beat a trainer battle', 'Defeated a rival encounter', 'Won a scripted trainer fight'],
  wildwin: ['Won a wild encounter', 'Fought off a wild Pokémon', 'Survived a wild battle'],
  badge: ['Earned a Gym Badge', 'Defeated a Gym Leader'],
};

const REWARD_KIND_WEIGHTS: Array<[RewardKind, number]> = [
  ['explore', 40],
  ['area', 20],
  ['wildwin', 15],
  ['trainer', 10],
  ['pokedex', 10],
  ['story', 4],
  ['badge', 1],
];

const REWARD_VALUE_RANGE: Record<RewardKind, [number, number]> = {
  explore: [0.02, 0.08],
  area: [0.1, 0.2],
  wildwin: [0.05, 0.15],
  trainer: [0.2, 0.4],
  pokedex: [0.15, 0.3],
  story: [0.3, 0.6],
  badge: [1, 1],
};

/**
 * Scripted viewer chatter for the persistent CHAT panel (`docs/stream-mvp-plan.md`, "Rail layout
 * v2"). Every line here passes `sanitizeChatText` and every name passes `validateDisplayName`,
 * because the fake feeds them through both on the way in, exactly as the real service does.
 *
 * `bot: true` lines are the bridge's own template replies, which it posts back to `/chat` so they
 * appear on screen alongside the viewers it is answering.
 */
const CHAT_SCRIPT: ReadonlyArray<{ by: string; text: string; bot?: boolean }> = [
  { by: 'mothra_fan', text: 'the ledge is RIGHT there' },
  { by: 'kc_gamma', text: 'it has been stuck on this rung for 20 minutes' },
  { by: 'ari_9', text: 'go left!! left!!' },
  { by: 'flybridgebot', text: 'Sugar from ari_9! The fly gets a brief PAM reward pulse.', bot: true },
  { by: 'proboscis_enjoyer', text: 'watching the reward bar is weirdly calming' },
  { by: 'lena_k', text: 'wait it actually learned to back out of the menu?' },
  { by: 'dendrite', text: 'first time seeing this, are those real neurons' },
  { by: 'flybridgebot', text: 'Every button press comes from the network. Say !how for what is real.', bot: true },
  { by: 'norbert', text: 'i have been here for three hours and i regret nothing' },
  { by: 'pam_neuron', text: 'good fly. good fly!' },
];

const FAKE_VERSIONS: FeedVersions = {
  kernel: 'lif-1ms-f64-v2',
  plasticity: 'fly-kc-mbon-rstdp-v2',
  adapter: 'pokemon-red-fake-v1',
  binjgb: 'binjgb-fake',
  dataset: 'fake' + '0'.repeat(60),
};

/** Game Boy 4-tone palette (darkest to lightest), RGBA. No Nintendo assets involved. */
const GB_PALETTE: ReadonlyArray<readonly [number, number, number, number]> = [
  [15, 56, 15, 255],
  [48, 98, 48, 255],
  [139, 172, 15, 255],
  [155, 188, 15, 255],
];

function drawFrame(frameCounter: number): Uint8Array {
  const rgba = new Uint8Array(FRAME_WIDTH * FRAME_HEIGHT * 4);
  const stripeX = frameCounter % FRAME_WIDTH;
  for (let y = 0; y < FRAME_HEIGHT; y++) {
    for (let x = 0; x < FRAME_WIDTH; x++) {
      const tileTone = ((x >> 3) + (y >> 3)) % 4;
      const distToStripe = Math.min(Math.abs(x - stripeX), FRAME_WIDTH - Math.abs(x - stripeX));
      const tone = distToStripe < 3 ? 3 : tileTone;
      const [r, g, b, a] = GB_PALETTE[tone] as readonly [number, number, number, number];
      const offset = (y * FRAME_WIDTH + x) * 4;
      rgba[offset] = r;
      rgba[offset + 1] = g;
      rgba[offset + 2] = b;
      rgba[offset + 3] = a;
    }
  }
  return rgba;
}

function drawAudio(samples: number, phaseStart: number, amplitude: number, freqHz: number): { bytes: Uint8Array; phaseEnd: number } {
  const floats = new Float32Array(Math.max(0, samples) * 2);
  let phase = phaseStart;
  const step = (2 * Math.PI * freqHz) / AUDIO_RATE;
  for (let i = 0; i < samples; i++) {
    const v = Math.sin(phase) * amplitude;
    floats[i * 2] = v;
    floats[i * 2 + 1] = v;
    phase += step;
  }
  phase %= 2 * Math.PI;
  return { bytes: new Uint8Array(floats.buffer, floats.byteOffset, floats.byteLength), phaseEnd: phase };
}

function drawSpikes(rng: Rng, neuronCount: number, targetCount: number): { bitset: Uint8Array; count: number } {
  const byteLength = Math.ceil(neuronCount / 8);
  const bitset = new Uint8Array(byteLength);
  const want = Math.min(targetCount, neuronCount);
  const chosen = new Set<number>();
  while (chosen.size < want) {
    chosen.add(randInt(rng, 0, neuronCount - 1));
  }
  for (const index of chosen) {
    const byte = index >> 3;
    const mask = 1 << (index & 7);
    bitset[byte] = (bitset[byte] as number) | mask;
  }
  return { bitset, count: chosen.size };
}

function weightedRewardKind(rng: Rng): RewardKind {
  const total = REWARD_KIND_WEIGHTS.reduce((sum, [, w]) => sum + w, 0);
  let roll = rng() * total;
  for (const [kind, weight] of REWARD_KIND_WEIGHTS) {
    if (roll < weight) return kind;
    roll -= weight;
  }
  return 'explore';
}

/** Deterministic, self-contained synthetic flysim: no `@flybrain/brain` dependency. */
export class FakeFlysim {
  private readonly scenario: FakeScenario;
  private readonly rng: Rng;
  private readonly allowReward: boolean;
  private readonly neuronCount: number;
  private readonly spikesPerTick: number;
  private readonly sugarDefaultMs: number;
  private readonly sugarMaxMs: number;
  private readonly sugarPerMinute: number;
  private readonly checkpointIntervalMs: number;
  private readonly chatEnabled: boolean;
  private readonly chatRing: number;
  private readonly chatPerNameMs: number;
  private readonly chatPerSecond: number;
  private readonly chatChatter: boolean;
  private readonly macroMode: MacroMode;
  private readonly fullPad: boolean;
  private readonly palette: FakePalette;

  private seq = 0;
  private frameCounter = 0;
  private elapsedMs = 0;
  private paused = false;
  private runSeconds = 0;

  private dominantDirection = 0;
  private dominantUntilMs = 400;
  // Resting values in the tens of Hz, matching the live run's measured order of magnitude for
  // these tracked roles (infra/docs/p0-measurements.md's local WSL run; the first live screenshot
  // against these fixtures pegged every circuit bar at 100% because a fixed panel scale was tuned
  // to fixture numbers well below what the real service produces). `populationRate` below is left
  // alone: it already matches the live ~13 Hz measurement.
  private rates: Record<string, number> = {
    command_0: 10, command_1: 10, command_2: 10, command_3: 10,
    command_4: 10, command_5: 10, command_6: 4, command_7: 4,
    steer_left: 9, steer_right: 9, forward: 11, backward: 8, proboscis: 7, reward_pam: 4,
  };
  private aPulseUntilMs = -1;
  private bPulseUntilMs = -1;
  private startPulseUntilMs = -1;
  private selectPulseUntilMs = -1;
  private populationRate = 13;

  private learningUpdates = 0;
  private learningChanged = 0;
  private readonly learningSynapses = 4_200_000;
  private learningSignal = 0;

  private badges = 0;
  private uniqueLocations = 1;
  private rewardTotal = 0;
  private rewardCounts: Record<RewardKind, number> = {
    story: 0, explore: 0, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0,
  };
  private nextRewardAtMs: number;

  private milestoneRank: number;
  private milestoneSinceMs = 0;
  private milestoneAttempts = 0;
  private nextMilestoneStepMs = 90_000;
  private nextStuckAttemptMs = 25_000;

  private sugarActive = false;
  private sugarRemainingMs = 0;
  private sugarCooldownMs = 0;
  private sugarLastBy: string | null = null;
  private sugarTodayCount = 0;
  private stimulateTimestamps: number[] = [];

  private chat_: ChatLine[] = [];
  private chatScriptIndex = 0;
  private nextChatAtMs = 0;
  private chatLastByMs = new Map<string, number>();
  private chatTimestamps: number[] = [];

  private checkpointGeneration = 0;
  private lastCheckpointWallMs = Date.now();
  private nextCheckpointAtMs: number;

  private audioPhase = 0;

  private readonly eventLog: FeedEvent[] = [];
  private pendingEvents: FeedEvent[] = [];
  private nextEventId = 1;

  /** Snapshot header from the most recent `tick()`, used by `status()` without re-stepping. */
  private lastHeader: FeedHeader;

  constructor(options: FakeFlysimOptions = {}) {
    this.scenario = options.scenario ?? 'running';
    this.rng = mulberry32(options.seed ?? 0xf1b0a1e);
    this.allowReward = options.allowReward ?? false;
    this.neuronCount = options.neuronCount ?? 139_255;
    this.spikesPerTick = options.spikesPerTick ?? 30_000;
    this.sugarDefaultMs = options.sugarDefaultMs ?? 400;
    this.sugarMaxMs = options.sugarMaxMs ?? 1000;
    this.sugarPerMinute = options.sugarPerMinute ?? 6;
    this.checkpointIntervalMs = options.checkpointIntervalMs ?? 5000;
    this.chatEnabled = options.chatEnabled ?? true;
    this.chatRing = Math.max(1, Math.min(options.chatRing ?? CHAT_RING_MAX, CHAT_RING_MAX));
    this.chatPerNameMs = options.chatPerNameMs ?? 2000;
    this.chatPerSecond = options.chatPerSecond ?? 5;
    this.chatChatter = options.chatChatter ?? true;
    this.macroMode = options.macroMode ?? 'raw';
    this.fullPad = options.fullPad ?? false;
    this.nextCheckpointAtMs = this.checkpointIntervalMs;
    this.nextRewardAtMs = randRange(this.rng, 15_000, 25_000);
    this.milestoneRank = this.scenario === 'stuck' ? 4 : 0;

    if (this.scenario === 'running' || pinnedScene(this.scenario) !== null) {
      // Dev convenience: skip the boot wait so the running scenario is immediately useful. The
      // two pinned scenes want the same thing for the same reason: the pad is what they are for.
      this.elapsedMs = 3000;
    }

    this.palette = new FakePalette(
      this.rng,
      this.macroMode,
      this.elapsedMs,
      pinnedScene(this.scenario),
      this.fullPad,
    );

    // Seed `lastHeader` once so `status()` has something to read before the server's own tick
    // loop starts; this is the only tick a status-only caller ever forces.
    this.lastHeader = this.tick(0).header;
  }

  private currentStatus(): FeedStatus {
    if (this.paused) return 'paused';
    if (this.scenario === 'boot') return 'booting';
    return this.elapsedMs < 3000 ? 'booting' : 'running';
  }

  /** Advance the simulation by `dtMs` of simulated time and produce one snapshot. */
  tick(dtMs: number): FakeSnapshot {
    this.seq += 1;
    this.frameCounter += 1;
    this.elapsedMs += dtMs;

    const status = this.currentStatus();
    const running = status === 'running';
    if (running) {
      this.runSeconds += dtMs / 1000;
      this.stepButtons(dtMs);
      this.stepLearning(dtMs);
      this.stepMilestone(dtMs);
      this.stepRewards();
      this.stepAutoCheckpoint();
      this.stepChat();
      this.stepPalette();
      this.stepMacroRates();
    }
    this.stepSugar(dtMs);

    const buttons = this.currentButtonMask();
    const palette = this.palette.state(this.elapsedMs);
    const game: FeedGame = {
      // In macros mode the scene and the mode word are the same fact: `battle` cannot be on
      // screen beside `OVERWORLD`. While booting the adapter has nothing to classify yet, and in
      // raw mode there is no scene to follow — the contract has `scene` as `unknown` there — so the
      // mode word stays what the fake has always published.
      mode:
        status === 'booting'
          ? 'BOOT'
          : this.macroMode === 'raw'
            ? 'OVERWORLD'
            : gameModeForScene(palette.scene),
      semanticRewards: true,
      map: running ? 1 + (this.milestoneRank % 12) : null,
      badges: this.badges,
      uniqueLocations: this.uniqueLocations,
      rewardTotal: round4(this.rewardTotal),
      rewardCounts: { ...this.rewardCounts },
      scene: status === 'booting' ? 'title' : palette.scene,
      macroMode: this.macroMode,
      palette: palette.entries,
      macro: palette.macro,
      macroOutcome: palette.outcome,
    };

    const rank = Math.min(this.milestoneRank, MILESTONE_LADDER.length - 1);
    const nextLabel = MILESTONE_LADDER[Math.min(rank + 1, MILESTONE_LADDER.length - 1)] as string;

    const frameBytes = drawFrame(this.frameCounter);
    const samples = Math.round((AUDIO_RATE * dtMs) / 1000);
    const audio = drawAudio(samples, this.audioPhase, 0.05, 220);
    this.audioPhase = audio.phaseEnd;
    const spikes = running ? drawSpikes(this.rng, this.neuronCount, this.jitterSpikeCount()) : { bitset: new Uint8Array(Math.ceil(this.neuronCount / 8)), count: 0 };

    const events = this.pendingEvents;
    this.pendingEvents = [];

    const attachments: AttachmentKind[] = ['frame', 'audio', 'spikes'];

    const header: FeedHeader = {
      protocol: 1,
      seq: this.seq,
      wallMs: Date.now(),
      status,
      realtimeFactor: running ? round4(1 + randRange(this.rng, -0.02, 0.02)) : 0,
      uptimeSeconds: round4(this.elapsedMs / 1000),
      runSeconds: round4(this.runSeconds),
      brainMs: this.elapsedMs,
      frame: this.frameCounter,
      buttons,
      rates: roundRates(this.rates),
      populationRate: round4(this.populationRate),
      spikeCount: spikes.count,
      learning: {
        enabled: true,
        updates: this.learningUpdates,
        changed: this.learningChanged,
        synapses: this.learningSynapses,
        signal: round4(this.learningSignal),
      },
      game,
      milestone: {
        rank,
        label: MILESTONE_LADDER[rank] as string,
        next: nextLabel,
        sinceSeconds: round4(this.milestoneSinceMs / 1000),
        attempts: this.milestoneAttempts,
        total: MILESTONE_LADDER.length,
      },
      sugar: {
        active: this.sugarActive,
        remainingMs: Math.max(0, Math.round(this.sugarRemainingMs)),
        cooldownMs: Math.max(0, Math.round(this.sugarCooldownMs)),
        lastBy: this.sugarLastBy,
        todayCount: this.sugarTodayCount,
      },
      events,
      // The kill switch omits the field entirely rather than sending an empty array, so a page
      // can tell "chat is off" from "nobody has said anything yet".
      chat: this.chatEnabled ? [...this.chat_] : undefined,
      attachments,
    };

    this.lastHeader = header;
    return { header, attachments: { frame: frameBytes, audio: audio.bytes, spikes: spikes.bitset } };
  }

  private jitterSpikeCount(): number {
    return Math.max(0, Math.round(this.spikesPerTick + randRange(this.rng, -100, 100)));
  }

  private stepButtons(dtMs: number): void {
    // A macro no longer borrows a button channel to be chosen on (`docs/design/macros.md` section
    // 12: the macro channels are their own group), so nothing is held here while one runs. The
    // real sim would be pressing the macro's script; this one keeps generating raw presses, which
    // is the right kind of lie and is stated where `stepPalette` wires it in.
    if (this.elapsedMs >= this.dominantUntilMs) {
      let next = randInt(this.rng, 0, 3);
      if (DIRECTIONS.length > 1) {
        while (next === this.dominantDirection) next = randInt(this.rng, 0, 3);
      }
      this.dominantDirection = next;
      this.dominantUntilMs = this.elapsedMs + 400 + randRange(this.rng, -60, 60);
    }

    for (let i = 0; i < 4; i++) {
      const role = DIRECTION_ROLE[i] as string;
      const target = i === this.dominantDirection ? randRange(this.rng, 35, 55) : randRange(this.rng, 8, 18);
      this.rates[role] = wander(this.rng, this.rates[role] ?? target, target, 0.35, 1.2, 5, 70);
    }

    if (this.aPulseUntilMs < this.elapsedMs && chance(this.rng, dtMs / 2500)) {
      this.aPulseUntilMs = this.elapsedMs + 85;
    }
    if (this.bPulseUntilMs < this.elapsedMs && chance(this.rng, dtMs / 3200)) {
      this.bPulseUntilMs = this.elapsedMs + 85;
    }
    if (this.startPulseUntilMs < this.elapsedMs && chance(this.rng, dtMs / 30_000)) {
      this.startPulseUntilMs = this.elapsedMs + 55;
    }
    if (this.selectPulseUntilMs < this.elapsedMs && chance(this.rng, dtMs / 30_000)) {
      this.selectPulseUntilMs = this.elapsedMs + 55;
    }

    this.rates.command_4 = wander(this.rng, this.rates.command_4 ?? 10, this.aPulseUntilMs >= this.elapsedMs ? 50 : 10, 0.4, 1, 5, 70);
    this.rates.command_5 = wander(this.rng, this.rates.command_5 ?? 10, this.bPulseUntilMs >= this.elapsedMs ? 50 : 10, 0.4, 1, 5, 70);
    this.rates.command_6 = wander(this.rng, this.rates.command_6 ?? 4, this.startPulseUntilMs >= this.elapsedMs ? 35 : 4, 0.4, 0.5, 2, 45);
    this.rates.command_7 = wander(this.rng, this.rates.command_7 ?? 4, this.selectPulseUntilMs >= this.elapsedMs ? 35 : 4, 0.4, 0.5, 2, 45);

    this.rates.steer_left = wander(this.rng, this.rates.steer_left ?? 9, 9, 0.2, 0.8, 2, 22);
    this.rates.steer_right = wander(this.rng, this.rates.steer_right ?? 9, 9, 0.2, 0.8, 2, 22);
    this.rates.forward = wander(this.rng, this.rates.forward ?? 11, 11, 0.2, 0.8, 2, 22);
    this.rates.backward = wander(this.rng, this.rates.backward ?? 8, 8, 0.2, 0.8, 2, 20);
    this.rates.proboscis = wander(this.rng, this.rates.proboscis ?? 7, 7, 0.2, 0.6, 1, 18);
    this.rates.reward_pam = wander(this.rng, this.rates.reward_pam ?? 4, this.sugarActive ? 38 : 4, 0.3, 1, 2, 55);

    this.populationRate = wander(this.rng, this.populationRate, 13, 0.2, 0.6, 8, 20);
  }

  private currentButtonMask(): number {
    let mask = 0;
    const status = this.currentStatus();
    if (status === 'running') {
      mask |= GAMEBOY_BUTTON_BITS[DIRECTIONS[this.dominantDirection] as string] as number;
      if (this.aPulseUntilMs >= this.elapsedMs) mask |= GAMEBOY_BUTTON_BITS.a as number;
      if (this.bPulseUntilMs >= this.elapsedMs) mask |= GAMEBOY_BUTTON_BITS.b as number;
      if (this.startPulseUntilMs >= this.elapsedMs) mask |= GAMEBOY_BUTTON_BITS.start as number;
      if (this.selectPulseUntilMs >= this.elapsedMs) mask |= GAMEBOY_BUTTON_BITS.select as number;
    }
    return mask;
  }

  private stepLearning(dtMs: number): void {
    const updates = randInt(this.rng, 0, 3);
    this.learningUpdates += updates;
    this.learningChanged += randInt(this.rng, 0, Math.min(2, updates));
    this.learningSignal = wander(this.rng, this.learningSignal, 0, 0.3, 0.04, -1, 1);
    void dtMs;
  }

  private stepMilestone(dtMs: number): void {
    this.milestoneSinceMs += dtMs;

    if (this.scenario === 'milestone') {
      if (this.elapsedMs >= this.nextMilestoneStepMs && this.milestoneRank < MILESTONE_LADDER.length - 1) {
        this.milestoneRank += 1;
        this.milestoneSinceMs = 0;
        this.nextMilestoneStepMs += 90_000;
        this.pushEvent({
          kind: 'milestone',
          label: `Reached: ${MILESTONE_LADDER[this.milestoneRank]}`,
          value: this.milestoneRank,
        });
      }
      return;
    }

    if (this.scenario === 'stuck') {
      if (this.elapsedMs >= this.nextStuckAttemptMs) {
        this.milestoneAttempts += 1;
        this.nextStuckAttemptMs += 25_000;
        this.pushEvent({
          kind: 'recovery',
          label: `Rolled back after getting stuck on "${MILESTONE_LADDER[this.milestoneRank]}"`,
          value: this.milestoneAttempts,
        });
      }
      return;
    }

    // boot / running: benign default, no scripted rank advancement.
  }

  private stepRewards(): void {
    if (this.elapsedMs < this.nextRewardAtMs) return;
    this.nextRewardAtMs = this.elapsedMs + randRange(this.rng, 15_000, 25_000);

    const kind = weightedRewardKind(this.rng);
    const [lo, hi] = REWARD_VALUE_RANGE[kind] as [number, number];
    const value = round4(randRange(this.rng, lo, hi));
    this.rewardTotal += value;
    this.rewardCounts[kind] += 1;
    if (kind === 'area' || kind === 'explore') this.uniqueLocations += 1;
    if (kind === 'badge') this.badges = Math.min(8, this.badges + 1);
    this.learningSignal = Math.min(1, this.learningSignal + 0.2);

    this.pushEvent({
      kind: 'reward',
      label: pick(this.rng, REWARD_LABELS[kind]),
      value,
      rewardKind: kind,
    });
  }

  private stepAutoCheckpoint(): void {
    if (this.elapsedMs < this.nextCheckpointAtMs) return;
    this.nextCheckpointAtMs += this.checkpointIntervalMs;
    this.checkpointGeneration += 1;
    this.lastCheckpointWallMs = Date.now();
    this.pushEvent({ kind: 'checkpoint', label: `Checkpoint ${this.checkpointGeneration} saved` });
  }

  /**
   * Scripted viewer chatter, one line every 8 to 22 simulated seconds, cycling `CHAT_SCRIPT`.
   *
   * Routed through the same admission path as `POST /chat`, so the fake cannot show a line the
   * real service would have refused. The wall-clock rate limits are skipped here: a fixture
   * replayed faster than real time must still produce chatter.
   */
  private stepChat(): void {
    if (!this.chatChatter || !this.chatEnabled) return;
    if (this.elapsedMs < this.nextChatAtMs) return;
    this.nextChatAtMs = this.elapsedMs + randRange(this.rng, 8_000, 22_000);

    const line = CHAT_SCRIPT[this.chatScriptIndex % CHAT_SCRIPT.length];
    this.chatScriptIndex += 1;
    if (line) this.acceptChatLine(line.by, line.text, line.bot ?? false);
  }

  /** Append one already-admitted line to the ring and log the matching `viewer` event. */
  private acceptChatLine(by: string, text: string, bot: boolean): number {
    const event = this.pushEvent({ kind: 'viewer', label: 'chat', by });
    const line: ChatLine = { id: event.id, wallMs: event.wallMs, by, text };
    if (bot) line.bot = true;
    this.chat_.push(line);
    while (this.chat_.length > this.chatRing) this.chat_.shift();
    return event.id;
  }

  /**
   * The scene walk and the macro decisions (`./palette.ts`), plus their `macro` events.
   *
   * One thing the fake cannot be faithful about: a macro's button script. There is no emulator
   * here, so while a macro runs the button mask keeps coming from `stepButtons` — which is the
   * right *kind* of lie (the presses are real presses either way, `docs/design/macros.md` section
   * 1) but not the right presses. `value` carries the slot so the event log can group by it.
   */
  private stepPalette(): void {
    for (const event of this.palette.step(this.elapsedMs)) {
      this.pushEvent({ kind: 'macro', label: event.label, value: event.slot });
    }
  }

  /**
   * The macro channels' own rates (`docs/design/macros.md` sections 11 and 12).
   *
   * All 22 are published in both modes, because they are populations of real neurons and they fire
   * whatever the decoder does with them. What the mode changes is the shape: the channels the
   * scene binds run above the masked ones and the running macro's channel runs highest, so the
   * SENSES panel's MACROS row and the lit cell say the same thing.
   */
  private stepMacroRates(): void {
    const bound = new Set(this.palette.boundNames());
    const running = this.palette.runningName;
    for (const name of MACRO_TYPES) {
      const role = macroRateRole(name);
      const target =
        name === running
          ? randRange(this.rng, 38, 52)
          : bound.has(name)
            ? randRange(this.rng, 14, 24)
            : randRange(this.rng, 3, 8);
      this.rates[role] = wander(this.rng, this.rates[role] ?? target, target, 0.35, 1.2, 2, 70);
    }
  }

  private stepSugar(dtMs: number): void {
    if (this.sugarActive) {
      this.sugarRemainingMs -= dtMs;
      if (this.sugarRemainingMs <= 0) {
        this.sugarRemainingMs = 0;
        this.sugarActive = false;
      }
    }
    if (this.sugarCooldownMs > 0) {
      this.sugarCooldownMs = Math.max(0, this.sugarCooldownMs - dtMs);
    }
  }

  private pushEvent(partial: Omit<FeedEvent, 'id' | 'wallMs' | 'brainMs'>): FeedEvent {
    const event: FeedEvent = { id: this.nextEventId++, wallMs: Date.now(), brainMs: this.elapsedMs, ...partial };
    this.pendingEvents.push(event);
    this.eventLog.push(event);
    return event;
  }

  // -- Control API -------------------------------------------------------------------------

  stimulate(request: StimulateRequest, nowWallMs = Date.now()): StimulateResult {
    this.stimulateTimestamps = this.stimulateTimestamps.filter((t) => nowWallMs - t < 60_000);

    if (this.sugarActive) {
      return { ok: false, retryAfterMs: Math.max(0, Math.round(this.sugarRemainingMs)) };
    }
    if (this.stimulateTimestamps.length >= this.sugarPerMinute) {
      const oldest = this.stimulateTimestamps[0] as number;
      return { ok: false, retryAfterMs: Math.max(0, 60_000 - (nowWallMs - oldest)) };
    }

    const durationMs = Math.min(request.durationMs ?? this.sugarDefaultMs, this.sugarMaxMs);
    this.stimulateTimestamps.push(nowWallMs);
    this.sugarActive = true;
    this.sugarRemainingMs = durationMs;
    this.sugarLastBy = request.by;
    this.sugarTodayCount += 1;

    const event = this.pushEvent({
      kind: 'sugar',
      label: `${request.by} fed the fly sugar`,
      by: request.by,
      value: durationMs,
    });
    return { ok: true, eventId: event.id };
  }

  reward(request: RewardRequest): RewardResult {
    if (!this.allowReward) return { ok: false, disabled: true };
    this.rewardTotal += request.value;
    this.learningSignal = Math.max(-1, Math.min(1, this.learningSignal + request.value));
    const event = this.pushEvent({
      kind: 'reward',
      label: `${request.by} sent a reward pulse (${request.value})`,
      value: request.value,
    });
    return { ok: true, eventId: event.id };
  }

  /**
   * `POST /chat` (`docs/control-api.md`): validate the name, sanitize the text, rate-limit per
   * name and globally, then append one line to the ring. Chat never reaches the simulation —
   * nothing here touches the network, the emulator or the reward path.
   */
  chat(request: ChatRequest, nowWallMs = Date.now()): ChatResult {
    if (!this.chatEnabled) return { ok: false, kind: 'disabled' };

    const by = validateDisplayName(request.by);
    if (by === FALLBACK_DISPLAY_NAME && request.by !== FALLBACK_DISPLAY_NAME) {
      return { ok: false, kind: 'rejected', reason: 'name' };
    }
    const text = classifyChatText(request.text);
    if (!text.ok) return { ok: false, kind: 'rejected', reason: text.reason };

    this.chatTimestamps = this.chatTimestamps.filter((at) => nowWallMs - at < 1000);
    if (this.chatTimestamps.length >= this.chatPerSecond) {
      const oldest = this.chatTimestamps[0] as number;
      return { ok: false, kind: 'rate_limited', retryAfterMs: Math.max(1, 1000 - (nowWallMs - oldest)) };
    }
    const lastByMs = this.chatLastByMs.get(by);
    if (lastByMs !== undefined && nowWallMs - lastByMs < this.chatPerNameMs) {
      return {
        ok: false,
        kind: 'rate_limited',
        retryAfterMs: Math.max(1, this.chatPerNameMs - (nowWallMs - lastByMs)),
      };
    }

    this.chatTimestamps.push(nowWallMs);
    this.chatLastByMs.set(by, nowWallMs);
    return { ok: true, eventId: this.acceptChatLine(by, text.text, request.bot === true) };
  }

  /** The chat ring as the next snapshot will carry it; `null` while the kill switch is off. */
  chatRingLines(): ChatLine[] | null {
    return this.chatEnabled ? [...this.chat_] : null;
  }

  checkpoint(): CheckpointResponse {
    this.checkpointGeneration += 1;
    this.lastCheckpointWallMs = Date.now();
    this.nextCheckpointAtMs = this.elapsedMs + this.checkpointIntervalMs;
    this.pushEvent({ kind: 'checkpoint', label: `Checkpoint ${this.checkpointGeneration} saved (forced)` });
    return { generation: this.checkpointGeneration };
  }

  pause(): { status: FeedStatus } {
    this.paused = true;
    return { status: 'paused' };
  }

  resume(): { status: FeedStatus } {
    this.paused = false;
    return { status: this.currentStatus() };
  }

  isHealthy(): boolean {
    // The fake loop always advances; kept as a real check so the shape matches the real service.
    return true;
  }

  events(sinceId = 0, limit = 100): EventsResponse {
    const filtered = this.eventLog.filter((event) => event.id > sinceId);
    return { events: filtered.slice(0, Math.max(0, limit)) };
  }

  /**
   * `GET /status`: the header from the most recent `tick()`, minus `events`/`attachments`, plus
   * version strings and checkpoint state. Reading it does not advance the simulation or consume
   * randomness — only the feed's own tick loop does that.
   */
  status(): StatusResponse {
    const { events: _events, attachments: _attachments, ...rest } = this.lastHeader;
    void _events;
    void _attachments;
    return {
      ...rest,
      version: FAKE_VERSIONS,
      checkpoint: { generation: this.checkpointGeneration, latestWallMs: this.lastCheckpointWallMs },
    };
  }
}

function round4(value: number): number {
  return Math.round(value * 10_000) / 10_000;
}

function roundRates(rates: Record<string, number>): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [key, value] of Object.entries(rates)) out[key] = round4(value);
  return out;
}
