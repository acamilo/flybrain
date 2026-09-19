//! Routing conformance: adversarial contract cases for bus-v1 sections 5-8, deliberately not
//! duplicating the happy paths already covered by `rpc.rs` and `pubsub.rs`. Generic synthetic
//! services and topics only; no application/session semantics. Every test runs over both the
//! in-memory transport and a Unix socket.

mod common;

use common::{Via, env, obj, quiet, sealed, within};
use flybus::{CancelState, Dispatch, ErrorCode, Retained, ServiceConfig, SubscriptionConfig};
use serde_json::json;

/// bus-v1 section 5: "One live registration owns a service name." A client re-registering a
/// name it already owns is a duplicate too, not an idempotent no-op, and the failed attempt
/// must not disturb the live registration.
async fn duplicate_registration_by_owner_itself_is_rejected(via: Via) {
    let e = env(via).await;
    let a = e.client("a").await;
    let caller = e.client("caller").await;
    let mut svc = a
        .register("agent.self", ServiceConfig::default())
        .await
        .unwrap();
    let incarnation = svc.incarnation().to_owned();
    let dup = a
        .register("agent.self", ServiceConfig::default())
        .await
        .unwrap_err();
    assert_eq!(dup.code, ErrorCode::Conflict);
    assert_eq!(
        svc.incarnation(),
        incarnation,
        "the failed self-duplicate did not replace the live registration"
    );
    let mut pending = caller
        .call("agent.self", Some(&incarnation), "M", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    assert!(req.reply(obj(json!({"ok": true})), &[]).await.unwrap());
    assert_eq!(
        within("result", pending.result()).await.unwrap().outcome()["ok"],
        true
    );
}

/// bus-v1 section 3: "Callers pin it after discovery." An unpinned call made *after* the name
/// changes hands must resolve to the new incarnation, never linger on the old one.
async fn unpinned_call_after_incarnation_replacement_reaches_the_new_holder(via: Via) {
    let e = env(via).await;
    let a = e.client("a").await;
    let b = e.client("b").await;
    let caller = e.client("caller").await;
    let first = a
        .register("agent.fly", ServiceConfig::default())
        .await
        .unwrap();
    let old = first.incarnation().to_owned();
    drop(first);
    // Unregistration is asynchronous; wait for the name to come free.
    let mut second = loop {
        match b.register("agent.fly", ServiceConfig::default()).await {
            Ok(s) => break s,
            Err(err) => assert_eq!(err.code, ErrorCode::Conflict),
        }
    };
    assert_ne!(second.incarnation(), old);
    let mut pending = caller
        .call("agent.fly", None, "M", obj(json!({})), &[])
        .await
        .unwrap();
    assert_eq!(
        pending.service_incarnation(),
        second.incarnation(),
        "unpinned discovery resolves to the current holder"
    );
    let req = within("request", second.next()).await.unwrap();
    assert_eq!(req.service_incarnation(), second.incarnation());
    assert!(
        req.reply(obj(json!({"from": "second"})), &[])
            .await
            .unwrap()
    );
    assert_eq!(
        within("result", pending.result()).await.unwrap().outcome()["from"],
        "second"
    );
}

/// bus-v1 section 6: "responses may complete out of order and correlate by callId." Two callers
/// interleave calls to the same service and the service answers in reverse admission order;
/// every result must reach the caller it belongs to, never a sibling's.
async fn out_of_order_replies_correlate_across_concurrent_callers(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let alice = e.client("alice").await;
    let bob = e.client("bob").await;
    let mut svc = server
        .register(
            "example.multi",
            ServiceConfig {
                max_queued: 8,
                max_in_flight: 4,
            },
        )
        .await
        .unwrap();
    let mut a1 = alice
        .call(
            "example.multi",
            None,
            "M",
            obj(json!({"who": "alice", "n": 1})),
            &[],
        )
        .await
        .unwrap();
    let mut b1 = bob
        .call(
            "example.multi",
            None,
            "M",
            obj(json!({"who": "bob", "n": 1})),
            &[],
        )
        .await
        .unwrap();
    let mut a2 = alice
        .call(
            "example.multi",
            None,
            "M",
            obj(json!({"who": "alice", "n": 2})),
            &[],
        )
        .await
        .unwrap();
    let mut b2 = bob
        .call(
            "example.multi",
            None,
            "M",
            obj(json!({"who": "bob", "n": 2})),
            &[],
        )
        .await
        .unwrap();
    let mut reqs = Vec::new();
    for _ in 0..4 {
        reqs.push(within("request", svc.next()).await.unwrap());
    }
    // Reply in the reverse of admission order.
    for r in reqs.iter().rev() {
        let who = r.payload()["who"].clone();
        let n = r.payload()["n"].clone();
        assert!(
            r.reply(obj(json!({"who": who, "n": n})), &[])
                .await
                .unwrap()
        );
    }
    drop(reqs);
    let ra1 = within("a1", a1.result()).await.unwrap();
    let rb1 = within("b1", b1.result()).await.unwrap();
    let ra2 = within("a2", a2.result()).await.unwrap();
    let rb2 = within("b2", b2.result()).await.unwrap();
    assert_eq!(
        (ra1.outcome()["who"].as_str(), ra1.outcome()["n"].clone()),
        (Some("alice"), json!(1))
    );
    assert_eq!(
        (rb1.outcome()["who"].as_str(), rb1.outcome()["n"].clone()),
        (Some("bob"), json!(1))
    );
    assert_eq!(
        (ra2.outcome()["who"].as_str(), ra2.outcome()["n"].clone()),
        (Some("alice"), json!(2))
    );
    assert_eq!(
        (rb2.outcome()["who"].as_str(), rb2.outcome()["n"].clone()),
        (Some("bob"), json!(2))
    );
}

/// bus-v1 section 5: "Queued cancellation releases its queued artifact roots." Not exercised by
/// `cancellation_states`, which never attaches artifacts.
async fn cancel_before_dispatch_releases_queued_artifact_roots(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register(
            "example.holdup",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let _held = caller
        .call("example.holdup", None, "Work", obj(json!({"n": 1})), &[])
        .await
        .unwrap();
    let req1 = within("request 1", svc.next()).await.unwrap();
    let art = sealed(&caller, b"queued-payload", "text/plain").await;
    let queued = caller
        .call(
            "example.holdup",
            None,
            "Work",
            obj(json!({"n": 2})),
            &[("x", &art)],
        )
        .await
        .unwrap();
    drop(art); // only the queued call's own root should keep the bytes alive now
    e.settle("the queued call's attachment is rooted", |s| {
        s.artifacts == 1 && s.artifact_roots >= 1
    })
    .await;
    assert_eq!(
        queued.cancel().await.unwrap(),
        CancelState::CancelledBeforeDispatch
    );
    e.settle("cancelling before dispatch released the queued root", |s| {
        s.artifacts == 0 && s.store_bytes == 0
    })
    .await;
    drop(req1);
}

/// bus-v1 section 6: after a post-dispatch cancel, "a later reply to a detached call returns
/// `routed:false`, with no caller-result roots" — including when that late reply carries an
/// artifact the caller must never see rooted.
async fn cancel_after_dispatch_then_late_reply_with_artifact_is_not_routed(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.late", ServiceConfig::default())
        .await
        .unwrap();
    let mut dispatched = caller
        .call("example.late", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    assert_eq!(
        dispatched.cancel().await.unwrap(),
        CancelState::ExecutionUnknown
    );
    let art = sealed(&server, b"late-with-artifact", "text/plain").await;
    let routed = req.reply(obj(json!({})), &[("a", &art)]).await.unwrap();
    assert!(
        !routed,
        "a detached call must not be routed, artifact attached or not"
    );
    drop((req, art));
    e.settle("no caller-side roots leaked from a detached reply", |s| {
        s.artifacts == 0 && s.owners == 0
    })
    .await;
    let after = within("result of a cancelled call", dispatched.result())
        .await
        .unwrap_err();
    assert_eq!(
        (after.code, after.dispatch),
        (ErrorCode::CallGone, Dispatch::Unknown)
    );
}

/// bus-v1 section 8.4: a caller's full disconnection (not merely dropping its pending future)
/// detaches its dispatched call, and the service itself keeps serving other callers afterward.
async fn caller_disconnect_detaches_dispatched_call_but_service_keeps_serving(via: Via) {
    let e = env(via).await;
    let server = e.client("server").await;
    let caller = e.client("caller").await;
    let mut svc = server
        .register("example.vanish", ServiceConfig::default())
        .await
        .unwrap();
    let pending = caller
        .call("example.vanish", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req = within("request", svc.next()).await.unwrap();
    caller.close().await;
    e.settle("caller gone", |s| s.connections == 1).await;
    drop(pending); // inert now: must not panic or double-release
    let routed = req.reply(obj(json!({"late": true})), &[]).await.unwrap();
    assert!(!routed, "the vanished caller cannot receive the reply");
    drop(req);
    e.settle("everything the vanished caller held is cleaned up", |s| {
        s.calls == 0 && s.owners == 0
    })
    .await;
    let other = e.client("other").await;
    let mut fresh = other
        .call("example.vanish", None, "Do", obj(json!({})), &[])
        .await
        .unwrap();
    let req2 = within("second request", svc.next()).await.unwrap();
    assert!(req2.reply(obj(json!({"ok": true})), &[]).await.unwrap());
    assert_eq!(
        within("result", fresh.result()).await.unwrap().outcome()["ok"],
        true
    );
}

/// bus-v1 section 8.4: disconnect "release[s] that connection's active writers/explicit/delivery
/// roots", for a subscriber holding both delivered-but-unconsumed and still-queued artifacts,
/// while a topic's retained value (owned by the topic, not the connection) survives untouched.
async fn subscriber_disconnect_releases_queued_and_delivered_artifacts_but_not_retention(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let reader = e.client("reader").await;
    publisher
        .declare_topic("t.gone", Retained::Latest)
        .await
        .unwrap();
    let mut sub = reader
        .subscribe(
            "t.gone",
            SubscriptionConfig::bounded().queued(4).in_flight(4),
        )
        .await
        .unwrap();
    for i in 0..3 {
        let a = sealed(&publisher, format!("m{i}").as_bytes(), "text/plain").await;
        publisher
            .publish("t.gone", obj(json!({})), &[("m", &a)])
            .await
            .unwrap();
    }
    let first = within("first delivered", sub.next()).await.unwrap();
    e.settle("three sealed objects, all rooted", |s| {
        s.sealed_artifacts == 3
    })
    .await;
    drop(first);
    reader.close().await;
    e.settle(
        "everything the reader held is gone; only retention remains",
        |s| s.sealed_artifacts == 1 && s.subscriptions == 0 && s.connections == 1,
    )
    .await;
    let art = sealed(&publisher, b"after-disconnect", "text/plain").await;
    publisher
        .publish("t.gone", obj(json!({})), &[("m", &art)])
        .await
        .unwrap();
    drop(art);
    e.settle("retention alone carries the new value", |s| {
        s.sealed_artifacts == 1 && s.artifact_roots == 1
    })
    .await;
}

/// bus-v1 section 7: "reject the whole publish; no partial fan-out or retained-latest update."
/// Not exercised with attachments by `bounded_fifo_and_atomic_backpressure`: a refused publish
/// must leave no artifact root anywhere, on any subscriber. `maxQueued` and `maxInFlight` are
/// separate counters, so filling `tight`'s single in-flight credit does not yet overflow it;
/// its one queue slot has to fill too before a third publish overflows it.
async fn bounded_overflow_rolls_back_all_artifact_roots(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let roomy = e.client("roomy").await;
    let tight = e.client("tight").await;
    publisher
        .declare_topic("t.atomic", Retained::None)
        .await
        .unwrap();
    let mut a = roomy
        .subscribe(
            "t.atomic",
            SubscriptionConfig::bounded().queued(4).in_flight(4),
        )
        .await
        .unwrap();
    let _tight_sub = tight
        .subscribe(
            "t.atomic",
            SubscriptionConfig::bounded().queued(1).in_flight(1),
        )
        .await
        .unwrap();
    let art1 = sealed(&publisher, b"one", "text/plain").await;
    publisher
        .publish("t.atomic", obj(json!({})), &[("x", &art1)])
        .await
        .unwrap(); // fills tight's in-flight credit
    drop(art1);
    let art2 = sealed(&publisher, b"two", "text/plain").await;
    publisher
        .publish("t.atomic", obj(json!({})), &[("x", &art2)])
        .await
        .unwrap(); // fills tight's one queue slot
    drop(art2);
    e.settle("two objects rooted before the overflow attempt", |s| {
        s.sealed_artifacts == 2
    })
    .await;
    let art3 = sealed(&publisher, b"three", "text/plain").await;
    let refused = publisher
        .publish("t.atomic", obj(json!({})), &[("x", &art3)])
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Backpressure);
    drop(art3);
    e.settle(
        "the refused publish left no trace: still exactly the first two objects",
        |s| s.sealed_artifacts == 2,
    )
    .await;
    let m1 = within("a's first", a.next()).await.unwrap();
    assert_eq!(m1.artifact("x").unwrap().read_all().await.unwrap(), b"one");
    let m2 = within("a's second", a.next()).await.unwrap();
    assert_eq!(m2.artifact("x").unwrap().read_all().await.unwrap(), b"two");
    quiet("a never saw the refused publish", a.next()).await;
}

/// bus-v1 section 7: "New subscriptions with replayLatest enqueue it before subsequent accepted
/// publications." A fresh `latest` subscription's replay claims its first in-flight credit
/// immediately (there is nothing else competing for it yet), so a publish accepted right after
/// subscribing must still be observed strictly after the replay, never ahead of or merged with
/// it: each keeps its own delivery.
async fn latest_replay_is_ordered_ahead_of_a_racing_publish(via: Via) {
    let e = env(via).await;
    let admin = e.client("admin").await;
    let reader = e.client("reader").await;
    admin
        .declare_topic("t.replay-race", Retained::Latest)
        .await
        .unwrap();
    admin
        .publish("t.replay-race", obj(json!({"v": "old"})), &[])
        .await
        .unwrap();
    let mut sub = reader
        .subscribe(
            "t.replay-race",
            SubscriptionConfig::latest().in_flight(1).replay(true),
        )
        .await
        .unwrap();
    admin
        .publish("t.replay-race", obj(json!({"v": "new"})), &[])
        .await
        .unwrap();
    let first = within("the replay arrives first", sub.next())
        .await
        .unwrap();
    assert_eq!(first.payload()["v"], "old");
    drop(first); // the sole in-flight credit must return before the queued second value moves
    let second = within("the racing publish follows, not coalesced away", sub.next())
        .await
        .unwrap();
    assert_eq!(second.payload()["v"], "new");
}

/// bus-v1 section 7: clearing releases only the retained root; a later `replayLatest`
/// subscription must see nothing until a fresh publish, not a stale or resurrected value.
async fn cleared_topic_gives_no_replay_until_a_fresh_publish(via: Via) {
    let e = env(via).await;
    let admin = e.client("admin").await;
    let reader = e.client("reader").await;
    admin
        .declare_topic("t.clear-replay", Retained::Latest)
        .await
        .unwrap();
    admin
        .publish("t.clear-replay", obj(json!({"v": 1})), &[])
        .await
        .unwrap();
    assert!(admin.clear_topic("t.clear-replay").await.unwrap());
    let mut sub = reader
        .subscribe("t.clear-replay", SubscriptionConfig::bounded().replay(true))
        .await
        .unwrap();
    quiet("nothing retained to replay after a clear", sub.next()).await;
    admin
        .publish("t.clear-replay", obj(json!({"v": 2})), &[])
        .await
        .unwrap();
    let m = within("a fresh publish arrives normally", sub.next())
        .await
        .unwrap();
    assert_eq!(m.payload()["v"], 2);
}

/// bus-v1 section 8.3: "dropping the message alone does not consume the delivery while a
/// renderer/encoder still uses its artifact." `latest_coalesces_only_undelivered_values` proves
/// this for `latest` mode; delivery-credit accounting must honour it for `bounded` mode too.
async fn bounded_credit_waits_for_every_extracted_artifact(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    let reader = e.client("reader").await;
    publisher
        .declare_topic("t.credit-hold", Retained::None)
        .await
        .unwrap();
    let mut sub = reader
        .subscribe(
            "t.credit-hold",
            SubscriptionConfig::bounded().in_flight(1).queued(4),
        )
        .await
        .unwrap();
    let a0 = sealed(&publisher, b"m0", "text/plain").await;
    let a1 = sealed(&publisher, b"m1", "text/plain").await;
    publisher
        .publish("t.credit-hold", obj(json!({})), &[("x", &a0)])
        .await
        .unwrap();
    publisher
        .publish("t.credit-hold", obj(json!({})), &[("x", &a1)])
        .await
        .unwrap();
    drop((a0, a1));
    let m0 = within("first", sub.next()).await.unwrap();
    let held = m0.artifact("x").unwrap();
    drop(m0); // the message struct is gone, but `held` still shares its delivery guard
    quiet(
        "credit withheld while an extracted artifact is still alive",
        sub.next(),
    )
    .await;
    drop(held);
    let m1 = within("second, only after the real release", sub.next())
        .await
        .unwrap();
    assert_eq!(m1.artifact("x").unwrap().read_all().await.unwrap(), b"m1");
}

/// Subscription ids are serials local to the issuing connection (`sub-<n>`), not a global
/// namespace: a second connection quoting another connection's literal id string has no route
/// to that subscription at all, so it can only ever land on (at most) its own same-numbered
/// subscription, never the owner's. `unsubscribe` reports this as `removed:false`, not an
/// error, and the owner's subscription keeps receiving messages untouched.
async fn unsubscribe_cannot_reach_another_connections_subscription_id(via: Via) {
    let e = env(via).await;
    let publisher = e.client("publisher").await;
    publisher
        .declare_topic("t.owned", Retained::None)
        .await
        .unwrap();
    let mut owner_raw = e.raw_hello("owner").await;
    let sub_id = {
        let r = owner_raw
            .call("subscribe", json!({"topic": "t.owned", "mode": "bounded", "maxQueued": 4, "maxInFlight": 4, "replayLatest": false}))
            .await
            .unwrap();
        r["subscriptionId"].as_str().unwrap().to_owned()
    };
    let mut intruder = e.raw_hello("intruder").await;
    let r = intruder
        .call("unsubscribe", json!({"subscriptionId": sub_id.clone()}))
        .await
        .unwrap();
    assert_eq!(
        r["removed"],
        json!(false),
        "the intruder issued no such subscription itself"
    );

    // The owner's subscription is unaffected by the intruder's attempt.
    let art = sealed(&publisher, b"still-mine", "text/plain").await;
    publisher
        .publish("t.owned", obj(json!({})), &[("x", &art)])
        .await
        .unwrap();
    drop(art);
    let delivered = owner_raw.event().await;
    assert_eq!(delivered.op, "topic.message");

    let removed = owner_raw
        .call("unsubscribe", json!({"subscriptionId": sub_id}))
        .await
        .unwrap();
    assert_eq!(removed["removed"], json!(true));
}

both_transports!(
    duplicate_registration_by_owner_itself_is_rejected,
    unpinned_call_after_incarnation_replacement_reaches_the_new_holder,
    out_of_order_replies_correlate_across_concurrent_callers,
    cancel_before_dispatch_releases_queued_artifact_roots,
    cancel_after_dispatch_then_late_reply_with_artifact_is_not_routed,
    caller_disconnect_detaches_dispatched_call_but_service_keeps_serving,
    subscriber_disconnect_releases_queued_and_delivered_artifacts_but_not_retention,
    bounded_overflow_rolls_back_all_artifact_roots,
    latest_replay_is_ordered_ahead_of_a_racing_publish,
    cleared_topic_gives_no_replay_until_a_fresh_publish,
    bounded_credit_waits_for_every_extracted_artifact,
    unsubscribe_cannot_reach_another_connections_subscription_id,
);
