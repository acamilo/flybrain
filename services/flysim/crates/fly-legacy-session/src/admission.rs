//! Sugar, admitted by the legacy rules onto the coordinator's admission queue, and the operator
//! reward pulse, refused (`legacy-gameboy-v1` section 15 and its amendment of 2026-09-29).
//!
//! The rules are flysim's own, unchanged: `flysim::ratelimit::RateLimiter` (the per-minute limit
//! and "no overlap with an active pulse") and the duration clamp to `[1, sugar_max_ms]`. What
//! changed is the pulse they read: the agent's `stimulusRemainingMs` from the **last completed
//! commit** ([`AdmissionQueue::stimulus_remaining_ms`]), which the operator accepted as one commit
//! stale. Until the first commit of an epoch it is unknown, and a request is refused with a retry.
//!
//! The operator's `POST /reward` pulse is refused. The legacy loop refuses it too unless an
//! operator turns `control.allow_reward` on (it is off in every shipped configuration); the new
//! runtime has no counterpart to turn on, and says so rather than approximating one -- a
//! reinforcement outside `Agent.Commit` is not a session-framework operation.

use flysim::ratelimit::{RateLimiter, Refusal};

use fly_session::coordinator::{Admission, AdmissionQueue};
use fly_session::fly_session_types::gameboy::STIMULUS_REWARD_PULSE;
use fly_session::types::*;

/// Why `POST /reward` is refused (`legacy-gameboy-v1` section 15, amendment of 2026-09-29).
pub const OPERATOR_REWARD_REFUSED: &str = "the operator reward pulse is not available on the session runtime (legacy-gameboy-v1 section 15)";

/// Retry advice while the pulse is unknown: one frame, rounded up.
pub const UNKNOWN_PULSE_RETRY_MS: u64 = 17;

/// The edge-side admission of the legacy composition.
pub struct LegacyAdmission {
    queue: AdmissionQueue,
    agent_id: Id,
    limiter: RateLimiter,
    default_ms: f64,
    max_ms: f64,
    serial: u64,
}

impl LegacyAdmission {
    /// flysim's defaults: 6 per minute, 400 ms by default, at most 1000 ms.
    pub fn new(queue: AdmissionQueue, agent_id: Id) -> LegacyAdmission {
        let control = flysim::config::Config::default().control;
        LegacyAdmission::with_limits(
            queue,
            agent_id,
            control.sugar_per_minute,
            control.sugar_default_ms,
            control.sugar_max_ms,
        )
    }

    pub fn with_limits(
        queue: AdmissionQueue,
        agent_id: Id,
        per_minute: usize,
        default_ms: f64,
        max_ms: f64,
    ) -> LegacyAdmission {
        LegacyAdmission {
            queue,
            agent_id,
            limiter: RateLimiter::new(per_minute),
            default_ms,
            max_ms,
            serial: 0,
        }
    }

    fn next_id(&mut self, kind: &str) -> Id {
        self.serial += 1;
        id(&format!("{kind}-{}", self.serial))
    }

    /// `POST /stimulate`: the legacy rule over the last commit's pulse. On admission the sugar is
    /// queued for the next Prepare; the result is its interaction id and the clamped duration.
    pub fn sugar(&mut self, now_ms: u64, duration_ms: Option<f64>) -> Result<(Id, f64), Refusal> {
        let Some(remaining) = self.queue.stimulus_remaining_ms(&self.agent_id) else {
            return Err(Refusal::PulseActive {
                retry_after_ms: UNKNOWN_PULSE_RETRY_MS,
            });
        };
        self.limiter.admit(now_ms, remaining)?;
        let duration = duration_ms
            .unwrap_or(self.default_ms)
            .clamp(1.0, self.max_ms)
            .min(self.max_ms);
        let interaction_id = self.next_id("sugar");
        self.queue.admit(Admission::Stimulus {
            agent_id: self.agent_id.clone(),
            interaction_id: interaction_id.clone(),
            kind_id: id(STIMULUS_REWARD_PULSE),
            duration_ms: duration,
        });
        Ok((interaction_id, duration))
    }

    /// `POST /reward`: refused, always (the legacy default, `control.allow_reward = false`).
    pub fn reward(&mut self, _value: f64) -> Result<Id, &'static str> {
        Err(OPERATOR_REWARD_REFUSED)
    }

    /// Admits exactly this sugar, bypassing the rules: a parity run replays the legacy loop's
    /// admitted inputs, it does not re-decide them.
    pub fn replay_sugar(&mut self, duration_ms: f64) -> Id {
        let interaction_id = self.next_id("sugar");
        self.queue.admit(Admission::Stimulus {
            agent_id: self.agent_id.clone(),
            interaction_id: interaction_id.clone(),
            kind_id: id(STIMULUS_REWARD_PULSE),
            duration_ms,
        });
        interaction_id
    }
}
