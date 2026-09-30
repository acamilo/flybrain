//! SHADOW-02 end to end on one machine, on the committed toy connectome in raw mode (an explicit
//! ROM job: `FLY_ROM`).
//!
//! The *real* `flysim` service writes its trace into a "container" directory; the relay
//! (`fly_legacy_session::shadow::relay`) streams it and the live saves through the *real*
//! `fly-shadow ingest` binary (over pipes where production has ssh) into a "box" mirror; the
//! remote shadow (`shadow::run` with a run id file) follows the mirror; the relay writes the
//! verdict back and keeps flysim's consumer heartbeat fresh only while all of that is healthy.
//!
//! 1. A fresh fly and a restart, with sugar: the box's `pass` arrives on the container side with
//!    this run's id, and finished trace files are removed on the container too.
//! 2. The remote shadow dies: the relay stops the heartbeat and flysim stops its trace; a new
//!    shadow starts a new window, reports that stop (which predates it) as a `trace-cap` skip,
//!    and follows the next process.
//! 3. The sync stalls (the ingest is stopped): the heartbeat goes stale, flysim stops its trace
//!    (`no-consumer`); once the sync is back the box sees the gap and fails the verdict as a
//!    `coverage` divergence, the relay brings it and `divergence.json` back and exits.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use fly_legacy_session::shadow::{
    self, Ended, ShadowConfig, StopFlag,
    relay::{self, RelayConfig, RelayEnd},
    release,
};
use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_parity;
use flysim::snapshot::MacroMode;

/// The `flysim` binary built from this tree (see `tests/shadow.rs`).
fn flysim_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
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
        Ok(status) => assert!(status.success(), "building flysim failed"),
        Err(e) => {
            eprintln!("skipping: cargo is not reachable to build flysim ({e})");
            return None;
        }
    }
    Some(profile_dir.join("flysim"))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
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
    Some((
        status,
        text.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned(),
    ))
}

struct Live {
    child: Child,
    control: u16,
}

impl Live {
    fn start(binary: &Path, rom: &Path, container: &Path, log: &Path) -> Live {
        let control = free_port();
        let child = Command::new(binary)
            .env("FLY_ROM", rom)
            .env("FLY_DATASET", legacy_parity::toy::dir())
            .env("FLY_STATE", container.join("durable"))
            .env("FLY_STATE_HOT", container.join("hot"))
            .env("FLY_FEED_BIND", format!("127.0.0.1:{}", free_port()))
            .env("FLY_CONTROL_BIND", format!("127.0.0.1:{control}"))
            .env("FLY_METRICS_ADDR", format!("127.0.0.1:{}", free_port()))
            .env("FLY_MACRO_MODE", "raw")
            .env("FLY_CHAT_ENABLED", "false")
            .env("FLYSIM_LOOP_GAME", "pokemon-red")
            // Real time: a live fly left running while the test waits must not outrun the shadow.
            .env("FLYSIM_LOOP_SPEED", "1")
            .env("FLYSIM_LOOP_HOT_SECONDS", "1")
            .env("FLYSIM_LOOP_CHECKPOINT_SECONDS", "4")
            .env("RAYON_NUM_THREADS", "1")
            .env("FLY_TRACE_DIR", container.join("trace"))
            .env("FLY_TRACE_LEDGERS", "1")
            // A stale heartbeat stops the trace after 4 s here (10 minutes on the stream).
            .env("FLY_TRACE_CONSUMER_STALE_SECONDS", "4")
            .env_remove("FLY_TRACE")
            .env_remove("NOTIFY_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(log).unwrap())
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

    fn brain_ms(&self) -> Option<f64> {
        let (status, body) = http(self.control, "GET", "/status", None)?;
        let v: Value = serde_json::from_str(&body).ok()?;
        (status == 200 && v["status"] == "running").then(|| v["brainMs"].as_f64())?
    }

    fn sugar(&self, ms: u32) {
        let body = format!(r#"{{"durationMs":{ms},"by":"shadow-test","source":"operator"}}"#);
        let (status, text) = http(self.control, "POST", "/stimulate", Some(&body)).unwrap();
        assert!((200..300).contains(&status), "sugar refused: {status} {text}");
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

impl Drop for Live {
    /// A failed assertion must not leave a live service writing into the next run's directories.
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The remote shadow on the mirror, as `flyshadow-remote.service` runs it.
fn box_shadow(rom: &Path, root: &Path) -> ShadowConfig {
    ShadowConfig {
        trace_dir: root.join("trace"),
        hot_dir: root.join("hot"),
        durable_dir: root.join("durable"),
        out_dir: root.join("out"),
        spool_dir: root.join("out/spool"),
        work_dir: root.join("work"),
        rom_path: rom.to_owned(),
        dataset_dir: legacy_parity::toy::dir(),
        profile: LegacyProfileKind::Toy,
        macro_mode: MacroMode::Raw,
        // The live service's loop.speed (real time), which its saves record.
        speed: 1.0,
        mode: ExecutionMode::InProcess,
        agent_threads: 1,
        required_brain_seconds: 20.0,
        all_files: false,
        keep_traces: false,
        keep_spool: false,
        exit_on_pass: false,
        max_transitions: None,
        spool_max_bytes: 1 << 30,
        context_lines: 5,
        poll: Duration::from_millis(20),
        lag_guard: None,
        binary_sha256: "test".to_owned(),
        binaries: Default::default(),
        release: String::new(),
        run_id_file: Some(root.join("run-id")),
    }
}

fn spawn_shadow(
    config: ShadowConfig,
    stop: StopFlag,
) -> std::thread::JoinHandle<Result<(Ended, shadow::verdict::Verdict), String>> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(shadow::run(config, stop))
    })
}

fn json_of(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[test]
fn the_remote_shadow_follows_through_the_relay_and_fails_closed() {
    let Some(rom) = std::env::var_os("FLY_ROM").map(PathBuf::from) else {
        eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
        return;
    };
    let Some(flysim) = flysim_binary() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let kept = std::env::var_os("FLY_SHADOW_TEST_DIR").map(PathBuf::from);
    if let Some(dir) = &kept {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
    }
    let root = kept.unwrap_or_else(|| tmp.path().to_owned());
    let container = root.join("container");
    let mirror = root.join("box");
    for d in ["trace", "hot", "durable", "shadow"] {
        std::fs::create_dir_all(container.join(d)).unwrap();
    }
    let consumer = container.join("trace").join(flysim::trace::CONSUMER_FILE);
    let verdict_path = container.join("shadow/verdict.json");

    // The ingest is the real binary; production reaches it through ssh, this test through pipes.
    let fly_shadow = PathBuf::from(env!("CARGO_BIN_EXE_fly-shadow"));
    let pid_file = root.join("ingest.pid");
    let script = format!(
        "echo $$ > {}; exec {} ingest --root {}",
        pid_file.display(),
        fly_shadow.display(),
        mirror.display()
    );
    let release_dir = release::this_release_of(&fly_shadow);
    let run_id = "1790000000000".to_owned();
    let mut config = RelayConfig::with_defaults(
        container.join("trace"),
        container.join("hot"),
        container.join("durable"),
        container.join("shadow"),
        vec!["sh".to_owned(), "-c".to_owned(), script],
        run_id.clone(),
    );
    config.release = release_dir.display().to_string();
    config.binaries = release::binaries_in(&release_dir).unwrap();
    config.env = [("FLY_MACRO_MODE".to_owned(), "raw".to_owned())].into();
    config.tick = Duration::from_millis(50);
    config.beat_every = Duration::from_secs(1);
    config.alive_within = Duration::from_secs(5);
    config.stall = Duration::from_secs(5);
    config.reconnect_after = Duration::from_secs(1);
    // The run id is the start's Unix ms: everything of this test is newer.
    config.run_id = run_id.clone();
    let relay_stop = StopFlag::default();
    let relay = {
        let (config, stop) = (config.clone(), relay_stop.clone());
        std::thread::spawn(move || relay::run(config, stop))
    };
    wait_for("the ingest to write the run id", 120, || {
        shadow::read_run_id(&mirror.join("run-id")).as_deref() == Some(run_id.as_str())
    });
    assert_eq!(
        std::fs::read_to_string(mirror.join("live.env")).unwrap(),
        "FLY_MACRO_MODE=raw\n",
        "only the forwarded composition settings reach the box"
    );
    let shadow_stop = StopFlag::default();
    let shadow = spawn_shadow(box_shadow(&rom, &mirror), shadow_stop.clone());
    wait_for("the relay's heartbeat (box shadow alive)", 60, || {
        consumer.is_file()
    });

    // 1. A fresh fly with sugar, and a restart.
    let live = Live::start(&flysim, &rom, &container, &root.join("flysim-1.log"));
    live.sugar(300);
    live.run_for(8_000.0);
    live.sugar(600);
    live.run_for(8_000.0);
    live.stop();
    let first_file = shadow::follow::trace_files(&container.join("trace")).unwrap()[0].clone();
    let live = Live::start(&flysim, &rom, &container, &root.join("flysim-2.log"));
    live.sugar(450);
    live.run_for(12_000.0);
    live.stop();
    wait_for("the box's pass, over both processes and all sugar", 300, || {
        let v = json_of(&verdict_path);
        assert_ne!(v["status"], "diverged", "{v}");
        v["status"] == "pass"
            && v["compared"]["segments"].as_u64() >= Some(2)
            && v["compared"]["sugar"].as_u64() >= Some(3)
    });
    let v = json_of(&verdict_path);
    eprintln!("relayed verdict: {}", serde_json::to_string_pretty(&v).unwrap());
    assert_eq!(v["runId"], run_id.as_str());
    assert!(v["firstDivergence"].is_null());
    assert!(v["relay"]["unsyncedBytes"].is_u64(), "{v}");
    assert!(v["compared"]["segments"].as_u64().unwrap() >= 2);
    assert!(v["compared"]["sugar"].as_u64().unwrap() >= 3);
    assert!(v["compared"]["checkpoints"]["identical"].as_u64().unwrap() >= 3);
    wait_for("the finished first file removed on the container", 60, || {
        !first_file.exists()
    });
    let relay_json = json_of(&container.join("shadow/relay.json"));
    assert_eq!(relay_json["healthy"], true, "{relay_json}");

    // 2. The remote shadow dies: no more heartbeat on the container, so flysim stops its trace.
    let live = Live::start(&flysim, &rom, &container, &root.join("flysim-3.log"));
    live.run_for(3_000.0);
    shadow_stop.request();
    let (ended, _) = shadow.join().unwrap().expect("the shadow ran");
    assert_eq!(ended, Ended::Stopped);
    std::thread::sleep(Duration::from_secs(7));
    let beat = mtime(&consumer);
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(
        mtime(&consumer),
        beat,
        "the relay must not beat for a dead remote shadow"
    );
    let second_file = shadow::follow::trace_files(&container.join("trace"))
        .unwrap()
        .pop()
        .unwrap();
    let mirrored = mirror.join("trace").join(second_file.file_name().unwrap());
    wait_for("the trace stopped (no consumer) and mirrored", 120, || {
        std::fs::read_to_string(&mirrored).is_ok_and(|t| t.contains("\"no-consumer\""))
    });
    // A new shadow (its unit's restart) starts a new window on the same run.
    let started_before = json_of(&verdict_path)["startedAt"].clone();
    let shadow_stop = StopFlag::default();
    let shadow = spawn_shadow(box_shadow(&rom, &mirror), shadow_stop.clone());
    wait_for("the heartbeat again", 60, || mtime(&consumer) != beat);
    wait_for("the new window relayed", 60, || {
        json_of(&verdict_path)["startedAt"] != started_before
    });
    live.stop();
    // The stop happened before this shadow: history, an allowed skip, not a coverage gap.
    wait_for("the stopped file skipped as trace-cap", 120, || {
        json_of(&verdict_path)["skipped"]
            .as_array()
            .is_some_and(|s| s.iter().any(|k| k["kind"] == "trace-cap"))
    });
    let live = Live::start(&flysim, &rom, &container, &root.join("flysim-4.log"));
    live.run_for(3_000.0);
    wait_for("the new window comparing the next process", 120, || {
        json_of(&verdict_path)["compared"]["transitions"].as_u64() > Some(0)
    });
    assert!(json_of(&verdict_path)["firstDivergence"].is_null());

    // 3. The sync stalls: the ingest stops reading.
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    Command::new("kill").args(["-STOP", &pid]).status().unwrap();
    let live_file = shadow::follow::trace_files(&container.join("trace"))
        .unwrap()
        .pop()
        .unwrap();
    wait_for("flysim to stop its trace for want of a consumer", 180, || {
        std::fs::read_to_string(&live_file).is_ok_and(|t| t.contains("\"no-consumer\""))
    });
    // The sync comes back (a new connection): the box sees the gap.
    Command::new("kill").args(["-KILL", &pid]).status().unwrap();
    let end = relay.join().unwrap().expect("the relay ran");
    assert_eq!(end, RelayEnd::Diverged, "the relay exits on the remote divergence");
    let v = json_of(&verdict_path);
    assert_eq!(v["status"], "diverged", "{v}");
    assert_eq!(v["firstDivergence"]["kind"], "coverage", "{v}");
    assert!(
        container.join("shadow/divergence.json").is_file(),
        "divergence.json comes back"
    );
    assert!(!consumer.exists(), "a stopped relay leaves no heartbeat");
    let (ended, verdict) = shadow.join().unwrap().expect("the shadow ran");
    assert_eq!(ended, Ended::Diverged, "{}", verdict.reason);
    live.stop();
    drop(relay_stop);
}
