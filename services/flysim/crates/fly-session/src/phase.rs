//! The session state machine of `step-v1` section 2, as an explicit edge table.
//!
//! ```text
//! Starting -> Ready(k) -> Preparing(k) -> Applying(k) -> Observing(k+1)
//!               ^                                          |
//!               +---------------- Ready(k+1) <- Committing(k)
//!
//! Ready(k) -> Paused(k) -> Ready(k)
//! Ready(k) / Paused(k) -> Capturing(k) -> same boundary
//! any unresolved partial failure -> Failed -> Restoring(new epoch) -> Paused(k)
//! terminal episode -> Paused(k) -> Resetting(new epoch) -> Ready(0)
//! ```
//!
//! A transition the table does not list is a bug, not a recoverable condition, so it returns
//! INVALID_PHASE rather than being silently applied.

use crate::types::{DomainError, ErrorCode};

/// Where the session is. The number is the committed boundary the phase belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Ready(u64),
    Preparing(u64),
    Applying(u64),
    /// `Observing(k+1)`: the world has reached `k+1` but nothing is committed yet.
    Observing(u64),
    /// `Committing(k)`: completing the transition `k -> k+1`.
    Committing(u64),
    Paused(u64),
    Capturing(u64),
    Failed,
    /// `Restoring(k)`: installing a coherent checkpoint of boundary `k` under a new epoch.
    Restoring(u64),
    /// `Resetting(k)`: leaving boundary `k` for a new epoch and episode at step 0.
    Resetting(u64),
}

impl Phase {
    pub fn label(&self) -> String {
        match self {
            Phase::Starting => "Starting".to_owned(),
            Phase::Ready(k) => format!("Ready({k})"),
            Phase::Preparing(k) => format!("Preparing({k})"),
            Phase::Applying(k) => format!("Applying({k})"),
            Phase::Observing(k) => format!("Observing({k})"),
            Phase::Committing(k) => format!("Committing({k})"),
            Phase::Paused(k) => format!("Paused({k})"),
            Phase::Capturing(k) => format!("Capturing({k})"),
            Phase::Failed => "Failed".to_owned(),
            Phase::Restoring(k) => format!("Restoring({k})"),
            Phase::Resetting(k) => format!("Resetting({k})"),
        }
    }

    /// The committed boundary, where the phase has one. `Observing(k+1)` does not: the world
    /// has moved but nothing is committed, so the committed boundary is still `k`.
    pub fn committed_boundary(&self) -> Option<u64> {
        match self {
            Phase::Ready(k) | Phase::Paused(k) | Phase::Capturing(k) => Some(*k),
            _ => None,
        }
    }

    /// Only a committed boundary is eligible for a checkpoint or a normal pause.
    pub fn is_committed_boundary(&self) -> bool {
        matches!(self, Phase::Ready(_) | Phase::Paused(_))
    }
}

/// The phase, plus the edge check that guards every change to it.
#[derive(Clone, Debug)]
pub struct PhaseMachine {
    phase: Phase,
    /// Where a `Capturing(k)` must return to.
    capture_origin: Option<Phase>,
}

impl Default for PhaseMachine {
    fn default() -> PhaseMachine {
        PhaseMachine::new()
    }
}

impl PhaseMachine {
    pub fn new() -> PhaseMachine {
        PhaseMachine { phase: Phase::Starting, capture_origin: None }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// True when `next` is an edge of the section 2 machine.
    pub fn allows(&self, next: Phase) -> bool {
        use Phase::*;
        // Any unresolved partial failure fails the epoch, from wherever the session was.
        if next == Failed {
            return self.phase != Failed;
        }
        match (self.phase, next) {
            (Starting, Ready(0)) => true,
            (Ready(k), Preparing(j)) => k == j,
            (Preparing(k), Applying(j)) => k == j,
            (Applying(k), Observing(j)) => j == k + 1,
            (Observing(j), Committing(k)) => j == k + 1,
            (Committing(k), Ready(j)) => j == k + 1,
            (Ready(k), Paused(j)) => k == j,
            (Paused(k), Ready(j)) => k == j,
            (Ready(k), Capturing(j)) | (Paused(k), Capturing(j)) => k == j,
            // Capturing returns to the boundary it came from, and only to that one.
            (Capturing(k), Ready(j)) => k == j && self.capture_origin == Some(Ready(k)),
            (Capturing(k), Paused(j)) => k == j && self.capture_origin == Some(Paused(k)),
            (Failed, Restoring(_)) => true,
            (Restoring(k), Paused(j)) => k == j,
            (Paused(k), Resetting(j)) => k == j,
            (Resetting(_), Ready(0)) => true,
            _ => false,
        }
    }

    /// Applies an edge, reporting the old and new labels for the trace.
    pub fn to(&mut self, next: Phase) -> Result<(String, String), DomainError> {
        if !self.allows(next) {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!("{} cannot move to {}", self.phase.label(), next.label()),
            ));
        }
        let from = self.phase.label();
        if matches!(next, Phase::Capturing(_)) {
            self.capture_origin = Some(self.phase);
        } else if !matches!(self.phase, Phase::Capturing(_)) {
            self.capture_origin = None;
        }
        self.phase = next;
        Ok((from, next.label()))
    }

    /// Fails the epoch from wherever the session was.
    pub fn fail(&mut self) -> (String, String) {
        let from = self.phase.label();
        self.phase = Phase::Failed;
        self.capture_origin = None;
        (from, self.phase.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_happy_path_walks_the_section_2_diagram() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        m.to(Phase::Preparing(0)).unwrap();
        m.to(Phase::Applying(0)).unwrap();
        m.to(Phase::Observing(1)).unwrap();
        m.to(Phase::Committing(0)).unwrap();
        m.to(Phase::Ready(1)).unwrap();
        assert_eq!(m.phase(), Phase::Ready(1));
    }

    #[test]
    fn a_step_cannot_be_skipped_or_rewound() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        assert!(m.to(Phase::Preparing(1)).is_err());
        m.to(Phase::Preparing(0)).unwrap();
        assert!(m.to(Phase::Observing(1)).is_err());
        m.to(Phase::Applying(0)).unwrap();
        assert!(m.to(Phase::Observing(0)).is_err());
    }

    #[test]
    fn only_a_committed_boundary_pauses_or_captures() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        m.to(Phase::Preparing(0)).unwrap();
        assert!(m.to(Phase::Paused(0)).is_err());
        assert!(m.to(Phase::Capturing(0)).is_err());
        assert!(!Phase::Preparing(0).is_committed_boundary());
        assert!(Phase::Ready(0).is_committed_boundary());
        assert!(Phase::Paused(3).is_committed_boundary());
    }

    #[test]
    fn a_capture_returns_to_the_boundary_it_came_from() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        m.to(Phase::Paused(0)).unwrap();
        m.to(Phase::Capturing(0)).unwrap();
        assert!(m.to(Phase::Ready(0)).is_err());
        m.to(Phase::Paused(0)).unwrap();
    }

    #[test]
    fn failure_leads_to_restore_and_then_to_a_paused_boundary() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        m.to(Phase::Preparing(0)).unwrap();
        m.fail();
        assert_eq!(m.phase(), Phase::Failed);
        assert!(m.to(Phase::Ready(0)).is_err());
        m.to(Phase::Restoring(0)).unwrap();
        m.to(Phase::Paused(0)).unwrap();
    }

    #[test]
    fn an_episode_reset_leaves_a_pause_and_lands_on_step_zero() {
        let mut m = PhaseMachine::new();
        m.to(Phase::Ready(0)).unwrap();
        m.to(Phase::Preparing(0)).unwrap();
        m.to(Phase::Applying(0)).unwrap();
        m.to(Phase::Observing(1)).unwrap();
        m.to(Phase::Committing(0)).unwrap();
        m.to(Phase::Ready(1)).unwrap();
        m.to(Phase::Paused(1)).unwrap();
        m.to(Phase::Resetting(1)).unwrap();
        m.to(Phase::Ready(0)).unwrap();
    }
}
