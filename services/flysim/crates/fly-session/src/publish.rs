//! The publication boundary of `publishing-v1`: what leaves the session, and what a refusal is.
//!
//! This is the PUBLISH-01 slice. It is an *internal* boundary on the *same* bus: there is no
//! second transport, no gateway process, no codec and no show or tournament service. What it
//! adds is the separation the contract asks for:
//!
//! ```text
//! session.<id>.descriptor   retained latest   framework: what this composition is
//! session.<id>.snapshots    retained latest   framework: the values of one committed boundary
//! session.<id>.events       bounded           framework: scoped domain events, not a log
//! <app>.state               retained latest   application-owned, application-shaped
//! <app>.cues                bounded           application-owned, under a declared policy
//! session.<id>.query        RPC, read-only    the repair path for a missed descriptor
//! ```
//!
//! Three rules decide everything below.
//!
//! 1. **A publication outcome is named.** [`PublicationOutcome`] is `Accepted`,
//!    `RefusedByObserver` or `Faulted`; nothing is dropped, retried or defaulted silently.
//!    `bus-v1` section 6 says plainly that "bounded event subscriptions can reject publication;
//!    latest spectator subscriptions cannot hold a required session transaction indefinitely",
//!    so a refusal is a thing the contract expects and this module counts, not an error to
//!    swallow. Only `BACKPRESSURE` is an observer's refusal. A store quota, a lost router or an
//!    unreadable payload is the session's own fault and fails the epoch.
//! 2. **An observer never moves the world.** Publication happens after the committed boundary
//!    is established. A refusal changes no phase, takes no step and releases no handle, and a
//!    latest observation topic supersedes the refused value at the next boundary, so a slow or
//!    bounded observer costs its own delivery and one boundary of a stream whose contract is
//!    "latest" -- never a tick, never a stall. The repair path recovers the exact value.
//! 3. **Agent state and media are one statement.** [`check_publication`] refuses a snapshot
//!    whose media does not belong to the boundary its agent state belongs to, before anything
//!    is published, so "future agent state with old media" is a failure rather than a frame.
//!
//! What this module deliberately does **not** contain: the approved public v2 wire schemas and
//! the stage adapters that speak them. `implementation.md` sequences those after this slice and
//! together with each other, and inventing a public byte format here would be exactly the
//! unapproved contract that ordering exists to prevent.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the glob
// keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// How many events one bounded batch may hold before the oldest are explicitly dropped.
pub const EVENT_BATCH_DEPTH: usize = 64;

/// The in-flight credits a framework consumer takes on an observation topic.
pub const SPECTATOR_CREDITS: u32 = 2;

// ----------------------------------------------------------------------------------------------
// Policy

/// The delivery a topic is published under. Declared once, never inferred per message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// One replaceable value. A consumer that falls behind loses intermediate values and
    /// nothing else; that loss is the contract of the topic, not an accident of load.
    LatestValue,
    /// A bounded batch. Nothing is coalesced: when the batch is full the oldest entries are
    /// dropped by an explicit count that travels with the next accepted batch.
    BoundedBatch { depth: usize },
}

impl Delivery {
    /// Whether the router retains the last value for a late subscriber.
    ///
    /// A latest observation is retained, because a consumer that arrives mid-session must be
    /// able to start; a bounded event stream is not, because it is "not a durable log"
    /// (publishing-v1 section 2) and a retained tail would look like one.
    pub fn retained(self) -> flybus::Retained {
        match self {
            Delivery::LatestValue => flybus::Retained::Latest,
            Delivery::BoundedBatch { .. } => flybus::Retained::None,
        }
    }

    /// The subscription a consumer of this topic takes.
    pub fn subscription(self) -> flybus::SubscriptionConfig {
        match self {
            Delivery::LatestValue => flybus::SubscriptionConfig::latest()
                .in_flight(SPECTATOR_CREDITS)
                .replay(true),
            Delivery::BoundedBatch { depth } => {
                flybus::SubscriptionConfig::bounded().queued(depth as u32)
            }
        }
    }
}

/// One published address and the delivery it was declared under.
#[derive(Clone, Debug)]
pub struct TopicPolicy {
    pub topic: String,
    pub delivery: Delivery,
}

impl TopicPolicy {
    pub fn latest(topic: impl Into<String>) -> TopicPolicy {
        TopicPolicy {
            topic: topic.into(),
            delivery: Delivery::LatestValue,
        }
    }

    pub fn bounded(topic: impl Into<String>, depth: usize) -> TopicPolicy {
        TopicPolicy {
            topic: topic.into(),
            delivery: Delivery::BoundedBatch { depth },
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Outcomes

/// What became of one publication. Every path through [`Publisher`] returns one of these.
#[derive(Clone, Debug, PartialEq)]
pub enum PublicationOutcome {
    /// The router admitted it. `replaced` counts the queued values it coalesced away, which
    /// is the only loss a latest subscriber can suffer and is reported, not hidden.
    Accepted {
        topic: String,
        topic_sequence: u64,
        subscribers: u64,
        replaced: u64,
    },
    /// A subscriber refused it. `bus-v1` section 5 rejects the whole publish for a bounded
    /// subscriber's full queue, so this is an observer's doing: the world is untouched, the
    /// value is recoverable through the query service, and the offender is named.
    RefusedByObserver { topic: String, detail: String },
    /// The session's own resource or identity fault. This one fails the epoch.
    Faulted { topic: String, detail: String },
}

impl PublicationOutcome {
    pub fn topic(&self) -> &str {
        match self {
            PublicationOutcome::Accepted { topic, .. }
            | PublicationOutcome::RefusedByObserver { topic, .. }
            | PublicationOutcome::Faulted { topic, .. } => topic,
        }
    }

    pub fn is_accepted(&self) -> bool {
        matches!(self, PublicationOutcome::Accepted { .. })
    }

    pub fn is_refused(&self) -> bool {
        matches!(self, PublicationOutcome::RefusedByObserver { .. })
    }

    /// The domain error a *fault* carries, or `None` for an accepted or refused publication.
    ///
    /// The certainty is `none`: publication happens after the boundary is committed and
    /// mutates no participant, so a failed publish has changed nothing in the world.
    pub fn fault(&self) -> Option<DomainError> {
        match self {
            PublicationOutcome::Faulted { topic, detail } => Some(DomainError::new(
                ErrorCode::BackendFailure,
                format!("publishing {topic}: {detail}"),
                MutationCertainty::None,
            )),
            _ => None,
        }
    }

    fn from_bus(topic: &str, result: Result<flybus::PublishReceipt, flybus::BusError>) -> Self {
        match result {
            Ok(receipt) => PublicationOutcome::Accepted {
                topic: topic.to_owned(),
                topic_sequence: receipt.topic_sequence,
                subscribers: receipt.subscribers,
                replaced: receipt.replaced,
            },
            // The one code an observer can cause. Everything else is ours.
            Err(e) if e.code == flybus::ErrorCode::Backpressure => {
                PublicationOutcome::RefusedByObserver {
                    topic: topic.to_owned(),
                    detail: e.message,
                }
            }
            Err(e) => PublicationOutcome::Faulted {
                topic: topic.to_owned(),
                detail: format!("{:?}: {}", e.code, e.message),
            },
        }
    }
}

/// Per-topic publication counters, for assertions and for an operator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicCounters {
    pub accepted: u64,
    pub refused: u64,
    pub faulted: u64,
    /// Values a latest subscriber's queue coalesced away, as the router reported them.
    pub replaced: u64,
}

/// What this session published and what happened to it.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    topics: BTreeMap<String, TopicCounters>,
    /// Events dropped from the bounded batch because the batch was full, cumulative.
    pub events_dropped: u64,
    /// Batches held for a later boundary because an observer refused them.
    pub events_held: u64,
    last: Option<PublicationOutcome>,
}

impl Ledger {
    pub fn counters(&self, topic: &str) -> TopicCounters {
        self.topics.get(topic).cloned().unwrap_or_default()
    }

    pub fn last(&self) -> Option<&PublicationOutcome> {
        self.last.as_ref()
    }

    /// Every refusal this session has met, by topic.
    pub fn refusals(&self) -> u64 {
        self.topics.values().map(|c| c.refused).sum()
    }

    fn record(&mut self, outcome: &PublicationOutcome) {
        let entry = self.topics.entry(outcome.topic().to_owned()).or_default();
        match outcome {
            PublicationOutcome::Accepted { replaced, .. } => {
                entry.accepted += 1;
                entry.replaced += replaced;
            }
            PublicationOutcome::RefusedByObserver { .. } => entry.refused += 1,
            PublicationOutcome::Faulted { .. } => entry.faulted += 1,
        }
        self.last = Some(outcome.clone());
    }
}

// ----------------------------------------------------------------------------------------------
// Published state and the repair path

/// Everything the query service can answer from: the descriptor revisions this session has
/// published, and the latest committed snapshot.
///
/// "Latest retained descriptors accelerate startup; RPC querying remains the repair path"
/// (publishing-v1 section 2). A consumer that meets a revision it does not hold asks here
/// instead of guessing the shape of the values it is reading.
#[derive(Clone, Debug, Default)]
pub struct PublishedState {
    descriptors: BTreeMap<u64, SessionDescriptor>,
    latest: Option<CommittedSnapshot>,
}

impl PublishedState {
    pub fn descriptor(&self, revision: u64) -> Option<&SessionDescriptor> {
        self.descriptors.get(&revision)
    }

    pub fn newest_descriptor(&self) -> Option<&SessionDescriptor> {
        self.descriptors.values().next_back()
    }

    pub fn latest_snapshot(&self) -> Option<&CommittedSnapshot> {
        self.latest.as_ref()
    }

    pub fn revisions(&self) -> Vec<u64> {
        self.descriptors.keys().copied().collect()
    }
}

/// The shared handle the publisher writes and the query service reads.
pub type SharedState = Arc<Mutex<PublishedState>>;

fn lock(state: &SharedState) -> std::sync::MutexGuard<'_, PublishedState> {
    state
        .lock()
        .expect("the published state is never held across a panic")
}

// ----------------------------------------------------------------------------------------------
// The bounded event batch

/// The pending event batch of `publishing-v1` section 6: bounded, explicit about loss.
///
/// Events are "scoped domain events; not a durable log" (section 2), and "bus publish
/// acceptance and delivery consumption are not durable acknowledgments" (section 6). So this
/// keeps a bounded batch and says exactly what it could not keep: `droppedBefore` travels with
/// the next accepted batch, so a subscriber reads a count rather than inferring a gap. What it
/// never does is grow without bound, retry forever or forget quietly.
#[derive(Clone, Debug, Default)]
pub struct EventOutbox {
    depth: usize,
    pending: VecDeque<(u64, TaskEvent)>,
    dropped_since_accepted: u64,
}

impl EventOutbox {
    pub fn new(depth: usize) -> EventOutbox {
        EventOutbox {
            depth,
            pending: VecDeque::new(),
            dropped_since_accepted: 0,
        }
    }

    /// Adds this boundary's events, dropping the oldest if the batch is over its depth.
    /// Returns how many were dropped by this call.
    pub fn offer(&mut self, source_step: u64, events: &[TaskEvent]) -> u64 {
        for event in events {
            self.pending.push_back((source_step, event.clone()));
        }
        let mut dropped = 0;
        while self.pending.len() > self.depth {
            self.pending.pop_front();
            dropped += 1;
        }
        self.dropped_since_accepted += dropped;
        dropped
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn dropped_since_accepted(&self) -> u64 {
        self.dropped_since_accepted
    }

    fn payload(&self, session_id: &Id, epoch: &Id) -> Map<String, Value> {
        let events: Vec<Value> = self
            .pending
            .iter()
            .map(|(source_step, event)| {
                let mut value = event.to_json();
                if let Value::Object(map) = &mut value {
                    map.insert("sourceStep".to_owned(), source_step.to_string().into());
                }
                value
            })
            .collect();
        object(json!({
            "sessionId": session_id.as_str(),
            "epoch": epoch.as_str(),
            "droppedBefore": self.dropped_since_accepted.to_string(),
            "events": Value::Array(events),
        }))
    }

    fn accepted(&mut self) {
        self.pending.clear();
        self.dropped_since_accepted = 0;
    }
}

// ----------------------------------------------------------------------------------------------
// Coherence

/// Refuses a snapshot whose media does not belong to the boundary its agent state belongs to.
///
/// This is the "future agent state is never mixed with old media" rule, checked on the way out
/// and again on the way in. Four things must agree:
///
/// 1. Each published view's `producedStep` is exactly what its declared delay implies for this
///    boundary. A frame from an older boundary is a refusal, not a substitution.
/// 2. Each published audio chunk's sample range ends where the stream's next chunk begins, so
///    a chunk from a previous transition cannot ride along under a new boundary.
/// 3. Every referenced view and chunk has its attachment, and no attachment is present that
///    the payload does not reference. An extra handle is an old boundary's frame.
/// 4. The snapshot agrees with the descriptor it names: revision, agent set, port assignment,
///    rate roles.
pub fn check_publication(
    descriptor: &SessionDescriptor,
    snapshot: &CommittedSnapshot,
    attachments: &[(String, ArtifactRef)],
    audio_next_sample: &BTreeMap<String, u64>,
) -> DomainResult<()> {
    snapshot
        .validate_against(descriptor)
        .map_err(|e| coherence(format!("snapshot and descriptor disagree: {e}")))?;
    check_views(descriptor, snapshot)?;
    check_attachments(snapshot, attachments)?;
    for chunk in &snapshot.audio {
        let end = chunk
            .first_sample
            .checked_add(chunk.sample_frames)
            .ok_or_else(|| {
                coherence(format!(
                    "audio stream {} overflows its sample position",
                    chunk.stream_id
                ))
            })?;
        match audio_next_sample.get(&chunk.stream_id) {
            Some(next) if *next == end => {}
            Some(next) => {
                return Err(coherence(format!(
                    "audio stream {} covers samples {}..{end} and this boundary ends at {next}",
                    chunk.stream_id, chunk.first_sample
                )));
            }
            None => {
                return Err(coherence(format!(
                    "audio stream {} has no accepted position at this boundary",
                    chunk.stream_id
                )));
            }
        }
    }
    Ok(())
}

/// Every published view comes from exactly the boundary its declared delay implies.
pub fn check_views(
    descriptor: &SessionDescriptor,
    snapshot: &CommittedSnapshot,
) -> DomainResult<()> {
    let boundary = snapshot.scope.step;
    for view in &snapshot.views {
        let declared = descriptor
            .environment
            .views
            .iter()
            .find(|v| v.view_id == view.view_id)
            .ok_or_else(|| coherence(format!("view {} is not declared", view.view_id)))?;
        let want = declared.required_produced_step(boundary);
        if view.produced_step != want {
            return Err(coherence(format!(
                "view {} at boundary {boundary} was produced at {}, and its declared delay of {} requires {want}",
                view.view_id, view.produced_step, declared.observation_delay_steps
            )));
        }
    }
    Ok(())
}

/// The handles and the references are the same media: no missing frame, no extra one, and
/// each handle is the artifact its reference names.
///
/// The identity comparison is the half that matters: an attachment set that matches by *name*
/// while one handle is the previous boundary's object is exactly "old media under new agent
/// state", and only the `ArtifactRef` sees it.
pub fn check_attachments(
    snapshot: &CommittedSnapshot,
    attachments: &[(String, ArtifactRef)],
) -> DomainResult<()> {
    let mut want: Vec<(String, &ArtifactRef)> = snapshot
        .views
        .iter()
        .map(|v| (crate::media::view_attachment(&v.view_id), &v.pixels))
        .chain(
            snapshot
                .audio
                .iter()
                .map(|a| (crate::media::audio_attachment(&a.stream_id), &a.samples)),
        )
        .collect();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let mut given: Vec<(String, &ArtifactRef)> =
        attachments.iter().map(|(n, r)| (n.clone(), r)).collect();
    given.sort_by(|a, b| a.0.cmp(&b.0));
    if want.len() != given.len() || want.iter().zip(&given).any(|(w, g)| w.0 != g.0) {
        let want: Vec<&String> = want.iter().map(|(n, _)| n).collect();
        let given: Vec<&String> = given.iter().map(|(n, _)| n).collect();
        return Err(coherence(format!(
            "the published handles {given:?} are not the media the snapshot references {want:?}"
        )));
    }
    for ((name, reference), (_, handle)) in want.iter().zip(&given) {
        if reference != handle {
            return Err(coherence(format!(
                "the handle published as {name} is artifact {} generation {}, and the snapshot references {} generation {}",
                handle.artifact_id, handle.generation, reference.artifact_id, reference.generation
            )));
        }
    }
    Ok(())
}

fn coherence(message: impl std::fmt::Display) -> DomainError {
    // Nothing was published, so nothing downstream saw a mixed boundary.
    DomainError::new(ErrorCode::BufferInvalid, message, MutationCertainty::None)
}

// ----------------------------------------------------------------------------------------------
// The publisher

/// The session's publication path. One bus client, three framework topics, named outcomes.
pub struct Publisher {
    bus: flybus::Client,
    session_id: Id,
    epoch: Id,
    descriptor_topic: TopicPolicy,
    snapshot_topic: TopicPolicy,
    event_topic: TopicPolicy,
    /// The STATE-01 checkpoint stream. A stream of distinct facts, so it is a bounded
    /// delivery and never a latest value: a "committed" that replaced a "queued" would erase
    /// the distinction the durable commit rules are built on.
    checkpoint_topic: TopicPolicy,
    outbox: EventOutbox,
    state: SharedState,
    ledger: Ledger,
    sequence: u64,
}

impl Publisher {
    pub fn new(
        bus: flybus::Client,
        session_id: &Id,
        epoch: &Id,
        topics: &crate::coordinator::Topics,
    ) -> Publisher {
        Publisher {
            bus,
            session_id: session_id.clone(),
            epoch: epoch.clone(),
            descriptor_topic: TopicPolicy::latest(&topics.descriptor),
            snapshot_topic: TopicPolicy::latest(&topics.snapshots),
            event_topic: TopicPolicy::bounded(&topics.events, EVENT_BATCH_DEPTH),
            checkpoint_topic: TopicPolicy::bounded(&topics.checkpoints, EVENT_BATCH_DEPTH),
            outbox: EventOutbox::new(EVENT_BATCH_DEPTH),
            state: Arc::new(Mutex::new(PublishedState::default())),
            ledger: Ledger::default(),
            sequence: 0,
        }
    }

    /// The declared policies, in publication order.
    pub fn policies(&self) -> Vec<TopicPolicy> {
        vec![
            self.descriptor_topic.clone(),
            self.snapshot_topic.clone(),
            self.event_topic.clone(),
            self.checkpoint_topic.clone(),
        ]
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn state(&self) -> SharedState {
        Arc::clone(&self.state)
    }

    pub fn outbox(&self) -> &EventOutbox {
        &self.outbox
    }

    /// The next publication sequence, which is monotonic within this publisher incarnation.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn incarnation(&self) -> String {
        self.bus.info().connection_id.clone()
    }

    /// Declares every framework topic under its policy. A topic already declared compatibly
    /// is accepted; a conflicting declaration is a fault here, not a silent reuse.
    pub async fn declare(&self) -> DomainResult<()> {
        for policy in self.policies() {
            self.bus
                .declare_topic(&policy.topic, policy.delivery.retained())
                .await
                .map_err(|e| {
                    DomainError::new(
                        ErrorCode::BackendFailure,
                        format!("declaring {}: {}", policy.topic, e.message),
                        MutationCertainty::None,
                    )
                })?;
        }
        Ok(())
    }

    /// Publishes a descriptor revision and records it as answerable by the query service.
    ///
    /// The revision is recorded even when the publication is refused: the repair path exists
    /// exactly for the consumer that did not receive it.
    pub async fn publish_descriptor(
        &mut self,
        descriptor: &SessionDescriptor,
    ) -> DomainResult<PublicationOutcome> {
        descriptor.validate().map_err(DomainError::invalid)?;
        {
            let mut state = lock(&self.state);
            if let Some(previous) = state.descriptor(descriptor.revision)
                && previous != descriptor
            {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!(
                        "descriptor revision {} was already published with another composition",
                        descriptor.revision
                    ),
                ));
            }
            state
                .descriptors
                .insert(descriptor.revision, descriptor.clone());
        }
        let topic = self.descriptor_topic.topic.clone();
        let outcome = PublicationOutcome::from_bus(
            &topic,
            self.bus
                .publish(&topic, object(descriptor.to_json()), &[])
                .await,
        );
        self.ledger.record(&outcome);
        Ok(outcome)
    }

    /// Publishes one committed boundary with the media handles it references.
    ///
    /// The coherence check runs first, so a snapshot that mixes boundaries never reaches a
    /// subscriber; the sequence advances only on a publication the router accepted, so
    /// "monotonic within publisherIncarnation" counts published values and not attempts.
    pub async fn publish_snapshot(
        &mut self,
        descriptor: &SessionDescriptor,
        snapshot: &CommittedSnapshot,
        attachments: &[(String, flybus::Artifact)],
        audio_next_sample: &BTreeMap<String, u64>,
    ) -> DomainResult<PublicationOutcome> {
        let named: Vec<(String, ArtifactRef)> = attachments
            .iter()
            .map(|(name, artifact)| (name.clone(), artifact.reference().clone()))
            .collect();
        check_publication(descriptor, snapshot, &named, audio_next_sample)?;
        let refs: Vec<(&str, &flybus::Artifact)> =
            attachments.iter().map(|(n, a)| (n.as_str(), a)).collect();
        let topic = self.snapshot_topic.topic.clone();
        let outcome = PublicationOutcome::from_bus(
            &topic,
            self.bus
                .publish(&topic, object(snapshot.to_json()), &refs)
                .await,
        );
        if outcome.is_accepted() {
            self.sequence += 1;
            lock(&self.state).latest = Some(snapshot.clone());
        }
        self.ledger.record(&outcome);
        Ok(outcome)
    }

    /// Offers this boundary's events to the bounded batch and publishes what it holds.
    ///
    /// An empty batch publishes nothing and is not an outcome. A refused batch is held for the
    /// next boundary and counted; what the depth pushed out is counted too and travels with
    /// the next accepted batch as `droppedBefore`.
    pub async fn publish_events(
        &mut self,
        source_step: u64,
        events: &[TaskEvent],
    ) -> Option<PublicationOutcome> {
        let dropped = self.outbox.offer(source_step, events);
        self.ledger.events_dropped += dropped;
        if self.outbox.is_empty() {
            return None;
        }
        let payload = self.outbox.payload(&self.session_id, &self.epoch);
        let topic = self.event_topic.topic.clone();
        let outcome =
            PublicationOutcome::from_bus(&topic, self.bus.publish(&topic, payload, &[]).await);
        match &outcome {
            PublicationOutcome::Accepted { .. } => self.outbox.accepted(),
            PublicationOutcome::RefusedByObserver { .. } => self.ledger.events_held += 1,
            PublicationOutcome::Faulted { .. } => {}
        }
        self.ledger.record(&outcome);
        Some(outcome)
    }
}

impl Publisher {
    /// Publishes one checkpoint fact on the checkpoint stream.
    ///
    /// It goes through the same named outcomes as everything else: a durable-commit fact that
    /// an observer refuses is counted and does not fail the session, because the durable
    /// acknowledgment is the store's, not the subscriber's -- "bus publish acceptance and
    /// delivery consumption are not durable acknowledgments" (publishing-v1 section 6).
    pub async fn publish_checkpoint(&mut self, payload: Map<String, Value>) -> PublicationOutcome {
        let topic = self.checkpoint_topic.topic.clone();
        let outcome =
            PublicationOutcome::from_bus(&topic, self.bus.publish(&topic, payload, &[]).await);
        self.ledger.record(&outcome);
        outcome
    }
}

// ----------------------------------------------------------------------------------------------
// The query service: the repair path

/// The read-only service name a session answers descriptor queries on.
pub fn query_service(session_id: &Id) -> String {
    format!("session.{session_id}.query")
}

/// `Session.GetDescriptor`: one published revision, or the newest.
pub const GET_DESCRIPTOR: &str = "Session.GetDescriptor";
/// `Session.GetSnapshot`: the latest committed snapshot, without its media handles.
pub const GET_SNAPSHOT: &str = "Session.GetSnapshot";

/// A running query service.
///
/// It is the repair path of `publishing-v1` section 2 and nothing else: two read methods over
/// state the publisher already published. It takes no parameters that select a participant, it
/// mutates nothing, and it is not a controller API -- there is no method here that could
/// advance, pause, stimulate, restore or reconfigure anything.
pub struct QueryService {
    task: tokio::task::JoinHandle<()>,
}

impl QueryService {
    pub async fn start(
        client: flybus::Client,
        session_id: &Id,
        state: SharedState,
    ) -> Result<QueryService, flybus::BusError> {
        let name = query_service(session_id);
        let mut service = client
            .register(&name, flybus::ServiceConfig::default())
            .await?;
        let worker_id = session_id.clone();
        // The bus connection id names this publisher incarnation. It has to be an `Id` to
        // travel in a reply, and a connection id that is not one is a refusal here rather
        // than a fallback name that two incarnations could share.
        let incarnation = parse_id(&format!("query-{}", client.info().connection_id))
            .map_err(|e| flybus::BusError::new(flybus::ErrorCode::InvalidEnvelope, e))?;
        let task = tokio::spawn(async move {
            while let Some(request) = service.next().await {
                let method = request.method().to_owned();
                let responder = request.responder();
                let parsed =
                    SessionRpcRequest::from_json(&Value::Object(request.payload().clone()));
                drop(request);
                let outcome = match parsed {
                    Ok(parsed) => answer(&parsed, &method, &worker_id, &incarnation, &state),
                    Err(e) => failure_outcome(
                        &DomainRequestId::from_serial(0),
                        &worker_id,
                        &incarnation,
                        None,
                        DomainError::invalid(format!("{method}: {e}")),
                    ),
                };
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
            }
        });
        Ok(QueryService { task })
    }

    /// Ends the service. Dropping one does the same thing.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for QueryService {
    /// A dropped session leaves no task reading a service it no longer answers for.
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn answer(
    request: &SessionRpcRequest,
    method: &str,
    worker_id: &Id,
    incarnation: &Id,
    state: &SharedState,
) -> SessionRpcOutcome {
    let result = match method {
        GET_DESCRIPTOR => get_descriptor(request, state),
        GET_SNAPSHOT => get_snapshot(request, state),
        other => Err(DomainError::before(
            ErrorCode::Unsupported,
            format!("{other} is not a method of the session query service"),
        )),
    };
    match result {
        Ok(result) => success_outcome(
            &request.request_id,
            worker_id,
            incarnation,
            request.scope.clone(),
            result,
        ),
        Err(error) => failure_outcome(
            &request.request_id,
            worker_id,
            incarnation,
            request.scope.clone(),
            error,
        ),
    }
}

fn get_descriptor(
    request: &SessionRpcRequest,
    state: &SharedState,
) -> DomainResult<Map<String, Value>> {
    let wanted = match request.params.get("revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(
            text.parse::<u64>()
                .map_err(|_| DomainError::invalid("revision is a decimal U64 string"))?,
        ),
        Some(_) => return Err(DomainError::invalid("revision is a decimal U64 string")),
    };
    let state = lock(state);
    let descriptor = match wanted {
        Some(revision) => state.descriptor(revision).ok_or_else(|| {
            // A revision this session never published is an answer, not an empty result.
            DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "descriptor revision {revision} was never published; this session has {:?}",
                    state.revisions()
                ),
            )
        })?,
        None => state.newest_descriptor().ok_or_else(|| {
            DomainError::before(ErrorCode::InvalidPhase, "no descriptor has been published")
        })?,
    };
    Ok(object(descriptor.to_json()))
}

fn get_snapshot(
    request: &SessionRpcRequest,
    state: &SharedState,
) -> DomainResult<Map<String, Value>> {
    if !request.params.as_object().is_some_and(Map::is_empty) {
        return Err(DomainError::invalid(
            "Session.GetSnapshot takes no parameters",
        ));
    }
    let state = lock(state);
    let snapshot = state.latest_snapshot().ok_or_else(|| {
        DomainError::before(ErrorCode::InvalidPhase, "no snapshot has been published")
    })?;
    Ok(object(snapshot.to_json()))
}

// ----------------------------------------------------------------------------------------------
// Application-owned state and cues

/// An application's own publication channel: its schema, its topics, its delivery policy.
///
/// The framework supplies the channel and nothing about what travels on it. "Application state
/// carries whatever the experience needs ... It is developed with its presentation, not forced
/// into a framework-wide show state/tournament schema" (publishing-v1 section 4). So the topics
/// are named by the application, the values are `TypedValue`s under the application's own
/// namespaced schema, and this module never looks inside one. There is no director here, no
/// bracket, no cast and no game.
pub struct ApplicationChannel {
    bus: flybus::Client,
    state_topic: TopicPolicy,
    cue_topic: TopicPolicy,
    ledger: Ledger,
    revision: u64,
}

impl ApplicationChannel {
    /// `prefix` is the application's own address root, for example `app.counter`.
    pub fn new(bus: flybus::Client, prefix: &str, cue_depth: usize) -> ApplicationChannel {
        ApplicationChannel {
            bus,
            state_topic: TopicPolicy::latest(format!("{prefix}.state")),
            cue_topic: TopicPolicy::bounded(format!("{prefix}.cues"), cue_depth),
            ledger: Ledger::default(),
            revision: 0,
        }
    }

    pub fn state_topic(&self) -> &str {
        &self.state_topic.topic
    }

    pub fn cue_topic(&self) -> &str {
        &self.cue_topic.topic
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub async fn declare(&self) -> DomainResult<()> {
        for policy in [&self.state_topic, &self.cue_topic] {
            self.bus
                .declare_topic(&policy.topic, policy.delivery.retained())
                .await
                .map_err(|e| {
                    DomainError::new(
                        ErrorCode::BackendFailure,
                        format!("declaring {}: {}", policy.topic, e.message),
                        MutationCertainty::None,
                    )
                })?;
        }
        Ok(())
    }

    /// Publishes application state at a committed boundary it names.
    ///
    /// The boundary travels with it because a consumer combines this with the framework
    /// snapshot, and cross-topic ordering is not guaranteed: "a subscriber receiving an unknown
    /// descriptor revision must fetch it ... or buffer a bounded number of snapshots, not infer
    /// shape" (publishing-v1 section 2). A named boundary is how the two are joined.
    pub async fn publish_state(&mut self, boundary: u64, value: &TypedValue) -> PublicationOutcome {
        self.revision += 1;
        let payload = object(json!({
            "boundary": boundary.to_string(),
            "revision": self.revision.to_string(),
            "state": value.to_json(),
        }));
        let topic = self.state_topic.topic.clone();
        let outcome =
            PublicationOutcome::from_bus(&topic, self.bus.publish(&topic, payload, &[]).await);
        self.ledger.record(&outcome);
        outcome
    }

    /// Publishes a presentation cue. Cues are presentation data: "presentation cues may be
    /// immediate; simulation effects apply at declared boundaries" (section 7). Nothing here
    /// reaches a worker, a controller or the world.
    pub async fn publish_cue(
        &mut self,
        boundary: u64,
        kind: &Id,
        value: &TypedValue,
    ) -> PublicationOutcome {
        let payload = object(json!({
            "boundary": boundary.to_string(),
            "kind": kind.as_str(),
            "cue": value.to_json(),
        }));
        let topic = self.cue_topic.topic.clone();
        let outcome =
            PublicationOutcome::from_bus(&topic, self.bus.publish(&topic, payload, &[]).await);
        self.ledger.record(&outcome);
        outcome
    }
}

// ----------------------------------------------------------------------------------------------
// The fake multi-agent consumer

/// One agent's committed values as a consumer reads them.
///
/// A presentation consumer is a *multi-agent* consumer: one snapshot carries the whole
/// composition, so there is no per-fly stream to join and no "current fly" to be stale.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentView {
    pub agent_id: Id,
    /// The index the descriptor says this agent's rates and geometry belong to. A consumer
    /// that maps anything spatial compares this, not `neuronCount`.
    pub index_digest: Digest,
    pub brain_ticks: u64,
    pub rates: Vec<(Id, f64)>,
    /// The decision of the transition that *ended* at this boundary, null only at boundary 0.
    pub decision: Option<TypedValue>,
    pub controls: Option<PortControl>,
}

/// One committed boundary as a consumer reads it, with the media handles it still owns.
pub struct SnapshotView {
    pub boundary: u64,
    pub descriptor_revision: u64,
    pub sequence: u64,
    /// How many undelivered snapshots the router coalesced away before this one.
    pub replaced: u64,
    pub agents: Vec<AgentView>,
    pub views: Vec<ViewRef>,
    pub audio: Vec<AudioRef>,
    /// The extracted handles, held after the message is dropped.
    pub artifacts: BTreeMap<String, flybus::Artifact>,
}

impl SnapshotView {
    pub fn agent(&self, agent_id: &Id) -> Option<&AgentView> {
        self.agents.iter().find(|a| a.agent_id == *agent_id)
    }
}

/// One bounded event batch as a consumer reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct EventBatchView {
    pub epoch: Id,
    /// How many events the publisher's bounded batch dropped before this one. A count, never
    /// a gap the consumer has to infer.
    pub dropped_before: u64,
    pub event_ids: Vec<Id>,
}

/// What one poll of a consumer produced.
#[derive(Clone, Debug, PartialEq)]
pub enum ConsumerOutcome {
    /// A descriptor this consumer now holds, and the agents it describes.
    Composition { revision: u64, agents: Vec<Id> },
    /// A snapshot whose descriptor revision this consumer holds and which agrees with it.
    Read { boundary: u64, agents: Vec<Id> },
    /// A snapshot naming a descriptor revision this consumer has never seen. Nothing is
    /// inferred from it: the consumer repairs through the query service and reads it again.
    UnknownRevision { revision: u64 },
    /// The composition changed under an agent this consumer had already mapped: the same
    /// neuron count, another index. Visible, named, and never silently remapped.
    IndexChanged {
        agent_id: Id,
        from: Digest,
        to: Digest,
    },
    /// The snapshot does not agree with the descriptor it names, or its media does not belong
    /// to its boundary.
    Incoherent { detail: String },
}

/// A presentation-side consumer of one session, over the same bus.
///
/// It is a regular bus subscriber (publishing-v1 section 5): latest subscriptions with finite
/// credits on the observation topics, a bounded subscription on events, and one read-only RPC
/// for repair. It is deliberately *not* a gateway: it resolves no artifact into a browser
/// transport, encodes nothing and holds no private owner token on anyone's behalf.
pub struct PresentationConsumer {
    bus: flybus::Client,
    query: String,
    descriptors: flybus::Subscription,
    snapshots: flybus::Subscription,
    events: flybus::Subscription,
    held: Vec<flybus::Message>,
    cached: BTreeMap<u64, SessionDescriptor>,
    /// The index digest this consumer has mapped geometry against, per agent.
    mapped: BTreeMap<Id, Digest>,
    /// Where each audio stream's last accepted chunk ended.
    audio_end: BTreeMap<Id, u64>,
    last: Option<SnapshotView>,
    repairs: u64,
    serial: u64,
    coalesced: u64,
}

impl PresentationConsumer {
    pub async fn attach(
        bus: flybus::Client,
        session_id: &Id,
        topics: &crate::coordinator::Topics,
    ) -> Result<PresentationConsumer, flybus::BusError> {
        let descriptors = bus
            .subscribe(&topics.descriptor, Delivery::LatestValue.subscription())
            .await?;
        let snapshots = bus
            .subscribe(&topics.snapshots, Delivery::LatestValue.subscription())
            .await?;
        let events = bus
            .subscribe(
                &topics.events,
                Delivery::BoundedBatch {
                    depth: EVENT_BATCH_DEPTH,
                }
                .subscription(),
            )
            .await?;
        Ok(PresentationConsumer {
            bus,
            query: query_service(session_id),
            descriptors,
            snapshots,
            events,
            held: Vec::new(),
            cached: BTreeMap::new(),
            mapped: BTreeMap::new(),
            audio_end: BTreeMap::new(),
            last: None,
            repairs: 0,
            serial: 0,
            coalesced: 0,
        })
    }

    /// How many descriptor revisions this consumer holds.
    pub fn revisions(&self) -> Vec<u64> {
        self.cached.keys().copied().collect()
    }

    pub fn descriptor(&self, revision: u64) -> Option<&SessionDescriptor> {
        self.cached.get(&revision)
    }

    /// How many times this consumer had to ask the query service for a descriptor.
    pub fn repairs(&self) -> u64 {
        self.repairs
    }

    /// How many snapshots the router coalesced away in this consumer's own queue.
    pub fn coalesced(&self) -> u64 {
        self.coalesced
    }

    pub fn last(&self) -> Option<&SnapshotView> {
        self.last.as_ref()
    }

    /// Takes the next descriptor from the retained-latest topic.
    ///
    /// A composition that moves an agent this consumer has already mapped geometry against is
    /// reported here and *not* cached: remapping silently is the one thing a consumer holding
    /// a spatial mapping must not do. (publishing-v1 section 3)
    pub async fn take_descriptor(&mut self) -> Option<ConsumerOutcome> {
        let message = self.descriptors.next().await?;
        let descriptor =
            match SessionDescriptor::from_json(&Value::Object(message.payload().clone())) {
                Ok(descriptor) => descriptor,
                Err(e) => {
                    return Some(ConsumerOutcome::Incoherent {
                        detail: e.to_string(),
                    });
                }
            };
        drop(message);
        for agent in &descriptor.agents {
            if let Some(mapped) = self.mapped.get(&agent.agent_id)
                && *mapped != agent.index_digest
            {
                return Some(ConsumerOutcome::IndexChanged {
                    agent_id: agent.agent_id.clone(),
                    from: mapped.clone(),
                    to: agent.index_digest.clone(),
                });
            }
        }
        let revision = descriptor.revision;
        let agents = descriptor
            .agents
            .iter()
            .map(|a| a.agent_id.clone())
            .collect();
        self.cached.insert(revision, descriptor);
        Some(ConsumerOutcome::Composition { revision, agents })
    }

    /// Asks the query service for a revision this consumer does not hold.
    ///
    /// This is the repair path, and it is an ordinary RPC on the same bus. A revision the
    /// session never published comes back as a named error rather than an empty answer.
    pub async fn repair(&mut self, revision: u64) -> DomainResult<u64> {
        self.serial += 1;
        self.repairs += 1;
        let request = SessionRpcRequest {
            request_id: DomainRequestId::from_serial(self.serial),
            scope: None,
            params: json!({ "revision": revision.to_string() }),
        };
        let mut pending = self
            .bus
            .call(
                &self.query,
                None,
                GET_DESCRIPTOR,
                object(request.to_json()),
                &[],
            )
            .await
            .map_err(|e| {
                DomainError::new(
                    ErrorCode::BackendFailure,
                    e.message,
                    MutationCertainty::None,
                )
            })?;
        let result = pending.result().await.map_err(|e| {
            DomainError::new(
                ErrorCode::BackendFailure,
                e.message,
                MutationCertainty::None,
            )
        })?;
        let outcome = SessionRpcOutcome::from_json(&Value::Object(result.outcome().clone()))
            .map_err(DomainError::invalid)?;
        drop(result);
        let value = outcome_result(&outcome)?;
        let descriptor = SessionDescriptor::from_json(value).map_err(DomainError::invalid)?;
        if descriptor.revision != revision {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the query service answered with another revision",
            ));
        }
        self.cached.insert(revision, descriptor);
        Ok(revision)
    }

    /// Takes the next committed snapshot and reads it against the descriptor it names.
    pub async fn take_snapshot(&mut self) -> Option<ConsumerOutcome> {
        let message = self.snapshots.next().await?;
        self.coalesced += message.replaced();
        Some(self.read(message))
    }

    /// Takes a snapshot without reading or releasing it, which is what a viewer that stopped
    /// rendering does. Its credits run out; nothing else in the session notices.
    pub async fn hold_snapshot(&mut self) -> bool {
        match self.snapshots.next().await {
            Some(message) => {
                self.coalesced += message.replaced();
                self.held.push(message);
                true
            }
            None => false,
        }
    }

    /// Holds a snapshot if one is queued right now, and answers `false` if none is.
    ///
    /// The bounded form: a test that means "take whatever is there" must not be able to
    /// block on a stream that is deliberately not producing.
    pub fn try_hold_snapshot(&mut self) -> bool {
        match self.snapshots.try_next() {
            Some(message) => {
                self.coalesced += message.replaced();
                self.held.push(message);
                true
            }
            None => false,
        }
    }

    /// Reads a snapshot if one is queued right now.
    pub fn try_take_snapshot(&mut self) -> Option<ConsumerOutcome> {
        let message = self.snapshots.try_next()?;
        self.coalesced += message.replaced();
        Some(self.read(message))
    }

    /// Releases everything this consumer was holding, returning its credits.
    pub fn release(&mut self) {
        self.held.clear();
    }

    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Takes the next bounded event batch.
    pub async fn take_events(&mut self) -> Option<EventBatchView> {
        let message = self.events.next().await?;
        let payload = message.payload().clone();
        drop(message);
        let epoch = payload.get("epoch").and_then(Value::as_str).map(id)?;
        let dropped_before = payload
            .get("droppedBefore")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<u64>().ok())?;
        let event_ids = payload
            .get("events")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        Some(EventBatchView {
            epoch,
            dropped_before,
            event_ids,
        })
    }

    /// Records that this consumer has mapped geometry against the agents of `revision`.
    ///
    /// A consumer that never calls this never claims a mapping, and a later index change is
    /// simply a new descriptor. One that does claim it gets [`ConsumerOutcome::IndexChanged`]
    /// when the composition moves under it.
    pub fn map_geometry(&mut self, revision: u64) -> DomainResult<()> {
        let descriptor = self.cached.get(&revision).ok_or_else(|| {
            DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("revision {revision} is not held by this consumer"),
            )
        })?;
        for agent in &descriptor.agents {
            self.mapped
                .insert(agent.agent_id.clone(), agent.index_digest.clone());
        }
        Ok(())
    }

    pub fn mapped_index(&self, agent_id: &Id) -> Option<&Digest> {
        self.mapped.get(agent_id)
    }

    fn read(&mut self, message: flybus::Message) -> ConsumerOutcome {
        let sequence = message.topic_sequence();
        let replaced = message.replaced();
        let snapshot = match CommittedSnapshot::from_json(&Value::Object(message.payload().clone()))
        {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return ConsumerOutcome::Incoherent {
                    detail: e.to_string(),
                };
            }
        };
        let Some(descriptor) = self.cached.get(&snapshot.descriptor_revision).cloned() else {
            // Not an error and not a guess: the consumer buffers nothing and infers nothing,
            // it repairs. (publishing-v1 section 2)
            return ConsumerOutcome::UnknownRevision {
                revision: snapshot.descriptor_revision,
            };
        };
        for agent in &descriptor.agents {
            if let Some(mapped) = self.mapped.get(&agent.agent_id)
                && *mapped != agent.index_digest
            {
                return ConsumerOutcome::IndexChanged {
                    agent_id: agent.agent_id.clone(),
                    from: mapped.clone(),
                    to: agent.index_digest.clone(),
                };
            }
        }
        let names: Vec<String> = message.attachment_names().map(str::to_owned).collect();
        let mut handles = Vec::new();
        for name in &names {
            match message.artifact(name) {
                Ok(artifact) => handles.push((name.clone(), artifact.reference().clone())),
                Err(e) => {
                    return ConsumerOutcome::Incoherent {
                        detail: format!("attachment {name}: {}", e.message),
                    };
                }
            }
        }
        if let Err(e) = check_received(&descriptor, &snapshot, &self.audio_end, &handles) {
            return ConsumerOutcome::Incoherent {
                detail: e.to_string(),
            };
        }
        // The renderer keeps its handles after the delivery is gone; the bytes stay alive
        // because the extracted handle owns them. (publishing-v1 section 5)
        let mut artifacts = BTreeMap::new();
        for name in &names {
            match message.artifact(name) {
                Ok(artifact) => {
                    artifacts.insert(name.clone(), artifact);
                }
                Err(e) => {
                    return ConsumerOutcome::Incoherent {
                        detail: format!("attachment {name}: {}", e.message),
                    };
                }
            }
        }
        drop(message);
        for chunk in &snapshot.audio {
            self.audio_end.insert(
                chunk.stream_id.clone(),
                chunk.first_sample + chunk.sample_frames,
            );
        }
        let agents: Vec<AgentView> = snapshot
            .agents
            .iter()
            .map(|agent| {
                let declared = descriptor
                    .agents
                    .iter()
                    .find(|a| a.agent_id == agent.agent_id)
                    .expect("validate_against checked the agent set");
                AgentView {
                    agent_id: agent.agent_id.clone(),
                    index_digest: declared.index_digest.clone(),
                    brain_ticks: agent.telemetry.brain_ticks,
                    rates: agent
                        .telemetry
                        .rates
                        .iter()
                        .map(|r| (r.role_id.clone(), r.hz))
                        .collect(),
                    decision: agent.selected_decision.clone(),
                    controls: agent.applied_controls.clone(),
                }
            })
            .collect();
        let outcome = ConsumerOutcome::Read {
            boundary: snapshot.scope.step,
            agents: agents.iter().map(|a| a.agent_id.clone()).collect(),
        };
        self.last = Some(SnapshotView {
            boundary: snapshot.scope.step,
            descriptor_revision: snapshot.descriptor_revision,
            sequence,
            replaced,
            agents,
            views: snapshot.views.clone(),
            audio: snapshot.audio.clone(),
            artifacts,
        });
        outcome
    }
}

/// The receiving half of the coherence rule.
///
/// A consumer does not trust that the publisher checked: it checks the same statement from the
/// other side, against the audio position it last accepted rather than the one the publisher
/// holds. That is what catches a chunk from an earlier transition arriving under a later
/// boundary, which is the one shape of "old media" a publisher-side check cannot see.
pub fn check_received(
    descriptor: &SessionDescriptor,
    snapshot: &CommittedSnapshot,
    audio_end: &BTreeMap<Id, u64>,
    attachments: &[(String, ArtifactRef)],
) -> DomainResult<()> {
    snapshot
        .validate_against(descriptor)
        .map_err(|e| coherence(format!("snapshot and descriptor disagree: {e}")))?;
    check_views(descriptor, snapshot)?;
    check_attachments(snapshot, attachments)?;
    for chunk in &snapshot.audio {
        match audio_end.get(&chunk.stream_id) {
            // A stream this consumer has not heard yet, or one that says it skipped: both are
            // declared states, not assumptions.
            None => {}
            Some(_) if chunk.discontinuity => {}
            // Forward is the latest subscription's own contract: a consumer that fell behind
            // was told so by `replaced`, and the boundaries in between are values it chose
            // not to receive. Backwards or overlapping is old media under a new boundary,
            // which no delivery policy explains.
            Some(end) if chunk.first_sample >= *end => {}
            Some(end) => {
                return Err(coherence(format!(
                    "audio stream {} starts at {} and the last chunk this consumer read ended at {end}",
                    chunk.stream_id, chunk.first_sample
                )));
            }
        }
    }
    Ok(())
}
