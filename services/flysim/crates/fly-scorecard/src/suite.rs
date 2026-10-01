//! A suite: the checkpoint set times the seeds, run as child processes, aggregated.
//!
//! One run is one process (`fly-scorecard run-one`), so two runs never share a thread pool, a
//! tokio runtime or a brain. The suite starts `jobs` children at a time and collects the JSON each
//! prints. A child that fails is recorded as a failed run and never silently dropped: a suite with
//! failed runs says so in its summary and `compare` pairs only what finished.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::compare::{METRICS, mean, stdev};
use crate::tally::RunReport;

pub const SCHEMA: &str = "fly-scorecard-v1";
pub const SET_SCHEMA: &str = "fly-scorecard-set-v1";

/// One checkpoint of the fixed set. Public files name the environment variable that holds the
/// path (the ROM tests' own, `rom-env.sh`), never a path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetEntry {
    pub id: String,
    /// The rung the checkpoint is at, for the report's order and the by-rung table.
    pub rung: u32,
    pub env: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckpointSet {
    pub schema: String,
    pub checkpoints: Vec<SetEntry>,
}

impl CheckpointSet {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let set: CheckpointSet = serde_json::from_str(&text).context("the checkpoint set")?;
        if set.schema != SET_SCHEMA {
            bail!("{}: schema {:?}, expected {SET_SCHEMA}", path.display(), set.schema);
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &set.checkpoints {
            if !seen.insert(entry.id.clone()) {
                bail!("{}: checkpoint id {:?} twice", path.display(), entry.id);
            }
        }
        Ok(set)
    }

    /// The entries whose checkpoint file exists in this environment, with their paths, and the
    /// ids that do not (reported, never skipped silently).
    pub fn resolve(&self, env: &dyn Fn(&str) -> Option<String>) -> (Vec<(SetEntry, PathBuf)>, Vec<String>) {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for entry in &self.checkpoints {
            match env(&entry.env).map(PathBuf::from).filter(|path| path.is_file()) {
                Some(path) => found.push((entry.clone(), path)),
                None => missing.push(format!("{} (${})", entry.id, entry.env)),
            }
        }
        (found, missing)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SuiteConfig {
    /// Brain minutes measured per run.
    pub minutes: f64,
    pub seeds: Vec<u32>,
    pub threads: usize,
    pub jobs: usize,
    pub probe_s: f64,
    pub window_s: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// The release or commit under test, as the caller names it.
    pub release: String,
    pub adapter: String,
    /// The compatibility string's SHA-256 prefix (the string itself names no private value, but
    /// the digest is enough to tell two builds' checkpoints apart).
    pub compatibility_sha: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FailedRun {
    pub checkpoint: String,
    pub seed: u32,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuiteReport {
    pub schema: String,
    pub label: String,
    pub build: BuildInfo,
    pub runtime: String,
    pub mode: String,
    pub config: SuiteConfig,
    pub runs: Vec<RunReport>,
    pub failed_runs: Vec<FailedRun>,
}

impl SuiteReport {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let suite: SuiteReport = serde_json::from_str(&text).with_context(|| format!("{} is not a scorecard", path.display()))?;
        if suite.schema != SCHEMA {
            bail!("{}: schema {:?}, expected {SCHEMA}", path.display(), suite.schema);
        }
        Ok(suite)
    }

    pub fn brain_hours(&self) -> f64 {
        self.runs.iter().map(|run| run.minutes).sum::<f64>() / 60.0
    }

    /// The headline: totals over every run, as rates per brain hour where that is the unit.
    pub fn headline(&self) -> Headline {
        let hours = self.brain_hours();
        let sum = |f: &dyn Fn(&RunReport) -> f64| self.runs.iter().map(f).sum::<f64>();
        let per_hour = |x: f64| if hours > 0.0 { x / hours } else { 0.0 };
        let probes = sum(&|r| r.watchdog.probes as f64);
        let windows = sum(&|r| r.hunt.windows as f64);
        let frames = sum(&|r| r.frames as f64);
        let finishes = sum(&|r| (r.macros.done + r.macros.failed()) as f64);
        Headline {
            runs: self.runs.len(),
            brain_hours: hours,
            rungs_per_hour: per_hour(sum(&|r| f64::from(r.rungs.gained))),
            runs_with_a_climb: self.runs.iter().filter(|r| r.rungs.gained > 0).count(),
            empty_pad_pct: if frames > 0.0 { 100.0 * sum(&|r| r.pad.empty_frames as f64) / frames } else { 0.0 },
            suspected_probe_pct: if probes > 0.0 { 100.0 * sum(&|r| r.watchdog.suspected as f64) / probes } else { 0.0 },
            ladder_events: self.runs.iter().map(|r| r.watchdog.ladder_events).sum(),
            runs_with_a_ladder_event: self.runs.iter().filter(|r| r.watchdog.ladder_events > 0).count(),
            hunt_flagged_pct: if windows > 0.0 { 100.0 * sum(&|r| r.hunt.flagged as f64) / windows } else { 0.0 },
            blocked_macro_pct: if finishes > 0.0 { 100.0 * sum(&|r| r.macros.failed() as f64) / finishes } else { 0.0 },
            buy_ball_done: self.runs.iter().map(|r| r.funnel.buy_ball_done).sum(),
            throws: self.runs.iter().map(|r| r.funnel.throw_ball_start).sum(),
            catches: self.runs.iter().map(|r| r.funnel.catches).sum(),
            heals: self.runs.iter().map(|r| r.heals.heal_done).sum(),
            whiteouts: self.runs.iter().map(|r| r.whiteouts).sum(),
            battles_won: self.runs.iter().map(|r| r.battles.won).sum(),
            battles_lost: self.runs.iter().map(|r| r.battles.lost).sum(),
            ratchet_rollbacks: self.runs.iter().map(|r| u64::from(r.ratchet.rollbacks)).sum(),
        }
    }

    /// A one-screen summary: the headline, then a row per checkpoint with its seeds' spread.
    pub fn markdown(&self, rungs: &BTreeMap<String, u32>) -> String {
        let h = self.headline();
        let mut out = String::new();
        out.push_str(&format!(
            "# Scorecard {}: {} runtime, {}\n\n",
            self.label, self.runtime, self.mode
        ));
        out.push_str(&format!(
            "{} runs, {:.2} brain hours ({} brain min each, seeds {:?}), adapter `{}`, compatibility `{}`.\n\n",
            h.runs,
            h.brain_hours,
            self.config.minutes,
            self.config.seeds,
            self.build.adapter,
            self.build.compatibility_sha
        ));
        if !self.failed_runs.is_empty() {
            out.push_str(&format!("**{} runs failed:**\n", self.failed_runs.len()));
            for failed in &self.failed_runs {
                out.push_str(&format!("- {} seed {}: {}\n", failed.checkpoint, failed.seed, failed.error));
            }
            out.push('\n');
        }
        out.push_str("| headline | value |\n| --- | ---: |\n");
        let rows: Vec<(&str, String)> = vec![
            ("rungs per brain hour", format!("{:.2} ({} of {} runs climbed)", h.rungs_per_hour, h.runs_with_a_climb, h.runs)),
            ("empty-pad time", format!("{:.2}% of frames", h.empty_pad_pct)),
            ("watchdog probes suspected", format!("{:.1}%", h.suspected_probe_pct)),
            ("ladder-worthy traps", format!("{} (in {} runs)", h.ladder_events, h.runs_with_a_ladder_event)),
            ("trap-hunt windows flagged", format!("{:.1}%", h.hunt_flagged_pct)),
            ("blocked macro finishes", format!("{:.1}%", h.blocked_macro_pct)),
            ("buy ball / throw / catch", format!("{} / {} / {}", h.buy_ball_done, h.throws, h.catches)),
            ("heals / whiteouts", format!("{} / {}", h.heals, h.whiteouts)),
            ("battles won / lost", format!("{} / {}", h.battles_won, h.battles_lost)),
            ("ratchet rollbacks", format!("{}", h.ratchet_rollbacks)),
        ];
        for (name, value) in rows {
            out.push_str(&format!("| {name} | {value} |\n"));
        }
        out.push_str("\n| checkpoint | rung | seeds | rungs/h | empty pad % | suspected % | traps | buy/throw/catch | won/lost | whiteouts |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |\n");
        let mut by: BTreeMap<&str, Vec<&RunReport>> = BTreeMap::new();
        for run in &self.runs {
            by.entry(run.checkpoint.as_str()).or_default().push(run);
        }
        let mut order: Vec<(&str, &Vec<&RunReport>)> = by.iter().map(|(k, v)| (*k, v)).collect();
        order.sort_by_key(|(id, runs)| (rungs.get(*id).copied().unwrap_or(runs[0].rungs.start), *id));
        for (id, runs) in order {
            let rate = |name: &str| -> Vec<f64> {
                let metric = METRICS.iter().find(|m| m.name == name).expect("a metric");
                runs.iter().map(|r| (metric.read)(r)).collect()
            };
            let rungs_h = rate("rungs_per_hour");
            let pad = rate("empty_pad_pct");
            let susp = rate("watchdog_suspected_pct");
            let sum = |f: &dyn Fn(&RunReport) -> u64| runs.iter().map(|r| f(r)).sum::<u64>();
            out.push_str(&format!(
                "| {id} | {} | {} | {:.1} ± {:.1} | {:.1} | {:.0} | {} | {}/{}/{} | {}/{} | {} |\n",
                rungs.get(id).copied().unwrap_or(runs[0].rungs.start),
                runs.len(),
                mean(&rungs_h),
                stdev(&rungs_h),
                mean(&pad),
                mean(&susp),
                sum(&|r| r.watchdog.ladder_events as u64),
                sum(&|r| r.funnel.buy_ball_done),
                sum(&|r| r.funnel.throw_ball_start),
                sum(&|r| r.funnel.catches),
                sum(&|r| r.battles.won),
                sum(&|r| r.battles.lost),
                sum(&|r| r.whiteouts),
            ));
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub runs: usize,
    pub brain_hours: f64,
    pub rungs_per_hour: f64,
    pub runs_with_a_climb: usize,
    pub empty_pad_pct: f64,
    pub suspected_probe_pct: f64,
    pub ladder_events: usize,
    pub runs_with_a_ladder_event: usize,
    pub hunt_flagged_pct: f64,
    pub blocked_macro_pct: f64,
    pub buy_ball_done: u64,
    pub throws: u64,
    pub catches: u64,
    pub heals: u64,
    pub whiteouts: u64,
    pub battles_won: u64,
    pub battles_lost: u64,
    pub ratchet_rollbacks: u64,
}

/// One run to do: a checkpoint of the set, a seed.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub path: PathBuf,
    pub seed: u32,
}

/// What every child shares.
#[derive(Clone, Debug)]
pub struct ChildArgs {
    pub runtime: String,
    pub mode: String,
    pub minutes: f64,
    pub threads: usize,
    pub probe_s: f64,
    pub window_s: f64,
    pub dataset: Option<PathBuf>,
}

/// Runs the jobs `jobs` at a time as children of this executable, calling `done` as each one
/// finishes (so a caller can write partial results). Returns the finished runs and the failures,
/// in job order.
pub fn run_jobs(
    exe: &Path,
    jobs: Vec<Job>,
    args: &ChildArgs,
    parallel: usize,
    done: &(dyn Fn(&Result<RunReport, FailedRun>) + Sync),
) -> Vec<Result<RunReport, FailedRun>> {
    let queue = Arc::new(Mutex::new(jobs.into_iter().enumerate().collect::<std::collections::VecDeque<_>>()));
    let results = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..parallel.max(1) {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            scope.spawn(move || {
                loop {
                    let next = queue.lock().expect("queue").pop_front();
                    let Some((index, job)) = next else { break };
                    let outcome = run_child(exe, &job, args);
                    done(&outcome);
                    results.lock().expect("results").push((index, outcome));
                }
            });
        }
    });
    let mut results = Arc::try_unwrap(results).expect("all workers joined").into_inner().expect("results");
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, outcome)| outcome).collect()
}

fn run_child(exe: &Path, job: &Job, args: &ChildArgs) -> Result<RunReport, FailedRun> {
    let fail = |error: String| FailedRun { checkpoint: job.id.clone(), seed: job.seed, error };
    let mut command = Command::new(exe);
    command
        .arg("run-one")
        .arg("--id")
        .arg(&job.id)
        .arg("--checkpoint")
        .arg(&job.path)
        .arg("--seed")
        .arg(job.seed.to_string())
        .arg("--runtime")
        .arg(&args.runtime)
        .arg("--mode")
        .arg(&args.mode)
        .arg("--minutes")
        .arg(args.minutes.to_string())
        .arg("--threads")
        .arg(args.threads.to_string())
        .arg("--probe-s")
        .arg(args.probe_s.to_string())
        .arg("--window-s")
        .arg(args.window_s.to_string());
    if let Some(dataset) = &args.dataset {
        command.arg("--dataset").arg(dataset);
    }
    // The report travels in a file: the emulator library prints the cartridge's header on stdout.
    let report = tempfile::NamedTempFile::new().map_err(|e| fail(format!("a scratch file: {e}")))?;
    command.arg("--out").arg(report.path());
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| fail(format!("starting the child: {e}")))?;
    if !status.success() {
        return Err(fail(format!("the child exited with {status}")));
    }
    let text = std::fs::read(report.path()).map_err(|e| fail(format!("the child's report: {e}")))?;
    serde_json::from_slice::<RunReport>(&text).map_err(|e| fail(format!("the child's report: {e}")))
}

/// Appends a line to a log, ignoring a failure to: progress is a courtesy.
pub fn note(path: &Path, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_is_resolved_from_the_environment_and_names_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.checkpoint");
        std::fs::write(&file, b"x").unwrap();
        let set = CheckpointSet {
            schema: SET_SCHEMA.to_owned(),
            checkpoints: vec![
                SetEntry { id: "a".into(), rung: 8, env: "FLY_A".into(), note: String::new() },
                SetEntry { id: "b".into(), rung: 9, env: "FLY_B".into(), note: String::new() },
                SetEntry { id: "c".into(), rung: 9, env: "FLY_C".into(), note: String::new() },
            ],
        };
        let env = |name: &str| match name {
            "FLY_A" => Some(file.display().to_string()),
            "FLY_C" => Some("/nonexistent/c.checkpoint".to_owned()),
            _ => None,
        };
        let (found, missing) = set.resolve(&env);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.id, "a");
        assert_eq!(missing, vec!["b ($FLY_B)".to_owned(), "c ($FLY_C)".to_owned()]);
    }

    #[test]
    fn the_shipped_set_parses_and_has_unique_ids() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../tools/scorecard/checkpoints.json");
        let set = CheckpointSet::load(&path).expect("the shipped set");
        assert!(set.checkpoints.len() >= 8);
        assert!(set.checkpoints.iter().all(|e| e.env.starts_with("FLY_") && e.env.ends_with("_CHECKPOINT")));
        assert!(set.checkpoints.iter().all(|e| (8..=15).contains(&e.rung)));
    }

    #[test]
    fn a_suite_round_trips_and_summarises() {
        let mut run = RunReport {
            checkpoint: "r08".into(),
            seed: 1,
            runtime: "session".into(),
            mode: "macros".into(),
            minutes: 12.0,
            frames: 1000,
            ..RunReport::default()
        };
        run.rungs.gained = 1;
        run.watchdog.probes = 4;
        run.macros.done = 9;
        run.macros.blocked = 1;
        let suite = SuiteReport {
            schema: SCHEMA.to_owned(),
            label: "t".into(),
            build: BuildInfo::default(),
            runtime: "session".into(),
            mode: "macros".into(),
            config: SuiteConfig { minutes: 12.0, seeds: vec![1], ..SuiteConfig::default() },
            runs: vec![run],
            failed_runs: vec![FailedRun { checkpoint: "r09".into(), seed: 1, error: "boom".into() }],
        };
        let text = serde_json::to_string(&suite).unwrap();
        let back: SuiteReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back, suite);
        let h = back.headline();
        assert!((h.rungs_per_hour - 5.0).abs() < 1e-9);
        assert!((h.blocked_macro_pct - 10.0).abs() < 1e-9);
        let md = back.markdown(&BTreeMap::new());
        assert!(md.contains("r08") && md.contains("1 runs failed") && md.contains("boom"));
    }
}
