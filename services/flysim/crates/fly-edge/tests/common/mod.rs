#![allow(dead_code)]

//! Shared pieces of the edge tests: the committed `.flyfeed` fixtures as snapshots, a feed
//! client, the two serving paths side by side, and a `.flyfeed` writer.

use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fly_edge::{EdgeConfig, EdgeMetrics};
use flysim::feed::FeedState;
use flysim::feedbus;
use flysim::metrics::Metrics;
use flysim::snapshot::{AttachmentKind, FeedHeader, Snapshot};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message as WsMessage;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("the repository root is above services/flysim/crates/fly-edge")
}

/// `u32 LE headerLength | header JSON | (u32 LE length | bytes)*`, split.
pub fn split(message: &[u8]) -> (&[u8], Vec<&[u8]>) {
    let read = |at: usize| u32::from_le_bytes(message[at..at + 4].try_into().unwrap()) as usize;
    let header_len = read(0);
    let header = &message[4..4 + header_len];
    let mut at = 4 + header_len;
    let mut attachments = Vec::new();
    while at < message.len() {
        let len = read(at);
        attachments.push(&message[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    (header, attachments)
}

/// A wire message back into the snapshot that produced it, or `None` when its header predates
/// fields the Rust producer always writes (the three oldest fixtures lack `game.scene`).
pub fn snapshot_of(message: &[u8]) -> Option<Snapshot> {
    let (header, attachments) = split(message);
    let header: FeedHeader = serde_json::from_slice(header).ok()?;
    let mut snapshot = Snapshot {
        header,
        frame: Arc::new(Vec::new()),
        audio: Arc::new(Vec::new()),
        spikes: Arc::new(Vec::new()),
    };
    for (kind, bytes) in snapshot
        .header
        .attachments
        .clone()
        .into_iter()
        .zip(attachments)
    {
        let bytes = Arc::new(bytes.to_vec());
        match kind {
            AttachmentKind::Frame => snapshot.frame = bytes,
            AttachmentKind::Audio => snapshot.audio = bytes,
            AttachmentKind::Spikes => snapshot.spikes = bytes,
        }
    }
    Some(snapshot)
}

/// Every record of `apps/stage/public/fixtures/<name>.flyfeed.gz`, as wire messages.
pub fn fixture_messages(name: &str) -> Vec<Vec<u8>> {
    let path = repo_root().join(format!("apps/stage/public/fixtures/{name}.flyfeed.gz"));
    let gz = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(&bytes[..8], b"FLYFEED\0", "{name}");
    let read = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(read(8), 1, "{name}: container version");
    let mut at = 16 + read(12);
    let mut out = Vec::new();
    while at < bytes.len() {
        let len = read(at);
        out.push(bytes[at + 4..at + 4 + len].to_vec());
        at += 4 + len;
    }
    out
}

/// A `.flyfeed` file of `messages` (`packages/feed/src/fixture.ts`).
pub fn encode_flyfeed(name: &str, source: &str, messages: &[Vec<u8>]) -> Vec<u8> {
    let wall = |message: &Vec<u8>| -> u64 {
        let header: serde_json::Value = serde_json::from_slice(split(message).0).unwrap();
        header["wallMs"].as_u64().unwrap_or(0)
    };
    let duration = match (messages.first(), messages.last()) {
        (Some(first), Some(last)) => wall(last).saturating_sub(wall(first)),
        _ => 0,
    };
    let manifest = serde_json::json!({
        "name": name,
        "protocol": 1,
        "recordedAt": "1970-01-01T00:00:00.000Z",
        "source": source,
        "snapshotCount": messages.len(),
        "durationMs": duration,
        "hz": 30,
        "attachmentPolicy": {
            "frame": { "stride": 1 },
            "audio": { "stride": 1 },
            "spikes": { "stride": 1 }
        },
    });
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let mut out = b"FLYFEED\0".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    out.extend_from_slice(&manifest);
    for message in messages {
        out.extend_from_slice(&(message.len() as u32).to_le_bytes());
        out.extend_from_slice(message);
    }
    out
}

pub fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect and say `hello`, retrying while the port is not bound yet (the edge binds only once
/// its first snapshot has arrived).
pub async fn connect(addr: SocketAddr, wants: &[&str]) -> Ws {
    let url = format!("ws://{addr}/feed");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((mut ws, _)) => {
                let hello = serde_json::json!({ "protocol": 1, "client": "test", "wants": wants });
                ws.send(WsMessage::Text(hello.to_string().into()))
                    .await
                    .unwrap();
                return ws;
            }
            Err(error) => {
                assert!(tokio::time::Instant::now() < deadline, "{url}: {error}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// The next binary message, within `within`.
pub async fn next_binary(ws: &mut Ws, within: Duration) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let message = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("a snapshot in time")
            .expect("the feed stays open")
            .expect("a well-formed frame");
        if let WsMessage::Binary(bytes) = message {
            return bytes.to_vec();
        }
    }
}

pub fn seq_of(message: &[u8]) -> u64 {
    let header: serde_json::Value = serde_json::from_slice(split(message).0).unwrap();
    header["seq"].as_u64().unwrap()
}

/// The same watch slot served both ways at once: flysim's direct server on `direct`, and the
/// bus (router, publisher, edge) on `edge`. Owns its runtime-side tasks through the handles.
pub struct Paths {
    pub snapshots: watch::Sender<Arc<Snapshot>>,
    pub direct: SocketAddr,
    pub edge: SocketAddr,
    pub publisher_metrics: Arc<Metrics>,
    pub edge_metrics: Arc<EdgeMetrics>,
    pub bus_dir: tempfile::TempDir,
    pub bus: feedbus::BusFeed,
}

/// Idle cadence long enough that no test sees a header repeated for idleness.
pub const NO_IDLE: Duration = Duration::from_secs(3_600);

/// Start both paths over `first`. `edge` false leaves the edge out (a test then plays its part).
pub async fn start(first: Snapshot, with_edge: bool) -> Paths {
    let bus_dir = tempfile::tempdir().unwrap();
    let (snapshots, receiver) = watch::channel(Arc::new(first));
    let bus = feedbus::start_router(bus_dir.path()).await.unwrap();
    let publisher_metrics = Arc::new(Metrics::default());
    tokio::spawn(feedbus::run_publisher(
        bus.router.clone(),
        receiver.clone(),
        Arc::clone(&publisher_metrics),
    ));

    let direct = free_port();
    let listener = tokio::net::TcpListener::bind(direct).await.unwrap();
    let state = FeedState {
        snapshots: receiver,
        metrics: Arc::new(Metrics::default()),
        idle_period: NO_IDLE,
    };
    tokio::spawn(async move { axum::serve(listener, flysim::feed::router(state)).await });

    let edge = free_port();
    let edge_metrics = Arc::new(EdgeMetrics::default());
    if with_edge {
        let config = EdgeConfig {
            bus_dir: bus_dir.path().to_path_buf(),
            feed_bind: edge,
            idle_period: NO_IDLE,
            metrics_bind: None,
            retry: Duration::from_millis(50),
        };
        tokio::spawn(fly_edge::run(config, Arc::clone(&edge_metrics)));
    }
    Paths {
        snapshots,
        direct,
        edge,
        publisher_metrics,
        edge_metrics,
        bus_dir,
        bus,
    }
}

pub fn out_dir() -> Option<PathBuf> {
    std::env::var_os("FLY_EDGE_PARITY_OUT").map(PathBuf::from)
}

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
