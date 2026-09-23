//! The Pokémon Red reward adapter, `pokered-unique8-v7`.
//!
//! A port of the prototype's `src/reward/pokemon-red.ts`. The gates and budgets
//! are unchanged; `docs/rewards-learning.md` holds the live rule table and the
//! source evidence behind each gate. v4 replaced the 16-rung boot-to-badges
//! ladder with the 38 rungs of `docs/design/ladder.md`; v5 adds one reward rule,
//! `boundary` (`docs/design/room-escape.md` section 2), which pays the first step
//! next to and the first step onto each of a map's exits; v6 adds `catch`, the
//! operator's decision of 2026-09-22, which pays for keeping a wild Pokémon; v7 adds
//! `talk` and `item` and stops `boundary` paying indoors, the operator's decision of
//! 2026-09-23 to pay for engaging with a building rather than for leaving it.

pub mod catalog;
pub mod engage;
#[cfg(test)]
pub(crate) mod fake_wram;
pub mod macros;
pub mod mapgrid;
pub mod maps;
pub mod scene;
pub mod state;
pub mod symbols;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::adapter::{
    AdapterError, DecoderPresetId, GameAdapter, MapEdge, MapExit, MapPlace, MapTile, MemoryReader,
    ProgressSnapshot, RewardEvent,
};
use crate::ordered::OrderedSet;
use catalog::{Counts, LastEvents, kind};
use symbols::ram;

/// Adapter version, pinned into the checkpoint compatibility string.
///
/// `v7` is the engagement rules: `talk` and `item` pay, and `boundary` stops paying on an
/// indoor map (the operator, 2026-09-23). Bumping it is what makes a `v6` checkpoint a
/// decision rather than an accident: the compatibility string is compared whole before a
/// restore is attempted, so a `v6` run is refused by default and resumed only when the
/// operator names it in `FLY_ACCEPT_ADAPTERS` ([`crate::compatibility::RestoreDecision`],
/// `docs/design/flysim.md`). That migration is safe in one direction only, and only for this
/// pair: `v6`'s ledger is a `v7` ledger holding no `talk:`, `item:` or `hidden:` keys, and the
/// first sample after the restore seeds the item keys from the cartridge's own bits, so
/// nothing already picked up pays ([`engage::ItemFlags::seed`]).
///
/// (`v6` was the `catch` rule and migrated `v5` the same way: an absent counter reads as
/// zero. `v5` was the `boundary` rule, and rejected `v4` because a ledger that had never
/// recorded a `boundary:` key could not be resumed as though its exits were already
/// collected. `v4` was the 38-rung ladder, and rejected `v3` because a stored rank
/// that meant "4 badges" on the old ladder is not a rung on the new one. Neither of
/// those is a migration; the last two are, because nothing an older ledger holds means
/// something different under the newer rules.)
pub const REWARD_ADAPTER: &str = "pokered-unique8-v7";

/// Adapter ids whose checkpoints `v7` can read.
///
/// Exactly one, and it is one because the engagement rules add ledger keys and change nothing
/// else a `v6` state holds: every field keeps its name, shape and meaning, the `talk:` keys start
/// empty (no conversation was ever paid), and the `item:`/`hidden:` keys are seeded from the
/// game's own flags on the first sample, so no pickup made under `v6` pays when a rollback
/// un-takes it. The `boundary:` keys a `v6` run earned indoors stay in the ledger and mean what
/// they meant -- `exit_visited` still reads them. `v5` is not here: the live run is `v6`, and
/// the one migration the operator asked for is the one this adapter tests. `v4` and `v3` stay
/// refused for the reasons [`REWARD_ADAPTER`] gives.
///
/// Listing an id here is necessary but not sufficient: `FLY_ACCEPT_ADAPTERS` must name it too
/// (`crate::compatibility::decide`, `docs/design/flysim.md`).
pub const MIGRATES_FROM: &[&str] = &["pokered-unique8-v6"];

/// The only cartridge semantic rewards are enabled for. Even the canonical
/// pret build stays disabled until reviewed; see `docs/rewards-learning.md`.
pub const SUPPORTED_ROM: &str =
    "0e853043e15e0f7dd150f297be5557be9890884bff3df1e9bbd7b3e1caef219f";

/// Schema version of [`PokemonRedReward::export_state`]. Anything older cannot
/// be credited under the current rules and rebaselines instead.
///
/// Bumped to 4 with the ladder even though no field changed shape: `progress` is
/// carried in the state, and a v3 `progress` is a rank on a different ladder.
/// Rebaselining costs one overworld sample; reading it as a v4 rank would put the
/// stream on the wrong rung until the fly next climbed.
///
/// *Not* bumped for the `boundary` rule. Its ledger keys live in the existing
/// `seen` array, so a v4 state is structurally a valid v5 state and means the same
/// thing field for field: every key it holds was earned under a rule that still
/// exists, and the exits it does not hold are exits the fly has not been paid for.
/// [`REWARD_ADAPTER`] is the gate that refuses such a checkpoint anyway, and it is
/// the right gate, because the objection to loading one is about semantics rather
/// than shape.
///
/// *Not* bumped for the `catch` rule either, and this time the answer matters,
/// because `v5` checkpoints are meant to be restorable under `v6`. The rule adds one
/// counter, `catchCounts`, and nothing else: every other field keeps its name, its
/// shape and its meaning, and a state written without the counter restores with it
/// empty, which is the truth about a run that was never paid for a catch. That is the
/// whole of the documented `v5` -> `v6` migration; see
/// [`crate::compatibility::RestoreDecision`].
///
/// *Not* bumped for the engagement rules either. `talk` and `item` key their payouts into the
/// existing `seen` array, the way `boundary` did, and the item seed is marked there too
/// ([`ITEMS_SEEDED`]); a `v6` state is structurally a `v7` state with none of those keys.
pub const STATE_VERSION: u64 = 4;

/// The `seen` key that says the item keys have been seeded from the cartridge's flags.
///
/// Absent from every `v6` state and from a fresh adapter; the first playable sample that finds
/// it absent writes one `item:`/`hidden:` key per item the game already shows as taken, pays
/// nothing for any of them, and writes this.
const ITEMS_SEEDED: &str = "items:seeded";

/// Catch payouts one species may earn in the lifetime of a run's ledger.
///
/// The same cap and the same reason as the wild-KO rule's three: a species the fly can
/// find over and over is a farm, and three is enough for the behaviour to be learned.
const MAX_CATCH_PAYOUTS: u64 = 3;

const BADGE_NAMES: [&str; 8] = [
    "BOULDER", "CASCADE", "THUNDER", "RAINBOW", "SOUL", "MARSH", "VOLCANO", "EARTH",
];

/// The milestone ladder, ranks 0 to 37 (`docs/design/ladder.md`).
///
/// Short because the stream's rung panel draws them in Press Start 2P at 33 px in
/// a 1012 px rail. Index is the rank; [`RUNGS`] holds the condition for each one.
pub const RANK_LADDER: [&str; 38] = [
    "BOOT",
    "BEDROOM",
    "DOWNSTAIRS",
    "PALLET TOWN",
    "OAK'S LAB",
    "GOT A STARTER",
    "OAK'S PARCEL",
    "POKEDEX",
    "VIRIDIAN CITY",
    "VIRIDIAN FOREST",
    "PEWTER CITY",
    "BOULDER BADGE",
    "MT. MOON",
    "CERULEAN CITY",
    "CASCADE BADGE",
    "NUGGET BRIDGE",
    "MET BILL",
    "VERMILION CITY",
    "HM CUT",
    "THUNDER BADGE",
    "ROCK TUNNEL",
    "LAVENDER TOWN",
    "CELADON CITY",
    "SILPH SCOPE",
    "RAINBOW BADGE",
    "POKE FLUTE",
    "FUCHSIA CITY",
    "SOUL BADGE",
    "SILPH CO. FREED",
    "MARSH BADGE",
    "CINNABAR ISLAND",
    "VOLCANO BADGE",
    "EARTH BADGE",
    "INDIGO PLATEAU",
    "BEAT LORELEI",
    "BEAT BRUNO",
    "BEAT AGATHA",
    "CHAMPION",
];

use maps::{PALLET_TOWN, REDS_HOUSE_1F};

/// Where each rung of [`RANK_LADDER`] is earned, for `GO OBJECTIVE`.
///
/// `docs/design/macros.md` section 9: "`GO OBJECTIVE` paths toward the next unreached ladder
/// rung's place where the catalog knows one (map id and, when known, a tile or warp) ... and
/// leave it unknown where it is not, in which case the plan falls through." Section 12 keeps the
/// places unchanged when the ranking goes away: a macro that knows where it is going is exactly
/// the "knowledge lives inside macros" the simplification asks for.
///
/// A place is a map, and where the game makes one derivable, an edge of that map. Tiles are still
/// absent everywhere: a tile would have to come from the map's `object_event`/`warp_event` data,
/// which is in a ROM bank this crate cannot reach, and `docs/design/ladder.md`'s own rule is that
/// a number nobody verified against the decomp does not go in. An edge needs no such number --
/// the connection bits are in WRAM -- which is why Oak's trigger can be named at all.
///
/// Where a rung is a badge or a flag inside a building this table names the building when its id
/// is one [`maps`] derives from a verified anchor (the three gyms the ladder's first half needs),
/// and the *town* otherwise: being in Celadon is the observable step toward a Rocket hideout the
/// fly cannot be routed into, and it costs nothing once the fly is there, because a place the fly
/// is standing on falls through to the next entry. Two rungs are conditional rather than constant
/// and live in [`PokemonRedReward::rung_place`], because the game moves them: Oak's own script and
/// the parcel.
const RUNG_PLACES: [Option<MapPlace>; RANK_LADDER.len()] = {
    const fn at(map: u8) -> Option<MapPlace> {
        Some(MapPlace::at(map))
    }
    /// A rung earned by talking to somebody on this map.
    const fn person(map: u8) -> Option<MapPlace> {
        Some(MapPlace::person(map))
    }
    /// A rung earned by pressing A at something on this map.
    const fn object(map: u8) -> Option<MapPlace> {
        Some(MapPlace::object(map))
    }
    [
        None,                             // 0 BOOT: nowhere.
        at(maps::REDS_HOUSE_2F),          // 1 BEDROOM
        at(maps::REDS_HOUSE_1F),          // 2 DOWNSTAIRS
        at(maps::PALLET_TOWN),            // 3 PALLET TOWN
        at(maps::OAKS_LAB),               // 4 OAK'S LAB (until Oak's script: see `rung_place`)
        // 5 GOT A STARTER: a Pokéball on a table, which is an object and not a person.
        object(maps::OAKS_LAB),
        // 6 OAK'S PARCEL, delivered to Oak: a conversation, so the place is the person (mart
        // first, see `rung_place`).
        person(maps::OAKS_LAB),
        // 7 POKEDEX, out of the same script, so the same person.
        person(maps::OAKS_LAB),
        at(maps::VIRIDIAN_CITY),          // 8 VIRIDIAN CITY
        at(maps::VIRIDIAN_FOREST),        // 9 VIRIDIAN FOREST
        at(maps::PEWTER_CITY),            // 10 PEWTER CITY
        // 11 BOULDER BADGE: a badge is the gym leader, who is a person.
        person(maps::PEWTER_GYM),
        at(maps::MT_MOON_1F),             // 12 MT. MOON
        at(maps::CERULEAN_CITY),          // 13 CERULEAN CITY
        person(maps::CERULEAN_GYM),       // 14 CASCADE BADGE, likewise
        at(maps::ROUTE_24),               // 15 NUGGET BRIDGE, north out of Cerulean
        at(maps::ROUTE_25),               // 16 MET BILL, whose house is on Route 25
        at(maps::VERMILION_CITY),         // 17 VERMILION CITY
        at(maps::VERMILION_CITY),         // 18 HM CUT, on the S.S. Anne at Vermilion's dock
        None,                             // 19 THUNDER BADGE: Vermilion Gym, id not derived
        at(maps::ROCK_TUNNEL_1F),         // 20 ROCK TUNNEL
        at(maps::LAVENDER_TOWN),          // 21 LAVENDER TOWN
        at(maps::CELADON_CITY),           // 22 CELADON CITY
        at(maps::CELADON_CITY),           // 23 SILPH SCOPE, in the hideout under the Game Corner
        at(maps::CELADON_CITY),           // 24 RAINBOW BADGE: Celadon Gym, id not derived
        at(maps::LAVENDER_TOWN),          // 25 POKE FLUTE, from Mr. Fuji in Lavender
        at(maps::FUCHSIA_CITY),           // 26 FUCHSIA CITY
        at(maps::FUCHSIA_CITY),           // 27 SOUL BADGE: Fuchsia Gym, id not derived
        at(maps::SAFFRON_CITY),           // 28 SILPH CO. FREED
        at(maps::SAFFRON_CITY),           // 29 MARSH BADGE: Saffron Gym, id not derived
        at(maps::CINNABAR_ISLAND),        // 30 CINNABAR ISLAND
        at(maps::CINNABAR_ISLAND),        // 31 VOLCANO BADGE: Cinnabar Gym, id not derived
        at(maps::VIRIDIAN_GYM),           // 32 EARTH BADGE, back at Viridian
        at(maps::INDIGO_PLATEAU_LOBBY),   // 33 INDIGO PLATEAU
        at(maps::INDIGO_PLATEAU_LOBBY),   // 34 BEAT LORELEI, through the lobby
        at(maps::INDIGO_PLATEAU_LOBBY),   // 35 BEAT BRUNO
        at(maps::INDIGO_PLATEAU_LOBBY),   // 36 BEAT AGATHA
        at(maps::INDIGO_PLATEAU_LOBBY),   // 37 CHAMPION
    ]
};

/// Bytes per entry in the current map's warp table.
///
/// `wWarpEntries:: ds MAX_WARP_EVENTS * 4 ; Y, X, warp ID, map ID` (`ram/wram.asm` at
/// [`symbols::POKERED_COMMIT`]). Only the first two are read here: the destination is where the
/// door goes, and the reward is for finding the door.
const WARP_ENTRY_BYTES: u16 = 4;

/// `DEF MAX_WARP_EVENTS EQU 32` (`constants/map_data_constants.asm`). `wNumberOfWarps` is clamped
/// to it before the table is walked, so a garbage count cannot read past the array.
const MAX_WARP_EVENTS: u8 = 32;

/// The label every `boundary` payout carries (`docs/design/room-escape.md` section 2), uppercased
/// like every other label this adapter emits. Adjacent and on-exit share it deliberately: they are
/// one rule, and "found an exit" is the true thing to put on the screen for both.
const BOUNDARY_LABEL: &str = "FOUND AN EXIT";

/// `wCurMapConnections` bits: `shift_const EAST, WEST, SOUTH, NORTH` in
/// `constants/map_data_constants.asm`, i.e. 1, 2, 4 and 8.
mod connection {
    pub const EAST: u8 = 1;
    pub const WEST: u8 = 2;
    pub const SOUTH: u8 = 4;
    pub const NORTH: u8 = 8;
}

/// What earns one rung.
///
/// Every variant is evaluated against *lifetime* state, never against the current
/// frame, so no rung can be lost once earned — see [`PokemonRedReward::rank_from`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    /// Rank 0: the ladder's floor, satisfied by nothing.
    Boot,
    /// The map id has entered the lifetime map ledger (first visit, standing still
    /// in the unscripted overworld -- not the current position).
    Visited(u8),
    /// At least this many bits set in `wObtainedBadges`, ever.
    Badges(u32),
    /// This event flag has been observed set at least once.
    Flag(&'static str),
    /// Entered into the Hall of Fame: `EVENT_BEAT_CHAMPION_RIVAL` was seen, or the
    /// cartridge's own durable `wNumHoFTeams` counter is non-zero. Needs both halves
    /// because the Hall of Fame script clears the flag; see
    /// [`PokemonRedReward::rank_from`].
    HallOfFame,
}

/// The condition for each rank, parallel to [`RANK_LADDER`].
///
/// Flag rungs name the flag by its pokered symbol, which is also the key the
/// adapter's lifetime `seen` ledger uses, so a rung is a ledger lookup.
const RUNGS: [Rung; RANK_LADDER.len()] = [
    Rung::Boot,
    Rung::Visited(maps::REDS_HOUSE_2F),
    Rung::Visited(maps::REDS_HOUSE_1F),
    Rung::Visited(maps::PALLET_TOWN),
    Rung::Visited(maps::OAKS_LAB),
    Rung::Flag("EVENT_GOT_STARTER"),
    // The parcel *delivered*, not merely collected: it is what unlocks the Pokédex,
    // so it sits directly below rung 7. EVENT_GOT_OAKS_PARCEL is the earlier beat
    // and is implied by Viridian City (rung 8) anyway.
    Rung::Flag("EVENT_OAK_GOT_PARCEL"),
    Rung::Flag("EVENT_GOT_POKEDEX"),
    Rung::Visited(maps::VIRIDIAN_CITY),
    Rung::Visited(maps::VIRIDIAN_FOREST),
    Rung::Visited(maps::PEWTER_CITY),
    Rung::Badges(1),
    Rung::Visited(maps::MT_MOON_1F),
    Rung::Visited(maps::CERULEAN_CITY),
    Rung::Badges(2),
    Rung::Flag("EVENT_BEAT_CERULEAN_RIVAL"),
    Rung::Flag("EVENT_GOT_SS_TICKET"),
    Rung::Visited(maps::VERMILION_CITY),
    Rung::Flag("EVENT_GOT_HM01"),
    Rung::Badges(3),
    Rung::Visited(maps::ROCK_TUNNEL_1F),
    Rung::Visited(maps::LAVENDER_TOWN),
    Rung::Visited(maps::CELADON_CITY),
    // There is no EVENT_GOT_SILPH_SCOPE: the Scope is a toggleable pick-up that
    // RocketHideoutB4FBeatGiovanniScript reveals, so beating Giovanni in the
    // hideout is the flag that means "the Scope is obtainable".
    Rung::Flag("EVENT_BEAT_ROCKET_HIDEOUT_GIOVANNI"),
    Rung::Badges(4),
    Rung::Flag("EVENT_GOT_POKE_FLUTE"),
    Rung::Visited(maps::FUCHSIA_CITY),
    Rung::Badges(5),
    Rung::Flag("EVENT_BEAT_SILPH_CO_GIOVANNI"),
    Rung::Badges(6),
    Rung::Visited(maps::CINNABAR_ISLAND),
    Rung::Badges(7),
    Rung::Badges(8),
    Rung::Visited(maps::INDIGO_PLATEAU_LOBBY),
    Rung::Flag("EVENT_BEAT_LORELEIS_ROOM_TRAINER_0"),
    Rung::Flag("EVENT_BEAT_BRUNOS_ROOM_TRAINER_0"),
    Rung::Flag("EVENT_BEAT_AGATHAS_ROOM_TRAINER_0"),
    Rung::HallOfFame,
];

/// Where one rung of the ladder is earned, [`RUNG_PLACES`] plus the two places the *game*
/// moves.
///
/// Both are read from the lifetime `seen` ledger, which is the same evidence the rung
/// conditions themselves use, so neither is a guess about the script's state:
///
/// - **rungs 4 and 5 are at the town's north exit until Oak's script has fired.** Oak stops
///   the fly on the path out of Pallet Town and walks it into the lab; before that the lab
///   holds nothing to take, and aiming at it walks the fly to a door it will be carried
///   through anyway. `EVENT_FOLLOWED_OAK_INTO_LAB` is the moment it has happened.
/// - **rung 6 is Viridian's mart until the parcel is in the bag.** The rung is the parcel
///   *delivered* (`docs/design/ladder.md`), and the delivery is at the lab, but the errand
///   starts at the mart counter two maps north. `EVENT_GOT_OAKS_PARCEL` is the half-way
///   point, and without this the catalog answers "the lab" to a fly that is standing in the
///   lab with nothing to hand over -- which is the town loop this method was written for
///   (`infra/docs/macros-bench.md`, the plan hour on rung 5).
fn rung_place(seen: &OrderedSet, rung: usize) -> Option<MapPlace> {
    match rung {
        4 | 5 if !seen.contains("EVENT_FOLLOWED_OAK_INTO_LAB") => {
            Some(MapPlace::edge(maps::PALLET_TOWN, MapEdge::North))
        }
        6 if !seen.contains("EVENT_GOT_OAKS_PARCEL") => {
            Some(MapPlace::at(maps::VIRIDIAN_MART))
        }
        _ => RUNG_PLACES.get(rung).copied().flatten(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Battle {
    key: String,
    wild: bool,
    saw_living: bool,
    ko: bool,
    /// Lifetime `species` payouts when this battle started.
    ///
    /// The "never owned this run" test for the catch rule, and an exact one: the only
    /// thing that can set a `wPokedexOwned` bit during a wild battle is the catch
    /// itself, so a `species` payout between the battle starting and the ball keeping
    /// the Pokémon *is* that Pokémon being new. It is read this way rather than from
    /// `wCapturedMonSpecies` directly because that byte is the cartridge's **internal**
    /// species index and the owned bitset is by **Pokédex number**; the two numberings
    /// differ and nothing in WRAM converts between them
    /// (`docs/design/macros-wram.md` section 2, "species numbering").
    ///
    /// `None` for a battle restored from a checkpoint written before this existed,
    /// which reads as "cannot tell" and pays the repeat amount rather than guessing
    /// generously.
    species_at_start: Option<u64>,
    /// The internal species index `wCapturedMonSpecies` named, once a ball has kept one.
    captured: Option<u8>,
    /// Whether that catch was a species this run had never owned, decided on the frame
    /// the capture was observed.
    captured_new: bool,
}

/// Immutable per-sample byte cache. Each address requested during one sample
/// crosses the FFI boundary at most once, across all flag and bit loops
/// including the 19 Pokédex bytes.
struct SampleCache<'a> {
    source: &'a mut dyn MemoryReader,
    bytes: HashMap<u16, u8>,
}

impl MemoryReader for SampleCache<'_> {
    fn read8(&mut self, address: u16) -> u8 {
        if let Some(value) = self.bytes.get(&address) {
            return *value;
        }
        let value = self.source.read8(address);
        self.bytes.insert(address, value);
        value
    }

    /// Straight through, uncached: a ROM byte cannot change, so there is nothing
    /// for a per-sample cache to save, and the caller that reads a blockset
    /// (`docs/design/macros.md` section 15) caches the decoded map instead.
    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        self.source.read_rom(bank, address)
    }
}

fn word(memory: &mut impl MemoryReader, address: u16) -> u32 {
    let high = u32::from(memory.read8(address));
    high * 256 + u32::from(memory.read8(address + 1))
}

fn flag(memory: &mut impl MemoryReader, bit: u16) -> bool {
    memory.read8(ram::wEventFlags + (bit >> 3)) & (1 << (bit & 7)) != 0
}

pub struct PokemonRedReward {
    rom_hash: String,
    milestones: HashSet<&'static str>,

    seen: OrderedSet,
    tiles: OrderedSet,
    tile_counts: BTreeMap<u8, u64>,
    wild_wins: BTreeMap<String, u64>,
    /// Catch payouts per species, by the cartridge's internal species index as a decimal
    /// string. The one field `v6` adds to the checkpoint; absent in a `v5` state, which
    /// reads as every species at zero.
    catch_counts: BTreeMap<String, u64>,
    replay_blocked: OrderedSet,
    counts: Counts,
    total: f64,
    recent: Vec<RewardEvent>,
    last: LastEvents,
    initialized: bool,
    saw_boot: bool,
    location: String,
    stable: u64,
    battle: Option<Battle>,
    mode: String,

    /// The `talk` rule's frame-to-frame watch. Transient: never checkpointed, cleared by a
    /// rollback and by a restore.
    talk: engage::TalkWatch,
    /// The item bitsets as of the last playable sample, so a pickup is a bit that *rose*.
    /// Transient for the same reason.
    item_flags: Option<engage::ItemFlags>,

    /// Transient, recomputed every sample and never checkpointed.
    safe: bool,
    progress: u32,
    badges: u32,
}

/// What the stream page shows about reward history.
#[derive(Debug, Clone, PartialEq)]
pub struct Statistics {
    pub counts: BTreeMap<&'static str, u64>,
    pub total: f64,
    pub recent: Vec<RewardEvent>,
    pub last: Vec<(&'static str, RewardEvent)>,
    pub mode: String,
    /// Legacy name for unique player-coordinate locations, kept because the
    /// prototype's protocol field is called `uniqueTiles`.
    pub unique_tiles: usize,
}

impl Default for PokemonRedReward {
    fn default() -> Self {
        Self::new()
    }
}

impl PokemonRedReward {
    pub fn new() -> Self {
        Self::with_rom_hash(SUPPORTED_ROM)
    }

    pub fn with_rom_hash(rom_hash: &str) -> Self {
        Self {
            rom_hash: rom_hash.to_string(),
            milestones: symbols::MILESTONES.into_iter().collect(),
            seen: OrderedSet::new(),
            tiles: OrderedSet::new(),
            tile_counts: BTreeMap::new(),
            wild_wins: BTreeMap::new(),
            catch_counts: BTreeMap::new(),
            replay_blocked: OrderedSet::new(),
            counts: Counts::default(),
            total: 0.0,
            recent: Vec::new(),
            last: LastEvents::default(),
            initialized: false,
            saw_boot: false,
            location: String::new(),
            stable: 0,
            battle: None,
            mode: "BOOT".to_string(),
            talk: engage::TalkWatch::default(),
            item_flags: None,
            safe: false,
            progress: 0,
            badges: 0,
        }
    }

    /// Whether the last sample was a state a snapshot can be restored into:
    /// stable controllable overworld coordinates with no dialogue font loaded.
    pub fn safe(&self) -> bool {
        self.safe
    }

    /// Rank on [`RANK_LADDER`], 0 to 37.
    pub fn rank(&self) -> u32 {
        self.progress
    }

    /// The highest rung whose condition is satisfied, i.e. the max over [`RUNGS`].
    ///
    /// Rungs may be earned out of order — a fly that stumbles into Viridian Forest
    /// before Viridian City still gets the higher rung — and the maximum is what
    /// makes that harmless.
    ///
    /// ## Why every condition reads the ledger, not the cartridge
    ///
    /// `docs/design/ladder.md` assumed "event flags never clear in normal play".
    /// That is false for exactly the rungs at the top of the ladder. In pokered at
    /// [`symbols::POKERED_COMMIT`], `IndigoPlateauLobby_Script` runs
    /// `ResetEventRange INDIGO_PLATEAU_EVENTS_START, EVENT_LANCES_ROOM_LOCK_DOOR`
    /// on every entry to the lobby once the player has started an Elite Four run,
    /// so Lorelei's, Bruno's, Agatha's and Lance's beat flags are all cleared the
    /// moment the fly walks back into the lobby — which is precisely what happens
    /// after a blackout. `HallOfFame.asm` then resets the *whole* range,
    /// `EVENT_BEAT_CHAMPION_RIVAL` included, as the player is entered into the Hall
    /// of Fame: the flag for the ladder's last rung is destroyed by the event that
    /// earns it. Those two `ResetEventRange` calls are the only ones in the game, so
    /// the other eleven flag rungs are genuinely sticky in WRAM.
    ///
    /// Rather than special-case four rungs, every flag rung is answered from the
    /// adapter's lifetime `seen` ledger, which already records each flag under its
    /// pokered name the first sample it is observed set (`sample` walks
    /// [`symbols::EVENTS`] on every playable frame, battles included, and `once`
    /// inserts whether or not it pays). The ledger survives a rollback —
    /// [`PokemonRedReward::clear_transient`] does not touch it — and round-trips
    /// through the checkpoint, so it needs no new state. `wNumHoFTeams` is the
    /// belt-and-braces half of the Champion rung: it is the counter `HallOfFamePC`
    /// increments, it is saved, and it is the only durable "has been Champion"
    /// signal for a cartridge whose ledger starts empty.
    ///
    /// Map rungs read the same ledger's `map:<id>` keys, which are first-visit by
    /// construction: they are only inserted from the unscripted, stable, overworld
    /// branch, so the archive for a map rung is taken standing still a few seconds
    /// after arrival, exactly as the safe-snapshot rules require.
    fn rank_from(&self, badges: u32, hall_of_fame: bool) -> u32 {
        let mut rank = 0;
        for index in 0..RUNGS.len() {
            if self.rung_satisfied(index, badges, hall_of_fame) {
                rank = index as u32;
            }
        }
        rank
    }

    /// Whether one rung's own condition holds, from the lifetime ledger and the two counters.
    ///
    /// Factored out of [`PokemonRedReward::rank_from`] so that "the highest rung satisfied" (the
    /// rank, which is what the stream shows and what the ratchet measures) and "the lowest rung
    /// *not* satisfied" (the objective, which is where the fly should be going) are the same
    /// evidence asked two different ways. They are not the same question:
    /// `docs/design/macros.md` section 12.4 has the save that proved it.
    fn rung_satisfied(&self, rung: usize, badges: u32, hall_of_fame: bool) -> bool {
        let Some(rung) = RUNGS.get(rung) else { return false };
        match rung {
            Rung::Boot => true,
            Rung::Visited(map) => {
                let mut key = String::with_capacity(8);
                let _ = write!(key, "map:{map}");
                self.seen.contains(&key)
            }
            Rung::Badges(count) => badges >= *count,
            Rung::Flag(name) => self.seen.contains(name),
            Rung::HallOfFame => hall_of_fame || self.seen.contains("EVENT_BEAT_CHAMPION_RIVAL"),
        }
    }

    /// The lowest rung of the ladder this run has *not* satisfied.
    ///
    /// Not `rank + 1`. The rank is the maximum over the satisfied rungs, so a rung earned out of
    /// order carries it past every rung skipped on the way — and the ladder's rungs are not a
    /// chain: "VIRIDIAN CITY" is a map the fly can walk to, while the two rungs under it are Oak's
    /// parcel *delivered* and the Pokédex, which are a script two maps south. The live save of
    /// 2026-09-17 was exactly that shape: `EVENT_GOT_OAKS_PARCEL` set and `EVENT_OAK_GOT_PARCEL`
    /// clear, rungs 6 and 7 unearned, rank 8 because Viridian City had been stood on — and
    /// `rank + 1` therefore aimed `GO OBJECTIVE` at Viridian Forest, north, through a gate the
    /// cartridge keeps shut until the parcel is delivered. The fly walked into it once per hold
    /// for eight hours (`infra/docs/macros-traps.md`).
    ///
    /// `None` once every rung is satisfied, which is the champion.
    fn next_unreached(&self, badges: u32, hall_of_fame: bool) -> Option<usize> {
        (0..RUNGS.len()).find(|rung| !self.rung_satisfied(*rung, badges, hall_of_fame))
    }

    /// `wCurMap` as of the last overworld sample, or `None` before one.
    ///
    /// Read back out of `location` (`"<map>:<x>:<y>"`), which is the only place the adapter
    /// keeps it. Cleared by [`PokemonRedReward::clear_transient`], as the coordinates are.
    pub fn map(&self) -> Option<u32> {
        self.location.split(':').next()?.parse().ok()
    }

    /// `(wCurMap, wXCoord, wYCoord)` as of the last overworld sample, or `None` before one.
    ///
    /// Read straight back out of `location` (`"<map>:<x>:<y>"`), the only place the adapter keeps
    /// any of the three, and cleared by [`PokemonRedReward::clear_transient`] with the rest of the
    /// transient state.
    pub fn area_and_tile(&self) -> Option<(u32, u32, u32)> {
        let mut parts = self.location.split(':');
        Some((
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ))
    }

    pub fn statistics(&self) -> Statistics {
        Statistics {
            counts: self.counts.to_map(),
            total: self.total,
            recent: self.recent.clone(),
            last: self.last.iter().map(|(kind, event)| (kind, event.clone())).collect(),
            mode: self.mode.clone(),
            unique_tiles: self.tiles.len(),
        }
    }

    /// Forget observations a rollback invalidates. Lifetime novelty survives,
    /// and every wild-KO key and every caught species paid so far is blocked from
    /// paying again, because after a rollback the same battle -- or the same catch --
    /// could otherwise be replayed for reward.
    pub fn clear_transient(&mut self) {
        self.location.clear();
        self.stable = 0;
        self.battle = None;
        self.safe = false;
        // A conversation or a pickup in flight across a rollback is not paid: the game the
        // fly returns to has not had it. What *was* paid stays in `seen` and blocks a replay.
        self.talk.clear();
        self.item_flags = None;
        let keys: Vec<String> = self.wild_wins.keys().cloned().collect();
        for key in keys {
            self.replay_blocked.insert(&key);
        }
        let caught: Vec<String> = self.catch_counts.keys().cloned().collect();
        for species in caught {
            self.replay_blocked.insert(&format!("catch:{species}"));
        }
    }

    /// Sample WRAM after one completed frame and return this frame's payouts.
    pub fn sample(&mut self, source: &mut dyn MemoryReader, brain_ms: f64) -> Vec<RewardEvent> {
        self.safe = false;
        if self.rom_hash != SUPPORTED_ROM {
            self.mode = "UNSUPPORTED ROM · SEMANTIC REWARDS OFF".to_string();
            return Vec::new();
        }
        let mut cache = SampleCache { source, bytes: HashMap::new() };
        let memory = &mut cache;

        let active = memory.read8(ram::wStatusFlags6) & 1 != 0;
        let map = memory.read8(ram::wCurMap);
        let width = u32::from(memory.read8(ram::wCurMapWidth)) * 2;
        let height = u32::from(memory.read8(ram::wCurMapHeight)) * 2;
        let x = u32::from(memory.read8(ram::wXCoord));
        let y = u32::from(memory.read8(ram::wYCoord));

        if !active {
            self.saw_boot = true;
            self.mode = "BOOT".to_string();
            self.stable = 0;
            self.battle = None;
            return Vec::new();
        }
        if map > 0xf7
            || width == 0
            || height == 0
            || x >= width
            || y >= height
            || memory.read8(ram::wPartyCount) > 6
        {
            self.mode = "TRANSITION".to_string();
            self.stable = 0;
            return Vec::new();
        }
        if memory.read8(ram::wBattleType) != 0 || memory.read8(ram::wStatusFlags7) & 1 != 0 {
            self.mode = "DEMO / SAFARI".to_string();
            self.battle = None;
            self.stable = 0;
            return Vec::new();
        }

        let mut emitted: Vec<RewardEvent> = Vec::new();
        let first = !self.initialized;

        // Persistent bitsets, in pokered declaration order.
        for (name, bit) in symbols::EVENTS {
            if !flag(memory, bit) {
                continue;
            }
            let kind =
                if self.milestones.contains(name) { kind::MILESTONE } else { kind::TRAINER };
            let label = name.strip_prefix("EVENT_").unwrap_or(name).replace('_', " ");
            self.once(&mut emitted, name, kind, label, first, brain_ms);
        }
        for i in 0..151u16 {
            if memory.read8(ram::wPokedexOwned + (i >> 3)) & (1 << (i & 7)) != 0 {
                self.once(
                    &mut emitted,
                    &format!("dex:{i}"),
                    kind::SPECIES,
                    format!("OWNED #{}", i + 1),
                    first,
                    brain_ms,
                );
            }
        }
        for i in 0..8u32 {
            if memory.read8(ram::wObtainedBadges) & (1 << i) != 0 {
                self.once(
                    &mut emitted,
                    &format!("badge:{i}"),
                    kind::BADGE,
                    BADGE_NAMES[i as usize].to_string(),
                    first,
                    brain_ms,
                );
            }
        }
        if first {
            self.seen.insert(&format!("map:{map}"));
            // A migrated save already in an early area is a baseline, not a new
            // arrival.
            if map == REDS_HOUSE_1F {
                self.seen.insert("early:downstairs");
            }
            if map == PALLET_TOWN {
                self.seen.insert("early:outside");
            }
            if self.saw_boot && !self.seen.contains("adventure") {
                self.emit(
                    &mut emitted,
                    kind::MILESTONE,
                    "ADVENTURE STARTED".to_string(),
                    1.0,
                    brain_ms,
                );
            }
            self.seen.insert("adventure");
            // The exits of wherever this sample happens to be, baselined for the same reason the
            // map id above is: a restored save standing in a doorway has not just found it.
            // `first` makes `boundary` write its ledger keys without paying for any of them.
            self.boundary(&mut emitted, memory, map, x, y, width, height, true, brain_ms);
            self.initialized = true;
        }
        self.items(&mut emitted, memory, brain_ms);

        let in_battle = memory.read8(ram::wIsInBattle);
        if in_battle == 1 || in_battle == 2 || in_battle == 255 {
            self.mode = "BATTLE".to_string();
            self.stable = 0;
            self.talk.interrupt();
            let species_paid = self.counts.get(kind::SPECIES);
            if self.battle.is_none() && in_battle != 255 {
                self.battle = Some(Battle {
                    key: battle_key(memory, map),
                    wild: in_battle == 1,
                    saw_living: false,
                    ko: false,
                    species_at_start: Some(species_paid),
                    captured: None,
                    captured_new: false,
                });
            }
            // The cartridge's own answer to "was one caught": `ram/wram.asm`'s comment on
            // this byte is "0 if no mon was captured". `ItemUseBall` zeroes it before every
            // throw and writes `wEnemyMonSpecies` into it only on the branch that keeps the
            // Pokémon, and `UseBagItem`'s `.returnAfterCapturingMon` zeroes it again on the
            // way out of the battle -- so it is non-zero for the hundreds of frames the
            // catch's own text and Pokédex screen take, and zero everywhere else.
            //
            // Read rather than derived from `wPartyCount`, because a catch with a full party
            // raises `wBoxCount` instead, and because `wPartyCount` also rises for a gift, a
            // trade and a revive out of the PC.
            let captured = memory.read8(ram::wCapturedMonSpecies);
            if let Some(battle) = &mut self.battle {
                let hp = word(memory, ram::wEnemyMonHP);
                let max = word(memory, ram::wEnemyMonMaxHP);
                if hp > 0 && hp <= max && max > 0 && max < 1000 {
                    battle.saw_living = true;
                    // Enemy identity isn't initialized at the first frame of
                    // battle entry.
                    battle.key = battle_key(memory, map);
                }
                if battle.saw_living && hp == 0 {
                    battle.ko = true;
                }
                if battle.wild && captured != 0 && battle.captured.is_none() {
                    battle.captured = Some(captured);
                    battle.captured_new =
                        battle.species_at_start.is_some_and(|before| species_paid > before);
                }
            }
        } else if in_battle == 0 {
            self.mode = "OVERWORLD".to_string();
            if let Some(battle) = self.battle.take() {
                let result = memory.read8(ram::wBattleResult);
                if battle.wild && battle.ko && result == 0 {
                    let count = self.wild_wins.get(&battle.key).copied().unwrap_or(0);
                    if count < 3 && !self.replay_blocked.contains(&battle.key) {
                        self.emit(
                            &mut emitted,
                            kind::BATTLE,
                            format!("WILD KO {}", battle.key),
                            1.0 / (count as f64 + 1.0),
                            brain_ms,
                        );
                    }
                    self.wild_wins.insert(battle.key.clone(), (count + 1).min(3));
                }
                // The catch rule (`docs/rewards-learning.md`, the operator 2026-09-22).
                //
                // Paid on the way out of the battle rather than on the capture frame, so that
                // it lands in the same place the wild-KO payout does and cannot fire twice for
                // one battle. `wBattleResult` is 2 on exactly two paths in the game:
                // `UseBagItem`'s `.returnAfterCapturingMon`, which is this one, and a link
                // battle whose opponent ran (`engine/battle/core.asm`), which this cartridge
                // never has. Requiring it as well as the captured species means a byte read
                // out of a half-initialised battle cannot pay.
                if let Some(species) = battle.captured
                    && battle.wild
                    && result == 2
                {
                    let key = species.to_string();
                    let paid = self.catch_counts.get(&key).copied().unwrap_or(0);
                    if paid < MAX_CATCH_PAYOUTS
                        && !self.replay_blocked.contains(&format!("catch:{key}"))
                    {
                        let value = if battle.captured_new {
                            catalog::rule(kind::CATCH)
                                .expect("the catch rule is in the catalog")
                                .value
                        } else {
                            catalog::CATCH_REPEAT_VALUE
                        };
                        self.emit_amount(
                            &mut emitted,
                            kind::CATCH,
                            format!("CAUGHT #{species}"),
                            value,
                            brain_ms,
                        );
                    }
                    self.catch_counts.insert(key, (paid + 1).min(MAX_CATCH_PAYOUTS));
                }
            }
            // The talk rule (`docs/rewards-learning.md`, the operator 2026-09-23): a conversation
            // the fly opened indoors, paid once per (map, object) when its box closes.
            if let Some(conversation) = self.talk.observe(memory, map, x, y) {
                self.once(
                    &mut emitted,
                    &conversation.key(),
                    kind::TALK,
                    conversation.label(),
                    false,
                    brain_ms,
                );
            }
            let location = format!("{map}:{x}:{y}");
            self.stable = if self.location == location { self.stable + 1 } else { 1 };
            self.location = location.clone();
            // Short-circuits exactly as the TypeScript `||` chain did, so a
            // scripted sample stops reading further gate bytes.
            let scripted = memory.read8(ram::wStatusFlags5) & 0xa1 != 0
                || memory.read8(ram::wJoyIgnore) != 0
                || memory.read8(ram::wMovementFlags) & 0xc7 != 0
                || memory.read8(ram::wStatusFlags6) & 0x5c != 0;
            if !scripted && self.stable >= 3 {
                self.safe = memory.read8(ram::wFontLoaded) & 1 == 0;
                if map == REDS_HOUSE_1F {
                    self.once(
                        &mut emitted,
                        "early:downstairs",
                        kind::MILESTONE,
                        "DOWNSTAIRS".to_string(),
                        first,
                        brain_ms,
                    );
                }
                if map == PALLET_TOWN {
                    self.once(
                        &mut emitted,
                        "early:outside",
                        kind::MILESTONE,
                        "OUTSIDE".to_string(),
                        first,
                        brain_ms,
                    );
                }
                self.once(
                    &mut emitted,
                    &format!("map:{map}"),
                    kind::MAP,
                    format!("AREA {map}"),
                    first,
                    brain_ms,
                );
                // After the ledger insert above, so arriving somewhere earns its rung on
                // the same sample it is first recorded on.
                self.badges =
                    self.badges.max(memory.read8(ram::wObtainedBadges).count_ones());
                let hall_of_fame = memory.read8(ram::wNumHoFTeams) > 0;
                self.progress = self.rank_from(self.badges, hall_of_fame);
                if !self.tiles.contains(&location) {
                    self.tiles.insert(&location);
                    let count = self.tile_counts.get(&map).copied().unwrap_or(0) + 1;
                    self.tile_counts.insert(map, count);
                    if count % 8 == 0 && count <= 200 {
                        self.emit(
                            &mut emitted,
                            kind::EXPLORATION,
                            format!("AREA {map}: {count} UNIQUE LOCATIONS"),
                            1.0,
                            brain_ms,
                        );
                    }
                }
            }
            // The boundary rule keeps its own gate, because the state it pays for is a state the
            // gate above deliberately rejects. `wMovementFlags` bits 0, 1 and 2 are
            // BIT_STANDING_ON_DOOR, BIT_EXITING_DOOR and BIT_STANDING_ON_WARP
            // (`constants/ram_constants.asm` at [`symbols::POKERED_COMMIT`]), all three inside
            // that mask's 0xc7 -- and standing on a door is precisely the thing
            // `docs/design/room-escape.md` section 2 wants to pay for, so under the shared gate
            // the on-exit half of the rule could never fire at all. Everything else the gate
            // above rejects is rejected here too: a ledge hop or a spin tile (0xc0), an ignored
            // joypad, and both status-flag masks. Nothing else in the adapter reads this gate, so
            // no other payout, the `safe` flag included, changes behaviour.
            if self.stable >= 3
                && memory.read8(ram::wStatusFlags5) & 0xa1 == 0
                && memory.read8(ram::wJoyIgnore) == 0
                && memory.read8(ram::wMovementFlags) & 0xc0 == 0
                && memory.read8(ram::wStatusFlags6) & 0x5c == 0
            {
                self.boundary(&mut emitted, memory, map, x, y, width, height, first, brain_ms);
            }
        }

        // Newest first, capped at eight, as the prototype's
        // `[...emitted, ...this.recent].slice(0, 8)` did.
        let mut recent = emitted.clone();
        recent.append(&mut self.recent);
        recent.truncate(8);
        self.recent = recent;
        emitted
    }

    fn emit(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        kind: &'static str,
        label: String,
        scale: f64,
        brain_ms: f64,
    ) {
        let rule = catalog::rule(kind).expect("emit is only called with catalog kinds");
        self.emit_amount(emitted, kind, label, rule.value * scale, brain_ms);
    }

    /// [`PokemonRedReward::emit`] with the payout stated outright instead of as a multiple
    /// of the catalog value.
    ///
    /// One rule needs it. `catch` pays 0.30 for a species this run has not caught and 0.10
    /// for one it has, and no binary float scales the first into exactly the second:
    /// `0.3 * (1.0 / 3.0)` is `0.09999999999999999`, and that is the number that would reach
    /// the ticker and the checkpoint. `boundary`'s pair, 0.05 and 0.10, *is* an exact scale
    /// of two, so that rule still goes through [`PokemonRedReward::emit`].
    fn emit_amount(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        kind: &'static str,
        label: String,
        value: f64,
        brain_ms: f64,
    ) {
        let rule = catalog::rule(kind).expect("emit is only called with catalog kinds");
        let event = RewardEvent {
            kind,
            label,
            brain_ms,
            value,
            stimulation_ms: rule.stimulation_ms,
        };
        emitted.push(event.clone());
        self.counts.bump(kind);
        self.total += event.value;
        self.last.set(event);
    }

    fn once(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        key: &str,
        kind: &'static str,
        label: String,
        first: bool,
        brain_ms: f64,
    ) {
        self.once_scaled(emitted, key, kind, label, 1.0, first, brain_ms);
    }

    /// [`PokemonRedReward::once`] with a multiplier on the catalog value, for a rule whose one
    /// kind pays two amounts.
    #[allow(clippy::too_many_arguments)]
    fn once_scaled(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        key: &str,
        kind: &'static str,
        label: String,
        scale: f64,
        first: bool,
        brain_ms: f64,
    ) {
        if self.seen.contains(key) {
            return;
        }
        self.seen.insert(key);
        if !first {
            self.emit(emitted, kind, label, scale, brain_ms);
        }
    }

    /// Pay the first step next to, and the first step onto, each of this map's exits.
    ///
    /// `docs/design/room-escape.md` section 2. Exits come from two places in WRAM, both verified
    /// against the disassembly at [`symbols::POKERED_COMMIT`]:
    ///
    /// - **the warp table.** `wNumberOfWarps` entries of [`WARP_ENTRY_BYTES`] at
    ///   `wWarpEntries`, each `Y, X, destination warp id, destination map id`
    ///   (`ram/wram.asm`'s own comment). `CheckWarpsCollision` and `CheckWarpsNoCollision`
    ///   compare those Y and X against `wYCoord` and `wXCoord` with no conversion, so warp
    ///   coordinates are in exactly the tile space this adapter already samples. Doors, stairs,
    ///   cave mouths and building entrances are all warps.
    /// - **the connected edges.** `wCurMapConnections` is a bitmask over
    ///   [`connection`]. The crossing itself happens one step *outside* the map --
    ///   `CheckMapConnections` fires on `wXCoord == $ff` going west and on
    ///   `wXCoord == wCurrentMapWidth2` going east, and `wCurrentMapWidth2` is `wCurMapWidth`
    ///   doubled, which is this function's `width` -- so the exit tiles are column 0, column
    ///   `width - 1`, row 0 and row `height - 1`, and no further address is needed to find them.
    ///
    /// Adjacent means the four-neighbourhood, Manhattan distance 1. Each `(map, exit)` pays its
    /// adjacent tiles once and its own tile once, for the lifetime of the `seen` ledger, so
    /// stepping on and off a doormat earns nothing the second time and a rollback cannot replay it
    /// (`clear_transient` does not touch the ledger).
    ///
    /// Consequences worth stating rather than discovering:
    ///
    /// - arriving on a map *through* an exit lands on that exit, so the arrival pays it: on a
    ///   connection it is the connection's own edge row, and on a warp it is the warp tile the
    ///   destination put the player on. Measured on the live release run (`infra/docs/
    ///   room-escape.md`): the ledger holds `boundary:37:1:7:on` and `boundary:38:1:7:on`, both
    ///   halves of Red's staircase, paid for arriving down and up it rather than for finding it.
    ///   That is one payout of 0.10 per exit in a lifetime, not a farm, but it is not a discovery
    ///   either.
    /// - the adapter's first playable sample baselines the exits at *that* position, the way it
    ///   baselines the map id, so a restored save standing in a doorway is not paid for where it
    ///   already is. Every other exit on the map is still there to be found.
    /// - a warp that fires on the step onto it can have its on-exit half go unobserved in the
    ///   *outbound* direction, because the cartridge overwrites the coordinates as part of the
    ///   warp. Doors and map edges are ordinary standable tiles and pay both halves outbound.
    ///
    /// What this is not: a path. No button is chosen here, nothing is planned, and no map
    /// knowledge reaches the readout. It is a reward the fly may or may not find, like every other
    /// rule in the catalog.
    ///
    /// **Indoors it pays nothing** (the operator, 2026-09-23): on a map [`engage::indoor`] calls a
    /// building, every key is still written to the ledger -- so [`GameAdapter::exit_visited`]
    /// answers exactly what it did, and a door found indoors is found -- but no payout is emitted.
    /// The exits of a town, a route, a forest or a cave pay as they always have.
    #[allow(clippy::too_many_arguments)]
    fn boundary(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        memory: &mut impl MemoryReader,
        map: u8,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        first: bool,
        brain_ms: f64,
    ) {
        // Collected first, paid second: the ledger writes need `&mut self` and the table walk
        // needs the sample cache, and the order of the collection is the order of the payouts.
        let mut hits: Vec<(String, bool)> = Vec::new();
        let pays = !engage::indoor(memory.read8(ram::wCurMapTileset));

        let warps = memory.read8(ram::wNumberOfWarps).min(MAX_WARP_EVENTS);
        for index in 0..u16::from(warps) {
            let entry = ram::wWarpEntries + index * WARP_ENTRY_BYTES;
            let warp_y = u32::from(memory.read8(entry));
            let warp_x = u32::from(memory.read8(entry + 1));
            let distance = x.abs_diff(warp_x) + y.abs_diff(warp_y);
            if distance <= 1 {
                hits.push((format!("boundary:{map}:{warp_y}:{warp_x}"), distance == 0));
            }
        }

        let connections = memory.read8(ram::wCurMapConnections);
        // (bit, key suffix, the exit row or column, this player's coordinate along that axis).
        let edges = [
            (connection::EAST, 'e', width - 1, x),
            (connection::WEST, 'w', 0, x),
            (connection::SOUTH, 's', height - 1, y),
            (connection::NORTH, 'n', 0, y),
        ];
        for (bit, name, edge, position) in edges {
            if connections & bit == 0 {
                continue;
            }
            let distance = position.abs_diff(edge);
            if distance <= 1 {
                hits.push((format!("boundary:{map}:edge:{name}"), distance == 0));
            }
        }

        for (key, on_exit) in hits {
            if first {
                // Baseline *both* halves of every exit already within reach: a save restored in a
                // doorway has found neither the door nor the tile beside it, and crediting one
                // half would pay the other for a single step sideways.
                self.seen.insert(&format!("{key}:near"));
                self.seen.insert(&format!("{key}:on"));
                continue;
            }
            let key = format!("{key}:{}", if on_exit { "on" } else { "near" });
            if !pays {
                self.seen.insert(&key);
                continue;
            }
            self.once_scaled(
                emitted,
                &key,
                kind::BOUNDARY,
                BOUNDARY_LABEL.to_string(),
                if on_exit { 2.0 } else { 1.0 },
                false,
                brain_ms,
            );
        }
    }

    /// Pay each item picked up since the last playable sample, once per item for the lifetime of
    /// the ledger (`docs/rewards-learning.md`, the operator 2026-09-23).
    ///
    /// The first sample that finds [`ITEMS_SEEDED`] absent -- a fresh adapter, or a `v6` state
    /// restored under `v7` -- keys every item the cartridge already shows as taken and pays for
    /// none of them. After that a pickup is a bit that rose between two playable samples
    /// ([`engage::pickups`]) and pays unless its key is already in `seen`, which is what stops a
    /// rollback that un-takes an item from paying for it twice.
    fn items(
        &mut self,
        emitted: &mut Vec<RewardEvent>,
        memory: &mut impl MemoryReader,
        brain_ms: f64,
    ) {
        let now = engage::ItemFlags::read(memory);
        if !self.seen.contains(ITEMS_SEEDED) {
            for key in now.seed() {
                self.seen.insert(&key);
            }
            self.seen.insert(ITEMS_SEEDED);
        } else if let Some(before) = &self.item_flags {
            for pickup in engage::pickups(memory, before, &now) {
                self.once(emitted, &pickup.key, kind::ITEM, pickup.label, false, brain_ms);
            }
        }
        self.item_flags = Some(now);
    }

    pub fn export_state(&self) -> Value {
        json!({
            "version": STATE_VERSION,
            "seen": self.seen.as_slice(),
            "tiles": self.tiles.as_slice(),
            "tileCounts": self.tile_counts,
            "wildWins": self.wild_wins,
            // The one field v6 adds. A v5 state does not carry it and restores with it
            // empty, which is the documented v5 -> v6 migration and the truth about a run
            // that was never paid for a catch.
            "catchCounts": self.catch_counts,
            "replayBlocked": self.replay_blocked.as_slice(),
            "counts": self.counts,
            "total": self.total,
            "recent": self.recent,
            "last": self.last,
            "initialized": self.initialized,
            "sawBoot": self.saw_boot,
            "location": self.location,
            "stable": self.stable,
            // Both are derived from the cartridge, but only inside the unscripted-overworld
            // branch of `observe`, so a restore that lands in a battle or a script would
            // otherwise report rank 0 and 0 badges until the fly next stands still outdoors —
            // minutes, not frames. They are cheap to carry and self-correcting once sampled.
            "progress": self.progress,
            "badges": self.badges,
            "battle": self.battle.as_ref().map(|battle| json!({
                "key": battle.key,
                "wild": battle.wild,
                "sawLiving": battle.saw_living,
                "ko": battle.ko,
                "speciesAtStart": battle.species_at_start,
                "captured": battle.captured,
                "capturedNew": battle.captured_new,
            })),
            "mode": self.mode,
        })
    }

    /// Restore lifetime history from [`PokemonRedReward::export_state`].
    ///
    /// A state whose `version` is not [`STATE_VERSION`] is ignored and the
    /// adapter rebaselines at its next valid sample, because old reward
    /// semantics cannot be credited under the current rules. Malformed
    /// version-3 state is an error, with the prototype's three distinct
    /// messages preserved so a failed restore reads the same in the log.
    ///
    /// One deliberate difference: everything is validated before anything is
    /// assigned. The prototype validated its replay ledger after it had already
    /// overwritten `seen` and `tiles`, so a bad ledger left the adapter half
    /// mutated. Nothing depended on that, and a failed restore is safer whole.
    pub fn import_state(&mut self, input: &Value) -> Result<(), AdapterError> {
        if input.get("version").and_then(Value::as_u64) != Some(STATE_VERSION) {
            return Ok(());
        }
        const BAD_CHECKPOINT: AdapterError = AdapterError("Invalid reward checkpoint");
        const BAD_HISTORY: AdapterError = AdapterError("Invalid reward history");
        const BAD_LEDGER: AdapterError = AdapterError("Invalid replay ledger");

        let seen = string_array(input.get("seen")).ok_or(BAD_CHECKPOINT)?;
        let tiles = string_array(input.get("tiles")).ok_or(BAD_CHECKPOINT)?;
        let total = input.get("total").and_then(Value::as_f64).ok_or(BAD_CHECKPOINT)?;
        if !total.is_finite() {
            return Err(BAD_CHECKPOINT);
        }
        let stable = input.get("stable").and_then(Value::as_u64).ok_or(BAD_CHECKPOINT)?;
        let initialized = input.get("initialized").and_then(Value::as_bool).ok_or(BAD_CHECKPOINT)?;
        let saw_boot = input.get("sawBoot").and_then(Value::as_bool).ok_or(BAD_CHECKPOINT)?;
        let location = input.get("location").and_then(Value::as_str).ok_or(BAD_CHECKPOINT)?;
        let mode = input.get("mode").and_then(Value::as_str).ok_or(BAD_CHECKPOINT)?;
        let recent_raw = input.get("recent").and_then(Value::as_array).ok_or(BAD_CHECKPOINT)?;
        // Optional: a state written before these were carried restores at 0 and re-derives them
        // at the next unscripted overworld sample, which is the old behaviour.
        let progress = input.get("progress").and_then(Value::as_u64).unwrap_or(0) as u32;
        let badges = input.get("badges").and_then(Value::as_u64).unwrap_or(0) as u32;

        let counts_raw = counted_record(input.get("counts")).ok_or(BAD_CHECKPOINT)?;
        let tile_counts_raw = counted_record(input.get("tileCounts")).ok_or(BAD_CHECKPOINT)?;
        let wild_wins_raw = counted_record(input.get("wildWins")).ok_or(BAD_CHECKPOINT)?;
        // Absent in every v5 state, and that absence is the migration: no species has been
        // paid for a catch, because the rule did not exist. Present but malformed is still
        // an error, the same as every other counter here.
        let catch_counts = match input.get("catchCounts") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(value) => counted_record(Some(value)).ok_or(BAD_CHECKPOINT)?,
        };

        let recent = recent_raw
            .iter()
            .map(parse_event)
            .collect::<Option<Vec<_>>>()
            .ok_or(BAD_HISTORY)?;
        let mut last = LastEvents::default();
        for (key, value) in
            input.get("last").and_then(Value::as_object).ok_or(BAD_HISTORY)?.iter()
        {
            let event = parse_event(value).ok_or(BAD_HISTORY)?;
            if catalog::index(key).is_some() {
                last.set(event);
            }
        }
        let battle = match input.get("battle") {
            None | Some(Value::Null) => None,
            Some(value) => Some(Battle {
                key: value.get("key").and_then(Value::as_str).ok_or(BAD_HISTORY)?.to_string(),
                wild: value.get("wild").and_then(Value::as_bool).ok_or(BAD_HISTORY)?,
                saw_living: value.get("sawLiving").and_then(Value::as_bool).ok_or(BAD_HISTORY)?,
                ko: value.get("ko").and_then(Value::as_bool).ok_or(BAD_HISTORY)?,
                // All three are v6's, and all three are optional for the same reason
                // `catchCounts` is. A v5 battle carries no `speciesAtStart`, which reads as
                // "cannot tell whether the caught species was new" and pays the repeat
                // amount: the conservative half of the rule, and at most 0.20 once.
                species_at_start: value.get("speciesAtStart").and_then(Value::as_u64),
                captured: value
                    .get("captured")
                    .and_then(Value::as_u64)
                    .and_then(|species| u8::try_from(species).ok()),
                captured_new: value
                    .get("capturedNew")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
        };
        let replay_blocked = match input.get("replayBlocked") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => string_array(Some(value)).ok_or(BAD_LEDGER)?,
        };

        let mut tile_counts = BTreeMap::new();
        for (key, count) in &tile_counts_raw {
            let map: u8 = key.parse().map_err(|_| BAD_CHECKPOINT)?;
            tile_counts.insert(map, *count);
        }

        self.seen = seen.iter().map(String::as_str).collect();
        self.tiles = tiles.iter().map(String::as_str).collect();
        // Both keys are aliases of the same arrival, and only one of them was
        // recorded by older builds.
        if self.seen.contains("map:37") {
            self.seen.insert("early:downstairs");
        }
        if self.seen.contains("map:0") {
            self.seen.insert("early:outside");
        }
        self.tile_counts = tile_counts;
        self.wild_wins = wild_wins_raw;
        self.catch_counts = catch_counts;
        self.replay_blocked = replay_blocked.iter().map(String::as_str).collect();
        self.counts = Counts::default();
        for (key, count) in &counts_raw {
            self.counts.set(key, *count);
        }
        self.total = total;
        self.recent = recent;
        self.last = last;
        self.initialized = initialized;
        self.saw_boot = saw_boot;
        self.location = location.to_string();
        self.stable = stable;
        self.battle = battle;
        self.mode = mode.to_string();
        self.progress = progress;
        self.badges = badges;
        self.talk.clear();
        self.item_flags = None;
        Ok(())
    }
}

fn battle_key(memory: &mut impl MemoryReader, map: u8) -> String {
    format!(
        "{}:{}:{}",
        map,
        memory.read8(ram::wEnemyMonSpecies),
        memory.read8(ram::wEnemyMonLevel)
    )
}

/// An array whose every element is a string, or `None`.
fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let array = value?.as_array()?;
    array.iter().map(|item| item.as_str().map(str::to_string)).collect()
}

/// `Number.MAX_SAFE_INTEGER`, so a checkpoint the TypeScript build could not
/// have written is rejected here too.
const MAX_SAFE_INTEGER: u64 = (1u64 << 53) - 1;

/// An object whose every value is a non-negative safe integer, or `None`.
/// The port of `Object.values(record).some(v => !Number.isSafeInteger(v) || v < 0)`.
fn counted_record(value: Option<&Value>) -> Option<BTreeMap<String, u64>> {
    let object = value?.as_object()?;
    object
        .iter()
        .map(|(key, value)| {
            let count = value.as_u64().filter(|count| *count <= MAX_SAFE_INTEGER)?;
            Some((key.clone(), count))
        })
        .collect()
}

fn parse_event(value: &Value) -> Option<RewardEvent> {
    let kind = catalog::rule(value.get("kind")?.as_str()?)?;
    let label = value.get("label")?.as_str()?.to_string();
    let brain_ms = value.get("brainMs")?.as_f64().filter(|ms| ms.is_finite())?;
    let reward = value.get("value")?.as_f64().filter(|value| value.is_finite())?;
    Some(RewardEvent {
        kind: kind.kind,
        label,
        brain_ms,
        value: reward,
        stimulation_ms: kind.stimulation_ms,
    })
}

impl GameAdapter for PokemonRedReward {
    fn id(&self) -> &'static str {
        REWARD_ADAPTER
    }

    fn migrates_from(&self) -> &'static [&'static str] {
        MIGRATES_FROM
    }

    fn rom_allowed(&self, sha256: &str) -> bool {
        sha256 == SUPPORTED_ROM
    }

    fn sample(&mut self, memory: &mut dyn MemoryReader, ms: f64) -> Vec<RewardEvent> {
        PokemonRedReward::sample(self, memory, ms)
    }

    fn mode(&self) -> &str {
        &self.mode
    }

    fn map_id(&self) -> Option<u32> {
        PokemonRedReward::map(self)
    }

    fn location(&self) -> Option<(u32, u32, u32)> {
        PokemonRedReward::area_and_tile(self)
    }

    fn progress(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            rank: self.progress,
            rank_max: RANK_LADDER.len() as u32 - 1,
            rank_label: RANK_LADDER[(self.progress as usize).min(RANK_LADDER.len() - 1)],
            counter: self.badges,
            counter_label: "BADGES",
            unique_locations: self.tiles.len(),
            reward_total: self.total,
            counts: self.counts.to_map(),
        }
    }

    fn safe_for_snapshot(&self) -> bool {
        self.safe
    }

    /// Whether the `boundary` ledger has already recorded this exit.
    ///
    /// The keys are [`PokemonRedReward::boundary`]'s own, built by the same `format!` and read out
    /// of the same lifetime `seen` set, so "visited" here means precisely what the reward rule
    /// means by it and the two cannot drift: `boundary:<map>:<y>:<x>` for a warp and
    /// `boundary:<map>:edge:<n|s|e|w>` for a map edge, each with an `:on` and a `:near` half.
    ///
    /// **Either half counts**, and that is the semantics rather than a convenience. `:on` is what
    /// an interior warp records — arriving through a staircase lands on it, which is why the live
    /// run's ledger holds both halves of Red's — but a town door fires on the step *onto* it, so
    /// `sample` never sees a stable frame there and the `:on` half is unobservable in both
    /// directions. On `:on` alone every house door in the game would read as unvisited forever,
    /// which is the behaviour this accessor exists to fix. So the question it answers is "has this
    /// run been paid for finding this exit" — found, not used — and that is what the "unvisited"
    /// in `GO OUT`, `GO WARP` and `GO ROUTE` is asking.
    ///
    /// One consequence worth stating: the adapter baselines both halves of every exit within one
    /// tile of the player on its first playable sample, so a run restored standing in a doorway
    /// starts with that one door already recorded. That is the same baseline the reward rule uses,
    /// and it is why `palette::ways` falls back to every exit when the unvisited set is empty
    /// rather than binding nothing.
    fn exit_visited(&self, exit: MapExit) -> bool {
        let key = match exit {
            MapExit::Warp { map, x, y } => format!("boundary:{map}:{y}:{x}"),
            MapExit::Edge { map, edge } => {
                let name = match edge {
                    MapEdge::North => 'n',
                    MapEdge::South => 's',
                    MapEdge::East => 'e',
                    MapEdge::West => 'w',
                };
                format!("boundary:{map}:edge:{name}")
            }
        };
        self.seen.contains(&format!("{key}:on")) || self.seen.contains(&format!("{key}:near"))
    }

    /// Whether this run has stood on `tile`, from the `exploration` rule's own ledger.
    ///
    /// The same read accessor shape as [`GameAdapter::exit_visited`], over the same `OrderedSet`
    /// the `exploration` payout counts: `sample` inserts `"{map}:{x}:{y}"` for every distinct
    /// player coordinate on a playable frame, lifetime, surviving `clear_transient` and round-
    /// tripping through the checkpoint. So "never stood on" is answerable without one byte of new
    /// state, and `GO FRONTIER` is its only caller.
    ///
    /// One consequence worth naming: the ledger is *lifetime*, so a restored run starts with the
    /// ground it has already covered marked. That is the honest answer -- this run has stood
    /// there -- and it is why the frontier moves outward over a run instead of resetting.
    fn tile_visited(&self, tile: MapTile) -> bool {
        self.tiles.contains(&format!("{}:{}:{}", tile.map, tile.x, tile.y))
    }

    /// Whether this run has ever been on `map`, from the same lifetime ledger the `map` payout
    /// and the ladder's map rungs read (`"map:{id}"`).
    fn map_visited(&self, map: u8) -> bool {
        self.seen.contains(&format!("map:{map}"))
    }

    /// The place of the lowest rung this run has not earned, when [`RUNG_PLACES`] knows one.
    ///
    /// [`PokemonRedReward::next_unreached`] rather than `rank + 1`, and its doc comment has the
    /// save that made the difference matter. Past the top of the ladder there is no next rung and
    /// this is `None`.
    fn objective(&self) -> Option<MapPlace> {
        // `hall_of_fame` is a WRAM byte and this accessor reads no memory, so the champion rung is
        // asked from the ledger alone (`EVENT_BEAT_CHAMPION_RIVAL`, the other half of its own
        // condition). It can only matter once every other rung is earned, and that rung's place is
        // the one the catalog does not know.
        let next = self.next_unreached(self.badges, false)?;
        rung_place(&self.seen, next)
    }

    fn next_rung(&self) -> Option<u32> {
        self.next_unreached(self.badges, false).and_then(|rung| u32::try_from(rung).ok())
    }

    fn rank_ladder(&self) -> &'static [&'static str] {
        &RANK_LADDER
    }

    fn decoder_preset(&self) -> DecoderPresetId {
        DecoderPresetId::GameBoy
    }

    fn clear_transient(&mut self) {
        PokemonRedReward::clear_transient(self);
    }

    fn export_state(&self) -> Value {
        PokemonRedReward::export_state(self)
    }

    fn import_state(&mut self, state: &Value) -> Result<(), AdapterError> {
        PokemonRedReward::import_state(self, state)
    }
}

#[cfg(test)]
mod tests;
