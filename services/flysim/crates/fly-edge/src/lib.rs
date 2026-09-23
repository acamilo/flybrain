//! `fly-edge`: the feed WebSocket, served from flysim's feed bus.
//!
//! With `FLY_FEED_VIA=bus` flysim does not bind the feed port. It publishes every snapshot on an
//! embedded flybus router (`flysim::feedbus`), and this process subscribes and serves
//! `ws://<feed.bind>/feed` to the stage, the bridge and tests. The contract is still
//! `docs/feed-protocol.md`, byte for byte: the snapshots come off the bus as the same
//! `flysim::snapshot::Snapshot` values and are written by the same `flysim::feed` server, so the
//! per-client `hello`, `wants`, drop-oldest and idle cadence are flysim's own code.
//!
//! Lifecycle (`docs/design/flybus.md`, amendment "Feed store lifecycle"):
//!
//! - the feed port is bound only once the first snapshot has arrived, so before that a client
//!   is refused exactly as it would be by a flysim that has not started;
//! - when the bus goes away (flysim stopped or restarted) the edge drops every client and unbinds
//!   the port, again exactly what a stopped flysim looks like to the stage, then reconnects every
//!   `retry` until a router answers. It never serves a stale snapshot as if it were live.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use flybus::{Client, ClientConfig, SubscriptionConfig};
use flysim::feed::{self, FeedState};
use flysim::feedbus;
use flysim::metrics::{Metrics, metric};
use tokio::sync::{oneshot, watch};

/// What the edge needs to know. Built from flysim's own configuration, so both processes read
/// one environment file and cannot disagree about the port, the bus directory or the cadence.
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// `feed.bus_dir`: the router's socket and store root.
    pub bus_dir: PathBuf,
    /// `feed.bind`, the port flysim leaves alone in bus mode.
    pub feed_bind: SocketAddr,
    /// `1 / loop.idle_snapshot_hz`, the protocol's header-only cadence.
    pub idle_period: Duration,
    /// `FLY_EDGE_METRICS_ADDR`: `/metrics` and `/healthz` for the watchdog, when set.
    pub metrics_bind: Option<SocketAddr>,
    /// Delay between attempts to reach the bus.
    pub retry: Duration,
}

impl EdgeConfig {
    pub fn from_flysim(config: &flysim::config::Config, metrics_bind: Option<SocketAddr>) -> Self {
        Self {
            bus_dir: config.feed.bus_dir.clone(),
            feed_bind: config.feed.bind,
            idle_period: config.publish_periods().1,
            metrics_bind,
            retry: Duration::from_millis(500),
        }
    }
}

/// The edge's counters. `feed` is the same `Metrics` type flysim uses, so
/// `fly_frames_sent_total` and `fly_feed_clients` mean exactly what they mean there.
#[derive(Debug, Default)]
pub struct EdgeMetrics {
    pub feed: Arc<Metrics>,
    /// Snapshots taken off the bus and handed to the feed server.
    pub snapshots: AtomicU64,
    /// 1 while subscribed and serving.
    pub connected: AtomicU64,
    /// Times a serving session ended because the bus went away.
    pub bus_lost: AtomicU64,
    /// Publications that could not be turned back into a snapshot.
    pub decode_failures: AtomicU64,
    /// Sessions that reached the bus but could not bind the feed port.
    pub bind_failures: AtomicU64,
}

impl EdgeMetrics {
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(1_024);
        let feed = &self.feed;
        metric(
            &mut out,
            "fly_frames_sent_total",
            "counter",
            "Feed snapshots written to a client socket.",
            Metrics::get(&feed.frames_sent),
        );
        metric(
            &mut out,
            "fly_feed_clients",
            "gauge",
            "Feed clients currently subscribed.",
            feed.clients(),
        );
        metric(
            &mut out,
            "fly_feed_dropped_total",
            "counter",
            "Snapshots superseded before a slow client could be sent them.",
            Metrics::get(&feed.feed_dropped),
        );
        metric(
            &mut out,
            "fly_edge_snapshots_total",
            "counter",
            "Snapshots taken off the feed bus.",
            self.snapshots.load(Ordering::Relaxed),
        );
        metric(
            &mut out,
            "fly_edge_bus_connected",
            "gauge",
            "1 while the edge is subscribed to the feed bus and serving.",
            self.connected.load(Ordering::Relaxed),
        );
        metric(
            &mut out,
            "fly_edge_bus_lost_total",
            "counter",
            "Serving sessions ended by the feed bus going away.",
            self.bus_lost.load(Ordering::Relaxed),
        );
        metric(
            &mut out,
            "fly_edge_decode_failures_total",
            "counter",
            "Feed bus publications that did not decode to a snapshot.",
            self.decode_failures.load(Ordering::Relaxed),
        );
        metric(
            &mut out,
            "fly_edge_bind_failures_total",
            "counter",
            "Times the bus was reachable but the feed port could not be bound.",
            self.bind_failures.load(Ordering::Relaxed),
        );
        out
    }
}

/// Serve until the process is stopped. Only a metrics listener that cannot bind is fatal;
/// everything about the bus is retried.
pub async fn run(config: EdgeConfig, metrics: Arc<EdgeMetrics>) -> Result<()> {
    if let Some(addr) = config.metrics_bind {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding the edge metrics listener on {addr}"))?;
        let app = axum::Router::new()
            .route("/metrics", get(prometheus))
            .route("/healthz", get(healthz))
            .with_state(Arc::clone(&metrics));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "the edge metrics listener stopped");
            }
        });
    }
    // One line per outage of each kind, not one per retry.
    let mut last: Option<&'static str> = None;
    loop {
        let end = session(&config, &metrics).await;
        match &end {
            SessionEnd::BusLost => {
                tracing::warn!("the feed bus went away; clients dropped, reconnecting");
            }
            SessionEnd::Unreachable(error) if last != Some(end.kind()) => {
                tracing::info!(error = format!("{error:#}"), "waiting for the feed bus");
            }
            SessionEnd::BindFailed(error) if last != Some(end.kind()) => {
                // The bus is fine; the port is not ours. Most likely flysim is still in direct
                // mode and holds it (FLY_FEED_VIA is not bus), or another process does.
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the bus is up but the feed port cannot be bound; retrying"
                );
            }
            _ => {}
        }
        last = match end {
            SessionEnd::BusLost => None,
            other => Some(other.kind()),
        };
        tokio::time::sleep(config.retry).await;
    }
}

/// Why a [`session`] ended.
enum SessionEnd {
    /// No router answered, or it closed before the first snapshot. Nothing was served.
    Unreachable(anyhow::Error),
    /// Subscribed and holding a snapshot, but the feed port could not be bound.
    BindFailed(anyhow::Error),
    /// A session that served has ended because the bus went away.
    BusLost,
}

impl SessionEnd {
    fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "unreachable",
            Self::BindFailed(_) => "bind",
            Self::BusLost => "lost",
        }
    }
}

async fn prometheus(State(metrics): State<Arc<EdgeMetrics>>) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        metrics.render(),
    )
}

async fn healthz(State(metrics): State<Arc<EdgeMetrics>>) -> impl IntoResponse {
    if metrics.connected.load(Ordering::Relaxed) == 1 {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "waiting for the feed bus")
    }
}

/// One subscription's lifetime.
async fn session(config: &EdgeConfig, metrics: &EdgeMetrics) -> SessionEnd {
    let (subscription, client, first) = match subscribe(config, metrics).await {
        Ok(subscribed) => subscribed,
        Err(error) => return SessionEnd::Unreachable(error),
    };
    let listener = match tokio::net::TcpListener::bind(config.feed_bind).await {
        Ok(listener) => listener,
        Err(error) => {
            metrics.bind_failures.fetch_add(1, Ordering::Relaxed);
            return SessionEnd::BindFailed(
                anyhow::Error::new(error)
                    .context(format!("binding the feed listener on {}", config.feed_bind)),
            );
        }
    };
    serve(config, metrics, client, subscription, first, listener).await;
    SessionEnd::BusLost
}

/// Connect, subscribe and wait for the first snapshot that decodes.
async fn subscribe(
    config: &EdgeConfig,
    metrics: &EdgeMetrics,
) -> Result<(flybus::Subscription, Client, flysim::snapshot::Snapshot)> {
    let client = Client::connect_unix(
        feedbus::socket_path(&config.bus_dir),
        ClientConfig::new(feedbus::EDGE, feedbus::store_root(&config.bus_dir)),
    )
    .await
    .map_err(|error| anyhow!("connecting to the feed bus: {error}"))?;
    // One in flight: while a snapshot is being copied out, the next one waits in the single
    // latest slot and anything newer replaces it. The edge is never more than one behind.
    let mut subscription = client
        .subscribe(
            feedbus::TOPIC,
            SubscriptionConfig::latest().in_flight(1).replay(true),
        )
        .await
        .map_err(|error| anyhow!("subscribing to {}: {error}", feedbus::TOPIC))?;
    let first = loop {
        let message = subscription
            .next()
            .await
            .ok_or_else(|| anyhow!("the feed bus closed before the first snapshot"))?;
        match feedbus::receive(&message).await {
            Ok(snapshot) => break snapshot,
            Err(error) => {
                metrics.decode_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, "a feed bus publication did not decode");
            }
        }
    };
    Ok((subscription, client, first))
}

/// Serve `listener` from `subscription` until the bus goes away.
async fn serve(
    config: &EdgeConfig,
    metrics: &EdgeMetrics,
    client: Client,
    mut subscription: flybus::Subscription,
    first: flysim::snapshot::Snapshot,
    listener: tokio::net::TcpListener,
) {
    let (snapshots, receiver) = watch::channel(Arc::new(first));
    metrics.snapshots.fetch_add(1, Ordering::Relaxed);
    tracing::info!(feed = %config.feed_bind, bus = %config.bus_dir.display(), "serving the feed from the bus");
    metrics.connected.store(1, Ordering::Relaxed);

    let state = FeedState {
        snapshots: receiver,
        metrics: Arc::clone(&metrics.feed),
        idle_period: config.idle_period,
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let result = axum::serve(listener, feed::router(state))
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
        if let Err(error) = result {
            tracing::error!(%error, "the feed listener stopped");
        }
    });

    while let Some(message) = subscription.next().await {
        match feedbus::receive(&message).await {
            Ok(snapshot) => {
                drop(message);
                snapshots.send_replace(Arc::new(snapshot));
                metrics.snapshots.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                metrics.decode_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, "a feed bus publication did not decode");
                if client.closed().is_some() {
                    break;
                }
            }
        }
    }

    // The bus is gone. Dropping the sender ends every client's pump (a closed stream, as when
    // flysim itself stops), and the graceful shutdown unbinds the port.
    metrics.connected.store(0, Ordering::Relaxed);
    metrics.bus_lost.fetch_add(1, Ordering::Relaxed);
    drop(snapshots);
    let _ = stop.send(());
    if tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .is_err()
    {
        tracing::warn!("the feed listener took more than 5 s to stop");
    }
}
