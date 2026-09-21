//! Frame pacing and the realtime-factor window.
//!
//! `docs/design/flysim.md` section 4: absolute deadlines (`next += period / speed`), never
//! `sleep(period)`, so sleep jitter does not accumulate; faster than real time sleeps the
//! difference; slower **never skips a frame**, because brain time and game time are one clock.
//! The shortfall is accumulated as lag and reported instead.

use std::time::{Duration, Instant};

/// Wall-clock pacing against an absolute deadline.
#[derive(Debug)]
pub struct Pacer {
    /// `None` when unthrottled (`loop.speed = 0`).
    period: Option<Duration>,
    next: Instant,
    lag: Duration,
    /// Lag at the last warning, so the warning repeats on growth rather than on every frame.
    warned_at: Option<Duration>,
}

/// Lag that earns the first warning.
pub const LAG_WARN_SECONDS: f64 = 1.0;
/// Extra lag that earns each subsequent warning.
pub const LAG_WARN_STEP_SECONDS: f64 = 30.0;

/// How far behind its schedule the loop will try to catch up before writing the backlog off.
///
/// Absolute deadlines are the point of this type: `thread::sleep` routinely overshoots by a
/// millisecond or two (more under WSL), and without compensation a 16.74 ms frame period becomes
/// an 18 ms one and the whole stream runs slow. Keeping the schedule means the next frame's
/// sleep is shortened by exactly the overshoot. Past this much, though, the loop is genuinely
/// too slow rather than merely jittery, and burning through a minutes-long backlog at full speed
/// would only make the stream stutter — so the backlog is recorded as lag and abandoned.
pub const MAX_CATCHUP: Duration = Duration::from_secs(1);

impl Pacer {
    /// `frame_ms` is the simulated length of one frame; `speed` is the target realtime factor,
    /// with 0 meaning "run flat out".
    pub fn new(frame_ms: f64, speed: f64, now: Instant) -> Self {
        let period = if speed > 0.0 {
            Some(Duration::from_secs_f64(frame_ms / 1000.0 / speed))
        } else {
            None
        };
        Self { period, next: now, lag: Duration::ZERO, warned_at: None }
    }

    pub fn unthrottled(&self) -> bool {
        self.period.is_none()
    }

    /// How far behind the deadline schedule the loop has fallen, in seconds.
    pub fn lag_seconds(&self) -> f64 {
        self.lag.as_secs_f64()
    }

    /// Advance the deadline by one frame and return how long to sleep (`ZERO` when already late).
    ///
    /// A small shortfall keeps the schedule, so the following frames make it up and sleep jitter
    /// does not accumulate. A shortfall past [`MAX_CATCHUP`] is recorded as lag and the schedule
    /// re-anchors on `now`: at that point the loop cannot keep up, and no amount of catching up
    /// will change that.
    pub fn next_sleep(&mut self, now: Instant) -> Duration {
        let Some(period) = self.period else {
            return Duration::ZERO;
        };
        self.next += period;
        if self.next > now {
            return self.next - now;
        }
        let behind = now - self.next;
        if behind > MAX_CATCHUP {
            self.lag += behind;
            self.next = now;
        }
        Duration::ZERO
    }

    /// How far behind the current deadline the loop is right now, in seconds. Unlike
    /// [`Pacer::lag_seconds`] this recovers: it is the jitter the next sleep will absorb.
    pub fn shortfall_seconds(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.next).as_secs_f64()
    }

    /// True when the current lag deserves a log line, latching so it is not one per frame.
    pub fn should_warn(&mut self) -> bool {
        let lag = self.lag.as_secs_f64();
        if lag < LAG_WARN_SECONDS {
            return false;
        }
        let threshold = match self.warned_at {
            None => LAG_WARN_SECONDS,
            Some(previous) => previous.as_secs_f64() + LAG_WARN_STEP_SECONDS,
        };
        if lag >= threshold {
            self.warned_at = Some(self.lag);
            return true;
        }
        false
    }
}

/// Simulated milliseconds per wall millisecond over a trailing window.
#[derive(Debug)]
pub struct RealtimeWindow {
    window: Duration,
    samples: std::collections::VecDeque<(Instant, f64)>,
}

impl RealtimeWindow {
    pub fn new(window: Duration) -> Self {
        Self { window, samples: std::collections::VecDeque::new() }
    }

    /// Record the brain clock at a wall-clock instant.
    pub fn record(&mut self, now: Instant, brain_ms: f64) {
        self.samples.push_back((now, brain_ms));
        while self.samples.len() > 2 {
            let second = self.samples[1].0;
            if now.duration_since(second) >= self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// The factor over the window, or 0 before there are two samples far enough apart.
    pub fn factor(&self) -> f64 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let wall_ms = last.0.duration_since(first.0).as_secs_f64() * 1000.0;
        if wall_ms <= 0.0 {
            return 0.0;
        }
        let simulated = last.1 - first.1;
        if !simulated.is_finite() || simulated < 0.0 {
            return 0.0;
        }
        simulated / wall_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_are_absolute_so_a_late_frame_does_not_shorten_the_next_one() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 1.0, start);
        assert_eq!(pacer.next_sleep(start), Duration::from_secs(1));
        // 400 ms of the second second is already gone: sleep the remaining 600 ms, not 1 s.
        let sleep = pacer.next_sleep(start + Duration::from_millis(1400));
        assert_eq!(sleep, Duration::from_millis(600));
    }

    #[test]
    fn falling_behind_accumulates_lag_and_never_skips_a_frame() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 1.0, start);
        // Each frame takes 3 s of wall clock for 1 s of simulated time.
        let mut now = start;
        for _ in 0..3 {
            now += Duration::from_secs(3);
            assert_eq!(pacer.next_sleep(now), Duration::ZERO);
        }
        assert!((pacer.lag_seconds() - 6.0).abs() < 1e-6, "{}", pacer.lag_seconds());
    }

    #[test]
    fn sleep_overshoot_is_absorbed_by_the_next_frame_rather_than_becoming_lag() {
        // What actually happens on a real box: every `thread::sleep` returns about 2 ms late.
        // Without compensation a 16.74 ms frame period would become 18.7 ms, the stream would
        // run at 0.9x for ever, and the feed would sit near 26 Hz instead of 30.
        let start = Instant::now();
        let frame_ms = 1000.0 / (4_194_304.0 / 70_224.0);
        let mut pacer = Pacer::new(frame_ms, 1.0, start);
        let overshoot = Duration::from_millis(2);
        let mut now = start;
        let mut slept = Duration::ZERO;
        for _ in 0..600 {
            let sleep = pacer.next_sleep(now);
            slept += sleep;
            now += sleep + overshoot;
        }
        assert_eq!(pacer.lag_seconds(), 0.0, "jitter is not lag");
        let elapsed = now.duration_since(start).as_secs_f64();
        let simulated = 600.0 * frame_ms / 1000.0;
        assert!(
            (elapsed - simulated).abs() < 0.01,
            "600 frames took {elapsed:.3} s of wall clock for {simulated:.3} s of game time"
        );
    }

    #[test]
    fn the_lag_warning_latches_and_then_repeats_per_step() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 1.0, start);
        assert!(!pacer.should_warn());
        // 1 s of period, 3.5 s of wall clock: 2.5 s behind, past MAX_CATCHUP.
        pacer.next_sleep(start + Duration::from_millis(3_500));
        assert!(pacer.should_warn(), "2.5 s of lag is past the first threshold");
        assert!(!pacer.should_warn(), "no second warning at the same lag");
        pacer.next_sleep(start + Duration::from_millis(40_000));
        assert!(pacer.should_warn(), "another 30 s of lag warns again");
    }

    #[test]
    fn a_shortfall_under_the_catchup_limit_is_visible_without_being_counted_as_lag() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 1.0, start);
        let late = start + Duration::from_millis(1_400);
        assert_eq!(pacer.next_sleep(late), Duration::ZERO);
        assert!((pacer.shortfall_seconds(late) - 0.4).abs() < 1e-6);
        assert_eq!(pacer.lag_seconds(), 0.0);
        // And the next frame's sleep is short by exactly that much.
        assert_eq!(pacer.next_sleep(late), Duration::from_millis(600));
    }

    #[test]
    fn unthrottled_never_sleeps() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 0.0, start);
        assert!(pacer.unthrottled());
        assert_eq!(pacer.next_sleep(start + Duration::from_secs(600)), Duration::ZERO);
        assert_eq!(pacer.lag_seconds(), 0.0, "unthrottled cannot be late");
    }

    #[test]
    fn speed_scales_the_period() {
        let start = Instant::now();
        let mut pacer = Pacer::new(1000.0, 4.0, start);
        assert_eq!(pacer.next_sleep(start), Duration::from_millis(250));
    }

    #[test]
    fn the_realtime_factor_is_simulated_over_wall_across_the_window() {
        let start = Instant::now();
        let mut window = RealtimeWindow::new(Duration::from_secs(1));
        assert_eq!(window.factor(), 0.0);
        window.record(start, 0.0);
        window.record(start + Duration::from_millis(500), 250.0);
        assert!((window.factor() - 0.5).abs() < 1e-9);

        // A sample is dropped only while the next one still spans the whole window, so the
        // factor stays local without ever being computed over less than a second.
        window.record(start + Duration::from_millis(2_000), 2_250.0);
        assert!((window.factor() - 4.0 / 3.0).abs() < 1e-9, "{}", window.factor());
        window.record(start + Duration::from_millis(2_500), 2_750.0);
        assert!((window.factor() - 1.25).abs() < 1e-9, "{}", window.factor());
        window.record(start + Duration::from_millis(3_100), 3_350.0);
        assert!((window.factor() - 1.0).abs() < 1e-9, "{}", window.factor());
    }
}
