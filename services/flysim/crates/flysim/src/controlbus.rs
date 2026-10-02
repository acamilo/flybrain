//! The control API as flybus RPC services with per-client grants (CTRL-01,
//! `docs/design/flybus.md`, "Control over the bus").
//!
//! Both halves of the encoding live here, so the services flysim registers and the edge that
//! calls them (`fly-control-edge`) cannot drift apart:
//!
//! - [`encode_request`] / [`decode_request`]: a [`ControlRequest`] as `(service, method,
//!   payload)`. The service is a [`Family`] in a [`Scope`]; the method is `Control.<Name>`.
//! - [`encode_reply`] / [`decode_reply`]: a [`ControlReply`] as an RPC outcome
//!   `{"status": <u16>, "body": <JSON>}` or `{"status": <u16>, "text": "..."}`, with a body too
//!   large for an envelope carried as a `body` artifact instead.
//!
//! The `status` is the `docs/control-api.md` status code of the same request, used as the
//! outcome's vocabulary rather than as transport: a bus caller reads `429 {retryAfterMs}`
//! exactly as an HTTP caller does, and the edge turns it back into HTTP with no table of its own.
//!
//! **Grants.** Bus grants are per service name, so the surface is split into five service
//! families and every participant is granted the families its role needs, by exact name, and
//! nothing else ([`Role`], [`grants`]). [`host_grants`] is the only grant that may register a
//! control service, and it belongs to an in-process participant no socket is bound to. No family
//! has a method that presses a button, edits game memory or changes the reward catalog, and no
//! role is granted `Any`, a prefix, `register`, `publish`, `subscribe` or topic management, so on
//! a router shared with environment or agent services a control client still cannot name them:
//! the "no button endpoint" guarantee is the grant table.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use flybus::{
    Artifact, BusError, Client, ClientConfig, Grants, Pattern, Policy, Request, Router,
    ServiceConfig,
};
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::control::{ControlReply, ControlRequest, ReplyBody};

/// The in-process participant that registers the control services. Never bound to a socket.
pub const HOST: &str = "flysim-control";
/// The participant `fly-control-edge` connects as, on [`EDGE_SOCKET`].
pub const EDGE: &str = "fly-control-edge";
/// `<bus_dir>/control-edge.sock`, launcher-bound to [`EDGE`].
pub const EDGE_SOCKET: &str = "control-edge.sock";
/// `<bus_dir>/control/<role>.sock`, one per native [`Role`], each launcher-bound to that role.
pub const ROLE_SOCKET_DIR: &str = "control";
/// The one session today: the live fly.
pub const LIVE_SESSION: &str = "main";
/// The one agent today (the session runtime's agent id).
pub const LIVE_AGENT: &str = "fly";
/// Every service name starts with this.
pub const PREFIX: &str = "fly.control.";
/// Replies up to this many JSON bytes ride in the envelope; anything larger becomes a `body`
/// artifact. Well under flybus's 65,536-byte envelope, leaving room for the router's ids. An
/// `/events` page can be far larger (4,096 events), `/status` is a few KB.
pub const INLINE_MAX: usize = 48 * 1024;
/// The attachment name of an out-of-line body.
pub const BODY_ARTIFACT: &str = "body";
/// The attachment name of an out-of-line request body: a `request` JSON value larger than
/// [`INLINE_MAX`] travels as this artifact, and the payload says so with `requestArtifact: true`.
/// The direct API takes bodies up to the HTTP layer's own limit, so the bus must too.
pub const REQUEST_ARTIFACT: &str = "request";
/// Per-service bounds. A full service refuses with `BACKPRESSURE` (the edge's 503 "queue is
/// full") instead of queueing without limit; each family has its own, so a stuck checkpoint
/// cannot hold up `/status`.
pub const SERVICE_CONFIG: ServiceConfig = ServiceConfig {
    max_queued: 16,
    max_in_flight: 16,
};

/// A group of control methods behind one service name, the unit a grant names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    /// `Control.Status`, `Control.Healthz`, `Control.Metrics`, `Control.Events`. Session-scoped:
    /// the session's status (the one fly's, while a session has one), its health, its metrics
    /// and its event log.
    Read,
    /// `Control.Status` for one agent. Agent-scoped. With one fly per session it answers what
    /// the session's `Control.Status` does; with N flies it is where each fly's own view lives.
    Status,
    /// `Control.Stimulate`: a sugar pulse into one agent's brain. Agent-scoped.
    Sugar,
    /// `Control.Reward`: a reinforcement pulse into one agent's plasticity. Agent-scoped.
    Reward,
    /// `Control.Chat`: a line for the session's on-screen ring. Session-scoped.
    Chat,
    /// `Control.Checkpoint`, `Control.Pause`, `Control.Resume`. Session-scoped.
    Ops,
}

impl Family {
    pub const ALL: [Family; 6] = [
        Family::Read,
        Family::Status,
        Family::Sugar,
        Family::Reward,
        Family::Chat,
        Family::Ops,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Family::Read => "read",
            Family::Status => "status",
            Family::Sugar => "sugar",
            Family::Reward => "reward",
            Family::Chat => "chat",
            Family::Ops => "ops",
        }
    }

    /// Whether the family acts on one agent (and so has one service per agent).
    pub const fn agent_scoped(self) -> bool {
        matches!(self, Family::Status | Family::Sugar | Family::Reward)
    }

    /// Every method the family's service answers.
    pub const fn methods(self) -> &'static [&'static str] {
        match self {
            Family::Read => &[
                "Control.Status",
                "Control.Healthz",
                "Control.Metrics",
                "Control.Events",
            ],
            Family::Status => &["Control.Status"],
            Family::Sugar => &["Control.Stimulate"],
            Family::Reward => &["Control.Reward"],
            Family::Chat => &["Control.Chat"],
            Family::Ops => &["Control.Checkpoint", "Control.Pause", "Control.Resume"],
        }
    }
}

/// One agent (fly) of a session, and the controller port it plays on, if the environment has
/// ports. A Game Boy has one pad, so the live fly is port 0; a console with four controller ports
/// can carry four flies, one per port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentScope {
    pub id: String,
    pub port: Option<u32>,
}

impl AgentScope {
    pub fn new(id: &str, port: Option<u32>) -> AgentScope {
        AgentScope {
            id: id.to_owned(),
            port,
        }
    }
}

/// Which agents of a scope a grant covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agents<'a> {
    /// Every agent of the session.
    All,
    /// One agent, by id.
    Id(&'a str),
    /// The agent bound to one controller port.
    Port(u32),
}

/// Which session, and which agents in it, a set of services addresses.
///
/// Session-scoped families are `fly.control.s.<session>.<family>`; agent-scoped ones are
/// `fly.control.s.<session>.a.<agent>.<family>`, one per agent. With N flies in a session each
/// fly has its own status, sugar and reward services, so a grant can name one fly (or the fly on
/// one port); with M sessions on a router each has its own names, so a grant can name one
/// session. A single-fly deployment is the same design with one agent: multi-fly is
/// configuration, not a new surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub session: String,
    pub agents: Vec<AgentScope>,
}

impl Scope {
    /// The live fly: session [`LIVE_SESSION`], agent [`LIVE_AGENT`] on port 0 (the Game Boy's
    /// one pad).
    pub fn live() -> Scope {
        Scope {
            session: LIVE_SESSION.to_owned(),
            agents: vec![AgentScope::new(LIVE_AGENT, Some(0))],
        }
    }

    /// Refuse ids that would not make a valid, unambiguous service name segment, and two agents
    /// with one id or on one port.
    pub fn validate(&self) -> Result<()> {
        let segment = |s: &str| {
            !s.is_empty()
                && s.len() <= 48
                && s.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                })
        };
        if !segment(&self.session) {
            bail!(
                "control scope: session id {:?} is not [a-z0-9_-]{{1,48}}",
                self.session
            );
        }
        if self.agents.is_empty() {
            bail!("control scope: no agents");
        }
        for (index, agent) in self.agents.iter().enumerate() {
            if !segment(&agent.id) {
                bail!("control scope: agent id {:?} is not [a-z0-9_-]{{1,48}}", agent.id);
            }
            for other in &self.agents[..index] {
                if other.id == agent.id {
                    bail!("control scope: agent {:?} twice", agent.id);
                }
                if agent.port.is_some() && other.port == agent.port {
                    bail!(
                        "control scope: agents {:?} and {:?} on one port",
                        other.id,
                        agent.id
                    );
                }
            }
        }
        Ok(())
    }

    /// The agent ids, in order.
    pub fn agent_ids(&self) -> impl Iterator<Item = &str> {
        self.agents.iter().map(|agent| agent.id.as_str())
    }

    /// The agent bound to `port`.
    pub fn agent_on_port(&self, port: u32) -> Option<&str> {
        self.agents
            .iter()
            .find(|agent| agent.port == Some(port))
            .map(|agent| agent.id.as_str())
    }

    /// The service name of a session-scoped `family`, or of an agent-scoped one for `agent`.
    pub fn service(&self, family: Family, agent: &str) -> String {
        if family.agent_scoped() {
            format!("{PREFIX}s.{}.a.{agent}.{}", self.session, family.as_str())
        } else {
            format!("{PREFIX}s.{}.{}", self.session, family.as_str())
        }
    }

    /// Every service in this scope, with its family and its agent (`None` for a session-scoped
    /// one): what the host registers.
    pub fn services(&self) -> Vec<(Family, Option<&str>, String)> {
        let mut out = Vec::new();
        for family in Family::ALL {
            if family.agent_scoped() {
                for agent in self.agent_ids() {
                    out.push((family, Some(agent), self.service(family, agent)));
                }
            } else {
                out.push((family, None, self.service(family, "")));
            }
        }
        out
    }

    /// The services of `families` in this scope: session-scoped ones, and agent-scoped ones for
    /// the `agents` selected. A port no agent is bound to selects no agent.
    pub fn services_of(&self, families: &[Family], agents: Agents<'_>) -> Vec<String> {
        let selected = match agents {
            Agents::All => None,
            Agents::Id(id) => Some(Some(id)),
            Agents::Port(port) => Some(self.agent_on_port(port)),
        };
        self.services()
            .into_iter()
            .filter(|(family, agent, _)| {
                families.contains(family)
                    && match (agent, selected) {
                        (Some(agent), Some(selected)) => selected == Some(*agent),
                        _ => true,
                    }
            })
            .map(|(_, _, name)| name)
            .collect()
    }
}

/// The clients of the control API today, by what they need (`docs/design/flybus.md`, "Control
/// over the bus", grant table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The broadcast page: it waits for `/healthz` and may read `/status`.
    Stage,
    /// The Twitch bridge: reads, sugar from points and chat commands, on-screen chat lines.
    Bridge,
    /// The watchdog, the probation and shadow guards, the loop-recover ladder and the stream
    /// check: reads only. A restart is systemd's, never a control call.
    Watchdog,
    /// The operator's own hand: everything the control API has.
    Operator,
    /// `fly-control-edge`, which serves the unchanged HTTP surface on `:7401`. Loopback HTTP has
    /// no caller identity, so the edge carries exactly the authority `:7401` has today: every
    /// family. Narrowing a client is moving it off HTTP onto its own role.
    Edge,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Stage,
        Role::Bridge,
        Role::Watchdog,
        Role::Operator,
        Role::Edge,
    ];
    /// The roles with a socket of their own under [`ROLE_SOCKET_DIR`].
    pub const NATIVE: [Role; 4] = [Role::Stage, Role::Bridge, Role::Watchdog, Role::Operator];

    /// The participant id the router binds to this role's socket.
    pub const fn participant(self) -> &'static str {
        match self {
            Role::Stage => "fly-stage",
            Role::Bridge => "fly-bridge",
            Role::Watchdog => "fly-watchdog",
            Role::Operator => "fly-operator",
            Role::Edge => EDGE,
        }
    }

    /// The socket file name (under [`ROLE_SOCKET_DIR`] for native roles).
    pub const fn socket_name(self) -> &'static str {
        match self {
            Role::Stage => "stage.sock",
            Role::Bridge => "bridge.sock",
            Role::Watchdog => "watchdog.sock",
            Role::Operator => "operator.sock",
            Role::Edge => EDGE_SOCKET,
        }
    }

    /// The families this role may call.
    pub const fn families(self) -> &'static [Family] {
        match self {
            Role::Stage | Role::Watchdog => &[Family::Read, Family::Status],
            Role::Bridge => &[Family::Read, Family::Status, Family::Sugar, Family::Chat],
            Role::Operator | Role::Edge => &Family::ALL,
        }
    }
}

/// `<bus_dir>/control-edge.sock` or `<bus_dir>/control/<role>.sock`.
pub fn socket_path(bus_dir: &Path, role: Role) -> PathBuf {
    match role {
        Role::Edge => bus_dir.join(EDGE_SOCKET),
        native => bus_dir.join(ROLE_SOCKET_DIR).join(native.socket_name()),
    }
}

/// Exact `call` grants on `families` in `scope`, for the `agents` selected, nothing else.
pub fn call_grants(scope: &Scope, families: &[Family], agents: Agents<'_>) -> Grants {
    Grants {
        call: scope
            .services_of(families, agents)
            .iter()
            .map(|name| Pattern::exact(name))
            .collect(),
        ..Grants::default()
    }
}

/// What `role` may do: call its families' services in `scope`, for every agent, by exact name.
pub fn grants(role: Role, scope: &Scope) -> Grants {
    call_grants(scope, role.families(), Agents::All)
}

/// What `role` may do, limited to the `agents` selected: the same families, but the agent-scoped
/// ones only for that fly (or the fly on that port). A bridge for one fly of several, say.
pub fn grants_for(role: Role, scope: &Scope, agents: Agents<'_>) -> Grants {
    call_grants(scope, role.families(), agents)
}

/// A player backing one fly (by id or by port) in a multi-fly session: read the session, see
/// and sugar that fly, nothing else. Not wired to a socket yet; it is how per-agent scoping is
/// granted when players arrive.
pub fn player_grants(scope: &Scope, agents: Agents<'_>) -> Grants {
    call_grants(scope, &[Family::Read, Family::Status, Family::Sugar], agents)
}

/// The host's grant: register every control service of `scope`, by exact name, and nothing else.
pub fn host_grants(scope: &Scope) -> Grants {
    Grants {
        register: scope
            .services()
            .iter()
            .map(|(_, _, name)| Pattern::exact(name))
            .collect(),
        ..Grants::default()
    }
}

/// `base` plus the control participants: the host and every [`Role`].
pub fn policy(base: Policy, scope: &Scope) -> Policy {
    let mut policy = base.client(HOST, host_grants(scope));
    for role in Role::ALL {
        policy = policy.client(role.participant(), grants(role, scope));
    }
    policy
}

// -- encoding ---------------------------------------------------------------------------------

/// A request as `(family, method, payload)`.
pub fn encode_request(request: &ControlRequest) -> (Family, &'static str, Map<String, Value>) {
    let mut payload = Map::new();
    let mut body = |value: &Option<Value>| {
        if let Some(value) = value {
            payload.insert("request".to_owned(), value.clone());
        }
    };
    let (family, method) = match request {
        ControlRequest::Status => (Family::Read, "Control.Status"),
        ControlRequest::Healthz => (Family::Read, "Control.Healthz"),
        ControlRequest::Metrics => (Family::Read, "Control.Metrics"),
        ControlRequest::Events { .. } => (Family::Read, "Control.Events"),
        ControlRequest::Stimulate { body: value } => {
            body(value);
            (Family::Sugar, "Control.Stimulate")
        }
        ControlRequest::Reward { body: value } => {
            body(value);
            (Family::Reward, "Control.Reward")
        }
        ControlRequest::Chat { body: value } => {
            body(value);
            (Family::Chat, "Control.Chat")
        }
        ControlRequest::Checkpoint => (Family::Ops, "Control.Checkpoint"),
        ControlRequest::Pause => (Family::Ops, "Control.Pause"),
        ControlRequest::Resume => (Family::Ops, "Control.Resume"),
    };
    if let ControlRequest::Events { since, limit } = request {
        if let Some(since) = since {
            payload.insert("since".to_owned(), Value::String(since.clone()));
        }
        if let Some(limit) = limit {
            payload.insert("limit".to_owned(), Value::String(limit.clone()));
        }
    }
    (family, method, payload)
}

/// Move a `request` body larger than [`INLINE_MAX`] out of `payload` and return its JSON bytes,
/// leaving `requestArtifact: true` behind. `None` (payload untouched) when it fits.
pub fn take_out_of_line_request(payload: &mut Map<String, Value>) -> Option<Vec<u8>> {
    let bytes = serde_json::to_vec(payload.get("request")?).ok()?;
    if bytes.len() <= INLINE_MAX {
        return None;
    }
    payload.remove("request");
    payload.insert("requestArtifact".to_owned(), Value::Bool(true));
    Some(bytes)
}

/// Why a bus request could not be read: answered as a control-API reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Undecodable(pub ControlReply);

/// The request a `(family, method, payload)` names. A method the family does not have is a 404,
/// as an unknown route is over HTTP; an unknown or mistyped payload field is a 400.
pub fn decode_request(
    family: Family,
    method: &str,
    payload: &Map<String, Value>,
) -> Result<ControlRequest, Undecodable> {
    if !family.methods().contains(&method) {
        return Err(Undecodable(ControlReply::error(
            404,
            format!(
                "no such method: {method} on the {} service",
                family.as_str()
            ),
        )));
    }
    let allowed: &[&str] = match method {
        "Control.Events" => &["since", "limit"],
        "Control.Stimulate" | "Control.Reward" | "Control.Chat" => &["request"],
        _ => &[],
    };
    if let Some(field) = payload.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Undecodable(ControlReply::error(
            400,
            format!("{method}: unknown field {field:?}"),
        )));
    }
    let string = |key: &str| -> Result<Option<String>, Undecodable> {
        match payload.get(key) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(Undecodable(ControlReply::error(
                400,
                format!("{method}: {key} must be a string"),
            ))),
        }
    };
    let body = || payload.get("request").cloned();
    Ok(match method {
        "Control.Status" => ControlRequest::Status,
        "Control.Healthz" => ControlRequest::Healthz,
        "Control.Metrics" => ControlRequest::Metrics,
        "Control.Events" => ControlRequest::Events {
            since: string("since")?,
            limit: string("limit")?,
        },
        "Control.Stimulate" => ControlRequest::Stimulate { body: body() },
        "Control.Reward" => ControlRequest::Reward { body: body() },
        "Control.Chat" => ControlRequest::Chat { body: body() },
        "Control.Checkpoint" => ControlRequest::Checkpoint,
        "Control.Pause" => ControlRequest::Pause,
        "Control.Resume" => ControlRequest::Resume,
        _ => unreachable!("every method of every family is listed above"),
    })
}

/// A body too large for an envelope: its bytes and content type.
pub type OutOfLine = (Vec<u8>, &'static str);

/// A reply as an outcome, plus the bytes of an out-of-line body (and their content type) when
/// the body is larger than [`INLINE_MAX`].
pub fn encode_reply(reply: &ControlReply) -> (Map<String, Value>, Option<OutOfLine>) {
    let mut outcome = Map::new();
    outcome.insert("status".to_owned(), json!(reply.status));
    match &reply.body {
        ReplyBody::Json(value) => {
            let bytes = serde_json::to_vec(value).unwrap_or_default();
            if bytes.len() <= INLINE_MAX {
                outcome.insert("body".to_owned(), value.clone());
                (outcome, None)
            } else {
                outcome.insert("bodyArtifact".to_owned(), json!("application/json"));
                (outcome, Some((bytes, "application/json")))
            }
        }
        ReplyBody::Text(text) => {
            if text.len() <= INLINE_MAX {
                outcome.insert("text".to_owned(), Value::String(text.clone()));
                (outcome, None)
            } else {
                outcome.insert("bodyArtifact".to_owned(), json!("text/plain"));
                (outcome, Some((text.clone().into_bytes(), "text/plain")))
            }
        }
    }
}

/// The reply an outcome carries; `artifact` is the `body` attachment's bytes, read by the caller
/// only when the outcome names one.
pub fn decode_reply(
    outcome: &Map<String, Value>,
    artifact: Option<Vec<u8>>,
) -> Result<ControlReply, String> {
    let status = outcome
        .get("status")
        .and_then(Value::as_u64)
        .filter(|status| (100..=599).contains(status))
        .ok_or("a control outcome without a status")? as u16;
    if let Some(value) = outcome.get("body") {
        return Ok(ControlReply::json(status, value.clone()));
    }
    if let Some(Value::String(text)) = outcome.get("text") {
        return Ok(ControlReply::text(status, text.clone()));
    }
    match outcome.get("bodyArtifact").and_then(Value::as_str) {
        Some(kind) => {
            let bytes = artifact.ok_or("a control outcome whose body artifact is missing")?;
            match kind {
                "application/json" => serde_json::from_slice(&bytes)
                    .map(|value| ControlReply::json(status, value))
                    .map_err(|error| format!("a control body artifact is not JSON: {error}")),
                "text/plain" => String::from_utf8(bytes)
                    .map(|text| ControlReply::text(status, text))
                    .map_err(|_| "a control text artifact is not UTF-8".to_owned()),
                other => Err(format!("a control body artifact of kind {other:?}")),
            }
        }
        None => Err("a control outcome without a body".to_owned()),
    }
}

/// Whether an outcome's body travels as an artifact.
pub fn has_body_artifact(outcome: &Map<String, Value>) -> bool {
    outcome.contains_key("bodyArtifact")
}

// -- the host ---------------------------------------------------------------------------------

/// The registered control services. Dropping it unregisters them and stops answering.
pub struct ControlHost {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _client: Client,
}

impl Drop for ControlHost {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Connect to `router` in process as [`HOST`] and register every service of `scope`, each
/// answered by [`crate::control::handle`] over `state`.
///
/// One flysim process runs one fly, so its scope is [`Scope::live`] and every agent-scoped
/// service answers for that fly. A multi-fly host would hand each agent's services that agent's
/// state; the names, the grants and the encoding stay as they are.
///
/// Requests are taken in the order the router dispatches them (FIFO per service) and answered
/// concurrently, each in a task of its own, up to [`SERVICE_CONFIG`]'s in-flight bound: a pause
/// waiting on a durable write does not hold up a resume, as two HTTP requests do not wait on each
/// other. A caller that cancels or goes away has its result discarded by the router.
pub async fn serve(router: &Router, state: AppState, scope: &Scope) -> Result<ControlHost> {
    scope.validate()?;
    let transport = router.connect_in_memory_as(HOST);
    let client = Client::connect(transport, ClientConfig::new(HOST, router.store_root()))
        .await
        .map_err(|error| anyhow::anyhow!("the control host could not connect: {error}"))?;
    let mut tasks = Vec::new();
    for (family, _agent, name) in scope.services() {
        let mut service = client
            .register(&name, SERVICE_CONFIG)
            .await
            .map_err(|error| anyhow::anyhow!("registering {name}: {error}"))
            .context("starting the control services")?;
        let state = state.clone();
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(request) = service.next().await {
                let state = state.clone();
                let client = client.clone();
                tokio::spawn(async move { answer(&client, &state, family, request).await });
            }
        }));
    }
    tracing::info!(
        session = %scope.session,
        agents = ?scope.agents,
        services = scope.services().len(),
        "control services registered on the bus"
    );
    Ok(ControlHost {
        tasks,
        _client: client,
    })
}

/// The request's payload with an out-of-line `request` body put back in place.
async fn request_payload(request: &Request) -> Result<Map<String, Value>, ControlReply> {
    let mut payload = request.payload().clone();
    if payload.remove("requestArtifact").is_none() {
        return Ok(payload);
    }
    let bytes = match request.artifact(REQUEST_ARTIFACT) {
        Ok(artifact) => artifact.read_all().await,
        Err(error) => Err(error),
    }
    .map_err(|error| {
        ControlReply::error(400, format!("the request body could not be read: {error}"))
    })?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| ControlReply::error(400, format!("the request body is not JSON: {error}")))?;
    payload.insert("request".to_owned(), value);
    Ok(payload)
}

async fn answer(client: &Client, state: &AppState, family: Family, request: Request) {
    let reply = match request_payload(&request).await {
        Err(reply) => reply,
        Ok(payload) => match decode_request(family, request.method(), &payload) {
            Ok(decoded) => crate::control::handle(state, decoded).await,
            Err(Undecodable(reply)) => reply,
        },
    };
    let (outcome, out_of_line) = encode_reply(&reply);
    let result = match out_of_line {
        None => request.reply(outcome, &[]).await,
        Some((bytes, content_type)) => match seal(client, &bytes, content_type).await {
            Ok(artifact) => request.reply(outcome, &[(BODY_ARTIFACT, &artifact)]).await,
            Err(error) => {
                tracing::warn!(%error, "a control reply body could not be sealed");
                let (outcome, _) = encode_reply(&ControlReply::error(
                    503,
                    "the reply could not be stored on the bus",
                ));
                request.reply(outcome, &[]).await
            }
        },
    };
    if let Err(error) = result {
        // The caller is gone or the bus is closing; there is no one left to tell.
        tracing::debug!(%error, method = request.method(), "a control reply was not delivered");
    }
}

async fn seal(client: &Client, bytes: &[u8], content_type: &str) -> Result<Artifact, BusError> {
    let mut writer = client
        .artifacts()
        .allocate(bytes.len() as u64, content_type)
        .await?;
    writer
        .write_all(bytes)
        .map_err(|error| BusError::new(flybus::ErrorCode::StoreFailure, error.to_string()))?;
    writer.seal().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words no control service or method may contain (`docs/control-api.md`, the structural
    /// guarantee; the same list `api.rs` checks its routes against).
    const FORBIDDEN: [&str; 10] = [
        "button", "buttons", "input", "press", "joypad", "key", "poke", "write", "memory",
        "catalog",
    ];

    fn two_flies() -> Scope {
        Scope {
            session: "arena".to_owned(),
            agents: vec![
                AgentScope::new("fly-a", Some(1)),
                AgentScope::new("fly-b", Some(2)),
            ],
        }
    }

    fn names(scope: &Scope) -> Vec<String> {
        scope.services().into_iter().map(|(_, _, n)| n).collect()
    }

    #[test]
    fn no_service_or_method_names_an_input() {
        for scope in [Scope::live(), two_flies()] {
            for (family, _, name) in scope.services() {
                assert!(flybus::wire::is_name(&name), "{name} is not a bus name");
                assert!(name.starts_with(PREFIX), "{name}");
                for method in family.methods() {
                    let lowered = format!("{name} {method}").to_ascii_lowercase();
                    for forbidden in FORBIDDEN {
                        assert!(
                            !lowered.contains(forbidden),
                            "{lowered} contains {forbidden}"
                        );
                    }
                }
            }
        }
        // One method per control-api.md endpoint (the `/status.json` alias is HTTP-only), plus
        // the per-agent status.
        let mut methods: Vec<&str> = Family::ALL
            .iter()
            .flat_map(|f| f.methods())
            .copied()
            .collect();
        assert_eq!(methods.len(), 11, "{methods:?}");
        methods.sort();
        methods.dedup();
        assert_eq!(methods.len(), 10, "{methods:?}");
    }

    #[test]
    fn the_live_names_are_these() {
        assert_eq!(
            names(&Scope::live()),
            [
                "fly.control.s.main.read",
                "fly.control.s.main.a.fly.status",
                "fly.control.s.main.a.fly.sugar",
                "fly.control.s.main.a.fly.reward",
                "fly.control.s.main.chat",
                "fly.control.s.main.ops",
            ]
        );
    }

    #[test]
    fn every_grant_is_an_exact_call_on_a_control_service_and_nothing_else() {
        for scope in [Scope::live(), two_flies()] {
            let services = names(&scope);
            let mut all: Vec<Grants> = Role::ALL.iter().map(|r| grants(*r, &scope)).collect();
            for role in Role::ALL {
                all.push(grants_for(role, &scope, Agents::Port(1)));
                all.push(grants_for(role, &scope, Agents::Id("fly")));
            }
            all.push(player_grants(&scope, Agents::Id(&scope.agents[0].id)));
            all.push(player_grants(&scope, Agents::Port(7)));
            for grant in &all {
                assert!(grant.register.is_empty() && grant.publish.is_empty());
                assert!(grant.subscribe.is_empty() && grant.manage_topics.is_empty());
                for pattern in &grant.call {
                    match pattern {
                        Pattern::Exact(name) => assert!(services.contains(name), "{name}"),
                        other => panic!("a non-exact grant: {other:?}"),
                    }
                }
            }
            let host = host_grants(&scope);
            assert!(host.call.is_empty() && host.publish.is_empty() && host.subscribe.is_empty());
            assert!(host.manage_topics.is_empty());
            assert_eq!(host.register.len(), services.len());
            for pattern in &host.register {
                assert!(matches!(pattern, Pattern::Exact(name) if services.contains(name)));
            }
        }
    }

    #[test]
    fn the_grant_table() {
        let scope = Scope::live();
        let can = |role: Role, family: Family| {
            let name = scope.service(family, LIVE_AGENT);
            grants(role, &scope).call.iter().any(|p| p.matches(&name))
        };
        use Family::*;
        let table = [
            (Role::Stage, [true, true, false, false, false, false]),
            (Role::Bridge, [true, true, true, false, true, false]),
            (Role::Watchdog, [true, true, false, false, false, false]),
            (Role::Operator, [true, true, true, true, true, true]),
            (Role::Edge, [true, true, true, true, true, true]),
        ];
        for (role, row) in table {
            for (family, expected) in [Read, Status, Sugar, Reward, Chat, Ops].into_iter().zip(row)
            {
                assert_eq!(can(role, family), expected, "{role:?} {family:?}");
            }
        }
    }

    #[test]
    fn a_player_sugars_only_their_own_fly_by_id_or_by_port() {
        let scope = two_flies();
        for grant in [
            player_grants(&scope, Agents::Id("fly-a")),
            player_grants(&scope, Agents::Port(1)),
        ] {
            let allowed = |name: &str| grant.call.iter().any(|p| p.matches(name));
            assert!(allowed(&scope.service(Family::Sugar, "fly-a")));
            assert!(allowed(&scope.service(Family::Status, "fly-a")));
            assert!(!allowed(&scope.service(Family::Sugar, "fly-b")));
            assert!(!allowed(&scope.service(Family::Status, "fly-b")));
            assert!(allowed(&scope.service(Family::Read, "")));
            assert!(!allowed(&scope.service(Family::Chat, "")));
            assert!(!allowed(&scope.service(Family::Ops, "")));
            assert!(!allowed(&scope.service(Family::Reward, "fly-a")));
        }
        // A port nobody plays on reaches no fly, only the session's read.
        let empty = player_grants(&scope, Agents::Port(4));
        assert_eq!(empty.call, vec![Pattern::exact(&scope.service(Family::Read, ""))]);
    }

    #[test]
    fn a_role_limited_to_one_fly_keeps_its_families_for_that_fly_only() {
        let scope = two_flies();
        let bridge_b = grants_for(Role::Bridge, &scope, Agents::Port(2));
        let allowed = |name: &str| bridge_b.call.iter().any(|p| p.matches(name));
        assert!(allowed(&scope.service(Family::Sugar, "fly-b")));
        assert!(!allowed(&scope.service(Family::Sugar, "fly-a")));
        assert!(allowed(&scope.service(Family::Chat, "")));
        assert!(!allowed(&scope.service(Family::Reward, "fly-b")));
        // The live, single-fly table is the same table with one agent.
        let live = Scope::live();
        assert_eq!(
            grants_for(Role::Bridge, &live, Agents::Port(0)),
            grants(Role::Bridge, &live)
        );
    }

    #[test]
    fn scopes_refuse_ids_that_would_break_a_name() {
        for (session, agent) in [
            ("", "fly"),
            ("a.b", "fly"),
            ("main", "Fly"),
            ("main", "a.b"),
        ] {
            let scope = Scope {
                session: session.to_owned(),
                agents: vec![AgentScope::new(agent, None)],
            };
            assert!(scope.validate().is_err(), "{session:?} {agent:?}");
        }
        assert!(Scope::live().validate().is_ok());
        assert!(two_flies().validate().is_ok());
        let mut same_port = two_flies();
        same_port.agents[1].port = Some(1);
        assert!(same_port.validate().is_err());
        let mut same_id = two_flies();
        same_id.agents[1].id = "fly-a".to_owned();
        assert!(same_id.validate().is_err());
        let mut no_ports = two_flies();
        no_ports.agents.iter_mut().for_each(|a| a.port = None);
        assert!(no_ports.validate().is_ok());
    }

    #[test]
    fn requests_round_trip_through_the_encoding() {
        let requests = [
            ControlRequest::Status,
            ControlRequest::Healthz,
            ControlRequest::Metrics,
            ControlRequest::Events {
                since: None,
                limit: None,
            },
            ControlRequest::Events {
                since: Some("12".into()),
                limit: Some("x".into()),
            },
            ControlRequest::Stimulate { body: None },
            ControlRequest::Stimulate {
                body: Some(json!({"by": "a", "source": "chat"})),
            },
            ControlRequest::Reward {
                body: Some(json!([1, 2])),
            },
            ControlRequest::Chat {
                body: Some(json!("text")),
            },
            ControlRequest::Checkpoint,
            ControlRequest::Pause,
            ControlRequest::Resume,
        ];
        for request in requests {
            let (family, method, payload) = encode_request(&request);
            assert_eq!(
                decode_request(family, method, &payload),
                Ok(request.clone())
            );
        }
        // An agent's status service answers the status method.
        assert_eq!(
            decode_request(Family::Status, "Control.Status", &Map::new()),
            Ok(ControlRequest::Status)
        );
        assert!(decode_request(Family::Status, "Control.Healthz", &Map::new()).is_err());
    }

    #[test]
    fn a_method_on_the_wrong_service_is_a_404_and_a_stray_field_a_400() {
        let Err(Undecodable(reply)) = decode_request(Family::Read, "Control.Pause", &Map::new())
        else {
            panic!("decoded");
        };
        assert_eq!(reply.status, 404);
        let Err(Undecodable(reply)) = decode_request(Family::Read, "Control.Press", &Map::new())
        else {
            panic!("decoded");
        };
        assert_eq!(reply.status, 404);
        let mut payload = Map::new();
        payload.insert("buttons".into(), json!(1));
        let Err(Undecodable(reply)) = decode_request(Family::Sugar, "Control.Stimulate", &payload)
        else {
            panic!("decoded");
        };
        assert_eq!(reply.status, 400);
        let mut payload = Map::new();
        payload.insert("since".into(), json!(3));
        assert!(decode_request(Family::Read, "Control.Events", &payload).is_err());
    }

    #[test]
    fn replies_round_trip_inline_and_out_of_line() {
        let big = "x".repeat(INLINE_MAX + 1);
        for reply in [
            ControlReply::json(202, json!({"eventId": 7})),
            ControlReply::json(429, json!({"retryAfterMs": 1250.5})),
            ControlReply::text(200, "# HELP x\nx 1\n".into()),
            ControlReply::json(200, json!({ "events": [big.clone()] })),
            ControlReply::text(200, big.clone()),
        ] {
            let (outcome, artifact) = encode_reply(&reply);
            assert_eq!(has_body_artifact(&outcome), artifact.is_some());
            let encoded = serde_json::to_vec(&outcome).unwrap();
            assert!(
                encoded.len() <= INLINE_MAX + 64,
                "{} bytes inline",
                encoded.len()
            );
            let decoded = decode_reply(&outcome, artifact.map(|(bytes, _)| bytes)).unwrap();
            assert_eq!(decoded, reply);
        }
        assert!(decode_reply(&Map::new(), None).is_err());
    }
}
