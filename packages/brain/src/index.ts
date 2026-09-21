/**
 * `@flybrain/brain` public API.
 *
 * Loaders are subpath exports (`@flybrain/brain/node`, `@flybrain/brain/browser`) so a bundle
 * never pulls in `node:zlib` and a server never depends on `DecompressionStream`.
 */

// Dataset layer: connectome artifact format, validation and fingerprinting.
export type { BrainDataset, BrainMetadata, CircuitRoles, Sha256Hex } from './dataset/format';
export { fingerprintDataset, validateDataset } from './dataset/format';

// Model layer: the LIF kernel, its reward-modulated plasticity, the retina projection and the
// deterministic RNG they share.
export { Xorshift32 } from './model/rng';
export { DEFAULT_RETINA_CONFIG, projectFrame, type RetinaColumns, type RetinaConfig } from './model/retina';
export {
  DEFAULT_PLASTICITY_CONFIG,
  PLASTICITY_VERSION,
  Plasticity,
  RewardModulatedStdp,
  plasticityVersion,
  type LearningStats,
  type PlasticityConfig,
  type PlasticityState,
} from './model/plasticity';
export {
  DEFAULT_LIF_CONFIG,
  FlyBrain,
  LifNetwork,
  MAX_RATE_ROLES,
  NEURAL_KERNEL_VERSION,
  kernelVersion,
  type LifConfig,
  type LifState,
} from './model/lif';

// Readout layer: population-rate decoding of network activity into output channels.
export { PopulationDecoder } from './readout/decoder';
export type { DecoderConfig, DecoderState, ExclusiveGroup, LegacyDecoderState, PulseChannel } from './readout/decoder';
export { GAMEBOY_BUTTONS, GAMEBOY_BUTTON_BITS, fromButtonMask, gameboyDecoderConfig, toButtonMask } from './readout/presets/gameboy';
export type { GameboyButton } from './readout/presets/gameboy';
export { platformerDecoderConfig } from './readout/presets/platformer';

// Agent layer: the environment-agnostic loop that composes the three layers above, plus the
// checkpoint envelope a host writes it to.
export {
  DEFAULT_STIMULATION_MS,
  DEFAULT_WARMUP_MS,
  GAMEBOY_MS_PER_FRAME,
  NeuralAgent,
  type AgentConfig,
  type AgentSnapshot,
  type AgentState,
  type RewardEvent,
  type TickOptions,
  type TickResult,
} from './agent/agent';
export {
  AGENT_CHUNK_NAMES,
  ENVELOPE_SCHEMA_VERSION,
  agentFromChunks,
  agentToChunks,
  decodeEnvelope,
  encodeEnvelope,
  type AgentManifest,
  type EnvelopeManifest,
  type EnvelopeParts,
} from './agent/envelope';
