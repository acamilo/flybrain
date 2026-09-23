/** The closed enums and method payloads of workers-v1, plus the `Worker.*` common methods. */
import { fail } from './canonical';
import { readRational, readScope } from './common';
import {
  type AudioDescriptor,
  type AudioRef,
  MAX_VIEWS,
  type ViewDescriptor,
  type ViewRef,
  readAudioDescriptor,
  readAudioList,
  readViewDescriptor,
  readViewList,
  validateAudioAgainst,
  validateViewAgainst,
} from './media';
import { Reader, requireSameOrder, requireUnique, u64 } from './reader';
import {
  type Digest,
  type DomainRequestId,
  type Id,
  type RationalNs,
  type SchemaRef,
  type Scope,
  type TypedValue,
  type U64,
  addRational,
  compareRational,
  domainRequestId,
  isRationalZero,
  requirePositiveRational,
} from './scalar';
import { readSchemaRef, readTypedValue, readNullableTypedValue } from './common';

export const MAX_AGENTS = 4;
export const MAX_PORTS = 4;
export const MAX_RATE_ROLES = 64;
/** Declared stimulus kinds per agent. Not a stated bound; recorded in the schema set. */
export const MAX_SUPPORTED_STIMULI = 64;
export const MAX_STIMULI = 64;
export const MAX_REWARDS = 64;
export const MAX_BUTTONS = 32;
export const MAX_AXES = 16;
export const MAX_ACKNOWLEDGE = 16;
export const MAX_ENGINE_FRAME_LEN = 64;
/** Not stated by a document; this crate's choice, published in the schema set. */
export const MAX_CAPABILITIES = 32;
/** The largest `workerThreads` a launcher may allocate to one worker (workers-v1 2). */
export const MAX_WORKER_THREADS = 4096;
export const MAX_SUPPORTED_MAJORS = 8;
export const MAX_MESSAGE_CODE_POINTS = 512;

export const ROLES = ['agent', 'environment', 'coordinator'] as const;
export type Role = (typeof ROLES)[number];

export const WORKER_STATES = [
  'uninitialized',
  'ready',
  'preparing',
  'prepared',
  'advancing',
  'committing',
  'capturing',
  'staged-restore',
  'restoring',
  'failed',
  'stopping',
] as const;
export type WorkerState = (typeof WORKER_STATES)[number];

export const RECOVERY = ['exact-checkpoint', 'episode-restart'] as const;
export type Recovery = (typeof RECOVERY)[number];

export const DETERMINISM = ['fixed-build', 'unverified'] as const;
export type Determinism = (typeof DETERMINISM)[number];

export const AXIS_RANGES = ['bipolar', 'unit'] as const;
export type AxisRange = (typeof AXIS_RANGES)[number];

export function axisBounds(range: AxisRange): [number, number] {
  return range === 'bipolar' ? [-1, 1] : [0, 1];
}

export interface AssetRef {
  id: Id;
  digest: Digest;
  byteLength: U64;
  format: Id;
}

export interface SensoryInput {
  boundary: U64;
  views: ViewRef[];
  structured: TypedValue | null;
}

export interface Stimulus {
  id: Id;
  kindId: Id;
  durationMs: number;
}

export interface Reward {
  eventId: Id;
  ruleId: Id;
  value: number;
}

export interface AgentTelemetry {
  brainTicks: U64;
  populationRateHz: number;
  rates: { roleId: Id; hz: number }[];
  learning: { enabled: boolean; updates: U64; changed: U64; signal: number };
  /**
   * Milliseconds of stimulation pulse still running after the operation, or null for an agent
   * that reports none. Amendment 2026-09-23 (RT-01a): sugar admission reads it from the last
   * commit (workers-v1 section 5).
   */
  stimulusRemainingMs: number | null;
}

export function readAssetRef(value: unknown): AssetRef {
  const reader = new Reader(value, 'AssetRef');
  const asset: AssetRef = {
    id: reader.id('id'),
    digest: reader.digest('digest'),
    byteLength: reader.u64('byteLength'),
    format: reader.id('format'),
  };
  reader.finish();
  if (u64(asset.byteLength) === 0n) fail('AssetRef: byteLength must be positive');
  return asset;
}

export function readSensoryInput(value: unknown): SensoryInput {
  const reader = new Reader(value, 'SensoryInput');
  const input: SensoryInput = {
    boundary: reader.u64('boundary'),
    views: readViewList(reader, 'views'),
    structured: readNullableTypedValue(reader.value('structured')),
  };
  reader.finish();
  for (const view of input.views) {
    if (u64(view.producedStep) > u64(input.boundary)) {
      fail(`SensoryInput: view "${view.viewId}" was produced after the observed boundary`);
    }
  }
  return input;
}

/** Required sensory views against the descriptors that declared them. */
export function validateSensoryInputAgainst(
  input: SensoryInput,
  descriptors: readonly ViewDescriptor[],
): void {
  for (const view of input.views) {
    const descriptor = descriptors.find((candidate) => candidate.viewId === view.viewId);
    if (!descriptor) {
      fail(`SensoryInput: view "${view.viewId}" is not declared by the environment`);
    }
    validateViewAgainst(view, descriptor, u64(input.boundary));
  }
}

/** A pixel-only profile rejects non-null structured input (workers-v1 section 1). */
export function validateSensoryInputForProfile(
  input: SensoryInput,
  structuredSensing: boolean,
): void {
  if (input.structured !== null && !structuredSensing) {
    fail('SensoryInput: a pixel-only profile rejects non-null structured input');
  }
}

export function readStimulus(value: unknown): Stimulus {
  const reader = new Reader(value, 'Stimulus');
  const stimulus: Stimulus = {
    id: reader.id('id'),
    kindId: reader.id('kindId'),
    durationMs: reader.finite('durationMs'),
  };
  reader.finish();
  if (stimulus.durationMs <= 0) fail('Stimulus: durationMs must be finite and > 0');
  return stimulus;
}

export function readReward(value: unknown): Reward {
  const reader = new Reader(value, 'Reward');
  const reward: Reward = {
    eventId: reader.id('eventId'),
    ruleId: reader.id('ruleId'),
    value: reader.finite('value'),
  };
  reader.finish();
  return reward;
}

export function readStimulusList(reader: Reader, key: string): Stimulus[] {
  const items = reader.list(key, 0, MAX_STIMULI, readStimulus);
  requireUnique(
    items.map((item) => item.id),
    key,
  );
  return items;
}

export function readRewardList(reader: Reader, key: string): Reward[] {
  const items = reader.list(key, 0, MAX_REWARDS, readReward);
  requireUnique(
    items.map((item) => item.eventId),
    key,
  );
  return items;
}

export function readAgentTelemetry(value: unknown): AgentTelemetry {
  const reader = new Reader(value, 'AgentTelemetry');
  const brainTicks = reader.u64('brainTicks');
  const populationRateHz = reader.finiteIn('populationRateHz', 0, Number.MAX_VALUE);
  const stimulusRemainingMs =
    reader.value('stimulusRemainingMs') === null
      ? null
      : reader.finiteIn('stimulusRemainingMs', 0, Number.MAX_VALUE);
  const rates = reader.list(('rates'), 0, MAX_RATE_ROLES, (item) => {
    const rate = new Reader(item, 'AgentTelemetry.rates');
    const entry = { roleId: rate.id('roleId'), hz: rate.finiteIn('hz', 0, Number.MAX_VALUE) };
    rate.finish();
    return entry;
  });
  const learningReader = new Reader(reader.value('learning'), 'AgentTelemetry.learning');
  const learning = {
    enabled: learningReader.boolean('enabled'),
    updates: learningReader.u64('updates'),
    changed: learningReader.u64('changed'),
    signal: learningReader.finite('signal'),
  };
  learningReader.finish();
  reader.finish();
  requireUnique(
    rates.map((rate) => rate.roleId),
    'AgentTelemetry.rates',
  );
  if (u64(learning.changed) > u64(learning.updates)) {
    fail('AgentTelemetry: learning.changed cannot exceed learning.updates');
  }
  return { brainTicks, populationRateHz, rates, learning, stimulusRemainingMs };
}

/** Rates are in profile-defined order (workers-v1 section 1). */
export function validateTelemetryRoles(
  telemetry: AgentTelemetry,
  roleOrder: readonly string[],
): void {
  requireSameOrder(
    telemetry.rates.map((rate) => rate.roleId),
    roleOrder,
    'AgentTelemetry.rates',
  );
}

// ------------------------------------------------------------------------------ agent methods

export interface AgentInitializeParams {
  agentId: Id;
  profile: AssetRef;
  seed: number;
  initialInput: SensoryInput;
  initialDecisionContext: TypedValue;
  workerThreads: number;
}

export interface AgentGraph {
  datasetDigest: Digest;
  indexDigest: Digest;
  neuronCount: U64;
  rateRoles: Id[];
  supportedStimuli: Id[];
}

export interface AgentInitializeResult {
  agentId: Id;
  profileDigest: Digest;
  tickDuration: RationalNs;
  warmupTicks: U64;
  committedStep: U64;
  decisionContextDigest: Digest;
  telemetry: AgentTelemetry;
  graph: AgentGraph;
}

export interface PrepareParams {
  agentId: Id;
  profileDigest: Digest;
  interval: RationalNs;
  decisionContextDigest: Digest;
  preStepStimulations: Stimulus[];
}

export interface PreparedDecision {
  agentId: Id;
  ticksAdvanced: U64;
  brainTicks: U64;
  remainder: RationalNs;
  decision: TypedValue;
}

export interface CommitParams {
  agentId: Id;
  preparedRequestId: DomainRequestId;
  nextInput: SensoryInput;
  nextDecisionContext: TypedValue;
  rewards: Reward[];
  taskStimulations: Stimulus[];
}

export interface AgentCommitResult {
  agentId: Id;
  committedStep: U64;
  decisionContextDigest: Digest;
  telemetry: AgentTelemetry;
}

export function readAgentInitializeParams(value: unknown): AgentInitializeParams {
  const reader = new Reader(value, 'AgentInitializeParams');
  const params: AgentInitializeParams = {
    agentId: reader.id('agentId'),
    profile: readAssetRef(reader.value('profile')),
    seed: reader.int('seed', -2_147_483_648, 2_147_483_647),
    initialInput: readSensoryInput(reader.value('initialInput')),
    initialDecisionContext: readTypedValue(reader.value('initialDecisionContext')),
    workerThreads: reader.int('workerThreads', 1, MAX_WORKER_THREADS),
  };
  reader.finish();
  return params;
}

export function readAgentGraph(value: unknown): AgentGraph {
  const reader = new Reader(value, 'AgentGraph');
  const graph: AgentGraph = {
    datasetDigest: reader.digest('datasetDigest'),
    indexDigest: reader.digest('indexDigest'),
    neuronCount: reader.u64('neuronCount'),
    rateRoles: reader.idList('rateRoles', 0, MAX_RATE_ROLES),
    supportedStimuli: reader.idList('supportedStimuli', 0, MAX_SUPPORTED_STIMULI),
  };
  reader.finish();
  requireUnique(graph.rateRoles, 'AgentGraph.rateRoles');
  requireUnique(graph.supportedStimuli, 'AgentGraph.supportedStimuli');
  return graph;
}

export function readAgentInitializeResult(value: unknown): AgentInitializeResult {
  const reader = new Reader(value, 'AgentInitializeResult');
  const result: AgentInitializeResult = {
    agentId: reader.id('agentId'),
    profileDigest: reader.digest('profileDigest'),
    tickDuration: readRational(reader.value('tickDuration')),
    warmupTicks: reader.u64('warmupTicks'),
    committedStep: reader.u64('committedStep'),
    decisionContextDigest: reader.digest('decisionContextDigest'),
    telemetry: readAgentTelemetry(reader.value('telemetry')),
    graph: readAgentGraph(reader.value('graph')),
  };
  reader.finish();
  requirePositiveRational(result.tickDuration, 'AgentInitializeResult.tickDuration');
  if (u64(result.committedStep) !== 0n) {
    fail('AgentInitializeResult: committedStep must be "0"');
  }
  // The rates a worker reports and the role order it declares are one statement.
  validateTelemetryRoles(result.telemetry, result.graph.rateRoles);
  return result;
}

export function readPrepareParams(value: unknown): PrepareParams {
  const reader = new Reader(value, 'PrepareParams');
  const params: PrepareParams = {
    agentId: reader.id('agentId'),
    profileDigest: reader.digest('profileDigest'),
    interval: readRational(reader.value('interval')),
    decisionContextDigest: reader.digest('decisionContextDigest'),
    preStepStimulations: readStimulusList(reader, 'preStepStimulations'),
  };
  reader.finish();
  requirePositiveRational(params.interval, 'PrepareParams.interval');
  return params;
}

export function readPreparedDecision(value: unknown): PreparedDecision {
  const reader = new Reader(value, 'PreparedDecision');
  const decision: PreparedDecision = {
    agentId: reader.id('agentId'),
    ticksAdvanced: reader.u64('ticksAdvanced'),
    brainTicks: reader.u64('brainTicks'),
    remainder: readRational(reader.value('remainder')),
    decision: readTypedValue(reader.value('decision')),
  };
  reader.finish();
  if (u64(decision.ticksAdvanced) > u64(decision.brainTicks)) {
    fail('PreparedDecision: ticksAdvanced cannot exceed the total brainTicks');
  }
  return decision;
}

/** The remainder is always `>= 0` and `< one model tick` (step-v1 section 5). */
export function validateRemainder(decision: PreparedDecision, tickDuration: RationalNs): void {
  requirePositiveRational(tickDuration, 'tickDuration');
  if (compareRational(decision.remainder, tickDuration) >= 0) {
    fail('PreparedDecision: remainder must be less than one model tick');
  }
}

export function readCommitParams(value: unknown): CommitParams {
  const reader = new Reader(value, 'CommitParams');
  const params: CommitParams = {
    agentId: reader.id('agentId'),
    preparedRequestId: domainRequestId(reader.string('preparedRequestId')),
    nextInput: readSensoryInput(reader.value('nextInput')),
    nextDecisionContext: readTypedValue(reader.value('nextDecisionContext')),
    rewards: readRewardList(reader, 'rewards'),
    taskStimulations: readStimulusList(reader, 'taskStimulations'),
  };
  reader.finish();
  return params;
}

/** The commit of transition `k -> k+1` carries `scope.step = k` and the input for `k+1`. */
export function validateCommitAgainstScope(params: CommitParams, scope: Scope): void {
  const expected = u64(scope.step) + 1n;
  if (u64(params.nextInput.boundary) !== expected) {
    fail(`CommitParams: nextInput.boundary must be ${expected} for scope.step ${scope.step}`);
  }
}

export function readAgentCommitResult(value: unknown): AgentCommitResult {
  const reader = new Reader(value, 'AgentCommitResult');
  const result: AgentCommitResult = {
    agentId: reader.id('agentId'),
    committedStep: reader.u64('committedStep'),
    decisionContextDigest: reader.digest('decisionContextDigest'),
    telemetry: readAgentTelemetry(reader.value('telemetry')),
  };
  reader.finish();
  return result;
}

// ------------------------------------------------------------------- environment methods

export interface AxisSchema {
  id: Id;
  range: AxisRange;
  neutral: number;
}

export interface ControllerSchema {
  schema: SchemaRef;
  buttons: Id[];
  axes: AxisSchema[];
}

export interface PortControl {
  portId: Id;
  buttons: { id: Id; down: boolean }[];
  axes: { id: Id; value: number }[];
}

export interface PortDescriptor {
  portId: Id;
  controls: ControllerSchema;
}

export interface EnvironmentDescriptor {
  backendDigest: Digest;
  contentDigest: Digest;
  configurationDigest: Digest;
  stepDuration: RationalNs;
  ports: PortDescriptor[];
  inspectionSchema: SchemaRef;
  views: ViewDescriptor[];
  audio: AudioDescriptor[];
  recovery: Recovery;
  determinism: Determinism;
}

export interface WorldObservation {
  boundary: U64;
  worldTime: RationalNs;
  engineFrame: string | null;
  sensoryViews: ViewRef[];
  inspection: TypedValue;
  broadcastViews: ViewRef[];
  audio: AudioRef[];
}

export interface EnvironmentInitializeParams {
  backendConfig: AssetRef;
  taskConfig: AssetRef;
  episodeId: Id;
  portBindings: { portId: Id; agentId: Id }[];
}

export interface EnvironmentInitializeResult {
  descriptor: EnvironmentDescriptor;
  observation: WorldObservation;
}

export interface AdvanceParams {
  batchId: Id;
  controls: PortControl[];
}

export interface StepResult {
  batchId: Id;
  appliedFromStep: U64;
  nextStep: U64;
  appliedControlsDigest: Digest;
  observation: WorldObservation;
}

export function readControllerSchema(value: unknown): ControllerSchema {
  const reader = new Reader(value, 'ControllerSchema');
  const controls: ControllerSchema = {
    schema: readSchemaRef(reader.value('schema')),
    buttons: reader.idList('buttons', 0, MAX_BUTTONS),
    axes: reader.list('axes', 0, MAX_AXES, (item) => {
      const axis = new Reader(item, 'ControllerSchema.axes');
      const id = axis.id('id');
      const range = axis.enumeration('range', AXIS_RANGES);
      const [low, high] = axisBounds(range);
      const neutral = axis.finiteIn('neutral', low, high);
      axis.finish();
      return { id, range, neutral };
    }),
  };
  reader.finish();
  requireUnique(controls.buttons, 'ControllerSchema.buttons');
  requireUnique(
    controls.axes.map((axis) => axis.id),
    'ControllerSchema.axes',
  );
  return controls;
}

export function readPortControl(value: unknown): PortControl {
  const reader = new Reader(value, 'PortControl');
  const control: PortControl = {
    portId: reader.id('portId'),
    buttons: reader.list('buttons', 0, MAX_BUTTONS, (item) => {
      const button = new Reader(item, 'PortControl.buttons');
      const entry = { id: button.id('id'), down: button.boolean('down') };
      button.finish();
      return entry;
    }),
    axes: reader.list('axes', 0, MAX_AXES, (item) => {
      const axis = new Reader(item, 'PortControl.axes');
      const entry = { id: axis.id('id'), value: axis.finite('value') };
      axis.finish();
      return entry;
    }),
  };
  reader.finish();
  requireUnique(
    control.buttons.map((button) => button.id),
    'PortControl.buttons',
  );
  requireUnique(
    control.axes.map((axis) => axis.id),
    'PortControl.axes',
  );
  return control;
}

/**
 * Every declared button and axis, in descriptor order, in range. An out-of-range value is
 * refused, never clamped (workers-v1 section 3).
 */
export function validatePortControlAgainst(
  control: PortControl,
  controls: ControllerSchema,
): void {
  requireSameOrder(
    control.buttons.map((button) => button.id),
    controls.buttons,
    'PortControl.buttons',
  );
  requireSameOrder(
    control.axes.map((axis) => axis.id),
    controls.axes.map((axis) => axis.id),
    'PortControl.axes',
  );
  control.axes.forEach((axis, index) => {
    const schema = controls.axes[index] as AxisSchema;
    const [low, high] = axisBounds(schema.range);
    if (axis.value < low || axis.value > high) {
      fail(
        `PortControl: axis "${axis.id}" value ${axis.value} is outside its ${schema.range} range and is refused, not clamped`,
      );
    }
  });
}

export function readEnvironmentDescriptor(value: unknown): EnvironmentDescriptor {
  const reader = new Reader(value, 'EnvironmentDescriptor');
  const descriptor: EnvironmentDescriptor = {
    backendDigest: reader.digest('backendDigest'),
    contentDigest: reader.digest('contentDigest'),
    configurationDigest: reader.digest('configurationDigest'),
    stepDuration: readRational(reader.value('stepDuration')),
    ports: reader.list('ports', 1, MAX_PORTS, (item) => {
      const port = new Reader(item, 'EnvironmentDescriptor.ports');
      const entry = {
        portId: port.id('portId'),
        controls: readControllerSchema(port.value('controls')),
      };
      port.finish();
      return entry;
    }),
    inspectionSchema: readSchemaRef(reader.value('inspectionSchema')),
    views: reader.list('views', 0, MAX_VIEWS, readViewDescriptor),
    audio: reader.list('audio', 0, 8, readAudioDescriptor),
    recovery: reader.enumeration('recovery', RECOVERY),
    determinism: reader.enumeration('determinism', DETERMINISM),
  };
  reader.finish();
  requirePositiveRational(descriptor.stepDuration, 'EnvironmentDescriptor.stepDuration');
  requireUnique(
    descriptor.ports.map((port) => port.portId),
    'EnvironmentDescriptor.ports',
  );
  requireUnique(
    descriptor.views.map((view) => view.viewId),
    'EnvironmentDescriptor.views',
  );
  requireUnique(
    descriptor.audio.map((stream) => stream.streamId),
    'EnvironmentDescriptor.audio',
  );
  return descriptor;
}

export function findPort(
  descriptor: EnvironmentDescriptor,
  portId: string,
): PortDescriptor | undefined {
  return descriptor.ports.find((port) => port.portId === portId);
}

/** One complete batch: every configured port once, in descriptor order. */
export function validateBatch(
  descriptor: EnvironmentDescriptor,
  controls: readonly PortControl[],
): void {
  requireSameOrder(
    controls.map((control) => control.portId),
    descriptor.ports.map((port) => port.portId),
    'Environment.Advance controls',
  );
  controls.forEach((control, index) => {
    validatePortControlAgainst(control, (descriptor.ports[index] as PortDescriptor).controls);
  });
}

export function readWorldObservation(value: unknown): WorldObservation {
  const reader = new Reader(value, 'WorldObservation');
  const observation: WorldObservation = {
    boundary: reader.u64('boundary'),
    worldTime: readRational(reader.value('worldTime')),
    engineFrame: reader.nullableBoundedString('engineFrame', MAX_ENGINE_FRAME_LEN),
    sensoryViews: readViewList(reader, 'sensoryViews'),
    inspection: readTypedValue(reader.value('inspection')),
    broadcastViews: readViewList(reader, 'broadcastViews'),
    audio: readAudioList(reader, 'audio'),
  };
  reader.finish();
  for (const view of [...observation.sensoryViews, ...observation.broadcastViews]) {
    if (u64(view.producedStep) > u64(observation.boundary)) {
      fail('WorldObservation: a view cannot be produced after the boundary');
    }
  }
  return observation;
}

export function validateObservationAgainst(
  observation: WorldObservation,
  descriptor: EnvironmentDescriptor,
): void {
  const expected = descriptor.inspectionSchema;
  const found = observation.inspection.schema;
  if (
    found.id !== expected.id ||
    found.version !== expected.version ||
    found.digest !== expected.digest
  ) {
    fail("WorldObservation: inspection must use the descriptor's inspectionSchema");
  }
  for (const view of [...observation.sensoryViews, ...observation.broadcastViews]) {
    const declared = descriptor.views.find((candidate) => candidate.viewId === view.viewId);
    if (!declared) {
      fail(`WorldObservation: view "${view.viewId}" is not declared by the descriptor`);
    }
    validateViewAgainst(view, declared, u64(observation.boundary));
  }
  for (const chunk of observation.audio) {
    const declared = descriptor.audio.find((candidate) => candidate.streamId === chunk.streamId);
    if (!declared) {
      fail(`WorldObservation: audio stream "${chunk.streamId}" is not declared by the descriptor`);
    }
    validateAudioAgainst(chunk, declared);
  }
}

export function readEnvironmentInitializeParams(value: unknown): EnvironmentInitializeParams {
  const reader = new Reader(value, 'EnvironmentInitializeParams');
  const params: EnvironmentInitializeParams = {
    backendConfig: readAssetRef(reader.value('backendConfig')),
    taskConfig: readAssetRef(reader.value('taskConfig')),
    episodeId: reader.id('episodeId'),
    portBindings: reader.list('portBindings', 1, MAX_PORTS, (item) => {
      const binding = new Reader(item, 'EnvironmentInitializeParams.portBindings');
      const entry = { portId: binding.id('portId'), agentId: binding.id('agentId') };
      binding.finish();
      return entry;
    }),
  };
  reader.finish();
  requireUnique(
    params.portBindings.map((binding) => binding.portId),
    'EnvironmentInitializeParams.portBindings portId',
  );
  requireUnique(
    params.portBindings.map((binding) => binding.agentId),
    'EnvironmentInitializeParams.portBindings agentId',
  );
  return params;
}

export function readEnvironmentInitializeResult(value: unknown): EnvironmentInitializeResult {
  const reader = new Reader(value, 'EnvironmentInitializeResult');
  const result: EnvironmentInitializeResult = {
    descriptor: readEnvironmentDescriptor(reader.value('descriptor')),
    observation: readWorldObservation(reader.value('observation')),
  };
  reader.finish();
  if (u64(result.observation.boundary) !== 0n) {
    fail('EnvironmentInitializeResult: the initial observation is boundary 0');
  }
  if (!isRationalZero(result.observation.worldTime)) {
    fail('EnvironmentInitializeResult: initial worldTime is zero (0/1)');
  }
  validateObservationAgainst(result.observation, result.descriptor);
  return result;
}

export function readAdvanceParams(value: unknown): AdvanceParams {
  const reader = new Reader(value, 'AdvanceParams');
  const params: AdvanceParams = {
    batchId: reader.id('batchId'),
    controls: reader.list('controls', 1, MAX_PORTS, readPortControl),
  };
  reader.finish();
  requireUnique(
    params.controls.map((control) => control.portId),
    'AdvanceParams.controls',
  );
  return params;
}

export function readStepResult(value: unknown): StepResult {
  const reader = new Reader(value, 'StepResult');
  const result: StepResult = {
    batchId: reader.id('batchId'),
    appliedFromStep: reader.u64('appliedFromStep'),
    nextStep: reader.u64('nextStep'),
    appliedControlsDigest: reader.digest('appliedControlsDigest'),
    observation: readWorldObservation(reader.value('observation')),
  };
  reader.finish();
  if (u64(result.nextStep) !== u64(result.appliedFromStep) + 1n) {
    fail('StepResult: nextStep must be appliedFromStep + 1; one result is one step');
  }
  if (u64(result.observation.boundary) !== u64(result.nextStep)) {
    fail('StepResult: the observation boundary must be nextStep');
  }
  return result;
}

/** Exactly boundary `k+1`, with world time advanced by exactly one `stepDuration`. */
export function validateStepResultAgainst(
  result: StepResult,
  descriptor: EnvironmentDescriptor,
  previous: WorldObservation,
): void {
  validateObservationAgainst(result.observation, descriptor);
  const expected = addRational(previous.worldTime, descriptor.stepDuration);
  if (compareRational(result.observation.worldTime, expected) !== 0) {
    fail('StepResult: worldTime must advance by exactly one stepDuration');
  }
  if (u64(result.observation.boundary) !== u64(previous.boundary) + 1n) {
    fail('StepResult: the observation must be exactly the next boundary');
  }
}

// --------------------------------------------------------------------- common worker methods

export interface HelloParams {
  sessionId: Id;
  expectedWorkerId: Id;
  role: Role;
  supportedMajors: number[];
}

export interface HelloResult {
  selectedMajor: 1;
  selectedMinor: 0;
  workerId: Id;
  incarnationId: Id;
  role: Role;
  buildDigest: Digest;
  contractDigest: Digest;
  capabilities: Id[];
  /**
   * `workerThreads` is the thread allocation this worker was launched within, added by the
   * 2026-09-22 amendment to workers-v1 section 2: the bound "within launcher allocation" had
   * no wire on which a caller could learn the allocation.
   */
  limits: { maxAgents: number; maxPorts: number; workerThreads: number };
}

export interface StatusResult {
  state: WorkerState;
  currentScope: Scope | null;
  activeRequestId: DomainRequestId | null;
  lastCompletedRequestId: DomainRequestId | null;
  lastBatchId: Id | null;
  progressCounter: U64;
}

export interface AcknowledgeParams {
  requestIds: DomainRequestId[];
}

export interface AcknowledgeResult {
  acknowledged: DomainRequestId[];
}

export interface ShutdownParams {
  reason: Id;
}

export interface ShutdownResult {
  stopping: true;
}

export interface TaskEvent {
  id: Id;
  kindId: Id;
  sourceStep: U64;
  agentId: Id | null;
  payload: TypedValue;
}

/**
 * `rollback` is the amendment of 2026-09-23 (RT-01a): only a composition that declares a
 * rollback policy may act on it.
 */
export const EPISODE_REQUEST_KINDS = ['terminal', 'rollback'] as const;
export type EpisodeRequestKind = (typeof EPISODE_REQUEST_KINDS)[number];

export interface EpisodeRequest {
  kind: EpisodeRequestKind;
  reason: Id;
  outcome: TypedValue;
}

/** Required capabilities are agent-step-v1 and world-step-v1 for their roles. */
export function requiredCapability(role: Role): string | null {
  if (role === 'agent') return 'agent-step-v1';
  if (role === 'environment') return 'world-step-v1';
  return null;
}

export function readHelloParams(value: unknown): HelloParams {
  const reader = new Reader(value, 'HelloParams');
  const params: HelloParams = {
    sessionId: reader.id('sessionId'),
    expectedWorkerId: reader.id('expectedWorkerId'),
    role: reader.enumeration('role', ROLES),
    supportedMajors: reader.list('supportedMajors', 1, MAX_SUPPORTED_MAJORS, (item) => {
      if (typeof item !== 'number' || !Number.isInteger(item) || item < 1 || item > 65_535) {
        fail('every supported major must be an integer 1..=65535');
      }
      return item;
    }),
  };
  reader.finish();
  requireUnique(
    params.supportedMajors.map(String),
    'HelloParams.supportedMajors',
  );
  return params;
}

export function readHelloResult(value: unknown): HelloResult {
  const reader = new Reader(value, 'HelloResult');
  const selectedMajor = reader.int('selectedMajor', 1, 1) as 1;
  const selectedMinor = reader.int('selectedMinor', 0, 0) as 0;
  const workerId = reader.id('workerId');
  const incarnationId = reader.id('incarnationId');
  const role = reader.enumeration('role', ROLES);
  const buildDigest = reader.digest('buildDigest');
  const contractDigest = reader.digest('contractDigest');
  const capabilities = reader.idList('capabilities', 0, MAX_CAPABILITIES);
  const limitsReader = new Reader(reader.value('limits'), 'HelloResult.limits');
  const limits = {
    maxAgents: limitsReader.int('maxAgents', 1, MAX_AGENTS),
    maxPorts: limitsReader.int('maxPorts', 1, MAX_PORTS),
    workerThreads: limitsReader.int('workerThreads', 1, MAX_WORKER_THREADS),
  };
  limitsReader.finish();
  reader.finish();
  requireUnique(capabilities, 'HelloResult.capabilities');
  const required = requiredCapability(role);
  if (required !== null && !capabilities.includes(required)) {
    fail(`HelloResult: a ${role} worker must advertise ${required}`);
  }
  return {
    selectedMajor,
    selectedMinor,
    workerId,
    incarnationId,
    role,
    buildDigest,
    contractDigest,
    capabilities,
    limits,
  };
}

export function readStatusResult(value: unknown): StatusResult {
  const reader = new Reader(value, 'StatusResult');
  const state = reader.enumeration('state', WORKER_STATES);
  const currentScopeValue = reader.value('currentScope');
  const currentScope = currentScopeValue === null ? null : readScope(currentScopeValue);
  const active = reader.value('activeRequestId');
  const completed = reader.value('lastCompletedRequestId');
  const result: StatusResult = {
    state,
    currentScope,
    activeRequestId: active === null ? null : domainRequestId(active),
    lastCompletedRequestId: completed === null ? null : domainRequestId(completed),
    lastBatchId: reader.nullableId('lastBatchId'),
    progressCounter: reader.u64('progressCounter'),
  };
  reader.finish();
  if (result.state === 'uninitialized' && result.currentScope !== null) {
    fail('StatusResult: an uninitialized worker has a null currentScope');
  }
  return result;
}

export function readAcknowledgeParams(value: unknown): AcknowledgeParams {
  const reader = new Reader(value, 'AcknowledgeParams');
  const params: AcknowledgeParams = {
    requestIds: reader.list('requestIds', 1, MAX_ACKNOWLEDGE, domainRequestId),
  };
  reader.finish();
  requireUnique(params.requestIds, 'AcknowledgeParams.requestIds');
  return params;
}

export function readAcknowledgeResult(value: unknown): AcknowledgeResult {
  const reader = new Reader(value, 'AcknowledgeResult');
  const result: AcknowledgeResult = {
    acknowledged: reader.list('acknowledged', 0, MAX_ACKNOWLEDGE, domainRequestId),
  };
  reader.finish();
  requireUnique(result.acknowledged, 'AcknowledgeResult.acknowledged');
  return result;
}

export function readShutdownParams(value: unknown): ShutdownParams {
  const reader = new Reader(value, 'ShutdownParams');
  const params = { reason: reader.id('reason') };
  reader.finish();
  return params;
}

export function readShutdownResult(value: unknown): ShutdownResult {
  const reader = new Reader(value, 'ShutdownResult');
  const result: ShutdownResult = { stopping: reader.constantTrue('stopping') };
  reader.finish();
  return result;
}

export function readTaskEvent(value: unknown): TaskEvent {
  const reader = new Reader(value, 'TaskEvent');
  const event: TaskEvent = {
    id: reader.id('id'),
    kindId: reader.id('kindId'),
    sourceStep: reader.u64('sourceStep'),
    agentId: reader.nullableId('agentId'),
    payload: readTypedValue(reader.value('payload')),
  };
  reader.finish();
  return event;
}

export function readEpisodeRequest(value: unknown): EpisodeRequest {
  const reader = new Reader(value, 'EpisodeRequest');
  const request: EpisodeRequest = {
    kind: reader.enumeration('kind', EPISODE_REQUEST_KINDS),
    reason: reader.id('reason'),
    outcome: readTypedValue(reader.value('outcome')),
  };
  reader.finish();
  return request;
}

export interface ActivateRestoreResult {
  committedStep: U64;
  checkpointId: Id;
  observation: WorldObservation | null;
}

export function readActivateRestoreResult(value: unknown): ActivateRestoreResult {
  const reader = new Reader(value, 'ActivateRestoreResult');
  const observationValue = reader.value('observation');
  const result: ActivateRestoreResult = {
    committedStep: reader.u64('committedStep'),
    checkpointId: reader.id('checkpointId'),
    observation: observationValue === null ? null : readWorldObservation(observationValue),
  };
  reader.finish();
  if (
    result.observation !== null &&
    u64(result.observation.boundary) !== u64(result.committedStep)
  ) {
    fail('ActivateRestoreResult: the observation boundary must be the committed step');
  }
  return result;
}

/** An environment returns its restored observation; an agent returns null. */
export function validateActivateRestoreForRole(result: ActivateRestoreResult, role: Role): void {
  if (role === 'environment' && result.observation === null) {
    fail('ActivateRestoreResult: an environment must return its restored observation');
  }
  if (role === 'agent' && result.observation !== null) {
    fail('ActivateRestoreResult: an agent returns a null observation');
  }
}
