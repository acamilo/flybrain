//! The router's state machine: every routing, queueing and ownership decision, with no I/O.
//!
//! One `State` sits behind one mutex. Connection tasks call in with a decoded command, get
//! their reply queued on the connection's control lane, and do any file I/O (staging
//! creation, seal copies, unlinks) only after the lock is released. Writers pull their next
//! frame from here, so a delivery is marked dispatched and assigned its delivery id at the
//! moment its bytes are about to be written, never earlier.
//!
//! Ownership: every sealed artifact has a root count. A root is held by each queued delivery,
//! each delivery handed to a client and not yet consumed, each explicit hold and each retained
//! topic value that references it; a delivery naming the same artifact twice holds one root.
//! The last root to go collects the artifact: its bytes leave the quota and its file is
//! unlinked. Staging writers are not roots; an unsealed artifact is owned by its writer alone.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::{Notify, watch};

use crate::error::{BusError, Dispatch, ErrorCode};
use crate::limits::Limits;
use crate::policy::{Grants, Policy};
use crate::store::{sealed_rel, staging_rel};
use crate::wire::{
    ArtifactRef, Attachment, Envelope, Fields, GENERATION, Identity, Kind, MAJOR, MAX_BATCH,
    MAX_CREDIT, MAX_ENVELOPE_BYTES, MINOR, WireError, id_batch, parse_serial_id, serial_id,
};

pub(crate) type ConnKey = u64;
type CallKey = u64;

/// After this many consecutive control or RPC frames while topic data waits, one topic frame
/// goes out, so telemetry is deprioritised but never starved outright.
const TOPIC_STARVATION_LIMIT: u32 = 16;
/// Placeholder that is as long as any router-issued serial id can be.
const MAX_SERIAL: u64 = u64::MAX;

/// Signals between a connection's tasks and the state.
pub(crate) struct ConnSignals {
    /// Wakes the writer: something may be ready to send.
    pub wake: Notify,
    /// Flips to true when the connection is closed; both tasks watch it.
    pub shutdown: watch::Sender<bool>,
    /// Orders connection teardown against the synchronous transport polls of the writer.
    pub write_gate: WriteGate,
    /// The last frames to write before closing: a refusal or `connection.closing` notice,
    /// preceded on router shutdown by `subscription.closed` notices.
    pub final_frames: std::sync::Mutex<Vec<Vec<u8>>>,
}

/// Orders teardown against the writer's synchronous transport polls.
///
/// Teardown marks the stream closing *before* it waits for a poll already in progress, so at
/// most that one poll can still write and every later one is refused, whichever task reaches
/// the lock first. The earlier design held one mutex across each poll instead, which a writer
/// sending a frame a byte per poll re-acquired hundreds of times while teardown waited for it:
/// teardown could be starved for a whole frame and the frame completed just before its
/// delivery owner was reclaimed. The lock is held only for these bookkeeping steps, never
/// across an await.
#[derive(Default)]
pub(crate) struct WriteGate {
    state: std::sync::Mutex<GateState>,
    /// Signalled when a poll leaves the transport.
    idle: std::sync::Condvar,
}

#[derive(Default)]
struct GateState {
    closing: bool,
    /// A writer is inside a synchronous transport poll right now.
    polling: bool,
    frame_len: usize,
    written: usize,
    cut_partial: bool,
}

impl WriteGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts one frame. False once teardown has begun.
    pub(crate) fn begin_frame(&self, len: usize) -> bool {
        let mut g = self.lock();
        if g.closing {
            return false;
        }
        g.frame_len = len;
        g.written = 0;
        true
    }

    /// Claims the transport for one synchronous poll. False once teardown has begun.
    pub(crate) fn enter_poll(&self) -> bool {
        let mut g = self.lock();
        if g.closing {
            return false;
        }
        debug_assert!(!g.polling, "one writer task polls one connection");
        g.polling = true;
        true
    }

    /// Releases the transport, accounting for what that poll wrote.
    pub(crate) fn leave_poll(&self, wrote: usize) {
        let mut g = self.lock();
        g.polling = false;
        g.written += wrote;
        debug_assert!(g.written <= g.frame_len);
        drop(g);
        self.idle.notify_all();
    }

    pub(crate) fn finish_frame(&self) {
        let mut g = self.lock();
        if !g.closing {
            debug_assert_eq!(g.written, g.frame_len);
            g.frame_len = 0;
            g.written = 0;
        }
    }

    /// Refuses every later poll, then waits for one already in progress and records whether it
    /// left a frame half written. The caller may reclaim owners once this returns.
    fn begin_close(&self) {
        let mut g = self.lock();
        g.closing = true;
        while g.polling {
            g = self.idle.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.cut_partial = g.written > 0 && g.written < g.frame_len;
    }

    pub(crate) fn cut_partial(&self) -> bool {
        self.lock().cut_partial
    }
}

impl ConnSignals {
    pub(crate) fn new() -> ConnSignals {
        ConnSignals {
            wake: Notify::new(),
            shutdown: watch::Sender::new(false),
            write_gate: WriteGate::default(),
            final_frames: std::sync::Mutex::new(Vec::new()),
        }
    }
}

/// What the connection task must do after `handle` returns.
pub(crate) enum Outcome {
    Done,
    /// Create the staging file, then call `finish_allocate`.
    Allocate(AllocJob),
    /// Copy staging into the sealed file (off the routing path), then call `finish_seal`.
    Seal(SealJob),
    /// The connection was closed; stop reading.
    Close,
}

pub(crate) struct AllocJob {
    pub conn: ConnKey,
    pub reply_to: String,
    pub serial: u64,
    pub len: u64,
    pub owner: u64,
}

pub(crate) struct SealJob {
    pub conn: ConnKey,
    pub reply_to: String,
    pub serial: u64,
    pub len: u64,
    pub digest: Option<String>,
    pub owner: u64,
}

pub(crate) enum NextFrame {
    Frame(Vec<u8>),
    Idle,
    Gone,
}

pub(crate) struct Outbound {
    kind: Kind,
    op: String,
    reply_to: Option<String>,
    body: Map<String, Value>,
    attachments: Vec<Attachment>,
    /// Conservative encoded size using the longest router envelope id.
    bytes: usize,
}

/// Registry counters, for tests and resource measurement. They are logical: an unlinked file
/// a process still has open keeps its pages until that process closes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouterStats {
    pub connections: usize,
    pub services: usize,
    pub topics: usize,
    pub subscriptions: usize,
    /// Call correlation records the router still keeps, detached ones included.
    pub calls: usize,
    /// Calls counted against their callers' active-call limits (admitted, not finished,
    /// not detached).
    pub active_calls: usize,
    /// Artifacts in any state: writing, sealing or sealed.
    pub artifacts: usize,
    pub sealed_artifacts: usize,
    /// Sum of root counts over sealed artifacts.
    pub artifact_roots: u64,
    /// Live owners (deliveries, holds, writers) over all connections.
    pub owners: usize,
    /// Live request reply capabilities. They are independent of delivery credit but share the
    /// per-client bound.
    pub reply_capabilities: usize,
    /// Admitted and not yet dispatched: topic messages in subscription queues, calls in
    /// service queues and results waiting for their caller.
    pub queued: usize,
    /// Staging, sealing-copy and sealed bytes currently charged to the store.
    pub store_bytes: u64,
    pub retained_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OwnerKey {
    Delivery(u64),
    Hold(u64),
}

enum Owner {
    Writer {
        artifact: u64,
        sealing: bool,
    },
    Hold {
        artifact: u64,
    },
    Delivery {
        artifacts: Vec<u64>,
        kind: DeliveryKind,
    },
}

impl Owner {
    /// Counted against the ordinary owner budget (everything but RPC deliveries).
    fn ordinary(&self) -> bool {
        !matches!(
            self,
            Owner::Delivery {
                kind: DeliveryKind::Request { .. } | DeliveryKind::Result { .. },
                ..
            }
        )
    }
}

enum DeliveryKind {
    Topic {
        sub: u64,
    },
    Request {
        call: CallKey,
        service: String,
        incarnation: String,
    },
    Result {
        call: CallKey,
    },
}

struct Hello {
    identity: Identity,
    grants: Grants,
}

struct Conn {
    signals: Arc<ConnSignals>,
    connection_id: String,
    hello: Option<Hello>,
    expected_client_id: Option<String>,
    last_command: Option<u64>,
    out_serial: u64,
    delivery_issued: u64,
    hold_issued: u64,
    sub_issued: u64,
    owners: HashMap<OwnerKey, Owner>,
    ordinary_owners: usize,
    subs: HashMap<u64, Sub>,
    sub_order: Vec<u64>,
    sub_cursor: usize,
    services: Vec<String>,
    svc_cursor: usize,
    /// Request delivery serial -> call, for `rpc.reply` correlation.
    requests: HashMap<u64, CallKey>,
    reply_capabilities: usize,
    call_watermark: Option<u64>,
    /// Admitted calls not yet finished or detached, by call serial.
    active_calls: HashMap<u64, CallKey>,
    result_queue: VecDeque<CallKey>,
    control: VecDeque<Outbound>,
    control_bytes: usize,
    streak: u32,
    queued_bytes: usize,
}

struct Art {
    state: ArtState,
    byte_length: u64,
    content_type: String,
    digest: Option<String>,
    roots: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArtState {
    Writing,
    Sealing,
    Sealed,
}

struct TopicMsg {
    topic: String,
    topic_incarnation: String,
    sequence: u64,
    payload: Map<String, Value>,
    attachments: Vec<(String, ArtifactRef)>,
    artifacts: Vec<u64>,
    bytes: usize,
    artifact_bytes: u64,
}

struct Sub {
    topic: String,
    topic_incarnation: String,
    latest: bool,
    max_queued: u64,
    max_in_flight: u64,
    in_flight: u64,
    queue: VecDeque<Arc<TopicMsg>>,
    replaced: u64,
}

struct Topic {
    incarnation: String,
    retain: bool,
    sequence: u64,
    retained: Option<Arc<TopicMsg>>,
    subscribers: Vec<(ConnKey, u64)>,
}

struct Svc {
    incarnation: String,
    conn: ConnKey,
    max_queued: u64,
    max_in_flight: u64,
    in_flight: u64,
    queue: VecDeque<CallKey>,
}

struct ResultMsg {
    responder: Identity,
    outcome: Map<String, Value>,
    attachments: Vec<(String, ArtifactRef)>,
    artifacts: Vec<u64>,
}

enum Phase {
    Queued,
    Dispatched { delivery: u64 },
    Replied { delivery: u64, result: CallResult },
}

enum CallResult {
    Queued(ResultMsg),
    Delivered,
}

struct Call {
    caller: ConnKey,
    caller_identity: Identity,
    call_serial: u64,
    call_id: String,
    target: String,
    service_incarnation: String,
    service_conn: ConnKey,
    method: String,
    payload: Map<String, Value>,
    attachments: Vec<(String, ArtifactRef)>,
    artifacts: Vec<u64>,
    phase: Phase,
    detached: bool,
    reply_capability: bool,
}

#[derive(Default)]
struct ClientRecord {
    live: Option<ConnKey>,
    last_incarnation: Option<String>,
}

pub(crate) struct State {
    pub router_id: String,
    pub store_id: String,
    contract_digest: String,
    limits: Limits,
    policy: Policy,
    closed: bool,
    conns: HashMap<ConnKey, Conn>,
    identities: HashMap<String, ClientRecord>,
    services: HashMap<String, Svc>,
    topics: HashMap<String, Topic>,
    artifacts: HashMap<u64, Art>,
    calls: HashMap<CallKey, Call>,
    next_conn: u64,
    next_service: u64,
    next_topic: u64,
    next_artifact: u64,
    next_call: u64,
    store_bytes: u64,
    retained_bytes: u64,
    subscriptions: usize,
    unlinks: Vec<String>,
    notices: Vec<(ConnKey, &'static str, Map<String, Value>)>,
}

fn obj(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn err<T>(code: ErrorCode, message: impl Into<String>) -> Result<T, BusError> {
    Err(BusError::new(code, message))
}

fn wire(e: WireError) -> BusError {
    e.into()
}

fn dedup(serials: Vec<u64>) -> Vec<u64> {
    let mut seen = HashSet::new();
    serials.into_iter().filter(|s| seen.insert(*s)).collect()
}

fn attachments_with_owner(list: &[(String, ArtifactRef)], owner: &str) -> Vec<Attachment> {
    list.iter()
        .map(|(name, r)| Attachment {
            name: name.clone(),
            reference: r.clone(),
            owner_id: owner.to_owned(),
        })
        .collect()
}

fn frame_len(
    kind: Kind,
    op: &str,
    body: Map<String, Value>,
    attachments: Vec<Attachment>,
) -> usize {
    let mut env = Envelope::new(serial_id("bus", MAX_SERIAL), kind, op, body);
    env.attachments = attachments;
    serde_json::to_vec(&env.to_value())
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
}

impl Call {
    fn request_body(&self, delivery_id: &str) -> Map<String, Value> {
        obj(vec![
            ("deliveryId", delivery_id.into()),
            ("callId", self.call_id.clone().into()),
            ("caller", self.caller_identity.to_json()),
            ("target", self.target.clone().into()),
            (
                "serviceIncarnation",
                self.service_incarnation.clone().into(),
            ),
            ("method", self.method.clone().into()),
            ("payload", Value::Object(self.payload.clone())),
        ])
    }

    fn result_body(&self, delivery_id: &str, result: &ResultMsg) -> Map<String, Value> {
        obj(vec![
            ("deliveryId", delivery_id.into()),
            ("callId", self.call_id.clone().into()),
            ("responder", result.responder.to_json()),
            (
                "serviceIncarnation",
                self.service_incarnation.clone().into(),
            ),
            ("outcome", Value::Object(result.outcome.clone())),
        ])
    }
}

impl TopicMsg {
    fn body(&self, delivery_id: &str, subscription_id: &str, replaced: u64) -> Map<String, Value> {
        obj(vec![
            ("deliveryId", delivery_id.into()),
            ("subscriptionId", subscription_id.into()),
            ("topic", self.topic.clone().into()),
            ("topicIncarnation", self.topic_incarnation.clone().into()),
            ("topicSequence", self.sequence.to_string().into()),
            ("replaced", replaced.to_string().into()),
            ("payload", Value::Object(self.payload.clone())),
        ])
    }
}

impl State {
    pub(crate) fn new(
        router_id: String,
        store_id: String,
        contract_digest: String,
        limits: Limits,
        policy: Policy,
    ) -> State {
        State {
            router_id,
            store_id,
            contract_digest,
            limits,
            policy,
            closed: false,
            conns: HashMap::new(),
            identities: HashMap::new(),
            services: HashMap::new(),
            topics: HashMap::new(),
            artifacts: HashMap::new(),
            calls: HashMap::new(),
            next_conn: 0,
            next_service: 0,
            next_topic: 0,
            next_artifact: 0,
            next_call: 0,
            store_bytes: 0,
            retained_bytes: 0,
            subscriptions: 0,
            unlinks: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Store files (relative paths) to unlink once the lock is released.
    pub(crate) fn take_unlinks(&mut self) -> Vec<String> {
        std::mem::take(&mut self.unlinks)
    }

    pub(crate) fn stats(&self) -> RouterStats {
        RouterStats {
            connections: self.conns.len(),
            services: self.services.len(),
            topics: self.topics.len(),
            subscriptions: self.subscriptions,
            calls: self.calls.len(),
            active_calls: self.conns.values().map(|c| c.active_calls.len()).sum(),
            artifacts: self.artifacts.len(),
            sealed_artifacts: self
                .artifacts
                .values()
                .filter(|a| a.state == ArtState::Sealed)
                .count(),
            artifact_roots: self.artifacts.values().map(|a| a.roots).sum(),
            owners: self.conns.values().map(|c| c.owners.len()).sum(),
            reply_capabilities: self.conns.values().map(|c| c.reply_capabilities).sum(),
            queued: self
                .conns
                .values()
                .map(|c| {
                    c.subs.values().map(|s| s.queue.len()).sum::<usize>() + c.result_queue.len()
                })
                .sum::<usize>()
                + self.services.values().map(|s| s.queue.len()).sum::<usize>(),
            store_bytes: self.store_bytes,
            retained_bytes: self.retained_bytes,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Connections

    pub(crate) fn add_conn(
        &mut self,
        signals: Arc<ConnSignals>,
        expected_client_id: Option<String>,
    ) -> Option<ConnKey> {
        if self.closed
            || self.conns.len() >= self.limits.max_clients
            || (expected_client_id.is_none() && !self.policy.permits_unbound_transport())
        {
            return None;
        }
        self.next_conn += 1;
        let key = self.next_conn;
        self.conns.insert(
            key,
            Conn {
                signals,
                connection_id: serial_id("conn", key),
                hello: None,
                expected_client_id,
                last_command: None,
                out_serial: 0,
                delivery_issued: 0,
                hold_issued: 0,
                sub_issued: 0,
                owners: HashMap::new(),
                ordinary_owners: 0,
                subs: HashMap::new(),
                sub_order: Vec::new(),
                sub_cursor: 0,
                services: Vec::new(),
                svc_cursor: 0,
                requests: HashMap::new(),
                reply_capabilities: 0,
                call_watermark: None,
                active_calls: HashMap::new(),
                result_queue: VecDeque::new(),
                control: VecDeque::new(),
                control_bytes: 0,
                streak: 0,
                queued_bytes: 0,
            },
        );
        Some(key)
    }

    fn wake(&self, c: ConnKey) {
        if let Some(conn) = self.conns.get(&c) {
            conn.signals.wake.notify_one();
        }
    }

    /// Builds a router-originated item without assigning its envelope id. IDs are assigned only
    /// after the scheduler selects an item, so fairness cannot reorder numbered envelopes.
    fn outbound(
        &self,
        kind: Kind,
        op: &str,
        reply_to: Option<String>,
        body: Map<String, Value>,
        attachments: Vec<Attachment>,
    ) -> Option<Outbound> {
        let mut env = Envelope::new(serial_id("bus", MAX_SERIAL), kind, op, body.clone());
        env.reply_to = reply_to;
        env.attachments = attachments.clone();
        let bytes = env.encode().ok()?.len();
        Some(Outbound {
            kind,
            op: op.to_owned(),
            reply_to: env.reply_to,
            body,
            attachments,
            bytes,
        })
    }

    fn encode_outbound(&mut self, c: ConnKey, item: Outbound) -> Option<Vec<u8>> {
        let conn = self.conns.get_mut(&c)?;
        conn.out_serial += 1;
        let mut env = Envelope::new(
            serial_id("bus", conn.out_serial),
            item.kind,
            &item.op,
            item.body,
        );
        env.reply_to = item.reply_to;
        env.attachments = item.attachments;
        env.encode().ok()
    }

    /// Queues a reply or notice on the control lane; closes the connection if the lane is full.
    fn push_control(&mut self, c: ConnKey, frame: Outbound) {
        let (frames, bytes) = (
            self.limits.max_control_frames,
            self.limits.max_control_bytes,
        );
        let Some(conn) = self.conns.get_mut(&c) else {
            return;
        };
        if conn.control.len() >= frames || conn.control_bytes + frame.bytes > bytes {
            let body = obj(vec![
                ("code", ErrorCode::QuotaExceeded.as_str().into()),
                (
                    "message",
                    "control lane exhausted: replies and notices are not being read".into(),
                ),
            ]);
            let last = self.outbound(Kind::Notice, "connection.closing", None, body, Vec::new());
            self.close_conn(c, last);
            return;
        }
        conn.control_bytes += frame.bytes;
        conn.control.push_back(frame);
        conn.signals.wake.notify_one();
    }

    fn reply(
        &mut self,
        c: ConnKey,
        reply_to: &str,
        op: &str,
        result: Result<Map<String, Value>, BusError>,
    ) {
        let body = reply_body(result);
        if let Some(frame) =
            self.outbound(Kind::Reply, op, Some(reply_to.to_owned()), body, Vec::new())
        {
            self.push_control(c, frame);
        }
    }

    fn notice(&mut self, c: ConnKey, op: &'static str, body: Map<String, Value>) {
        self.notices.push((c, op, body));
    }

    /// Sends deferred notices. Pushing one may close its connection, whose cleanup may defer
    /// more, so this loops until none are left.
    fn flush_notices(&mut self) {
        while !self.notices.is_empty() {
            for (c, op, body) in std::mem::take(&mut self.notices) {
                if let Some(frame) = self.outbound(Kind::Notice, op, None, body, Vec::new()) {
                    self.push_control(c, frame);
                }
            }
        }
    }

    /// Closes `c`: records the final frames, signals both tasks and releases everything the
    /// connection owned. Replies still queued on its control lane are dropped.
    pub(crate) fn close_conn(
        &mut self,
        c: ConnKey,
        final_frames: impl IntoIterator<Item = Outbound>,
    ) {
        let Some(conn) = self.conns.get(&c) else {
            return;
        };
        let signals = conn.signals.clone();
        let final_frames = final_frames
            .into_iter()
            .filter_map(|frame| self.encode_outbound(c, frame));
        signals
            .final_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(final_frames);
        // Teardown refuses every later transport poll first, then waits for a poll already in
        // progress, and only then reclaims what the connection owned. So a delivery frame is
        // either complete before its owner is reclaimed, or left truncated with nothing more
        // appended to the stream; the writer can never finish it afterwards.
        signals.write_gate.begin_close();
        self.disconnect(c);
        signals.shutdown.send_replace(true);
        signals.wake.notify_one();
        self.flush_notices();
    }

    fn violation(&mut self, c: ConnKey, message: String) -> Outcome {
        let body = obj(vec![
            ("code", ErrorCode::InvalidEnvelope.as_str().into()),
            ("message", message.into()),
        ]);
        let last = self.outbound(Kind::Notice, "connection.closing", None, body, Vec::new());
        self.close_conn(c, last);
        Outcome::Close
    }

    /// A frame that failed to decode: close with a notice.
    pub(crate) fn reject_frame(&mut self, c: ConnKey, message: String) {
        self.violation(c, message);
    }

    /// Closes every connection and refuses new ones.
    pub(crate) fn shutdown(&mut self) {
        self.closed = true;
        let keys: Vec<ConnKey> = self.conns.keys().copied().collect();
        for c in keys {
            let mut finals = Vec::new();
            let subs: Vec<(u64, String, String)> = self.conns[&c]
                .subs
                .iter()
                .map(|(id, s)| (*id, s.topic.clone(), s.topic_incarnation.clone()))
                .collect();
            for (id, topic, inc) in subs {
                let body = obj(vec![
                    ("subscriptionId", serial_id("sub", id).into()),
                    ("topic", topic.into()),
                    ("topicIncarnation", inc.into()),
                    ("reason", "router-stopping".into()),
                ]);
                finals.extend(self.outbound(
                    Kind::Notice,
                    "subscription.closed",
                    None,
                    body,
                    Vec::new(),
                ));
            }
            let body = obj(vec![
                ("code", ErrorCode::RouterLost.as_str().into()),
                ("message", "router stopping".into()),
            ]);
            finals.extend(self.outbound(
                Kind::Notice,
                "connection.closing",
                None,
                body,
                Vec::new(),
            ));
            self.close_conn(c, finals);
        }
        let topics: Vec<String> = self.topics.keys().cloned().collect();
        for t in topics {
            if let Some(msg) = self.topics.get_mut(&t).and_then(|t| t.retained.take()) {
                self.retained_bytes -= msg.artifact_bytes;
                self.drop_roots(&msg.artifacts);
            }
        }
    }

    /// Releases everything `c` owned. Retained topic values stay; they are router-owned.
    pub(crate) fn disconnect(&mut self, c: ConnKey) {
        let Some(conn) = self.conns.get(&c) else {
            return;
        };
        if let Some(h) = &conn.hello
            && let Some(rec) = self.identities.get_mut(&h.identity.client_id)
            && rec.live == Some(c)
        {
            rec.live = None;
        }
        // Services: queued calls fail; dispatched ones are settled with the owners below.
        for name in conn.services.clone() {
            self.remove_service(&name, "disconnected");
        }
        // Subscriptions.
        let conn = self.conns.get_mut(&c).expect("present");
        let subs: Vec<u64> = conn.sub_order.clone();
        for id in subs {
            self.drop_subscription(c, id);
        }
        // Owners.
        let conn = self.conns.get_mut(&c).expect("present");
        let owners: Vec<(OwnerKey, Owner)> = conn.owners.drain().collect();
        conn.ordinary_owners = 0;
        for (_, owner) in owners {
            self.release_owner(c, owner);
        }
        // Calls dispatched to this connection that it never answered, whether or not it still
        // held the request: the caller learns the call may have executed.
        let served: Vec<CallKey> = self
            .calls
            .iter()
            .filter(|(_, call)| {
                call.service_conn == c && matches!(call.phase, Phase::Dispatched { .. })
            })
            .map(|(k, _)| *k)
            .collect();
        for key in served {
            let call = &self.calls[&key];
            if !call.detached {
                let body = obj(vec![
                    ("callId", call.call_id.clone().into()),
                    ("code", ErrorCode::NoService.as_str().into()),
                    ("message", "the service disconnected before replying".into()),
                    ("dispatch", Dispatch::Dispatched.as_str().into()),
                ]);
                let caller = call.caller;
                self.notice(caller, "call.failed", body);
            }
            self.remove_call(key);
        }
        // Calls this connection made.
        let mine: Vec<CallKey> = self
            .calls
            .iter()
            .filter(|(_, call)| call.caller == c)
            .map(|(k, _)| *k)
            .collect();
        for key in mine {
            let Some(call) = self.calls.get_mut(&key) else {
                continue;
            };
            match call.phase {
                Phase::Queued => {
                    let (target, inc) = (call.target.clone(), call.service_incarnation.clone());
                    if let Some(svc) = self
                        .services
                        .get_mut(&target)
                        .filter(|s| s.incarnation == inc)
                    {
                        svc.queue.retain(|k| *k != key);
                    }
                    self.remove_call(key);
                }
                // The service may still reply (it will hear `routed:false`) or consume the
                // request; the record goes when either happens.
                Phase::Dispatched { .. } => {
                    call.detached = true;
                    if !call.reply_capability {
                        self.remove_call(key);
                    }
                }
                Phase::Replied { .. } => self.remove_call(key),
            }
        }
        if let Some(conn) = self.conns.remove(&c) {
            debug_assert!(conn.owners.is_empty());
        }
    }

    // -----------------------------------------------------------------------------------------
    // Roots and artifacts

    fn add_roots(&mut self, serials: &[u64]) {
        for s in serials {
            if let Some(a) = self.artifacts.get_mut(s) {
                a.roots += 1;
            }
        }
    }

    fn drop_roots(&mut self, serials: &[u64]) {
        for s in serials {
            let Some(a) = self.artifacts.get_mut(s) else {
                continue;
            };
            a.roots -= 1;
            if a.roots == 0 && a.state == ArtState::Sealed {
                let a = self.artifacts.remove(s).expect("present");
                self.store_bytes -= a.byte_length;
                self.unlinks.push(sealed_rel(*s));
            }
        }
    }

    fn reference(&self, serial: u64) -> Option<ArtifactRef> {
        let a = self.artifacts.get(&serial)?;
        Some(ArtifactRef {
            store_id: self.store_id.clone(),
            artifact_id: serial_id("a", serial),
            generation: GENERATION,
            byte_length: a.byte_length,
            content_type: a.content_type.clone(),
            digest: a.digest.clone(),
        })
    }

    /// Drops an unsealed writer's artifact and its staging reservation.
    fn abandon_writer(&mut self, serial: u64) {
        if let Some(a) = self.artifacts.remove(&serial) {
            self.store_bytes -= a.byte_length;
            self.unlinks.push(staging_rel(serial));
        }
    }

    fn take_owner(&mut self, c: ConnKey, key: OwnerKey) -> Option<Owner> {
        let conn = self.conns.get_mut(&c)?;
        let owner = conn.owners.remove(&key)?;
        if owner.ordinary() {
            conn.ordinary_owners -= 1;
        }
        Some(owner)
    }

    fn insert_owner(&mut self, c: ConnKey, key: OwnerKey, owner: Owner) {
        let conn = self.conns.get_mut(&c).expect("present");
        if owner.ordinary() {
            conn.ordinary_owners += 1;
        }
        conn.owners.insert(key, owner);
    }

    fn ordinary_budget_left(&self, c: ConnKey) -> bool {
        let conn = &self.conns[&c];
        conn.ordinary_owners
            < self.limits.max_owners_per_client - self.limits.reserved_owners_per_client
            && conn.owners.len() < self.limits.max_owners_per_client
    }

    /// Releases a removed owner's roots and returns its credit.
    fn release_owner(&mut self, c: ConnKey, owner: Owner) {
        match owner {
            Owner::Hold { artifact } => self.drop_roots(&[artifact]),
            // A sealing writer's artifact is reclaimed by `finish_seal`, which finds its owner gone.
            Owner::Writer {
                artifact,
                sealing: false,
            } => self.abandon_writer(artifact),
            Owner::Writer { sealing: true, .. } => {}
            Owner::Delivery { artifacts, kind } => {
                self.drop_roots(&artifacts);
                match kind {
                    DeliveryKind::Topic { sub } => {
                        if let Some(s) = self
                            .conns
                            .get_mut(&c)
                            .and_then(|conn| conn.subs.get_mut(&sub))
                        {
                            s.in_flight -= 1;
                        }
                    }
                    DeliveryKind::Request {
                        call,
                        service,
                        incarnation,
                    } => {
                        if let Some(svc) = self
                            .services
                            .get_mut(&service)
                            .filter(|s| s.incarnation == incarnation)
                        {
                            svc.in_flight -= 1;
                        }
                        // A detached call without a remaining reply capability is terminal.
                        if self
                            .calls
                            .get(&call)
                            .is_some_and(|e| e.detached && !e.reply_capability)
                        {
                            self.remove_call(call);
                        }
                    }
                    DeliveryKind::Result { call } => self.remove_call(call),
                }
            }
        }
        self.wake(c);
    }

    /// Checks that `owner_id` is a live owner on `c` holding the sealed artifact `r`.
    fn check_owned(&self, c: ConnKey, r: &ArtifactRef, owner_id: &str) -> Result<u64, BusError> {
        if r.store_id != self.store_id {
            return err(
                ErrorCode::ArtifactGone,
                "reference from another store incarnation",
            );
        }
        let Some(serial) = parse_serial_id("a", &r.artifact_id) else {
            return err(ErrorCode::ArtifactGone, "unknown artifact id");
        };
        let key = if let Some(n) = parse_serial_id("dlv", owner_id) {
            OwnerKey::Delivery(n)
        } else if let Some(n) = parse_serial_id("own", owner_id) {
            OwnerKey::Hold(n)
        } else {
            return err(ErrorCode::OwnerInvalid, "not an owner id");
        };
        let conn = &self.conns[&c];
        let holds = match conn.owners.get(&key) {
            None => {
                return err(
                    ErrorCode::OwnerInvalid,
                    format!("{owner_id} is not a live owner on this connection"),
                );
            }
            Some(Owner::Writer { artifact, .. }) if *artifact == serial => {
                return err(ErrorCode::ArtifactUnsealed, "artifact is not sealed");
            }
            Some(Owner::Writer { .. }) => false,
            Some(Owner::Hold { artifact }) => *artifact == serial,
            Some(Owner::Delivery { artifacts, .. }) => artifacts.contains(&serial),
        };
        if !holds {
            return err(
                ErrorCode::OwnerInvalid,
                format!("{owner_id} does not own {}", r.artifact_id),
            );
        }
        let Some(art) = self.artifacts.get(&serial) else {
            return err(ErrorCode::ArtifactGone, "artifact was collected");
        };
        if art.state != ArtState::Sealed {
            return err(ErrorCode::ArtifactUnsealed, "artifact is not sealed");
        }
        if self.reference(serial).as_ref() != Some(r) {
            return err(
                ErrorCode::ArtifactMismatch,
                "reference does not match the artifact",
            );
        }
        Ok(serial)
    }

    /// Validates every attachment and returns the distinct artifacts, in first-seen order.
    fn check_attachments(&self, c: ConnKey, list: &[Attachment]) -> Result<Vec<u64>, BusError> {
        let mut serials = Vec::with_capacity(list.len());
        for a in list {
            serials.push(self.check_owned(c, &a.reference, &a.owner_id)?);
        }
        Ok(dedup(serials))
    }

    // -----------------------------------------------------------------------------------------
    // Commands

    pub(crate) fn handle(&mut self, c: ConnKey, env: Envelope) -> Outcome {
        let Some(conn) = self.conns.get_mut(&c) else {
            return Outcome::Close;
        };
        if env.kind != Kind::Command || env.reply_to.is_some() {
            return self.violation(c, "clients send commands only, with a null replyTo".into());
        }
        let Some(serial) = parse_serial_id("msg", &env.id) else {
            return self.violation(c, "command ids are msg-<U64>".into());
        };
        if conn.last_command.is_some_and(|last| serial <= last) {
            return self.violation(c, "command ids must increase".into());
        }
        conn.last_command = Some(serial);
        if conn.hello.is_none() {
            if env.op != "bus.hello" {
                return self.violation(c, "the first command must be bus.hello".into());
            }
            return self.hello(c, env);
        }
        if env.op == "bus.hello" {
            return self.violation(c, "bus.hello was already negotiated".into());
        }
        if env.major != MAJOR || env.minor != MINOR {
            return self.violation(
                c,
                format!("envelopes on this connection are {MAJOR}.{MINOR}"),
            );
        }
        let op = env.op.clone();
        let result = if !env.attachments.is_empty()
            && !matches!(op.as_str(), "rpc.call" | "rpc.reply" | "publish")
        {
            err(
                ErrorCode::InvalidEnvelope,
                format!("{op} takes no attachments"),
            )
        } else {
            match op.as_str() {
                "service.register" => self.op_register(c, &env),
                "service.unregister" => self.op_unregister(c, &env),
                "rpc.call" => self.op_call(c, &env),
                "rpc.reply" => self.op_reply(c, &env),
                "rpc.responder.release" => self.op_responder_release(c, &env),
                "rpc.cancel" => self.op_cancel(c, &env),
                "topic.declare" => self.op_declare(c, &env),
                "topic.clear" => self.op_clear(c, &env),
                "topic.delete" => self.op_delete(c, &env),
                "subscribe" => self.op_subscribe(c, &env),
                "unsubscribe" => self.op_unsubscribe(c, &env),
                "publish" => self.op_publish(c, &env),
                "delivery.consumed" => self.op_consumed(c, &env),
                "artifact.release" => self.op_release(c, &env),
                "artifact.open" => self.op_open(c, &env),
                "artifact.retain" => self.op_retain(c, &env),
                "artifact.allocate" => match self.op_allocate(c, &env) {
                    Ok(job) => return Outcome::Allocate(job),
                    Err(e) => Err(e),
                },
                "artifact.seal" => match self.op_seal(c, &env) {
                    Ok(job) => return Outcome::Seal(job),
                    Err(e) => Err(e),
                },
                _ => err(
                    ErrorCode::InvalidEnvelope,
                    format!("unknown operation {op:?}"),
                ),
            }
        };
        self.reply(c, &env.id, &op, result);
        self.flush_notices();
        if self.conns.contains_key(&c) {
            Outcome::Done
        } else {
            Outcome::Close
        }
    }

    fn grants(&self, c: ConnKey) -> &Grants {
        &self.conns[&c].hello.as_ref().expect("after hello").grants
    }

    fn identity(&self, c: ConnKey) -> Identity {
        self.conns[&c]
            .hello
            .as_ref()
            .expect("after hello")
            .identity
            .clone()
    }

    fn hello(&mut self, c: ConnKey, env: Envelope) -> Outcome {
        let parsed = (|| -> Result<(String, String, Vec<u64>), WireError> {
            let mut f = Fields::of(&env.body, "bus.hello");
            let client_id = f.id("clientId")?;
            let incarnation = f.id("clientIncarnation")?;
            let majors = f.array("supportedMajors", 1, 16)?;
            let mut out = Vec::new();
            for m in majors {
                match m.as_u64() {
                    Some(n) if (1..=MAX_CREDIT).contains(&n) => out.push(n),
                    _ => {
                        return Err(WireError(
                            "bus.hello: supportedMajors holds integers 1..=65535".into(),
                        ));
                    }
                }
            }
            f.finish()?;
            Ok((client_id, incarnation, out))
        })();
        let refuse = |s: &mut State, e: BusError| {
            let body = reply_body(Err(e));
            let last = s.outbound(
                Kind::Reply,
                "bus.hello",
                Some(env.id.clone()),
                body,
                Vec::new(),
            );
            s.close_conn(c, last);
            Outcome::Close
        };
        let (client_id, incarnation, majors) = match parsed {
            Ok(p) => p,
            Err(e) => return refuse(self, e.into()),
        };
        if !env.attachments.is_empty() {
            return refuse(self, BusError::invalid("bus.hello takes no attachments"));
        }
        if !majors.contains(&MAJOR) {
            return refuse(
                self,
                BusError::new(
                    ErrorCode::VersionMismatch,
                    format!("this router speaks major {MAJOR} only"),
                ),
            );
        }
        let Some(grants) = self.policy.grants_for(&client_id) else {
            return refuse(
                self,
                BusError::new(ErrorCode::NotAuthorized, "client id is not configured"),
            );
        };
        let rec = self.identities.entry(client_id.clone()).or_default();
        if rec.live.is_some() {
            return refuse(
                self,
                BusError::new(ErrorCode::NotAuthorized, "client id is already connected"),
            );
        }
        if rec.last_incarnation.as_deref() == Some(incarnation.as_str()) {
            return refuse(
                self,
                BusError::new(
                    ErrorCode::NotAuthorized,
                    "client incarnation was already used",
                ),
            );
        }
        if let Some(expected) = self.conns[&c].expected_client_id.as_deref()
            && expected != client_id
        {
            return refuse(
                self,
                BusError::new(
                    ErrorCode::NotAuthorized,
                    "client id does not match the launcher's transport binding",
                ),
            );
        }
        let rec = self.identities.get_mut(&client_id).expect("inserted");
        rec.live = Some(c);
        rec.last_incarnation = Some(incarnation.clone());
        let identity = Identity {
            client_id,
            client_incarnation: incarnation,
        };
        let conn = self.conns.get_mut(&c).expect("present");
        conn.hello = Some(Hello { identity, grants });
        let value = obj(vec![
            ("routerId", self.router_id.clone().into()),
            ("connectionId", conn.connection_id.clone().into()),
            ("selectedMajor", MAJOR.into()),
            ("selectedMinor", MINOR.into()),
            ("contractDigest", self.contract_digest.clone().into()),
            ("limits", self.limits.to_json()),
        ]);
        self.reply(c, &env.id, "bus.hello", Ok(value));
        if self.conns.contains_key(&c) {
            Outcome::Done
        } else {
            Outcome::Close
        }
    }

    // ---- services and RPC

    fn op_register(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "service.register");
        let name = f.name("name").map_err(wire)?;
        let max_queued = f.int("maxQueued", 1, MAX_CREDIT).map_err(wire)?;
        let max_in_flight = f.int("maxInFlight", 1, MAX_CREDIT).map_err(wire)?;
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).register, &name) {
            return err(ErrorCode::NotAuthorized, format!("may not register {name}"));
        }
        if max_queued > self.limits.max_service_queued
            || max_in_flight > self.limits.max_service_in_flight
        {
            return err(
                ErrorCode::QuotaExceeded,
                "maxQueued or maxInFlight exceeds the configured limit",
            );
        }
        if self.services.contains_key(&name) {
            return err(ErrorCode::Conflict, format!("{name} is already registered"));
        }
        if self.services.len() >= self.limits.max_services {
            return err(ErrorCode::QuotaExceeded, "too many services");
        }
        self.next_service += 1;
        let incarnation = serial_id("svc", self.next_service);
        self.services.insert(
            name.clone(),
            Svc {
                incarnation: incarnation.clone(),
                conn: c,
                max_queued,
                max_in_flight,
                in_flight: 0,
                queue: VecDeque::new(),
            },
        );
        self.conns.get_mut(&c).expect("present").services.push(name);
        Ok(obj(vec![("serviceIncarnation", incarnation.into())]))
    }

    fn op_unregister(
        &mut self,
        c: ConnKey,
        env: &Envelope,
    ) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "service.unregister");
        let name = f.name("name").map_err(wire)?;
        let incarnation = f.id("serviceIncarnation").map_err(wire)?;
        f.finish().map_err(wire)?;
        let mine = self
            .services
            .get(&name)
            .is_some_and(|s| s.conn == c && s.incarnation == incarnation);
        if mine {
            self.remove_service(&name, "unregistered");
        }
        Ok(obj(vec![("removed", mine.into())]))
    }

    /// Removes a registration. Queued calls fail with `call.failed`; each affected caller also
    /// hears `route.removed`. Dispatched calls stay correlated: the service may still reply.
    fn remove_service(&mut self, name: &str, reason: &str) {
        let Some(svc) = self.services.remove(name) else {
            return;
        };
        if let Some(conn) = self.conns.get_mut(&svc.conn) {
            conn.services.retain(|n| n != name);
        }
        let mut callers = Vec::new();
        for (_, call) in self.calls.iter() {
            let open = matches!(call.phase, Phase::Queued | Phase::Dispatched { .. });
            if call.service_incarnation == svc.incarnation
                && open
                && !call.detached
                && !callers.contains(&call.caller)
            {
                callers.push(call.caller);
            }
        }
        for caller in callers {
            let body = obj(vec![
                ("name", name.into()),
                ("serviceIncarnation", svc.incarnation.clone().into()),
                ("reason", reason.into()),
            ]);
            self.notice(caller, "route.removed", body);
        }
        for key in svc.queue {
            let Some(call) = self.calls.get(&key) else {
                continue;
            };
            let body = obj(vec![
                ("callId", call.call_id.clone().into()),
                ("code", ErrorCode::NoService.as_str().into()),
                (
                    "message",
                    format!("{name} was {reason} before the call was dispatched").into(),
                ),
                ("dispatch", Dispatch::NotDispatched.as_str().into()),
            ]);
            let caller = call.caller;
            self.notice(caller, "call.failed", body);
            self.remove_call(key);
        }
        let abandoned: Vec<CallKey> = self
            .calls
            .iter()
            .filter(|(_, call)| {
                call.service_incarnation == svc.incarnation
                    && matches!(call.phase, Phase::Dispatched { .. })
                    && !call.reply_capability
            })
            .map(|(key, _)| *key)
            .collect();
        for key in abandoned {
            let call = &self.calls[&key];
            if !call.detached {
                self.notice(
                    call.caller,
                    "call.failed",
                    obj(vec![
                        ("callId", call.call_id.clone().into()),
                        ("code", ErrorCode::NoService.as_str().into()),
                        (
                            "message",
                            format!("{name} was {reason} after dispatch").into(),
                        ),
                        ("dispatch", Dispatch::Dispatched.as_str().into()),
                    ]),
                );
            }
            self.remove_call(key);
        }
    }

    /// Forgets a call: frees the caller's slot, the reply correlation and any roots the call
    /// itself still holds (queued request, queued result).
    fn remove_call(&mut self, key: CallKey) {
        let Some(call) = self.calls.remove(&key) else {
            return;
        };
        if let Some(conn) = self.conns.get_mut(&call.caller) {
            if conn.active_calls.get(&call.call_serial) == Some(&key) {
                conn.active_calls.remove(&call.call_serial);
            }
            conn.result_queue.retain(|k| *k != key);
        }
        match call.phase {
            Phase::Queued => self.drop_roots(&call.artifacts),
            Phase::Dispatched { delivery } => {
                if let Some(conn) = self.conns.get_mut(&call.service_conn) {
                    conn.requests.remove(&delivery);
                    if call.reply_capability {
                        conn.reply_capabilities -= 1;
                    }
                }
            }
            Phase::Replied { delivery, result } => {
                if let Some(conn) = self.conns.get_mut(&call.service_conn) {
                    conn.requests.remove(&delivery);
                    if call.reply_capability {
                        conn.reply_capabilities -= 1;
                    }
                }
                if let CallResult::Queued(msg) = result {
                    self.drop_roots(&msg.artifacts);
                }
            }
        }
    }

    fn op_call(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "rpc.call");
        let call_id = f.id("callId").map_err(wire)?;
        let target = f.name("target").map_err(wire)?;
        let expected = f.nullable_id("expectedIncarnation").map_err(wire)?;
        let method = f.method("method").map_err(wire)?;
        let payload = f.object("payload").map_err(wire)?.clone();
        f.finish().map_err(wire)?;
        let Some(call_serial) = parse_serial_id("call", &call_id) else {
            return err(ErrorCode::InvalidEnvelope, "callId must be call-<U64>");
        };
        if self.conns[&c]
            .call_watermark
            .is_some_and(|w| call_serial <= w)
        {
            return err(
                ErrorCode::InvalidEnvelope,
                "callId was already used or is not increasing",
            );
        }
        // The issued-id history advances after syntactic validation, before semantic admission.
        self.conns.get_mut(&c).expect("present").call_watermark = Some(call_serial);
        if !Grants::allows(&self.grants(c).call, &target) {
            return err(ErrorCode::NotAuthorized, format!("may not call {target}"));
        }
        let conn = &self.conns[&c];
        if conn.active_calls.len() >= self.limits.max_active_calls_per_client {
            return err(ErrorCode::Backpressure, "too many active calls");
        }
        let Some(svc) = self.services.get(&target) else {
            return err(ErrorCode::NoService, format!("{target} is not registered"));
        };
        if expected.as_ref().is_some_and(|e| *e != svc.incarnation) {
            return err(
                ErrorCode::TargetChanged,
                format!("{target} is now {}", svc.incarnation),
            );
        }
        if svc.queue.len() as u64 >= svc.max_queued {
            return err(ErrorCode::Backpressure, format!("{target} queue is full"));
        }
        let artifacts = self.check_attachments(c, &env.attachments)?;
        let call = Call {
            caller: c,
            caller_identity: self.identity(c),
            call_serial,
            call_id,
            target: target.clone(),
            service_incarnation: svc.incarnation.clone(),
            service_conn: svc.conn,
            method,
            payload,
            attachments: env
                .attachments
                .iter()
                .map(|a| (a.name.clone(), a.reference.clone()))
                .collect(),
            artifacts,
            phase: Phase::Queued,
            detached: false,
            reply_capability: false,
        };
        let placeholder = serial_id("dlv", MAX_SERIAL);
        let size = frame_len(
            Kind::Delivery,
            "rpc.request",
            call.request_body(&placeholder),
            attachments_with_owner(&call.attachments, &placeholder),
        );
        if size > MAX_ENVELOPE_BYTES {
            return err(
                ErrorCode::InvalidEnvelope,
                "the request delivery would exceed the envelope limit",
            );
        }
        // Admission: roots first, then the queue entry, then the reply.
        self.add_roots(&call.artifacts);
        self.next_call += 1;
        let key = self.next_call;
        let incarnation = call.service_incarnation.clone();
        let service_conn = call.service_conn;
        self.calls.insert(key, call);
        self.services
            .get_mut(&target)
            .expect("checked")
            .queue
            .push_back(key);
        let conn = self.conns.get_mut(&c).expect("present");
        conn.active_calls.insert(call_serial, key);
        self.wake(service_conn);
        Ok(obj(vec![
            ("accepted", true.into()),
            ("serviceIncarnation", incarnation.into()),
        ]))
    }

    fn op_reply(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "rpc.reply");
        let call_id = f.id("callId").map_err(wire)?;
        let delivery_id = f.id("requestDeliveryId").map_err(wire)?;
        let outcome = f.object("outcome").map_err(wire)?.clone();
        f.finish().map_err(wire)?;
        let Some(n) = parse_serial_id("dlv", &delivery_id) else {
            return err(
                ErrorCode::InvalidEnvelope,
                "requestDeliveryId must be dlv-<U64>",
            );
        };
        let conn = &self.conns[&c];
        if n > conn.delivery_issued {
            return err(
                ErrorCode::OwnerInvalid,
                "requestDeliveryId was never issued to this connection",
            );
        }
        let Some(&key) = conn.requests.get(&n) else {
            return err(
                ErrorCode::CallGone,
                "no call awaits a reply for that request",
            );
        };
        let call = &self.calls[&key];
        if call.call_id != call_id {
            return err(
                ErrorCode::InvalidEnvelope,
                "callId does not match the request",
            );
        }
        if matches!(call.phase, Phase::Replied { .. }) {
            return err(ErrorCode::CallGone, "the call was already replied to");
        }
        let artifacts = self.check_attachments(c, &env.attachments)?;
        let result = ResultMsg {
            responder: self.identity(c),
            outcome,
            attachments: env
                .attachments
                .iter()
                .map(|a| (a.name.clone(), a.reference.clone()))
                .collect(),
            artifacts,
        };
        let placeholder = serial_id("dlv", MAX_SERIAL);
        let size = frame_len(
            Kind::Delivery,
            "rpc.result",
            call.result_body(&placeholder, &result),
            attachments_with_owner(&result.attachments, &placeholder),
        );
        if size > MAX_ENVELOPE_BYTES {
            return err(
                ErrorCode::InvalidEnvelope,
                "the result delivery would exceed the envelope limit",
            );
        }
        if call.detached {
            self.remove_call(key);
            return Ok(obj(vec![("routed", false.into())]));
        }
        let caller = call.caller;
        self.add_roots(&result.artifacts);
        let released_capability = self.calls[&key].reply_capability;
        if released_capability {
            self.conns
                .get_mut(&c)
                .expect("service is connected")
                .reply_capabilities -= 1;
        }
        let call = self.calls.get_mut(&key).expect("present");
        call.reply_capability = false;
        call.phase = Phase::Replied {
            delivery: n,
            result: CallResult::Queued(result),
        };
        self.conns
            .get_mut(&caller)
            .expect("attached caller is connected")
            .result_queue
            .push_back(key);
        self.wake(caller);
        Ok(obj(vec![("routed", true.into())]))
    }

    fn op_responder_release(
        &mut self,
        c: ConnKey,
        env: &Envelope,
    ) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "rpc.responder.release");
        let call_id = f.id("callId").map_err(wire)?;
        let delivery_id = f.id("requestDeliveryId").map_err(wire)?;
        f.finish().map_err(wire)?;
        let Some(delivery) = parse_serial_id("dlv", &delivery_id) else {
            return err(
                ErrorCode::InvalidEnvelope,
                "requestDeliveryId must be dlv-<U64>",
            );
        };
        if delivery > self.conns[&c].delivery_issued {
            return err(
                ErrorCode::OwnerInvalid,
                "requestDeliveryId was never issued to this connection",
            );
        }
        let Some(&key) = self.conns[&c].requests.get(&delivery) else {
            return Ok(obj(vec![("released", false.into())]));
        };
        if self.calls[&key].call_id != call_id {
            return err(
                ErrorCode::InvalidEnvelope,
                "callId does not match the request",
            );
        }
        if !self.calls[&key].reply_capability {
            return Ok(obj(vec![("released", false.into())]));
        }
        let detached = self.calls[&key].detached;
        let service_live = {
            let call = &self.calls[&key];
            self.services.get(&call.target).is_some_and(|service| {
                service.conn == call.service_conn && service.incarnation == call.service_incarnation
            })
        };
        self.calls
            .get_mut(&key)
            .expect("request maps to a call")
            .reply_capability = false;
        self.conns
            .get_mut(&c)
            .expect("service is connected")
            .reply_capabilities -= 1;
        if detached {
            self.remove_call(key);
        } else {
            let call = &self.calls[&key];
            let (code, message) = if service_live {
                (
                    ErrorCode::CallGone,
                    "the last responder was released without replying",
                )
            } else {
                (
                    ErrorCode::NoService,
                    "the service route ended after dispatch without a reply",
                )
            };
            self.notice(
                call.caller,
                "call.failed",
                obj(vec![
                    ("callId", call.call_id.clone().into()),
                    ("code", code.as_str().into()),
                    ("message", message.into()),
                    ("dispatch", Dispatch::Dispatched.as_str().into()),
                ]),
            );
            self.remove_call(key);
        }
        Ok(obj(vec![("released", true.into())]))
    }

    fn op_cancel(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "rpc.cancel");
        let call_id = f.id("callId").map_err(wire)?;
        f.finish().map_err(wire)?;
        let Some(serial) = parse_serial_id("call", &call_id) else {
            return err(ErrorCode::InvalidEnvelope, "callId must be call-<U64>");
        };
        let state = match self.conns[&c].active_calls.get(&serial).copied() {
            None => "call-gone",
            Some(key) => match self.calls[&key].phase {
                Phase::Queued => {
                    let call = &self.calls[&key];
                    let (target, inc) = (call.target.clone(), call.service_incarnation.clone());
                    if let Some(svc) = self
                        .services
                        .get_mut(&target)
                        .filter(|s| s.incarnation == inc)
                    {
                        svc.queue.retain(|k| *k != key);
                    }
                    self.remove_call(key);
                    "cancelled-before-dispatch"
                }
                Phase::Dispatched { .. } => {
                    self.calls.get_mut(&key).expect("present").detached = true;
                    self.conns
                        .get_mut(&c)
                        .expect("present")
                        .active_calls
                        .remove(&serial);
                    if !self.calls[&key].reply_capability {
                        self.remove_call(key);
                    }
                    "execution-unknown"
                }
                Phase::Replied { .. } => "completed",
            },
        };
        Ok(obj(vec![("state", state.into())]))
    }

    // ---- topics

    fn op_declare(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "topic.declare");
        let name = f.name("name").map_err(wire)?;
        let retain = match f.string("retained").map_err(wire)? {
            "none" => false,
            "latest" => true,
            _ => {
                return err(
                    ErrorCode::InvalidEnvelope,
                    "retained is \"none\" or \"latest\"",
                );
            }
        };
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).manage_topics, &name) {
            return err(ErrorCode::NotAuthorized, format!("may not declare {name}"));
        }
        if let Some(t) = self.topics.get(&name) {
            if t.retain != retain {
                return err(
                    ErrorCode::Conflict,
                    format!("{name} is declared with other settings"),
                );
            }
            return Ok(obj(vec![
                ("declared", false.into()),
                ("topicIncarnation", t.incarnation.clone().into()),
            ]));
        }
        if self.topics.len() >= self.limits.max_topics {
            return err(ErrorCode::QuotaExceeded, "too many topics");
        }
        self.next_topic += 1;
        let incarnation = serial_id("top", self.next_topic);
        self.topics.insert(
            name,
            Topic {
                incarnation: incarnation.clone(),
                retain,
                sequence: 0,
                retained: None,
                subscribers: Vec::new(),
            },
        );
        Ok(obj(vec![
            ("declared", true.into()),
            ("topicIncarnation", incarnation.into()),
        ]))
    }

    fn op_clear(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "topic.clear");
        let name = f.name("name").map_err(wire)?;
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).manage_topics, &name) {
            return err(ErrorCode::NotAuthorized, format!("may not clear {name}"));
        }
        let Some(t) = self.topics.get_mut(&name) else {
            return err(ErrorCode::NoTopic, format!("{name} is not declared"));
        };
        let old = t.retained.take();
        let cleared = old.is_some();
        if let Some(msg) = old {
            self.retained_bytes -= msg.artifact_bytes;
            self.drop_roots(&msg.artifacts);
        }
        Ok(obj(vec![("cleared", cleared.into())]))
    }

    fn op_delete(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "topic.delete");
        let name = f.name("name").map_err(wire)?;
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).manage_topics, &name) {
            return err(ErrorCode::NotAuthorized, format!("may not delete {name}"));
        }
        let Some(t) = self.topics.get(&name) else {
            return Ok(obj(vec![("deleted", false.into())]));
        };
        if !t.subscribers.is_empty() {
            return err(ErrorCode::Conflict, format!("{name} still has subscribers"));
        }
        let t = self.topics.remove(&name).expect("present");
        if let Some(msg) = t.retained {
            self.retained_bytes -= msg.artifact_bytes;
            self.drop_roots(&msg.artifacts);
        }
        Ok(obj(vec![("deleted", true.into())]))
    }

    fn op_subscribe(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "subscribe");
        let topic = f.name("topic").map_err(wire)?;
        let latest = match f.string("mode").map_err(wire)? {
            "latest" => true,
            "bounded" => false,
            _ => {
                return err(
                    ErrorCode::InvalidEnvelope,
                    "mode is \"latest\" or \"bounded\"",
                );
            }
        };
        let max_queued = f.int("maxQueued", 1, MAX_CREDIT).map_err(wire)?;
        let max_in_flight = f.int("maxInFlight", 1, MAX_CREDIT).map_err(wire)?;
        let replay = f.boolean("replayLatest").map_err(wire)?;
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).subscribe, &topic) {
            return err(
                ErrorCode::NotAuthorized,
                format!("may not subscribe to {topic}"),
            );
        }
        if latest && max_queued != 1 {
            return err(
                ErrorCode::InvalidEnvelope,
                "latest subscriptions have maxQueued 1",
            );
        }
        let (queue_cap, credit_cap) = if latest {
            (1, self.limits.max_latest_in_flight)
        } else {
            (
                self.limits.max_bounded_queued,
                self.limits.max_bounded_in_flight,
            )
        };
        if max_queued > queue_cap || max_in_flight > credit_cap {
            return err(
                ErrorCode::QuotaExceeded,
                "maxQueued or maxInFlight exceeds the configured limit",
            );
        }
        let Some(t) = self.topics.get(&topic) else {
            return err(ErrorCode::NoTopic, format!("{topic} is not declared"));
        };
        let conn = &self.conns[&c];
        if conn.subs.len() >= self.limits.max_subscriptions_per_client
            || self.subscriptions >= self.limits.max_subscriptions
        {
            return err(ErrorCode::QuotaExceeded, "too many subscriptions");
        }
        let incarnation = t.incarnation.clone();
        let replayed = if replay { t.retained.clone() } else { None };
        if !latest
            && replayed.as_ref().is_some_and(|msg| {
                conn.queued_bytes.saturating_add(msg.bytes)
                    > self.limits.max_queued_bytes_per_client
            })
        {
            return err(
                ErrorCode::Backpressure,
                "retained replay exceeds the bounded subscriber's queued-byte limit",
            );
        }
        let mut sub = Sub {
            topic: topic.clone(),
            topic_incarnation: incarnation.clone(),
            latest,
            max_queued,
            max_in_flight,
            in_flight: 0,
            queue: VecDeque::new(),
            replaced: 0,
        };
        let mut bytes = 0;
        if let Some(msg) = replayed {
            self.add_roots(&msg.artifacts);
            if !latest {
                bytes = msg.bytes;
            }
            sub.queue.push_back(msg);
        }
        let conn = self.conns.get_mut(&c).expect("present");
        conn.sub_issued += 1;
        let id = conn.sub_issued;
        conn.queued_bytes += bytes;
        conn.subs.insert(id, sub);
        conn.sub_order.push(id);
        self.subscriptions += 1;
        self.topics
            .get_mut(&topic)
            .expect("present")
            .subscribers
            .push((c, id));
        self.wake(c);
        Ok(obj(vec![
            ("subscriptionId", serial_id("sub", id).into()),
            ("topicIncarnation", incarnation.into()),
        ]))
    }

    /// Removes a subscription and releases its queue. Deliveries already handed out keep their
    /// roots until consumed.
    fn drop_subscription(&mut self, c: ConnKey, id: u64) -> bool {
        let Some(conn) = self.conns.get_mut(&c) else {
            return false;
        };
        let Some(sub) = conn.subs.remove(&id) else {
            return false;
        };
        conn.sub_order.retain(|s| *s != id);
        self.subscriptions -= 1;
        if !sub.latest {
            conn.queued_bytes -= sub.queue.iter().map(|m| m.bytes).sum::<usize>();
        }
        if let Some(t) = self
            .topics
            .get_mut(&sub.topic)
            .filter(|t| t.incarnation == sub.topic_incarnation)
        {
            t.subscribers.retain(|s| *s != (c, id));
        }
        for msg in sub.queue {
            self.drop_roots(&msg.artifacts);
        }
        true
    }

    fn op_unsubscribe(
        &mut self,
        c: ConnKey,
        env: &Envelope,
    ) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "unsubscribe");
        let sid = f.id("subscriptionId").map_err(wire)?;
        f.finish().map_err(wire)?;
        let removed = parse_serial_id("sub", &sid).is_some_and(|id| self.drop_subscription(c, id));
        Ok(obj(vec![("removed", removed.into())]))
    }

    fn op_publish(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "publish");
        let topic = f.name("topic").map_err(wire)?;
        let payload = f.object("payload").map_err(wire)?.clone();
        f.finish().map_err(wire)?;
        if !Grants::allows(&self.grants(c).publish, &topic) {
            return err(
                ErrorCode::NotAuthorized,
                format!("may not publish to {topic}"),
            );
        }
        let Some(t) = self.topics.get(&topic) else {
            return err(ErrorCode::NoTopic, format!("{topic} is not declared"));
        };
        let artifacts = self.check_attachments(c, &env.attachments)?;
        let artifact_bytes: u64 = artifacts
            .iter()
            .map(|s| self.artifacts[s].byte_length)
            .sum();
        let mut msg = TopicMsg {
            topic: topic.clone(),
            topic_incarnation: t.incarnation.clone(),
            sequence: t.sequence + 1,
            payload,
            attachments: env
                .attachments
                .iter()
                .map(|a| (a.name.clone(), a.reference.clone()))
                .collect(),
            artifacts,
            bytes: 0,
            artifact_bytes,
        };
        let placeholder = serial_id("dlv", MAX_SERIAL);
        msg.bytes = frame_len(
            Kind::Delivery,
            "topic.message",
            msg.body(&placeholder, &serial_id("sub", MAX_SERIAL), MAX_SERIAL),
            attachments_with_owner(&msg.attachments, &placeholder),
        );
        if msg.bytes > MAX_ENVELOPE_BYTES {
            return err(
                ErrorCode::InvalidEnvelope,
                "the delivery would exceed the envelope limit",
            );
        }
        // Admission is all or nothing: check every subscriber and the retention quota before
        // touching anything.
        let mut extra: HashMap<ConnKey, usize> = HashMap::new();
        for (sc, sid) in &t.subscribers {
            let conn = &self.conns[sc];
            let sub = &conn.subs[sid];
            if sub.latest {
                continue;
            }
            if sub.queue.len() as u64 >= sub.max_queued {
                return err(
                    ErrorCode::Backpressure,
                    "a bounded subscriber's queue is full",
                );
            }
            let add = extra.entry(*sc).or_default();
            *add += msg.bytes;
            if conn.queued_bytes + *add > self.limits.max_queued_bytes_per_client {
                return err(
                    ErrorCode::Backpressure,
                    "a bounded subscriber's queued bytes are at the limit",
                );
            }
        }
        if t.retain {
            let old = t.retained.as_ref().map_or(0, |m| m.artifact_bytes);
            if self.retained_bytes - old + msg.artifact_bytes > self.limits.max_retained_bytes {
                return err(ErrorCode::QuotaExceeded, "retained bytes are at the limit");
            }
        }
        // Accept.
        let msg = Arc::new(msg);
        let t = self.topics.get_mut(&topic).expect("present");
        t.sequence += 1;
        let subscribers = t.subscribers.clone();
        let retain = t.retain;
        let mut replaced = 0u64;
        for (sc, sid) in &subscribers {
            let conn = self.conns.get_mut(sc).expect("subscriber is connected");
            let sub = conn.subs.get_mut(sid).expect("subscription is live");
            let dropped = if sub.latest {
                let old = sub.queue.pop_front();
                if old.is_some() {
                    sub.replaced += 1;
                    replaced += 1;
                }
                old
            } else {
                conn.queued_bytes += msg.bytes;
                None
            };
            sub.queue.push_back(msg.clone());
            self.add_roots(&msg.artifacts);
            if let Some(old) = dropped {
                self.drop_roots(&old.artifacts);
            }
            self.wake(*sc);
        }
        if retain {
            self.add_roots(&msg.artifacts);
            self.retained_bytes += msg.artifact_bytes;
            let old = self
                .topics
                .get_mut(&topic)
                .expect("present")
                .retained
                .replace(msg.clone());
            if let Some(old) = old {
                self.retained_bytes -= old.artifact_bytes;
                self.drop_roots(&old.artifacts);
            }
        }
        Ok(obj(vec![
            ("topicSequence", msg.sequence.to_string().into()),
            ("subscribers", subscribers.len().to_string().into()),
            ("replaced", replaced.to_string().into()),
        ]))
    }

    // ---- ownership

    fn op_consumed(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "delivery.consumed");
        let ids = id_batch(
            f.array("deliveryIds", 1, MAX_BATCH).map_err(wire)?,
            "deliveryIds",
        )
        .map_err(wire)?;
        f.finish().map_err(wire)?;
        let issued = self.conns[&c].delivery_issued;
        let mut serials = Vec::with_capacity(ids.len());
        for id in &ids {
            match parse_serial_id("dlv", id) {
                Some(n) if n <= issued => serials.push(n),
                Some(_) => return err(ErrorCode::OwnerInvalid, format!("{id} was never issued")),
                None => {
                    return err(
                        ErrorCode::OwnerInvalid,
                        format!("{id} is not a delivery id"),
                    );
                }
            }
        }
        let mut released = 0u64;
        for n in serials {
            if let Some(owner) = self.take_owner(c, OwnerKey::Delivery(n)) {
                released += 1;
                self.release_owner(c, owner);
            }
        }
        Ok(obj(vec![("released", released.to_string().into())]))
    }

    fn op_release(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "artifact.release");
        let ids =
            id_batch(f.array("ownerIds", 1, MAX_BATCH).map_err(wire)?, "ownerIds").map_err(wire)?;
        f.finish().map_err(wire)?;
        let issued = self.conns[&c].hold_issued;
        let mut serials = Vec::with_capacity(ids.len());
        for id in &ids {
            match parse_serial_id("own", id) {
                Some(n) if n <= issued => serials.push(n),
                Some(_) => return err(ErrorCode::OwnerInvalid, format!("{id} was never issued")),
                None => {
                    return err(
                        ErrorCode::OwnerInvalid,
                        format!("{id} is not a hold; deliveries end with delivery.consumed"),
                    );
                }
            }
        }
        let mut released = 0u64;
        for n in serials {
            if let Some(owner) = self.take_owner(c, OwnerKey::Hold(n)) {
                released += 1;
                self.release_owner(c, owner);
            }
        }
        Ok(obj(vec![("released", released.to_string().into())]))
    }

    fn op_open(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "artifact.open");
        let r = ArtifactRef::from_json(f.value("ref").map_err(wire)?).map_err(wire)?;
        let owner = f.id("ownerId").map_err(wire)?;
        f.finish().map_err(wire)?;
        let serial = self.check_owned(c, &r, &owner)?;
        let loc = crate::wire::Location {
            store_id: self.store_id.clone(),
            relative_path: sealed_rel(serial),
        };
        Ok(obj(vec![("readLocation", loc.to_json())]))
    }

    fn op_retain(&mut self, c: ConnKey, env: &Envelope) -> Result<Map<String, Value>, BusError> {
        let mut f = Fields::of(&env.body, "artifact.retain");
        let r = ArtifactRef::from_json(f.value("ref").map_err(wire)?).map_err(wire)?;
        let owner = f.id("ownerId").map_err(wire)?;
        f.finish().map_err(wire)?;
        let serial = self.check_owned(c, &r, &owner)?;
        if !self.ordinary_budget_left(c) {
            return err(ErrorCode::QuotaExceeded, "owner budget exhausted");
        }
        let conn = self.conns.get_mut(&c).expect("present");
        conn.hold_issued += 1;
        let n = conn.hold_issued;
        self.insert_owner(c, OwnerKey::Hold(n), Owner::Hold { artifact: serial });
        self.add_roots(&[serial]);
        Ok(obj(vec![("ownerId", serial_id("own", n).into())]))
    }

    fn op_allocate(&mut self, c: ConnKey, env: &Envelope) -> Result<AllocJob, BusError> {
        let mut f = Fields::of(&env.body, "artifact.allocate");
        let len = f.u64_string("byteLength").map_err(wire)?;
        let content_type = f.string("contentType").map_err(wire)?.to_owned();
        f.finish().map_err(wire)?;
        if !crate::wire::is_content_type(&content_type) {
            return err(
                ErrorCode::InvalidEnvelope,
                "contentType must be 1..=127 printable ASCII characters",
            );
        }
        if len > self.limits.max_artifact_bytes {
            return err(
                ErrorCode::QuotaExceeded,
                "byteLength exceeds the per-object limit",
            );
        }
        if self.store_bytes.saturating_add(len) > self.limits.max_store_bytes {
            return err(ErrorCode::QuotaExceeded, "the store is full");
        }
        if !self.ordinary_budget_left(c) {
            return err(ErrorCode::QuotaExceeded, "owner budget exhausted");
        }
        self.next_artifact += 1;
        let serial = self.next_artifact;
        self.artifacts.insert(
            serial,
            Art {
                state: ArtState::Writing,
                byte_length: len,
                content_type,
                digest: None,
                roots: 0,
            },
        );
        self.store_bytes += len;
        let conn = self.conns.get_mut(&c).expect("present");
        conn.hold_issued += 1;
        let owner = conn.hold_issued;
        self.insert_owner(
            c,
            OwnerKey::Hold(owner),
            Owner::Writer {
                artifact: serial,
                sealing: false,
            },
        );
        Ok(AllocJob {
            conn: c,
            reply_to: env.id.clone(),
            serial,
            len,
            owner,
        })
    }

    pub(crate) fn finish_allocate(&mut self, job: AllocJob, created: std::io::Result<()>) {
        let live = self.conns.get(&job.conn).is_some_and(|conn| {
            matches!(
                conn.owners.get(&OwnerKey::Hold(job.owner)),
                Some(Owner::Writer { .. })
            )
        });
        match created {
            Err(e) => {
                if live {
                    self.take_owner(job.conn, OwnerKey::Hold(job.owner));
                }
                if let Some(a) = self.artifacts.remove(&job.serial) {
                    self.store_bytes -= a.byte_length;
                }
                self.reply(
                    job.conn,
                    &job.reply_to,
                    "artifact.allocate",
                    err(ErrorCode::StoreFailure, format!("staging: {e}")),
                );
            }
            Ok(()) if !live || !self.artifacts.contains_key(&job.serial) => {
                // The connection went away while the file was being made.
                self.unlinks.push(staging_rel(job.serial));
            }
            Ok(()) => {
                let loc = crate::wire::Location {
                    store_id: self.store_id.clone(),
                    relative_path: staging_rel(job.serial),
                };
                let value = obj(vec![
                    ("artifactId", serial_id("a", job.serial).into()),
                    ("generation", GENERATION.to_string().into()),
                    ("ownerId", serial_id("own", job.owner).into()),
                    ("writeLocation", loc.to_json()),
                ]);
                self.reply(job.conn, &job.reply_to, "artifact.allocate", Ok(value));
            }
        }
        self.flush_notices();
    }

    fn op_seal(&mut self, c: ConnKey, env: &Envelope) -> Result<SealJob, BusError> {
        let mut f = Fields::of(&env.body, "artifact.seal");
        let artifact_id = f.id("artifactId").map_err(wire)?;
        let generation = f.u64_string("generation").map_err(wire)?;
        let owner_id = f.id("ownerId").map_err(wire)?;
        let digest = match f.value("digest").map_err(wire)? {
            Value::Null => None,
            Value::String(s) if crate::wire::is_digest(s) => Some(s.clone()),
            _ => {
                return err(
                    ErrorCode::InvalidEnvelope,
                    "digest must be null or 64 lowercase hex digits",
                );
            }
        };
        f.finish().map_err(wire)?;
        let Some(serial) = parse_serial_id("a", &artifact_id) else {
            return err(ErrorCode::ArtifactGone, "unknown artifact id");
        };
        let Some(owner) = parse_serial_id("own", &owner_id) else {
            return err(ErrorCode::OwnerInvalid, "ownerId is not a writer");
        };
        match self.conns[&c].owners.get(&OwnerKey::Hold(owner)) {
            Some(Owner::Writer {
                artifact,
                sealing: false,
            }) if *artifact == serial => {}
            Some(Owner::Writer {
                artifact,
                sealing: true,
            }) if *artifact == serial => {
                return err(ErrorCode::OwnerInvalid, "a seal is already in progress");
            }
            Some(Owner::Hold { artifact }) if *artifact == serial => {
                return err(ErrorCode::OwnerInvalid, "the artifact is already sealed");
            }
            _ => {
                return err(
                    ErrorCode::OwnerInvalid,
                    format!("{owner_id} is not the writer of {artifact_id}"),
                );
            }
        }
        if generation != GENERATION {
            return err(ErrorCode::ArtifactGone, "unknown generation");
        }
        let len = self.artifacts[&serial].byte_length;
        if self.store_bytes.saturating_add(len) > self.limits.max_store_bytes {
            self.take_owner(c, OwnerKey::Hold(owner));
            self.abandon_writer(serial);
            return err(
                ErrorCode::QuotaExceeded,
                "no room for the sealing copy; the staging object was released",
            );
        }
        self.store_bytes += len;
        self.artifacts.get_mut(&serial).expect("present").state = ArtState::Sealing;
        let conn = self.conns.get_mut(&c).expect("present");
        conn.owners.insert(
            OwnerKey::Hold(owner),
            Owner::Writer {
                artifact: serial,
                sealing: true,
            },
        );
        Ok(SealJob {
            conn: c,
            reply_to: env.id.clone(),
            serial,
            len,
            digest,
            owner,
        })
    }

    pub(crate) fn finish_seal(
        &mut self,
        job: SealJob,
        result: Result<(), crate::store::SealFailure>,
    ) {
        let owner_live = self.conns.get(&job.conn).is_some_and(|conn| {
            matches!(conn.owners.get(&OwnerKey::Hold(job.owner)), Some(Owner::Writer { artifact, sealing: true }) if *artifact == job.serial)
        });
        let reply = match (result, owner_live) {
            (Ok(()), true) => {
                let art = self
                    .artifacts
                    .get_mut(&job.serial)
                    .expect("sealing artifact is registered");
                art.state = ArtState::Sealed;
                art.digest = job.digest.clone();
                art.roots = 1;
                // The seal task already unlinked staging; its reservation ends here.
                self.store_bytes -= job.len;
                let conn = self.conns.get_mut(&job.conn).expect("present");
                conn.owners.insert(
                    OwnerKey::Hold(job.owner),
                    Owner::Hold {
                        artifact: job.serial,
                    },
                );
                let r = self.reference(job.serial).expect("present");
                Ok(obj(vec![
                    ("ref", r.to_json()),
                    ("ownerId", serial_id("own", job.owner).into()),
                ]))
            }
            (result, _) => {
                if self.artifacts.remove(&job.serial).is_some() {
                    self.store_bytes -= 2 * job.len;
                }
                self.unlinks.push(sealed_rel(job.serial));
                if owner_live {
                    self.take_owner(job.conn, OwnerKey::Hold(job.owner));
                }
                match result {
                    Ok(()) => err(
                        ErrorCode::ArtifactGone,
                        "the writer was released during the seal",
                    ),
                    Err(crate::store::SealFailure::Mismatch(m)) => {
                        err(ErrorCode::ArtifactMismatch, m)
                    }
                    Err(crate::store::SealFailure::Io(e)) => {
                        err(ErrorCode::StoreFailure, format!("seal: {e}"))
                    }
                }
            }
        };
        self.reply(job.conn, &job.reply_to, "artifact.seal", reply);
        self.flush_notices();
    }

    // -----------------------------------------------------------------------------------------
    // Writers pull frames

    fn topic_ready(&self, c: ConnKey) -> bool {
        if !self.ordinary_budget_left(c) {
            return false;
        }
        let conn = &self.conns[&c];
        conn.subs
            .values()
            .any(|s| s.in_flight < s.max_in_flight && !s.queue.is_empty())
    }

    /// The next frame for `c`: control first, then RPC, then topic data, with topic data
    /// guaranteed a turn after [`TOPIC_STARVATION_LIMIT`] higher-priority frames.
    pub(crate) fn next_frame(&mut self, c: ConnKey) -> NextFrame {
        if !self.conns.contains_key(&c) {
            return NextFrame::Gone;
        }
        let topic_ready = self.topic_ready(c);
        let starving = topic_ready && self.conns[&c].streak >= TOPIC_STARVATION_LIMIT;
        if !starving && let Some(f) = self.pop_control(c).or_else(|| self.dispatch_rpc(c)) {
            if topic_ready {
                self.conns.get_mut(&c).expect("present").streak += 1;
            }
            return self.selected_frame(c, f);
        }
        if let Some(f) = self.dispatch_topic(c) {
            self.conns.get_mut(&c).expect("present").streak = 0;
            return self.selected_frame(c, f);
        }
        match self.pop_control(c).or_else(|| self.dispatch_rpc(c)) {
            Some(f) => self.selected_frame(c, f),
            None => NextFrame::Idle,
        }
    }

    fn selected_frame(&mut self, c: ConnKey, frame: Outbound) -> NextFrame {
        match self.encode_outbound(c, frame) {
            Some(bytes) => NextFrame::Frame(bytes),
            None => NextFrame::Gone,
        }
    }

    fn pop_control(&mut self, c: ConnKey) -> Option<Outbound> {
        let conn = self.conns.get_mut(&c)?;
        let f = conn.control.pop_front()?;
        conn.control_bytes -= f.bytes;
        Some(f)
    }

    fn dispatch_rpc(&mut self, c: ConnKey) -> Option<Outbound> {
        if self.conns[&c].owners.len() + self.conns[&c].reply_capabilities
            >= self.limits.max_owners_per_client
        {
            return None;
        }
        // Results for calls this connection made.
        if let Some(key) = self.conns.get_mut(&c)?.result_queue.pop_front() {
            let call = self.calls.get_mut(&key)?;
            let Phase::Replied { delivery, result } =
                std::mem::replace(&mut call.phase, Phase::Queued)
            else {
                unreachable!("queued results belong to replied calls");
            };
            let CallResult::Queued(msg) = result else {
                unreachable!("result queued once")
            };
            call.phase = Phase::Replied {
                delivery,
                result: CallResult::Delivered,
            };
            let conn = self.conns.get_mut(&c).expect("present");
            conn.delivery_issued += 1;
            let n = conn.delivery_issued;
            let did = serial_id("dlv", n);
            let call = &self.calls[&key];
            let body = call.result_body(&did, &msg);
            let atts = attachments_with_owner(&msg.attachments, &did);
            self.insert_owner(
                c,
                OwnerKey::Delivery(n),
                Owner::Delivery {
                    artifacts: msg.artifacts,
                    kind: DeliveryKind::Result { call: key },
                },
            );
            return self.outbound(Kind::Delivery, "rpc.result", None, body, atts);
        }
        // Requests for services this connection registered, round robin.
        let conn = &self.conns[&c];
        let n_svcs = conn.services.len();
        for i in 0..n_svcs {
            let idx = (conn.svc_cursor + i) % n_svcs;
            let name = conn.services[idx].clone();
            let svc = self.services.get_mut(&name)?;
            if svc.in_flight >= svc.max_in_flight || svc.queue.is_empty() {
                continue;
            }
            if self.conns[&c].owners.len() + self.conns[&c].reply_capabilities + 2
                > self.limits.max_owners_per_client
            {
                continue;
            }
            let key = svc.queue.pop_front().expect("nonempty");
            svc.in_flight += 1;
            let incarnation = svc.incarnation.clone();
            let conn = self.conns.get_mut(&c).expect("present");
            conn.svc_cursor = idx + 1;
            conn.delivery_issued += 1;
            let n = conn.delivery_issued;
            conn.requests.insert(n, key);
            conn.reply_capabilities += 1;
            let call = self.calls.get_mut(&key).expect("queued call is registered");
            // Dispatched before a byte of it is written.
            call.phase = Phase::Dispatched { delivery: n };
            call.reply_capability = true;
            let did = serial_id("dlv", n);
            let body = call.request_body(&did);
            let atts = attachments_with_owner(&call.attachments, &did);
            let artifacts = call.artifacts.clone();
            self.insert_owner(
                c,
                OwnerKey::Delivery(n),
                Owner::Delivery {
                    artifacts,
                    kind: DeliveryKind::Request {
                        call: key,
                        service: name,
                        incarnation,
                    },
                },
            );
            return self.outbound(Kind::Delivery, "rpc.request", None, body, atts);
        }
        None
    }

    fn dispatch_topic(&mut self, c: ConnKey) -> Option<Outbound> {
        if !self.ordinary_budget_left(c) {
            return None;
        }
        let conn = self.conns.get_mut(&c)?;
        let n_subs = conn.sub_order.len();
        for i in 0..n_subs {
            let idx = (conn.sub_cursor + i) % n_subs;
            let id = conn.sub_order[idx];
            let sub = conn.subs.get_mut(&id).expect("ordered subscription exists");
            if sub.in_flight >= sub.max_in_flight || sub.queue.is_empty() {
                continue;
            }
            let msg = sub.queue.pop_front().expect("nonempty");
            sub.in_flight += 1;
            let replaced = std::mem::take(&mut sub.replaced);
            if !sub.latest {
                conn.queued_bytes -= msg.bytes;
            }
            conn.sub_cursor = idx + 1;
            conn.delivery_issued += 1;
            let n = conn.delivery_issued;
            let did = serial_id("dlv", n);
            let body = msg.body(&did, &serial_id("sub", id), replaced);
            let atts = attachments_with_owner(&msg.attachments, &did);
            self.insert_owner(
                c,
                OwnerKey::Delivery(n),
                Owner::Delivery {
                    artifacts: msg.artifacts.clone(),
                    kind: DeliveryKind::Topic { sub: id },
                },
            );
            return self.outbound(Kind::Delivery, "topic.message", None, body, atts);
        }
        None
    }
}

fn reply_body(result: Result<Map<String, Value>, BusError>) -> Map<String, Value> {
    match result {
        Ok(value) => obj(vec![("ok", true.into()), ("value", Value::Object(value))]),
        Err(e) => obj(vec![
            ("ok", false.into()),
            (
                "error",
                Value::Object(obj(vec![
                    ("code", e.code.as_str().into()),
                    ("message", e.message.into()),
                    ("dispatch", e.dispatch.as_str().into()),
                ])),
            ),
        ]),
    }
}
