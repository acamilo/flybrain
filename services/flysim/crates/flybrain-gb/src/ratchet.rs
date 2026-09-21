//! The progress ratchet: a port of the prototype's `src/runtime/ratchet.ts`.
//!
//! It keeps the best safe game snapshot seen so far and decides when a stalled
//! run should be rolled back to it. Generic over the adapter's rank: nothing
//! here knows about Pokémon, so a platformer adapter's ladder works unchanged.
//!
//! Limits (`docs/ratchet-sprint.md`): 120 brain seconds of stall, 180-second
//! recovery cooldown, three attempts per best rank, 36 lifetime recoveries.
//! They are deliberately conservative and can leave an unproductive run
//! unrecovered once exhausted.
//!
//! The lifetime budget scales with the ladder: it was twelve for a 16-rung ladder
//! and is 36 for the 38-rung one (`docs/design/ladder.md`), so three attempts at
//! every rung is still within budget. The per-rung allowance is unchanged, and so
//! are both windows.
//!
//! Those numbers are only the *defaults*. A game can ask for others, and for one
//! extra trigger, through the [`RecoveryPolicy`] its adapter returns
//! ([`crate::adapter::GameAdapter::recovery_policy`]). The default policy is
//! exactly the Pokémon behaviour above, game-over trigger disabled, so an adapter
//! that does not override it is unaffected; `docs/design/platformer.md` §5 explains
//! why a platformer needs a game-over restore and a lifetime budget larger still.
//!
//! The rank bound is not one of those numbers: it is the length of the running
//! adapter's ladder, which [`Ratchet::import`] takes as an argument.

use serde::{Deserialize, Serialize};

use crate::emulator::FRAMEBUFFER_LEN;

/// Brain milliseconds of stall before a recovery may fire.
pub const STALL_MS: u64 = 120_000;
/// Brain milliseconds between recoveries.
pub const RECOVERY_COOLDOWN_MS: u64 = 180_000;
/// Brain milliseconds of sustained unsafe activity that restarts the stall
/// window. Short walking or warp transitions are below this.
pub const UNSAFE_RESET_MS: u64 = 1_000;
/// Recovery attempts allowed at one best rank.
pub const MAX_ATTEMPTS: u64 = 3;
/// Lifetime recovery budget.
pub const MAX_RECOVERIES: u64 = 36;

/// Per-game recovery limits and triggers.
///
/// [`Default`] is the Pokémon policy, unchanged: the constants above, and no
/// game-over trigger. An adapter overrides it by returning its own from
/// [`crate::adapter::GameAdapter::recovery_policy`], which is where the
/// platformer's larger budgets live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPolicy {
    /// Brain milliseconds without progress before a stall recovery may fire.
    pub stall_ms: u64,
    /// Brain milliseconds between stall recoveries.
    pub cooldown_ms: u64,
    /// Brain milliseconds between game-over recoveries, or `None` to disable the
    /// game-over trigger entirely. `None` is the default: Pokémon Red has no
    /// game-over state the ratchet acts on, and a blackout there is not a lost
    /// run.
    pub game_over_cooldown_ms: Option<u64>,
    /// Brain milliseconds of sustained unsafe activity that restarts the stall
    /// window.
    pub unsafe_reset_ms: u64,
    /// Recovery attempts allowed at one best rank.
    pub max_attempts: u64,
    /// Lifetime recovery budget.
    pub max_recoveries: u64,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            stall_ms: STALL_MS,
            cooldown_ms: RECOVERY_COOLDOWN_MS,
            game_over_cooldown_ms: None,
            unsafe_reset_ms: UNSAFE_RESET_MS,
            max_attempts: MAX_ATTEMPTS,
            max_recoveries: MAX_RECOVERIES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatchetState {
    pub version: u32,
    pub best: u64,
    pub recoveries: u64,
    pub attempts: u64,
    #[serde(rename = "lastProgress")]
    pub last_progress: u64,
    #[serde(rename = "lastRecovery")]
    pub last_recovery: u64,
    pub coverage: u64,
}

impl Default for RatchetState {
    fn default() -> Self {
        Self {
            version: 1,
            best: 0,
            recoveries: 0,
            attempts: 0,
            last_progress: 0,
            last_recovery: 0,
            coverage: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetError(pub &'static str);

impl std::fmt::Display for RatchetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for RatchetError {}

/// The best safe snapshot: emulator state plus the exact framebuffer that was
/// on screen when it was taken. The framebuffer is not part of binjgb's
/// `EmulatorState`, so it has to be carried alongside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub game: Vec<u8>,
    pub frame: Vec<u8>,
}

/// Only safe, stable overworld samples count toward the stall window.
#[derive(Debug, Clone, Default)]
pub struct Ratchet {
    pub state: RatchetState,
    pub snapshot: Option<Snapshot>,
    policy: RecoveryPolicy,
    unsafe_since: Option<u64>,
}

impl Ratchet {
    pub fn new() -> Self {
        Self::default()
    }

    /// A ratchet with per-game limits, from the adapter's
    /// [`RecoveryPolicy`].
    pub fn with_policy(policy: RecoveryPolicy) -> Self {
        Self { policy, ..Self::default() }
    }

    pub fn policy(&self) -> RecoveryPolicy {
        self.policy
    }

    /// Record one sample and say whether the caller should now recover.
    ///
    /// `capture` is invoked only when `rank` beats the best safe rank seen; it
    /// must return the current emulator state and framebuffer.
    pub fn observe<F>(
        &mut self,
        safe: bool,
        rank: u64,
        coverage: u64,
        now: u64,
        capture: F,
    ) -> bool
    where
        F: FnOnce() -> Snapshot,
    {
        self.observe_with_game_over(safe, rank, coverage, now, false, capture)
    }

    /// [`Ratchet::observe`] plus the game-over trigger.
    ///
    /// `game_over` comes from [`crate::adapter::GameAdapter::game_over`], which
    /// is false for every adapter that does not implement it. When it is true
    /// *and* the policy enables the trigger, the stall window is skipped: a game
    /// over discards the whole run and parks the fly on a static title screen,
    /// so there is nothing to wait for (`docs/design/platformer.md` §5, restore
    /// trigger A). Its own cooldown and the same attempt and lifetime budgets
    /// still apply, so a run that cannot be saved is left alone.
    pub fn observe_with_game_over<F>(
        &mut self,
        safe: bool,
        rank: u64,
        coverage: u64,
        now: u64,
        game_over: bool,
        capture: F,
    ) -> bool
    where
        F: FnOnce() -> Snapshot,
    {
        if !safe {
            let since = *self.unsafe_since.get_or_insert(now);
            // Allow brief walking/warp instability; sustained menus, scripts
            // and battles restart the stall window rather than aging into a
            // reset.
            if now.saturating_sub(since) >= self.policy.unsafe_reset_ms {
                self.state.last_progress = now;
            }
        } else {
            self.unsafe_since = None;
        }
        if coverage > self.state.coverage {
            self.state.last_progress = now;
        }
        self.state.coverage = self.state.coverage.max(coverage);
        if safe && rank > self.state.best {
            self.snapshot = Some(capture());
            self.state.best = rank;
            self.state.attempts = 0;
            self.state.last_progress = now;
        }
        if self.budget_spent() {
            return false;
        }
        // Trigger A, game over: no stall wait, its own cooldown. `lastRecovery`
        // is 0 until the first recovery, so the cooldown only applies once one
        // has happened; otherwise a game over in the first minute of a run would
        // be measured against the clock's origin. The stall trigger keeps its
        // original behaviour here, which is why this is not shared.
        if let Some(cooldown) = self.policy.game_over_cooldown_ms.filter(|_| game_over) {
            let cooling = self.state.recoveries > 0
                && now.saturating_sub(self.state.last_recovery) < cooldown;
            return !cooling && self.spend(now);
        }
        // Trigger B, stall: an unsafe sample never recovers, because the restore
        // would land on top of whatever made it unsafe.
        if !safe
            || now.saturating_sub(self.state.last_progress) < self.policy.stall_ms
            || now.saturating_sub(self.state.last_recovery) < self.policy.cooldown_ms
        {
            return false;
        }
        self.spend(now)
    }

    /// Whether no recovery is possible at all: nothing archived, or a budget
    /// exhausted. Deliberately checked before either trigger, so an exhausted run
    /// stays unrecovered rather than looping.
    fn budget_spent(&self) -> bool {
        self.snapshot.is_none()
            || self.state.attempts >= self.policy.max_attempts
            || self.state.recoveries >= self.policy.max_recoveries
    }

    /// Charge one recovery against both budgets and restart the stall window.
    fn spend(&mut self, now: u64) -> bool {
        self.state.recoveries += 1;
        self.state.attempts += 1;
        self.state.last_recovery = now;
        self.state.last_progress = now;
        true
    }

    /// Restore from a checkpoint. `None` is a legacy checkpoint with no ratchet
    /// extension: it seeds itself at the next safe observation.
    ///
    /// `ladder_len` is the length of the running adapter's
    /// [`crate::GameAdapter::rank_ladder`], which bounds `best`. It is a parameter
    /// rather than a constant because the bound belongs to the adapter: this module
    /// knows nothing about any game, the Pokémon ladder is 38 rungs and the
    /// platformer's is 16, and a `best` past the end of the live ladder would index
    /// a label that does not exist. The budget bounds, by contrast, come from this
    /// ratchet's [`RecoveryPolicy`], because those are per-game numbers.
    pub fn import(
        &mut self,
        state: Option<RatchetState>,
        snapshot: Option<Snapshot>,
        ladder_len: usize,
    ) -> Result<(), RatchetError> {
        let Some(state) = state else { return Ok(()) };
        const INVALID: RatchetError = RatchetError("Invalid ratchet checkpoint");
        // The budget bounds come from this ratchet's own policy, so a checkpoint
        // written under a larger per-game budget is not rejected by the default
        // one and vice versa.
        if state.version != 1
            || state.best >= ladder_len as u64
            || state.attempts > self.policy.max_attempts
            || state.recoveries > self.policy.max_recoveries
        {
            return Err(INVALID);
        }
        if state.best > 0 {
            match &snapshot {
                Some(snapshot)
                    if !snapshot.game.is_empty() && snapshot.frame.len() == FRAMEBUFFER_LEN => {}
                _ => return Err(INVALID),
            }
        }
        self.state = state;
        self.snapshot = snapshot;
        self.unsafe_since = None;
        Ok(())
    }

    /// The snapshot's emulator state, if one has been captured.
    pub fn game(&self) -> Option<&[u8]> {
        self.snapshot.as_ref().map(|snapshot| snapshot.game.as_slice())
    }

    /// The snapshot's framebuffer, if one has been captured.
    pub fn frame(&self) -> Option<&[u8]> {
        self.snapshot.as_ref().map(|snapshot| snapshot.frame.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The ladder length these tests bound `best` against: the Pokemon ladder's
    /// 38 rungs, so ranks 0..=37 are in range and 38 is not.
    const LADDER: usize = 38;

    fn snapshot(id: u8) -> Snapshot {
        Snapshot { game: vec![id], frame: vec![0; FRAMEBUFFER_LEN] }
    }

    #[test]
    fn captures_rising_safe_milestones_persists_best_bounds_recovery_and_excludes_dialogue() {
        let mut ratchet = Ratchet::new();
        let captures = Cell::new(0u8);
        let capture = || {
            captures.set(captures.get() + 1);
            snapshot(captures.get())
        };

        ratchet.observe(false, 5, 0, 0, capture);
        assert_eq!(captures.get(), 0, "an unsafe sample never captures");

        ratchet.observe(true, 1, 1, 10, capture);
        ratchet.observe(true, 1, 1, 20, capture);
        assert_eq!(captures.get(), 1, "an equal rank does not recapture");

        ratchet.observe(true, 3, 2, 30, capture);
        ratchet.observe(true, 2, 2, 40, capture);
        assert_eq!(captures.get(), 2, "a regressed rank does not recapture");

        let mut restored = Ratchet::new();
        restored.import(Some(ratchet.state), ratchet.snapshot.clone(), LADDER).unwrap();

        assert!(!restored.observe(false, 2, 2, 179_000, capture));
        assert!(!restored.observe(false, 2, 2, 180_000, capture));
        assert!(!restored.observe(true, 2, 2, 180_001, capture));
        assert!(restored.observe(true, 2, 2, 300_001, capture));
        assert!(restored.observe(true, 2, 2, 480_001, capture));
        assert!(restored.observe(true, 2, 2, 660_001, capture));
        assert!(!restored.observe(true, 2, 2, 840_001, capture), "three attempts per rank");

        assert_eq!(restored.state.best, 3);
        assert_eq!(restored.game().unwrap()[0], 2);

        let bad = RatchetState { version: 2, ..ratchet.state };
        assert_eq!(
            Ratchet::new().import(Some(bad), ratchet.snapshot.clone(), LADDER).unwrap_err(),
            RatchetError("Invalid ratchet checkpoint")
        );
    }

    #[test]
    fn a_legacy_checkpoint_without_a_ratchet_extension_is_accepted() {
        let mut ratchet = Ratchet::new();
        ratchet.import(None, None, LADDER).unwrap();
        assert_eq!(ratchet.state, RatchetState::default());
        assert!(ratchet.snapshot.is_none());
    }

    #[test]
    fn a_best_rank_without_a_usable_snapshot_is_rejected() {
        let state = RatchetState { best: 3, ..RatchetState::default() };
        assert!(Ratchet::new().import(Some(state), None, LADDER).is_err());
        assert!(
            Ratchet::new()
                .import(Some(state), Some(Snapshot { game: vec![1], frame: vec![0; 16] }), LADDER)
                .is_err(),
            "a truncated framebuffer is not a snapshot"
        );
        assert!(
            Ratchet::new()
                .import(Some(state), Some(Snapshot { game: Vec::new(), frame: vec![0; FRAMEBUFFER_LEN] }), LADDER)
                .is_err()
        );
        assert!(Ratchet::new().import(Some(state), Some(snapshot(1)), LADDER).is_ok());
    }

    #[test]
    fn out_of_range_counters_are_rejected() {
        for state in [
            RatchetState { best: LADDER as u64, ..RatchetState::default() },
            RatchetState { attempts: MAX_ATTEMPTS + 1, ..RatchetState::default() },
            RatchetState { recoveries: MAX_RECOVERIES + 1, ..RatchetState::default() },
        ] {
            assert!(
                Ratchet::new().import(Some(state), Some(snapshot(1)), LADDER).is_err(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn the_rank_bound_is_the_ladder_the_caller_passes_not_a_constant() {
        // The same checkpoint is valid under one adapter's ladder and not another's.
        let state = RatchetState { best: 20, ..RatchetState::default() };
        assert!(Ratchet::new().import(Some(state), Some(snapshot(1)), 38).is_ok());
        assert!(
            Ratchet::new().import(Some(state), Some(snapshot(1)), 16).is_err(),
            "rank 20 is off the end of a 16-rung ladder"
        );

        // The top rung is in range and one past it is not, for any length.
        for ladder in [1usize, 8, 16, 38] {
            let top = RatchetState { best: ladder as u64 - 1, ..RatchetState::default() };
            let past = RatchetState { best: ladder as u64, ..RatchetState::default() };
            assert!(
                Ratchet::new().import(Some(top), Some(snapshot(1)), ladder).is_ok(),
                "ladder {ladder}: rank {} should be in range",
                ladder - 1
            );
            assert!(
                Ratchet::new().import(Some(past), Some(snapshot(1)), ladder).is_err(),
                "ladder {ladder}: rank {ladder} should be out of range"
            );
        }
    }

    #[test]
    fn the_budgets_are_the_long_ladder_s() {
        // 36 lifetime recoveries against the 38-rung ladder, three per rung as
        // before: the lifetime budget is what stops a pathological run, and it is
        // the only one the longer ladder changed.
        assert_eq!(MAX_RECOVERIES, 36);
        assert_eq!(MAX_ATTEMPTS, 3);
        assert_eq!(STALL_MS, 120_000, "the stall window is unchanged");
        assert_eq!(RECOVERY_COOLDOWN_MS, 180_000, "and so is the cooldown");
    }

    #[test]
    fn sustained_unsafe_activity_restarts_the_stall_window() {
        let mut ratchet = Ratchet::new();
        let capture = || snapshot(1);
        ratchet.observe(true, 1, 1, 0, capture);
        // A stall long enough to recover, interrupted by a sustained battle.
        assert!(!ratchet.observe(false, 1, 1, 100_000, || snapshot(1)));
        assert!(!ratchet.observe(false, 1, 1, 130_000, || snapshot(1)));
        // lastProgress moved to 130_000, so 200_000 is only 70 s of stall.
        assert!(!ratchet.observe(true, 1, 1, 200_000, || snapshot(1)));
        assert!(ratchet.observe(true, 1, 1, 260_000, || snapshot(1)));
    }

    #[test]
    fn new_coverage_resets_the_stall_window() {
        let mut ratchet = Ratchet::new();
        ratchet.observe(true, 1, 1, 0, || snapshot(1));
        assert!(!ratchet.observe(true, 1, 2, 119_000, || snapshot(1)));
        // The new coverage moved lastProgress to 119_000, so this is 111 s.
        assert!(!ratchet.observe(true, 1, 2, 230_000, || snapshot(1)));
        assert!(ratchet.observe(true, 1, 2, 239_001, || snapshot(1)));
    }
}
