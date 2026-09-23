//! AGENT-01: the legacy agent worker against the neural core driven directly, on the committed
//! toy connectome, under the launcher in every execution mode.
//!
//! `FLY_UPDATE_FIXTURES=1` rewrites the toy dataset and the goldens; otherwise the committed
//! files must be exactly what this build computes.

use std::sync::Arc;

use fly_session::ExecutionMode;
use fly_session::legacy_agent::{LegacyProfileKind, TOY_FINGERPRINT};
use fly_session::legacy_parity::{
    self, DirectSource, LegacyRig, LegacyScript, ParityRecord, ReferenceSource, RigAgent, toy,
};
use fly_session::types::id;
use flybrain_core::dataset::load_brain_dataset_from_dir;
use serde_json::Value;

fn updating() -> bool {
    std::env::var_os("FLY_UPDATE_FIXTURES").is_some()
}

fn toy_agent(agent_id: &str, script: &LegacyScript) -> RigAgent {
    RigAgent {
        agent_id: id(agent_id),
        port_id: id("p1"),
        dataset_dir: toy::dir(),
        profile: LegacyProfileKind::Toy,
        macro_channels: script.macro_channels.clone(),
        worker_threads: 1,
    }
}

fn reference(script: &LegacyScript) -> Vec<ParityRecord> {
    let dataset = Arc::new(load_brain_dataset_from_dir(toy::dir()).expect("the toy dataset loads"));
    DirectSource { dataset }
        .records(script)
        .expect("the reference runs")
}

async fn on_worker(mode: ExecutionMode, script: &LegacyScript) -> Vec<ParityRecord> {
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut rig = LegacyRig::start(root.path(), mode, &[toy_agent("fly-a", script)])
        .await
        .expect("the rig starts");
    let records = legacy_parity::run_on_worker(&mut rig, &id("fly-a"), script)
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
    rig.stop().await;
    records
}

fn golden(script: &LegacyScript, records: &[ParityRecord]) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{}.golden.json", script.name));
    let mut text =
        serde_json::to_string_pretty(&legacy_parity::golden_json(script, records)).expect("json");
    text.push('\n');
    if updating() {
        std::fs::write(&path, &text).expect("write the golden");
    }
    let found = std::fs::read_to_string(&path).expect("the committed golden");
    if found != text {
        let want: Value = serde_json::from_str(&found).expect("golden json");
        let got: Value = serde_json::from_str(&text).expect("json");
        let rows = |v: &Value| -> Vec<String> {
            v["rows"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| r.as_str().unwrap_or("").to_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let first = rows(&want)
            .into_iter()
            .zip(rows(&got))
            .find(|(a, b)| a != b);
        panic!(
            "{} is stale (first differing row: {first:?}); rerun with FLY_UPDATE_FIXTURES=1",
            path.display()
        );
    }
}

#[test]
fn the_committed_toy_dataset_is_what_the_generator_writes() {
    let dir = toy::dir();
    if updating() {
        toy::write(&dir).expect("write the toy dataset");
    }
    for (name, bytes) in toy::files() {
        let found = std::fs::read(dir.join(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            found, bytes,
            "{name} is stale; rerun with FLY_UPDATE_FIXTURES=1"
        );
    }
    let dataset = load_brain_dataset_from_dir(&dir).expect("the toy dataset loads");
    assert_eq!(
        dataset.fingerprint.as_deref(),
        Some(TOY_FINGERPRINT),
        "the toy profile embeds it"
    );
}

/// The macros-mode scenario, in all three execution modes, against the direct reference and
/// the committed golden.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_macros_scenario_matches_the_reference_in_every_mode() {
    let script = legacy_parity::toy_macros_script(260);
    let want = reference(&script);
    golden(&script, &want);
    for mode in ExecutionMode::all() {
        let got = on_worker(mode, &script).await;
        legacy_parity::compare(&want, &got).unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
    }
}

/// Raw mode: no macro group, `bound` never masks, the decision never names a macro.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_raw_scenario_matches_the_reference_in_every_mode() {
    let script = legacy_parity::toy_raw_script(80);
    let want = reference(&script);
    golden(&script, &want);
    assert!(want.iter().all(|r| {
        r.decision
            .as_ref()
            .is_none_or(|d| d.macro_channel.is_none())
    }));
    for mode in ExecutionMode::all() {
        let got = on_worker(mode, &script).await;
        legacy_parity::compare(&want, &got).unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
    }
}

/// A second fly's script: the same shape, other frames and no rewards, so any gain, trace, RNG
/// or hold shared between the two would show in one of them.
fn quiet_twin(script: &LegacyScript) -> LegacyScript {
    let mut twin = script.clone();
    twin.name = format!("{}-twin", script.name);
    for step in &mut twin.steps {
        if let legacy_parity::ScriptStep::Frame {
            frame,
            rewards,
            sugar,
            ..
        } = step
        {
            *frame = (*frame + 3) % 8;
            rewards.clear();
            sugar.clear();
        }
    }
    twin
}

async fn step_one(
    driver: &mut legacy_parity::AgentDriver,
    index: usize,
    step: &legacy_parity::ScriptStep,
) -> ParityRecord {
    match step {
        legacy_parity::ScriptStep::Frame {
            sugar,
            frame,
            rewards,
            next_context,
        } => driver
            .frame(index + 1, sugar, *frame, rewards, next_context)
            .await
            .expect("a frame"),
        legacy_parity::ScriptStep::Rollback { frame, context } => driver
            .rollback(index + 1, *frame, context)
            .await
            .expect("a rollback"),
        legacy_parity::ScriptStep::Restore => unreachable!("the pair scripts do not restore"),
    }
}

/// Two flies in one composition, dispatched concurrently and then in reversed order, one of
/// them sweeping on two threads: each is exactly its own solo reference. No gains, RNG, holds
/// or pulse cross between agents, and neither dispatch order nor worker count moves a result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_agents_share_nothing_whatever_the_dispatch_order_or_thread_count() {
    let a_script = legacy_parity::toy_raw_script(60);
    let b_script = quiet_twin(&a_script);
    let a_want = reference(&a_script);
    let b_want = reference(&b_script);
    assert_ne!(a_want, b_want);
    for (mode, reversed) in [
        (ExecutionMode::Thread, false),
        (ExecutionMode::Process, true),
    ] {
        let root = tempfile::tempdir().expect("a temporary directory");
        let mut a_agent = toy_agent("fly-a", &a_script);
        a_agent.worker_threads = 2;
        let mut b_agent = toy_agent("fly-b", &b_script);
        b_agent.port_id = id("p2");
        let rig = LegacyRig::start(root.path(), mode, &[a_agent, b_agent])
            .await
            .expect("the rig starts");
        let mut a = rig.driver(&id("fly-a"), &a_script);
        let mut b = rig.driver(&id("fly-b"), &b_script);
        let mut a_got = vec![a.initialize(a_script.initial_frame).await.expect("a")];
        let mut b_got = vec![b.initialize(b_script.initial_frame).await.expect("b")];
        for index in 0..a_script.steps.len() {
            let (ra, rb) = if reversed {
                let rb = step_one(&mut b, index, &b_script.steps[index]).await;
                let ra = step_one(&mut a, index, &a_script.steps[index]).await;
                (ra, rb)
            } else {
                tokio::join!(
                    step_one(&mut a, index, &a_script.steps[index]),
                    step_one(&mut b, index, &b_script.steps[index])
                )
            };
            a_got.push(ra);
            b_got.push(rb);
        }
        // Checkpoints are compared on the solo runs; here the per-step records are.
        let strip = |records: &[ParityRecord]| -> Vec<ParityRecord> {
            records
                .iter()
                .cloned()
                .map(|mut r| {
                    r.state = None;
                    r
                })
                .collect()
        };
        legacy_parity::compare(&strip(&a_want), &a_got)
            .unwrap_or_else(|e| panic!("{} fly-a: {e}", mode.label()));
        legacy_parity::compare(&strip(&b_want), &b_got)
            .unwrap_or_else(|e| panic!("{} fly-b: {e}", mode.label()));
        rig.stop().await;
    }
}

mod refusals {
    use super::*;
    use fly_session::legacy_agent::LegacyAgentProfile;
    use fly_session::types::*;
    use fly_session_types::extensions::{
        AgentRollbackParams, ROLLBACK_CAPABILITY, ROLLBACK_POLICY,
    };

    fn code(reply: Result<fly_session::rpc::DomainReply, DomainError>) -> ErrorCode {
        match reply {
            Err(e) => e.code,
            Ok(reply) => match reply.result() {
                Err(e) => e.code,
                Ok(v) => panic!("expected a refusal, got {v}"),
            },
        }
    }

    async fn rig_with(root: &std::path::Path, agent: RigAgent) -> LegacyRig {
        LegacyRig::start(root, ExecutionMode::InProcess, &[agent])
            .await
            .expect("the rig starts")
    }

    fn init_params(
        driver: &legacy_parity::AgentDriver,
        input: SensoryInput,
        profile: AssetRef,
        seed: i32,
    ) -> serde_json::Value {
        AgentInitializeParams {
            agent_id: id("fly-a"),
            profile,
            seed,
            initial_input: input,
            initial_decision_context: driver.context().clone(),
            worker_threads: 1,
        }
        .to_json()
    }

    /// Initialize refuses another profile, a dataset that is not the profile's, a seed the
    /// kernel version does not allow, and a context naming a channel the composition lacks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_checks_the_profile_the_dataset_the_seed_and_the_context() {
        let script = legacy_parity::toy_macros_script(4);
        let root = tempfile::tempdir().expect("tmp");
        let rig = rig_with(root.path(), toy_agent("fly-a", &script)).await;
        let mut driver = rig.driver(&id("fly-a"), &script);
        let scope = driver.current_scope();
        let (input, frame) = driver.input(0, 0).await.expect("a frame");
        let production = LegacyAgentProfile::production().asset;
        let params = init_params(
            &driver,
            input.clone(),
            production,
            legacy_parity::LEGACY_SEED,
        );
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Initialize",
                        Some(scope.clone()),
                        params,
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::IdentityMismatch,
            "another profile"
        );
        let toy_profile = driver.profile().clone();
        let params = init_params(&driver, input.clone(), toy_profile.clone(), 7);
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Initialize",
                        Some(scope.clone()),
                        params,
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::IncompatibleState,
            "seed 7 would be another kernel version"
        );
        let mut params = init_params(
            &driver,
            input.clone(),
            toy_profile.clone(),
            legacy_parity::LEGACY_SEED,
        );
        let bad = fly_session_types::gameboy::ReadoutContext {
            boot: true,
            bound: vec!["macro_heal".to_owned()],
            location: None,
        };
        params["initialDecisionContext"] = bad.to_typed().to_json();
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Initialize",
                        Some(scope.clone()),
                        params,
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::InvalidArgument,
            "a bound channel outside the composition"
        );
        // Every refusal above was made before a model existed: the worker still initializes.
        driver
            .initialize(0)
            .await
            .expect("the worker is still uninitialized and accepts a good request");
        rig.stop().await;

        // A worker configured for the production profile, pointed at the toy connectome.
        let root = tempfile::tempdir().expect("tmp");
        let mut agent = toy_agent("fly-a", &script);
        agent.profile = LegacyProfileKind::Production;
        let rig = rig_with(root.path(), agent).await;
        let mut driver = rig.driver(&id("fly-a"), &script);
        let (input, frame) = driver.input(0, 0).await.expect("a frame");
        let params = init_params(
            &driver,
            input,
            LegacyAgentProfile::production().asset,
            legacy_parity::LEGACY_SEED,
        );
        let scope = driver.current_scope();
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Initialize",
                        Some(scope),
                        params,
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::IdentityMismatch,
            "the toy connectome is not the dataset the production profile embeds"
        );
        rig.stop().await;
    }

    /// Prepare refuses another cadence and an undeclared stimulus kind; a retried Prepare
    /// replays its decision without ticking again; Rollback refuses the wrong epochs and a
    /// prepared agent; StageRestore refuses another fly's compatibility.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn step_rollback_and_restore_refuse_what_they_must() {
        let script = legacy_parity::toy_macros_script(4);
        let root = tempfile::tempdir().expect("tmp");
        let rig = rig_with(root.path(), toy_agent("fly-a", &script)).await;
        let mut driver = rig.driver(&id("fly-a"), &script);
        let hello = HelloParams {
            session_id: id("legacy"),
            expected_worker_id: id("fly-a"),
            role: Role::Agent,
            supported_majors: vec![1],
        };
        let hello: HelloResult = driver
            .call_raw("Worker.Hello", None, hello.to_json(), &[])
            .await
            .expect("hello")
            .parse()
            .expect("a hello result");
        assert!(
            hello.capabilities.iter().any(|c| c == ROLLBACK_CAPABILITY),
            "{:?}",
            hello.capabilities
        );
        driver.initialize(0).await.expect("initialize");
        let scope = driver.current_scope();
        let prepare = |interval: RationalNs, kind: &str| PrepareParams {
            agent_id: id("fly-a"),
            profile_digest: driver.profile().digest.clone(),
            interval,
            decision_context_digest: driver.context().digest(),
            pre_step_stimulations: vec![Stimulus {
                id: id("s1"),
                kind_id: kind.to_owned(),
                duration_ms: 50.0,
            }],
        };
        let sixty = hz(60).expect("60 Hz");
        let frame_interval = fly_session_types::gameboy::step_duration();
        let bad_cadence = prepare(sixty, "reward-pulse").to_json();
        let bad_kind = prepare(frame_interval, "arena.milestone").to_json();
        let good = prepare(frame_interval, "reward-pulse").to_json();
        assert_eq!(
            code(
                driver
                    .call_raw("Agent.Prepare", Some(scope.clone()), bad_cadence, &[])
                    .await
            ),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(
                driver
                    .call_raw("Agent.Prepare", Some(scope.clone()), bad_kind, &[])
                    .await
            ),
            ErrorCode::Unsupported
        );
        let first = driver
            .call_raw("Agent.Prepare", Some(scope.clone()), good.clone(), &[])
            .await
            .expect("prepare");
        let prepared: PreparedDecision = first.parse().expect("a decision");
        let again = driver
            .retry_raw(
                first.request_id.clone(),
                "Agent.Prepare",
                Some(scope.clone()),
                good,
            )
            .await
            .expect("a replay");
        let replayed: PreparedDecision = again.parse().expect("the same decision");
        assert_eq!(
            prepared, replayed,
            "a duplicate Prepare returns the same decision"
        );

        // Rollback while Prepared.
        let (input, frame) = driver.input(2, 0).await.expect("a frame");
        let rollback = |prior: &str| AgentRollbackParams {
            agent_id: id("fly-a"),
            prior_epoch: id(prior),
            policy: ROLLBACK_POLICY.to_owned(),
            input: input.clone(),
            decision_context: driver.context().clone(),
        };
        let next_epoch = scope_at("legacy", "e2", 0);
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Rollback",
                        Some(next_epoch.clone()),
                        rollback("e1").to_json(),
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::InvalidPhase,
            "a prepared agent is mid-transition"
        );
        rig.stop().await;

        let root = tempfile::tempdir().expect("tmp");
        let mut rig = rig_with(root.path(), toy_agent("fly-a", &script)).await;
        let mut driver = rig.driver(&id("fly-a"), &script);
        driver.initialize(0).await.expect("initialize");
        let (input, frame) = driver.input(2, 0).await.expect("a frame");
        let context = driver.context().clone();
        let params = |prior: &str| {
            AgentRollbackParams {
                agent_id: id("fly-a"),
                prior_epoch: id(prior),
                policy: ROLLBACK_POLICY.to_owned(),
                input: input.clone(),
                decision_context: context.clone(),
            }
            .to_json()
        };
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Rollback",
                        Some(scope_at("legacy", "e3", 0)),
                        params("e2"),
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::StaleEpoch,
            "the prior epoch is not the one this worker is in"
        );
        assert_eq!(
            code(
                driver
                    .call_raw(
                        "Agent.Rollback",
                        Some(scope_at("legacy", "e1", 0)),
                        params("e0"),
                        &[("view.lcd", &frame)]
                    )
                    .await
            ),
            ErrorCode::StaleEpoch,
            "a rollback moves to a new epoch"
        );
        let first = driver
            .call_raw(
                "Agent.Rollback",
                Some(scope_at("legacy", "e2", 0)),
                params("e1"),
                &[("view.lcd", &frame)],
            )
            .await
            .expect("a rollback");
        first.result().expect("applied");
        let replay = driver
            .retry_raw(
                first.request_id.clone(),
                "Agent.Rollback",
                Some(scope_at("legacy", "e2", 0)),
                params("e1"),
            )
            .await
            .expect("a replay of the same rollback");
        assert_eq!(
            replay.result().expect("replayed"),
            first.result().expect("applied"),
            "a lost reply replays, it does not roll back twice"
        );

        // A capture restored with someone else's compatibility is refused before staging.
        driver.bump_epoch();
        let captured = driver.capture().await.expect("capture");
        let target = rig.replace(&id("fly-a")).await.expect("a replacement");
        let mut forged = captured.result.clone();
        forged.compatibility_digest = digest_of_bytes(b"another fly");
        let forged = legacy_parity::Captured {
            result: forged,
            artifact: captured.artifact.clone(),
            bytes: captured.bytes.clone(),
            scope: captured.scope.clone(),
        };
        let refused = driver
            .restore_into(target, &forged)
            .await
            .expect_err("another fly's compatibility");
        assert!(refused.contains("IncompatibleState"), "{refused}");
        rig.stop().await;
    }
}

/// The real dataset, an explicit job: `FLY_AGENT01_FAFB=1` with `data/fafb-v783` checked out.
/// Release mode is the sensible way to run it.
mod fafb {
    use super::*;
    use fly_session::legacy_parity::{RewardEvent, ScriptStep, fafb_dir, frame_pool};
    use fly_session_types::gameboy::{Location, ReadoutContext};

    fn enabled() -> Option<std::path::PathBuf> {
        if std::env::var_os("FLY_AGENT01_FAFB").is_none() {
            eprintln!("skipping: FLY_AGENT01_FAFB is not set");
            return None;
        }
        let dir = fafb_dir();
        if dir.is_none() {
            eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        }
        dir
    }

    fn pokered_channels() -> Vec<String> {
        let file = fly_session_types::fixtures::load("gameboy-decoder-config.json").expect("vectors");
        file["cases"][1]["macroChannels"]
            .as_array()
            .expect("the Pokemon Red macro group")
            .iter()
            .map(|c| c.as_str().expect("a channel").to_owned())
            .collect()
    }

    /// The live macros-mode composition's shape on the real connectome, with recorded inputs.
    fn script(frames: usize) -> LegacyScript {
        let channels = pokered_channels();
        let bound = |k: usize| -> Vec<String> {
            let take: &[usize] = match k {
                0..20 => &[],
                20..50 => &[0, 1, 9],
                _ => &[1, 4, 5, 6],
            };
            take.iter().map(|i| channels[*i].clone()).collect()
        };
        let mut steps = Vec::new();
        for k in 1..=frames {
            steps.push(ScriptStep::Frame {
                sugar: if k == 12 { vec![400.0] } else { vec![] },
                frame: k % 6,
                rewards: if k % 11 == 0 {
                    vec![RewardEvent { value: 0.5, stimulation_ms: 120.0 }]
                } else {
                    vec![]
                },
                next_context: ReadoutContext {
                    boot: k < 10,
                    bound: bound(k),
                    location: (k > 5).then_some(Location { area: 12, x: 3 + (k / 40) as u32, y: 7 }),
                },
            });
            if k == 60 {
                steps.push(ScriptStep::Rollback {
                    frame: 2,
                    context: ReadoutContext { boot: false, bound: bound(k), location: None },
                });
            }
            if k == 75 {
                steps.push(ScriptStep::Restore);
            }
        }
        let last = steps.len() - 1;
        LegacyScript {
            name: "fafb-macros".to_owned(),
            macro_channels: channels,
            frames: Arc::new(frame_pool(6, 783)),
            initial_frame: 0,
            initial_context: ReadoutContext { boot: true, bound: vec![], location: None },
            steps,
            checkpoints: [30, last].into_iter().collect(),
        }
    }

    /// The tracked rate roles on the committed dataset are the fixture's list, and each is
    /// published as itself (`legacy-rate-role-id-v1`).
    #[test]
    fn the_fafb_rate_roles_are_the_fixture_list() {
        let Some(dir) = enabled() else { return };
        let dataset = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset loads"));
        let config = flybrain_core::agent::AgentConfig::with_decoder(
            flybrain_core::decoder::gameboy::gameboy_decoder_config(),
        );
        let agent = flybrain_core::agent::NeuralAgent::new(dataset, config).expect("the agent");
        let tracked: Vec<String> = agent.network.rates.keys().cloned().collect();
        let file = fly_session_types::fixtures::load("gameboy-rate-roles.json").expect("fixture");
        let want: Vec<String> = file["fafbTrackedRoles"]
            .as_array()
            .expect("list")
            .iter()
            .map(|v| v.as_str().expect("a name").to_owned())
            .collect();
        assert_eq!(tracked, want);
    }

    /// Recorded inputs through the worker (in-process and as a process) against `NeuralAgent`
    /// driven directly, on `gameboy-legacy-fafb-v783-v1` itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_fafb_scenario_matches_the_direct_reference() {
        let Some(dir) = enabled() else { return };
        let script = script(90);
        let started = std::time::Instant::now();
        let dataset = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset loads"));
        let want = DirectSource { dataset }.records(&script).expect("the reference runs");
        eprintln!("reference: {} records in {:?}", want.len(), started.elapsed());
        let summary = legacy_parity::golden_json(&script, &want);
        eprintln!("reference digest {}", summary["digest"]);
        for mode in [ExecutionMode::InProcess, ExecutionMode::Process] {
            let started = std::time::Instant::now();
            let root = tempfile::tempdir().expect("tmp");
            let agent = RigAgent {
                agent_id: id("fly-a"),
                port_id: id("p1"),
                dataset_dir: dir.clone(),
                profile: LegacyProfileKind::Production,
                macro_channels: script.macro_channels.clone(),
                worker_threads: 1,
            };
            let mut rig = LegacyRig::start(root.path(), mode, &[agent]).await.expect("the rig");
            let got = legacy_parity::run_on_worker(&mut rig, &id("fly-a"), &script)
                .await
                .unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
            rig.stop().await;
            legacy_parity::compare(&want, &got).unwrap_or_else(|e| panic!("{}: {e}", mode.label()));
            eprintln!("{}: {} records identical in {:?}", mode.label(), got.len(), started.elapsed());
        }
        for row in summary["rows"].as_array().expect("rows").iter().step_by(10) {
            eprintln!("  {}", row.as_str().expect("a row"));
        }
    }
}
