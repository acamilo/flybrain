//! The synthetic sequential transaction, run over both transports and printed.
//!
//! ```text
//! cargo run -p fly-session --example session
//! ```
//!
//! Two fake agents, one counter arena, one coordinator, one router. Nothing here needs a ROM,
//! a dataset, a GPU or a network.

use fly_session::types::*;
use fly_session::harness::{HarnessConfig, SessionHarness, Via};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    for via in [Via::Memory, Via::Unix] {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut harness = SessionHarness::start(via, dir.path(), HarnessConfig::default())
            .await
            .expect("the session starts");
        harness.coordinator.bootstrap().await.expect("bootstrap");
        println!("--- {via:?}: Ready(0) with the world stopped at boundary 0");
        let reports = harness.coordinator.run(3).await.expect("three transitions");

        for (k, transition) in harness.coordinator.trace.transitions.iter().enumerate() {
            let ticks: Vec<String> = transition
                .behaviour
                .agents
                .iter()
                .map(|a| format!("{}={} ticks", a.agent_id, a.ticks_advanced))
                .collect();
            println!(
                "step {k}: {}  batch={} boundary={} events={}",
                ticks.join(" "),
                transition.behaviour.batch_id,
                transition.behaviour.acknowledged_boundary,
                transition.behaviour.event_ids.len()
            );
        }
        let progress = harness.coordinator.task_progress();
        println!(
            "{via:?}: {} advances, counter {}, reward {}, {} publications, last boundary {}",
            harness.coordinator.stats().advances,
            progress.integer("counter").unwrap_or_default(),
            progress.number("totalReward").unwrap_or_default(),
            harness.coordinator.stats().publications,
            reports.last().map(|r| r.boundary).unwrap_or_default(),
        );
        // The behaviour trace is what two runs in different dispatch orders must agree on.
        for line in harness.coordinator.trace.behavior() {
            println!("  behaviour: {line}");
        }
        harness.shutdown().await;
        drop(dir);
    }
}
