//! The feed over flybus (`FLY_FEED_VIA=bus`, `docs/design/flybus.md` "Feed over the bus").
//!
//! Both halves of the bus encoding live here, so the publisher in flysim and the subscriber in
//! `fly-edge` cannot drift apart:
//!
//! - [`publish`] turns one [`Snapshot`] into one publication on [`TOPIC`]: every attachment the
//!   header lists as a sealed artifact named after its kind (`frame`, `audio`, `spikes`), and the
//!   header itself as the envelope payload `{"header": {...}}`. A header too large for an
//!   envelope travels as a `header` artifact instead, so no snapshot is ever unpublishable.
//! - [`receive`] turns that publication back into the same [`Snapshot`], which `fly-edge` hands to
//!   [`crate::feed`] exactly as flysim does. The WebSocket bytes are therefore produced by the same
//!   `Snapshot::encode` on both paths.
//!
//! The simulation thread never sees any of this. It publishes into its `watch` slot as it
//! always has; [`run_publisher`] is a task on the bus's own runtime that reads that slot and
//! skips whatever it was too slow to see, the same drop-oldest rule every feed client gets.
//! A stalled router, a full store or an absent edge can cost snapshots on the bus, never a
//! frame of the loop.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flybus::{
    Artifact, BusError, Client, ClientConfig, ErrorCode, Grants, Limits, Message, Pattern, Policy,
    PublishReceipt, Retained, Router, RouterConfig, UnixListenerHandle,
};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::metrics::Metrics;
use crate::snapshot::{AttachmentKind, FeedHeader, Snapshot};

/// The one topic: `latest` retention, so a subscriber that joins late starts from the newest
/// snapshot and one that falls behind is coalesced rather than queued.
pub const TOPIC: &str = "fly.feed.snapshots";
/// The publisher's participant id (in-process, launcher-bound).
pub const PUBLISHER: &str = "flysim";
/// The edge's participant id; the Unix socket is bound to it.
pub const EDGE: &str = "fly-edge";
/// Socket file under `feed.bus_dir`, bound to [`EDGE`] only.
pub const SOCKET: &str = "edge.sock";
/// Store root under `feed.bus_dir`; the router makes its per-incarnation directory inside it.
pub const STORE: &str = "store";
/// Headers up to this many JSON bytes ride in the envelope; anything larger becomes an artifact.
/// Well under flybus's 65,536-byte envelope limit, leaving room for the attachment references
/// and the router's ids. A real header is 2 to 8 KB.
pub const HEADER_INLINE_MAX: usize = 48 * 1024;
/// The attachment name of an out-of-line header.
pub const HEADER_ARTIFACT: &str = "header";

/// `<bus_dir>/edge.sock`.
pub fn socket_path(bus_dir: &Path) -> PathBuf {
    bus_dir.join(SOCKET)
}

/// `<bus_dir>/store`.
pub fn store_root(bus_dir: &Path) -> PathBuf {
    bus_dir.join(STORE)
}

/// The router limits for the feed (`docs/design/flybus.md`, amendment "Feed sizing").
///
/// One snapshot with attachments is 122,367 bytes on the live fly: a 92,160-byte 160x144 RGBA
/// frame, a 17,407-byte spike bitset (139,255 neurons) and about 12,800 bytes of audio (1,600
/// stereo f32 frames at 48 kHz per 30 Hz snapshot). A `latest` subscriber pins at most its one
/// queued slot plus its in-flight credits, the topic pins one retained value, and the publisher
/// holds one snapshot of staging plus the sealed copy while it seals.
///
/// Only one client can subscribe at all: the publisher is in process, and the one socket is
/// launcher-bound to [`EDGE`], which the router admits once at a time. So the worst case is
/// that client holding every subscription it may open ([`Limits::max_subscriptions_per_client`],
/// 4), each never consuming with in-flight credits at the cap of 2: `4 * 3 + 1 + 2 = 15`
/// snapshots, about 1.8 MB. `max_clients` bounds connections, pending handshakes included, not
/// subscribers. The store cap is well over ten times that so a burst of catch-up audio after a
/// stall still fits, and it is RAM (tmpfs), so it is kept small on purpose.
pub fn limits() -> Limits {
    Limits {
        max_clients: 8,
        max_services: 8,
        max_topics: 8,
        max_subscriptions_per_client: 4,
        max_subscriptions: 16,
        max_latest_in_flight: 2,
        max_owners_per_client: 64,
        reserved_owners_per_client: 8,
        // Audio accumulates while the loop is behind its publish deadline; 4 MiB is ten seconds
        // of it, far past anything the pacer allows before it logs lag.
        max_artifact_bytes: 4 << 20,
        max_store_bytes: 32 << 20,
        max_retained_bytes: 8 << 20,
        ..Limits::default()
    }
}

/// flysim may declare and publish the feed topic; the edge may only subscribe to it.
pub fn policy() -> Policy {
    Policy::closed()
        .client(
            PUBLISHER,
            Grants {
                publish: vec![Pattern::exact(TOPIC)],
                manage_topics: vec![Pattern::exact(TOPIC)],
                ..Grants::default()
            },
        )
        .client(
            EDGE,
            Grants {
                subscribe: vec![Pattern::exact(TOPIC)],
                ..Grants::default()
            },
        )
}

/// A running router and the edge's socket. Dropping it stops listening; the router stops with
/// the runtime it was started on.
pub struct BusFeed {
    pub router: Router,
    _listener: UnixListenerHandle,
}

/// Start the embedded router under `bus_dir` and listen for the edge on `<bus_dir>/edge.sock`.
///
/// Must run inside a Tokio runtime. `Router::new` removes store directories a previous flysim
/// left behind (their `flock` is free once that process is gone); a stale socket file is removed
/// here, because a socket outlives its listener on disk.
pub async fn start_router(bus_dir: &Path) -> anyhow::Result<BusFeed> {
    use anyhow::Context as _;
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(bus_dir)
        .with_context(|| format!("creating the bus directory {}", bus_dir.display()))?;
    let root = store_root(bus_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .with_context(|| format!("creating the bus store root {}", root.display()))?;
    let socket = socket_path(bus_dir);
    match std::fs::remove_file(&socket) {
        Ok(()) => tracing::info!(socket = %socket.display(), "removed a stale bus socket"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("removing {}", socket.display()));
        }
    }
    let mut config = RouterConfig::new(root);
    config.limits = limits();
    config.policy = policy();
    let router = Router::new(config).context("starting the flybus router")?;
    let listener = router
        .listen_unix_as(&socket, EDGE)
        .await
        .with_context(|| format!("listening on {}", socket.display()))?;
    tracing::info!(
        socket = %socket.display(),
        store = %router.store_dir().display(),
        router = router.router_id(),
        "feed bus listening"
    );
    Ok(BusFeed {
        router,
        _listener: listener,
    })
}

fn attachment_name(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Frame => "frame",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Spikes => "spikes",
    }
}

fn content_type(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Frame => "image/x-rgba",
        AttachmentKind::Audio => "audio/x-f32le",
        AttachmentKind::Spikes => "application/x-spike-bitset",
    }
}

fn bytes_of(snapshot: &Snapshot, kind: AttachmentKind) -> &[u8] {
    match kind {
        AttachmentKind::Frame => &snapshot.frame,
        AttachmentKind::Audio => &snapshot.audio,
        AttachmentKind::Spikes => &snapshot.spikes,
    }
}

async fn seal(client: &Client, bytes: &[u8], content_type: &str) -> Result<Artifact, BusError> {
    let mut writer = client
        .artifacts()
        .allocate(bytes.len() as u64, content_type)
        .await?;
    writer
        .write_all(bytes)
        .map_err(|error| BusError::new(ErrorCode::StoreFailure, format!("staging: {error}")))?;
    writer.seal().await
}

/// Publish one snapshot: its attachments as artifacts, its header as the payload.
pub async fn publish(client: &Client, snapshot: &Snapshot) -> Result<PublishReceipt, BusError> {
    let header = &snapshot.header;
    let json = serde_json::to_vec(header).expect("a FeedHeader always serializes");

    let mut kinds: Vec<AttachmentKind> = Vec::with_capacity(3);
    for kind in header.attachments.iter().copied() {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let mut artifacts: Vec<(&'static str, Artifact)> = Vec::with_capacity(4);
    for kind in kinds {
        let artifact = seal(client, bytes_of(snapshot, kind), content_type(kind)).await?;
        artifacts.push((attachment_name(kind), artifact));
    }

    let mut payload = Map::new();
    if json.len() <= HEADER_INLINE_MAX {
        let value: Value = serde_json::from_slice(&json).expect("a serialized header re-parses");
        payload.insert("header".into(), value);
    } else {
        artifacts.push((
            HEADER_ARTIFACT,
            seal(client, &json, "application/json").await?,
        ));
    }
    let attachments: Vec<(&str, &Artifact)> = artifacts
        .iter()
        .map(|(name, artifact)| (*name, artifact))
        .collect();
    client.publish(TOPIC, payload, &attachments).await
}

/// Rebuild the snapshot one publication carries. The message's delivery is released when the
/// caller drops it; every byte has been copied out by then.
pub async fn receive(message: &Message) -> Result<Snapshot, BusError> {
    let invalid = |what: String| BusError::new(ErrorCode::InvalidEnvelope, what);
    let header: FeedHeader = match message.payload().get("header") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("feed header: {error}")))?,
        None => {
            let bytes = message.artifact(HEADER_ARTIFACT)?.read_all().await?;
            serde_json::from_slice(&bytes)
                .map_err(|error| invalid(format!("feed header artifact: {error}")))?
        }
    };
    let mut snapshot = Snapshot {
        header,
        frame: Arc::new(Vec::new()),
        audio: Arc::new(Vec::new()),
        spikes: Arc::new(Vec::new()),
    };
    let kinds = snapshot.header.attachments.clone();
    for kind in kinds {
        let bytes = Arc::new(message.artifact(attachment_name(kind))?.read_all().await?);
        match kind {
            AttachmentKind::Frame => snapshot.frame = bytes,
            AttachmentKind::Audio => snapshot.audio = bytes,
            AttachmentKind::Spikes => snapshot.spikes = bytes,
        }
    }
    Ok(snapshot)
}

/// How often a failing publisher repeats its warning.
const WARN_EVERY: Duration = Duration::from_secs(10);

/// Publish every snapshot the sim puts in its watch slot until the sim is gone.
///
/// Connects in process as [`PUBLISHER`], declares [`TOPIC`] with `latest` retention and
/// publishes the current snapshot first, so an edge that connects at once still sees the boot
/// state. A refused publication (a full store, say) is counted and skipped; a lost connection
/// is re-made after a second. Borrows of the watch slot end before any await, exactly as in
/// [`crate::feed`]: a held borrow is a lock the sim thread's next publish would wait on.
pub async fn run_publisher(
    router: Router,
    mut snapshots: watch::Receiver<Arc<Snapshot>>,
    metrics: Arc<Metrics>,
) {
    let store_root = router.store_root().to_path_buf();
    let mut last_warning: Option<Instant> = None;
    let mut warn = |error: &BusError, what: &str| {
        if last_warning.is_none_or(|at| at.elapsed() >= WARN_EVERY) {
            tracing::warn!(%error, "feed bus: {what}");
            last_warning = Some(Instant::now());
        }
    };
    loop {
        let transport = router.connect_in_memory_as(PUBLISHER);
        let client =
            match Client::connect(transport, ClientConfig::new(PUBLISHER, &store_root)).await {
                Ok(client) => client,
                Err(error) => {
                    warn(&error, "the publisher could not connect");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
        if let Err(error) = client.declare_topic(TOPIC, Retained::Latest).await {
            warn(&error, "the feed topic could not be declared");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let mut current = snapshots.borrow_and_update().clone();
        loop {
            match publish(&client, &current).await {
                Ok(_) => Metrics::incr(&metrics.bus_published),
                Err(error) => {
                    Metrics::incr(&metrics.bus_publish_failures);
                    warn(&error, "a snapshot was not published");
                    if client.closed().is_some() {
                        break;
                    }
                }
            }
            if snapshots.changed().await.is_err() {
                // The sim thread is gone; so is the service.
                client.close().await;
                return;
            }
            current = snapshots.borrow_and_update().clone();
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limits_validate_and_hold_the_worst_case_with_room() {
        let limits = limits();
        limits.validate().unwrap();
        // A full snapshot on the live fly (see `limits`).
        let snapshot_bytes = crate::snapshot::FRAME_BYTES + 139_255usize.div_ceil(8) + 12_800;
        assert_eq!(snapshot_bytes, 122_367);
        // One subscribing client (the socket's), every subscription it may open, none consuming.
        let subscriptions = limits.max_subscriptions_per_client as u64;
        let pinned = subscriptions * (1 + limits.max_latest_in_flight) + 1 + 2;
        assert_eq!(pinned, 15);
        assert!(
            pinned * snapshot_bytes as u64 * 10 <= limits.max_store_bytes,
            "{pinned}"
        );
        assert!(limits.max_artifact_bytes >= crate::snapshot::FRAME_BYTES as u64 * 40);
    }
}
