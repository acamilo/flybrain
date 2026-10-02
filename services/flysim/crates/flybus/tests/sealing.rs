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

/// A recycled writer is a fresh allocation in content too (review N3): a smaller artifact
/// after a larger one carries none of the larger one's bytes, sealed by prefix (a sealing
/// reply) or whole (`seal()`), and the reissued staging file itself reads as zeros.
async fn a_recycled_writer_never_shows_the_previous_artifact(via: Via) {
    let e = env(via).await;
    let svc_client = e.client("svc").await;
    let mut svc = svc_client
        .register("t.frames", ServiceConfig::default())
        .await
        .unwrap();
    let caller = e.client("caller").await;
    let server = tokio::spawn(async move {
        let mut writer = svc_client
            .artifacts()
            .allocate(8192, "application/x-frame")
            .await
            .unwrap();
        // Large, then small, then smaller: each reply seals what its round wrote.
        for (round, len) in [(0xA0u8, 8000usize), (0xB1, 100), (0xC2, 3)] {
            let req = svc.next().await.unwrap();
            writer.write_all(&vec![round; len]).unwrap();
            let unsealed = writer.into_unsealed();
            let artifact = unsealed.artifact().clone();
            let out = req
                .responder()
                .reply_sealing(obj(json!({})), &[("frame", &artifact)], vec![unsealed], true)
                .await
                .unwrap();
            writer = out.writers.into_iter().next().unwrap();
        }
        // The last recycled writer, sealed whole after writing a little.
        writer.write_all(&[0xD3; 5]).unwrap();
        writer.seal().await.unwrap()
    });
    for (round, len) in [(0xA0u8, 8000usize), (0xB1, 100), (0xC2, 3)] {
        let result = within(
            "sealed reply",
            caller.call_and_wait("t.frames", None, "Frame", obj(json!({})), &[]),
        )
        .await
        .unwrap();
        let frame = result.artifact("frame").unwrap();
        assert_eq!(frame.read_all().await.unwrap(), vec![round; len], "round {round:#x}");
    }
    let whole = within("server", server).await.unwrap();
    let bytes = whole.read_all().await.unwrap();
    assert_eq!(bytes.len(), 8192);
    assert_eq!(&bytes[..5], &[0xD3; 5]);
    assert!(
        bytes[5..].iter().all(|b| *b == 0),
        "unwritten bytes of a recycled writer read as zeros, not the previous artifacts'"
    );

    // On the wire: the reissued staging file is the allocation's length, all zeros.
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await),
        "OK"
    );
    let (a, p) = raw.allocate(4096).await;
    std::fs::write(&p, vec![0xEE; 4096]).unwrap();
    let att = json!([{"name": "x", "ref": reference(&a, "4096"), "ownerId": a["ownerId"]}]);
    let v = raw
        .call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att)
        .await
        .unwrap();
    let next = v["writers"][0].as_object().unwrap().clone();
    let staging = std::fs::read(raw.path(&next["writeLocation"])).unwrap();
    assert_eq!(staging, vec![0u8; 4096]);
    // And whole-sealed untouched, it is an artifact of zeros.
    let r = raw.seal(&next, json!(null)).await.unwrap();
    let open = raw
        .call("artifact.open", json!({"ref": r["ref"], "ownerId": next["ownerId"]}))
        .await
        .unwrap();
    assert_eq!(std::fs::read(raw.path(&open["readLocation"])).unwrap(), vec![0u8; 4096]);
}

/// A rename that fails after a good copy (review N5) refuses the send and leaves no staging file:
/// neither the writer's own nor one under the recycled name.
async fn a_failed_recycle_rename_leaves_no_staging_file(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await),
        "OK"
    );
    let (a, p) = raw.allocate(8).await;
    std::fs::write(&p, b"12345678").unwrap();
    // The recycled writer is the next artifact serial; a directory in its place makes the
    // rename fail (EISDIR) once the copy has been made.
    let serial: u64 = a["artifactId"].as_str().unwrap().strip_prefix("a-").unwrap().parse().unwrap();
    let blocker = p.with_file_name(format!("a-{}", serial + 1));
    std::fs::create_dir(&blocker).unwrap();
    let att = json!([{"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]}]);
    let r = raw
        .call_with("publish", json!({"topic": "t.x", "payload": {}, "recycle": true}), att)
        .await;
    assert_eq!(code(&r), "STORE_FAILURE");
    assert!(!p.exists(), "the writer's own staging file was unlinked");
    std::fs::remove_dir(&blocker).unwrap();
    assert_eq!(code(&raw.seal(&a, json!(null)).await), "OWNER_INVALID");
    e.settle("nothing held", |s| s.artifacts == 0 && s.store_bytes == 0).await;
    e.settle_files("sealed", 0).await;
    e.settle_files("staging", 0).await;
}

/// `recycle` is refused in a body with no sealing attachment, `false` included, and one writer
/// may not be named by two attachments of the same command (review N4).
async fn recycle_and_duplicate_writer_edges_are_refused(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    assert_eq!(
        code(&raw.call("topic.declare", json!({"name": "t.x", "retained": "none"})).await),
        "OK"
    );
    let r = raw
        .call("publish", json!({"topic": "t.x", "payload": {}, "recycle": false}))
        .await;
    assert_eq!(code(&r), "INVALID_ENVELOPE");
    let (a, _) = raw.allocate(8).await;
    let att = json!([
        {"name": "x", "ref": reference(&a, "8"), "ownerId": a["ownerId"]},
        {"name": "y", "ref": reference(&a, "8"), "ownerId": a["ownerId"]},
    ]);
    let r = raw.call_with("publish", json!({"topic": "t.x", "payload": {}}), att).await;
    assert_eq!(code(&r), "INVALID_ENVELOPE");
    // Refused, the send released the writer it named, as any refused sealing send does.
    assert_eq!(code(&raw.seal(&a, json!(null)).await), "OWNER_INVALID");
    e.settle("nothing held", |s| s.artifacts == 0 && s.store_bytes == 0).await;
}

fn reference(a: &serde_json::Map<String, serde_json::Value>, len: &str) -> serde_json::Value {
    json!({"storeId": a["writeLocation"]["storeId"], "artifactId": a["artifactId"],
        "generation": "1", "byteLength": len, "contentType": "application/octet-stream",
        "digest": null})
}

both_transports!(
    a_reply_seals_and_recycles_its_writers,
    a_refused_sealing_send_releases_its_writers,
    a_recycled_writer_never_shows_the_previous_artifact,
    a_failed_recycle_rename_leaves_no_staging_file,
    recycle_and_duplicate_writer_edges_are_refused,
);
