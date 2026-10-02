//! `fly-control-edge`: serve the control API (`:7401`) from flysim's control services on the bus.
//!
//! ```sh
//! FLY_CONTROL_VIA=bus flysim-session &
//! fly-control-edge
//! ```
//!
//! Configured through the same environment as flysim (`FLY_CONTROL_BIND`, `FLY_BUS_DIR`, or
//! `--config flysim.toml`), plus `FLY_CONTROL_EDGE_METRICS_ADDR` for its own `/metrics` and
//! `/healthz`. `infra/units/flycontrol-edge.service` runs it with no arguments.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use fly_control_edge::{EdgeConfig, EdgeMetrics};

#[derive(Debug, Parser)]
#[command(
    name = "fly-control-edge",
    about = "The control API, served from flysim's control services on the bus.",
    version
)]
struct Args {
    /// Path to `flysim.toml`, read for `[control]` and `[feed] bus_dir`. Environment overrides
    /// apply as they do for flysim.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing();
    let config = flysim::config::Config::load(args.config.as_deref())?;
    let metrics_bind = match std::env::var("FLY_CONTROL_EDGE_METRICS_ADDR") {
        Ok(value) if !value.is_empty() => Some(value.parse().with_context(|| {
            format!("FLY_CONTROL_EDGE_METRICS_ADDR: {value:?} is not a host:port address")
        })?),
        _ => None,
    };
    let edge = EdgeConfig::from_flysim(&config, metrics_bind);
    if config.control.via != flysim::config::ControlVia::Bus {
        tracing::warn!(
            "FLY_CONTROL_VIA is not \"bus\": flysim serves the control API itself and binds {}; \
             this edge will wait for control services that are not there",
            edge.control_bind
        );
    }
    tracing::info!(config = ?edge, "fly-control-edge starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("fly-control-edge")
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    runtime.block_on(async move {
        let metrics = Arc::new(EdgeMetrics::default());
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("installing the SIGTERM handler")?;
        tokio::select! {
            result = fly_control_edge::run(edge, metrics) => result,
            _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT: shutting down"); Ok(()) }
            _ = terminate.recv() => { tracing::info!("SIGTERM: shutting down"); Ok(()) }
        }
    })
}

/// Logs to stderr, like flysim, under `FLY_CONTROL_EDGE_LOG` (or `RUST_LOG`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("FLY_CONTROL_EDGE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
