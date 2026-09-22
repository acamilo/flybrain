/** Native observation media (state-media-v1 section 2) and the `State.*` payloads (section 5). */
import { fail } from './canonical';
import { readRational, readScope } from './common';
import { Reader, readArtifactRef, requireUnique, u64 } from './reader';
import { type ArtifactRef, type Digest, type Id, type Scope, type U64, isDigest } from './scalar';

/** Max views per sensory input (workers-v1 section 1), and per descriptor list. */
export const MAX_VIEWS = 8;
export const MAX_VIEW_DIMENSION = 4096;
export const MAX_PIXEL_ASPECT = 65_535;
export const MAX_OBSERVATION_DELAY_STEPS = 8;
export const MAX_SAMPLE_FRAMES = 192_000;
/** Not a stated bound; this crate's choice, published in the schema set. */
export const MAX_AUDIO_STREAMS = 8;

export interface ViewDescriptor {
  viewId: Id;
  width: number;
  height: number;
  format: 'rgba8';
  rowStride: number;
  pixelAspect: { numerator: number; denominator: number };
  observationDelaySteps: number;
}

export interface ViewRef {
  viewId: Id;
  producedStep: U64;
  pixels: ArtifactRef;
}

export interface AudioDescriptor {
  streamId: Id;
  sampleRate: number;
  channels: number;
  format: 'f32le-interleaved';
}

export interface AudioRef {
  streamId: Id;
  firstSample: U64;
  sampleFrames: number;
  samples: ArtifactRef;
  discontinuity: boolean;
}

export function readViewDescriptor(value: unknown): ViewDescriptor {
  const reader = new Reader(value, 'ViewDescriptor');
  const viewId = reader.id('viewId');
  const width = reader.int('width', 1, MAX_VIEW_DIMENSION);
  const height = reader.int('height', 1, MAX_VIEW_DIMENSION);
  const format = reader.constant('format', 'rgba8');
  const rowStride = reader.int('rowStride', 1, MAX_VIEW_DIMENSION * 4);
  const aspectReader = new Reader(reader.value('pixelAspect'), 'ViewDescriptor.pixelAspect');
  const pixelAspect = {
    numerator: aspectReader.int('numerator', 1, MAX_PIXEL_ASPECT),
    denominator: aspectReader.int('denominator', 1, MAX_PIXEL_ASPECT),
  };
  aspectReader.finish();
  const observationDelaySteps = reader.int(
    'observationDelaySteps',
    0,
    MAX_OBSERVATION_DELAY_STEPS,
  );
  reader.finish();
  if (rowStride !== width * 4) {
    fail('ViewDescriptor: rowStride must be exactly 4 x width (no padded rows in v1)');
  }
  return { viewId, width, height, format, rowStride, pixelAspect, observationDelaySteps };
}

/** The exact byte length of one frame of this view. */
export function frameBytes(descriptor: ViewDescriptor): number {
  return descriptor.rowStride * descriptor.height;
}

/** `max(0, boundary - observationDelaySteps)` (state-media-v1 section 2). */
export function requiredProducedStep(descriptor: ViewDescriptor, boundary: bigint): bigint {
  const delay = BigInt(descriptor.observationDelaySteps);
  return boundary > delay ? boundary - delay : 0n;
}

export function readViewRef(value: unknown): ViewRef {
  const reader = new Reader(value, 'ViewRef');
  const view: ViewRef = {
    viewId: reader.id('viewId'),
    producedStep: reader.u64('producedStep'),
    pixels: readArtifactRef(reader.value('pixels')),
  };
  reader.finish();
  if (u64(view.pixels.byteLength) === 0n) {
    fail('ViewRef: pixels must have a positive byte length');
  }
  return view;
}

export function readViewList(reader: Reader, key: string): ViewRef[] {
  const views = reader.list(key, 0, MAX_VIEWS, readViewRef);
  requireUnique(
    views.map((view) => view.viewId),
    key,
  );
  return views;
}

export function readAudioDescriptor(value: unknown): AudioDescriptor {
  const reader = new Reader(value, 'AudioDescriptor');
  const descriptor: AudioDescriptor = {
    streamId: reader.id('streamId'),
    sampleRate: reader.int('sampleRate', 8_000, 192_000),
    channels: reader.int('channels', 1, 8),
    format: reader.constant('format', 'f32le-interleaved'),
  };
  reader.finish();
  return descriptor;
}

export function readAudioRef(value: unknown): AudioRef {
  const reader = new Reader(value, 'AudioRef');
  const chunk: AudioRef = {
    streamId: reader.id('streamId'),
    firstSample: reader.u64('firstSample'),
    sampleFrames: reader.int('sampleFrames', 0, MAX_SAMPLE_FRAMES),
    samples: readArtifactRef(reader.value('samples')),
    discontinuity: reader.boolean('discontinuity'),
  };
  reader.finish();
  if (u64(chunk.firstSample) + BigInt(chunk.sampleFrames) > 18446744073709551615n) {
    fail('AudioRef: firstSample + sampleFrames overflows U64');
  }
  return chunk;
}

export function readAudioList(reader: Reader, key: string): AudioRef[] {
  const audio = reader.list(key, 0, MAX_AUDIO_STREAMS, readAudioRef);
  requireUnique(
    audio.map((chunk) => chunk.streamId),
    key,
  );
  return audio;
}

/** Byte shape and producing boundary against the descriptor that declared this view. */
export function validateViewAgainst(
  view: ViewRef,
  descriptor: ViewDescriptor,
  boundary: bigint | null,
): void {
  if (view.viewId !== descriptor.viewId) {
    fail(`ViewRef: viewId "${view.viewId}" does not match descriptor "${descriptor.viewId}"`);
  }
  if (u64(view.pixels.byteLength) !== BigInt(frameBytes(descriptor))) {
    fail(
      `ViewRef ${view.viewId}: artifact is ${view.pixels.byteLength} bytes, rowStride x height is ${frameBytes(descriptor)}`,
    );
  }
  if (boundary !== null) {
    const expected = requiredProducedStep(descriptor, boundary);
    if (u64(view.producedStep) !== expected) {
      fail(
        `ViewRef ${view.viewId}: producedStep ${view.producedStep} must be max(0, ${boundary} - ${descriptor.observationDelaySteps}) = ${expected}`,
      );
    }
  }
}

export function validateAudioAgainst(chunk: AudioRef, descriptor: AudioDescriptor): void {
  if (chunk.streamId !== descriptor.streamId) {
    fail(`AudioRef: streamId "${chunk.streamId}" does not match the descriptor`);
  }
  const expected = BigInt(chunk.sampleFrames) * BigInt(descriptor.channels) * 4n;
  if (u64(chunk.samples.byteLength) !== expected) {
    fail(
      `AudioRef ${chunk.streamId}: artifact is ${chunk.samples.byteLength} bytes, sampleFrames x channels x 4 is ${expected}`,
    );
  }
}

// ---------------------------------------------------------------------------------- State.*

export interface CaptureParams {
  checkpointId: Id;
}

export interface CaptureResult {
  checkpointId: Id;
  boundary: U64;
  compatibilityDigest: Digest;
  payload: ArtifactRef;
}

export interface StageRestoreParams {
  checkpointId: Id;
  sourceScope: Scope;
  compatibilityDigest: Digest;
  payload: ArtifactRef;
}

export interface StageRestoreResult {
  checkpointId: Id;
  restoreToken: Id;
}

export interface ActivateRestoreParams {
  restoreToken: Id;
}

function checkpointPayload(reader: Reader, key: string): ArtifactRef {
  const reference = readArtifactRef(reader.value(key));
  if (!isDigest(reference.digest)) {
    fail(`${key}: a checkpoint payload must carry a content digest`);
  }
  return reference;
}

export function readCaptureParams(value: unknown): CaptureParams {
  const reader = new Reader(value, 'CaptureParams');
  const params = { checkpointId: reader.id('checkpointId') };
  reader.finish();
  return params;
}

export function readCaptureResult(value: unknown): CaptureResult {
  const reader = new Reader(value, 'CaptureResult');
  const result: CaptureResult = {
    checkpointId: reader.id('checkpointId'),
    boundary: reader.u64('boundary'),
    compatibilityDigest: reader.digest('compatibilityDigest'),
    payload: checkpointPayload(reader, 'payload'),
  };
  reader.finish();
  return result;
}

export function readStageRestoreParams(value: unknown): StageRestoreParams {
  const reader = new Reader(value, 'StageRestoreParams');
  const params: StageRestoreParams = {
    checkpointId: reader.id('checkpointId'),
    sourceScope: readScope(reader.value('sourceScope')),
    compatibilityDigest: reader.digest('compatibilityDigest'),
    payload: checkpointPayload(reader, 'payload'),
  };
  reader.finish();
  return params;
}

export function readStageRestoreResult(value: unknown): StageRestoreResult {
  const reader = new Reader(value, 'StageRestoreResult');
  const result: StageRestoreResult = {
    checkpointId: reader.id('checkpointId'),
    restoreToken: reader.id('restoreToken'),
  };
  reader.finish();
  return result;
}

export function readActivateRestoreParams(value: unknown): ActivateRestoreParams {
  const reader = new Reader(value, 'ActivateRestoreParams');
  const params = { restoreToken: reader.id('restoreToken') };
  reader.finish();
  return params;
}

export { readRational };
