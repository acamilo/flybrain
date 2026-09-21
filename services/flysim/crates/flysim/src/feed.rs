//! The snapshot feed, `ws://127.0.0.1:7400/feed`.
//!
//! Binding contract: `docs/feed-protocol.md`. Behavioural reference:
//! `packages/feed/src/fake/server.ts`.
//!
//! - one binary message per snapshot, framed by [`crate::snapshot::Snapshot::encode`];
//! - the client sends exactly one JSON `hello` and nothing else; `wants` is honoured per
//!   connection, so the bridge (which asks for no attachments) costs no bandwidth;
//! - 30 snapshots a second while running, 2 while paused or booting (header only);
//! - drop-oldest, never queue: the sim publishes into a `watch` slot, so a slow client misses
//!   snapshots instead of slowing the loop down. Those misses are counted.

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde::Deserialize;

use crate::snapshot::{AttachmentKind, FeedStatus, PROTOCOL, Snapshot, Wants};
use crate::{AppState, metrics::Metrics};

/// The one JSON text message a client sends on connect.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientHello {
    pub protocol: u32,
    /// `stage`, `bridge` or `test`. Recorded in the log; it changes no behaviour.
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub wants: Vec<AttachmentKind>,
}

/// Close code for a protocol violation, as the reference server uses.
const CLOSE_PROTOCOL_ERROR: u16 = 1002;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/feed", any(upgrade))
        .fallback(not_found)
        .with_state(state)
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

async fn upgrade(upgrade: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    upgrade.on_upgrade(move |socket| serve_client(socket, state))
}

async fn serve_client(mut socket: WebSocket, state: AppState) {
    let Some(hello) = read_hello(&mut socket).await else {
        return;
    };
    let wants = Wants::from_kinds(&hello.wants);
    tracing::info!(
        client = hello.client.as_deref().unwrap_or("unknown"),
        frame = wants.frame,
        audio = wants.audio,
        spikes = wants.spikes,
        "feed client connected"
    );
    state.shared.metrics.client_joined();
    let result = pump(&mut socket, &state, wants).await;
    state.shared.metrics.client_left();
    match result {
        Ok(()) => tracing::info!("feed client disconnected"),
        Err(error) => tracing::info!(%error, "feed client dropped"),
    }
}

/// Wait for the `hello`, closing on anything that is not one.
async fn read_hello(socket: &mut WebSocket) -> Option<ClientHello> {
    while let Some(message) = socket.recv().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            // Pings and pongs are answered by the transport; keep waiting for the hello.
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Binary(_)) => {
                let _ = socket
                    .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: CLOSE_PROTOCOL_ERROR,
                        reason: "hello must be JSON".into(),
                    })))
                    .await;
                return None;
            }
            Ok(Message::Close(_)) | Err(_) => return None,
        };
        let hello: ClientHello = match serde_json::from_str(&text) {
            Ok(hello) => hello,
            Err(_) => {
                let _ = socket
                    .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: CLOSE_PROTOCOL_ERROR,
                        reason: "hello must be JSON".into(),
                    })))
                    .await;
                return None;
            }
        };
        if hello.protocol != PROTOCOL {
            let _ = socket
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: CLOSE_PROTOCOL_ERROR,
                    reason: format!("unsupported protocol {}", hello.protocol).into(),
                })))
                .await;
            return None;
        }
        return Some(hello);
    }
    None
}

async fn pump(socket: &mut WebSocket, state: &AppState, wants: Wants) -> Result<(), axum::Error> {
    let mut receiver = state.snapshots.clone();
    let (_, idle_period) = state.shared.config.publish_periods();
    let mut last_seq = 0u64;

    // The current snapshot first, so a client that connects while paused or booting sees the
    // state immediately instead of waiting for the next publish. Every borrow of the watch slot
    // ends before the await: the guard is a lock, and holding one across a send would let a slow
    // client block the sim thread's next publish.
    let current = receiver.borrow_and_update().clone();
    send(socket, state, &current, wants, &mut last_seq).await?;

    loop {
        tokio::select! {
            // A client is a display: after the hello it says nothing. Reading anyway is what
            // notices a close and keeps the transport's ping/pong answered.
            incoming = socket.recv() => match incoming {
                None | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Err(error)) => return Err(error),
                Some(Ok(_)) => continue,
            },
            changed = tokio::time::timeout(idle_period, receiver.changed()) => match changed {
                Ok(Ok(())) => {
                    let snapshot = receiver.borrow_and_update().clone();
                    send(socket, state, &snapshot, wants, &mut last_seq).await?;
                }
                // The sim thread is gone.
                Ok(Err(_)) => return Ok(()),
                Err(_) => {
                    // No new snapshot for a whole idle period. While running that is a stall
                    // and there is nothing to say; while booting or paused it is the protocol's
                    // 2 Hz header-only cadence.
                    let snapshot = receiver.borrow().clone();
                    if snapshot.header.status != FeedStatus::Running {
                        send(socket, state, &snapshot, wants, &mut last_seq).await?;
                    }
                }
            },
        }
    }
}

async fn send(
    socket: &mut WebSocket,
    state: &AppState,
    snapshot: &Arc<Snapshot>,
    wants: Wants,
    last_seq: &mut u64,
) -> Result<(), axum::Error> {
    let seq = snapshot.header.seq;
    if seq > *last_seq + 1 && *last_seq != 0 {
        Metrics::add(&state.shared.metrics.feed_dropped, seq - *last_seq - 1);
    }
    *last_seq = seq;
    socket.send(Message::Binary(snapshot.encode(wants).into())).await?;
    Metrics::incr(&state.shared.metrics.frames_sent);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_is_parsed_as_the_contract_spells_it() {
        let hello: ClientHello = serde_json::from_str(
            r#"{"protocol":1,"client":"stage","wants":["frame","audio","spikes"]}"#,
        )
        .unwrap();
        assert_eq!(hello.protocol, 1);
        assert_eq!(hello.client.as_deref(), Some("stage"));
        assert_eq!(Wants::from_kinds(&hello.wants), Wants::all());

        // The bridge asks for no attachments.
        let hello: ClientHello =
            serde_json::from_str(r#"{"protocol":1,"client":"bridge","wants":[]}"#).unwrap();
        assert_eq!(Wants::from_kinds(&hello.wants), Wants::none());

        // A hello with no `wants` at all is the same as an empty one.
        let hello: ClientHello = serde_json::from_str(r#"{"protocol":1,"client":"test"}"#).unwrap();
        assert_eq!(Wants::from_kinds(&hello.wants), Wants::none());
    }

    #[test]
    fn an_unknown_attachment_kind_is_a_rejected_hello_rather_than_a_silent_drop() {
        assert!(serde_json::from_str::<ClientHello>(r#"{"protocol":1,"wants":["video"]}"#).is_err());
    }
}
