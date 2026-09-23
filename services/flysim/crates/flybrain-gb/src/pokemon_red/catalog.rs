//! The `pokered-unique8-v7` reward catalog.
//!
//! A direct port of the prototype's `src/reward/catalog.ts`, including the
//! declaration order, which is the order `counts` and `last` serialize in.

use std::collections::BTreeMap;

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::adapter::RewardEvent;

/// Interned reward kind names. These are the JSON keys in a checkpoint and the
/// grouping keys the page's ticker uses.
pub mod kind {
    pub const MILESTONE: &str = "milestone";
    pub const EXPLORATION: &str = "exploration";
    pub const MAP: &str = "map";
    pub const SPECIES: &str = "species";
    pub const TRAINER: &str = "trainer";
    pub const BATTLE: &str = "battle";
    pub const BADGE: &str = "badge";
    pub const BOUNDARY: &str = "boundary";
    pub const CATCH: &str = "catch";
    pub const TALK: &str = "talk";
    pub const ITEM: &str = "item";
}

/// What a `catch` of a species this run has already caught pays.
///
/// Not a multiple of the rule's catalog value, because no binary float scales 0.30 into
/// exactly 0.10: `0.3 * (1.0 / 3.0)` is `0.09999999999999999`, and that number would reach
/// the ticker, the checkpoint and `docs/rewards-learning.md`'s table as itself. `boundary`'s
/// two payouts are 0.05 and 0.10, which a scale of two does express exactly, so that rule
/// still goes through the scaling path.
pub const CATCH_REPEAT_VALUE: f64 = 0.10;

#[derive(Debug, Clone, Copy)]
pub struct RewardRule {
    pub kind: &'static str,
    /// Short name shown on screen.
    pub label: &'static str,
    pub trigger: &'static str,
    pub value: f64,
    /// PAM stimulation this payout drives. Overlapping pulses take their
    /// maximum; the neural side owns that, not this crate.
    pub stimulation_ms: u32,
}

pub const REWARDS: [RewardRule; 11] = [
    RewardRule {
        kind: kind::MILESTONE,
        label: "Story",
        trigger: "First story flag / adventure started",
        value: 1.0,
        stimulation_ms: 250,
    },
    RewardRule {
        kind: kind::EXPLORATION,
        label: "Explore",
        trigger: "8 new player-coordinate locations; max 25/map",
        value: 0.05,
        stimulation_ms: 80,
    },
    RewardRule {
        kind: kind::MAP,
        label: "Area",
        trigger: "First visit to a map",
        value: 0.2,
        stimulation_ms: 100,
    },
    RewardRule {
        kind: kind::SPECIES,
        label: "Pokedex",
        trigger: "First owned species (catch, gift, evolution)",
        value: 0.5,
        stimulation_ms: 200,
    },
    RewardRule {
        kind: kind::TRAINER,
        label: "Trainer",
        trigger: "First defeated-trainer event flag",
        value: 0.5,
        stimulation_ms: 200,
    },
    RewardRule {
        kind: kind::BATTLE,
        label: "Wild win",
        trigger: "Confirmed wild KO; max 3 per map/species/level",
        value: 0.1,
        stimulation_ms: 100,
    },
    RewardRule {
        kind: kind::BADGE,
        label: "Badge",
        trigger: "Each new badge bit",
        value: 3.0,
        stimulation_ms: 400,
    },
    // `docs/design/room-escape.md` section 2. Appended rather than slotted next to
    // `exploration` so the declaration order of the seven rules the prototype shipped --
    // which is the order simultaneous payouts reach the ticker, and the key order `counts`
    // serializes in -- does not move.
    //
    // One rule, two payouts: the catalog value is the *adjacent* one, and standing on the
    // exit itself pays twice it through `emit`'s scale. A kind carries one value, and a
    // second kind for the same rule would put a second counter on the page for no reason.
    RewardRule {
        kind: kind::BOUNDARY,
        label: "Exit",
        trigger: "First tile adjacent to a map exit (x2 on the exit); once per map and exit",
        value: 0.05,
        stimulation_ms: 100,
    },
    // The operator's decision of 2026-09-22: the fly is paid for *keeping* a wild Pokémon, not
    // only for knocking one out. Appended rather than slotted next to `species` for the same
    // reason `boundary` was appended -- the declaration order is the key order `counts`
    // serializes in, and every checkpoint already written carries the first eight in this order.
    //
    // One rule, two payouts, like `boundary`: this value is what a species this run has never
    // caught pays, and [`CATCH_REPEAT_VALUE`] is what a repeat pays. The existing `species`
    // rule is untouched and still pays 0.50 the first time a species is owned by any means, so
    // a first catch of a new species pays 0.50 + 0.30 across two kinds.
    RewardRule {
        kind: kind::CATCH,
        label: "Catch",
        trigger: "Wild Pokémon caught; 0.10 for a species already caught; max 3 per species",
        value: 0.30,
        stimulation_ms: 150,
    },
    // The operator's decision of 2026-09-23: pay the fly for engaging with what is *inside* a
    // building rather than for leaving it (`boundary` pays nothing on an indoor map from v7).
    // Both appended, for the reason every rule since `boundary` was: the declaration order is
    // the key order `counts` serializes in, and every checkpoint already written carries the
    // first nine in this order.
    //
    // `talk` is one payout per person or sign per map for the lifetime of the ledger, and only
    // indoors: the conversation the fly opened by pressing A at it, paid when the box closes.
    RewardRule {
        kind: kind::TALK,
        label: "Talk",
        trigger: "Conversation the fly opened indoors; once per map and person or sign",
        value: 0.10,
        stimulation_ms: 100,
    },
    // `item` is one payout per item ball or hidden item for the lifetime of the ledger, on any
    // map: the cartridge's own "this one has been taken" bit rising.
    RewardRule {
        kind: kind::ITEM,
        label: "Item",
        trigger: "Item ball or hidden item picked up; once per item",
        value: 0.15,
        stimulation_ms: 120,
    },
];

/// Position of `kind` in [`REWARDS`], or `None` for an unknown kind. This is
/// the port of the TypeScript `kind in REWARDS` membership test.
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
/// object holding only the kinds that have paid, like the prototype's
/// `Partial<Record<RewardKind, RewardEvent>>`.
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
        REWARDS.iter().enumerate().filter_map(|(i, rule)| {
            self.0[i].as_ref().map(|event| (rule.kind, event))
        })
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
    fn catalog_values_match_the_prototype() {
        assert_eq!(rule(kind::MILESTONE).unwrap().value, 1.0);
        assert_eq!(rule(kind::EXPLORATION).unwrap().value, 0.05);
        assert_eq!(rule(kind::MAP).unwrap().value, 0.2);
        assert_eq!(rule(kind::SPECIES).unwrap().value, 0.5);
        assert_eq!(rule(kind::TRAINER).unwrap().value, 0.5);
        assert_eq!(rule(kind::BATTLE).unwrap().value, 0.1);
        assert_eq!(rule(kind::BADGE).unwrap().value, 3.0);
        // Not the prototype's: `docs/design/room-escape.md` section 2 adds it. The on-exit
        // payout is this value scaled by two, which is what keeps 0.05 and 0.10 out of two
        // separate kinds.
        assert_eq!(rule(kind::BOUNDARY).unwrap().value, 0.05);
        assert_eq!(rule(kind::BOUNDARY).unwrap().stimulation_ms, 100);
        // Nor the prototype's: the operator's catch rule, `pokered-unique8-v6`.
        assert_eq!(rule(kind::CATCH).unwrap().value, 0.30);
        assert_eq!(CATCH_REPEAT_VALUE, 0.10);
        assert_eq!(rule(kind::CATCH).unwrap().stimulation_ms, 150);
        // Nor these: the operator's engagement rules, `pokered-unique8-v7`.
        assert_eq!(rule(kind::TALK).unwrap().value, 0.10);
        assert_eq!(rule(kind::TALK).unwrap().stimulation_ms, 100);
        assert_eq!(rule(kind::ITEM).unwrap().value, 0.15);
        assert_eq!(rule(kind::ITEM).unwrap().stimulation_ms, 120);
        assert!(rule("blackout").is_none(), "the catalog has no penalties");
        assert!(REWARDS.iter().all(|rule| rule.value > 0.0));
    }

    #[test]
    fn new_rules_are_appended_so_the_older_key_order_does_not_move() {
        let order: Vec<&str> = REWARDS.iter().map(|rule| rule.kind).collect();
        assert_eq!(
            order,
            vec![
                kind::MILESTONE,
                kind::EXPLORATION,
                kind::MAP,
                kind::SPECIES,
                kind::TRAINER,
                kind::BATTLE,
                kind::BADGE,
                kind::BOUNDARY,
                kind::CATCH,
                kind::TALK,
                kind::ITEM,
            ]
        );
        assert_eq!(index(kind::ITEM), Some(REWARDS.len() - 1));
    }

    #[test]
    fn empty_counts_lists_every_kind_at_zero() {
        let counts = Counts::default();
        let json = serde_json::to_value(counts).unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(object.len(), REWARDS.len());
        assert!(object.values().all(|value| value == 0));
    }
}
