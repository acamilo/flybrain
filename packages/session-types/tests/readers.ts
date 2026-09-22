/** One place that knows how to read every type named by a fixture. */
import type { Json } from '../src/canonical';
import {
  readRational,
  readSchemaRef,
  readScope,
  readTypedValue,
} from '../src/common';
import {
  readActivateRestoreParams,
  readAudioDescriptor,
  readAudioRef,
  readCaptureParams,
  readCaptureResult,
  readStageRestoreParams,
  readStageRestoreResult,
  readViewDescriptor,
  readViewRef,
} from '../src/media';
import { readCommittedSnapshot, readSessionDescriptor } from '../src/publishing';
import {
  readSessionRpcFailure,
  readSessionRpcRequest,
  readSessionRpcSuccess,
} from '../src/rpc';
import {
  readTraceBehaviour,
  readTraceOperational,
  readTransitionTrace,
} from '../src/trace';
import {
  readAcknowledgeParams,
  readAcknowledgeResult,
  readActivateRestoreResult,
  readAdvanceParams,
  readAgentCommitResult,
  readAgentInitializeParams,
  readAgentInitializeResult,
  readAgentTelemetry,
  readAssetRef,
  readCommitParams,
  readControllerSchema,
  readEnvironmentDescriptor,
  readEnvironmentInitializeParams,
  readEnvironmentInitializeResult,
  readEpisodeRequest,
  readHelloParams,
  readHelloResult,
  readPortControl,
  readPrepareParams,
  readPreparedDecision,
  readReward,
  readSensoryInput,
  readShutdownParams,
  readShutdownResult,
  readStatusResult,
  readStepResult,
  readStimulus,
  readTaskEvent,
  readWorldObservation,
} from '../src/workers';

/** Every type the fixtures name, and the reader that validates it. */
export const READERS: Record<string, (value: unknown) => unknown> = {
  Scope: readScope,
  RationalNs: readRational,
  SchemaRef: readSchemaRef,
  TypedValue: readTypedValue,
  SessionRpcRequest: readSessionRpcRequest,
  SessionRpcSuccess: readSessionRpcSuccess,
  SessionRpcFailure: readSessionRpcFailure,
  AssetRef: readAssetRef,
  SensoryInput: readSensoryInput,
  Stimulus: readStimulus,
  Reward: readReward,
  AgentTelemetry: readAgentTelemetry,
  AgentInitializeParams: readAgentInitializeParams,
  AgentInitializeResult: readAgentInitializeResult,
  PrepareParams: readPrepareParams,
  PreparedDecision: readPreparedDecision,
  CommitParams: readCommitParams,
  AgentCommitResult: readAgentCommitResult,
  ControllerSchema: readControllerSchema,
  PortControl: readPortControl,
  EnvironmentDescriptor: readEnvironmentDescriptor,
  EnvironmentInitializeParams: readEnvironmentInitializeParams,
  EnvironmentInitializeResult: readEnvironmentInitializeResult,
  WorldObservation: readWorldObservation,
  AdvanceParams: readAdvanceParams,
  StepResult: readStepResult,
  HelloParams: readHelloParams,
  HelloResult: readHelloResult,
  StatusResult: readStatusResult,
  AcknowledgeParams: readAcknowledgeParams,
  AcknowledgeResult: readAcknowledgeResult,
  ShutdownParams: readShutdownParams,
  ShutdownResult: readShutdownResult,
  TaskEvent: readTaskEvent,
  EpisodeRequest: readEpisodeRequest,
  ViewDescriptor: readViewDescriptor,
  ViewRef: readViewRef,
  AudioDescriptor: readAudioDescriptor,
  AudioRef: readAudioRef,
  CaptureParams: readCaptureParams,
  CaptureResult: readCaptureResult,
  StageRestoreParams: readStageRestoreParams,
  StageRestoreResult: readStageRestoreResult,
  ActivateRestoreParams: readActivateRestoreParams,
  ActivateRestoreResult: readActivateRestoreResult,
  SessionDescriptor: readSessionDescriptor,
  CommittedSnapshot: readCommittedSnapshot,
  TraceBehaviour: readTraceBehaviour,
  TraceOperational: readTraceOperational,
  TransitionTrace: readTransitionTrace,
};

/** Reads the value as `typeName` and hands back what the reader reconstructed. */
export function roundTrip(typeName: string, value: Json): unknown {
  const reader = READERS[typeName];
  if (!reader) throw new Error(`no fixture reader for type "${typeName}"`);
  return reader(value);
}
