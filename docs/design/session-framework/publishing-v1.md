# Application, presentation and audience contracts

Status: **draft 2**, 2026-09-18. All internal communication uses [Flybus](bus-v1.md). This
document defines ownership and logical data contracts, not the public feed v2 byte format.
Existing public v1 contracts remain unchanged for the legacy application.

## 1. Applications orchestrate; presentation owns the show

Fly Plays Pokémon is an application assembled from simulation and presentation components.
Its supervisory code and interface share an application-owned, versioned state/event schema.
A Melee competition or ecosystem can choose another schema. Director, tournament, bracket,
cast of 32 personas and story segments are examples, not mandatory framework services/types.

The framework supplies sessions, agents, backend/task interfaces, capability descriptions,
native observations and generic UI/client primitives. The application chooses lifecycle,
profiles, game-aware macros/recovery, identities/history, interventions and narrative behavior.
Frame-by-frame scheduling remains inside the session; the application need not RPC each tick.

Presentation owns selection/layout, resizing, overlays, compositing, audio mixing, encoding,
browser-facing delivery, recording choices and stream output. Native 480p game data is a
perfectly valid framework output. Neither the router nor generic session assumes Twitch,
1920×1080, particular colors, specific React components or an automatic tournament dashboard.

## 2. One bus, complementary data sources

```text
Session ── committed observations/events + artifact handles ──┐
                                                           ├─ Flybus ── presentation application
Application ── own state/events/presentation cues ────────────┘
Presentation gateway ── application-defined browser delivery ── frontend
```

Example addresses (chosen by composition, not recognized by router code):

| Address | Pattern / purpose |
| --- | --- |
| `session.demo` | RPC: domain lifecycle/status/capabilities; exact methods require session API schemas |
| `session.demo.descriptor` | Pub/sub, retained latest: framework descriptor |
| `session.demo.snapshots` | Pub/sub, latest: committed simulation values and native media refs |
| `session.demo.events` | Pub/sub, bounded: scoped domain events; not a durable log |
| `app.pokemon.state` | Pub/sub, retained latest: application-specific state |
| `app.pokemon.cues` | Pub/sub: application narrative/presentation events under declared delivery policy |
| `app.pokemon` | RPC: application queries/admission, e.g. restore UI state or request a supported effect |

Descriptor revisions and scope link observations to schemas. Cross-topic ordering is not
guaranteed; a subscriber receiving an unknown descriptor revision must fetch it through the
application/session query contract or buffer a bounded number of snapshots, not infer shape.
Latest retained descriptors accelerate startup; RPC querying remains the repair path.

## 3. Common simulation descriptors and committed values

Types use [session RPC](ipc-v1.md), [workers](workers-v1.md), [state/media](state-media-v1.md):

```ts
interface SessionDescriptor {
  sessionId: Id; revision: U64; compositionDigest: Digest;
  schedulerId: "lockstep-v1";
  environment: EnvironmentDescriptor; taskSchema: SchemaRef;
  agents: {
    agentId: Id; portId: Id; profileDigest: Digest;
    datasetDigest: Digest; indexDigest: Digest; neuronCount: U64;
    rateRoles: Id[]; supportedStimuli: Id[];
  }[];
  assets: AssetRef[];
}
interface CommittedSnapshot {
  descriptorRevision: U64; publisherIncarnation: Id;
  scope: Scope; episodeId: Id; sequence: U64; worldTime: RationalNs;
  agents: {
    agentId: Id; telemetry: AgentTelemetry;
    selectedDecision: TypedValue | null;
    appliedControls: PortControl | null;
  }[];
  progress: TypedValue;
  media: { views: ViewRef[]; audio: AudioRef[] };
  eventIds: Id[];
}
```

Publish only after all agent commits establish Ready(k). Decisions/controls describe the
transition ending at that boundary, null at initial boundary 0. Health updates are separate
and never claim an uncommitted future boundary. Every transient media reference is a declared
bus attachment held through publication admission. Ordinary snapshot publication is latest/
bounded and never waits for a spectator to consume it.

Publication sequence is monotonic within publisherIncarnation. Epoch determines simulation
timeline; router topicSequence only determines bus acceptance order. Never equate these.
Geometry/spike mapping requires indexDigest, not merely the same number of neurons. Persistent
AssetRefs survive release packaging; ephemeral ArtifactRefs never become permanent asset URLs.

## 4. Flexible data, authored UI

Common UI primitives should understand media, agents, controllers, typed measurements,
progressions, collections and events. Descriptors change infrequently; values change frequently.
Application-specific structures remain namespaced, schema-validated extensions, such as
`pokemon.progress.v1` or `melee.match.v1`. They are not mandatory fields on every snapshot.

Proposed measurement vocabulary to formalize with public v2 schemas:

```text
Definition: id, owner, label, kind, unit, optional range, schema revision
Sample: id, producing scope/time, validity, value
Validity: measured | unknown | unsupported | stale
```

Kinds include number/counter, gauge, duration, state, progression and collection with typed
item schemas. A measured zero is distinct from unknown. Stale values retain original timestamps.
Units/ranges are metadata, not pixel sizes. Unknown optional extensions may be omitted or shown
generically; unsupported required schemas are visible errors. No remote executable UI payloads.

Application state carries whatever the experience needs: progress history, featured fly,
competition records, season state or sponsor effects. It is developed with its presentation,
not forced into a framework-wide “show state”/tournament schema. An application can reuse
generic components and add its own panels without changing worker/transport contracts.

## 5. Artifact consumption and browser boundary

The native presentation client is a regular bus subscriber. Its renderer may hold an extracted
Artifact after dropping the message; the SDK delays consumption until actual use finishes.
Latest coalescing only drops queued values. A stalled consumer is constrained by finite credits,
owners and store budgets; it cannot make the router overwrite an in-use image.

The browser does not receive private owner tokens or local storage paths. A presentation
gateway resolves/copies/encodes artifacts into its chosen browser transport and then drops
its bus handles. That is an application-edge adapter, not a second framework communications
stack. Compositor/encoder/recorder processes inside the application can exchange their own
artifacts through the same bus when useful.

Dense spike publication is optional and identifies agent, index digest and covered ticks.
The runtime need not publish every neuron every millisecond. Required sensory data and
optional spectator data have distinct budgets; UI focus never changes an agent's input,
controller assignment or an already resolved stimulation/effect target.

## 6. Events, persistence and recovery visibility

Events identify session/epoch, source boundary, episode, optional agent, kind and typed payload.
Task events are emitted after committed transitions; capture/admission events describe their
actual phase. Bus publish acceptance and delivery consumption are not durable acknowledgments.

When durability is required, call a configured event-store client/service (over the same bus)
and await its append/commit acknowledgment under the session's configured policy. Pub/sub
remains useful for live observers; reconnecting clients query durable history through ordinary
RPCs. The initial conformance policy pauses at the next safe boundary if durable event
admission/commit fails, retaining only a bounded pending batch. No hidden durable broker queue.

After rollback publish old/new epochs, checkpoint identity and abandoned step ranges.
Application history can mark outcomes aborted/superseded; it does not erase records merely
because emulator time moved backward. Media reports the corresponding discontinuity.

## 7. Supervisory and audience effects

Application supervision uses bus RPCs for configured lifecycle/intervention capabilities
and pub/sub for application state/cues. The Twitch adapter can be a constrained bus client
of the application's admission service; viewers/browser clients never obtain worker control.
Legacy HTTP bridge behavior remains until deliberately migrated.

Effects are declared by the task/backend/profile: valid targets, parameter schema, timing,
duration/stacking, implementation capability and outcome events. Examples include neural
stimulation or future game items/modifiers, where verified implementations exist. Generic
game boons and a public v2 admission schema are follow-on work; initially new-session audience
effects remain disabled. No arbitrary controller/game-memory write endpoint is introduced.

```text
requested → rejected
          → accepted(target, epoch, earliestStep) → scheduled → applied → expired
                                                └─ failed / cancelled
                                                    applied → rolled-back
```

The application persists interaction identity/target and defines retry, redemption, refund
and recovery policy. A gift is an intervention, not automatically an earned neural reward.
Presentation cues may be immediate; simulation effects apply at declared boundaries. The
supervisor does not bypass the complete-batch step barrier. Chat text remains presentation
data; template-only replies/quiet mode and existing no-public-button rules continue.

## 8. Presentation acceptance criteria

- Per-agent/session stores and rate/afterglow state, not one mutable global fly.
- Framework plus application schema streams, with descriptor repair/reconnect behavior.
- Native view dimensions and aspect; application-controlled output resolution and composition.
- Explicit audio ownership, timestamped overlays and bounded queues/discontinuities.
- Last-use artifact release, cached/replay-safe references and slow-observer isolation.
- Game-specific labels/views without Pokémon fields in generic runtime/router schemas.
- Actual UI changes reviewed as PNGs with browser/legibility gates. This contract is not screen approval.
