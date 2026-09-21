/**
 * `@flybrain/feed` public API.
 *
 * Shared types and the binary snapshot codec for the feed protocol (`docs/feed-protocol.md`)
 * and control API (`docs/control-api.md`). The fake flysim dev server lives at the `./fake`
 * subpath (`src/fake/server.ts`) so a browser bundle never pulls in `node:http` or `ws`.
 */

// Protocol constants.
export {
  AUDIO_RATE,
  CONTROL_PORT,
  FEED_PORT,
  FEED_PROTOCOL,
  FRAME_HEIGHT,
  FRAME_WIDTH,
  MACRO_CHANNELS,
  MACRO_SLOTS,
  MACRO_TYPES,
  macroChannel,
  macroRateRole,
  macroTypeIndex,
} from './types';

// Feed protocol types.
export type {
  ActionSource,
  AttachmentKind,
  ChatLine,
  ClientHello,
  FeedClientKind,
  FeedEvent,
  FeedEventKind,
  FeedGame,
  FeedHeader,
  FeedLearning,
  FeedMilestone,
  FeedStatus,
  FeedSugar,
  GameMode,
  GameScene,
  MacroMode,
  MacroOutcome,
  MacroOutcomeKind,
  PaletteEntry,
  RewardKind,
  RunningMacro,
} from './types';

// The scene's macros, read out of a header (`docs/design/macros.md` sections 6 and 12).
export { paletteView } from './palette';
export type { PaletteCell, PaletteView } from './palette';

// Control API types.
export type {
  ChatRequest,
  ChatResponse,
  CheckpointResponse,
  CheckpointStatus,
  ErrorResponse,
  EventsResponse,
  FeedVersions,
  HealthzResponse,
  RewardRequest,
  RewardResponse,
  StatusResponse,
  StimulateRateLimitedResponse,
  StimulateRequest,
  StimulateResponse,
} from './types';

// Binary snapshot codec.
export { FeedCodecError, decodeSnapshot, encodeSnapshot } from './codec';

// Viewer display name validation (the single chokepoint; see `docs/design/stage-bridge.md` section C).
export { FALLBACK_DISPLAY_NAME, validateDisplayName } from './names';

// Chat text sanitization (the same rules `services/flysim/crates/flysim/src/chat.rs` enforces).
export {
  ALLOWED_PUNCTUATION,
  CHAT_MAX_TEXT_LENGTH,
  CHAT_RING_MAX,
  classifyChatText,
  looksLikeUrl,
  sanitizeChatText,
} from './chat';
export type { ChatRejectReason, ChatTextResult } from './chat';
// `.flyfeed` fixture container: recorded runs, replayable with no service.
export {
  FLYFEED_MAGIC,
  FLYFEED_VERSION,
  FlyfeedError,
  decodeFlyfeed,
  encodeFlyfeed,
  encodeFlyfeedHeader,
  encodeFlyfeedRecord,
  isGzip,
  iterateFlyfeedRecords,
  readFlyfeedManifest,
} from './fixture';
export type { FlyfeedAttachmentPolicy, FlyfeedManifest } from './fixture';
