//! Prometheus text metrics, served at `GET /metrics`.
//!
//! `infra/bin/fly-watchdog` scrapes two of these by name and restarts units on what it finds, so
//! `fly_frames_sent_total` and `fly_feed_clients` are part of the operational contract:
//!
//! - a flat `fly_frames_sent_total` across two passes, or `fly_feed_clients` at 0, means the page
//!   is dead or frozen even though Chromium is alive (watchdog check 2);
//! - `/healthz` plus the hot-checkpoint mtime cover flysim itself (watchdog check 1).

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use crate::snapshot::{FeedStatus, Snapshot};

/// Process-wide counters. Everything is relaxed: these are observations, not synchronisation.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Snapshots actually written to a client socket.
    pub frames_sent: AtomicU64,
    /// Feed clients that finished their `hello` and are being served.
    pub feed_clients: AtomicI64,
    /// Snapshots a client never saw because a newer one replaced it (drop-oldest).
    pub feed_dropped: AtomicU64,
    /// Snapshots the sim published into the watch channel.
    pub snapshots_published: AtomicU64,
    /// Emulator frames the sim has run in this process.
    pub sim_frames: AtomicU64,
    pub events_total: AtomicU64,
    pub sugar_accepted_total: AtomicU64,
    pub sugar_refused_total: AtomicU64,
    /// Chat lines appended to the ring.
    pub chat_accepted_total: AtomicU64,
    /// Chat lines refused, one counter per `crate::chat::RejectReason` in `RejectReason::ALL`
    /// order — rendered as `fly_chat_rejected_total{reason="..."}`.
    pub chat_rejected_total: [AtomicU64; crate::chat::RejectReason::ALL.len()],
    pub checkpoints_written_total: AtomicU64,
    pub checkpoint_failures_total: AtomicU64,
    pub recoveries_total: AtomicU64,
    /// Latest committed durable generation.
    pub checkpoint_generation: AtomicU64,
    /// `Date.now()` of that commit.
    pub checkpoint_wall_ms: AtomicU64,
    /// Pacing shortfall in milliseconds (never negative; the loop does not skip frames).
    pub lag_ms: AtomicU64,
    /// 1 when the restore fell back past the newest candidate.
    pub restore_fallback: AtomicU64,
    /// Snapshots published on the feed bus (`FLY_FEED_VIA=bus`); 0 in direct mode.
    pub bus_published: AtomicU64,
    /// Snapshots the feed bus refused or could not take; each one is skipped, never retried.
    pub bus_publish_failures: AtomicU64,
    /// The loop's compute time per frame, its pacing sleep excluded (BUS-01).
    pub frame_work: FrameWork,
}

/// Frames in [`FrameWork`]'s window: a minute at real time.
pub const FRAME_WORK_WINDOW: usize = 3_600;

/// The loop's compute time per frame over the last [`FRAME_WORK_WINDOW`] frames: everything one
/// iteration of a running loop does -- commands, the transition, the publish, the checkpoint
/// hand-off, the event log -- and not the pacing sleep after it, so the headroom against the
/// 16.74 ms real-time budget is visible while the loop is paced (the realtime factor then reads
/// 1.0 whatever the margin). A paused loop records nothing. Both runtimes record it the same way.
#[derive(Debug, Default)]
pub struct FrameWork {
    ring: Mutex<FrameWorkRing>,
}

#[derive(Debug, Default)]
struct FrameWorkRing {
    /// Milliseconds, oldest overwritten first once the window is full.
    samples: Vec<f32>,
    next: usize,
    total: u64,
}

/// [`FrameWork`] summarised for `/metrics`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameWorkSummary {
    /// Frames in the window (at most [`FRAME_WORK_WINDOW`]).
    pub frames: usize,
    /// Frames ever recorded by this process.
    pub total: u64,
    pub mean_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl FrameWork {
    /// One frame's work. A lock the loop takes once a frame and a scrape takes once in a while.
    pub fn record(&self, work: Duration) {
        let ms = work.as_secs_f64() * 1000.0;
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if ring.samples.len() < FRAME_WORK_WINDOW {
            if ring.samples.capacity() == 0 {
                ring.samples.reserve_exact(FRAME_WORK_WINDOW);
            }
            ring.samples.push(ms as f32);
        } else {
            let next = ring.next;
            ring.samples[next] = ms as f32;
        }
        ring.next = (ring.next + 1) % FRAME_WORK_WINDOW;
        ring.total += 1;
    }

    /// Mean, 99th percentile (nearest rank) and maximum over the window; zeros when empty.
    pub fn summary(&self) -> FrameWorkSummary {
        let (mut samples, total) = {
            let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            (ring.samples.clone(), ring.total)
        };
        let frames = samples.len();
        if frames == 0 {
            return FrameWorkSummary::default();
        }
        let mean_ms = samples.iter().map(|&v| f64::from(v)).sum::<f64>() / frames as f64;
        let max_ms = samples.iter().fold(0.0f32, |a, &b| a.max(b));
        let rank = (frames * 99).div_ceil(100).max(1) - 1;
        let (_, p99, _) = samples.select_nth_unstable_by(rank, f32::total_cmp);
        FrameWorkSummary {
            frames,
            total,
            mean_ms,
            p99_ms: f64::from(*p99),
            max_ms: f64::from(max_ms),
        }
    }
}

impl Metrics {
    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    pub fn set(gauge: &AtomicU64, value: u64) {
        gauge.store(value, Ordering::Relaxed);
    }

    pub fn get(value: &AtomicU64) -> u64 {
        value.load(Ordering::Relaxed)
    }

    pub fn clients(&self) -> i64 {
        self.feed_clients.load(Ordering::Relaxed)
    }

    pub fn client_joined(&self) {
        self.feed_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn client_left(&self) {
        self.feed_clients.fetch_sub(1, Ordering::Relaxed);
    }

    /// Count one refused chat line under its reason label.
    pub fn chat_rejected(&self, reason: crate::chat::RejectReason) {
        Self::incr(&self.chat_rejected_total[reason.index()]);
    }
}

/// One metric line plus its help and type headers.
pub fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
    let _ = writeln!(out, "{name} {value}");
}

/// Render the Prometheus text exposition for the current counters and the newest snapshot.
pub fn render(metrics: &Metrics, snapshot: &Snapshot, now_wall_ms: u64) -> String {
    let header = &snapshot.header;
    let mut out = String::with_capacity(4_096);

    metric(
        &mut out,
        "fly_frames_sent_total",
        "counter",
        "Feed snapshots written to a client socket.",
        Metrics::get(&metrics.frames_sent),
    );
    metric(
        &mut out,
        "fly_feed_clients",
        "gauge",
        "Feed clients currently subscribed.",
        metrics.clients(),
    );
    metric(
        &mut out,
        "fly_feed_dropped_total",
        "counter",
        "Snapshots superseded before a slow client could be sent them.",
        Metrics::get(&metrics.feed_dropped),
    );
    metric(
        &mut out,
        "fly_bus_published_total",
        "counter",
        "Snapshots published on the feed bus (FLY_FEED_VIA=bus).",
        Metrics::get(&metrics.bus_published),
    );
    metric(
        &mut out,
        "fly_bus_publish_failures_total",
        "counter",
        "Snapshots the feed bus did not take; skipped, like any superseded snapshot.",
        Metrics::get(&metrics.bus_publish_failures),
    );
    metric(
        &mut out,
        "fly_snapshots_published_total",
        "counter",
        "Snapshots the simulation published.",
        Metrics::get(&metrics.snapshots_published),
    );
    metric(
        &mut out,
        "fly_sim_frames_total",
        "counter",
        "Emulator frames run in this process.",
        Metrics::get(&metrics.sim_frames),
    );
    metric(
        &mut out,
        "fly_frame",
        "counter",
        "Emulator frame counter, continuous across restores.",
        header.frame,
    );
    metric(
        &mut out,
        "fly_brain_ms",
        "counter",
        "Simulated milliseconds, continuous across restores.",
        header.brain_ms,
    );
    metric(
        &mut out,
        "fly_realtime_factor",
        "gauge",
        "Simulated milliseconds per wall millisecond over the last second.",
        header.realtime_factor,
    );
    let work = metrics.frame_work.summary();
    metric(
        &mut out,
        "fly_frame_work_mean_ms",
        "gauge",
        "Mean compute time per frame over the last 3600 frames, pacing sleep excluded; 0 before the first.",
        work.mean_ms,
    );
    metric(
        &mut out,
        "fly_frame_work_p99_ms",
        "gauge",
        "99th percentile compute time per frame over the same window.",
        work.p99_ms,
    );
    metric(
        &mut out,
        "fly_frame_work_max_ms",
        "gauge",
        "Longest compute time of one frame over the same window.",
        work.max_ms,
    );
    metric(
        &mut out,
        "fly_frame_work_frames",
        "gauge",
        "Frames in the compute-time window (at most 3600).",
        work.frames,
    );
    metric(
        &mut out,
        "fly_uptime_seconds",
        "gauge",
        "Seconds since this process started.",
        header.uptime_seconds,
    );
    metric(
        &mut out,
        "fly_lag_seconds",
        "gauge",
        "Accumulated pacing shortfall. The loop never skips a frame, so this only grows.",
        Metrics::get(&metrics.lag_ms) as f64 / 1000.0,
    );
    metric(
        &mut out,
        "fly_population_rate_hz",
        "gauge",
        "Population firing rate.",
        header.population_rate,
    );
    metric(
        &mut out,
        "fly_spike_count",
        "gauge",
        "Neurons that spiked since the previous snapshot.",
        header.spike_count,
    );
    metric(
        &mut out,
        "fly_learning_updates_total",
        "counter",
        "Plasticity updates applied.",
        header.learning.updates,
    );
    metric(
        &mut out,
        "fly_learning_signal",
        "gauge",
        "Current reinforcement signal.",
        header.learning.signal,
    );
    metric(
        &mut out,
        "fly_milestone_rank",
        "gauge",
        "Position on the adapter's milestone ladder.",
        header.milestone.rank,
    );
    metric(
        &mut out,
        "fly_milestone_since_seconds",
        "gauge",
        "Simulated seconds at the current rank (the stuck-o-meter).",
        header.milestone.since_seconds,
    );
    metric(
        &mut out,
        "fly_milestone_attempts",
        "gauge",
        "Game rollbacks since reaching this rank.",
        header.milestone.attempts,
    );
    metric(
        &mut out,
        "fly_reward_total",
        "counter",
        "Lifetime sum of every reward payout.",
        header.game.reward_total,
    );
    metric(
        &mut out,
        "fly_unique_locations",
        "gauge",
        "Distinct player positions observed.",
        header.game.unique_locations,
    );
    metric(
        &mut out,
        "fly_badges",
        "gauge",
        "The game's headline counter (badges for Pokemon Red).",
        header.game.badges,
    );
    metric(
        &mut out,
        "fly_recoveries_total",
        "counter",
        "Ratchet rollbacks performed.",
        Metrics::get(&metrics.recoveries_total),
    );
    metric(
        &mut out,
        "fly_events_total",
        "counter",
        "Events appended to the event log.",
        Metrics::get(&metrics.events_total),
    );
    metric(
        &mut out,
        "fly_sugar_accepted_total",
        "counter",
        "Stimulation pulses accepted.",
        Metrics::get(&metrics.sugar_accepted_total),
    );
    metric(
        &mut out,
        "fly_sugar_refused_total",
        "counter",
        "Stimulation requests refused by the rate limit or an active pulse.",
        Metrics::get(&metrics.sugar_refused_total),
    );
    metric(
        &mut out,
        "fly_chat_accepted_total",
        "counter",
        "Chat lines accepted into the feed header ring.",
        Metrics::get(&metrics.chat_accepted_total),
    );
    metric(
        &mut out,
        "fly_chat_ring_lines",
        "gauge",
        "Chat lines currently in the ring; 0 while the kill switch is off.",
        header.chat.as_ref().map_or(0, Vec::len),
    );
    {
        // One series per reason, always present, so a dashboard does not have to wait for the
        // first refusal of each kind to learn the label exists.
        use std::fmt::Write as _;
        let _ = writeln!(
            out,
            "# HELP fly_chat_rejected_total Chat lines refused, by the rule that refused them."
        );
        let _ = writeln!(out, "# TYPE fly_chat_rejected_total counter");
        for reason in crate::chat::RejectReason::ALL {
            let _ = writeln!(
                out,
                "fly_chat_rejected_total{{reason=\"{}\"}} {}",
                reason.as_str(),
                Metrics::get(&metrics.chat_rejected_total[reason.index()])
            );
        }
    }
    metric(
        &mut out,
        "fly_checkpoints_written_total",
        "counter",
        "Checkpoints committed, hot and durable.",
        Metrics::get(&metrics.checkpoints_written_total),
    );
    metric(
        &mut out,
        "fly_checkpoint_failures_total",
        "counter",
        "Checkpoint commits that failed.",
        Metrics::get(&metrics.checkpoint_failures_total),
    );
    metric(
        &mut out,
        "fly_checkpoint_generation",
        "gauge",
        "Newest committed durable generation.",
        Metrics::get(&metrics.checkpoint_generation),
    );
    let checkpoint_wall_ms = Metrics::get(&metrics.checkpoint_wall_ms);
    metric(
        &mut out,
        "fly_checkpoint_age_seconds",
        "gauge",
        "Seconds since the newest durable commit; -1 before the first one.",
        if checkpoint_wall_ms == 0 {
            -1.0
        } else {
            now_wall_ms.saturating_sub(checkpoint_wall_ms) as f64 / 1000.0
        },
    );
    metric(
        &mut out,
        "fly_restore_fallback",
        "gauge",
        "1 when startup restored from something other than the newest candidate.",
        Metrics::get(&metrics.restore_fallback),
    );
    for status in [
        FeedStatus::Booting,
        FeedStatus::Running,
        FeedStatus::Paused,
        FeedStatus::Recovering,
        FeedStatus::Error,
    ] {
        let name = serde_json::to_string(&status).unwrap_or_default();
        let name = name.trim_matches('"');
        use std::fmt::Write as _;
        if status == FeedStatus::Booting {
            let _ = writeln!(out, "# HELP fly_status The loop status, one series per state.");
            let _ = writeln!(out, "# TYPE fly_status gauge");
        }
        let _ = writeln!(
            out,
            "fly_status{{status=\"{name}\"}} {}",
            u8::from(status == snapshot.header.status)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_names_the_watchdog_scrapes_are_rendered_in_its_own_format() {
        let metrics = Metrics::default();
        Metrics::add(&metrics.frames_sent, 42);
        metrics.client_joined();
        metrics.client_joined();
        metrics.client_left();

        let text = render(&metrics, &crate::simloop::booting_snapshot(0, 0, crate::snapshot::MacroMode::Raw), 0);
        // `awk '/^fly_frames_sent_total/ {print $2; exit}'` must find the value in field 2.
        let line = text
            .lines()
            .find(|line| line.starts_with("fly_frames_sent_total "))
            .expect("fly_frames_sent_total");
        assert_eq!(line.split_whitespace().nth(1), Some("42"));
        let line = text
            .lines()
            .find(|line| line.starts_with("fly_feed_clients "))
            .expect("fly_feed_clients");
        assert_eq!(line.split_whitespace().nth(1), Some("1"));
    }

    #[test]
    fn a_refused_chat_line_counts_under_its_own_reason_label() {
        use crate::chat::RejectReason;
        let metrics = Metrics::default();
        metrics.chat_rejected(RejectReason::Url);
        metrics.chat_rejected(RejectReason::Url);
        metrics.chat_rejected(RejectReason::DenyList);
        Metrics::add(&metrics.chat_accepted_total, 3);

        let text = render(&metrics, &crate::simloop::booting_snapshot(0, 0, crate::snapshot::MacroMode::Raw), 0);
        assert!(text.contains("fly_chat_rejected_total{reason=\"url\"} 2"), "{text}");
        assert!(text.contains("fly_chat_rejected_total{reason=\"deny_list\"} 1"));
        assert!(text.contains("fly_chat_rejected_total{reason=\"charset\"} 0"));
        assert!(text.contains("fly_chat_accepted_total 3"));
        // The boot snapshot has no ring yet, and the gauge says 0 rather than going missing.
        assert!(text.contains("fly_chat_ring_lines 0"));
    }

    #[test]
    fn frame_work_is_the_mean_p99_and_max_of_the_last_window() {
        let work = FrameWork::default();
        assert_eq!(work.summary(), FrameWorkSummary::default());
        // 100 frames of 1..=100 ms: nearest-rank p99 is 99, the mean 50.5.
        for ms in 1..=100u64 {
            work.record(Duration::from_millis(ms));
        }
        let s = work.summary();
        assert_eq!((s.frames, s.total), (100, 100));
        assert!((s.mean_ms - 50.5).abs() < 1e-9, "{s:?}");
        assert_eq!((s.p99_ms, s.max_ms), (99.0, 100.0));
        // The window keeps the newest FRAME_WORK_WINDOW frames: 3600 frames of 2 ms push every
        // older one out.
        for _ in 0..FRAME_WORK_WINDOW {
            work.record(Duration::from_millis(2));
        }
        let s = work.summary();
        assert_eq!((s.frames, s.total), (FRAME_WORK_WINDOW, 100 + FRAME_WORK_WINDOW as u64));
        assert_eq!((s.mean_ms, s.p99_ms, s.max_ms), (2.0, 2.0, 2.0));
        let metrics = Metrics::default();
        metrics.frame_work.record(Duration::from_micros(12_500));
        let text = render(&metrics, &crate::simloop::booting_snapshot(0, 0, crate::snapshot::MacroMode::Raw), 0);
        assert!(text.contains("fly_frame_work_mean_ms 12.5\n"), "{text}");
        assert!(text.contains("fly_frame_work_p99_ms 12.5\n"));
        assert!(text.contains("fly_frame_work_frames 1\n"));
    }

    #[test]
    fn every_series_has_a_help_and_type_header_and_a_finite_value() {
        let text = render(&Metrics::default(), &crate::simloop::booting_snapshot(0, 0, crate::snapshot::MacroMode::Raw), 0);
        let mut declared = 0;
        for line in text.lines() {
            if line.starts_with("# TYPE ") {
                declared += 1;
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            let (_, value) = line.rsplit_once(' ').expect("name and value");
            let value: f64 = value.parse().unwrap_or_else(|_| panic!("{line}"));
            assert!(value.is_finite(), "{line}");
        }
        assert!(declared > 20, "{declared} series declared");
        assert!(text.contains("fly_chat_rejected_total{reason=\"url\"} 0"));
        assert!(text.contains("fly_chat_rejected_total{reason=\"deny_list\"} 0"));
        assert!(text.contains("fly_status{status=\"booting\"} 1"));
        assert!(text.contains("fly_status{status=\"running\"} 0"));
        assert!(text.contains("fly_checkpoint_age_seconds -1"));
    }
}
