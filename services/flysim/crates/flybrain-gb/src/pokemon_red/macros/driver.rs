//! Pokémon Red behind the sim loop's macro seam (`crate::macros`).
//!
//! Agent B's [`MacroMachine`] reads the game through [`MacroState`] and agent A's [`PokeState`]
//! implements it over live WRAM; this file is the twenty lines that put the two together and hand
//! the result to the loop as a [`MacroPalette`]. It is the only place where the executor, the
//! scene detector and a `MemoryReader` meet.
//!
//! What it adds beyond the wiring: the palette the last `observe` bound is *remembered*, because
//! `MacroMachine::start` takes the palette it is starting a slot from and the loop should not have
//! to carry one. The remembered palette is one frame old at most — `observe` runs after every
//! frame and `start` before the next one, which read the same WRAM — and `start` checks the scene
//! against it anyway (`Refusal::WrongScene`), so a stale palette refuses rather than acting.

use crate::adapter::MemoryReader;
use crate::macros::{
    MacroPalette, Observed, Outcome, PaletteMode, RunLedger, SLOTS, SceneId, SlotBinding, Started,
};

use super::super::mapgrid::MapGrids;
use super::super::state::PokeState;
use super::cartridge::{
    Areas, Frontiers, LAST_MAP, MacroState, Pushed, Stood, Talked, Targets, Tile, outdoors,
};
use super::geography;
use super::executor::{MacroAbort, MacroMachine, Refusal};
use super::palette::{self, MacroId, Palette};
use super::plan;
use super::state::{GameState, Scene};

/// The longest a warp's tear is honoured (row 58): the thirty-two frames measured at the Pewter Gym
/// door, with room for a slower fade, and short enough that a false reading costs a second and a
/// half of an empty pad rather than a stall.
pub const TEAR_FRAMES: u16 = 90;

/// The macro palette over Pokémon Red.
#[derive(Debug, Clone)]
pub struct PokemonPalette {
    machine: MacroMachine,
    /// Whether a slot is a channel or a rank in the scene's plan
    /// ([`PaletteMode`], `docs/design/macros.md` sections 3 and 9).
    ///
    /// The only thing this changes is which of two functions deals the slots. Everything after
    /// that — the executor, the refusals, the outcomes, the feed — cannot tell the two modes
    /// apart, which is the point: a plan *is* a palette whose slots are ranks.
    mode: PaletteMode,
    /// The palette the last [`MacroPalette::observe`] bound, and the scene it was bound for.
    palette: Option<Palette>,
    /// What this session has talked to (`docs/design/macros.md` section 12).
    ///
    /// Owned here because this is the one type that both builds the state the macros read and
    /// steps the machine that writes to it: [`MacroMachine::take_talked`] hands over a finished
    /// `TALK`'s target and this records it, one frame later than the press and before the next
    /// `observe` asks about it. Session state, not run state -- it does not reach the checkpoint
    /// and a restored run offers every person once more.
    talked: Talked,
    /// Which targets are excluded and which have been reached
    /// (`docs/design/macros.md` section 12, the Viridian stall). Owned here for the same reason
    /// the talked ledger is: this is the one type that both builds the state the macros read and
    /// steps the machine that writes to it. Session state -- not checkpointed.
    targets: Targets,
    /// The ground this session has watched the fly stand on (`docs/design/macros.md` section
    /// 12.7). Owned here for the same reason the two ledgers above are: this is the one type that
    /// both builds the state the macros read and sees every frame the fly stands on. Session
    /// state -- not checkpointed.
    stood: Stood,
    /// Which of each area's errands this run has discharged (`docs/design/macros.md` section 13).
    ///
    /// Owned here for the same reason the three ledgers above are: this is the one type that both
    /// builds the state the macros read and sees every frame the fly is standing somewhere. The
    /// entry is written on *arrival* -- the frame the fly is observed standing on a mart's or a
    /// centre's own map -- which is the moment the errand is discharged. A purchase or a heal marks
    /// nothing: what the errand asked for was the visit. Session state, not checkpointed.
    areas: Areas,
    /// Maps whose frontier a `GO FRONTIER` has proved unreachable (`docs/design/macros.md`
    /// section 12.14).
    ///
    /// Owned here beside the other session ledgers and written from the same two places they are:
    /// the machine's refusal on one side and the frame the fly is standing on new ground on the
    /// other. Like [`Pushed`] it has no window -- ground the map has fenced off is still fenced
    /// off ten brain minutes later -- and unlike it, it is *cleared*, by the only event that can
    /// change the answer.
    frontiers: Frontiers,
    /// Tiles the cartridge pushes the fly off (`infra/docs/macros-traps.md` row 37).
    ///
    /// Owned here beside the other session ledgers and for the same reason. Unlike the target
    /// ledgers this one has no window: Viridian City's (19, 9) prints "This is private property!"
    /// and walks the fly down on every frame it stands there until the Pokédex exists, and a map
    /// does not stop being like that ten brain minutes later.
    pushed: Pushed,
    /// The decoded walkability of the map the fly is on (`docs/design/macros.md` section 15).
    ///
    /// Owned here beside the session ledgers because it is the same shape of thing: built from
    /// the cartridge, valid for as long as the map is loaded, dropped on arrival somewhere else,
    /// and never checkpointed -- a restored run decodes the map again on its first overworld
    /// frame. It is a *cache* rather than a ledger: nothing about the run is in it, only what the
    /// cartridge's own tables say about the ground.
    grids: MapGrids,
    /// The fewest map hops between the fly and its objective this run has managed, and which
    /// objective that was (`docs/design/macros.md` section 12.15).
    ///
    /// Session state beside the ledgers and never checkpointed, and unlike them it is not a fact
    /// about the map at all: it is the one reading the *sim loop* takes from the macro layer, for
    /// the ratchet's stall window. The objective is carried with the number because the ladder's
    /// next rung changes as the run climbs, and "nearer" means nothing across two different
    /// places.
    nearest: Option<(u8, u32)>,
    /// Whether the last `observe` was the frame that number fell on.
    nearer: bool,
    /// The map of the last frame that was not a warp's tear, and how many tear frames have run
    /// since (row 58, [`PokemonPalette::tear`], [`TEAR_FRAMES`]).
    ///
    /// Measured on the cartridge at the Pewter Gym's door: `wCurMap` changes to the new map
    /// **thirty-two frames** before the map header, the coordinates and the warp table follow it,
    /// while the screen fades. On those frames every reading in the seam describes the map the fly
    /// just left under the new map's id -- the player "on map 54 at (16, 17)", which is Pewter
    /// City's doormat -- and nothing sets the joypad bits `controllable` reads until the fade is
    /// over, so the scene read `Overworld` and a pad was dealt. A walk started there plans over
    /// the wrong map, ends when the cartridge takes the joypad at the end of the fade, and the
    /// ledgers wrote what it had been aiming at against the new map's id.
    settled: Option<u8>,
    tear_frames: u16,
    /// The brain clock of the frame being decided, from [`MacroPalette::clock`].
    ///
    /// The blocked ledger is a *window*, so it needs the same clock the loop publishes rather
    /// than a frame count of its own: a macro's frames and the brain's milliseconds are not the
    /// same unit and the exclusion is quoted in brain minutes.
    now_ms: f64,
}

impl PokemonPalette {
    pub fn new(seed: u32) -> Self {
        Self::with_mode(seed, PaletteMode::Palette)
    }

    pub fn with_mode(seed: u32, mode: PaletteMode) -> Self {
        Self {
            machine: MacroMachine::new(seed),
            mode,
            palette: None,
            talked: Talked::default(),
            targets: Targets::new(),
            stood: Stood::default(),
            areas: Areas::default(),
            frontiers: Frontiers::default(),
            pushed: Pushed::default(),
            grids: MapGrids::default(),
            nearest: None,
            nearer: false,
            settled: None,
            tear_frames: 0,
            now_ms: 0.0,
        }
    }

    /// Whether this frame is a warp's tear (row 58): the map byte has changed since the last
    /// settled frame, and the fly still stands on a warp of the *loaded* table that leads to the
    /// map the byte now names. Updates the settled map on every frame that is not one.
    ///
    /// On a tear the warp table is still the map the fly just left, so the tile underfoot is the
    /// door it walked through: Pewter City's (16, 17), whose destination is the gym, under the
    /// gym's id; or a building's doormat, whose destination is `LAST_MAP`, under the id of the
    /// town outside. Once the header loads, the table is the new map's and the tile underfoot is
    /// the arrival warp, which leads back where the fly came from -- so the reading ends by itself.
    /// Measured: thirty-two frames at the gym door each way.
    ///
    /// The map byte having changed is what keeps the teleport pads of Saffron Gym and Silph Co. --
    /// the three maps in Red with a warp to themselves -- from reading as a tear: a pad moves the
    /// fly without changing the map. Bounded by [`TEAR_FRAMES`] all the same, because an empty pad
    /// that did not end would be a fly that waits for ever.
    fn tear(&mut self, state: &mut dyn MacroState) -> bool {
        let Some(player) = state.player() else { return false };
        let changed = self.settled.is_some_and(|was| was != player.map);
        let torn = changed
            && self.tear_frames < TEAR_FRAMES
            && state.warps().iter().any(|warp| {
                (warp.x, warp.y) == (player.x, player.y)
                    && (warp.destination_map == player.map
                        || (warp.destination_map == LAST_MAP && outdoors(player.map)))
            });
        if torn {
            self.tear_frames += 1;
        } else {
            self.tear_frames = 0;
            self.settled = Some(player.map);
        }
        torn
    }

    /// Frames the running macro has spent, for a log line.
    pub fn frames(&self) -> u32 {
        self.machine.frames()
    }

    /// How many things this session has talked to, for a log line and the tests.
    pub fn talked(&self) -> usize {
        self.talked.len()
    }

    /// How many targets are excluded and how many reached, for a log line and the tests.
    pub fn targets(&self) -> (usize, usize) {
        self.targets.len()
    }

    /// How many tiles this session has watched the fly stand on, for a log line and the tests.
    pub fn stood(&self) -> usize {
        self.stood.len()
    }

    /// How many of the run's errands have been discharged, for a log line and the tests.
    pub fn errands(&self) -> usize {
        self.areas.len()
    }

    /// How many tiles the cartridge has pushed the fly off, for a log line and the tests.
    pub fn pushed(&self) -> usize {
        self.pushed.len()
    }

    /// How many maps have proved their frontier unreachable, for a log line and the tests.
    pub fn exhausted(&self) -> usize {
        self.frontiers.len()
    }

    /// Read the frame exactly as the macros read it -- this session's ledgers included -- and hand
    /// the state to `visit`. A survey seam (`examples/scene_probe.rs`), not a decision path: it
    /// writes nothing, and what it sees is what the next `observe` would deal from.
    pub fn inspect<R>(
        &mut self,
        memory: &mut dyn MemoryReader,
        ledger: &dyn RunLedger,
        visit: impl FnOnce(&mut dyn MacroState) -> R,
    ) -> R {
        let mut state = PokeState::with_ledgers(
            memory,
            ledger,
            &self.talked,
            &self.targets,
            &self.stood,
            &self.areas,
            &self.pushed,
        )
        .caching_grid(&mut self.grids)
        .with_frontiers(&self.frontiers);
        visit(&mut state)
    }

    /// The session's no-window ledgers, for a survey line: the tiles the cartridge has pushed the
    /// fly off and the maps whose frontier is marked unreachable.
    pub fn fences(&self) -> (&Pushed, &Frontiers) {
        (&self.pushed, &self.frontiers)
    }

    /// The session's ledgers, writable, for a survey that has to rebuild a live session's state
    /// from a checkpoint (a restore starts them empty by design). Never called by the loop.
    #[doc(hidden)]
    pub fn ledgers_mut(
        &mut self,
    ) -> (&mut Talked, &mut Targets, &mut Stood, &mut Pushed, &mut Frontiers) {
        (&mut self.talked, &mut self.targets, &mut self.stood, &mut self.pushed, &mut self.frontiers)
    }

    /// Take whatever the machine's last finished macro earned into the session's ledgers.
    fn record_talk(&mut self) {
        if let Some((map, target)) = self.machine.take_talked() {
            self.talked.record(map, target);
        }
        // Several at once, for a refusal that found no route to any of its goals.
        while let Some((map, target)) = self.machine.take_blocked() {
            self.targets.record_blocked(map, target);
        }
        if let Some((map, target)) = self.machine.take_reached() {
            self.targets.record_reached(map, target);
        }
        // A tile the cartridge drove the fly off: no window, because the map is like that until
        // the event that unlocks it, and nothing here knows which event that is (row 37).
        if let Some((map, tile)) = self.machine.take_pushed() {
            self.pushed.record(map, tile);
        }
        // A refusal from where the fly is standing: that button is not dealt again from this tile
        // for the window (row 57). The fly moving, or the window closing, deals it again.
        if let Some((map, slot, tile)) = self.machine.take_refused() {
            self.targets.record_refused(map, slot, tile);
        }
        // A frontier the walk could not reach any of: a fact about this map's ground, with no
        // window on it (section 12.14).
        if let Some(map) = self.machine.take_exhausted() {
            self.frontiers.record(map);
        }
        if let Some((map, target, closer)) = self.machine.take_timeout() {
            self.targets.record_timeout(map, target, closer);
        }
    }
}

impl MacroPalette for PokemonPalette {
    fn clock(&mut self, ms: f64) {
        self.now_ms = ms;
        self.targets.clock(ms);
    }

    fn observe(&mut self, memory: &mut dyn MemoryReader, ledger: &dyn RunLedger) -> Observed {
        let torn = {
            let mut state = PokeState::new(memory);
            self.tear(&mut state)
        };
        let (scene, bindings, standing, stepping, approach) = {
            let Self {
                machine,
                mode,
                palette: cached,
                talked,
                targets,
                stood,
                areas,
                frontiers,
                pushed,
                grids,
                ..
            } = self;
            let mut state = PokeState::with_ledgers(
                memory, ledger, talked, targets, &*stood, &*areas, &*pushed,
            )
            .caching_grid(grids)
            .with_frontiers(&*frontiers);
            // `GameState::scene` is `pokemon_red::scene::detect` over the same reader, so the
            // palette and the scene the feed reports cannot disagree about which frame they are
            // for.
            // A warp's tear is a warp in flight: the cartridge is driving and the seam's readings
            // are the last map's under the new map's id, which is section 12.13's `Unknown` with
            // nothing on screen -- an empty pad the fly waits out, for thirty-two frames (row 58).
            let scene = if torn { Scene::Unknown } else { state.scene() };
            // Whether a conversation has ended, and how, is a question about the frames *after*
            // the `TALK` gave the buttons back, so the machine is given every frame rather than
            // only the ones it owns (`docs/design/macros.md` section 12.4).
            machine.observe_frame(&mut state);
            let palette = match mode {
                PaletteMode::Palette => Palette::for_scene(scene, &mut state),
                PaletteMode::Plan => plan::plan_for(scene, &mut state),
            };
            let bindings = bindings(&palette);
            // Where the fly is standing, for the ledger below. Only on a frame the fly is its own
            // master: while the cartridge is walking it -- a warp in flight, a ledge hop, a script
            // -- the coordinates and the loaded map header are from different frames, and a tile
            // recorded from that pair is a tile of nowhere.
            let standing = (!state.scripted() && !torn).then(|| state.player()).flatten();
            // And the tile the step in flight is landing on (row 54). Read from the same frame and
            // behind the same "the fly is its own master" gate as the ground itself.
            let stepping = standing.and_then(|_| state.stepping_onto());
            // How far the objective is, over the same map graph `GO OBJECTIVE` walks (section
            // 12.15). Read from the same frame and the same state everything else is, and only
            // where the fly is its own master, for the same reason the ground is.
            let approach = standing.and_then(|player| {
                let objective = palette::objective_place(&mut state)?;
                let hops =
                    geography::hops(geography::region_at(player.map, player.y), objective.map)?;
                Some((objective.map, hops))
            });
            *cached = Some(palette);
            (scene, bindings, standing, stepping, approach)
        };
        // Section 12.7: the macro layer's own answer to "has the run stood here", because the
        // adapter's reward ledger cannot record a doormat.
        if let Some(player) = standing {
            // New ground under the fly is the one thing that can change which tiles of this map
            // it can reach, so it is what clears the map's frontier mark (section 12.14). A tile
            // the ledger already had changes nothing and clears nothing.
            if self.stood.record(player.map, Tile::new(player.x, player.y)) {
                self.frontiers.clear(player.map);
            }
            // The tile a step in flight is landing on is ground this run has covered: the
            // cartridge owns the animation and no press stops it, and the screen has already
            // centred on it. Without this the fly's own next tile is a frontier for the fifteen
            // frames it takes to get there, which `GO FRONTIER` arrives at without moving
            // (`infra/docs/macros-traps.md` row 54).
            if let Some(onto) = stepping
                && self.stood.record(player.map, onto)
            {
                self.frontiers.clear(player.map);
            }
            // Section 13's `areaVisited(kind, area)`: the errand is paid on *entering*, so the
            // ledger is written from the same frame that records the ground. Standing on the
            // building's own map is the whole test -- the fly is inside it -- and it is written
            // only on a frame the fly is its own master, for the same reason the ground is: while
            // the cartridge is walking it through a warp the coordinates and the loaded map header
            // are from different frames.
            if let Some(kind) = geography::amenity_at(player.map)
                && let Some(area) = geography::area_of(player.map)
            {
                self.areas.record(kind, area);
            }
        }
        // Section 12.15: nearer the objective than this run has ever been, which is the other
        // thing that is plainly progress and which the ratchet's stall window cannot see in the
        // exploration ledger. A level, true on the frame the number falls and false after, so
        // there is nothing to checkpoint and nothing to drift. A different objective starts the
        // measurement again: the ladder's next rung moves as the run climbs and "nearer" means
        // nothing across two different places.
        self.nearer = match (self.nearest, approach) {
            (_, None) => false,
            (None, Some(_)) => false,
            (Some((was, best)), Some((map, hops))) => map == was && hops < best,
        };
        self.nearest = match (self.nearest, approach) {
            (_, None) => self.nearest,
            (Some((was, best)), Some((map, hops))) if map == was => Some((map, best.min(hops))),
            (_, Some(now)) => Some(now),
        };
        // The talked entry `observe_frame` may just have earned, into the ledger the next frame
        // reads.
        self.record_talk();
        Observed { scene: scene_id(scene), bindings }
    }

    fn start(
        &mut self,
        slot: u8,
        memory: &mut dyn MemoryReader,
        ledger: &dyn RunLedger,
    ) -> Started {
        let Some(palette) = self.palette else {
            // Nothing has been observed yet, so there is no palette to start a slot from.
            return Started::Refused { name: None, reason: "unobserved" };
        };
        let slot = MacroId(slot);
        let name = palette.slot(slot).map(|spec| spec.name);
        let begun = {
            let mut state = PokeState::with_ledgers(
                memory,
                ledger,
                &self.talked,
                &self.targets,
                &self.stood,
                &self.areas,
                &self.pushed,
            )
            .caching_grid(&mut self.grids)
            .with_frontiers(&self.frontiers);
            self.machine.start(&palette, slot, &mut state)
        };
        let started = match begun {
            // A started macro always has a name: the machine refuses an unbound slot before it
            // can start one. If that ever stopped being true, abandoning the macro is better
            // than publishing a nameless one and inventing its outcome later.
            Ok(()) => match name {
                Some(name) => Started::Running(name),
                None => {
                    self.machine.cancel();
                    let _ = self.machine.take_outcome();
                    Started::Refused { name: None, reason: "unbound" }
                }
            },
            Err(refused) => Started::Refused {
                // An unbound slot is "no action", not a failed macro, so it carries no name and
                // the loop reports no outcome for it.
                name: if refused.reason == Refusal::Unbound { None } else { name },
                reason: refused.reason.label(),
            },
        };
        // A `no route` refusal earns blocked-ledger entries and presses nothing, so nothing else
        // would ever collect them: the sim loop calls `step` only for a macro that started. Taken
        // here, before the next decision, so the exclusion is in the ledger the next `observe`
        // reads rather than one hold late.
        self.record_talk();
        started
    }

    fn step(&mut self, memory: &mut dyn MemoryReader, ledger: &dyn RunLedger) -> Option<u8> {
        let mask = {
            let mut state = PokeState::with_ledgers(
                memory,
                ledger,
                &self.talked,
                &self.targets,
                &self.stood,
                &self.areas,
                &self.pushed,
            )
            .caching_grid(&mut self.grids)
            .with_frontiers(&self.frontiers);
            self.machine.step(&mut state)
        };
        // A macro that just finished may have been the press that talked to something; the ledger
        // is written after the step so the state the step read is the one the fly chose in.
        self.record_talk();
        mask
    }

    fn running(&self) -> Option<&'static str> {
        self.machine.running()
    }

    fn take_finished(&mut self) -> Option<(&'static str, Outcome)> {
        let (name, abort) = self.machine.take_outcome()?;
        // An unbound slot records a nameless refusal, because the machine cannot name a macro
        // that does not exist in this scene. That is "no action" rather than an outcome: it is
        // taken (so it cannot linger into the next frame) and reported as nothing.
        if name.is_empty() {
            return None;
        }
        Some((name, outcome(abort)))
    }

    fn nearer_the_objective(&self) -> bool {
        self.nearer
    }

    fn cancel(&mut self) {
        self.machine.cancel();
        // A cancelled macro talked to nothing, and a stale entry would silence a person for the
        // rest of the session.
        let _ = self.machine.take_talked();
        // Nor did it fail at its target: a rollback is the loop's doing and not the map's, so
        // neither ledger is written. `cancel` reports `Blocked` to the feed, which is the honest
        // outcome for the macro, and excluding a target for it would punish the fly for a
        // rollback it did not cause.
        while self.machine.take_blocked().is_some() {}
        let _ = self.machine.take_timeout();
        let _ = self.machine.take_reached();
        // A rollback is not the map pushing the fly anywhere, nor its frontier going out of
        // reach: the fly is about to be standing somewhere else.
        let _ = self.machine.take_pushed();
        let _ = self.machine.take_exhausted();
        let _ = self.machine.take_refused();
        // The cached palette was dealt for a frame that is being thrown away. Dropping it makes
        // the next `start` before the next `observe` a nameless refusal, which presses nothing
        // and reports nothing, rather than a named refusal against a scene that no longer exists.
        self.palette = None;
    }
}

/// Every bound slot of `palette`, in slot order, with its gloss.
fn bindings(palette: &Palette) -> Vec<SlotBinding> {
    (0..SLOTS)
        .filter_map(|slot| {
            let spec = palette.slot(MacroId(slot))?;
            Some(SlotBinding {
                slot,
                name: spec.name,
                gloss: spec.kind.gloss(),
                channel: spec.kind.channel(),
                tag: spec.kind.channel_tag(),
            })
        })
        .collect()
}

/// `Scene` onto the feed's closed set. A forced switch is its own name because it is the one
/// battle state with a different palette; every other battle frame is `battle`.
fn scene_id(scene: Scene) -> SceneId {
    match scene {
        Scene::Title => SceneId::Title,
        Scene::Overworld => SceneId::Overworld,
        Scene::Dialog => SceneId::Dialog,
        Scene::Menu => SceneId::Menu,
        Scene::Battle { forced_switch: true, .. } => SceneId::BattleSwitch,
        Scene::Battle { .. } => SceneId::Battle,
        Scene::Shop => SceneId::Shop,
        Scene::Pc => SceneId::Pc,
        Scene::Unknown => SceneId::Unknown,
    }
}

const fn outcome(abort: MacroAbort) -> Outcome {
    match abort {
        MacroAbort::Done => Outcome::Done,
        MacroAbort::Blocked => Outcome::Blocked,
        MacroAbort::Timeout => Outcome::Timeout,
        MacroAbort::Refused => Outcome::Refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{MapEdge, MapExit};
    use crate::macros::NoLedger;
    use crate::pokemon_red::fake_wram::{self, REDS_HOUSE_1F, WALL_TILE, Wram};
    use crate::pokemon_red::macros::geography::Amenity;
    use crate::pokemon_red::maps;
    use crate::pokemon_red::macros::cartridge::{Edge, ExitId, MacroState};

    /// A ledger with one exit in it, for the wiring test below.
    struct OneVisited(MapExit);

    impl RunLedger for OneVisited {
        fn exit_visited(&self, exit: MapExit) -> bool {
            exit == self.0
        }
    }

    #[test]
    fn a_warps_tear_deals_no_pad() {
        // Row 58, measured at the Pewter Gym's door: `wCurMap` names the gym for thirty-two frames
        // while the header, the coordinates and the warp table are still Pewter City's -- "map 54
        // at (16, 17)", which is the town's doormat. A pad dealt there started a walk over the
        // wrong map, and what it was aiming at went into the ledgers under the gym's id.
        let mut wram = Wram::overworld();
        wram.map(maps::PEWTER_CITY, 20, 18, 16, 18)
            .warps(&[(16, 17, 0, maps::PEWTER_GYM), (29, 13, 0, maps::PEWTER_MUSEUM_1F)]);
        let mut palette = PokemonPalette::new(7);
        let settled = palette.observe(&mut wram, &NoLedger);
        assert_eq!(settled.scene, SceneId::Overworld);

        // The step onto the door lands and the map byte changes; nothing else has loaded.
        wram.map(maps::PEWTER_GYM, 20, 18, 16, 17);
        let torn = palette.observe(&mut wram, &NoLedger);
        assert_eq!(torn.scene, SceneId::Unknown, "a warp in flight");
        assert!(torn.bindings.is_empty(), "and nothing to press: {:?}", torn.bindings);
        assert_eq!(palette.observe(&mut wram, &NoLedger).scene, SceneId::Unknown);

        // The header loads: the gym's own size, its doormat, its own table.
        wram.map(maps::PEWTER_GYM, 5, 7, 4, 13).warps(&[(4, 13, 2, 0xff), (5, 13, 2, 0xff)]);
        assert_eq!(palette.observe(&mut wram, &NoLedger).scene, SceneId::Overworld);

        // And out again: the doormat's `LAST_MAP` under the town's id is the same tear.
        wram.map(maps::PEWTER_CITY, 5, 7, 4, 13);
        assert_eq!(palette.observe(&mut wram, &NoLedger).scene, SceneId::Unknown);
    }

    #[test]
    fn a_teleport_pad_is_not_a_tear() {
        // Saffron Gym and two Silph Co. floors warp to themselves. Standing on a pad whose
        // destination is the map the fly is on is an ordinary frame there, and an empty pad on it
        // would be a fly that waits for ever: the map byte did not change, so it is not a tear.
        let mut wram = Wram::overworld();
        wram.map(0xb2, 10, 9, 1, 1).warps(&[(1, 1, 3, 0xb2), (5, 5, 0, 0xb2)]);
        let mut palette = PokemonPalette::new(7);
        for _ in 0..3 {
            assert_eq!(palette.observe(&mut wram, &NoLedger).scene, SceneId::Overworld);
        }
        // And a tear that does not end is still bounded.
        wram.map(0x02, 10, 9, 1, 1).warps(&[(1, 1, 0, 0x02)]);
        let mut torn = 0;
        for _ in 0..(TEAR_FRAMES + 10) {
            if palette.observe(&mut wram, &NoLedger).scene == SceneId::Unknown {
                torn += 1;
            }
        }
        assert_eq!(torn, u32::from(TEAR_FRAMES), "at most {TEAR_FRAMES} frames");
    }

    #[test]
    fn the_tile_a_step_is_landing_on_is_ground_the_run_has_covered() {
        // Row 54 of `infra/docs/macros-traps.md`. `wXCoord` and `wYCoord` are the tile the step
        // began on until the frame it ends, so without this the ground under the fly is unrecorded
        // for fifteen frames of every sixteen: `path::frontier` keeps offering the tile the fly is
        // already halfway onto, `GO FRONTIER` is dealt aiming at it, and `Arrival::Step` reports
        // `done` the instant the step it did not make lands -- a macro that completes without
        // changing anything, which is section 12.2's trap.
        let (mut wram, blocks, blockset) = Wram::town();
        let mut palette = PokemonPalette::new(7);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 1, "standing still, the tile under the fly and nothing else");

        // Mid-step west: the coordinates still read (3, 4), the screen is already centred on
        // (2, 4).
        wram.mid_step(-1, 0, &blocks, &blockset);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 2, "and the tile the step is landing on");

        // The step lands. The ledger had it already, so nothing new is recorded and the frontier
        // mark is not cleared a second time.
        wram.map(fake_wram::PALLET_TOWN, 10, 9, 2, 4)
            .fill_screen(WALL_TILE)
            .screen_from_blocks(&blocks, &blockset);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 2, "the tile it landed on was already ground it had covered");
    }

    #[test]
    fn a_fresh_cartridge_reads_as_the_title_and_binds_nothing() {
        // All-zero WRAM: the game-timer bit is clear, which is the title screen.
        let mut wram = Wram::new();
        let mut palette = PokemonPalette::new(7);
        let observed = palette.observe(&mut wram, &NoLedger);
        assert_eq!(observed.scene, SceneId::Title);
        assert!(observed.bindings.is_empty(), "{:?}", observed.bindings);
        assert_eq!(palette.running(), None);
        // A slot of an empty palette presses nothing and reports no outcome.
        assert_eq!(
            palette.start(0, &mut wram, &NoLedger),
            Started::Refused { name: None, reason: "unbound" }
        );
        assert_eq!(palette.take_finished(), None);
    }

    /// A ledger whose objective is one map, for the approach reading below.
    struct Bound(u8);

    impl RunLedger for Bound {
        fn exit_visited(&self, _exit: MapExit) -> bool {
            false
        }

        fn objective(&self) -> Option<crate::adapter::MapPlace> {
            Some(crate::adapter::MapPlace {
                map: self.0,
                tile: None,
                warp: None,
                edge: None,
                target: None,
            })
        }
    }

    #[test]
    fn the_objective_getting_nearer_is_read_once_per_step_of_the_road() {
        // Section 12.15, the rung-10 stall. The ratchet's stall window is reset by ground never
        // stood on, and a fly walking a road it has already covered earns none -- so this is the
        // other reading, and what it has to be is a *level* that is true on the frame the hop
        // count falls and false on every frame after it. Pewter's own museum is the road: the
        // upper floor is three hops from the gym, the ground floor two, the town one.
        let mut wram = Wram::new();
        wram.started().map(maps::PEWTER_MUSEUM_2F, 4, 4, 3, 6).facing(0).house_collision();
        let mut palette = PokemonPalette::new(7);
        // Both of Pewter's errands discharged, so the objective is the rung's own place for the
        // whole test: section 13 puts an unvisited mart or centre *ahead* of it, and that is a
        // different place to be near.
        palette.areas.record(Amenity::Mart, maps::PEWTER_CITY);
        palette.areas.record(Amenity::Center, maps::PEWTER_CITY);
        let ledger = Bound(maps::PEWTER_GYM);

        palette.observe(&mut wram, &ledger);
        assert!(!palette.nearer_the_objective(), "the first reading is a measurement, not a step");
        palette.observe(&mut wram, &ledger);
        assert!(!palette.nearer_the_objective(), "standing still is not nearer");

        wram.map(maps::PEWTER_MUSEUM_1F, 4, 4, 3, 6);
        palette.observe(&mut wram, &ledger);
        assert!(palette.nearer_the_objective(), "down the stairs is one hop nearer");
        palette.observe(&mut wram, &ledger);
        assert!(!palette.nearer_the_objective(), "and it is read once, not held");

        // Back upstairs is not progress, and it does not undo the number either: the measurement
        // is the best this run has managed, so walking the road twice pays once.
        wram.map(maps::PEWTER_MUSEUM_2F, 4, 4, 3, 6);
        palette.observe(&mut wram, &ledger);
        assert!(!palette.nearer_the_objective());
        wram.map(maps::PEWTER_MUSEUM_1F, 4, 4, 3, 6);
        palette.observe(&mut wram, &ledger);
        assert!(!palette.nearer_the_objective(), "ground already gained is not gained again");

        wram.map(maps::PEWTER_CITY, 4, 4, 3, 6);
        palette.observe(&mut wram, &ledger);
        assert!(palette.nearer_the_objective(), "out of the front door is nearer still");
    }

    #[test]
    fn standing_on_new_ground_is_what_clears_a_maps_frontier_mark() {
        // Section 12.14's other half, wired: the mark is written by a `GO FRONTIER` that could
        // reach none of its goals and cleared by the fly standing somewhere on that map it had
        // not stood on before -- the only event that can change which tiles it can reach. A tile
        // the stood ledger already has changes nothing, which is what keeps the mark from being
        // cleared by the fly pacing the ground it has covered.
        let mut wram = Wram::overworld();
        let mut palette = PokemonPalette::new(7);
        palette.frontiers.record(REDS_HOUSE_1F);
        assert_eq!(palette.exhausted(), 1);

        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.exhausted(), 0, "the first frame is new ground, so it clears");

        palette.frontiers.record(REDS_HOUSE_1F);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(
            palette.exhausted(),
            1,
            "standing on the same tile again is not new ground and clears nothing"
        );

        wram.map(REDS_HOUSE_1F, 4, 4, 3, 5);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.exhausted(), 0, "a tile the run had not stood on clears it");
    }

    #[test]
    fn every_bound_slot_carries_a_name_and_a_gloss() {
        let mut wram = Wram::overworld();
        let mut palette = PokemonPalette::new(7);
        let observed = palette.observe(&mut wram, &NoLedger);
        assert_eq!(observed.scene, SceneId::Overworld);
        assert!(!observed.bindings.is_empty());
        for binding in &observed.bindings {
            assert!(binding.slot < SLOTS);
            assert!(!binding.name.is_empty() && binding.name.len() <= 14, "{binding:?}");
            assert!(!binding.gloss.is_empty(), "{binding:?}");
            assert!(
                binding.gloss.split_whitespace().count() <= 3,
                "a gloss is two or three words: {binding:?}"
            );
        }
        let slots: Vec<u8> = observed.bindings.iter().map(|binding| binding.slot).collect();
        let mut sorted = slots.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(slots, sorted, "slots are reported once each, in order");
    }

    /// The ledger reaches the palette: a map whose only warp is already in it has no second exit
    /// for `GO OUT` to prefer, and one that is not has one.
    ///
    /// This is the whole of the wiring under test — `AdapterLedger` -> `PokeState::with_ledger` ->
    /// `MacroState::exit_visited` -> `palette::ways` — and the fallback is what makes it
    /// observable: with every exit visited the slot is still bound (a room whose doors are all in
    /// the ledger still has to be left), so what changes is *which* exit the walk aims at, which
    /// is why this test asks the ledger directly as well.
    #[test]
    fn the_exploration_ledger_reaches_the_palette() {
        // Red's ground floor: the staircase at (7, 1) and a doormat at (2, 3) on the bottom row.
        let mut wram = Wram::overworld();
        wram.warps(&[(7, 1, 2, 0x26), (2, 3, 1, 0xff)]);
        let mut palette = PokemonPalette::new(7);

        let fresh = palette.observe(&mut wram, &NoLedger);
        assert_eq!(fresh.scene, SceneId::Overworld);
        assert!(
            fresh.bindings.iter().any(|binding| binding.name == "GO OUT"),
            "a map with two warps binds a way out: {:?}",
            fresh.bindings
        );

        // The staircase, by its tile on this map, which is how the adapter's ledger names it.
        let stairs = OneVisited(MapExit::Warp { map: REDS_HOUSE_1F, x: 7, y: 1 });
        let mut state = PokeState::with_ledger(&mut wram, &stairs);
        assert!(
            state.exit_visited(ExitId::Warp(0)),
            "the ledger's answer crosses the seam for the warp the test put in it"
        );
        assert!(!state.exit_visited(ExitId::Warp(1)), "and only for that one");
        assert!(!state.exit_visited(ExitId::Warp(9)), "a warp index past the table is not an exit");
        assert!(
            !state.exit_visited(ExitId::Edge(Edge::North)),
            "an edge of this map is a different key"
        );

        // And an edge asked for as an edge.
        let north = OneVisited(MapExit::Edge { map: REDS_HOUSE_1F, edge: MapEdge::North });
        let mut state = PokeState::with_ledger(&mut wram, &north);
        assert!(state.exit_visited(ExitId::Edge(Edge::North)));
        assert!(!state.exit_visited(ExitId::Edge(Edge::South)));
    }

    /// The tile the fly is standing on is visited, whatever the run ledger can record.
    ///
    /// `infra/docs/macros-traps.md` row 32: the adapter's `exploration` ledger is a *reward*
    /// ledger and its payout gate rejects every frame with `wMovementFlags`' door and warp bits
    /// set, so a doormat — walkable, standable ground — can never be recorded in it. The macro
    /// layer keeps its own answer, written once per frame by `observe`, and `NoLedger` here is
    /// exactly what the adapter looks like for such a tile: it records nothing at all.
    #[test]
    fn the_tile_the_fly_stands_on_is_visited_although_the_run_ledger_records_nothing() {
        // Red's ground floor, the fly at (3, 6).
        let mut wram = Wram::overworld();
        let mut palette = PokemonPalette::new(7);
        {
            let mut state = PokeState::with_ledger(&mut wram, &NoLedger);
            assert!(!state.tile_visited(3, 6), "an empty run ledger holds no ground");
        }
        assert_eq!(palette.stood(), 0, "nothing observed yet");

        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 1, "one frame on one tile is one tile of ground");
        {
            // The same seam every macro reads the ledgers through.
            let mut state = PokeState::with_ledgers(
                &mut wram,
                &NoLedger,
                &palette.talked,
                &palette.targets,
                &palette.stood,
                &palette.areas,
                &palette.pushed,
            );
            assert!(state.tile_visited(3, 6), "and the answer crosses the seam");
            assert!(!state.tile_visited(3, 5), "only the tile it stood on");
        }

        // A second frame on the same tile adds nothing; a step adds one.
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 1);
        wram.set(crate::pokemon_red::symbols::ram::wYCoord, 5);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 2);

        // A frame the cartridge is driving records nothing: the coordinates and the loaded map
        // header are from different frames then, so the pair is a tile of nowhere.
        wram.set(crate::pokemon_red::symbols::ram::wJoyIgnore, 1);
        wram.set(crate::pokemon_red::symbols::ram::wYCoord, 4);
        palette.observe(&mut wram, &NoLedger);
        assert_eq!(palette.stood(), 2, "nothing is recorded while the fly is not its own master");
    }
}
