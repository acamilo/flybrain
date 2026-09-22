//! Time and pacing: `step-v1` section 5.
//!
//! The accumulator is the contract crate's exact rational arithmetic, never rounded
//! nanoseconds. A 60 Hz world with a 1 ms model tick advances 16, 17, 17 ticks over its first
//! three steps and comes back to a remainder of exactly zero; rounding to microseconds does
//! not.

use crate::types::{DomainError, DomainType, ErrorCode, MutationCertainty, RationalNs};

/// One agent's tick accumulator: its tick duration, its remainder and its executed count.
#[derive(Clone, Debug)]
pub struct TickAccumulator {
    tick_duration: RationalNs,
    remainder: RationalNs,
    executed_ticks: u64,
    warmup_offset: u64,
}

impl TickAccumulator {
    /// A fresh accumulator. The tick duration must be positive.
    pub fn new(tick_duration: RationalNs) -> Result<TickAccumulator, String> {
        tick_duration.validate().map_err(|e| e.0)?;
        tick_duration.require_positive("tick duration").map_err(|e| e.0)?;
        Ok(TickAccumulator {
            tick_duration,
            remainder: RationalNs::ZERO,
            executed_ticks: 0,
            warmup_offset: 0,
        })
    }

    /// The exact accumulator a capture recorded.
    ///
    /// The remainder is restored, never rounded or reset: a resumed agent that started its
    /// first interval from zero would drift away from the run it is supposed to continue.
    pub fn restored(
        tick_duration: RationalNs,
        remainder: RationalNs,
        executed_ticks: u64,
        warmup_offset: u64,
    ) -> Result<TickAccumulator, String> {
        let mut accumulator = TickAccumulator::new(tick_duration)?;
        remainder.validate().map_err(|e| e.0)?;
        if remainder >= tick_duration {
            return Err("a captured remainder is not below one model tick".to_owned());
        }
        if warmup_offset > executed_ticks {
            return Err("a captured warm-up offset exceeds the executed tick count".to_owned());
        }
        accumulator.remainder = remainder;
        accumulator.executed_ticks = executed_ticks;
        accumulator.warmup_offset = warmup_offset;
        Ok(accumulator)
    }

    pub fn tick_duration(&self) -> RationalNs {
        self.tick_duration
    }

    /// The persisted remainder: always >= 0 and < one model tick.
    pub fn remainder(&self) -> RationalNs {
        self.remainder
    }

    /// Every tick this accumulator has executed, warm-up included.
    pub fn executed_ticks(&self) -> u64 {
        self.executed_ticks
    }

    /// The warm-up ticks executed before the first gameplay transition.
    pub fn warmup_offset(&self) -> u64 {
        self.warmup_offset
    }

    /// Accounts for `ticks` of warm-up. Warm-up does not consume an environment interval, so
    /// it never touches the remainder.
    pub fn warm_up(&mut self, ticks: u64) -> Result<(), String> {
        self.warmup_offset = self
            .warmup_offset
            .checked_add(ticks)
            .ok_or_else(|| "warm-up tick count overflows".to_owned())?;
        self.executed_ticks = self
            .executed_ticks
            .checked_add(ticks)
            .ok_or_else(|| "executed tick count overflows".to_owned())?;
        Ok(())
    }

    /// Adds one environment interval and returns the whole ticks it covers.
    ///
    /// ```text
    /// accumulator += environment step duration
    /// ticks = floor(accumulator / model tick duration)
    /// accumulator -= ticks * model tick duration
    /// ```
    pub fn advance(&mut self, interval: &RationalNs) -> Result<u64, String> {
        interval.validate().map_err(|e| e.0)?;
        interval.require_positive("environment interval").map_err(|e| e.0)?;
        let accumulated = self.remainder.checked_add(interval).map_err(|e| e.0)?;
        let (ticks, remainder) =
            accumulated.divide_floor(&self.tick_duration).map_err(|e| e.0)?;
        debug_assert!(
            remainder < self.tick_duration,
            "the remainder must stay below one model tick"
        );
        self.remainder = remainder;
        self.executed_ticks = self
            .executed_ticks
            .checked_add(ticks)
            .ok_or_else(|| "executed tick count overflows".to_owned())?;
        Ok(ticks)
    }

    pub fn brain_ticks(&self) -> u64 {
        self.executed_ticks
    }
}

/// The coordinator's pacing authority: absolute deadlines after committed boundaries.
///
/// Only one pacing authority may be active, so this is the coordinator's and the backend does
/// not throttle as well. When behind, it omits the sleep and reports the lag; it never skips a
/// world step or drops a neural tick.
#[derive(Clone, Debug)]
pub struct Pacing {
    step_duration: RationalNs,
    next_deadline: Option<std::time::Instant>,
    lag: std::time::Duration,
    lagged_steps: u64,
}

impl Pacing {
    pub fn new(step_duration: RationalNs) -> Pacing {
        Pacing {
            step_duration,
            next_deadline: None,
            lag: std::time::Duration::ZERO,
            lagged_steps: 0,
        }
    }

    /// The wall-clock period of one step, rounded for sleeping only. Simulation time stays
    /// rational; this value is never fed back into the accumulator.
    fn period(&self) -> std::time::Duration {
        let ns = u128::from(self.step_duration.numerator)
            / u128::from(self.step_duration.denominator).max(1);
        std::time::Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    /// Waits until this step's deadline. Returns the lag if the deadline had already passed.
    pub async fn wait(&mut self) -> Option<std::time::Duration> {
        let now = std::time::Instant::now();
        let deadline = self.next_deadline.unwrap_or(now);
        let outcome = if deadline > now {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            None
        } else {
            let behind = now.duration_since(deadline);
            if !behind.is_zero() {
                self.lag += behind;
                self.lagged_steps += 1;
            }
            Some(behind)
        };
        self.next_deadline = Some(deadline.max(now) + self.period());
        outcome
    }

    pub fn total_lag(&self) -> std::time::Duration {
        self.lag
    }

    pub fn lagged_steps(&self) -> u64 {
        self.lagged_steps
    }
}

/// Converts a whole tick count to the legacy f64 millisecond clock, refusing a run that has
/// left the exactly representable range.
pub fn ticks_to_legacy_millis(ticks: u64, tick_duration: &RationalNs) -> Result<f64, DomainError> {
    // 2^53 is the last integer f64 represents exactly; beyond it a millisecond clock starts
    // skipping representable ticks, so the run is refused rather than silently rounded.
    const EXACT_F64_INTEGERS: u64 = 1 << 53;
    if ticks >= EXACT_F64_INTEGERS {
        return Err(DomainError::new(
            ErrorCode::InvalidArgument,
            "tick count exceeds the range the legacy millisecond clock represents exactly",
            MutationCertainty::None,
        ));
    }
    let per_tick_ms =
        tick_duration.numerator as f64 / (tick_duration.denominator as f64 * 1_000_000.0);
    Ok(ticks as f64 * per_tick_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{hz, millis};

    #[test]
    fn a_60_hz_world_with_a_1_ms_tick_runs_16_17_17() {
        let step = hz(60).unwrap();
        let mut acc = TickAccumulator::new(millis(1).unwrap()).unwrap();
        let ticks: Vec<u64> = (0..3).map(|_| acc.advance(&step).unwrap()).collect();
        assert_eq!(ticks, vec![16, 17, 17]);
        assert_eq!(ticks.iter().sum::<u64>(), 50);
        assert!(acc.remainder().is_zero());
    }

    #[test]
    fn the_remainder_stays_below_one_tick_and_never_goes_negative() {
        let step = hz(60).unwrap();
        let tick = millis(1).unwrap();
        let mut acc = TickAccumulator::new(tick).unwrap();
        for _ in 0..600 {
            acc.advance(&step).unwrap();
            assert!(acc.remainder() < tick);
        }
        // 600 steps of 1/60 s is exactly 10 s, which is 10,000 whole milliseconds.
        assert_eq!(acc.executed_ticks(), 10_000);
        assert!(acc.remainder().is_zero());
    }

    #[test]
    fn warm_up_ticks_do_not_touch_the_remainder() {
        let mut acc = TickAccumulator::new(millis(1).unwrap()).unwrap();
        acc.warm_up(25).unwrap();
        assert_eq!(acc.executed_ticks(), 25);
        assert_eq!(acc.warmup_offset(), 25);
        assert!(acc.remainder().is_zero());
        assert_eq!(acc.advance(&hz(60).unwrap()).unwrap(), 16);
    }

    #[test]
    fn a_zero_interval_is_refused_rather_than_silently_producing_no_ticks() {
        let mut acc = TickAccumulator::new(millis(1).unwrap()).unwrap();
        assert!(acc.advance(&RationalNs::ZERO).is_err());
    }

    #[test]
    fn the_legacy_millisecond_clock_refuses_a_run_beyond_its_exact_range() {
        let tick = millis(1).unwrap();
        assert_eq!(ticks_to_legacy_millis(50, &tick).unwrap(), 50.0);
        assert!(ticks_to_legacy_millis(1 << 53, &tick).is_err());
    }
}
