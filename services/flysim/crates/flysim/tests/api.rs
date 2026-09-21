//! The control API surface, driven over the built router.
//!
//! Binding contract: `docs/control-api.md`. Behavioural reference:
//! `packages/feed/src/fake/server.ts`, which this service must be a drop-in replacement for.
//!
//! The headline assertion is the negative one: **there is no route that presses a button**, and
//! `docs/control-api.md` calls that "a structural guarantee, not a configuration". It is checked
//! two ways here — over the route table the router is built from, and by asking the live router
//! for every plausible spelling of such a route and requiring a 404.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use flysim::api::{READ_ONLY_ROUTES, ROUTES};
use flysim::chat::{ChatRefusal, RejectReason};
use flysim::eventlog::{NewEvent, now_wall_ms};
use flysim::ratelimit::Refusal;
use flysim::simloop::Command;
use flysim::snapshot::{FeedEventKind, FeedStatus};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Send one request through the router and return the status, the headers and the JSON body.
async fn call(
    router: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(value) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::String(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    };
    (status, headers, value)
}

// ---------------------------------------------------------------------------------------------
// The structural guarantee
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn there_is_no_button_route_and_nothing_that_looks_like_one() {
    let (state, _commands, _snapshots) = common::test_state(|_| {});

    for path in [
        "/button",
        "/buttons",
        "/button/a",
        "/press",
        "/input",
        "/joypad",
        "/keys",
        "/poke",
        "/memory",
        "/wram",
        "/catalog",
        "/rewards",
        "/manual",
    ] {
        for method in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            let (status, _, body) =
                call(flysim::api::router(state.clone()), method.clone(), path, Some(json!({})))
                    .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(
                body["error"],
                json!(format!("no such route: {method} {path}")),
                "{method} {path}"
            );
        }
    }

    // And the table the router is built from names every route there is.
    assert_eq!(ROUTES.len(), 11, "a route was added without updating the table: {ROUTES:?}");
    for (_, path) in ROUTES {
        assert!(
            !path.to_ascii_lowercase().contains("button"),
            "{path} is a button route"
        );
    }
}

#[tokio::test]
async fn every_route_in_the_table_exists_and_every_other_path_is_404() {
    let (state, mut commands, _snapshots) = common::test_state(|config| {
        config.control.allow_reward = true;
    });
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            answer(command);
        }
    });
    state.shared.beat();

    for (method, path) in ROUTES {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let body = match *path {
            "/stimulate" => Some(json!({ "by": "alex", "source": "operator" })),
            "/reward" => Some(json!({ "value": 0.5, "by": "alex", "source": "operator" })),
            "/chat" => Some(json!({ "by": "alex", "text": "hello" })),
            _ => None,
        };
        let (status, headers, _) =
            call(flysim::api::router(state.clone()), method.clone(), path, body).await;
        assert!(status.is_success(), "{method} {path} answered {status}");
        assert_eq!(
            headers.get(header::CACHE_CONTROL).map(|value| value.to_str().unwrap()),
            Some("no-store"),
            "{method} {path}"
        );
    }

    // The wrong method on a real path is still a 404, as the reference server answers.
    let (status, _, _) =
        call(flysim::api::router(state.clone()), Method::GET, "/stimulate", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(flysim::api::router(state), Method::POST, "/status", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_read_only_listener_serves_metrics_and_status_but_nothing_that_changes_state() {
    let (state, _commands, _snapshots) = common::test_state(|config| {
        config.control.allow_reward = true;
    });
    for (method, path) in READ_ONLY_ROUTES {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let (status, _, _) =
            call(flysim::api::metrics_router(state.clone()), method, path, None).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "{path}");
    }
    for path in ["/stimulate", "/reward", "/chat", "/checkpoint", "/pause", "/resume", "/events"] {
        let (status, _, _) = call(
            flysim::api::metrics_router(state.clone()),
            Method::POST,
            path,
            Some(json!({ "by": "alex", "source": "operator", "value": 1.0 })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} must not be reachable from outside");
    }
}

// ---------------------------------------------------------------------------------------------
// Per-route behaviour
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn status_is_the_feed_header_minus_events_and_attachments_plus_versions() {
    let (state, _commands, _snapshots) = common::test_state(|_| {});
    let snapshot = state.snapshot();
    let (status, _, body) =
        call(flysim::api::router(state.clone()), Method::GET, "/status", None).await;
    assert_eq!(status, StatusCode::OK);

    let header = serde_json::to_value(&snapshot.header).unwrap();
    for (key, value) in header.as_object().unwrap() {
        match key.as_str() {
            "events" | "attachments" => {
                assert!(body.get(key).is_none(), "{key} must not be in /status");
            }
            _ => assert_eq!(&body[key], value, "{key}"),
        }
    }
    assert!(body["version"].is_object(), "{body}");
    assert_eq!(body["checkpoint"]["generation"], json!(0));
    assert_eq!(body["checkpoint"]["latestWallMs"], json!(0));

    // `/status.json` is the same document under the name the metrics listener is scraped with.
    let (_, _, alias) = call(flysim::api::router(state), Method::GET, "/status.json", None).await;
    assert_eq!(alias["seq"], body["seq"]);
    assert_eq!(alias["version"], body["version"]);
}

#[tokio::test]
async fn healthz_is_503_until_the_loop_has_advanced_and_200_for_two_seconds_after() {
    let (state, _commands, _snapshots) = common::test_state(|_| {});

    let (status, _, body) =
        call(flysim::api::router(state.clone()), Method::GET, "/healthz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "no heartbeat yet");
    assert_eq!(body["error"], json!("loop has not advanced in the last 2 seconds"));

    state.shared.beat();
    let (status, _, body) =
        call(flysim::api::router(state.clone()), Method::GET, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "ok" }));

    // A heartbeat from more than two seconds ago is a 503 again, which is what makes the
    // watchdog restart a wedged loop.
    state
        .shared
        .heartbeat_ms
        .store(now_wall_ms() - 2_001, std::sync::atomic::Ordering::Relaxed);
    let (status, _, _) = call(flysim::api::router(state), Method::GET, "/healthz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn stimulate_validates_its_body_and_reports_the_event_id_or_the_retry_delay() {
    let (state, mut commands, _snapshots) = common::test_state(|_| {});
    tokio::spawn(async move {
        let mut call_index = 0;
        while let Some(command) = commands.recv().await {
            match command {
                Command::Stimulate { duration_ms, by, source, reply } => {
                    assert_eq!(source, "chat");
                    call_index += 1;
                    let outcome = match call_index {
                        1 => {
                            assert_eq!(by, "alex");
                            assert_eq!(duration_ms, None, "an absent durationMs stays absent");
                            Ok(7)
                        }
                        2 => {
                            assert_eq!(duration_ms, Some(900.0));
                            Ok(8)
                        }
                        3 => Err(Refusal::PulseActive { retry_after_ms: 250 }),
                        _ => Err(Refusal::RateLimited { retry_after_ms: 41_500 }),
                    };
                    let _ = reply.send(outcome);
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
    });

    let post = |body: Value| {
        let router = flysim::api::router(state.clone());
        async move { call(router, Method::POST, "/stimulate", Some(body)).await }
    };

    let (status, _, body) = post(json!({ "by": "alex", "source": "chat" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({ "eventId": 7 }));

    let (status, _, body) =
        post(json!({ "durationMs": 900, "by": "alex", "source": "chat" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({ "eventId": 8 }));

    let (status, _, body) = post(json!({ "by": "alex", "source": "chat" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, json!({ "retryAfterMs": 250 }), "an active pulse blocks");

    let (status, _, body) = post(json!({ "by": "alex", "source": "chat" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, json!({ "retryAfterMs": 41_500 }), "the per-minute budget is spent");

    // Bad bodies never reach the simulation.
    for bad in [
        json!({}),
        json!({ "by": "alex" }),
        json!({ "source": "chat" }),
        json!({ "by": 1, "source": "chat" }),
        json!({ "by": "alex", "source": "bridge" }),
        json!({ "by": "alex", "source": null }),
    ] {
        let (status, _, body) = post(bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], json!("expected { durationMs?, by, source }"), "{bad}");
    }
    let (status, _, body) =
        post(json!({ "durationMs": -5, "by": "alex", "source": "chat" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("durationMs must be a positive number"));

    // An empty body is a 400, not a panic.
    let (status, _, _) =
        call(flysim::api::router(state), Method::POST, "/stimulate", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reward_is_403_while_disabled_and_accepted_once_an_operator_turns_it_on() {
    let (disabled, mut commands, _snapshots) = common::test_state(|_| {});
    tokio::spawn(async move {
        if let Some(command) = commands.recv().await {
            panic!("a disabled reward must not reach the simulation: {command:?}");
        }
    });
    let (status, _, body) = call(
        flysim::api::router(disabled),
        Method::POST,
        "/reward",
        Some(json!({ "value": 0.5, "by": "alex", "source": "operator" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("reward is disabled (control.allow_reward = false)"));

    let (enabled, mut commands, _snapshots) = common::test_state(|config| {
        config.control.allow_reward = true;
    });
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                Command::Reward { value, by, source, reply } => {
                    assert_eq!((value, by.as_str(), source.as_str()), (0.5, "alex", "operator"));
                    let _ = reply.send(99);
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
    });
    let (status, _, body) = call(
        flysim::api::router(enabled.clone()),
        Method::POST,
        "/reward",
        Some(json!({ "value": 0.5, "by": "alex", "source": "operator" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({ "eventId": 99 }));

    for bad in [
        json!({ "by": "alex", "source": "chat" }),
        json!({ "value": "0.5", "by": "alex", "source": "chat" }),
        json!({ "value": 0.5, "source": "chat" }),
        json!({ "value": 0.5, "by": "alex" }),
    ] {
        let (status, _, body) = call(
            flysim::api::router(enabled.clone()),
            Method::POST,
            "/reward",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], json!("expected { value, by, source }"), "{bad}");
    }
}

#[tokio::test]
async fn checkpoint_pause_and_resume_report_what_the_simulation_did() {
    let (state, mut commands, _snapshots) = common::test_state(|_| {});
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            answer(command);
        }
    });

    let (status, _, body) =
        call(flysim::api::router(state.clone()), Method::POST, "/checkpoint", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "generation": 12 }));

    let (status, _, body) =
        call(flysim::api::router(state.clone()), Method::POST, "/pause", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "paused" }));

    let (status, _, body) = call(flysim::api::router(state), Method::POST, "/resume", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "running" }));
}

#[tokio::test]
async fn a_failed_checkpoint_is_a_500_with_the_reason() {
    let (state, mut commands, _snapshots) = common::test_state(|_| {});
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            if let Command::Checkpoint { reply } = command {
                let _ = reply.send(Err("No space left on device".to_string()));
            }
        }
    });
    let (status, _, body) =
        call(flysim::api::router(state), Method::POST, "/checkpoint", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], json!("No space left on device"));
}

#[tokio::test]
async fn a_stopped_simulation_is_a_503_rather_than_a_hang() {
    let (state, commands, _snapshots) = common::test_state(|_| {});
    drop(commands);
    let (status, _, body) = call(
        flysim::api::router(state.clone()),
        Method::POST,
        "/stimulate",
        Some(json!({ "by": "alex", "source": "chat" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("the simulation has stopped"));

    // Read-only routes keep answering, so an operator can still see what happened.
    let (status, _, _) = call(flysim::api::router(state), Method::GET, "/status", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_full_command_queue_is_a_503_rather_than_a_wait() {
    // Nothing drains the receiver, so the bounded queue fills and then refuses.
    let (state, _commands, _snapshots) = common::test_state(|_| {});
    let mut refused = 0;
    for _ in 0..(flysim::simloop::COMMAND_QUEUE + 4) {
        let (status, _, _) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            call(
                flysim::api::router(state.clone()),
                Method::POST,
                "/resume",
                None,
            ),
        )
        .await
        .unwrap_or((StatusCode::REQUEST_TIMEOUT, Default::default(), Value::Null));
        if status == StatusCode::SERVICE_UNAVAILABLE {
            refused += 1;
        }
    }
    assert!(refused >= 4, "{refused} of the requests were refused");
}

#[tokio::test]
async fn events_pages_the_log_by_id() {
    let (state, _commands, _snapshots) = common::test_state(|_| {});
    let dir = tempfile::tempdir().unwrap();
    let mut log =
        flysim::eventlog::EventLog::open(dir.path(), state.shared.events.clone()).unwrap();
    for index in 0..5 {
        log.append(
            1_000 + index,
            index as f64,
            NewEvent::new(FeedEventKind::System, format!("event {index}")),
        );
    }

    let get = |query: &str| {
        let router = flysim::api::router(state.clone());
        let uri = format!("/events{query}");
        async move { call(router, Method::GET, &uri, None).await }
    };

    let (status, _, body) = get("").await;
    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0]["id"], json!(1));
    assert_eq!(events[0]["label"], json!("event 0"));
    assert_eq!(events[0]["kind"], json!("system"));

    let (_, _, body) = get("?since=3").await;
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["id"], json!(4));

    let (_, _, body) = get("?since=0&limit=2").await;
    assert_eq!(body["events"].as_array().unwrap().len(), 2);

    let (_, _, body) = get("?since=99").await;
    assert_eq!(body["events"], json!([]));

    // Unparseable parameters fall back to the defaults, as `Number.parseInt(...) || 0` does.
    for query in ["?since=abc", "?limit=abc", "?since=&limit=", "?limit=0", "?since=-1"] {
        let (status, _, body) = get(query).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(body["events"].as_array().unwrap().len(), 5, "{query}");
    }
}

#[tokio::test]
async fn metrics_is_prometheus_text_on_both_listeners() {
    let (state, _commands, _snapshots) = common::test_state(|_| {});
    for router in [
        flysim::api::router(state.clone()),
        flysim::api::metrics_router(state.clone()),
    ] {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("\nfly_frames_sent_total 0\n"), "{text}");
        assert!(text.contains("\nfly_feed_clients 0\n"), "{text}");
        assert!(text.contains("fly_milestone_rank 22"), "{text}");
    }
}

#[tokio::test]
async fn chat_is_403_while_disabled_and_never_reaches_the_simulation() {
    let (state, mut commands, _snapshots) = common::test_state(|config| {
        config.chat.enabled = false;
    });
    tokio::spawn(async move {
        if let Some(command) = commands.recv().await {
            panic!("a disabled chat endpoint must not reach the simulation: {command:?}");
        }
    });
    let (status, _, body) = call(
        flysim::api::router(state),
        Method::POST,
        "/chat",
        Some(json!({ "by": "alex", "text": "hello" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("chat is disabled (chat.enabled = false)"));
}

#[tokio::test]
async fn chat_validates_its_body_and_maps_refusals_onto_422_and_429() {
    let (state, mut commands, _snapshots) = common::test_state(|_| {});
    tokio::spawn(async move {
        let mut call_index = 0;
        while let Some(command) = commands.recv().await {
            match command {
                Command::Chat { by, text, bot, reply } => {
                    call_index += 1;
                    let outcome = match call_index {
                        1 => {
                            assert_eq!(by, "alex");
                            assert_eq!(text, "go left");
                            assert!(!bot, "bot defaults to false when the field is absent");
                            Ok(5)
                        }
                        2 => {
                            assert!(bot, "the bridge's own replies arrive with bot: true");
                            Ok(6)
                        }
                        3 => Err(ChatRefusal::Rejected(RejectReason::Url)),
                        4 => Err(ChatRefusal::Rejected(RejectReason::DenyList)),
                        _ => Err(ChatRefusal::RateLimited { retry_after_ms: 1_500 }),
                    };
                    let _ = reply.send(outcome);
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
    });

    let post = |body: Value| {
        let router = flysim::api::router(state.clone());
        async move { call(router, Method::POST, "/chat", Some(body)).await }
    };

    let (status, _, body) = post(json!({ "by": "alex", "text": "go left" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({ "eventId": 5 }));

    let (status, _, body) =
        post(json!({ "by": "flybridgebot", "text": "Sugar from alex!", "bot": true })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({ "eventId": 6 }));

    // A rule refusal is a 422 naming the rule, which is also the metric label.
    let (status, _, body) = post(json!({ "by": "alex", "text": "bit.ly" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], json!("chat line refused: url"));
    let (status, _, body) = post(json!({ "by": "alex", "text": "denied" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], json!("chat line refused: deny_list"));

    // A rate limit is a 429 in the same currency as /stimulate's.
    let (status, _, body) = post(json!({ "by": "alex", "text": "again" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, json!({ "retryAfterMs": 1_500 }));

    // Malformed bodies never reach the simulation.
    for bad in [
        json!({}),
        json!({ "by": "alex" }),
        json!({ "text": "hello" }),
        json!({ "by": 1, "text": "hello" }),
        json!({ "by": "alex", "text": 1 }),
        json!({ "by": "alex", "text": null }),
    ] {
        let (status, _, body) = post(bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], json!("expected { by, text, bot? }"), "{bad}");
    }
    let (status, _, body) = post(json!({ "by": "alex", "text": "hi", "bot": "yes" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("bot must be a boolean"));

    let (status, _, _) = call(flysim::api::router(state), Method::POST, "/chat", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an empty body is a 400, not a panic");
}

/// Canned answers for the commands a test does not care about.
fn answer(command: Command) {
    match command {
        Command::Stimulate { reply, .. } => {
            let _ = reply.send(Ok(1));
        }
        Command::Reward { reply, .. } => {
            let _ = reply.send(2);
        }
        Command::Chat { reply, .. } => {
            let _ = reply.send(Ok(3));
        }
        Command::Checkpoint { reply } => {
            let _ = reply.send(Ok(12));
        }
        Command::Pause { reply } => {
            let _ = reply.send(FeedStatus::Paused);
        }
        Command::Resume { reply } => {
            let _ = reply.send(FeedStatus::Running);
        }
        Command::Shutdown { reply } => {
            let _ = reply.send(());
        }
    }
}
