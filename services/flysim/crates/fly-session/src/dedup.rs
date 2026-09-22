//! Domain deduplication and the result cache of `ipc-v1` section 5.
//!
//! The operation key for a step mutation is `(sessionId, epoch, step, method, workerId)`, and
//! there is at most one Prepare, Commit or Advance for it. The cache decides, *before* any
//! phase check or artifact dereference, whether an arriving request is the original, a safe
//! replay, a conflicting change, a duplicate of something still running, or a retry whose
//! result is gone.
//!
//! A cached reply owns its artifacts through explicit holds, so a replay is still valid after
//! the first caller consumed its delivery. Dropping the record drops those holds.

use std::collections::{BTreeMap, VecDeque};

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// `ipc-v1` section 5: unacknowledged lifecycle replies are bounded at 16, then BUSY.
pub const MAX_UNACKNOWLEDGED: usize = 16;
/// `ipc-v1` section 5: Status and Acknowledge keep a cache of their last 16 replies.
pub const MAX_READONLY_REPLIES: usize = 16;
/// `ipc-v1` section 5: keep the current and the immediately previous step's records.
pub const RETAINED_STEPS: u64 = 2;

/// Which retention rule an operation follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpClass {
    /// Prepare, Commit, Advance: keyed by scope and retained for two steps.
    StepMutation,
    /// Initialize and capture: retained until `Worker.Acknowledge`.
    Lifecycle,
    /// Status and Acknowledge: a small last-16 reply cache, no mutation key.
    ReadOnly,
}

/// A step mutation's identity. Not a bus call id and not a batch id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationKey {
    pub session_id: Id,
    pub epoch: Id,
    pub step: u64,
    pub method: String,
    pub worker_id: Id,
}

/// A terminal domain reply plus the artifact holds that keep its attachments readable.
#[derive(Clone)]
pub struct CachedReply {
    pub outcome: SessionRpcOutcome,
    pub artifacts: Vec<(String, flybus::Artifact)>,
}

impl CachedReply {
    pub fn new(outcome: SessionRpcOutcome) -> CachedReply {
        CachedReply { outcome, artifacts: Vec::new() }
    }

    pub fn with_artifacts(
        outcome: SessionRpcOutcome,
        artifacts: Vec<(String, flybus::Artifact)>,
    ) -> CachedReply {
        CachedReply { outcome, artifacts }
    }

    /// The attachment list for a fresh `rpc.reply`, over the same immutable bytes.
    pub fn attachments(&self) -> Vec<(&str, &flybus::Artifact)> {
        self.artifacts.iter().map(|(n, a)| (n.as_str(), a)).collect()
    }
}

impl std::fmt::Debug for CachedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedReply")
            .field("outcome", &self.outcome)
            .field("artifacts", &self.artifacts.len())
            .finish()
    }
}

/// What the cache decided about an arriving request.
#[derive(Debug)]
pub enum Admission {
    /// The original: run it, then `record` its reply.
    Execute,
    /// A safe replay: return this reply again, with fresh delivery ownership.
    Replay(CachedReply),
    /// A terminal domain error for this bus call, with no second mutation started.
    Refuse(DomainError),
}

#[derive(Clone, Debug)]
struct Record {
    request_id: DomainRequestId,
    body: Digest,
    reply: CachedReply,
}

/// One worker's domain request cache.
pub struct ResultCache {
    steps: BTreeMap<OperationKey, Record>,
    active: BTreeMap<OperationKey, (DomainRequestId, Digest)>,
    lifecycle: BTreeMap<String, Record>,
    lifecycle_order: VecDeque<String>,
    readonly: VecDeque<(String, CachedReply)>,
    highest_serial: Option<u64>,
    step_watermark: Option<u64>,
    /// Serials retired by `Worker.Acknowledge`; reuse below this is refused without keeping a
    /// tombstone per request.
    acknowledged_watermark: Option<u64>,
}

impl Default for ResultCache {
    fn default() -> ResultCache {
        ResultCache::new()
    }
}

impl ResultCache {
    pub fn new() -> ResultCache {
        ResultCache {
            steps: BTreeMap::new(),
            active: BTreeMap::new(),
            lifecycle: BTreeMap::new(),
            lifecycle_order: VecDeque::new(),
            readonly: VecDeque::new(),
            highest_serial: None,
            step_watermark: None,
            acknowledged_watermark: None,
        }
    }

    /// Decides what to do with an arriving request. Identity is checked before phase.
    pub fn admit(
        &mut self,
        class: OpClass,
        key: &OperationKey,
        request: DomainRequestId,
        body: &Digest,
    ) -> Admission {
        match class {
            OpClass::StepMutation => self.admit_step(key, request, body),
            OpClass::Lifecycle => self.admit_lifecycle(request, body),
            OpClass::ReadOnly => Admission::Execute,
        }
    }

    fn admit_step(&mut self, key: &OperationKey, request: DomainRequestId, body: &Digest) -> Admission {
        if let Some(record) = self.steps.get(key) {
            if record.request_id == request && record.body == *body {
                return Admission::Replay(record.reply.clone());
            }
            return Admission::Refuse(DomainError::new(
                ErrorCode::Conflict,
                "this operation key already holds a different request or body",
                // The earlier result stands; refusing the duplicate undoes nothing.
                MutationCertainty::None,
            ));
        }
        if !self.active.is_empty() && !self.active.contains_key(key) {
            // One mutation executes at a time. A second, different one is refused before
            // admission rather than queued behind the first.
            return Admission::Refuse(DomainError::before(
                ErrorCode::Busy,
                "another step mutation is already executing on this worker",
            ));
        }
        if let Some((active_request, active_body)) = self.active.get(key) {
            if *active_request == request && *active_body == *body {
                // The duplicate bus call started no work at all, so its certainty is none;
                // it must not be mistaken for the original operation's failure.
                return Admission::Refuse(DomainError::before(
                    ErrorCode::InProgress,
                    "the original operation is still executing; this duplicate started no work",
                ));
            }
            return Admission::Refuse(DomainError::new(
                ErrorCode::Conflict,
                "an operation with a different body is active for this key",
                MutationCertainty::None,
            ));
        }
        // No record and nothing active. Eviction must never re-enable execution, so a step
        // below the retention window is refused on the serial and step watermarks.
        if let Some(watermark) = self.step_watermark
            && key.step + RETAINED_STEPS <= watermark
        {
            let issued = self.highest_serial.is_some_and(|h| request.serial() <= h);
            return Admission::Refuse(if issued {
                DomainError::new(
                    ErrorCode::ResultExpired,
                    "the retained result for this request is gone; it is never recomputed",
                    MutationCertainty::Unknown,
                )
            } else {
                DomainError::before(
                    ErrorCode::StaleStep,
                    "a newly issued request cannot name a step the worker has left behind",
                )
            });
        }
        if let Some(watermark) = self.acknowledged_watermark
            && request.serial() <= watermark
        {
            return Admission::Refuse(DomainError::new(
                ErrorCode::ResultExpired,
                "this request serial was acknowledged and cannot be reused",
                MutationCertainty::Unknown,
            ));
        }
        self.begin(key.clone(), request, body.clone());
        Admission::Execute
    }

    fn admit_lifecycle(&mut self, request: DomainRequestId, body: &Digest) -> Admission {
        let id = request.as_str().to_owned();
        if let Some(record) = self.lifecycle.get(&id) {
            if record.body == *body {
                return Admission::Replay(record.reply.clone());
            }
            return Admission::Refuse(DomainError::before(
                ErrorCode::Conflict,
                "this lifecycle request id already holds a different body",
            ));
        }
        if let Some(watermark) = self.acknowledged_watermark
            && request.serial() <= watermark
        {
            return Admission::Refuse(DomainError::new(
                ErrorCode::ResultExpired,
                "this request serial was acknowledged and cannot be reused",
                MutationCertainty::Unknown,
            ));
        }
        if self.lifecycle.len() >= MAX_UNACKNOWLEDGED {
            return Admission::Refuse(DomainError::before(
                ErrorCode::Busy,
                "16 lifecycle replies are unacknowledged; acknowledge some before sending more",
            ));
        }
        Admission::Execute
    }

    fn begin(&mut self, key: OperationKey, request: DomainRequestId, body: Digest) {
        self.highest_serial = Some(self.highest_serial.map_or(request.serial(), |h| h.max(request.serial())));
        self.step_watermark = Some(self.step_watermark.map_or(key.step, |w| w.max(key.step)));
        self.active.insert(key, (request, body));
    }

    /// Stores a terminal reply for a step mutation and prunes what has aged out.
    pub fn record(
        &mut self,
        key: OperationKey,
        request: DomainRequestId,
        body: Digest,
        reply: CachedReply,
    ) {
        self.active.remove(&key);
        let step = key.step;
        self.steps.insert(key, Record { request_id: request, body, reply });
        self.step_watermark = Some(self.step_watermark.map_or(step, |w| w.max(step)));
        self.prune();
    }

    /// Stores a lifecycle reply, retained until `Worker.Acknowledge`.
    pub fn record_lifecycle(&mut self, request: DomainRequestId, body: Digest, reply: CachedReply) {
        let id = request.as_str().to_owned();
        self.highest_serial = Some(self.highest_serial.map_or(request.serial(), |h| h.max(request.serial())));
        if self.lifecycle.insert(id.clone(), Record { request_id: request, body, reply }).is_none()
        {
            self.lifecycle_order.push_back(id);
        }
    }

    /// Stores a read-only reply in the last-16 cache.
    pub fn record_readonly(&mut self, request: DomainRequestId, reply: CachedReply) {
        let id = request.as_str().to_owned();
        self.readonly.retain(|(existing, _)| *existing != id);
        self.readonly.push_back((id, reply));
        while self.readonly.len() > MAX_READONLY_REPLIES {
            self.readonly.pop_front();
        }
    }

    /// Releases an operation that did not mutate anything, so the key stays free.
    pub fn abandon(&mut self, key: &OperationKey) {
        self.active.remove(key);
    }

    /// `Worker.Acknowledge`: drops those lifecycle records, ignoring unknown ids, and raises
    /// the serial watermark so an acknowledged id cannot be reused.
    pub fn acknowledge(&mut self, ids: &[DomainRequestId]) -> Vec<DomainRequestId> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(record) = self.lifecycle.remove(id.as_str()) {
                self.lifecycle_order.retain(|existing| existing != id.as_str());
                self.acknowledged_watermark = Some(
                    self.acknowledged_watermark
                        .map_or(record.request_id.serial(), |w| w.max(record.request_id.serial())),
                );
                out.push(id.clone());
            }
        }
        out
    }

    pub fn unacknowledged(&self) -> usize {
        self.lifecycle.len()
    }

    pub fn retained_steps(&self) -> usize {
        self.steps.len()
    }

    pub fn has_active(&self) -> bool {
        !self.active.is_empty()
    }

    /// Deliberately drops a step record, so a retry meets RESULT_EXPIRED instead of a replay.
    pub fn expire_step(&mut self, key: &OperationKey) -> bool {
        self.steps.remove(key).is_some()
    }

    fn prune(&mut self) {
        let Some(watermark) = self.step_watermark else { return };
        let floor = watermark.saturating_sub(RETAINED_STEPS - 1);
        self.steps.retain(|key, _| key.step >= floor);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    
    fn key(step: u64, method: &str) -> OperationKey {
        OperationKey {
            session_id: id("demo"),
            epoch: id("e1"),
            step,
            method: method.to_owned(),
            worker_id: id("fly-a"),
        }
    }

    fn reply(tag: &str) -> CachedReply {
        let mut result = serde_json::Map::new();
        result.insert("tag".into(), tag.into());
        CachedReply::new(SessionRpcOutcome::Success(SessionRpcSuccess {
            request_id: DomainRequestId::from_serial(1),
            worker_id: id("fly-a"),
            incarnation_id: id("inc-1"),
            scope: Some(scope_at("demo", "e1", 0)),
            result: Value::Object(result),
        }))
    }

    fn body(s: &str) -> Digest {
        digest_of_bytes(s.as_bytes())
    }

    #[test]
    fn the_same_key_request_and_body_replays() {
        let mut c = ResultCache::new();
        let k = key(0, "Agent.Prepare");
        assert!(matches!(c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("a")), Admission::Execute));
        c.record(k.clone(), DomainRequestId::from_serial(1), body("a"), reply("first"));
        match c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("a")) {
            Admission::Replay(r) => {
                assert_eq!(r.outcome.result().unwrap()["tag"], "first");
            }
            other => panic!("wanted a replay, got {other:?}"),
        }
    }

    #[test]
    fn a_changed_body_for_a_recorded_key_is_a_conflict() {
        let mut c = ResultCache::new();
        let k = key(0, "Environment.Advance");
        c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("a"));
        c.record(k.clone(), DomainRequestId::from_serial(1), body("a"), reply("first"));
        match c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("b")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::Conflict),
            other => panic!("wanted CONFLICT, got {other:?}"),
        }
    }

    #[test]
    fn a_duplicate_while_the_original_runs_is_in_progress() {
        let mut c = ResultCache::new();
        let k = key(0, "Agent.Prepare");
        c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("a"));
        match c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(1), &body("a")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::InProgress),
            other => panic!("wanted IN_PROGRESS, got {other:?}"),
        }
    }

    #[test]
    fn an_evicted_step_gives_result_expired_and_a_fresh_serial_gives_stale_step() {
        let mut c = ResultCache::new();
        for step in 0..4u64 {
            let k = key(step, "Agent.Prepare");
            c.admit(OpClass::StepMutation, &k, DomainRequestId::from_serial(step + 1), &body("a"));
            c.record(k, DomainRequestId::from_serial(step + 1), body("a"), reply("x"));
        }
        assert_eq!(c.retained_steps(), 2);
        // req-1 named step 0, whose record is long gone.
        match c.admit(OpClass::StepMutation, &key(0, "Agent.Prepare"), DomainRequestId::from_serial(1), &body("a")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::ResultExpired),
            other => panic!("wanted RESULT_EXPIRED, got {other:?}"),
        }
        // A serial above the highest issued is a new operation naming an old step.
        match c.admit(OpClass::StepMutation, &key(0, "Agent.Prepare"), DomainRequestId::from_serial(99), &body("a")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::StaleStep),
            other => panic!("wanted STALE_STEP, got {other:?}"),
        }
    }

    #[test]
    fn lifecycle_replies_are_bounded_and_released_by_acknowledge() {
        let mut c = ResultCache::new();
        for serial in 1..=MAX_UNACKNOWLEDGED as u64 {
            assert!(matches!(
                c.admit(OpClass::Lifecycle, &key(0, "Agent.Initialize"), DomainRequestId::from_serial(serial), &body("a")),
                Admission::Execute
            ));
            c.record_lifecycle(DomainRequestId::from_serial(serial), body("a"), reply("init"));
        }
        match c.admit(OpClass::Lifecycle, &key(0, "Agent.Initialize"), DomainRequestId::from_serial(99), &body("a")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::Busy),
            other => panic!("wanted BUSY, got {other:?}"),
        }
        let dropped = c.acknowledge(&[
            DomainRequestId::from_serial(1),
            DomainRequestId::from_serial(404),
        ]);
        assert_eq!(dropped, vec![DomainRequestId::from_serial(1)]);
        assert_eq!(c.unacknowledged(), MAX_UNACKNOWLEDGED - 1);
        // The acknowledged serial cannot come back.
        match c.admit(OpClass::Lifecycle, &key(0, "Agent.Initialize"), DomainRequestId::from_serial(1), &body("a")) {
            Admission::Refuse(e) => assert_eq!(e.code, ErrorCode::ResultExpired),
            other => panic!("wanted RESULT_EXPIRED, got {other:?}"),
        }
    }
}
