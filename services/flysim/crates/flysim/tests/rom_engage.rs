//! The engagement rewards against the real cartridge: a conversation indoors and an item ball.
//!
//! Gated on `FLY_ROM` *and* on a checkpoint, the way every ROM test in this workspace is, and
//! skips cleanly without either:
//!
//! ```sh
//! FLY_ROM="$HOME/roms/pokemon-red.gb" \
//!   FLY_ENGAGE_CHECKPOINT=.local/checkpoints/<a rung-10 Pewter checkpoint> \
//!   cargo test --release -p flysim --test rom_engage -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! The synthetic traces in `pokemon_red/tests.rs` write `wFontLoaded`, `wSpriteIndex`,
//! `wToggleableObjectFlags` and the rest from the disassembly. They cannot say that an A press at
//! a person on this cartridge opens the box on a frame whose previous sample was the fly's own,
//! that `wSpriteIndex` names that person by the time the adapter samples, or that `PickUpItem`
//! raises the ball's bit on a frame the adapter sees. This test does, with the shipping adapter
//! sampling once a frame, and it restores the checkpoint's own `v6` reward ledger -- so it is also
//! the `v6` -> `v7` migration on real game state: the item keys are seeded from the cartridge's
//! bits and nothing already taken pays.
//!
//! ## How the fly is moved
//!
//! Not by the macro layer, and not by a brain: a small scripted walker with a breadth-first route
//! over the whole-map grid (`state::map_grid`), pressing one direction at a time, answering text
//! with B and battles with A. The question is what the adapter reads, not whether anything finds
//! its way there. From the Pewter checkpoint it walks south out of the city, down Route 2 into the
//! Viridian Forest north gate -- a building, where it talks to the old man twice -- and on into
//! the forest to the Antidote ball at (25, 11), which it picks up, and then, after rolling the
//! emulator back to before the pickup, picks up again.

use std::collections::VecDeque;

use flybrain_gb::adapter::RewardEvent;
use flybrain_gb::pokemon_red::macros::state::{Facing, Walkable};
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, catalog, engage, state};
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter, buttons};

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const PEWTER_CITY: u8 = 0x02;
const ROUTE_2: u8 = 0x0d;
const NORTH_GATE: u8 = 0x2f;
const VIRIDIAN_FOREST: u8 = 0x33;
/// `constants/item_constants.asm`: `ANTIDOTE` is `$0b`.
const ANTIDOTE: &str = "FOUND ITEM #11";

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_ENGAGE_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

struct Run {
    gb: Emulator,
    adapter: PokemonRedReward,
    ms: f64,
    frames: u64,
    payouts: Vec<RewardEvent>,
    /// `(wCurMap, wCurMapTileset)` on the frame each payout in `payouts` was made.
    payout_maps: Vec<(u8, u8)>,
}

fn delta(facing: Facing) -> (i16, i16) {
    facing.delta()
}

fn mask(facing: Facing) -> u8 {
    match facing {
        Facing::Up => buttons::UP,
        Facing::Down => buttons::DOWN,
        Facing::Left => buttons::LEFT,
        Facing::Right => buttons::RIGHT,
    }
}

const FACINGS: [Facing; 4] = [Facing::Up, Facing::Down, Facing::Left, Facing::Right];

impl Run {
    fn resume(rom: &[u8], checkpoint: &flysim::store::Checkpoint) -> Self {
        let mut gb = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the cartridge");
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
        let mut adapter = PokemonRedReward::new();
        // The checkpoint was written by `pokered-unique8-v6`: this is the migration.
        adapter.import_state(&checkpoint.runtime.reward).expect("a v6 ledger is a v7 ledger");
        Self { gb, adapter, ms: 0.0, frames: 0, payouts: Vec::new(), payout_maps: Vec::new() }
    }

    fn byte(&mut self, address: u16) -> u8 {
        self.gb.read_wram(address)
    }

    fn frame(&mut self, mask: u8) {
        self.gb.set_buttons(mask);
        self.gb.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        self.frames += 1;
        let ms = self.ms;
        let events = self.adapter.sample(&mut self.gb, ms);
        let here = (self.byte(ram::wCurMap), self.byte(ram::wCurMapTileset));
        self.payout_maps.extend(events.iter().map(|_| here));
        self.payouts.extend(events);
        assert!(self.frames < 200_000, "the walker is lost: {}", self.whereabouts());
    }

    fn whereabouts(&mut self) -> String {
        format!(
            "map {:#04x} at ({}, {}), battle {}, font {}",
            self.byte(ram::wCurMap),
            self.byte(ram::wXCoord),
            self.byte(ram::wYCoord),
            self.byte(ram::wIsInBattle),
            self.byte(ram::wFontLoaded)
        )
    }

    fn map(&mut self) -> u8 {
        self.byte(ram::wCurMap)
    }

    fn at(&mut self) -> (u8, u8) {
        (self.byte(ram::wXCoord), self.byte(ram::wYCoord))
    }

    /// Whatever is on screen that is not the fly's to walk through: a battle (A through it) or a
    /// text box (B through it). Returns once the overworld is controllable again.
    fn settle(&mut self) {
        for tick in 0..20_000u32 {
            let in_battle = self.byte(ram::wIsInBattle) != 0;
            let open = self.byte(ram::wFontLoaded) & 1 != 0;
            if !in_battle && !open && state::controllable(&mut self.gb) {
                if self.byte(ram::wWalkCounter) == 0 {
                    return;
                }
                self.frame(buttons::NONE);
                continue;
            }
            let press = if in_battle { buttons::A } else { buttons::B };
            self.frame(if tick % 8 < 3 { press } else { buttons::NONE });
        }
        panic!("the screen never settled: {}", self.whereabouts());
    }

    /// Press `facing` until the player has moved one tile, the map has changed, or it is plain
    /// that the step is refused (which turns the player to face that way).
    fn step(&mut self, facing: Facing) {
        let (map, from) = (self.map(), self.at());
        for _ in 0..48 {
            self.frame(mask(facing));
            if self.map() != map || self.at() != from {
                break;
            }
            if self.byte(ram::wIsInBattle) != 0 || self.byte(ram::wFontLoaded) & 1 != 0 {
                break;
            }
        }
        self.frame(buttons::NONE);
        self.settle();
    }

    /// Breadth-first over the decoded map, people excluded, from where the player stands to the
    /// nearest tile `goal` accepts; the first step of that route, or `None`.
    fn route(&mut self, goal: &dyn Fn(u8, u8) -> bool) -> Option<Facing> {
        let grid = state::map_grid(&mut self.gb).ok()?;
        let people: Vec<(u8, u8)> =
            state::npcs(&mut self.gb).iter().map(|npc| (npc.x, npc.y)).collect();
        let (width, height) = (grid.width(), grid.height());
        let start = self.at();
        let mut first: Vec<Option<Facing>> = vec![None; usize::from(width) * usize::from(height)];
        let mut seen = vec![false; first.len()];
        let index = |x: u8, y: u8| usize::from(y) * usize::from(width) + usize::from(x);
        let mut queue = VecDeque::from([start]);
        seen[index(start.0, start.1)] = true;
        while let Some((x, y)) = queue.pop_front() {
            if (x, y) != start && goal(x, y) {
                return first[index(x, y)];
            }
            for facing in FACINGS {
                if grid.walled(x, y, facing) {
                    continue;
                }
                let (dx, dy) = delta(facing);
                let (Ok(nx), Ok(ny)) =
                    (u8::try_from(i16::from(x) + dx), u8::try_from(i16::from(y) + dy))
                else {
                    continue;
                };
                if nx >= width || ny >= height || seen[index(nx, ny)] {
                    continue;
                }
                if grid.walkable(nx, ny) != Walkable::Yes || people.contains(&(nx, ny)) {
                    continue;
                }
                seen[index(nx, ny)] = true;
                first[index(nx, ny)] = if (x, y) == start { Some(facing) } else { first[index(x, y)] };
                queue.push_back((nx, ny));
            }
        }
        None
    }

    /// Walk until standing on a tile `goal` accepts, on this map.
    fn walk_to(&mut self, goal: &dyn Fn(u8, u8) -> bool) {
        let map = self.map();
        for _ in 0..400 {
            self.settle();
            assert_eq!(self.map(), map, "the walk left the map: {}", self.whereabouts());
            let (x, y) = self.at();
            if goal(x, y) {
                return;
            }
            let Some(facing) = self.route(goal) else {
                // A grid refused on this frame, or a person in the way: let a frame go by.
                self.frame(buttons::NONE);
                continue;
            };
            self.step(facing);
        }
        let grid = state::map_grid(&mut self.gb);
        let detail = match &grid {
            Ok(grid) => {
                let (x, y) = self.at();
                let mut rows = String::new();
                for ty in 0..grid.height().min(24) {
                    for tx in 0..grid.width() {
                        rows.push(if (tx, ty) == (x, y) {
                            '@'
                        } else {
                            match grid.walkable(tx, ty) {
                                Walkable::Yes => '.',
                                Walkable::No => '#',
                                Walkable::Unknown => '?',
                            }
                        });
                    }
                    rows.push('\n');
                }
                format!("reachable {}\n{rows}", grid.reachable_from(x, y))
            }
            Err(refusal) => format!("grid refused: {}", refusal.label()),
        };
        panic!("never reached the goal: {}; {detail}", self.whereabouts());
    }

    /// Walk to `(x, y)` and keep pressing `out` until the map changes.
    fn leave_by(&mut self, x: u8, y: u8, out: Facing) {
        let map = self.map();
        self.walk_to(&|tx, ty| (tx, ty) == (x, y));
        for _ in 0..8 {
            self.step(out);
            if self.map() != map {
                // The warp's fade and the new map's first frames.
                for _ in 0..60 {
                    self.frame(buttons::NONE);
                }
                self.settle();
                return;
            }
        }
        panic!("pressing {out:?} at ({x}, {y}) never left the map: {}", self.whereabouts());
    }

    /// Stand beside `(x, y)`, face it, press A once and let the conversation run to its end.
    /// Returns the payouts the conversation produced.
    fn press_a_at(&mut self, x: u8, y: u8) -> Vec<RewardEvent> {
        self.walk_to(&|tx, ty| tx.abs_diff(x) + ty.abs_diff(y) == 1);
        let (px, py) = self.at();
        let facing = FACINGS
            .into_iter()
            .find(|facing| {
                let (dx, dy) = delta(*facing);
                i16::from(px) + dx == i16::from(x) && i16::from(py) + dy == i16::from(y)
            })
            .expect("a neighbour faces the target one way");
        // Turn in place: the step is refused because the thing is in the way.
        self.step(facing);
        assert_eq!(self.at(), (px, py), "turning must not move the player");
        self.press_a_here()
    }

    /// Press A where the player stands and faces, and let whatever it opens run to its end.
    fn press_a_here(&mut self) -> Vec<RewardEvent> {
        for _ in 0..4 {
            self.frame(buttons::NONE);
        }
        let before = self.payouts.len();
        for _ in 0..6 {
            self.frame(buttons::A);
        }
        self.frame(buttons::NONE);
        self.settle();
        for _ in 0..8 {
            self.frame(buttons::NONE);
        }
        self.payouts[before..].to_vec()
    }

    fn of_kind(&self, kind: &str) -> usize {
        self.payouts.iter().filter(|event| event.kind == kind).count()
    }
}

fn kinds(events: &[RewardEvent]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind).collect()
}

#[test]
fn a_conversation_indoors_and_an_item_ball_each_pay_exactly_once_on_the_cartridge() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_ENGAGE_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, &checkpoint);
    if run.map() != PEWTER_CITY {
        eprintln!("skipped: the checkpoint is on map {:#04x}, not Pewter City", run.map());
        return;
    }
    let before = run.adapter.progress().counts;

    // The restore's first sample: the v6 ledger holds no item keys, so this is the seed.
    run.settle();
    assert!(run.payouts.is_empty(), "the migration pays nothing: {:?}", run.payouts);

    // Pewter City's south edge, onto Route 2.
    let height = run.byte(ram::wCurMapHeight) * 2;
    run.walk_to(&|_, y| y == height - 1);
    let (x, y) = run.at();
    run.leave_by(x, y, Facing::Down);
    assert_eq!(run.map(), ROUTE_2, "{}", run.whereabouts());

    // Route 2's door into the forest's north gate, `warp_event 3, 11`: the gate is south of
    // the city, so its door is entered heading south, from the tile above it.
    run.leave_by(3, 10, Facing::Down);
    assert_eq!(run.map(), NORTH_GATE, "{}", run.whereabouts());
    let tileset = run.byte(ram::wCurMapTileset);
    assert!(engage::indoor(tileset), "the gate is a building (tileset {tileset})");

    // The old man at (2, 5): one conversation, one payout, when the box closes.
    let first = run.press_a_at(2, 5);
    eprintln!("gate, first conversation: {first:?}");
    let talks: Vec<&RewardEvent> =
        first.iter().filter(|event| event.kind == catalog::kind::TALK).collect();
    assert_eq!(talks.len(), 1, "one conversation, one payout: {first:?}");
    assert_eq!(talks[0].value, 0.10);
    assert!(talks[0].label.starts_with("TALKED TO #"), "{}", talks[0].label);
    // The same man again, and again: not a farm.
    for _ in 0..2 {
        let again = run.press_a_at(2, 5);
        assert!(
            !again.iter().any(|event| event.kind == catalog::kind::TALK),
            "a second conversation with the same person pays nothing: {again:?}"
        );
    }

    // On through the gate, `warp_event 4, 7`, into the forest.
    run.leave_by(4, 7, Facing::Down);
    assert_eq!(run.map(), VIRIDIAN_FOREST, "{}", run.whereabouts());
    assert!(!engage::indoor(run.byte(ram::wCurMapTileset)), "the forest is not a building");

    // The Antidote ball at (25, 11). Save the game just before, to take it twice.
    run.walk_to(&|tx, ty| tx.abs_diff(25) + ty.abs_diff(11) == 1);
    let slot = run.gb.export_state().expect("an emulator state");
    let first = run.press_a_at(25, 11);
    eprintln!("forest, the ball: {first:?}");
    let items: Vec<&RewardEvent> =
        first.iter().filter(|event| event.kind == catalog::kind::ITEM).collect();
    assert_eq!(items.len(), 1, "one pickup, one payout: {first:?}");
    assert_eq!(items[0].value, 0.15);
    assert_eq!(items[0].label, ANTIDOTE);
    assert!(
        !first.iter().any(|event| event.kind == catalog::kind::TALK),
        "a ball is not a conversation, and the forest is not indoors"
    );
    // The ball is gone: pressing A at the empty tile pays nothing.
    let empty = run.press_a_here();
    assert!(!empty.iter().any(|event| event.kind == catalog::kind::ITEM), "{empty:?}");

    // A rollback to the slot with the ball still there, the way the ratchet restores one, and
    // the fly takes it again: the same item, and it does not pay twice.
    run.gb.import_state(&slot).expect("the slot restores");
    run.adapter.clear_transient();
    run.frame(buttons::NONE);
    run.settle();
    let again = run.press_a_at(25, 11);
    eprintln!("forest, the ball after a rollback: {again:?}");
    assert!(
        !again.iter().any(|event| event.kind == catalog::kind::ITEM),
        "once per item for the run: {again:?}"
    );

    let after = run.adapter.progress().counts;
    eprintln!(
        "{:.1} brain minutes; talk {} -> {}, item {} -> {}; every payout (kind, (map, tileset)): {:?}",
        run.ms / 60_000.0,
        before[catalog::kind::TALK],
        after[catalog::kind::TALK],
        before[catalog::kind::ITEM],
        after[catalog::kind::ITEM],
        kinds(&run.payouts).iter().zip(&run.payout_maps).collect::<Vec<_>>()
    );
    // The gate's exits were stood beside and walked through, and none of them paid: an indoor
    // exit pays nothing. What *does* show up under the gate's id is Route 2's door, the
    // outdoor exit the fly took: for the thirty-odd frames of `PlayMapChangeSound` the cartridge
    // has already written the new `wCurMap` while the header, the warp table and the tileset
    // are still Route 2's, and the rule reads that frame as what it is -- an outdoor exit.
    for (event, (map, tileset)) in run.payouts.iter().zip(&run.payout_maps) {
        assert!(
            !(event.kind == catalog::kind::BOUNDARY && engage::indoor(*tileset)),
            "a boundary payout on an indoor map's own header (map {map}): {event:?}"
        );
    }
    assert_eq!(run.of_kind(catalog::kind::TALK), 1);
    assert_eq!(run.of_kind(catalog::kind::ITEM), 1);
    assert_eq!(after[catalog::kind::TALK], before[catalog::kind::TALK] + 1);
    assert_eq!(after[catalog::kind::ITEM], before[catalog::kind::ITEM] + 1);
}
