//! The `sml-progress-v1` reward catalog: `docs/design/platformer.md` §2, verbatim.
//!
//! Same shape as [`crate::pokemon_red::catalog`], including the declaration
//! order, which is the order `counts` and `last` serialize in. All values are
//! positive: there are no penalties, and death pays nothing and costs nothing
//! (§2, "Novelty caps so oscillation cannot farm").

use std::collections::BTreeMap;

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::adapter::RewardEvent;

/// Interned reward kind names. These are the JSON keys in a checkpoint and the
/// grouping keys the page's ticker uses.
pub mod kind {
    /// First boot -> playable transition.
    pub const STARTED: &str = "started";
    /// A new ten-column band of the current level.
    pub const BAND: &str = "band";
    /// `hCoins` increased.
    pub const COIN: &str = "coin";
    /// `wScore` increased; stands in for "enemy defeated" (§2).
    pub const SCORE: &str = "score";
    /// Super Mario or Superball Mario.
    pub const POWERUP: &str = "powerup";
    /// `wLives` increased.
    pub const LIFE: &str = "life";
    /// A level index never reached before.
    pub const LEVEL: &str = "level";
    /// A world never reached before.
    pub const WORLD: &str = "world";
    /// `hWinCount` rose: the game was finished.
    pub const CLEAR: &str = "clear";
}

#[derive(Debug, Clone, Copy)]
pub struct RewardRule {
    pub kind: &'static str,
    /// Short name shown on screen.
    pub label: &'static str,
    pub trigger: &'static str,
    /// Full payout. `score` multiplies this by its own delta and decay factor;
    /// every other kind pays it whole.
    pub value: f64,
    /// Lifetime payouts of this kind allowed per level, when the design caps it
    /// by a counter rather than by a novelty key. `None` means either uncapped
    /// (`life`) or capped by a key in the lifetime ledger (`band`, `powerup`,
    /// `level`, `world`, `started`, `clear`).
    pub per_level_cap: Option<u64>,
    /// PAM stimulation this payout drives. Overlapping pulses take their
    /// maximum; the neural side owns that, not this crate.
    pub stimulation_ms: u32,
}

pub const REWARDS: [RewardRule; 9] = [
    RewardRule {
        kind: kind::STARTED,
        label: "Start",
        trigger: "First boot -> playable transition observed",
        value: 1.0,
        per_level_cap: None,
        stimulation_ms: 250,
    },
    RewardRule {
        kind: kind::BAND,
        label: "Ground",
        trigger: "Each new 10-column band of the current level; max 2*(screens-3) per level",
        value: 0.05,
        per_level_cap: None,
        stimulation_ms: 80,
    },
    RewardRule {
        kind: kind::COIN,
        label: "Coin",
        trigger: "hCoins BCD increases (a mod-100 wrap counts as +1); 40 payouts per level",
        value: 0.02,
        per_level_cap: Some(40),
        stimulation_ms: 60,
    },
    RewardRule {
        kind: kind::SCORE,
        label: "Points",
        trigger: "wScore 3-byte BCD increases; min(1, delta/400), decays 1/(1+floor(n/4))",
        value: 0.05,
        per_level_cap: Some(20),
        stimulation_ms: 80,
    },
    RewardRule {
        kind: kind::POWERUP,
        label: "Power-up",
        trigger: "hSuperStatus reaches 2, or hSuperballMario becomes nonzero; first of each per level",
        value: 0.5,
        per_level_cap: None,
        stimulation_ms: 200,
    },
    RewardRule {
        kind: kind::LIFE,
        label: "1UP",
        trigger: "wLives increases",
        value: 1.0,
        per_level_cap: None,
        stimulation_ms: 250,
    },
    RewardRule {
        kind: kind::LEVEL,
        label: "Level",
        trigger: "hLevelIndex rises past its lifetime max",
        value: 3.0,
        per_level_cap: None,
        stimulation_ms: 400,
    },
    RewardRule {
        kind: kind::WORLD,
        label: "World",
        trigger: "High nibble of hWorldAndLevel rises past its lifetime max",
        value: 5.0,
        per_level_cap: None,
        stimulation_ms: 500,
    },
    RewardRule {
        kind: kind::CLEAR,
        label: "Game clear",
        trigger: "hWinCount rises",
        value: 10.0,
        per_level_cap: None,
        stimulation_ms: 800,
    },
];

/// Score payouts per level before the decay step changes
/// (`1/(1 + floor(n/4))`, mirroring the wild-KO decay in the Pokémon adapter).
pub const SCORE_DECAY_STEP: u64 = 4;

/// Score delta, in points, that earns a full [`kind::SCORE`] payout. Smaller
/// deltas pay `delta/400` of it.
pub const SCORE_FULL_DELTA: f64 = 400.0;

/// Position of `kind` in [`REWARDS`], or `None` for an unknown kind.
pub fn index(kind: &str) -> Option<usize> {
    REWARDS.iter().position(|rule| rule.kind == kind)
}

pub fn rule(kind: &str) -> Option<&'static RewardRule> {
    index(kind).map(|i| &REWARDS[i])
}

/// Lifetime payout count per kind, in catalog order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts([u64; REWARDS.len()]);

impl Counts {
    pub fn bump(&mut self, kind: &str) {
        if let Some(i) = index(kind) {
            self.0[i] += 1;
        }
    }

    pub fn get(&self, kind: &str) -> u64 {
        index(kind).map_or(0, |i| self.0[i])
    }

    pub fn set(&mut self, kind: &str, value: u64) {
        if let Some(i) = index(kind) {
            self.0[i] = value;
        }
    }

    pub fn to_map(self) -> BTreeMap<&'static str, u64> {
        REWARDS.iter().map(|rule| (rule.kind, self.get(rule.kind))).collect()
    }
}

impl Serialize for Counts {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(REWARDS.len()))?;
        for (i, rule) in REWARDS.iter().enumerate() {
            map.serialize_entry(rule.kind, &self.0[i])?;
        }
        map.end()
    }
}

/// The most recent payout of each kind, in catalog order. Serializes as an
/// object holding only the kinds that have paid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LastEvents([Option<RewardEvent>; REWARDS.len()]);

impl LastEvents {
    pub fn set(&mut self, event: RewardEvent) {
        if let Some(i) = index(event.kind) {
            self.0[i] = Some(event);
        }
    }

    pub fn get(&self, kind: &str) -> Option<&RewardEvent> {
        index(kind).and_then(|i| self.0[i].as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &RewardEvent)> {
        REWARDS
            .iter()
            .enumerate()
            .filter_map(|(i, rule)| self.0[i].as_ref().map(|event| (rule.kind, event)))
    }
}

impl Serialize for LastEvents {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (kind, event) in self.iter() {
            map.serialize_entry(kind, event)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_values_match_the_design_document() {
        assert_eq!(rule(kind::STARTED).unwrap().value, 1.0);
        assert_eq!(rule(kind::BAND).unwrap().value, 0.05);
        assert_eq!(rule(kind::COIN).unwrap().value, 0.02);
        assert_eq!(rule(kind::SCORE).unwrap().value, 0.05);
        assert_eq!(rule(kind::POWERUP).unwrap().value, 0.5);
        assert_eq!(rule(kind::LIFE).unwrap().value, 1.0);
        assert_eq!(rule(kind::LEVEL).unwrap().value, 3.0);
        assert_eq!(rule(kind::WORLD).unwrap().value, 5.0);
        assert_eq!(rule(kind::CLEAR).unwrap().value, 10.0);
        assert!(REWARDS.iter().all(|rule| rule.value > 0.0), "no penalties");
        assert!(rule("death").is_none(), "death pays nothing and costs nothing");
    }

    #[test]
    fn stimulation_and_caps_match_the_design_document() {
        let stimulation: Vec<u32> = REWARDS.iter().map(|rule| rule.stimulation_ms).collect();
        assert_eq!(stimulation, [250, 80, 60, 80, 200, 250, 400, 500, 800]);
        assert_eq!(rule(kind::COIN).unwrap().per_level_cap, Some(40));
        assert_eq!(rule(kind::SCORE).unwrap().per_level_cap, Some(20));
        assert_eq!(rule(kind::LIFE).unwrap().per_level_cap, None, "1UPs are uncapped");
    }

    #[test]
    fn empty_counts_lists_every_kind_at_zero() {
        let json = serde_json::to_value(Counts::default()).unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(object.len(), REWARDS.len());
        assert!(object.values().all(|value| value == 0));
        // Declaration order is the serialization order.
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            ["started", "band", "coin", "score", "powerup", "life", "level", "world", "clear"]
        );
    }
}
