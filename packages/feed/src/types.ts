/**
 * Feed protocol v1 and control API v1 types.
 *
 * Binding contracts: `docs/feed-protocol.md` (the WebSocket snapshot feed) and
 * `docs/control-api.md` (the localhost HTTP control API). The Rust side (`services/flysim`)
 * mirrors these shapes with serde and a JSON schema test (`src/schema.json` in this package)
 * pins `FeedHeader` for both languages.
 */

/** Feed and control API protocol version. Bumps on any breaking change. */
export const FEED_PROTOCOL = 1;

/** Default port for the WebSocket snapshot feed (`ws://127.0.0.1:7400/feed`). */
export const FEED_PORT = 7400;

/** Default port for the localhost HTTP control API (`http://127.0.0.1:7401`). */
export const CONTROL_PORT = 7401;

/** Emulator framebuffer width in pixels. */
export const FRAME_WIDTH = 160;

/** Emulator framebuffer height in pixels. */
export const FRAME_HEIGHT = 144;

/** Audio attachment sample rate, Hz. */
export const AUDIO_RATE = 48000;

// ---------------------------------------------------------------------------------------------
// Feed protocol (docs/feed-protocol.md)
// ---------------------------------------------------------------------------------------------

/** Loop / service status, also reported by the control API's `GET /status`. */
export type FeedStatus = 'booting' | 'running' | 'paused' | 'recovering' | 'error';

/** Game mode as classified by the reward adapter. */
export type GameMode = 'BOOT' | 'OVERWORLD' | 'BATTLE' | 'TRANSITION' | 'DEMO' | 'SAFARI' | 'UNKNOWN';

/** Reward categories the adapter tracks counts for. */
export type RewardKind = 'story' | 'explore' | 'area' | 'pokedex' | 'trainer' | 'wildwin' | 'badge';

// ---------------------------------------------------------------------------------------------
// Macro palette. `docs/design/macros.md` is the design and `docs/feed-protocol.md` the contract
// for these five fields; where they differ the protocol wins, which is why the mode is
// `macroMode` here (see `FeedGame`).
// ---------------------------------------------------------------------------------------------

/**
 * Scene the adapter detected, which is what decides the macros on the pad.
 *
 * The wire spelling is lower-case and hyphenated, one name per `Scene` variant in
 * `docs/design/macros.md` section 2, with the battle variant's `forced_switch` flag folded into
 * its own name (`battle-switch`) because the two have different palettes and every consumer
 * switches on the pair rather than on a boolean. Closed set: an adapter with other states folds
 * onto these or reports `unknown`, exactly as `GameMode` already works.
 */
export type GameScene =
  | 'title'
  | 'overworld'
  | 'dialog'
  | 'menu'
  | 'battle'
  | 'battle-switch'
  | 'shop'
  | 'pc'
  | 'unknown';

/**
 * Whether the decoder's channels press buttons only, or buttons and the scene's macros.
 *
 * `raw` is the default and the only mode measured so far (`docs/design/macros.md` section 7): the
 * eight button channels go straight to the button register. `macros` (section 12) adds the second
 * exclusive group: one channel per macro type, of which the scene binds the ones whose
 * preconditions hold, decided with the same hold, hysteresis and fatigue as the direction group.
 * Whichever bound channel wins, starts, and while it runs it owns the pad.
 *
 * Nothing else changes between the two: the readout is the same, and a consumer draws the same
 * strip either way. `palette` and `plan` were the two earlier shapes of this field and are gone.
 */
export type MacroMode = 'raw' | 'macros';

/**
 * Macros a scene can bind at once, which is what `slot` indexes: one per type since section 14.
 *
 * Six until then — the readout's D-pad and A/B from section 1 — which stayed a cap on how many
 * buttons a scene could deal long after a macro stopped being a meaning laid over a button. A slot
 * is now a *type index* ({@link macroTypeIndex}), the strip draws every cell in that fixed order
 * and lights the bound ones, and nothing is ever truncated off a pad.
 */
export const MACRO_SLOTS = 31;

/**
 * The 31 macro types and their channel tags, in the contract's fixed order
 * (`docs/design/macros.md` sections 12, 13 and 14). This order is also the screen's cell order,
 * and since section 14 it is the `slot` index as well, so a cell never moves.
 *
 * A type is a button: its own population (`macro_<type>`), its own channel, the same meaning in
 * every scene. The tag is what a cell and a rate bar are labelled with, short enough for both
 * (`MB` is the mushroom body, whose output neurons the populations are drawn from).
 */
const MACRO_TABLE = [
  ['GO OBJECTIVE', 'MB·GOAL'],
  ['GO OUT', 'MB·OUT'],
  ['GO WARP', 'MB·WARP'],
  ['GO ROUTE', 'MB·ROUTE'],
  ['GO ITEM', 'MB·ITEM'],
  ['GO NPC', 'MB·NPC'],
  ['GO FRONTIER', 'MB·FRONT'],
  // Section 13's two errands, beside the other walks: a cell's neighbours on screen are the
  // macros it is most often dealt with.
  ['GO SHOP', 'MB·SHOP'],
  ['GO HEAL', 'MB·HEAL'],
  ['TALK', 'MB·TALK'],
  ['MENU', 'MB·MENU'],
  ['NEXT', 'MB·NEXT'],
  ['YES', 'MB·YES'],
  ['NO', 'MB·NO'],
  ['CLOSE', 'MB·CLOSE'],
  ['CONFIRM', 'MB·CONF'],
  ['BACK', 'MB·BACK'],
  // Section 14: one button per move slot, where `ATTACK` was. Which move is used is the fly's
  // choice, and no "best move" knowledge remains on either side of the wire.
  ['MOVE 1', 'MB·MV1'],
  ['MOVE 2', 'MB·MV2'],
  ['MOVE 3', 'MB·MV3'],
  ['MOVE 4', 'MB·MV4'],
  ['SWITCH', 'MB·SWAP'],
  ['ITEM', 'MB·BAG'],
  // `MB·BALL` is the ball the fly throws; the mart's is `MB·PBALL`.
  ['THROW BALL', 'MB·BALL'],
  ['RUN', 'MB·RUN'],
  ['BUY POTION', 'MB·POTN'],
  ['BUY BALL', 'MB·PBALL'],
  ['BUY ANTIDOTE', 'MB·ANTI'],
  ['BUY REPEL', 'MB·REPEL'],
  // The conversation at the Pokémon Center counter. Not `MB·HEAL`, which is the walk to its door:
  // two channels cannot share a tag on a screen that identifies a cell by it.
  ['HEAL', 'MB·NURSE'],
  ['LEAVE', 'MB·LEAVE'],
] as const satisfies readonly (readonly [string, string])[];

/** The 31 type names, in contract order. */
export const MACRO_TYPES: readonly string[] = MACRO_TABLE.map(([name]) => name);

/** The 31 channel tags, in the same order. */
export const MACRO_CHANNELS: readonly string[] = MACRO_TABLE.map(([, channel]) => channel);

const MACRO_BY_NAME = new Map<string, { channel: string; index: number }>(
  MACRO_TABLE.map(([name, channel], index) => [name, { channel, index }]),
);

/**
 * Where a type sits in the fixed order, or -1 for a name the contract does not have.
 *
 * This is how a display sorts the scene's macros into cells: by type, not by the slot they
 * arrived in, so a macro is always in the same place on screen.
 */
export function macroTypeIndex(name: string): number {
  return MACRO_BY_NAME.get(name)?.index ?? -1;
}

/** A type's channel tag, or `''` for a name the contract does not have. */
export function macroChannel(name: string): string {
  return MACRO_BY_NAME.get(name)?.channel ?? '';
}

/**
 * A type's rate role in `brain.rates`: `macro_` plus the name lowercased with spaces replaced by
 * underscores, so `GO OUT` is `macro_go_out` (`docs/design/macros.md` section 11).
 *
 * The rule rather than a table, because the dataset generates the roles from the same names
 * (`tools/build_flywire.py`); a table here would be a second place for them to drift.
 */
export function macroRateRole(name: string): string {
  return `macro_${name.toLowerCase().replaceAll(' ', '_')}`;
}

/** One macro the current scene binds. */
export interface PaletteEntry {
  /** 0..5, ascending: which of the scene's bound macros this is, not which button. */
  slot: number;
  /** The macro's name, at most 14 characters (`docs/design/macros.md` section 3). */
  name: string;
  /** Two or three words of what it does in this scene, e.g. "nearest door". */
  gloss: string;
  /**
   * The type's channel tag, e.g. `MB·ATK` ({@link MACRO_CHANNELS}).
   *
   * The channel the decoder would have to win for this macro to start. It is a property of the
   * type rather than of the scene, so it is the same tag wherever the macro is bound.
   */
  channel: string;
}

/** The macro the fly chose and the sim is running. */
export interface RunningMacro {
  slot: number;
  name: string;
  /** Simulated ms since `start` was accepted. */
  sinceMs: number;
}

/** How a macro ended (`docs/design/macros.md` section 4). `refused` never pressed anything. */
export type MacroOutcomeKind = 'done' | 'blocked' | 'timeout' | 'refused';

/** The macro that ended most recently, so a display can show the result for a beat. */
export interface MacroOutcome {
  slot: number;
  name: string;
  outcome: MacroOutcomeKind;
  /** Simulation clock at which it ended. */
  atMs: number;
}

/** Binary attachment kinds that can follow a snapshot header. */
export type AttachmentKind = 'frame' | 'audio' | 'spikes';

/**
 * Event kinds appended to `FeedHeader.events` and the append-only event log.
 *
 * `macro` is the palette's (`docs/design/macros.md` section 5): one event when a macro starts and
 * one when it ends, labelled `<NAME> start` and `<NAME> done|blocked|timeout|refused`, so the
 * ticker shows what the fly chose and the event log records it.
 */
export type FeedEventKind =
  | 'reward'
  | 'sugar'
  | 'milestone'
  | 'recovery'
  | 'checkpoint'
  | 'viewer'
  | 'system'
  | 'macro';

/** Source of a viewer- or operator-triggered action (stimulate/reward). */
export type ActionSource = 'chat' | 'points' | 'operator';

/** One entry in `FeedHeader.events` / the event log. */
export interface FeedEvent {
  /** Monotonically increasing across the run. */
  id: number;
  wallMs: number;
  brainMs: number;
  kind: FeedEventKind;
  /** Short human text, template-generated, never raw chat. */
  label: string;
  /** Reward value or milestone rank. */
  value?: number;
  rewardKind?: RewardKind;
  /** Viewer display name for sugar/viewer events. */
  by?: string;
}

/** Learning / plasticity counters. */
export interface FeedLearning {
  enabled: boolean;
  updates: number;
  changed: number;
  synapses: number;
  signal: number;
}

/** Game state as classified by the reward adapter. */
export interface FeedGame {
  mode: GameMode;
  /** False when the ROM hash is not the audited one. */
  semanticRewards: boolean;
  /** Current map id when known. */
  map: number | null;
  /** 0..8. */
  badges: number;
  /** Exploration coverage. */
  uniqueLocations: number;
  rewardTotal: number;
  rewardCounts: Record<RewardKind, number>;

  // -- Macro palette. Additive, protocol version unchanged (`docs/design/macros.md` section 6).
  // Every field is optional: a producer predating the palette omits all five, and a consumer that
  // sees no `mode` draws the raw-mode layout. In raw mode `palette` is empty and `macro` is null.

  /**
   * Scene the macros are bound for. Absent from a producer with no scene detection, and
   * `unknown` in raw mode, where nothing is bound and so no scene is claimed.
   */
  scene?: GameScene;
  /**
   * What the decoder's channels mean right now. Absent means `raw`.
   *
   * **Named `macroMode`, not `mode`.** `docs/design/macros.md` section 6 writes this field as
   * `game.mode`, which the header has carried since protocol v1 as the adapter's `GameMode`
   * (`BOOT`/`OVERWORLD`/…). Two different closed sets cannot share one key, so the palette's mode
   * takes a qualified name and the older field keeps the one it shipped with. Read it through
   * {@link paletteView} rather than by hand, so a later rename is one function.
   */
  macroMode?: MacroMode;
  /**
   * The macros this scene binds, ascending by slot, 0 to 6 entries.
   *
   * A macro whose precondition fails is omitted rather than sent as null: its channel is masked
   * out of the decision, so the button is not on the pad (`docs/design/macros.md` section 12).
   * The order here is the producer's; a display sorts by type ({@link macroTypeIndex}) so a macro
   * keeps its place on screen from scene to scene.
   */
  palette?: PaletteEntry[];
  /** The macro running right now, or null between macros. */
  macro?: RunningMacro | null;
  /**
   * The macro that ended most recently, kept until the next one starts, or null before the first
   * one ends. Kept rather than pulsed for one frame so a display can show a result for a beat.
   */
  macroOutcome?: MacroOutcome | null;
}

/** Milestone ratchet ladder state. */
export interface FeedMilestone {
  /** Position on the ladder, `0..total - 1`. */
  rank: number;
  /** Human label, e.g. "Left the bedroom". */
  label: string;
  /** Label of rank + 1. */
  next: string;
  /** Simulated seconds at current rank (the stuck-o-meter). */
  sinceSeconds: number;
  /** Game rollbacks since reaching this rank. */
  attempts: number;
  /**
   * Rungs the game adapter's ladder has, so a consumer can draw the right number of them
   * without knowing the game. Optional: a producer predating it omits it, and a consumer
   * that sees no `total` falls back to its own idea of the ladder length.
   */
  total?: number;
}

/** PAM stimulation ("sugar") state. */
export interface FeedSugar {
  /** A stimulation pulse is currently applied. */
  active: boolean;
  remainingMs: number;
  /** Until the next viewer sugar is accepted. */
  cooldownMs: number;
  /** Display name of the last redeemer. */
  lastBy: string | null;
  todayCount: number;
}

/**
 * One accepted chat line in the feed header's bounded `chat` ring.
 *
 * `by` has already been through `validateDisplayName` and `text` through `sanitizeChatText`
 * (`src/chat.ts`) in the bridge, and flysim enforces both again before appending the line — see
 * `docs/control-api.md` (`POST /chat`). Raw chat never reaches the page.
 */
export interface ChatLine {
  /** Monotonically increasing across the run: the event id of the accepted line. */
  id: number;
  wallMs: number;
  /** Validated display name (`src/names.ts` rules). */
  by: string;
  /**
   * Sanitized text: at most `CHAT_MAX_TEXT_LENGTH` code points of letters, digits, spaces and
   * common punctuation, no control characters, no URLs, deny-list filtered. The service refuses
   * anything else.
   */
  text: string;
  /** True for the bridge's own template replies. */
  bot?: boolean;
}

/** The header of one feed snapshot. Small (under 4 KB); attachments carry the bulk. */
export interface FeedHeader {
  protocol: 1;
  /** Monotonically increasing. */
  seq: number;
  /** `Date.now()` at publish. */
  wallMs: number;
  status: FeedStatus;
  /** Simulated ms per wall ms over the last second. */
  realtimeFactor: number;
  /** Since service start. */
  uptimeSeconds: number;
  /** Total simulated seconds across restores (from checkpoint). */
  runSeconds: number;
  /** Simulation clock. */
  brainMs: number;
  /** Emulator frame counter. */
  frame: number;
  /** Bitmask, GAMEBOY_BUTTON_BITS order (up,down,left,right,a,b,start,select). */
  buttons: number;
  /** Hz per tracked role: command_0..7, steer_left, steer_right, forward, backward, proboscis, reward_pam. */
  rates: Record<string, number>;
  /** Hz. */
  populationRate: number;
  /**
   * Number of neurons that spiked since the previous snapshot (set bits in the `spikes`
   * attachment bitset). 0 when the `spikes` attachment is omitted this snapshot.
   */
  spikeCount: number;
  learning: FeedLearning;
  game: FeedGame;
  milestone: FeedMilestone;
  sugar: FeedSugar;
  /** Events since the previous snapshot (usually empty). */
  events: FeedEvent[];
  /**
   * The chat lines the control API has accepted, oldest first, at most `CHAT_RING_MAX`. Omitted
   * entirely while `[chat] enabled = false` in `flysim.toml` — that is the kill switch.
   */
  chat?: ChatLine[];
  attachments: AttachmentKind[];
}

// ---------------------------------------------------------------------------------------------
// Client hello (docs/feed-protocol.md, "Client hello")
// ---------------------------------------------------------------------------------------------

/** Identifies the kind of feed consumer. */
export type FeedClientKind = 'stage' | 'bridge' | 'test';

/** The one JSON text message a client sends on connect. */
export interface ClientHello {
  protocol: 1;
  client: FeedClientKind;
  wants: AttachmentKind[];
}

// ---------------------------------------------------------------------------------------------
// Control API (docs/control-api.md)
// ---------------------------------------------------------------------------------------------

/** Version strings reported by `GET /status`. */
export interface FeedVersions {
  kernel: string;
  plasticity: string;
  adapter: string;
  binjgb: string;
  /** Dataset fingerprint. */
  dataset: string;
}

/** Checkpoint state reported by `GET /status`. */
export interface CheckpointStatus {
  latestWallMs: number;
  generation: number;
}

/**
 * `GET /status` response: same fields as the feed header minus `events` and `attachments`,
 * plus version strings and checkpoint state.
 */
export interface StatusResponse
  extends Omit<FeedHeader, 'events' | 'attachments'> {
  version: FeedVersions;
  checkpoint: CheckpointStatus;
}

/** `POST /stimulate` request body. */
export interface StimulateRequest {
  /** Default and maximum come from config (default 400, max 1000). */
  durationMs?: number;
  by: string;
  source: ActionSource;
}

/** `POST /stimulate` 202 response body: the pulse was accepted. */
export interface StimulateResponse {
  eventId: number;
}

/**
 * `POST /stimulate` 429 response body: the global rate limit or an active pulse blocked it.
 */
export interface StimulateRateLimitedResponse {
  retryAfterMs: number;
}

/** `POST /reward` request body. Disabled by default (`control.allowReward = false`), 403 when disabled. */
export interface RewardRequest {
  value: number;
  by: string;
  source: ActionSource;
}

/** `POST /reward` 202 response body. */
export interface RewardResponse {
  eventId: number;
}

/**
 * `POST /chat` request body.
 *
 * The bridge forwards only messages Twitch AutoMod already let through, with `by` validated and
 * `text` sanitized; flysim enforces both again, applies the deny list, and rate-limits per name
 * and globally (`docs/control-api.md`).
 */
export interface ChatRequest {
  by: string;
  text: string;
  /** True for the bridge's own template replies, which are marked as the bot's on screen. */
  bot?: boolean;
}

/** `POST /chat` 202 response body: the line was appended to the ring. */
export interface ChatResponse {
  eventId: number;
}

/** `POST /checkpoint` response body: the generation written. */
export interface CheckpointResponse {
  generation: number;
}

/** `GET /events?since=<id>&limit=<n>` response: a page of the append-only event log. */
export interface EventsResponse {
  events: FeedEvent[];
}

/** `GET /healthz` response body (200 case). 503 with no advancing loop carries no body contract. */
export interface HealthzResponse {
  status: 'ok';
}

/** Error body shared by 403/429/4xx control API responses that are not the specific shapes above. */
export interface ErrorResponse {
  error: string;
}
