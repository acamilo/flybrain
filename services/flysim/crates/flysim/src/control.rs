//! The control API's semantics, independent of how a request arrives (CTRL-01).
//!
//! Binding contract: `docs/control-api.md`. Every endpoint there is one [`ControlRequest`]
//! variant, and [`handle`] is the only code that decides what it does: the HTTP listener on
//! `:7401` ([`crate::api`]) and the control services on the bus ([`crate::controlbus`]) both call
//! it, so the status codes, the bodies and the commands the loop receives are one copy whichever
//! way the caller came in.
//!
//! [`ControlRequest`] is the whole surface. It has no variant that presses a button, edits game
//! memory or changes the reward catalog, and [`crate::simloop::Command`] has none either: the
//! structural guarantee of `docs/control-api.md` is a property of these two types.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::chat::ChatRefusal;
use crate::eventlog::now_wall_ms;
use crate::simloop::Command;
use crate::{AppState, metrics};

/// How long a caller waits for the sim thread to pick a command up. One frame is 16.7 ms, so
/// this is generous; exceeding it means the loop is wedged, which is a 503, not a hang.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// The Prometheus text exposition's content type (`GET /metrics`).
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// One control request, as `docs/control-api.md` lists them.
///
/// Bodies are carried as the caller sent them, parsed (`None` when the body was empty or not
/// JSON), and the `/events` query as its raw strings, so validation, its lenient defaults and
/// its error messages live in [`handle`] alone.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    /// `GET /status` (and its alias `/status.json`).
    Status,
    /// `GET /healthz`.
    Healthz,
    /// `GET /metrics`.
    Metrics,
    /// `GET /events?since=<id>&limit=<n>`.
    Events {
        since: Option<String>,
        limit: Option<String>,
    },
    /// `POST /stimulate`: a sugar (PAM) pulse.
    Stimulate { body: Option<Value> },
    /// `POST /reward`: disabled unless `control.allow_reward`.
    Reward { body: Option<Value> },
    /// `POST /chat`: one line for the chat ring; never reaches the simulation.
    Chat { body: Option<Value> },
    /// `POST /checkpoint`.
    Checkpoint,
    /// `POST /pause`.
    Pause,
    /// `POST /resume`.
    Resume,
}

impl ControlRequest {
    /// The `docs/control-api.md` path this request is (for logs and tables).
    pub fn path(&self) -> &'static str {
        match self {
            Self::Status => "/status",
            Self::Healthz => "/healthz",
            Self::Metrics => "/metrics",
            Self::Events { .. } => "/events",
            Self::Stimulate { .. } => "/stimulate",
            Self::Reward { .. } => "/reward",
            Self::Chat { .. } => "/chat",
            Self::Checkpoint => "/checkpoint",
            Self::Pause => "/pause",
            Self::Resume => "/resume",
        }
    }

    /// Whether the loop is waited on without a deadline: a forced checkpoint answers once the
    /// bytes are on disk, and a pause takes an implicit durable checkpoint.
    pub fn waits_for_a_write(&self) -> bool {
        matches!(self, Self::Checkpoint | Self::Pause)
    }
}

/// A reply body: JSON for every endpoint but `/metrics`, which is Prometheus text.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyBody {
    Json(Value),
    Text(String),
}

/// What [`handle`] answers: the `docs/control-api.md` status code and body.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlReply {
    pub status: u16,
    pub body: ReplyBody,
}

impl ControlReply {
    pub fn json(status: u16, value: Value) -> ControlReply {
        ControlReply {
            status,
            body: ReplyBody::Json(value),
        }
    }

    pub fn error(status: u16, message: impl std::fmt::Display) -> ControlReply {
        ControlReply::json(status, json!({ "error": message.to_string() }))
    }

    pub fn text(status: u16, text: String) -> ControlReply {
        ControlReply {
            status,
            body: ReplyBody::Text(text),
        }
    }
}

/// The messages a failed hand-off to the loop answers with (503). The bus edge uses the same
/// words for the same failures on its side of the bus, so a client cannot tell them apart.
pub mod unavailable {
    pub const QUEUE_FULL: &str = "the simulation command queue is full";
    pub const STOPPED: &str = "the simulation has stopped";
    pub const TIMED_OUT: &str = "the simulation did not answer in time";
    pub const DROPPED: &str = "the simulation dropped the request";
}

/// Answer one control request.
pub async fn handle(state: &AppState, request: ControlRequest) -> ControlReply {
    match request {
        ControlRequest::Status => status(state),
        ControlRequest::Healthz => healthz(state),
        ControlRequest::Metrics => prometheus(state),
        ControlRequest::Events { since, limit } => events(state, since, limit),
        ControlRequest::Stimulate { body } => stimulate(state, body).await,
        ControlRequest::Reward { body } => reward(state, body).await,
        ControlRequest::Chat { body } => chat(state, body).await,
        ControlRequest::Checkpoint => checkpoint(state).await,
        ControlRequest::Pause => pause(state).await,
        ControlRequest::Resume => resume(state).await,
    }
}

// -- GET /status ------------------------------------------------------------------------------

/// The feed header minus `events` and `attachments`, plus `version` and `checkpoint`.
///
/// Built by reshaping the header itself rather than by a parallel struct, so "same fields as the
/// feed header" cannot drift.
fn status(state: &AppState) -> ControlReply {
    let snapshot = state.snapshot();
    let mut value = match serde_json::to_value(&snapshot.header) {
        Ok(Value::Object(map)) => map,
        _ => return ControlReply::error(500, "could not render the status"),
    };
    value.remove("events");
    value.remove("attachments");
    let versions = state.shared.versions.get().cloned().unwrap_or_default();
    value.insert(
        "version".to_string(),
        serde_json::to_value(versions).unwrap_or(Value::Null),
    );
    let checkpoint = state
        .shared
        .checkpoint
        .lock()
        .map(|status| *status)
        .unwrap_or_default();
    value.insert(
        "checkpoint".to_string(),
        serde_json::to_value(checkpoint).unwrap_or(Value::Null),
    );
    // The readout's own numbers, additive: one row per channel with the role it reads, the resting
    // rate it is normalized against and the score the last decode computed, plus the roles a
    // restore has left for the next decode to calibrate (`docs/readout.md`).
    let decoder = state
        .shared
        .decoder
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default();
    value.insert(
        "decoder".to_string(),
        serde_json::to_value(decoder).unwrap_or(Value::Null),
    );
    ControlReply::json(200, Value::Object(value))
}

// -- GET /healthz -----------------------------------------------------------------------------

/// 200 while the loop advanced in the last 2 seconds, else 503. This is what systemd's
/// `wait-for-health` helper and `infra/bin/fly-watchdog` check first.
fn healthz(state: &AppState) -> ControlReply {
    if state.shared.healthy(now_wall_ms()) {
        ControlReply::json(200, json!({ "status": "ok" }))
    } else {
        ControlReply::error(503, "loop has not advanced in the last 2 seconds")
    }
}

// -- GET /metrics -----------------------------------------------------------------------------

fn prometheus(state: &AppState) -> ControlReply {
    let snapshot = state.snapshot();
    ControlReply::text(
        200,
        metrics::render(&state.shared.metrics, &snapshot, now_wall_ms()),
    )
}

// -- GET /events ------------------------------------------------------------------------------

/// `?since=<id>&limit=<n>`, parsed as leniently as the reference server's
/// `Number.parseInt(...) || 0`: anything unreadable falls back to the default.
fn events(state: &AppState, since: Option<String>, limit: Option<String>) -> ControlReply {
    let since = since
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let limit = limit
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(100)
        .min(crate::eventlog::RING_CAPACITY);
    let events = state.shared.events.page(since, limit);
    ControlReply::json(200, json!({ "events": events }))
}

// -- POST /stimulate --------------------------------------------------------------------------

/// `chat`, `points` or `operator`, as `isActionSource` in the reference server.
pub fn action_source(value: Option<&Value>) -> Option<&str> {
    match value.and_then(Value::as_str) {
        Some(source @ ("chat" | "points" | "operator")) => Some(source),
        _ => None,
    }
}

async fn stimulate(state: &AppState, request: Option<Value>) -> ControlReply {
    let Some(request) = request else {
        return ControlReply::error(400, "expected { durationMs?, by, source }");
    };
    let (Some(by), Some(source)) = (
        request.get("by").and_then(Value::as_str),
        action_source(request.get("source")),
    ) else {
        return ControlReply::error(400, "expected { durationMs?, by, source }");
    };
    let duration_ms = match request.get("durationMs") {
        None | Some(Value::Null) => None,
        Some(value) => match value
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            Some(value) => Some(value),
            None => {
                return ControlReply::error(400, "durationMs must be a positive number");
            }
        },
    };

    let (reply, response) = oneshot::channel();
    let command = Command::Stimulate {
        duration_ms,
        by: by.to_string(),
        source: source.to_string(),
        reply,
    };
    match dispatch(state, command, response).await {
        Ok(Ok(event_id)) => ControlReply::json(202, json!({ "eventId": event_id })),
        Ok(Err(refusal)) => {
            ControlReply::json(429, json!({ "retryAfterMs": refusal.retry_after_ms() }))
        }
        Err(reply) => reply,
    }
}

// -- POST /reward -----------------------------------------------------------------------------

async fn reward(state: &AppState, request: Option<Value>) -> ControlReply {
    if !state.shared.config.control.allow_reward {
        // Present so that the later "who trains the fly" work does not change the API.
        return ControlReply::error(403, "reward is disabled (control.allow_reward = false)");
    }
    let Some(request) = request else {
        return ControlReply::error(400, "expected { value, by, source }");
    };
    let (Some(value), Some(by), Some(source)) = (
        request
            .get("value")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite()),
        request.get("by").and_then(Value::as_str),
        action_source(request.get("source")),
    ) else {
        return ControlReply::error(400, "expected { value, by, source }");
    };

    let (reply, response) = oneshot::channel();
    let command = Command::Reward {
        value,
        by: by.to_string(),
        source: source.to_string(),
        reply,
    };
    match dispatch(state, command, response).await {
        Ok(event_id) => ControlReply::json(202, json!({ "eventId": event_id })),
        Err(reply) => reply,
    }
}

// -- POST /chat -------------------------------------------------------------------------------

/// `{ by, text, bot? }`: one line for the feed header's chat ring.
///
/// The kill switch is answered here, because it is pure configuration and a 403 should not need
/// the sim thread. Everything else — the name rule, the sanitizer, the deny list and the two rate
/// limits — is decided on the sim thread, which owns the ring, the limiter and the event log.
///
/// Status codes, per `docs/control-api.md`: 202 with the event id, 403 while chat is disabled,
/// 422 when a rule refused the line, 429 when a rate limit did, 400 for a malformed body.
async fn chat(state: &AppState, request: Option<Value>) -> ControlReply {
    if !state.shared.config.chat.enabled {
        return ControlReply::error(403, "chat is disabled (chat.enabled = false)");
    }
    let Some(request) = request else {
        return ControlReply::error(400, "expected { by, text, bot? }");
    };
    let (Some(by), Some(text)) = (
        request.get("by").and_then(Value::as_str),
        request.get("text").and_then(Value::as_str),
    ) else {
        state
            .shared
            .metrics
            .chat_rejected(crate::chat::RejectReason::Malformed);
        return ControlReply::error(400, "expected { by, text, bot? }");
    };
    let bot = match request.get("bot") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return ControlReply::error(400, "bot must be a boolean"),
    };

    let (reply, response) = oneshot::channel();
    let command = Command::Chat {
        by: by.to_string(),
        text: text.to_string(),
        bot,
        reply,
    };
    match dispatch(state, command, response).await {
        Ok(Ok(event_id)) => ControlReply::json(202, json!({ "eventId": event_id })),
        Ok(Err(ChatRefusal::Rejected(reason))) => ControlReply::json(
            422,
            json!({ "error": format!("chat line refused: {}", reason.as_str()) }),
        ),
        Ok(Err(ChatRefusal::RateLimited { retry_after_ms })) => {
            ControlReply::json(429, json!({ "retryAfterMs": retry_after_ms }))
        }
        Err(reply) => reply,
    }
}

// -- POST /checkpoint, /pause, /resume --------------------------------------------------------

async fn checkpoint(state: &AppState) -> ControlReply {
    let (reply, response) = oneshot::channel();
    // A forced checkpoint answers only once the bytes are on disk, so a caller that is about to
    // restart the service can rely on it.
    match dispatch_with_timeout(state, Command::Checkpoint { reply }, response, None).await {
        Ok(Ok(generation)) => ControlReply::json(200, json!({ "generation": generation })),
        Ok(Err(message)) => ControlReply::error(500, message),
        Err(reply) => reply,
    }
}

async fn pause(state: &AppState) -> ControlReply {
    let (reply, response) = oneshot::channel();
    // A pause takes an implicit durable checkpoint, so it can take as long as one write.
    match dispatch_with_timeout(state, Command::Pause { reply }, response, None).await {
        Ok(status) => ControlReply::json(200, json!({ "status": status })),
        Err(reply) => reply,
    }
}

async fn resume(state: &AppState) -> ControlReply {
    let (reply, response) = oneshot::channel();
    match dispatch(state, Command::Resume { reply }, response).await {
        Ok(status) => ControlReply::json(200, json!({ "status": status })),
        Err(reply) => reply,
    }
}

// -- plumbing ---------------------------------------------------------------------------------

async fn dispatch<T>(
    state: &AppState,
    command: Command,
    response: oneshot::Receiver<T>,
) -> Result<T, ControlReply> {
    dispatch_with_timeout(state, command, response, Some(COMMAND_TIMEOUT)).await
}

/// Queue a command and wait for its reply.
///
/// The queue is bounded: a full queue is a 503 rather than a wait, because the only way to fill
/// it is a wedged loop or a flood, and neither is improved by blocking the caller.
async fn dispatch_with_timeout<T>(
    state: &AppState,
    command: Command,
    response: oneshot::Receiver<T>,
    timeout: Option<Duration>,
) -> Result<T, ControlReply> {
    if let Err(error) = state.commands.try_send(command) {
        return Err(match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                ControlReply::error(503, unavailable::QUEUE_FULL)
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                ControlReply::error(503, unavailable::STOPPED)
            }
        });
    }
    let awaited = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, response).await {
            Ok(result) => result,
            Err(_) => return Err(ControlReply::error(503, unavailable::TIMED_OUT)),
        },
        None => response.await,
    };
    awaited.map_err(|_| ControlReply::error(503, unavailable::DROPPED))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_sources_are_the_three_the_contract_names() {
        for source in ["chat", "points", "operator"] {
            assert_eq!(action_source(Some(&json!(source))), Some(source));
        }
        for rejected in [json!("bridge"), json!(1), json!(null), Value::Null] {
            assert_eq!(action_source(Some(&rejected)), None);
        }
        assert_eq!(action_source(None), None);
    }
}
