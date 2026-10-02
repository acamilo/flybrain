//! Release A against release B: a verdict per metric from the runs, not from one run.
//!
//! The runs of the two suites are paired by `(checkpoint, seed)`. Each pair gives one difference
//! per metric, signed so that positive means B is worse. A metric fails when B is worse than A by
//! more than its tolerance **and** a sign-flip permutation test over the pairs says the shortfall
//! is not noise (one-sided, `alpha`). Otherwise it passes, and when B is better by the same two
//! tests it says so. A metric with no direction (`info`) is only reported.
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
        read: |r| per_hour(r.funnel.throw_ball_start as f64, r),
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
    /// Every metric that failed, in order.
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
    let mut metrics = Vec::new();
    for metric in METRICS {
        let va: Vec<f64> = pairs.iter().map(|(ra, _)| (metric.read)(ra)).collect();
        let vb: Vec<f64> = pairs.iter().map(|(_, rb)| (metric.read)(rb)).collect();
        let (mean_a, mean_b) = (mean(&va), mean(&vb));
        let delta = mean_b - mean_a;
        let tolerance = metric.abs_tol.max(metric.rel_tol * mean_a.abs());
        // Signed so that positive is worse.
        let sign = match metric.direction {
            Direction::Up => -1.0,
            Direction::Down | Direction::Info => 1.0,
        };
        let worse: Vec<f64> = va.iter().zip(&vb).map(|(x, y)| sign * (y - x)).collect();
        let better: Vec<f64> = worse.iter().map(|d| -d).collect();
        let (p_worse, p_better) = (sign_flip_p(&worse), sign_flip_p(&better));
        let shortfall = sign * delta;
        let verdict = if pairs.is_empty() {
            Verdict::None
        } else if metric.direction == Direction::Info {
            Verdict::Info
        } else if shortfall > tolerance && p_worse < alpha {
            Verdict::Fail
        } else if -shortfall > tolerance && p_better < alpha {
            Verdict::Better
        } else {
            Verdict::Pass
        };
        metrics.push(MetricVerdict {
            metric: metric.name.to_owned(),
            unit: metric.unit.to_owned(),
            direction: metric.direction,
            pairs: pairs.len(),
            mean_a,
            mean_b,
            delta,
            p_worse,
            p_better,
            tolerance,
            verdict,
        });
    }
    let failed: Vec<String> = metrics
        .iter()
        .filter(|m| m.verdict == Verdict::Fail)
        .map(|m| m.metric.clone())
        .collect();
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
            "{} paired runs (alpha {} one-sided, sign-flip test); {} only in {}, {} only in {}.\n\n",
            self.pairs, self.alpha, self.unpaired_a, self.a, self.unpaired_b, self.b
        ));
        for note in &self.notes {
            out.push_str(&format!("- note: {note}\n"));
        }
        if !self.notes.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "| metric | {} | {} | delta | tolerance | p worse | p better | verdict |\n",
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
                m.p_worse,
                m.p_better,
                match m.verdict {
                    Verdict::Pass => "pass",
                    Verdict::Better => "better",
                    Verdict::Fail => "**FAIL**",
                    Verdict::Info => "info",
                    Verdict::None => "n/a",
                }
            ));
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
