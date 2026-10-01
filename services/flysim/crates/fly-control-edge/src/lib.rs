//! `fly-control-edge`: the control API on `:7401`, answered by flysim's control services on the
//! bus (CTRL-01, `docs/design/flybus.md`, "Control over the bus").
//!
//! With `FLY_CONTROL_VIA=bus` flysim (either runtime) does not bind `control.bind`. It registers
//! the control services on its embedded router (`flysim::controlbus`), and this process serves the
//! HTTP surface of `docs/control-api.md` to the bridge, the stage, the infra scripts and the
//! operator's `curl` by calling them. The HTTP layer is flysim's own `api::router_with`, so the
//! routes, the 404s, the extractors, the headers and the body encoding are the code direct mode
//! runs; this crate only adds a [`BusBackend`] that carries each request across the bus.
//!
//! Lifecycle (as `fly-edge` for the feed):
//!
//! - the control port is bound only once the control services answer, so before that a client
//!   is refused exactly as it is by a flysim that has not started;
//! - when the bus goes away (flysim stopped or restarted) the edge unbinds the port, again what a
//!   stopped flysim looks like, and reconnects every `retry` until a router answers.
//!
//! Failures on the bus side map onto the 503s direct mode answers for the same failure (the
//! words are `flysim::control::unavailable`), so a client cannot tell which side was in trouble.

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
use flybus::{BusError, Client, ClientConfig, Dispatch, ErrorCode};
use flysim::api::ControlBackend;
use flysim::control::{COMMAND_TIMEOUT, ControlReply, ControlRequest, unavailable};
use flysim::controlbus::{self, Role, Scope};
use flysim::feedbus;
use flysim::metrics::metric;
use tokio::sync::oneshot;

/// How much longer than the services' own [`COMMAND_TIMEOUT`] the edge waits before it gives up
/// on a timed request and cancels it. The services answer a wedged loop with their own 503 at
/// [`COMMAND_TIMEOUT`]; this guard only covers a bus or a host that stopped answering at all.
pub const DEADLINE_MARGIN: Duration = Duration::from_secs(2);

/// What the edge needs to know, from flysim's own configuration.
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// `feed.bus_dir`: the router's sockets and store root.
    pub bus_dir: PathBuf,
    /// `control.bind`, the port flysim leaves alone in bus mode.
    pub control_bind: SocketAddr,
    /// `FLY_CONTROL_EDGE_METRICS_ADDR`: the edge's own `/metrics` and `/healthz`, when set.
    pub metrics_bind: Option<SocketAddr>,
    /// Delay between attempts to reach the bus.
    pub retry: Duration,
    /// The session and agents whose services `:7401` addresses.
    pub scope: Scope,
}

impl EdgeConfig {
    pub fn from_flysim(config: &flysim::config::Config, metrics_bind: Option<SocketAddr>) -> Self {
        Self {
            bus_dir: config.feed.bus_dir.clone(),
            control_bind: config.control.bind,
            metrics_bind,
            retry: Duration::from_millis(500),
            scope: Scope::live(),
        }
    }
}

/// The edge's counters.
#[derive(Debug, Default)]
pub struct EdgeMetrics {
    /// 1 while connected and serving.
    pub connected: AtomicU64,
    /// Requests carried across the bus.
    pub calls: AtomicU64,
    /// Requests that ended in a bus failure rather than a reply (each a 503 or 403 to the
    /// client).
    pub call_failures: AtomicU64,
    /// Requests the edge cancelled at its own deadline.
    pub cancelled: AtomicU64,
    /// Serving sessions ended by the bus going away.
    pub bus_lost: AtomicU64,
    /// Sessions that reached the bus but could not bind the control port.
    pub bind_failures: AtomicU64,
}

impl EdgeMetrics {
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(1_024);
        for (name, kind, help, value) in [
            (
                "fly_control_edge_bus_connected",
                "gauge",
                "1 while the edge is connected to the control bus and serving.",
                &self.connected,
            ),
            (
                "fly_control_edge_calls_total",
                "counter",
                "Control requests carried across the bus.",
                &self.calls,
            ),
            (
                "fly_control_edge_call_failures_total",
                "counter",
                "Control requests that ended in a bus failure instead of a reply.",
                &self.call_failures,
            ),
            (
                "fly_control_edge_cancelled_total",
                "counter",
                "Control requests the edge cancelled at its own deadline.",
                &self.cancelled,
            ),
            (
                "fly_control_edge_bus_lost_total",
                "counter",
                "Serving sessions ended by the control bus going away.",
                &self.bus_lost,
            ),
            (
                "fly_control_edge_bind_failures_total",
                "counter",
                "Times the bus was reachable but the control port could not be bound.",
                &self.bind_failures,
            ),
        ] {
            metric(&mut out, name, kind, help, value.load(Ordering::Relaxed));
        }
        out
    }
}

/// A [`ControlBackend`] that answers each request by calling the control services on the bus.
#[derive(Clone)]
pub struct BusBackend {
    client: Client,
    scope: Scope,
    metrics: Arc<EdgeMetrics>,
}

impl BusBackend {
    pub fn new(client: Client, scope: Scope, metrics: Arc<EdgeMetrics>) -> BusBackend {
        BusBackend {
            client,
            scope,
            metrics,
        }
    }

    /// The bus connection.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The service a request goes to: the session's, or the first agent's for the agent-scoped
    /// families (`:7401` addresses the one live fly; a second fly has no HTTP surface).
    fn service(&self, family: controlbus::Family) -> String {
        let agent = self.scope.agent_ids().next().unwrap_or("");
        self.scope.service(family, agent)
    }

    /// Carry one request across the bus and wait for its reply, under the edge's deadline.
    pub async fn call(&self, request: ControlRequest) -> ControlReply {
        self.metrics.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = if request.waits_for_a_write() {
            None
        } else {
            Some(COMMAND_TIMEOUT + DEADLINE_MARGIN)
        };
        let (family, method, payload) = controlbus::encode_request(&request);
        let service = self.service(family);
        let mut pending = match self.client.call(&service, None, method, payload, &[]).await {
            Ok(pending) => pending,
            Err(error) => return self.failed(&error),
        };
        let result = match deadline {
            None => pending.result().await,
            Some(deadline) => match tokio::time::timeout(deadline, pending.result()).await {
                Ok(result) => result,
                Err(_) => {
                    // Cancelled before dispatch, the loop never sees it; dispatched, it may still
                    // act, exactly as a direct request whose caller timed out.
                    let state = pending.cancel().await;
                    self.metrics.cancelled.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(method, ?state, "a control call passed the edge's deadline");
                    return ControlReply::error(503, unavailable::TIMED_OUT);
                }
            },
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.failed(&error),
        };
        let artifact = if controlbus::has_body_artifact(result.outcome()) {
            match result.artifact(controlbus::BODY_ARTIFACT) {
                Ok(artifact) => match artifact.read_all().await {
                    Ok(bytes) => Some(bytes),
                    Err(error) => return self.failed(&error),
                },
                Err(error) => return self.failed(&error),
            }
        } else {
            None
        };
        match controlbus::decode_reply(result.outcome(), artifact) {
            Ok(reply) => reply,
            Err(error) => {
                self.metrics.call_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, method, "an undecodable control reply");
                ControlReply::error(502, format!("the control service answered badly: {error}"))
            }
        }
    }

    fn failed(&self, error: &BusError) -> ControlReply {
        self.metrics.call_failures.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(%error, "a control call failed on the bus");
        bus_failure(error)
    }
}

impl ControlBackend for BusBackend {
    fn handle(
        &self,
        request: ControlRequest,
    ) -> impl std::future::Future<Output = ControlReply> + Send {
        let backend = self.clone();
        async move { backend.call(request).await }
    }
}

/// The reply a client gets for a request the bus did not deliver, in the words direct mode uses
/// for the same failure.
pub fn bus_failure(error: &BusError) -> ControlReply {
    match error.code {
        // The service's queue and in-flight slots are full: the direct API's full command queue.
        ErrorCode::Backpressure => ControlReply::error(503, unavailable::QUEUE_FULL),
        // Nobody is serving it: before dispatch the loop never saw it, as a closed command queue.
        ErrorCode::NoService if error.dispatch == Dispatch::NotDispatched => {
            ControlReply::error(503, unavailable::STOPPED)
        }
        ErrorCode::NoService | ErrorCode::CallGone | ErrorCode::RouterLost => {
            ControlReply::error(503, unavailable::DROPPED)
        }
        ErrorCode::NotAuthorized => ControlReply::error(
            403,
            format!("not authorized on the control bus: {}", error.message),
        ),
        code => ControlReply::error(
            503,
            format!("the control bus refused the request: {}", code.as_str()),
        ),
    }
}

/// Serve until the process is stopped. Only a metrics listener that cannot bind is fatal;
/// everything about the bus is retried.
pub async fn run(config: EdgeConfig, metrics: Arc<EdgeMetrics>) -> Result<()> {
    config.scope.validate()?;
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
    let mut last: Option<&'static str> = None;
    loop {
        let end = session(&config, &metrics).await;
        match &end {
            SessionEnd::BusLost => {
                tracing::warn!("the control bus went away; :7401 unbound, reconnecting");
            }
            SessionEnd::Unreachable(error) if last != Some(end.kind()) => {
                tracing::info!(error = format!("{error:#}"), "waiting for the control bus");
            }
            SessionEnd::BindFailed(error) if last != Some(end.kind()) => {
                // The bus is fine; the port is not ours. Most likely flysim is still in direct
                // mode and holds it (FLY_CONTROL_VIA is not bus), or another process does.
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the bus is up but the control port cannot be bound; retrying"
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

enum SessionEnd {
    Unreachable(anyhow::Error),
    BindFailed(anyhow::Error),
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
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "waiting for the control bus",
        )
    }
}

/// Connect as [`controlbus::EDGE`] and wait until the control services answer.
pub async fn connect(config: &EdgeConfig, metrics: &Arc<EdgeMetrics>) -> Result<BusBackend> {
    let client = Client::connect_unix(
        controlbus::socket_path(&config.bus_dir, Role::Edge),
        ClientConfig::new(controlbus::EDGE, feedbus::store_root(&config.bus_dir)),
    )
    .await
    .map_err(|error| anyhow!("connecting to the control bus: {error}"))?;
    let backend = BusBackend::new(client, config.scope.clone(), Arc::clone(metrics));
    // The host registers its services right after the router starts; until it has, a call is
    // NO_SERVICE. Bind :7401 only once one is answered, whatever it answers.
    let (family, method, payload) = controlbus::encode_request(&ControlRequest::Healthz);
    let service = backend.service(family);
    for _ in 0..20 {
        match backend
            .client
            .call(&service, None, method, payload.clone(), &[])
            .await
        {
            Ok(mut pending) => match pending.result().await {
                Ok(_) => return Ok(backend),
                Err(error) if error.code == ErrorCode::NoService => {}
                Err(error) => return Err(anyhow!("the control services did not answer: {error}")),
            },
            Err(error) if error.code == ErrorCode::NoService => {}
            Err(error) => return Err(anyhow!("calling the control services: {error}")),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!(
        "the control services are not registered on the bus"
    ))
}

async fn session(config: &EdgeConfig, metrics: &Arc<EdgeMetrics>) -> SessionEnd {
    let backend = match connect(config, metrics).await {
        Ok(backend) => backend,
        Err(error) => return SessionEnd::Unreachable(error),
    };
    let listener = match tokio::net::TcpListener::bind(config.control_bind).await {
        Ok(listener) => listener,
        Err(error) => {
            metrics.bind_failures.fetch_add(1, Ordering::Relaxed);
            return SessionEnd::BindFailed(anyhow::Error::new(error).context(format!(
                "binding the control listener on {}",
                config.control_bind
            )));
        }
    };
    serve(config, metrics, backend, listener).await;
    SessionEnd::BusLost
}

/// Serve `listener` until the bus connection closes.
async fn serve(
    config: &EdgeConfig,
    metrics: &EdgeMetrics,
    backend: BusBackend,
    listener: tokio::net::TcpListener,
) {
    tracing::info!(control = %config.control_bind, bus = %config.bus_dir.display(), "serving the control API from the bus");
    metrics.connected.store(1, Ordering::Relaxed);
    let client = backend.client.clone();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let result = axum::serve(listener, flysim::api::router_with(backend))
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
        if let Err(error) = result {
            tracing::error!(%error, "the control listener stopped");
        }
    });
    while client.closed().is_none() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    metrics.connected.store(0, Ordering::Relaxed);
    metrics.bus_lost.fetch_add(1, Ordering::Relaxed);
    let _ = stop.send(());
    if tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .is_err()
    {
        tracing::warn!("the control listener took more than 5 s to stop");
    }
}
