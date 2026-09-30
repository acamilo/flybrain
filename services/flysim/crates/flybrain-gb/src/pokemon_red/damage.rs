//! What the damage rule of `pokered-unique8-v8` reads: HP the fly's own attack removed from the
//! Pokémon it is fighting.
//!
//! The operator's decision of 2026-09-29 (`docs/rewards-learning.md`, "Damage rewards"), chosen
//! over a pad rule that would have withheld a stat move once it had been used. Row 67
//! (`infra/docs/macros-traps.md`) is the reason: the live fly lost the Pewter Gym's Jr. Trainer 29
//! times running with TAIL WHIP 155, BUBBLE 23, TACKLE 0, and nothing in the catalog paid for
//! anything that happened inside a battle it did not win. This rule pays for the attack that lands,
//! so the move that deals damage is worth more than the one that does not, in the battle where the
//! choice is made.
//!
//! Everything here is a read of game memory after a frame. Nothing chooses, biases or presses a
//! button: which moves are on the pad is still the scene's (`docs/design/macros.md`), and which one
//! is pressed is still the fly's.
//!
//! ## What counts as the fly's attack
//!
//! A drop in `wEnemyMonHP` on a sample where `hWhoseTurn` reads 0 (the player's side is acting)
//! and `wPlayerMoveNum` is not `STRUGGLE`. At the pinned commit (`engine/battle/core.asm`):
//!
//! - `ApplyDamageToEnemyPokemon` subtracts `wDamage` from `wEnemyMonHP` inside
//!   `ExecutePlayerMove`, with `hWhoseTurn` 0. Multi-hit moves subtract once per hit.
//! - the enemy's poison and burn ticks and a Leech Seed on the enemy are
//!   `HandlePoisonBurnLeechSeed`, which picks the enemy's HP *because* `hWhoseTurn` is 1;
//!   the enemy's recoil, its confusion self-hit, its own Explosion and a Substitute it makes
//!   all happen in `ExecuteEnemyMove`, with `hWhoseTurn` 1. None of them pays.
//! - a Leech Seed on the *player* heals the enemy on the player's turn: HP goes up, which never
//!   pays (below). Damage into an enemy Substitute is `AttackSubstitute`, which never touches
//!   `wEnemyMonHP`.
//! - `STRUGGLE` is the move the cartridge executes for a Pokémon with no PP. The fly chose FIGHT,
//!   not the move, so it is not paid for; winning still pays what winning pays.
//!
//! ## What cannot be farmed
//!
//! - **Heal.** Each target keeps its lowest HP this battle; only HP below that pays. A Potion,
//!   Recover, Rest or a drain lifts the HP and the same HP is not paid twice.
//! - **Switch.** A target is `(wEnemyMonPartyPos, species, level)`, so a trainer's Pokémon that
//!   goes out and comes back keeps its mark, and two Weedles of one level are two targets. A
//!   target is marked the first time it reads full HP, which is how every enemy Pokémon enters a
//!   battle in this game, so a stale `wEnemyMon` left over from the last battle neither pays nor
//!   hides the real Pokémon's first hit.
//! - **The battle.** At most [`BATTLE_CAP`] a battle, whatever the party size.
//! - **Wild Pokémon.** A wild battle pays at the scale the wild-KO rule uses -- 1, 1/2, 1/3 --
//!   for its first three battles per `(map, species, level)` that paid any damage, and nothing
//!   after, and a rollback blocks every key that has paid. Trainer battles have no such ledger:
//!   a trainer is only fought again after losing to it (or a rollback), and the per-battle cap
//!   bounds each of those.

use serde_json::{Value, json};

use crate::adapter::MemoryReader;

use super::state::poke;
use super::symbols::ram;

/// What a trainer's Pokémon knocked out by the fly's own hit adds to that hit.
///
/// The damage share of the knockout hit is already paid; this is the step "one of the trainer's
/// Pokémon is down", on the frame it happens. Trainer battles only: a wild knockout is the
/// `battle` rule's, paid on the way out of the battle.
pub const KO_BONUS: f64 = 0.05;

/// The most damage pays in one battle: the `trainer` rule's own 0.50, so that a battle's worth
/// of damage is worth what winning it is. The Pewter Gym's Jr. Trainer (two Pokémon) reaches it
/// exactly: 2 x (0.20 + 0.05).
pub const BATTLE_CAP: f64 = 0.50;

/// Wild battles per `(map, species, level)` that may pay damage, for the lifetime of the ledger:
/// the wild-KO rule's three, for the same reason.
pub const MAX_WILD_BATTLES: u64 = 3;

/// The sanity bound the adapter already applies to an enemy's max HP (`sample`'s wild-KO read):
/// the largest in the game is under 1000, so anything else is a byte read out of a battle that
/// is not set up yet.
const MAX_HP_BOUND: u16 = 1000;

/// One enemy Pokémon, as far as a battle can tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// `wEnemyMonPartyPos`: the enemy party slot (`$ff` before a trainer's first send-out).
    pub slot: u8,
    pub species: u8,
    pub level: u8,
}

/// The bytes one sample reads for the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub target: Target,
    pub hp: u16,
    pub max_hp: u16,
    /// `hWhoseTurn` reads 0.
    pub fly_turn: bool,
    /// `wPlayerMoveNum`.
    pub move_id: u8,
}

impl Reading {
    /// Read one sample, or `None` when the enemy's HP is not a readable HP: a max of 0 or past
    /// [`MAX_HP_BOUND`], or a current HP above the max -- which is also what the one torn read
    /// possible here looks like, `ApplyDamageToEnemyPokemon`'s `sbc` having wrapped below zero
    /// before it clamps.
    pub fn read(memory: &mut impl MemoryReader) -> Option<Self> {
        let hp = word(memory, ram::wEnemyMonHP);
        let max_hp = word(memory, ram::wEnemyMonMaxHP);
        if max_hp == 0 || max_hp >= MAX_HP_BOUND || hp > max_hp {
            return None;
        }
        Some(Self {
            target: Target {
                slot: memory.read8(ram::wEnemyMonPartyPos),
                species: memory.read8(ram::wEnemyMonSpecies),
                level: memory.read8(ram::wEnemyMonLevel),
            },
            hp,
            max_hp,
            fly_turn: memory.read8(poke::H_WHOSE_TURN) == 0,
            move_id: memory.read8(ram::wPlayerMoveNum),
        })
    }
}

fn word(memory: &mut impl MemoryReader, address: u16) -> u16 {
    u16::from(memory.read8(address)) << 8 | u16::from(memory.read8(address + 1))
}

/// HP the fly's own attack removed on one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub target: Target,
    pub removed: u16,
    pub max_hp: u16,
    pub knocked_out: bool,
}

/// One target's mark: its max HP when first seen and the lowest HP it has shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    target: Target,
    max_hp: u16,
    low: u16,
}

/// The rule's state for one battle. Carried in the adapter's `battle` (and so in a checkpoint
/// taken mid-battle); a new battle starts with a new one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BattleDamage {
    marks: Vec<Mark>,
    /// Paid so far this battle, after the wild scale.
    paid: f64,
    /// The wild scale, decided at the battle's first hit; always 1 in a trainer battle.
    scale: Option<f64>,
    /// A battle restored from a state written before this rule: a target first seen is marked
    /// at the HP it has *now*, since what it had lost before the restore is unknown.
    restored: bool,
}

impl BattleDamage {
    /// The state for a battle a `v7` checkpoint was taken in.
    pub fn restored() -> Self {
        Self { restored: true, ..Self::default() }
    }

    pub fn paid(&self) -> f64 {
        self.paid
    }

    pub fn scale(&self) -> Option<f64> {
        self.scale
    }

    /// Update the marks with one sample and say what the fly is owed for, if anything.
    ///
    /// Every drop moves the mark, whoever caused it, so a poison tick is never paid later as
    /// part of the fly's next hit. Only a drop on the fly's own turn, by a move it chose, is a
    /// [`Hit`].
    pub fn observe(&mut self, reading: Reading) -> Option<Hit> {
        let index = match self.marks.iter().position(|mark| mark.target == reading.target) {
            Some(index) if self.marks[index].max_hp == reading.max_hp => index,
            found => {
                // Every enemy Pokémon enters a battle at its max HP -- a wild one is generated
                // full, a trainer's party is built full -- so a target not yet marked is only
                // real once it reads full. Anything else is `wEnemyMon` still holding the last
                // battle's Pokémon: `InitBattleVariables` does not clear it, and a trainer
                // battle sets `wIsInBattle` a whole transition before `LoadEnemyMonData` runs.
                // In the row 67 ring that stale Pokémon is the same trainer's, in the same
                // slot, at the HP it beat the fly with. A battle restored from a state without
                // this rule cannot know, and marks what it reads.
                if !self.restored && reading.hp != reading.max_hp {
                    return None;
                }
                let mark =
                    Mark { target: reading.target, max_hp: reading.max_hp, low: reading.hp };
                match found {
                    // The same slot, species and level with another max HP is another Pokémon
                    // (a wild one with other DVs): its mark starts again.
                    Some(index) => {
                        self.marks[index] = mark;
                        index
                    }
                    None => {
                        self.marks.push(mark);
                        self.marks.len() - 1
                    }
                }
            }
        };
        let mark = &mut self.marks[index];
        if reading.hp >= mark.low {
            return None;
        }
        let removed = mark.low - reading.hp;
        mark.low = reading.hp;
        if !reading.fly_turn || reading.move_id == poke::moves::STRUGGLE {
            return None;
        }
        Some(Hit {
            target: reading.target,
            removed,
            max_hp: reading.max_hp,
            knocked_out: reading.hp == 0,
        })
    }

    /// Decide the wild scale once per battle. `ledger` is `(battles already paid for this key,
    /// whether a rollback blocked it)`; the caller counts the battle when this returns a
    /// non-zero scale for the first time.
    pub fn decide_scale(&mut self, trainer: bool, ledger: (u64, bool)) -> (f64, bool) {
        if let Some(scale) = self.scale {
            return (scale, false);
        }
        let (count, blocked) = ledger;
        let scale = if trainer {
            1.0
        } else if blocked || count >= MAX_WILD_BATTLES {
            0.0
        } else {
            1.0 / (count as f64 + 1.0)
        };
        self.scale = Some(scale);
        (scale, !trainer && scale > 0.0)
    }

    /// What `hit` pays: `value` per whole Pokémon's max HP, plus [`KO_BONUS`] for a trainer's
    /// Pokémon knocked out, at the battle's scale, within what is left of [`BATTLE_CAP`].
    pub fn pay(&mut self, hit: Hit, value: f64, trainer: bool) -> f64 {
        let scale = self.scale.unwrap_or(0.0);
        let mut amount = value * f64::from(hit.removed) / f64::from(hit.max_hp);
        if trainer && hit.knocked_out {
            amount += KO_BONUS;
        }
        let amount = (amount * scale).min((BATTLE_CAP - self.paid).max(0.0));
        self.paid += amount;
        amount
    }

    pub fn to_json(&self) -> Value {
        json!({
            "paid": self.paid,
            "scale": self.scale,
            "restored": self.restored,
            "marks": self.marks.iter().map(|mark| json!([
                mark.target.slot, mark.target.species, mark.target.level, mark.max_hp, mark.low,
            ])).collect::<Vec<_>>(),
        })
    }

    /// The inverse of [`BattleDamage::to_json`]; `None` for anything malformed.
    pub fn from_json(value: &Value) -> Option<Self> {
        let paid = value.get("paid")?.as_f64().filter(|paid| paid.is_finite() && *paid >= 0.0)?;
        let scale = match value.get("scale") {
            None | Some(Value::Null) => None,
            Some(scale) => Some(scale.as_f64().filter(|scale| (0.0..=1.0).contains(scale))?),
        };
        let restored = value.get("restored")?.as_bool()?;
        let byte = |value: &Value| value.as_u64().and_then(|v| u8::try_from(v).ok());
        let hp = |value: &Value| value.as_u64().and_then(|v| u16::try_from(v).ok());
        let marks = value
            .get("marks")?
            .as_array()?
            .iter()
            .map(|mark| {
                let mark = mark.as_array().filter(|mark| mark.len() == 5)?;
                Some(Mark {
                    target: Target {
                        slot: byte(&mark[0])?,
                        species: byte(&mark[1])?,
                        level: byte(&mark[2])?,
                    },
                    max_hp: hp(&mark[3])?,
                    low: hp(&mark[4])?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { marks, paid, scale, restored })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGLETT: Target = Target { slot: 0, species: 0x3b, level: 11 };
    const SANDSHREW: Target = Target { slot: 1, species: 0x60, level: 11 };
    const BUBBLE: u8 = 145;
    const TACKLE: u8 = 33;

    fn fly(target: Target, hp: u16, max_hp: u16, move_id: u8) -> Reading {
        Reading { target, hp, max_hp, fly_turn: true, move_id }
    }

    fn enemy(target: Target, hp: u16, max_hp: u16) -> Reading {
        Reading { target, hp, max_hp, fly_turn: false, move_id: BUBBLE }
    }

    #[test]
    fn a_drop_on_the_flys_turn_is_a_hit_and_one_on_the_enemys_is_not() {
        let mut battle = BattleDamage::default();
        assert_eq!(battle.observe(fly(DIGLETT, 31, 31, BUBBLE)), None, "full HP is the mark");
        let hit = battle.observe(fly(DIGLETT, 20, 31, BUBBLE)).unwrap();
        assert_eq!((hit.removed, hit.knocked_out), (11, false));
        // The enemy's own poison tick: the mark moves, nothing is owed.
        assert_eq!(battle.observe(enemy(DIGLETT, 18, 31)), None);
        // ... so the next hit is paid from 18, not from 20.
        assert_eq!(battle.observe(fly(DIGLETT, 12, 31, TACKLE)).unwrap().removed, 6);
        // The same HP on the next sample is not a second hit.
        assert_eq!(battle.observe(fly(DIGLETT, 12, 31, TACKLE)), None);
    }

    #[test]
    fn struggle_is_not_the_flys_choice_and_pays_nothing() {
        let mut battle = BattleDamage::default();
        battle.observe(fly(DIGLETT, 31, 31, BUBBLE));
        assert_eq!(battle.observe(fly(DIGLETT, 25, 31, poke::moves::STRUGGLE)), None);
        assert_eq!(battle.observe(fly(DIGLETT, 20, 31, BUBBLE)).unwrap().removed, 5);
    }

    #[test]
    fn healed_hp_is_not_paid_twice() {
        let mut battle = BattleDamage::default();
        battle.observe(fly(DIGLETT, 31, 31, BUBBLE));
        assert_eq!(battle.observe(fly(DIGLETT, 10, 31, BUBBLE)).unwrap().removed, 21);
        // A Super Potion on the trainer's turn, or a drain on the fly's: up is never paid.
        assert_eq!(battle.observe(enemy(DIGLETT, 31, 31)), None);
        assert_eq!(battle.observe(fly(DIGLETT, 31, 31, BUBBLE)), None);
        // Down to 15 again is ground already paid for; 15 to 4 is new.
        assert_eq!(battle.observe(fly(DIGLETT, 15, 31, BUBBLE)), None);
        let hit = battle.observe(fly(DIGLETT, 0, 31, BUBBLE)).unwrap();
        assert_eq!((hit.removed, hit.knocked_out), (10, true));
    }

    #[test]
    fn a_switch_keeps_each_targets_mark() {
        let mut battle = BattleDamage::default();
        battle.observe(fly(DIGLETT, 31, 31, BUBBLE));
        battle.observe(fly(DIGLETT, 16, 31, BUBBLE));
        // Sandshrew comes out: a new target, marked at its max.
        assert_eq!(battle.observe(enemy(SANDSHREW, 33, 33)), None);
        assert_eq!(battle.observe(fly(SANDSHREW, 30, 33, TACKLE)).unwrap().removed, 3);
        // Diglett comes back at the 16 it left with: nothing new until it goes below.
        assert_eq!(battle.observe(enemy(DIGLETT, 16, 31)), None);
        assert_eq!(battle.observe(fly(DIGLETT, 10, 31, BUBBLE)).unwrap().removed, 6);
        // Two Pokémon of one species and level in different slots are two targets.
        let twin = Target { slot: 2, ..DIGLETT };
        assert_eq!(battle.observe(enemy(twin, 31, 31)), None);
        assert_eq!(battle.observe(fly(twin, 21, 31, BUBBLE)).unwrap().removed, 10);
    }

    #[test]
    fn a_stale_enemy_from_the_last_battle_cannot_pay_or_hide_the_real_one() {
        // The first in-battle samples can still hold the last battle's Pokémon at the HP it
        // finished on -- in the row 67 ring, the same trainer's Diglett in the same slot. It is
        // not marked until it reads full, so neither it nor the real Diglett's first hit is lost.
        let mut battle = BattleDamage::default();
        assert_eq!(battle.observe(enemy(DIGLETT, 7, 31)), None);
        assert_eq!(battle.observe(fly(DIGLETT, 3, 31, BUBBLE)), None, "not marked: not paid");
        assert_eq!(battle.observe(enemy(DIGLETT, 31, 31)), None, "the real one loads full");
        assert_eq!(battle.observe(fly(DIGLETT, 20, 31, BUBBLE)).unwrap().removed, 11);
        // The same identity with another max HP is another Pokémon: its mark starts again.
        assert_eq!(battle.observe(fly(DIGLETT, 29, 29, BUBBLE)), None);
        assert_eq!(battle.observe(fly(DIGLETT, 19, 29, BUBBLE)).unwrap().removed, 10);
    }

    #[test]
    fn a_battle_restored_without_the_rule_marks_targets_where_they_stand() {
        let mut battle = BattleDamage::restored();
        assert_eq!(battle.observe(fly(DIGLETT, 12, 31, BUBBLE)), None, "12/31 is the mark");
        assert_eq!(battle.observe(fly(DIGLETT, 5, 31, BUBBLE)).unwrap().removed, 7);
    }

    #[test]
    fn a_trainer_battle_pays_the_share_plus_the_knockout_and_stops_at_the_cap() {
        let mut battle = BattleDamage::default();
        assert_eq!(battle.decide_scale(true, (99, true)), (1.0, false), "no ledger for trainers");
        let value = 0.20;
        battle.observe(fly(DIGLETT, 31, 31, BUBBLE));
        let hit = battle.observe(fly(DIGLETT, 0, 31, BUBBLE)).unwrap();
        assert!((battle.pay(hit, value, true) - 0.25).abs() < 1e-12, "0.20 + the 0.05 knockout");
        battle.observe(fly(SANDSHREW, 33, 33, BUBBLE));
        let hit = battle.observe(fly(SANDSHREW, 0, 33, BUBBLE)).unwrap();
        assert!((battle.pay(hit, value, true) - 0.25).abs() < 1e-12);
        assert!((battle.paid() - BATTLE_CAP).abs() < 1e-12, "two Pokémon reach the cap");
        let third = Target { slot: 2, ..SANDSHREW };
        battle.observe(fly(third, 33, 33, BUBBLE));
        let hit = battle.observe(fly(third, 0, 33, BUBBLE)).unwrap();
        assert_eq!(battle.pay(hit, value, true), 0.0, "a third pays nothing");
    }

    #[test]
    fn a_wild_battle_pays_one_half_one_third_then_nothing() {
        let value = 0.20;
        let mut paid = Vec::new();
        for count in 0..4 {
            let mut battle = BattleDamage::default();
            let (scale, counted) = battle.decide_scale(false, (count, false));
            assert_eq!(counted, count < MAX_WILD_BATTLES);
            battle.observe(fly(DIGLETT, 31, 31, TACKLE));
            let hit = battle.observe(fly(DIGLETT, 0, 31, TACKLE)).unwrap();
            paid.push(battle.pay(hit, value, false));
            assert_eq!(battle.decide_scale(false, (0, false)).0, scale, "decided once a battle");
        }
        assert!((paid[0] - 0.20).abs() < 1e-12, "no knockout bonus in the wild: {paid:?}");
        assert!((paid[1] - 0.10).abs() < 1e-12);
        assert!((paid[2] - 0.20 / 3.0).abs() < 1e-12);
        assert_eq!(paid[3], 0.0);
        let mut blocked = BattleDamage::default();
        assert_eq!(blocked.decide_scale(false, (0, true)), (0.0, false), "a rollback blocks it");
    }

    #[test]
    fn the_battle_state_round_trips() {
        let mut battle = BattleDamage::default();
        battle.decide_scale(false, (1, false));
        battle.observe(fly(DIGLETT, 31, 31, BUBBLE));
        let hit = battle.observe(fly(DIGLETT, 20, 31, BUBBLE)).unwrap();
        battle.pay(hit, 0.2, false);
        let back = BattleDamage::from_json(&battle.to_json()).unwrap();
        assert_eq!(back, battle);
        assert!(BattleDamage::from_json(&json!({ "paid": -1.0 })).is_none());
        assert!(
            BattleDamage::from_json(&json!({
                "paid": 0.0, "scale": null, "restored": false, "marks": [[0, 1, 2, 3]]
            }))
            .is_none()
        );
    }
}
