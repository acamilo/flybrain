//! Macro-palette tests that need a real cartridge.
//!
//! Gated on `FLY_ROM` pointing at a Game Boy ROM file, the same convention `tests/rom.rs` and
//! `examples/room_escape.rs` use. The ROM never enters this repository (`.gitignore` excludes
//! `*.gb`), so these skip cleanly when the variable is unset:
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo test --release --test rom_macros -- --nocapture
//! ```
//!
//! ## What these can check that a fake cannot
//!
//! `docs/design/macros.md` section 3 says `GO EXIT` walks to the "nearest unvisited warp or
//! map-edge exit". Whether that works is a question about the cartridge: about the warp table's
//! layout, about the sixteen-frame step, about a doormat that fires on the step *off* it and not
//! on a sideways step onto it. The unit tests in `pokemon_red::macros::tests` encode those facts;
//! only the cartridge can confirm them.
//!
//! ## Where the state comes from
//!
//! Agent A's WRAM layer (`pokemon_red::state`, `docs/design/macros-wram.md`) is the real
//! implementation of the seam. It is not landed yet, and these tests must not grow a second one:
//! [`Cart`] therefore answers from two sources and nothing else.
//!
//! - **Addresses the reward adapter already samples**: `wCurMap`, `wXCoord`, `wYCoord`,
//!   `wCurMapWidth`, `wCurMapHeight`, `wCurMapConnections`, `wNumberOfWarps`, `wWarpEntries`,
//!   `wFontLoaded`, `wIsInBattle`. Every one of them is read by `PokemonRedReward::sample` for
//!   the `boundary` rule or the mode string, with the layout documented there.
//! - **The survey**, for the walkable predicate: a breadth-first walk of the map with real button
//!   presses on throwaway emulators, exactly as `examples/room_escape.rs` does it. That is
//!   measurement, not a WRAM read, and it is the instrument
//!   `docs/design/room-escape.md` section 3 used to establish what the room actually is.
//!
//! [`Cart::scene`] is a deliberately crude proxy — overworld, dialog or battle — because scene
//! detection is agent A's and duplicating it here would be testing this file's guess rather than
//! A's answer. What is under test is the executor.

use std::collections::{BTreeMap, BTreeSet};

use flybrain_gb::adapter::{MapTile, MemoryReader};
use flybrain_gb::pokemon_red::macros::cartridge::{ExitId, MacroState};
use flybrain_gb::pokemon_red::macros::executor::{MacroAbort, MacroMachine};
use flybrain_gb::pokemon_red::macros::palette::{
    MacroId, MacroKind, MacroSpec, Palette, objective_goals, precondition,
};
use flybrain_gb::pokemon_red::macros::palette::ways;
use flybrain_gb::pokemon_red::macros::path::Way;
use flybrain_gb::pokemon_red::macros::state::{
    Battle, Connections, Facing, GameState, MapSize, Npc, Party, Pc, Player, Scene, Shop, Sign,
    StartMenu, TextBox, Walkable, Warp,
};
use flybrain_gb::pokemon_red::state::PokeState;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, SUPPORTED_ROM};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter, buttons,
};

/// Map ids, from `constants/map_constants.asm`.
const REDS_HOUSE_1F: u8 = 0x25;
const REDS_HOUSE_2F: u8 = 0x26;
const PALLET_TOWN: u8 = 0x00;
const OAKS_LAB: u8 = 0x28;
const BLUES_HOUSE: u8 = 0x27;
const VIRIDIAN_MART: u8 = 0x2a;

/// `SPRITE_POKE_BALL`, which is also `FIRST_STILL_SPRITE` — the first sprite id that is not a
/// person (`constants/sprite_constants.asm`). The three starters in Oak's lab are
/// `object_event`s with exactly this picture.
const SPRITE_POKE_BALL: u8 = 0x3d;

/// `$ff` in a warp's destination: "the map the player came from" (`constants/map_constants.asm`,
/// `LAST_MAP`). Red's front doors are written this way rather than naming the town.
const LAST_MAP: u8 = 0xff;

/// One Game Boy frame on the brain clock, as every harness in the workspace counts it.
const MS_PER_FRAME: f64 = 1000.0 / 59.7275;

/// Bytes per warp table entry: `Y, X, destination warp id, destination map id`.
const WARP_BYTES: u16 = 4;

/// Frames [`Harness::settle`] waits before it believes the adapter. See its doc comment: one
/// frame after a warp `wCurMap` is already the new map and the warp table is still the old one's.
const SETTLE_FLOOR: u32 = 120;

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

macro_rules! skip_without_rom {
    () => {
        match rom() {
            Some(rom) => rom,
            None => {
                eprintln!("skipped: FLY_ROM is not set");
                return;
            }
        }
    };
}

fn emulator(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge")
}

/// The seam, answered from the adapter's own addresses plus a surveyed walkable set.
struct Cart<'a> {
    gb: &'a mut Emulator,
    /// Tiles the survey reached on this map. Everything else is not walkable.
    walkable: &'a BTreeSet<(u8, u8)>,
    /// The map the survey was taken on; a different map means the set says nothing.
    surveyed: u8,
    /// Which way the player faces, tracked from the presses the harness has made, because the
    /// facing byte is not one the reward adapter reads.
    facing: Facing,
    /// The exploration ledger. Agent A's real implementation reads the adapter's; here a test
    /// puts an exit in it to stand in for "this run has already been through that door".
    visited: &'a BTreeSet<ExitId>,
    /// Tiles of this map the test says the run has stood on, for `GO FRONTIER`.
    stood: &'a BTreeSet<(u8, u8)>,
}

impl GameState for Cart<'_> {
    fn scene(&mut self) -> Scene {
        if self.gb.read_wram(ram::wIsInBattle) != 0 {
            return Scene::Battle { own_turn: true, forced_switch: false };
        }
        if self.gb.read_wram(ram::wFontLoaded) & 1 != 0 {
            return Scene::Dialog;
        }
        Scene::Overworld
    }

    fn player(&mut self) -> Option<Player> {
        Some(Player {
            map: self.gb.read_wram(ram::wCurMap),
            x: self.gb.read_wram(ram::wXCoord),
            y: self.gb.read_wram(ram::wYCoord),
            facing: self.facing,
        })
    }

    fn map_size(&mut self) -> Option<MapSize> {
        // `wCurMapWidth` and `wCurMapHeight` are in blocks and the player's coordinates are in
        // tiles, two tiles to the block — which is the conversion `PokemonRedReward::sample`
        // makes before it range-checks a coordinate.
        let width = self.gb.read_wram(ram::wCurMapWidth).checked_mul(2)?;
        let height = self.gb.read_wram(ram::wCurMapHeight).checked_mul(2)?;
        (width > 0 && height > 0).then_some(MapSize { width, height })
    }

    fn party(&mut self) -> Party {
        Party::default()
    }

    fn battle(&mut self) -> Option<Battle> {
        None
    }

    fn text_box(&mut self) -> TextBox {
        let open = self.gb.read_wram(ram::wFontLoaded) & 1 != 0;
        TextBox { open, waiting: open }
    }

    fn start_menu(&mut self) -> Option<StartMenu> {
        None
    }

    fn shop(&mut self) -> Option<Shop> {
        None
    }

    fn pc(&mut self) -> Option<Pc> {
        None
    }

    fn money(&mut self) -> u32 {
        0
    }

    fn bag(&mut self) -> Vec<flybrain_gb::pokemon_red::macros::state::BagItem> {
        Vec::new()
    }

    fn npcs(&mut self) -> Vec<Npc> {
        Vec::new()
    }

    /// The sign table is agent A's ([`PokeState`]), and the tests that need it — `GO ITEM`'s —
    /// use that rather than this proxy. The `GO EXIT` and `NEXT` tests below need no signs, so
    /// answering "none" here is the honest reading for them and not a stub for a missing read.
    fn signs(&mut self) -> Vec<Sign> {
        Vec::new()
    }

    fn walkable(&mut self, x: u8, y: u8) -> Walkable {
        if self.gb.read_wram(ram::wCurMap) != self.surveyed {
            return Walkable::Unknown;
        }
        if self.walkable.contains(&(x, y)) {
            return Walkable::Yes;
        }
        // A warp tile is standable by definition, and the survey cannot see that: a step onto a
        // staircase changes the map, so the walk that found it recorded an exit rather than a
        // tile. Without this the only way out of Red's bedroom is not a tile the pathfinder can
        // aim at.
        if self.warps().iter().any(|warp| warp.x == x && warp.y == y) {
            return Walkable::Yes;
        }
        Walkable::No
    }

    fn warps(&mut self) -> Vec<Warp> {
        let count = self.gb.read_wram(ram::wNumberOfWarps).min(32);
        (0..u16::from(count))
            .map(|index| {
                let entry = ram::wWarpEntries + index * WARP_BYTES;
                Warp {
                    y: self.gb.read_wram(entry),
                    x: self.gb.read_wram(entry + 1),
                    destination_warp: self.gb.read_wram(entry + 2),
                    destination_map: self.gb.read_wram(entry + 3),
                }
            })
            .collect()
    }

    fn connections(&mut self) -> Connections {
        // `shift_const EAST, WEST, SOUTH, NORTH` in `constants/map_data_constants.asm`, the same
        // bits the adapter's `boundary` rule reads.
        let bits = self.gb.read_wram(ram::wCurMapConnections);
        Connections {
            east: bits & 1 != 0,
            west: bits & 2 != 0,
            south: bits & 4 != 0,
            north: bits & 8 != 0,
        }
    }
}

/// No move table, no chart and no stock: `GO EXIT` and `NEXT` need none of them, and a battle
/// test needs agent A's real implementation rather than a guess here. The ledger is the one
/// extension these tests do answer, because whether the staircase has been used is the whole
/// difference between a fresh run and a restored one.
impl MacroState for Cart<'_> {
    fn exit_visited(&mut self, exit: ExitId) -> bool {
        self.visited.contains(&exit)
    }

    fn tile_visited(&mut self, x: u8, y: u8) -> bool {
        self.stood.contains(&(x, y))
    }
}

/// A cartridge, the reward adapter beside it, and the survey of the map it is standing on.
struct Harness {
    gb: Emulator,
    adapter: PokemonRedReward,
    ms: f64,
    rom: Vec<u8>,
    walkable: BTreeSet<(u8, u8)>,
    surveyed: u8,
    facing: Facing,
    visited: BTreeSet<ExitId>,
    stood: BTreeSet<(u8, u8)>,
    /// Every (map, x, y) the fly has stood on, in order and without consecutive repeats: the
    /// test's own measurement of where it went, independent of the adapter's ledger and of the
    /// stability gate that ledger goes through.
    trail: Vec<(u8, u8, u8)>,
}

impl Harness {
    /// Boot the cartridge to a playable bedroom.
    ///
    /// The button pattern is `tests/rom.rs`'s: Start and A on a slow alternation, because a held
    /// button is ignored and each press has to be released, then B alone — the intro's A-mashing
    /// leaves a text box open and until it is dismissed the player cannot move at all.
    fn boot(rom: Vec<u8>) -> Self {
        let mut gb = emulator(&rom);
        assert_eq!(gb.rom_sha256(), SUPPORTED_ROM, "FLY_ROM is not the pinned cartridge");
        let mut adapter = PokemonRedReward::new();
        let mut ms = 0.0;
        for frame in 0..6_000u32 {
            let mask = match frame % 32 {
                0..=7 => buttons::START,
                16..=23 => buttons::A,
                _ => buttons::NONE,
            };
            gb.set_buttons(mask);
            gb.run_frame().expect("a frame should complete");
            ms += MS_PER_FRAME;
            adapter.sample(&mut gb, ms);
            if adapter.mode() == "OVERWORLD" && frame > 3_000 {
                break;
            }
        }
        for frame in 0..12_000u32 {
            gb.set_buttons(if frame % 24 < 8 { buttons::B } else { buttons::NONE });
            gb.run_frame().expect("a frame should complete");
            ms += MS_PER_FRAME;
            adapter.sample(&mut gb, ms);
            if adapter.safe_for_snapshot() {
                break;
            }
        }
        assert!(
            adapter.safe_for_snapshot(),
            "the intro never settled into a stable dialogue-free overworld (mode {})",
            adapter.mode()
        );
        assert_eq!(adapter.map_id(), Some(u32::from(REDS_HOUSE_2F)), "a cold boot ends in bed");
        let mut harness = Self {
            gb,
            adapter,
            ms,
            rom,
            walkable: BTreeSet::new(),
            surveyed: 0xff,
            facing: Facing::Down,
            visited: BTreeSet::new(),
            stood: BTreeSet::new(),
            trail: Vec::new(),
        };
        harness.resurvey();
        harness
    }

    fn map(&mut self) -> u8 {
        self.gb.read_wram(ram::wCurMap)
    }

    fn tile(&mut self) -> (u8, u8) {
        (self.gb.read_wram(ram::wXCoord), self.gb.read_wram(ram::wYCoord))
    }

    /// Let a warp transition finish, so the surveyed state is one the player can move from.
    ///
    /// A state exported in the middle of a warp has the player's input locked, and a survey taken
    /// on it finds exactly one tile. `safe_for_snapshot` is the reward adapter's own answer to
    /// "is this a state a snapshot can be restored into": stable controllable overworld
    /// coordinates with no dialogue font loaded.
    ///
    /// It is not sufficient on its own, and that is worth recording: `wCurMap` changes *before*
    /// the new map's data is loaded, so a sample taken one frame after a warp reports the new map
    /// while `wNumberOfWarps` and the warp table still hold the old one's. The player has also
    /// been standing still since before the warp, so the adapter's stability gate is already
    /// satisfied and it calls that frame safe. Hence the floor: the wait is at least
    /// [`SETTLE_FLOOR`] frames whatever the adapter says.
    fn settle(&mut self) {
        for frame in 0..600u32 {
            self.press(buttons::NONE);
            if frame >= SETTLE_FLOOR && self.adapter.safe_for_snapshot() {
                return;
            }
        }
        panic!("the game never settled (mode {})", self.adapter.mode());
    }

    /// Walk the current map with real presses and remember which tiles can be stood on.
    fn resurvey(&mut self) {
        let state = self.gb.export_state().expect("state export");
        let (reachable, _) = survey(&self.rom, &state);
        self.surveyed = self.map();
        self.walkable = reachable;
    }

    fn state(&mut self) -> Cart<'_> {
        Cart {
            gb: &mut self.gb,
            walkable: &self.walkable,
            surveyed: self.surveyed,
            facing: self.facing,
            visited: &self.visited,
            stood: &self.stood,
        }
    }

    /// Hold `mask` for one frame, sampling the adapter as the sim loop does.
    fn press(&mut self, mask: u8) {
        if let Some(facing) = direction_of(mask) {
            self.facing = facing;
        }
        self.gb.set_buttons(mask);
        self.gb.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        let ms = self.ms;
        self.adapter.sample(&mut self.gb, ms);
        let (map, (x, y)) = (self.map(), self.tile());
        if self.trail.last() != Some(&(map, x, y)) {
            self.trail.push((map, x, y));
        }
    }

    /// Start `kind`'s slot in the current scene's palette and run it to completion.
    fn run(&mut self, kind: MacroKind) -> MacroAbort {
        let (palette, slot) = self.pick(kind);
        let mut machine = MacroMachine::new(0x517e_ed01);
        machine
            .start(&palette, slot, &mut self.state())
            .unwrap_or_else(|error| panic!("{} was refused: {:?}", kind.name(), error.reason));
        while let Some(mask) = machine.step(&mut self.state()) {
            self.press(mask);
        }
        machine.outcome().expect("a finished macro has an outcome").1
    }

    /// `EVENT_FOLLOWED_OAK_INTO_LAB`, bit 0 of `wEventFlags`: whether Oak has walked the player
    /// into his lab, which is what puts the three balls in play.
    fn followed_oak(&mut self) -> bool {
        self.gb.read_wram(ram::wEventFlags) & 1 != 0
    }

    fn party_count(&mut self) -> u8 {
        self.gb.read_wram(ram::wPartyCount)
    }

    /// Drive the cartridge until `done` says so, and return the frame it ended on.
    ///
    /// `tests/rom_scene.rs`'s `drive_until`, unchanged in substance: a fixed-seed walk with a
    /// per-cycle B press to close whatever a stray press opened, and an optional bias that aims
    /// it. The bias is the *test's* knowledge of the game and decides which states get produced;
    /// nothing it returns is ever asserted. The stage's own button is B throughout here, because
    /// these tests must reach Oak's lab **without** taking a starter: A on a ball is the thing
    /// under test.
    fn drive_until(
        &mut self,
        what: &str,
        budget: u32,
        seed: u64,
        mut bias: impl FnMut(&mut Harness) -> Option<u8>,
        mut done: impl FnMut(&mut Harness) -> bool,
    ) {
        let mut state = seed;
        for frame in 0..budget {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let random = [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT]
                [((state >> 33) % 4) as usize];
            let step = match bias(self) {
                Some(direction) if (state >> 41).is_multiple_of(2) => direction,
                _ => random,
            };
            let mask = match frame % 48 {
                0..=7 | 24..=31 => step,
                12..=15 | 36..=39 => buttons::B,
                _ => buttons::NONE,
            };
            self.press(mask);
            if done(self) {
                eprintln!("{what}: reached after {frame} frames of the stage");
                return;
            }
        }
        panic!(
            "{what}: not reached in {budget} frames (map {:#04x}, tile {:?}, party {})",
            self.map(),
            self.tile(),
            self.party_count()
        );
    }

    /// The slot `kind` is bound to in the current scene, found by name rather than by number.
    fn pick(&mut self, kind: MacroKind) -> (Palette, MacroId) {
        let mut state = self.state();
        let scene = state.scene();
        let palette = Palette::for_scene(scene, &mut state);
        let slot = palette
            .slots
            .iter()
            .position(|slot| slot.is_some_and(|spec| spec.kind == kind))
            .unwrap_or_else(|| {
                panic!("{} is not bound in the {} palette", kind.name(), scene.label())
            });
        (palette, MacroId(slot as u8))
    }
}

fn direction_of(mask: u8) -> Option<Facing> {
    match mask {
        buttons::UP => Some(Facing::Up),
        buttons::DOWN => Some(Facing::Down),
        buttons::LEFT => Some(Facing::Left),
        buttons::RIGHT => Some(Facing::Right),
        _ => None,
    }
}

/// A surveyed map: which tiles can be stood on, and a save state taken on each.
type Survey = (BTreeSet<(u8, u8)>, BTreeMap<(u8, u8), Vec<u8>>);

/// The starting map's reachable tiles and a save state on each, measured rather than assumed.
///
/// `examples/room_escape.rs`'s instrument, unchanged in substance: a breadth-first walk with real
/// button presses on throwaway emulators, from each tile holding each direction until the
/// coordinates change or 48 frames pass. Nothing neural is involved and the measured run's own
/// emulator is untouched.
fn survey(rom: &[u8], state: &[u8]) -> Survey {
    let read = |probe: &mut Emulator| {
        (probe.read8(ram::wCurMap), probe.read8(ram::wXCoord), probe.read8(ram::wYCoord))
    };
    // Pulsed, not held: after a warp the game ignores a direction that is merely held down, so a
    // survey taken on a staircase landing with one long hold finds exactly one tile. Measured on
    // Red's ground floor, and the same reason the walk in `executor.rs` pulses.
    let step = |probe: &mut Emulator, mask: u8| {
        let before = read(probe);
        let mut moved = false;
        for frame in 0..96u32 {
            probe.set_buttons(if frame % 24 < 12 { mask } else { buttons::NONE });
            probe.run_frame().expect("a frame should complete");
            if read(probe) != before {
                moved = true;
                break;
            }
        }
        probe.set_buttons(buttons::NONE);
        for _ in 0..10 {
            probe.run_frame().expect("a frame should complete");
        }
        (read(probe), moved)
    };
    let restore = |bytes: &[u8]| {
        let mut probe = emulator(rom);
        probe.import_state(bytes).expect("a surveyed state should import");
        probe
    };

    let mut first = restore(state);
    let (start_map, x, y) = read(&mut first);
    let mut reachable = BTreeSet::new();
    let mut states: BTreeMap<(u8, u8), Vec<u8>> = BTreeMap::new();
    reachable.insert((x, y));
    states.insert((x, y), state.to_vec());
    let mut queue = vec![(x, y)];
    while let Some(tile) = queue.pop() {
        for mask in [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT] {
            let mut probe = restore(&states[&tile]);
            let ((map, x, y), moved) = step(&mut probe, mask);
            if !moved || map != start_map {
                continue;
            }
            if reachable.insert((x, y)) {
                states.insert((x, y), probe.export_state().expect("state export"));
                queue.push((x, y));
            }
        }
    }
    (reachable, states)
}

// ---------------------------------------------------------------------------------------------
// GO EXIT
// ---------------------------------------------------------------------------------------------

/// `GO EXIT` from Red's bedroom reaches the staircase, which is the map's only warp.
///
/// This is the half of `docs/design/room-escape.md` section 3 that the design got right for the
/// wrong reason: "leaving the map is not leaving the house", and the staircase is one step and
/// lands in the other room. One macro should do it, because the bedroom has exactly one exit.
#[test]
fn go_warp_from_the_bedroom_reaches_the_ground_floor() {
    let mut harness = Harness::boot(skip_without_rom!());
    assert_eq!(harness.map(), REDS_HOUSE_2F);

    // The warp table, as `RedsHouse2F_Object` declares it: `warp_event 7, 1, REDS_HOUSE_1F, 3`,
    // and `MACRO warp_event` emits `db \2, \1, ...`, so the bytes read Y then X.
    let warps = harness.state().warps();
    assert_eq!(warps.len(), 1, "the bedroom has one warp: {warps:?}");
    assert_eq!((warps[0].x, warps[0].y), (7, 1), "the staircase");
    let spawn = harness.tile();
    assert_ne!(spawn, (7, 1), "the fly does not spawn on the stairs");

    // Section 9.1: the bedroom's one warp goes to another interior map, so it is a *passage*
    // and `GO WARP` is the macro that takes it. `GO OUT` has nothing to aim at up here at all,
    // which is the distinction the operator asked for, measured on the cartridge.
    assert_eq!(warps[0].destination_map, REDS_HOUSE_1F, "an interior destination");
    assert!(
        harness.state().warps().iter().all(|warp| warp.destination_map != LAST_MAP),
        "the bedroom has no door to the outside"
    );
    let outcome = harness.run(MacroKind::GoWarp);
    eprintln!("GO WARP from {spawn:?}: {outcome:?}, now on map {:#x}", harness.map());
    assert_eq!(outcome, MacroAbort::Done);
    assert_eq!(harness.map(), REDS_HOUSE_1F, "down the stairs");
}

/// `GO OUT` from the ground floor reaches Pallet Town.
///
/// The test the v0.1.0 release box failed for forty minutes: four tiles are one press from
/// outside, that press is DOWN and only DOWN, and a sideways step onto a doormat does nothing
/// (`docs/design/room-escape.md` section 3). The staircase used to compete with the doormats here
/// and had to be put into the ledger by hand to keep `GO EXIT` off it; under section 9.1 it is a
/// *passage* and `GO OUT` cannot see it at all, which is the distinction doing real work.
#[test]
fn go_out_from_the_ground_floor_reaches_pallet_town() {
    let mut harness = Harness::boot(skip_without_rom!());
    assert_eq!(harness.run(MacroKind::GoWarp), MacroAbort::Done, "down the stairs first");
    assert_eq!(harness.map(), REDS_HOUSE_1F);
    harness.settle();
    harness.resurvey();
    eprintln!("the ground floor has {} reachable tiles", harness.walkable.len());
    assert!(harness.walkable.len() > 24, "the survey found {} tiles", harness.walkable.len());

    let warps = harness.state().warps();
    let height = harness.state().map_size().expect("a loaded map").height;
    let mats: Vec<&Warp> = warps.iter().filter(|warp| warp.y + 1 == height).collect();
    assert_eq!(mats.len(), 2, "two doormats side by side on the bottom row: {warps:?}");
    for mat in &mats {
        // `$ff` is the decomp's "the map the player came from", which is how a front door is
        // written: the door does not name Pallet Town, it names outside.
        assert_eq!(mat.destination_map, LAST_MAP, "a doormat leads out: {mat:?}");
    }
    assert!(
        warps.iter().any(|warp| warp.destination_map == REDS_HOUSE_2F),
        "the staircase is here too: {warps:?}"
    );
    // And `GO OUT` is pointed at the two doormats and nothing else, by classification rather than
    // by a ledger entry the test had to plant.
    let outs: Vec<(u8, u8)> = {
        let ledger = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &ledger);
        ways(&mut state, Way::Exit).iter().map(|exit| (exit.tile.x, exit.tile.y)).collect()
    };
    assert_eq!(outs.len(), 2, "the two doormats and not the staircase: {outs:?}");
    assert!(outs.iter().all(|(_, y)| *y + 1 == height), "both on the bottom row: {outs:?}");

    // One macro per attempt: a walk is capped at 600 frames, and the ground floor is wide enough
    // that a run from the far corner can use them up. The fly would press the slot again; here
    // the test does, and three attempts is the same bound the walk's own failure rule uses.
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let from = harness.tile();
        let outcome = harness.run(MacroKind::GoOut);
        outcomes.push((from, outcome, harness.map()));
        if harness.map() != REDS_HOUSE_1F {
            break;
        }
    }
    eprintln!("GO OUT attempts: {outcomes:?}");
    assert_eq!(harness.map(), PALLET_TOWN, "the fly left the house: {outcomes:?}");
    assert!(
        outcomes.iter().all(|(_, outcome, _)| *outcome == MacroAbort::Done),
        "no attempt blocked or timed out: {outcomes:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GO ITEM, and GO EXIT against the adapter's own ledger
// ---------------------------------------------------------------------------------------------

/// `rom_scene.rs`'s bias, with the A press taken out: reach Oak's lab, do not take a starter.
///
/// Three facts, each from the disassembly. `PalletTownDefaultScript` triggers on `wYCoord == 1`,
/// so the top row of Pallet Town anywhere along it is enough and Oak walks the player in himself;
/// the two house doors are at x 5 and x 13 and a door warps on the step onto it, so step off those
/// columns before walking north; and the three balls are at the top of the lab
/// (`object_event 6, 3` / `7, 3` / `8, 3`), so the lab is somewhere to walk north in once
/// `EVENT_FOLLOWED_OAK_INTO_LAB` is set.
fn toward_the_lab(harness: &mut Harness) -> Option<u8> {
    match harness.map() {
        PALLET_TOWN => {
            Some(if matches!(harness.tile().0, 5 | 13) { buttons::LEFT } else { buttons::UP })
        }
        OAKS_LAB if harness.followed_oak() => Some(buttons::UP),
        _ => Some(buttons::DOWN),
    }
}

/// Run `kind` over agent A's real seam and the adapter's real ledger.
///
/// [`Harness::run`] drives the [`Cart`] proxy, which reports no sprites and no signs; `GO ITEM`
/// needs both, and `GO EXIT`'s "unvisited" needs the ledger the adapter is filling in as the walk
/// goes. The borrows are taken and dropped inside the loop because each frame reads the state and
/// then presses a button, which writes it.
fn run_real(harness: &mut Harness, kind: MacroKind) -> MacroAbort {
    let mut machine = MacroMachine::new(0x517e_ed02);
    {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        let scene = state.scene();
        let palette = Palette::for_scene(scene, &mut state);
        let slot = palette
            .slots
            .iter()
            .position(|slot| slot.is_some_and(|spec| spec.kind == kind))
            .unwrap_or_else(|| {
                panic!("{} is not bound in the {} palette", kind.name(), scene.label())
            });
        machine
            .start(&palette, MacroId(slot as u8), &mut state)
            .unwrap_or_else(|error| panic!("{} was refused: {:?}", kind.name(), error.reason));
    }
    loop {
        let mask = {
            let exits = AdapterLedger(&harness.adapter);
            let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
            machine.step(&mut state)
        };
        match mask {
            Some(mask) => harness.press(mask),
            None => break,
        }
    }
    machine.outcome().expect("a finished macro has an outcome").1
}

/// `GO ITEM` walks to a starter Pokéball in Oak's lab, and `TALK` there offers the starter.
///
/// The slot `LOOK` used to hold, doing the thing `LOOK` could not: the balls are `object_event`s
/// with `SPRITE_POKE_BALL`, which is not a person, so `GO NPC` never aimed at one and nothing in
/// the overworld palette walked the fly to the table. The proof that it is really a starter ball
/// and not some other object is the party: only the three balls on that table hand over a
/// Pokémon, so a species landing in `wPartyMon1` after `TALK` and a few `NEXT`s can only have
/// come from one of them.
#[test]
fn go_item_reaches_a_starter_ball_and_talk_offers_it() {
    let mut harness = Harness::boot(skip_without_rom!());
    harness.drive_until("pallet town", 200_000, 0x5eed_1234_5678_9abc, |_| None, |harness| {
        harness.map() == PALLET_TOWN && harness.adapter.safe_for_snapshot()
    });
    harness.drive_until("oak's lab", 900_000, 0x1357_9bdf_2468_ace0, toward_the_lab, |harness| {
        harness.map() == OAKS_LAB && harness.followed_oak() && harness.adapter.safe_for_snapshot()
    });
    assert_eq!(harness.party_count(), 0, "the drive must not have taken a starter itself");

    // Fixture selection, not the assertion: walk north until the nearest object to the fly is one
    // of the three balls rather than one of the two Pokédexes on the other table, and is two or
    // more tiles away so that the macro has to walk rather than turn. The overworld requirement is
    // agent A's scene rather than the adapter's snapshot gate, because a frame that is one press
    // from Oak's own dialogue satisfies the second and not the first. What `GO ITEM` does about it
    // is what is under test.
    harness.drive_until("the ball table", 200_000, 0x2468_ace0_1357_9bdf, toward_the_lab, |harness| {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        if state.scene() != Scene::Overworld {
            return false;
        }
        let Some(player) = state.player() else { return false };
        let mut objects: Vec<(u32, u8)> = state
            .npcs()
            .iter()
            .filter(|npc| !npc.person())
            .map(|npc| {
                let distance =
                    u32::from(npc.x.abs_diff(player.x)) + u32::from(npc.y.abs_diff(player.y));
                (distance, npc.picture)
            })
            .collect();
        objects.sort_unstable();
        objects
            .first()
            .is_some_and(|(distance, picture)| *picture == SPRITE_POKE_BALL && *distance >= 2)
    });

    let balls: Vec<(u8, u8)> = {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        state
            .npcs()
            .iter()
            .filter(|npc| npc.picture == SPRITE_POKE_BALL)
            .map(|npc| (npc.x, npc.y))
            .collect()
    };
    eprintln!("the lab's balls are at {balls:?}, the fly at {:?}", harness.tile());
    assert_eq!(balls.len(), 3, "three starters on the table: {balls:?}");

    let from = harness.tile();
    let outcome = run_real(&mut harness, MacroKind::GoItem);
    let (x, y) = harness.tile();
    eprintln!("GO ITEM: {from:?} -> {:?} facing {:?}, {outcome:?}", (x, y), harness.facing);
    assert_eq!(outcome, MacroAbort::Done);
    assert_ne!((x, y), from, "the fixture put the fly two tiles away, so the macro walked");
    let ball = balls
        .iter()
        .find(|(bx, by)| u32::from(bx.abs_diff(x)) + u32::from(by.abs_diff(y)) == 1)
        .unwrap_or_else(|| {
            panic!("the walk did not end one tile from a ball: at {:?}, balls {balls:?}", (x, y))
        });
    // And it is looking at that ball: the press that ends the walk turns the player into the
    // occupied tile, which is the same button the fly's own raw presses use.
    let (dx, dy) = harness.facing.delta();
    assert_eq!(
        (i16::from(x) + dx, i16::from(y) + dy),
        (i16::from(ball.0), i16::from(ball.1)),
        "facing {:?} from {:?} does not point at the ball at {ball:?}",
        harness.facing,
        (x, y)
    );

    // TALK is the A press, and the dialog it opens is the one that offers a starter.
    assert_eq!(
        harness.state().scene(),
        Scene::Overworld,
        "something interrupted between the two macros"
    );
    let outcome = run_real(&mut harness, MacroKind::Talk);
    assert_eq!(outcome, MacroAbort::Done);
    assert!(harness.state().text_box().open, "TALK opened no text box");

    // Answer it: A advances the offer and takes the YES it opens on. Only the three balls on that
    // table put a Pokémon in the party, so the party count alone settles which dialog it was.
    for _ in 0..40 {
        if harness.party_count() > 0 {
            break;
        }
        for frame in 0..32 {
            harness.press(if frame < 8 { buttons::A } else { buttons::NONE });
        }
    }
    assert_eq!(harness.party_count(), 1, "answering the dialog did not hand over a Pokémon");

    // `wPartyCount` leads the struct it counts: `AddPartyMon` writes the count and fills the 44
    // bytes over the frames after it, and Oak's gift is spread across a script — 3,245 frames when
    // `docs/design/macros-wram.md` measured it. So the species is waited for rather than read on
    // the frame the count changed.
    let mut species = 0;
    for frame in 0..12_000u32 {
        harness.press(if frame % 32 < 8 { buttons::B } else { buttons::NONE });
        species = harness.gb.read_wram(ram::wPartyMon1);
        if species != 0 {
            break;
        }
    }
    eprintln!("after TALK and YES: party {}, species {species:#04x}", harness.party_count());
    assert!(
        [0x99u8, 0xb0, 0xb1].contains(&species),
        "the dialog GO ITEM walked to was the starter choice: Bulbasaur ($99), Charmander ($b0) \
         or Squirtle ($b1), got {species:#04x}"
    );
}

/// In Pallet Town, `GO EXIT` prefers an exit this run has not been to over the door it came out
/// of.
///
/// The ledger is the reward adapter's own `boundary` set, filled in by the same `sample` calls the
/// sim loop makes, with nothing put into it by hand: walking out of Red's house records that door
/// and leaves the town's other doors and its two route edges unrecorded. Before this was wired
/// `exit_visited` was constantly false, so the nearest exit won — and the nearest exit to a fly
/// standing outside its own front door is that front door.
#[test]
fn go_route_in_pallet_town_prefers_an_unvisited_exit_over_the_door_it_came_out_of() {
    let mut harness = Harness::boot(skip_without_rom!());
    harness.drive_until("pallet town", 200_000, 0x5eed_1234_5678_9abc, |_| None, |harness| {
        harness.map() == PALLET_TOWN && harness.adapter.safe_for_snapshot()
    });
    harness.settle();
    harness.resurvey();

    let (visited, unvisited, door, goals) = {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        let all = flybrain_gb::pokemon_red::macros::exits(&mut state);
        let mut visited = Vec::new();
        let mut unvisited = Vec::new();
        for exit in &all {
            if state.exit_visited(exit.id) {
                visited.push(*exit);
            } else {
                unvisited.push(*exit);
            }
        }
        let door = state
            .warps()
            .iter()
            .position(|warp| warp.destination_map == REDS_HOUSE_1F)
            .map(|index| state.warps()[index]);
        let goals: Vec<(u8, u8)> = ways(&mut state, Way::Route)
            .iter()
            .map(|exit| (exit.tile.x, exit.tile.y))
            .collect();
        (visited, unvisited, door, goals)
    };
    let door = door.expect("Pallet Town has a warp back into Red's house");
    eprintln!(
        "Pallet Town: the fly is at {:?}, Red's door is at {:?}, {} exits recorded, {} not",
        harness.tile(),
        (door.x, door.y),
        visited.len(),
        unvisited.len()
    );
    assert!(
        !visited.is_empty(),
        "walking out of the house should have recorded at least one exit in the ledger"
    );
    assert!(!unvisited.is_empty(), "and the rest of the town's exits should be unrecorded");
    assert!(
        !goals.contains(&(door.x, door.y)),
        "GO ROUTE still aims at the door it came out of: goals {goals:?}"
    );
    // Section 9.2: a connection to a map this run has not stood on outranks every door, and the
    // town's two connections are its top and bottom rows. The boundary ledger no longer decides
    // this -- walking along the top row used to mark the way to Route 1 "visited" and hand the
    // choice to whichever front door was nearest.
    assert!(
        goals.iter().all(|(_, y)| *y == 0 || *y == 17),
        "every goal is a row of the map's edge, i.e. a connection: goals {goals:?}"
    );

    let outcome = run_real(&mut harness, MacroKind::GoRoute);
    eprintln!("GO ROUTE: {outcome:?}, now on map {:#04x} at {:?}", harness.map(), harness.tile());
    assert_ne!(harness.map(), REDS_HOUSE_1F, "the fly went straight back indoors");
}

/// `GO FRONTIER` from Pallet Town walks onto a tile this run has never stood on.
///
/// Section 9: it "replaces `WANDER` in every mode: walk to the nearest tile bordering ground this
/// run has never stood on, using the exploration ledger, and face it. No random steps remain
/// anywhere in the palette." On the cartridge that means two things at once — the ledger the walk
/// reads is the adapter's own `exploration` set, filled in by the same `sample` calls the sim loop
/// makes, and the press that faces the new ground is a press that walks onto it. The assertion is
/// the ledger's: the tile the fly ends on was not in it when the macro started.
#[test]
fn go_frontier_in_pallet_town_reaches_a_tile_the_run_has_not_stood_on() {
    let mut harness = Harness::boot(skip_without_rom!());
    harness.drive_until("pallet town", 200_000, 0x5eed_1234_5678_9abc, |_| None, |harness| {
        harness.map() == PALLET_TOWN && harness.adapter.safe_for_snapshot()
    });
    harness.settle();
    harness.resurvey();

    let before = harness.tile();
    let map = harness.map();
    let size = harness.state().map_size().expect("a loaded map");
    // The ledger as it stands *before* the macro, asked tile by tile through the same accessor
    // `GO FRONTIER` uses. Snapshotting it here rather than counting `unique_locations` afterwards
    // is deliberate: the `exploration` payout has its own stability gate, so the count lags the
    // step by a few frames and would make this a test of the gate.
    let covered: BTreeSet<(u8, u8)> = (0..size.height)
        .flat_map(|y| (0..size.width).map(move |x| (x, y)))
        .filter(|(x, y)| harness.adapter.tile_visited(MapTile { map, x: *x, y: *y }))
        .collect();
    assert!(covered.contains(&before), "the fly has stood where it is standing: {before:?}");
    let frontiers = {
        let ledger = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &ledger);
        flybrain_gb::pokemon_red::macros::path::frontier(&mut state)
    };
    assert!(
        !frontiers.is_empty(),
        "a town the fly has just walked into has ground it has not covered"
    );

    let walked_from = harness.trail.len();
    let outcome = run_real(&mut harness, MacroKind::GoFrontier);
    let after = harness.tile();
    // Where the walk actually went, measured by the harness rather than by the reward rule: the
    // `exploration` payout has its own stability gate, so the ledger lags the step by a few
    // frames and counting it afterwards would test the gate instead of the macro.
    let walked: Vec<(u8, u8, u8)> = harness.trail[walked_from..].to_vec();
    eprintln!(
        "GO FRONTIER: {outcome:?}, {} frontier tiles, {before:?} -> {after:?}, \
         {} tiles in the ledger before, walked {walked:?}",
        frontiers.len(),
        covered.len()
    );
    assert_eq!(outcome, MacroAbort::Done);
    assert_ne!(after, before, "the macro moved the fly");
    assert!(
        walked.iter().any(|(_, x, y)| !covered.contains(&(*x, *y))),
        "and stood on ground this run's ledger had not recorded: {walked:?}"
    );
    // The map id is not asserted, and that is worth recording rather than tightening: a door is
    // walkable ground the run has never stood on, so the nearest frontier tile in a town is very
    // often a doorway, and the step onto it warps on the same frame the coordinates change --
    // which is why the walked tile above can carry the interior's map id. That is `GO FRONTIER`
    // finding new ground, not a fault, and the ledger it asked is the same either way.
    eprintln!("GO FRONTIER ended on map {:#04x} at {after:?}", harness.map());
}

// ---------------------------------------------------------------------------------------------
// NEXT
// ---------------------------------------------------------------------------------------------

/// A dialog `NEXT` advances a text box, and leaving it alone does not.
///
/// Two halves, and the control is the half that carries the claim. A text box waiting for A stays
/// open for as long as you leave it alone — four seconds of it here — so the box closing after a
/// handful of `NEXT` macros is what proves that `NEXT` moved the text rather than time. Each macro
/// is one page: a whole palette lookup, a pulse the game's own debounce accepts, and an outcome.
#[test]
fn next_advances_a_text_box_and_waiting_does_not() {
    let rom = skip_without_rom!();
    let mut harness = Harness::boot(rom.clone());
    let state = harness.gb.export_state().expect("state export");
    let (_, states) = survey(&rom, &state);

    let Some((open, tile, facing)) = find_text_box(&rom, &states) else {
        panic!("nothing in Red's bedroom opened a text box on an A press");
    };
    eprintln!("a text box opens facing {facing:?} from {tile:?}");

    // The control: the box waits, for far longer than the macros below take.
    let mut idle = emulator(&rom);
    idle.import_state(&open).expect("import");
    for _ in 0..240 {
        idle.set_buttons(buttons::NONE);
        idle.run_frame().expect("a frame should complete");
    }
    assert!(idle.read_wram(ram::wFontLoaded) & 1 != 0, "a waiting text box does not close itself");

    // The macro, one page at a time.
    let mut harness = Harness::boot(rom);
    harness.gb.import_state(&open).expect("import");
    harness.facing = facing;
    assert_eq!(harness.state().scene(), Scene::Dialog, "the proxy sees a text box");

    let mut outcomes = Vec::new();
    for _ in 0..10 {
        if !harness.state().text_box().open {
            break;
        }
        outcomes.push(harness.run(MacroKind::Next));
    }
    eprintln!("NEXT outcomes: {outcomes:?}");
    assert!(!harness.state().text_box().open, "NEXT never got through the text: {outcomes:?}");
    assert!(!outcomes.is_empty(), "the box was already closed");
    assert!(
        outcomes.iter().all(|outcome| *outcome == MacroAbort::Done),
        "every NEXT finished: {outcomes:?}"
    );
}

/// Find a tile and facing where one A press opens a message box, by trying them.
///
/// The survey again: restore onto each reachable tile, turn each way, press A, and see whether the
/// dialogue font loads. No map knowledge and no address beyond `wFontLoaded`, which is the byte
/// the reward adapter's own safe-snapshot gate reads.
///
/// Red's bedroom has two things that load that font and they are not the same kind of thing: the
/// PC, which is a menu that A goes *deeper* into, and the television, which is a message that A
/// advances. Telling them apart is scene detection, which is agent A's half of this work, so this
/// picks the one that behaves like a message — a raw A press, repeated, clears it — and that
/// choice is fixture selection, not the assertion. What the test then measures is whether the
/// `NEXT` *macro* does it: the palette lookup, the pulse timing and the outcome.
fn find_text_box(
    rom: &[u8],
    states: &BTreeMap<(u8, u8), Vec<u8>>,
) -> Option<(Vec<u8>, (u8, u8), Facing)> {
    for (tile, state) in states {
        for (facing, mask) in [
            (Facing::Up, buttons::UP),
            (Facing::Down, buttons::DOWN),
            (Facing::Left, buttons::LEFT),
            (Facing::Right, buttons::RIGHT),
        ] {
            let mut probe = emulator(rom);
            probe.import_state(state).expect("import");
            // Turn: a direction the player is not facing spends its first press turning, and a
            // press has to be released before the next one registers.
            for frame in 0..24 {
                probe.set_buttons(if frame < 8 { mask } else { buttons::NONE });
                probe.run_frame().expect("a frame should complete");
            }
            if (probe.read8(ram::wXCoord), probe.read8(ram::wYCoord)) != *tile {
                continue;
            }
            for frame in 0..48 {
                probe.set_buttons(if frame < 8 { buttons::A } else { buttons::NONE });
                probe.run_frame().expect("a frame should complete");
            }
            if probe.read8(ram::wFontLoaded) & 1 == 0 {
                continue;
            }
            let open = probe.export_state().expect("state export");
            if dismissible(rom, &open) {
                return Some((open, *tile, facing));
            }
        }
    }
    None
}

/// Whether repeated A presses clear this box, which is what separates a message from a menu.
fn dismissible(rom: &[u8], open: &[u8]) -> bool {
    let mut probe = emulator(rom);
    probe.import_state(open).expect("import");
    for _ in 0..10 {
        for frame in 0..32 {
            probe.set_buttons(if frame < 8 { buttons::A } else { buttons::NONE });
            probe.run_frame().expect("a frame should complete");
        }
        if probe.read8(ram::wFontLoaded) & 1 == 0 {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// ATTACK
// ---------------------------------------------------------------------------------------------

/// `MOVE 1` selects the first move and the enemy's HP drops.
///
/// Gated twice over: on `FLY_ROM` and on `FLY_MACRO_BATTLE`, a save state taken on the player's
/// turn of a battle. A cold boot cannot reach one — Oak stops the player leaving Pallet Town
/// without a Pokémon — so the state has to come from somewhere, and
/// `docs/design/macros-wram.md` is where agent A records how theirs was made. Skips cleanly
/// without it, and says what it wanted.
///
/// It also needs agent A's real `MacroState`: [`Cart`] takes every default, so it reports no
/// battle at all and `MOVE 1` would not be bound. The assertions below are therefore about the
/// cartridge and are left to run against A's implementation; what this test pins today is that
/// the gate is wired and the reason it cannot run yet is recorded.
#[test]
fn attack_selects_a_move_and_the_enemy_loses_hp() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(path) = std::env::var_os("FLY_MACRO_BATTLE") else {
        eprintln!(
            "skipped: FLY_MACRO_BATTLE is not set. It wants a binjgb save state taken on the \
             player's turn of a battle; see docs/design/macros-wram.md."
        );
        return;
    };
    let state = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("FLY_MACRO_BATTLE is {path:?}: {error}"));
    let mut gb = emulator(&rom);
    gb.import_state(&state).expect("the battle state should import");
    assert_ne!(gb.read_wram(ram::wIsInBattle), 0, "FLY_MACRO_BATTLE is not a battle");
    let before = u16::from(gb.read_wram(ram::wEnemyMonHP)) * 256
        + u16::from(gb.read_wram(ram::wEnemyMonHP + 1));
    assert!(before > 0, "the enemy is already fainted");

    let mut harness = Harness::boot(rom);
    harness.gb.import_state(&state).expect("import");
    let outcome = harness.run(MacroKind::Move1);
    let after = u16::from(harness.gb.read_wram(ram::wEnemyMonHP)) * 256
        + u16::from(harness.gb.read_wram(ram::wEnemyMonHP + 1));
    eprintln!("MOVE 1: {outcome:?}, enemy HP {before} -> {after}");
    assert_eq!(outcome, MacroAbort::Done);
    assert!(after < before, "the enemy's HP did not drop: {before} -> {after}");
}

/// Start `kind` on its own, over agent A's real seam and the adapter's real ledgers.
///
/// [`run_real`] finds the macro in the scene's palette; this one hands the machine a pad with one
/// button on it, which is what lets a test drive `GO OBJECTIVE` -- a macro the palette row does not
/// carry -- without asserting anything about which buttons a scene deals.
fn run_kind(harness: &mut Harness, kind: MacroKind) -> MacroAbort {
    let mut machine = MacroMachine::new(0x517e_ed03);
    {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        let scene = state.scene();
        let palette =
        {
            let mut slots = [None; flybrain_gb::pokemon_red::macros::SLOTS];
            slots[usize::from(kind.slot())] = Some(MacroSpec::of(kind));
            Palette { scene, slots }
        };
        machine
            .start(&palette, MacroId(0), &mut state)
            .unwrap_or_else(|error| panic!("{} was refused: {:?}", kind.name(), error.reason));
    }
    loop {
        let mask = {
            let exits = AdapterLedger(&harness.adapter);
            let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
            machine.step(&mut state)
        };
        match mask {
            Some(mask) => harness.press(mask),
            None => break,
        }
    }
    machine.outcome().expect("a finished macro has an outcome").1
}

/// After the starter, the ways onward out of Pallet Town leave north instead of into a house.
///
/// This is the stream's own state (the operator, 2026-09-16: "fly is looping"): rank 5, standing in Pallet
/// Town, cycling through the town's houses and never taking the Route 1 connection. Three things
/// had to be true at once for that, and this test is all three on the cartridge:
///
/// - **the rung place.** Rung 6 is the parcel *delivered*, and the old catalog answered "Oak's
///   lab" -- the map the fly was standing beside -- so `GO OBJECTIVE` had nothing to aim at. The
///   errand starts at Viridian's mart, two maps north.
/// - **the route over maps.** A step off a map edge carried no destination at all, so no exit on
///   this map could ever be "the way to Viridian". The first hop now comes from the static map
///   graph and the edge knows which map is on the other side.
/// - **what "visited" means.** The adapter's `boundary` ledger pays for standing *beside* an
///   exit, so the north connection read as used up after one walk along the top row, while a
///   front door the fly had been through read as fresh.
///
/// The rank is advanced through the adapter's own checkpoint rather than by taking a starter on
/// the cartridge: the rung is an event flag in the lifetime ledger, and taking one for real means
/// winning the rival battle that follows it, which is a different test's business. Everything the
/// assertions read -- the place, the graph, the ledger -- is the real catalog answering for rank 5.
#[test]
fn after_the_starter_the_way_out_of_pallet_town_is_north() {
    let mut harness = Harness::boot(skip_without_rom!());
    harness.drive_until("pallet town", 200_000, 0x5eed_1234_5678_9abc, |_| None, |harness| {
        harness.map() == PALLET_TOWN && harness.adapter.safe_for_snapshot()
    });
    harness.settle();
    harness.resurvey();

    // Rank 5, as the stream is: the starter is a flag in the lifetime ledger.
    //
    // The lab's own map rung goes in with it, because the objective is the lowest rung this run
    // has *not* satisfied (`docs/design/macros.md` section 12.4) and a ledger that holds the
    // starter without the room it was taken in is not a save the cartridge can produce. Before
    // that change the objective was `rank + 1` and the inconsistency did not show.
    let mut state = harness.adapter.export_state();
    let seen = state["seen"].as_array_mut().expect("the ledger is an array");
    seen.push(serde_json::json!("EVENT_FOLLOWED_OAK_INTO_LAB"));
    seen.push(serde_json::json!("EVENT_GOT_STARTER"));
    seen.push(serde_json::json!(format!("map:{OAKS_LAB}")));
    harness.adapter.import_state(&state).expect("its own checkpoint, one flag richer");
    harness.settle();
    assert_eq!(harness.adapter.progress().rank, 5, "GOT A STARTER");
    assert_eq!(
        harness.adapter.objective().map(|place| place.map),
        Some(VIRIDIAN_MART),
        "the next rung is the parcel, and the parcel is at Viridian's mart"
    );

    // Both ways onward aim at the north edge, and neither is a door.
    let (route_goals, objective_tiles, on_pad) = {
        let exits = AdapterLedger(&harness.adapter);
        let mut state = PokeState::with_ledger(&mut harness.gb, &exits);
        let route: Vec<(u8, u8)> =
            ways(&mut state, Way::Route).iter().map(|exit| (exit.tile.x, exit.tile.y)).collect();
        let objective: Vec<(u8, u8)> =
            objective_goals(&mut state).iter().map(|aim| (aim.tile.x, aim.tile.y)).collect();
        let on_pad = precondition(MacroKind::GoObjective, &mut state);
        (route, objective, on_pad)
    };
    eprintln!("Pallet Town at rank 5: GO ROUTE {route_goals:?}, GO OBJECTIVE {objective_tiles:?}");
    assert!(on_pad, "GO OBJECTIVE is a button that exists here");
    assert!(!objective_tiles.is_empty());
    assert!(
        objective_tiles.iter().all(|(_, y)| *y == 0),
        "the way to Viridian is the town's north edge: {objective_tiles:?}"
    );
    assert!(
        route_goals.iter().all(|(_, y)| *y == 0 || *y == 17),
        "and GO ROUTE prefers a connection over any door: {route_goals:?}"
    );

    // Drive it on the cartridge. `GO OBJECTIVE` walks the fly north and never into one of the
    // town's houses, which is the whole of what the stream was doing wrong. It is driven to within
    // a few tiles of the exit rather than through it: pre-starter, the north path is Oak's own
    // script -- he stops the player and walks them to the lab -- and that script is what rung 4's
    // place is for, not something a macro test should be fighting.
    let houses = [REDS_HOUSE_1F, BLUES_HOUSE, OAKS_LAB];
    let (start_x, start_y) = harness.tile();
    let mut macros = 0;
    while harness.tile().1 > 3 {
        assert!(macros < 8, "still at {:?} after {macros} macros", harness.tile());
        let outcome = run_kind(&mut harness, MacroKind::GoObjective);
        macros += 1;
        for _ in 0..60 {
            harness.press(buttons::NONE);
        }
        assert!(
            !houses.contains(&harness.map()),
            "{outcome:?} took the fly indoors to {:#04x} with Route 1 unvisited",
            harness.map()
        );
        if harness.map() != PALLET_TOWN {
            break;
        }
    }
    let (x, y) = harness.tile();
    eprintln!("GO OBJECTIVE walked from {:?} to {:?} in {macros} macros", (start_x, start_y), (x, y));
    assert!(y < start_y, "the fly went north, toward Route 1 and the parcel beyond it");
}
