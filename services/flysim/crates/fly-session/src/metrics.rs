//! Latency and resource samples, for the measurements the implementation guide's section 5
//! asks every slice to report.
//!
//! These are local synthetic timings on one machine. No host capacity claim follows from any
//! number this module produces, and nothing here is a gameplay latency goal: the percentiles
//! exist so that the three execution modes can be compared against each other.

use std::collections::BTreeMap;

/// One sample set's order statistics, by nearest rank.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Percentiles {
    pub count: usize,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl Percentiles {
    fn of(sorted: &[u64]) -> Percentiles {
        let rank = |p: f64| -> u64 {
            if sorted.is_empty() {
                return 0;
            }
            let n = sorted.len() as f64;
            let index = (p * n).ceil() as usize;
            sorted[index.clamp(1, sorted.len()) - 1]
        };
        Percentiles {
            count: sorted.len(),
            p50_ns: rank(0.50),
            p95_ns: rank(0.95),
            p99_ns: rank(0.99),
            max_ns: sorted.last().copied().unwrap_or_default(),
        }
    }

    pub fn p50_us(&self) -> f64 {
        self.p50_ns as f64 / 1000.0
    }

    pub fn p95_us(&self) -> f64 {
        self.p95_ns as f64 / 1000.0
    }

    pub fn p99_us(&self) -> f64 {
        self.p99_ns as f64 / 1000.0
    }
}

/// Named duration samples. One name is one measured path: a domain method, or the
/// coordinator's whole critical path for a transition.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    samples: BTreeMap<String, Vec<u64>>,
}

impl Metrics {
    pub fn record(&mut self, what: &str, elapsed: std::time::Duration) {
        let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.samples.entry(what.to_owned()).or_default().push(ns);
    }

    pub fn percentiles(&self, what: &str) -> Option<Percentiles> {
        let mut values = self.samples.get(what)?.clone();
        values.sort_unstable();
        Some(Percentiles::of(&values))
    }

    pub fn names(&self) -> Vec<String> {
        self.samples.keys().cloned().collect()
    }

    pub fn count(&self, what: &str) -> usize {
        self.samples.get(what).map(Vec::len).unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// This process's peak resident set, in KiB, from its own status file.
pub fn peak_rss_kib() -> Option<u64> {
    peak_rss_of("/proc/self/status")
}

/// One child process's peak resident set, in KiB. `None` once the child is gone.
pub fn peak_rss_kib_of(pid: u32) -> Option<u64> {
    peak_rss_of(&format!("/proc/{pid}/status"))
}

fn peak_rss_of(path: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// How many physical cores this machine has, counted as distinct (package, core) pairs.
///
/// Falls back to the logical count, which is what a thread budget has to use when the
/// topology cannot be read.
pub fn physical_cores() -> usize {
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        let mut pairs: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        let (mut package, mut core) = (None, None);
        for line in text.lines() {
            if line.trim().is_empty() {
                if let (Some(p), Some(c)) = (package.take(), core.take()) {
                    pairs.insert((p, c));
                }
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                match key.trim() {
                    "physical id" => package = Some(value.trim().to_owned()),
                    "core id" => core = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
        }
        if let (Some(p), Some(c)) = (package, core) {
            pairs.insert((p, c));
        }
        if !pairs.is_empty() {
            return pairs.len();
        }
    }
    logical_cores()
}

/// How many hardware threads this machine reports.
pub fn logical_cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut m = Metrics::default();
        for ns in 1..=100u64 {
            m.record("x", std::time::Duration::from_nanos(ns));
        }
        let p = m.percentiles("x").expect("recorded");
        assert_eq!(p.count, 100);
        assert_eq!(p.p50_ns, 50);
        assert_eq!(p.p95_ns, 95);
        assert_eq!(p.p99_ns, 99);
        assert_eq!(p.max_ns, 100);
        assert!(m.percentiles("y").is_none());
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let mut m = Metrics::default();
        m.record("x", std::time::Duration::from_nanos(7));
        let p = m.percentiles("x").expect("recorded");
        assert_eq!((p.p50_ns, p.p95_ns, p.p99_ns, p.max_ns), (7, 7, 7, 7));
    }

    #[test]
    fn the_machine_reports_at_least_one_core() {
        assert!(physical_cores() >= 1);
        assert!(logical_cores() >= 1);
        assert!(peak_rss_kib().unwrap_or(1) > 0);
    }
}
