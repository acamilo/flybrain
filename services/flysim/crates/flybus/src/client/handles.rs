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

use super::reactor::{CallSlot, ClientConn, Control, Extra, Hook, OutCommand, WriterGrant};
use crate::error::{BusError, Dispatch, ErrorCode};
use crate::store::{open_read, open_write, resolve};
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
) -> Result<(Vec<Attachment>, Vec<Arc<OwnerGuard>>), BusError> {
    let mut atts = Vec::with_capacity(list.len());
    let mut keep = Vec::with_capacity(list.len());
    for (name, a) in list {
        let Holder::Bus(owner) = &a.holder else {
            // An in-memory artifact is not in any router's store, so no message may name it.
            return Err(BusError::invalid(format!(
                "attachment {name:?} is an in-memory artifact; seal a copy into the store first"
            ))
            .with_dispatch(Dispatch::NotDispatched));
        };
        atts.push(Attachment {
            name: (*name).to_owned(),
            reference: a.reference.as_ref().clone(),
            owner_id: owner.id.clone(),
            read_location: None,
        });
        keep.push(owner.clone());
    }
    Ok((atts, keep))
}

/// One attachment of a delivery, with the read location the router issued for it (bus-v1
/// section 12 amendment 2026-10-01; `None` from a router that issues none).
#[derive(Clone, Debug)]
pub(crate) struct Delivered {
    pub name: String,
    pub reference: ArtifactRef,
    pub location: Option<Arc<Location>>,
}

fn find(list: &[Delivered], guard: &Arc<OwnerGuard>, name: &str) -> Result<Artifact, BusError> {
    list.iter()
        .find(|d| d.name == name)
        .map(|d| Artifact {
            reference: Arc::new(d.reference.clone()),
            holder: Holder::Bus(guard.clone()),
            location: d.location.clone(),
        })
        .ok_or_else(|| BusError::invalid(format!("no attachment named {name:?}")))
}

// ---------------------------------------------------------------------------------------------
// Artifacts

/// A read-only, cloneable handle on an immutable artifact. While any clone (or a file opened
/// from it) lives, the owner it came from keeps the bytes alive.
///
/// An artifact is either sealed in a router's store (every handle the bus hands out) or an
/// **in-memory** artifact ([`Artifact::in_memory`]): an immutable buffer in this address space
/// that no router knows about. The second kind is for participants that share one process and
/// hand each other buffers directly, without a bus message (PERF-01's in-process local lane);
/// it reads, clones and retains like any other handle, and it is refused as a bus attachment,
/// so it can never be named by a message a router would have to resolve.
#[derive(Clone)]
pub struct Artifact {
    pub(crate) reference: Arc<ArtifactRef>,
    pub(crate) holder: Holder,
    /// Where the bytes are, when the router said so in the delivery that granted this handle:
    /// [`Artifact::open`] then needs no `artifact.open` round trip (BUS-01).
    pub(crate) location: Option<Arc<Location>>,
}

/// What keeps an artifact's bytes alive.
#[derive(Clone)]
pub(crate) enum Holder {
    /// A router-side owner: a delivery or an explicit hold.
    Bus(Arc<OwnerGuard>),
    /// The bytes themselves.
    Memory(Arc<[u8]>),
}

/// The store id every in-memory artifact reference carries. No router issues it: a router's
/// store ids are `store-<tag>`.
pub const IN_MEMORY_STORE_ID: &str = "in-memory";

impl std::fmt::Debug for Artifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifact")
            .field("reference", &self.reference)
            .field("owner", &self.owner_id())
            .finish()
    }
}

impl Artifact {
    /// An immutable in-memory artifact holding `bytes`, with no digest. Nothing is copied.
    pub fn in_memory(content_type: &str, bytes: impl Into<Arc<[u8]>>) -> Artifact {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let bytes: Arc<[u8]> = bytes.into();
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed) + 1;
        Artifact {
            reference: Arc::new(ArtifactRef {
                store_id: IN_MEMORY_STORE_ID.to_owned(),
                artifact_id: format!("m-{serial}"),
                generation: GENERATION,
                byte_length: bytes.len() as u64,
                content_type: content_type.to_owned(),
                digest: None,
            }),
            holder: Holder::Memory(bytes),
            location: None,
        }
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    /// The same artifact as an in-memory handle with the **same reference**: a sealed one is
    /// read once, an in-memory one is returned as it is. For a caller in the owner's process
    /// that must keep the bytes without holding the owner's connection (a handle is only valid
    /// as an attachment on the connection that owns it).
    pub async fn to_memory(&self) -> Result<Artifact, BusError> {
        if self.is_in_memory() {
            return Ok(self.clone());
        }
        let bytes = self.read_all().await?;
        if bytes.len() as u64 != self.reference.byte_length {
            return Err(BusError::new(
                ErrorCode::ArtifactMismatch,
                "sealed bytes disagree with the reference's length",
            ));
        }
        Ok(Artifact {
            reference: self.reference.clone(),
            holder: Holder::Memory(bytes.into()),
            location: None,
        })
    }

    /// True for an [`Artifact::in_memory`] handle.
    pub fn is_in_memory(&self) -> bool {
        matches!(self.holder, Holder::Memory(_))
    }

    /// True when this handle's owner is an explicit hold of its own connection -- a sealed
    /// writer or a [`Artifact::retain`] -- rather than a delivery. Keeping a clone of such a handle
    /// keeps the bytes as long as a [`Artifact::retain`] would, and consumes no delivery credit,
    /// so a holder that only needs the bytes to stay alive need not take another hold (BUS-01).
    pub fn is_hold(&self) -> bool {
        matches!(&self.holder, Holder::Bus(owner) if !owner.delivery)
    }

    /// The bytes of an in-memory artifact, without a copy; `None` for a sealed one.
    pub fn memory(&self) -> Option<&Arc<[u8]>> {
        match &self.holder {
            Holder::Memory(bytes) => Some(bytes),
            Holder::Bus(_) => None,
        }
    }

    /// The router-side owner this handle rides on: a delivery id or an explicit hold id.
    /// An in-memory artifact has none and reports its store id.
    pub fn owner_id(&self) -> &str {
        match &self.holder {
            Holder::Bus(owner) => &owner.id,
            Holder::Memory(_) => IN_MEMORY_STORE_ID,
        }
    }

    /// Opens the sealed bytes read-only. The file keeps this handle (and so its owner) alive.
    pub async fn open(&self) -> Result<ArtifactFile, BusError> {
        let owner = match &self.holder {
            Holder::Bus(owner) => owner,
            Holder::Memory(bytes) => {
                return Ok(ArtifactFile {
                    file: Backing::Memory(io::Cursor::new(bytes.clone())),
                    _artifact: self.clone(),
                });
            }
        };
        let shared = &owner.conn.shared;
        // The delivery that granted this handle said where the bytes are (BUS-01); otherwise the
        // router is asked. Either way the location is store-issued and resolved below the store
        // root, and the owner this handle keeps holds the bytes while the file is open.
        let loc = match &self.location {
            Some(loc) => loc.as_ref().clone(),
            None => {
                let mut body = Map::new();
                body.insert("ref".into(), self.reference.to_json());
                body.insert("ownerId".into(), owner.id.clone().into());
                let reply = shared
                    .command(
                        "artifact.open",
                        body,
                        Vec::new(),
                        vec![owner.clone()],
                        Hook::None,
                    )
                    .await?;
                Location::from_json(Fields::of(&reply.value, "reply").value("readLocation")?)?
            }
        };
        if loc.store_id != self.reference.store_id {
            return Err(BusError::new(
                ErrorCode::ArtifactGone,
                "read location names another store",
            ));
        }
        let path = resolve(&shared.store_root, &loc.store_id, &loc, &shared.dirs)?;
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
            file: Backing::File(file),
            _artifact: self.clone(),
        })
    }

    /// Reads the whole artifact: on the blocking pool, or in place up to
    /// [`crate::store::INLINE_IO_BYTES`] (a tmpfs read of that size is cheaper than the pool hop);
    /// an in-memory one is copied in place.
    pub async fn read_all(&self) -> Result<Vec<u8>, BusError> {
        if let Holder::Memory(bytes) = &self.holder {
            return Ok(bytes.to_vec());
        }
        let mut file = self.open().await?;
        if file.len() <= crate::store::INLINE_IO_BYTES {
            let mut out = Vec::with_capacity(file.len() as usize);
            return file
                .read_to_end(&mut out)
                .map(|_| out)
                .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()));
        }
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::with_capacity(file.len() as usize);
            file.read_to_end(&mut out).map(|_| out)
        })
        .await
        .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()))?
        .map_err(|e| BusError::new(ErrorCode::StoreFailure, e.to_string()))
    }

    /// Creates an independent explicit hold, so the bytes outlive this handle's delivery.
    /// An in-memory artifact's hold is another handle on the same bytes.
    pub async fn retain(&self) -> Result<Artifact, BusError> {
        let owner = match &self.holder {
            Holder::Bus(owner) => owner,
            Holder::Memory(_) => return Ok(self.clone()),
        };
        let mut body = Map::new();
        body.insert("ref".into(), self.reference.to_json());
        body.insert("ownerId".into(), owner.id.clone().into());
        let shared = &owner.conn.shared;
        let reply = shared
            .command(
                "artifact.retain",
                body,
                Vec::new(),
                vec![owner.clone()],
                Hook::Owner,
            )
            .await?;
        match reply.extra {
            Extra::Owner(owner) => Ok(Artifact {
                reference: self.reference.clone(),
                holder: Holder::Bus(owner),
                location: self.location.clone(),
            }),
            _ => Err(BusError::lost("retain reply without an owner")),
        }
    }
}

/// An open, read-only sealed file. Holds its [`Artifact`].
pub struct ArtifactFile {
    file: Backing,
    _artifact: Artifact,
}

enum Backing {
    File(File),
    Memory(io::Cursor<Arc<[u8]>>),
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
        match &mut self.file {
            Backing::File(file) => file.read(buf),
            Backing::Memory(cursor) => cursor.read(buf),
        }
    }
}

impl Seek for ArtifactFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match &mut self.file {
            Backing::File(file) => file.seek(pos),
            Backing::Memory(cursor) => cursor.seek(pos),
        }
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
    pub(crate) store_id: String,
    pub(crate) content_type: String,
}

impl ArtifactWriter {
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Bytes written so far: what a sealing send seals ([`Responder::reply_sealing`]).
    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Ends writing and returns the artifact as a sealing send will seal it: its first
    /// [`written`](Self::written) bytes, under this writer's owner, which becomes the hold
    /// (bus-v1 section 12 amendment 2026-10-02). The handle is usable once a send that attaches
    /// it -- [`Responder::reply_sealing`] -- is admitted; before that it is only a name for it.
    pub fn into_unsealed(mut self) -> Unsealed {
        drop(self.file.take());
        let reference = ArtifactRef {
            store_id: self.store_id.clone(),
            artifact_id: self.artifact_id.clone(),
            generation: GENERATION,
            byte_length: self.written,
            content_type: self.content_type.clone(),
            digest: None,
        };
        Unsealed {
            artifact: Artifact {
                reference: Arc::new(reference),
                holder: Holder::Bus(self.owner.clone()),
                location: None,
            },
            capacity: self.byte_length,
            content_type: std::mem::take(&mut self.content_type),
        }
    }

    /// Closes the writable handle and seals the whole allocation, returning the immutable
    /// artifact on an explicit hold. Bytes not written read as zeros: the staging file is
    /// preallocated, and a recycled writer's ([`SealedReply::writers`]) is reset to zeros when
    /// the router reissues it, so no artifact ever carries bytes of the one before it.
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
            holder: Holder::Bus(self.owner.clone()),
            location: None,
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
    pub(crate) attachments: Vec<Delivered>,
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
        self.attachments.iter().map(|d| d.name.as_str())
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
    pub(crate) attachments: Vec<Delivered>,
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

/// A writer's artifact waiting for the send that seals it ([`ArtifactWriter::into_unsealed`]).
pub struct Unsealed {
    artifact: Artifact,
    capacity: u64,
    content_type: String,
}

impl Unsealed {
    /// The handle the sealed artifact will have; attach it to the sealing send.
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }
}

/// What a sealing reply ([`Responder::reply_sealing`]) produced.
pub struct SealedReply {
    /// As [`Responder::reply`]: routed to the caller, or the caller had detached.
    pub routed: bool,
    /// With `recycle`, a fresh writer of the same allocation for each artifact sealed. Its
    /// staging file is the sealed artifact's, emptied: it reads as zeros, as a new
    /// allocation does.
    pub writers: Vec<ArtifactWriter>,
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
        let (atts, keep) = attachment_list(attachments)?;
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
        // An admitted reply ends this call's reply authority at the router (routed or, for a
        // detached caller, retired), so there is nothing left to release (BUS-02): the
        // `rpc.responder.release` the guard would send is a no-op command on every call.
        self.guard
            .replied
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(Fields::of(&reply.value, "reply").boolean("routed")?)
    }
}

impl Responder {
    /// Replies with `attachments`, among which the artifacts of `unsealed` are sealed by the
    /// router as it admits the reply (bus-v1 section 12 amendment 2026-10-02): each becomes an
    /// immutable artifact exactly as if it had been sealed and then attached, without the
    /// `artifact.seal` round trip. With `recycle` the router reissues each one's staging
    /// allocation as a new writer, which saves the next `artifact.allocate`. Refused, nothing
    /// is admitted and every unsealed artifact is gone.
    pub async fn reply_sealing(
        &self,
        outcome: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
        unsealed: Vec<Unsealed>,
        recycle: bool,
    ) -> Result<SealedReply, BusError> {
        let (atts, keep) = attachment_list(attachments)?;
        let shapes: Vec<(String, u64, String)> = unsealed
            .into_iter()
            .filter_map(|u| {
                let name = atts
                    .iter()
                    .find(|a| a.reference.artifact_id == u.artifact.reference.artifact_id)?
                    .name
                    .clone();
                Some((name, u.capacity, u.content_type))
            })
            .collect();
        let mut body = Map::new();
        body.insert("callId".into(), self.call_id.clone().into());
        body.insert(
            "requestDeliveryId".into(),
            self.request_delivery_id.clone().into(),
        );
        body.insert("outcome".into(), Value::Object(outcome));
        if recycle && !shapes.is_empty() {
            body.insert("recycle".into(), true.into());
        }
        let shared = &self.guard.conn.shared;
        let reply = shared
            .command("rpc.reply", body, atts, keep, Hook::Writers)
            .await?;
        self.guard
            .replied
            .store(true, std::sync::atomic::Ordering::Release);
        let routed = Fields::of(&reply.value, "reply").boolean("routed")?;
        let grants = match reply.extra {
            Extra::Writers(grants) => grants,
            _ => Vec::new(),
        };
        let mut writers = Vec::with_capacity(grants.len());
        for WriterGrant { name, artifact_id, owner, location } in grants {
            let Some((_, capacity, content_type)) = shapes.iter().find(|(n, _, _)| *n == name)
            else {
                continue;
            };
            let path = resolve(&shared.store_root, &location.store_id, &location, &shared.dirs)?;
            let file = open_write(&path)
                .map_err(|e| BusError::new(ErrorCode::StoreFailure, format!("staging: {e}")))?;
            writers.push(ArtifactWriter {
                file: Some(file),
                owner,
                artifact_id,
                byte_length: *capacity,
                written: 0,
                store_id: location.store_id,
                content_type: content_type.clone(),
            });
        }
        Ok(SealedReply { routed, writers })
    }
}

pub(crate) struct ReplyGuard {
    pub conn: Arc<ClientConn>,
    pub call_id: String,
    pub request_delivery_id: String,
    /// Set once the router admitted a reply: the authority is spent, no release is sent.
    pub replied: std::sync::atomic::AtomicBool,
}

impl Drop for ReplyGuard {
    fn drop(&mut self) {
        if self.replied.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
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
    pub(crate) attachments: Vec<Delivered>,
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
