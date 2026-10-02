//! Release A against release B: a verdict per metric from the runs, not from one run.
//!
//! The runs of the two suites are paired by `(checkpoint, seed)`. Each pair gives one difference
//! per metric, signed so that positive means B is worse. A metric fails when B is worse than A by
//! more than its tolerance **and** a sign-flip permutation test over the pairs says the shortfall
//! is not noise (one-sided, `alpha`, Holm-adjusted over the judged metrics so the whole compare
//! has a family-wise false-fail rate of at most `alpha` under "nothing changed", where the
//! unadjusted per-metric test had about 15% over 13 metrics). Otherwise it passes, and when B is
//! better by the same two tests it says so. A metric with no direction (`info`) is only reported.
//!
//! The pooled test cannot see a trap confined to one place (three seeds of one checkpoint among
//! thirty-six pairs), so a second, non-statistical rule runs per checkpoint: see [`trap_rule`].
//!
//! The test is exact up to 20 pairs and a fixed-seed Monte-Carlo beyond, so the same two suites
//! always give the same verdicts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::suite::SuiteReport;
use crate::tally::RunReport;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// More is better.
    Up,
    /// Less is better.
    Down,
    /// Reported, never judged.
    Info,
}

/// One metric: how to read it off a run, which way is good, and how big a change must be before
/// it can count.
pub struct Metric {
    pub name: &'static str,
    pub unit: &'static str,
    pub direction: Direction,
    /// A change smaller than this (in `unit`) is never a regression or an improvement.
    pub abs_tol: f64,
    /// ... nor one smaller than this share of A's mean.
    pub rel_tol: f64,
    pub read: fn(&RunReport) -> f64,
}

fn per_hour(count: f64, run: &RunReport) -> f64 {
    if run.minutes <= 0.0 { 0.0 } else { count * 60.0 / run.minutes }
}

fn pct(part: f64, whole: f64) -> f64 {
    if whole <= 0.0 { 0.0 } else { 100.0 * part / whole }
}

/// The scorecard's metrics, in report order.
pub const METRICS: &[Metric] = &[
    Metric {
        name: "rungs_per_hour",
        unit: "rungs/h",
        direction: Direction::Up,
        abs_tol: 0.5,
        rel_tol: 0.15,
        read: |r| per_hour(f64::from(r.rungs.gained), r),
    },
    Metric {
        name: "places_per_hour",
        unit: "places/h",
        direction: Direction::Up,
        abs_tol: 5.0,
        rel_tol: 0.15,
        read: |r| per_hour(f64::from(r.places_end.saturating_sub(r.places_start)), r),
    },
    Metric {
        name: "empty_pad_pct",
        unit: "% of frames",
        direction: Direction::Down,
        abs_tol: 1.0,
        rel_tol: 0.25,
        read: |r| 100.0 * r.pad.empty_fraction,
    },
    Metric {
        name: "watchdog_suspected_pct",
        unit: "% of probes",
        direction: Direction::Down,
        abs_tol: 5.0,
        rel_tol: 0.25,
        read: |r| pct(r.watchdog.suspected as f64, r.watchdog.probes as f64),
    },
    Metric {
        name: "ladder_events_per_hour",
        unit: "traps/h",
        direction: Direction::Down,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.watchdog.ladder_events as f64, r),
    },
    // The trap hunt's windows are reported, not judged: a fly that fights for most of a run
    // presses NEXT through battle text more than ten times in two minutes, which that instrument
    // reads as a repeated sequence, so on these checkpoints nine windows in ten are flagged and
    // the number has no room to move. The watchdog's rules above are the judged ones.
    Metric {
        name: "hunt_flagged_pct",
        unit: "% of windows",
        direction: Direction::Info,
        abs_tol: 0.0,
        rel_tol: 0.0,
        read: |r| pct(r.hunt.flagged as f64, r.hunt.windows as f64),
    },
    Metric {
        name: "ratchet_rollbacks_per_hour",
        unit: "rollbacks/h",
        direction: Direction::Down,
        abs_tol: 1.0,
        rel_tol: 0.25,
        read: |r| per_hour(f64::from(r.ratchet.rollbacks), r),
    },
    Metric {
        name: "blocked_macro_pct",
        unit: "% of finishes",
        direction: Direction::Down,
        abs_tol: 2.0,
        rel_tol: 0.25,
        read: |r| {
            let finishes = r.macros.done + r.macros.failed();
            pct(r.macros.failed() as f64, finishes as f64)
        },
    },
    Metric {
        name: "buy_ball_per_hour",
        unit: "buys/h",
        direction: Direction::Up,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.funnel.buy_ball_done as f64, r),
    },
    Metric {
        name: "throws_per_hour",
        unit: "throws/h",
        direction: Direction::Up,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.funnel.throw_ball_done as f64, r),
    },
    // Reported, never judged on its own: a blocked THROW BALL storm is the trap rule's and the
    // watchdog's business, and `throws_per_hour` above counts only throws that happened.
    Metric {
        name: "throw_ball_blocked_per_hour",
        unit: "blocked/h",
        direction: Direction::Info,
        abs_tol: 0.0,
        rel_tol: 0.0,
        read: |r| per_hour(r.macros.by_macro.get("THROW BALL").map_or(0, |c| c.blocked) as f64, r),
    },
    Metric {
        name: "catches_per_hour",
        unit: "catches/h",
        direction: Direction::Up,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.funnel.catches as f64, r),
    },
    Metric {
        name: "battles_won_per_hour",
        unit: "wins/h",
        direction: Direction::Up,
        abs_tol: 1.0,
        rel_tol: 0.2,
        read: |r| per_hour(r.battles.won as f64, r),
    },
    Metric {
        name: "battles_lost_per_hour",
        unit: "losses/h",
        direction: Direction::Down,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.battles.lost as f64, r),
    },
    Metric {
        name: "whiteouts_per_hour",
        unit: "whiteouts/h",
        direction: Direction::Down,
        abs_tol: 0.5,
        rel_tol: 0.25,
        read: |r| per_hour(r.whiteouts as f64, r),
    },
    // Reported, never judged: healing more can follow losing more.
    Metric {
        name: "heals_per_hour",
        unit: "heals/h",
        direction: Direction::Info,
        abs_tol: 0.0,
        rel_tol: 0.0,
        read: |r| per_hour(r.heals.heal_done as f64, r),
    },
    Metric {
        name: "macro_starts_per_minute",
        unit: "starts/min",
        direction: Direction::Info,
        abs_tol: 0.0,
        rel_tol: 0.0,
        read: |r| if r.minutes <= 0.0 { 0.0 } else { r.macros.starts as f64 / r.minutes },
    },
];

pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
}

pub fn stdev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() - 1) as f64).sqrt()
}

/// One-sided sign-flip p-value for "the mean of `diffs` is greater than zero by more than chance":
/// the share of sign assignments whose mean is at least the observed one. Exact for up to 20
/// differences, 20,000 fixed-seed draws beyond.
pub fn sign_flip_p(diffs: &[f64]) -> f64 {
    let n = diffs.len();
    if n == 0 {
        return 1.0;
    }
    let observed: f64 = diffs.iter().sum();
    let eps = 1e-9 * (1.0 + observed.abs());
    if n <= 20 {
        let total = 1u64 << n;
        let mut at_least = 0u64;
        for mask in 0..total {
            let sum: f64 = diffs
                .iter()
                .enumerate()
                .map(|(i, d)| if mask >> i & 1 == 1 { -d } else { *d })
                .sum();
            if sum >= observed - eps {
                at_least += 1;
            }
        }
        return at_least as f64 / total as f64;
    }
    let draws = 20_000u64;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut at_least = 1u64; // the observed assignment is one of them
    for _ in 0..draws {
        let mut sum = 0.0;
        let mut bits = 0u64;
        for (i, d) in diffs.iter().enumerate() {
            if i % 64 == 0 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bits = state;
            }
            sum += if bits >> (i % 64) & 1 == 1 { -d } else { *d };
        }
        if sum >= observed - eps {
            at_least += 1;
        }
    }
    at_least as f64 / (draws + 1) as f64
}

/// Holm's step-down adjustment: the adjusted p of each value (same order), so that rejecting
/// where `adjusted < alpha` controls the family-wise error rate at `alpha` under any dependence
/// between the tests.
pub fn holm_adjust(ps: &[f64]) -> Vec<f64> {
    let m = ps.len();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&i, &j| ps[i].total_cmp(&ps[j]));
    let mut adjusted = vec![1.0; m];
    let mut running = 0.0f64;
    for (rank, &i) in order.iter().enumerate() {
        running = running.max(((m - rank) as f64 * ps[i]).min(1.0));
        adjusted[i] = running;
    }
    adjusted
}

/// Blocked finishes of one macro in one run that make a storm no watchdog window is needed to
/// read: the row-71 bag bug blocked THROW BALL 131 times in ten brain minutes.
pub const STORM_BLOCKED: u64 = 50;
/// Trapped runs of B on a checkpoint (and this many more than A) that fail the checkpoint.
pub const TRAP_RUNS: usize = 2;

/// Why a run counts as trapped, if it does: a recovery-ladder event (two suspected watchdog
/// probes in a row, any rule including stalled and zero-progress), at least half the probes
/// suspected, or a blocked-macro storm.
pub fn trapped(run: &RunReport) -> Option<String> {
    if let Some((name, n)) = storm(run) {
        return Some(format!("{n} blocked {name}"));
    }
    let w = &run.watchdog;
    if w.ladder_events > 0 {
        return Some(format!("{} ladder event(s), {}/{} probes suspected", w.ladder_events, w.suspected, w.probes));
    }
    if w.probes >= 2 && w.suspected * 2 >= w.probes {
        return Some(format!("{}/{} probes suspected", w.suspected, w.probes));
    }
    None
}

/// The macro with the most blocked finishes in the run when that is a storm.
pub fn storm(run: &RunReport) -> Option<(String, u64)> {
    run.macros
        .by_macro
        .iter()
        .map(|(name, c)| (name.clone(), c.blocked))
        .filter(|(_, n)| *n >= STORM_BLOCKED)
        .max_by_key(|(_, n)| *n)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckpointTrap {
    pub checkpoint: String,
    pub runs_a: usize,
    pub runs_b: usize,
    pub trapped_a: usize,
    pub trapped_b: usize,
    pub storms_a: usize,
    pub storms_b: usize,
    /// What B's trapped runs looked like (seed: reason).
    pub detail: Vec<String>,
}

/// The per-checkpoint trap rule, independent of the cross-seed test: a checkpoint FAILS when B is
/// trapped on at least `TRAP_RUNS` of its runs and on at least `TRAP_RUNS` more than A, or when B
/// has a blocked-macro storm on any run and A has none on that checkpoint. A fault confined to one
/// place cannot fail the pooled test (three seeds give a sign-flip p of at least 1/8, and the
/// shift is diluted over every checkpoint), but it is exactly the regression class this exists
/// for. Returns the failing checkpoints.
pub fn trap_rule(a: &SuiteReport, b: &SuiteReport) -> Vec<CheckpointTrap> {
    let mut ids: Vec<&str> = b.runs.iter().map(|r| r.checkpoint.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut failing = Vec::new();
    for id in ids {
        let ra: Vec<&RunReport> = a.runs.iter().filter(|r| r.checkpoint == id).collect();
        let rb: Vec<&RunReport> = b.runs.iter().filter(|r| r.checkpoint == id).collect();
        if ra.is_empty() {
            continue;
        }
        let trapped_a = ra.iter().filter(|r| trapped(r).is_some()).count();
        let trapped_b = rb.iter().filter(|r| trapped(r).is_some()).count();
        let storms_a = ra.iter().filter(|r| storm(r).is_some()).count();
        let storms_b = rb.iter().filter(|r| storm(r).is_some()).count();
        let fails = (trapped_b >= TRAP_RUNS && trapped_b >= trapped_a + TRAP_RUNS) || (storms_b > 0 && storms_a == 0);
        if fails {
            failing.push(CheckpointTrap {
                checkpoint: id.to_owned(),
                runs_a: ra.len(),
                runs_b: rb.len(),
                trapped_a,
                trapped_b,
                storms_a,
                storms_b,
                detail: rb.iter().filter_map(|r| trapped(r).map(|why| format!("seed {}: {why}", r.seed))).collect(),
            });
        }
    }
    failing
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Better,
    Fail,
    Info,
    /// No run of A pairs with a run of B.
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricVerdict {
    pub metric: String,
    pub unit: String,
    pub direction: Direction,
    pub pairs: usize,
    pub mean_a: f64,
    pub mean_b: f64,
    pub delta: f64,
    /// The one-sided p of "B is worse" and of "B is better".
    pub p_worse: f64,
    pub p_better: f64,
    /// The same, Holm-adjusted over the judged metrics (what the verdict uses).
    #[serde(default)]
    pub p_worse_adj: f64,
    #[serde(default)]
    pub p_better_adj: f64,
    /// The smallest change that counts, in the metric's unit.
    pub tolerance: f64,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub schema: String,
    pub a: String,
    pub b: String,
    pub alpha: f64,
    pub pairs: usize,
    pub unpaired_a: usize,
    pub unpaired_b: usize,
    pub notes: Vec<String>,
    pub metrics: Vec<MetricVerdict>,
    /// Checkpoints the per-checkpoint trap rule failed.
    #[serde(default)]
    pub trapped_checkpoints: Vec<CheckpointTrap>,
    /// Every metric that failed, in order, then `trap@<checkpoint>` for each failed checkpoint.
    pub failed: Vec<String>,
    pub pass: bool,
}

fn key(run: &RunReport) -> (String, u32) {
    (run.checkpoint.clone(), run.seed)
}

/// Compares two suites. `alpha` is the one-sided significance level.
pub fn compare(a: &SuiteReport, b: &SuiteReport, alpha: f64) -> Comparison {
    let by_key = |suite: &SuiteReport| -> BTreeMap<(String, u32), RunReport> {
        suite.runs.iter().map(|run| (key(run), run.clone())).collect()
    };
    let runs_a = by_key(a);
    let runs_b = by_key(b);
    let pairs: Vec<(&RunReport, &RunReport)> =
        runs_a.iter().filter_map(|(k, ra)| runs_b.get(k).map(|rb| (ra, rb))).collect();
    let mut notes = Vec::new();
    if a.config.minutes != b.config.minutes {
        notes.push(format!(
            "the suites ran different lengths ({} and {} brain minutes): rates are comparable, counts are not",
            a.config.minutes, b.config.minutes
        ));
    }
    if a.runtime != b.runtime {
        notes.push(format!("the suites ran on different runtimes ({} and {})", a.runtime, b.runtime));
    }
    if a.config.probe_s != b.config.probe_s || a.config.window_s != b.config.window_s {
        notes.push("the watchdog probes differ between the suites".to_owned());
    }
    if pairs.len() < 8 {
        notes.push(format!(
            "only {} paired runs: the permutation test cannot reach alpha {alpha} with fewer than {} pairs",
            pairs.len(),
            (1.0 / alpha).log2().ceil() as usize
        ));
    }
    // First the raw p-values of every metric, then Holm over the judged ones.
    struct Raw {
        mean_a: f64,
        mean_b: f64,
        delta: f64,
        tolerance: f64,
        shortfall: f64,
        p_worse: f64,
        p_better: f64,
    }
    let raws: Vec<Raw> = METRICS
        .iter()
        .map(|metric| {
            let va: Vec<f64> = pairs.iter().map(|(ra, _)| (metric.read)(ra)).collect();
            let vb: Vec<f64> = pairs.iter().map(|(_, rb)| (metric.read)(rb)).collect();
            let (mean_a, mean_b) = (mean(&va), mean(&vb));
            let delta = mean_b - mean_a;
            // Signed so that positive is worse.
            let sign = match metric.direction {
                Direction::Up => -1.0,
                Direction::Down | Direction::Info => 1.0,
            };
            let worse: Vec<f64> = va.iter().zip(&vb).map(|(x, y)| sign * (y - x)).collect();
            let better: Vec<f64> = worse.iter().map(|d| -d).collect();
            Raw {
                mean_a,
                mean_b,
                delta,
                tolerance: metric.abs_tol.max(metric.rel_tol * mean_a.abs()),
                shortfall: sign * delta,
                p_worse: sign_flip_p(&worse),
                p_better: sign_flip_p(&better),
            }
        })
        .collect();
    let judged: Vec<usize> = (0..METRICS.len()).filter(|&i| METRICS[i].direction != Direction::Info).collect();
    let adj_worse = holm_adjust(&judged.iter().map(|&i| raws[i].p_worse).collect::<Vec<_>>());
    let adj_better = holm_adjust(&judged.iter().map(|&i| raws[i].p_better).collect::<Vec<_>>());
    let mut metrics = Vec::new();
    for (i, metric) in METRICS.iter().enumerate() {
        let raw = &raws[i];
        let slot = judged.iter().position(|&j| j == i);
        let (p_worse_adj, p_better_adj) = slot.map_or((raw.p_worse, raw.p_better), |k| (adj_worse[k], adj_better[k]));
        let verdict = if pairs.is_empty() {
            Verdict::None
        } else if metric.direction == Direction::Info {
            Verdict::Info
        } else if raw.shortfall > raw.tolerance && p_worse_adj < alpha {
            Verdict::Fail
        } else if -raw.shortfall > raw.tolerance && p_better_adj < alpha {
            Verdict::Better
        } else {
            Verdict::Pass
        };
        metrics.push(MetricVerdict {
            metric: metric.name.to_owned(),
            unit: metric.unit.to_owned(),
            direction: metric.direction,
            pairs: pairs.len(),
            mean_a: raw.mean_a,
            mean_b: raw.mean_b,
            delta: raw.delta,
            p_worse: raw.p_worse,
            p_better: raw.p_better,
            p_worse_adj,
            p_better_adj,
            tolerance: raw.tolerance,
            verdict,
        });
    }
    let trapped_checkpoints = trap_rule(a, b);
    let mut failed: Vec<String> = metrics
        .iter()
        .filter(|m| m.verdict == Verdict::Fail)
        .map(|m| m.metric.clone())
        .collect();
    failed.extend(trapped_checkpoints.iter().map(|t| format!("trap@{}", t.checkpoint)));
    Comparison {
        schema: "fly-scorecard-compare-v1".to_owned(),
        a: a.label.clone(),
        b: b.label.clone(),
        alpha,
        pairs: pairs.len(),
        unpaired_a: runs_a.len() - pairs.len(),
        unpaired_b: runs_b.len() - pairs.len(),
        notes,
        pass: failed.is_empty() && !pairs.is_empty(),
        failed,
        metrics,
        trapped_checkpoints,
    }
}

impl Comparison {
    /// The comparison as a markdown table.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Scorecard: {} vs {}: {}\n\n",
            self.a,
            self.b,
            if self.pass { "PASS" } else { "FAIL" }
        ));
        out.push_str(&format!(
            "{} paired runs (alpha {} one-sided, sign-flip test, Holm over the judged metrics); {} only in {}, {} only in {}.\n\n",
            self.pairs, self.alpha, self.unpaired_a, self.a, self.unpaired_b, self.b
        ));
        for note in &self.notes {
            out.push_str(&format!("- note: {note}\n"));
        }
        if !self.notes.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "| metric | {} | {} | delta | tolerance | p worse (Holm) | p better (Holm) | verdict |\n",
            self.a, self.b
        ));
        out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n");
        for m in &self.metrics {
            let arrow = match m.direction {
                Direction::Up => "up",
                Direction::Down => "down",
                Direction::Info => "info",
            };
            out.push_str(&format!(
                "| {} ({}, {}) | {:.2} | {:.2} | {:+.2} | {:.2} | {:.3} | {:.3} | {} |\n",
                m.metric,
                arrow,
                m.unit,
                m.mean_a,
                m.mean_b,
                m.delta,
                m.tolerance,
                m.p_worse_adj,
                m.p_better_adj,
                match m.verdict {
                    Verdict::Pass => "pass",
                    Verdict::Better => "better",
                    Verdict::Fail => "**FAIL**",
                    Verdict::Info => "info",
                    Verdict::None => "n/a",
                }
            ));
        }
        if !self.trapped_checkpoints.is_empty() {
            out.push_str("\n**Per-checkpoint trap rule: FAIL**\n\n");
            for t in &self.trapped_checkpoints {
                out.push_str(&format!(
                    "- `{}`: {} trapped / {} storm run(s) of {} in {}, against {} / {} of {} in {} ({})\n",
                    t.checkpoint,
                    t.trapped_b,
                    t.storms_b,
                    t.runs_b,
                    self.b,
                    t.trapped_a,
                    t.storms_a,
                    t.runs_a,
                    self.a,
                    t.detail.join("; ")
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::{SuiteConfig, SuiteReport};

    #[test]
    fn the_exact_test_has_the_textbook_p() {
        // Seven positive differences: only the all-positive assignment is as extreme.
        assert!((sign_flip_p(&[1.0; 7]) - 1.0 / 128.0).abs() < 1e-12);
        // All-negative: every assignment is at least as extreme as the observed.
        assert!((sign_flip_p(&[-1.0; 7]) - 1.0).abs() < 1e-12);
        // Nothing observed is no evidence.
        assert_eq!(sign_flip_p(&[]), 1.0);
        assert!((sign_flip_p(&[0.0; 5]) - 1.0).abs() < 1e-12);
        // Mixed: three of four positive, the largest positive.
        let p = sign_flip_p(&[3.0, 1.0, 1.0, -1.0]);
        assert!(p > 0.0 && p < 0.5, "{p}");
    }

    #[test]
    fn beyond_twenty_pairs_the_draws_are_fixed() {
        let diffs: Vec<f64> = (0..30).map(|i| 1.0 + f64::from(i % 3)).collect();
        let p = sign_flip_p(&diffs);
        assert_eq!(p, sign_flip_p(&diffs));
        assert!(p < 0.001, "{p}");
        let noise: Vec<f64> = (0..30).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        assert!(sign_flip_p(&noise) > 0.3);
    }

    fn run(checkpoint: &str, seed: u32, mutate: impl FnOnce(&mut RunReport)) -> RunReport {
        let mut r = RunReport {
            checkpoint: checkpoint.to_owned(),
            seed,
            runtime: "session".into(),
            mode: "macros".into(),
            minutes: 12.0,
            ..RunReport::default()
        };
        r.macros.done = 100;
        mutate(&mut r);
        r
    }

    fn suite(label: &str, runs: Vec<RunReport>) -> SuiteReport {
        SuiteReport {
            schema: crate::suite::SCHEMA.to_owned(),
            label: label.to_owned(),
            build: Default::default(),
            runtime: "session".into(),
            mode: "macros".into(),
            config: SuiteConfig { minutes: 12.0, seeds: vec![1, 2, 3], ..SuiteConfig::default() },
            runs,
            failed_runs: Vec::new(),
        }
    }

    fn many(label: &str, f: impl Fn(&str, u32, &mut RunReport)) -> SuiteReport {
        let mut runs = Vec::new();
        for c in ["r08", "r09", "r10", "r11", "r12", "r13"] {
            for s in 1..=3 {
                runs.push(run(c, s, |r| f(c, s, r)));
            }
        }
        suite(label, runs)
    }

    #[test]
    fn identical_suites_pass_with_nothing_better_or_worse() {
        let a = many("a", |_, s, r| r.rungs.gained = s % 2);
        let b = many("b", |_, s, r| r.rungs.gained = s % 2);
        let c = compare(&a, &b, 0.05);
        assert!(c.pass, "{:?}", c.failed);
        assert!(c.metrics.iter().all(|m| matches!(m.verdict, Verdict::Pass | Verdict::Info)));
        assert_eq!(c.pairs, 18);
    }

    #[test]
    fn a_consistent_drop_in_rungs_fails_and_a_single_bad_run_does_not() {
        let a = many("a", |_, _, r| r.rungs.gained = 1);
        let b = many("b", |_, _, r| r.rungs.gained = 0);
        let c = compare(&a, &b, 0.05);
        assert!(!c.pass);
        assert_eq!(c.failed, vec!["rungs_per_hour".to_owned()]);
        assert!(c.markdown().contains("**FAIL**"));
        // One run of eighteen collapses: not significant, not a regression.
        let b = many("b", |ck, s, r| r.rungs.gained = if ck == "r10" && s == 2 { 0 } else { 1 });
        assert!(compare(&a, &b, 0.05).pass);
    }

    #[test]
    fn a_rise_in_traps_fails_and_a_drop_is_better() {
        let calm = many("a", |_, _, _| {});
        let trapped = many("b", |_, _, r| {
            r.watchdog.ladder_events = 1;
            r.watchdog.probes = 4;
            r.watchdog.suspected = 3;
        });
        let up = compare(&calm, &trapped, 0.05);
        assert!(up.failed.contains(&"ladder_events_per_hour".to_owned()), "{:?}", up.failed);
        assert!(up.failed.contains(&"watchdog_suspected_pct".to_owned()));
        let down = compare(&trapped, &calm, 0.05);
        assert!(down.pass);
        let better: Vec<&str> = down
            .metrics
            .iter()
            .filter(|m| m.verdict == Verdict::Better)
            .map(|m| m.metric.as_str())
            .collect();
        assert!(better.contains(&"ladder_events_per_hour"), "{better:?}");
    }

    #[test]
    fn a_change_inside_the_tolerance_is_noise_however_consistent() {
        // 0.25 pp more empty pad on every run: significant, but under the 1 pp floor.
        let a = many("a", |_, _, r| r.pad.empty_fraction = 0.10);
        let b = many("b", |_, _, r| r.pad.empty_fraction = 0.1025);
        let c = compare(&a, &b, 0.05);
        let pad = c.metrics.iter().find(|m| m.metric == "empty_pad_pct").unwrap();
        assert_eq!(pad.verdict, Verdict::Pass, "{pad:?}");
    }

    #[test]
    fn runs_pair_by_checkpoint_and_seed_and_unpaired_runs_are_reported() {
        let a = many("a", |_, _, _| {});
        let mut b = many("b", |_, _, _| {});
        b.runs.retain(|r| r.checkpoint != "r13");
        let c = compare(&a, &b, 0.05);
        assert_eq!((c.pairs, c.unpaired_a, c.unpaired_b), (15, 3, 0));
        let none = compare(&a, &suite("empty", Vec::new()), 0.05);
        assert!(!none.pass);
        assert!(none.metrics.iter().all(|m| m.verdict == Verdict::None));
    }

    fn row71(r: &mut RunReport) {
        // The v0.7.3 row-71 bag bug: THROW BALL started and blocked 131 times, suspected on every probe.
        let c = r.macros.by_macro.entry("THROW BALL".into()).or_default();
        c.start = 132;
        c.blocked = 131;
        r.macros.blocked = 131;
        r.watchdog.probes = 4;
        r.watchdog.suspected = 4;
        r.watchdog.ladder_events = 1;
    }

    #[test]
    fn a_trap_on_one_checkpoint_fails_though_the_pooled_test_cannot_see_it() {
        let a = many("a", |_, _, _| {});
        let b = many("b", |ck, _, r| {
            if ck == "r10" {
                row71(r)
            }
        });
        let c = compare(&a, &b, 0.05);
        assert!(!c.pass);
        assert!(c.failed.contains(&"trap@r10".to_owned()), "{:?}", c.failed);
        assert_eq!(c.trapped_checkpoints.len(), 1);
        assert_eq!(c.trapped_checkpoints[0].trapped_b, 3);
        assert!(c.markdown().contains("`r10`"));
        // ... while the same suite against itself passes.
        assert!(compare(&b, &b, 0.05).pass);
        assert!(compare(&a, &a, 0.05).pass);
    }

    #[test]
    fn the_trap_rule_wants_two_seeds_or_a_storm_and_a_clean_a() {
        let a = many("a", |_, _, _| {});
        // One trapped seed with no storm: not enough.
        let one = many("b", |ck, s, r| {
            if ck == "r10" && s == 1 {
                r.watchdog.probes = 4;
                r.watchdog.suspected = 3;
                r.watchdog.ladder_events = 1;
            }
        });
        assert!(trap_rule(&a, &one).is_empty());
        // Two trapped seeds fail it.
        let two = many("b", |ck, s, r| {
            if ck == "r10" && s <= 2 {
                r.watchdog.probes = 4;
                r.watchdog.suspected = 3;
                r.watchdog.ladder_events = 1;
            }
        });
        assert_eq!(trap_rule(&a, &two)[0].checkpoint, "r10");
        // One seed with a storm fails it.
        let storm_one = many("b", |ck, s, r| {
            if ck == "r11" && s == 3 {
                r.macros.by_macro.entry("GO OUT".into()).or_default().blocked = 50;
            }
        });
        assert_eq!(trap_rule(&a, &storm_one)[0].checkpoint, "r11");
        // The same storm in A as well is not new.
        assert!(trap_rule(&storm_one, &storm_one).is_empty());
        // A was trapped on two seeds already: B trapped on the same two is not a new trap.
        assert!(trap_rule(&two, &two).is_empty());
    }

    #[test]
    fn a_blocked_throw_is_not_a_throw() {
        let r = run("x", 1, |r| {
            r.funnel.throw_ball_start = 132;
            r.funnel.throw_ball_done = 1;
            row71(r);
        });
        let get = |name: &str| (METRICS.iter().find(|m| m.name == name).unwrap().read)(&r);
        assert!((get("throws_per_hour") - 5.0).abs() < 1e-9);
        assert!((get("throw_ball_blocked_per_hour") - 131.0 * 5.0).abs() < 1e-9);
    }

    #[test]
    fn holm_adjusts_step_down_and_never_below_the_raw_p() {
        let adj = holm_adjust(&[0.01, 0.04, 0.03, 0.20]);
        let want = [0.04, 0.09, 0.09, 0.20];
        for (x, y) in adj.iter().zip(want) {
            assert!((x - y).abs() < 1e-12, "{adj:?}");
        }
        assert!(holm_adjust(&[]).is_empty());
        assert_eq!(holm_adjust(&[0.6, 0.9]), vec![1.0, 1.0]);
    }

    /// The false-fail rate of the whole compare under "nothing changed but the trajectory":
    /// resample each checkpoint's seeds with replacement (A is the committed v0.7.5 card, B a
    /// bootstrap of it) and count failing compares. Slow; run with `--ignored --nocapture`.
    #[test]
    #[ignore = "a simulation: about a minute in release"]
    fn null_false_fail_rate() {
        let text = include_str!("../../../../../tools/scorecard/baseline-v0.7.5.json");
        let a: SuiteReport = serde_json::from_str(text).expect("baseline");
        let mut ids: Vec<String> = a.runs.iter().map(|r| r.checkpoint.clone()).collect();
        ids.dedup();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let (reps, mut fails, mut metric_fails, mut trap_fails) = (200, 0, 0, 0);
        let mut by_checkpoint = BTreeMap::new();
        for _ in 0..reps {
            let mut b = a.clone();
            b.label = "boot".into();
            b.runs.clear();
            for id in &ids {
                let pool: Vec<&RunReport> = a.runs.iter().filter(|r| &r.checkpoint == id).collect();
                for (seed, _) in pool.iter().enumerate() {
                    let mut r = pool[next(pool.len())].clone();
                    r.seed = pool[seed].seed;
                    b.runs.push(r);
                }
            }
            let c = compare(&a, &b, 0.05);
            if !c.pass {
                fails += 1;
            }
            metric_fails += usize::from(c.failed.iter().any(|f| !f.starts_with("trap@")));
            trap_fails += usize::from(!c.trapped_checkpoints.is_empty());
            for t in &c.trapped_checkpoints {
                *by_checkpoint.entry(t.checkpoint.clone()).or_insert(0usize) += 1;
            }
        }
        println!("null compares: {fails}/{reps} fail ({metric_fails} by a metric, {trap_fails} by the trap rule)");
        println!("trap rule by checkpoint: {by_checkpoint:?}");
    }

    #[test]
    fn rates_are_per_brain_hour() {
        let r = run("x", 1, |r| {
            r.rungs.gained = 1;
            r.whiteouts = 2;
        });
        let get = |name: &str| (METRICS.iter().find(|m| m.name == name).unwrap().read)(&r);
        assert!((get("rungs_per_hour") - 5.0).abs() < 1e-9);
        assert!((get("whiteouts_per_hour") - 10.0).abs() < 1e-9);
        let empty = RunReport::default();
        assert_eq!((METRICS[0].read)(&empty), 0.0);
    }
}
