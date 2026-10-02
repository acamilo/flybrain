//! The control API, `http://127.0.0.1:7401`: the HTTP layer.
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
//!
//! This module only turns HTTP into a [`ControlRequest`] and a [`ControlReply`] back into HTTP.
//! What a request does is [`crate::control::handle`]'s, reached through a [`ControlBackend`]:
//! [`AppState`] calls it in this process (`control.via = direct`), and `fly-control-edge` calls
//! it across the bus (`control.via = bus`, [`crate::controlbus`]). The extractors, the 404s, the
//! headers and the body encoding are this one copy in both cases, so the bytes on `:7401` do not
//! depend on which side of the bus answered.

use std::future::Future;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

pub use crate::control::COMMAND_TIMEOUT;
use crate::AppState;
use crate::control::{ControlReply, ControlRequest, METRICS_CONTENT_TYPE, ReplyBody};

/// Whatever answers a [`ControlRequest`]: the sim in this process, or the control services on
/// the bus.
pub trait ControlBackend: Clone + Send + Sync + 'static {
    fn handle(&self, request: ControlRequest) -> impl Future<Output = ControlReply> + Send;
}

impl ControlBackend for AppState {
    fn handle(&self, request: ControlRequest) -> impl Future<Output = ControlReply> + Send {
        let state = self.clone();
        async move { crate::control::handle(&state, request).await }
    }
}

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

pub fn router(state: AppState) -> Router {
    router_with(state)
}

/// The full control router over any backend: [`ROUTES`] and a 404 for everything else.
pub fn router_with<B: ControlBackend>(backend: B) -> Router {
    Router::new()
        .route("/status", get(status::<B>))
        .route("/status.json", get(status::<B>))
        .route("/stimulate", post(stimulate::<B>))
        .route("/reward", post(reward::<B>))
        .route("/chat", post(chat::<B>))
        .route("/checkpoint", post(checkpoint::<B>))
        .route("/pause", post(pause::<B>))
        .route("/resume", post(resume::<B>))
        .route("/events", get(events::<B>))
        .route("/healthz", get(healthz::<B>))
        .route("/metrics", get(prometheus::<B>))
        .fallback(not_found)
        // The reference server answers every unmatched method-and-path pair with the same 404,
        // so `GET /stimulate` is "no such route" rather than a 405. Being a drop-in replacement
        // for it matters more here than the distinction does.
        .method_not_allowed_fallback(not_found)
        .layer(axum::middleware::map_response(no_store))
        .with_state(backend)
}

/// The read-only listener: [`READ_ONLY_ROUTES`] and nothing else.
pub fn metrics_router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status::<AppState>))
        .route("/status.json", get(status::<AppState>))
        .route("/healthz", get(healthz::<AppState>))
        .route("/metrics", get(prometheus::<AppState>))
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

/// A [`ControlReply`] as HTTP: JSON bodies as `axum::Json` writes them, `/metrics` as
/// Prometheus text.
pub fn into_response(reply: ControlReply) -> Response {
    let status =
        StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    match reply.body {
        ReplyBody::Json(value) => (status, axum::Json(value)).into_response(),
        ReplyBody::Text(text) => {
            (status, [(header::CONTENT_TYPE, METRICS_CONTENT_TYPE)], text).into_response()
        }
    }
}

async fn answer<B: ControlBackend>(backend: &B, request: ControlRequest) -> Response {
    into_response(backend.handle(request).await)
}

/// The request body as JSON, or `None` when it is empty or not JSON (the reference server's
/// "malformed body").
fn parse_json(bytes: &Bytes) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

async fn status<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Status).await
}

async fn healthz<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Healthz).await
}

async fn prometheus<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Metrics).await
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<String>,
    limit: Option<String>,
}

async fn events<B: ControlBackend>(
    State(backend): State<B>,
    Query(query): Query<EventsQuery>,
) -> Response {
    let request = ControlRequest::Events {
        since: query.since,
        limit: query.limit,
    };
    answer(&backend, request).await
}

async fn stimulate<B: ControlBackend>(State(backend): State<B>, bytes: Bytes) -> Response {
    answer(&backend, ControlRequest::Stimulate { body: parse_json(&bytes) }).await
}

async fn reward<B: ControlBackend>(State(backend): State<B>, bytes: Bytes) -> Response {
    answer(&backend, ControlRequest::Reward { body: parse_json(&bytes) }).await
}

async fn chat<B: ControlBackend>(State(backend): State<B>, bytes: Bytes) -> Response {
    answer(&backend, ControlRequest::Chat { body: parse_json(&bytes) }).await
}

async fn checkpoint<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Checkpoint).await
}

async fn pause<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Pause).await
}

async fn resume<B: ControlBackend>(State(backend): State<B>) -> Response {
    answer(&backend, ControlRequest::Resume).await
}

async fn not_found(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    into_response(ControlReply::json(
        404,
        json!({ "error": format!("no such route: {method} {}", uri.path()) }),
    ))
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
}
