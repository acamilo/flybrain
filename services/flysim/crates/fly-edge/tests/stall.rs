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
//! The gated tests assert the claim, and only the claim: the pacer reports no lag, no watch send
//! waits on a consumer, no publication is refused, the store stays bounded, and a healthy client
//! still reaches the newest snapshot. Those hold on a box at any load, because none of them is a
//! rate.
//!
//! How fast the loop's sleeps come back and how many snapshots a debug-build publisher gets
//! through measure the OS scheduler and the CPU left over, not the bus: a starved publisher
//! coalesces by design. Those bounds are in `the_three_scenarios_keep_their_rates`, which is
//! `#[ignore]`d; run it on a quiet box with
//! `cargo test --release -p fly-edge --test stall -- --ignored --nocapture`.

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

fn print_report(report: &LoopReport, publisher: &Metrics, what: &str) {
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
}

/// The claim: the loop is never held by the bus, whatever the load.
fn assert_unharmed(report: &LoopReport, publisher: &Metrics, what: &str) {
    assert_eq!(report.lag_seconds, 0.0, "{what}: the pacer fell behind");
    // A watch send is a lock and a swap. A send that waited on a consumer would be a whole
    // stall, seconds; 50 ms leaves room for a preempted thread on a loaded box.
    assert!(
        report.worst_send < Duration::from_millis(50),
        "{what}: a send took {:?}",
        report.worst_send
    );
    // A slow or absent consumer is never a reason to refuse a latest publication.
    assert_eq!(
        Metrics::get(&publisher.bus_publish_failures),
        0,
        "{what}: a publication was refused"
    );
    // Coalescing is allowed, stopping is not.
    assert!(
        Metrics::get(&publisher.bus_published) >= 1,
        "{what}: nothing reached the bus"
    );
}

/// Rates: meaningful only on a quiet box (see the module comment).
fn assert_rates(report: &LoopReport, publisher: &Metrics, what: &str) {
    // Sleep overshoot is absorbed by the next frame; under one frame at p99 means the loop kept
    // its absolute deadlines.
    assert!(
        report.p99_shortfall < 0.016,
        "{what}: p99 shortfall {:.2} ms",
        report.p99_shortfall * 1e3
    );
    let published = Metrics::get(&publisher.bus_published);
    assert!(
        published * 2 >= report.published,
        "{what}: only {published} of {} reached the bus",
        report.published
    );
}

const SECONDS: f64 = 6.0;

/// What a scenario leaves for the rate checks.
struct Outcome {
    report: LoopReport,
    publisher: Arc<Metrics>,
    /// Snapshots the healthy client received, where there is one.
    healthy_received: Option<u64>,
}

async fn stalled_clients() -> Outcome {
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
                let message = next_binary(&mut healthy, Duration::from_secs(120)).await;
                newest.store(seq_of(&message), Ordering::Relaxed);
                received.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    let snapshots = paths.snapshots.clone();
    let report = tokio::task::spawn_blocking(move || run_loop(snapshots, template, SECONDS))
        .await
        .unwrap();
    print_report(&report, &paths.publisher_metrics, "stalled clients");
    assert_unharmed(&report, &paths.publisher_metrics, "stalled clients");

    // The healthy client reaches the last snapshot published: the newest one always gets
    // through, however many in between were coalesced.
    let last = paths.snapshots.borrow().header.seq;
    let deadline = Instant::now() + Duration::from_secs(60);
    while newest.load(Ordering::Relaxed) < last {
        assert!(
            Instant::now() < deadline,
            "healthy client stuck at {} of {last}",
            newest.load(Ordering::Relaxed)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The stalled ones are still connected, not dropped for being slow.
    assert_eq!(paths.edge_metrics.feed.clients(), 4);
    reader.abort();
    drop(stalled);
    Outcome {
        report,
        publisher: Arc::clone(&paths.publisher_metrics),
        healthy_received: Some(received.load(Ordering::Relaxed)),
    }
}

async fn hoarding_subscriber() -> Outcome {
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
    print_report(&report, &paths.publisher_metrics, "hoarding subscriber");
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
    Outcome {
        report,
        publisher: Arc::clone(&paths.publisher_metrics),
        healthy_received: None,
    }
}

async fn absent_edge() -> Outcome {
    let template = full_snapshot();
    let paths = start(template.clone(), false).await;
    let snapshots = paths.snapshots.clone();
    let report = tokio::task::spawn_blocking(move || run_loop(snapshots, template, SECONDS))
        .await
        .unwrap();
    print_report(&report, &paths.publisher_metrics, "absent edge");
    assert_unharmed(&report, &paths.publisher_metrics, "absent edge");
    let stats = paths.bus.router.stats();
    assert!(
        stats.store_bytes <= 3 * 122_367,
        "store holds {} bytes",
        stats.store_bytes
    );
    Outcome {
        report,
        publisher: Arc::clone(&paths.publisher_metrics),
        healthy_received: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_of_the_edge_that_never_read_do_not_lag_the_loop_or_the_healthy_client() {
    stalled_clients().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bus_subscriber_that_never_releases_does_not_lag_the_loop_or_fill_the_store() {
    hoarding_subscriber().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absent_edge_costs_the_loop_nothing() {
    absent_edge().await;
}

/// The same three scenarios, plus the rates. A measurement of the box as much as of the bus,
/// so not part of the workspace gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing: needs a quiet box; run with --release -- --ignored"]
async fn the_three_scenarios_keep_their_rates() {
    for (what, outcome) in [
        ("stalled clients", stalled_clients().await),
        ("hoarding subscriber", hoarding_subscriber().await),
        ("absent edge", absent_edge().await),
    ] {
        assert_rates(&outcome.report, &outcome.publisher, what);
        if let Some(got) = outcome.healthy_received {
            assert!(
                got * 2 >= outcome.report.published,
                "{what}: the healthy client got only {got} of {}",
                outcome.report.published
            );
        }
    }
}
