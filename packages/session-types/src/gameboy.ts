/**
 * The legacy Game Boy composition's registered schemas and declarations (PROF-02a,
 * `docs/design/session-framework/legacy-gameboy-v1.md`).
 *
 * The Rust twin is `fly-session-types/src/gameboy.rs`, which renders every declaration and
 * digest into `fixtures/gameboy-legacy.json`. This side keeps the references as constants and
 * its tests recompute each digest from that file with this package's own canonical JSON, so a
 * drift in either language fails.
 *
 * Nothing here is a generic session type: these values travel inside `TypedValue`s or are
 * documents an `AssetRef` or the composition digest names.
 */
import { digestOf, fail } from './canonical';
import { readRational, readSchemaRef } from './common';
import { Reader, readArtifactRef, requireUnique, u64 } from './reader';
import type { ArtifactRef, Digest, Id, RationalNs, SchemaRef, TypedValue } from './scalar';
import { isDigest } from './scalar';
import { type AssetRef, readAssetRef } from './workers';
import { MAX_SLOTS, ROLLBACK_POLICY, SLOTS_CAPABILITY } from './extensions';

export const PROFILE_ID = 'gameboy-legacy-fafb-v783-v1';
export const DATASET_ID = 'fafb-v783';
export const FINGERPRINT_SCHEMA = 1;
/** Today's schema-1 fingerprint of `data/fafb-v783`: seven SHA-256 digests joined with ':'. */
export const FAFB_V783_FINGERPRINT = [
  '75ba5d3536a2862fdb9f4ef1a76b96fe099a7201ac737f0cf53fe4c5ad4183f3',
  '1657ba7716494c9db95a13b1527bc129226363f99b395774de1f76ff28571f0e',
  '63b1acb26272edccdcfacf3c2ff58069e0cefaa84451429c989258bafaeae1d5',
  'f567d7f07227e71c0df2ab4e6510f3a3f79e51b675095792e16d216d74b7b5b7',
  'ece0b5e76d1884dd2f3ff6e0362bc284febe205ac1ce07fdab5009942ddffb62',
  'b8c33144d4cec31c3ac4b6091ef1f4207f567c4f710f7c13fd2dc47c44c2b634',
  'dbbafc044cd50aad7b792615988ff6d9991c846cc3d8b2eafc86b7c357b5eefc',
].join(':');
export const KERNEL_VERSION = 'lif-1ms-f64-v2';
export const PLASTICITY_VERSION = 'fly-kc-mbon-rstdp-v2';
export const WARMUP_MS = 2500;
export const STIMULUS_REWARD_PULSE = 'reward-pulse';
export const EXCEPTION_MACRO_ROLES = 'macro-roles-outside-fingerprint';
export const PROFILE_FORMAT = 'fly-profile-v1';
export const VIEW_ID = 'lcd';
export const VIEW_WIDTH = 160;
export const VIEW_HEIGHT = 144;
export const GAMEBOY_BUTTONS = ['up', 'down', 'left', 'right', 'a', 'b', 'start', 'select'] as const;
export const MEMORY_IMAGE_BYTES = 65_536n;
export const EXECUTOR_ID = 'pokered-macros-v1';
export const RESTORE_SEMANTICS = 'legacy-transient-reset';
export const CHECKPOINT_FORMAT_OF_RECORD = 'FLYSIM01';
export const SCHEDULER = 'lockstep-v1';
export const SETUP_FRAMES = 1;
export const MAX_MACRO_CHANNELS = 64;
export const MAX_COMPATIBILITY_BYTES = 1024;
export const MACRO_MODES = ['raw', 'macros'] as const;
export const ROLLBACK_TRIGGERS = ['stall', 'game-over'] as const;

/** One Game Boy frame, 70224 cycles at 4194304 Hz: `8572265625/512` ns exactly. */
export const STEP_DURATION: RationalNs = { numerator: '8572265625', denominator: '512' };
/** One model tick, 1 ms. */
export const TICK_DURATION: RationalNs = { numerator: '1000000', denominator: '1' };

/** The registered payload schema references, digests over their canonical declarations. */
export const READOUT_CONTEXT_SCHEMA: SchemaRef = {
  id: 'gameboy-readout-context-v1',
  version: 1,
  digest: '78a5312f8608a1399e7a16549a1b95a43b87137618d59815b9b06c1bb184014d',
};
export const CHANNELS_SCHEMA: SchemaRef = {
  id: 'gameboy-channels-v1',
  version: 1,
  digest: '28bd89bfa41ec73fb2785959a64d9975b4a932e10cd9e51dc4bc29020c823595',
};
export const JOYPAD_SCHEMA: SchemaRef = {
  id: 'gameboy-joypad-v1',
  version: 1,
  digest: '1bde5fa114b99824ad608fba4ea85706cb0cebc6123a778dc1e5f89791d2a05e',
};
export const MEMORY_INSPECTION_SCHEMA: SchemaRef = {
  id: 'gameboy-memory-inspection-v1',
  version: 1,
  digest: 'd6cb62248bfdac2ffdf00290ffbebf9a101b1fe28f7be766d24db28ede8da3e6',
};
export const ROLLBACK_REQUEST_SCHEMA: SchemaRef = {
  id: 'legacy-ratchet-rollback-v1',
  version: 1,
  digest: '0610f899a4746e5991a07dbc3c847ada15d7c68cedf0664244a3c1d87c175c70',
};
export const PAYLOAD_SCHEMAS: readonly SchemaRef[] = [
  READOUT_CONTEXT_SCHEMA,
  CHANNELS_SCHEMA,
  JOYPAD_SCHEMA,
  MEMORY_INSPECTION_SCHEMA,
  ROLLBACK_REQUEST_SCHEMA,
];
/** The legacy profile document's AssetRef digest: SHA-256 of its canonical JSON. */
export const PROFILE_DIGEST = '41e5d1ac62ab23f1b2d7252d52faac08b269c85e6b4c9ed7a370c74032c60878';

/** `ChannelName`: a decoder channel or rate-role name. Not an Id: legacy names carry '_'. */
export function isChannelName(value: unknown): value is string {
  return typeof value === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(value);
}

function sameSchema(found: SchemaRef, expected: SchemaRef, what: string): void {
  if (found.id !== expected.id || found.version !== expected.version || found.digest !== expected.digest) {
    fail(`${what} must be the registered ${expected.id} reference`);
  }
}

function exact(found: unknown, expected: unknown, what: string): void {
  if (found !== expected) fail(`${what} must be ${JSON.stringify(expected)}`);
}

function channelList(reader: Reader, key: string, high: number): string[] {
  const items = reader.list(key, 0, high, (item) => {
    if (!isChannelName(item)) fail('every entry must be a channel name');
    return item;
  });
  requireUnique(items, key);
  return items;
}

function uniqueIds(reader: Reader, key: string, low: number, high: number): Id[] {
  const ids = reader.idList(key, low, high);
  requireUnique(ids, key);
  return ids;
}

function typedAs<T>(value: TypedValue, schema: SchemaRef, read: (v: unknown) => T): T {
  sameSchema(value.schema, schema, 'TypedValue.schema');
  return read(value.value);
}

// gameboy-readout-context-v1 ---------------------------------------------------------------

export interface GameboyLocation {
  area: number;
  x: number;
  y: number;
}

export interface GameboyReadoutContext {
  boot: boolean;
  bound: string[];
  location: GameboyLocation | null;
}

export function readGameboyReadoutContext(value: unknown): GameboyReadoutContext {
  const reader = new Reader(value, 'GameboyReadoutContext');
  const boot = reader.boolean('boot');
  const bound = channelList(reader, 'bound', MAX_MACRO_CHANNELS);
  const raw = reader.value('location');
  let location: GameboyLocation | null = null;
  if (raw !== null) {
    const l = new Reader(raw, 'GameboyReadoutContext.location');
    location = {
      area: l.int('area', 0, 4_294_967_295),
      x: l.int('x', 0, 4_294_967_295),
      y: l.int('y', 0, 4_294_967_295),
    };
    l.finish();
  }
  reader.finish();
  return { boot, bound, location };
}

export function readGameboyReadoutContextTyped(value: TypedValue): GameboyReadoutContext {
  return typedAs(value, READOUT_CONTEXT_SCHEMA, readGameboyReadoutContext);
}

/** `bound` is a subset of the composition's macro channels, in their order. */
export function validateReadoutContextAgainst(
  context: GameboyReadoutContext,
  macroChannels: readonly string[],
): void {
  let cursor = 0;
  for (const channel of context.bound) {
    const offset = macroChannels.slice(cursor).indexOf(channel);
    if (offset < 0) {
      fail(
        `GameboyReadoutContext: bound channel "${channel}" is not a macro channel of the composition, or is out of order`,
      );
    }
    cursor += offset + 1;
  }
}

// gameboy-channels-v1 ----------------------------------------------------------------------

export interface GameboyChannelsDecision {
  buttons: { id: (typeof GAMEBOY_BUTTONS)[number]; down: boolean }[];
  macro: string | null;
}

export function readGameboyChannelsDecision(value: unknown): GameboyChannelsDecision {
  const reader = new Reader(value, 'GameboyChannelsDecision');
  const buttons = reader.list('buttons', 8, 8, (item) => {
    const b = new Reader(item, 'GameboyChannelsDecision.buttons');
    const entry = { id: b.id('id'), down: b.boolean('down') };
    b.finish();
    return entry;
  });
  buttons.forEach((button, index) => {
    if (button.id !== GAMEBOY_BUTTONS[index]) {
      fail(`GameboyChannelsDecision: buttons must list ${GAMEBOY_BUTTONS.join(',')} in that order`);
    }
  });
  const macro = reader.value('macro');
  if (macro !== null && !isChannelName(macro)) {
    fail('GameboyChannelsDecision: macro must be null or a channel name');
  }
  reader.finish();
  return {
    buttons: buttons as GameboyChannelsDecision['buttons'],
    macro: macro as string | null,
  };
}

export function readGameboyChannelsDecisionTyped(value: TypedValue): GameboyChannelsDecision {
  return typedAs(value, CHANNELS_SCHEMA, readGameboyChannelsDecision);
}

/** The joypad mask, bit `i` for `GAMEBOY_BUTTONS[i]`. */
export function channelsMask(decision: GameboyChannelsDecision): number {
  return decision.buttons.reduce((mask, button, bit) => (button.down ? mask | (1 << bit) : mask), 0);
}

/** The macro group only ever activates a bound channel. */
export function validateDecisionAgainst(
  decision: GameboyChannelsDecision,
  context: GameboyReadoutContext,
): void {
  if (decision.macro !== null && !context.bound.includes(decision.macro)) {
    fail(`GameboyChannelsDecision: macro "${decision.macro}" is not bound in the decision context`);
  }
}

// gameboy-memory-inspection-v1 -------------------------------------------------------------

export interface GameboyMemoryInspection {
  memory: ArtifactRef;
  romDigest: Digest;
}

export function readGameboyMemoryInspection(value: unknown): GameboyMemoryInspection {
  const reader = new Reader(value, 'GameboyMemoryInspection');
  const inspection: GameboyMemoryInspection = {
    memory: readArtifactRef(reader.value('memory')),
    romDigest: reader.digest('romDigest'),
  };
  reader.finish();
  if (u64(inspection.memory.byteLength) !== MEMORY_IMAGE_BYTES) {
    fail(`GameboyMemoryInspection: the memory image is exactly ${MEMORY_IMAGE_BYTES} bytes`);
  }
  return inspection;
}

export function readGameboyMemoryInspectionTyped(value: TypedValue): GameboyMemoryInspection {
  return typedAs(value, MEMORY_INSPECTION_SCHEMA, readGameboyMemoryInspection);
}

/** The ROM the executor reads is the content the environment runs. */
export function validateInspectionAgainst(
  inspection: GameboyMemoryInspection,
  contentDigest: Digest,
  rom: AssetRef,
): void {
  if (inspection.romDigest !== contentDigest || rom.digest !== contentDigest) {
    fail(
      'GameboyMemoryInspection: romDigest, the environment contentDigest and the executor rom must agree',
    );
  }
}

// legacy-ratchet-rollback-v1 ---------------------------------------------------------------

export interface LegacyRatchetRollbackRequest {
  slotId: Id;
  trigger: (typeof ROLLBACK_TRIGGERS)[number];
}

export function readLegacyRatchetRollbackRequest(value: unknown): LegacyRatchetRollbackRequest {
  const reader = new Reader(value, 'LegacyRatchetRollbackRequest');
  const request: LegacyRatchetRollbackRequest = {
    slotId: reader.id('slotId'),
    trigger: reader.enumeration('trigger', ROLLBACK_TRIGGERS),
  };
  reader.finish();
  return request;
}

export function readLegacyRatchetRollbackRequestTyped(
  value: TypedValue,
): LegacyRatchetRollbackRequest {
  return typedAs(value, ROLLBACK_REQUEST_SCHEMA, readLegacyRatchetRollbackRequest);
}

// The legacy profile ------------------------------------------------------------------------

export interface LegacyGameboyProfile {
  profileId: string;
  datasetId: string;
  fingerprintSchema: number;
  datasetFingerprint: string;
  kernelVersion: string;
  plasticityVersion: string;
  tickDuration: RationalNs;
  warmupMs: number;
  view: { viewId: Id; width: number; height: number };
  supportedStimuli: Id[];
  readoutContextSchema: SchemaRef;
  decisionSchema: SchemaRef;
  legacyExceptions: Id[];
}

function sameRational(found: RationalNs, expected: RationalNs, what: string): void {
  if (found.numerator !== expected.numerator || found.denominator !== expected.denominator) {
    fail(`${what} must be ${expected.numerator}/${expected.denominator}`);
  }
}

export function readLegacyGameboyProfile(value: unknown): LegacyGameboyProfile {
  const reader = new Reader(value, 'LegacyGameboyProfile');
  const viewReader = (raw: unknown) => {
    const v = new Reader(raw, 'LegacyGameboyProfile.view');
    const view = {
      viewId: v.id('viewId'),
      width: v.int('width', VIEW_WIDTH, VIEW_WIDTH),
      height: v.int('height', VIEW_HEIGHT, VIEW_HEIGHT),
    };
    v.finish();
    return view;
  };
  const profile: LegacyGameboyProfile = {
    profileId: reader.string('profileId'),
    datasetId: reader.string('datasetId'),
    fingerprintSchema: reader.int('fingerprintSchema', FINGERPRINT_SCHEMA, FINGERPRINT_SCHEMA),
    datasetFingerprint: reader.string('datasetFingerprint'),
    kernelVersion: reader.string('kernelVersion'),
    plasticityVersion: reader.string('plasticityVersion'),
    tickDuration: readRational(reader.value('tickDuration')),
    warmupMs: reader.int('warmupMs', WARMUP_MS, WARMUP_MS),
    view: viewReader(reader.value('view')),
    supportedStimuli: uniqueIds(reader, 'supportedStimuli', 1, 1),
    readoutContextSchema: readSchemaRef(reader.value('readoutContextSchema')),
    decisionSchema: readSchemaRef(reader.value('decisionSchema')),
    legacyExceptions: uniqueIds(reader, 'legacyExceptions', 1, 1),
  };
  reader.finish();
  exact(profile.profileId, PROFILE_ID, 'profileId');
  exact(profile.datasetId, DATASET_ID, 'datasetId');
  exact(profile.kernelVersion, KERNEL_VERSION, 'kernelVersion');
  exact(profile.plasticityVersion, PLASTICITY_VERSION, 'plasticityVersion');
  sameRational(profile.tickDuration, TICK_DURATION, 'LegacyGameboyProfile: tickDuration');
  exact(profile.view.viewId, VIEW_ID, 'view.viewId');
  exact(profile.supportedStimuli[0], STIMULUS_REWARD_PULSE, 'supportedStimuli[0]');
  exact(profile.legacyExceptions[0], EXCEPTION_MACRO_ROLES, 'legacyExceptions[0]');
  if (profile.datasetFingerprint !== FAFB_V783_FINGERPRINT) {
    fail(
      "LegacyGameboyProfile: datasetFingerprint must be today's fafb-v783 schema-1 fingerprint; another fingerprint is another profile",
    );
  }
  sameSchema(profile.readoutContextSchema, READOUT_CONTEXT_SCHEMA, 'readoutContextSchema');
  sameSchema(profile.decisionSchema, CHANNELS_SCHEMA, 'decisionSchema');
  return profile;
}

// The legacy composition --------------------------------------------------------------------

export interface LegacyGameboyComposition {
  compositionId: Id;
  scheduler: string;
  profile: AssetRef;
  executor: {
    id: string;
    rom: AssetRef;
    adapter: Id;
    symbolProvenance: string;
    mode: (typeof MACRO_MODES)[number];
    macroChannels: string[];
  };
  decoderConfigDigest: Digest;
  environment: {
    extensions: Id[];
    slots: Id[];
    stepDuration: RationalNs;
    inspectionSchema: SchemaRef;
    controllerSchema: SchemaRef;
    setupFrames: number;
    audio: { sampleRate: number; channels: number };
  };
  episodePolicy: string;
  restore: string;
  checkpointFormatOfRecord: string;
  flysimCompatibility: string;
}

export function readLegacyGameboyComposition(value: unknown): LegacyGameboyComposition {
  const reader = new Reader(value, 'LegacyGameboyComposition');
  const compositionId = reader.id('compositionId');
  const scheduler = reader.string('scheduler');
  const profile = readAssetRef(reader.value('profile'));
  const e = new Reader(reader.value('executor'), 'LegacyGameboyComposition.executor');
  const executor = {
    id: e.string('id'),
    rom: readAssetRef(e.value('rom')),
    adapter: e.id('adapter'),
    symbolProvenance: e.boundedString('symbolProvenance', 64),
    mode: e.enumeration('mode', MACRO_MODES),
    macroChannels: channelList(e, 'macroChannels', MAX_MACRO_CHANNELS),
  };
  e.finish();
  const decoderConfigDigest = reader.digest('decoderConfigDigest');
  const n = new Reader(reader.value('environment'), 'LegacyGameboyComposition.environment');
  const extensions = uniqueIds(n, 'extensions', 1, 1);
  const slots = uniqueIds(n, 'slots', 1, MAX_SLOTS);
  const stepDuration = readRational(n.value('stepDuration'));
  const inspectionSchema = readSchemaRef(n.value('inspectionSchema'));
  const controllerSchema = readSchemaRef(n.value('controllerSchema'));
  const setupFrames = n.int('setupFrames', SETUP_FRAMES, SETUP_FRAMES);
  const a = new Reader(n.value('audio'), 'environment.audio');
  const audio = { sampleRate: a.int('sampleRate', 8000, 192_000), channels: a.int('channels', 2, 2) };
  a.finish();
  n.finish();
  const composition: LegacyGameboyComposition = {
    compositionId,
    scheduler,
    profile,
    executor,
    decoderConfigDigest,
    environment: {
      extensions,
      slots,
      stepDuration,
      inspectionSchema,
      controllerSchema,
      setupFrames,
      audio,
    },
    episodePolicy: reader.string('episodePolicy'),
    restore: reader.string('restore'),
    checkpointFormatOfRecord: reader.string('checkpointFormatOfRecord'),
    flysimCompatibility: reader.string('flysimCompatibility'),
  };
  reader.finish();
  exact(scheduler, SCHEDULER, 'scheduler');
  exact(executor.id, EXECUTOR_ID, 'executor.id');
  exact(extensions[0], SLOTS_CAPABILITY, 'environment.extensions[0]');
  sameRational(stepDuration, STEP_DURATION, 'LegacyGameboyComposition: environment.stepDuration');
  sameSchema(inspectionSchema, MEMORY_INSPECTION_SCHEMA, 'environment.inspectionSchema');
  sameSchema(controllerSchema, JOYPAD_SCHEMA, 'environment.controllerSchema');
  exact(composition.episodePolicy, ROLLBACK_POLICY, 'episodePolicy');
  exact(composition.restore, RESTORE_SEMANTICS, 'restore');
  exact(composition.checkpointFormatOfRecord, CHECKPOINT_FORMAT_OF_RECORD, 'checkpointFormatOfRecord');
  if (profile.format !== PROFILE_FORMAT || profile.digest !== PROFILE_DIGEST) {
    fail(
      'LegacyGameboyComposition: profile must name the legacy profile document (format fly-profile-v1, its digest)',
    );
  }
  if (executor.mode === 'raw' && executor.macroChannels.length !== 0) {
    fail('LegacyGameboyComposition: raw mode deals no macro channels');
  }
  if (executor.mode === 'macros' && executor.macroChannels.length === 0) {
    fail('LegacyGameboyComposition: macros mode needs its macro channels');
  }
  const text = composition.flysimCompatibility;
  if (text.length === 0 || Buffer.byteLength(text, 'utf8') > MAX_COMPATIBILITY_BYTES) {
    fail('LegacyGameboyComposition: flysimCompatibility must be 1..=1024 bytes');
  }
  const segments = text.split('/');
  const agrees =
    segments.length >= 6 &&
    segments[0] === KERNEL_VERSION &&
    segments[1] === executor.adapter &&
    segments[2] === FAFB_V783_FINGERPRINT &&
    segments[3] === PLASTICITY_VERSION &&
    segments[5] === `pokered:${executor.symbolProvenance}`;
  if (!agrees) {
    fail(
      "LegacyGameboyComposition: flysimCompatibility's kernel, adapter, fingerprint, plasticity and pokered segments must agree with the declaration",
    );
  }
  return composition;
}

/** SHA-256 of the canonical declaration: the `declaration=` line of the composition digest. */
export function compositionDeclarationDigest(composition: LegacyGameboyComposition): Digest {
  const digest = digestOf(composition);
  if (!isDigest(digest)) fail('digest');
  return digest;
}
