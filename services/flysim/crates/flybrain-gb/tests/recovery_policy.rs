//! Per-game recovery policies: the game-over trigger and the larger budgets.
//!
//! `docs/design/platformer.md` §5 ("Recovery and ratchet rules") asks for a
//! restore trigger a platformer needs and Pokémon does not, plus budgets a
//! platformer would otherwise spend in its first hour. Both arrive as a
//! [`RecoveryPolicy`] the adapter provides, so the ratchet itself stays generic.
//!
//! The first test is the important one: Pokémon's behaviour must be bit-for-bit
//! what it was before the policy existed.

use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::platformer::{PlatformerAdapter, RECOVERY_POLICY};
use flybrain_gb::ratchet::{
    MAX_ATTEMPTS, MAX_RECOVERIES, RECOVERY_COOLDOWN_MS, RecoveryPolicy, STALL_MS, UNSAFE_RESET_MS,
};
use flybrain_gb::{FRAMEBUFFER_LEN, Ratchet, RatchetState, Snapshot};

fn snapshot() -> Snapshot {
    Snapshot { game: vec![1], frame: vec![0; FRAMEBUFFER_LEN] }
}

#[test]
fn the_pokemon_adapter_keeps_the_original_policy_exactly() {
    let adapter = flybrain_gb::adapter_for("pokemon-red").unwrap();
    let policy = adapter.recovery_policy();
    assert_eq!(policy, RecoveryPolicy::default());
    assert_eq!(policy.stall_ms, STALL_MS);
    assert_eq!(policy.cooldown_ms, RECOVERY_COOLDOWN_MS);
    assert_eq!(policy.unsafe_reset_ms, UNSAFE_RESET_MS);
    assert_eq!(policy.max_attempts, MAX_ATTEMPTS);
    assert_eq!(policy.max_recoveries, MAX_RECOVERIES);
    assert_eq!(policy.game_over_cooldown_ms, None, "Pokémon has no game-over restore");
    assert!(!adapter.game_over());
}

#[test]
fn a_default_policy_ignores_the_game_over_flag_entirely() {
    let mut ratchet = Ratchet::new();
    ratchet.observe(true, 1, 1, 0, snapshot);
    // Same clock as the stall test below, where the platformer policy recovers
    // at once: with the trigger disabled nothing happens until the stall window.
    assert!(!ratchet.observe_with_game_over(false, 1, 1, 70_000, true, snapshot));
    assert!(!ratchet.observe_with_game_over(true, 1, 1, 70_000, true, snapshot));
    assert_eq!(ratchet.state.recoveries, 0);
    // And the 120-second window still fires on its own.
    assert!(ratchet.observe(true, 1, 1, 200_000, snapshot));
}

#[test]
fn the_platformer_policy_restores_on_a_game_over_without_waiting_out_a_stall() {
    let mut ratchet = Ratchet::with_policy(RECOVERY_POLICY);
    ratchet.observe(true, 4, 1, 0, snapshot);
    assert!(
        ratchet.observe_with_game_over(false, 4, 1, 1_000, true, snapshot),
        "a game over discards the run; there is nothing to wait for"
    );
    assert_eq!(ratchet.state.recoveries, 1);
    assert_eq!(ratchet.state.attempts, 1);

    // Its own 60-second cooldown, not the 180-second stall cooldown.
    assert!(!ratchet.observe_with_game_over(false, 4, 1, 60_000, true, snapshot));
    assert!(ratchet.observe_with_game_over(false, 4, 1, 61_000, true, snapshot));
    assert_eq!(ratchet.state.recoveries, 2);

    // Three attempts per rank, then the run is left alone.
    assert!(ratchet.observe_with_game_over(false, 4, 1, 130_000, true, snapshot));
    assert!(!ratchet.observe_with_game_over(false, 4, 1, 200_000, true, snapshot));
    assert_eq!(ratchet.state.attempts, 3);

    // A new best rank refreshes the attempt budget, as it does for a stall.
    ratchet.observe(true, 5, 1, 260_000, snapshot);
    assert_eq!(ratchet.state.attempts, 0);
    assert!(ratchet.observe_with_game_over(false, 5, 1, 330_000, true, snapshot));
}

#[test]
fn the_platformer_stall_window_is_three_hundred_seconds() {
    let mut ratchet = Ratchet::with_policy(RECOVERY_POLICY);
    ratchet.observe(true, 4, 1, 0, snapshot);
    assert!(!ratchet.observe(true, 4, 1, 299_000, snapshot), "Pokémon would have recovered");
    assert!(ratchet.observe(true, 4, 1, 300_001, snapshot));
    // A new band is new coverage, which restarts the window.
    assert!(!ratchet.observe(true, 4, 2, 600_001, snapshot));
    assert!(!ratchet.observe(true, 4, 2, 890_000, snapshot));
    assert!(ratchet.observe(true, 4, 2, 901_000, snapshot));
}

#[test]
fn an_exhausted_lifetime_budget_leaves_the_run_unrecovered() {
    let policy = RecoveryPolicy { max_recoveries: 2, ..RECOVERY_POLICY };
    let mut ratchet = Ratchet::with_policy(policy);
    ratchet.observe(true, 4, 1, 0, snapshot);
    let mut now = 0u64;
    let mut recovered = 0;
    for _ in 0..6 {
        now += 100_000;
        // A fresh best rank each time, so the per-rank budget is never the limit.
        ratchet.observe(true, 4 + recovered as u64 + 1, 1, now, snapshot);
        now += 100_000;
        if ratchet.observe_with_game_over(false, 4, 1, now, true, snapshot) {
            recovered += 1;
        }
    }
    assert_eq!(recovered, 2, "the lifetime budget is the ceiling");
    assert_eq!(ratchet.state.recoveries, 2);
}

#[test]
fn a_checkpoint_is_validated_against_this_ratchets_own_budgets() {
    let platformer = RatchetState {
        best: 4,
        recoveries: 40,
        attempts: 1,
        ..RatchetState::default()
    };
    // 40 recoveries is inside the platformer's budget of 48 and outside Pokémon's 36.
    assert!(
        Ratchet::with_policy(RECOVERY_POLICY)
            .import(Some(platformer), Some(snapshot()), platformer_ladder())
            .is_ok()
    );
    assert!(
        Ratchet::new().import(Some(platformer), Some(snapshot()), pokemon_ladder()).is_err()
    );
    assert_eq!(RECOVERY_POLICY.max_recoveries, 48);
    assert_eq!(MAX_RECOVERIES, 36, "and Pokémon's is the long ladder's 36");
}

/// The two bounds are independent: the budgets come from the policy, the rank bound
/// from the adapter's ladder. A roomy policy does not widen the ladder, and a long
/// ladder does not widen the budgets.
#[test]
fn the_rank_bound_is_the_adapters_ladder_whatever_the_policy_allows() {
    // Rank 20 exists on Pokémon's 38-rung ladder and not on the platformer's 16,
    // so the platformer's roomier policy still rejects it.
    let deep = RatchetState { best: 20, ..RatchetState::default() };
    assert!(
        Ratchet::with_policy(RECOVERY_POLICY)
            .import(Some(deep), Some(snapshot()), platformer_ladder())
            .is_err(),
        "rank 20 is off the end of a 16-rung ladder"
    );
    assert!(
        Ratchet::new().import(Some(deep), Some(snapshot()), pokemon_ladder()).is_ok(),
        "and on the ladder it came from it is fine under the default policy"
    );

    // The converse: a checkpoint inside the platformer's ladder but past Pokémon's
    // lifetime budget is refused for the budget, not the rank.
    let spent = RatchetState { best: 4, recoveries: 40, ..RatchetState::default() };
    assert!(Ratchet::new().import(Some(spent), Some(snapshot()), pokemon_ladder()).is_err());
}

fn platformer_ladder() -> usize {
    let len = PlatformerAdapter::new().rank_ladder().len();
    assert_eq!(len, 16, "docs/design/platformer.md §3: ranks 0 to 15");
    len
}

fn pokemon_ladder() -> usize {
    let len = flybrain_gb::adapter_for("pokemon-red").unwrap().rank_ladder().len();
    assert_eq!(len, 38, "docs/design/ladder.md: 38 rungs");
    len
}

#[test]
fn the_platformer_adapter_supplies_that_policy() {
    let adapter = PlatformerAdapter::new();
    assert_eq!(adapter.recovery_policy(), RECOVERY_POLICY);
    assert_eq!(RECOVERY_POLICY.game_over_cooldown_ms, Some(60_000));
    assert_eq!(RECOVERY_POLICY.stall_ms, 300_000);
    assert_eq!(RECOVERY_POLICY.max_attempts, 3);
}
