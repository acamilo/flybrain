//! The control API, `http://127.0.0.1:7401`.
//!
//! Binding contract: `docs/control-api.md`. Behavioural reference:
//! `packages/feed/src/fake/server.ts`, which this must be a drop-in replacement for — same
//! status codes, same bodies, same validation.
//!
//! > There is deliberately **no endpoint that presses buttons, edits game memory, or changes the
//! > reward catalog**. That is a structural guarantee, not a configuration.
//!
//! [`ROUTES`] is the entire surface; the router is built from it, so a route cannot be added
//! without appearing in that table, and `tests/api.rs` asserts on the table itself.

use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::chat::ChatRefusal;
use crate::eventlog::now_wall_ms;
use crate::simloop::Command;
use crate::{AppState, metrics};

/// Every route the control API serves, in the order `docs/control-api.md` lists them.
///
/// `/status.json` is an alias of `/status` (the read-only metrics listener that
/// `infra/bin/fly-watchdog` scrapes asks for that name); `/metrics` is Prometheus text.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/status"),
    ("GET", "/status.json"),
    ("POST", "/stimulate"),
    ("POST", "/reward"),
    ("POST", "/chat"),
    ("POST", "/checkpoint"),
    ("POST", "/pause"),
    ("POST", "/resume"),
    ("GET", "/events"),
    ("GET", "/healthz"),
    ("GET", "/metrics"),
];

/// The read-only subset served on `control.metrics_bind` (`FLY_METRICS_ADDR`), which is reachable
/// from outside the container and therefore carries nothing that changes state.
pub const READ_ONLY_ROUTES: &[(&str, &str)] = &[
    ("GET", "/status"),
    ("GET", "/status.json"),
    ("GET", "/healthz"),
    ("GET", "/metrics"),
];

/// How long a caller waits for the sim thread to pick a command up. One frame is 16.7 ms, so
/// this is generous; exceeding it means the loop is wedged, which is a 503, not a hang.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/status.json", get(status))
        .route("/stimulate", post(stimulate))
        .route("/reward", post(reward))
        .route("/chat", post(chat))
        .route("/checkpoint", post(checkpoint))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
        .route("/events", get(events))
        .route("/healthz", get(healthz))
        .route("/metrics", get(prometheus))
        .fallback(not_found)
        // The reference server answers every unmatched method-and-path pair with the same 404,
        // so `GET /stimulate` is "no such route" rather than a 405. Being a drop-in replacement
        // for it matters more here than the distinction does.
        .method_not_allowed_fallback(not_found)
        .layer(axum::middleware::map_response(no_store))
        .with_state(state)
}

/// The read-only listener: [`READ_ONLY_ROUTES`] and nothing else.
pub fn metrics_router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/status.json", get(status))
        .route("/healthz", get(healthz))
        .route("/metrics", get(prometheus))
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(axum::middleware::map_response(no_store))
        .with_state(state)
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn body(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    body(status, json!({ "error": message.to_string() }))
}

// -- GET /status ------------------------------------------------------------------------------

/// The feed header minus `events` and `attachments`, plus `version` and `checkpoint`.
///
/// Built by reshaping the header itself rather than by a parallel struct, so "same fields as the
/// feed header" cannot drift.
async fn status(State(state): State<AppState>) -> Response {
    let snapshot = state.snapshot();
    let mut value = match serde_json::to_value(&snapshot.header) {
        Ok(Value::Object(map)) => map,
        _ => return error(StatusCode::INTERNAL_SERVER_ERROR, "could not render the status"),
    };
    value.remove("events");
    value.remove("attachments");
    let versions = state.shared.versions.get().cloned().unwrap_or_default();
    value.insert("version".to_string(), serde_json::to_value(versions).unwrap_or(Value::Null));
    let checkpoint = state
        .shared
        .checkpoint
        .lock()
        .map(|status| *status)
        .unwrap_or_default();
    value.insert("checkpoint".to_string(), serde_json::to_value(checkpoint).unwrap_or(Value::Null));
    // The readout's own numbers, additive: one row per channel with the role it reads, the resting
    // rate it is normalized against and the score the last decode computed, plus the roles a
    // restore has left for the next decode to calibrate (`docs/readout.md`).
    let decoder = state
        .shared
        .decoder
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default();
    value.insert("decoder".to_string(), serde_json::to_value(decoder).unwrap_or(Value::Null));
    body(StatusCode::OK, Value::Object(value))
}

// -- GET /healthz -----------------------------------------------------------------------------

/// 200 while the loop advanced in the last 2 seconds, else 503. This is what systemd's
/// `wait-for-health` helper and `infra/bin/fly-watchdog` check first.
async fn healthz(State(state): State<AppState>) -> Response {
    if state.shared.healthy(now_wall_ms()) {
        body(StatusCode::OK, json!({ "status": "ok" }))
    } else {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "loop has not advanced in the last 2 seconds",
        )
    }
}

// -- GET /metrics -----------------------------------------------------------------------------

async fn prometheus(State(state): State<AppState>) -> Response {
    let snapshot = state.snapshot();
    let text = metrics::render(&state.shared.metrics, &snapshot, now_wall_ms());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        text,
    )
        .into_response()
}

// -- GET /events ------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<String>,
    limit: Option<String>,
}

/// `?since=<id>&limit=<n>`, parsed as leniently as the reference server's
/// `Number.parseInt(...) || 0`: anything unreadable falls back to the default.
async fn events(State(state): State<AppState>, Query(query): Query<EventsQuery>) -> Response {
    let since = query
        .since
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let limit = query
        .limit
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(100)
        .min(crate::eventlog::RING_CAPACITY);
    let events = state.shared.events.page(since, limit);
    body(StatusCode::OK, json!({ "events": events }))
}

// -- POST /stimulate --------------------------------------------------------------------------

fn parse_json(bytes: &Bytes) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

/// `chat`, `points` or `operator`, as `isActionSource` in the reference server.
fn action_source(value: Option<&Value>) -> Option<&str> {
    match value.and_then(Value::as_str) {
        Some(source @ ("chat" | "points" | "operator")) => Some(source),
        _ => None,
    }
}

async fn stimulate(State(state): State<AppState>, bytes: Bytes) -> Response {
    let Some(request) = parse_json(&bytes) else {
        return error(StatusCode::BAD_REQUEST, "expected { durationMs?, by, source }");
    };
    let (Some(by), Some(source)) = (
        request.get("by").and_then(Value::as_str),
        action_source(request.get("source")),
    ) else {
        return error(StatusCode::BAD_REQUEST, "expected { durationMs?, by, source }");
    };
    let duration_ms = match request.get("durationMs") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_f64().filter(|value| value.is_finite() && *value > 0.0) {
            Some(value) => Some(value),
            None => {
                return error(StatusCode::BAD_REQUEST, "durationMs must be a positive number");
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
    match dispatch(&state, command, response).await {
        Ok(Ok(event_id)) => body(StatusCode::ACCEPTED, json!({ "eventId": event_id })),
        Ok(Err(refusal)) => body(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "retryAfterMs": refusal.retry_after_ms() }),
        ),
        Err(response) => response,
    }
}

// -- POST /reward -----------------------------------------------------------------------------

async fn reward(State(state): State<AppState>, bytes: Bytes) -> Response {
    if !state.shared.config.control.allow_reward {
        // Present so that the later "who trains the fly" work does not change the API.
        return error(
            StatusCode::FORBIDDEN,
            "reward is disabled (control.allow_reward = false)",
        );
    }
    let Some(request) = parse_json(&bytes) else {
        return error(StatusCode::BAD_REQUEST, "expected { value, by, source }");
    };
    let (Some(value), Some(by), Some(source)) = (
        request.get("value").and_then(Value::as_f64).filter(|value| value.is_finite()),
        request.get("by").and_then(Value::as_str),
        action_source(request.get("source")),
    ) else {
        return error(StatusCode::BAD_REQUEST, "expected { value, by, source }");
    };

    let (reply, response) = oneshot::channel();
    let command = Command::Reward {
        value,
        by: by.to_string(),
        source: source.to_string(),
        reply,
    };
    match dispatch(&state, command, response).await {
        Ok(event_id) => body(StatusCode::ACCEPTED, json!({ "eventId": event_id })),
        Err(response) => response,
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
async fn chat(State(state): State<AppState>, bytes: Bytes) -> Response {
    if !state.shared.config.chat.enabled {
        return error(StatusCode::FORBIDDEN, "chat is disabled (chat.enabled = false)");
    }
    let Some(request) = parse_json(&bytes) else {
        return error(StatusCode::BAD_REQUEST, "expected { by, text, bot? }");
    };
    let (Some(by), Some(text)) = (
        request.get("by").and_then(Value::as_str),
        request.get("text").and_then(Value::as_str),
    ) else {
        state
            .shared
            .metrics
            .chat_rejected(crate::chat::RejectReason::Malformed);
        return error(StatusCode::BAD_REQUEST, "expected { by, text, bot? }");
    };
    let bot = match request.get("bot") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return error(StatusCode::BAD_REQUEST, "bot must be a boolean"),
    };

    let (reply, response) = oneshot::channel();
    let command = Command::Chat { by: by.to_string(), text: text.to_string(), bot, reply };
    match dispatch(&state, command, response).await {
        Ok(Ok(event_id)) => body(StatusCode::ACCEPTED, json!({ "eventId": event_id })),
        Ok(Err(ChatRefusal::Rejected(reason))) => body(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "error": format!("chat line refused: {}", reason.as_str()) }),
        ),
        Ok(Err(ChatRefusal::RateLimited { retry_after_ms })) => body(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "retryAfterMs": retry_after_ms }),
        ),
        Err(response) => response,
    }
}

// -- POST /checkpoint, /pause, /resume --------------------------------------------------------

async fn checkpoint(State(state): State<AppState>) -> Response {
    let (reply, response) = oneshot::channel();
    // A forced checkpoint answers only once the bytes are on disk, so a caller that is about to
    // restart the service can rely on it.
    match dispatch_with_timeout(&state, Command::Checkpoint { reply }, response, None).await {
        Ok(Ok(generation)) => body(StatusCode::OK, json!({ "generation": generation })),
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(response) => response,
    }
}

async fn pause(State(state): State<AppState>) -> Response {
    let (reply, response) = oneshot::channel();
    // A pause takes an implicit durable checkpoint, so it can take as long as one write.
    match dispatch_with_timeout(&state, Command::Pause { reply }, response, None).await {
        Ok(status) => body(StatusCode::OK, json!({ "status": status })),
        Err(response) => response,
    }
}

async fn resume(State(state): State<AppState>) -> Response {
    let (reply, response) = oneshot::channel();
    match dispatch(&state, Command::Resume { reply }, response).await {
        Ok(status) => body(StatusCode::OK, json!({ "status": status })),
        Err(response) => response,
    }
}

// -- plumbing ---------------------------------------------------------------------------------

async fn not_found(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    error(
        StatusCode::NOT_FOUND,
        format!("no such route: {method} {}", uri.path()),
    )
}

async fn dispatch<T>(
    state: &AppState,
    command: Command,
    response: oneshot::Receiver<T>,
) -> Result<T, Response> {
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
) -> Result<T, Response> {
    if let Err(error) = state.commands.try_send(command) {
        return Err(match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => self::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "the simulation command queue is full",
            ),
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                self::error(StatusCode::SERVICE_UNAVAILABLE, "the simulation has stopped")
            }
        });
    }
    let awaited = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, response).await {
            Ok(result) => result,
            Err(_) => {
                return Err(self::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the simulation did not answer in time",
                ));
            }
        },
        None => response.await,
    };
    awaited.map_err(|_| {
        self::error(StatusCode::SERVICE_UNAVAILABLE, "the simulation dropped the request")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_route_table_contains_no_input_path_of_any_kind() {
        // The structural guarantee of `docs/control-api.md`, asserted over the table the router
        // is built from. `tests/api.rs` also drives the built router over HTTP.
        for (_, path) in ROUTES.iter().chain(READ_ONLY_ROUTES) {
            let lowered = path.to_ascii_lowercase();
            for forbidden in [
                "button", "buttons", "input", "press", "joypad", "key", "poke", "write", "memory",
                "catalog",
            ] {
                assert!(!lowered.contains(forbidden), "{path} contains {forbidden}");
            }
        }
    }

    #[test]
    fn the_read_only_listener_exposes_no_mutating_route() {
        for (method, path) in READ_ONLY_ROUTES {
            assert_eq!(*method, "GET", "{path}");
            assert!(ROUTES.contains(&(method, path)), "{path} is not a control route");
        }
    }

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
