//! Black-box conformance tests for artifact ownership (bus-v1 section 8): allocate/seal/open,
//! immutability past a stale writable handle, ownership surviving message drop, explicit
//! retain/release, final-owner collection, root accounting across queued/latest/retained
//! delivery, forward-before-release ordering, admission rollback, disconnect cleanup, and
//! rejection of stale or forged references and owners. Every test runs over both transports.
//!
//! These tests use only the public `flybus` API (plus the `common::Raw` protocol harness for
//! adversarial frames no well-behaved SDK client would send) and generic bytes over synthetic
//! messages; nothing here depends on any application on top of the bus.

mod common;

use std::io::Write;

use common::{Via, code, env, env_with, obj, sealed, within};
use flybus::{ErrorCode, Limits, Policy, Retained, ServiceConfig, SubscriptionConfig};
use serde_json::{Value, json};

// -------------------------------------------------------------------------------------------
// Allocate / seal / open

async fn allocate_write_seal_open_roundtrip_and_mismatches(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;

    let mut w = c.artifacts().allocate(5, "text/plain").await.unwrap();
    assert_eq!(w.byte_length(), 5);
    w.write_all(b"hello").unwrap();
    assert!(
        w.write(b"!").is_err(),
        "a write past the declared length must fail locally"
    );
    let art = w.seal().await.unwrap();
    assert_eq!(art.reference().byte_length, 5);
    assert_eq!(art.reference().content_type, "text/plain");
    assert_eq!(art.read_all().await.unwrap(), b"hello");
    drop(art);
    e.settle("sealed object collected", |s| s.sealed_artifacts == 0)
        .await;

    // Sealing declares more bytes than were actually staged: rejected, not silently truncated.
    let mut raw = e.raw_hello("raw").await;
    let (alloc, path) = raw.allocate(10).await;
    std::fs::write(&path, b"short").unwrap();
    assert_eq!(
        code(&raw.seal(&alloc, Value::Null).await),
        "ARTIFACT_MISMATCH"
    );

    // A syntactically valid but wrong digest is refused too.
    let (alloc2, path2) = raw.allocate(4).await;
    std::fs::write(&path2, b"data").unwrap();
    assert_eq!(
        code(&raw.seal(&alloc2, json!("0".repeat(64))).await),
        "ARTIFACT_MISMATCH"
    );
}

/// Sealing copies staging into a fresh inode; a producer's writable handle that survives the
/// seal (or a duplicate of it) can only reach the abandoned staging file, never the sealed
/// bytes (bus-v1 section 8.1 and the store module's own contract).
async fn seal_is_immutable_despite_a_stale_writable_handle(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("producer").await;
    let (alloc, path) = raw.allocate(7).await;
    let mut stale = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    stale.write_all(b"before1").unwrap();
    stale.flush().unwrap();

    let sealed_reply = raw.seal(&alloc, Value::Null).await.unwrap();
    // Tamper through the handle only after the router has answered the seal: the sealed copy
    // already exists and the staging file is already unlinked.
    stale.write_all(b"AFTER!!").unwrap();

    let open_reply = raw
        .call(
            "artifact.open",
            json!({"ref": sealed_reply["ref"], "ownerId": sealed_reply["ownerId"]}),
        )
        .await
        .unwrap();
    let sealed_path = raw.path(&open_reply["readLocation"]);
    assert_eq!(
        std::fs::read(sealed_path).unwrap(),
        b"before1",
        "a post-seal write must never reach the sealed copy"
    );
}

// -------------------------------------------------------------------------------------------
// Ownership surviving drop, explicit retain/release, final-owner collection

/// The doc's own illustrative snippet (bus-v1 section 2): `drop(message)` alone must not
/// consume the delivery while an artifact extracted from it is still held.
async fn extracted_artifact_outlives_the_message_it_came_from(via: Via) {
    let e = env(via).await;
    let camera = e.client("camera").await;
    let viewer = e.client("viewer").await;
    camera
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut sub = viewer
        .subscribe("world.demo.frame", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let frame = sealed(&camera, b"pixels", "image/x-rgba").await;
    camera
        .publish("world.demo.frame", obj(json!({})), &[("frame", &frame)])
        .await
        .unwrap();
    drop(frame);

    let message = within("frame delivered", sub.next()).await.unwrap();
    let image = message.artifact("frame").unwrap();
    drop(message);
    e.settle("the delivery still owns its artifact", |s| {
        s.owners == 1 && s.sealed_artifacts == 1
    })
    .await;
    assert_eq!(
        image.read_all().await.unwrap(),
        b"pixels",
        "the surviving handle still reads fine"
    );
    drop(image);
    e.settle("dropping the last handle finally consumes it", |s| {
        s.owners == 0 && s.sealed_artifacts == 0
    })
    .await;
}

async fn explicit_retain_outlives_the_original_hold(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    let original = sealed(&c, b"payload", "application/octet-stream").await;
    let retained = original.retain().await.unwrap();
    assert_eq!(
        retained.reference().artifact_id,
        original.reference().artifact_id
    );
    assert_ne!(
        retained.owner_id(),
        original.owner_id(),
        "retain creates an independent owner"
    );

    drop(original);
    e.settle("the retained hold keeps it sealed", |s| {
        s.sealed_artifacts == 1 && s.artifact_roots == 1
    })
    .await;
    assert_eq!(retained.read_all().await.unwrap(), b"payload");
    drop(retained);
    e.settle("collected once the last owner drops", |s| {
        s.sealed_artifacts == 0 && s.owners == 0
    })
    .await;
}

async fn collection_waits_for_every_retained_owner(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    let a = sealed(&c, b"payload", "application/octet-stream").await;
    let h1 = a.retain().await.unwrap();
    let h2 = a.retain().await.unwrap();
    drop(a);
    e.settle("two independent holds remain", |s| {
        s.owners == 2 && s.sealed_artifacts == 1
    })
    .await;
    drop(h1);
    e.settle("one hold remains", |s| {
        s.owners == 1 && s.sealed_artifacts == 1
    })
    .await;
    drop(h2);
    e.settle("the final owner's drop collects it", |s| {
        s.owners == 0 && s.sealed_artifacts == 0 && s.store_bytes == 0
    })
    .await;
    assert_eq!(e.files("sealed"), 0);
}

// -------------------------------------------------------------------------------------------
// Root accounting across queued, latest and retained delivery (bus-v1 section 8.2)

/// A message queued behind a saturated credit already holds a root at admission time, before
/// it is ever dispatched to the client; dispatch transfers that root into an owner, it does
/// not add a second one.
async fn queued_deliveries_hold_roots_before_dispatch(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    let r = e.client("r").await;
    p.declare_topic("t.queued", Retained::None).await.unwrap();
    let mut sub = r
        .subscribe(
            "t.queued",
            SubscriptionConfig::bounded().in_flight(1).queued(4),
        )
        .await
        .unwrap();

    let a1 = sealed(&p, b"m1", "application/octet-stream").await;
    let a2 = sealed(&p, b"m2", "application/octet-stream").await;
    p.publish("t.queued", obj(json!({})), &[("x", &a1)])
        .await
        .unwrap();
    p.publish("t.queued", obj(json!({})), &[("x", &a2)])
        .await
        .unwrap();
    drop((a1, a2));
    e.settle("one dispatched owner, two roots", |s| {
        s.artifact_roots == 2 && s.owners == 1 && s.sealed_artifacts == 2
    })
    .await;

    let held = within("dispatched message", sub.next()).await.unwrap();
    assert_eq!(held.artifact("x").unwrap().read_all().await.unwrap(), b"m1");
    drop(held);
    let second = within("previously queued message", sub.next())
        .await
        .unwrap();
    assert_eq!(
        second.artifact("x").unwrap().read_all().await.unwrap(),
        b"m2"
    );
    e.settle("dispatch moved the root, it did not add one", |s| {
        s.artifact_roots == 1 && s.owners == 1
    })
    .await;
    drop(second);
    e.settle("fully collected", |s| {
        s.artifact_roots == 0 && s.owners == 0 && s.sealed_artifacts == 0
    })
    .await;
}

/// `latest` never queues more than one undelivered value; coalescing releases the replaced
/// root immediately, and the already-dispatched value stays independent of it.
async fn latest_mode_holds_at_most_two_roots_delivered_plus_queued(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    let r = e.client("r").await;
    p.declare_topic("t.latest", Retained::None).await.unwrap();
    let mut sub = r
        .subscribe("t.latest", SubscriptionConfig::latest().in_flight(1))
        .await
        .unwrap();
    for i in 0..3u8 {
        let a = sealed(&p, &[i], "application/octet-stream").await;
        p.publish("t.latest", obj(json!({})), &[("x", &a)])
            .await
            .unwrap();
    }
    // i=0 was already dispatched (an owner); i=1 was coalesced away by i=2 while still queued.
    e.settle(
        "a delivered root plus one coalesced-survivor root, no more",
        |s| s.artifact_roots == 2 && s.owners == 1 && s.sealed_artifacts == 2,
    )
    .await;

    let first = within("delivered", sub.next()).await.unwrap();
    assert_eq!(first.artifact("x").unwrap().read_all().await.unwrap(), &[0]);
    drop(first);
    let last = within("coalesced survivor", sub.next()).await.unwrap();
    assert_eq!(last.artifact("x").unwrap().read_all().await.unwrap(), &[2]);
    drop(last);
    e.settle("fully collected", |s| {
        s.artifact_roots == 0 && s.owners == 0 && s.sealed_artifacts == 0
    })
    .await;
}

/// A retained topic value is a root the router itself keeps; it has no per-connection owner,
/// so nothing about a subscriber connecting, reading or disconnecting can move it.
async fn retained_topic_value_holds_a_root_independent_of_subscribers(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    p.declare_topic("t.retained", Retained::Latest)
        .await
        .unwrap();
    let a = sealed(&p, b"snapshot", "application/octet-stream").await;
    let r = p
        .publish("t.retained", obj(json!({})), &[("x", &a)])
        .await
        .unwrap();
    assert_eq!(r.subscribers, 0);
    drop(a);
    e.settle("a retained root with no owner", |s| {
        s.artifact_roots == 1 && s.owners == 0 && s.sealed_artifacts == 1
    })
    .await;
    assert!(p.clear_topic("t.retained").await.unwrap());
    e.settle("clearing releases the retained root", |s| {
        s.artifact_roots == 0 && s.sealed_artifacts == 0
    })
    .await;
}

// -------------------------------------------------------------------------------------------
// Forward before release (bus-v1 section 8.2: destination roots before source release)

/// A forward or reply must use a still-live source owner; once a delivery is released its
/// artifacts cannot source a new admission, but forwarding *before* releasing is exactly what
/// keeps bytes alive once the original delivery finally goes.
async fn forward_requires_the_source_owner_still_live(via: Via) {
    let e = env(via).await;
    let caller = e.client("caller").await;
    let consumer = e.client("consumer").await;
    consumer
        .declare_topic("t.relay", Retained::None)
        .await
        .unwrap();
    let mut sub = consumer
        .subscribe("t.relay", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let mut server = e.raw_hello("server").await;
    assert_eq!(
        code(
            &server
                .call(
                    "service.register",
                    json!({"name": "agent.relay", "maxQueued": 4, "maxInFlight": 4})
                )
                .await
        ),
        "OK"
    );

    // Round 1: release the request delivery, then try to forward using its stale owner.
    let art1 = sealed(&caller, b"payload-1", "application/octet-stream").await;
    let _c1 = caller
        .call(
            "agent.relay",
            None,
            "Relay",
            obj(json!({})),
            &[("in", &art1)],
        )
        .await
        .unwrap();
    let req1 = within("request 1", server.event()).await;
    let delivery1 = req1.body["deliveryId"].as_str().unwrap().to_owned();
    let att1 = req1.attachments[0].clone();
    assert_eq!(
        code(
            &server
                .call("delivery.consumed", json!({"deliveryIds": [delivery1]}))
                .await
        ),
        "OK"
    );
    e.settle("the request delivery is released", |s| s.owners == 1)
        .await; // only art1's own hold remains
    let refused = server
        .call_with(
            "publish",
            json!({"topic": "t.relay", "payload": {}}),
            json!([{"name": "out", "ref": att1.reference.to_json(), "ownerId": att1.owner_id}]),
        )
        .await;
    assert_eq!(
        code(&refused),
        "OWNER_INVALID",
        "a released delivery cannot source a forward"
    );
    drop(art1);

    // Round 2: forward while the request delivery is still live, then release it.
    let art2 = sealed(&caller, b"payload-2", "application/octet-stream").await;
    let _c2 = caller
        .call(
            "agent.relay",
            None,
            "Relay",
            obj(json!({})),
            &[("in", &art2)],
        )
        .await
        .unwrap();
    let req2 = within("request 2", server.event()).await;
    let delivery2 = req2.body["deliveryId"].as_str().unwrap().to_owned();
    let att2 = req2.attachments[0].clone();
    let forwarded = server
        .call_with(
            "publish",
            json!({"topic": "t.relay", "payload": {}}),
            json!([{"name": "out", "ref": att2.reference.to_json(), "ownerId": att2.owner_id}]),
        )
        .await
        .unwrap();
    assert_eq!(forwarded["subscribers"], "1");
    assert_eq!(
        code(
            &server
                .call("delivery.consumed", json!({"deliveryIds": [delivery2]}))
                .await
        ),
        "OK"
    );
    drop(art2);

    let msg = within("relayed", sub.next()).await.unwrap();
    assert_eq!(
        msg.artifact("out").unwrap().read_all().await.unwrap(),
        b"payload-2"
    );
}

// -------------------------------------------------------------------------------------------
// Failed admission rollback (bus-v1 section 6/7: rejection establishes no root)

async fn rejected_publish_creates_no_roots(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    let r = e.client("r").await;
    p.declare_topic("t.rollback", Retained::None).await.unwrap();
    let mut sub = r
        .subscribe(
            "t.rollback",
            SubscriptionConfig::bounded().queued(1).in_flight(1),
        )
        .await
        .unwrap();
    p.publish("t.rollback", obj(json!({})), &[]).await.unwrap();
    let held = within("first delivered", sub.next()).await.unwrap(); // consumes the one in-flight credit
    p.publish("t.rollback", obj(json!({})), &[]).await.unwrap(); // fills the one queue slot

    let art = sealed(&p, b"payload", "application/octet-stream").await;
    let before = e.stats();
    let refused = p
        .publish("t.rollback", obj(json!({})), &[("x", &art)])
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Backpressure);
    assert_eq!(
        e.stats(),
        before,
        "a refused publish must not touch any existing root, owner or byte count"
    );

    drop(held);
    p.declare_topic("t.rollback.ok", Retained::None)
        .await
        .unwrap();
    let receipt = p
        .publish("t.rollback.ok", obj(json!({})), &[("x", &art)])
        .await
        .unwrap();
    assert_eq!(
        receipt.subscribers, 0,
        "the untouched artifact is still usable after the rejection"
    );
}

async fn rejected_call_creates_no_roots(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "agent.rollback",
            ServiceConfig {
                max_queued: 1,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let _c1 = caller
        .call("agent.rollback", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let dispatched = within("dispatched", svc.next()).await.unwrap();
    let _c2 = caller
        .call("agent.rollback", None, "Work", obj(json!({})), &[])
        .await
        .unwrap(); // fills the queue

    let art = sealed(&caller, b"payload", "application/octet-stream").await;
    let before = e.stats();
    let refused = caller
        .call(
            "agent.rollback",
            None,
            "Work",
            obj(json!({})),
            &[("x", &art)],
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Backpressure);
    assert_eq!(
        e.stats(),
        before,
        "a refused call must not touch any existing root, owner or byte count"
    );
    drop(dispatched);
}

// -------------------------------------------------------------------------------------------
// Disconnect cleanup (bus-v1 section 8.4)

/// An abrupt disconnect (no `artifact.release` ever sent) must still abandon an unsealed
/// writer's staging reservation.
async fn disconnect_abandons_an_unsealed_writer(via: Via) {
    let e = env(via).await;
    {
        let mut raw = e.raw_hello("producer").await;
        let (_alloc, path) = raw.allocate(64).await;
        std::fs::write(&path, [7u8; 64]).unwrap();
        e.settle("writer visible", |s| {
            s.artifacts == 1 && s.store_bytes == 64
        })
        .await;
        // `raw` drops here without ever releasing anything.
    }
    e.settle("disconnect abandoned the writer", |s| {
        s.artifacts == 0 && s.store_bytes == 0 && s.owners == 0
    })
    .await;
    assert_eq!(e.files("staging"), 0);
}

/// An abrupt disconnect must release an explicit hold too, when it was the object's only root.
async fn disconnect_releases_an_explicit_hold(via: Via) {
    let e = env(via).await;
    {
        let mut raw = e.raw_hello("producer").await;
        let (alloc, path) = raw.allocate(5).await;
        std::fs::write(&path, b"hello").unwrap();
        let reply = raw.seal(&alloc, Value::Null).await.unwrap();
        assert!(reply.contains_key("ref"));
    }
    e.settle("disconnect released the only hold", |s| {
        s.sealed_artifacts == 0 && s.owners == 0 && s.store_bytes == 0
    })
    .await;
    assert_eq!(e.files("sealed"), 0);
}

/// A vanished subscriber must give up both a delivery it already holds and one still queued
/// behind it, never issued to the wire.
async fn disconnect_releases_queued_and_dispatched_deliveries(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    p.declare_topic("t.gone", Retained::None).await.unwrap();
    {
        let mut raw = e.raw_hello("subscriber").await;
        assert_eq!(
            code(&raw.call("subscribe", json!({"topic": "t.gone", "mode": "bounded", "maxQueued": 4, "maxInFlight": 1, "replayLatest": false})).await),
            "OK"
        );
        let a1 = sealed(&p, b"m1", "application/octet-stream").await;
        let a2 = sealed(&p, b"m2", "application/octet-stream").await;
        p.publish("t.gone", obj(json!({})), &[("x", &a1)])
            .await
            .unwrap();
        p.publish("t.gone", obj(json!({})), &[("x", &a2)])
            .await
            .unwrap();
        drop((a1, a2));
        let delivered = within("dispatched delivery", raw.event()).await;
        assert_eq!(delivered.op, "topic.message");
        e.settle("one dispatched, one still queued", |s| {
            s.artifact_roots == 2 && s.sealed_artifacts == 2
        })
        .await;
        // `raw` drops here without ever consuming the dispatched delivery.
    }
    e.settle("disconnect released both", |s| {
        s.artifact_roots == 0 && s.owners == 0 && s.sealed_artifacts == 0 && s.store_bytes == 0
    })
    .await;
}

// -------------------------------------------------------------------------------------------
// Stale references, stale generations and forged/borrowed owners

async fn stale_store_incarnation_and_generation_are_rejected(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("c").await;
    let (alloc, path) = raw.allocate(4).await;
    std::fs::write(&path, b"data").unwrap();
    let sealed_reply = raw.seal(&alloc, Value::Null).await.unwrap();

    let mut bad_store_ref = sealed_reply["ref"].clone();
    bad_store_ref["storeId"] = json!("store-does-not-exist");
    let r = raw
        .call(
            "artifact.open",
            json!({"ref": bad_store_ref, "ownerId": sealed_reply["ownerId"]}),
        )
        .await;
    assert_eq!(
        code(&r),
        "ARTIFACT_GONE",
        "a reference naming another store incarnation is stale"
    );

    let (alloc2, path2) = raw.allocate(4).await;
    std::fs::write(&path2, b"more!").unwrap();
    let bad_generation = raw
        .call("artifact.seal", json!({"artifactId": alloc2["artifactId"], "generation": "2", "ownerId": alloc2["ownerId"], "digest": Value::Null}))
        .await;
    assert_eq!(
        code(&bad_generation),
        "ARTIFACT_GONE",
        "v1 has exactly one generation"
    );
}

async fn owner_ids_are_scoped_to_their_connection(via: Via) {
    let e = env(via).await;
    let mut a = e.raw_hello("a").await;
    let mut b = e.raw_hello("b").await;
    let (alloc, path) = a.allocate(4).await;
    std::fs::write(&path, b"data").unwrap();
    let sealed_reply = a.seal(&alloc, Value::Null).await.unwrap();
    let (reference, owner_id) = (sealed_reply["ref"].clone(), sealed_reply["ownerId"].clone());

    // `b` was never issued this owner id; a real owner id from another connection is just as
    // invalid as one that was never issued at all.
    assert_eq!(
        code(
            &b.call(
                "artifact.open",
                json!({"ref": reference, "ownerId": owner_id})
            )
            .await
        ),
        "OWNER_INVALID"
    );
    assert_eq!(
        code(
            &b.call(
                "artifact.retain",
                json!({"ref": reference, "ownerId": owner_id})
            )
            .await
        ),
        "OWNER_INVALID"
    );

    // The writer's own owner id cannot open before sealing.
    let (alloc2, path2) = a.allocate(4).await;
    std::fs::write(&path2, b"data").unwrap();
    let unsealed_ref = json!({
        "storeId": alloc2["writeLocation"]["storeId"], "artifactId": alloc2["artifactId"], "generation": "1",
        "byteLength": "4", "contentType": "application/octet-stream", "digest": Value::Null,
    });
    assert_eq!(
        code(
            &a.call(
                "artifact.open",
                json!({"ref": unsealed_ref, "ownerId": alloc2["ownerId"]})
            )
            .await
        ),
        "ARTIFACT_UNSEALED"
    );

    // A malformed owner id is refused outright, not treated as an unknown serial.
    assert_eq!(
        code(
            &a.call(
                "artifact.open",
                json!({"ref": reference, "ownerId": "not-an-owner"})
            )
            .await
        ),
        "OWNER_INVALID"
    );
}

// -------------------------------------------------------------------------------------------
// Bounds, quotas and path containment (bus-v1 section 4/9)

async fn artifact_bounds_and_owner_budget_are_enforced(via: Via) {
    let limits = Limits {
        max_artifact_bytes: 100,
        max_store_bytes: 160,
        max_owners_per_client: 4,
        reserved_owners_per_client: 1,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let c = e.client("c").await;

    let too_big = c
        .artifacts()
        .allocate(101, "application/octet-stream")
        .await
        .unwrap_err();
    assert_eq!(
        too_big.code,
        ErrorCode::QuotaExceeded,
        "byteLength exceeds the per-object limit"
    );

    let a = sealed(&c, &[0u8; 80], "application/octet-stream").await;
    let before = e.stats();
    let no_room = c
        .artifacts()
        .allocate(90, "application/octet-stream")
        .await
        .unwrap_err();
    assert_eq!(no_room.code, ErrorCode::QuotaExceeded, "the store is full");
    assert_eq!(
        e.stats(),
        before,
        "a rejected allocation must not charge the store"
    );

    // Owner budget: 4 total minus 1 reserved leaves room for 3 ordinary owners; `a` itself is
    // already one of them, so exactly two more retains fit.
    let h1 = a.retain().await.unwrap();
    let h2 = a.retain().await.unwrap();
    let over = a.retain().await.unwrap_err();
    assert_eq!(
        over.code,
        ErrorCode::QuotaExceeded,
        "owner budget exhausted"
    );
    drop((h1, h2, a));
}

/// Every location the router issues stays a plain relative path under the store; the router
/// never hands the client anything to escape with (store.rs enforces this on the resolving
/// side; this checks the issuing side never even offers an unsafe shape).
async fn issued_locations_are_relative_and_contained(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("c").await;
    let (alloc, path) = raw.allocate(4).await;
    let assert_safe = |loc: &Value| {
        assert_eq!(loc["storeId"].as_str().unwrap(), e.router.store_id());
        let rel = loc["relativePath"].as_str().unwrap();
        assert!(
            !rel.starts_with('/') && !rel.contains(".."),
            "{rel:?} escapes the store"
        );
        assert!(
            rel.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'.' | b'_' | b'-' | b'/')),
            "{rel:?} has an unexpected character"
        );
    };
    assert_safe(&alloc["writeLocation"]);

    std::fs::write(&path, b"data").unwrap();
    let sealed_reply = raw.seal(&alloc, Value::Null).await.unwrap();
    let open_reply = raw
        .call(
            "artifact.open",
            json!({"ref": sealed_reply["ref"], "ownerId": sealed_reply["ownerId"]}),
        )
        .await
        .unwrap();
    assert_safe(&open_reply["readLocation"]);
    assert_eq!(
        std::fs::read(raw.path(&open_reply["readLocation"])).unwrap(),
        b"data"
    );
}

both_transports!(
    allocate_write_seal_open_roundtrip_and_mismatches,
    seal_is_immutable_despite_a_stale_writable_handle,
    extracted_artifact_outlives_the_message_it_came_from,
    explicit_retain_outlives_the_original_hold,
    collection_waits_for_every_retained_owner,
    queued_deliveries_hold_roots_before_dispatch,
    latest_mode_holds_at_most_two_roots_delivered_plus_queued,
    retained_topic_value_holds_a_root_independent_of_subscribers,
    forward_requires_the_source_owner_still_live,
    rejected_publish_creates_no_roots,
    rejected_call_creates_no_roots,
    disconnect_abandons_an_unsealed_writer,
    disconnect_releases_an_explicit_hold,
    disconnect_releases_queued_and_dispatched_deliveries,
    stale_store_incarnation_and_generation_are_rejected,
    owner_ids_are_scoped_to_their_connection,
    artifact_bounds_and_owner_budget_are_enforced,
    issued_locations_are_relative_and_contained,
);
