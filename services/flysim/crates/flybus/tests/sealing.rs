//! Sealing sends (bus-v1 section 12, amendment 2026-10-02): a sending command that attaches its
//! sender's own writer seals it as the command is admitted, optionally recycling the staging
//! allocation as a new writer; a refused one releases every writer it named.

mod common;

use std::io::Write;

use common::{Via, code, env, obj, within};
use flybus::ServiceConfig;
use serde_json::json;

/// A reply that seals its writers: the caller reads exactly the bytes written, the responder's
/// handle is the hold its writer became, and the recycled writer serves the next reply.
async fn a_reply_seals_and_recycles_its_writers(via: Via) {
    let e = env(via).await;
    let svc_client = e.client("svc").await;
    let mut svc = svc_client
        .register("t.echo", ServiceConfig::default())
        .await
        .unwrap();
    let caller = e.client("caller").await;
    let server = tokio::spawn(async move {
        let mut writer = svc_client
            .artifacts()
            .allocate(4096, "application/x-frame")
            .await
            .unwrap();
        let mut held = Vec::new();
        for round in 0..3u8 {
            let req = svc.next().await.unwrap();
            // Fewer bytes than the allocation: the sealed artifact is the prefix written.
            let body = vec![round; 1000 + usize::from(round)];
            writer.write_all(&body).unwrap();
            assert_eq!(writer.written(), body.len() as u64);
            let unsealed = writer.into_unsealed();
            let artifact = unsealed.artifact().clone();
            let out = req
                .responder()
                .reply_sealing(obj(json!({"round": round})), &[("frame", &artifact)], vec![unsealed], true)
                .await
                .unwrap();
            assert!(out.routed);
            assert!(artifact.is_hold(), "the writer became the responder's hold");
            assert_eq!(out.writers.len(), 1, "the allocation comes back as a new writer");
            writer = out.writers.into_iter().next().unwrap();
            assert_eq!(writer.byte_length(), 4096);
            assert_eq!(writer.written(), 0);
            assert_eq!(writer.content_type(), "application/x-frame");
            held.push(artifact);
        }
        (held, writer)
    });
    for round in 0..3u8 {
        let result = within(
            "sealed reply",
            caller.call_and_wait("t.echo", None, "Echo", obj(json!({})), &[]),
        )
        .await
        .unwrap();
        let frame = result.artifact("frame").unwrap();
        let r = frame.reference();
        assert_eq!(r.byte_length, 1000 + u64::from(round));
        assert_eq!(r.content_type, "application/x-frame");
        assert_eq!(r.digest, None);
        assert_eq!(frame.read_all().await.unwrap(), vec![round; 1000 + usize::from(round)]);
    }
    let (held, writer) = within("server", server).await.unwrap();
    // The responder's holds read the same bytes as the caller's deliveries.
    for (round, artifact) in held.iter().enumerate() {
        assert_eq!(artifact.read_all().await.unwrap(), vec![round as u8; 1000 + round]);
    }
    assert_eq!(e.files("staging"), 1, "one staging file served every reply");
    drop((held, writer));
    e.settle("collected", |s| s.artifacts == 0 && s.store_bytes == 0)
        .await;
    e.settle_files("sealed", 0).await;
    e.settle_files("staging", 0).await;
}

/// A sealing attachment must describe its writer, and a refused sealing send -- refused before
/// or after the seal -- releases every writer it named.
async fn a_refused_sealing_send_releases_its_writers(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await),
        "OK"
    );
    let reference = |a: &serde_json::Map<String, serde_json::Value>, len: &str| {
        json!({"storeId": a["writeLocation"]["storeId"], "artifactId": a["artifactId"],
            "generation": "1", "byteLength": len, "contentType": "application/octet-stream",
            "digest": null})
    };

    // Longer than the allocation: refused, and the writer is gone.
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "9"), "ownerId": a["ownerId"]}]);
    let r = raw.call_with("publish", json!({"topic": "t.x", "payload": {}}), att).await;
    assert_eq!(code(&r), "ARTIFACT_MISMATCH");
    assert_eq!(code(&raw.seal(&a, json!(null)).await), "OWNER_INVALID");

    // Sealed, then the command is refused (no such topic): the seal goes with it.
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]}]);
    let r = raw.call_with("publish", json!({"topic": "t.nobody", "payload": {}}), att).await;
    assert_eq!(code(&r), "NO_TOPIC");
    let r = raw
        .call("artifact.open", json!({"ref": reference(&a, "8"), "ownerId": a["ownerId"]}))
        .await;
    assert_eq!(code(&r), "OWNER_INVALID");

    // `recycle` needs a writer to recycle, and must be a boolean.
    let r = raw
        .call("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}))
        .await;
    assert_eq!(code(&r), "INVALID_ENVELOPE");
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]}]);
    let r = raw
        .call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": 1}), att)
        .await;
    assert_eq!(code(&r), "INVALID_ENVELOPE");

    // A recycling publish returns the new writer, named by attachment.
    let (a, _) = raw.allocate(8).await;
    let att = json!([{"name": "x", "ref": reference(&a, "5"), "ownerId": a["ownerId"]}]);
    let v = raw
        .call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att)
        .await
        .unwrap();
    let writers = v["writers"].as_array().unwrap();
    assert_eq!(writers.len(), 1);
    assert_eq!(writers[0]["name"], "x");
    let next = writers[0].as_object().unwrap().clone();
    assert_ne!(next["artifactId"], a["artifactId"]);
    // The recycled writer is an ordinary writer: it seals the whole allocation.
    assert_eq!(code(&raw.seal(&next, json!(null)).await), "OK");
    e.settle("only the two seals remain", |s| s.sealed_artifacts == 2)
        .await;
}

both_transports!(
    a_reply_seals_and_recycles_its_writers,
    a_refused_sealing_send_releases_its_writers,
);
