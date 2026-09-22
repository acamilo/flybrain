//! The same synthetic session in all three execution modes, printing the one behaviour trace
//! they agree on.
//!
//! ```sh
//! cargo run -p fly-session --example processes
//! ```
//!
//! The separate-process run starts one agent process per fly and one environment process
//! through this crate's own binary, so it needs that binary built:
//!
//! ```sh
//! cargo build -p fly-session --bin fly-session
//! ```

use fly_session::harness::{ExecutionMode, HarnessConfig, SessionHarness, Via};
use fly_session::launcher::default_worker_program;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let program = default_worker_program();
    println!("worker program: {}", program.display());
    let mut agreed: Option<Vec<String>> = None;

    for mode in ExecutionMode::all() {
        let dir = tempfile::tempdir()?;
        let config = HarnessConfig { mode, ..HarnessConfig::default() };
        println!(
            "\n=== {} : {} threads for {} participants plus the coordinator",
            mode.label(),
            config.budget()?.total(),
            config.agents.len() + 1
        );
        let mut harness = SessionHarness::start(Via::Unix, dir.path(), config).await?;
        harness.coordinator.bootstrap().await?;
        let reports = harness.coordinator.run(3).await?;
        for report in &reports {
            println!("  committed boundary {}", report.boundary);
        }
        for (worker_id, status) in harness.launcher.health_check_all().await {
            match status {
                Ok(status) => println!(
                    "  {worker_id}: {:?}, progress {}",
                    status.state, status.progress_counter
                ),
                Err(e) => println!("  {worker_id}: unhealthy: {e}"),
            }
        }
        let behaviour = harness.coordinator.trace.behavior();
        match &agreed {
            None => {
                println!("  behaviour trace, {} transitions:", behaviour.len());
                for line in &behaviour {
                    println!("    {line}");
                }
                agreed = Some(behaviour);
            }
            Some(first) => {
                assert_eq!(
                    &behaviour, first,
                    "{} produced a different behaviour trace",
                    mode.label()
                );
                println!("  behaviour trace: identical to the first run");
            }
        }
        let reaped = harness.launcher.reap_all("example").await;
        for (worker_id, outcome) in reaped {
            println!("  reaped {worker_id}: {outcome:?}");
        }
        harness.shutdown().await;
        drop(dir);
    }
    println!("\nall three execution modes produced one behaviour trace");
    Ok(())
}
