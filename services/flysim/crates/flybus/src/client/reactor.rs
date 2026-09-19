//! The client's connection reactor: one reader task, one writer task, and the shared state
//! they and every handle use.
//!
//! Lanes: the writer sends queued control items (consumes, releases, cancels) before ordinary
//! commands, so dropping handles is never stuck behind a burst of publishes. Command ids are
//! assigned when a command is written, which keeps them strictly increasing on the wire
//! whatever order the lanes interleave in.
//!
//! Locking rule: nothing that can own a guard is dropped while the state mutex is held, because
//! guard drops lock it. Values are taken out under the lock and dropped after it is released.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use super::SessionInfo;
use super::handles::{
    CallGuard, Message, OwnerGuard, ReplyGuard, Request, RpcResult, ServiceGuard, SubscriptionGuard,
};
use crate::error::{BusError, Dispatch, ErrorCode};
use crate::transport::Transport;
use crate::wire::{
    ArtifactRef, Attachment, Envelope, Fields, GENERATION, Identity, Kind, MAJOR, MAX_BATCH, MINOR,
    WireError, parse_serial_id, parse_u64, read_frame, serial_id, write_frame,
};

pub(crate) type Responder = oneshot::Sender<Result<Reply, BusError>>;

/// A successful reply, plus whatever handle the reactor built from it.
pub(crate) struct Reply {
    pub value: Map<String, Value>,
    pub extra: Extra,
}

pub(crate) enum Extra {
    None,
    Owner(Arc<OwnerGuard>),
    Service(ServiceGuard, mpsc::UnboundedReceiver<Request>),
    Subscription(SubscriptionGuard, mpsc::UnboundedReceiver<Message>),
    Call(CallGuard),
}

/// What the reactor does with a reply before handing it over. Handles are built here, inside
/// the reactor, so a caller that abandoned its future cannot leak an owner it never saw: the
/// handle drops with the undelivered reply and releases itself.
pub(crate) enum Hook {
    None,
    /// `value.ownerId` is a new explicit hold.
    Owner,
    Register,
    Subscribe,
    Call(String),
    Cancel(String),
}

pub(crate) struct OutCommand {
    pub op: &'static str,
    pub body: Map<String, Value>,
    pub attachments: Vec<Attachment>,
    pub respond: Option<Responder>,
    pub hook: Hook,
    /// Source owners held until the router has answered (bus-v1 section 8.2).
    pub keep: Vec<Arc<OwnerGuard>>,
}

pub(crate) enum Control {
    Consumed(String),
    Release(String),
    Cancel(String, Option<Responder>),
    Unregister {
        name: String,
        service_incarnation: String,
    },
    ResponderReleased {
        call_id: String,
        request_delivery_id: String,
    },
}

pub(crate) enum CallSlot {
    Waiting(Option<oneshot::Sender<Result<RpcResult, BusError>>>),
    Abandoned,
}

struct Pending {
    op: &'static str,
    respond: Option<Responder>,
    hook: Hook,
    _keep: Vec<Arc<OwnerGuard>>,
}

pub(crate) struct ClientState {
    pub closed: Option<BusError>,
    pub shutting_down: bool,
    pub control: VecDeque<Control>,
    pub ordinary: VecDeque<OutCommand>,
    next_msg: u64,
    last_router: u64,
    last_delivery: u64,
    pending: HashMap<u64, Pending>,
    pub services: HashMap<String, mpsc::UnboundedSender<Request>>,
    pub subscriptions: HashMap<String, mpsc::UnboundedSender<Message>>,
    pub calls: HashMap<String, CallSlot>,
    pub next_call: u64,
    pub control_errors: u64,
}

pub(crate) struct Shared {
    pub info: SessionInfo,
    pub store_root: PathBuf,
    state: Mutex<ClientState>,
    wake: Notify,
    control_capacity: usize,
    pub done: watch::Sender<bool>,
    shutdown: watch::Sender<bool>,
}

/// The connection's lifetime token. Every public handle holds one; the connection closes when
/// the last is dropped (or on [`crate::Client::close`]).
pub(crate) struct ClientConn {
    pub shared: Arc<Shared>,
}

impl Drop for ClientConn {
    fn drop(&mut self) {
        self.shared.begin_shutdown();
    }
}

impl Shared {
    pub(crate) fn new(
        info: SessionInfo,
        store_root: PathBuf,
        control_capacity: usize,
        next_msg: u64,
        last_router: u64,
    ) -> Shared {
        Shared {
            info,
            store_root,
            state: Mutex::new(ClientState {
                closed: None,
                shutting_down: false,
                control: VecDeque::new(),
                ordinary: VecDeque::new(),
                next_msg,
                last_router,
                last_delivery: 0,
                pending: HashMap::new(),
                services: HashMap::new(),
                subscriptions: HashMap::new(),
                calls: HashMap::new(),
                next_call: 0,
                control_errors: 0,
            }),
            wake: Notify::new(),
            control_capacity,
            done: watch::Sender::new(false),
            shutdown: watch::Sender::new(false),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn begin_shutdown(&self) {
        self.lock().shutting_down = true;
        self.wake.notify_one();
    }

    /// Queues a control item. A full control lane closes the connection rather than lose a
    /// release (bus-v1 section 8.3).
    pub(crate) fn push_control(&self, item: Control) {
        let overflow = {
            let mut st = self.lock();
            if st.closed.is_some() {
                Some(item)
            } else if st.control.len() >= self.control_capacity {
                st.closed = Some(BusError::lost("client control lane exhausted"));
                Some(item)
            } else {
                st.control.push_back(item);
                None
            }
        };
        self.wake.notify_one();
        drop(overflow);
    }

    /// Queues an ordinary command, or hands it back with the reason it cannot be sent.
    pub(crate) fn enqueue(&self, cmd: OutCommand) -> Result<(), BusError> {
        let rejected = {
            let mut st = self.lock();
            match &st.closed {
                Some(e) => Some((e.clone().with_dispatch(Dispatch::NotDispatched), cmd)),
                None => {
                    st.ordinary.push_back(cmd);
                    None
                }
            }
        };
        self.wake.notify_one();
        match rejected {
            Some((e, cmd)) => {
                drop(cmd);
                Err(e)
            }
            None => Ok(()),
        }
    }

    /// Queues a command and waits for its reply.
    pub(crate) async fn command(
        &self,
        op: &'static str,
        body: Map<String, Value>,
        attachments: Vec<Attachment>,
        keep: Vec<Arc<OwnerGuard>>,
        hook: Hook,
    ) -> Result<Reply, BusError> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(OutCommand {
            op,
            body,
            attachments,
            respond: Some(tx),
            hook,
            keep,
        })?;
        rx.await
            .unwrap_or_else(|_| Err(BusError::lost("connection closed")))
    }

    /// Fails everything outstanding. Values are dropped after the lock is released.
    fn fail_all(&self, reason: BusError) {
        let (pending, calls, services, subscriptions, control, ordinary, reason) = {
            let mut st = self.lock();
            let reason = st.closed.get_or_insert(reason).clone();
            (
                std::mem::take(&mut st.pending),
                std::mem::take(&mut st.calls),
                std::mem::take(&mut st.services),
                std::mem::take(&mut st.subscriptions),
                std::mem::take(&mut st.control),
                std::mem::take(&mut st.ordinary),
                reason,
            )
        };
        for (_, p) in pending {
            if let Some(tx) = p.respond {
                let _ = tx.send(Err(reason.clone().with_dispatch(Dispatch::Unknown)));
            }
        }
        for (_, slot) in calls {
            if let CallSlot::Waiting(Some(tx)) = slot {
                let _ = tx.send(Err(reason.clone().with_dispatch(Dispatch::Unknown)));
            }
        }
        for cmd in ordinary {
            if let Some(tx) = cmd.respond {
                let _ = tx.send(Err(reason.clone().with_dispatch(Dispatch::NotDispatched)));
            }
        }
        for item in control {
            if let Control::Cancel(_, Some(tx)) = item {
                let _ = tx.send(Err(reason.clone().with_dispatch(Dispatch::NotDispatched)));
            }
        }
        drop((services, subscriptions));
        self.done.send_replace(true);
    }

    fn terminate(&self, reason: BusError) {
        self.fail_all(reason);
        self.shutdown.send_replace(true);
        self.wake.notify_waiters();
    }
}

// ---------------------------------------------------------------------------------------------
// Writer

enum Next {
    Send(Vec<u8>),
    Failed(OutCommand, BusError),
    Idle,
    Exit,
}

fn take_batch(control: &mut VecDeque<Control>) -> Option<OutCommand> {
    let first = control.pop_front()?;
    let (op, key, mut ids) = match first {
        Control::Cancel(call_id, respond) => {
            let mut body = Map::new();
            body.insert("callId".into(), call_id.clone().into());
            let hook = Hook::Cancel(call_id);
            return Some(OutCommand {
                op: "rpc.cancel",
                body,
                attachments: Vec::new(),
                respond,
                hook,
                keep: Vec::new(),
            });
        }
        Control::ResponderReleased {
            call_id,
            request_delivery_id,
        } => {
            let mut body = Map::new();
            body.insert("callId".into(), call_id.into());
            body.insert("requestDeliveryId".into(), request_delivery_id.into());
            return Some(OutCommand {
                op: "rpc.responder.release",
                body,
                attachments: Vec::new(),
                respond: None,
                hook: Hook::None,
                keep: Vec::new(),
            });
        }
        Control::Unregister {
            name,
            service_incarnation,
        } => {
            let mut body = Map::new();
            body.insert("name".into(), name.into());
            body.insert("serviceIncarnation".into(), service_incarnation.into());
            return Some(OutCommand {
                op: "service.unregister",
                body,
                attachments: Vec::new(),
                respond: None,
                hook: Hook::None,
                keep: Vec::new(),
            });
        }
        Control::Consumed(id) => ("delivery.consumed", "deliveryIds", vec![id]),
        Control::Release(id) => ("artifact.release", "ownerIds", vec![id]),
    };
    while ids.len() < MAX_BATCH {
        match (control.front(), op) {
            (Some(Control::Consumed(_)), "delivery.consumed")
            | (Some(Control::Release(_)), "artifact.release") => match control.pop_front() {
                Some(Control::Consumed(id) | Control::Release(id)) => ids.push(id),
                _ => unreachable!("front was checked"),
            },
            _ => break,
        }
    }
    let mut body = Map::new();
    body.insert(
        key.into(),
        Value::Array(ids.into_iter().map(Value::String).collect()),
    );
    Some(OutCommand {
        op,
        body,
        attachments: Vec::new(),
        respond: None,
        hook: Hook::None,
        keep: Vec::new(),
    })
}

fn next_outgoing(shared: &Shared) -> Next {
    let mut st = shared.lock();
    if st.closed.is_some() {
        return Next::Exit;
    }
    let cmd = match take_batch(&mut st.control).or_else(|| st.ordinary.pop_front()) {
        Some(cmd) => cmd,
        None if st.shutting_down => {
            st.closed = Some(BusError::lost("client closed"));
            return Next::Exit;
        }
        None => return Next::Idle,
    };
    st.next_msg += 1;
    let n = st.next_msg;
    let mut env = Envelope::new(serial_id("msg", n), Kind::Command, cmd.op, cmd.body.clone());
    env.attachments = cmd.attachments.clone();
    match env.encode() {
        Ok(bytes) => {
            st.pending.insert(
                n,
                Pending {
                    op: cmd.op,
                    respond: cmd.respond,
                    hook: cmd.hook,
                    _keep: cmd.keep,
                },
            );
            Next::Send(bytes)
        }
        Err(e) => Next::Failed(cmd, e.into()),
    }
}

pub(crate) async fn write_loop(shared: Arc<Shared>, mut wr: WriteHalf<Transport>) {
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        match next_outgoing(&shared) {
            Next::Send(bytes) => {
                let result = tokio::select! {
                    result = write_frame(&mut wr, &bytes) => result,
                    _ = shutdown.wait_for(|v| *v) => break,
                };
                if result.is_err() {
                    shared.terminate(BusError::lost("write to router failed"));
                    break;
                }
            }
            Next::Failed(cmd, e) => {
                let slot = match &cmd.hook {
                    Hook::Call(call_id) => shared.lock().calls.remove(call_id),
                    _ => None,
                };
                drop(slot);
                if let Some(tx) = cmd.respond {
                    let _ = tx.send(Err(e));
                }
            }
            Next::Idle => {
                tokio::select! {
                    _ = shared.wake.notified() => {}
                    _ = shutdown.wait_for(|v| *v) => break,
                }
            }
            Next::Exit => break,
        }
    }
    tokio::select! {
        _ = wr.shutdown() => {}
        _ = shutdown.wait_for(|v| *v) => {}
        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
    let reason = shared
        .lock()
        .closed
        .clone()
        .unwrap_or_else(|| BusError::lost("client writer stopped"));
    shared.terminate(reason);
}

// ---------------------------------------------------------------------------------------------
// Reader

pub(crate) async fn read_loop(
    shared: Arc<Shared>,
    conn: Weak<ClientConn>,
    mut rd: ReadHalf<Transport>,
) {
    let mut shutdown = shared.shutdown.subscribe();
    let reason = loop {
        let frame = tokio::select! {
            frame = read_frame(&mut rd) => frame,
            _ = shutdown.wait_for(|v| *v) => return,
        };
        let bytes = match frame {
            Ok(Some(b)) => b,
            Ok(None) => break BusError::lost("router closed the connection"),
            Err(e) => break BusError::lost(format!("router connection failed: {e}")),
        };
        let env = match Envelope::decode(&bytes) {
            Ok(env) => env,
            Err(e) => break BusError::lost(format!("router sent an invalid envelope: {e}")),
        };
        if let Err(e) = on_envelope(&shared, &conn, env) {
            break BusError::lost(format!("router protocol violation: {e}"));
        }
    };
    shared.terminate(reason);
}

fn on_envelope(
    shared: &Arc<Shared>,
    conn: &Weak<ClientConn>,
    env: Envelope,
) -> Result<(), WireError> {
    if env.major != MAJOR || env.minor != MINOR {
        return Err(WireError(format!(
            "router envelope version is not {MAJOR}.{MINOR}"
        )));
    }
    let router_serial = parse_serial_id("bus", &env.id)
        .filter(|n| *n > 0)
        .ok_or_else(|| WireError("router envelope id is not canonical bus-<U64>".into()))?;
    {
        let mut st = shared.lock();
        if router_serial <= st.last_router {
            return Err(WireError(
                "router envelope ids must strictly increase".into(),
            ));
        }
        st.last_router = router_serial;
    }
    match env.kind {
        Kind::Reply => {
            if !env.attachments.is_empty() {
                return Err(WireError("router replies cannot carry attachments".into()));
            }
            let n = env
                .reply_to
                .as_deref()
                .and_then(|r| parse_serial_id("msg", r));
            let n = n.ok_or_else(|| WireError("reply without a command id".into()))?;
            let pending = {
                let mut st = shared.lock();
                let Some(pending) = st.pending.get(&n) else {
                    return Err(WireError(format!("reply to unknown command msg-{n}")));
                };
                if env.op != pending.op {
                    return Err(WireError(format!(
                        "reply operation {:?} does not match {:?}",
                        env.op, pending.op
                    )));
                }
                st.pending.remove(&n)
            };
            let pending =
                pending.ok_or_else(|| WireError(format!("reply to unknown command msg-{n}")))?;
            let result = parse_reply(&env.body)?;
            if let Ok(value) = &result {
                validate_reply_value(pending.op, value)?;
            }
            complete(shared, conn, pending, result)
        }
        Kind::Delivery => {
            if env.reply_to.is_some() {
                return Err(WireError("deliveries must have null replyTo".into()));
            }
            on_delivery(shared, conn, env)
        }
        Kind::Notice => {
            if env.reply_to.is_some() || !env.attachments.is_empty() {
                return Err(WireError(
                    "notices must have null replyTo and no attachments".into(),
                ));
            }
            on_notice(shared, env)
        }
        Kind::Command => Err(WireError("routers do not send commands".into())),
    }
}

fn parse_reply(
    body: &Map<String, Value>,
) -> Result<Result<Map<String, Value>, BusError>, WireError> {
    let mut f = Fields::of(body, "reply");
    if f.boolean("ok")? {
        let value = f.object("value")?.clone();
        f.finish()?;
        return Ok(Ok(value));
    }
    let e = parse_error(f.object("error")?)?;
    f.finish()?;
    Ok(Err(e))
}

fn parse_error(m: &Map<String, Value>) -> Result<BusError, WireError> {
    let mut g = Fields::of(m, "error");
    let code = ErrorCode::parse(g.string("code")?)
        .ok_or_else(|| WireError("unknown error code".into()))?;
    let message = g.string("message")?.to_owned();
    let dispatch = Dispatch::parse(g.string("dispatch")?)
        .ok_or_else(|| WireError("unknown dispatch".into()))?;
    g.finish()?;
    Ok(BusError {
        code,
        message,
        dispatch,
    })
}

fn serial_field(f: &mut Fields<'_>, key: &'static str, prefix: &str) -> Result<String, WireError> {
    let id = f.id(key)?;
    parse_serial_id(prefix, &id)
        .ok_or_else(|| WireError(format!("{key} is not canonical {prefix}-<U64>")))?;
    Ok(id)
}

fn validate_reply_value(op: &str, value: &Map<String, Value>) -> Result<(), WireError> {
    let mut f = Fields::of(value, "reply value");
    match op {
        "service.register" => {
            serial_field(&mut f, "serviceIncarnation", "svc")?;
        }
        "service.unregister" => {
            f.boolean("removed")?;
        }
        "rpc.call" => {
            if !f.boolean("accepted")? {
                return Err(WireError(
                    "an admitted rpc.call must say accepted:true".into(),
                ));
            }
            serial_field(&mut f, "serviceIncarnation", "svc")?;
        }
        "rpc.reply" => {
            f.boolean("routed")?;
        }
        "rpc.responder.release" => {
            f.boolean("released")?;
        }
        "rpc.cancel" => match f.string("state")? {
            "cancelled-before-dispatch" | "execution-unknown" | "completed" | "call-gone" => {}
            _ => return Err(WireError("unknown rpc.cancel state".into())),
        },
        "topic.declare" => {
            f.boolean("declared")?;
            serial_field(&mut f, "topicIncarnation", "top")?;
        }
        "topic.clear" => {
            f.boolean("cleared")?;
        }
        "topic.delete" => {
            f.boolean("deleted")?;
        }
        "subscribe" => {
            serial_field(&mut f, "subscriptionId", "sub")?;
            serial_field(&mut f, "topicIncarnation", "top")?;
        }
        "unsubscribe" => {
            f.boolean("removed")?;
        }
        "publish" => {
            f.u64_string("topicSequence")?;
            f.u64_string("subscribers")?;
            f.u64_string("replaced")?;
        }
        "delivery.consumed" | "artifact.release" => {
            f.u64_string("released")?;
        }
        "artifact.allocate" => {
            serial_field(&mut f, "artifactId", "a")?;
            if f.u64_string("generation")? != GENERATION {
                return Err(WireError("unsupported artifact generation".into()));
            }
            serial_field(&mut f, "ownerId", "own")?;
            crate::wire::Location::from_json(f.value("writeLocation")?)?;
        }
        "artifact.seal" => {
            ArtifactRef::from_json(f.value("ref")?)?;
            serial_field(&mut f, "ownerId", "own")?;
        }
        "artifact.open" => {
            crate::wire::Location::from_json(f.value("readLocation")?)?;
        }
        "artifact.retain" => {
            serial_field(&mut f, "ownerId", "own")?;
        }
        _ => return Err(WireError(format!("reply for unknown operation {op:?}"))),
    }
    f.finish()
}

fn owner_guard(conn: &Weak<ClientConn>, id: String, delivery: bool) -> Option<Arc<OwnerGuard>> {
    Some(Arc::new(OwnerGuard {
        id,
        delivery,
        conn: conn.upgrade()?,
    }))
}

fn complete(
    shared: &Arc<Shared>,
    conn: &Weak<ClientConn>,
    pending: Pending,
    result: Result<Map<String, Value>, BusError>,
) -> Result<(), WireError> {
    let Pending {
        respond,
        hook,
        _keep,
        ..
    } = pending;
    let reply = match (hook, result) {
        (Hook::Call(call_id), Err(e)) => {
            let slot = shared.lock().calls.remove(&call_id);
            drop(slot);
            Err(e)
        }
        (_, Err(e)) => Err(e),
        (Hook::None, Ok(value)) => Ok(Reply {
            value,
            extra: Extra::None,
        }),
        (Hook::Owner, Ok(value)) => {
            let id = Fields::of(&value, "reply").id("ownerId")?;
            match owner_guard(conn, id, false) {
                Some(g) => Ok(Reply {
                    value,
                    extra: Extra::Owner(g),
                }),
                None => Err(BusError::lost("client closed")),
            }
        }
        (Hook::Register, Ok(value)) => {
            let inc = Fields::of(&value, "reply").id("serviceIncarnation")?;
            match conn.upgrade() {
                Some(conn) => {
                    let (tx, rx) = mpsc::unbounded_channel();
                    shared.lock().services.insert(inc.clone(), tx);
                    let guard = ServiceGuard {
                        conn,
                        incarnation: inc,
                    };
                    Ok(Reply {
                        value,
                        extra: Extra::Service(guard, rx),
                    })
                }
                None => Err(BusError::lost("client closed")),
            }
        }
        (Hook::Subscribe, Ok(value)) => {
            let id = Fields::of(&value, "reply").id("subscriptionId")?;
            match conn.upgrade() {
                Some(conn) => {
                    let (tx, rx) = mpsc::unbounded_channel();
                    shared.lock().subscriptions.insert(id.clone(), tx);
                    let guard = SubscriptionGuard { conn, id };
                    Ok(Reply {
                        value,
                        extra: Extra::Subscription(guard, rx),
                    })
                }
                None => Err(BusError::lost("client closed")),
            }
        }
        (Hook::Call(call_id), Ok(value)) => match conn.upgrade() {
            Some(conn) => Ok(Reply {
                value,
                extra: Extra::Call(CallGuard {
                    conn,
                    call_id,
                    done: false,
                }),
            }),
            None => Err(BusError::lost("client closed")),
        },
        (Hook::Cancel(call_id), Ok(value)) => {
            let state = Fields::of(&value, "reply").string("state")?.to_owned();
            if state != "completed" {
                // No result will follow: resolve whoever still waits for one.
                let slot = shared.lock().calls.remove(&call_id);
                if let Some(CallSlot::Waiting(Some(tx))) = slot {
                    let dispatch = if state == "cancelled-before-dispatch" {
                        Dispatch::NotDispatched
                    } else {
                        Dispatch::Unknown
                    };
                    let e = BusError::new(ErrorCode::CallGone, format!("cancelled: {state}"))
                        .with_dispatch(dispatch);
                    let _ = tx.send(Err(e));
                }
            }
            Ok(Reply {
                value,
                extra: Extra::None,
            })
        }
    };
    match respond {
        Some(tx) => {
            // If the caller is gone the reply, and any handle in it, drops here and releases.
            let _ = tx.send(reply);
        }
        None => {
            if reply.is_err() {
                shared.lock().control_errors += 1;
            }
        }
    }
    Ok(())
}

fn attachments(env: &Envelope, delivery_id: &str) -> Result<Vec<(String, ArtifactRef)>, WireError> {
    env.attachments
        .iter()
        .map(|a| {
            if a.owner_id != delivery_id {
                return Err(WireError(
                    "delivery attachment owner is not the delivery".into(),
                ));
            }
            Ok((a.name.clone(), a.reference.clone()))
        })
        .collect()
}

fn u64_field(f: &mut Fields<'_>, key: &'static str) -> Result<u64, WireError> {
    let s = f.string(key)?;
    parse_u64(s).ok_or_else(|| WireError(format!("{key} is not a U64")))
}

fn on_delivery(
    shared: &Arc<Shared>,
    conn: &Weak<ClientConn>,
    env: Envelope,
) -> Result<(), WireError> {
    let mut f = Fields::of(&env.body, "delivery");
    let delivery_id = f.id("deliveryId")?;
    let delivery_serial = parse_serial_id("dlv", &delivery_id)
        .filter(|n| *n > 0)
        .ok_or_else(|| WireError("delivery id is not canonical dlv-<U64>".into()))?;
    let atts = attachments(&env, &delivery_id)?;
    // Without a live connection handle there is nobody to hand this to.
    let Some(guard) = owner_guard(conn, delivery_id.clone(), true) else {
        return Ok(());
    };
    match env.op.as_str() {
        "rpc.request" => {
            let call_id = f.id("callId")?;
            if parse_serial_id("call", &call_id).is_none() {
                return Err(WireError("rpc.request callId is not call-<U64>".into()));
            }
            let caller = Identity::from_json(f.value("caller")?)?;
            let target = f.name("target")?;
            let service_incarnation = f.id("serviceIncarnation")?;
            if parse_serial_id("svc", &service_incarnation).is_none() {
                return Err(WireError("service incarnation is not svc-<U64>".into()));
            }
            let method = f.method("method")?;
            let payload = f.object("payload")?.clone();
            f.finish()?;
            accept_delivery_serial(shared, delivery_serial)?;
            let req = Request {
                reply_guard: Arc::new(ReplyGuard {
                    conn: guard.conn.clone(),
                    call_id: call_id.clone(),
                    request_delivery_id: delivery_id.clone(),
                }),
                guard,
                call_id,
                caller,
                target,
                service_incarnation,
                method,
                payload,
                attachments: atts,
            };
            let tx = shared
                .lock()
                .services
                .get(&req.service_incarnation)
                .cloned();
            if let Some(tx) = tx {
                let _ = tx.send(req);
            }
        }
        "rpc.result" => {
            let call_id = f.id("callId")?;
            if parse_serial_id("call", &call_id).is_none() {
                return Err(WireError("rpc.result callId is not call-<U64>".into()));
            }
            let responder = Identity::from_json(f.value("responder")?)?;
            let service_incarnation = f.id("serviceIncarnation")?;
            if parse_serial_id("svc", &service_incarnation).is_none() {
                return Err(WireError("service incarnation is not svc-<U64>".into()));
            }
            let outcome = f.object("outcome")?.clone();
            f.finish()?;
            accept_delivery_serial(shared, delivery_serial)?;
            let res = RpcResult {
                guard,
                call_id,
                responder,
                service_incarnation,
                outcome,
                attachments: atts,
            };
            let slot = shared.lock().calls.remove(&res.call_id);
            match slot {
                Some(CallSlot::Waiting(Some(tx))) => {
                    let _ = tx.send(Ok(res));
                }
                // Abandoned or unknown: dropping it consumes the delivery.
                other => drop((other, res)),
            }
        }
        "topic.message" => {
            let subscription_id = f.id("subscriptionId")?;
            if parse_serial_id("sub", &subscription_id).is_none() {
                return Err(WireError("subscription id is not sub-<U64>".into()));
            }
            let topic = f.name("topic")?;
            let topic_incarnation = f.id("topicIncarnation")?;
            if parse_serial_id("top", &topic_incarnation).is_none() {
                return Err(WireError("topic incarnation is not top-<U64>".into()));
            }
            let topic_sequence = u64_field(&mut f, "topicSequence")?;
            let replaced = u64_field(&mut f, "replaced")?;
            let payload = f.object("payload")?.clone();
            f.finish()?;
            accept_delivery_serial(shared, delivery_serial)?;
            let msg = Message {
                guard,
                subscription_id,
                topic,
                topic_incarnation,
                topic_sequence,
                replaced,
                payload,
                attachments: atts,
            };
            let tx = shared
                .lock()
                .subscriptions
                .get(&msg.subscription_id)
                .cloned();
            if let Some(tx) = tx {
                let _ = tx.send(msg);
            }
        }
        other => return Err(WireError(format!("unknown delivery {other:?}"))),
    }
    Ok(())
}

fn accept_delivery_serial(shared: &Shared, serial: u64) -> Result<(), WireError> {
    let mut st = shared.lock();
    if serial <= st.last_delivery {
        return Err(WireError("delivery ids must strictly increase".into()));
    }
    st.last_delivery = serial;
    Ok(())
}

fn on_notice(shared: &Arc<Shared>, env: Envelope) -> Result<(), WireError> {
    let mut f = Fields::of(&env.body, "notice");
    match env.op.as_str() {
        "call.failed" => {
            let call_id = f.id("callId")?;
            if parse_serial_id("call", &call_id).is_none() {
                return Err(WireError("call.failed callId is not call-<U64>".into()));
            }
            let code = ErrorCode::parse(f.string("code")?)
                .ok_or_else(|| WireError("unknown error code".into()))?;
            let message = f.string("message")?.to_owned();
            let dispatch = Dispatch::parse(f.string("dispatch")?)
                .ok_or_else(|| WireError("unknown dispatch".into()))?;
            f.finish()?;
            let slot = shared.lock().calls.remove(&call_id);
            if let Some(CallSlot::Waiting(Some(tx))) = slot {
                let _ = tx.send(Err(BusError {
                    code,
                    message,
                    dispatch,
                }));
            }
        }
        "route.removed" => {
            f.name("name")?;
            serial_field(&mut f, "serviceIncarnation", "svc")?;
            f.string("reason")?;
            f.finish()?;
        }
        "subscription.closed" => {
            let id = serial_field(&mut f, "subscriptionId", "sub")?;
            f.name("topic")?;
            serial_field(&mut f, "topicIncarnation", "top")?;
            f.string("reason")?;
            f.finish()?;
            let tx = shared.lock().subscriptions.remove(&id);
            drop(tx);
        }
        "connection.closing" => {
            let code = ErrorCode::parse(f.string("code")?)
                .ok_or_else(|| WireError("unknown error code".into()))?;
            let message = f.string("message")?.to_owned();
            f.finish()?;
            let mut st = shared.lock();
            if st.closed.is_none() {
                st.closed = Some(BusError::new(code, message).with_dispatch(Dispatch::Unknown));
            }
        }
        other => return Err(WireError(format!("unknown notice {other:?}"))),
    }
    Ok(())
}
