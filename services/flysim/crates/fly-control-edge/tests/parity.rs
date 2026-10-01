//! Every `docs/control-api.md` endpoint through the edge equals direct, byte for byte.
//!
//! Each case is sent twice, to flysim's own `:7401` router (`control.via = direct`) and to the
//! same router over the bus (`fly-control-edge`'s [`BusBackend`]), and the status, every header
//! and the body bytes must be equal. The stand-in loop answers each command from what the request
//! says, so both sends get the same answer. The last test does it over real TCP sockets against
//! the running edge, where the whole HTTP response must be equal but for its `date` line.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{exchange, rig};
use serde_json::json;

/// `(method, uri, content type, body)`.
type Case = (Method, &'static str, Option<&'static str>, Vec<u8>);

fn json_case(method: Method, uri: &'static str, body: serde_json::Value) -> Case {
    (
        method,
        uri,
        Some("application/json"),
        serde_json::to_vec(&body).unwrap(),
    )
}

fn bare(method: Method, uri: &'static str) -> Case {
    (method, uri, None, Vec::new())
}

fn raw(method: Method, uri: &'static str, body: &str) -> Case {
    (
        method,
        uri,
        Some("application/json"),
        body.as_bytes().to_vec(),
    )
}

/// The cases every configuration runs: each endpoint, its validation and its refusals.
fn cases() -> Vec<Case> {
    use Method as M;
    vec![
        bare(M::GET, "/status"),
        bare(M::GET, "/status.json"),
        bare(M::GET, "/healthz"),
        bare(M::GET, "/metrics"),
        // /events: the page, its parameters and the lenient defaults.
        bare(M::GET, "/events"),
        bare(M::GET, "/events?since=3"),
        bare(M::GET, "/events?since=0&limit=2"),
        bare(M::GET, "/events?since=99"),
        bare(M::GET, "/events?since=abc&limit=abc"),
        bare(M::GET, "/events?since=&limit="),
        bare(M::GET, "/events?limit=0"),
        bare(M::GET, "/events?since=-1&limit=100000"),
        bare(M::GET, "/events?since=1&since=2"),
        bare(M::GET, "/events?unknown=1"),
        // /stimulate: accepted, both refusals, every malformed shape.
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "viewer", "source": "points"}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "viewer", "source": "chat", "durationMs": 300}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "op", "source": "operator", "durationMs": null}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "pulse", "source": "points"}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "spent", "source": "chat"}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "viewer", "source": "bridge"}),
        ),
        json_case(M::POST, "/stimulate", json!({"by": "viewer"})),
        json_case(M::POST, "/stimulate", json!({"source": "points"})),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "viewer", "source": "points", "durationMs": -5}),
        ),
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "viewer", "source": "points", "durationMs": "400"}),
        ),
        json_case(M::POST, "/stimulate", json!([1, 2, 3])),
        raw(M::POST, "/stimulate", "{not json"),
        raw(M::POST, "/stimulate", "\"a string\""),
        bare(M::POST, "/stimulate"),
        (
            M::POST,
            "/stimulate",
            Some("text/plain"),
            b"{\"by\":\"v\",\"source\":\"chat\"}".to_vec(),
        ),
        (M::POST, "/stimulate", None, vec![0xff, 0xfe, 0x00]),
        // The loop dropped the request: the 503 both sides answer.
        json_case(
            M::POST,
            "/stimulate",
            json!({"by": "drop", "source": "chat"}),
        ),
        // /reward: 403 unless enabled; its validation once it is.
        json_case(
            M::POST,
            "/reward",
            json!({"value": 0.5, "by": "op", "source": "operator"}),
        ),
        json_case(
            M::POST,
            "/reward",
            json!({"value": "x", "by": "op", "source": "operator"}),
        ),
        bare(M::POST, "/reward"),
        // /chat: accepted, both refusals, the body rules.
        json_case(
            M::POST,
            "/chat",
            json!({"by": "viewer_two", "text": "go fly go"}),
        ),
        json_case(
            M::POST,
            "/chat",
            json!({"by": "bot", "text": "hello", "bot": true}),
        ),
        json_case(
            M::POST,
            "/chat",
            json!({"by": "viewer", "text": "see http://x.example"}),
        ),
        json_case(M::POST, "/chat", json!({"by": "fast", "text": "again"})),
        json_case(
            M::POST,
            "/chat",
            json!({"by": "viewer", "text": "hi", "bot": "yes"}),
        ),
        json_case(M::POST, "/chat", json!({"by": "viewer"})),
        json_case(M::POST, "/chat", json!({"text": 5})),
        raw(M::POST, "/chat", "]"),
        bare(M::POST, "/chat"),
        // /checkpoint, /pause, /resume.
        bare(M::POST, "/checkpoint"),
        bare(M::POST, "/pause"),
        bare(M::POST, "/resume"),
        json_case(M::POST, "/pause", json!({"ignored": true})),
        // Everything that is not a route is the same 404, the wrong method on a route included.
        bare(M::GET, "/stimulate"),
        bare(M::POST, "/status"),
        bare(M::PUT, "/pause"),
        bare(M::DELETE, "/checkpoint"),
        json_case(M::POST, "/button", json!({"button": "a"})),
        bare(M::GET, "/joypad"),
        bare(M::POST, "/memory"),
        bare(M::GET, "/"),
        bare(M::GET, "/status/"),
    ]
}

async fn compare_all(
    configure: impl FnOnce(&mut flysim::config::Config),
    cases: &[Case],
) -> Vec<(String, StatusCode)> {
    let rig = rig(configure).await;
    let mut seen = Vec::new();
    for (method, uri, content_type, body) in cases {
        let direct = exchange(rig.direct(), method, uri, *content_type, body).await;
        let edge = exchange(rig.via_edge(), method, uri, *content_type, body).await;
        assert_eq!(
            direct,
            edge,
            "{method} {uri}: direct {} {:?} vs edge {} {:?}",
            direct.status,
            String::from_utf8_lossy(&direct.body),
            edge.status,
            String::from_utf8_lossy(&edge.body)
        );
        seen.push((format!("{method} {uri}"), direct.status));
    }
    assert_eq!(rig.metrics.call_failures.load(Ordering::Relaxed), 0);
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_endpoint_through_the_edge_equals_direct() {
    let seen = compare_all(|_| {}, &cases()).await;
    // The cases reach every status code control-api.md names, so the comparison is not of
    // twenty identical 200s.
    let codes: std::collections::BTreeSet<u16> = seen.iter().map(|(_, s)| s.as_u16()).collect();
    for code in [200, 202, 400, 403, 404, 422, 429, 503] {
        assert!(codes.contains(&code), "no case answered {code}: {seen:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_reward_enabled_and_chat_disabled_too() {
    let seen = compare_all(
        |config| {
            config.control.allow_reward = true;
            config.chat.enabled = false;
        },
        &cases(),
    )
    .await;
    let status = |what: &str| seen.iter().find(|(w, _)| w == what).unwrap().1;
    assert_eq!(status("POST /chat"), StatusCode::FORBIDDEN);
    assert_eq!(status("POST /reward"), StatusCode::ACCEPTED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_checkpoint_an_unhealthy_loop_and_a_timeout_match_too() {
    let rig = rig(|_| {}).await;
    rig.sim.fail_checkpoint.store(true, Ordering::SeqCst);
    for (method, uri, body) in [
        (Method::POST, "/checkpoint", Vec::new()),
        // The loop takes the command but never answers: both sides wait COMMAND_TIMEOUT and 503.
        (
            Method::POST,
            "/stimulate",
            serde_json::to_vec(&json!({"by": "hang", "source": "chat"})).unwrap(),
        ),
    ] {
        let direct = exchange(rig.direct(), &method, uri, Some("application/json"), &body).await;
        let edge = exchange(
            rig.via_edge(),
            &method,
            uri,
            Some("application/json"),
            &body,
        )
        .await;
        assert_eq!(direct, edge, "{method} {uri}");
        assert!(
            direct.status.is_server_error(),
            "{method} {uri}: {}",
            direct.status
        );
    }
    // A loop that has not advanced for 2 s.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    let direct = exchange(rig.direct(), &Method::GET, "/healthz", None, &[]).await;
    let edge = exchange(rig.via_edge(), &Method::GET, "/healthz", None, &[]).await;
    assert_eq!(direct, edge);
    assert_eq!(direct.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_events_page_rides_as_an_artifact_and_still_matches() {
    let rig = rig(|_| {}).await;
    // Enough events that the page is far over one envelope.
    let mut log = flysim::eventlog::EventLog::open(
        &rig.dir.path().join("more-events"),
        rig.state.shared.events.clone(),
    )
    .unwrap();
    for index in 0..2_000u64 {
        log.append(
            2_000 + index,
            index as f64,
            flysim::eventlog::NewEvent::new(
                flysim::snapshot::FeedEventKind::Reward,
                format!("a reasonably long reward label number {index}"),
            )
            .value(0.25),
        );
    }
    let uri = "/events?since=0&limit=4096";
    let direct = exchange(rig.direct(), &Method::GET, uri, None, &[]).await;
    let edge = exchange(rig.via_edge(), &Method::GET, uri, None, &[]).await;
    assert!(
        direct.body.len() > flysim::controlbus::INLINE_MAX,
        "{} bytes",
        direct.body.len()
    );
    assert_eq!(direct, edge);
    // The artifact is released once read.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.bus.router.stats().artifacts, 0);
}

/// The running edge over real sockets: the whole HTTP/1.1 response equals flysim's own listener's,
/// `date` aside, for a request of every kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_tcp_the_edge_answers_the_same_bytes_as_flysim() {
    let rig = rig(|_| {}).await;
    rig.close_edge().await;
    // flysim's own listener, as direct mode binds it.
    let direct_addr = common::free_port();
    let listener = tokio::net::TcpListener::bind(direct_addr).await.unwrap();
    let router = rig.direct();
    tokio::spawn(async move { axum::serve(listener, router).await });
    // The edge process's loop, on another port.
    let edge_addr = common::free_port();
    let config = fly_control_edge::EdgeConfig {
        control_bind: edge_addr,
        ..rig.config.clone()
    };
    let metrics = std::sync::Arc::new(fly_control_edge::EdgeMetrics::default());
    let edge = tokio::spawn(fly_control_edge::run(
        config,
        std::sync::Arc::clone(&metrics),
    ));
    let mut waited = 0;
    while metrics.connected.load(Ordering::Relaxed) == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        waited += 1;
        assert!(waited < 250, "the edge never bound");
    }
    let body = r#"{"by":"viewer","source":"points","durationMs":300}"#;
    let chat = r#"{"by":"viewer","text":"see http://x.example"}"#;
    for request in [
        "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_owned(),
        "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_owned(),
        "GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_owned(),
        "GET /events?since=2&limit=3 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_owned(),
        format!(
            "POST /stimulate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{chat}",
            chat.len()
        ),
        "POST /pause HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        "POST /reward HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        "GET /buttons HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_owned(),
    ] {
        let direct = common::raw_http(direct_addr, &request).await.unwrap();
        let via_edge = common::raw_http(edge_addr, &request).await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&direct),
            String::from_utf8_lossy(&via_edge),
            "{request}"
        );
        assert!(direct.starts_with(b"HTTP/1.1 "), "{}", String::from_utf8_lossy(&direct));
    }
    edge.abort();
}
