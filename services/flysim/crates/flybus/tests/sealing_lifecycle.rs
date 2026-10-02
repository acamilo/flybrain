//! Adversarial lifecycle checks of sealing sends (bus-v1 section 12, amendment 2026-10-02),
//! from the BUS-02 review: whatever refuses or interrupts a sealing send -- the command, the
//! quota, a seal, the owner budget, the sender vanishing -- leaves no bytes, artifact or store
//! file behind; a recycled staging file never aliases what was sealed from it; and every
//! fan-out delivery reads its own bytes.

mod common;

use std::os::unix::fs::MetadataExt;

use common::{Via, code, env, env_with};
use flybus::{Limits, Policy};
use serde_json::{Map, Value, json};

fn reference(a: &Map<String, Value>, len: &str) -> Value {
    json!({"storeId": a["writeLocation"]["storeId"], "artifactId": a["artifactId"],
        "generation": "1", "byteLength": len, "contentType": "application/octet-stream",
        "digest": null})
}

async fn clean(e: &common::Env, what: &str) {
    e.settle(what, |s| s.artifacts == 0 && s.store_bytes == 0).await;
    e.settle_files("sealed", 0).await;
    e.settle_files("staging", 0).await;
}

/// Refused after the seal, with recycle: nothing leaks (bytes, artifacts, files).
async fn refused_after_seal_with_recycle_leaks_nothing(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    let (a, p) = raw.allocate(300_000).await; // over INLINE_IO_BYTES: spawn_blocking path
    std::fs::write(&p, vec![7u8; 300_000]).unwrap();
    let (b, _) = raw.allocate(8).await;
    let att = json!([
        {"name": "x", "ref": reference(&a, "299999"), "ownerId": a["ownerId"]},
        {"name": "y", "ref": reference(&b, "8"), "ownerId": b["ownerId"]},
    ]);
    let r = raw
        .call_with("publish", json!({"topic": "t.nobody", "payload": {}, "recycle": true}), att)
        .await;
    assert_eq!(code(&r), "NO_TOPIC");
    clean(&e, "refused after seal").await;
}

/// Quota refusal: the writer is released and accounting returns to zero.
async fn quota_refusal_releases(via: Via) {
    let limits = Limits { max_store_bytes: 12, max_artifact_bytes: 12, max_retained_bytes: 12, ..Limits::default() };
    let e = env_with(via, limits, Policy::open()).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await), "OK");
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]}]);
    let r = raw.call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await;
    assert_eq!(code(&r), "QUOTA_EXCEEDED");
    assert_eq!(code(&raw.seal(&a, json!(null)).await), "OWNER_INVALID");
    clean(&e, "quota").await;
    // A prefix fits where the whole would not: 8 allocated + 4 sealed = 12.
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "4"), "ownerId": a["ownerId"]}]);
    let v = raw.call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await.unwrap();
    let w = v["writers"][0].as_object().unwrap().clone();
    e.settle("prefix + recycled staging", |s| s.store_bytes == 12 && s.artifacts == 2).await;
    let r = raw.call("artifact.release", json!({"ownerIds": [a["ownerId"], w["ownerId"]]})).await.unwrap();
    assert_eq!(r["released"], "2");
    clean(&e, "released").await;
}

/// The seal fails (staging truncated by the producer) on the second of two writers, the first
/// already renamed for recycling: both released, every file gone.
async fn seal_failure_mid_send_cleans_up(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await), "OK");
    let (a, _) = raw.allocate(8).await;
    let (b, pb) = raw.allocate(8).await;
    std::fs::OpenOptions::new().write(true).open(&pb).unwrap().set_len(0).unwrap();
    let att = json!([
        {"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]},
        {"name": "y", "ref": reference(&b, "8"), "ownerId": b["ownerId"]},
    ]);
    let r = raw.call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await;
    assert_eq!(code(&r), "ARTIFACT_MISMATCH");
    assert_eq!(code(&raw.seal(&a, json!(null)).await), "OWNER_INVALID");
    clean(&e, "seal failure").await;
}

/// No owner budget for the recycled writer: it is left out, its staging file goes, accounting holds.
async fn recycle_without_budget(via: Via) {
    let limits = Limits { max_owners_per_client: 65, reserved_owners_per_client: 64, ..Limits::default() };
    let e = env_with(via, limits, Policy::open()).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await), "OK");
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "5"), "ownerId": a["ownerId"]}]);
    let v = raw.call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await.unwrap();
    assert!(v.get("writers").is_none(), "no budget, no writer: {v:?}");
    e.settle("only the sealed prefix", |s| s.store_bytes == 5 && s.artifacts == 1).await;
    e.settle_files("staging", 0).await;
    let r = raw.call("artifact.release", json!({"ownerIds": [a["ownerId"]]})).await.unwrap();
    assert_eq!(r["released"], "1");
    clean(&e, "released").await;
}

/// The recycled staging file is not the sealed inode; writing it leaves the sealed bytes alone;
/// releasing the old owner twice never touches the new writer (owner ids never reused).
async fn recycled_file_never_aliases_sealed(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await), "OK");
    let (a, pa) = raw.allocate(8).await;
    std::fs::write(&pa, b"AAAAAAAA").unwrap();
    let att = json!([{"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]}]);
    let v = raw.call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await.unwrap();
    let w = v["writers"][0].as_object().unwrap().clone();
    let pw = raw.path(&w["writeLocation"]);
    let open = raw.call("artifact.open", json!({"ref": reference(&a, "8"), "ownerId": a["ownerId"]})).await.unwrap();
    let ps = raw.path(&open["readLocation"]);
    assert_ne!(std::fs::metadata(&ps).unwrap().ino(), std::fs::metadata(&pw).unwrap().ino());
    assert_eq!(std::fs::metadata(&ps).unwrap().mode() & 0o777, 0o444);
    let held = std::fs::File::open(&ps).unwrap();
    std::fs::write(&pw, b"BBBBBBBB").unwrap();
    assert_eq!(std::fs::read(&ps).unwrap(), b"AAAAAAAA");
    drop(held);
    // Release the old owner twice: the second is a no-op; the new writer still seals.
    let r = raw.call("artifact.release", json!({"ownerIds": [a["ownerId"]]})).await.unwrap();
    assert_eq!(r["released"], "1");
    let r = raw.call("artifact.release", json!({"ownerIds": [a["ownerId"]]})).await.unwrap();
    assert_eq!(r["released"], "0");
    let r = raw.seal(&w, json!(null)).await.unwrap();
    assert_eq!(r["ref"]["byteLength"], "8");
    let rd = raw.call("artifact.open", json!({"ref": r["ref"], "ownerId": w["ownerId"]})).await.unwrap();
    assert_eq!(std::fs::read(raw.path(&rd["readLocation"])).unwrap(), b"BBBBBBBB");
    let r = raw.call("artifact.release", json!({"ownerIds": [w["ownerId"]]})).await.unwrap();
    assert_eq!(r["released"], "1");
    clean(&e, "all released").await;
}

/// A client that sends a large sealing send and vanishes: nothing leaks.
async fn sender_vanishes_mid_seal(via: Via) {
    let e = env(via).await;
    let mut keep = e.raw_hello("keeper").await;
    assert_eq!(code(&keep.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await), "OK");
    for i in 0..5 {
        let mut raw = e.raw_hello(&format!("raw{i}")).await;
        let (a, p) = raw.allocate(8 << 20).await;
        std::fs::write(&p, vec![1u8; 8 << 20]).unwrap();
        let att = json!([{"name": "x", "ref": reference(&a, &(8u64 << 20).to_string()), "ownerId": a["ownerId"]}]);
        raw.command("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att).await;
        drop(raw);
        e.settle("gone", |s| s.connections == 1).await;
    }
    clean(&e, "vanished").await;
}

/// A subscriber holding a delivered artifact while the publisher recycles and republishes:
/// each delivery reads its own bytes (fan-out to two subscribers).
async fn fanout_reads_own_bytes(via: Via) {
    use std::io::Write;
    let e = env(via).await;
    let publisher = e.client("pub").await;
    let s1c = e.client("s1").await;
    let s2c = e.client("s2").await;
    publisher.declare_topic("t.f", flybus::Retained::None).await.unwrap();
    let mut s1 = s1c.subscribe("t.f", flybus::SubscriptionConfig::bounded()).await.unwrap();
    let mut s2 = s2c.subscribe("t.f", flybus::SubscriptionConfig::bounded()).await.unwrap();
    let mut w = publisher.artifacts().allocate(4096, "application/octet-stream").await.unwrap();
    for round in 0..3u8 {
        w.write_all(&[round; 100]).unwrap();
        let u = w.into_unsealed();
        let a = u.artifact().clone();
        publisher.publish("t.f", serde_json::Map::new(), &[("x", &a)]).await.unwrap();
        drop(u);
        w = publisher.artifacts().allocate(4096, "application/octet-stream").await.unwrap();
    }
    let mut got1 = Vec::new();
    let mut got2 = Vec::new();
    for _ in 0..3 {
        got1.push(s1.next().await.unwrap());
        got2.push(s2.next().await.unwrap());
    }
    for (i, (m1, m2)) in got1.iter().zip(&got2).enumerate() {
        assert_eq!(m1.artifact("x").unwrap().read_all().await.unwrap(), vec![i as u8; 100]);
        assert_eq!(m2.artifact("x").unwrap().read_all().await.unwrap(), vec![i as u8; 100]);
    }
}

both_transports!(
    refused_after_seal_with_recycle_leaks_nothing,
    quota_refusal_releases,
    seal_failure_mid_send_cleans_up,
    recycle_without_budget,
    recycled_file_never_aliases_sealed,
    sender_vanishes_mid_seal,
    fanout_reads_own_bytes,
);
