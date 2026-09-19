//! Wire and transport conformance (bus-v1 §4 and §11 acceptance test 1): bounded
//! little-endian framing, partial reads/writes, strict JSON (duplicate keys at any depth,
//! invalid UTF-8, unknown fields), malformed ids/u64s, connection negotiation, and behavioral
//! parity between the in-memory and Unix-domain transports.
//!
//! This suite speaks the raw protocol by hand rather than going through the SDK: it sends
//! frames no correct client would ever construct, so it can check what the router does with a
//! hostile or merely buggy peer, not just what a well-behaved one gets back.

mod common;

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use common::{Via, code, env, obj, within};
use flybus::Limits;
use flybus::wire::{Envelope, Kind, MAX_ENVELOPE_BYTES, contract_digest, read_frame, write_frame};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

// -------------------------------------------------------------------------------------------
// Low-level helpers: a hand-rolled connection independent of `common::Raw`, for the framing
// tests that need control over individual bytes and reads that `Raw`'s all-at-once
// `send_bytes` cannot express.

/// Forces every read through this wrapper to surface at most one byte, so a caller reading a
/// frame through it can only succeed by looping the way [`read_frame`] does — never by
/// getting lucky with one big read.
struct Trickle<R> {
    inner: R,
}

impl<R: AsyncRead + Unpin> AsyncRead for Trickle<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut one = [0u8; 1];
        let mut small = ReadBuf::new(&mut one);
        match Pin::new(&mut this.inner).poll_read(cx, &mut small) {
            Poll::Ready(Ok(())) => {
                let n = small.filled().len();
                if n > 0 {
                    buf.put_slice(&one[..n]);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

async fn send_envelope(
    wr: &mut (impl AsyncWrite + Unpin),
    id: &str,
    op: &str,
    body: Map<String, Value>,
) {
    let env = Envelope::new(id.to_owned(), Kind::Command, op, body);
    write_frame(wr, &env.encode().unwrap()).await.unwrap();
}

/// Writes the same frame [`send_envelope`] would, one byte at a time, flushing and yielding
/// between every byte.
async fn send_envelope_byte_by_byte(
    wr: &mut (impl AsyncWrite + Unpin),
    id: &str,
    op: &str,
    body: Map<String, Value>,
) {
    let env = Envelope::new(id.to_owned(), Kind::Command, op, body);
    let bytes = env.encode().unwrap();
    let len = (bytes.len() as u32).to_le_bytes();
    for byte in len.iter().chain(bytes.iter()) {
        wr.write_all(std::slice::from_ref(byte)).await.unwrap();
        wr.flush().await.unwrap();
        tokio::task::yield_now().await;
    }
}

async fn recv_envelope(rd: &mut (impl AsyncRead + Unpin)) -> Envelope {
    let bytes = within("frame", read_frame(rd))
        .await
        .unwrap()
        .expect("stream open");
    Envelope::decode(&bytes).unwrap()
}

/// A hand-rolled `bus.hello`, sent as command `msg-1`. Returns the reply's `value` object.
async fn manual_hello(
    rd: &mut (impl AsyncRead + Unpin),
    wr: &mut (impl AsyncWrite + Unpin),
    id: &str,
    slow: bool,
) -> Map<String, Value> {
    let body = obj(
        json!({"clientId": id, "clientIncarnation": format!("inc-{id}"), "supportedMajors": [1]}),
    );
    if slow {
        send_envelope_byte_by_byte(wr, "msg-1", "bus.hello", body).await;
    } else {
        send_envelope(wr, "msg-1", "bus.hello", body).await;
    }
    let reply = recv_envelope(rd).await;
    assert_eq!(reply.kind, Kind::Reply);
    assert_eq!(reply.reply_to.as_deref(), Some("msg-1"));
    assert_eq!(
        reply.body["ok"],
        json!(true),
        "hello failed: {:?}",
        reply.body
    );
    reply.body["value"].as_object().unwrap().clone()
}

// -------------------------------------------------------------------------------------------
// Bounded little-endian framing (bus-v1 §4: "Reject ... zero/oversize frames. Read length
// before allocating.")

async fn zero_length_frame_closes_the_connection(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("zerolen").await;
    raw.send_prefix(0).await;
    let body = raw
        .closing()
        .await
        .expect("a zero-length frame must close the connection with a notice");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// The length prefix is checked, and the frame refused, before any body bytes are read. We
/// announce an oversize frame and never send its body: an implementation that read the length
/// after allocating (or tried to read the body anyway) would hang here instead of refusing
/// promptly, and `closing`'s bounded wait turns that into a loud failure rather than a stall.
async fn oversize_length_prefix_is_rejected_before_reading_body(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("oversize").await;
    raw.send_prefix((MAX_ENVELOPE_BYTES as u32) + 1).await;
    let body = raw
        .closing()
        .await
        .expect("an oversize frame must be refused, not hung waiting for a body that never comes");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// A frame at exactly the envelope ceiling decodes and dispatches normally; the same shape one
/// byte past it is refused before any JSON parsing.
async fn frame_at_the_size_ceiling_is_accepted_one_byte_over_is_not(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("ceiling").await;
    // Pad an otherwise-ordinary rpc.call to an exact byte count: measure the unpadded shape
    // once, then fill the gap with a string needing no escaping, so each character costs
    // exactly one byte and the target length is hit without any search.
    let shape = |pad: usize| {
        json!({
            "protocol": "flybus", "major": 1, "minor": 0,
            "id": "msg-9", "replyTo": null, "kind": "command", "op": "rpc.call",
            "body": {
                "callId": "call-1", "target": "no.such.service", "expectedIncarnation": null,
                "method": "M", "payload": {"pad": "a".repeat(pad)},
            },
            "attachments": [],
        })
    };
    let base_len = serde_json::to_vec(&shape(0)).unwrap().len();
    let exact = serde_json::to_vec(&shape(MAX_ENVELOPE_BYTES - base_len)).unwrap();
    assert_eq!(exact.len(), MAX_ENVELOPE_BYTES);
    raw.send_bytes(&exact).await;
    let reply = raw.reply("msg-9").await;
    // Decoded and dispatched fine: an ordinary domain reply (no such service), not a refusal.
    assert_eq!(code(&reply), "NO_SERVICE");

    let mut over_raw = e.raw_hello("overceiling").await;
    let over = serde_json::to_vec(&shape(MAX_ENVELOPE_BYTES - base_len + 1)).unwrap();
    assert_eq!(over.len(), MAX_ENVELOPE_BYTES + 1);
    over_raw.send_bytes(&over).await;
    let body = over_raw
        .closing()
        .await
        .expect("a frame one byte over the ceiling must be refused, not parsed");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// bus-v1 §4 specifies "u32 little-endian" explicitly. The prefix is built by hand here,
/// independent of the library's own `to_le_bytes` call, for a frame long enough (over 255
/// bytes) that a big-endian misreading would produce a huge, unmistakably wrong length. A
/// byte-order regression shows up as a bounded timeout below, not a silent pass.
async fn length_prefix_is_little_endian(via: Via) {
    let e = env(via).await;
    let (mut rd, mut wr) = tokio::io::split(e.transport().await);
    manual_hello(&mut rd, &mut wr, "byteorder", false).await;

    let body = obj(json!({"name": "a".repeat(190), "retained": "none"}));
    let frame_env = Envelope::new("msg-2".into(), Kind::Command, "topic.declare", body);
    let bytes = frame_env.encode().unwrap();
    assert!(
        bytes.len() > 255,
        "the padding must force a non-trivial high byte in the length"
    );
    let len = bytes.len() as u32;
    let prefix = [
        (len & 0xFF) as u8,
        ((len >> 8) & 0xFF) as u8,
        ((len >> 16) & 0xFF) as u8,
        ((len >> 24) & 0xFF) as u8,
    ];
    assert_eq!(
        prefix,
        len.to_le_bytes(),
        "sanity: the hand-built prefix matches the standard LE encoding"
    );
    let mut frame = prefix.to_vec();
    frame.extend_from_slice(&bytes);
    wr.write_all(&frame).await.unwrap();
    wr.flush().await.unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), recv_envelope(&mut rd))
        .await
        .expect("the router must read the length as little-endian, not hang reinterpreting it");
    assert_eq!(reply.body["ok"], json!(true), "{:?}", reply.body);
    assert_eq!(reply.body["value"]["declared"], json!(true));
}

// -------------------------------------------------------------------------------------------
// Partial reads and writes (bus-v1 §4: "Handle partial reads/writes.")

/// A stream that ends mid-frame is a clean disconnect, not a panic and not a phantom command:
/// the router just stops reading and tears the connection down, the same as for a client that
/// vanishes between frames.
async fn truncated_frame_disconnects_cleanly(via: Via) {
    let e = env(via).await;
    {
        let mut raw = e.raw_hello("truncated").await;
        // Announce a 64-byte body, then drop the connection before sending any of it.
        raw.send_prefix(64).await;
    }
    e.settle("the truncated connection is gone", |s| s.connections == 0)
        .await;
}

/// A frame written to the router one byte at a time, with a yield between every byte, decodes
/// exactly as if it had arrived in one write.
async fn a_frame_written_one_byte_at_a_time_still_decodes(via: Via) {
    let e = env(via).await;
    let (mut rd, mut wr) = tokio::io::split(e.transport().await);
    let value = manual_hello(&mut rd, &mut wr, "trickle-writer", true).await;
    assert_eq!(value["selectedMajor"], json!(1));
    assert_eq!(value["contractDigest"], json!(contract_digest()));
}

/// The same guarantee on the reading side: a reply read one byte at a time through
/// [`read_frame`] — the exact function both the router and the client SDK use — reassembles
/// correctly.
async fn a_reply_read_one_byte_at_a_time_still_decodes(via: Via) {
    let e = env(via).await;
    let (rd, mut wr) = tokio::io::split(e.transport().await);
    let mut trickle = Trickle { inner: rd };
    let value = manual_hello(&mut trickle, &mut wr, "trickle-reader", false).await;
    assert_eq!(value["selectedMajor"], json!(1));
}

// -------------------------------------------------------------------------------------------
// Strict JSON: duplicate keys at any depth, invalid UTF-8.

/// bus-v1 §4: "Reject duplicate JSON keys" — the same rule at the top level, nested inside
/// `body`, and nested inside an array element within it.
async fn duplicate_json_keys_are_rejected_at_every_depth(via: Via) {
    let e = env(via).await;
    let cases: [&[u8]; 4] = [
        br#"{"protocol":"flybus","protocol":"flybus","major":1,"minor":0,"id":"msg-1","replyTo":null,"kind":"command","op":"bus.hello","body":{"clientId":"x","clientIncarnation":"y","supportedMajors":[1]},"attachments":[]}"#,
        br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-1","replyTo":null,"kind":"command","op":"bus.hello","body":{"clientId":"x","clientId":"z","clientIncarnation":"y","supportedMajors":[1]},"attachments":[]}"#,
        br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-1","replyTo":null,"kind":"command","op":"rpc.call","body":{"callId":"call-1","target":"a.b","expectedIncarnation":null,"method":"M","payload":{"n":{"d":1,"d":2}}},"attachments":[]}"#,
        br#"{"protocol":"flybus","major":1,"minor":0,"id":"msg-1","replyTo":null,"kind":"command","op":"rpc.call","body":{"callId":"call-1","target":"a.b","expectedIncarnation":null,"method":"M","payload":{"items":[{"x":1,"x":2}]}},"attachments":[]}"#,
    ];
    for (i, case) in cases.iter().enumerate() {
        let mut raw = e.raw().await;
        raw.send_bytes(case).await;
        let body = raw.closing().await;
        assert!(
            body.is_some(),
            "case {i}: duplicate keys must close the connection"
        );
        assert_eq!(body.unwrap()["code"], json!("INVALID_ENVELOPE"), "case {i}");
    }
}

/// Invalid UTF-8 anywhere in the frame is refused before JSON parsing starts, whether it falls
/// inside a string value, trails a complete object, or leads the buffer.
async fn invalid_utf8_is_rejected(via: Via) {
    let e = env(via).await;
    let cases: [&[u8]; 3] = [
        b"{\"protocol\":\"flybus\",\"major\":1,\"minor\":0,\"id\":\"msg-1\",\"replyTo\":null,\"kind\":\"command\",\"op\":\"bus.hello\",\"body\":{\"clientId\":\"\xff\",\"clientIncarnation\":\"y\",\"supportedMajors\":[1]},\"attachments\":[]}",
        b"{\"protocol\":\"flybus\",\"major\":1,\"minor\":0,\"id\":\"msg-1\",\"replyTo\":null,\"kind\":\"command\",\"op\":\"bus.hello\",\"body\":{\"clientId\":\"x\",\"clientIncarnation\":\"y\",\"supportedMajors\":[1]},\"attachments\":[]}\xff",
        b"\xc0\x80{\"protocol\":\"flybus\"}",
    ];
    for (i, case) in cases.iter().enumerate() {
        let mut raw = e.raw().await;
        raw.send_bytes(case).await;
        let body = raw.closing().await;
        assert!(
            body.is_some(),
            "case {i}: invalid UTF-8 must close the connection"
        );
        assert_eq!(body.unwrap()["code"], json!("INVALID_ENVELOPE"), "case {i}");
    }
}

// -------------------------------------------------------------------------------------------
// Unknown envelope fields: a transport-level refusal at the envelope's own level, an ordinary
// domain error inside an operation's body.

async fn unknown_top_level_field_closes_the_connection(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    let v = json!({
        "protocol": "flybus", "major": 1, "minor": 0,
        "id": "msg-1", "replyTo": null, "kind": "command", "op": "bus.hello",
        "body": {"clientId": "x", "clientIncarnation": "y", "supportedMajors": [1]},
        "attachments": [],
        "extra": true,
    });
    raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
    let body = raw
        .closing()
        .await
        .expect("an unknown envelope field must close the connection");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// A field the *operation* does not recognize is a normal `ok:false` reply, not a transport
/// violation: only the envelope's own shape is the wire's concern, so the connection stays
/// open and keeps working afterward.
async fn unknown_body_field_is_a_domain_error_not_a_disconnect(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("unknownfield").await;
    let bogus = json!({
        "callId": "call-1", "target": "no.such.service", "expectedIncarnation": null,
        "method": "M", "payload": {}, "bogus": true,
    });
    let reply = raw.call("rpc.call", bogus).await;
    assert_eq!(code(&reply), "INVALID_ENVELOPE");
    let ok = raw
        .call(
            "topic.declare",
            json!({"name": "still.alive", "retained": "none"}),
        )
        .await;
    assert_eq!(code(&ok), "OK");
}

// -------------------------------------------------------------------------------------------
// Malformed ids and u64s.

/// Every envelope-level scalar the wire validates before dispatch: a malformed `id`, an
/// unparseable `kind`, and an `op` outside its `[a-z.]` alphabet. Each is refused at decode,
/// before hello state or operation semantics are consulted at all.
async fn malformed_envelope_scalars_are_rejected(via: Via) {
    let e = env(via).await;
    let hello_body = json!({"clientId": "x", "clientIncarnation": "y", "supportedMajors": [1]});
    let base = |id: &str, kind: &str, op: &str| {
        json!({
            "protocol": "flybus", "major": 1, "minor": 0,
            "id": id, "replyTo": null, "kind": kind, "op": op,
            "body": hello_body, "attachments": [],
        })
    };
    let cases = [
        (base("", "command", "bus.hello"), "empty id"),
        (base("MSG-1", "command", "bus.hello"), "uppercase id"),
        (
            base("-x", "command", "bus.hello"),
            "id starting with a separator",
        ),
        (
            base(&"a".repeat(65), "command", "bus.hello"),
            "id over 64 characters",
        ),
        (base("msg-1", "bogus", "bus.hello"), "unknown kind"),
        (base("msg-1", "command", "Bus.Hello"), "uppercase op"),
        (base("msg-1", "command", "bus.hello1"), "digit in op"),
    ];
    for (v, what) in cases {
        let mut raw = e.raw().await;
        raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
        let body = raw.closing().await;
        assert!(body.is_some(), "{what}: must close the connection");
        assert_eq!(body.unwrap()["code"], json!("INVALID_ENVELOPE"), "{what}");
    }
}

async fn non_null_reply_to_on_a_command_is_rejected(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("replyto").await;
    let v = json!({
        "protocol": "flybus", "major": 1, "minor": 0,
        "id": "msg-2", "replyTo": "msg-1", "kind": "command", "op": "topic.declare",
        "body": {"name": "a.b", "retained": "none"}, "attachments": [],
    });
    raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
    let body = raw
        .closing()
        .await
        .expect("a non-null replyTo on a command must close the connection");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// `msg-<U64>` is canonical, not merely `Id`-shaped: no leading zero, no missing digits, no
/// other prefix.
async fn non_canonical_command_ids_are_rejected(via: Via) {
    let e = env(via).await;
    for bad in ["msg-01", "msg-abc", "notmsg-1", "msg--1", "msg-"] {
        let mut raw = e.raw().await;
        let v = json!({
            "protocol": "flybus", "major": 1, "minor": 0,
            "id": bad, "replyTo": null, "kind": "command", "op": "bus.hello",
            "body": {"clientId": "x", "clientIncarnation": "y", "supportedMajors": [1]},
            "attachments": [],
        });
        raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
        let body = raw.closing().await;
        assert!(
            body.is_some(),
            "{bad}: a non-canonical command id must close the connection"
        );
        assert_eq!(body.unwrap()["code"], json!("INVALID_ENVELOPE"), "{bad}");
    }
}

async fn non_increasing_command_ids_are_rejected(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("nonincreasing").await; // hello already spent msg-1
    let v = json!({
        "protocol": "flybus", "major": 1, "minor": 0,
        "id": "msg-1", "replyTo": null, "kind": "command", "op": "topic.declare",
        "body": {"name": "a.b", "retained": "none"}, "attachments": [],
    });
    raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
    let body = raw
        .closing()
        .await
        .expect("a repeated command id must close the connection");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

/// Below the envelope's own `id`, an operation's own ids (`callId`) get the same canonical-U64
/// treatment, but as an ordinary domain reply: the connection is unharmed by a bad one.
async fn malformed_call_id_is_a_domain_error(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("badcallid").await;
    for bad in ["call-01", "call-abc", "callx-1", ""] {
        let reply =
            raw.call("rpc.call", json!({"callId": bad, "target": "a.b", "expectedIncarnation": null, "method": "M", "payload": {}})).await;
        assert_eq!(code(&reply), "INVALID_ENVELOPE", "{bad:?}");
    }
}

/// U64-string fields (`ipc-v1.md` §2: `"0"` or `[1-9][0-9]*`, at most `u64::MAX`) reject a
/// leading zero, a non-digit, an empty string, whitespace, a decimal point and an overflow —
/// each as a domain error the connection survives.
async fn malformed_u64_fields_are_a_domain_error(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("badu64").await;
    for bad in ["01", "-1", "abc", "", "18446744073709551616", " 1", "1.0"] {
        let reply = raw
            .call(
                "artifact.allocate",
                json!({"byteLength": bad, "contentType": "application/octet-stream"}),
            )
            .await;
        assert_eq!(code(&reply), "INVALID_ENVELOPE", "{bad:?}");
    }
    let ok = raw
        .call(
            "artifact.allocate",
            json!({"byteLength": "1", "contentType": "application/octet-stream"}),
        )
        .await;
    assert_eq!(code(&ok), "OK");
}

// -------------------------------------------------------------------------------------------
// Negotiation (bus-v1 §4, "Connection negotiation").

async fn first_command_must_be_hello(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    raw.command(
        "service.register",
        json!({"name": "x.y", "maxQueued": 1, "maxInFlight": 1}),
        json!([]),
    )
    .await;
    let body = raw
        .closing()
        .await
        .expect("a non-hello first command must close the connection");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

async fn hello_refuses_an_unsupported_major(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    let reply = raw.call("bus.hello", json!({"clientId": "futuristic", "clientIncarnation": "inc-1", "supportedMajors": [2, 3]})).await;
    assert_eq!(code(&reply), "VERSION_MISMATCH");
    assert!(
        raw.recv().await.is_none(),
        "the connection must close right after refusing the major"
    );
}

async fn hello_refuses_attachments(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    let attachments = json!([{
        "name": "a",
        "ref": {
            "storeId": "s", "artifactId": "a-1", "generation": "1",
            "byteLength": "1", "contentType": "x", "digest": null,
        },
        "ownerId": "o",
    }]);
    let reply = raw
        .call_with(
            "bus.hello",
            json!({"clientId": "attacher", "clientIncarnation": "inc-1", "supportedMajors": [1]}),
            attachments,
        )
        .await;
    assert_eq!(code(&reply), "INVALID_ENVELOPE");
}

async fn a_second_hello_on_the_same_connection_is_rejected(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw_hello("rehello").await;
    let v = json!({
        "protocol": "flybus", "major": 1, "minor": 0,
        "id": "msg-2", "replyTo": null, "kind": "command", "op": "bus.hello",
        "body": {"clientId": "rehello", "clientIncarnation": "inc-2", "supportedMajors": [1]},
        "attachments": [],
    });
    raw.send_bytes(&serde_json::to_vec(&v).unwrap()).await;
    let body = raw
        .closing()
        .await
        .expect("a second bus.hello must close the connection");
    assert_eq!(body["code"], json!("INVALID_ENVELOPE"));
}

async fn hello_reports_the_contract_digest_and_valid_limits(via: Via) {
    let e = env(via).await;
    let mut raw = e.raw().await;
    let reply = raw.hello("reporter").await.unwrap();
    assert_eq!(reply["selectedMajor"], json!(1));
    assert_eq!(reply["selectedMinor"], json!(0));
    assert_eq!(reply["contractDigest"], json!(contract_digest()));
    assert!(reply["routerId"].as_str().unwrap().starts_with("router-"));
    assert!(reply["connectionId"].as_str().unwrap().starts_with("conn-"));
    let limits = Limits::from_json(&reply["limits"])
        .expect("the router's own limits object must round-trip");
    assert_eq!(limits, Limits::default());
}

both_transports!(
    zero_length_frame_closes_the_connection,
    oversize_length_prefix_is_rejected_before_reading_body,
    frame_at_the_size_ceiling_is_accepted_one_byte_over_is_not,
    length_prefix_is_little_endian,
    truncated_frame_disconnects_cleanly,
    a_frame_written_one_byte_at_a_time_still_decodes,
    a_reply_read_one_byte_at_a_time_still_decodes,
    duplicate_json_keys_are_rejected_at_every_depth,
    invalid_utf8_is_rejected,
    unknown_top_level_field_closes_the_connection,
    unknown_body_field_is_a_domain_error_not_a_disconnect,
    malformed_envelope_scalars_are_rejected,
    non_null_reply_to_on_a_command_is_rejected,
    non_canonical_command_ids_are_rejected,
    non_increasing_command_ids_are_rejected,
    malformed_call_id_is_a_domain_error,
    malformed_u64_fields_are_a_domain_error,
    first_command_must_be_hello,
    hello_refuses_an_unsupported_major,
    hello_refuses_attachments,
    a_second_hello_on_the_same_connection_is_rejected,
    hello_reports_the_contract_digest_and_valid_limits,
);

// -------------------------------------------------------------------------------------------
// bus-v1 §11 acceptance test 1: "In-memory transport must pass the same tests as Unix
// sockets." Every test above already runs on both (that is what `both_transports!` is for);
// this one drives the identical raw script over both side by side in a single test, so a
// transport-specific quirk in one implementation cannot hide behind "it still passes its own
// copy of the suite."

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_and_unix_negotiate_the_identical_contract() {
    let mem = env(Via::Memory).await;
    let unix = env(Via::Unix).await;
    let mut raw_mem = mem.raw().await;
    let mut raw_unix = unix.raw().await;

    let hm = raw_mem.hello("parity").await.unwrap();
    let hu = raw_unix.hello("parity").await.unwrap();
    assert_eq!(hm["selectedMajor"], hu["selectedMajor"]);
    assert_eq!(hm["selectedMinor"], hu["selectedMinor"]);
    assert_eq!(hm["contractDigest"], hu["contractDigest"]);
    assert_eq!(hm["limits"], hu["limits"]);

    let cm = raw_mem
        .call(
            "topic.declare",
            json!({"name": "parity.topic", "retained": "latest"}),
        )
        .await
        .unwrap();
    let cu = raw_unix
        .call(
            "topic.declare",
            json!({"name": "parity.topic", "retained": "latest"}),
        )
        .await
        .unwrap();
    assert_eq!(cm["declared"], cu["declared"]);
    assert!(cm["topicIncarnation"].as_str().unwrap().starts_with("top-"));
    assert!(cu["topicIncarnation"].as_str().unwrap().starts_with("top-"));

    // The identical malformed frame gets the identical transport-level refusal on both.
    let mut bad_mem = mem.raw_hello("parity-bad").await;
    let mut bad_unix = unix.raw_hello("parity-bad").await;
    let bad_frame = br#"{"a":1,"a":2}"#;
    bad_mem.send_bytes(bad_frame).await;
    bad_unix.send_bytes(bad_frame).await;
    let bm = bad_mem.closing().await.unwrap();
    let bu = bad_unix.closing().await.unwrap();
    assert_eq!(bm["code"], bu["code"]);
}
