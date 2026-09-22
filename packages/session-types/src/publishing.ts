/** The publication types of publishing-v1 section 3. */
import { fail } from './canonical';
import { readRational, readScope, readSchemaRef, readTypedValue, readNullableTypedValue } from './common';
import {
  type AudioRef,
  MAX_VIEWS,
  type ViewRef,
  readAudioList,
  readViewList,
} from './media';
import { Reader, requireUnique, u64 } from './reader';
import { type Digest, type Id, type RationalNs, type SchemaRef, type Scope, type TypedValue, type U64 } from './scalar';
import {
  type AgentTelemetry,
  type AssetRef,
  type EnvironmentDescriptor,
  MAX_AGENTS,
  MAX_RATE_ROLES,
  MAX_SUPPORTED_STIMULI,
  type PortControl,
  findPort,
  readAgentTelemetry,
  readAssetRef,
  readEnvironmentDescriptor,
  readPortControl,
  validatePortControlAgainst,
  validateTelemetryRoles,
} from './workers';

export { MAX_SUPPORTED_STIMULI } from './workers';

/** Not stated by a document; this crate's choices, published in the schema set. */
export const MAX_ASSETS = 64;
export const MAX_SNAPSHOT_EVENTS = 64;

export interface AgentDescriptor {
  agentId: Id;
  portId: Id;
  profileDigest: Digest;
  datasetDigest: Digest;
  indexDigest: Digest;
  neuronCount: U64;
  rateRoles: Id[];
  supportedStimuli: Id[];
}

export interface SessionDescriptor {
  sessionId: Id;
  revision: U64;
  compositionDigest: Digest;
  schedulerId: 'lockstep-v1';
  environment: EnvironmentDescriptor;
  taskSchema: SchemaRef;
  agents: AgentDescriptor[];
  assets: AssetRef[];
}

export interface SnapshotAgent {
  agentId: Id;
  telemetry: AgentTelemetry;
  selectedDecision: TypedValue | null;
  appliedControls: PortControl | null;
}

export interface CommittedSnapshot {
  descriptorRevision: U64;
  publisherIncarnation: Id;
  scope: Scope;
  episodeId: Id;
  sequence: U64;
  worldTime: RationalNs;
  agents: SnapshotAgent[];
  progress: TypedValue;
  media: { views: ViewRef[]; audio: AudioRef[] };
  eventIds: Id[];
}

export function readSessionDescriptor(value: unknown): SessionDescriptor {
  const reader = new Reader(value, 'SessionDescriptor');
  const descriptor: SessionDescriptor = {
    sessionId: reader.id('sessionId'),
    revision: reader.u64('revision'),
    compositionDigest: reader.digest('compositionDigest'),
    schedulerId: reader.constant('schedulerId', 'lockstep-v1'),
    environment: readEnvironmentDescriptor(reader.value('environment')),
    taskSchema: readSchemaRef(reader.value('taskSchema')),
    agents: reader.list('agents', 1, MAX_AGENTS, (item) => {
      const agent = new Reader(item, 'SessionDescriptor.agents');
      const entry: AgentDescriptor = {
        agentId: agent.id('agentId'),
        portId: agent.id('portId'),
        profileDigest: agent.digest('profileDigest'),
        datasetDigest: agent.digest('datasetDigest'),
        indexDigest: agent.digest('indexDigest'),
        neuronCount: agent.u64('neuronCount'),
        rateRoles: agent.idList('rateRoles', 0, MAX_RATE_ROLES),
        supportedStimuli: agent.idList('supportedStimuli', 0, MAX_SUPPORTED_STIMULI),
      };
      agent.finish();
      requireUnique(entry.rateRoles, 'SessionDescriptor.agents rateRoles');
      requireUnique(entry.supportedStimuli, 'SessionDescriptor.agents supportedStimuli');
      return entry;
    }),
    assets: reader.list('assets', 0, MAX_ASSETS, readAssetRef),
  };
  reader.finish();
  requireUnique(
    descriptor.agents.map((agent) => agent.agentId),
    'SessionDescriptor.agents agentId',
  );
  requireUnique(
    descriptor.agents.map((agent) => agent.portId),
    'SessionDescriptor.agents portId',
  );
  requireUnique(
    descriptor.assets.map((asset) => asset.id),
    'SessionDescriptor.assets',
  );
  for (const agent of descriptor.agents) {
    if (!findPort(descriptor.environment, agent.portId)) {
      fail(
        `SessionDescriptor: agent "${agent.agentId}" is bound to port "${agent.portId}", which the environment does not declare`,
      );
    }
  }
  return descriptor;
}

export function readCommittedSnapshot(value: unknown): CommittedSnapshot {
  const reader = new Reader(value, 'CommittedSnapshot');
  const descriptorRevision = reader.u64('descriptorRevision');
  const publisherIncarnation = reader.id('publisherIncarnation');
  const scope = readScope(reader.value('scope'));
  const episodeId = reader.id('episodeId');
  const sequence = reader.u64('sequence');
  const worldTime = readRational(reader.value('worldTime'));
  const agents = reader.list('agents', 1, MAX_AGENTS, (item) => {
    const agent = new Reader(item, 'CommittedSnapshot.agents');
    const controls = agent.value('appliedControls');
    const entry: SnapshotAgent = {
      agentId: agent.id('agentId'),
      telemetry: readAgentTelemetry(agent.value('telemetry')),
      selectedDecision: readNullableTypedValue(agent.value('selectedDecision')),
      appliedControls: controls === null ? null : readPortControl(controls),
    };
    agent.finish();
    return entry;
  });
  const progress = readTypedValue(reader.value('progress'));
  const mediaReader = new Reader(reader.value('media'), 'CommittedSnapshot.media');
  const media = {
    views: readViewList(mediaReader, 'views'),
    audio: readAudioList(mediaReader, 'audio'),
  };
  mediaReader.finish();
  const eventIds = reader.idList('eventIds', 0, MAX_SNAPSHOT_EVENTS);
  reader.finish();
  requireUnique(
    agents.map((agent) => agent.agentId),
    'CommittedSnapshot.agents',
  );
  requireUnique(
    media.views.map((view) => view.viewId),
    'CommittedSnapshot.media.views',
  );
  requireUnique(eventIds, 'CommittedSnapshot.eventIds');
  if (media.views.length > MAX_VIEWS) fail('CommittedSnapshot: at most 8 views');
  const atBoundaryZero = u64(scope.step) === 0n;
  for (const agent of agents) {
    // "Decisions/controls describe the transition ending at that boundary, null at initial
    // boundary 0." (publishing-v1 section 3, and its 2026-09-22 amendment for a boundary that
    // was installed rather than produced.)
    if (atBoundaryZero && (agent.selectedDecision !== null || agent.appliedControls !== null)) {
      fail('CommittedSnapshot: at boundary 0 selectedDecision and appliedControls are null');
    }
    if ((agent.selectedDecision === null) !== (agent.appliedControls === null)) {
      fail(
        'CommittedSnapshot: selectedDecision and appliedControls are null together or present together',
      );
    }
  }
  // A boundary is produced by a transition or installed by one, and the whole snapshot says
  // which: every agent carries the transition that ended here, or none does.
  if (agents.some((a) => (a.selectedDecision === null) !== (agents[0].selectedDecision === null))) {
    fail(
      'CommittedSnapshot: either every agent carries the transition that ended here, or none does',
    );
  }
  return {
    descriptorRevision,
    publisherIncarnation,
    scope,
    episodeId,
    sequence,
    worldTime,
    agents,
    progress,
    media,
    eventIds,
  };
}

/** Descriptor agreement: revision, session, agent set and each agent's assigned port. */
export function validateSnapshotAgainst(
  snapshot: CommittedSnapshot,
  descriptor: SessionDescriptor,
): void {
  if (snapshot.descriptorRevision !== descriptor.revision) {
    fail('CommittedSnapshot: descriptorRevision does not match the descriptor');
  }
  if (snapshot.scope.sessionId !== descriptor.sessionId) {
    fail('CommittedSnapshot: sessionId does not match the descriptor');
  }
  for (const agent of snapshot.agents) {
    const declared = descriptor.agents.find((candidate) => candidate.agentId === agent.agentId);
    if (!declared) {
      fail(`CommittedSnapshot: agent "${agent.agentId}" is not in the descriptor`);
    }
    validateTelemetryRoles(agent.telemetry, declared.rateRoles);
    if (agent.appliedControls !== null) {
      if (agent.appliedControls.portId !== declared.portId) {
        fail(
          `CommittedSnapshot: agent "${agent.agentId}" controls port "${agent.appliedControls.portId}", not its assigned "${declared.portId}"`,
        );
      }
      const port = findPort(descriptor.environment, declared.portId);
      if (!port) fail('CommittedSnapshot: assigned port is not declared');
      validatePortControlAgainst(agent.appliedControls, port.controls);
    }
  }
}
