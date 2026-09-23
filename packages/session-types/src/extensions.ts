/**
 * The extension methods of the 2026-09-23 amendments (RT-01a, workers-v1 section 7):
 * `Environment.SaveSlot`, `Environment.RestoreSlot` and `Agent.Rollback`.
 *
 * The Rust twin is `fly-session-types/src/extensions.rs`. The shapes are generic; which
 * composition may use them is a capability negotiated by `Worker.Hello`. Nothing console
 * specific is in these payloads: the Game Boy lives in the registered schemas of `gameboy`.
 */
import { fail } from './canonical';
import { readTypedValue } from './common';
import { Reader, u64 } from './reader';
import type { Digest, Id, Scope, TypedValue, U64 } from './scalar';
import {
  type AgentTelemetry,
  type SensoryInput,
  type WorldObservation,
  readAgentTelemetry,
  readSensoryInput,
  readWorldObservation,
} from './workers';

export const SLOTS_CAPABILITY = 'gameboy-slots-v1';
export const ROLLBACK_CAPABILITY = 'legacy-ratchet-rollback-v1';
export const ROLLBACK_POLICY = 'legacy-ratchet-rollback-v1';
export const METHOD_SAVE_SLOT = 'Environment.SaveSlot';
export const METHOD_RESTORE_SLOT = 'Environment.RestoreSlot';
export const METHOD_AGENT_ROLLBACK = 'Agent.Rollback';
/** Slots one environment may hold. Not a stated bound; recorded in the schema set. */
export const MAX_SLOTS = 4;

function requirePolicy(policy: string, what: string): void {
  if (policy !== ROLLBACK_POLICY) {
    fail(`${what}: policy must be ${ROLLBACK_POLICY}, the only rollback policy defined`);
  }
}

export interface SaveSlotParams {
  slotId: Id;
}

export function readSaveSlotParams(value: unknown): SaveSlotParams {
  const reader = new Reader(value, 'SaveSlotParams');
  const params: SaveSlotParams = { slotId: reader.id('slotId') };
  reader.finish();
  return params;
}

export interface SaveSlotResult {
  slotId: Id;
  boundary: U64;
  stateDigest: Digest;
  byteLength: U64;
}

export function readSaveSlotResult(value: unknown): SaveSlotResult {
  const reader = new Reader(value, 'SaveSlotResult');
  const result: SaveSlotResult = {
    slotId: reader.id('slotId'),
    boundary: reader.u64('boundary'),
    stateDigest: reader.digest('stateDigest'),
    byteLength: reader.u64('byteLength'),
  };
  reader.finish();
  if (u64(result.byteLength) === 0n) fail('SaveSlotResult: byteLength must be positive');
  return result;
}

/** The slot records the committed boundary the call was scoped to. */
export function validateSaveSlotAgainstScope(result: SaveSlotResult, scope: Scope): void {
  if (result.boundary !== scope.step) {
    fail(`SaveSlotResult: boundary ${result.boundary} must be the scoped committed step ${scope.step}`);
  }
}

export interface RestoreSlotParams {
  slotId: Id;
  priorEpoch: Id;
  policy: Id;
}

export function readRestoreSlotParams(value: unknown): RestoreSlotParams {
  const reader = new Reader(value, 'RestoreSlotParams');
  const params: RestoreSlotParams = {
    slotId: reader.id('slotId'),
    priorEpoch: reader.id('priorEpoch'),
    policy: reader.id('policy'),
  };
  reader.finish();
  requirePolicy(params.policy, 'RestoreSlotParams');
  return params;
}

/** A rollback always moves to a new epoch. */
export function validateRestoreSlotAgainstScope(params: RestoreSlotParams, scope: Scope): void {
  if (params.priorEpoch === scope.epoch) {
    fail('RestoreSlotParams: priorEpoch must differ from the scoped (new) epoch');
  }
}

export interface RestoreSlotResult {
  slotId: Id;
  committedStep: U64;
  observation: WorldObservation;
}

export function readRestoreSlotResult(value: unknown): RestoreSlotResult {
  const reader = new Reader(value, 'RestoreSlotResult');
  const result: RestoreSlotResult = {
    slotId: reader.id('slotId'),
    committedStep: reader.u64('committedStep'),
    observation: readWorldObservation(reader.value('observation')),
  };
  reader.finish();
  if (result.observation.boundary !== result.committedStep) {
    fail('RestoreSlotResult: the observation boundary must be the committed step');
  }
  if (result.observation.audio.length !== 0) {
    fail('RestoreSlotResult: a restored slot ran no transition and carries no audio chunk');
  }
  return result;
}

export interface AgentRollbackParams {
  agentId: Id;
  priorEpoch: Id;
  policy: Id;
  input: SensoryInput;
  decisionContext: TypedValue;
}

export function readAgentRollbackParams(value: unknown): AgentRollbackParams {
  const reader = new Reader(value, 'AgentRollbackParams');
  const params: AgentRollbackParams = {
    agentId: reader.id('agentId'),
    priorEpoch: reader.id('priorEpoch'),
    policy: reader.id('policy'),
    input: readSensoryInput(reader.value('input')),
    decisionContext: readTypedValue(reader.value('decisionContext')),
  };
  reader.finish();
  requirePolicy(params.policy, 'AgentRollbackParams');
  return params;
}

/** A new epoch, and the installed input is the restored boundary: the scoped step. */
export function validateAgentRollbackAgainstScope(params: AgentRollbackParams, scope: Scope): void {
  if (params.priorEpoch === scope.epoch) {
    fail('AgentRollbackParams: priorEpoch must differ from the scoped (new) epoch');
  }
  if (params.input.boundary !== scope.step) {
    fail(`AgentRollbackParams: input.boundary ${params.input.boundary} must be the scoped step ${scope.step}`);
  }
}

export interface AgentRollbackResult {
  agentId: Id;
  committedStep: U64;
  decisionContextDigest: Digest;
  telemetry: AgentTelemetry;
}

export function readAgentRollbackResult(value: unknown): AgentRollbackResult {
  const reader = new Reader(value, 'AgentRollbackResult');
  const result: AgentRollbackResult = {
    agentId: reader.id('agentId'),
    committedStep: reader.u64('committedStep'),
    decisionContextDigest: reader.digest('decisionContextDigest'),
    telemetry: readAgentTelemetry(reader.value('telemetry')),
  };
  reader.finish();
  return result;
}

/** A rollback runs no tick: the acknowledged step is the scoped one. */
export function validateAgentRollbackResultAgainstScope(
  result: AgentRollbackResult,
  scope: Scope,
): void {
  if (result.committedStep !== scope.step) {
    fail(
      `AgentRollbackResult: committedStep ${result.committedStep} must be the scoped step ${scope.step}; a rollback runs no tick`,
    );
  }
}
