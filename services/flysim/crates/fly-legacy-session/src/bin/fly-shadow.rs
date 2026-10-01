//! `fly-shadow`: SHADOW-01, the session runtime run beside the live fly.
//!
//! ```text
//! fly-shadow run    [options]   follow the live trace, compare, write the verdict (the unit)
//! fly-shadow check  [options]   exit 0 only if the verdict allows the cutover (CUT-01's hook)
//! fly-shadow relay              SHADOW-02, on the release container: stream the trace and the
//!                               saves to a build box's ingest, write its verdict back here
//! fly-shadow ingest --root DIR  SHADOW-02, on the build box: the relay key's forced command
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
//! | `--lag-guard-margin S` | 0.05 s/s; the shadow pauses 60 s while the live `fly_lag_seconds` grows faster than `fly-shadow-run`'s baseline rate (`<out>/baseline.json`) plus this margin (the baseline's own margin wins), and stops pausing for 10 minutes when a pause did not help (0 disables) |
//! | `--metrics HOST:PORT` | the live metrics listener |
//! | `--spool-max-mib N` | 512 |
//! | `--context N` | 30 transition pairs before a divergence |
//! | `--all-files` | follow every trace file present, oldest first (a rehearsal) |
//! | `--keep-traces` | keep compared trace files |
//! | `--keep-spool` | keep every spooled live checkpoint (a rehearsal replays them) |
//! | `--exit-on-pass` | exit once the verdict is `pass` |
//! | `--max-transitions N` | stop after N compared transitions |
//! | `--run-id-file FILE` | a remote shadow (SHADOW-02): the run id `fly-shadow ingest` writes; it goes into the verdict, and a change stops the shadow with exit status 4 so its unit starts the new run |
//!
//! Exit status: 0 passed or stopped, 3 diverged, 4 a new run, 2 the shadow's own error.
//!
//! `relay` (SHADOW-02) reads the live service's configuration like `run` (`FLY_TRACE_DIR`, the
//! stores, `FLY_SHADOW_DIR`) and these: `FLY_SHADOW_RUN_ID` (required, `fly-shadow-run start`
//! sets it: the start's Unix ms); `FLY_SHADOW_REMOTE` (the box's ssh destination),
//! `FLY_SHADOW_REMOTE_KEY`, `FLY_SHADOW_REMOTE_KNOWN_HOSTS`, `FLY_SHADOW_REMOTE_PORT` (all from the
//! operator's private `/etc/fly/shadow-remote.env`); `FLY_SHADOW_REMOTE_COMMAND` replaces the
//! whole ssh command (tests); `FLY_SHADOW_RELAY_STALL_SECONDS` (60). Exit status: 0 stopped,
//! 3 the remote shadow diverged, 2 an error.
//!
//! `ingest --root DIR [--keep-mib N]` (SHADOW-02) serves one relay connection on stdin and stdout
//! and writes nothing outside DIR; N bounds the mirrored saves (3072).
//!
//! `check --verdict FILE [--current DIR] [--binary NAME] [--compatibility STRING]
//! [--max-age-seconds N]`: the cutover rule of `fly_legacy_session::shadow::verdict`. `--current`
//! is the release link CUT-01 switches into (default `/opt/fly/current`): the verdict must be for
//! the directory it resolves to, with every shadowed binary unchanged there. `--binary` is the
//! file name of the session-runtime binary CUT-01 switches to (default `flysim-session`,
//! SERVE-01's service). `--compatibility` defaults to this build's legacy compatibility string.

use std::path::PathBuf;
use std::time::Duration;

use fly_legacy_session::shadow::{
    self, LagGuard, ShadowConfig, StopFlag, ingest, relay, release, remote, verdict,
};
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
    release::file_sha256(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())))
}

fn this_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|e| die(format!("current_exe: {e}")))
}

/// SHA-256 of the release binaries in `dir`, by name.
fn binaries_in(dir: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    release::binaries_in(dir).unwrap_or_else(|e| die(format!("{}: {e}", dir.display())))
}

fn this_release() -> PathBuf {
    release::this_release()
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
        "relay" => {
            args.done();
            relay_main()
        }
        "ingest" => ingest_main(args),
        "check" => {
            let verdict_path: PathBuf = args
                .value("--verdict")
                .map(PathBuf::from)
                .unwrap_or_else(|| usage());
            // The release CUT-01 switches into (what `/opt/fly/current` resolves to now), and the
            // binary in it that it switches to: SERVE-01's service.
            let current = args
                .value("--current")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/opt/fly/current"));
            let binary = args
                .value("--binary")
                .unwrap_or_else(|| "flysim-session".to_owned());
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
            let dir = current
                .canonicalize()
                .unwrap_or_else(|e| die(format!("{}: {e}", current.display())));
            let binaries = binaries_in(&dir);
            let dir_text = dir.display().to_string();
            let release = verdict::Release {
                dir: &dir_text,
                binaries: &binaries,
            };
            match verdict::allows_cutover(&value, &binary, &release, &compatibility, now, max_age) {
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
    let lag_margin: f64 = args.parsed("--lag-guard-margin").unwrap_or(0.05);
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
    let run_id_file = args.value("--run-id-file").map(PathBuf::from);
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
    let baseline_file = out_dir.join("baseline.json");
    let lag_guard = (lag_margin > 0.0)
        .then_some(())
        .and(metrics)
        .map(|metrics_addr| LagGuard {
            metrics_addr,
            baseline_file: Some(baseline_file),
            margin: lag_margin,
            window: Duration::from_secs(60),
            back_off: Duration::from_secs(60),
            suppress: Duration::from_secs(600),
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
        binaries: binaries_in(&this_release()),
        release: this_release().display().to_string(),
        run_id_file,
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
        Ok((shadow::Ended::NewRun, _)) => {
            eprintln!("fly-shadow: a new run was started; exiting for the unit to start it");
            std::process::exit(4)
        }
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

/// Signals set the stop flag.
fn stop_on_signals(stop: &StopFlag) {
    let stop = stop.clone();
    std::thread::Builder::new()
        .name("fly-shadow-signals".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a signal runtime");
            runtime.block_on(async {
                use tokio::signal::unix::{SignalKind, signal};
                let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
                let mut int = signal(SignalKind::interrupt()).expect("SIGINT");
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            });
            stop.request();
        })
        .expect("the signal thread starts");
}

/// `fly-shadow relay`: the release container's half of a remote shadow (SHADOW-02).
fn relay_main() {
    let config = flysim::config::Config::load(None).unwrap_or_else(|e| die(format!("{e:#}")));
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let out_dir = env_path("FLY_SHADOW_DIR").unwrap_or_else(|| die("FLY_SHADOW_DIR is required"));
    let trace_dir = env_path(flysim::trace::DIR_ENV)
        .unwrap_or_else(|| die("FLY_TRACE_DIR is required"));
    let run_id = var("FLY_SHADOW_RUN_ID").unwrap_or_else(|| {
        die("FLY_SHADOW_RUN_ID is required (fly-shadow-run start sets it)")
    });
    let command: Vec<String> = match var("FLY_SHADOW_REMOTE_COMMAND") {
        Some(command) => command.split_whitespace().map(str::to_owned).collect(),
        None => {
            let target = var("FLY_SHADOW_REMOTE")
                .unwrap_or_else(|| die("FLY_SHADOW_REMOTE (the box's ssh destination) is required"));
            let key = env_path("FLY_SHADOW_REMOTE_KEY")
                .unwrap_or_else(|| die("FLY_SHADOW_REMOTE_KEY is required"));
            let known = env_path("FLY_SHADOW_REMOTE_KNOWN_HOSTS")
                .unwrap_or_else(|| die("FLY_SHADOW_REMOTE_KNOWN_HOSTS is required"));
            let port = var("FLY_SHADOW_REMOTE_PORT").map(|p| {
                p.parse::<u16>()
                    .unwrap_or_else(|_| die(format!("FLY_SHADOW_REMOTE_PORT {p:?}")))
            });
            relay::ssh_command(&target, &key, &known, port)
        }
    };
    let mut relay_config = relay::RelayConfig::with_defaults(
        trace_dir,
        config.paths.hot_dir.clone(),
        config.paths.save_dir.clone(),
        out_dir,
        command,
        run_id,
    );
    relay_config.release = this_release().display().to_string();
    relay_config.binaries = binaries_in(&this_release());
    relay_config.env = remote::FORWARDED_ENV
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), var(name)?)))
        .collect();
    if let Some(seconds) = var("FLY_SHADOW_RELAY_STALL_SECONDS") {
        relay_config.stall = Duration::from_secs(
            seconds
                .parse()
                .unwrap_or_else(|_| die(format!("FLY_SHADOW_RELAY_STALL_SECONDS {seconds:?}"))),
        );
    }
    eprintln!(
        "fly-shadow relay: run {}, release {}, following {} (stores {} and {}, read only)",
        relay_config.run_id,
        relay_config.release,
        relay_config.trace_dir.display(),
        relay_config.hot_dir.display(),
        relay_config.durable_dir.display()
    );
    let stop = StopFlag::default();
    stop_on_signals(&stop);
    match relay::run(relay_config, stop) {
        Ok(relay::RelayEnd::Stopped) => eprintln!("fly-shadow relay: stopped"),
        Ok(relay::RelayEnd::Diverged) => std::process::exit(3),
        Err(e) => die(e),
    }
}

/// `fly-shadow ingest --root DIR`: the build box's half (SHADOW-02), one connection on stdio.
fn ingest_main(mut args: Args) {
    let root = args
        .value("--root")
        .map(PathBuf::from)
        .unwrap_or_else(|| die("ingest needs --root DIR"));
    let keep_mib: u64 = args.parsed("--keep-mib").unwrap_or(3072);
    args.done();
    let mut config = ingest::IngestConfig::new(
        root,
        this_release().display().to_string(),
        binaries_in(&this_release()),
    );
    config.keep_checkpoint_bytes = keep_mib << 20;
    let input = std::io::BufReader::with_capacity(1 << 20, std::io::stdin());
    if let Err(e) = ingest::serve(input, Box::new(std::io::stdout()), config) {
        die(e);
    }
}
