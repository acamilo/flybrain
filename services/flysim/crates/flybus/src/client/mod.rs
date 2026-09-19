//! The client SDK: one connection for RPC, pub/sub and artifacts.

mod handles;
mod reactor;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;

use handles::attachment_list;
pub use handles::{
    Artifact, ArtifactFile, ArtifactWriter, CancelState, Message, PendingCall, Request, Responder,
    RpcResult, Service, Subscription,
};
use reactor::{CallSlot, ClientConn, Extra, Hook, OutCommand, Shared};

use crate::error::{BusError, Dispatch, ErrorCode};
use crate::limits::Limits;
use crate::store::{open_write, resolve};
use crate::transport::Transport;
use crate::wire::{
    Envelope, Fields, Identity, Kind, Location, MAJOR, contract_digest, hex, is_content_type,
    is_id, parse_serial_id, read_frame, serial_id, write_frame,
};

/// How a participant connects.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// The configured participant identity the launcher's policy knows.
    pub client_id: String,
    /// This SDK client's lifetime; generated when `None`. Reconnecting needs a new one.
    pub client_incarnation: Option<String>,
    /// The router's store root, as configured for the router.
    pub store_root: PathBuf,
    /// Queued consumes, releases and cancels before the client gives up on the connection.
    pub control_lane_capacity: usize,
}

impl ClientConfig {
    pub fn new(client_id: &str, store_root: impl Into<PathBuf>) -> ClientConfig {
        ClientConfig {
            client_id: client_id.to_owned(),
            client_incarnation: None,
            store_root: store_root.into(),
            control_lane_capacity: 4096,
        }
    }
}

/// What `bus.hello` negotiated.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub router_id: String,
    pub connection_id: String,
    pub identity: Identity,
    pub selected_major: u64,
    pub selected_minor: u64,
    pub contract_digest: String,
    pub limits: Limits,
}

/// `publish` admission: counts, not consumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishReceipt {
    pub topic_sequence: u64,
    pub subscribers: u64,
    pub replaced: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicInfo {
    /// False when an identical declaration already existed.
    pub declared: bool,
    pub topic_incarnation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    None,
    Latest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// One replaceable queued value; `max_queued` is always 1.
    Latest,
    /// FIFO; a full queue refuses the whole publication.
    Bounded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionConfig {
    pub mode: Mode,
    pub max_queued: u32,
    pub max_in_flight: u32,
    pub replay_latest: bool,
}

impl SubscriptionConfig {
    /// `latest`, 1 queued, 2 in flight.
    pub fn latest() -> SubscriptionConfig {
        SubscriptionConfig {
            mode: Mode::Latest,
            max_queued: 1,
            max_in_flight: 2,
            replay_latest: false,
        }
    }

    /// `bounded`, 64 queued, 16 in flight.
    pub fn bounded() -> SubscriptionConfig {
        SubscriptionConfig {
            mode: Mode::Bounded,
            max_queued: 64,
            max_in_flight: 16,
            replay_latest: false,
        }
    }

    pub fn queued(mut self, n: u32) -> SubscriptionConfig {
        self.max_queued = n;
        self
    }

    pub fn in_flight(mut self, n: u32) -> SubscriptionConfig {
        self.max_in_flight = n;
        self
    }

    pub fn replay(mut self, replay: bool) -> SubscriptionConfig {
        self.replay_latest = replay;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceConfig {
    pub max_queued: u32,
    pub max_in_flight: u32,
}

impl Default for ServiceConfig {
    fn default() -> ServiceConfig {
        ServiceConfig {
            max_queued: 16,
            max_in_flight: 16,
        }
    }
}

/// A connected participant. Cheap to clone; the connection closes when the client, and every
/// handle made from it, is dropped, or on [`close`](Client::close).
#[derive(Clone)]
pub struct Client {
    conn: Arc<ClientConn>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("info", self.info()).finish()
    }
}

fn fresh_incarnation() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(std::process::id().to_le_bytes());
    h.update(nanos.to_le_bytes());
    h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    format!("inc-{}", hex(&h.finalize()[..8]))
}

fn field<'a>(m: &'a Map<String, Value>) -> Fields<'a> {
    Fields::of(m, "reply")
}

impl Client {
    /// Connects over a transport and negotiates `bus.hello`. Must run inside a Tokio runtime.
    pub async fn connect(transport: Transport, config: ClientConfig) -> Result<Client, BusError> {
        if !is_id(&config.client_id) {
            return Err(BusError::invalid("client id is not a valid id"));
        }
        let incarnation = config
            .client_incarnation
            .clone()
            .unwrap_or_else(fresh_incarnation);
        if !is_id(&incarnation) {
            return Err(BusError::invalid("client incarnation is not a valid id"));
        }
        let (mut rd, mut wr) = tokio::io::split(transport);
        let mut body = Map::new();
        body.insert("clientId".into(), config.client_id.clone().into());
        body.insert("clientIncarnation".into(), incarnation.clone().into());
        body.insert("supportedMajors".into(), Value::Array(vec![MAJOR.into()]));
        let hello = Envelope::new(serial_id("msg", 1), Kind::Command, "bus.hello", body);
        let lost = |e: String| BusError::lost(format!("hello: {e}"));
        write_frame(&mut wr, &hello.encode()?)
            .await
            .map_err(|e| lost(e.to_string()))?;
        let bytes = read_frame(&mut rd)
            .await
            .map_err(|e| lost(e.to_string()))?
            .ok_or_else(|| lost("connection closed".into()))?;
        let env = Envelope::decode(&bytes).map_err(|e| lost(e.0))?;
        let hello_router_serial = parse_serial_id("bus", &env.id)
            .filter(|n| *n > 0)
            .ok_or_else(|| lost("hello reply id is not canonical bus-<U64>".into()))?;
        if env.major != MAJOR
            || env.minor != crate::wire::MINOR
            || env.kind != Kind::Reply
            || env.reply_to.as_deref() != Some("msg-1")
            || env.op != "bus.hello"
            || !env.attachments.is_empty()
        {
            return Err(lost("expected the hello reply".into()));
        }
        let mut f = Fields::of(&env.body, "reply");
        if !f.boolean("ok")? {
            let e = f.object("error")?;
            let mut g = Fields::of(e, "error");
            let code = ErrorCode::parse(g.string("code")?)
                .ok_or_else(|| BusError::lost("hello error has unknown code"))?;
            let message = g.string("message")?.to_owned();
            let dispatch = Dispatch::parse(g.string("dispatch")?)
                .ok_or_else(|| BusError::lost("hello error has unknown dispatch"))?;
            g.finish()?;
            f.finish()?;
            return Err(BusError {
                code,
                message,
                dispatch,
            });
        }
        let value = f.object("value")?;
        let mut v = Fields::of(value, "hello");
        let info = SessionInfo {
            router_id: v.id("routerId")?,
            connection_id: v.id("connectionId")?,
            identity: Identity {
                client_id: config.client_id.clone(),
                client_incarnation: incarnation,
            },
            selected_major: v.int("selectedMajor", 0, u64::MAX)?,
            selected_minor: v.int("selectedMinor", 0, u64::MAX)?,
            contract_digest: v.string("contractDigest")?.to_owned(),
            limits: Limits::from_json(v.value("limits")?)?,
        };
        v.finish()?;
        f.finish()?;
        if info.selected_major != MAJOR
            || info.selected_minor != crate::wire::MINOR
            || info.contract_digest != contract_digest()
        {
            return Err(BusError::new(
                ErrorCode::VersionMismatch,
                "router speaks a different contract",
            ));
        }
        let shared = Arc::new(Shared::new(
            info,
            config.store_root,
            config.control_lane_capacity.max(1),
            1,
            hello_router_serial,
        ));
        let conn = Arc::new(ClientConn {
            shared: shared.clone(),
        });
        tokio::spawn(reactor::read_loop(
            shared.clone(),
            Arc::downgrade(&conn),
            rd,
        ));
        tokio::spawn(reactor::write_loop(shared, wr));
        Ok(Client { conn })
    }

    /// Connects to a router's Unix-domain socket.
    pub async fn connect_unix(
        path: impl AsRef<Path>,
        config: ClientConfig,
    ) -> Result<Client, BusError> {
        let t = Transport::unix(path)
            .await
            .map_err(|e| BusError::lost(format!("connect: {e}")))?;
        Client::connect(t, config).await
    }

    fn shared(&self) -> &Shared {
        &self.conn.shared
    }

    pub fn info(&self) -> &SessionInfo {
        &self.shared().info
    }

    /// Why the connection closed, once it has.
    pub fn closed(&self) -> Option<BusError> {
        self.shared().lock().closed.clone()
    }

    /// Replies to fire-and-forget releases, consumes and unregisters that came back as
    /// errors. Nonzero means the client and router disagreed about ownership.
    pub fn control_errors(&self) -> u64 {
        self.shared().lock().control_errors
    }

    /// Flushes queued releases, closes the connection and waits until the reader has stopped.
    /// Handles that outlive this become inert; the router releases what they owned.
    pub async fn close(self) {
        let shared = self.conn.shared.clone();
        shared.begin_shutdown();
        let mut done = shared.done.subscribe();
        drop(self);
        let _ = done.wait_for(|v| *v).await;
    }

    // ---- services and RPC

    /// Registers an exclusive service endpoint.
    pub async fn register(&self, name: &str, config: ServiceConfig) -> Result<Service, BusError> {
        let mut body = Map::new();
        body.insert("name".into(), name.into());
        body.insert("maxQueued".into(), config.max_queued.into());
        body.insert("maxInFlight".into(), config.max_in_flight.into());
        let reply = self
            .shared()
            .command(
                "service.register",
                body,
                Vec::new(),
                Vec::new(),
                Hook::Register,
            )
            .await?;
        match reply.extra {
            Extra::Service(guard, rx) => Ok(Service {
                name: name.to_owned(),
                rx,
                guard,
            }),
            _ => Err(BusError::lost("register reply without a service")),
        }
    }

    /// Calls `method` on `service`, pinned to `expected_incarnation` when given. Returns once
    /// the router has admitted the call; the result arrives through the [`PendingCall`].
    pub async fn call(
        &self,
        service: &str,
        expected_incarnation: Option<&str>,
        method: &str,
        payload: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
    ) -> Result<PendingCall, BusError> {
        let (atts, keep) = attachment_list(attachments);
        let (tx, rx) = oneshot::channel();
        let (reply_tx, reply_rx) = oneshot::channel();
        let shared = self.shared();
        // The call id, its result slot and its queue position are fixed together, so call ids
        // reach the router in increasing order.
        let rejected = {
            let mut st = shared.lock();
            if let Some(e) = &st.closed {
                Some((e.clone(), keep))
            } else {
                st.next_call += 1;
                let call_id = serial_id("call", st.next_call);
                let mut body = Map::new();
                body.insert("callId".into(), call_id.clone().into());
                body.insert("target".into(), service.into());
                body.insert(
                    "expectedIncarnation".into(),
                    expected_incarnation.map_or(Value::Null, Value::from),
                );
                body.insert("method".into(), method.into());
                body.insert("payload".into(), Value::Object(payload));
                st.calls
                    .insert(call_id.clone(), CallSlot::Waiting(Some(tx)));
                st.ordinary.push_back(OutCommand {
                    op: "rpc.call",
                    body,
                    attachments: atts,
                    respond: Some(reply_tx),
                    hook: Hook::Call(call_id),
                    keep,
                });
                None
            }
        };
        if let Some((e, keep)) = rejected {
            drop(keep);
            return Err(e);
        }
        shared.wake();
        let reply = reply_rx
            .await
            .unwrap_or_else(|_| Err(BusError::lost("connection closed")))?;
        let service_incarnation = field(&reply.value).id("serviceIncarnation")?;
        match reply.extra {
            Extra::Call(guard) => Ok(PendingCall {
                guard,
                rx,
                service_incarnation,
            }),
            _ => Err(BusError::lost("call reply without a call handle")),
        }
    }

    /// Admits a call and waits for its result.
    pub async fn call_and_wait(
        &self,
        service: &str,
        expected_incarnation: Option<&str>,
        method: &str,
        payload: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
    ) -> Result<RpcResult, BusError> {
        let mut pending = self
            .call(service, expected_incarnation, method, payload, attachments)
            .await?;
        pending.result().await
    }

    // ---- topics

    pub async fn declare_topic(
        &self,
        name: &str,
        retained: Retained,
    ) -> Result<TopicInfo, BusError> {
        let mut body = Map::new();
        body.insert("name".into(), name.into());
        body.insert(
            "retained".into(),
            if retained == Retained::Latest {
                "latest"
            } else {
                "none"
            }
            .into(),
        );
        let reply = self
            .shared()
            .command("topic.declare", body, Vec::new(), Vec::new(), Hook::None)
            .await?;
        let mut f = field(&reply.value);
        Ok(TopicInfo {
            declared: f.boolean("declared")?,
            topic_incarnation: f.id("topicIncarnation")?,
        })
    }

    /// Releases the retained value. `false` if there was none.
    pub async fn clear_topic(&self, name: &str) -> Result<bool, BusError> {
        let mut body = Map::new();
        body.insert("name".into(), name.into());
        let reply = self
            .shared()
            .command("topic.clear", body, Vec::new(), Vec::new(), Hook::None)
            .await?;
        Ok(field(&reply.value).boolean("cleared")?)
    }

    /// Deletes a topic with no subscribers. `false` if it did not exist.
    pub async fn delete_topic(&self, name: &str) -> Result<bool, BusError> {
        let mut body = Map::new();
        body.insert("name".into(), name.into());
        let reply = self
            .shared()
            .command("topic.delete", body, Vec::new(), Vec::new(), Hook::None)
            .await?;
        Ok(field(&reply.value).boolean("deleted")?)
    }

    pub async fn subscribe(
        &self,
        topic: &str,
        config: SubscriptionConfig,
    ) -> Result<Subscription, BusError> {
        let mut body = Map::new();
        body.insert("topic".into(), topic.into());
        body.insert(
            "mode".into(),
            if config.mode == Mode::Latest {
                "latest"
            } else {
                "bounded"
            }
            .into(),
        );
        body.insert("maxQueued".into(), config.max_queued.into());
        body.insert("maxInFlight".into(), config.max_in_flight.into());
        body.insert("replayLatest".into(), config.replay_latest.into());
        let reply = self
            .shared()
            .command("subscribe", body, Vec::new(), Vec::new(), Hook::Subscribe)
            .await?;
        let topic_incarnation = field(&reply.value).id("topicIncarnation")?;
        match reply.extra {
            Extra::Subscription(guard, rx) => Ok(Subscription {
                rx,
                guard,
                topic_incarnation,
            }),
            _ => Err(BusError::lost("subscribe reply without a subscription")),
        }
    }

    /// Publishes. The attachments' owners are held until the router has answered.
    pub async fn publish(
        &self,
        topic: &str,
        payload: Map<String, Value>,
        attachments: &[(&str, &Artifact)],
    ) -> Result<PublishReceipt, BusError> {
        let (atts, keep) = attachment_list(attachments);
        let mut body = Map::new();
        body.insert("topic".into(), topic.into());
        body.insert("payload".into(), Value::Object(payload));
        let reply = self
            .shared()
            .command("publish", body, atts, keep, Hook::None)
            .await?;
        let mut f = field(&reply.value);
        Ok(PublishReceipt {
            topic_sequence: f.u64_string("topicSequence")?,
            subscribers: f.u64_string("subscribers")?,
            replaced: f.u64_string("replaced")?,
        })
    }

    // ---- artifacts

    pub fn artifacts(&self) -> Artifacts<'_> {
        Artifacts { client: self }
    }
}

/// The artifact half of the client API.
pub struct Artifacts<'a> {
    client: &'a Client,
}

impl Artifacts<'_> {
    /// Reserves `byte_length` bytes of private staging storage and opens it for writing.
    pub async fn allocate(
        &self,
        byte_length: u64,
        content_type: &str,
    ) -> Result<ArtifactWriter, BusError> {
        if !is_content_type(content_type) {
            return Err(BusError::invalid(
                "contentType must be 1..=127 printable ASCII characters",
            ));
        }
        let shared = self.client.shared();
        let mut body = Map::new();
        body.insert("byteLength".into(), byte_length.to_string().into());
        body.insert("contentType".into(), content_type.into());
        let reply = shared
            .command(
                "artifact.allocate",
                body,
                Vec::new(),
                Vec::new(),
                Hook::Owner,
            )
            .await?;
        let Extra::Owner(owner) = reply.extra else {
            return Err(BusError::lost("allocate reply without an owner"));
        };
        let mut f = field(&reply.value);
        let artifact_id = f.id("artifactId")?;
        if parse_serial_id("a", &artifact_id).is_none() {
            return Err(BusError::lost("router issued a malformed artifact id"));
        }
        let loc = Location::from_json(f.value("writeLocation")?)?;
        let path = resolve(&shared.store_root, &loc.store_id, &loc)?;
        let file = open_write(&path)
            .map_err(|e| BusError::new(ErrorCode::StoreFailure, format!("staging: {e}")))?;
        Ok(ArtifactWriter {
            file: Some(file),
            owner,
            artifact_id,
            byte_length,
            written: 0,
        })
    }
}
