//! Backpressure, cancellation and the edge's lifecycle.
//!
//! - a family whose service is full refuses with the direct API's "queue is full" 503, and the
//!   other families keep answering;
//! - a call cancelled before dispatch never reaches the loop; a caller that goes away, or a call
//!   past the edge's deadline, leaves no call on the router;
//! - the edge unbinds `:7401` when the bus goes away and binds it again when a router is back.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::{exchange, rig};
use flybus::{CancelState, Client, ClientConfig, Policy, Router, RouterConfig};
use flysim::control::{ControlRequest, unavailable};
use flysim::controlbus::{self, Family, Role, Scope};
use serde_json::json;

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_service_is_a_503_and_the_other_families_still_answer() {
    let rig = rig(|_| {}).await;
    rig.sim.hold.store(true, Ordering::SeqCst);
    let slots =
        (controlbus::SERVICE_CONFIG.max_in_flight + controlbus::SERVICE_CONFIG.max_queued) as usize;
    // The first 16 fill the service's in-flight slots and reach the (wedged) loop; only then
    // the next 16 fill its queue. (Sent as one burst, the queue bound can be met before the
    // router has dispatched the first ones: a bounded queue refuses what it cannot hold at that
    // instant, which is the point, but not what this test counts.)
    let in_flight = controlbus::SERVICE_CONFIG.max_in_flight as usize;
    let mut pauses = Vec::new();
    for batch in [in_flight, slots - in_flight] {
        for _ in 0..batch {
            let edge = rig.edge.clone();
            pauses.push(tokio::spawn(async move {
                edge.call(ControlRequest::Pause).await
            }));
        }
        until("the in-flight pauses", || rig.sim.received("pause") == in_flight).await;
    }
    let started = Instant::now();
    while (rig.bus.router.stats().calls as usize) < slots {
        if started.elapsed() > Duration::from_secs(30) {
            let mut finished = Vec::new();
            for pause in pauses.iter_mut().filter(|p| p.is_finished()) {
                finished.push(format!("{:?}", pause.await.unwrap()));
            }
            panic!(
                "not every pause admitted: {:?}, received {}, finished {finished:?}",
                rig.bus.router.stats(),
                rig.sim.received("pause")
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // One more is refused at once, in the direct API's words.
    let started = Instant::now();
    let refused = exchange(rig.via_edge(), &Method::POST, "/pause", None, &[]).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.json(), json!({"error": unavailable::QUEUE_FULL}));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a full service made the caller wait"
    );

    // Reads, sugar and chat are other services: they answer while ops is saturated.
    let status = exchange(rig.via_edge(), &Method::GET, "/status", None, &[]).await;
    assert_eq!(status.status, StatusCode::OK);
    let healthz = exchange(rig.via_edge(), &Method::GET, "/healthz", None, &[]).await;
    assert_eq!(healthz.status, StatusCode::OK);

    // A queued pause cancelled before dispatch never reaches the loop.
    let operator = Client::connect_unix(
        controlbus::socket_path(&rig.config.bus_dir, Role::Operator),
        ClientConfig::new(
            Role::Operator.participant(),
            flysim::feedbus::store_root(&rig.config.bus_dir),
        ),
    )
    .await
    .unwrap();
    // Let the loop go: the held pauses are dropped (503 "dropped"), the queued ones answered.
    rig.sim.hold.store(false, Ordering::SeqCst);
    rig.sim.release_held();
    for pause in pauses {
        let reply = pause.await.unwrap();
        // The 16 the wedged loop dropped answer "dropped"; the 16 queued ones were answered.
        assert!(
            reply.status == 200 || reply.json_error() == Some(unavailable::DROPPED),
            "{reply:?}"
        );
    }
    let answered = rig.sim.received("pause");
    assert_eq!(answered, slots);

    // Wedge it again with 16 of the operator's own in flight; the 17th waits in the queue and
    // is cancelled there.
    rig.sim.hold.store(true, Ordering::SeqCst);
    let (family, method, payload) = controlbus::encode_request(&ControlRequest::Pause);
    let service = Scope::live().service(family, "");
    let mut holders = Vec::new();
    for _ in 0..controlbus::SERVICE_CONFIG.max_in_flight {
        holders.push(
            operator
                .call(&service, None, method, payload.clone(), &[])
                .await
                .unwrap(),
        );
    }
    until("the in-flight pauses", || {
        rig.sim.received("pause") == slots + controlbus::SERVICE_CONFIG.max_in_flight as usize
    })
    .await;
    let queued = operator
        .call(&service, None, method, payload.clone(), &[])
        .await
        .unwrap();
    assert_eq!(
        queued.cancel().await.unwrap(),
        CancelState::CancelledBeforeDispatch
    );
    rig.sim.hold.store(false, Ordering::SeqCst);
    rig.sim.release_held();
    for mut holder in holders {
        let _ = holder.result().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.sim.received("pause"),
        slots + controlbus::SERVICE_CONFIG.max_in_flight as usize,
        "the cancelled pause reached the loop"
    );
    until("no calls left on the router", || {
        rig.bus.router.stats().calls == 0
    })
    .await;
}

trait ErrorText {
    fn json_error(&self) -> Option<&str>;
}

impl ErrorText for flysim::control::ControlReply {
    fn json_error(&self) -> Option<&str> {
        match &self.body {
            flysim::control::ReplyBody::Json(value) => value["error"].as_str(),
            flysim::control::ReplyBody::Text(_) => None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_goes_away_leaves_no_call_behind() {
    let rig = rig(|_| {}).await;
    rig.sim.hold.store(true, Ordering::SeqCst);
    // An HTTP client that gives up on a pause (which waits for a durable write, so has no
    // deadline of its own): axum drops the handler, the edge drops the pending call.
    let gave_up = tokio::time::timeout(
        Duration::from_millis(300),
        exchange(rig.via_edge(), &Method::POST, "/pause", None, &[]),
    )
    .await;
    assert!(gave_up.is_err());
    until("the dropped call to be dispatched", || {
        rig.sim.received("pause") == 1
    })
    .await;
    // The host still holds it (the loop has it), so the router keeps the correlation until the
    // host answers; then the detached result is discarded and nothing is left.
    rig.sim.hold.store(false, Ordering::SeqCst);
    rig.sim.release_held();
    until("no calls left on the router", || {
        rig.bus.router.stats().calls == 0
    })
    .await;
    // And the edge is fine afterwards.
    let ok = exchange(rig.via_edge(), &Method::POST, "/resume", None, &[]).await;
    assert_eq!(ok.status, StatusCode::OK);
}

/// A host that registered the services but never answers: the edge gives up at its own deadline,
/// cancels, and answers the direct API's timeout 503.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_cancels_at_its_deadline_when_the_host_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let scope = Scope::live();
    let mut config = RouterConfig::new(dir.path());
    config.policy = controlbus::policy(Policy::closed(), &scope);
    let router = Router::new(config).unwrap();
    let silent = Client::connect(
        router.connect_in_memory_as(controlbus::HOST),
        ClientConfig::new(controlbus::HOST, router.store_root()),
    )
    .await
    .unwrap();
    let name = scope.service(Family::Sugar, controlbus::LIVE_AGENT);
    let mut service = silent
        .register(&name, controlbus::SERVICE_CONFIG)
        .await
        .unwrap();
    let held = tokio::spawn(async move {
        let mut kept = Vec::new();
        while let Some(request) = service.next().await {
            kept.push(request);
        }
    });
    let edge_client = Client::connect(
        router.connect_in_memory_as(controlbus::EDGE),
        ClientConfig::new(controlbus::EDGE, router.store_root()),
    )
    .await
    .unwrap();
    let metrics = Arc::new(fly_control_edge::EdgeMetrics::default());
    let backend = fly_control_edge::BusBackend::new(edge_client, scope, Arc::clone(&metrics));
    let started = Instant::now();
    let reply = exchange(
        flysim::api::router_with(backend),
        &Method::POST,
        "/stimulate",
        Some("application/json"),
        &serde_json::to_vec(&json!({"by": "v", "source": "chat"})).unwrap(),
    )
    .await;
    let waited = started.elapsed();
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.json(), json!({"error": unavailable::TIMED_OUT}));
    let deadline = flysim::control::COMMAND_TIMEOUT + fly_control_edge::DEADLINE_MARGIN;
    assert!(
        waited >= deadline && waited < deadline + Duration::from_secs(2),
        "{waited:?}"
    );
    assert_eq!(metrics.cancelled.load(Ordering::Relaxed), 1);
    held.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_unbinds_when_the_bus_goes_and_binds_again_when_it_is_back() {
    let mut rig = rig(|_| {}).await;
    // The edge process connects as the edge participant, which the router admits once.
    rig.close_edge().await;
    let addr = common::free_port();
    let config = fly_control_edge::EdgeConfig {
        control_bind: addr,
        ..rig.config.clone()
    };
    let metrics = Arc::new(fly_control_edge::EdgeMetrics::default());
    let edge = tokio::spawn(fly_control_edge::run(config, Arc::clone(&metrics)));
    until("the edge to bind", || {
        metrics.connected.load(Ordering::Relaxed) == 1
    })
    .await;
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let served = common::raw_http(addr, request).await.unwrap();
    assert!(
        served.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&served)
    );

    // flysim stops: its router goes, and the port with it.
    rig.host.take();
    rig.bus.router.shutdown();
    until("the edge to notice", || {
        metrics.bus_lost.load(Ordering::Relaxed) == 1
    })
    .await;
    let started = Instant::now();
    while common::raw_http(addr, request).await.is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            ":7401 stayed bound"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // flysim is back: a new router on the same directory, the services registered again.
    let scope = Scope::live();
    let uses = flysim::bus::Uses {
        feed: false,
        control: true,
    };
    let bus = flysim::bus::start(&rig.config.bus_dir, uses, &scope)
        .await
        .unwrap();
    let _host = controlbus::serve(&bus.router, rig.state.clone(), &scope)
        .await
        .unwrap();
    until("the edge to bind again", || {
        metrics.connected.load(Ordering::Relaxed) == 1
    })
    .await;
    let served = common::raw_http(addr, request).await.unwrap();
    assert!(
        served.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&served)
    );
    edge.abort();
}
