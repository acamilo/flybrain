//! The sugar admission rules, enforced server side.
//!
//! `docs/control-api.md`: "sugar accepted at most 6 per minute globally, no overlap with an
//! active pulse, per-viewer limits are the bridge's job. Limits are enforced here regardless of
//! what the bridge does."
//!
//! Both rules answer with the same currency as the HTTP response: the milliseconds after which a
//! retry could succeed, so the caller never has to guess (`429 { retryAfterMs }`).

use std::collections::VecDeque;

/// One minute, the window `control.sugar_per_minute` is counted over.
pub const WINDOW_MS: u64 = 60_000;

/// Why a stimulation request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A pulse is still being applied; `retry_after_ms` is what is left of it.
    PulseActive { retry_after_ms: u64 },
    /// The global per-minute budget is spent; `retry_after_ms` is until the oldest hit ages out.
    RateLimited { retry_after_ms: u64 },
}

impl Refusal {
    pub fn retry_after_ms(self) -> u64 {
        match self {
            Self::PulseActive { retry_after_ms } | Self::RateLimited { retry_after_ms } => {
                retry_after_ms
            }
        }
    }
}

/// A fixed-budget sliding window over wall-clock milliseconds.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    limit: usize,
    window_ms: u64,
    hits: VecDeque<u64>,
}

impl RateLimiter {
    pub fn new(limit: usize) -> Self {
        Self::with_window(limit, WINDOW_MS)
    }

    pub fn with_window(limit: usize, window_ms: u64) -> Self {
        Self { limit: limit.max(1), window_ms, hits: VecDeque::new() }
    }

    /// Admit one request at `now_ms`, or say when to come back.
    ///
    /// `active_pulse_ms` is the remaining length of the pulse currently being applied, which is
    /// the "no overlap with an active pulse" rule: it is checked first, so a caller arriving
    /// during a pulse is told to wait rather than quietly spending one of the six slots.
    pub fn admit(&mut self, now_ms: u64, active_pulse_ms: f64) -> Result<(), Refusal> {
        self.expire(now_ms);
        if active_pulse_ms > 0.0 {
            return Err(Refusal::PulseActive {
                retry_after_ms: active_pulse_ms.ceil().max(0.0) as u64,
            });
        }
        if self.hits.len() >= self.limit {
            let oldest = *self.hits.front().expect("non-empty when at the limit");
            return Err(Refusal::RateLimited {
                retry_after_ms: self.window_ms.saturating_sub(now_ms.saturating_sub(oldest)),
            });
        }
        self.hits.push_back(now_ms);
        Ok(())
    }

    /// Milliseconds until another request would be admitted on the per-minute rule alone; 0 when
    /// one would be admitted now. This is the feed header's `sugar.cooldownMs`.
    pub fn cooldown_ms(&mut self, now_ms: u64) -> u64 {
        self.expire(now_ms);
        if self.hits.len() < self.limit {
            return 0;
        }
        let oldest = *self.hits.front().expect("non-empty when at the limit");
        self.window_ms.saturating_sub(now_ms.saturating_sub(oldest))
    }

    /// Requests admitted inside the current window.
    pub fn used(&self) -> usize {
        self.hits.len()
    }

    fn expire(&mut self, now_ms: u64) {
        while let Some(oldest) = self.hits.front().copied() {
            if now_ms.saturating_sub(oldest) >= self.window_ms {
                self.hits.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_spent_and_then_refilled_one_slot_at_a_time() {
        let mut limiter = RateLimiter::new(6);
        for index in 0..6 {
            limiter.admit(1_000 + index * 100, 0.0).unwrap();
        }
        assert_eq!(limiter.used(), 6);

        // The seventh in the same minute waits for the first one to age out.
        let refusal = limiter.admit(2_000, 0.0).unwrap_err();
        assert_eq!(refusal, Refusal::RateLimited { retry_after_ms: 60_000 - 1_000 });
        assert_eq!(limiter.cooldown_ms(2_000), 59_000);

        // One slot back at 61,000: the hit from 1,000 has aged out, the one from 1,100 has not.
        limiter.admit(61_000, 0.0).unwrap();
        assert_eq!(limiter.used(), 6);
        assert_eq!(limiter.admit(61_001, 0.0).unwrap_err().retry_after_ms(), 60_000 - 59_901);

        // Past the whole window, the budget is clean again.
        for index in 0..6 {
            limiter.admit(200_000 + index, 0.0).unwrap();
        }
        assert_eq!(limiter.cooldown_ms(200_005), 60_000 - 5);
    }

    #[test]
    fn an_active_pulse_blocks_without_spending_a_slot() {
        let mut limiter = RateLimiter::new(6);
        let refusal = limiter.admit(1_000, 250.4).unwrap_err();
        assert_eq!(refusal, Refusal::PulseActive { retry_after_ms: 251 });
        assert_eq!(limiter.used(), 0, "a refused request must not cost budget");
        limiter.admit(1_000, 0.0).unwrap();
        assert_eq!(limiter.used(), 1);
    }

    #[test]
    fn a_flood_never_admits_more_than_the_budget() {
        let mut limiter = RateLimiter::new(6);
        let mut admitted = 0;
        // 100 requests per second for 10 seconds, all inside one window.
        for step in 0..1_000u64 {
            if limiter.admit(step * 10, 0.0).is_ok() {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 6);
    }

    #[test]
    fn cooldown_is_zero_while_budget_remains() {
        let mut limiter = RateLimiter::new(2);
        assert_eq!(limiter.cooldown_ms(0), 0);
        limiter.admit(0, 0.0).unwrap();
        assert_eq!(limiter.cooldown_ms(0), 0);
        limiter.admit(10, 0.0).unwrap();
        assert_eq!(limiter.cooldown_ms(10), 59_990);
    }

    #[test]
    fn a_zero_limit_is_clamped_to_one_rather_than_deadlocking() {
        let mut limiter = RateLimiter::new(0);
        limiter.admit(0, 0.0).unwrap();
        assert!(limiter.admit(1, 0.0).is_err());
    }
}
