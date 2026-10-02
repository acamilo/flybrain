//! What the scorecard sees of one frame, whichever runtime ran it.
//!
//! The legacy loop hands a harness a live emulator and the frame's events; the session runtime
//! hands it a task object holding the boundary's memory image. Both are reduced to the same
//! [`Frame`] here, so every metric is computed once ([`crate::tally`]) and a comparison between
//! the two runtimes cannot differ in what was measured.

use flybrain_gb::MemoryReader;
use flybrain_gb::pokemon_red::state;
use flybrain_gb::pokemon_red::symbols::ram;

/// One macro event: a start (`outcome` is `None`) or a finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroEv {
    pub name: &'static str,
    pub outcome: Option<&'static str>,
}

/// A rollback the ratchet ran at this boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rolled {
    /// The trigger was a game over, else a stall.
    pub game_over: bool,
}

/// Facts read off game memory on one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// `wIsInBattle`: 0 none, 1 wild, 2 trainer, `$ff` the frame a battle is lost.
    pub battle: u8,
    /// Party members with a species (a half-written slot is not counted).
    pub party: u8,
    /// Of them, those with HP left.
    pub alive: u8,
    /// Poke Balls of every kind in the bag.
    pub balls: u32,
    pub money: u32,
    /// Species owned (Pokedex bits).
    pub owned: u32,
    /// The map the player stands on, `0xff` when the adapter cannot place the player.
    pub map: u32,
}

impl Facts {
    /// Every member of a non-empty party has fainted: the state a whiteout starts from.
    pub fn all_fainted(&self) -> bool {
        self.party > 0 && self.alive == 0
    }

    pub fn in_battle(&self) -> bool {
        self.battle != 0
    }
}

/// Reads [`Facts`] from any memory (a live emulator or a boundary image).
pub fn facts_of(memory: &mut dyn MemoryReader) -> Facts {
    let battle = memory.read8(ram::wIsInBattle);
    let party = state::party(memory);
    let real: Vec<_> = party.mons.iter().filter(|mon| mon.species != 0).collect();
    let alive = real.iter().filter(|mon| mon.hp > 0).count();
    let balls: u32 = state::bag(memory)
        .iter()
        .filter(|item| (0x01..=0x04).contains(&item.id))
        .map(|item| u32::from(item.count))
        .sum();
    let money = state::money(memory);
    let owned: u32 = (0..19u16).map(|i| memory.read8(ram::wPokedexOwned + i).count_ones()).sum();
    let map = state::player(memory).map_or(0xff, |player| u32::from(player.map));
    Facts {
        battle,
        party: real.len() as u8,
        alive: alive as u8,
        balls,
        money,
        owned,
        map,
    }
}

/// One frame, as the scorecard takes it.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The brain clock of the frame, ms.
    pub ms: f64,
    /// The macro layer's events this frame: executed, abandoned and a rollback's, in order.
    pub events: Vec<MacroEv>,
    /// Rewards paid this frame: the adapter's kind and value.
    pub rewards: Vec<(&'static str, f64)>,
    pub rank: u32,
    /// Distinct places the adapter has observed (the exploration count).
    pub places: u32,
    /// `(map, x, y)` the adapter placed the player on, when it could.
    pub location: Option<(u32, u32, u32)>,
    /// The macro layer is on (macros mode).
    pub macros_on: bool,
    /// No macro runs and the scene dealt no button.
    pub pad_empty: bool,
    pub scene: &'static str,
    pub rollback: Option<Rolled>,
    pub facts: Facts,
}

/// The idle frames a seed starts with.
///
/// A checkpoint restores a deterministic brain, so two runs from it with different "seeds" would
/// be the same run. A seed therefore chooses how many frames the readout is ignored before the
/// measurement starts: the brain still ticks, the noise generator still advances, the game sees
/// no button, and from the first measured frame the brain is on a different point of its own
/// trajectory. Seed 0 is no idle at all (the run the trap hunt does from the same checkpoint).
/// Nothing presses for the fly and nothing changes what it chooses once the measurement starts.
pub fn idle_frames(seed: u32) -> u32 {
    if seed == 0 {
        return 0;
    }
    // splitmix64 of the seed, so consecutive seeds are unrelated; 1 to 30 seconds of idle.
    let mut z = u64::from(seed).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    60 + (z % 1741) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_zero_is_no_idle_and_seeds_differ() {
        assert_eq!(idle_frames(0), 0);
        let idles: Vec<u32> = (1..=8).map(idle_frames).collect();
        assert!(idles.iter().all(|n| (60..=1800).contains(n)));
        let mut sorted = idles.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), idles.len(), "{idles:?}");
        assert_eq!(idle_frames(3), idle_frames(3));
    }

    #[test]
    fn a_party_that_has_all_fainted() {
        let down = Facts { party: 2, alive: 0, ..Facts::default() };
        assert!(down.all_fainted());
        assert!(!Facts { party: 2, alive: 1, ..Facts::default() }.all_fainted());
        assert!(!Facts::default().all_fainted(), "no party is not a whiteout");
    }
}
