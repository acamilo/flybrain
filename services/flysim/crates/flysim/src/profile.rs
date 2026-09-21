//! Per-phase timing for the sim thread, off unless `FLY_PROFILE_SECONDS` is set.
//!
//! The question this exists to answer is "where does a game frame go on the sim thread", which
//! `flybrain-core`'s `examples/ablate.rs` answers for the brain alone and a sampling profiler
//! answers by symbol rather than by phase. The five brain phases come straight from
//! [`flybrain_core::lif::PhaseTimings`]; the loop phases are lapped here.
//!
//! ```sh
//! FLY_PROFILE_SECONDS=30 flysim --config flysim.toml
//! ```
//!
//! Cost when off is one `bool` test per lap, and when on one `Instant::now()` per lap — about a
//! dozen per 16.74 ms frame, so under a microsecond against a frame that costs milliseconds.
//! `infra/docs/simloop-profile.md` is the measurement this was written for.

use std::time::{Duration, Instant};

use flybrain_core::lif::PhaseTimings;

/// The environment variable that turns the table on, in seconds between reports.
pub const INTERVAL_ENV: &str = "FLY_PROFILE_SECONDS";

/// One phase of the sim thread's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Draining the command queue and the deny-list check.
    Commands,
    /// `Network::step`, as one wall-clock total. The brain's own five phases come from
    /// `PhaseTimings` and add up to very nearly this.
    Step,
    /// Decode plus `set_buttons`.
    Decode,
    /// `Emulator::run_frame`.
    Emulate,
    /// The framebuffer copy, the retina projection and the audio DC blocker.
    Retina,
    /// `GameAdapter::sample`, the PAM stimulation, `reinforce` and the reward events.
    Rewards,
    /// `GameAdapter::progress`, the rank tracking and `Ratchet::observe`.
    Ratchet,
    /// Building a snapshot and handing it to the watch channel.
    Publish,
    /// Serializing a checkpoint and queueing it for the writer thread.
    Checkpoint,
    /// The event log's fsync.
    Log,
}

impl Phase {
    const ALL: [Phase; 10] = [
        Phase::Commands,
        Phase::Step,
        Phase::Decode,
        Phase::Emulate,
        Phase::Retina,
        Phase::Rewards,
        Phase::Ratchet,
        Phase::Publish,
        Phase::Checkpoint,
        Phase::Log,
    ];

    fn label(self) -> &'static str {
        match self {
            Phase::Commands => "commands",
            Phase::Step => "brain step",
            Phase::Decode => "decoder",
            Phase::Emulate => "emulator",
            Phase::Retina => "retina+audio",
            Phase::Rewards => "rewards",
            Phase::Ratchet => "ratchet",
            Phase::Publish => "publish",
            Phase::Checkpoint => "checkpoint",
            Phase::Log => "event log",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|phase| *phase == self)
            .expect("every phase is in ALL")
    }
}

/// A lap timer over the sim thread's phases, plus the brain's own five.
#[derive(Debug)]
pub struct Profiler {
    interval: Option<Duration>,
    next_report: Instant,
    mark: Instant,
    frames: u64,
    totals: [u64; Phase::ALL.len()],
    worst: [u64; Phase::ALL.len()],
    /// Whole-iteration wall time, sleep included, so the table can be read against 16.74 ms.
    iteration_ns: u64,
    worst_iteration_ns: u64,
    brain: PhaseTimings,
}

impl Profiler {
    /// Read `FLY_PROFILE_SECONDS`. Absent, unparseable or zero means off.
    pub fn from_env(now: Instant) -> Self {
        let interval = std::env::var(INTERVAL_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|seconds| *seconds > 0.0)
            .map(Duration::from_secs_f64);
        Self {
            interval,
            next_report: now + interval.unwrap_or(Duration::ZERO),
            mark: now,
            frames: 0,
            totals: [0; Phase::ALL.len()],
            worst: [0; Phase::ALL.len()],
            iteration_ns: 0,
            worst_iteration_ns: 0,
            brain: PhaseTimings::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.interval.is_some()
    }

    /// Start a phase. Every `lap` measures from the previous `start` or `lap`.
    #[inline]
    pub fn start(&mut self) {
        if self.interval.is_some() {
            self.mark = Instant::now();
        }
    }

    /// Charge the time since the last mark to `phase`.
    #[inline]
    pub fn lap(&mut self, phase: Phase) {
        if self.interval.is_none() {
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.mark).as_nanos() as u64;
        let slot = phase.index();
        self.totals[slot] += elapsed;
        self.worst[slot] = self.worst[slot].max(elapsed);
        self.mark = now;
    }

    /// One whole loop iteration, sleep included.
    #[inline]
    pub fn iteration(&mut self, wall: Duration) {
        if self.interval.is_none() {
            return;
        }
        self.frames += 1;
        let ns = wall.as_nanos() as u64;
        self.iteration_ns += ns;
        self.worst_iteration_ns = self.worst_iteration_ns.max(ns);
    }

    /// Fold in the brain's own phase totals, which the kernel accumulates separately.
    pub fn absorb_brain(&mut self, timings: PhaseTimings) {
        if self.interval.is_none() {
            return;
        }
        self.brain.ticks += timings.ticks;
        self.brain.drive_ns += timings.drive_ns;
        self.brain.sweep_ns += timings.sweep_ns;
        self.brain.observe_ns += timings.observe_ns;
        self.brain.propagate_ns += timings.propagate_ns;
        self.brain.rates_ns += timings.rates_ns;
    }

    /// True once the reporting interval has elapsed; the caller then calls [`Profiler::report`].
    #[inline]
    pub fn due(&self, now: Instant) -> bool {
        self.interval.is_some() && now >= self.next_report && self.frames > 0
    }

    /// The table, as one multi-line string, and reset the window.
    pub fn report(&mut self, now: Instant, realtime_factor: f64) -> String {
        let frames = self.frames.max(1) as f64;
        let mut out = String::new();
        out.push_str(&format!(
            "sim thread, {} frames over {:.1} s, realtime factor {realtime_factor:.3}\n",
            self.frames,
            self.iteration_ns as f64 / 1e9,
        ));
        out.push_str(&format!(
            "{:<14} {:>10} {:>10}\n",
            "phase", "ms/frame", "worst ms"
        ));
        let mut accounted = 0u64;
        for phase in Phase::ALL {
            let slot = phase.index();
            accounted += self.totals[slot];
            out.push_str(&format!(
                "{:<14} {:>10.3} {:>10.3}\n",
                phase.label(),
                self.totals[slot] as f64 / 1e6 / frames,
                self.worst[slot] as f64 / 1e6,
            ));
        }
        out.push_str(&format!(
            "{:<14} {:>10.3} {:>10}\n",
            "= work",
            accounted as f64 / 1e6 / frames,
            "",
        ));
        out.push_str(&format!(
            "{:<14} {:>10.3} {:>10.3}\n",
            "iteration",
            self.iteration_ns as f64 / 1e6 / frames,
            self.worst_iteration_ns as f64 / 1e6,
        ));
        if self.brain.ticks > 0 {
            let per_frame = |value: u64| value as f64 / 1e6 / frames;
            out.push_str(&format!(
                "brain phases, {:.2} ticks/frame: drive {:.3} sweep {:.3} observe {:.3} \
                 propagate {:.3} rates {:.3} ms/frame\n",
                self.brain.ticks as f64 / frames,
                per_frame(self.brain.drive_ns),
                per_frame(self.brain.sweep_ns),
                per_frame(self.brain.observe_ns),
                per_frame(self.brain.propagate_ns),
                per_frame(self.brain.rates_ns),
            ));
        }
        self.frames = 0;
        self.totals = [0; Phase::ALL.len()];
        self.worst = [0; Phase::ALL.len()];
        self.iteration_ns = 0;
        self.worst_iteration_ns = 0;
        self.brain = PhaseTimings::default();
        self.next_report = now + self.interval.unwrap_or(Duration::ZERO);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profiler_with_no_environment_variable_records_nothing() {
        let mut profiler = Profiler {
            interval: None,
            ..Profiler::from_env(Instant::now())
        };
        profiler.start();
        profiler.lap(Phase::Step);
        profiler.iteration(Duration::from_millis(16));
        assert!(!profiler.enabled());
        assert!(!profiler.due(Instant::now()));
        assert_eq!(profiler.frames, 0);
        assert_eq!(profiler.totals[Phase::Step.index()], 0);
    }

    #[test]
    fn every_phase_has_its_own_slot_and_a_label() {
        let mut labels: Vec<&str> = Phase::ALL.iter().map(|phase| phase.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Phase::ALL.len());
        for (index, phase) in Phase::ALL.iter().enumerate() {
            assert_eq!(phase.index(), index);
        }
    }

    #[test]
    fn an_enabled_profiler_charges_a_phase_and_then_resets() {
        let start = Instant::now();
        let mut profiler = Profiler {
            interval: Some(Duration::from_secs(0)),
            ..Profiler::from_env(start)
        };
        profiler.start();
        std::thread::sleep(Duration::from_millis(2));
        profiler.lap(Phase::Emulate);
        profiler.iteration(Duration::from_millis(3));
        assert!(profiler.due(Instant::now()));
        let table = profiler.report(Instant::now(), 1.0);
        assert!(table.contains("emulator"), "{table}");
        assert_eq!(profiler.frames, 0);
        assert_eq!(profiler.totals[Phase::Emulate.index()], 0);
    }
}
