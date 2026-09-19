//! bus-v1 section 11 item 6: two parallel fake agents, complete-batch environment RPC,
//! committed snapshot publication and a deliberately slow presentation consumer, all over one
//! router. Generic services only; nothing here knows what a brain or a game is.

mod common;

use std::io::Write;
use std::time::Duration;

use common::{Via, env_with, obj, within};
use flybus::{
    Client, ErrorCode, Grants, Limits, Pattern, Policy, Retained, ServiceConfig, SubscriptionConfig,
};
use serde_json::json;

const STEPS: u64 = 20;
const FRAME: usize = 160 * 144 * 4;

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

/// An environment service: each Advance produces a frame artifact filled with the step number.
fn spawn_environment(client: Client, mut svc: flybus::Service) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(req) = svc.next().await {
            let step = req.payload()["step"].as_u64().unwrap();
            let mut w = client
                .artifacts()
                .allocate(FRAME as u64, "image/x-rgba")
                .await
                .unwrap();
            w.write_all(&vec![step as u8; FRAME]).unwrap();
            let frame = w.seal().await.unwrap();
            req.reply(
                obj(json!({"step": step, "width": 160, "height": 144})),
                &[("frame", &frame)],
            )
            .await
            .unwrap();
        }
    })
}

/// An agent service: checks the frame it was sent and answers with a digest of it.
fn spawn_agent(name: &'static str, mut svc: flybus::Service) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(req) = svc.next().await {
            let step = req.payload()["step"].as_u64().unwrap();
            let bytes = req.artifact("frame").unwrap().read_all().await.unwrap();
            let ok = bytes.len() == FRAME && bytes.iter().all(|b| *b == step as u8);
            req.reply(
                obj(json!({"agent": name, "step": step, "frameOk": ok})),
                &[],
            )
            .await
            .unwrap();
        }
    })
}

async fn session_over_one_router(via: Via) {
    let policy = Policy::closed()
        .client(
            "coordinator",
            grants(|g| {
                g.call = vec![Pattern::prefix("agent."), Pattern::exact("env.demo")];
                g.publish = vec![Pattern::prefix("session.demo.")];
                g.manage_topics = vec![Pattern::prefix("session.demo.")];
            }),
        )
        .client(
            "agent-a",
            grants(|g| g.register = vec![Pattern::exact("agent.fly-a")]),
        )
        .client(
            "agent-b",
            grants(|g| g.register = vec![Pattern::exact("agent.fly-b")]),
        )
        .client(
            "environment",
            grants(|g| g.register = vec![Pattern::exact("env.demo")]),
        )
        .client(
            "presenter",
            grants(|g| g.subscribe = vec![Pattern::prefix("session.demo.")]),
        )
        .client(
            "recorder",
            grants(|g| g.subscribe = vec![Pattern::prefix("session.demo.")]),
        );
    let e = env_with(via, Limits::default(), policy).await;

    let environment = e.client("environment").await;
    let env_svc = environment
        .register(
            "env.demo",
            ServiceConfig {
                max_queued: 4,
                max_in_flight: 1,
            },
        )
        .await
        .unwrap();
    let env_inc = env_svc.incarnation().to_owned();
    let env_task = spawn_environment(environment.clone(), env_svc);
    let a = e.client("agent-a").await;
    let b = e.client("agent-b").await;
    let a_svc = a
        .register("agent.fly-a", ServiceConfig::default())
        .await
        .unwrap();
    let b_svc = b
        .register("agent.fly-b", ServiceConfig::default())
        .await
        .unwrap();
    let (a_inc, b_inc) = (
        a_svc.incarnation().to_owned(),
        b_svc.incarnation().to_owned(),
    );
    let agents = [spawn_agent("fly-a", a_svc), spawn_agent("fly-b", b_svc)];

    let coordinator = e.client("coordinator").await;
    coordinator
        .declare_topic("session.demo.snapshots", Retained::Latest)
        .await
        .unwrap();

    let presenter = e.client("presenter").await;
    // Observers cannot drive the environment.
    let denied = presenter
        .call(
            "env.demo",
            None,
            "Environment.Advance",
            obj(json!({"step": 0})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::NotAuthorized);
    let mut slow = presenter
        .subscribe(
            "session.demo.snapshots",
            SubscriptionConfig::latest().in_flight(1),
        )
        .await
        .unwrap();
    let presenting = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(m) = slow.next().await {
            let frame = m.artifact("frame").unwrap();
            drop(m);
            tokio::time::sleep(Duration::from_millis(25)).await; // a slow renderer
            let bytes = frame.read_all().await.unwrap();
            let step = bytes[0] as u64;
            seen.push((step, frame.reference().artifact_id.clone()));
            if step == STEPS {
                break;
            }
        }
        seen
    });
    let recorder = e.client("recorder").await;
    let mut all = recorder
        .subscribe("session.demo.snapshots", SubscriptionConfig::bounded())
        .await
        .unwrap();
    let recording = tokio::spawn(async move {
        let mut seq = Vec::new();
        while let Some(m) = all.next().await {
            seq.push((m.topic_sequence(), m.payload()["step"].as_u64().unwrap()));
            if seq.len() as u64 == STEPS {
                break;
            }
        }
        seq
    });

    for step in 1..=STEPS {
        let advanced = coordinator
            .call_and_wait(
                "env.demo",
                Some(&env_inc),
                "Environment.Advance",
                obj(json!({"step": step})),
                &[],
            )
            .await
            .unwrap();
        // Forward the delivery-owned frame to both agents at once.
        let frame = advanced.artifact("frame").unwrap();
        let attachments = [("frame", &frame)];
        let (ra, rb) = tokio::join!(
            coordinator.call_and_wait(
                "agent.fly-a",
                Some(&a_inc),
                "Agent.Prepare",
                obj(json!({"step": step})),
                &attachments
            ),
            coordinator.call_and_wait(
                "agent.fly-b",
                Some(&b_inc),
                "Agent.Prepare",
                obj(json!({"step": step})),
                &attachments
            ),
        );
        for r in [ra.unwrap(), rb.unwrap()] {
            assert_eq!(
                (
                    r.outcome()["step"].as_u64(),
                    r.outcome()["frameOk"].as_bool()
                ),
                (Some(step), Some(true))
            );
        }
        let receipt = coordinator
            .publish(
                "session.demo.snapshots",
                obj(json!({"step": step})),
                &[("frame", &frame)],
            )
            .await
            .unwrap();
        assert_eq!(receipt.topic_sequence, step);
    }

    let recorded = within("recorder", recording).await.unwrap();
    assert_eq!(
        recorded,
        (1..=STEPS).map(|s| (s, s)).collect::<Vec<_>>(),
        "the bounded recorder misses nothing"
    );
    let presented = within("presenter", presenting).await.unwrap();
    assert_eq!(
        presented.last().unwrap().0,
        STEPS,
        "the slow consumer ends on the latest snapshot"
    );
    assert!(
        presented.len() < STEPS as usize,
        "the slow consumer skipped snapshots: {presented:?}"
    );
    assert!(presented.windows(2).all(|w| w[0].0 < w[1].0));

    for t in agents {
        t.abort();
    }
    env_task.abort();
    drop((a, b, environment, presenter, recorder));
    // Only the retained snapshot's frame is left once everyone is gone.
    let s = e
        .settle("session torn down", |s| {
            s.calls == 0 && s.sealed_artifacts == 1 && s.owners == 0
        })
        .await;
    assert_eq!(s.store_bytes, FRAME as u64);
    assert!(
        coordinator
            .clear_topic("session.demo.snapshots")
            .await
            .unwrap()
    );
    e.settle("retained frame collected", |s| s.artifacts == 0)
        .await;
}

both_transports!(session_over_one_router);
