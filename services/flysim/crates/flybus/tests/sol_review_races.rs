mod common;

use common::{Via, env, env_with, obj, within};
use flybus::wire::{Envelope, Kind, contract_digest, read_frame, write_frame};
use flybus::{
    CancelState, Client, ClientConfig, Dispatch, ErrorCode, Limits, Policy, Retained, Router,
    RouterConfig, ServiceConfig, SubscriptionConfig, Transport,
};
use serde_json::{Map, Value, json};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;

struct WriteFailsAfterHello {
    response: Vec<u8>,
    read: usize,
    writes: usize,
}

impl AsyncRead for WriteFailsAfterHello {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read == self.response.len() {
            return Poll::Pending;
        }
        let n = buf.remaining().min(self.response.len() - self.read);
        let end = self.read + n;
        buf.put_slice(&self.response[self.read..end]);
        self.read = end;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for WriteFailsAfterHello {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.writes == 0 {
            self.writes = 1;
            Poll::Ready(Ok(buf.len()))
        } else {
            Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected")))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct WriteBlocksAsReaderFails {
    response: Vec<u8>,
    read: usize,
    hello_written: bool,
    command_write_started: bool,
    reader_waker: Option<Waker>,
    dropped: Option<oneshot::Sender<()>>,
}

impl Drop for WriteBlocksAsReaderFails {
    fn drop(&mut self) {
        if let Some(dropped) = self.dropped.take() {
            let _ = dropped.send(());
        }
    }
}

impl AsyncRead for WriteBlocksAsReaderFails {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read < self.response.len() {
            let n = buf.remaining().min(self.response.len() - self.read);
            let end = self.read + n;
            buf.put_slice(&self.response[self.read..end]);
            self.read = end;
            return Poll::Ready(Ok(()));
        }
        if self.command_write_started {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "injected reader failure",
            )));
        }
        self.reader_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for WriteBlocksAsReaderFails {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if !self.hello_written {
            self.hello_written = true;
            return Poll::Ready(Ok(buf.len()));
        }
        self.command_write_started = true;
        if let Some(waker) = self.reader_waker.take() {
            waker.wake();
        }
        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responder_survives_cancel_then_request_drop() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.race", ServiceConfig::default())
        .await
        .unwrap();
    let pending = caller
        .call("example.race", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let request = within("request", service.next()).await.unwrap();
    let responder = request.responder();

    assert_eq!(
        pending.cancel().await.unwrap(),
        CancelState::ExecutionUnknown
    );
    drop(request);
    e.settle("request consumed", |s| s.owners == 0).await;

    let routed = responder.reply(obj(json!({"done": true})), &[]).await;
    assert_eq!(routed, Ok(false), "a detached reply is still a valid reply");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_request_consumption_retires_correlation() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.leak", ServiceConfig::default())
        .await
        .unwrap();
    let mut pending = caller
        .call("example.leak", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let request = within("request", service.next()).await.unwrap();
    drop(request);
    let failure = within("last responder terminal failure", pending.result()).await;
    let failure = failure.unwrap_err();
    assert_eq!(
        (failure.code, failure.dispatch),
        (ErrorCode::CallGone, Dispatch::Dispatched)
    );
    e.settle("consumed call retired", |s| {
        s.calls == 0 && s.active_calls == 0 && s.owners == 0 && s.reply_capabilities == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_replay_obeys_bounded_queue_byte_quota() {
    let limits = Limits {
        max_queued_bytes_per_client: 1,
        max_owners_per_client: 2,
        reserved_owners_per_client: 1,
        ..Limits::default()
    };
    let e = env_with(Via::Memory, limits, Policy::open()).await;
    let publisher = e.client("publisher").await;
    let reader = e.client("reader").await;
    publisher
        .declare_topic("t.replay", Retained::Latest)
        .await
        .unwrap();
    publisher
        .publish("t.replay", obj(json!({"large": "x".repeat(1024)})), &[])
        .await
        .unwrap();

    // Exhaust the reader's ordinary-owner allowance so replay cannot immediately dispatch.
    let _hold = reader
        .artifacts()
        .allocate(0, "application/octet-stream")
        .await
        .unwrap()
        .seal()
        .await
        .unwrap();
    let replay = reader
        .subscribe("t.replay", SubscriptionConfig::bounded().replay(true))
        .await;
    assert_eq!(replay.unwrap_err().code, ErrorCode::Backpressure);
    assert_eq!(e.stats().subscriptions, 0, "failed replay is atomic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_replay_remains_bounded_outside_the_bounded_byte_pool() {
    let limits = Limits {
        max_queued_bytes_per_client: 1,
        max_owners_per_client: 2,
        reserved_owners_per_client: 1,
        ..Limits::default()
    };
    let e = env_with(Via::Memory, limits, Policy::open()).await;
    let publisher = e.client("publisher").await;
    let reader = e.client("reader").await;
    publisher
        .declare_topic("t.latest", Retained::Latest)
        .await
        .unwrap();
    publisher
        .publish("t.latest", obj(json!({"large": "x".repeat(1024)})), &[])
        .await
        .unwrap();
    let _hold = reader
        .artifacts()
        .allocate(0, "application/octet-stream")
        .await
        .unwrap()
        .seal()
        .await
        .unwrap();
    let sub = reader
        .subscribe("t.latest", SubscriptionConfig::latest().replay(true))
        .await
        .unwrap();
    assert_eq!(e.stats().subscriptions, 1);
    drop(sub);
    e.settle("latest replay released", |s| s.subscriptions == 0)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_connections_are_bounded_and_hello_expires() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.limits.max_clients = 1;
    config.hello_timeout = Duration::from_millis(40);
    config.policy = Policy::closed()
        .client("first", flybus::Grants::all())
        .client("second", flybus::Grants::all());
    let router = Router::new(config).unwrap();

    let pending = router.connect_in_memory_as("first");
    // Registration happens on the router's own task. How long that takes is this box's
    // business; that it happens is the router's.
    within("the pending connection occupies the only slot", async {
        while router.stats().connections != 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    assert_eq!(router.stats().connections, 1);
    let refused = Client::connect(
        router.connect_in_memory_as("second"),
        ClientConfig::new("second", dir.path()),
    )
    .await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::RouterLost);
    // The 40 ms hello timeout expires on the router's clock: wait for the expiry to be
    // observed rather than sleep past it and read the count once.
    within("the pending Hello expires", async {
        while router.stats().connections != 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    assert_eq!(router.stats().connections, 0, "pending Hello timed out");
    drop(pending);

    let second = Client::connect(
        router.connect_in_memory_as("second"),
        ClientConfig::new("second", dir.path()),
    )
    .await
    .unwrap();
    second.close().await;
    let unbound = Client::connect(
        router.connect_in_memory(),
        ClientConfig::new("first", dir.path()),
    )
    .await;
    assert_eq!(unbound.unwrap_err().code, ErrorCode::RouterLost);
}

fn reply_body(value: Value) -> Map<String, Value> {
    obj(json!({"ok": true, "value": value}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_rejects_wrong_version_and_operation_from_router() {
    let dir = tempfile::tempdir().unwrap();
    let (client_stream, mut fake_router) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let hello = read_frame(&mut fake_router).await.unwrap().unwrap();
        let hello = Envelope::decode(&hello).unwrap();
        let hello_reply = Envelope {
            major: 1,
            minor: 0,
            id: "bus-1".into(),
            reply_to: Some(hello.id),
            kind: Kind::Reply,
            op: "bus.hello".into(),
            body: reply_body(json!({
                "routerId": "router-fake",
                "connectionId": "conn-1",
                "selectedMajor": 1,
                "selectedMinor": 0,
                "contractDigest": contract_digest(),
                "limits": Limits::default().to_json(),
            })),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &hello_reply.encode().unwrap())
            .await
            .unwrap();

        let command = read_frame(&mut fake_router).await.unwrap().unwrap();
        let command = Envelope::decode(&command).unwrap();
        let invalid_reply = Envelope {
            major: 2,
            minor: 0,
            id: "bus-1".into(),
            reply_to: Some(command.id),
            kind: Kind::Reply,
            op: "publish".into(),
            body: reply_body(json!({
                "declared": true,
                "topicIncarnation": "top-1",
            })),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &invalid_reply.encode().unwrap())
            .await
            .unwrap();
    });

    let client = Client::connect(
        Transport::from_stream(client_stream),
        ClientConfig::new("client", dir.path()),
    )
    .await
    .unwrap();
    let result = client.declare_topic("t.x", Retained::None).await;
    assert!(
        result.is_err(),
        "invalid router envelope was accepted: {result:?}"
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_strictly_validates_hello_envelope() {
    for case in ["version", "operation", "id", "attachments"] {
        let dir = tempfile::tempdir().unwrap();
        let (client_stream, mut fake_router) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let hello =
                Envelope::decode(&read_frame(&mut fake_router).await.unwrap().unwrap()).unwrap();
            let mut reply = Envelope {
                major: 1,
                minor: 0,
                id: "bus-1".into(),
                reply_to: Some(hello.id),
                kind: Kind::Reply,
                op: "bus.hello".into(),
                body: reply_body(json!({
                    "routerId": "router-fake",
                    "connectionId": "conn-1",
                    "selectedMajor": 1,
                    "selectedMinor": 0,
                    "contractDigest": contract_digest(),
                    "limits": Limits::default().to_json(),
                })),
                attachments: Vec::new(),
            };
            match case {
                "version" => reply.major = 2,
                "operation" => reply.op = "publish".into(),
                "id" => reply.id = "msg-1".into(),
                "attachments" => reply.attachments.push(flybus::wire::Attachment {
                    name: "x".into(),
                    reference: flybus::ArtifactRef {
                        store_id: "store-fake".into(),
                        artifact_id: "a-1".into(),
                        generation: 1,
                        byte_length: 0,
                        content_type: "x/y".into(),
                        digest: None,
                    },
                    owner_id: "own-1".into(),
                }),
                _ => unreachable!(),
            }
            write_frame(&mut fake_router, &reply.encode().unwrap())
                .await
                .unwrap();
        });
        let result = Client::connect(
            Transport::from_stream(client_stream),
            ClientConfig::new("client", dir.path()),
        )
        .await;
        assert!(result.is_err(), "invalid Hello case {case} was accepted");
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_rejects_unknown_reply_fields_and_invalid_direction_rules() {
    let dir = tempfile::tempdir().unwrap();
    let (client_stream, mut fake_router) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let hello =
            Envelope::decode(&read_frame(&mut fake_router).await.unwrap().unwrap()).unwrap();
        let hello_reply = Envelope {
            major: 1,
            minor: 0,
            id: "bus-1".into(),
            reply_to: Some(hello.id),
            kind: Kind::Reply,
            op: "bus.hello".into(),
            body: reply_body(json!({
                "routerId": "router-fake",
                "connectionId": "conn-1",
                "selectedMajor": 1,
                "selectedMinor": 0,
                "contractDigest": contract_digest(),
                "limits": Limits::default().to_json(),
            })),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &hello_reply.encode().unwrap())
            .await
            .unwrap();
        let command =
            Envelope::decode(&read_frame(&mut fake_router).await.unwrap().unwrap()).unwrap();
        let invalid = Envelope {
            major: 1,
            minor: 0,
            id: "bus-2".into(),
            reply_to: Some(command.id),
            kind: Kind::Reply,
            op: "topic.declare".into(),
            body: reply_body(json!({
                "declared": true,
                "topicIncarnation": "top-1",
                "extra": true,
            })),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &invalid.encode().unwrap())
            .await
            .unwrap();
    });
    let client = Client::connect(
        Transport::from_stream(client_stream),
        ClientConfig::new("client", dir.path()),
    )
    .await
    .unwrap();
    assert!(client.declare_topic("t.x", Retained::None).await.is_err());
    server.await.unwrap();

    let (client_stream, mut fake_router) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let hello =
            Envelope::decode(&read_frame(&mut fake_router).await.unwrap().unwrap()).unwrap();
        let hello_reply = Envelope {
            major: 1,
            minor: 0,
            id: "bus-1".into(),
            reply_to: Some(hello.id),
            kind: Kind::Reply,
            op: "bus.hello".into(),
            body: reply_body(json!({
                "routerId": "router-fake",
                "connectionId": "conn-1",
                "selectedMajor": 1,
                "selectedMinor": 0,
                "contractDigest": contract_digest(),
                "limits": Limits::default().to_json(),
            })),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &hello_reply.encode().unwrap())
            .await
            .unwrap();
        let invalid_notice = Envelope {
            major: 1,
            minor: 0,
            id: "bus-2".into(),
            reply_to: Some("msg-99".into()),
            kind: Kind::Notice,
            op: "connection.closing".into(),
            body: obj(json!({"code": "ROUTER_LOST", "message": "bad direction"})),
            attachments: Vec::new(),
        };
        write_frame(&mut fake_router, &invalid_notice.encode().unwrap())
            .await
            .unwrap();
    });
    let client = Client::connect(
        Transport::from_stream(client_stream),
        ClientConfig::new("client", dir.path()),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while client.closed().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_failure_terminates_reader_and_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    let hello_reply = Envelope {
        major: 1,
        minor: 0,
        id: "bus-1".into(),
        reply_to: Some("msg-1".into()),
        kind: Kind::Reply,
        op: "bus.hello".into(),
        body: reply_body(json!({
            "routerId": "router-fake",
            "connectionId": "conn-1",
            "selectedMajor": 1,
            "selectedMinor": 0,
            "contractDigest": contract_digest(),
            "limits": Limits::default().to_json(),
        })),
        attachments: Vec::new(),
    }
    .encode()
    .unwrap();
    let mut response = (hello_reply.len() as u32).to_le_bytes().to_vec();
    response.extend(hello_reply);
    let client = Client::connect(
        Transport::from_stream(WriteFailsAfterHello {
            response,
            read: 0,
            writes: 0,
        }),
        ClientConfig::new("client", dir.path()),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        client.declare_topic("t.x", Retained::None),
    )
    .await
    .expect("writer failure left command pending");
    assert_eq!(result.unwrap_err().code, ErrorCode::RouterLost);
    tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .expect("writer failure left close waiting");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_failure_cancels_blocked_write_and_drops_transport() {
    let dir = tempfile::tempdir().unwrap();
    let hello_reply = Envelope {
        major: 1,
        minor: 0,
        id: "bus-1".into(),
        reply_to: Some("msg-1".into()),
        kind: Kind::Reply,
        op: "bus.hello".into(),
        body: reply_body(json!({
            "routerId": "router-fake",
            "connectionId": "conn-1",
            "selectedMajor": 1,
            "selectedMinor": 0,
            "contractDigest": contract_digest(),
            "limits": Limits::default().to_json(),
        })),
        attachments: Vec::new(),
    }
    .encode()
    .unwrap();
    let mut response = (hello_reply.len() as u32).to_le_bytes().to_vec();
    response.extend(hello_reply);
    let (dropped, transport_dropped) = oneshot::channel();
    let client = Client::connect(
        Transport::from_stream(WriteBlocksAsReaderFails {
            response,
            read: 0,
            hello_written: false,
            command_write_started: false,
            reader_waker: None,
            dropped: Some(dropped),
        }),
        ClientConfig::new("client", dir.path()),
    )
    .await
    .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        client.declare_topic("t.x", Retained::None),
    )
    .await
    .expect("reader failure left command pending");
    assert_eq!(result.unwrap_err().code, ErrorCode::RouterLost);
    tokio::time::timeout(Duration::from_secs(1), transport_dropped)
        .await
        .expect("reader failure left writer blocked")
        .expect("transport drop signal was discarded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_call_id_still_advances_monotonic_watermark() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let _service = server
        .register("example.ids", ServiceConfig::default())
        .await
        .unwrap();
    let mut raw = e.raw_hello("caller").await;

    let rejected = raw
        .call(
            "rpc.call",
            json!({"callId": "call-5", "target": "missing.service", "expectedIncarnation": null, "method": "M", "payload": {}}),
        )
        .await;
    assert_eq!(common::code(&rejected), "NO_SERVICE");
    let decreasing = raw
        .call(
            "rpc.call",
            json!({"callId": "call-4", "target": "example.ids", "expectedIncarnation": null, "method": "M", "payload": {}}),
        )
        .await;
    assert_eq!(common::code(&decreasing), "INVALID_ENVELOPE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsent_oversized_call_rolls_back_its_local_slot() {
    let limits = Limits {
        max_active_calls_per_client: 1,
        ..Limits::default()
    };
    let e = env_with(Via::Memory, limits, Policy::open()).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.rollback", ServiceConfig::default())
        .await
        .unwrap();
    let oversized = caller
        .call(
            "example.rollback",
            None,
            "Work",
            obj(json!({"blob": "x".repeat(70_000)})),
            &[],
        )
        .await;
    assert_eq!(oversized.unwrap_err().code, ErrorCode::InvalidEnvelope);

    let mut pending = caller
        .call("example.rollback", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let request = within("request after rollback", service.next())
        .await
        .unwrap();
    request.reply(obj(json!({"ok": true})), &[]).await.unwrap();
    assert_eq!(pending.result().await.unwrap().outcome()["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_disconnect_cleanup_works_before_and_after_consumption() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let mut service = server
        .register("example.detach", ServiceConfig::default())
        .await
        .unwrap();

    let caller = e.client("caller-a").await;
    let pending = caller
        .call("example.detach", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let request = within("request a", service.next()).await.unwrap();
    let responder = request.responder();
    drop(request);
    e.settle("request consumed with responder", |s| {
        s.owners == 0 && s.reply_capabilities == 1
    })
    .await;
    caller.close().await;
    drop(pending);
    // `Client::close` waits for this client to stop; the router marks the call detached when
    // *it* observes the disconnect, in its own connection task. Asserting the reply routing
    // before that is a race: under load the reply reaches the router first and is routed to a
    // connection that is already closing. Nothing escapes -- teardown releases those roots --
    // but `routed` is then true. The contract sentence is about a reply to an already detached
    // call, so the test waits for the teardown it is talking about.
    e.settle("caller-a disconnected", |s| s.connections == 1).await;
    assert!(!responder.reply(obj(json!({})), &[]).await.unwrap());
    drop(responder);
    e.settle("retained responder retired", |s| {
        s.calls == 0 && s.reply_capabilities == 0
    })
    .await;

    let caller = e.client("caller-b").await;
    let pending = caller
        .call("example.detach", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();
    let request = within("request b", service.next()).await.unwrap();
    drop(request);
    e.settle("capability released first", |s| s.reply_capabilities == 0)
        .await;
    caller.close().await;
    drop(pending);
    e.settle("consumed call retired on disconnect", |s| s.calls == 0)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_service_with_buffered_requests_retires_every_call() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let service = server
        .register(
            "example.buffered",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 4,
            },
        )
        .await
        .unwrap();
    let mut calls = Vec::new();
    for _ in 0..4 {
        calls.push(
            caller
                .call("example.buffered", None, "Work", obj(json!({})), &[])
                .await
                .unwrap(),
        );
    }
    e.settle("requests buffered in SDK", |s| s.reply_capabilities == 4)
        .await;
    drop(service);
    for call in &mut calls {
        let error = within("buffered call failure", call.result())
            .await
            .unwrap_err();
        assert_eq!(
            (error.code, error.dispatch),
            (ErrorCode::NoService, flybus::Dispatch::Dispatched)
        );
    }
    e.settle("buffered calls retired", |s| {
        s.calls == 0 && s.reply_capabilities == 0 && s.owners == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_racing_cancel_has_only_the_two_contract_outcomes() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.reply-race", ServiceConfig::default())
        .await
        .unwrap();
    for _ in 0..32 {
        let mut pending = caller
            .call("example.reply-race", None, "Work", obj(json!({})), &[])
            .await
            .unwrap();
        let request = within("racing request", service.next()).await.unwrap();
        let responder = request.responder();
        let (cancel, reply) = tokio::join!(
            pending.cancel(),
            responder.reply(obj(json!({"ok": true})), &[])
        );
        match (cancel.unwrap(), reply.unwrap()) {
            (CancelState::Completed, true) => {
                assert_eq!(pending.result().await.unwrap().outcome()["ok"], true);
            }
            (CancelState::ExecutionUnknown, false) => {
                assert_eq!(
                    pending.result().await.unwrap_err().code,
                    ErrorCode::CallGone
                );
            }
            other => panic!("invalid cancel/reply race outcome: {other:?}"),
        }
        drop((request, responder));
    }
    e.settle("race calls retired", |s| {
        s.calls == 0 && s.reply_capabilities == 0 && s.owners == 0
    })
    .await;
}
