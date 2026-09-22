//! The whole-map walkability grid against the cartridge (`docs/design/macros.md` section 15).
//!
//! Gated on `FLY_ROM` and on a `FLYSIM01` checkpoint, and skips cleanly without either. The
//! checkpoints live outside the tree (`.local/` is not tracked) and are the release container's
//! own states, pulled read-only:
//!
//! ```sh
//! FLY_ROM="$HOME/…/Pokemon Red (U) [S][BF].gb" \
//!   FLY_GRID_CHECKPOINT=.local/checkpoints/rank9-viridian-forest.checkpoint \
//!   cargo test --release -p flysim --test rom_map_grid -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! The unit tests in `pokemon_red/mapgrid/tests.rs` decode a made-up tileset, and the ones in
//! `pokemon_red/state/tests.rs` decode synthetic WRAM with a synthetic blockset in a synthetic
//! bank. Neither can say that those are the bytes the *game* writes: a wrong border, a wrong
//! stride or the wrong corner of a block all still answer, and answer plausibly. Three things here
//! need a running cartridge:
//!
//! 1. **the decode against the window predicate**, on every tile of the map the ten-by-nine
//!    screen window can answer for — the same comparison the reader makes before it trusts
//!    itself, done over the whole window rather than the player's own neighbourhood;
//! 2. **the decode against real presses** (the survey method of `docs/design/room-escape.md`
//!    section 3): every tile the survey can stand on is `Yes`, and every step it refuses is `No`,
//!    a directed wall, or a tile a sprite is standing on;
//! 3. **the walks**: that `GO FRONTIER` aims at ground outside the window and reaches it, and that
//!    a way out of the map is one plan away rather than a re-plan at every window edge.

use std::collections::{BTreeMap, BTreeSet};

use flybrain_gb::macros::{MacroPalette, Started};
use flybrain_gb::pokemon_red::macros::cartridge::{MacroState, Tile};
use flybrain_gb::pokemon_red::macros::palette::Palette;
use flybrain_gb::pokemon_red::macros::state::{Facing, MapGrid, Walkable};
use flybrain_gb::pokemon_red::macros::{PokemonPalette, path};
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, scene, state};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, buttons,
};

/// One game frame at the Game Boy's real rate, for the adapter's brain clock.
const MS_PER_FRAME: f64 = 1000.0 / 59.7275;

/// Frames a restored state is given before anything reads the screen buffer.
///
/// `wTileMap` is the current view only on a *running* machine: read straight after
/// `import_state`, with no frame in between, it holds the view from wherever the state was taken
/// (`docs/design/macros-wram.md`). Twenty is what `tests/rom_scene.rs` gives every restored state
/// and what the survey below gives every one of its own.
const SETTLE_FRAMES: u32 = 20;

/// The seed the macro tests run the executor with. Nothing here depends on it: the slot is chosen
/// by name, not by a readout.
const SEED: u32 = 20_260_922;

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_GRID_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_TRAP_CHECKPOINT"))
        .or_else(|| std::env::var_os("FLY_MACRO_CHECKPOINT"))?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

macro_rules! skip_without {
    () => {
        match (rom(), checkpoint()) {
            (Some(rom), Some(checkpoint)) => (rom, checkpoint),
            (None, _) => {
                eprintln!("skipped: FLY_ROM is not set");
                return;
            }
            (_, None) => {
                eprintln!("skipped: no FLY_GRID_CHECKPOINT / FLY_TRAP_CHECKPOINT");
                return;
            }
        }
    };
}

fn emulator(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge")
}

/// A cartridge resumed from a checkpoint, with the reward ledger the checkpoint carried and the
/// macro palette the sim loop runs.
struct Game {
    gb: Emulator,
    adapter: PokemonRedReward,
    palette: PokemonPalette,
    ms: f64,
}

impl Game {
    fn resume(rom: &[u8], checkpoint: &flysim::store::Checkpoint) -> Self {
        let mut gb = emulator(rom);
        let mut adapter = PokemonRedReward::new();
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
        adapter.import_state(&checkpoint.runtime.reward).expect("the checkpoint's reward ledger");
        let mut game =
            Self { gb, adapter, palette: PokemonPalette::new(SEED), ms: 0.0 };
        game.settle_overworld();
        if let Some(target) = std::env::var("FLY_GRID_TO_MAP")
            .ok()
            .and_then(|value| u8::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        {
            game.reach_map(target);
        }
        game
    }

    /// Drive the cartridge with raw presses until the fly is standing on `target`.
    ///
    /// The test's knowledge of the game, not the fly's, and the same device
    /// `tests/rom_scene.rs`'s biased walk is: it decides which state gets *produced* and nothing
    /// it does is asserted. What it is for is the second map of the survey
    /// (`docs/design/macros.md` section 15 asks for two): no checkpoint in `.local/` is standing
    /// on Pallet Town, and Oak's lab is one door south of it.
    ///
    /// A biased random walk rather than the macros, deliberately: driving the state under test
    /// into place with the macros under test is circular, and a walk needs no map knowledge. The
    /// cycle is `tests/rom_scene.rs`'s — a direction, then B, which closes whatever a stray press
    /// opened.
    fn reach_map(&mut self, target: u8) {
        let mut seed = u64::from(SEED);
        let toward = if target < self.map() { buttons::DOWN } else { buttons::UP };
        let mut arrived: Option<Tile> = None;
        let mut walked = 0;
        for frame in 0..90_000u32 {
            if self.map() == target && walked > 0 {
                eprintln!(
                    "reached map {target:#04x} after {frame} frames, {walked} tiles into it, at \
                     {:?}",
                    self.tile()
                );
                self.settle_overworld();
                return;
            }
            // A few tiles *into* the map, not the doormat: a warp writes the map id before the
            // header and the blocks, so the arrival frame itself is the torn one
            // (`pokemon_red::state::still_the_loaded_map`).
            if self.map() == target && self.tile() != arrived.unwrap_or(Tile::new(255, 255)) {
                match arrived {
                    None => arrived = Some(self.tile()),
                    Some(_) => walked += 1,
                }
            }
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let random = [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT]
                [((seed >> 33) % 4) as usize];
            // Half biased, half random: the random half is what gets the walk around the
            // furniture the bias walks it into.
            let step = if (seed >> 41).is_multiple_of(2) { toward } else { random };
            let mask = match frame % 48 {
                0..=7 | 24..=31 => step,
                12..=15 => buttons::B,
                _ => buttons::NONE,
            };
            self.frame(mask);
        }
        assert_eq!(self.map(), target, "FLY_GRID_TO_MAP: never reached map {target:#04x}");
    }

    /// Settle a restored state until the cartridge is showing the overworld, pressing nothing.
    ///
    /// Two reasons, both measured. `wTileMap` is the current view only on a *running* machine, so
    /// every restored state needs frames before anything reads the screen
    /// (`docs/design/macros-wram.md`); and a checkpoint is a frame of a live run, which can be
    /// mid-step, mid-warp or inside a script that is walking the fly — Viridian City's own
    /// checkpoint reads `Scene::Unknown` and its screen buffer is a tile out of step with its
    /// coordinates, which is exactly the frame [`state::map_grid`] refuses rather than decodes.
    /// A grid is a question about the overworld, so the test asks it there.
    fn settle_overworld(&mut self) {
        for _ in 0..1_200 {
            self.frame(buttons::NONE);
            if matches!(scene::detect(&mut self.gb), scene::Scene::Overworld) {
                self.settle(SETTLE_FRAMES);
                return;
            }
        }
        eprintln!("the restored state never settled into the overworld");
    }

    /// Run frames with nothing held, sampling the adapter and observing the palette as the sim
    /// loop does.
    fn settle(&mut self, frames: u32) {
        for _ in 0..frames {
            self.frame(buttons::NONE);
        }
    }

    /// One frame with these buttons held, then the adapter's sample and the palette's observation
    /// — the sim loop's order.
    fn frame(&mut self, mask: u8) {
        self.gb.set_buttons(mask);
        self.gb.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        self.adapter.sample(&mut self.gb, self.ms);
        let ledger = AdapterLedger(&self.adapter);
        self.palette.clock(self.ms);
        let _ = self.palette.observe(&mut self.gb, &ledger);
    }

    fn map(&mut self) -> u8 {
        self.gb.read_wram(ram::wCurMap)
    }

    fn tile(&mut self) -> Tile {
        Tile::new(self.gb.read_wram(ram::wXCoord), self.gb.read_wram(ram::wYCoord))
    }

    /// The decoded grid, or the reason there is none.
    fn grid(&mut self) -> Result<MapGrid, state::GridRefusal> {
        state::map_grid(&mut self.gb)
    }

    /// Run `body` with the state the macros read, over this game's ledgers.
    fn with_state<T>(&mut self, body: impl FnOnce(&mut dyn MacroState) -> T) -> T {
        let ledger = AdapterLedger(&self.adapter);
        let mut poke = state::PokeState::with_ledger(&mut self.gb, &ledger);
        body(&mut poke)
    }

    /// The slot this scene binds `name` to, or `None` when the button is not on the pad.
    fn slot(&mut self, name: &str) -> Option<u8> {
        self.with_state(|state| {
            let scene = state.scene();
            let palette = Palette::for_scene(scene, state);
            palette
                .slots
                .iter()
                .enumerate()
                .find(|(_, spec)| spec.is_some_and(|spec| spec.name == name))
                .map(|(slot, _)| slot as u8)
        })
    }

    /// Start `name` and run it to its outcome, returning the outcome's label and the frames spent.
    ///
    /// The slot is chosen by name rather than by a readout, which is what makes this a test of the
    /// macro instead of a test of the decoder: `docs/design/macros.md` section 12 has the fly
    /// choosing, and what is under test here is what the chosen walk does.
    fn run_macro(&mut self, name: &str) -> Option<(String, u32)> {
        let slot = self.slot(name)?;
        let ledger = AdapterLedger(&self.adapter);
        match self.palette.start(slot, &mut self.gb, &ledger) {
            Started::Refused { reason, .. } => Some((format!("refused: {reason}"), 0)),
            Started::Running(_) => {
                let mut frames = 0;
                loop {
                    let ledger = AdapterLedger(&self.adapter);
                    let mask = self.palette.step(&mut self.gb, &ledger);
                    match mask {
                        None => break,
                        Some(mask) => {
                            self.frame(mask);
                            frames += 1;
                        }
                    }
                    if frames > 4_000 {
                        break;
                    }
                }
                let outcome = self
                    .palette
                    .take_finished()
                    .map(|(_, outcome)| format!("{outcome:?}"))
                    .unwrap_or_else(|| "none".to_string());
                Some((outcome, frames))
            }
        }
    }
}

/// Whether a tile is inside the ten-by-nine window the screen buffer can answer for.
fn in_window(player: Tile, x: u8, y: u8) -> bool {
    let dx = i32::from(x) - i32::from(player.x);
    let dy = i32::from(y) - i32::from(player.y);
    (-4..=5).contains(&dx) && (-4..=4).contains(&dy)
}

#[test]
fn the_decoded_grid_agrees_with_the_window_on_every_tile_the_window_can_answer() {
    let (rom, checkpoint) = skip_without!();
    let mut game = Game::resume(&rom, &checkpoint);
    let map = game.map();
    let here = game.tile();
    let grid = match game.grid() {
        Ok(grid) => grid,
        Err(refusal) => {
            eprintln!("skipped: no grid on map {map:#04x} ({})", refusal.label());
            return;
        }
    };
    eprintln!(
        "map {map:#04x} {}x{} at {here:?}: walkable {}, reachable {}, unknown {}",
        grid.width(),
        grid.height(),
        grid.walkable_count(),
        grid.reachable_from(here.x, here.y),
        grid.unknown_count()
    );
    assert_eq!(grid.map(), map);
    assert_eq!(grid.unknown_count(), 0, "every block of the map is in the blockset");

    let mut checked = 0;
    let mut disagreements = Vec::new();
    for y in 0..grid.height() {
        for x in 0..grid.width() {
            let Some(tile) = state::map_tile_id(&mut game.gb, x, y) else { continue };
            let window = state::walkable(&mut game.gb, x, y);
            checked += 1;
            if grid.tile_id(x, y) != Some(tile) || grid.walkable(x, y) != window {
                disagreements.push((x, y, tile, grid.tile_id(x, y), window, grid.walkable(x, y)));
            }
            assert!(in_window(here, x, y), "the window answered outside its own window");
        }
    }
    eprintln!("{checked} tiles the window can answer for, {} disagreements", disagreements.len());
    assert!(disagreements.is_empty(), "{disagreements:?}");
    assert!(checked >= 40, "the window should answer for most of its ninety tiles: {checked}");
}

/// The survey: walk the map with real presses on throwaway emulators, tile by tile.
///
/// `docs/design/room-escape.md` section 3's method, and the one `tests/rom_scene.rs` used to pin
/// the window predicate's screen origin. Each tile keeps the state the walk arrived on, so every
/// press is made from the tile it belongs to rather than from wherever a long walk ended up.
///
/// Bounded by `FLY_GRID_SURVEY_TILES` (default [`SURVEY_TILES`]) because a whole forest is
/// 700-odd tiles and four presses each, and the claim is about the *rule*, not about coverage: a
/// wrong corner, a wrong stride or a wrong border disagrees within a dozen tiles.
type Survey = (
    BTreeMap<Tile, Vec<u8>>,
    BTreeSet<(Tile, &'static str)>,
    BTreeSet<(Tile, &'static str)>,
    BTreeSet<(Tile, &'static str)>,
    BTreeSet<(Tile, &'static str)>,
);

/// Tiles the survey stands on before it stops, unless `FLY_GRID_SURVEY_TILES` says otherwise.
const SURVEY_TILES: usize = 120;

/// The four presses, with the direction each one is and the tile offset it aims at.
const PRESSES: [(&str, u8, Facing); 4] = [
    ("up", buttons::UP, Facing::Up),
    ("down", buttons::DOWN, Facing::Down),
    ("left", buttons::LEFT, Facing::Left),
    ("right", buttons::RIGHT, Facing::Right),
];

fn survey(rom: &[u8], start: &[u8], budget: usize) -> Survey {
    let read = |probe: &mut Emulator| {
        (
            probe.read_wram(ram::wCurMap),
            Tile::new(probe.read_wram(ram::wXCoord), probe.read_wram(ram::wYCoord)),
        )
    };
    // 120 frames of held direction, then a release and twenty frames to settle, which are the two
    // numbers `docs/design/macros-wram.md` measured: a press the player is not already facing
    // turns first and steps second (53 frames from one staircase), and a state exported with a
    // button held does not respond to that button after the import.
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
        for _ in 0..SETTLE_FRAMES {
            probe.run_frame().expect("a frame should complete");
        }
        (read(probe), moved)
    };
    let restore = |state: &[u8]| {
        let mut probe = emulator(rom);
        probe.import_state(state).expect("a surveyed state should import");
        probe
    };

    let mut first = restore(start);
    for _ in 0..SETTLE_FRAMES {
        first.run_frame().expect("a frame should complete");
    }
    let (map, here) = read(&mut first);
    let mut states: BTreeMap<Tile, Vec<u8>> = BTreeMap::new();
    let mut refused: BTreeSet<(Tile, &'static str)> = BTreeSet::new();
    let mut left: BTreeSet<(Tile, &'static str)> = BTreeSet::new();
    let mut interrupted: BTreeSet<(Tile, &'static str)> = BTreeSet::new();
    let mut blocked_by_sprite: BTreeSet<(Tile, &'static str)> = BTreeSet::new();
    states.insert(here, first.export_state().expect("a settled state should export"));
    let mut queue = std::collections::VecDeque::from([here]);
    while let Some(tile) = queue.pop_front() {
        if states.len() >= budget {
            break;
        }
        for (name, mask, _) in PRESSES {
            let mut probe = restore(&states[&tile]);
            // A press the cartridge answers with something other than a step -- a wild encounter
            // in the grass, a bug catcher's line of sight, a script that takes the joypad -- says
            // nothing about the ground either way, so the survey records it as its own category
            // rather than as a wall. Viridian Forest is full of them: three tiles of the first
            // hundred and twenty started a battle in three directions each.
            if !matches!(scene::detect(&mut probe), scene::Scene::Overworld) {
                interrupted.insert((tile, name));
                continue;
            }
            let ((there, at), moved) = step(&mut probe, mask);
            if !matches!(scene::detect(&mut probe), scene::Scene::Overworld) {
                interrupted.insert((tile, name));
                continue;
            }
            if !moved {
                // **A person in the way is not a wall.** The collision table knows nothing about
                // sprites and says so (`docs/design/macros-wram.md`), and Pallet Town's two
                // villagers walk: whether one was standing on the target tile has to be read from
                // the frame the press was made in, not from the state the survey started in.
                let facing = PRESSES
                    .iter()
                    .find(|(press, _, _)| *press == name)
                    .map(|(_, _, facing)| *facing)
                    .expect("a known press");
                let ahead = tile.step(facing);
                let sprite = ahead.is_some_and(|ahead| {
                    state::npcs(&mut probe).iter().any(|npc| (npc.x, npc.y) == (ahead.x, ahead.y))
                });
                if sprite {
                    blocked_by_sprite.insert((tile, name));
                } else {
                    refused.insert((tile, name));
                }
                continue;
            }
            if there != map {
                // A warp: the press left the map, so it says nothing about the ground here.
                left.insert((tile, name));
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(entry) = states.entry(at) {
                entry.insert(probe.export_state().expect("state export"));
                queue.push_back(at);
            }
        }
    }
    (states, refused, left, interrupted, blocked_by_sprite)
}

#[test]
fn the_decoded_grid_matches_a_survey_with_real_presses() {
    let (rom, checkpoint) = skip_without!();
    let mut game = Game::resume(&rom, &checkpoint);
    let map = game.map();
    let grid = match game.grid() {
        Ok(grid) => grid,
        Err(refusal) => {
            eprintln!("skipped: no grid on map {map:#04x} ({})", refusal.label());
            return;
        }
    };
    let npcs: BTreeSet<Tile> = game
        .with_state(|state| state.npcs().iter().map(|npc| Tile::new(npc.x, npc.y)).collect());
    let budget = std::env::var("FLY_GRID_SURVEY_TILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SURVEY_TILES);
    let start = game.gb.export_state().expect("the settled state should export");
    let (stood, refused, left, interrupted, sprites) = survey(&rom, &start, budget);
    eprintln!(
        "survey of map {map:#04x}: {} tiles stood on, {} refused presses, {} presses that left \
         the map, {} the cartridge answered with a battle or a script, {} a sprite was standing \
         in the way of",
        stood.len(),
        refused.len(),
        left.len(),
        interrupted.len(),
        sprites.len()
    );
    assert!(stood.len() > 8, "the survey barely moved: {} tiles", stood.len());

    // Every tile the survey stood on is ground the grid calls walkable. This is the half that
    // catches a decode that is too *strict* — a wall where the game lets the fly stand.
    let mut wrong_walls = Vec::new();
    for tile in stood.keys() {
        if grid.walkable(tile.x, tile.y) != Walkable::Yes {
            wrong_walls.push((*tile, grid.walkable(tile.x, tile.y), grid.tile_id(tile.x, tile.y)));
        }
    }
    assert!(wrong_walls.is_empty(), "tiles the survey stood on that the grid calls walls: {wrong_walls:?}");

    // Every press the cartridge refused is a wall in the grid, a directed wall out of that tile,
    // or a tile a sprite is standing on — the three things a refusal can be
    // (`docs/design/macros.md` section 15). This is the half that catches a decode that is too
    // *permissive*.
    let mut unexplained = Vec::new();
    for (tile, name) in &refused {
        let (_, _, facing) = PRESSES.iter().find(|(press, _, _)| press == name).copied().unwrap();
        let ahead = tile.step(facing);
        let explained = match ahead {
            None => true,
            Some(ahead) => {
                ahead.x >= grid.width()
                    || ahead.y >= grid.height()
                    || grid.walkable(ahead.x, ahead.y) != Walkable::Yes
                    || grid.walled(tile.x, tile.y, facing)
                    || npcs.contains(&ahead)
            }
        };
        if !explained {
            unexplained.push((
                *tile,
                grid.tile_id(tile.x, tile.y),
                *name,
                ahead,
                ahead.map(|at| grid.tile_id(at.x, at.y)),
                grid.walled(tile.x, tile.y, facing),
            ));
        }
    }
    assert!(
        unexplained.is_empty(),
        "presses the cartridge refused that the grid calls open ground: {unexplained:?}"
    );

    // And every step that *worked* is one the grid would have planned: walkable, not walled.
    let mut wrongly_walled = Vec::new();
    for tile in stood.keys() {
        for (name, _, facing) in PRESSES {
            if refused.contains(&(*tile, name))
                || left.contains(&(*tile, name))
                || interrupted.contains(&(*tile, name))
                || sprites.contains(&(*tile, name))
            {
                continue;
            }
            let Some(ahead) = tile.step(facing) else { continue };
            if !stood.contains_key(&ahead) {
                continue;
            }
            if grid.walkable(ahead.x, ahead.y) != Walkable::Yes
                || grid.walled(tile.x, tile.y, facing)
            {
                wrongly_walled.push((*tile, name, ahead));
            }
        }
    }
    assert!(
        wrongly_walled.is_empty(),
        "steps the cartridge made that the grid calls walls: {wrongly_walled:?}"
    );
    eprintln!(
        "the grid agrees with every one of {} stood tiles and {} refused presses on map {map:#04x}",
        stood.len(),
        refused.len()
    );
}

#[test]
fn go_frontier_aims_outside_the_window_and_walks_there() {
    let (rom, checkpoint) = skip_without!();
    let mut game = Game::resume(&rom, &checkpoint);
    let map = game.map();
    let Ok(grid) = game.grid() else {
        eprintln!("skipped: no grid on map {map:#04x}");
        return;
    };

    // **The plan.** Every frontier tile the ten-by-nine window cannot answer for is ground the
    // fly could not have aimed at before section 15, and a route to the nearest of them is a
    // plan that crosses the window with no guesses in it.
    let (here, outside, plan) = game.with_state(|state| {
        let here = state.player().map(|player| Tile::new(player.x, player.y)).expect("a player");
        let outside: Vec<Tile> = path::frontier(state)
            .into_iter()
            .map(|(tile, _)| tile)
            .filter(|tile| !in_window(here, tile.x, tile.y))
            .collect();
        let plan = path::route(state, &outside);
        (here, outside, plan)
    });
    eprintln!(
        "map {map:#04x} at {here:?}: {} frontier tiles outside the window, plan {:?} steps",
        outside.len(),
        plan.as_ref().map(|route| route.steps.len())
    );
    if outside.is_empty() {
        eprintln!("skipped: every frontier tile on this map is inside the window");
        return;
    }
    let plan = plan.expect("a route to the frontier beyond the window");
    assert!(plan.goal.is_some(), "the plan only approaches the frontier");
    let mut at = here;
    for facing in &plan.steps {
        at = at.step(*facing).expect("a step inside the map");
        assert_eq!(
            grid.walkable(at.x, at.y),
            Walkable::Yes,
            "the plan walks through {at:?}, which the grid does not call ground"
        );
    }
    assert!(outside.contains(&at), "the plan ends on {at:?}, which is not one of its goals");
    assert!(
        !in_window(here, at.x, at.y),
        "the plan ends on {at:?}, which the window could have answered for anyway"
    );

    // **The walk.** The fly's own choice is not being simulated here — the slot is started by
    // name — but everything after that is the shipping executor: one plan, the per-step moved
    // check, the three-failure rule, the frame cap. What is asserted is that some hold of
    // `GO FRONTIER` ends on ground that was outside the window when the hold began, which is the
    // whole of the operator's ask.
    let mut left_the_window = None;
    let mut left_the_map = None;
    let mut done = 0;
    for hold in 0..12 {
        let began = game.tile();
        let Some((outcome, frames)) = game.run_macro("GO FRONTIER") else {
            eprintln!("GO FRONTIER left the pad after {hold} holds");
            break;
        };
        let landed = game.tile();
        if outcome == "Done" {
            done += 1;
        }
        eprintln!("hold {hold}: {began:?} -> {landed:?} in {frames} frames, {outcome}");
        if !in_window(began, landed.x, landed.y) {
            left_the_window = Some((hold, began, landed, frames));
            break;
        }
        if game.map() != map {
            left_the_map = Some((hold, game.map()));
            break;
        }
    }
    assert!(done > 0, "no hold of GO FRONTIER finished");
    match (left_the_window, left_the_map) {
        (Some((hold, began, landed, frames)), _) => eprintln!(
            "GO FRONTIER walked out of its own window on hold {hold}: {began:?} -> {landed:?} in \
             {frames} frames"
        ),
        // A doormat is unstood ground and the frontier is allowed to aim at it
        // (`docs/design/macros.md` section 12.7: the reward ledger can never record a warp tile),
        // so a near frontier that is a door takes the fly off the map before the far one is
        // reached. That is the frontier's own rule rather than the grid's doing, and the plan
        // above is what this test is for; the arrival is asserted on a map with no door next to
        // the fly.
        (None, Some((hold, map))) => eprintln!(
            "GO FRONTIER stepped onto a warp tile on hold {hold} and left for map {map:#04x} \
             before it left its window"
        ),
        (None, None) => panic!("no hold of GO FRONTIER left the window it started in"),
    }
}

#[test]
fn a_way_out_of_the_map_is_one_plan_away() {
    let (rom, checkpoint) = skip_without!();
    let mut game = Game::resume(&rom, &checkpoint);
    let map = game.map();
    let Ok(grid) = game.grid() else {
        eprintln!("skipped: no grid on map {map:#04x}");
        return;
    };
    let (here, plans) = game.with_state(|state| {
        let here = state.player().map(|player| Tile::new(player.x, player.y)).expect("a player");
        let exits = path::exits(state);
        let mut plans = Vec::new();
        for exit in &exits {
            if let Some(route) = path::route(state, &[exit.tile]) {
                plans.push((exit.id, exit.tile, exit.way, route.goal.is_some(), route.steps.clone()));
            }
        }
        (here, plans)
    });
    assert!(!plans.is_empty(), "a map with no way out at all");
    let mut crossed = 0;
    let mut reached_one = false;
    for (id, tile, way, reached, steps) in &plans {
        // Every tile of the plan is ground the grid calls walkable: a plan with no guesses in it,
        // which is what "one plan per walk" needs to mean.
        let mut at = here;
        let mut guesses = 0;
        for facing in steps {
            at = at.step(*facing).expect("a step inside the map");
            if grid.walkable(at.x, at.y) != Walkable::Yes {
                guesses += 1;
            }
        }
        eprintln!(
            "{id:?} at {tile:?} ({way:?}): {} steps, reached {reached}, {guesses} tiles the grid \
             does not call ground",
            steps.len()
        );
        assert_eq!(guesses, 0, "the plan to {tile:?} walks through {guesses} tiles of not-ground");
        if !*reached {
            // An exit tile the map fences off from where the fly stands: the grid *knows* it
            // cannot be reached, which is the honest answer and is what the closest-approach route
            // is for. Pallet Town's north-west corner is one — walkable ground behind a fence.
            eprintln!("  (that one only approaches: {} of {} tiles reachable)", 0, steps.len());
            continue;
        }
        reached_one = true;
        assert_eq!(at, *tile, "the plan to {tile:?} ends on {at:?}");
        if steps.len() > 9 {
            crossed += 1;
        }
    }
    assert!(reached_one, "no way out of map {map:#04x} can be reached at all");
    assert!(
        crossed > 0,
        "no way out of map {map:#04x} is further than the window, so this checkpoint cannot show \
         the difference"
    );
}

