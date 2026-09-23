//! `flysim`: the fly brain and its Game Boy, as a service.
//!
//! One process runs the simulation, publishes 30 snapshots a second over a WebSocket, serves a
//! localhost control API and checkpoints itself. The two binding contracts are
//! `docs/feed-protocol.md` (the feed, port 7400) and `docs/control-api.md` (control, port 7401);
//! `docs/design/flysim.md` sections 4 to 9 are the implementation plan, with the override at the
//! top of that document taking precedence.
//!
//! ```text
//!                      +-- watch<Snapshot> --> feed  :7400/feed   (axum + ws)
//!  sim thread ---------+                   \-> feedbus -> flybus -> fly-edge :7400/feed
//!                      |                        (FLY_FEED_VIA=bus instead of the line above)
//!   agent              +-- Shared ------------> api   :7401        (axum)
//!   emulator           |                          /status /stimulate /reward /checkpoint
//!   adapter            |                          /pause /resume /events /healthz /metrics
//!   ratchet            +<-- mpsc<Command> ------ api
//!   event log
//!   checkpoints -------> store thread --------> save_dir + hot_dir
//! ```
//!
//! There is deliberately **no endpoint that presses buttons**. The decoder is the only writer of
//! joypad state; `api::ROUTES` is the whole surface and `tests/api.rs` asserts on it.

pub mod api;
pub mod chat;
pub mod config;
pub mod eventlog;
pub mod feed;
pub mod feedbus;
pub mod macros;
pub mod metrics;
pub mod pacing;
pub mod profile;
pub mod ratelimit;
pub mod reset;
pub mod sdnotify;
pub mod simloop;
pub mod snapshot;
pub mod store;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};

use crate::config::{Config, FeedVia};
use crate::eventlog::{EventRing, now_wall_ms};
use crate::simloop::{COMMAND_QUEUE, Command, Shared, Sim, booting_snapshot};
use crate::snapshot::Snapshot;

/// What the two listeners share with the sim thread.
#[derive(Clone)]
pub struct AppState {
    pub shared: Arc<Shared>,
    pub commands: mpsc::Sender<Command>,
    pub snapshots: watch::Receiver<Arc<Snapshot>>,
}

impl AppState {
    /// The newest published snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshots.borrow())
    }

    /// What the feed server needs, when flysim serves the feed itself.
    pub fn feed(&self) -> feed::FeedState {
        feed::FeedState {
            snapshots: self.snapshots.clone(),
            metrics: Arc::clone(&self.shared.metrics),
            idle_period: self.shared.config.publish_periods().1,
        }
    }
}

/// Run the service until a signal or a fatal simulation error.
///
/// The simulation runs on the calling thread and the listeners on a two-worker tokio runtime, so
/// a stalled socket cannot preempt the loop and the process exits when the loop does.
pub fn run(config: Config) -> Result<()> {
    let ring = EventRing::new();
    let shared = Arc::new(Shared::new(config.clone(), ring));
    let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE);
    let (snapshots_tx, snapshots) = watch::channel(Arc::new(booting_snapshot(
        0,
        now_wall_ms(),
        config.macros.mode,
    )));
    let state = AppState { shared: Arc::clone(&shared), commands: commands.clone(), snapshots };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("flysim-net")
        .enable_all()
        .build()
        .context("building the tokio runtime")?;

    let feed_addr = config.feed.bind;
    let control_addr = config.control.bind;
    let metrics_addr = config.control.metrics_bind;
    let via = config.feed.via;
    let listeners = runtime.block_on(async {
        // In bus mode the feed port belongs to `fly-edge`; binding it here would take it away.
        let feed = match via {
            FeedVia::Direct => Some(
                tokio::net::TcpListener::bind(feed_addr)
                    .await
                    .with_context(|| format!("binding the feed listener on {feed_addr}"))?,
            ),
            FeedVia::Bus => None,
        };
        let control = tokio::net::TcpListener::bind(control_addr)
            .await
            .with_context(|| format!("binding the control listener on {control_addr}"))?;
        let metrics = match metrics_addr {
            Some(addr) => Some(
                tokio::net::TcpListener::bind(addr)
                    .await
                    .with_context(|| format!("binding the metrics listener on {addr}"))?,
            ),
            None => None,
        };
        Ok::<_, anyhow::Error>((feed, control, metrics))
    })?;
    let (feed_listener, control_listener, metrics_listener) = listeners;
    tracing::info!(
        feed = %feed_addr,
        feed_via = via.as_str(),
        control = %control_addr,
        metrics = ?metrics_addr,
        "listening"
    );

    if let Some(feed_listener) = feed_listener {
        let state = state.feed();
        runtime.spawn(async move {
            if let Err(error) = axum::serve(feed_listener, feed::router(state)).await {
                tracing::error!(%error, "the feed listener stopped");
            }
        });
    }
    // The bus gets a runtime of its own, so neither its router nor the artifact copies can take
    // a worker from the control API; and it is fed from the watch slot, never from the sim thread.
    let bus_runtime = match via {
        FeedVia::Direct => None,
        FeedVia::Bus => {
            let bus_runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("flysim-bus")
                .enable_all()
                .build()
                .context("building the bus runtime")?;
            let bus = bus_runtime.block_on(feedbus::start_router(&config.feed.bus_dir))?;
            bus_runtime.spawn(feedbus::run_publisher(
                bus.router.clone(),
                state.snapshots.clone(),
                Arc::clone(&state.shared.metrics),
            ));
            Some((bus_runtime, bus))
        }
    };
    {
        let state = state.clone();
        runtime.spawn(async move {
            if let Err(error) = axum::serve(control_listener, api::router(state)).await {
                tracing::error!(%error, "the control listener stopped");
            }
        });
    }
    if let Some(listener) = metrics_listener {
        let state = state.clone();
        runtime.spawn(async move {
            if let Err(error) = axum::serve(listener, api::metrics_router(state)).await {
                tracing::error!(%error, "the metrics listener stopped");
            }
        });
    }
    runtime.spawn(shutdown_on_signal(commands));
    runtime.spawn(reload_chat_deny_list_on_sighup(Arc::clone(&state.shared)));

    let notifier = sdnotify::Notifier::from_env();
    let mut sim = Sim::boot(shared, snapshots_tx, command_rx)?;
    notifier.notify("READY=1\nSTATUS=simulation running\n");
    let result = sim.run(&notifier);
    notifier.notify("STOPPING=1\n");
    drop(sim);
    if let Some((bus_runtime, bus)) = bus_runtime {
        bus.router.shutdown();
        drop(bus);
        bus_runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    }
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

/// SIGHUP means "re-read the chat deny list" (`docs/control-api.md`, `[chat] deny_list`).
///
/// It deliberately does nothing else: no config reload, no restart. An operator editing
/// `/srv/fly/chat-deny.txt` should not risk the stream, and the loop picks the change up at its
/// next iteration (it also re-reads the file once a minute regardless, for an operator who
/// forgets the signal).
async fn reload_chat_deny_list_on_sighup(shared: Arc<Shared>) {
    let mut hangup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::error!(%error, "could not install the SIGHUP handler");
            return;
        }
    };
    while hangup.recv().await.is_some() {
        tracing::info!("SIGHUP: re-reading the chat deny list");
        shared.request_chat_reload();
    }
}

/// Turn SIGTERM and SIGINT into one clean shutdown command.
async fn shutdown_on_signal(commands: mpsc::Sender<Command>) {
    let mut terminate = match tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    ) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::error!(%error, "could not install the SIGTERM handler");
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT: shutting down"),
        _ = terminate.recv() => tracing::info!("SIGTERM: shutting down"),
    }
    let (reply, _ack) = tokio::sync::oneshot::channel();
    if commands.send(Command::Shutdown { reply }).await.is_err() {
        tracing::warn!("the simulation had already stopped");
    }
}
