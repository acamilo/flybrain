/**
 * The trace format of step-v1 section 8, split into behaviour and operational metadata.
 *
 * `behaviourEquals` compares the first half only, which is the comparison section 8 asks for:
 * sequential, concurrent and reversed runs must agree "excluding wall time, request ids and
 * other explicitly operational metadata".
 */
import { canonicalize, digestOf, fail } from './canonical';
import { readRational, readScope } from './common';
import { MAX_VIEWS } from './media';
import { Reader, requireUnique, u64 } from './reader';
import {
  type BusCallId,
  type Digest,
  type DomainRequestId,
  type Id,
  type OwnerToken,
  type RationalNs,
  type Scope,
  type U64,
  busCallId,
  domainRequestId,
  ownerToken,
} from './scalar';
import { MAX_AGENTS, MAX_RATE_ROLES } from './workers';
import { MAX_SLOTS } from './extensions';

/** Boundary actions at the reached boundary (amendment of 2026-09-23, RT-01a). */
export const BOUNDARY_ACTION_KINDS = ['save-slot', 'rollback'] as const;
export type BoundaryActionKind = (typeof BOUNDARY_ACTION_KINDS)[number];
/** Every slot at most once, plus one rollback. */
export const MAX_BOUNDARY_ACTIONS = MAX_SLOTS + 1;

export interface BoundaryAction {
  kind: BoundaryActionKind;
  slotId: Id;
  stateDigest: Digest | null;
}

/** A checkpoint capture at the reached boundary and how many boundary actions preceded it. */
export interface TraceCapture {
  checkpointId: Id;
  afterActions: number;
}

export interface TraceAgent {
  agentId: Id;
  profileDigest: Digest;
  ticksAdvanced: U64;
  brainTicks: U64;
  remainder: RationalNs;
  decisionDigest: Digest;
  committedStep: U64;
}

export interface TraceObservation {
  viewId: Id;
  producedStep: U64;
}

export interface TraceBehaviour {
  scope: Scope;
  agents: TraceAgent[];
  batchId: Id;
  controlDigest: Digest;
  acknowledgedBoundary: U64;
  observationBoundaries: TraceObservation[];
  outcomeIds: Id[];
  eventIds: Id[];
  publishedBoundary: U64;
  boundaryActions: BoundaryAction[];
}

export interface TraceRequest {
  agentId: Id;
  requestId: DomainRequestId;
}

export interface TraceOperational {
  wallTimeNs: U64;
  prepareRequestIds: TraceRequest[];
  advanceRequestId: DomainRequestId;
  commitRequestIds: TraceRequest[];
  busCallIds: BusCallId[];
  deliveryIds: OwnerToken[];
  captures: TraceCapture[];
}

export interface TransitionTrace {
  behaviour: TraceBehaviour;
  operational: TraceOperational;
}

export function readTraceBehaviour(value: unknown): TraceBehaviour {
  const reader = new Reader(value, 'TraceBehaviour');
  const scope = readScope(reader.value('scope'));
  const agents = reader.list('agents', 1, MAX_AGENTS, (item) => {
    const agent = new Reader(item, 'TraceBehaviour.agents');
    const entry: TraceAgent = {
      agentId: agent.id('agentId'),
      profileDigest: agent.digest('profileDigest'),
      ticksAdvanced: agent.u64('ticksAdvanced'),
      brainTicks: agent.u64('brainTicks'),
      remainder: readRational(agent.value('remainder')),
      decisionDigest: agent.digest('decisionDigest'),
      committedStep: agent.u64('committedStep'),
    };
    agent.finish();
    return entry;
  });
  const batchId = reader.id('batchId');
  const controlDigest = reader.digest('controlDigest');
  const acknowledgedBoundary = reader.u64('acknowledgedBoundary');
  const observationBoundaries = reader.list(
    'observationBoundaries',
    0,
    MAX_VIEWS * 2,
    (item) => {
      const observation = new Reader(item, 'TraceBehaviour.observationBoundaries');
      const entry: TraceObservation = {
        viewId: observation.id('viewId'),
        producedStep: observation.u64('producedStep'),
      };
      observation.finish();
      return entry;
    },
  );
  const outcomeIds = reader.idList('outcomeIds', 0, MAX_RATE_ROLES);
  const eventIds = reader.idList('eventIds', 0, MAX_RATE_ROLES);
  const publishedBoundary = reader.u64('publishedBoundary');
  const boundaryActions = reader.list('boundaryActions', 0, MAX_BOUNDARY_ACTIONS, (item) => {
    const action = new Reader(item, 'TraceBehaviour.boundaryActions');
    const entry: BoundaryAction = {
      kind: action.enumeration('kind', BOUNDARY_ACTION_KINDS),
      slotId: action.id('slotId'),
      stateDigest: action.value('stateDigest') === null ? null : action.digest('stateDigest'),
    };
    action.finish();
    return entry;
  });
  reader.finish();
  validateBoundaryActions(boundaryActions);

  requireUnique(
    agents.map((agent) => agent.agentId),
    'TraceBehaviour.agents',
  );
  requireUnique(
    observationBoundaries.map((observation) => observation.viewId),
    'TraceBehaviour.observationBoundaries',
  );
  requireUnique(eventIds, 'TraceBehaviour.eventIds');
  requireUnique(outcomeIds, 'TraceBehaviour.outcomeIds');
  const next = u64(scope.step) + 1n;
  for (const agent of agents) {
    if (u64(agent.committedStep) !== next) {
      fail("TraceBehaviour: every commit acknowledgment is the transition's next boundary");
    }
  }
  if (u64(acknowledgedBoundary) !== next) {
    fail('TraceBehaviour: the acknowledged boundary is scope.step + 1');
  }
  if (u64(publishedBoundary) !== u64(acknowledgedBoundary)) {
    fail('TraceBehaviour: the published boundary is the boundary every agent committed');
  }
  return {
    scope,
    agents,
    batchId,
    controlDigest,
    acknowledgedBoundary,
    observationBoundaries,
    outcomeIds,
    eventIds,
    publishedBoundary,
    boundaryActions,
  };
}

/** Slot saves first, each slot once with its digest; at most one rollback, last, no digest. */
function validateBoundaryActions(actions: readonly BoundaryAction[]): void {
  let rolledBack = false;
  const saved: string[] = [];
  for (const action of actions) {
    if (rolledBack) fail('TraceBehaviour: nothing follows a rollback at the same boundary');
    if (action.kind === 'save-slot') {
      if (action.stateDigest === null) fail('TraceBehaviour: a slot save records its state digest');
      if (saved.includes(action.slotId)) {
        fail('TraceBehaviour: a slot is saved at most once per boundary');
      }
      saved.push(action.slotId);
    } else {
      if (action.stateDigest !== null) fail('TraceBehaviour: a rollback records no state digest');
      rolledBack = true;
    }
  }
}

/** How many leading boundary actions are slot saves. */
export function slotSaves(behaviour: TraceBehaviour): number {
  let count = 0;
  for (const action of behaviour.boundaryActions) {
    if (action.kind !== 'save-slot') break;
    count += 1;
  }
  return count;
}

export function readTraceOperational(value: unknown): TraceOperational {
  const reader = new Reader(value, 'TraceOperational');
  const readRequests = (item: unknown): TraceRequest => {
    const request = new Reader(item, 'TraceOperational request');
    const entry: TraceRequest = {
      agentId: request.id('agentId'),
      requestId: domainRequestId(request.string('requestId')),
    };
    request.finish();
    return entry;
  };
  const operational: TraceOperational = {
    wallTimeNs: reader.u64('wallTimeNs'),
    prepareRequestIds: reader.list('prepareRequestIds', 1, MAX_AGENTS, readRequests),
    advanceRequestId: domainRequestId(reader.string('advanceRequestId')),
    commitRequestIds: reader.list('commitRequestIds', 1, MAX_AGENTS, readRequests),
    busCallIds: reader.list('busCallIds', 0, 64, busCallId),
    deliveryIds: reader.list('deliveryIds', 0, 64, ownerToken),
    captures: reader.list('captures', 0, MAX_BOUNDARY_ACTIONS + 1, (item) => {
      const capture = new Reader(item, 'TraceOperational.captures');
      const entry: TraceCapture = {
        checkpointId: capture.id('checkpointId'),
        afterActions: capture.int('afterActions', 0, MAX_BOUNDARY_ACTIONS),
      };
      capture.finish();
      return entry;
    }),
  };
  reader.finish();
  requireUnique(
    operational.prepareRequestIds.map((request) => request.agentId),
    'TraceOperational.prepareRequestIds',
  );
  requireUnique(
    operational.commitRequestIds.map((request) => request.agentId),
    'TraceOperational.commitRequestIds',
  );
  requireUnique(operational.busCallIds, 'TraceOperational.busCallIds');
  requireUnique(operational.deliveryIds, 'TraceOperational.deliveryIds');
  requireUnique(
    operational.captures.map((capture) => capture.checkpointId),
    'TraceOperational.captures',
  );
  let last = 0;
  for (const capture of operational.captures) {
    if (capture.afterActions < last) {
      fail('TraceOperational: captures are recorded in the order they were taken');
    }
    last = capture.afterActions;
  }
  return operational;
}

export function readTransitionTrace(value: unknown): TransitionTrace {
  const reader = new Reader(value, 'TransitionTrace');
  const trace: TransitionTrace = {
    behaviour: readTraceBehaviour(reader.value('behaviour')),
    operational: readTraceOperational(reader.value('operational')),
  };
  reader.finish();
  const agents = trace.behaviour.agents.map((agent) => agent.agentId);
  for (const phase of [trace.operational.prepareRequestIds, trace.operational.commitRequestIds]) {
    for (const request of phase) {
      if (!agents.includes(request.agentId)) {
        fail(
          `TransitionTrace: request recorded for "${request.agentId}", which is not in the transition`,
        );
      }
    }
  }
  // A slot save due at a boundary completes before any capture there (legacy-gameboy-v1 16).
  const saves = slotSaves(trace.behaviour);
  for (const capture of trace.operational.captures) {
    if (capture.afterActions < saves) {
      fail(
        `TransitionTrace: capture "${capture.checkpointId}" was taken before this boundary's slot saves completed`,
      );
    }
    if (capture.afterActions > trace.behaviour.boundaryActions.length) {
      fail(
        `TransitionTrace: capture "${capture.checkpointId}" counts more boundary actions than were applied`,
      );
    }
  }
  return trace;
}

/** Sorts the order-free collections, so completion order cannot change the comparison. */
export function normalizeBehaviour(behaviour: TraceBehaviour): TraceBehaviour {
  return {
    ...behaviour,
    agents: [...behaviour.agents].sort((left, right) =>
      left.agentId < right.agentId ? -1 : left.agentId > right.agentId ? 1 : 0,
    ),
    observationBoundaries: [...behaviour.observationBoundaries].sort((left, right) =>
      left.viewId < right.viewId ? -1 : left.viewId > right.viewId ? 1 : 0,
    ),
  };
}

export function behaviourDigest(behaviour: TraceBehaviour): string {
  return digestOf(normalizeBehaviour(behaviour));
}

export function behaviourEquals(left: TransitionTrace, right: TransitionTrace): boolean {
  return (
    canonicalize(normalizeBehaviour(left.behaviour)) ===
    canonicalize(normalizeBehaviour(right.behaviour))
  );
}

/** The behaviour fields that differ, named. Empty when `behaviourEquals` holds. */
export function behaviourDiff(left: TransitionTrace, right: TransitionTrace): string[] {
  const a = normalizeBehaviour(left.behaviour);
  const b = normalizeBehaviour(right.behaviour);
  const out: string[] = [];
  const differs = (first: unknown, second: unknown): boolean =>
    canonicalize(first) !== canonicalize(second);
  if (differs(a.scope, b.scope)) out.push(`scope: ${canonicalize(a.scope)} vs ${canonicalize(b.scope)}`);
  if (a.batchId !== b.batchId) out.push(`batchId: ${a.batchId} vs ${b.batchId}`);
  if (a.controlDigest !== b.controlDigest) out.push('controlDigest differs');
  if (a.acknowledgedBoundary !== b.acknowledgedBoundary) {
    out.push(`acknowledgedBoundary: ${a.acknowledgedBoundary} vs ${b.acknowledgedBoundary}`);
  }
  if (a.publishedBoundary !== b.publishedBoundary) {
    out.push(`publishedBoundary: ${a.publishedBoundary} vs ${b.publishedBoundary}`);
  }
  if (differs(a.observationBoundaries, b.observationBoundaries)) {
    out.push('observationBoundaries differ');
  }
  if (differs(a.outcomeIds, b.outcomeIds)) out.push('outcomeIds differ');
  if (differs(a.eventIds, b.eventIds)) out.push('eventIds differ');
  if (differs(a.boundaryActions, b.boundaryActions)) out.push('boundaryActions differ');
  const idsA = a.agents.map((agent) => agent.agentId);
  const idsB = b.agents.map((agent) => agent.agentId);
  if (differs(idsA, idsB)) {
    out.push(`agents: [${idsA.join(', ')}] vs [${idsB.join(', ')}]`);
  } else {
    a.agents.forEach((agent, index) => {
      if (differs(agent, b.agents[index])) {
        out.push(`agent ${agent.agentId}: behaviour differs`);
      }
    });
  }
  return out;
}

/** Two whole runs agree on behaviour, transition by transition. */
export function runsEqual(
  left: readonly TransitionTrace[],
  right: readonly TransitionTrace[],
): boolean {
  return (
    left.length === right.length &&
    left.every((trace, index) => behaviourEquals(trace, right[index] as TransitionTrace))
  );
}
