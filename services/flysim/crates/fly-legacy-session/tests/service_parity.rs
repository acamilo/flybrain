//! SERVE-01: the session runtime's service host against the legacy service loop, at the level
//! of the two binding contracts (`docs/feed-protocol.md`, `docs/control-api.md`).
//!
//! Both runtimes are started from the same seeded FLYSIM01 store, behind flysim's own control
//! router (`flysim::api::router`) and snapshot slot, and driven by the same script of control
//! requests, each sent on the snapshot of a named frame so it lands before the next one (the loop
//! is paced slowly enough that a request always arrives while it sleeps). Every published feed
//! header and its three attachments, every control response, the event log, the sugar journal
//! and the final checkpoint are compared:
//!
//! - **feed**: every running snapshot, header field by field and attachments byte for byte,
//!   except the fields that are wall clock by definition (`wallMs`, `uptimeSeconds`,
//!   `realtimeFactor`, `sugar.cooldownMs`, an event's or chat line's `wallMs`) and `seq`, which
//!   counts the idle headers a pause publishes at 2 Hz of wall clock (the script holds the
//!   resume until the recorder has seen the first one, so each runtime must publish one);
//! - **control**: status code and body of every request (`/stimulate` 202 and 429 with the same
//!   `retryAfterMs`, `/reward` 403, `/chat` 202/422, `/checkpoint` the same generation,
//!   `/pause`, `/resume`, `/events`, `/healthz`), `/status` minus its wall-clock fields, and the
//!   `/metrics` series names;
//! - **store**: the event log file, the journal lines (minus `wallMs`, the boot header's runtime
//!   and wall clock), and the final durable checkpoint decoded (minus `wallMs`).
//!
//! The in-process session runs a second time with its control API on the bus (CTRL-01): the
//! same script through `fly-control-edge`'s router and the control services on an embedded
//! router, compared against the legacy run the same way.
//!
//! Gated on `FLY_ROM` (source `bin/rom-env.sh`). `toy_raw_*` uses the committed toy connectome and
//! a store seeded by a legacy fresh start; `fafb_*` (also `FLY_SERVE01_FAFB`) the real connectome
//! from the ENV-01 service trace's `rollback` checkpoint, which rolls back on its first boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request};
use fly_legacy_session::composition::SessionArm;
use fly_legacy_session::service::{ServiceOptions, run_host};
use fly_session::legacy_agent::LegacyProfileKind;
use flysim::AppState;
use flysim::config::Config;
use flysim::eventlog::EventRing;
use flysim::simloop::{Command, Shared, Sim, booting_snapshot};
use flysim::snapshot::{FeedStatus, MacroMode, Snapshot};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tower::ServiceExt;

fn rom_path() -> Option<PathBuf> {
    match std::env::var_os("FLY_ROM") {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
            None
        }
    }
}

/// `FLYSIM_LOG=info` shows both runtimes' logs.
fn init_tracing() {
    if let Ok(filter) = std::env::var("FLYSIM_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(std::io::stderr)
            .try_init();
    }
}

fn sha(bytes: &[u8]) -> String {
    fly_session::types::sha256_hex(bytes)
}

/// Which runtime is behind the router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    Legacy,
    Session(SessionArm),
    /// The session runtime with its control API on the bus (`FLY_CONTROL_VIA=bus`, CTRL-01): the
    /// script goes through `fly-control-edge`'s router, over the edge's Unix socket, to the
    /// control services on the embedded router.
    SessionOverBus(SessionArm),
}

impl Runtime {
    fn label(&self) -> String {
        match self {
            Runtime::Legacy => "legacy".to_owned(),
            Runtime::Session(arm) => format!("session ({})", arm.label()),
            Runtime::SessionOverBus(arm) => {
                format!("session ({}) over the control bus", arm.label())
            }
        }
    }
}

/// One control request of the script: at the snapshot of running frame `at` (counted from the
/// first running snapshot, 0-based), send `method path body`.
#[derive(Clone, Debug)]
struct Action {
    at: usize,
    method: Method,
    path: &'static str,
    body: Option<Value>,
}

fn act(at: usize, method: Method, path: &'static str, body: Option<Value>) -> Action {
    Action {
        at,
        method,
        path,
        body,
    }
}

/// The script both runtimes are driven by.
fn script() -> Vec<Action> {
    let sugar = || Some(json!({"by": "viewer_one", "source": "points", "durationMs": 300}));
    vec![
        act(2, Method::GET, "/healthz", None),
        act(3, Method::POST, "/stimulate", sugar()),
        // Inside the pulse: refused with the pulse's own retry.
        act(4, Method::POST, "/stimulate", sugar()),
        act(5, Method::POST, "/reward", Some(json!({"value": 1.0, "by": "op", "source": "operator"}))),
        act(6, Method::POST, "/chat", Some(json!({"by": "viewer_two", "text": "go fly go"}))),
        act(7, Method::POST, "/chat", Some(json!({"by": "not a name!", "text": "hello"}))),
        act(8, Method::POST, "/stimulate", Some(json!({"by": "x"}))),
        act(12, Method::POST, "/checkpoint", None),
        // Two sugars before one commit (the TASK-01 review's N1): the legacy drain refuses the
        // second with the first one's pulse as the retry, and so must the session's.
        act(24, Method::POST, "/stimulate", Some(json!({"by": "viewer_five", "source": "points", "durationMs": 600}))),
        act(24, Method::POST, "/stimulate", Some(json!({"by": "viewer_six", "source": "chat"}))),
        act(40, Method::POST, "/stimulate", Some(json!({"by": "viewer_three", "source": "chat"}))),
        act(41, Method::POST, "/stimulate", Some(json!({"by": "viewer_three", "source": "chat", "durationMs": 5000}))),
        act(60, Method::POST, "/pause", None),
        act(60, Method::POST, "/pause", None),
        act(60, Method::POST, "/resume", None),
        act(70, Method::GET, "/events?since=0&limit=500", None),
        act(80, Method::POST, "/chat", Some(json!({"by": "viewer_two", "text": "again", "bot": true}))),
        act(90, Method::GET, "/nope", None),
        act(95, Method::POST, "/stimulate", Some(json!({"by": "viewer_four", "source": "points", "durationMs": 50}))),
    ]
}

/// What one run produced.
struct Recording {
    /// False when the recorder missed a frame; such a run is repeated, never compared.
    complete: bool,
    /// Every running snapshot: the comparable header and the attachments' digests.
    running: Vec<(Value, [String; 3])>,
    /// The idle headers a pause published (compared for shape, not count).
    idle: Vec<Value>,
    responses: Vec<(String, u16, Value)>,
    status: Value,
    metric_names: Vec<String>,
    events: Vec<Value>,
    event_file: Vec<Value>,
    journal: Vec<Value>,
    final_checkpoint: Value,
}

/// The header without the fields that are wall clock by definition.
fn comparable(snapshot: &Snapshot) -> Value {
    let mut value = serde_json::to_value(&snapshot.header).expect("a header serializes");
    let map = value.as_object_mut().expect("an object");
    for key in ["wallMs", "uptimeSeconds", "realtimeFactor", "seq"] {
        map.remove(key);
    }
    if let Some(sugar) = map.get_mut("sugar").and_then(Value::as_object_mut) {
        sugar.remove("cooldownMs");
    }
    for list in ["events", "chat"] {
        if let Some(items) = map.get_mut(list).and_then(Value::as_array_mut) {
            for item in items {
                if let Some(item) = item.as_object_mut() {
                    item.remove("wallMs");
                }
            }
        }
    }
    value
}

fn without_wall(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("wallMs");
    }
    value
}

async fn call(router: axum::Router, action: &Action) -> (u16, Value) {
    let request = Request::builder()
        .method(action.method.clone())
        .uri(action.path);
    let request = match &action.body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 16 << 20)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

/// The service configuration of a run: flysim's defaults with the store under `dir`, every frame
/// published, no timed checkpoints (the script and the boundaries take them), chat on.
fn config(rom: &Path, dataset: &Path, dir: &Path, mode: MacroMode, speed: f64) -> Config {
    let mut config = Config::default();
    config.paths.rom = rom.to_path_buf();
    config.paths.dataset = dataset.to_path_buf();
    config.paths.save_dir = dir.join("durable");
    config.paths.hot_dir = dir.join("hot");
    config.loop_.game = "pokemon-red".to_owned();
    config.loop_.speed = speed;
    config.loop_.threads = 1;
    config.loop_.snapshot_hz = 1_000_000.0;
    config.loop_.hot_seconds = 1e9;
    config.loop_.checkpoint_seconds = 1e9;
    config.macros.mode = mode;
    config.chat.enabled = true;
    config
}

/// Start `runtime` over `config` behind the control router, drive the script for `frames`
/// running snapshots, shut down, and collect everything.
fn record(
    runtime: Runtime,
    config: Config,
    profile: LegacyProfileKind,
    frames: usize,
    actions: &[Action],
) -> Recording {
    let shared = Arc::new(Shared::new(config.clone(), EventRing::new()));
    let (commands, command_rx) = mpsc::channel::<Command>(flysim::simloop::COMMAND_QUEUE);
    let (snapshots_tx, snapshots) = watch::channel(Arc::new(booting_snapshot(
        0,
        0,
        config.macros.mode,
    )));
    let state = AppState {
        shared: Arc::clone(&shared),
        commands: commands.clone(),
        snapshots: snapshots.clone(),
    };
    let session_dir = config.paths.save_dir.parent().unwrap().join("session");
    let loop_shared = Arc::clone(&shared);
    let thread = std::thread::spawn(move || -> anyhow::Result<()> {
        let notifier = flysim::sdnotify::Notifier::from_env();
        match runtime {
            Runtime::Legacy => {
                let mut sim = Sim::boot(loop_shared, snapshots_tx, command_rx)?;
                sim.run(&notifier)
            }
            Runtime::Session(arm) | Runtime::SessionOverBus(arm) => {
                let options = ServiceOptions {
                    mode: arm.mode,
                    transport: arm.transport,
                    profile,
                    session_dir,
                    placements: Default::default(),
                };
                run_host(loop_shared, snapshots_tx, command_rx, &notifier, &options)
            }
        }
    });

    let driver = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let bus_dir = config.paths.save_dir.parent().unwrap().join("bus");
    let (router, _control_bus) = match runtime {
        Runtime::SessionOverBus(_) => driver.block_on(async {
            let scope = flysim::controlbus::Scope::live();
            let uses = flysim::bus::Uses { feed: false, control: true };
            let bus = flysim::bus::start(&bus_dir, uses, &scope).await.unwrap();
            let host = flysim::controlbus::serve(&bus.router, state.clone(), &scope)
                .await
                .unwrap();
            let mut edge_config = fly_control_edge::EdgeConfig::from_flysim(&config, None);
            edge_config.bus_dir = bus_dir.clone();
            let metrics = Arc::new(fly_control_edge::EdgeMetrics::default());
            let backend = fly_control_edge::connect(&edge_config, &metrics).await.unwrap();
            (flysim::api::router_with(backend), Some((bus, host)))
        }),
        _ => (flysim::api::router(state.clone()), None),
    };
    let metrics_router = flysim::api::metrics_router(state.clone());
    let recording = driver.block_on(async {
        let mut snapshots = snapshots;
        let mut running: Vec<(Value, [String; 3])> = Vec::new();
        let mut idle = Vec::new();
        let mut pending = Vec::new();
        let mut last_frame: Option<u64> = None;
        let mut complete = true;
        let deadline = Instant::now() + Duration::from_secs(1800);
        while running.len() < frames {
            assert!(Instant::now() < deadline, "{}: the run stalled", runtime.label());
            match tokio::time::timeout(Duration::from_secs(120), snapshots.changed()).await {
                Ok(Ok(())) => {}
                // The loop is gone: its own error is the failure (the thread is joined below).
                Ok(Err(_)) => break,
                Err(_) => panic!(
                    "{}: no snapshot for 120 s after {} running",
                    runtime.label(),
                    running.len()
                ),
            }
            let snapshot = Arc::clone(&snapshots.borrow_and_update());
            match snapshot.header.status {
                FeedStatus::Running | FeedStatus::Recovering => {
                    // Every frame publishes; a gap means the recorder fell behind the loop.
                    let frame = snapshot.header.frame;
                    if let Some(last) = last_frame
                        && frame != last + 1
                    {
                        // The recorder fell behind the loop (a loaded box): this run cannot be
                        // compared, and the caller runs it again.
                        eprintln!(
                            "  {}: the recorder missed frames {}..{frame}; the run will be repeated",
                            runtime.label(),
                            last + 1
                        );
                        complete = false;
                    }
                    last_frame = Some(frame);
                    let index = running.len();
                    if index.is_multiple_of(30) {
                        eprintln!("  {}: running snapshot {index} (frame {frame})", runtime.label());
                    }
                    running.push((
                        comparable(&snapshot),
                        [
                            sha(&snapshot.frame),
                            sha(&snapshot.audio),
                            sha(&snapshot.spikes),
                        ],
                    ));
                    // Sent now, in order, each drained before the next frame.
                    for action in actions.iter().filter(|a| a.at == index) {
                        if action.path == "/resume" {
                            // The pause is only observable as idle headers if the loop stays
                            // paused until it has published one (a header-only publish is due
                            // within the 2 Hz idle period). Resuming 2 ms after the pause let a
                            // loaded box skip it in one runtime and not the other, so wait, bounded,
                            // for the condition instead of racing it.
                            let before = idle.len();
                            let waited = Instant::now();
                            while idle.len() == before {
                                match tokio::time::timeout(Duration::from_secs(30), snapshots.changed()).await {
                                    Ok(Ok(())) => {}
                                    other => panic!(
                                        "{}: no idle header within 30 s of the pause ({other:?})",
                                        runtime.label()
                                    ),
                                }
                                let snap = Arc::clone(&snapshots.borrow_and_update());
                                if snap.header.status == FeedStatus::Paused {
                                    idle.push(comparable(&snap));
                                }
                            }
                            eprintln!("  {}: idle header after {:.1?} paused", runtime.label(), waited.elapsed());
                        }
                        let router = router.clone();
                        let label = format!("{} {}", action_label(action), index);
                        let action = action.clone();
                        let handle = tokio::spawn(async move { call(router, &action).await });
                        // One at a time, so the loop drains them in script order.
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        pending.push((label, handle));
                    }
                    // Requests sent at one frame must all be drained together (two sugars before
                    // one commit): if the loop published another snapshot while they were being
                    // sent, a loaded box split them across frames and the run cannot be compared.
                    let group = actions.iter().filter(|a| a.at == index).count();
                    let pauses = actions.iter().any(|a| a.at == index && a.path.starts_with("/pause"));
                    if group > 1 && !pauses && snapshots.has_changed().unwrap_or(false) {
                        eprintln!(
                            "  {}: the loop moved on while {group} requests were sent at frame {index}; the run will be repeated",
                            runtime.label()
                        );
                        complete = false;
                    }
                }
                FeedStatus::Paused => idle.push(comparable(&snapshot)),
                FeedStatus::Booting | FeedStatus::Error => {}
            }
        }
        // Stop at once, so the final save is of the frame after the last one recorded.
        let (reply, ack) = tokio::sync::oneshot::channel();
        if commands.send(Command::Shutdown { reply }).await.is_ok() {
            let _ = ack.await;
        }
        let mut responses = Vec::new();
        for (label, handle) in pending {
            let (status, body) = handle.await.unwrap();
            responses.push((label, status, body));
        }
        let (_, status) = call(
            router.clone(),
            &act(0, Method::GET, "/status", None),
        )
        .await;
        let (_, metrics) = call(
            metrics_router.clone(),
            &act(0, Method::GET, "/metrics", None),
        )
        .await;
        let metric_names: Vec<String> = metrics
            .as_str()
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| l.split([' ', '{']).next())
            .map(str::to_owned)
            .collect();
        (running, idle, responses, status, metric_names, complete)
    });
    thread
        .join()
        .expect("the loop thread")
        .unwrap_or_else(|e| panic!("{}: {e:#}", runtime.label()));
    let (running, idle, responses, status, metric_names, complete) = recording;
    assert_eq!(running.len(), frames, "{}: the loop stopped early", runtime.label());

    let events: Vec<Value> = shared
        .events
        .page(0, flysim::eventlog::RING_CAPACITY)
        .into_iter()
        .map(|e| without_wall(serde_json::to_value(e).unwrap()))
        .collect();
    let event_file: Vec<Value> =
        flysim::eventlog::read_events(&config.paths.save_dir.join("events.jsonl"))
            .unwrap_or_default()
            .into_iter()
            .map(|e| without_wall(serde_json::to_value(e).unwrap()))
            .collect();
    let journal: Vec<Value> = flysim::journal::read(&config.paths.hot_dir.join(flysim::journal::FILE_NAME))
        .unwrap_or_default()
        .into_iter()
        .map(|mut line| {
            if let Some(boot) = line.get_mut("boot").and_then(Value::as_object_mut) {
                boot.remove("runtime");
                boot.remove("wallMs");
            }
            without_wall(line)
        })
        .collect();
    let durable = flysim::store::Store::new(&config.paths.save_dir, 64);
    let latest = durable.highest_generation();
    let bytes = std::fs::read(
        flysim::store::restore_order(
            &flysim::store::Store::new(&config.paths.hot_dir, 64),
            &durable,
        )
        .into_iter()
        .find(|c| c.generation == Some(latest))
        .expect("the final save")
        .path,
    )
    .unwrap();
    let final_checkpoint = checkpoint_json(&bytes);
    Recording {
        complete,
        running,
        idle,
        responses,
        status,
        metric_names,
        events,
        event_file,
        journal,
        final_checkpoint,
    }
}

fn action_label(action: &Action) -> String {
    format!("{} {}", action.method, action.path)
}

/// A FLYSIM01 checkpoint as comparable JSON: the runtime half without `wallMs`, and a digest of
/// every agent array.
fn checkpoint_json(bytes: &[u8]) -> Value {
    let checkpoint = flysim::store::decode(bytes).expect("FLYSIM01");
    let runtime = &checkpoint.runtime;
    let agent = &checkpoint.agent;
    json!({
        "generation": runtime.generation,
        "romSha256": runtime.rom_sha256,
        "emulatorFrame": runtime.emulator_frame,
        "compatibility": runtime.compatibility,
        "buttons": runtime.buttons,
        "rankSinceMs": runtime.rank_since_ms,
        "lastEventId": runtime.last_event_id,
        "reward": runtime.reward,
        "ratchet": serde_json::to_value(runtime.ratchet).unwrap(),
        "emulator": sha(&runtime.emulator),
        "framebuffer": sha(&runtime.framebuffer),
        "ratchetGame": sha(&runtime.ratchet_game),
        "agent": sha(format!("{agent:?}").as_bytes()),
    })
}

/// Field-by-field comparison with the first difference as the failure.
fn same(what: &str, a: &Value, b: &Value) {
    if a != b {
        panic!(
            "{what} differs:\n  legacy  {}\n  session {}",
            truncate(a),
            truncate(b)
        );
    }
}

fn truncate(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 4000 {
        format!("{}...", &text[..4000])
    } else {
        text
    }
}

fn compare(legacy: &Recording, session: &Recording, label: &str) {
    assert_eq!(
        legacy.running.len(),
        session.running.len(),
        "{label}: running snapshots"
    );
    for (n, ((a, da), (b, db))) in legacy.running.iter().zip(&session.running).enumerate() {
        if a != b {
            // Name the first differing field.
            let (Some(am), Some(bm)) = (a.as_object(), b.as_object()) else {
                panic!()
            };
            for key in am.keys().chain(bm.keys()) {
                same(
                    &format!("{label}: snapshot {n} (frame {}) {key}", a["frame"]),
                    &a[key.as_str()],
                    &b[key.as_str()],
                );
            }
        }
        for (i, name) in ["frame", "audio", "spikes"].iter().enumerate() {
            assert_eq!(
                da[i], db[i],
                "{label}: snapshot {n} (frame {}) attachment {name}",
                a["frame"]
            );
        }
    }
    assert!(!legacy.idle.is_empty(), "{label}: the legacy pause published no idle header");
    assert!(!session.idle.is_empty(), "{label}: the session pause published no idle header");
    if let (Some(a), Some(b)) = (legacy.idle.first(), session.idle.first()) {
        // An idle header carries the events not yet published, and which publish that is (the
        // pause's own or a later one, whichever the recorder saw first) is wall clock; the events
        // themselves are compared in the running snapshots and the event log.
        let strip = |v: &Value| {
            let mut v = v.clone();
            if let Some(map) = v.as_object_mut() {
                map.remove("events");
            }
            v
        };
        same(&format!("{label}: the first idle header"), &strip(a), &strip(b));
    }
    assert_eq!(legacy.responses.len(), session.responses.len());
    for ((la, sa, ba), (lb, sb, bb)) in legacy.responses.iter().zip(&session.responses) {
        assert_eq!(la, lb);
        assert_eq!(sa, sb, "{label}: {la} status");
        let strip = |v: &Value| {
            let mut v = v.clone();
            if let Some(events) = v.get_mut("events").and_then(Value::as_array_mut) {
                for e in events {
                    if let Some(e) = e.as_object_mut() {
                        e.remove("wallMs");
                    }
                }
            }
            v
        };
        same(&format!("{label}: {la} body"), &strip(ba), &strip(bb));
    }
    let status = |v: &Value| {
        let mut v = v.clone();
        let map = v.as_object_mut().unwrap();
        for key in ["wallMs", "uptimeSeconds", "realtimeFactor", "seq"] {
            map.remove(key);
        }
        if let Some(s) = map.get_mut("sugar").and_then(Value::as_object_mut) {
            s.remove("cooldownMs");
        }
        if let Some(c) = map.get_mut("checkpoint").and_then(Value::as_object_mut) {
            c.remove("latestWallMs");
        }
        if let Some(items) = map.get_mut("chat").and_then(Value::as_array_mut) {
            for item in items {
                item.as_object_mut().unwrap().remove("wallMs");
            }
        }
        v
    };
    // `/status` is read between frames on both sides; the frame it lands on is the driver's
    // timing, so only its shape and its stable parts are compared.
    let (a, b) = (status(&legacy.status), status(&session.status));
    for key in ["version", "status", "chat"] {
        same(&format!("{label}: /status {key}"), &a[key], &b[key]);
    }
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&a), keys(&b), "{label}: /status fields");
    assert_eq!(
        keys(&a["decoder"]),
        keys(&b["decoder"]),
        "{label}: /status decoder fields"
    );
    same(
        &format!("{label}: /status decoder channels"),
        &json!(a["decoder"]["channels"].as_array().map(|c| c.iter().map(|x| (x["channel"].clone(), x["role"].clone())).collect::<Vec<_>>())),
        &json!(b["decoder"]["channels"].as_array().map(|c| c.iter().map(|x| (x["channel"].clone(), x["role"].clone())).collect::<Vec<_>>())),
    );
    assert_eq!(legacy.metric_names, session.metric_names, "{label}: /metrics series");
    same(&format!("{label}: event ring"), &json!(legacy.events), &json!(session.events));
    same(
        &format!("{label}: events.jsonl"),
        &json!(legacy.event_file),
        &json!(session.event_file),
    );
    same(&format!("{label}: sugar journal"), &json!(legacy.journal), &json!(session.journal));
    same(
        &format!("{label}: final checkpoint"),
        &legacy.final_checkpoint,
        &session.final_checkpoint,
    );
}

/// Copy a seeded store (both directories) into a run's own directory.
fn seed(from: &Path, to: &Path) {
    for sub in ["durable", "hot"] {
        let src = from.join(sub);
        let dst = to.join(sub);
        std::fs::create_dir_all(&dst).unwrap();
        if let Ok(entries) = std::fs::read_dir(&src) {
            for entry in entries.flatten() {
                if entry.file_type().unwrap().is_file() {
                    std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
                }
            }
        }
    }
}

fn modes() -> Vec<SessionArm> {
    SessionArm::parity_arms()
}

#[allow(clippy::too_many_arguments)]
fn run_pair(
    rom: &Path,
    dataset: &Path,
    seeded: &Path,
    profile: LegacyProfileKind,
    mode: MacroMode,
    speed: f64,
    frames: usize,
    label: &str,
) {
    init_tracing();
    let actions = script();
    let root = tempfile::tempdir().unwrap();
    // One run of `runtime` from a freshly seeded copy of the store, repeated (up to three times)
    // when the recorder fell behind the loop on a loaded box.
    let run = |runtime: Runtime, name: &str| -> Recording {
        for attempt in 1..=3 {
            let dir = root.path().join(format!("{name}-{attempt}"));
            seed(seeded, &dir);
            let recording = record(
                runtime,
                config(rom, dataset, &dir, mode, speed),
                profile,
                frames,
                &actions,
            );
            if recording.complete {
                return recording;
            }
        }
        panic!("{}: the recorder fell behind three times", runtime.label());
    };
    let started = Instant::now();
    let legacy = run(Runtime::Legacy, "legacy");
    eprintln!("{label}: legacy {} running snapshots in {:.1?}", legacy.running.len(), started.elapsed());
    let runtimes = modes()
        .into_iter()
        .map(Runtime::Session)
        .chain([Runtime::SessionOverBus(SessionArm::LOCAL)]);
    for runtime in runtimes {
        let started = Instant::now();
        let exec = match runtime {
            Runtime::Session(exec) | Runtime::SessionOverBus(exec) => exec,
            Runtime::Legacy => unreachable!(),
        };
        let name = match runtime {
            Runtime::SessionOverBus(_) => format!("session-bus-{}", exec.label()),
            _ => format!("session-{}", exec.label()),
        };
        let session = run(runtime, &name);
        let label = format!("{label}, {}", runtime.label());
        compare(&legacy, &session, &label);
        let rewards = legacy
            .events
            .iter()
            .filter(|e| e["kind"] == "reward")
            .count();
        let macros = legacy.events.iter().filter(|e| e["kind"] == "macro").count();
        let kinds: std::collections::BTreeMap<String, usize> =
            legacy.events.iter().fold(Default::default(), |mut m, e| {
                *m.entry(e["kind"].as_str().unwrap_or("?").to_owned()).or_default() += 1;
                m
            });
        eprintln!("{label}: event kinds {kinds:?}");
        eprintln!(
            "{label}: IDENTICAL -- {} running snapshots (frames, audio, spikes byte-equal), {} idle, \
             {} control responses, {} events ({rewards} reward, {macros} macro), {} journal lines, \
             final checkpoint g{} equal ({:.1?})",
            session.running.len(),
            session.idle.len(),
            session.responses.len(),
            session.events.len(),
            session.journal.len(),
            session.final_checkpoint["generation"],
            started.elapsed()
        );
        for (what, status, body) in &session.responses {
            eprintln!("  {what} -> {status} {}", truncate(body));
        }
    }
}

/// A store seeded by a legacy fresh start of `seconds` wall seconds, shut down cleanly.
fn seed_by_legacy_fresh_start(rom: &Path, dataset: &Path, dir: &Path, mode: MacroMode) {
    let config = config(rom, dataset, dir, mode, 1.0);
    let shared = Arc::new(Shared::new(config.clone(), EventRing::new()));
    let (commands, command_rx) = mpsc::channel::<Command>(64);
    let (snapshots_tx, mut snapshots) =
        watch::channel(Arc::new(booting_snapshot(0, 0, mode)));
    let thread = std::thread::spawn(move || -> anyhow::Result<()> {
        let mut sim = Sim::boot(shared, snapshots_tx, command_rx)?;
        sim.run(&flysim::sdnotify::Notifier::from_env())
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        loop {
            snapshots.changed().await.unwrap();
            if snapshots.borrow().header.frame >= 120 {
                break;
            }
        }
        let (reply, ack) = tokio::sync::oneshot::channel();
        commands.send(Command::Shutdown { reply }).await.unwrap();
        let _ = ack.await;
    });
    thread.join().unwrap().unwrap();
}

#[test]
fn toy_raw_service_contracts() {
    let Some(rom) = rom_path() else { return };
    let dataset = fly_session::legacy_parity::toy::dir();
    let seeded = tempfile::tempdir().unwrap();
    seed_by_legacy_fresh_start(&rom, &dataset, seeded.path(), MacroMode::Raw);
    run_pair(
        &rom,
        &dataset,
        seeded.path(),
        LegacyProfileKind::Toy,
        MacroMode::Raw,
        0.25,
        120,
        "toy raw",
    );
}

// No toy macros run: AGENT-01's worker refuses a macro channel whose rate role the dataset does
// not track, and the committed toy connectome tracks none (the legacy loop merges them in). The
// production connectome tracks them all; `fafb_macros_*` below is the macros-mode run.

/// The real connectome from the live service's `rollback` checkpoint (ENV-01 traces): the
/// ratchet rolls back on the first boundary, so the recovery event, the `recovering` status and
/// the after-rollback durable save are on the contract too.
#[test]
fn fafb_macros_service_contracts_through_a_rollback() {
    let Some(rom) = rom_path() else { return };
    if std::env::var_os("FLY_SERVE01_FAFB").is_none() {
        eprintln!("skipping: FLY_SERVE01_FAFB is not set");
        return;
    }
    let Some(dataset) = fly_session::legacy_parity::fafb_dir() else {
        eprintln!("skipping: no data/fafb-v783");
        return;
    };
    // `FLY_SERVE01_FAFB_CHECKPOINT` picks another start (a reward-bearing one for a long run).
    let source = std::env::var_os("FLY_SERVE01_FAFB_CHECKPOINT")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("FLY_ENV01_TRACE_DIR")
                .map(|dir| PathBuf::from(dir).join("rollback.checkpoint"))
                .filter(|p| p.is_file())
        })
        .or_else(|| std::env::var_os("FLY_DOOR_CHECKPOINT").map(PathBuf::from));
    let Some(source) = source else {
        eprintln!("skipping: no source checkpoint");
        return;
    };
    eprintln!("fafb: from {}", source.display());
    let mut bytes = std::fs::read(&source).unwrap();
    if std::env::var_os("FLY_SERVE01_THIN").is_some() {
        bytes = reward_bearing(&bytes);
    }
    let generation = flysim::store::decode(&bytes).unwrap().runtime.generation;
    let seeded = tempfile::tempdir().unwrap();
    flysim::store::Store::new(seeded.path().join("durable"), 4)
        .commit(generation, &bytes, None)
        .unwrap();
    let frames = std::env::var("FLY_SERVE01_FAFB_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let speed = std::env::var("FLY_SERVE01_FAFB_SPEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.1);
    run_pair(
        &rom,
        &dataset,
        seeded.path(),
        LegacyProfileKind::Production,
        MacroMode::Macros,
        speed,
        frames,
        "fafb macros",
    );
}

/// TASK-01's ledger thinning (`session_trace.rs`), so a brain run from a stream checkpoint earns
/// rewards: the current map unvisited, its exits unfound, its ground unwalked, no wild win a
/// replay (`FLY_SERVE01_THIN`).
fn reward_bearing(bytes: &[u8]) -> Vec<u8> {
    let mut checkpoint = flysim::store::decode(bytes).expect("FLYSIM01");
    let reward = &mut checkpoint.runtime.reward;
    let map = reward["location"]
        .as_str()
        .and_then(|l| l.split(':').next())
        .unwrap_or("0")
        .to_owned();
    if let Some(seen) = reward["seen"].as_array_mut() {
        seen.retain(|key| {
            let key = key.as_str().unwrap_or("");
            key != format!("map:{map}") && !key.starts_with(&format!("boundary:{map}:"))
        });
    }
    if let Some(tiles) = reward["tiles"].as_array_mut() {
        tiles.retain(|tile| !tile.as_str().unwrap_or("").starts_with(&format!("{map}:")));
    }
    if let Some(counts) = reward["tileCounts"].as_object_mut() {
        counts.remove(&map);
    }
    reward["wildWins"] = json!({});
    reward["replayBlocked"] = json!([]);
    flysim::store::encode(&checkpoint.agent, &checkpoint.runtime).expect("encodes")
}
