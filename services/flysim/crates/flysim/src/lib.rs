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
//!
//! With `FLY_CONTROL_VIA=bus` the control API is not bound here: `control` answers the same
//! requests as RPC services on the embedded router (`controlbus`, `bus`), and `fly-control-edge`
//! serves `:7401` with this crate's own `api` router calling them.

pub mod api;
pub mod bus;
pub mod chat;
pub mod config;
pub mod control;
pub mod controlbus;
pub mod eventlog;
pub mod feed;
pub mod feedbus;
pub mod frame;
pub mod journal;
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
pub mod trace;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};

use crate::config::{Config, ControlVia, FeedVia};
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
    serve(config, |shared, snapshots, commands, notifier| {
        let mut sim = Sim::boot(shared, snapshots, commands)?;
        notifier.notify("READY=1\nSTATUS=simulation running\n");
        let result = sim.run(notifier);
        notifier.notify("STOPPING=1\n");
        drop(sim);
        result
    })
}

/// Bind the listeners and serve them around `sim`, which runs on the calling thread and
/// returns when the service should stop; it sends `READY=1` once it is up.
///
/// `sim` is handed the state the listeners share, the snapshot slot they read, the command
/// queue they write, and the systemd notifier. The legacy loop ([`Sim`], through [`run`]) and the
/// session runtime (`fly-legacy-session`'s service host, SERVE-01) are both run this way, so the
/// feed listener, the bus publisher, the control API, the metrics listener and the signal
/// handling are one copy of the code whichever runtime is behind them: the binding contracts
/// (`docs/feed-protocol.md`, `docs/control-api.md`) are served by the same bytes.
pub fn serve<F>(config: Config, sim: F) -> Result<()>
where
    F: FnOnce(
        Arc<Shared>,
        watch::Sender<Arc<Snapshot>>,
        mpsc::Receiver<Command>,
        &sdnotify::Notifier,
    ) -> Result<()>,
{
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
    let control_via = config.control.via;
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
        // Likewise the control port belongs to `fly-control-edge` when control rides the bus.
        let control = match control_via {
            ControlVia::Direct => Some(
                tokio::net::TcpListener::bind(control_addr)
                    .await
                    .with_context(|| format!("binding the control listener on {control_addr}"))?,
            ),
            ControlVia::Bus => None,
        };
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
        control_via = control_via.as_str(),
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
    // One router for whatever rides the bus (`bus`): the feed publisher and the control
    // services are two participants on it.
    let uses = bus::Uses {
        feed: via == FeedVia::Bus,
        control: control_via == ControlVia::Bus,
    };
    let bus_runtime = if uses.feed || uses.control {
        let bus_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("flysim-bus")
            // EDGE-02 review N1: the router, the publisher and the artifact copies cost
            // milliseconds a frame; on the sim's dispatcher cpu they stall every barrier. When the
            // session reserved aux cpus (FLY_SESSION_AUX_CPUS) these threads, the blocking pool
            // included, run there; otherwise they keep the host mask they inherited.
            .on_thread_start(|| {
                flybrain_core::pool::confine_to_aux();
            })
            .enable_all()
            .build()
            .context("building the bus runtime")?;
        let scope = controlbus::Scope::live();
        let bus = bus_runtime.block_on(bus::start(&config.feed.bus_dir, uses, &scope))?;
        if uses.feed {
            bus_runtime.spawn(feedbus::run_publisher(
                bus.router.clone(),
                state.snapshots.clone(),
                Arc::clone(&state.shared.metrics),
            ));
        }
        let control_host = if uses.control {
            Some(bus_runtime.block_on(controlbus::serve(&bus.router, state.clone(), &scope))?)
        } else {
            None
        };
        Some((bus_runtime, bus, control_host))
    } else {
        None
    };
    if let Some(control_listener) = control_listener {
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
    let result = sim(shared, snapshots_tx, command_rx, &notifier);
    if let Some((bus_runtime, bus, control_host)) = bus_runtime {
        // The publisher ends by itself once the watch sender is gone; stopping the runtime under
        // it, rather than the router first, keeps a last in-flight publish from being logged as
        // a refusal. The edges see their sockets close either way.
        drop(control_host);
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
