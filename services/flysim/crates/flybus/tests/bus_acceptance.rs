//! The BUS-01, BUS-02 and BUS-03 acceptance bullets of the implementation guide that the
//! other suites do not already prove, one test per bullet, named after the bullet, plus the
//! transport-equivalence traces.
//!
//! `docs/design/session-framework/bus-conformance.md` maps every bullet of all three lists to
//! the test that proves it; the bullets already covered elsewhere are cited there instead of
//! being repeated here.

mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Duration;

use common::{Env, Trace, Via, env, obj, quiet, sealed, within};
use flybus::{
    CancelState, Client, Dispatch, ErrorCode, Retained, Service, ServiceConfig, SubscriptionConfig,
};
use serde_json::json;

/// A service that executes once per domain `requestId`, keeps its result artifact on an
/// explicit hold of its own and answers a repeat from that cache (bus-v1 section 6).
fn spawn_cache_service(client: Client, mut svc: Service) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cache: HashMap<String, flybus::Artifact> = HashMap::new();
        let mut executions = 0u64;
        while let Some(req) = svc.next().await {
            let rid = req.payload()["requestId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if !cache.contains_key(&rid) {
                executions += 1;
                let bytes = format!("state of {rid}").into_bytes();
                let mut w = client
                    .artifacts()
                    .allocate(bytes.len() as u64, "text/plain")
                    .await
                    .unwrap();
                w.write_all(&bytes).unwrap();
                cache.insert(rid.clone(), w.seal().await.unwrap());
            }
            let art = cache[&rid].clone();
            let _ = req
                .reply(
                    obj(json!({"requestId": rid, "executions": executions})),
                    &[("state", &art)],
                )
                .await;
        }
    })
}

/// One execution per domain `requestId`: the endpoint owns the result artifact and replays it
/// from its own hold. Returns whether the reply was routed to a still-attached caller.
async fn serve_cached(
    server: &Client,
    req: &flybus::Request,
    cache: &mut HashMap<String, flybus::Artifact>,
    executions: &mut u64,
) -> bool {
    let rid = req.payload()["requestId"].as_str().unwrap().to_owned();
    if !cache.contains_key(&rid) {
        *executions += 1;
        let bytes = format!("state of {rid}").into_bytes();
        cache.insert(rid.clone(), sealed(server, &bytes, "text/plain").await);
    }
    let art = cache[&rid].clone();
    req.reply(obj(json!({"executions": *executions})), &[("state", &art)])
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// BUS-01

/// BUS-01: "lost result", and BUS-03: "lost replies and cache replay remain valid".
///
/// The caller never reads its admitted result and then loses its connection. The router keeps
/// no result cache of its own, leaks no root, and the endpoint's own hold still replays the
/// same bytes for a repeat of the domain request.
async fn a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let svc = server
        .register("agent.lossy", ServiceConfig::default())
        .await
        .unwrap();
    let handler = spawn_cache_service(server.clone(), svc);

    // A caller that admits a call and never reads the result delivery.
    let mut raw = e.raw_hello("caller").await;
    let accepted = raw
        .call(
            "rpc.call",
            json!({
                "callId": "call-1", "target": "agent.lossy", "expectedIncarnation": null,
                "method": "Agent.Prepare", "payload": {"requestId": "req-41"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(accepted["accepted"], json!(true));
    // Two roots: the endpoint's cache hold and the caller's result.
    e.settle("result admitted", |s| {
        s.sealed_artifacts == 1 && s.artifact_roots == 2
    })
    .await;
    drop(raw);
    e.settle("the lost result leaks nothing", |s| {
        s.calls == 0 && s.artifact_roots == 1 && s.owners == 1 && s.sealed_artifacts == 1
    })
    .await;

    // The domain retry returns the cached artifact, still readable.
    let caller = e.client("retry").await;
    let res = within(
        "cache replay",
        caller.call_and_wait(
            "agent.lossy",
            None,
            "Agent.Prepare",
            obj(json!({"requestId": "req-41"})),
            &[],
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        res.outcome()["executions"], 1,
        "the lost result was recomputed"
    );
    let art = res.artifact("state").unwrap();
    assert_eq!(art.read_all().await.unwrap(), b"state of req-41");
    drop((art, res));
    handler.abort();
}

/// BUS-01: "retransmission fixtures". The same domain request body is sent twice under two
/// bus call ids, pinned to one service incarnation; the endpoint executes once (bus-v1
/// section 6, ipc-v1 section 3).
async fn a_retransmission_repeats_the_domain_request_under_a_fresh_call_id(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("agent.retried", ServiceConfig::default())
        .await
        .unwrap();
    let incarnation = svc.incarnation().to_owned();
    // The fixture: one domain body, sent twice, byte for byte.
    let body = obj(json!({"requestId": "req-41", "params": {"step": "41"}}));

    let first = caller
        .call(
            "agent.retried",
            Some(&incarnation),
            "Agent.Prepare",
            body.clone(),
            &[],
        )
        .await
        .unwrap();
    let first_call_id = first.call_id().to_owned();
    let attempt = within("first attempt", svc.next()).await.unwrap();
    assert_eq!(attempt.payload(), &body);
    // The caller gives up. Cancelling after dispatch cannot undo the work.
    assert_eq!(first.cancel().await.unwrap(), CancelState::ExecutionUnknown);

    // The endpoint finishes anyway and caches the result; the reply reaches nobody.
    let mut executions = 0u64;
    let mut cache: HashMap<String, flybus::Artifact> = HashMap::new();
    assert_eq!(attempt.call_id(), first_call_id);
    let routed = serve_cached(&server, &attempt, &mut cache, &mut executions).await;
    assert!(!routed, "the detached caller was still reachable");
    drop(attempt);

    // The retry: a new bus call id, the original domain body, the same pinned incarnation.
    let mut second = caller
        .call(
            "agent.retried",
            Some(&incarnation),
            "Agent.Prepare",
            body.clone(),
            &[],
        )
        .await
        .unwrap();
    assert_ne!(
        second.call_id(),
        first_call_id,
        "a safe retry uses a fresh bus call id"
    );
    assert_eq!(second.service_incarnation(), incarnation);
    let repeat = within("retry", svc.next()).await.unwrap();
    assert_eq!(repeat.payload(), &body, "the domain body changed");
    assert_ne!(repeat.call_id(), first_call_id);
    assert!(serve_cached(&server, &repeat, &mut cache, &mut executions).await);
    drop(repeat);
    let res = within("retry result", second.result()).await.unwrap();
    assert_eq!(res.outcome()["executions"], 1, "the retry re-executed");
    assert_eq!(
        res.artifact("state").unwrap().read_all().await.unwrap(),
        b"state of req-41"
    );
    assert_eq!(executions, 1);
    drop(res);
    drop(cache);
}

/// BUS-01: "no automatic retry/failover". Neither a queued nor a dispatched call is replayed
/// onto a replacement registration, and an old pinned incarnation fails rather than reaching
/// the new holder (bus-v1 sections 3 and 6).
async fn no_automatic_retry_or_failover_onto_a_replacement_registration(via: Via) {
    let e = env(via).await;
    let first_host = e.client("first").await;
    let second_host = e.client("second").await;
    let caller = e.client("caller").await;
    let mut svc = first_host
        .register(
            "agent.fly-a",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let old = svc.incarnation().to_owned();
    let mut dispatched = caller
        .call("agent.fly-a", Some(&old), "Agent.Prepare", obj(json!({"n": 1})), &[])
        .await
        .unwrap();
    let held = within("request", svc.next()).await.unwrap();
    let mut queued = caller
        .call("agent.fly-a", Some(&old), "Agent.Prepare", obj(json!({"n": 2})), &[])
        .await
        .unwrap();

    // The worker goes away with one call dispatched and one still queued. Unregister first,
    // while the request credit is still held, so the queued call cannot be dispatched.
    drop(svc);
    let q = within("queued call", queued.result()).await.unwrap_err();
    assert_eq!(
        (q.code, q.dispatch),
        (ErrorCode::NoService, Dispatch::NotDispatched)
    );
    drop(held);
    first_host.close().await;
    let d = within("dispatched call", dispatched.result())
        .await
        .unwrap_err();
    assert_eq!(d.dispatch, Dispatch::Dispatched, "{d}");
    assert!(
        matches!(d.code, ErrorCode::NoService | ErrorCode::CallGone),
        "{d}"
    );

    // A restarted worker takes the name. Nothing is replayed onto it.
    let mut replacement = second_host
        .register("agent.fly-a", ServiceConfig::default())
        .await
        .unwrap();
    assert_ne!(replacement.incarnation(), old);
    quiet("a retry onto the replacement", replacement.next()).await;
    let pinned = caller
        .call("agent.fly-a", Some(&old), "Agent.Prepare", obj(json!({"n": 3})), &[])
        .await
        .unwrap_err();
    assert_eq!(pinned.code, ErrorCode::TargetChanged);
    assert_eq!(pinned.dispatch, Dispatch::NotDispatched);

    // Only the caller's own fresh call reaches the new incarnation.
    let mut fresh = caller
        .call(
            "agent.fly-a",
            Some(replacement.incarnation()),
            "Agent.Prepare",
            obj(json!({"n": 1})),
            &[],
        )
        .await
        .unwrap();
    let req = within("fresh request", replacement.next()).await.unwrap();
    assert_eq!(req.payload()["n"], 1);
    assert!(req.reply(obj(json!({"ok": true})), &[]).await.unwrap());
    assert_eq!(
        within("fresh result", fresh.result())
            .await
            .unwrap()
            .outcome()["ok"],
        true
    );
}

/// BUS-01: "status RPC can respond while another handler is delayed". One service, two calls:
/// the dispatcher answers the status call concurrently with an open mutation, and the mutation
/// completes out of order afterwards (bus-v1 section 6).
async fn a_status_rpc_responds_while_another_handler_is_delayed(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "agent.fly-a",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 4,
            },
        )
        .await
        .unwrap();
    let mut advance = caller
        .call(
            "agent.fly-a",
            None,
            "Environment.Advance",
            obj(json!({"step": "41"})),
            &[],
        )
        .await
        .unwrap();
    let delayed = within("advance request", svc.next()).await.unwrap();

    let mut status = caller
        .call("agent.fly-a", None, "Worker.Status", obj(json!({})), &[])
        .await
        .unwrap();
    let status_req = within("status request", svc.next()).await.unwrap();
    assert_eq!(status_req.method(), "Worker.Status");
    assert!(
        status_req
            .reply(obj(json!({"phase": "advancing"})), &[])
            .await
            .unwrap()
    );
    let answered = within("status result", status.result()).await.unwrap();
    assert_eq!(answered.outcome()["phase"], "advancing");
    drop((answered, status_req));

    assert!(
        tokio::time::timeout(Duration::from_millis(150), advance.result())
            .await
            .is_err(),
        "the delayed handler answered early"
    );
    assert!(
        delayed
            .reply(obj(json!({"step": "41"})), &[])
            .await
            .unwrap()
    );
    let done = within("advance result", advance.result()).await.unwrap();
    assert_eq!(done.outcome()["step"], "41");
    drop((done, delayed));
    e.settle("calls retired", |s| s.calls == 0 && s.owners == 0)
        .await;
}

// ---------------------------------------------------------------------------------------------
// BUS-03

/// BUS-03: "disconnect releases logical ownership without mutating still-mapped bytes"
/// (bus-v1 section 8.4). The consumer's connection ends while it still has the sealed file
/// open; the router reclaims every logical root and unlinks the file, and the open handle
/// still reads the original bytes.
async fn disconnect_releases_logical_ownership_without_mutating_open_bytes(via: Via) {
    let e = env(via).await;
    let producer = e.client("producer").await;
    let consumer = e.client("consumer").await;
    producer
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut sub = consumer
        .subscribe("world.demo.frame", SubscriptionConfig::latest())
        .await
        .unwrap();
    let pixels: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let frame = sealed(&producer, &pixels, "image/x-rgba").await;
    producer
        .publish(
            "world.demo.frame",
            obj(json!({"n": 1})),
            &[("frame", &frame)],
        )
        .await
        .unwrap();
    drop(frame);

    let msg = within("frame", sub.next()).await.unwrap();
    let image = msg.artifact("frame").unwrap();
    drop(msg);
    let mut file = image.open().await.unwrap();
    let mut head = vec![0u8; 16];
    file.read_exact(&mut head).unwrap();
    assert_eq!(head, pixels[..16]);
    assert_eq!(e.files("sealed"), 1);

    // The connection ends with the file still open.
    drop((sub, image));
    consumer.close().await;
    e.settle("logical ownership released", |s| {
        s.owners == 0 && s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
    e.settle_files("sealed", 0).await;

    // Reclaiming the registry entry did not touch the inode.
    let mut rest = Vec::new();
    file.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, pixels[16..]);
    assert_eq!(file.len(), pixels.len() as u64);
}

// ---------------------------------------------------------------------------------------------
// BUS-01: both transports produce equivalent behaviour traces

/// An RPC scenario: registration, admission, FIFO dispatch, service backpressure, cancel
/// before dispatch, reply and result, and retirement.
async fn rpc_trace(e: &Env, t: &Trace) {
    let server = e.client("trace-server").await;
    let caller = e.client("trace-caller").await;
    let mut svc = server
        .register(
            "agent.traced",
            ServiceConfig {
                max_queued: 1,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    t.record("service registered");
    let mut first = caller
        .call(
            "agent.traced",
            Some(svc.incarnation()),
            "Agent.Prepare",
            obj(json!({"n": 1})),
            &[],
        )
        .await
        .unwrap();
    t.record("call admitted n=1");
    let req = within("traced request", svc.next()).await.unwrap();
    t.record(format!(
        "request method={} n={} caller={}",
        req.method(),
        req.payload()["n"],
        req.caller().client_id
    ));
    let mut queued = caller
        .call(
            "agent.traced",
            Some(svc.incarnation()),
            "Agent.Prepare",
            obj(json!({"n": 2})),
            &[],
        )
        .await
        .unwrap();
    t.record("call admitted n=2");
    let refused = caller
        .call(
            "agent.traced",
            Some(svc.incarnation()),
            "Agent.Prepare",
            obj(json!({"n": 3})),
            &[],
        )
        .await
        .unwrap_err();
    t.record(format!(
        "call refused {} {}",
        refused.code,
        refused.dispatch.as_str()
    ));
    t.record(format!("cancel state={:?}", queued.cancel().await.unwrap()));
    let gone = within("cancelled result", queued.result()).await.unwrap_err();
    t.record(format!(
        "cancelled result {} {}",
        gone.code,
        gone.dispatch.as_str()
    ));
    t.record(format!(
        "reply routed={}",
        req.reply(obj(json!({"prepared": 1})), &[]).await.unwrap()
    ));
    let res = within("traced result", first.result()).await.unwrap();
    t.record(format!(
        "result prepared={} responder={}",
        res.outcome()["prepared"],
        res.responder().client_id
    ));
    drop((res, req));
    let s = e
        .settle("traced calls retired", |s| s.calls == 0 && s.owners == 0)
        .await;
    t.record(format!(
        "retired calls={} active={} owners={}",
        s.calls, s.active_calls, s.owners
    ));
}

/// A pub/sub and artifact scenario: retained declaration, a bounded and a latest subscriber,
/// a late replaying subscriber, latest replacement of an undelivered value, an extracted
/// artifact outliving its message, an explicit hold, clear, delete and collection.
async fn pubsub_artifact_trace(e: &Env, t: &Trace) {
    let topic = "session.demo.snapshots";
    let producer = e.client("trace-producer").await;
    let reader = e.client("trace-reader").await;
    let spectator = e.client("trace-spectator").await;
    let latecomer = e.client("trace-latecomer").await;
    let declared = producer.declare_topic(topic, Retained::Latest).await.unwrap();
    t.record(format!("topic declared={}", declared.declared));

    let mut bounded = reader
        .subscribe(topic, SubscriptionConfig::bounded().queued(4).in_flight(4))
        .await
        .unwrap();
    // One credit only: while its message is held, the next publication queues and the one
    // after that replaces it.
    let mut latest = spectator
        .subscribe(topic, SubscriptionConfig::latest().in_flight(1))
        .await
        .unwrap();
    t.record("two subscriptions");

    let first = sealed(&producer, b"snapshot-1", "application/octet-stream").await;
    let r1 = producer
        .publish(topic, obj(json!({"step": "1"})), &[("state", &first)])
        .await
        .unwrap();
    t.record(format!(
        "publish seq={} subscribers={} replaced={}",
        r1.topic_sequence, r1.subscribers, r1.replaced
    ));
    drop(first);

    let m1 = within("bounded 1", bounded.next()).await.unwrap();
    t.record(format!(
        "bounded seq={} replaced={} step={} attachments=[{}]",
        m1.topic_sequence(),
        m1.replaced(),
        m1.payload()["step"],
        m1.attachment_names().collect::<Vec<_>>().join(",")
    ));
    let state = m1.artifact("state").unwrap();
    drop(m1);
    // The extracted handle keeps the delivery alive past the message object.
    let bytes = state.read_all().await.unwrap();
    t.record(format!("artifact bytes={}", bytes.len()));
    let kept = state.retain().await.unwrap();
    drop(state);
    t.record("explicit hold taken");

    let held = within("latest 1", latest.next()).await.unwrap();
    t.record(format!(
        "latest seq={} replaced={}",
        held.topic_sequence(),
        held.replaced()
    ));

    let mut replaying = latecomer
        .subscribe(
            topic,
            SubscriptionConfig::bounded().queued(4).in_flight(4).replay(true),
        )
        .await
        .unwrap();
    let replayed = within("replay", replaying.next()).await.unwrap();
    t.record(format!(
        "replayed seq={} step={}",
        replayed.topic_sequence(),
        replayed.payload()["step"]
    ));
    drop(replayed);

    let second = sealed(&producer, b"snapshot-2", "application/octet-stream").await;
    let r2 = producer
        .publish(topic, obj(json!({"step": "2"})), &[("state", &second)])
        .await
        .unwrap();
    t.record(format!(
        "publish seq={} subscribers={} replaced={}",
        r2.topic_sequence, r2.subscribers, r2.replaced
    ));
    drop(second);
    for (who, sub) in [("bounded", &mut bounded), ("replaying", &mut replaying)] {
        let m = within("second delivery", sub.next()).await.unwrap();
        t.record(format!(
            "{who} seq={} replaced={} step={}",
            m.topic_sequence(),
            m.replaced(),
            m.payload()["step"]
        ));
    }

    // The latest subscriber still holds its only credit, so this replaces its queued value.
    let r3 = producer
        .publish(topic, obj(json!({"step": "3"})), &[])
        .await
        .unwrap();
    t.record(format!(
        "publish seq={} subscribers={} replaced={}",
        r3.topic_sequence, r3.subscribers, r3.replaced
    ));
    for (who, sub) in [("bounded", &mut bounded), ("replaying", &mut replaying)] {
        let m = within("third delivery", sub.next()).await.unwrap();
        t.record(format!(
            "{who} seq={} replaced={} step={}",
            m.topic_sequence(),
            m.replaced(),
            m.payload()["step"]
        ));
    }
    drop(held);
    let coalesced = within("latest 2", latest.next()).await.unwrap();
    t.record(format!(
        "latest seq={} replaced={} step={}",
        coalesced.topic_sequence(),
        coalesced.replaced(),
        coalesced.payload()["step"]
    ));
    drop(coalesced);

    t.record(format!(
        "cleared={}",
        producer.clear_topic(topic).await.unwrap()
    ));
    drop((bounded, latest, replaying));
    e.settle("unsubscribed", |s| s.subscriptions == 0).await;
    t.record(format!(
        "deleted={}",
        producer.delete_topic(topic).await.unwrap()
    ));
    drop(kept);
    let s = e
        .settle("traced artifacts collected", |s| {
            s.artifacts == 0 && s.store_bytes == 0 && s.owners == 0
        })
        .await;
    t.record(format!(
        "collected artifacts={} roots={} owners={} retained_bytes={}",
        s.artifacts, s.artifact_roots, s.owners, s.retained_bytes
    ));
    e.settle_files("sealed", 0).await;
    t.record("store empty");
}

async fn behaviour_trace(via: Via) -> Vec<String> {
    let e = env(via).await;
    let t = Trace::new();
    rpc_trace(&e, &t).await;
    pubsub_artifact_trace(&e, &t).await;
    t.events()
}

/// BUS-01: "both transports produce equivalent behavior traces for the same scenario". The
/// trace records behaviour only: methods, payload fields, counts, sequences, credits, states
/// and error codes, never a router-issued id, a path or a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_transports_produce_equivalent_behaviour_traces() {
    let memory = behaviour_trace(Via::Memory).await;
    let unix = behaviour_trace(Via::Unix).await;
    if std::env::var_os("FLYBUS_TRACE").is_some() {
        for (i, event) in memory.iter().enumerate() {
            println!("{i:3}  {event}");
        }
    }
    assert!(memory.len() >= 25, "a thin trace: {memory:#?}");
    assert_eq!(
        memory, unix,
        "the in-memory and Unix-socket traces disagree\nmemory: {memory:#?}\nunix: {unix:#?}"
    );
}

both_transports!(
    a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays,
    a_retransmission_repeats_the_domain_request_under_a_fresh_call_id,
    no_automatic_retry_or_failover_onto_a_replacement_registration,
    a_status_rpc_responds_while_another_handler_is_delayed,
    disconnect_releases_logical_ownership_without_mutating_open_bytes,
);
