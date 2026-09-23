//! What the sim loop needs from a game, independent of which game it is.
//!
//! One `flysim` binary serves both planned demos (Pokémon Red and a Game Boy
//! platformer), selecting the adapter at startup. Everything Pokémon-specific
//! lives in [`crate::pokemon_red`]; [`crate::ratchet`] is generic over
//! [`ProgressSnapshot::rank`] and imports no game symbols.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::ratchet::RecoveryPolicy;

/// Anything the reward adapter can sample bytes from.
///
/// `&mut self` because implementations cache: [`crate::Emulator`] memoizes each
/// address for the current frame, and each adapter memoizes again per sample, so
/// a sample crosses the FFI boundary at most once per address. This is the Rust
/// equivalent of the prototype's `MemoryReader` interface.
pub trait MemoryReader {
    fn read8(&mut self, address: u16) -> u8;

    /// One byte of a ROM bank, by bank number rather than off the CPU bus.
    ///
    /// [`MemoryReader::read8`] reads the bus, where banks 1 and up are whichever
    /// bank the cartridge's last switch left mapped -- so a table in bank 3 is
    /// unreadable through it, and the only way to make it readable would be to
    /// *write* the mapper's bank register. The joypad is the only write this
    /// workspace makes into a running game (`docs/design/macros.md` section 12),
    /// so this reads the cartridge image the process already holds instead: the
    /// same bytes, addressed the way the disassembly addresses them.
    ///
    /// `address` is a CPU address: below `$4000` it is bank 0 whatever `bank`
    /// says, and `$4000..$8000` is the banked window. Anything else, and any
    /// offset past the end of the image, is `None`.
    ///
    /// The default is `None`: a reader with no cartridge behind it cannot answer,
    /// and every caller of this is written to narrow rather than guess when it
    /// does not (`docs/design/macros-wram.md`, the whole-map grid).
    fn read_rom(&mut self, _bank: u8, _address: u16) -> Option<u8> {
        None
    }
}

impl MemoryReader for &mut dyn MemoryReader {
    fn read8(&mut self, address: u16) -> u8 {
        (**self).read8(address)
    }

    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        (**self).read_rom(bank, address)
    }
}

/// One reward payout in one frame.
///
/// `kind` is an adapter-owned interned name (Pokémon: `milestone`,
/// `exploration`, `map`, `species`, `trainer`, `battle`, `badge`, `boundary`, `catch`,
/// `talk`, `item`); it is the key the statistics counters and the on-screen ticker group
/// by. Field names serialize exactly as the prototype's `RewardEvent` did, so a
/// checkpoint written by either implementation reads in the other.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RewardEvent {
    pub kind: &'static str,
    pub label: String,
    #[serde(rename = "brainMs")]
    pub brain_ms: f64,
    pub value: f64,
    /// PAM stimulation this payout drives, from the adapter's catalog. Not part
    /// of the prototype's serialized event; skipped so checkpoints match.
    #[serde(skip)]
    pub stimulation_ms: u32,
}

/// Everything the stream page shows about "how far along is it", with no game
/// knowledge on the page side.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProgressSnapshot {
    /// Position on the adapter's milestone ladder, `0..=rank_max`.
    pub rank: u32,
    /// Highest rank the ladder defines.
    pub rank_max: u32,
    /// Label for the current rank, taken from [`GameAdapter::rank_ladder`].
    pub rank_label: &'static str,
    /// The game's headline countable: badges for Pokémon, lives for a
    /// platformer.
    pub counter: u32,
    /// What `counter` counts, for the page's label.
    pub counter_label: &'static str,
    /// Distinct player positions the adapter has observed (the prototype's
    /// `uniqueTiles`).
    pub unique_locations: usize,
    /// Lifetime sum of every payout.
    pub reward_total: f64,
    /// Lifetime payout count per `kind`.
    pub counts: BTreeMap<&'static str, u64>,
}

/// Which decoder configuration the game wants. Both demos are Game Boy titles,
/// but they need different timings: `docs/design/platformer.md` §4 explains why a
/// side-scroller needs gapless direction holds and a 300 ms jump, and the sim
/// loop selects the configuration from this id without learning about games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecoderPresetId {
    GameBoy,
    Platformer,
}

/// One way out of a map, in the terms an exploration ledger records.
///
/// The macro palette's `GO EXIT` needs to know which of the current map's exits this run has
/// already been through, and the answer is lifetime state an adapter already keeps: Pokémon Red's
/// `boundary` rule (`docs/design/room-escape.md` section 2) pays the first step next to and the
/// first step onto each exit, so its ledger holds a key per exit per map. This is the shape of the
/// question, with no game in it — a warp is a tile and an edge is a compass direction — and
/// [`GameAdapter::exit_visited`] is the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapExit {
    /// A warp — a door, a staircase, a cave mouth — on this tile of this map.
    Warp { map: u8, x: u8, y: u8 },
    /// A step off this edge of this map.
    Edge { map: u8, edge: MapEdge },
}

/// Which edge of a map a [`MapExit::Edge`] crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapEdge {
    North,
    South,
    East,
    West,
}

/// One tile of one map, in the terms an exploration ledger records.
///
/// [`MapExit`]'s companion, and the same kind of question: `GO FRONTIER` walks to "the nearest
/// tile bordering ground this run has never stood on" (`docs/design/macros.md` section 9), and
/// which tiles this run has stood on is lifetime state an adapter already keeps -- Pokemon Red's
/// `exploration` rule counts distinct player coordinates per map. [`GameAdapter::tile_visited`]
/// is the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MapTile {
    pub map: u8,
    pub x: u8,
    pub y: u8,
}

/// Where the ladder's next unreached rung is, as far as the adapter's rung catalog knows.
///
/// `GO OBJECTIVE` "paths toward the next unreached ladder rung's place where the catalog knows
/// one (map id and, when known, a tile or warp)" (`docs/design/macros.md` section 9). The map id
/// is the only part the Pokemon catalog can derive for every rung it knows at all, so `tile` and
/// `warp` are both optional and both unset rather than guessed; a rung with no place at all is
/// `None` from [`GameAdapter::objective`] and the plan falls through to its next entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MapPlace {
    /// The map the rung is earned on.
    pub map: u8,
    /// A tile on that map to stand on, when the catalog knows one.
    pub tile: Option<(u8, u8)>,
    /// An index into that map's warp table, when the catalog knows one.
    pub warp: Option<u8>,
    /// A map edge to step off, when the place is a connection rather than a tile.
    ///
    /// The third way a rung's place can be finer than "this map", and the one the cartridge makes
    /// cheapest: Oak stops the fly at Pallet Town's north exit, and "the north exit" is a
    /// connection rather than a warp or a coordinate. A tile would have to be invented from
    /// memory, which `docs/design/ladder.md`'s own "verified against the decomp" rule forbids;
    /// an edge is read straight off the map's connection bits.
    pub edge: Option<MapEdge>,
    /// What on that map earns the rung, when the rung is earned by talking to something rather
    /// than by standing somewhere.
    ///
    /// The fourth way a place can be finer than "this map", and the one the ladder needs most:
    /// most rungs of the early game are *conversations*. Oak's parcel is delivered by pressing A
    /// at Oak; the Pokédex comes out of the same script; the starter is a Pokéball on a table; a
    /// badge is the gym leader. A place that is only a map sends `GO OBJECTIVE` to the map's own
    /// door and calls that arriving — which is the Pallet Town loop of 2026-09-17
    /// (`infra/docs/macros-traps.md` row 29): the doormat, then `GO OUT` straight back out, every
    /// three brain seconds at Oak's lab door.
    ///
    /// Deliberately **not a sprite slot**. A slot is the index of the map's `object_event` list,
    /// which lives in a ROM bank this crate cannot reach, and `docs/design/ladder.md`'s rule is
    /// that a number nobody verified against the decomp does not go in. A *kind* needs no number:
    /// the macro layer already reads the loaded map's sprite list and already splits it into people
    /// and objects ([`crate::pokemon_red::macros::path`]), so "a person on this map" and "an object
    /// on this map" are both answerable from WRAM.
    pub target: Option<PlaceKind>,
}

/// What kind of thing on a map earns a rung ([`MapPlace::target`]).
///
/// The same split the macro palette already makes, and for the same reason: pokered's
/// `FIRST_STILL_SPRITE` divides the sprite list into people and things, `GO NPC` takes the first
/// half and `GO ITEM` the second, and the talked ledger keys both by the identity the cartridge
/// gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaceKind {
    /// Someone to talk to: Oak with the parcel, a gym leader with a badge.
    Person,
    /// Something to press A at: the starter's Pokéball, an item ball, a sign.
    Object,
}

impl MapPlace {
    /// A place that is a whole map and nothing finer.
    pub const fn at(map: u8) -> Self {
        Self { map, tile: None, warp: None, edge: None, target: None }
    }

    /// A place that is one of a map's edges: the connection out of it.
    pub const fn edge(map: u8, edge: MapEdge) -> Self {
        Self { map, tile: None, warp: None, edge: Some(edge), target: None }
    }

    /// A place that is a person somewhere on this map.
    pub const fn person(map: u8) -> Self {
        Self { map, tile: None, warp: None, edge: None, target: Some(PlaceKind::Person) }
    }

    /// A place that is an object somewhere on this map.
    pub const fn object(map: u8) -> Self {
        Self { map, tile: None, warp: None, edge: None, target: Some(PlaceKind::Object) }
    }
}

/// Reasons an adapter refuses a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError(pub &'static str);

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for AdapterError {}

/// A game, as the sim loop sees it.
pub trait GameAdapter: Send {
    /// Adapter version string, pinned into the checkpoint compatibility string.
    /// Pokémon: `pokered-unique8-v7`.
    fn id(&self) -> &'static str;

    /// Earlier [`GameAdapter::id`]s whose checkpoints this build can read, by a migration
    /// this adapter has written down and tested.
    ///
    /// The default is empty: an adapter migrates from nothing unless it says otherwise, which
    /// is the behaviour every adapter had before this existed. It is only half of the gate --
    /// [`crate::compatibility::decide`] also requires the operator to have named the same id in
    /// `FLY_ACCEPT_ADAPTERS` for that deploy -- so listing an id here never migrates a live run
    /// on its own.
    fn migrates_from(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether semantic rewards are enabled for this cartridge. An adapter that
    /// says no must still sample without paying anything, so the stream keeps
    /// running with a visible "rewards off" mode.
    fn rom_allowed(&self, sha256: &str) -> bool;

    /// Sample game state after one completed frame and return this frame's
    /// payouts. `ms` is the brain clock, recorded on each event.
    fn sample(&mut self, memory: &mut dyn MemoryReader, ms: f64) -> Vec<RewardEvent>;

    /// Short human-readable state for the page: `BOOT`, `OVERWORLD`, `BATTLE`…
    fn mode(&self) -> &str;

    /// Whether the readout should use its permissive boot variant, which is what
    /// lets Start and Select fire on a title screen.
    ///
    /// The default is the Pokémon rule the sim loop used before this existed:
    /// exactly the `BOOT` mode string. An adapter with several non-playable
    /// states overrides it — the platformer's `DEMO`, `GAME OVER` and
    /// `TRANSITION` all need Start available (`docs/design/platformer.md` §4).
    fn boot(&self) -> bool {
        self.mode() == "BOOT"
    }

    /// Id of the area the player is in, when the adapter knows one.
    ///
    /// `docs/feed-protocol.md` carries this as `game.map` (`number | null`). It is an accessor
    /// over state the adapter already tracks, not a new observation: the default returns `None`
    /// for an adapter with no notion of an area.
    fn map_id(&self) -> Option<u32> {
        None
    }

    /// Where the player is: the area id and the tile coordinates within it.
    ///
    /// An accessor over state the adapter already samples, like [`GameAdapter::map_id`]: it is not
    /// a new observation and it reaches nothing but the sim loop, which compares it with the
    /// previous frame's to decide whether the button it is holding moved the player at all (the
    /// readout's blocked-direction cooldown, `docs/readout.md`). The default returns `None` for
    /// an adapter with no notion of a location, which switches the rule off.
    ///
    /// The area is part of it because the coordinates alone are not enough: a warp very often
    /// lands on the coordinates it left -- Red's staircase is (7, 1) on both floors -- so a
    /// caller watching only `(x, y)` would read a change of room as standing still.
    fn location(&self) -> Option<(u32, u32, u32)> {
        None
    }

    fn progress(&self) -> ProgressSnapshot;

    /// The ratchet's "safe observation" gate: true only when the game is in a
    /// state a snapshot can be restored into without landing mid-script.
    fn safe_for_snapshot(&self) -> bool;

    /// Whether the run has just ended in a way that discards it: a platformer
    /// game over. The ratchet restores immediately when this is true and the
    /// game's [`RecoveryPolicy`] enables the trigger.
    ///
    /// The default is false, which is Pokémon's behaviour: a blackout there costs
    /// money and teleports the player, it does not end the run.
    fn game_over(&self) -> bool {
        false
    }

    /// Recovery limits and triggers for this game. The default is the Pokémon
    /// policy (120 s stall, 180 s cooldown, 3 attempts per rank, 36 lifetime for
    /// the 38-rung ladder, no game-over trigger), so an adapter that ignores this
    /// is unaffected.
    fn recovery_policy(&self) -> RecoveryPolicy {
        RecoveryPolicy::default()
    }

    /// Provenance of the semantics a checkpoint was earned under, for the last
    /// segment of the compatibility string ([`crate::Compatibility`]).
    ///
    /// The default is the pokered commit, which is what the sim loop passed for
    /// every game before this existed, so Pokémon Red's compatibility string is
    /// unchanged to the byte and its checkpoints keep loading. An adapter whose
    /// symbols come from somewhere else returns its own, which makes a
    /// checkpoint from one game structurally unloadable under the other —
    /// `docs/design/platformer.md` §8.5 asks for exactly that, so that a ROM
    /// revision or an adapter version cannot be swapped underneath a run.
    fn symbol_provenance(&self) -> String {
        crate::pokemon_red::symbols::POKERED_COMMIT.to_string()
    }

    /// Whether this run has already been through `exit`, from the adapter's own exploration
    /// ledger.
    ///
    /// A **read** accessor: it takes `&self`, records nothing and pays nothing, so adding it
    /// leaves [`GameAdapter::id`] and the checkpoint compatibility string untouched. The macro
    /// palette's `GO EXIT` is its only caller (`docs/design/macros.md` section 3: "visit sets for
    /// `GO EXIT` are the adapter's existing per-map exploration ledger").
    ///
    /// The default is `false` — every exit unvisited — for an adapter that keeps no such ledger.
    /// That is the honest default rather than a convenient one: it makes `GO EXIT` prefer nothing,
    /// which is what it did before this existed.
    fn exit_visited(&self, exit: MapExit) -> bool {
        let _ = exit;
        false
    }

    /// Whether this run has already stood on `tile`, from the adapter's own exploration ledger.
    ///
    /// A **read** accessor, exactly as [`GameAdapter::exit_visited`] is: `&self`, records nothing,
    /// pays nothing, and so leaves [`GameAdapter::id`] and the checkpoint compatibility string
    /// untouched. `GO FRONTIER` is its only caller (`docs/design/macros.md` section 9).
    ///
    /// The default is `false` -- no ground covered -- for an adapter with no such ledger, which
    /// makes every tile frontier and `GO FRONTIER` a walk to the nearest reachable tile.
    fn tile_visited(&self, tile: MapTile) -> bool {
        let _ = tile;
        false
    }

    /// Whether this run has ever been on `map`, from the same ledger.
    ///
    /// `GO ROUTE` prefers "a door into a building whose interior is unvisited"
    /// (`docs/design/macros.md` section 9.1), and that is this question. Read-only like the other
    /// two; the default is `false`, i.e. every interior unvisited.
    fn map_visited(&self, map: u8) -> bool {
        let _ = map;
        false
    }

    /// Where the ladder's next unreached rung is, when the adapter's rung catalog knows.
    ///
    /// The one place map knowledge crosses from the adapter to the macro layer, and it crosses as
    /// a *place* rather than as a route: `GO OBJECTIVE` still has to find its own way there over
    /// the warp and collision data it can see, and gives the buttons back when it cannot
    /// (`docs/design/macros.md` section 9). Read-only, and `None` both for an adapter with no
    /// ladder places and for a rung whose place is not derivable -- in which case the plan falls
    /// through to its next entry rather than inventing one.
    fn objective(&self) -> Option<MapPlace> {
        None
    }

    /// Which rung of [`GameAdapter::rank_ladder`] the fly is trying for, when the adapter knows.
    ///
    /// The lowest rung this run has *not* earned, which is not `rank + 1`: the rank is the maximum
    /// over satisfied rungs, so a rung earned out of order carries it past every rung skipped on
    /// the way (`docs/design/macros.md` section 12.4). The header's `milestone.next` is this label,
    /// because a fly walking two maps south to deliver a parcel while the screen says "→ VIRIDIAN
    /// FOREST" is a screen that is lying about what the fly is doing.
    ///
    /// `None` for an adapter with no ladder, and at the top of one.
    fn next_rung(&self) -> Option<u32> {
        None
    }

    /// Labels for ranks `0..=rank_max`, so the page never hardcodes a ladder.
    fn rank_ladder(&self) -> &'static [&'static str];

    fn decoder_preset(&self) -> DecoderPresetId;

    /// Drop observations that a rollback invalidates, keeping lifetime history.
    fn clear_transient(&mut self);

    fn export_state(&self) -> Value;

    /// Restore lifetime history. Incompatible schema versions rebaseline
    /// silently (returning `Ok`); malformed state is an error.
    fn import_state(&mut self, state: &Value) -> Result<(), AdapterError>;
}

/// Build the adapter for a game id: `pokemon-red` or `platformer`.
///
/// The platformer adapter comes back with no ROM pin, so it reports
/// `SEMANTIC REWARDS OFF` for every cartridge. Use [`adapter_for_with_rom_pin`]
/// to supply the hash from `[game.platformer] rom_sha256`.
pub fn adapter_for(game: &str) -> Option<Box<dyn GameAdapter>> {
    adapter_for_with_rom_pin(game, None)
}

/// [`adapter_for`], with the configured cartridge hash for games whose ROM pin
/// is configuration rather than a build constant.
///
/// Only the platformer has one: the design pins Super Mario Land (World) (Rev A)
/// but records only its SHA-1, so the SHA-256 arrives from the config file
/// (`docs/design/platformer.md` §8.5). Pokémon Red's pin stays a constant and
/// `rom_pin` is ignored for it.
pub fn adapter_for_with_rom_pin(game: &str, rom_pin: Option<&str>) -> Option<Box<dyn GameAdapter>> {
    match game {
        "pokemon-red" => Some(Box::new(crate::pokemon_red::PokemonRedReward::new())),
        "platformer" => Some(Box::new(crate::platformer::PlatformerAdapter::with_rom_pin(
            rom_pin,
        ))),
        _ => None,
    }
}

/// Every game id [`adapter_for`] accepts.
pub const GAME_IDS: &[&str] = &["pokemon-red", "platformer"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_game_id_builds() {
        for id in GAME_IDS {
            assert!(adapter_for(id).is_some(), "{id}");
        }
        assert!(adapter_for("sonic").is_none());
    }

    #[test]
    fn the_default_trait_methods_preserve_the_pokemon_behaviour() {
        let adapter = adapter_for("pokemon-red").unwrap();
        assert_eq!(adapter.mode(), "BOOT");
        assert!(adapter.boot(), "the default boot rule is the BOOT mode string");
        assert!(!adapter.game_over(), "Pokémon has no game-over trigger");
        assert_eq!(adapter.recovery_policy(), RecoveryPolicy::default());
        assert_eq!(adapter.recovery_policy().game_over_cooldown_ms, None);
        assert_eq!(
            adapter.symbol_provenance(),
            crate::pokemon_red::symbols::POKERED_COMMIT,
            "the compatibility string must not move under existing checkpoints"
        );
    }

    #[test]
    fn the_two_games_cannot_share_a_checkpoint() {
        let pokemon = adapter_for("pokemon-red").unwrap();
        let platformer =
            adapter_for_with_rom_pin("platformer", Some(&"a".repeat(64))).unwrap();
        assert_ne!(pokemon.id(), platformer.id());
        assert!(
            !platformer.migrates_from().contains(&pokemon.id()),
            "a migration never crosses games"
        );
        assert_ne!(pokemon.symbol_provenance(), platformer.symbol_provenance());
        // And two ROM revisions of the same game cannot either.
        let other = adapter_for_with_rom_pin("platformer", Some(&"b".repeat(64))).unwrap();
        assert_ne!(platformer.symbol_provenance(), other.symbol_provenance());
    }

    #[test]
    fn a_rank_ladder_covers_every_rank_the_adapter_can_report() {
        for id in GAME_IDS {
            let adapter = adapter_for(id).unwrap();
            let snapshot = adapter.progress();
            assert_eq!(
                adapter.rank_ladder().len(),
                snapshot.rank_max as usize + 1,
                "{id}"
            );
            assert!(snapshot.rank <= snapshot.rank_max, "{id}");
        }
    }
}
