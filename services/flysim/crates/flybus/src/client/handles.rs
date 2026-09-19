//! RAII handles: artifacts, writers, deliveries, services, subscriptions and pending calls.
//!
//! Every owner the router tracks for this client is behind one `OwnerGuard`. Its last clone
//! dropping queues `delivery.consumed` (for a delivery) or `artifact.release` (for a hold or
//! writer). A message, the artifacts extracted from it and any open files share the message's
//! guard, so a delivery is consumed only when all of them are gone.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

use super::reactor::{CallSlot, ClientConn, Control, Extra, Hook, OutCommand};
use crate::error::{BusError, Dispatch, ErrorCode};
use crate::store::{open_read, resolve};
use crate::wire::{ArtifactRef, Attachment, Fields, GENERATION, Identity, Location};

pub(crate) struct OwnerGuard {
    pub id: String,
    pub delivery: bool,
    pub conn: Arc<ClientConn>,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        let id = std::mem::take(&mut self.id);
        let item = if self.delivery {
            Control::Consumed(id)
        } else {
            Control::Release(id)
        };
        self.conn.shared.push_control(item);
    }
}

pub(crate) fn attachment_list(
    list: &[(&str, &Artifact)],
) -> (Vec<Attachment>, Vec<Arc<OwnerGuard>>) {
    let atts = list
        .iter()
        .map(|(name, a)| Attachment {
            name: (*name).to_owned(),
            reference: a.reference.as_ref().clone(),
            owner_id: a.owner.id.clone(),
        })
        .collect();
    let keep = list.iter().map(|(_, a)| a.owner.clone()).collect();
    (atts, keep)
}

fn find(
    list: &[(String, ArtifactRef)],
    guard: &Arc<OwnerGuard>,
    name: &str,
) -> Result<Artifact, BusError> {
    list.iter()
        .find(|(n, _)| n == name)
        .map(|(_, r)| Artifact {
            reference: Arc::new(r.clone()),
            owner: guard.clone(),
        })
        .ok_or_else(|| BusError::invalid(format!("no attachment named {name:?}")))
}

// ---------------------------------------------------------------------------------------------
// Artifacts

/// A read-only, cloneable handle on an immutable artifact. While any clone (or a file opened
/// from it) lives, the owner it came from keeps the bytes alive.
#[derive(Clone)]
pub struct Artifact {
    pub(crate) reference: Arc<ArtifactRef>,
    pub(crate) owner: Arc<OwnerGuard>,
}

impl std::fmt::Debug for Artifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifact")
            .field("reference", &self.reference)
            .field("owner", &self.owner.id)
            .finish()
    }
}

impl Artifact {
    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    /// The router-side owner this handle rides on: a delivery id or an explicit hold id.
    pub fn owner_id(&self) -> &str {
        &self.owner.id
    }

    /// Opens the sealed bytes read-only. The file keeps this handle (and so its owner) alive.
    pub async fn open(&self) -> Result<ArtifactFile, BusError> {
        let mut body = Map::new();
        body.insert("ref".into(), self.reference.to_json());
        body.insert("ownerId".into(), self.owner.id.clone().into());
        let shared = &self.owner.conn.shared;
        let reply = shared
            .command(
                "artifact.open",
                body,
                Vec::new(),
                vec![self.owner.clone()],
                Hook::None,
            )
            .await?;
        let loc = Location::from_json(Fields::of(&reply.value, "reply").value("readLocation")?)?;
        if loc.store_id != self.reference.store_id {
            return Err(BusError::new(
                ErrorCode::ArtifactGone,
                "read location names another store",
            ));
        }
        let path = resolve(&shared.store_root, &loc.store_id, &loc)?;
        let file = open_read(&path)
            .map_err(|e| BusError::new(ErrorCode::StoreFailure, format!("open: {e}")))?;
        let len = file
            .metadata()
            .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()))?
            .len();
        if len != self.reference.byte_length {
            return Err(BusError::new(
                ErrorCode::ArtifactMismatch,
                "sealed file length disagrees with the reference",
            ));
        }
        Ok(ArtifactFile {
            file,
            _artifact: self.clone(),
        })
    }

    /// Reads the whole artifact (on the blocking pool).
    pub async fn read_all(&self) -> Result<Vec<u8>, BusError> {
        let mut file = self.open().await?;
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::with_capacity(file.len() as usize);
            file.read_to_end(&mut out).map(|_| out)
        })
        .await
        .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()))?
        .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()))
    }

    /// Creates an independent explicit hold, so the bytes outlive this handle's delivery.
    pub async fn retain(&self) -> Result<Artifact, BusError> {
        let mut body = Map::new();
        body.insert("ref".into(), self.reference.to_json());
        body.insert("ownerId".into(), self.owner.id.clone().into());
        let shared = &self.owner.conn.shared;
        let reply = shared
            .command(
                "artifact.retain",
                body,
                Vec::new(),
                vec![self.owner.clone()],
                Hook::Owner,
            )
            .await?;
        match reply.extra {
            Extra::Owner(owner) => Ok(Artifact {
                reference: self.reference.clone(),
                owner,
            }),
            _ => Err(BusError::lost("retain reply without an owner")),
        }
    }
}

/// An open, read-only sealed file. Holds its [`Artifact`].
pub struct ArtifactFile {
    file: File,
    _artifact: Artifact,
}

impl ArtifactFile {
    pub fn len(&self) -> u64 {
        self._artifact.reference.byte_length
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn artifact(&self) -> &Artifact {
        &self._artifact
    }
}

impl Read for ArtifactFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for ArtifactFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

/// The unique writer of a freshly allocated artifact. Not cloneable. Dropping it unsealed
/// releases the staging storage; [`ArtifactWriter::seal`] consumes it.
pub struct ArtifactWriter {
    pub(crate) file: Option<File>,
    pub(crate) owner: Arc<OwnerGuard>,
    pub(crate) artifact_id: String,
    pub(crate) byte_length: u64,
    pub(crate) written: u64,
}

impl ArtifactWriter {
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Closes the writable handle and seals, returning the immutable artifact on an explicit
    /// hold. Bytes not written read as zeros: the staging file is preallocated.
    pub async fn seal(self) -> Result<Artifact, BusError> {
        self.seal_with_digest(None).await
    }

    /// As [`seal`](Self::seal), and the router refuses the seal unless the content's SHA-256
    /// (lowercase hex) equals `digest`.
    pub async fn seal_with_digest(mut self, digest: Option<String>) -> Result<Artifact, BusError> {
        drop(self.file.take());
        let mut body = Map::new();
        body.insert("artifactId".into(), self.artifact_id.clone().into());
        body.insert("generation".into(), GENERATION.to_string().into());
        body.insert("ownerId".into(), self.owner.id.clone().into());
        body.insert("digest".into(), digest.map_or(Value::Null, Value::String));
        let shared = &self.owner.conn.shared;
        let reply = shared
            .command(
                "artifact.seal",
                body,
                Vec::new(),
                vec![self.owner.clone()],
                Hook::None,
            )
            .await?;
        let reference = ArtifactRef::from_json(Fields::of(&reply.value, "reply").value("ref")?)?;
        Ok(Artifact {
            reference: Arc::new(reference),
            owner: self.owner.clone(),
        })
    }
}

impl Write for ArtifactWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let room = self.byte_length - self.written;
        if buf.len() as u64 > room {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write past the allocated length",
            ));
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("writer is closed"))?;
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

// ---------------------------------------------------------------------------------------------
// Deliveries

/// One `topic.message` delivery. Dropping it, and every artifact taken from it, consumes the
/// delivery and returns its credit.
pub struct Message {
    pub(crate) guard: Arc<OwnerGuard>,
    pub(crate) subscription_id: String,
    pub(crate) topic: String,
    pub(crate) topic_incarnation: String,
    pub(crate) topic_sequence: u64,
    pub(crate) replaced: u64,
    pub(crate) payload: Map<String, Value>,
    pub(crate) attachments: Vec<(String, ArtifactRef)>,
}

impl Message {
    pub fn delivery_id(&self) -> &str {
        &self.guard.id
    }
    pub fn subscription_id(&self) -> &str {
        &self.subscription_id
    }
    pub fn topic(&self) -> &str {
        &self.topic
    }
    pub fn topic_incarnation(&self) -> &str {
        &self.topic_incarnation
    }
    pub fn topic_sequence(&self) -> u64 {
        self.topic_sequence
    }
    /// Undelivered messages coalesced into this one since the previous delivery.
    pub fn replaced(&self) -> u64 {
        self.replaced
    }
    pub fn payload(&self) -> &Map<String, Value> {
        &self.payload
    }
    pub fn attachment_names(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().map(|(n, _)| n.as_str())
    }
    /// An artifact handle sharing this delivery's guard.
    pub fn artifact(&self, name: &str) -> Result<Artifact, BusError> {
        find(&self.attachments, &self.guard, name)
    }
}

/// An incoming `rpc.request`. Dropping it (and its artifacts) consumes the request delivery;
/// that is independent of replying.
pub struct Request {
    pub(crate) guard: Arc<OwnerGuard>,
    pub(crate) reply_guard: Arc<ReplyGuard>,
    pub(crate) call_id: String,
    pub(crate) caller: Identity,
    pub(crate) target: String,
    pub(crate) service_incarnation: String,
    pub(crate) method: String,
    pub(crate) payload: Map<String, Value>,
    pub(crate) attachments: Vec<(String, ArtifactRef)>,
}

impl Request {
    pub fn delivery_id(&self) -> &str {
        &self.guard.id
    }
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
    /// The caller identity supplied by the router. It is authenticated only when the router
    /// accepted this connection through a launcher-bound `*_as` transport entry point.
    pub fn caller(&self) -> &Identity {
        &self.caller
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn service_incarnation(&self) -> &str {
        &self.service_incarnation
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn payload(&self) -> &Map<String, Value> {
        &self.payload
    }
    pub fn artifact(&self, name: &str) -> Result<Artifact, BusError> {
        find(&self.attachments, &self.guard, name)
    }
    /// A reply capability that keeps bounded router correlation alive independently of the
    /// request delivery credit.
    pub fn responder(&self) -> Responder {
        Responder {
            guard: self.reply_guard.clone(),
            call_id: self.call_id.clone(),
            request_delivery_id: self.guard.id.clone(),
        }
    }
    /// Replies. `Ok(true)` if routed to the caller, `Ok(false)` if the caller had detached.
    pub async fn reply(
        &self,
        outcome: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
    ) -> Result<bool, BusError> {
        self.responder().reply(outcome, attachments).await
    }
}

/// Replies to one request, whether or not the request delivery is still held.
#[derive(Clone)]
pub struct Responder {
    guard: Arc<ReplyGuard>,
    call_id: String,
    request_delivery_id: String,
}

impl Responder {
    pub async fn reply(
        &self,
        outcome: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
    ) -> Result<bool, BusError> {
        let (atts, keep) = attachment_list(attachments);
        let mut body = Map::new();
        body.insert("callId".into(), self.call_id.clone().into());
        body.insert(
            "requestDeliveryId".into(),
            self.request_delivery_id.clone().into(),
        );
        body.insert("outcome".into(), Value::Object(outcome));
        let reply = self
            .guard
            .conn
            .shared
            .command("rpc.reply", body, atts, keep, Hook::None)
            .await?;
        Ok(Fields::of(&reply.value, "reply").boolean("routed")?)
    }
}

pub(crate) struct ReplyGuard {
    pub conn: Arc<ClientConn>,
    pub call_id: String,
    pub request_delivery_id: String,
}

impl Drop for ReplyGuard {
    fn drop(&mut self) {
        self.conn.shared.push_control(Control::ResponderReleased {
            call_id: self.call_id.clone(),
            request_delivery_id: self.request_delivery_id.clone(),
        });
    }
}

/// A terminal `rpc.result`. Dropping it (and its artifacts) consumes the result delivery.
pub struct RpcResult {
    pub(crate) guard: Arc<OwnerGuard>,
    pub(crate) call_id: String,
    pub(crate) responder: Identity,
    pub(crate) service_incarnation: String,
    pub(crate) outcome: Map<String, Value>,
    pub(crate) attachments: Vec<(String, ArtifactRef)>,
}

impl RpcResult {
    pub fn delivery_id(&self) -> &str {
        &self.guard.id
    }
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
    pub fn responder(&self) -> &Identity {
        &self.responder
    }
    pub fn service_incarnation(&self) -> &str {
        &self.service_incarnation
    }
    pub fn outcome(&self) -> &Map<String, Value> {
        &self.outcome
    }
    pub fn artifact(&self, name: &str) -> Result<Artifact, BusError> {
        find(&self.attachments, &self.guard, name)
    }
}

// ---------------------------------------------------------------------------------------------
// Services, subscriptions, calls

pub(crate) struct ServiceGuard {
    pub conn: Arc<ClientConn>,
    pub incarnation: String,
}

/// A registered service endpoint. Dropping it unregisters.
pub struct Service {
    pub(crate) name: String,
    pub(crate) rx: mpsc::UnboundedReceiver<Request>,
    pub(crate) guard: ServiceGuard,
}

impl Service {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn incarnation(&self) -> &str {
        &self.guard.incarnation
    }
    /// The next request, or `None` once the connection is closed.
    pub async fn next(&mut self) -> Option<Request> {
        self.rx.recv().await
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let shared = &self.guard.conn.shared;
        let tx = shared.lock().services.remove(&self.guard.incarnation);
        drop(tx);
        // Route removal must precede abandoning buffered requests, whose last responder drops
        // queue responder-release controls. This preserves NO_SERVICE as their terminal cause.
        shared.push_control(Control::Unregister {
            name: self.name.clone(),
            service_incarnation: self.guard.incarnation.clone(),
        });
        self.rx.close();
        while let Ok(request) = self.rx.try_recv() {
            drop(request);
        }
    }
}

pub(crate) struct SubscriptionGuard {
    pub conn: Arc<ClientConn>,
    pub id: String,
}

/// An exact-topic subscription. Dropping it unsubscribes; messages already handed out stay
/// valid.
pub struct Subscription {
    pub(crate) rx: mpsc::UnboundedReceiver<Message>,
    pub(crate) guard: SubscriptionGuard,
    pub(crate) topic_incarnation: String,
}

impl Subscription {
    pub fn id(&self) -> &str {
        &self.guard.id
    }
    pub fn topic_incarnation(&self) -> &str {
        &self.topic_incarnation
    }
    /// The next message, or `None` once the subscription or connection is closed.
    pub async fn next(&mut self) -> Option<Message> {
        self.rx.recv().await
    }
    /// A message already delivered to this client, without waiting.
    pub fn try_next(&mut self) -> Option<Message> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let shared = &self.guard.conn.shared;
        let tx = shared.lock().subscriptions.remove(&self.guard.id);
        drop(tx);
        let mut body = Map::new();
        body.insert("subscriptionId".into(), self.guard.id.clone().into());
        let cmd = OutCommand {
            op: "unsubscribe",
            body,
            attachments: Vec::new(),
            respond: None,
            hook: Hook::None,
            keep: Vec::new(),
        };
        let _ = shared.enqueue(cmd);
    }
}

pub(crate) struct CallGuard {
    pub conn: Arc<ClientConn>,
    pub call_id: String,
    pub done: bool,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let shared = &self.conn.shared;
        let waiting = {
            let mut st = shared.lock();
            match st.calls.remove(&self.call_id) {
                Some(slot @ CallSlot::Waiting(_)) => {
                    st.calls.insert(self.call_id.clone(), CallSlot::Abandoned);
                    Some(slot)
                }
                other => {
                    if let Some(o) = other {
                        st.calls.insert(self.call_id.clone(), o);
                    }
                    None
                }
            }
        };
        if waiting.is_some() {
            // Best effort; a result that still arrives is consumed by the reactor.
            shared.push_control(Control::Cancel(self.call_id.clone(), None));
        }
    }
}

macro_rules! debug_fields {
    ($ty:ident, |$s:ident| { $($name:literal => $val:expr),* $(,)? }) => {
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let $s = self;
                f.debug_struct(stringify!($ty))$(.field($name, &$val))*.finish()
            }
        }
    };
}

debug_fields!(ArtifactFile, |s| { "artifact" => s._artifact });
debug_fields!(ArtifactWriter, |s| { "artifact_id" => s.artifact_id, "byte_length" => s.byte_length, "owner" => s.owner.id });
debug_fields!(Message, |s| {
    "delivery_id" => s.guard.id, "topic" => s.topic, "topic_sequence" => s.topic_sequence, "replaced" => s.replaced,
});
debug_fields!(Request, |s| { "delivery_id" => s.guard.id, "call_id" => s.call_id, "method" => s.method });
debug_fields!(RpcResult, |s| { "delivery_id" => s.guard.id, "call_id" => s.call_id });
debug_fields!(Responder, |s| { "call_id" => s.call_id, "request_delivery_id" => s.request_delivery_id });
debug_fields!(Service, |s| { "name" => s.name, "incarnation" => s.guard.incarnation });
debug_fields!(Subscription, |s| { "id" => s.guard.id, "topic_incarnation" => s.topic_incarnation });
debug_fields!(PendingCall, |s| { "call_id" => s.guard.call_id, "service_incarnation" => s.service_incarnation });

/// The router's answer to `rpc.cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelState {
    CancelledBeforeDispatch,
    ExecutionUnknown,
    Completed,
    CallGone,
}

/// An admitted call. [`result`](Self::result) waits for the terminal result. Dropping it
/// unfinished sends a best-effort `rpc.cancel` and consumes any result that still arrives.
pub struct PendingCall {
    pub(crate) guard: CallGuard,
    pub(crate) rx: oneshot::Receiver<Result<RpcResult, BusError>>,
    pub(crate) service_incarnation: String,
}

impl PendingCall {
    pub fn call_id(&self) -> &str {
        &self.guard.call_id
    }

    pub fn service_incarnation(&self) -> &str {
        &self.service_incarnation
    }

    /// Waits for the result. Cancel-safe: wrap it in a timeout and call it again, or cancel.
    pub async fn result(&mut self) -> Result<RpcResult, BusError> {
        if self.guard.done {
            return Err(BusError::new(ErrorCode::CallGone, "result already taken"));
        }
        let r = (&mut self.rx)
            .await
            .unwrap_or_else(|_| Err(BusError::lost("connection closed")));
        self.guard.done = true;
        r
    }

    /// Asks the router to cancel. Only `CancelledBeforeDispatch` establishes that the handler
    /// never ran. After any state but `Completed`, [`result`](Self::result) fails with
    /// `CALL_GONE`.
    pub async fn cancel(&self) -> Result<CancelState, BusError> {
        let (tx, rx) = oneshot::channel();
        let shared = &self.guard.conn.shared;
        shared.push_control(Control::Cancel(self.guard.call_id.clone(), Some(tx)));
        let reply = rx
            .await
            .unwrap_or_else(|_| Err(BusError::lost("connection closed")))?;
        match Fields::of(&reply.value, "reply").string("state")? {
            "cancelled-before-dispatch" => Ok(CancelState::CancelledBeforeDispatch),
            "execution-unknown" => Ok(CancelState::ExecutionUnknown),
            "completed" => Ok(CancelState::Completed),
            "call-gone" => Ok(CancelState::CallGone),
            other => Err(BusError::lost(format!("unknown cancel state {other:?}"))
                .with_dispatch(Dispatch::Unknown)),
        }
    }
}
