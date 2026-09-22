//! Artifacts: allocate/seal/read, immutability against live writable handles, ownership
//! through fan-out and retention, quotas, atomic admission, watermarked releases, abandoned
//! futures, disconnects and router restarts.

mod common;

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::{Via, code, env, env_with, obj, quiet, sealed, within};
use flybus::{
    ErrorCode, Limits, Policy, Retained, Router, RouterConfig, ServiceConfig, SubscriptionConfig,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn allocate_write_seal_read(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut w = c
        .artifacts()
        .allocate(data.len() as u64, "application/octet-stream")
        .await
        .unwrap();
    assert!(
        w.write_all(&[0; 100_001]).is_err(),
        "writes past the allocation are refused"
    );
    w.write_all(&data).unwrap();
    assert_eq!(e.files("staging"), 1);
    let art = w.seal_with_digest(Some(sha(&data))).await.unwrap();
    let r = art.reference();
    assert_eq!(
        (r.generation, r.byte_length, r.digest.clone()),
        (1, 100_000, Some(sha(&data)))
    );
    assert_eq!(r.store_id, e.router.store_id());
    assert_eq!(art.read_all().await.unwrap(), data);
    assert_eq!((e.files("staging"), e.files("sealed")), (0, 1));
    let meta = std::fs::metadata(e.router.store_dir().join("sealed").join(&r.artifact_id)).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o222,
        0,
        "sealed files are read-only"
    );

    let empty = c
        .artifacts()
        .allocate(0, "text/plain")
        .await
        .unwrap()
        .seal()
        .await
        .unwrap();
    assert!(empty.read_all().await.unwrap().is_empty());
    assert_eq!(
        c.artifacts().allocate(1, "").await.unwrap_err().code,
        ErrorCode::InvalidEnvelope
    );
    drop((art, empty));
    e.settle("collected", |s| s.artifacts == 0 && s.store_bytes == 0)
        .await;
    e.settle_files("sealed", 0).await;
}

async fn unsealed_artifacts_cannot_be_used(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(
            &raw.call("topic.declare", json!({"name": "t.x", "retained": "none"}))
                .await
        ),
        "OK"
    );
    let (a, _) = raw.allocate(8).await;
    let store = a["writeLocation"]["storeId"].clone();
    let reference = json!({"storeId": store, "artifactId": a["artifactId"], "generation": "1", "byteLength": "8",
        "contentType": "application/octet-stream", "digest": null});
    let att = json!([{"name": "x", "ref": reference, "ownerId": a["ownerId"]}]);
    let r = raw
        .call_with("publish", json!({"topic": "t.x", "payload": {}}), att)
        .await;
    assert_eq!(code(&r), "ARTIFACT_UNSEALED");
    let r = raw
        .call(
            "artifact.open",
            json!({"ref": reference, "ownerId": a["ownerId"]}),
        )
        .await;
    assert_eq!(code(&r), "ARTIFACT_UNSEALED");
    let r = raw
        .call(
            "artifact.retain",
            json!({"ref": reference, "ownerId": a["ownerId"]}),
        )
        .await;
    assert_eq!(code(&r), "ARTIFACT_UNSEALED");
    assert_eq!(code(&raw.seal(&a, Value::Null).await), "OK");
    assert_eq!(
        code(&raw.seal(&a, Value::Null).await),
        "OWNER_INVALID",
        "a writer seals once"
    );
    assert_eq!(
        code(
            &raw.call(
                "artifact.allocate",
                json!({"byteLength": "01", "contentType": "x"})
            )
            .await
        ),
        "INVALID_ENVELOPE"
    );
}

/// A producer that kept (or duplicated) its writable descriptor cannot change sealed bytes.
async fn seal_is_immune_to_live_writable_handles(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    let (a, path) = raw.allocate(8).await;
    let mut kept = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    kept.write_all(b"AAAAAAAA").unwrap();
    let mut dup = kept.try_clone().unwrap();
    let sealed = raw.seal(&a, json!(sha(b"AAAAAAAA"))).await.unwrap();
    // Both descriptors still write, into an inode the store no longer uses.
    dup.seek(SeekFrom::Start(0)).unwrap();
    let _ = dup.write_all(b"BBBBBBBB");
    let _ = kept.write_all(b"CCCC");
    assert!(!path.exists(), "staging is unlinked after sealing");
    let open = raw
        .call(
            "artifact.open",
            json!({"ref": sealed["ref"], "ownerId": sealed["ownerId"]}),
        )
        .await
        .unwrap();
    let read_path = raw.path(&open["readLocation"]);
    assert_eq!(std::fs::read(&read_path).unwrap(), b"AAAAAAAA");
    assert_eq!(
        std::fs::metadata(&read_path).unwrap().permissions().mode() & 0o222,
        0
    );
}

async fn seal_checks_length_and_digest(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    let (short, path) = raw.allocate(4).await;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(2)
        .unwrap();
    assert_eq!(
        code(&raw.seal(&short, Value::Null).await),
        "ARTIFACT_MISMATCH"
    );
    let (long, path) = raw.allocate(4).await;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(6)
        .unwrap();
    assert_eq!(
        code(&raw.seal(&long, Value::Null).await),
        "ARTIFACT_MISMATCH"
    );
    let (bad, _) = raw.allocate(4).await;
    assert_eq!(
        code(&raw.seal(&bad, json!("XYZ")).await),
        "INVALID_ENVELOPE"
    );
    assert_eq!(
        code(&raw.seal(&bad, json!("0".repeat(64))).await),
        "ARTIFACT_MISMATCH"
    );
    // Failed seals clean up both the staging file and the copy.
    let s = e.settle("failed seals cleaned", |s| s.artifacts == 0).await;
    assert_eq!((s.store_bytes, s.owners), (0, 0));
    e.settle_files("staging", 0).await;
    e.settle_files("sealed", 0).await;

    let c = e.client("c").await;
    let mut w = c.artifacts().allocate(3, "text/plain").await.unwrap();
    w.write_all(b"abc").unwrap();
    let err = w.seal_with_digest(Some(sha(b"abd"))).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ArtifactMismatch);
    e.settle("sdk seal cleaned", |s| {
        s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
}

async fn quotas_are_enforced(via: Via) {
    let limits = Limits {
        max_store_bytes: 1000,
        max_artifact_bytes: 600,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let c = e.client("c").await;
    assert_eq!(
        c.artifacts().allocate(601, "x/y").await.unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
    let w = c.artifacts().allocate(600, "x/y").await.unwrap();
    assert_eq!(
        c.artifacts().allocate(401, "x/y").await.unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
    // Sealing copies, so it needs room for both; failing releases the staging object.
    assert_eq!(w.seal().await.unwrap_err().code, ErrorCode::QuotaExceeded);
    e.settle("staging released", |s| s.store_bytes == 0 && s.owners == 0)
        .await;

    let limits = Limits {
        max_store_bytes: 1200,
        max_artifact_bytes: 600,
        max_owners_per_client: 4,
        reserved_owners_per_client: 1,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let c = e.client("c").await;
    let art = c
        .artifacts()
        .allocate(600, "x/y")
        .await
        .unwrap()
        .seal()
        .await
        .unwrap();
    assert_eq!(e.stats().store_bytes, 600);
    let _w1 = c.artifacts().allocate(1, "x/y").await.unwrap();
    let _w2 = c.artifacts().allocate(1, "x/y").await.unwrap();
    assert_eq!(
        c.artifacts().allocate(1, "x/y").await.unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        art.retain().await.unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
}

async fn fan_out_shares_one_object_and_the_last_consumer_collects(via: Via) {
    let e = env(via).await;
    let camera = e.client("camera").await;
    camera
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut viewers = Vec::new();
    for i in 0..3 {
        let c = e.client(&format!("viewer-{i}")).await;
        let s = c
            .subscribe("world.demo.frame", SubscriptionConfig::bounded())
            .await
            .unwrap();
        viewers.push((c, s));
    }
    let frame: Vec<u8> = (0..640 * 480 * 4).map(|i| (i % 256) as u8).collect();
    let art = sealed(&camera, &frame, "image/x-rgba").await;
    let r = camera
        .publish(
            "world.demo.frame",
            obj(json!({"width": 640, "height": 480})),
            &[("frame", &art)],
        )
        .await
        .unwrap();
    assert_eq!(r.subscribers, 3);
    drop(art);
    let mut messages = Vec::new();
    for (_, s) in viewers.iter_mut() {
        let m = within("frame", s.next()).await.unwrap();
        assert_eq!(
            m.artifact("frame").unwrap().read_all().await.unwrap(),
            frame
        );
        messages.push(m);
    }
    let s = e
        .settle("three deliveries of one object", |s| s.artifact_roots == 3)
        .await;
    assert_eq!((s.sealed_artifacts, s.store_bytes), (1, frame.len() as u64));
    assert_eq!(e.files("sealed"), 1, "fan-out never copies bytes");
    let path = e.router.store_dir().join("sealed").join(
        &messages[0]
            .artifact("frame")
            .unwrap()
            .reference()
            .artifact_id,
    );
    for (i, m) in messages.drain(..).enumerate() {
        drop(m);
        let left = 2 - i as u64;
        e.settle("one root per consumer", |s| s.artifact_roots == left)
            .await;
        if left > 0 {
            assert!(
                path.exists(),
                "collected while {left} consumers still hold it"
            );
        }
    }
    e.settle_files("sealed", 0).await;
    e.settle("collected", |s| s.artifacts == 0 && s.store_bytes == 0)
        .await;
}

async fn extracted_artifacts_outlive_their_message(via: Via) {
    let e = env(via).await;
    let p = e.client("p").await;
    let r = e.client("r").await;
    p.declare_topic("t.hold", Retained::None).await.unwrap();
    let mut sub = r
        .subscribe("t.hold", SubscriptionConfig::bounded().in_flight(1))
        .await
        .unwrap();
    for i in 0..3 {
        let a = sealed(&p, format!("f{i}").as_bytes(), "text/plain").await;
        p.publish("t.hold", obj(json!({})), &[("f", &a)])
            .await
            .unwrap();
    }
    let m = within("first", sub.next()).await.unwrap();
    let image = m.artifact("f").unwrap();
    drop(m);
    // The extracted handle still owns the delivery, so the credit is not back yet.
    quiet("credit held by the extracted artifact", sub.next()).await;
    assert_eq!(image.read_all().await.unwrap(), b"f0");
    let file = image.open().await.unwrap();
    drop(image);
    quiet("credit held by the open file", sub.next()).await;
    drop(file);
    let m = within("second", sub.next()).await.unwrap();
    // An explicit hold outlives the delivery and does not keep its credit.
    let kept = m.artifact("f").unwrap().retain().await.unwrap();
    drop(m);
    let third = within("third", sub.next()).await.unwrap();
    assert_eq!(kept.read_all().await.unwrap(), b"f1");
    drop((third, kept));
    e.settle("collected", |s| s.artifacts == 0 && s.owners == 0)
        .await;
}

async fn failed_admission_is_atomic(via: Via) {
    let e = env(via).await;
    let reader = e.client("reader").await;
    let server = e.client("server").await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(
            &raw.call(
                "topic.declare",
                json!({"name": "t.atomic", "retained": "latest"})
            )
            .await
        ),
        "OK"
    );
    let mut sub = reader
        .subscribe("t.atomic", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let mut svc = server
        .register("example.atomic", ServiceConfig::default())
        .await
        .unwrap();
    let (a, path) = raw.allocate(3).await;
    std::fs::write(&path, b"abc").unwrap();
    let s = raw.seal(&a, Value::Null).await.unwrap();
    let good = |name: &str| json!({"name": name, "ref": s["ref"], "ownerId": s["ownerId"]});
    let before = e.stats();
    let mut wrong_len = s["ref"].clone();
    wrong_len["byteLength"] = json!("4");
    let mut other_store = s["ref"].clone();
    other_store["storeId"] = json!("store-0000000000000000");
    let cases = [
        (
            json!([good("a"), {"name": "b", "ref": s["ref"], "ownerId": "own-7"}]),
            "OWNER_INVALID",
        ),
        (
            json!([good("a"), {"name": "b", "ref": wrong_len, "ownerId": s["ownerId"]}]),
            "ARTIFACT_MISMATCH",
        ),
        (
            json!([good("a"), {"name": "b", "ref": other_store, "ownerId": s["ownerId"]}]),
            "ARTIFACT_GONE",
        ),
    ];
    for (i, (atts, want)) in cases.iter().enumerate() {
        let r = raw
            .call_with(
                "publish",
                json!({"topic": "t.atomic", "payload": {}}),
                atts.clone(),
            )
            .await;
        assert_eq!(code(&r), *want);
        let call = json!({"callId": format!("call-{}", i + 1), "target": "example.atomic", "expectedIncarnation": null, "method": "M", "payload": {}});
        let r = raw.call_with("rpc.call", call, atts.clone()).await;
        assert_eq!(
            code(&r),
            *want,
            "semantic refusal advances correlation history but preserves the refusal reason"
        );
    }
    assert_eq!(
        e.stats(),
        before,
        "refused admissions leave no roots, calls or retained values"
    );
    quiet("no partial delivery", sub.next()).await;
    quiet("no partial request", svc.next()).await;
    // Two names for one object: both are delivered, one root is held.
    let r = raw
        .call_with(
            "publish",
            json!({"topic": "t.atomic", "payload": {}}),
            json!([good("a"), good("b")]),
        )
        .await
        .unwrap();
    assert_eq!(
        r["topicSequence"], "1",
        "refused publications spent no sequence number"
    );
    let m = within("delivery", sub.next()).await.unwrap();
    assert_eq!(m.attachment_names().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(m.artifact("b").unwrap().read_all().await.unwrap(), b"abc");
    // Hold, retained value, delivery: three roots, not four.
    e.settle("one root per holder", |s| s.artifact_roots == 3)
        .await;
}

async fn release_ids_are_watermarked(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    let (a1, _) = raw.allocate(4).await;
    let (a2, _) = raw.allocate(4).await;
    let rel = |ids: Value| json!({ "ownerIds": ids });
    assert_eq!(
        code(
            &raw.call("artifact.release", rel(json!([a2["ownerId"], "own-99"])))
                .await
        ),
        "OWNER_INVALID"
    );
    assert_eq!(
        e.stats().artifacts,
        2,
        "a batch with a future id releases nothing"
    );
    assert_eq!(
        raw.call("artifact.release", rel(json!([a1["ownerId"]])))
            .await
            .unwrap()["released"],
        "1"
    );
    assert_eq!(
        raw.call("artifact.release", rel(json!([a1["ownerId"]])))
            .await
            .unwrap()["released"],
        "0"
    );
    assert_eq!(
        code(&raw.call("artifact.release", rel(json!(["dlv-1"]))).await),
        "OWNER_INVALID"
    );
    assert_eq!(
        code(&raw.call("artifact.release", rel(json!([]))).await),
        "INVALID_ENVELOPE"
    );
    let many: Vec<String> = (1..=65).map(|i| format!("own-{i}")).collect();
    assert_eq!(
        code(&raw.call("artifact.release", rel(json!(many))).await),
        "INVALID_ENVELOPE"
    );
    let consumed = |ids: Value| json!({ "deliveryIds": ids });
    assert_eq!(
        code(
            &raw.call("delivery.consumed", consumed(json!(["dlv-3"])))
                .await
        ),
        "OWNER_INVALID"
    );
    assert_eq!(
        code(
            &raw.call("delivery.consumed", consumed(json!([a2["ownerId"]])))
                .await
        ),
        "OWNER_INVALID"
    );
    assert_eq!(
        raw.call(
            "artifact.release",
            rel(json!([a2["ownerId"], a2["ownerId"]]))
        )
        .await
        .unwrap()["released"],
        "1"
    );
    e.settle("all released", |s| s.artifacts == 0 && s.store_bytes == 0)
        .await;
}

async fn owners_are_scoped_to_their_connection(via: Via) {
    let e = env(via).await;
    let owner = e.client("owner").await;
    let art = sealed(&owner, b"private", "text/plain").await;
    let reference = art.reference().to_json();
    let mut thief = e.raw_hello("thief").await;
    let open = |owner_id: &str| json!({"ref": reference, "ownerId": owner_id});
    // Naming the victim's owner id means nothing on another connection.
    assert_eq!(
        code(&thief.call("artifact.open", open(art.owner_id())).await),
        "OWNER_INVALID"
    );
    let (mine, _) = thief.allocate(1).await;
    let my_owner = mine["ownerId"].as_str().unwrap().to_owned();
    assert_eq!(
        code(&thief.call("artifact.open", open(&my_owner)).await),
        "OWNER_INVALID"
    );
    assert_eq!(
        code(&thief.call("artifact.retain", open(&my_owner)).await),
        "OWNER_INVALID"
    );
    assert_eq!(
        code(
            &thief
                .call("artifact.release", json!({"ownerIds": [art.owner_id()]}))
                .await
        ),
        "OK"
    );
    assert_eq!(
        art.read_all().await.unwrap(),
        b"private",
        "another connection cannot release it"
    );
}

async fn abandoned_futures_do_not_leak_owners(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    // Each future is polled once (its command is sent) and then dropped.
    let _ = tokio::time::timeout(Duration::ZERO, c.artifacts().allocate(1000, "x/y")).await;
    e.settle("abandoned allocation released", |s| {
        s.artifacts == 0 && s.owners == 0 && s.store_bytes == 0
    })
    .await;
    e.settle_files("staging", 0).await;

    let mut w = c.artifacts().allocate(4, "x/y").await.unwrap();
    w.write_all(b"seal").unwrap();
    let _ = tokio::time::timeout(Duration::ZERO, w.seal()).await;
    e.settle("abandoned seal released", |s| {
        s.artifacts == 0 && s.owners == 0
    })
    .await;

    let art = sealed(&c, b"keep", "x/y").await;
    let _ = tokio::time::timeout(Duration::ZERO, art.retain()).await;
    e.settle("abandoned retain released", |s| s.owners == 1)
        .await;
    drop(art);
    e.settle("all released", |s| s.artifacts == 0 && s.owners == 0)
        .await;
    assert_eq!(c.control_errors(), 0);
}

async fn writer_drop_releases_staging(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    let mut w = c.artifacts().allocate(1000, "x/y").await.unwrap();
    w.write_all(&[1; 10]).unwrap();
    assert_eq!(e.files("staging"), 1);
    drop(w);
    e.settle("staging released", |s| {
        s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
    e.settle_files("staging", 0).await;
}

async fn disconnect_releases_all_but_retained(via: Via) {
    let e = env(via).await;
    let producer = e.client("producer").await;
    let reader = e.client("reader").await;
    producer
        .declare_topic("t.keep", Retained::Latest)
        .await
        .unwrap();
    let mut sub = reader
        .subscribe("t.keep", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let a1 = sealed(&producer, b"retained", "x/y").await;
    let a2 = sealed(&producer, b"held", "x/y").await;
    let w3 = producer.artifacts().allocate(10, "x/y").await.unwrap();
    producer
        .publish("t.keep", obj(json!({})), &[("a", &a1)])
        .await
        .unwrap();
    let m = within("delivery", sub.next()).await.unwrap();
    // Close both while handles are still alive; the router releases what they owned.
    producer.close().await;
    reader.close().await;
    let s = e
        .settle("connections gone", |s| s.connections == 0 && s.owners == 0)
        .await;
    assert_eq!(
        (s.artifacts, s.artifact_roots, s.store_bytes),
        (1, 1, 8),
        "only the retained value survives"
    );
    // Handles that outlived their connection are inert.
    assert_eq!(a2.read_all().await.unwrap_err().code, ErrorCode::RouterLost);
    drop((a1, a2, w3, m, sub));
    let admin = e.client("admin").await;
    assert!(admin.clear_topic("t.keep").await.unwrap());
    e.settle("retained released", |s| {
        s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
}

async fn router_restart_invalidates_old_handles(via: Via) {
    let e = env(via).await;
    let c = e.client("c").await;
    c.declare_topic("t.r", Retained::None).await.unwrap();
    let mut sub = c
        .subscribe("t.r", SubscriptionConfig::latest())
        .await
        .unwrap();
    let art = sealed(&c, b"old", "x/y").await;
    e.router.shutdown();
    assert!(within("subscription closed", sub.next()).await.is_none());
    let err = art.read_all().await.unwrap_err();
    assert_eq!(err.code, ErrorCode::RouterLost);
    assert!(c.closed().is_some());
    assert_eq!(
        e.try_client("late").await.unwrap_err().code,
        ErrorCode::RouterLost
    );

    // A restarted router on the same root is a new store incarnation.
    let mut cfg = RouterConfig::new(e.router.store_root());
    cfg.policy = Policy::open();
    let second = Router::new(cfg).unwrap();
    assert_ne!(second.store_id(), e.router.store_id());
    let mut raw = common::Raw::over(second.connect_in_memory(), second.store_root());
    raw.hello("new").await.unwrap();
    let (mine, _) = raw.allocate(1).await;
    // An old reference is refused even alongside an owner that is live on the new router.
    let r = raw
        .call(
            "artifact.open",
            json!({"ref": art.reference().to_json(), "ownerId": mine["ownerId"]}),
        )
        .await;
    assert_eq!(code(&r), "ARTIFACT_GONE");
    let client = flybus::Client::connect(
        second.connect_in_memory(),
        flybus::ClientConfig::new("sdk", second.store_root()),
    )
    .await
    .unwrap();
    let fresh = sealed(&client, b"new", "x/y").await;
    assert_eq!(fresh.read_all().await.unwrap(), b"new");
}

both_transports!(
    allocate_write_seal_read,
    unsealed_artifacts_cannot_be_used,
    seal_is_immune_to_live_writable_handles,
    seal_checks_length_and_digest,
    quotas_are_enforced,
    fan_out_shares_one_object_and_the_last_consumer_collects,
    extracted_artifacts_outlive_their_message,
    failed_admission_is_atomic,
    release_ids_are_watermarked,
    owners_are_scoped_to_their_connection,
    abandoned_futures_do_not_leak_owners,
    writer_drop_releases_staging,
    disconnect_releases_all_but_retained,
    router_restart_invalidates_old_handles,
);
