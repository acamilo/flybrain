/** The domain request/reply envelope of ipc-v1 section 3 and the error codes of section 7. */
import { fail, rejectBusIdentities } from './canonical';
import { bodyDigest, readNullableScope } from './common';
import { Reader } from './reader';
import { type DomainRequestId, type Id, type Scope, domainRequestId } from './scalar';
import { MAX_MESSAGE_CODE_POINTS } from './workers';

export const ERROR_CODES = [
  'INVALID_ARGUMENT',
  'UNSUPPORTED',
  'IDENTITY_MISMATCH',
  'STALE_EPOCH',
  'STALE_STEP',
  'FUTURE_STEP',
  'INVALID_PHASE',
  'CONFLICT',
  'IN_PROGRESS',
  'BUSY',
  'BUFFER_INVALID',
  'RESULT_EXPIRED',
  'INCOMPATIBLE_STATE',
  'BACKEND_FAILURE',
  'INTERNAL',
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

export const MUTATION_CERTAINTIES = ['none', 'applied', 'unknown'] as const;
export type MutationCertainty = (typeof MUTATION_CERTAINTIES)[number];

/** The codes raised strictly before any mutation, so their certainty is `none`. */
export const BEFORE_MUTATION: readonly ErrorCode[] = [
  'INVALID_ARGUMENT',
  'UNSUPPORTED',
  'IDENTITY_MISMATCH',
  'STALE_EPOCH',
  'STALE_STEP',
  'FUTURE_STEP',
  'INVALID_PHASE',
  'CONFLICT',
  'IN_PROGRESS',
  'BUSY',
  'BUFFER_INVALID',
  'INCOMPATIBLE_STATE',
];

export interface SessionRpcRequest {
  requestId: DomainRequestId;
  scope: Scope | null;
  params: Record<string, unknown>;
}

export interface SessionRpcSuccess {
  type: 'result';
  requestId: DomainRequestId;
  workerId: Id;
  incarnationId: Id;
  scope: Scope | null;
  result: Record<string, unknown>;
}

export interface SessionRpcFailure {
  type: 'error';
  requestId: DomainRequestId;
  workerId: Id;
  incarnationId: Id;
  scope: Scope | null;
  error: { code: ErrorCode; message: string; mutation: MutationCertainty };
}

export type SessionRpcOutcome = SessionRpcSuccess | SessionRpcFailure;

export function readSessionRpcRequest(value: unknown): SessionRpcRequest {
  const reader = new Reader(value, 'SessionRpcRequest');
  const request: SessionRpcRequest = {
    requestId: domainRequestId(reader.string('requestId')),
    scope: readNullableScope(reader.value('scope')),
    params: reader.object('params'),
  };
  reader.finish();
  rejectBusIdentities(request.params);
  return request;
}

/** The canonical body digest of this request under `method` (ipc-v1 section 5). */
export function requestBodyDigest(request: SessionRpcRequest, method: string): string {
  return bodyDigest(method, request.scope, request.params);
}

export function readSessionRpcSuccess(value: unknown): SessionRpcSuccess {
  const reader = new Reader(value, 'SessionRpcSuccess');
  const success: SessionRpcSuccess = {
    type: reader.constant('type', 'result'),
    requestId: domainRequestId(reader.string('requestId')),
    workerId: reader.id('workerId'),
    incarnationId: reader.id('incarnationId'),
    scope: readNullableScope(reader.value('scope')),
    result: reader.object('result'),
  };
  reader.finish();
  return success;
}

export function readSessionRpcFailure(value: unknown): SessionRpcFailure {
  const reader = new Reader(value, 'SessionRpcFailure');
  const type = reader.constant('type', 'error');
  const requestId = domainRequestId(reader.string('requestId'));
  const workerId = reader.id('workerId');
  const incarnationId = reader.id('incarnationId');
  const scope = readNullableScope(reader.value('scope'));
  const errorReader = new Reader(reader.value('error'), 'SessionRpcFailure.error');
  const error = {
    code: errorReader.enumeration('code', ERROR_CODES),
    message: errorReader.boundedString('message', MAX_MESSAGE_CODE_POINTS),
    mutation: errorReader.enumeration('mutation', MUTATION_CERTAINTIES),
  };
  errorReader.finish();
  reader.finish();
  if (BEFORE_MUTATION.includes(error.code) && error.mutation !== 'none') {
    fail(`SessionRpcFailure: ${error.code} is raised before mutation, so mutation is "none"`);
  }
  return { type, requestId, workerId, incarnationId, scope, error };
}

export function readSessionRpcOutcome(value: unknown): SessionRpcOutcome {
  const type = (value as { type?: unknown } | null)?.type;
  if (type === 'result') return readSessionRpcSuccess(value);
  if (type === 'error') return readSessionRpcFailure(value);
  return fail('SessionRpcOutcome: type must be "result" or "error"');
}

/** Replies echo the original scope (ipc-v1 section 3). */
export function echoes(outcome: SessionRpcOutcome, request: SessionRpcRequest): boolean {
  const sameScope =
    outcome.scope === null || request.scope === null
      ? outcome.scope === request.scope
      : outcome.scope.sessionId === request.scope.sessionId &&
        outcome.scope.epoch === request.scope.epoch &&
        outcome.scope.step === request.scope.step;
  return outcome.requestId === request.requestId && sameScope;
}
