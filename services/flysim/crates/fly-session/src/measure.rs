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
//!
//! **Every row runs in a process of its own.** The coordinator's memory figure is a peak --
//! `VmHWM` never falls -- so several rows sharing one process would each report where that
//! process had already been rather than what its own mode costs, and the column would order
//! itself by row position instead of by mode. The parent spawns one `measure-row` child per
//! row and reads its result back, so each figure belongs to the row that produced it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::coordinator::DispatchOrder;
use crate::harness::{AgentSpec, HarnessConfig, SessionHarness, Via};
use crate::launcher::{ExecutionMode, flags};
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
    /// Frames this run actually observed, counted from the behaviour trace: every sensory
    /// view of every transition, plus the one the environment sealed for boundary zero.
    ///
    /// Counted rather than calculated, so a backend that sealed two frames per boundary would
    /// show up here instead of being hidden by arithmetic.
    pub frames_observed: u64,
}

impl Row {
    /// Frames the store collected: observed, minus the ones still owned at the end.
    pub fn collected(&self) -> u64 {
        self.frames_observed.saturating_sub(self.sealed_final as u64)
    }

    /// The row as one JSON object, for the child that measured it to hand back.
    pub fn to_json(&self) -> Value {
        let p = |x: &Percentiles| {
            json!({"count": x.count, "p50": x.p50_ns, "p95": x.p95_ns, "p99": x.p99_ns,
                   "max": x.max_ns})
        };
        json!({
            "mode": self.mode.label(),
            "agents": self.agents,
            "workerThreads": self.worker_threads,
            "physicalCores": self.physical_cores,
            "budgetTotal": self.budget_total,
            "budgetUsed": self.budget_used,
            "steps": self.steps,
            "prepare": p(&self.prepare),
            "commit": p(&self.commit),
            "advance": p(&self.advance),
            "status": p(&self.status),
            "step": p(&self.step),
            "coordinatorPeakRssKib": self.coordinator_peak_rss_kib,
            "participantsPeakRssKib": self.participants_peak_rss_kib,
            "ownersMax": self.owners_max,
            "ownersFinal": self.owners_final,
            "artifactRootsMax": self.artifact_roots_max,
            "queuedMax": self.queued_max,
            "sealedMax": self.sealed_max,
            "sealedFinal": self.sealed_final,
            "storeBytesMax": self.store_bytes_max,
            "storeBytesFinal": self.store_bytes_final,
            "framesObserved": self.frames_observed,
        })
    }

    /// Reads back what a `measure-row` child printed.
    pub fn from_json(value: &Value) -> Result<Row, String> {
        let u = |name: &str| -> Result<u64, String> {
            value
                .get(name)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("measure row: {name} is missing or not a number"))
        };
        let p = |name: &str| -> Result<Percentiles, String> {
            let v = value
                .get(name)
                .ok_or_else(|| format!("measure row: {name} is missing"))?;
            let f = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or_default();
            Ok(Percentiles {
                count: f("count") as usize,
                p50_ns: f("p50"),
                p95_ns: f("p95"),
                p99_ns: f("p99"),
                max_ns: f("max"),
            })
        };
        let mode = match value.get("mode").and_then(Value::as_str) {
            Some("in-process") => ExecutionMode::InProcess,
            Some("thread") => ExecutionMode::Thread,
            Some("process") => ExecutionMode::Process,
            other => return Err(format!("measure row: unknown mode {other:?}")),
        };
        Ok(Row {
            mode,
            agents: u("agents")? as usize,
            worker_threads: u("workerThreads")? as usize,
            physical_cores: u("physicalCores")? as usize,
            budget_total: u("budgetTotal")? as usize,
            budget_used: u("budgetUsed")? as usize,
            steps: u("steps")?,
            prepare: p("prepare")?,
            commit: p("commit")?,
            advance: p("advance")?,
            status: p("status")?,
            step: p("step")?,
            coordinator_peak_rss_kib: u("coordinatorPeakRssKib")?,
            participants_peak_rss_kib: u("participantsPeakRssKib")?,
            owners_max: u("ownersMax")? as usize,
            owners_final: u("ownersFinal")? as usize,
            artifact_roots_max: u("artifactRootsMax")?,
            queued_max: u("queuedMax")? as usize,
            sealed_max: u("sealedMax")? as usize,
            sealed_final: u("sealedFinal")? as usize,
            store_bytes_max: u("storeBytesMax")?,
            store_bytes_final: u("storeBytesFinal")?,
            frames_observed: u("framesObserved")?,
        })
    }
}

/// Runs the comparison, one child process per row.
///
/// `program` is this crate's binary; each row is measured by a `measure-row` invocation of it
/// so that the row's memory peak is its own and not the accumulated peak of the rows before
/// it. The order of the rows therefore cannot change any of their numbers.
pub fn run(config: &MeasureConfig, program: &std::path::Path) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for mode in &config.modes {
        for agents in &config.agent_counts {
            rows.push(row_in_a_child(config, program, *mode, *agents)?);
        }
    }
    Ok(rows)
}

fn row_in_a_child(
    config: &MeasureConfig,
    program: &std::path::Path,
    mode: ExecutionMode,
    agents: usize,
) -> Result<Row, String> {
    let output = std::process::Command::new(program)
        .arg("measure-row")
        .arg(format!("--{}", flags::MODE))
        .arg(mode.label())
        .arg(format!("--{}", flags::AGENTS))
        .arg(agents.to_string())
        .arg(format!("--{}", flags::STEPS))
        .arg(config.steps.to_string())
        .arg(format!("--{}", flags::WARMUP_STEPS))
        .arg(config.warmup_steps.to_string())
        .arg(format!("--{}", flags::WORKER_THREADS))
        .arg(config.worker_threads.to_string())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("measure-row {}: {e}", mode.label()))?;
    if !output.status.success() {
        return Err(format!(
            "measure-row {} {agents}: exited {}: {}",
            mode.label(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("measure-row {}: no row on stdout", mode.label()))?;
    let value: Value = serde_json::from_str(line)
        .map_err(|e| format!("measure-row {}: unreadable row: {e}", mode.label()))?;
    Row::from_json(&value)
}

/// Measures exactly one row, in this process. The `measure-row` subcommand's body.
pub async fn one(
    config: &MeasureConfig,
    mode: ExecutionMode,
    agents: usize,
) -> Result<Row, String> {
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
        // Counted from the behaviour trace: every sensory view of every transition this run
        // recorded, plus the frame the environment sealed for boundary zero.
        frames_observed: 1 + harness
            .coordinator
            .trace
            .transitions
            .iter()
            .map(|t| t.behaviour.observation_boundaries.len() as u64)
            .sum::<u64>(),
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
            "Physical cores: {}. Coordinator reservation: 1 thread. Every row was measured in \
a process of its own, so no figure depends on the order of the rows.\n\n",
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
roots max | queued max | sealed max/final | store bytes max/final | frames observed/collected |\n",
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
            r.frames_observed,
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
