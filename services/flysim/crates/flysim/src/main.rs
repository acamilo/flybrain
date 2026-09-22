//! `flysim`: the fly brain and its Game Boy, as a service.
//!
//! ```sh
//! cargo run --release -p flysim -- --config flysim.toml
//! ```
//!
//! The config file is optional: `infra/units/flysim.service` runs the binary with no arguments
//! and configures it entirely through the environment (see `config.rs`).

use anyhow::Result;
use clap::Parser;
use flysim::config::Config;

#[derive(Debug, Parser)]
#[command(
    name = "flysim",
    about = "The fly brain and its Game Boy: snapshot feed, control API, checkpoints.",
    version
)]
struct Args {
    /// Path to `flysim.toml`. Without it, defaults plus `FLY_*` / `FLYSIM_*` environment
    /// overrides are used.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    /// Print the resolved configuration and exit.
    #[arg(long)]
    check_config: bool,

    /// Print the checkpoint compatibility string this build would write, and exit.
    ///
    /// `infra/05-deploy.sh` compares it with the durable state's `manifest.json` before it
    /// flips the `current` symlink: a build whose string differs refuses every existing
    /// checkpoint and then refuses to start at all, which is a black stream if the first time
    /// anyone finds out is at deploy.
    #[arg(long)]
    print_compatibility: bool,

    /// Print the compatibility string the checkpoints in this directory carry, and exit.
    ///
    /// Prints nothing and exits 0 when the directory holds no decodable checkpoint, which is
    /// the "nothing to compare" case for a fresh container.
    #[arg(long, value_name = "DIR")]
    print_state_compatibility: Option<std::path::PathBuf>,

    /// Restart the run from the milestone archive for this ladder rung, and exit.
    ///
    /// Run with flysim stopped: it rewrites both checkpoint stores.
    /// `infra/bin/fly-reset-to-milestone` is the operator-facing wrapper and the sequence
    /// around it is in `infra/docs/runbook.md`. The current state is copied to a dated
    /// directory first, so this is reversible by hand.
    #[arg(long, value_name = "RANK")]
    reset_to_milestone: Option<u32>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing();

    let config = Config::load(args.config.as_deref())?;
    // No secrets live in this service or its config, so it is logged in full.
    tracing::info!(config = ?config, "resolved configuration");
    if args.check_config {
        println!("{}", toml::to_string_pretty(&config)?);
        return Ok(());
    }
    if args.print_compatibility {
        println!("{}", flysim::simloop::compatibility_string(&config)?);
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

    flysim::run(config)
}

/// Logs go to **stderr**, deliberately.
///
/// binjgb's `init_emulator` prints the cartridge header to stdout on every boot and silencing it
/// would mean patching a vendored source (`crates/flybrain-gb/README.md`, "Known quirk"), so
/// stdout is left to it and stderr carries everything flysim says. Under systemd both land in
/// the journal; in a terminal `2>/dev/null` and `1>/dev/null` separate them cleanly.
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
