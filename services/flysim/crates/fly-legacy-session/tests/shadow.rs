//! SHADOW-01 end to end, on the committed toy connectome in raw mode (an explicit ROM job:
//! `FLY_ROM`, and the workspace's `flysim` binary beside this test's).
//!
//! The *real* `flysim` service runs with the shadow's trace switches (`FLY_TRACE_DIR`,
//! `FLY_TRACE_LEDGERS=1`, a hot save every second), takes sugar through its control API and is
//! restarted once; the shadow (`fly_legacy_session::shadow::run`) follows it from its first frame.
//! Every transition, ledger digest and live save must agree, across both processes.
//!
//! Then the negative controls, on copies of the same trace and spool: a work-RAM digest flipped,
//! a sugar removed, a ledger digest flipped and a live save's bytes changed must each stop a
//! fresh shadow with a `diverged` verdict naming the right kind, field and step.
//!
//! The FAFB rehearsal of the same pipeline, at stream scale, is `tools/shadow-rehearsal.sh`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use fly_legacy_session::shadow::{self, Ended, ShadowConfig, StopFlag, verdict::Status};
use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity;
use flysim::snapshot::MacroMode;

/// The `flysim` binary built from *this* tree, in this test's own target directory and profile.
///
/// `cargo test -p fly-legacy-session` does not build `flysim`'s binary, so a binary found beside the
/// test can be stale -- built from another tree that shared the target directory -- and a stale
/// flysim that predates `FLY_TRACE_DIR` writes no trace at all (the "one trace file per process"
/// failure, 0 files, seen on a build box whose target directory other trees shared). So the test
/// builds it: `cargo build -p flysim --bin flysim` with the same target directory, profile and
/// flags, a no-op when it is current. `None` (skip) only when cargo is not reachable.
fn flysim_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?; // <target>/<profile>/deps/<test>
    let target = profile_dir.parent()?;
    let release = profile_dir.file_name()? == "release";
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .args(["build", "-p", "flysim", "--bin", "flysim"])
        .env("CARGO_TARGET_DIR", target)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if release {
        build.arg("--release");
    }
    match build.status() {
        Ok(status) => assert!(
            status.success(),
            "building the flysim binary under test failed"
        ),
        Err(e) => {
            eprintln!("skipping: cargo is not reachable to build flysim ({e})");
            return None;
        }
    }
    let path = profile_dir.join("flysim");
    assert!(
        path.is_file(),
        "no flysim binary at {} after the build",
        path.display()
    );
    Some(path)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("an ephemeral port")
        .local_addr()
        .expect("its address")
        .port()
}

fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> Option<(u16, String)> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let body = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    Some((status, body))
}

struct Live {
    child: Child,
    control: u16,
}

struct LiveDirs {
    hot: PathBuf,
    durable: PathBuf,
    trace: PathBuf,
}

fn start_live(binary: &Path, rom: &Path, dirs: &LiveDirs, log: &Path) -> Live {
    start_live_traced(binary, rom, dirs, log, true)
}

/// `traced = false` starts the service with no `FLY_TRACE_DIR`: a live process that leaves no
/// trace, as one whose trace cannot be created would.
fn start_live_traced(binary: &Path, rom: &Path, dirs: &LiveDirs, log: &Path, traced: bool) -> Live {
    let trace_dir = if traced {
        dirs.trace.clone()
    } else {
        PathBuf::new()
    };
    let control = free_port();
    let child = Command::new(binary)
        .env("FLY_ROM", rom)
        .env("FLY_DATASET", legacy_parity::toy::dir())
        .env("FLY_STATE", &dirs.durable)
        .env("FLY_STATE_HOT", &dirs.hot)
        .env("FLY_FEED_BIND", format!("127.0.0.1:{}", free_port()))
        .env("FLY_CONTROL_BIND", format!("127.0.0.1:{control}"))
        .env("FLY_METRICS_ADDR", format!("127.0.0.1:{}", free_port()))
        .env("FLY_MACRO_MODE", "raw")
        .env("FLY_CHAT_ENABLED", "false")
        .env("FLYSIM_LOOP_GAME", "pokemon-red")
        .env("FLYSIM_LOOP_SPEED", "0")
        .env("FLYSIM_LOOP_HOT_SECONDS", "1")
        .env("FLYSIM_LOOP_CHECKPOINT_SECONDS", "4")
        .env("RAYON_NUM_THREADS", "1")
        .env("FLY_TRACE_DIR", &trace_dir)
        .env("FLY_TRACE_LEDGERS", "1")
        .env_remove("FLY_TRACE")
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).expect("a log"))
        .spawn()
        .expect("flysim starts");
    let live = Live { child, control };
    let deadline = Instant::now() + Duration::from_secs(240);
    while live.brain_ms().is_none() {
        assert!(Instant::now() < deadline, "flysim never ran");
        std::thread::sleep(Duration::from_millis(200));
    }
    live
}

impl Live {
    fn brain_ms(&self) -> Option<f64> {
        let (status, body) = http(self.control, "GET", "/status", None)?;
        let v: Value = serde_json::from_str(&body).ok()?;
        (status == 200 && v["status"] == "running").then(|| v["brainMs"].as_f64())?
    }

    fn sugar(&self, ms: u32) {
        let body = format!(r#"{{"durationMs":{ms},"by":"shadow-test","source":"operator"}}"#);
        let (status, text) =
            http(self.control, "POST", "/stimulate", Some(&body)).expect("the control API");
        assert!(
            (200..300).contains(&status),
            "sugar refused: {status} {text}"
        );
    }

    fn run_for(&self, brain_ms: f64) {
        let start = self.brain_ms().expect("running");
        while self.brain_ms().unwrap_or(start) < start + brain_ms {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop(mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
    }
}

fn config(
    rom: &Path,
    root: &Path,
    trace: &Path,
    sources: (&Path, &Path),
    spool: &Path,
) -> ShadowConfig {
    ShadowConfig {
        trace_dir: trace.to_owned(),
        hot_dir: sources.0.to_owned(),
        durable_dir: sources.1.to_owned(),
        out_dir: root.join("out"),
        spool_dir: spool.to_owned(),
        work_dir: root.join("work"),
        rom_path: rom.to_owned(),
        dataset_dir: legacy_parity::toy::dir(),
        profile: LegacyProfileKind::Toy,
        macro_mode: MacroMode::Raw,
        speed: 0.0,
        mode: ExecutionMode::InProcess,
        agent_threads: 1,
        required_brain_seconds: 30.0,
        all_files: true,
        keep_traces: true,
        keep_spool: true,
        exit_on_pass: false,
        max_transitions: None,
        spool_max_bytes: 1 << 30,
        context_lines: 5,
        poll: Duration::from_millis(20),
        lag_guard: None,
        binary_sha256: "test".to_owned(),
        binaries: Default::default(),
        release: String::new(),
        run_id_file: None,
    }
}

fn transitions(trace: &Path) -> Vec<(PathBuf, usize)> {
    shadow::follow::trace_files(trace)
        .unwrap()
        .into_iter()
        .map(|path| {
            let n = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| l.contains("\"behaviour\""))
                .count();
            (path, n)
        })
        .collect()
}

fn verdict_of(out: &Path) -> Value {
    std::fs::read_to_string(out.join("verdict.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// Copies `trace` into a new directory, applying `edit` to the first file's lines.
fn tampered(trace: &Path, to: &Path, edit: impl Fn(usize, &mut Value) -> bool) -> u64 {
    std::fs::create_dir_all(to).unwrap();
    let mut step = None;
    for (i, (path, _)) in transitions(trace).into_iter().enumerate() {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut out = String::new();
        let mut n = 0;
        for line in text.lines() {
            let mut v: Value = serde_json::from_str(line).unwrap();
            if i == 0 && v.get("behaviour").is_some() {
                if step.is_none() && edit(n, &mut v) {
                    step = v["behaviour"]["step"].as_str().and_then(|s| s.parse().ok());
                }
                n += 1;
            }
            out.push_str(&v.to_string());
            out.push('\n');
        }
        std::fs::write(to.join(path.file_name().unwrap()), out).unwrap();
    }
    step.expect("the edit applied")
}

/// The shadow on a thread of its own with its own runtime (the session's future is not `Send`).
fn spawn_shadow(
    config: ShadowConfig,
    stop: StopFlag,
) -> std::thread::JoinHandle<Result<(Ended, shadow::verdict::Verdict), String>> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(shadow::run(config, stop))
    })
}

fn diverges(
    rom: &Path,
    root: &Path,
    trace: &Path,
    spool: &Path,
    empty: &Path,
) -> shadow::verdict::Verdict {
    diverges_with(rom, root, trace, spool, empty, |_| {})
}

fn diverges_with(
    rom: &Path,
    root: &Path,
    trace: &Path,
    spool: &Path,
    empty: &Path,
    edit: impl FnOnce(&mut ShadowConfig),
) -> shadow::verdict::Verdict {
    let _ = std::fs::remove_dir_all(root.join("out"));
    let mut config = config(rom, root, trace, (empty, empty), spool);
    edit(&mut config);
    let stop = StopFlag::default();
    let handle = spawn_shadow(config, stop.clone());
    let deadline = Instant::now() + Duration::from_secs(600);
    while !handle.is_finished() {
        if Instant::now() > deadline {
            stop.request();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let (ended, verdict) = handle.join().unwrap().expect("the shadow ran");
    assert_eq!(ended, Ended::Diverged, "{:?}", verdict.reason);
    verdict
}

#[test]
fn the_shadow_follows_the_real_service_and_catches_every_planted_difference() {
    let Some(rom) = std::env::var_os("FLY_ROM").map(PathBuf::from) else {
        eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
        return;
    };
    let Some(binary) = flysim_binary() else {
        eprintln!("skipping: no flysim binary beside this test (cargo test --workspace builds it)");
        return;
    };
    // `FLY_SHADOW_TEST_DIR` keeps everything for a look afterwards.
    let tmp = tempfile::tempdir().unwrap();
    let kept = std::env::var_os("FLY_SHADOW_TEST_DIR").map(PathBuf::from);
    if let Some(dir) = &kept {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
    }
    let root_path = kept.unwrap_or_else(|| tmp.path().to_owned());
    let root = &root_path;
    let dirs = LiveDirs {
        hot: root.join("live/hot"),
        durable: root.join("live/durable"),
        trace: root.join("live/trace"),
    };
    let spool = root.join("shadow/spool");
    let shadow_root = root.join("shadow");
    let config = config(
        &rom,
        &shadow_root,
        &dirs.trace,
        (&dirs.hot, &dirs.durable),
        &spool,
    );
    let out = config.out_dir.clone();
    let stop = StopFlag::default();
    let handle = spawn_shadow(config, stop.clone());
    // The live recorder starts no trace without the shadow's heartbeat.
    let heartbeat = dirs.trace.join(flysim::trace::CONSUMER_FILE);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !heartbeat.is_file() {
        assert!(Instant::now() < deadline, "the shadow wrote no heartbeat");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Process one: a fresh fly (the stores are empty), two sugars, 25 brain seconds.
    let live = start_live(&binary, &rom, &dirs, &root.join("flysim-1.log"));
    live.sugar(300);
    live.run_for(8_000.0);
    live.sugar(700);
    live.run_for(17_000.0);
    live.stop();
    // Process two: restores its own latest save, a sugar, 15 brain seconds.
    let live = start_live(&binary, &rom, &dirs, &root.join("flysim-2.log"));
    live.run_for(5_000.0);
    live.sugar(450);
    live.run_for(10_000.0);
    live.stop();

    let files = transitions(&dirs.trace);
    assert_eq!(
        files.len(),
        2,
        "one trace file per process; the shadow's verdict: {}; divergence: {}",
        verdict_of(&out),
        std::fs::read_to_string(out.join("divergence.json")).unwrap_or_default()
    );
    let total: usize = files.iter().map(|(_, n)| n).sum();
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let v = verdict_of(&out);
        if v["compared"]["transitions"].as_u64() == Some(total as u64) || handle.is_finished() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shadow did not catch up: {v}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    // The compared count and the status are recomputed on different verdict writes: a loaded box
    // read the new count with the status still `running` (the v0.7.1 release gate). Wait, bounded,
    // for the status to settle before asserting it.
    let settle_by = Instant::now() + Duration::from_secs(120);
    let passed = loop {
        let v = verdict_of(&out);
        if v["status"] != "running" || handle.is_finished() || Instant::now() >= settle_by {
            break v;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    eprintln!(
        "shadow after both processes: {}",
        serde_json::to_string_pretty(&passed).unwrap()
    );
    assert_eq!(
        passed["status"], "pass",
        "40 brain seconds against a 30 s window"
    );
    assert!(passed["firstDivergence"].is_null());

    // Process three runs untraced while the shadow follows: it can never be compared, so the
    // shadow must fail its verdict (coverage), not keep a pass built on the traced processes.
    let live = start_live_traced(&binary, &rom, &dirs, &root.join("flysim-3.log"), false);
    live.run_for(3_000.0);
    live.stop();
    let deadline = Instant::now() + Duration::from_secs(120);
    while !handle.is_finished() {
        if Instant::now() > deadline {
            stop.request();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let (ended, verdict) = handle.join().unwrap().expect("the shadow ran");
    assert_eq!(
        ended,
        Ended::Diverged,
        "an untraced live process fails the verdict"
    );
    let coverage = verdict.divergence.as_ref().expect("a divergence");
    assert_eq!(coverage.kind, "coverage", "{}", coverage.detail);
    assert_eq!(verdict.status, Status::Diverged);
    assert_eq!(
        transitions(&dirs.trace).len(),
        2,
        "process three left no trace"
    );
    assert_eq!(verdict.agreement.transitions, total as u64);
    assert_eq!(verdict.segments_compared, 2);
    assert!(verdict.skipped.is_empty(), "{:?}", verdict.skipped);
    assert_eq!(verdict.agreement.sugar, 3);
    assert_eq!(
        verdict.ledger_checks, total as u64,
        "a ledger digest every boundary"
    );
    // At least the fresh fly's startup save and both shutdown saves; the hot saves, one a wall
    // second, add as many more as the box's speed gives.
    assert!(
        verdict.checkpoints.identical >= 3,
        "live saves compared byte for byte: {:?}",
        verdict.checkpoints
    );

    // The negative controls, each on a copy.
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let neg = root.join("neg");

    let at = tampered(&dirs.trace, &neg.join("wram/trace"), |n, v| {
        (n == 700)
            .then(|| v["behaviour"]["wramDigest"] = Value::String("0".repeat(64)))
            .is_some()
    });
    let got = diverges(
        &rom,
        &neg.join("wram"),
        &neg.join("wram/trace"),
        &spool,
        &empty,
    );
    let d = got.divergence.expect("a divergence");
    assert_eq!((d.kind.as_str(), d.step), ("transition", Some(at)));
    assert_eq!(d.difference.unwrap().field, "wramDigest");
    assert!(!d.context.is_empty());
    assert_eq!(got.status, Status::Diverged);

    let at = tampered(&dirs.trace, &neg.join("ledgers/trace"), |n, v| {
        (n == 900)
            .then(|| v["behaviour"]["ledgersDigest"] = Value::String("0".repeat(64)))
            .is_some()
    });
    let got = diverges(
        &rom,
        &neg.join("ledgers"),
        &neg.join("ledgers/trace"),
        &spool,
        &empty,
    );
    let d = got.divergence.expect("a divergence");
    assert_eq!((d.kind.as_str(), d.step), ("ledgers", Some(at)));

    // A sugar the live loop applied, missing from the shadow's inputs: the brain differs from
    // that transition on, and the comparison says where.
    let at = tampered(&dirs.trace, &neg.join("sugar/trace"), |_, v| {
        let had = v["behaviour"]["admissions"]
            .as_array()
            .is_some_and(|a| !a.is_empty());
        if had {
            v["behaviour"]["admissions"] = Value::Array(Vec::new());
            // The admissions field itself would differ first; hold it to the tampered value.
        }
        had
    });
    let got = diverges(
        &rom,
        &neg.join("sugar"),
        &neg.join("sugar/trace"),
        &spool,
        &empty,
    );
    let d = got.divergence.expect("a divergence");
    eprintln!(
        "sugar removed at step {at}: diverged at {:?} on {:?}",
        d.step, d.difference
    );
    assert_eq!(d.kind, "transition");
    assert!(d.step.is_some_and(|s| s >= at));

    // A live save whose bytes differ (its rankSinceMs moved by one ms).
    let copy = neg.join("save/spool");
    std::fs::create_dir_all(&copy).unwrap();
    for entry in std::fs::read_dir(&spool).unwrap().flatten() {
        std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
    }
    let (first, _) = transitions(&dirs.trace).remove(0);
    let text = std::fs::read_to_string(&first).unwrap();
    let (generation, at) = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("behaviour").is_some())
        .find_map(|v| {
            let capture = v["operational"]["captures"].as_array()?.first()?.clone();
            let generation: u64 = capture["checkpointId"]
                .as_str()?
                .strip_prefix('g')?
                .parse()
                .ok()?;
            let step: u64 = v["behaviour"]["step"].as_str()?.parse().ok()?;
            Some((generation, step))
        })
        .expect("a live save during the run");
    let path = copy.join(format!("g{generation}.checkpoint"));
    let mut checkpoint = flysim::store::load(&path).unwrap();
    checkpoint.runtime.rank_since_ms += 1.0;
    std::fs::write(
        &path,
        flysim::store::encode(&checkpoint.agent, &checkpoint.runtime).unwrap(),
    )
    .unwrap();
    let got = diverges(&rom, &neg.join("save"), &dirs.trace, &copy, &empty);
    let d = got.divergence.expect("a divergence");
    assert_eq!((d.kind.as_str(), d.step), ("checkpoint", Some(at)));
    assert!(d.detail.contains("rankSinceMs"), "{}", d.detail);

    // A session-side failure is a divergence, never a skip: a candidate whose restore gate
    // refuses the live startup save (another dataset profile, so another compatibility string).
    let got = diverges_with(&rom, &neg.join("gate"), &dirs.trace, &spool, &empty, |c| {
        c.profile = LegacyProfileKind::Production;
    });
    let d = got.divergence.expect("a divergence");
    assert_eq!(d.kind, "boot", "{}", d.detail);
    assert!(d.detail.contains("restore gate"), "{}", d.detail);
    assert!(got.skipped.is_empty());

    // r3 N5: a declared reward pulse ends the comparison of its trace, but a stop later in the
    // same trace (here the byte cap) must still end the window: what was compared before the
    // pulse, and the untraced time after the stop, must not count toward a pass.
    let full_brain_seconds = verdict.brain_seconds();
    let at = tampered(&dirs.trace, &neg.join("reward/trace"), |n, v| {
        (n == 100)
            .then(|| v["behaviour"]["admissions"] = serde_json::json!([{"kind": "reward"}]))
            .is_some()
    });
    let files = transitions(&neg.join("reward/trace"));
    let (first_name, second_n) = (
        files[0].0.file_name().unwrap().to_string_lossy().into_owned(),
        files[1].1,
    );
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&files[0].0).unwrap();
        writeln!(f, "{{\"truncated\":true,\"reason\":\"byte-cap\"}}").unwrap();
    }
    let _ = std::fs::remove_dir_all(neg.join("reward").join("out"));
    let reward_config = self::config(&rom, &neg.join("reward"), &neg.join("reward/trace"), (&empty, &empty), &spool);
    let out = reward_config.out_dir.clone();
    let stop = StopFlag::default();
    let handle = spawn_shadow(reward_config, stop.clone());
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let v = verdict_of(&out);
        let ended = !v["window"]["ends"].as_array().is_none_or(|e| e.is_empty());
        if (ended && v["compared"]["transitions"].as_u64() >= Some(101 + second_n as u64))
            || handle.is_finished()
            || Instant::now() > deadline
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.request();
    let (ended, verdict) = handle.join().unwrap().expect("the shadow ran");
    assert_ne!(ended, Ended::Diverged, "{:?}", verdict.reason);
    let j = verdict.to_json();
    eprintln!("reward then stop (reward at step {at}): {j}");
    assert_eq!(j["skipped"][0]["kind"], "operator-reward-pulse", "{j}");
    assert_eq!(j["window"]["lastStopTrace"], first_name.as_str(), "{j}");
    assert_eq!(j["window"]["ends"][0]["trace"], first_name.as_str(), "{j}");
    assert!(j["window"]["ends"][0]["discardedBrainSeconds"].as_f64().unwrap() > 0.0, "{j}");
    // Only the second process counts, after the window ended: what the first one compared before
    // the pulse is dropped and its uncompared remainder never was added, so less than the whole.
    let whole = full_brain_seconds;
    let after = j["compared"]["brainSeconds"].as_f64().unwrap();
    let dropped = j["window"]["ends"][0]["discardedBrainSeconds"].as_f64().unwrap();
    assert!(after + dropped < whole - 1.0, "{after} + {dropped} against {whole}: {j}");
    assert_ne!(j["status"], "pass", "{j}");
}
