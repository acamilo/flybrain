//! `flysim-session`: the live fly on the session runtime, as a drop-in for `flysim` (SERVE-01).
//!
//! ```sh
//! flysim-session                 # what flysim.service runs after `fly-runtime session`
//! flysim-session --print-compatibility
//! ```
//!
//! It reads exactly `flysim`'s configuration (`/etc/fly/fly.env` through the unit, or
//! `--config flysim.toml`), writes the same checkpoint stores, event log, sugar journal and chat
//! sidecar, and serves the same feed, control API and metrics. Three variables are its own:
//! `FLY_SESSION_DIR` (the session's router store and sockets, default `/run/fly/session`),
//! `FLY_SESSION_MODE` (`in-process`, `thread` or `process`) and `FLY_SESSION_PROFILE`
//! (`production`, or `toy` for tests).
//!
//! The one-shot flags are `flysim`'s: `--check-config`, `--print-compatibility` (the session's
//! own string, which must equal `flysim`'s), `--print-state-compatibility` and
//! `--reset-to-milestone` (the store is shared, so these are `flysim`'s own functions).

use anyhow::Result;
use clap::Parser;
use flysim::config::Config;

#[derive(Debug, Parser)]
#[command(
    name = "flysim-session",
    about = "The fly brain and its Game Boy on the session runtime: flysim's feed, control API and checkpoints.",
    version
)]
struct Args {
    /// Path to `flysim.toml`. Without it, defaults plus `FLY_*` / `FLYSIM_*` environment
    /// overrides are used, exactly as for `flysim`.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    /// Print the resolved configuration and exit.
    #[arg(long)]
    check_config: bool,

    /// Print the checkpoint compatibility string this build would write, and exit.
    #[arg(long)]
    print_compatibility: bool,

    /// Print the compatibility string the checkpoints in this directory carry, and exit.
    #[arg(long, value_name = "DIR")]
    print_state_compatibility: Option<std::path::PathBuf>,

    /// Restart the run from the milestone archive for this ladder rung, and exit (with the
    /// service stopped; `flysim --reset-to-milestone`, the same store).
    #[arg(long, value_name = "RANK")]
    reset_to_milestone: Option<u32>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing();

    let config = Config::load(args.config.as_deref())?;
    let options = fly_legacy_session::service::ServiceOptions::from_env()?;
    tracing::info!(config = ?config, options = ?options, "resolved configuration");
    if args.check_config {
        println!("{}", toml::to_string_pretty(&config)?);
        return Ok(());
    }
    if args.print_compatibility {
        println!(
            "{}",
            fly_legacy_session::service::compatibility_string(&config, &options)?
        );
        return Ok(());
    }
    if let Some(dir) = args.print_state_compatibility.as_deref() {
        if let Some(found) = flysim::store::state_compatibility(dir) {
            println!("{found}");
        }
        return Ok(());
    }
    if let Some(rank) = args.reset_to_milestone {
        let stamp = flysim::reset::utc_stamp(flysim::eventlog::now_wall_ms());
        let durable = config.paths.save_dir.clone();
        let hot = config.paths.hot_dir.clone();
        let archive = flysim::reset::default_archive_dir(&durable, &stamp);
        for line in flysim::reset::reset_to_milestone(&durable, &hot, rank, &archive)? {
            println!("{line}");
        }
        return Ok(());
    }

    fly_legacy_session::service::run(config, options)
}

/// Logs go to stderr, as `flysim`'s do (binjgb prints the cartridge header to stdout).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("FLYSIM_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
