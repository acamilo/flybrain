//! Pub/sub: exact topics, bounded FIFO with whole-publication backpressure, latest coalescing,
//! credits, retention, incarnations and fair control delivery under a saturated subscriber.

mod common;

use std::time::Duration;

use common::{Via, code, env, env_with, obj, quiet, sealed, within};
use flybus::{ErrorCode, Limits, Mode, Policy, Retained, ServiceConfig, SubscriptionConfig};
use serde_json::json;

async fn bounded_fifo_and_atomic_backpressure(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let slow = e.client("slow").await;
    let fast = e.client("fast").await;
    publisher
        .declare_topic("session.demo.events", Retained::Latest)
        .await
        .unwrap();
    let mut a = slow
        .subscribe(
            "session.demo.events",
            SubscriptionConfig::bounded().queued(2).in_flight(1),
        )
        .await
        .unwrap();
    let mut b = fast
        .subscribe("session.demo.events", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let p = |n: u64| obj(json!({ "n": n }));

    let r1 = publisher
        .publish("session.demo.events", p(1), &[])
        .await
        .unwrap();
    assert_eq!((r1.topic_sequence, r1.subscribers, r1.replaced), (1, 2, 0));
    let m1 = within("a gets 1", a.next()).await.unwrap();
    publisher
        .publish("session.demo.events", p(2), &[])
        .await
        .unwrap();
    publisher
        .publish("session.demo.events", p(3), &[])
        .await
        .unwrap();
    // a holds 1 in flight and 2, 3 queued: the next publication would overflow it, so nobody
    // gets it, the retained value does not move and no sequence number is spent.
    let full = publisher
        .publish("session.demo.events", p(4), &[])
        .await
        .unwrap_err();
    assert_eq!(full.code, ErrorCode::Backpressure);
    for n in 1..=3u64 {
        let m = within("b in order", b.next()).await.unwrap();
        assert_eq!(
            (m.payload()["n"].as_u64(), m.topic_sequence()),
            (Some(n), n)
        );
    }
    quiet("b gets no partial fan-out", b.next()).await;
    let mut late = fast
        .subscribe(
            "session.demo.events",
            SubscriptionConfig::bounded().replay(true),
        )
        .await
        .unwrap();
    let replayed = within("replay", late.next()).await.unwrap();
    assert_eq!(
        replayed.payload()["n"],
        3,
        "the refused publication did not become the retained value"
    );
    drop(m1);
    let m2 = within("credit returned", a.next()).await.unwrap();
    assert_eq!(m2.payload()["n"], 2);
    let r5 = publisher
        .publish("session.demo.events", p(5), &[])
        .await
        .unwrap();
    assert_eq!(r5.topic_sequence, 4);
}

async fn latest_coalesces_only_undelivered_values(via: Via) {
    let e = env(via).await;
    let camera = e.client("camera").await;
    let viewer = e.client("viewer").await;
    camera
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut sub = viewer
        .subscribe(
            "world.demo.frame",
            SubscriptionConfig::latest().in_flight(1),
        )
        .await
        .unwrap();
    let frames: Vec<_> = futures_join(&camera, 4).await;
    let r = camera
        .publish(
            "world.demo.frame",
            obj(json!({"n": 1})),
            &[("frame", &frames[0])],
        )
        .await
        .unwrap();
    assert_eq!(r.replaced, 0);
    let first = within("first frame", sub.next()).await.unwrap();
    let held = first.artifact("frame").unwrap();
    let mut replaced = 0;
    for (i, f) in frames.iter().enumerate().skip(1) {
        let r = camera
            .publish(
                "world.demo.frame",
                obj(json!({"n": i + 1})),
                &[("frame", f)],
            )
            .await
            .unwrap();
        replaced += r.replaced;
    }
    assert_eq!(
        replaced, 2,
        "frames 2 and 3 were replaced while undelivered"
    );
    drop(frames);
    // Replaced queue entries released their roots; the delivered frame and the queued one stay.
    e.settle("replaced frames collected", |s| s.sealed_artifacts == 2)
        .await;
    assert_eq!(
        held.read_all().await.unwrap(),
        b"frame-0",
        "delivered data is never reclaimed early"
    );
    drop((first, held));
    let last = within("latest frame", sub.next()).await.unwrap();
    assert_eq!((last.topic_sequence(), last.replaced()), (4, 2));
    assert_eq!(
        last.artifact("frame").unwrap().read_all().await.unwrap(),
        b"frame-3"
    );
    drop(last);
    e.settle("all collected", |s| s.artifacts == 0 && s.owners == 0)
        .await;
}

async fn futures_join(client: &flybus::Client, n: usize) -> Vec<flybus::Artifact> {
    let mut out = Vec::new();
    for i in 0..n {
        out.push(sealed(client, format!("frame-{i}").as_bytes(), "image/x-rgba").await);
    }
    out
}

async fn credits_return_only_on_consume(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let reader = e.client("reader").await;
    publisher
        .declare_topic("t.credits", Retained::None)
        .await
        .unwrap();
    let mut sub = reader
        .subscribe("t.credits", SubscriptionConfig::bounded().in_flight(2))
        .await
        .unwrap();
    for n in 0..5 {
        publisher
            .publish("t.credits", obj(json!({"n": n})), &[])
            .await
            .unwrap();
    }
    let m0 = within("0", sub.next()).await.unwrap();
    let m1 = within("1", sub.next()).await.unwrap();
    quiet("third while two are held", sub.next()).await;
    drop(m0);
    let m2 = within("2 after a consume", sub.next()).await.unwrap();
    assert_eq!(m2.payload()["n"], 2);
    quiet("fourth while two are held", sub.next()).await;
    drop((m1, m2));
    assert_eq!(within("3", sub.next()).await.unwrap().payload()["n"], 3);
}

async fn retained_replay_clear_delete_and_incarnations(via: Via) {
    let e = env(via).await;
    let admin = e.client("admin").await;
    let reader = e.client("reader").await;
    let info = admin
        .declare_topic("session.demo.snapshots", Retained::Latest)
        .await
        .unwrap();
    assert!(info.declared);
    let again = admin
        .declare_topic("session.demo.snapshots", Retained::Latest)
        .await
        .unwrap();
    assert_eq!(
        (again.declared, &again.topic_incarnation),
        (false, &info.topic_incarnation)
    );
    let conflict = admin
        .declare_topic("session.demo.snapshots", Retained::None)
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::Conflict);

    let snap = sealed(&admin, b"snapshot-1", "application/octet-stream").await;
    let r = admin
        .publish(
            "session.demo.snapshots",
            obj(json!({"step": "1"})),
            &[("state", &snap)],
        )
        .await
        .unwrap();
    assert_eq!(r.subscribers, 0);
    drop(snap);
    e.settle("retention holds the only root", |s| {
        s.sealed_artifacts == 1 && s.artifact_roots == 1
    })
    .await;

    let mut plain = reader
        .subscribe("session.demo.snapshots", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let mut replay = reader
        .subscribe(
            "session.demo.snapshots",
            SubscriptionConfig::latest().replay(true),
        )
        .await
        .unwrap();
    let m = within("replay", replay.next()).await.unwrap();
    assert_eq!(m.topic_sequence(), 1, "replay keeps the original sequence");
    assert_eq!(
        m.artifact("state").unwrap().read_all().await.unwrap(),
        b"snapshot-1"
    );
    quiet("no replay without replayLatest", plain.next()).await;

    assert!(admin.clear_topic("session.demo.snapshots").await.unwrap());
    assert!(!admin.clear_topic("session.demo.snapshots").await.unwrap());
    // Clearing does not invalidate the delivery still held.
    assert_eq!(
        m.artifact("state").unwrap().read_all().await.unwrap(),
        b"snapshot-1"
    );
    drop(m);
    e.settle("cleared value collected", |s| s.artifacts == 0)
        .await;

    let busy = admin
        .delete_topic("session.demo.snapshots")
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::Conflict);
    admin
        .publish("session.demo.snapshots", obj(json!({"step": "2"})), &[])
        .await
        .unwrap();
    let old = within("before delete", plain.next()).await.unwrap();
    drop((plain, replay));
    e.settle("unsubscribed", |s| s.subscriptions == 0).await;
    assert!(admin.delete_topic("session.demo.snapshots").await.unwrap());
    assert!(!admin.delete_topic("session.demo.snapshots").await.unwrap());
    let fresh = admin
        .declare_topic("session.demo.snapshots", Retained::Latest)
        .await
        .unwrap();
    assert_ne!(fresh.topic_incarnation, info.topic_incarnation);
    assert_eq!(
        old.topic_incarnation(),
        info.topic_incarnation,
        "old deliveries keep their incarnation"
    );
    let r = admin
        .publish("session.demo.snapshots", obj(json!({})), &[])
        .await
        .unwrap();
    assert_eq!(
        r.topic_sequence, 1,
        "a fresh incarnation restarts its sequence"
    );
}

async fn zero_subscriber_publish_retains_nothing(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    p.declare_topic("t.void", Retained::None).await.unwrap();
    let art = sealed(&p, &[7u8; 4096], "application/octet-stream").await;
    let r = p
        .publish("t.void", obj(json!({})), &[("x", &art)])
        .await
        .unwrap();
    assert_eq!(r.subscribers, 0);
    drop(art);
    e.settle("no owner left", |s| s.artifacts == 0 && s.store_bytes == 0)
        .await;
}

async fn unsubscribe_discards_queue_but_not_deliveries(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    let r = e.client("r").await;
    p.declare_topic("t.unsub", Retained::None).await.unwrap();
    let mut sub = r
        .subscribe("t.unsub", SubscriptionConfig::bounded().in_flight(1))
        .await
        .unwrap();
    for i in 0..3 {
        let a = sealed(&p, format!("m{i}").as_bytes(), "text/plain").await;
        p.publish("t.unsub", obj(json!({})), &[("m", &a)])
            .await
            .unwrap();
    }
    let first = within("first", sub.next()).await.unwrap();
    e.settle("three objects", |s| s.sealed_artifacts == 3).await;
    drop(sub);
    e.settle("queued entries released", |s| {
        s.sealed_artifacts == 1 && s.subscriptions == 0
    })
    .await;
    assert_eq!(
        first.artifact("m").unwrap().read_all().await.unwrap(),
        b"m0"
    );
    drop(first);
    e.settle("delivered entry released", |s| s.artifacts == 0)
        .await;
}

async fn subscription_and_topic_validation(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    let missing = c
        .subscribe("t.missing", SubscriptionConfig::latest())
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NoTopic);
    assert_eq!(
        c.publish("t.missing", obj(json!({})), &[])
            .await
            .unwrap_err()
            .code,
        ErrorCode::NoTopic
    );
    c.declare_topic("t.ok", Retained::None).await.unwrap();
    let bad = SubscriptionConfig {
        mode: Mode::Latest,
        max_queued: 2,
        max_in_flight: 1,
        replay_latest: false,
    };
    assert_eq!(
        c.subscribe("t.ok", bad).await.unwrap_err().code,
        ErrorCode::InvalidEnvelope
    );
    let over = SubscriptionConfig::latest().in_flight(3);
    assert_eq!(
        c.subscribe("t.ok", over).await.unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
    let zero = SubscriptionConfig::bounded().in_flight(0);
    assert_eq!(
        c.subscribe("t.ok", zero).await.unwrap_err().code,
        ErrorCode::InvalidEnvelope
    );
    assert_eq!(
        c.declare_topic("t..bad", Retained::None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidEnvelope
    );
    assert_eq!(
        c.declare_topic("T.upper", Retained::None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidEnvelope
    );
    // The connection survives every refusal above.
    assert!(
        c.subscribe("t.ok", SubscriptionConfig::bounded())
            .await
            .is_ok()
    );
}

async fn topic_and_retention_quotas(via: Via) {
    let limits = Limits {
        max_topics: 2,
        max_retained_bytes: 100,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let c = e.client("c").await;
    c.declare_topic("t.a", Retained::Latest).await.unwrap();
    c.declare_topic("t.b", Retained::Latest).await.unwrap();
    assert_eq!(
        c.declare_topic("t.c", Retained::None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::QuotaExceeded
    );
    let small = sealed(&c, &[1u8; 60], "application/octet-stream").await;
    let big = sealed(&c, &[2u8; 50], "application/octet-stream").await;
    c.publish("t.a", obj(json!({})), &[("x", &small)])
        .await
        .unwrap();
    // Two names for one object count once.
    c.publish("t.a", obj(json!({})), &[("x", &small), ("y", &small)])
        .await
        .unwrap();
    assert_eq!(e.stats().retained_bytes, 60);
    let over = c
        .publish("t.b", obj(json!({})), &[("x", &big)])
        .await
        .unwrap_err();
    assert_eq!(over.code, ErrorCode::QuotaExceeded);
    assert_eq!(
        e.stats().retained_bytes,
        60,
        "a refused publication changes nothing"
    );
    // Replacing a topic's own retained value is measured net of the old one.
    c.publish("t.a", obj(json!({})), &[("x", &big)])
        .await
        .unwrap();
    assert_eq!(e.stats().retained_bytes, 50);
}

/// A subscriber that never reads its socket must not slow anyone else: publications are
/// admitted until its queue refuses them, and RPC and control traffic between other clients
/// (and to a client that merely holds its credits) keep flowing.
async fn saturated_subscriber_does_not_block_control(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let busy = e.client("busy").await;
    let caller = e.client("caller").await;
    publisher
        .declare_topic("t.flood", Retained::None)
        .await
        .unwrap();

    // 1. A raw subscriber that never reads.
    let mut stuck = e.raw_hello("stuck").await;
    let r = stuck
        .call("subscribe", json!({"topic": "t.flood", "mode": "bounded", "maxQueued": 64, "maxInFlight": 16, "replayLatest": false}))
        .await;
    assert_eq!(code(&r), "OK");
    // 2. An SDK subscriber that reads but never consumes, and also serves RPC.
    let _held = busy
        .subscribe("t.flood", SubscriptionConfig::bounded().in_flight(16))
        .await
        .unwrap();
    let mut svc = busy
        .register("busy.status", ServiceConfig::default())
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        while let Some(req) = svc.next().await {
            req.reply(obj(json!({"alive": true})), &[]).await.unwrap();
        }
    });

    let blob = "x".repeat(60_000);
    let mut accepted = 0;
    let refused = loop {
        match within(
            "publish",
            publisher.publish("t.flood", obj(json!({"blob": blob})), &[]),
        )
        .await
        {
            Ok(_) => accepted += 1,
            Err(err) => break err,
        }
        assert!(accepted < 1000, "publications were never refused");
    };
    assert_eq!(refused.code, ErrorCode::Backpressure);
    assert!(accepted >= 16, "only {accepted} publications were admitted");
    for _ in 0..20 {
        let res = tokio::time::timeout(
            Duration::from_secs(2),
            caller.call_and_wait("busy.status", None, "Status", obj(json!({})), &[]),
        )
        .await
        .expect("RPC to a client whose subscription is saturated")
        .unwrap();
        assert_eq!(res.outcome()["alive"], true);
    }
    // The saturated subscriber's own control lane still answers once it reads again.
    let id = stuck
        .command(
            "topic.declare",
            json!({"name": "t.other", "retained": "none"}),
            json!([]),
        )
        .await;
    assert_eq!(code(&stuck.reply(&id).await), "OK");
    server.abort();
}

async fn router_shutdown_closes_subscriptions_with_notices(via: Via) {
    let e = env(via).await;
    let admin = e.client("admin").await;
    admin.declare_topic("t.stop", Retained::None).await.unwrap();
    let mut raw = e.raw_hello("raw").await;
    let sub = raw
        .call("subscribe", json!({"topic": "t.stop", "mode": "latest", "maxQueued": 1, "maxInFlight": 1, "replayLatest": false}))
        .await
        .unwrap();
    e.router.shutdown();
    let closed = raw.event().await;
    assert_eq!(
        (closed.op.as_str(), &closed.body["subscriptionId"]),
        ("subscription.closed", &sub["subscriptionId"])
    );
    assert_eq!(closed.body["reason"], "router-stopping");
    let last = raw.event().await;
    assert_eq!(
        (last.op.as_str(), last.body["code"].as_str()),
        ("connection.closing", Some("ROUTER_LOST"))
    );
    assert!(raw.recv().await.is_none());
}

both_transports!(
    router_shutdown_closes_subscriptions_with_notices,
    bounded_fifo_and_atomic_backpressure,
    latest_coalesces_only_undelivered_values,
    credits_return_only_on_consume,
    retained_replay_clear_delete_and_incarnations,
    zero_subscriber_publish_retains_nothing,
    unsubscribe_discards_queue_but_not_deliveries,
    subscription_and_topic_validation,
    topic_and_retention_quotas,
    saturated_subscriber_does_not_block_control,
);
