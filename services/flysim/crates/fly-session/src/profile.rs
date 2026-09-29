//! A switchable per-phase timer for measuring the session runtime (TASK-01 review N5).
//!
//! Off unless `FLY_SESSION_PROFILE` is set when the process starts; off, a span is one relaxed
//! atomic load. On, every span adds its duration to a process-wide table that [`report`] reads.
//! In-process execution puts the coordinator and every worker in one table; a worker process
//! keeps its own. Not a service metric: a measurement tool.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

static STATE: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 off, 2 on
static TABLE: Mutex<BTreeMap<&'static str, (u64, Duration)>> = Mutex::new(BTreeMap::new());

fn on() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let enabled = std::env::var_os("FLY_SESSION_PROFILE").is_some();
            STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
            enabled
        }
    }
}

/// Times until dropped.
pub struct Span(Option<(&'static str, Instant)>);

/// Starts timing `name`.
pub fn span(name: &'static str) -> Span {
    Span(on().then(|| (name, Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((name, started)) = self.0.take() {
            let mut table = TABLE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = table.entry(name).or_insert((0, Duration::ZERO));
            entry.0 += 1;
            entry.1 += started.elapsed();
        }
    }
}

/// `(name, count, mean)` for every span recorded, then clears the table.
pub fn report() -> Vec<(&'static str, u64, Duration)> {
    let mut table = TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let out = table
        .iter()
        .map(|(name, (count, total))| (*name, *count, *total / (*count).max(1) as u32))
        .collect();
    table.clear();
    out
}
