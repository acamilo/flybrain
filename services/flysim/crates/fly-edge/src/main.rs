//! `fly-edge`: serve the feed WebSocket from flysim's feed bus.
//!
//! ```sh
//! FLY_FEED_VIA=bus flysim &
//! fly-edge
//! ```
//!
//! Configured through the same environment as flysim (`FLY_FEED_BIND`, `FLY_BUS_DIR`,
//! `FLYSIM_LOOP_IDLE_SNAPSHOT_HZ`, or `--config flysim.toml`), plus `FLY_EDGE_METRICS_ADDR` for
//! its own `/metrics` and `/healthz`. `infra/units/flyedge.service` runs it with no arguments.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use fly_edge::{EdgeConfig, EdgeMetrics};

#[derive(Debug, Parser)]
#[command(
    name = "fly-edge",
    about = "The feed WebSocket, served from flysim's feed bus.",
    version
)]
struct Args {
    /// Path to `flysim.toml`, read for `[feed]` and `[loop]`. Environment overrides apply as
    /// they do for flysim.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing();
    let config = flysim::config::Config::load(args.config.as_deref())?;
    let metrics_bind = match std::env::var("FLY_EDGE_METRICS_ADDR") {
        Ok(value) if !value.is_empty() => Some(value.parse().with_context(|| {
            format!("FLY_EDGE_METRICS_ADDR: {value:?} is not a host:port address")
        })?),
        _ => None,
    };
    let edge = EdgeConfig::from_flysim(&config, metrics_bind);
    if config.feed.via != flysim::config::FeedVia::Bus {
        tracing::warn!(
            "FLY_FEED_VIA is not \"bus\": flysim serves the feed itself and binds {}; \
             this edge will wait for a bus that is not there",
            edge.feed_bind
        );
    }
    tracing::info!(config = ?edge, "fly-edge starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("fly-edge")
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    runtime.block_on(async move {
        let metrics = Arc::new(EdgeMetrics::default());
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("installing the SIGTERM handler")?;
        tokio::select! {
            result = fly_edge::run(edge, metrics) => result,
            _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT: shutting down"); Ok(()) }
            _ = terminate.recv() => { tracing::info!("SIGTERM: shutting down"); Ok(()) }
        }
    })
}

/// Logs to stderr, like flysim, under `FLY_EDGE_LOG` (or `RUST_LOG`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("FLY_EDGE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
