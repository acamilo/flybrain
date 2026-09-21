//! Scene detection and the state accessors, against the real cartridge.
//!
//! Gated on `FLY_ROM`, like `tests/rom.rs`: the ROM never enters this repository and these tests
//! skip cleanly without it.
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo test --release --test rom_scene -- --nocapture
//! ```
//!
//! ## What only a cartridge can check
//!
//! The synthetic traces in `pokemon_red/scene/tests.rs` and `pokemon_red/state/tests.rs` check
//! that the accessors read the bytes `docs/design/macros-wram.md` says they read. They cannot
//! check that those are the bytes the game writes. Five things here can only be checked against a
//! running cartridge, and each one is an assertion that would have caught a plausible mistake:
//!
//! 1. **The scene at each of the run's real states.** Title, the bedroom, the ground floor, Pallet
//!    Town, a dialogue in Oak's lab, and a battle.
//! 2. **The screen-to-map mapping.** The walkable predicate reads the tile the player stands on at
//!    screen (8, 9) and two screen tiles per map tile. If the origin or the stride were wrong the
//!    predicate would still answer, plausibly, and wrongly — so it is compared against a survey
//!    that walks the map with real button presses on throwaway emulators, tile by tile, the method
//!    `docs/design/room-escape.md` section 3 used to find the six presses that leave Red's ground
//!    floor.
//! 3. **That the collision list is reachable at all.** It lives in ROM, not WRAM; the claim that
//!    bank 0 is always mapped is a claim about the cartridge.
//! 4. **The party after the starter**, which is the first party the game ever writes.
//! 5. **A battle**, including whose turn it is, which no snapshot in the repository carries: the
//!    fixtures are a boot and two rooms of an empty house. Section "producing a battle" below
//!    records how this one is made.
//!
//! ## Producing the states
//!
//! There are no archived save states in the repository — `.gitignore` excludes `*.state` along
//! with the ROM — so these tests produce their own the way `examples/room_escape.rs` does: boot
//! the cartridge once, then drive it with a fixed-seed random walk and button pulses, stage by
//! stage, each stage ending on a condition rather than a frame count. The emulator is
//! deterministic and the walk is seeded, so the run is reproducible; the timeline is printed with
//! `--nocapture` and the observed frame counts are in `docs/design/macros-wram.md`.
//!
//! A random walk rather than steering, for the same reason `tests/rom.rs` gives: steering needs a
//! collision map to be any good, and using this crate's own walkable predicate to reach the state
//! that tests the walkable predicate would be circular. The walk needs no map knowledge.
//!
//! ### Producing a battle
//!
//! The first battle a fresh cartridge can reach is the rival's, in Oak's lab, and the path to it
//! is scripted: leaving Pallet Town to the north triggers Oak, who walks the player into the lab;
//! taking a starter from a ball needs one A press on the right tile and one on the YES of a
//! two-option menu; the rival then takes his and challenges the player where he stands. So the
//! walk that reaches Pallet Town, with A pulses added, reaches a trainer battle with no knowledge
//! of any of it.

use flybrain_gb::adapter::{GameAdapter, MemoryReader};
use flybrain_gb::pokemon_red::macros::state::{BattleKind, BattleMenu, Scene, Walkable};
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, SUPPORTED_ROM, scene, state};
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, buttons};

use std::collections::{BTreeMap, BTreeSet};

/// `constants/map_constants.asm`.
const REDS_HOUSE_1F: u8 = 0x25;
const REDS_HOUSE_2F: u8 = 0x26;
const PALLET_TOWN: u8 = 0x00;
const OAKS_LAB: u8 = 0x28;

/// One game frame at the Game Boy's real rate, for the adapter's brain clock.
const MS_PER_FRAME: f64 = 1000.0 / 59.7275;

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn emulator(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge")
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

/// The cartridge, the adapter and the brain clock, driven one frame at a time.
struct Run {
    emulator: Emulator,
    adapter: PokemonRedReward,
    ms: f64,
    frame: u32,
}

impl Run {
    fn new(rom: &[u8]) -> Self {
        let emulator = emulator(rom);
        assert_eq!(
            emulator.rom_sha256(),
            SUPPORTED_ROM,
            "FLY_ROM is not the cartridge the adapter is pinned to"
        );
        Self { emulator, adapter: PokemonRedReward::new(), ms: 0.0, frame: 0 }
    }

    /// One frame with these buttons held, then one adapter sample — the sim loop's order.
    fn step(&mut self, mask: u8) {
        self.emulator.set_buttons(mask);
        self.emulator.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        self.frame += 1;
        self.adapter.sample(&mut self.emulator, self.ms);
    }

    fn scene(&mut self) -> Scene {
        scene::detect(&mut self.emulator)
    }

    fn map(&mut self) -> u8 {
        self.emulator.read_wram(ram::wCurMap)
    }

    fn tile(&mut self) -> (u8, u8) {
        (self.emulator.read_wram(ram::wXCoord), self.emulator.read_wram(ram::wYCoord))
    }

    fn party_count(&mut self) -> u8 {
        self.emulator.read_wram(ram::wPartyCount)
    }

    /// `EVENT_FOLLOWED_OAK_INTO_LAB`, bit 0 of `wEventFlags`: whether Oak has walked the player
    /// into his lab, which is what makes the three balls takeable.
    fn followed_oak(&mut self) -> bool {
        self.emulator.read_wram(ram::wEventFlags) & 1 != 0
    }

    fn why(&mut self) -> String {
        scene::why_unknown(&mut self.emulator)
    }

    /// Mash Start and A through the intro and the naming screens, then B until the adapter reports
    /// a settled, dialogue-free overworld sample. `tests/rom.rs` explains the pattern: a held
    /// button is ignored, and the intro's A-mashing leaves a text box open that only B dismisses.
    fn boot_to_bedroom(&mut self) {
        for frame in 0..8_000u32 {
            let mask = match frame % 32 {
                0..=7 => buttons::START,
                16..=23 => buttons::A,
                _ => buttons::NONE,
            };
            self.step(mask);
            if frame > 3_000 && self.adapter.mode() == "OVERWORLD" {
                break;
            }
        }
        for frame in 0..12_000u32 {
            self.step(if frame % 24 < 8 { buttons::B } else { buttons::NONE });
            if self.adapter.safe_for_snapshot() {
                break;
            }
        }
        let settled = self.adapter.safe_for_snapshot();
        let mode = self.adapter.mode().to_string();
        assert!(settled, "the intro never settled (mode {mode}, {})", self.why());
        assert_eq!(self.adapter.map_id(), Some(u32::from(REDS_HOUSE_2F)));
    }

    /// Drive the cartridge until `done` says the stage is over, and return the frame it ended on.
    ///
    /// The pattern per 48-frame cycle is a direction, then B, then the stage's own button: B
    /// closes whatever a stray A opened, which is the difference between a walk that explores and
    /// one that stands in front of a bookshelf reading it four hundred thousand times (measured:
    /// an A-only walk never left the tile it entered Oak's lab on).
    ///
    /// `bias` aims the walk. With `None` the direction is uniform, which is what `tests/rom.rs`
    /// uses and needs no map knowledge; with `Some(direction)` three presses in four are that
    /// direction and the fourth is random, which is how a stage that has to leave a town by a
    /// particular edge gets there in a test's worth of frames. The bias is the test's, not the
    /// fly's: it is how the state under test is *produced*, never part of what is asserted.
    fn drive_until(
        &mut self,
        what: &str,
        budget: u32,
        seed: u64,
        press: u8,
        mut bias: impl FnMut(&mut Run) -> Option<u8>,
        mut done: impl FnMut(&mut Run) -> bool,
    ) -> u32 {
        let start = self.frame;
        let mut state = seed;
        let mut visited = BTreeSet::new();
        for frame in 0..budget {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let random = [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT]
                [((state >> 33) % 4) as usize];
            let step = match bias(self) {
                // Half biased, half random: the random half is what gets the walk around the
                // furniture the bias walks it into.
                Some(direction) if (state >> 41).is_multiple_of(2) => direction,
                _ => random,
            };
            let mask = match frame % 48 {
                0..=7 | 24..=31 => step,
                12..=15 => buttons::B,
                36..=39 => press,
                _ => buttons::NONE,
            };
            self.step(mask);
            let tile = self.tile();
            visited.insert((self.map(), tile.0, tile.1));
            if done(self) {
                eprintln!(
                    "{what}: reached at frame {} ({} frames into the stage, {} tiles visited)",
                    self.frame,
                    self.frame - start,
                    visited.len()
                );
                return self.frame;
            }
        }
        panic!(
            "{what}: not reached in {budget} frames (map {:#04x}, tile {:?}, party {}, \
             {} tiles visited, {})",
            self.map(),
            self.tile(),
            self.party_count(),
            visited.len(),
            self.why()
        );
    }
}

/// The uniform random walk, which needs no knowledge of any map.
fn no_bias(_: &mut Run) -> Option<u8> {
    None
}

/// Aim the walk at the starter.
///
/// This is the test's knowledge of the game, not the fly's: it decides which states get
/// *produced*, and nothing it returns is ever asserted. Three facts, each from the disassembly:
///
/// - `PalletTownDefaultScript` triggers on `wYCoord == 1`, so reaching the top row of Pallet Town
///   anywhere along it is enough; Oak then walks the player into the lab himself.
/// - the two house doors in Pallet Town are at x 5 and x 13 (`warp_event 5, 5` and
///   `warp_event 13, 5`) and a door warps on the step onto it, so an UP bias from the spawn walks
///   straight back indoors. Step off those columns first.
/// - the three balls are at the top of Oak's lab, and they are only takeable once
///   `EVENT_FOLLOWED_OAK_INTO_LAB` is set, so the lab is somewhere to leave before that and
///   somewhere to walk north in after it.
fn toward_the_starter(run: &mut Run) -> Option<u8> {
    let map = run.map();
    match map {
        PALLET_TOWN => {
            Some(if matches!(run.tile().0, 5 | 13) { buttons::LEFT } else { buttons::UP })
        }
        OAKS_LAB if run.followed_oak() => Some(buttons::UP),
        _ => Some(buttons::DOWN),
    }
}

/// Walk a map with real button presses on throwaway emulators.
///
/// `examples/room_escape.rs`'s `survey`, with the presses that left the map recorded per tile.
/// Returns every tile reachable from the starting state and, for each tile, the presses that leave
/// the map.
type Survey = (
    BTreeSet<(u8, u8)>,
    BTreeMap<(u8, u8), Vec<&'static str>>,
    BTreeMap<(u8, u8), Vec<u8>>,
);

fn survey(rom: &[u8], state: &[u8]) -> Survey {
    let read = |probe: &mut Emulator| {
        (
            probe.read8(ram::wCurMap),
            probe.read8(ram::wXCoord),
            probe.read8(ram::wYCoord),
        )
    };
    // Hold the direction until the coordinates change, then release and let the machine settle
    // before the state is kept. Two numbers here were measured rather than guessed:
    //
    // - **120 frames**, not `examples/room_escape.rs`'s 48. A press the player is not already
    //   facing turns it first and steps second, and the pair took 53 frames from the ground
    //   floor's staircase: a 48-frame window reads that as a wall.
    // - **the release.** A state exported while a button was held does not respond to that
    //   button, or to any other, being held again from the frame after the import — measured on
    //   this cartridge, all four directions, 64 frames each, no movement. Releasing and running
    //   twenty frames before the export fixes it, so every state this survey keeps is settled.
    let step = |probe: &mut Emulator, mask: u8| {
        let before = read(probe);
        probe.set_buttons(mask);
        let mut moved = false;
        for _ in 0..120 {
            probe.run_frame().expect("a frame should complete");
            if read(probe) != before {
                moved = true;
                break;
            }
        }
        probe.set_buttons(buttons::NONE);
        for _ in 0..20 {
            probe.run_frame().expect("a frame should complete");
        }
        (read(probe), moved)
    };
    let restore = |state: &[u8]| {
        let mut probe = emulator(rom);
        probe.import_state(state).expect("a surveyed state should import");
        probe
    };

    let mut first = restore(state);
    let (start_map, x, y) = read(&mut first);
    let mut reachable = BTreeSet::new();
    let mut exits: BTreeMap<(u8, u8), Vec<&'static str>> = BTreeMap::new();
    let mut states: BTreeMap<(u8, u8), Vec<u8>> = BTreeMap::new();
    reachable.insert((x, y));
    states.insert((x, y), state.to_vec());
    let mut queue = vec![(x, y)];
    while let Some(tile) = queue.pop() {
        for (name, mask) in [
            ("up", buttons::UP),
            ("down", buttons::DOWN),
            ("left", buttons::LEFT),
            ("right", buttons::RIGHT),
        ] {
            let mut probe = restore(&states[&tile]);
            let ((map, x, y), moved) = step(&mut probe, mask);
            if !moved {
                continue;
            }
            if map != start_map {
                exits.entry(tile).or_default().push(name);
                continue;
            }
            if reachable.insert((x, y)) {
                states.insert((x, y), probe.export_state().expect("state export"));
                queue.push((x, y));
            }
        }
    }
    (reachable, exits, states)
}

#[test]
fn the_title_screen_is_the_title_scene_and_nothing_else() {
    let rom = skip_without_rom!();
    let mut run = Run::new(&rom);

    // A cold boot's WRAM is pseudorandom (binjgb seeds it), so the first frames can read a garbage
    // game-timer bit; `tests/rom.rs` records the same caveat. Let the boot code clear WRAM first.
    for _ in 0..600 {
        run.step(buttons::NONE);
    }

    let mut scenes = BTreeSet::new();
    for _ in 0..3_000 {
        run.step(buttons::NONE);
        scenes.insert(format!("{:?}", run.scene()));
    }
    eprintln!("title: scenes over 3,000 idle frames after the boot logo = {scenes:?}");
    assert_eq!(
        scenes.len(),
        1,
        "the title screen should be one scene and one scene only"
    );
    assert_eq!(run.scene(), Scene::Title);
    // And the adapter agrees, which is what keeps the palette's Title and the page's BOOT one
    // thing rather than two.
    assert_eq!(run.adapter.mode(), "BOOT");

    // Nothing else is readable there: no player, no party, no walkable tile.
    assert_eq!(state::player(&mut run.emulator), None);
    assert!(state::party(&mut run.emulator).mons.is_empty());
    assert_eq!(state::walkable(&mut run.emulator, 3, 6), Walkable::Unknown);
}

#[test]
fn the_bedroom_the_ground_floor_and_pallet_town_are_the_overworld() {
    let rom = skip_without_rom!();
    let mut run = Run::new(&rom);
    run.boot_to_bedroom();

    // 1. The bedroom.
    assert_eq!(run.scene(), Scene::Overworld, "{}", run.why());
    let player = state::player(&mut run.emulator).expect("a loaded map");
    assert_eq!(player.map, REDS_HOUSE_2F);
    eprintln!("bedroom: player at ({}, {}) facing {:?}", player.x, player.y, player.facing);
    assert_eq!(
        state::map_size(&mut run.emulator).map(|size| (size.width, size.height)),
        Some((8, 8)),
        "Red's bedroom is 4 by 4 blocks"
    );
    // The party is empty before the starter, and that is a fact about the cartridge rather than
    // about the accessor: `wPartyCount` is zero and the six party structs are untouched.
    assert!(state::party(&mut run.emulator).mons.is_empty());
    assert_eq!(state::money(&mut run.emulator), 3_000, "the starting wallet");
    assert!(state::bag(&mut run.emulator).is_empty());
    assert!(state::battle(&mut run.emulator).is_none());
    assert!(state::start_menu(&mut run.emulator).is_none());
    assert!(state::shop(&mut run.emulator).is_none());
    assert!(state::pc(&mut run.emulator).is_none());

    // The bedroom's one warp is the staircase, `warp_event 7, 1, REDS_HOUSE_1F, 3`.
    let warps = state::warps(&mut run.emulator);
    eprintln!("bedroom: warps {warps:?}");
    assert_eq!(warps.len(), 1);
    assert_eq!((warps[0].x, warps[0].y), (7, 1));
    assert_eq!(warps[0].destination_map, REDS_HOUSE_1F);
    // `warp_event`'s fourth argument is one-based and the byte is not: `MACRO warp_event` emits
    // `db \2, \1, \4 - 1, \3`, so the declared warp 3 is stored as 2.
    assert_eq!(warps[0].destination_warp, 2, "the declared warp 3, stored zero-based");
    assert!(
        !state::connections(&mut run.emulator).any(),
        "an indoor map has no connected edges"
    );

    // 2. The ground floor.
    run.drive_until("downstairs", 40_000, 0x5eed_1234_5678_9abc, buttons::B, no_bias, |run| {
        run.map() == REDS_HOUSE_1F && run.adapter.safe_for_snapshot()
    });
    assert_eq!(run.scene(), Scene::Overworld, "{}", run.why());
    assert_eq!(state::player(&mut run.emulator).map(|player| player.map), Some(REDS_HOUSE_1F));

    // 3. Pallet Town.
    run.drive_until("pallet town", 80_000, 0x1234_5678_9abc_def0, buttons::B, no_bias, |run| {
        run.map() == PALLET_TOWN && run.adapter.safe_for_snapshot()
    });
    assert_eq!(run.scene(), Scene::Overworld, "{}", run.why());
    let player = state::player(&mut run.emulator).expect("a loaded map");
    assert_eq!(player.map, PALLET_TOWN);
    assert_eq!(
        state::map_size(&mut run.emulator).map(|size| (size.width, size.height)),
        Some((20, 18)),
        "Pallet Town is 10 by 9 blocks"
    );
    let connections = state::connections(&mut run.emulator);
    eprintln!("pallet town: connections {connections:?}, warps {}", state::warps(&mut run.emulator).len());
    assert!(connections.north, "Route 1 is north of Pallet Town");
    assert!(connections.south, "Route 21 is south of it");
    assert!(!connections.east && !connections.west);

    // Outdoors the map is wider than the screen's window, so the far side of the town is not
    // answerable and must say so rather than guess.
    let far = state::walkable(&mut run.emulator, 19, 17);
    eprintln!("pallet town: walkable(19, 17) = {far:?} from ({}, {})", player.x, player.y);
    if player.x.abs_diff(19) > 5 || player.y.abs_diff(17) > 4 {
        assert_eq!(far, Walkable::Unknown);
    }
}

#[test]
fn the_walkable_predicate_agrees_with_a_survey_of_the_ground_floor() {
    let rom = skip_without_rom!();
    let mut run = Run::new(&rom);
    run.boot_to_bedroom();
    run.drive_until("downstairs", 40_000, 0x5eed_1234_5678_9abc, buttons::B, no_bias, |run| {
        run.map() == REDS_HOUSE_1F && run.adapter.safe_for_snapshot()
    });
    // Release and settle before exporting: a state exported mid-press does not respond to a held
    // button afterwards (measured; see `survey`).
    for _ in 0..20 {
        run.step(buttons::NONE);
    }
    let ground_floor = run.emulator.export_state().expect("state export");

    // The survey: real presses, throwaway emulators, no map knowledge.
    let (reachable, exits, states) = survey(&rom, &ground_floor);
    eprintln!(
        "survey: map {REDS_HOUSE_1F:#04x} has {} reachable tiles and {} tiles with an exit",
        reachable.len(),
        exits.len()
    );
    assert_eq!(
        reachable.len(),
        48,
        "`docs/design/room-escape.md` section 3 counted 48 reachable tiles"
    );

    // The six presses that leave the map, exactly as section 3 tabulates them.
    let mut found: Vec<((u8, u8), &'static str)> = exits
        .iter()
        .flat_map(|(tile, presses)| presses.iter().map(move |press| (*tile, *press)))
        .collect();
    found.sort();
    let mut expected: Vec<((u8, u8), &'static str)> = vec![
        ((6, 1), "right"),
        ((7, 2), "up"),
        ((2, 6), "down"),
        ((3, 6), "down"),
        ((2, 7), "down"),
        ((3, 7), "down"),
    ];
    expected.sort();
    eprintln!("survey: exits {found:?}");
    assert_eq!(found, expected, "the six presses that leave Red's ground floor");

    // Now the predicate, against the survey, from every one of the 48 tiles. This is the
    // assertion that catches a wrong screen origin or a wrong stride: those still answer, and
    // still answer plausibly, because they read a real tile id from the wrong place.
    //
    // One rule of the emulator matters here and is worth recording: `wTileMap` is only the
    // current view on a *running* machine. Read straight after `import_state`, with no frame in
    // between, it holds the view from wherever the state was taken, and the predicate answers
    // about the wrong tiles — measured, four tiles of this map disagreed exactly that way. So
    // every restored state is given twenty idle frames before it is read, which is also what the
    // sim loop does by construction: the adapter samples after a completed frame.
    let mut probe = emulator(&rom);
    let mut checked = 0usize;
    let mut blocked_by_an_npc = Vec::new();
    let mut mismatches = Vec::new();
    let mut size = None;
    for (tile, state) in &states {
        probe.import_state(state).expect("a surveyed state should import");
        for _ in 0..20 {
            probe.set_buttons(buttons::NONE);
            probe.run_frame().expect("a frame should complete");
        }
        let player = state::player(&mut probe).expect("a loaded map");
        assert_eq!((player.x, player.y), *tile, "the survey's state for {tile:?}");
        let map = state::map_size(&mut probe).expect("a loaded map");
        size = Some(map);
        let npcs = state::npcs(&mut probe);
        for (press, (dx, dy)) in
            [("up", (0, -1)), ("down", (0, 1)), ("left", (-1, 0)), ("right", (1, 0))]
        {
            let (nx, ny) = (i32::from(tile.0) + dx, i32::from(tile.1) + dy);
            if nx < 0 || ny < 0 {
                // A tile coordinate cannot be negative, so there is nothing to ask about.
                continue;
            }
            if nx >= i32::from(map.width) || ny >= i32::from(map.height) {
                // Off the map. The two doormats leave it downwards from the bottom row, and they
                // do it by warping rather than by stepping onto a tile, so there is no walkable
                // tile out there to find.
                assert_eq!(
                    state::walkable(&mut probe, nx as u8, ny as u8),
                    Walkable::No,
                    "off the map from {tile:?} pressing {press}"
                );
                continue;
            }
            let (nx, ny) = (nx as u8, ny as u8);
            let moved = reachable.contains(&(nx, ny))
                || exits.get(tile).is_some_and(|presses| presses.contains(&press));
            let answer = state::walkable(&mut probe, nx, ny);
            checked += 1;
            match (answer, moved) {
                (Walkable::Yes, true) | (Walkable::No, false) => {}
                // A tile whose id is passable but which an NPC is standing on: the predicate is
                // about tiles and says so, and `npcs` is the accessor that covers the difference.
                (Walkable::Yes, false)
                    if npcs.iter().any(|npc| (npc.x, npc.y) == (nx, ny)) =>
                {
                    blocked_by_an_npc.push((*tile, press, (nx, ny)));
                }
                _ => mismatches.push((*tile, press, (nx, ny), answer, moved)),
            }
        }
    }
    eprintln!(
        "survey: the predicate agreed with the survey on {checked} presses from {} tiles; \
         {} blocked by an NPC standing on a passable tile",
        states.len(),
        blocked_by_an_npc.len()
    );
    if !blocked_by_an_npc.is_empty() {
        eprintln!("survey: NPC-blocked {blocked_by_an_npc:?}");
    }
    assert!(mismatches.is_empty(), "the predicate disagreed with the survey: {mismatches:?}");
    assert!(checked >= 150, "only {checked} presses were compared");
    let size = size.expect("at least one surveyed tile");
    assert_eq!((size.width, size.height), (8, 8), "Red's ground floor is 4 by 4 blocks");

    // And the tiles the six presses act on, named: the staircase, which both of the presses that
    // leave upstairs act on, and the two doormats, which the two presses from the row above act
    // on. `GO EXIT` needs exactly these to be walkable.
    probe.import_state(&states[&(6, 1)]).expect("state import");
    for _ in 0..20 {
        probe.run_frame().expect("a frame should complete");
    }
    assert_eq!(state::walkable(&mut probe, 7, 1), Walkable::Yes, "the staircase at (7, 1)");
    let warps = state::warps(&mut probe);
    eprintln!("survey: warps {warps:?}");
    assert_eq!(warps.len(), 3, "two doormats and the staircase");
    assert!(warps.iter().any(|warp| (warp.x, warp.y) == (7, 1) && warp.destination_map == REDS_HOUSE_2F));
    // `warp_event 2, 7, LAST_MAP, 1` and `warp_event 3, 7, LAST_MAP, 1`: the doormats go back to
    // whichever map the player came from, which the cartridge writes as $ff.
    assert_eq!(
        warps.iter().filter(|warp| warp.y == 7 && warp.destination_map == 0xff).count(),
        2,
        "the two doormats"
    );
    // `warp_event`'s fourth argument is one-based and the byte is not: the macro emits `\4 - 1`.
    assert!(
        warps.iter().all(|warp| warp.destination_warp == 0),
        "every warp on this map declares destination warp 1, stored as 0: {warps:?}"
    );

    probe.import_state(&states[&(3, 6)]).expect("state import");
    for _ in 0..20 {
        probe.run_frame().expect("a frame should complete");
    }
    for mat in [(2u8, 7u8), (3, 7)] {
        assert_eq!(
            state::walkable(&mut probe, mat.0, mat.1),
            Walkable::Yes,
            "the doormat at {mat:?}"
        );
    }
    assert!(
        !state::connections(&mut probe).any(),
        "an indoor map has no connected edges: the way out is a warp"
    );
}

#[test]
fn oaks_lab_produces_a_dialog_a_starter_and_a_battle() {
    let rom = skip_without_rom!();
    let mut run = Run::new(&rom);
    run.boot_to_bedroom();
    run.drive_until("pallet town", 120_000, 0x5eed_1234_5678_9abc, buttons::B, no_bias, |run| {
        run.map() == PALLET_TOWN && run.adapter.safe_for_snapshot()
    });

    // One stage carries the rest of the run: Oak's script, the lab, the dialogue in it and the
    // starter. Oak triggers on `wYCoord == 1` — the top row of Pallet Town, anywhere along it —
    // and then walks the player into the lab himself, where the three balls are. The dialogue is
    // recorded as it goes past rather than aimed at, because there is no state in which a lab
    // without a dialogue in it is interesting: Oak talks the whole way through.
    let mut lab_dialog = None;
    let mut lab_dialog_state = None;
    let mut counted = None;
    run.drive_until("a starter", 900_000, 0x1357_9bdf_2468_ace0, buttons::A, toward_the_starter, |run| {
        if run.party_count() > 0 && counted.is_none() {
            counted = Some(run.frame);
        }
        if run.map() == OAKS_LAB && run.scene() == Scene::Dialog && lab_dialog.is_none() {
            lab_dialog = Some(run.frame);
            let text = state::text_box(&mut run.emulator);
            lab_dialog_state = Some((
                text.open,
                text.waiting,
                state::player(&mut run.emulator).is_some(),
                state::walkable(&mut run.emulator, 4, 4),
            ));
        }
        // `wPartyCount` leads the party struct: `AddPartyMon` writes the count first and fills
        // the 44 bytes over the frames after it, so the count alone is not a Pokémon yet. This
        // is the gotcha `docs/design/macros-wram.md` records for the party accessor, measured
        // here as the gap between `counted` and this condition.
        state::party(&mut run.emulator)
            .mons
            .first()
            .is_some_and(|mon| mon.species != 0 && mon.level != 0 && mon.max_hp != 0)
    });
    let dialog = lab_dialog.expect("a dialogue in Oak's lab");
    let (open, waiting, map_loaded, walkable_under_the_box) =
        lab_dialog_state.expect("the dialogue's readings");
    eprintln!("dialog: first seen in Oak's lab at frame {dialog}");
    assert!(open && waiting, "a dialog is an open box that waits: open {open} waiting {waiting}");
    // A dialog is not the overworld, however much of the map is still underneath it.
    assert!(map_loaded, "the map is still loaded during a dialogue");
    assert_eq!(
        walkable_under_the_box,
        Walkable::Unknown,
        "the screen buffer holds the text box, not the map"
    );

    let counted = counted.expect("the party count changed");
    let party = state::party(&mut run.emulator);
    eprintln!(
        "starter: wPartyCount became 1 at frame {counted}, the species was written {} frames \
         later; party {party:?}",
        run.frame - counted
    );
    assert!(
        run.frame > counted,
        "the count and the species landing on the same frame would make the note in \
         macros-wram.md wrong"
    );
    assert_eq!(party.mons.len(), 1);
    let starter = party.mons[0];
    // Species ids are pokered's *internal* indices, not Pokédex numbers: BULBASAUR is $99,
    // CHARMANDER $b0 and SQUIRTLE $b1 (`constants/pokemon_constants.asm`), while the adapter's
    // Pokédex bitset is by Pokédex number. The accessor passes the byte through and
    // `docs/design/macros-wram.md` says which numbering it is; asserting 1, 4 or 7 here is the
    // mistake this assertion exists to prevent.
    assert!(
        [0x99u8, 0xb0, 0xb1].contains(&starter.species),
        "the starter is Bulbasaur ($99), Charmander ($b0) or Squirtle ($b1), got {:#04x}",
        starter.species
    );
    assert_eq!(starter.level, 5, "every starter is level 5");
    assert_eq!(starter.hp, starter.max_hp, "a gift Pokémon arrives at full HP");
    assert!((18..=21).contains(&starter.max_hp), "max HP {}", starter.max_hp);
    assert!(!starter.fainted());
    assert!(
        (starter.hp_fraction() - 1.0).abs() < 1e-12,
        "fraction {}",
        starter.hp_fraction()
    );
    // Level-1 learnsets: Bulbasaur and Squirtle know TACKLE (33), Charmander SCRATCH (10), and
    // all three know a second move — GROWL (45) or TAIL WHIP (39). Both damaging moves have 35
    // PP, which is what makes the packed PP byte readable here.
    let first = starter.moves[0].expect("a starter knows at least one move");
    assert!([33u8, 10].contains(&first.id), "first move {}", first.id);
    assert_eq!(first.pp, 35, "TACKLE and SCRATCH both have 35 PP");
    assert_eq!(first.pp_up, 0);
    let second = starter.moves[1].expect("a starter knows two moves");
    assert!([45u8, 39].contains(&second.id), "second move {}", second.id);
    assert!(starter.moves[2].is_none() && starter.moves[3].is_none());

    // The battle: the rival takes his starter and challenges the player where he stands.
    run.drive_until("a battle", 400_000, 0x2468_ace0_1357_9bdf, buttons::A, no_bias, |run| {
        matches!(run.scene(), Scene::Battle { .. })
    });
    let scene = run.scene();
    assert!(matches!(scene, Scene::Battle { .. }), "{scene:?}");
    eprintln!("battle: first frame {scene:?}, {:?}", state::battle(&mut run.emulator));
    // On the frame a battle starts the combatants are not written yet — the reward adapter's own
    // comment says the same about `wEnemyMonSpecies` — so the scene is a battle before there is
    // anything in it. That is the right order for the detector and it is why the palette's
    // `own_turn` is a separate question from "is this a battle".
    assert_eq!(run.adapter.mode(), "BATTLE");
    assert_eq!(
        state::walkable(&mut run.emulator, 4, 4),
        Walkable::Unknown,
        "a battle is never the overworld, whatever is on screen"
    );

    // Wait for the fly's turn: the top-level FIGHT / PKMN / ITEM / RUN menu, which arrives after
    // the opening text. B advances text and does nothing to that menu, so it cannot overshoot
    // into the move list the way A would.
    let mut own_turn = None;
    for frame in 0..8_000u32 {
        run.step(if frame % 24 < 8 { buttons::B } else { buttons::NONE });
        if let Scene::Battle { own_turn: true, .. } = run.scene() {
            own_turn = Some(run.frame);
            break;
        }
    }
    let own_turn = own_turn.unwrap_or_else(|| {
        panic!("the battle menu never opened ({}, {:?})", run.why(), run.scene())
    });
    let battle = state::battle(&mut run.emulator).expect("a battle");
    eprintln!("battle: the menu opened at frame {own_turn}, {battle:?}");
    assert!(battle.own_turn && !battle.forced_switch);
    assert_eq!(battle.kind, BattleKind::Trainer, "the rival is a trainer, not a wild Pokémon");
    assert_eq!(
        battle.menu,
        BattleMenu::Main { cursor: 0 },
        "a fresh battle menu starts on FIGHT"
    );

    let enemy = battle.enemy.expect("an opposing Pokémon");
    assert!(
        [0x99u8, 0xb0, 0xb1].contains(&enemy.species),
        "the rival's starter, got {:#04x}",
        enemy.species
    );
    assert_ne!(enemy.species, starter.species, "the rival takes the other one");
    assert_eq!(enemy.level, 5);
    assert_eq!(enemy.hp, enemy.max_hp, "a fresh battle");
    let own = battle.own.expect("the Pokémon that is out");
    assert_eq!(own.species, starter.species);
    assert_eq!(own.max_hp, starter.max_hp, "the battler is a copy of the party entry");
    assert_eq!(own.moves[0].map(|first| first.id), starter.moves[0].map(|first| first.id));
    // In a battle the party reports which slot is out.
    assert_eq!(state::party(&mut run.emulator).active, Some(0));

    // The cursor is real: RIGHT moves it to the other column, which is ITEM.
    for frame in 0..72u32 {
        run.step(if frame % 24 < 8 { buttons::RIGHT } else { buttons::NONE });
    }
    let battle = state::battle(&mut run.emulator).expect("a battle");
    eprintln!("battle: after RIGHT, menu {:?}", battle.menu);
    assert_eq!(battle.menu, BattleMenu::Main { cursor: 2 }, "RIGHT from FIGHT is ITEM");
    assert!(battle.own_turn, "the menu is still the fly's turn");

    // And DOWN from ITEM is RUN.
    for frame in 0..72u32 {
        run.step(if frame % 24 < 8 { buttons::DOWN } else { buttons::NONE });
    }
    let battle = state::battle(&mut run.emulator).expect("a battle");
    eprintln!("battle: after DOWN, menu {:?}", battle.menu);
    assert_eq!(battle.menu, BattleMenu::Main { cursor: 3 }, "DOWN from ITEM is RUN");

    // The adapter's own mode agrees, so the palette and the page cannot disagree about a battle.
    assert_eq!(run.adapter.mode(), "BATTLE");
}
