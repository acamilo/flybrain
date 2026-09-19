//! Wire and connection: hello negotiation, strict frame and envelope validation, body errors
//! that keep the connection, envelope size limits and control-lane exhaustion.

mod common;

use std::time::Duration;

use common::{Raw, Via, code, env, env_with, obj};
use flybus::wire::{Kind, MAX_ENVELOPE_BYTES, contract_digest};
use flybus::{ErrorCode, Limits, Policy, Retained};
use serde_json::{Value, json};

async fn hello_negotiation(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    let v = raw.hello("probe").await.unwrap();
    assert_eq!(v["routerId"], e.router.router_id());
    assert_eq!(
        (v["selectedMajor"].as_u64(), v["selectedMinor"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(v["contractDigest"], contract_digest());
    assert!(v["connectionId"].as_str().unwrap().starts_with("conn-"));
    assert_eq!(
        flybus::Limits::from_json(&v["limits"]).unwrap(),
        Limits::default()
    );
    let c = e.client("sdk").await;
    assert_eq!(c.info().router_id, e.router.router_id());
    assert_ne!(c.info().connection_id, v["connectionId"]);
}

async fn expect_closed(raw: &mut Raw, want: &str) {
    let notice = raw
        .closing()
        .await
        .unwrap_or_else(|| panic!("no connection.closing notice (wanted {want})"));
    assert_eq!(notice["code"], want, "{notice:?}");
}

async fn hello_refusals(via: Via) {
    let e = env_with(
        via,
        Limits::default(),
        Policy::closed().client("known", flybus::Grants::all()),
    )
    .await;
    let mut raw = e.raw_as("known").await;
    raw.command(
        "topic.declare",
        json!({"name": "t.x", "retained": "none"}),
        json!([]),
    )
    .await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;

    let mut raw = e.raw_as("known").await;
    let r = raw
        .call(
            "bus.hello",
            json!({"clientId": "known", "clientIncarnation": "i-1", "supportedMajors": [2, 3]}),
        )
        .await;
    assert_eq!(code(&r), "VERSION_MISMATCH");
    assert!(
        raw.recv().await.is_none(),
        "refused hello closes the connection"
    );

    let mut raw = e.raw_as("known").await;
    assert_eq!(code(&raw.hello("stranger").await), "NOT_AUTHORIZED");
    assert!(raw.recv().await.is_none());

    let mut raw = e.raw_as("known").await;
    let r = raw.call("bus.hello", json!({"clientId": "known", "clientIncarnation": "i-2", "supportedMajors": [1], "admin": true})).await;
    assert_eq!(code(&r), "INVALID_ENVELOPE");

    let mut raw = e.raw_as("known").await;
    raw.hello("known").await.unwrap();
    raw.command(
        "bus.hello",
        json!({"clientId": "known", "clientIncarnation": "i-3", "supportedMajors": [1]}),
        json!([]),
    )
    .await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;
    e.settle("all refused connections released", |s| s.connections == 0)
        .await;
}

fn envelope(id: &str, body: Value) -> Value {
    json!({"protocol": "flybus", "major": 1, "minor": 0, "id": id, "replyTo": null, "kind": "command",
        "op": "publish", "body": body, "attachments": []})
}

async fn malformed_frames_close_the_connection(via: Via) {
    let e = env(via).await;
    let admin = e.client("admin").await;
    admin.declare_topic("t.x", Retained::None).await.unwrap();
    let publish = |id: &str| envelope(id, json!({"topic": "t.x", "payload": {}}));
    let text = |v: Value| serde_json::to_string(&v).unwrap();
    let att = |name: &str| {
        json!({"name": name, "ref": {"storeId": "s", "artifactId": "a-1", "generation": "1",
        "byteLength": "1", "contentType": "x", "digest": null}, "ownerId": "own-1"})
    };
    let mut too_many = publish("msg-2");
    too_many["attachments"] = Value::Array((0..33).map(|i| att(&format!("a{i}"))).collect());
    let mut duplicate_names = publish("msg-2");
    duplicate_names["attachments"] = json!([att("a"), att("a")]);
    let mut unknown_field = publish("msg-2");
    unknown_field["priority"] = json!(1);
    let mut from_router = publish("msg-2");
    from_router["kind"] = json!("reply");
    let mut reply_to = publish("msg-2");
    reply_to["replyTo"] = json!("msg-1");
    let mut major = publish("msg-2");
    major["major"] = json!(2);
    let mut not_flybus = publish("msg-2");
    not_flybus["protocol"] = json!("flybusx");

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("nested duplicate key", br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-2","replyTo":null,"kind":"command","op":"publish","body":{"topic":"t.x","payload":{"a":{"b":1,"b":2}}},"attachments":[]}"#.to_vec()),
        ("top-level duplicate key", br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-2","id":"msg-3","replyTo":null,"kind":"command","op":"publish","body":{"topic":"t.x","payload":{}},"attachments":[]}"#.to_vec()),
        ("NaN", br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-2","replyTo":null,"kind":"command","op":"publish","body":{"topic":"t.x","payload":{"v":NaN}},"attachments":[]}"#.to_vec()),
        ("invalid UTF-8", [&text(publish("msg-2")).into_bytes()[..60], b"\xff\xfe", &text(publish("msg-2")).into_bytes()[62..]].concat()),
        ("trailing bytes", [text(publish("msg-2")).into_bytes(), b" {}".to_vec()].concat()),
        ("not an object", b"[1,2,3]".to_vec()),
        ("non-canonical id", text(publish("msg-02")).into_bytes()),
        ("too many attachments", text(too_many).into_bytes()),
        ("duplicate attachment names", text(duplicate_names).into_bytes()),
        ("unknown envelope field", text(unknown_field).into_bytes()),
        ("reply kind from a client", text(from_router).into_bytes()),
        ("non-null replyTo", text(reply_to).into_bytes()),
        ("wrong major after hello", text(major).into_bytes()),
        ("wrong protocol", text(not_flybus).into_bytes()),
    ];
    for (i, (what, bytes)) in cases.into_iter().enumerate() {
        let mut raw = e.raw_hello(&format!("bad-{i}")).await;
        raw.send_bytes(&bytes).await;
        let notice = raw
            .closing()
            .await
            .unwrap_or_else(|| panic!("{what}: no closing notice"));
        assert_eq!(notice["code"], "INVALID_ENVELOPE", "{what}");
        e.settle(what, |s| s.connections == 1).await;
    }

    // Ids must increase.
    let mut raw = e.raw_hello("replay").await;
    raw.send_bytes(&serde_json::to_vec(&publish("msg-5")).unwrap())
        .await;
    raw.send_bytes(&serde_json::to_vec(&publish("msg-5")).unwrap())
        .await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;

    // Framing: a zero length, and a length over the limit with no body behind it.
    let mut raw = e.raw_hello("zero").await;
    raw.send_prefix(0).await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;
    let mut raw = e.raw_hello("huge").await;
    raw.send_prefix(u32::MAX).await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;
    // A frame cut short by the end of the stream just ends the connection.
    let mut raw = e.raw_hello("short").await;
    raw.send_prefix(100).await;
    drop(raw);
    e.settle("only the admin remains", |s| s.connections == 1)
        .await;
}

async fn body_errors_keep_the_connection(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("raw").await;
    let cases = [
        ("no.such.op", json!({}), json!([])),
        (
            "topic.declare",
            json!({"name": "t.a", "retained": "none", "extra": 1}),
            json!([]),
        ),
        (
            "topic.declare",
            json!({"name": "t..a", "retained": "none"}),
            json!([]),
        ),
        (
            "topic.declare",
            json!({"name": "t.a", "retained": "sometimes"}),
            json!([]),
        ),
        ("topic.declare", json!({"name": "t.a"}), json!([])),
        (
            "subscribe",
            json!({"topic": "t.a", "mode": "bounded", "maxQueued": 0, "maxInFlight": 1, "replayLatest": false}),
            json!([]),
        ),
        (
            "subscribe",
            json!({"topic": "t.a", "mode": "bounded", "maxQueued": 1.0, "maxInFlight": 1, "replayLatest": false}),
            json!([]),
        ),
        (
            "subscribe",
            json!({"topic": "t.a", "mode": "bounded", "maxQueued": 65536, "maxInFlight": 1, "replayLatest": false}),
            json!([]),
        ),
        (
            "artifact.allocate",
            json!({"byteLength": 5, "contentType": "x"}),
            json!([]),
        ),
        (
            "artifact.allocate",
            json!({"byteLength": "18446744073709551616", "contentType": "x"}),
            json!([]),
        ),
        (
            "rpc.call",
            json!({"callId": "call-1", "target": "a.b", "expectedIncarnation": null, "method": "", "payload": {}}),
            json!([]),
        ),
        (
            "rpc.call",
            json!({"callId": "call-1", "target": "a.b", "expectedIncarnation": null, "method": "M", "payload": []}),
            json!([]),
        ),
        (
            "delivery.consumed",
            json!({"deliveryIds": "dlv-1"}),
            json!([]),
        ),
        (
            "topic.declare",
            json!({"name": "t.a", "retained": "none"}),
            json!([{"name": "x", "ref": {"storeId": "s", "artifactId": "a-1", "generation": "1", "byteLength": "1", "contentType": "x", "digest": null}, "ownerId": "own-1"}]),
        ),
    ];
    for (op, body, atts) in cases {
        let r = raw.call_with(op, body.clone(), atts).await;
        assert_eq!(
            r,
            Err(("INVALID_ENVELOPE".into(), "not-dispatched".into())),
            "{op} {body}"
        );
    }
    assert_eq!(
        code(
            &raw.call("topic.declare", json!({"name": "t.a", "retained": "none"}))
                .await
        ),
        "OK"
    );
    assert_eq!(e.stats().connections, 1);
}

async fn envelope_size_limits(via: Via) {
    let e = env(via).await;
    // Exactly 65536 bytes is a frame the router reads (and answers); one more is not.
    let sized = |id: &str, total: usize| {
        let base = serde_json::to_vec(
            &json!({"protocol": "flybus", "major": 1, "minor": 0, "id": id, "replyTo": null,
            "kind": "command", "op": "no.such.op", "body": {"pad": ""}, "attachments": []}),
        )
        .unwrap();
        let pad = "p".repeat(total - base.len());
        let bytes = serde_json::to_vec(
            &json!({"protocol": "flybus", "major": 1, "minor": 0, "id": id, "replyTo": null,
            "kind": "command", "op": "no.such.op", "body": {"pad": pad}, "attachments": []}),
        )
        .unwrap();
        assert_eq!(bytes.len(), total);
        bytes
    };
    let mut raw = e.raw_hello("raw").await;
    raw.send_bytes(&sized("msg-2", MAX_ENVELOPE_BYTES)).await;
    assert_eq!(code(&raw.reply("msg-2").await), "INVALID_ENVELOPE");
    raw.send_bytes(&sized("msg-3", MAX_ENVELOPE_BYTES + 1))
        .await;
    expect_closed(&mut raw, "INVALID_ENVELOPE").await;

    // The SDK refuses to send an oversized envelope and the connection lives on.
    let c = e.client("c").await;
    c.declare_topic("t.big", Retained::None).await.unwrap();
    let huge = c
        .publish(
            "t.big",
            obj(json!({"blob": "x".repeat(MAX_ENVELOPE_BYTES)})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(huge.code, ErrorCode::InvalidEnvelope);
    // An envelope that fits but whose delivery (with its added ids) would not is refused at
    // admission rather than truncated later.
    let edge = c
        .publish("t.big", obj(json!({"blob": "x".repeat(65_300)})), &[])
        .await
        .unwrap_err();
    assert_eq!(edge.code, ErrorCode::InvalidEnvelope);
    assert!(edge.message.contains("delivery"), "{edge}");
    assert_eq!(
        c.publish("t.big", obj(json!({"blob": "x".repeat(60_000)})), &[])
            .await
            .unwrap()
            .topic_sequence,
        1
    );
}

/// A client that sends commands and never reads replies is closed once its control lane is
/// full, with a notice, instead of the router buffering without bound.
async fn control_lane_exhaustion_closes(via: Via) {
    let limits = Limits {
        max_control_frames: 4,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let mut raw = e.raw_hello("flood").await;
    let mut writer = raw.take_writer();
    let flood = tokio::spawn(async move {
        for i in 2..20_000u64 {
            let env = json!({"protocol": "flybus", "major": 1, "minor": 0, "id": format!("msg-{i}"), "replyTo": null,
                "kind": "command", "op": "no.such.op", "body": {}, "attachments": []});
            if !writer.send(&serde_json::to_vec(&env).unwrap()).await {
                break;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut replies = 0;
    let mut last = None;
    while let Some(env) = raw.recv().await {
        match env.kind {
            Kind::Reply => replies += 1,
            _ => last = Some(env),
        }
    }
    let last = last.expect("a closing notice");
    assert_eq!(
        (last.op.as_str(), last.body["code"].as_str()),
        ("connection.closing", Some("QUOTA_EXCEEDED"))
    );
    assert!(replies < 19_998, "the router stopped answering");
    flood.abort();
    e.settle("flooder released", |s| s.connections == 0).await;
}

both_transports!(
    hello_negotiation,
    hello_refusals,
    malformed_frames_close_the_connection,
    body_errors_keep_the_connection,
    envelope_size_limits,
    control_lane_exhaustion_closes,
);
