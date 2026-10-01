//! EDGE-02: the session runtime serves the feed over the bus.
//!
//! `flysim-session` runs behind `flysim::serve`, so `FLY_FEED_VIA=bus` should work on it exactly
//! as on `flysim`. This runs the real `flysim-session` binary (the toy connectome, the real
//! cartridge, a store seeded by a legacy fresh start) twice from copies of the same store:
//!
//! - `direct`: the binary binds the feed port and a WebSocket client records it;
//! - `bus`: the binary binds nothing on the feed port, `fly-edge` (in process) does, and the same
//!   client records that. The binary is then stopped and started again under the same edge, which
//!   must unbind, reconnect and serve the new instance.
//!
//! The two recordings are compared snapshot by snapshot on the sim frame: the header minus the
//! fields that are wall clock by definition, and the three attachments byte for byte. Gated on
//! `FLY_ROM` (source `bin/rom-env.sh`): without it the test returns early.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use fly_edge::{EdgeConfig, EdgeMetrics};
use flysim::config::{Config, FeedVia};
use flysim::eventlog::EventRing;
use flysim::simloop::{Command, Shared, Sim, booting_snapshot};
use flysim::snapshot::MacroMode;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;

fn rom_path() -> Option<PathBuf> {
    match std::env::var_os("FLY_ROM") {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
            None
        }
    }
}

fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

/// The service configuration: slow enough that a recorder on a loaded box sees every frame, every
/// frame published, no timed checkpoints.
fn config(rom: &Path, dataset: &Path, dir: &Path, via: FeedVia, bus_dir: &Path) -> Config {
    let mut config = Config::default();
    config.paths.rom = rom.to_path_buf();
    config.paths.dataset = dataset.to_path_buf();
    config.paths.save_dir = dir.join("durable");
    config.paths.hot_dir = dir.join("hot");
    config.loop_.game = "pokemon-red".to_owned();
    config.loop_.speed = 0.25;
    config.loop_.threads = 1;
    config.loop_.snapshot_hz = 240.0;
    config.loop_.hot_seconds = 1e9;
    config.loop_.checkpoint_seconds = 1e9;
    config.macros.mode = MacroMode::Raw;
    config.chat.enabled = false;
    config.feed.via = via;
    config.feed.bus_dir = bus_dir.to_path_buf();
    config.feed.bind = free_port();
    config.control.bind = free_port();
    config.control.metrics_bind = Some(free_port());
    config
}

/// A store seeded by a legacy fresh start, shut down cleanly (as `service_parity` does).
fn seed_by_legacy_fresh_start(rom: &Path, dataset: &Path, dir: &Path) {
    let mut config = config(rom, dataset, dir, FeedVia::Direct, dir);
    config.loop_.speed = 1.0;
    let shared = Arc::new(Shared::new(config.clone(), EventRing::new()));
    let (commands, command_rx) = mpsc::channel::<Command>(64);
    let (snapshots_tx, mut snapshots) =
        watch::channel(Arc::new(booting_snapshot(0, 0, MacroMode::Raw)));
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

fn seed(from: &Path, to: &Path) {
    for sub in ["durable", "hot"] {
        let dst = to.join(sub);
        std::fs::create_dir_all(&dst).unwrap();
        if let Ok(entries) = std::fs::read_dir(from.join(sub)) {
            for entry in entries.flatten() {
                if entry.file_type().unwrap().is_file() {
                    std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
                }
            }
        }
    }
}

/// A running `flysim-session`, stopped (SIGTERM, then kill) when dropped.
struct Service {
    child: Child,
    config: Config,
}

impl Service {
    fn start(config: Config, work: &Path, name: &str) -> Service {
        let path = work.join(format!("{name}.toml"));
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        let log = std::fs::File::create(work.join(format!("{name}.log"))).unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_flysim-session"))
            .arg("--config")
            .arg(&path)
            .env("FLY_SESSION_PROFILE", "toy")
            .env("FLY_SESSION_DIR", work.join(format!("{name}-session")))
            // The test process's own cpus are not the sim's.
            .env("FLY_SESSION_PIN", "0")
            .env_remove("FLY_FEED_VIA")
            .env_remove("FLY_BUS_DIR")
            .env_remove("FLY_FEED_BIND")
            .env_remove("FLY_CONTROL_BIND")
            .env_remove("FLY_METRICS_ADDR")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("flysim-session starts");
        Service { child, config }
    }

    /// SIGTERM and wait for the clean stop (the unit's own way to stop it).
    fn stop(&mut self) {
        let pid = self.child.id().to_string();
        let _ = Process::new("kill").args(["-TERM", &pid]).status();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Whether `flysim-session` itself holds the feed port (a connect succeeds and the peer is
    /// not the edge): used only for the direct run, where it must.
    fn log(&self, work: &Path, name: &str) -> PathBuf {
        work.join(format!("{name}.log"))
    }

    fn feed_bind(&self) -> SocketAddr {
        self.config.feed.bind
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What one running snapshot comes to on the wire.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    header: Value,
    attachments: Vec<Vec<u8>>,
}

fn comparable(header: &Value) -> Value {
    let mut value = header.clone();
    let map = value.as_object_mut().expect("a header is an object");
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

fn parse(message: &[u8]) -> Seen {
    let read = |at: usize| u32::from_le_bytes(message[at..at + 4].try_into().unwrap()) as usize;
    let header_len = read(0);
    let header: Value = serde_json::from_slice(&message[4..4 + header_len]).unwrap();
    let mut at = 4 + header_len;
    let mut attachments = Vec::new();
    while at < message.len() {
        let len = read(at);
        attachments.push(message[at + 4..at + 4 + len].to_vec());
        at += 4 + len;
    }
    Seen {
        header,
        attachments,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect (retrying while nothing listens yet) and say `hello`.
async fn connect(addr: SocketAddr, within: Duration, log: &Path) -> Ws {
    let url = format!("ws://{addr}/feed");
    let deadline = Instant::now() + within;
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((mut ws, _)) => {
                let hello = serde_json::json!({ "protocol": 1, "client": "feed-bus-test", "wants": ["frame", "audio", "spikes"] });
                ws.send(WsMessage::Text(hello.to_string().into()))
                    .await
                    .unwrap();
                return ws;
            }
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "{url}: {error}\n--- {} ---\n{}",
                    log.display(),
                    std::fs::read_to_string(log).unwrap_or_default()
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Running snapshots (three attachments) by sim frame, until `frames` of them or `within`.
async fn record(ws: &mut Ws, frames: usize, within: Duration) -> BTreeMap<u64, Seen> {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen = BTreeMap::new();
    while seen.len() < frames {
        let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, ws.next()).await else {
            break;
        };
        if let WsMessage::Binary(bytes) = message {
            let snapshot = parse(&bytes);
            if snapshot.attachments.len() == 3 {
                let frame = snapshot.header["frame"].as_u64().unwrap();
                seen.insert(frame, snapshot);
            }
        }
    }
    seen
}

fn fresh_dir(root: &Path, seeded: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    seed(seeded, &dir);
    dir
}

#[test]
fn the_session_runtime_serves_the_same_feed_over_the_bus() {
    let Some(rom) = rom_path() else { return };
    let dataset = fly_session::legacy_parity::toy::dir();
    let root = tempfile::tempdir().unwrap();
    let seeded = root.path().join("seeded");
    std::fs::create_dir_all(&seeded).unwrap();
    seed_by_legacy_fresh_start(&rom, &dataset, &seeded);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    const FRAMES: usize = 90;
    let within = Duration::from_secs(90);

    // Direct: the service binds the feed port itself.
    let direct = {
        let dir = fresh_dir(root.path(), &seeded, "direct");
        let bus = root.path().join("direct-bus");
        let mut service = Service::start(
            config(&rom, &dataset, &dir, FeedVia::Direct, &bus),
            root.path(),
            "direct",
        );
        let recorded = runtime.block_on(async {
            let mut ws = connect(
                service.feed_bind(),
                Duration::from_secs(60),
                &service.log(root.path(), "direct"),
            )
            .await;
            record(&mut ws, FRAMES, within).await
        });
        service.stop();
        assert!(!bus.join("edge.sock").exists(), "direct mode opened a bus");
        recorded
    };
    assert!(
        direct.len() >= FRAMES,
        "direct recorded {} running snapshots",
        direct.len()
    );

    // Bus: the service leaves the feed port to the edge.
    let bus_dir = root.path().join("bus");
    let bus = {
        let dir = fresh_dir(root.path(), &seeded, "bus");
        let service_config = config(&rom, &dataset, &dir, FeedVia::Bus, &bus_dir);
        let edge_addr = service_config.feed.bind;
        let edge_metrics = Arc::new(EdgeMetrics::default());
        let edge = EdgeConfig {
            retry: Duration::from_millis(100),
            ..EdgeConfig::from_flysim(&service_config, None)
        };
        let _edge = runtime.spawn(fly_edge::run(edge, Arc::clone(&edge_metrics)));
        let mut service = Service::start(service_config.clone(), root.path(), "bus");
        let recorded = runtime.block_on(async {
            // Nothing is bound until the edge has its first snapshot, and it is the edge that binds.
            let mut ws = connect(
                edge_addr,
                Duration::from_secs(60),
                &root.path().join("bus.log"),
            )
            .await;
            let recorded = record(&mut ws, FRAMES, within).await;
            assert_eq!(edge_metrics.connected.load(Ordering::Relaxed), 1);
            recorded
        });
        service.stop();
        // The edge notices, drops its clients and unbinds the port.
        let lost = Instant::now();
        while edge_metrics.connected.load(Ordering::Relaxed) != 0 {
            assert!(
                lost.elapsed() < Duration::from_secs(10),
                "the edge never saw the bus go"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(edge_metrics.bus_lost.load(Ordering::Relaxed) >= 1);

        // A new instance under the same edge: it reconnects by itself and serves the new router.
        let mut again = Service::start(service_config, root.path(), "bus-again");
        runtime.block_on(async {
            let mut ws = connect(
                edge_addr,
                Duration::from_secs(60),
                &root.path().join("bus-again.log"),
            )
            .await;
            let after = record(&mut ws, 10, within).await;
            assert!(
                after.len() >= 10,
                "after the restart: {} running snapshots",
                after.len()
            );
        });
        assert_eq!(edge_metrics.connected.load(Ordering::Relaxed), 1);
        again.stop();
        recorded
    };
    assert!(
        bus.len() >= FRAMES,
        "bus recorded {} running snapshots",
        bus.len()
    );

    // The same frames, the same bytes.
    let mut compared = 0;
    for (frame, a) in &direct {
        let Some(b) = bus.get(frame) else { continue };
        assert_eq!(
            comparable(&a.header),
            comparable(&b.header),
            "frame {frame}: header"
        );
        assert_eq!(a.attachments.len(), b.attachments.len(), "frame {frame}");
        for (index, (x, y)) in a.attachments.iter().zip(&b.attachments).enumerate() {
            assert!(x == y, "frame {frame}: attachment {index} differs");
        }
        compared += 1;
    }
    eprintln!(
        "session feed: direct {} and bus {} running snapshots, {compared} frames in common and identical",
        direct.len(),
        bus.len()
    );
    assert!(compared >= FRAMES / 2, "only {compared} frames in common");
}
