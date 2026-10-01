//! The grant table refuses what it should, over the router's real sockets.
//!
//! - each native role calls exactly its families, and a refused call never reaches the loop;
//! - no role can register a service (so none can stand in for the control host), publish,
//!   subscribe or manage a topic;
//! - a socket is bound to its role: claiming another identity on it is refused at hello;
//! - on a router shared with a service that does take input (an environment's, say), no control
//!   participant can call it: "nothing can press buttons" holds for the grant table, not only for
//!   the list of control methods;
//! - per-agent scoping: a player may sugar their own fly and not another.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::http::{Method, StatusCode};
use common::{exchange, rig, rig_with};
use flybus::{
    BusError, Client, ClientConfig, Dispatch, ErrorCode, Grants, Pattern, Policy, Retained, Router,
    RouterConfig, ServiceConfig,
};
use flysim::bus::Uses;
use flysim::control::ControlRequest;
use flysim::controlbus::{self, Family, Role, Scope};
use serde_json::{Map, json};

fn request_of(family: Family) -> ControlRequest {
    match family {
        Family::Read | Family::Status => ControlRequest::Status,
        Family::Sugar => ControlRequest::Stimulate {
            body: Some(json!({"by": "viewer", "source": "points"})),
        },
        Family::Reward => ControlRequest::Reward {
            body: Some(json!({"value": 1.0, "by": "op", "source": "operator"})),
        },
        Family::Chat => ControlRequest::Chat {
            body: Some(json!({"by": "viewer", "text": "hi"})),
        },
        Family::Ops => ControlRequest::Resume,
    }
}

async fn call(
    client: &Client,
    scope: &Scope,
    family: Family,
    agent: &str,
) -> Result<Map<String, serde_json::Value>, BusError> {
    let (encoded_family, method, payload) = controlbus::encode_request(&request_of(family));
    // An agent's status service answers the same method the session's read does.
    if family != Family::Status {
        assert_eq!(encoded_family, family);
    }
    let service = scope.service(family, agent);
    let result = client
        .call_and_wait(&service, None, method, payload, &[])
        .await?;
    Ok(result.outcome().clone())
}

fn assert_refused(result: Result<impl std::fmt::Debug, BusError>, what: &str) {
    let error = result.expect_err(what);
    assert_eq!(error.code, ErrorCode::NotAuthorized, "{what}: {error}");
    assert_eq!(error.dispatch, Dispatch::NotDispatched, "{what}");
}

async fn connect_role(
    bus_dir: &std::path::Path,
    role: Role,
    claimed: &str,
) -> Result<Client, BusError> {
    Client::connect_unix(
        controlbus::socket_path(bus_dir, role),
        ClientConfig::new(claimed, flysim::feedbus::store_root(bus_dir)),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_role_calls_exactly_its_families_and_nothing_else() {
    let rig = rig_with(
        |_| {},
        Uses {
            feed: true,
            control: true,
        },
    )
    .await;
    rig.close_edge().await;
    let bus_dir = rig.config.bus_dir.clone();
    let scope = Scope::live();
    for role in Role::ALL {
        let client = connect_role(&bus_dir, role, role.participant())
            .await
            .unwrap();
        for family in Family::ALL {
            let before = rig.sim.received.lock().unwrap().len();
            let result = call(&client, &scope, family, controlbus::LIVE_AGENT).await;
            if role.families().contains(&family) {
                let outcome = result.unwrap_or_else(|e| panic!("{role:?} {family:?}: {e}"));
                assert!(outcome["status"].as_u64().is_some(), "{role:?} {family:?}");
            } else {
                assert_refused(result, &format!("{role:?} {family:?}"));
                assert_eq!(
                    rig.sim.received.lock().unwrap().len(),
                    before,
                    "{role:?} {family:?}: a refused call reached the loop"
                );
            }
        }

        // Nobody but the in-process host may register anything, a control name least of all.
        for name in [
            scope.service(Family::Ops, ""),
            scope.service(Family::Sugar, controlbus::LIVE_AGENT),
            "fly.control.s.main.buttons".to_owned(),
            "anything".to_owned(),
        ] {
            assert_refused(
                client
                    .register(&name, ServiceConfig::default())
                    .await
                    .map(|_| ()),
                &format!("{role:?} registering {name}"),
            );
        }
        // No topics: not the feed, not a new one.
        assert_refused(
            client
                .subscribe(flysim::feedbus::TOPIC, flybus::SubscriptionConfig::latest())
                .await
                .map(|_| ()),
            &format!("{role:?} subscribing to the feed"),
        );
        assert_refused(
            client
                .declare_topic("fly.control.events", Retained::None)
                .await
                .map(|_| ()),
            &format!("{role:?} declaring a topic"),
        );
        assert_refused(
            client
                .publish(flysim::feedbus::TOPIC, Map::new(), &[])
                .await
                .map(|_| ()),
            &format!("{role:?} publishing on the feed"),
        );
        client.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_admits_only_its_own_role() {
    let rig = rig(|_| {}).await;
    let bus_dir = rig.config.bus_dir.clone();
    for (socket, claimed) in [
        (Role::Stage, Role::Operator.participant()),
        (Role::Bridge, controlbus::HOST),
        (Role::Watchdog, controlbus::EDGE),
        (Role::Edge, Role::Operator.participant()),
    ] {
        let result = connect_role(&bus_dir, socket, claimed).await;
        assert!(
            result.is_err(),
            "{claimed} was admitted on {socket:?}'s socket"
        );
    }
    // No socket is bound to the host.
    assert!(
        !bus_dir.join("control").join("flysim-control.sock").exists()
            && std::fs::read_dir(bus_dir.join("control")).unwrap().count() == Role::NATIVE.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_shared_router_no_control_participant_can_reach_an_input_service() {
    let dir = tempfile::tempdir().unwrap();
    let scope = Scope::live();
    // An environment that takes input lives on the same router as the control services.
    let input = "fly.env.s.main.input";
    let policy = controlbus::policy(
        Policy::closed().client(
            "env",
            Grants {
                register: vec![Pattern::exact(input)],
                ..Grants::default()
            },
        ),
        &scope,
    );
    let mut config = RouterConfig::new(dir.path());
    config.policy = policy;
    let router = Router::new(config).unwrap();
    let env = Client::connect(
        router.connect_in_memory_as("env"),
        ClientConfig::new("env", router.store_root()),
    )
    .await
    .unwrap();
    let mut service = env.register(input, ServiceConfig::default()).await.unwrap();
    let pressed = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let pressed = Arc::clone(&pressed);
        tokio::spawn(async move {
            while let Some(request) = service.next().await {
                pressed.fetch_add(1, Ordering::SeqCst);
                let _ = request.reply(Map::new(), &[]).await;
            }
        });
    }
    for role in Role::ALL {
        let client = Client::connect(
            router.connect_in_memory_as(role.participant()),
            ClientConfig::new(role.participant(), router.store_root()),
        )
        .await
        .unwrap();
        for method in ["Environment.Step", "Control.Stimulate", "Press"] {
            let mut payload = Map::new();
            payload.insert("buttons".into(), json!(1));
            assert_refused(
                client
                    .call(input, None, method, payload, &[])
                    .await
                    .map(|_| ()),
                &format!("{role:?} calling {input} {method}"),
            );
        }
        client.close().await;
    }
    assert_eq!(pressed.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_sugars_their_own_fly_and_no_other() {
    // Two flies on two controller ports; one player backs the fly on port 1 and another is
    // granted by id. Single-fly is the same table with one agent.
    let dir = tempfile::tempdir().unwrap();
    let scope = Scope {
        session: "arena".to_owned(),
        agents: vec![
            controlbus::AgentScope::new("fly-a", Some(1)),
            controlbus::AgentScope::new("fly-b", Some(2)),
        ],
    };
    let policy = controlbus::policy(Policy::closed(), &scope)
        .client(
            "player-a",
            controlbus::player_grants(&scope, controlbus::Agents::Port(1)),
        )
        .client(
            "player-b",
            controlbus::player_grants(&scope, controlbus::Agents::Id("fly-b")),
        );
    let mut config = RouterConfig::new(dir.path());
    config.policy = policy;
    let router = Router::new(config).unwrap();
    let rig = rig(|_| {}).await;
    let _host = controlbus::serve(&router, rig.state.clone(), &scope)
        .await
        .unwrap();
    let player = Client::connect(
        router.connect_in_memory_as("player-a"),
        ClientConfig::new("player-a", router.store_root()),
    )
    .await
    .unwrap();
    let other = Client::connect(
        router.connect_in_memory_as("player-b"),
        ClientConfig::new("player-b", router.store_root()),
    )
    .await
    .unwrap();
    assert_eq!(
        call(&other, &scope, Family::Sugar, "fly-b").await.unwrap()["status"],
        json!(202)
    );
    assert_refused(
        call(&other, &scope, Family::Sugar, "fly-a").await,
        "player-b sugar on fly-a",
    );
    assert_eq!(
        call(&player, &scope, Family::Status, "fly-a").await.unwrap()["status"],
        json!(200)
    );
    assert_refused(
        call(&player, &scope, Family::Status, "fly-b").await,
        "status of fly-b",
    );
    let outcome = call(&player, &scope, Family::Sugar, "fly-a").await.unwrap();
    assert_eq!(outcome["status"], json!(202));
    assert_eq!(outcome["body"], json!({"eventId": 41}));
    assert_eq!(
        call(&player, &scope, Family::Read, "").await.unwrap()["status"],
        json!(200)
    );
    assert_refused(
        call(&player, &scope, Family::Sugar, "fly-b").await,
        "sugar on fly-b",
    );
    assert_refused(
        call(&player, &scope, Family::Reward, "fly-a").await,
        "reward on fly-a",
    );
    assert_refused(call(&player, &scope, Family::Chat, "").await, "chat");
    assert_refused(call(&player, &scope, Family::Ops, "").await, "ops");
    // The live session's names are not the arena's.
    assert_refused(
        call(&player, &Scope::live(), Family::Read, "").await,
        "another session's read",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_method_the_service_does_not_have_is_a_404_and_reaches_nothing() {
    let rig = rig(|_| {}).await;
    let operator = connect_role(
        &rig.config.bus_dir,
        Role::Operator,
        Role::Operator.participant(),
    )
    .await
    .unwrap();
    let scope = Scope::live();
    for (family, method) in [
        (Family::Read, "Control.Pause"),
        (Family::Sugar, "Control.Press"),
        (Family::Ops, "Control.Buttons"),
        (Family::Chat, "Control.Stimulate"),
    ] {
        let result = operator
            .call_and_wait(
                &scope.service(family, controlbus::LIVE_AGENT),
                None,
                method,
                Map::new(),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            result.outcome()["status"],
            json!(404),
            "{family:?} {method}"
        );
    }
    assert!(rig.sim.received.lock().unwrap().is_empty());
}

/// An HTTP edge holding a narrower role answers what the role may not do with a 403, and the
/// loop never sees it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edge_on_a_narrow_role_refuses_over_http() {
    let rig = rig(|_| {}).await;
    let stage = connect_role(&rig.config.bus_dir, Role::Stage, Role::Stage.participant())
        .await
        .unwrap();
    let backend = fly_control_edge::BusBackend::new(stage, Scope::live(), Arc::clone(&rig.metrics));
    let router = || flysim::api::router_with(backend.clone());
    let ok = exchange(router(), &Method::GET, "/healthz", None, &[]).await;
    assert_eq!(ok.status, StatusCode::OK);
    for (uri, body) in [
        ("/stimulate", json!({"by": "v", "source": "chat"})),
        ("/chat", json!({"by": "v", "text": "hi"})),
        ("/pause", json!({})),
        ("/checkpoint", json!({})),
    ] {
        let refused = exchange(
            router(),
            &Method::POST,
            uri,
            Some("application/json"),
            &serde_json::to_vec(&body).unwrap(),
        )
        .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{uri}");
        assert!(
            refused.json()["error"]
                .as_str()
                .unwrap()
                .starts_with("not authorized on the control bus"),
            "{uri}"
        );
    }
    assert!(rig.sim.received.lock().unwrap().is_empty());
}
