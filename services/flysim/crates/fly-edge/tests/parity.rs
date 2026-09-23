//! Parity: a feed served through the bus and `fly-edge` is the feed flysim serves directly.
//!
//! The committed stage fixtures (`apps/stage/public/fixtures/*.flyfeed.gz`, the recordings the
//! stage's e2e suite plays) are fed snapshot by snapshot into one watch slot, served both ways at
//! once, and recorded by one client per path and per `wants` flavour: the stage's (everything),
//! the bridge's (nothing) and a frame-only one. The two recordings must match: headers equal
//! apart from wall-time fields and attachments byte-equal -- and in fact the whole messages are
//! byte-equal, because both are written by `Snapshot::encode` from equal snapshots. The edge's
//! attachments must also equal the fixture's own.
//!
//! `FLY_EDGE_PARITY_OUT=<dir>` also writes each pair of recordings as `.flyfeed` files, which
//! `packages/feed`'s `decodeFlyfeed` reads. `FLY_EDGE_PARITY_ALL=1` replays whole fixtures
//! instead of their first 400 snapshots.

mod common;

use std::time::Duration;

use common::*;
use serde_json::Value;

/// The fixtures whose headers carry every field the Rust producer writes. The three older ones
/// (`cold-open`, `steady`, `big-moment`) predate `game.scene` and cannot be a Rust `Snapshot`.
const FIXTURES: [&str; 4] = ["macros", "shop", "center", "bigpad"];
const WANTS: [(&str, &[&str]); 3] = [
    ("all", &["frame", "audio", "spikes"]),
    ("none", &[]),
    ("frame", &["frame"]),
];

/// The header with every `wallMs` removed, at any depth.
fn without_wall_time(header: &[u8]) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("wallMs");
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value: Value = serde_json::from_slice(header).unwrap();
    strip(&mut value);
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_writes_the_bytes_the_direct_feed_writes_for_every_committed_fixture() {
    let limit = if std::env::var_os("FLY_EDGE_PARITY_ALL").is_some() {
        usize::MAX
    } else {
        400
    };
    for name in FIXTURES {
        let snapshots: Vec<_> = fixture_messages(name)
            .iter()
            .take(limit)
            .map(|message| {
                (
                    message.clone(),
                    snapshot_of(message).unwrap_or_else(|| panic!("{name}")),
                )
            })
            .collect();
        assert!(
            snapshots.len() >= 100,
            "{name}: {} snapshots",
            snapshots.len()
        );

        let paths = start(snapshots[0].1.clone(), true).await;
        let mut clients = Vec::new();
        for (flavour, wants) in WANTS {
            let direct = connect(paths.direct, wants).await;
            let edge = connect(paths.edge, wants).await;
            clients.push((flavour, direct, edge, Vec::new(), Vec::new()));
        }

        // Lockstep: publish one snapshot, wait until every client has it. Nothing is superseded,
        // so both recordings are complete and comparable message by message.
        for (index, (_, snapshot)) in snapshots.iter().enumerate() {
            if index > 0 {
                paths
                    .snapshots
                    .send_replace(std::sync::Arc::new(snapshot.clone()));
            }
            for (_, direct, edge, direct_log, edge_log) in &mut clients {
                for (ws, log) in [
                    (&mut *direct, &mut *direct_log),
                    (&mut *edge, &mut *edge_log),
                ] {
                    let message = next_binary(ws, Duration::from_secs(20)).await;
                    assert_eq!(seq_of(&message), snapshot.header.seq, "{name} #{index}");
                    log.push(message);
                }
            }
        }

        for (flavour, _, _, direct_log, edge_log) in &clients {
            assert_eq!(direct_log.len(), snapshots.len());
            assert_eq!(edge_log.len(), direct_log.len());
            for (index, (direct, edge)) in direct_log.iter().zip(edge_log).enumerate() {
                let (direct_header, direct_attachments) = split(direct);
                let (edge_header, edge_attachments) = split(edge);
                assert_eq!(
                    without_wall_time(direct_header),
                    without_wall_time(edge_header),
                    "{name}/{flavour} #{index}: headers"
                );
                assert_eq!(
                    direct_attachments, edge_attachments,
                    "{name}/{flavour} #{index}: attachments"
                );
                // The stronger fact: the whole message, wall times included, is the same bytes.
                assert!(direct == edge, "{name}/{flavour} #{index}: messages differ");
                if *flavour == "all" {
                    let (_, fixture_attachments) = split(&snapshots[index].0);
                    assert_eq!(
                        edge_attachments, fixture_attachments,
                        "{name} #{index}: vs the fixture"
                    );
                }
            }
            if let Some(dir) = out_dir() {
                write(
                    &dir.join(format!("{name}-{flavour}-direct.flyfeed")),
                    &encode_flyfeed(name, "flysim direct", direct_log),
                );
                write(
                    &dir.join(format!("{name}-{flavour}-edge.flyfeed")),
                    &encode_flyfeed(name, "flysim bus + fly-edge", edge_log),
                );
            }
        }
        let published = flysim::metrics::Metrics::get(&paths.publisher_metrics.bus_published);
        assert!(
            published >= snapshots.len() as u64,
            "{name}: {published} published"
        );
        assert_eq!(
            flysim::metrics::Metrics::get(&paths.publisher_metrics.bus_publish_failures),
            0
        );
        eprintln!(
            "{name}: {} snapshots x {} flavours identical on both paths",
            snapshots.len(),
            WANTS.len()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_header_too_large_for_an_envelope_travels_as_an_artifact_and_arrives_intact() {
    let mut snapshot = snapshot_of(&fixture_messages("macros")[10]).unwrap();
    // Far past flybus's 65,536-byte envelope: 400 events of 200 characters.
    for id in 0..400u64 {
        snapshot.header.events.push(flysim::snapshot::FeedEvent {
            id: 10_000 + id,
            wall_ms: 1_757_000_000_000 + id,
            brain_ms: 5.0,
            kind: flysim::snapshot::FeedEventKind::System,
            label: "x".repeat(200),
            value: None,
            reward_kind: None,
            by: None,
        });
    }
    assert!(serde_json::to_vec(&snapshot.header).unwrap().len() > 65_536);
    let paths = start(snapshot.clone(), true).await;
    let mut direct = connect(paths.direct, &["frame", "audio", "spikes"]).await;
    let mut edge = connect(paths.edge, &["frame", "audio", "spikes"]).await;
    let direct = next_binary(&mut direct, Duration::from_secs(20)).await;
    let edge = next_binary(&mut edge, Duration::from_secs(20)).await;
    assert!(direct == edge);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_drops_its_clients_and_unbinds_when_the_bus_goes_away_then_comes_back() {
    let snapshot = snapshot_of(&fixture_messages("shop")[5]).unwrap();
    let paths = start(snapshot.clone(), true).await;
    let mut edge = connect(paths.edge, &[]).await;
    next_binary(&mut edge, Duration::from_secs(20)).await;

    // flysim stopping is its router stopping.
    let Paths {
        snapshots,
        edge: edge_addr,
        edge_metrics,
        bus_dir,
        bus,
        ..
    } = paths;
    bus.router.shutdown();
    drop(bus);
    drop(snapshots);
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        use futures_util::StreamExt as _;
        loop {
            match edge.next().await {
                None | Some(Err(_)) => break,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the client was not dropped");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while tokio::net::TcpStream::connect(edge_addr).await.is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the feed port stayed bound"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        edge_metrics
            .connected
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    // A new flysim on the same directory: the edge finds it and serves again.
    let (snapshots, receiver) = tokio::sync::watch::channel(std::sync::Arc::new(snapshot.clone()));
    let bus = flysim::feedbus::start_router(bus_dir.path()).await.unwrap();
    tokio::spawn(flysim::feedbus::run_publisher(
        bus.router.clone(),
        receiver,
        std::sync::Arc::new(flysim::metrics::Metrics::default()),
    ));
    let mut edge = connect(edge_addr, &[]).await;
    let message = next_binary(&mut edge, Duration::from_secs(20)).await;
    assert_eq!(seq_of(&message), snapshot.header.seq);
    assert_eq!(
        edge_metrics
            .bus_lost
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    drop(snapshots);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edge_whose_port_is_taken_keeps_retrying_and_serves_once_it_is_free() {
    let snapshot = snapshot_of(&fixture_messages("center")[7]).unwrap();
    // Someone else (flysim still in direct mode, say) holds the feed port before the edge starts.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap();
    let bus_dir = tempfile::tempdir().unwrap();
    let (snapshots, receiver) = tokio::sync::watch::channel(std::sync::Arc::new(snapshot.clone()));
    let bus = flysim::feedbus::start_router(bus_dir.path()).await.unwrap();
    tokio::spawn(flysim::feedbus::run_publisher(
        bus.router.clone(),
        receiver,
        std::sync::Arc::new(flysim::metrics::Metrics::default()),
    ));
    let metrics = std::sync::Arc::new(fly_edge::EdgeMetrics::default());
    tokio::spawn(fly_edge::run(
        fly_edge::EdgeConfig {
            bus_dir: bus_dir.path().to_path_buf(),
            feed_bind: port,
            idle_period: NO_IDLE,
            metrics_bind: None,
            retry: Duration::from_millis(50),
        },
        std::sync::Arc::clone(&metrics),
    ));
    // It reaches the bus, fails to bind, and says so rather than claiming to wait for the bus.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while metrics
        .bind_failures
        .load(std::sync::atomic::Ordering::Relaxed)
        < 3
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the edge never reached the bus"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        metrics.connected.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        metrics.bus_lost.load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    drop(squatter);
    let mut edge = connect(port, &[]).await;
    let message = next_binary(&mut edge, Duration::from_secs(20)).await;
    assert_eq!(seq_of(&message), snapshot.header.seq);
    assert_eq!(
        metrics.connected.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    drop(snapshots);
    drop(bus);
}
