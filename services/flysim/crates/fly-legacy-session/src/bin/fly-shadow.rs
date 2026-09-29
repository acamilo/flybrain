//! `fly-shadow`: SHADOW-01, the session runtime run beside the live fly.
//!
//! ```text
//! fly-shadow run   [options]    follow the live trace, compare, write the verdict (the unit)
//! fly-shadow check [options]    exit 0 only if the verdict allows the cutover (CUT-01's hook)
//! ```
//!
//! `run` reads the live service's own configuration the way flysim does (`/etc/fly/fly.env` through
//! the unit's `EnvironmentFile`, `FLY_STATE`, `FLY_STATE_HOT`, ...): the cartridge, the dataset, the
//! stores (read only), the macro mode, the speed and the metrics listener. Options:
//!
//! | Option | Default |
//! | --- | --- |
//! | `--trace-dir DIR` | `$FLY_TRACE_DIR` |
//! | `--out DIR` | `$FLY_SHADOW_DIR`: `verdict.json`, `divergence.json` |
//! | `--spool DIR` | `<out>/spool` |
//! | `--work DIR` | `$FLY_SHADOW_WORK`, else `<out>/work`: sockets and artifact stores |
//! | `--mode in-process\|thread\|process` | `in-process` |
//! | `--threads N` | `$FLY_SHADOW_THREADS`, else 2: the shadow agent's sweep threads |
//! | `--required-brain-seconds S` | 10800 |
//! | `--max-live-lag-seconds S` | 0.5; the shadow pauses 60 s whenever the live `fly_lag_seconds` grew by more than S within 30 s (0 disables) |
//! | `--metrics HOST:PORT` | the live metrics listener |
//! | `--spool-max-mib N` | 512 |
//! | `--context N` | 30 transition pairs before a divergence |
//! | `--all-files` | follow every trace file present, oldest first (a rehearsal) |
//! | `--keep-traces` | keep compared trace files |
//! | `--keep-spool` | keep every spooled live checkpoint (a rehearsal replays them) |
//! | `--exit-on-pass` | exit once the verdict is `pass` |
//! | `--max-transitions N` | stop after N compared transitions |
//!
//! Exit status: 0 passed or stopped, 3 diverged, 2 the shadow's own error.
//!
//! `check --verdict FILE [--binary FILE] [--compatibility STRING] [--max-age-seconds N]`: the
//! cutover rule of `fly_legacy_session::shadow::verdict`. `--binary` is the session-runtime binary
//! CUT-01 switches to (default: `fly-session` beside this executable); it must be one of the
//! shadowed release's binaries by name and SHA-256. `--compatibility` defaults to this build's
//! legacy compatibility string.

use std::path::PathBuf;
use std::time::Duration;

use fly_legacy_session::shadow::{self, LagGuard, ShadowConfig, StopFlag, verdict};
use fly_session::ExecutionMode;

fn usage() -> ! {
    eprintln!("usage: fly-shadow run [options] | fly-shadow check --verdict FILE [options]");
    std::process::exit(2);
}

fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("fly-shadow: {message}");
    std::process::exit(2);
}

struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }

    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.0.iter().position(|a| a == name)?;
        if i + 1 >= self.0.len() {
            die(format!("{name} needs a value"));
        }
        let value = self.0.remove(i + 1);
        self.0.remove(i);
        Some(value)
    }

    fn parsed<T: std::str::FromStr>(&mut self, name: &str) -> Option<T> {
        self.value(name).map(|v| {
            v.parse()
                .unwrap_or_else(|_| die(format!("{name}: cannot parse {v:?}")))
        })
    }

    fn done(self) {
        if !self.0.is_empty() {
            die(format!("unknown arguments: {:?}", self.0));
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn file_sha256(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    shadow::sha256_hex(&bytes)
}

fn this_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|e| die(format!("current_exe: {e}")))
}

/// SHA-256 of the release binaries beside this one, by name.
fn release_binaries() -> std::collections::BTreeMap<String, String> {
    let dir = this_binary()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default();
    ["fly-shadow", "fly-session", "flysim", "fly-edge"]
        .iter()
        .filter_map(|name| {
            let path = dir.join(name);
            path.is_file()
                .then(|| ((*name).to_owned(), file_sha256(&path)))
        })
        .collect()
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let command = argv.remove(0);
    let mut args = Args(argv);
    match command.as_str() {
        "run" => run(args),
        "check" => {
            let verdict_path: PathBuf = args
                .value("--verdict")
                .map(PathBuf::from)
                .unwrap_or_else(|| usage());
            let binary = args.value("--binary").map_or_else(
                || this_binary().with_file_name("fly-session"),
                PathBuf::from,
            );
            let compatibility = args.value("--compatibility");
            let max_age: i64 = args.parsed("--max-age-seconds").unwrap_or(300);
            args.done();
            let compatibility = compatibility.unwrap_or_else(|| {
                let config = flysim::config::Config::load(None).unwrap_or_else(|e| die(e));
                flysim::simloop::compatibility_string(&config).unwrap_or_else(|e| die(e))
            });
            let text = std::fs::read_to_string(&verdict_path)
                .unwrap_or_else(|e| die(format!("{}: {e}", verdict_path.display())));
            let value: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|e| die(format!("verdict: {e}")));
            let now = verdict::parse_iso(&verdict::now_iso()).expect("now parses");
            let name = binary
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sha = file_sha256(&binary);
            match verdict::allows_cutover(&value, (&name, &sha), &compatibility, now, max_age) {
                Ok(()) => println!("cutover allowed: {}", value["reason"]),
                Err(reason) => {
                    println!("cutover refused: {reason}");
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}

fn run(mut args: Args) {
    let config = flysim::config::Config::load(None).unwrap_or_else(|e| die(format!("{e:#}")));
    let out_dir = args
        .value("--out")
        .map(PathBuf::from)
        .or_else(|| env_path("FLY_SHADOW_DIR"))
        .unwrap_or_else(|| die("--out or FLY_SHADOW_DIR is required"));
    let trace_dir = args
        .value("--trace-dir")
        .map(PathBuf::from)
        .or_else(|| env_path(flysim::trace::DIR_ENV))
        .unwrap_or_else(|| die("--trace-dir or FLY_TRACE_DIR is required"));
    let spool_dir = args
        .value("--spool")
        .map(PathBuf::from)
        .unwrap_or_else(|| out_dir.join("spool"));
    let work_dir = args
        .value("--work")
        .map(PathBuf::from)
        .or_else(|| env_path("FLY_SHADOW_WORK"))
        .unwrap_or_else(|| out_dir.join("work"));
    let mode = match args.value("--mode").as_deref() {
        None | Some("in-process") => ExecutionMode::InProcess,
        Some("thread") => ExecutionMode::Thread,
        Some("process") => ExecutionMode::Process,
        Some(other) => die(format!("--mode {other:?}")),
    };
    let agent_threads: usize = args.parsed("--threads").unwrap_or_else(|| {
        std::env::var("FLY_SHADOW_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    });
    let required: f64 = args
        .parsed("--required-brain-seconds")
        .unwrap_or(shadow::REQUIRED_BRAIN_SECONDS);
    let max_lag: f64 = args.parsed("--max-live-lag-seconds").unwrap_or(0.5);
    let metrics = args.value("--metrics").or_else(|| {
        config.control.metrics_bind.map(|addr| {
            if addr.ip().is_unspecified() {
                format!("127.0.0.1:{}", addr.port())
            } else {
                addr.to_string()
            }
        })
    });
    let spool_mib: u64 = args.parsed("--spool-max-mib").unwrap_or(512);
    let context_lines: usize = args.parsed("--context").unwrap_or(30);
    let all_files = args.flag("--all-files");
    let keep_traces = args.flag("--keep-traces");
    let keep_spool = args.flag("--keep-spool");
    let exit_on_pass = args.flag("--exit-on-pass");
    let max_transitions: Option<u64> = args.parsed("--max-transitions");
    // Undocumented: the toy connectome, for tests only.
    let profile = match args.value("--profile") {
        None => fly_session::legacy_agent::LegacyProfileKind::Production,
        Some(p) => {
            fly_session::legacy_agent::LegacyProfileKind::parse(&p).unwrap_or_else(|e| die(e))
        }
    };
    args.done();

    if config.loop_.game != "pokemon-red" {
        die(format!(
            "the legacy composition is Pokémon Red, not {:?}",
            config.loop_.game
        ));
    }
    if u64::from(config.feed.audio_hz) != fly_session::legacy_env::DEFAULT_AUDIO_RATE {
        die(format!(
            "the live service's audio rate {} is not the legacy environment's {}",
            config.feed.audio_hz,
            fly_session::legacy_env::DEFAULT_AUDIO_RATE
        ));
    }
    let lag_guard = (max_lag > 0.0)
        .then_some(())
        .and(metrics)
        .map(|metrics_addr| LagGuard {
            metrics_addr,
            growth_seconds: max_lag,
            window: Duration::from_secs(30),
            back_off: Duration::from_secs(60),
        });
    let shadow_config = ShadowConfig {
        trace_dir,
        hot_dir: config.paths.hot_dir.clone(),
        durable_dir: config.paths.save_dir.clone(),
        out_dir,
        spool_dir,
        work_dir,
        rom_path: config.paths.rom.clone(),
        dataset_dir: config.paths.dataset.clone(),
        profile,
        macro_mode: config.macros.mode,
        speed: config.loop_.speed,
        mode,
        agent_threads,
        required_brain_seconds: required,
        all_files,
        keep_traces,
        keep_spool,
        exit_on_pass,
        max_transitions,
        spool_max_bytes: spool_mib << 20,
        context_lines,
        poll: Duration::from_millis(100),
        lag_guard,
        binary_sha256: file_sha256(&this_binary()),
        binaries: release_binaries(),
        release: this_binary()
            .parent()
            .and_then(|d| d.canonicalize().ok())
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
    };
    eprintln!(
        "fly-shadow: {} mode, {} threads, following {} (stores {} and {}, read only)",
        mode.label(),
        agent_threads,
        shadow_config.trace_dir.display(),
        shadow_config.hot_dir.display(),
        shadow_config.durable_dir.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| die(format!("tokio: {e}")));
    let stop = StopFlag::default();
    let ended = runtime.block_on(async {
        let signals = stop.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
            let mut int = signal(SignalKind::interrupt()).expect("SIGINT");
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            eprintln!("fly-shadow: stopping");
            signals.request();
        });
        shadow::run(shadow_config, stop).await
    });
    match ended {
        Ok((shadow::Ended::Diverged, _)) => std::process::exit(3),
        Ok((ended, verdict)) => {
            eprintln!(
                "fly-shadow: {ended:?}; {} transitions ({:.1} brain s) identical, verdict {}",
                verdict.agreement.transitions,
                verdict.brain_seconds(),
                verdict.status.as_str()
            );
        }
        Err(e) => die(e),
    }
}
