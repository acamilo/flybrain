//! A slow or absent edge never slows the loop.
//!
//! A thread stands in for the sim loop: flysim's own `Pacer` at realtime speed and 60 Hz Game
//! Boy frames, publishing full-size snapshots (a real 92,160-byte frame, a 17,407-byte spike
//! bitset for 139,255 neurons, 12,800 bytes of audio) into the watch slot every second frame,
//! with `watch::Sender::send`, exactly as `Sim::publish` does. Around it, three kinds of bad
//! consumer:
//!
//! - the edge is up but three of its WebSocket clients never read, so their sockets fill;
//! - the edge's place on the bus is held by a subscriber that takes deliveries and never
//!   releases them (the "slow edge");
//! - nobody is subscribed at all (the "absent edge").
//!
//! In every case the pacer reports no lag, no watch send is slow, the publisher keeps
//! publishing without a refusal, and where there is a healthy client it stays current.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::*;
use flybus::{Client, ClientConfig, SubscriptionConfig};
use flysim::feedbus;
use flysim::metrics::Metrics;
use flysim::pacing::Pacer;
use flysim::snapshot::{AttachmentKind, FeedStatus, Snapshot};

/// A full-size running snapshot from a real fixture frame.
fn full_snapshot() -> Snapshot {
    let mut snapshot = snapshot_of(&fixture_messages("macros")[3]).unwrap();
    assert_eq!(snapshot.frame.len(), flysim::snapshot::FRAME_BYTES);
    snapshot.header.status = FeedStatus::Running;
    snapshot.header.attachments = AttachmentKind::ALL.to_vec();
    snapshot.spikes = Arc::new(vec![0b1010_0101; 139_255usize.div_ceil(8)]);
    snapshot.audio = Arc::new(vec![7; 12_800]);
    snapshot
}

struct LoopReport {
    frames: u64,
    published: u64,
    lag_seconds: f64,
    worst_send: Duration,
    worst_shortfall: f64,
    p99_shortfall: f64,
}

/// Run the stand-in loop for `seconds` on its own thread.
fn run_loop(
    snapshots: tokio::sync::watch::Sender<Arc<Snapshot>>,
    template: Snapshot,
    seconds: f64,
) -> LoopReport {
    std::thread::spawn(move || {
        let frame_ms = flysim::config::GAMEBOY_MS_PER_FRAME;
        let mut pacer = Pacer::new(frame_ms, 1.0, Instant::now());
        let frames = (seconds * 1000.0 / frame_ms) as u64;
        let mut worst_send = Duration::ZERO;
        let mut shortfalls = Vec::with_capacity(frames as usize);
        let mut seq = template.header.seq;
        let mut published = 0;
        for frame in 0..frames {
            if frame % 2 == 0 {
                let mut snapshot = template.clone();
                seq += 1;
                snapshot.header.seq = seq;
                snapshot.header.frame = frame;
                let started = Instant::now();
                let _ = snapshots.send(Arc::new(snapshot));
                worst_send = worst_send.max(started.elapsed());
                published += 1;
            }
            let now = Instant::now();
            shortfalls.push(pacer.shortfall_seconds(now));
            let sleep = pacer.next_sleep(now);
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        }
        shortfalls.sort_by(f64::total_cmp);
        LoopReport {
            frames,
            published,
            lag_seconds: pacer.lag_seconds(),
            worst_send,
            worst_shortfall: *shortfalls.last().unwrap(),
            p99_shortfall: shortfalls[shortfalls.len() * 99 / 100],
        }
    })
    .join()
    .unwrap()
}

fn assert_unharmed(report: &LoopReport, publisher: &Metrics, what: &str) {
    eprintln!(
        "{what}: {} frames, {} snapshots, lag {:.3} s, worst send {:?}, shortfall p99 {:.2} ms worst {:.2} ms, bus published {} failed {}",
        report.frames,
        report.published,
        report.lag_seconds,
        report.worst_send,
        report.p99_shortfall * 1e3,
        report.worst_shortfall * 1e3,
        Metrics::get(&publisher.bus_published),
        Metrics::get(&publisher.bus_publish_failures),
    );
    assert_eq!(report.lag_seconds, 0.0, "{what}: the pacer fell behind");
    // A watch send is a lock and a swap. Generous for a loaded 4-core box; a send that waited on
    // a consumer would be one whole stall, seconds.
    assert!(
        report.worst_send < Duration::from_millis(20),
        "{what}: a send took {:?}",
        report.worst_send
    );
    // Sleep overshoot is absorbed by the next frame; staying under one frame at p99 means the
    // loop kept its absolute deadlines.
    assert!(
        report.p99_shortfall < 0.016,
        "{what}: p99 shortfall {:.2} ms",
        report.p99_shortfall * 1e3
    );
    assert_eq!(
        Metrics::get(&publisher.bus_publish_failures),
        0,
        "{what}: a publication was refused"
    );
    // The publisher is allowed to coalesce, never to stop.
    let published = Metrics::get(&publisher.bus_published);
    assert!(
        published * 2 >= report.published,
        "{what}: only {published} of {} reached the bus",
        report.published
    );
}

const SECONDS: f64 = 6.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_of_the_edge_that_never_read_do_not_lag_the_loop_or_the_healthy_client() {
    let template = full_snapshot();
    let paths = start(template.clone(), true).await;
    // Three stages that said hello and then stopped reading: their sockets fill and stay full.
    let mut stalled = Vec::new();
    for _ in 0..3 {
        stalled.push(connect(paths.edge, &["frame", "audio", "spikes"]).await);
    }
    // One healthy stage, read continuously.
    let mut healthy = connect(paths.edge, &["frame", "audio", "spikes"]).await;
    let newest = Arc::new(AtomicU64::new(0));
    let received = Arc::new(AtomicU64::new(0));
    let reader = {
        let (newest, received) = (Arc::clone(&newest), Arc::clone(&received));
        tokio::spawn(async move {
            loop {
                let message = next_binary(&mut healthy, Duration::from_secs(30)).await;
                newest.store(seq_of(&message), Ordering::Relaxed);
                received.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    let snapshots = paths.snapshots.clone();
    let report = tokio::task::spawn_blocking(move || run_loop(snapshots, template, SECONDS))
        .await
        .unwrap();
    assert_unharmed(&report, &paths.publisher_metrics, "stalled clients");

    // The healthy client is current: within a few snapshots of the last one published.
    let last = paths.snapshots.borrow().header.seq;
    let deadline = Instant::now() + Duration::from_secs(10);
    while newest.load(Ordering::Relaxed) < last {
        assert!(
            Instant::now() < deadline,
            "healthy client stuck at {} of {last}",
            newest.load(Ordering::Relaxed)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let got = received.load(Ordering::Relaxed);
    assert!(
        got * 2 >= report.published,
        "the healthy client got only {got} of {}",
        report.published
    );
    // The stalled ones are still connected, not dropped for being slow.
    assert_eq!(paths.edge_metrics.feed.clients(), 4);
    reader.abort();
    drop(stalled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bus_subscriber_that_never_releases_does_not_lag_the_loop_or_fill_the_store() {
    let template = full_snapshot();
    let paths = start(template.clone(), false).await;
    // The edge's seat, taken by a subscriber that keeps every delivery it gets.
    let client = Client::connect_unix(
        feedbus::socket_path(paths.bus_dir.path()),
        ClientConfig::new(feedbus::EDGE, feedbus::store_root(paths.bus_dir.path())),
    )
    .await
    .unwrap();
    let mut subscription = client
        .subscribe(
            feedbus::TOPIC,
            SubscriptionConfig::latest().in_flight(2).replay(true),
        )
        .await
        .unwrap();
    let hoard = tokio::spawn(async move {
        let mut kept = Vec::new();
        while let Some(message) = subscription.next().await {
            kept.push(message);
        }
        kept.len()
    });

    let snapshots = paths.snapshots.clone();
    let report = tokio::task::spawn_blocking(move || run_loop(snapshots, template, SECONDS))
        .await
        .unwrap();
    assert_unharmed(&report, &paths.publisher_metrics, "hoarding subscriber");
    let stats = paths.bus.router.stats();
    eprintln!(
        "hoarding subscriber: store {} bytes, retained {} bytes",
        stats.store_bytes, stats.retained_bytes
    );
    // Held: two in flight, one queued, one retained, and whatever is mid-seal. Bounded, not
    // growing with the number published.
    assert!(
        stats.store_bytes <= 8 * 122_367,
        "store holds {} bytes",
        stats.store_bytes
    );
    hoard.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absent_edge_costs_the_loop_nothing() {
    let template = full_snapshot();
    let paths = start(template.clone(), false).await;
    let snapshots = paths.snapshots.clone();
    let report = tokio::task::spawn_blocking(move || run_loop(snapshots, template, SECONDS))
        .await
        .unwrap();
    assert_unharmed(&report, &paths.publisher_metrics, "absent edge");
    let stats = paths.bus.router.stats();
    assert!(
        stats.store_bytes <= 3 * 122_367,
        "store holds {} bytes",
        stats.store_bytes
    );
}
