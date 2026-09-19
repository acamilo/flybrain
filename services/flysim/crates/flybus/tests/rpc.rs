//! RPC: exclusive registration, pinned incarnations, request/reply, FIFO, bounds,
//! cancellation, disconnects and an endpoint-side result cache. Every test runs over both the
//! in-memory transport and a Unix socket.

mod common;

use std::collections::HashMap;
use std::io::Write;

use common::{Via, code, env, env_with, obj, quiet, sealed, within};
use flybus::{CancelState, Dispatch, ErrorCode, Grants, Limits, Pattern, Policy, ServiceConfig};
use serde_json::json;

async fn request_reply_roundtrip(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.counter", ServiceConfig::default())
        .await
        .unwrap();
    let mut pending = caller
        .call(
            "example.counter",
            Some(svc.incarnation()),
            "Counter.Increment",
            obj(json!({"amount": 1})),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(pending.service_incarnation(), svc.incarnation());
    let req = within("request", svc.next()).await.unwrap();
    assert_eq!(req.method(), "Counter.Increment");
    assert_eq!(req.payload()["amount"], 1);
    assert_eq!(req.target(), "example.counter");
    // The router, not the body, says who called.
    assert_eq!(req.caller(), &caller.info().identity);
    assert!(req.reply(obj(json!({"value": 1})), &[]).await.unwrap());
    drop(req);
    let res = within("result", pending.result()).await.unwrap();
    assert_eq!(res.outcome()["value"], 1);
    assert_eq!(res.responder(), &server.info().identity);
    assert_eq!(res.call_id(), pending.call_id());
    drop(res);
    e.settle("everything consumed", |s| s.calls == 0 && s.owners == 0)
        .await;
}

async fn registration_is_exclusive_and_pinned(via: Via) {
    let e = env(via).await;
    let a = e.client("a").await;
    let b = e.client("b").await;
    let caller = e.client("caller").await;
    let first = a
        .register("agent.fly-a", ServiceConfig::default())
        .await
        .unwrap();
    let dup = b
        .register("agent.fly-a", ServiceConfig::default())
        .await
        .unwrap_err();
    assert_eq!(dup.code, ErrorCode::Conflict);
    let old = first.incarnation().to_owned();
    drop(first);
    // Unregistration is asynchronous; wait for the name to come free.
    let second = loop {
        match b.register("agent.fly-a", ServiceConfig::default()).await {
            Ok(s) => break s,
            Err(err) => assert_eq!(err.code, ErrorCode::Conflict),
        }
    };
    assert_ne!(second.incarnation(), old);
    let changed = caller
        .call(
            "agent.fly-a",
            Some(&old),
            "Agent.Prepare",
            obj(json!({})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(changed.code, ErrorCode::TargetChanged);
    assert_eq!(changed.dispatch, Dispatch::NotDispatched);
    let missing = caller
        .call("agent.nobody", None, "Agent.Prepare", obj(json!({})), &[])
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NoService);
    let over = b
        .register(
            "agent.big",
            ServiceConfig {
                max_queued: 17,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(over.code, ErrorCode::QuotaExceeded);
}

async fn authority_is_enforced(via: Via) {
    let policy = Policy::closed()
        .client(
            "server",
            Grants {
                register: vec![Pattern::exact("a.svc")],
                ..Grants::default()
            },
        )
        .client(
            "caller",
            Grants {
                call: vec![Pattern::prefix("a.")],
                ..Grants::default()
            },
        );
    let e = env_with(via, Limits::default(), policy).await;
    assert_eq!(
        e.try_client("stranger").await.unwrap_err().code,
        ErrorCode::NotAuthorized
    );
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    // One live connection per configured identity.
    assert_eq!(
        e.try_client("caller").await.unwrap_err().code,
        ErrorCode::NotAuthorized
    );
    assert_eq!(
        server
            .register("b.svc", ServiceConfig::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAuthorized
    );
    let _svc = server
        .register("a.svc", ServiceConfig::default())
        .await
        .unwrap();
    assert_eq!(
        caller
            .register("a.other", ServiceConfig::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAuthorized
    );
    assert_eq!(
        caller
            .call("b.svc", None, "M", obj(json!({})), &[])
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAuthorized
    );
    assert_eq!(
        caller
            .declare_topic("a.t", flybus::Retained::None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAuthorized
    );
    assert!(
        caller
            .call("a.svc", None, "M", obj(json!({})), &[])
            .await
            .is_ok()
    );
    // A reconnect must present a new incarnation.
    let mut cfg = e.config("server");
    cfg.client_incarnation = Some(server.info().identity.client_incarnation.clone());
    drop(_svc);
    server.close().await;
    e.settle("server gone", |s| s.connections == 1).await;
    let reused = flybus::Client::connect(e.transport_as("server").await, cfg)
        .await
        .unwrap_err();
    assert_eq!(reused.code, ErrorCode::NotAuthorized);
    assert!(e.try_client("server").await.is_ok());
}

async fn fifo_dispatch_and_out_of_order_completion(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.echo", ServiceConfig::default())
        .await
        .unwrap();
    let mut calls = Vec::new();
    for i in 0..6 {
        calls.push(
            caller
                .call("example.echo", None, "Echo", obj(json!({"i": i})), &[])
                .await
                .unwrap(),
        );
    }
    let mut reqs = Vec::new();
    for i in 0..6 {
        let r = within("request", svc.next()).await.unwrap();
        assert_eq!(r.payload()["i"], i, "first dispatch is FIFO per caller");
        reqs.push(r);
    }
    for r in reqs.iter().rev() {
        assert!(
            r.reply(obj(json!({"echo": r.payload()["i"]})), &[])
                .await
                .unwrap()
        );
    }
    drop(reqs);
    for (i, mut c) in calls.into_iter().enumerate() {
        let res = within("result", c.result()).await.unwrap();
        assert_eq!(res.outcome()["echo"], i, "results correlate by call id");
    }
}

async fn service_queue_backpressure(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "example.slow",
            ServiceConfig {
                max_queued: 1,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let _c1 = caller
        .call("example.slow", None, "Work", obj(json!({"n": 1})), &[])
        .await
        .unwrap();
    let held = within("first request", svc.next()).await.unwrap();
    let _c2 = caller
        .call("example.slow", None, "Work", obj(json!({"n": 2})), &[])
        .await
        .unwrap();
    let full = caller
        .call("example.slow", None, "Work", obj(json!({"n": 3})), &[])
        .await
        .unwrap_err();
    assert_eq!(full.code, ErrorCode::Backpressure);
    assert_eq!(full.dispatch, Dispatch::NotDispatched);
    // In-flight credit returns only when the request delivery is consumed.
    quiet("second request while the first is held", svc.next()).await;
    drop(held);
    let second = within("second request", svc.next()).await.unwrap();
    assert_eq!(second.payload()["n"], 2);
}

async fn cancellation_states(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "example.worker",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();

    let mut dispatched = caller
        .call("example.worker", None, "Work", obj(json!({"n": 1})), &[])
        .await
        .unwrap();
    let req1 = within("request 1", svc.next()).await.unwrap();
    let mut queued = caller
        .call("example.worker", None, "Work", obj(json!({"n": 2})), &[])
        .await
        .unwrap();

    assert_eq!(
        queued.cancel().await.unwrap(),
        CancelState::CancelledBeforeDispatch
    );
    let gone = within("cancelled result", queued.result())
        .await
        .unwrap_err();
    assert_eq!(
        (gone.code, gone.dispatch),
        (ErrorCode::CallGone, Dispatch::NotDispatched)
    );

    assert_eq!(
        dispatched.cancel().await.unwrap(),
        CancelState::ExecutionUnknown
    );
    let unknown = within("detached result", dispatched.result())
        .await
        .unwrap_err();
    assert_eq!(
        (unknown.code, unknown.dispatch),
        (ErrorCode::CallGone, Dispatch::Unknown)
    );
    // The handler still finishes; its reply reaches nobody and is not an error.
    assert!(!req1.reply(obj(json!({"late": true})), &[]).await.unwrap());
    drop(req1);

    let mut done = caller
        .call("example.worker", None, "Work", obj(json!({"n": 3})), &[])
        .await
        .unwrap();
    let req3 = within("request 3", svc.next()).await.unwrap();
    assert_eq!(
        req3.payload()["n"],
        3,
        "the cancelled call was never dispatched"
    );
    assert!(req3.reply(obj(json!({"ok": 3})), &[]).await.unwrap());
    // Wait for the result to be admitted before cancelling.
    e.settle("result admitted", |s| s.calls == 1).await;
    let state = done.cancel().await.unwrap();
    assert_eq!(state, CancelState::Completed);
    let res = within("completed result", done.result()).await.unwrap();
    assert_eq!(res.outcome()["ok"], 3);
    drop((res, req3));
    e.settle("calls retired", |s| s.calls == 0).await;
    assert_eq!(done.cancel().await.unwrap(), CancelState::CallGone);
}

async fn replies_are_single_and_independent_of_the_request_guard(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.once", ServiceConfig::default())
        .await
        .unwrap();
    let mut pending = caller
        .call("example.once", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    let responder = req.responder();
    drop(req); // consumed before replying
    assert!(
        responder
            .reply(obj(json!({"first": true})), &[])
            .await
            .unwrap()
    );
    let again = responder
        .reply(obj(json!({"second": true})), &[])
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::CallGone);
    let res = within("result", pending.result()).await.unwrap();
    assert_eq!(res.outcome()["first"], true);
}

async fn service_disconnect_fails_calls(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "example.fragile",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let mut c1 = caller
        .call("example.fragile", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let held = within("request", svc.next()).await.unwrap();
    let mut c2 = caller
        .call("example.fragile", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    // Keep the first delivery credit occupied until unregister has synchronously failed the
    // queued call. Otherwise consuming it may truthfully dispatch call 2 before unregister.
    drop(svc);
    let r2 = within("queued call", c2.result()).await.unwrap_err();
    assert_eq!(
        (r2.code, r2.dispatch),
        (ErrorCode::NoService, Dispatch::NotDispatched)
    );
    drop(held);
    server.close().await;
    let r1 = within("dispatched call", c1.result()).await.unwrap_err();
    assert_eq!(
        (r1.code, r1.dispatch),
        (ErrorCode::NoService, Dispatch::Dispatched)
    );
    e.settle("nothing left", |s| s.calls == 0 && s.services == 0)
        .await;
}

async fn unregister_fails_queued_but_dispatched_may_reply(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "example.leaving",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let mut c1 = caller
        .call("example.leaving", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    let mut c2 = caller
        .call("example.leaving", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    drop(svc);
    let r2 = within("queued call", c2.result()).await.unwrap_err();
    assert_eq!(
        (r2.code, r2.dispatch),
        (ErrorCode::NoService, Dispatch::NotDispatched)
    );
    assert!(req.reply(obj(json!({"done": true})), &[]).await.unwrap());
    assert_eq!(
        within("dispatched call", c1.result())
            .await
            .unwrap()
            .outcome()["done"],
        true
    );
}

async fn raw_call_ids_and_forged_replies(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let mut svc = server
        .register("example.raw", ServiceConfig::default())
        .await
        .unwrap();
    let mut raw = e.raw_hello("rawcaller").await;
    let call = |id: &str| json!({"callId": id, "target": "example.raw", "expectedIncarnation": null, "method": "M", "payload": {}});
    assert_eq!(code(&raw.call("rpc.call", call("call-5")).await), "OK");
    assert_eq!(
        code(&raw.call("rpc.call", call("call-5")).await),
        "INVALID_ENVELOPE"
    );
    assert_eq!(
        code(&raw.call("rpc.call", call("call-3")).await),
        "INVALID_ENVELOPE"
    );
    assert_eq!(
        code(&raw.call("rpc.call", call("call-05")).await),
        "INVALID_ENVELOPE"
    );
    // Identity comes from the connection; a body cannot claim one.
    let mut forged = call("call-6");
    forged["caller"] = json!({"clientId": "server", "clientIncarnation": "x"});
    assert_eq!(
        code(&raw.call("rpc.call", forged).await),
        "INVALID_ENVELOPE"
    );
    assert_eq!(code(&raw.call("rpc.call", call("call-6")).await), "OK");
    let req = within("request", svc.next()).await.unwrap();
    assert_eq!(req.caller().client_id, "rawcaller");
    // A third party cannot answer someone else's request.
    let mut other = e.raw_hello("intruder").await;
    let r = other
        .call(
            "rpc.reply",
            json!({"callId": req.call_id(), "requestDeliveryId": req.delivery_id(), "outcome": {}}),
        )
        .await;
    assert_eq!(code(&r), "OWNER_INVALID");
    assert!(req.reply(obj(json!({"real": true})), &[]).await.unwrap());
    let result = raw.event().await;
    assert_eq!(result.op, "rpc.result");
    assert_eq!(result.body["outcome"]["real"], true);
    assert_eq!(result.body["responder"]["clientId"], "server");
}

/// bus-v1 section 6: an endpoint caches Artifact handles plus payload; a domain retry with a
/// fresh call id gets fresh delivery ownership over the same immutable bytes.
async fn endpoint_cache_replays_artifact_results(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("agent.cached", ServiceConfig::default())
        .await
        .unwrap();
    let service = tokio::spawn(async move {
        let mut cache: HashMap<String, flybus::Artifact> = HashMap::new();
        let mut executions = 0;
        while let Some(req) = svc.next().await {
            if req.method() == "Cache.Evict" {
                cache.clear();
                req.reply(obj(json!({})), &[]).await.unwrap();
                continue;
            }
            let rid = req.payload()["requestId"].as_str().unwrap().to_owned();
            if !cache.contains_key(&rid) {
                executions += 1;
                let bytes = format!("result of {rid}").into_bytes();
                let mut w = server
                    .artifacts()
                    .allocate(bytes.len() as u64, "text/plain")
                    .await
                    .unwrap();
                w.write_all(&bytes).unwrap();
                cache.insert(rid.clone(), w.seal().await.unwrap());
            }
            let art = cache[&rid].clone();
            req.reply(obj(json!({"executions": executions})), &[("state", &art)])
                .await
                .unwrap();
        }
    });
    let mut ids = Vec::new();
    for _ in 0..2 {
        let res = within(
            "result",
            caller.call_and_wait(
                "agent.cached",
                None,
                "Agent.Prepare",
                obj(json!({"requestId": "req-41"})),
                &[],
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            res.outcome()["executions"],
            1,
            "the retry did not re-execute"
        );
        let art = res.artifact("state").unwrap();
        assert_eq!(art.read_all().await.unwrap(), b"result of req-41");
        ids.push((
            res.delivery_id().to_owned(),
            art.reference().artifact_id.clone(),
        ));
    }
    assert_ne!(ids[0].0, ids[1].0, "each replay is a fresh delivery");
    assert_eq!(ids[0].1, ids[1].1, "of the same immutable object");
    e.settle("cache holds the only root", |s| {
        s.sealed_artifacts == 1 && s.artifact_roots == 1
    })
    .await;
    caller
        .call_and_wait("agent.cached", None, "Cache.Evict", obj(json!({})), &[])
        .await
        .unwrap();
    e.settle("eviction collects", |s| {
        s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
    e.settle_files("sealed", 0).await;
    service.abort();
}

async fn dropped_call_is_cancelled_and_late_result_consumed(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.abandon", ServiceConfig::default())
        .await
        .unwrap();
    let art = sealed(&server, b"payload", "text/plain").await;
    let pending = caller
        .call("example.abandon", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    drop(pending);
    e.settle("call detached", |s| s.calls == 1 && s.active_calls == 0)
        .await;
    // Detached: the reply is not routed and creates no caller-side roots.
    let routed = req.reply(obj(json!({})), &[("a", &art)]).await.unwrap();
    assert!(!routed);
    drop((req, art));
    e.settle("nothing retained", |s| {
        s.calls == 0 && s.artifacts == 0 && s.owners == 0
    })
    .await;

    // A result that arrives after its caller stopped waiting is consumed by the reactor.
    let pending = caller
        .call("example.abandon", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    let art = sealed(&server, b"late", "text/plain").await;
    req.reply(obj(json!({})), &[("a", &art)]).await.unwrap();
    drop((req, art));
    e.settle("result delivered", |s| s.calls == 1 && s.owners == 1)
        .await;
    drop(pending);
    e.settle("result consumed", |s| {
        s.calls == 0 && s.artifacts == 0 && s.owners == 0
    })
    .await;
    assert_eq!(caller.control_errors(), 0);
}

async fn active_call_limit(via: Via) {
    let limits = Limits {
        max_active_calls_per_client: 2,
        ..Limits::default()
    };
    let e = env_with(via, limits, Policy::open()).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let _svc = server
        .register("example.limit", ServiceConfig::default())
        .await
        .unwrap();
    let _a = caller
        .call("example.limit", None, "M", obj(json!({})), &[])
        .await
        .unwrap();
    let _b = caller
        .call("example.limit", None, "M", obj(json!({})), &[])
        .await
        .unwrap();
    let c = caller
        .call("example.limit", None, "M", obj(json!({})), &[])
        .await
        .unwrap_err();
    assert_eq!(c.code, ErrorCode::Backpressure);
}

both_transports!(
    request_reply_roundtrip,
    registration_is_exclusive_and_pinned,
    authority_is_enforced,
    fifo_dispatch_and_out_of_order_completion,
    service_queue_backpressure,
    cancellation_states,
    replies_are_single_and_independent_of_the_request_guard,
    service_disconnect_fails_calls,
    unregister_fails_queued_but_dispatched_may_reply,
    raw_call_ids_and_forged_replies,
    endpoint_cache_replays_artifact_results,
    dropped_call_is_cancelled_and_late_result_consumed,
    active_call_limit,
);
