//! The execution-mode comparison the implementation guide's section 5 asks for.
//!
//! It runs the same synthetic session in each execution mode, at one, two and four agents,
//! and reports the thread allocation, the RPC and critical-path percentiles, the memory peaks
//! and the router's owner, collection and queue counters.
//!
//! **These are local synthetic timings on one machine, and no host capacity claim follows
//! from any of them.** They exist so the three modes can be compared with each other: the
//! question this slice has to answer is what a process boundary costs, not how fast anything
//! is. Pacing is switched off for the run, so a transition follows the one before it as fast
//! as the participants answer and the samples are work rather than sleep.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::coordinator::DispatchOrder;
use crate::harness::{AgentSpec, HarnessConfig, SessionHarness, Via};
use crate::launcher::ExecutionMode;
use crate::metrics::{Percentiles, physical_cores};

/// What to compare.
#[derive(Clone, Debug)]
pub struct MeasureConfig {
    /// Transitions per run, after the warm-up ones.
    pub steps: u64,
    /// Transitions run before sampling starts, so first-call costs are not in the samples.
    pub warmup_steps: u64,
    pub agent_counts: Vec<usize>,
    pub modes: Vec<ExecutionMode>,
    /// The within-agent worker count each agent asks its launcher for.
    pub worker_threads: usize,
}

impl Default for MeasureConfig {
    fn default() -> MeasureConfig {
        MeasureConfig {
            steps: 200,
            warmup_steps: 10,
            agent_counts: vec![1, 2, 4],
            modes: ExecutionMode::all().to_vec(),
            worker_threads: 1,
        }
    }
}

/// The highest value each router counter reached while the transitions ran.
#[derive(Debug, Default)]
struct Peaks {
    owners: AtomicU64,
    roots: AtomicU64,
    queued: AtomicU64,
    sealed: AtomicU64,
    store_bytes: AtomicU64,
}

impl Peaks {
    fn observe(&self, stats: &flybus::RouterStats) {
        raise(&self.owners, stats.owners as u64);
        raise(&self.roots, stats.artifact_roots);
        raise(&self.queued, stats.queued as u64);
        raise(&self.sealed, stats.sealed_artifacts as u64);
        raise(&self.store_bytes, stats.store_bytes);
    }

    fn read(&self) -> (usize, u64, usize, usize, u64) {
        (
            self.owners.load(Ordering::Relaxed) as usize,
            self.roots.load(Ordering::Relaxed),
            self.queued.load(Ordering::Relaxed) as usize,
            self.sealed.load(Ordering::Relaxed) as usize,
            self.store_bytes.load(Ordering::Relaxed),
        )
    }
}

fn raise(slot: &AtomicU64, value: u64) {
    slot.fetch_max(value, Ordering::Relaxed);
}

/// One measured composition.
#[derive(Clone, Debug)]
pub struct Row {
    pub mode: ExecutionMode,
    pub agents: usize,
    pub worker_threads: usize,
    pub physical_cores: usize,
    pub budget_total: usize,
    pub budget_used: usize,
    pub steps: u64,
    /// `Agent.Prepare`, over every agent and every transition.
    pub prepare: Percentiles,
    pub commit: Percentiles,
    pub advance: Percentiles,
    /// `Worker.Status`: an RPC with no domain work behind it, so it is the router and
    /// transport floor rather than a measure of the worker.
    pub status: Percentiles,
    /// One whole transition, pacing excluded: the critical path.
    pub step: Percentiles,
    pub coordinator_peak_rss_kib: u64,
    /// The sum of the peak resident sets of the participants with processes of their own.
    pub participants_peak_rss_kib: u64,
    pub owners_max: usize,
    pub owners_final: usize,
    pub artifact_roots_max: u64,
    pub queued_max: usize,
    pub sealed_max: usize,
    pub sealed_final: usize,
    pub store_bytes_max: u64,
    pub store_bytes_final: u64,
    /// One sealed frame per boundary, boundary zero included.
    pub frames_produced: u64,
}

impl Row {
    /// Frames the store collected: produced, minus the ones still owned at the end.
    pub fn collected(&self) -> u64 {
        self.frames_produced.saturating_sub(self.sealed_final as u64)
    }
}

/// Runs the comparison. Every row is one composition in one mode.
pub async fn run(config: &MeasureConfig) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for mode in &config.modes {
        for agents in &config.agent_counts {
            rows.push(one(config, *mode, *agents).await?);
        }
    }
    Ok(rows)
}

async fn one(config: &MeasureConfig, mode: ExecutionMode, agents: usize) -> Result<Row, String> {
    let dir = std::env::temp_dir().join(format!(
        "fly-session-measure-{}-{agents}-{}",
        mode.label(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = measure_in(config, mode, agents, &dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

async fn measure_in(
    config: &MeasureConfig,
    mode: ExecutionMode,
    agents: usize,
    dir: &std::path::Path,
) -> Result<Row, String> {
    let specs: Vec<AgentSpec> = (0..agents)
        .map(|i| AgentSpec {
            worker_threads: config.worker_threads,
            ..AgentSpec::new(&format!("fly-{}", (b'a' + i as u8) as char), &format!("p{}", i + 1), 7 + i as i32)
        })
        .collect();
    let harness_config = HarnessConfig {
        agents: specs,
        mode,
        ..HarnessConfig::default()
    };
    let budget = harness_config.budget().map_err(|e| e.to_string())?;
    let (budget_total, budget_used_floor) = (budget.total(), harness_config.required_threads());
    let mut harness = SessionHarness::start(Via::Unix, dir, harness_config)
        .await
        .map_err(|e| format!("{}: start: {}", mode.label(), e.message))?;
    harness.coordinator.dispatch = DispatchOrder::Concurrent;
    harness.coordinator.disable_pacing();
    harness
        .coordinator
        .bootstrap()
        .await
        .map_err(|e| format!("{}: bootstrap: {e}", mode.label()))?;
    // The warm-up transitions pay the first-call costs; their samples are then discarded.
    harness
        .coordinator
        .run(config.warmup_steps)
        .await
        .map_err(|e| format!("{}: warm-up: {e}", mode.label()))?;
    harness.coordinator.metrics.clear();

    // The router's counters are sampled *while* transitions run, not between them: a queue
    // that is empty at every committed boundary says nothing about whether it stayed bounded
    // during the transaction, which is the thing section 4 asks about.
    let peaks = Arc::new(Peaks::default());
    let sampling = Arc::new(AtomicBool::new(true));
    let sampler = tokio::spawn({
        let router = harness.router().clone();
        let peaks = peaks.clone();
        let sampling = sampling.clone();
        async move {
            while sampling.load(Ordering::Relaxed) {
                peaks.observe(&router.stats());
                tokio::time::sleep(std::time::Duration::from_micros(200)).await;
            }
            peaks.observe(&router.stats());
        }
    });

    let environment = harness.environment_id();
    let mut status = crate::metrics::Metrics::default();
    for _ in 0..config.steps {
        harness
            .coordinator
            .step()
            .await
            .map_err(|e| format!("{}: step: {e}", mode.label()))?;
        // One status call per transition: an RPC the worker answers from a cell rather than
        // from its endpoint, so it is the router-and-transport floor the domain calls sit on.
        let started = std::time::Instant::now();
        let _ = harness.launcher.health_check(&environment).await;
        status.record("Worker.Status", started.elapsed());
    }
    sampling.store(false, Ordering::Relaxed);
    let _ = sampler.await;
    let (owners_max, roots_max, queued_max, sealed_max, store_bytes_max) = peaks.read();

    let metrics = &harness.coordinator.metrics;
    let zero = Percentiles::default();
    let row = Row {
        mode,
        agents,
        worker_threads: config.worker_threads,
        physical_cores: physical_cores(),
        budget_total,
        budget_used: budget_used_floor,
        steps: config.steps,
        prepare: metrics.percentiles("Agent.Prepare").unwrap_or(zero),
        commit: metrics.percentiles("Agent.Commit").unwrap_or(zero),
        advance: metrics.percentiles("Environment.Advance").unwrap_or(zero),
        status: status.percentiles("Worker.Status").unwrap_or(zero),
        step: metrics.percentiles("step").unwrap_or(zero),
        coordinator_peak_rss_kib: crate::metrics::peak_rss_kib().unwrap_or_default(),
        participants_peak_rss_kib: harness
            .launcher
            .peak_rss_kib()
            .iter()
            .filter(|(who, _)| who.as_str() != "coordinator")
            .map(|(_, kib)| *kib)
            .sum(),
        owners_max,
        owners_final: harness.router_stats().owners,
        artifact_roots_max: roots_max,
        queued_max,
        sealed_max,
        sealed_final: harness.router_stats().sealed_artifacts,
        store_bytes_max,
        store_bytes_final: harness.router_stats().store_bytes,
        frames_produced: config.steps + config.warmup_steps + 1,
    };
    harness.shutdown().await;
    Ok(row)
}

/// The measurement table, as Markdown.
pub fn table(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(
        "Local synthetic timings on one machine. Not a capacity claim, and not a latency goal.\n\n",
    );
    if let Some(first) = rows.first() {
        out.push_str(&format!(
            "Physical cores: {}. Coordinator reservation: 1 thread.\n\n",
            first.physical_cores
        ));
    }
    out.push_str(
        "| mode | agents | threads/agent | budget used/total | Prepare p50/p95/p99 us | \
Commit p50/p95/p99 us | Advance p50/p95/p99 us | Status p50/p99 us | step p50/p95/p99 us |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {}/{} | {:.0}/{:.0}/{:.0} | {:.0}/{:.0}/{:.0} | \
{:.0}/{:.0}/{:.0} | {:.0}/{:.0} | {:.0}/{:.0}/{:.0} |\n",
            r.mode.label(),
            r.agents,
            r.worker_threads,
            r.budget_used,
            r.budget_total,
            r.prepare.p50_us(),
            r.prepare.p95_us(),
            r.prepare.p99_us(),
            r.commit.p50_us(),
            r.commit.p95_us(),
            r.commit.p99_us(),
            r.advance.p50_us(),
            r.advance.p95_us(),
            r.advance.p99_us(),
            r.status.p50_us(),
            r.status.p99_us(),
            r.step.p50_us(),
            r.step.p95_us(),
            r.step.p99_us(),
        ));
    }
    out.push('\n');
    out.push_str(
        "| mode | agents | coordinator peak RSS KiB | participant peak RSS KiB | owners max/final | \
roots max | queued max | sealed max/final | store bytes max/final | frames produced/collected |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {} | {}/{} | {}/{} | {}/{} |\n",
            r.mode.label(),
            r.agents,
            r.coordinator_peak_rss_kib,
            r.participants_peak_rss_kib,
            r.owners_max,
            r.owners_final,
            r.artifact_roots_max,
            r.queued_max,
            r.sealed_max,
            r.sealed_final,
            r.store_bytes_max,
            r.store_bytes_final,
            r.frames_produced,
            r.collected(),
        ));
    }
    out
}

/// The rows keyed by mode and agent count, for a caller that wants one of them.
pub fn by_composition(rows: &[Row]) -> BTreeMap<(String, usize), Row> {
    rows.iter()
        .map(|r| ((r.mode.label().to_owned(), r.agents), r.clone()))
        .collect()
}
