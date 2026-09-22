mod common;

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use common::{Raw, Via, env, env_with, obj, sealed, within};
use flybus::wire::parse_serial_id;
use flybus::{
    Client, ClientConfig, Dispatch, ErrorCode, Limits, Policy, Retained, Router, RouterConfig,
    ServiceConfig, SubscriptionConfig, Transport,
};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;

struct WriteProbe<S> {
    inner: S,
    state: Arc<Mutex<ProbeState>>,
    first: Arc<Notify>,
}

#[derive(Default)]
struct ProbeState {
    armed: bool,
    released: bool,
    first_written: bool,
    writer: Option<Waker>,
    bytes: Vec<u8>,
    ops: Vec<String>,
}

impl<S> WriteProbe<S> {
    fn new(inner: S) -> (WriteProbe<S>, WriteProbeHandle) {
        let state = Arc::new(Mutex::new(ProbeState::default()));
        let first = Arc::new(Notify::new());
        (
            WriteProbe {
                inner,
                state: state.clone(),
                first: first.clone(),
            },
            WriteProbeHandle { state, first },
        )
    }
}

#[derive(Clone)]
struct WriteProbeHandle {
    state: Arc<Mutex<ProbeState>>,
    first: Arc<Notify>,
}

impl WriteProbeHandle {
    fn arm(&self) {
        let mut state = self.state.lock().unwrap();
        state.armed = true;
        state.released = false;
        state.first_written = false;
        state.writer = None;
        state.bytes.clear();
        state.ops.clear();
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.released = true;
        if let Some(waker) = state.writer.take() {
            waker.wake();
        }
    }

    async fn wait_first(&self) {
        loop {
            let notified = self.first.notified();
            if self.state.lock().unwrap().first_written {
                return;
            }
            tokio::time::timeout(Duration::from_secs(2), notified)
                .await
                .expect("router never wrote the first byte");
        }
    }

    fn ops(&self) -> Vec<String> {
        self.state.lock().unwrap().ops.clone()
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for WriteProbe<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WriteProbe<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        {
            let mut state = self.state.lock().unwrap();
            if state.armed && !state.released && state.first_written {
                state.writer = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        let limit = if self.state.lock().unwrap().armed {
            1
        } else {
            buf.len()
        };
        match Pin::new(&mut self.inner).poll_write(cx, &buf[..limit]) {
            Poll::Ready(Ok(n)) => {
                let mut state = self.state.lock().unwrap();
                state.bytes.extend_from_slice(&buf[..n]);
                while state.bytes.len() >= 4 {
                    let len = u32::from_le_bytes(state.bytes[..4].try_into().unwrap()) as usize;
                    if state.bytes.len() < len + 4 {
                        break;
                    }
                    let frame = state.bytes.drain(..len + 4).collect::<Vec<_>>();
                    let envelope = flybus::wire::Envelope::decode(&frame[4..]).unwrap();
                    state.ops.push(envelope.op);
                }
                if state.armed && !state.first_written && n > 0 {
                    state.first_written = true;
                    self.first.notify_waiters();
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct PollHold<S> {
    inner: S,
    state: Arc<(Mutex<PollHoldState>, Condvar)>,
    entered: Arc<Notify>,
}

#[derive(Default)]
struct PollHoldState {
    armed: bool,
    entered: bool,
    released: bool,
}

#[derive(Clone)]
struct PollHoldHandle {
    state: Arc<(Mutex<PollHoldState>, Condvar)>,
    entered: Arc<Notify>,
}

impl<S> PollHold<S> {
    fn new(inner: S) -> (PollHold<S>, PollHoldHandle) {
        let state = Arc::new((Mutex::new(PollHoldState::default()), Condvar::new()));
        let entered = Arc::new(Notify::new());
        (
            PollHold {
                inner,
                state: state.clone(),
                entered: entered.clone(),
            },
            PollHoldHandle { state, entered },
        )
    }
}

impl PollHoldHandle {
    fn arm(&self) {
        self.state.0.lock().unwrap().armed = true;
    }

    async fn wait_entered(&self) {
        loop {
            let notified = self.entered.notified();
            if self.state.0.lock().unwrap().entered {
                return;
            }
            tokio::time::timeout(Duration::from_secs(2), notified)
                .await
                .expect("transport poll_write was never entered");
        }
    }

    fn release(&self) {
        let mut state = self.state.0.lock().unwrap();
        state.released = true;
        self.state.1.notify_all();
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PollHold<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PollHold<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let should_hold = {
            let mut state = self.state.0.lock().unwrap();
            if state.armed && !state.entered {
                state.entered = true;
                self.entered.notify_waiters();
                true
            } else {
                false
            }
        };
        if should_hold {
            let mut state = self.state.0.lock().unwrap();
            while !state.released {
                state = self.state.1.wait(state).unwrap();
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_write(cx, &buf[..1])
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn wait_for_single_delivery_owner(router: &Router) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let stats = router.stats();
        if stats.owners == 1 && stats.artifact_roots == 1 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "source hold was not released before teardown: {stats:?}"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_last_attached_responder_settles_the_call() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.abandoned", ServiceConfig::default())
        .await
        .unwrap();
    let mut pending = caller
        .call("example.abandoned", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();

    let request = within("request", service.next()).await.unwrap();
    drop(request);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stats = e.stats();
    assert_eq!(stats.reply_capabilities, 0);
    assert_eq!(stats.owners, 0);
    assert_eq!(stats.calls, 0, "the last reply capability was released");
    assert_eq!(stats.active_calls, 0, "the caller slot was not retired");
    let error = tokio::time::timeout(Duration::from_millis(250), pending.result())
        .await
        .expect("the caller was left waiting after all reply authority was gone")
        .unwrap_err();
    assert_eq!(
        (error.code, error.dispatch),
        (ErrorCode::CallGone, Dispatch::Dispatched)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_last_attached_responder_releases_request_attachments() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.abandoned-artifact", ServiceConfig::default())
        .await
        .unwrap();
    let artifact = sealed(&caller, b"request", "application/octet-stream").await;
    let mut pending = caller
        .call(
            "example.abandoned-artifact",
            None,
            "Work",
            obj(json!({})),
            &[("data", &artifact)],
        )
        .await
        .unwrap();
    drop(artifact);

    let request = within("artifact request", service.next()).await.unwrap();
    drop(request);
    let error = within("abandoned artifact call", pending.result())
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.dispatch),
        (ErrorCode::CallGone, Dispatch::Dispatched)
    );
    e.settle("abandoned artifact released", |stats| {
        stats.calls == 0
            && stats.active_calls == 0
            && stats.reply_capabilities == 0
            && stats.owners == 0
            && stats.artifacts == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_last_responder_clone_settles_the_call() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.abandoned-clone", ServiceConfig::default())
        .await
        .unwrap();
    let mut pending = caller
        .call("example.abandoned-clone", None, "Work", obj(json!({})), &[])
        .await
        .unwrap();

    let request = within("clone request", service.next()).await.unwrap();
    let responder = request.responder();
    drop(request);
    e.settle("clone retains reply capability", |stats| {
        stats.calls == 1
            && stats.active_calls == 1
            && stats.reply_capabilities == 1
            && stats.owners == 0
    })
    .await;
    drop(responder);
    let error = within("clone release failure", pending.result())
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.dispatch),
        (ErrorCode::CallGone, Dispatch::Dispatched)
    );
    e.settle("clone release retired call", |stats| {
        stats.calls == 0 && stats.active_calls == 0 && stats.reply_capabilities == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_last_responder_clone_with_attachment_settles_the_call() {
    let e = env(Via::Memory).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut service = server
        .register("example.abandoned-clone-artifact", ServiceConfig::default())
        .await
        .unwrap();
    let artifact = sealed(&caller, b"request", "application/octet-stream").await;
    let mut pending = caller
        .call(
            "example.abandoned-clone-artifact",
            None,
            "Work",
            obj(json!({})),
            &[("data", &artifact)],
        )
        .await
        .unwrap();
    drop(artifact);

    let request = within("clone artifact request", service.next())
        .await
        .unwrap();
    let responder = request.responder();
    drop(request);
    e.settle("clone keeps only reply capability", |stats| {
        stats.calls == 1
            && stats.active_calls == 1
            && stats.reply_capabilities == 1
            && stats.owners == 0
            && stats.artifacts == 0
    })
    .await;
    drop(responder);
    let error = within("clone artifact release failure", pending.result())
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.dispatch),
        (ErrorCode::CallGone, Dispatch::Dispatched)
    );
    e.settle("clone artifact release retired call", |stats| {
        stats.calls == 0 && stats.active_calls == 0 && stats.reply_capabilities == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fair_topic_insertion_preserves_router_envelope_order() {
    let limits = Limits {
        max_control_frames: 4096,
        max_control_bytes: 16 << 20,
        ..Limits::default()
    };
    let e = env_with(Via::Memory, limits, Policy::open()).await;
    let publisher = e.client("publisher").await;
    publisher
        .declare_topic("t.order", Retained::None)
        .await
        .unwrap();

    let mut raw = e.raw_hello("reader").await;
    raw.call(
        "subscribe",
        json!({"topic": "t.order", "mode": "latest", "maxQueued": 1, "maxInFlight": 1, "replayLatest": false}),
    )
    .await
    .unwrap();
    let mut writer = raw.take_writer();
    let flood = tokio::spawn(async move {
        for n in 3..3000u64 {
            let envelope = json!({
                "protocol": "flybus", "major": 1, "minor": 0,
                "id": format!("msg-{n}"), "replyTo": null, "kind": "command",
                "op": "no.such.op", "body": {}, "attachments": []
            });
            if !writer.send(&serde_json::to_vec(&envelope).unwrap()).await {
                break;
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    publisher
        .publish("t.order", obj(json!({"ready": true})), &[])
        .await
        .unwrap();

    let mut last = 0;
    let mut saw_delivery = false;
    for _ in 0..2500 {
        let envelope = within("router frame", raw.recv()).await.unwrap();
        let serial = parse_serial_id("bus", &envelope.id).unwrap();
        assert!(
            serial > last,
            "router emitted {} after bus-{last}",
            envelope.id
        );
        last = serial;
        saw_delivery |= envelope.op == "topic.message";
        if saw_delivery && envelope.kind == flybus::wire::Kind::Reply {
            break;
        }
    }
    assert!(
        saw_delivery,
        "fair scheduling never inserted the topic message"
    );
    flood.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_accepts_fair_topic_insertion_through_saturated_control_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    config.limits.max_control_frames = 4096;
    config.limits.max_control_bytes = 16 << 20;
    let router = Router::new(config).unwrap();
    let publisher = Client::connect(
        router.connect_in_memory_as("publisher"),
        ClientConfig::new("publisher", dir.path()),
    )
    .await
    .unwrap();
    publisher
        .declare_topic("t.sdk-order", Retained::None)
        .await
        .unwrap();

    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (probed, probe) = WriteProbe::new(router_stream);
    router.serve_as(probed, "reader");
    let reader = Client::connect(
        Transport::from_stream(client_stream),
        ClientConfig::new("reader", dir.path()),
    )
    .await
    .unwrap();
    let mut subscription = reader
        .subscribe("t.sdk-order", SubscriptionConfig::latest().in_flight(1))
        .await
        .unwrap();
    probe.arm();

    let mut backlog = Vec::new();
    let completed = Arc::new(AtomicUsize::new(0));
    for n in 0..256 {
        let client = reader.clone();
        let completed = completed.clone();
        backlog.push(tokio::spawn(async move {
            let result = client
                .declare_topic(&format!("t.backlog-{n}"), Retained::None)
                .await;
            completed.fetch_add(1, Ordering::SeqCst);
            result
        }));
    }
    probe.wait_first().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while router.stats().topics < 257 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "control backlog was not admitted"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "writer was not blocked"
    );
    publisher
        .publish("t.sdk-order", obj(json!({"ready": true})), &[])
        .await
        .unwrap();
    assert_eq!(router.stats().queued, 1, "topic delivery was not queued");

    probe.release();
    let message = within("SDK fair delivery", subscription.next())
        .await
        .expect("subscription closed on router envelope ordering");
    assert_eq!(message.payload()["ready"], true);
    assert!(
        reader.closed().is_none(),
        "strict SDK rejected router output"
    );
    for task in backlog {
        task.await.unwrap().unwrap();
    }
    let ops = probe.ops();
    let topic = ops
        .iter()
        .position(|op| op == "topic.message")
        .expect("instrumented stream did not carry the topic delivery");
    assert!(
        ops[topic + 1..].iter().any(|op| op == "topic.declare"),
        "topic delivery was not fairly inserted ahead of remaining controls: {ops:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_deliver_a_frame_after_releasing_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    let router = Router::new(config).unwrap();
    let publisher = Client::connect(
        router.connect_in_memory_as("publisher"),
        ClientConfig::new("publisher", dir.path()),
    )
    .await
    .unwrap();
    publisher
        .declare_topic("t.shutdown", Retained::None)
        .await
        .unwrap();

    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (probed, probe) = WriteProbe::new(router_stream);
    router.serve_as(probed, "reader");
    let mut raw = Raw::over(Transport::from_stream(client_stream), dir.path());
    raw.hello("reader").await.unwrap();
    raw.call(
        "subscribe",
        json!({"topic": "t.shutdown", "mode": "latest", "maxQueued": 1, "maxInFlight": 1, "replayLatest": false}),
    )
    .await
    .unwrap();
    probe.arm();

    let artifact = sealed(&publisher, b"still-owned", "application/octet-stream").await;
    publisher
        .publish("t.shutdown", obj(json!({})), &[("data", &artifact)])
        .await
        .unwrap();
    drop(artifact);
    probe.wait_first().await;
    wait_for_single_delivery_owner(&router).await;
    let before = router.stats();
    assert_eq!((before.owners, before.artifact_roots), (1, 1));

    router.shutdown();
    assert_eq!(
        router.stats().owners,
        0,
        "shutdown released the selected owner"
    );
    assert_eq!(
        router.stats().artifacts,
        0,
        "shutdown collected its attachment"
    );
    assert!(
        within("truncated topic frame", raw.recv()).await.is_none(),
        "a complete frame was appended after a partial topic delivery"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_close_cancels_partial_topic_frame_before_reclaiming_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    let router = Router::new(config).unwrap();
    let publisher = Client::connect(
        router.connect_in_memory_as("publisher"),
        ClientConfig::new("publisher", dir.path()),
    )
    .await
    .unwrap();
    publisher
        .declare_topic("t.close", Retained::None)
        .await
        .unwrap();
    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (probed, probe) = WriteProbe::new(router_stream);
    router.serve_as(probed, "reader");
    let mut raw = Raw::over(Transport::from_stream(client_stream), dir.path());
    raw.hello("reader").await.unwrap();
    raw.call(
        "subscribe",
        json!({"topic": "t.close", "mode": "latest", "maxQueued": 1, "maxInFlight": 1, "replayLatest": false}),
    )
    .await
    .unwrap();
    probe.arm();

    let artifact = sealed(&publisher, b"close", "application/octet-stream").await;
    publisher
        .publish("t.close", obj(json!({})), &[("data", &artifact)])
        .await
        .unwrap();
    drop(artifact);
    probe.wait_first().await;
    wait_for_single_delivery_owner(&router).await;

    raw.next = 0;
    raw.command("topic.declare", json!({}), json!([])).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while router.stats().connections != 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "protocol close did not finish"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(router.stats().owners, 0);
    assert_eq!(router.stats().artifacts, 0);
    assert!(
        within("protocol-close truncated frame", raw.recv())
            .await
            .is_none(),
        "a final notice was appended after a partial frame"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teardown_waits_for_an_active_transport_poll_before_reclaiming() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    let router = Router::new(config).unwrap();
    let publisher = Client::connect(
        router.connect_in_memory_as("publisher"),
        ClientConfig::new("publisher", dir.path()),
    )
    .await
    .unwrap();
    publisher
        .declare_topic("t.poll-gate", Retained::None)
        .await
        .unwrap();
    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (held, hold) = PollHold::new(router_stream);
    router.serve_as(held, "reader");
    let mut raw = Raw::over(Transport::from_stream(client_stream), dir.path());
    raw.hello("reader").await.unwrap();
    raw.call(
        "subscribe",
        json!({"topic": "t.poll-gate", "mode": "latest", "maxQueued": 1, "maxInFlight": 1, "replayLatest": false}),
    )
    .await
    .unwrap();
    hold.arm();

    let artifact = sealed(&publisher, b"poll", "application/octet-stream").await;
    let sealed_path = router
        .store_dir()
        .join("sealed")
        .join(&artifact.reference().artifact_id);
    // A deliberately large delivery: the transport under the gate writes one byte per poll,
    // so a writer that resumes cannot possibly finish this frame inside the window between
    // releasing the held poll and teardown marking the stream closing. Without that, a short
    // frame sometimes completes first, which is teardown's other legal arm and would make the
    // assertions below a coin toss rather than a test of the ordering.
    publisher
        .publish(
            "t.poll-gate",
            obj(json!({"blob": "p".repeat(50_000)})),
            &[("data", &artifact)],
        )
        .await
        .unwrap();
    drop(artifact);
    hold.wait_entered().await;
    wait_for_single_delivery_owner(&router).await;

    let done = Arc::new(AtomicBool::new(false));
    let shutdown_done = done.clone();
    let shutdown_router = router.clone();
    // The thread announces itself before calling shutdown, so the assertions below need no
    // sleep: teardown cannot get past the write gate until the held poll returns, which only
    // `hold.release()` allows.
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        started_tx.send(()).expect("the test is waiting");
        shutdown_router.shutdown();
        shutdown_done.store(true, Ordering::SeqCst);
    });
    started_rx.recv().expect("shutdown thread started");
    for _ in 0..64 {
        assert!(
            !done.load(Ordering::SeqCst),
            "teardown completed while poll_write was active"
        );
        assert!(
            sealed_path.exists(),
            "artifact was reclaimed while poll_write was active"
        );
        tokio::task::yield_now().await;
    }

    hold.release();
    shutdown.join().unwrap();
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(router.stats().owners, 0);
    assert_eq!(router.stats().artifacts, 0);
    assert!(!sealed_path.exists());
    let mut ops = Vec::new();
    while let Some(envelope) = within("poll-gate close", raw.recv()).await {
        assert_ne!(
            envelope.op, "topic.message",
            "delivery completed after teardown reclaimed its owner"
        );
        ops.push(envelope.op);
    }
    // The delivery never completes, so only teardown's two shapes are legal: the frame was
    // cut short and nothing whatever follows it, or it never began and the stream is still
    // frame aligned, in which case the closing notices are all that follow.
    assert!(
        ops.is_empty()
            || ops == ["subscription.closed".to_owned(), "connection.closing".to_owned()],
        "a cut stream carries nothing more and an aligned one exactly the closing notices: \
         {ops:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_partial_rpc_request_before_reclaiming_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    let router = Router::new(config).unwrap();
    let caller = Client::connect(
        router.connect_in_memory_as("caller"),
        ClientConfig::new("caller", dir.path()),
    )
    .await
    .unwrap();
    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (probed, probe) = WriteProbe::new(router_stream);
    router.serve_as(probed, "server");
    let mut raw = Raw::over(Transport::from_stream(client_stream), dir.path());
    raw.hello("server").await.unwrap();
    raw.call(
        "service.register",
        json!({"name": "example.partial-request", "maxQueued": 1, "maxInFlight": 1}),
    )
    .await
    .unwrap();
    probe.arm();

    let artifact = sealed(&caller, b"request", "application/octet-stream").await;
    let _pending = caller
        .call(
            "example.partial-request",
            None,
            "Work",
            obj(json!({})),
            &[("data", &artifact)],
        )
        .await
        .unwrap();
    drop(artifact);
    probe.wait_first().await;
    wait_for_single_delivery_owner(&router).await;
    let before = router.stats();
    assert_eq!((before.owners, before.artifact_roots), (1, 1));

    router.shutdown();
    assert_eq!(router.stats().owners, 0);
    assert_eq!(router.stats().artifacts, 0);
    assert!(
        within("truncated request frame", raw.recv())
            .await
            .is_none(),
        "a complete frame was appended after a partial RPC request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_partial_rpc_result_before_reclaiming_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path());
    config.policy = Policy::open();
    let router = Router::new(config).unwrap();
    let server = Client::connect(
        router.connect_in_memory_as("server"),
        ClientConfig::new("server", dir.path()),
    )
    .await
    .unwrap();
    let mut service = server
        .register("example.partial-result", ServiceConfig::default())
        .await
        .unwrap();
    let (client_stream, router_stream) = tokio::io::duplex(64 * 1024);
    let (probed, probe) = WriteProbe::new(router_stream);
    router.serve_as(probed, "caller");
    let mut raw = Raw::over(Transport::from_stream(client_stream), dir.path());
    raw.hello("caller").await.unwrap();
    raw.call(
        "rpc.call",
        json!({"callId": "call-1", "target": "example.partial-result", "expectedIncarnation": null, "method": "Work", "payload": {}}),
    )
    .await
    .unwrap();
    probe.arm();
    let request = within("result request", service.next()).await.unwrap();
    let artifact = sealed(&server, b"result", "application/octet-stream").await;
    request
        .reply(obj(json!({})), &[("data", &artifact)])
        .await
        .unwrap();
    drop((artifact, request));
    probe.wait_first().await;
    wait_for_single_delivery_owner(&router).await;
    let before = router.stats();
    assert_eq!((before.owners, before.artifact_roots), (1, 1));

    router.shutdown();
    assert_eq!(router.stats().owners, 0);
    assert_eq!(router.stats().artifacts, 0);
    assert!(
        within("truncated result frame", raw.recv()).await.is_none(),
        "a complete frame was appended after a partial RPC result"
    );
}
