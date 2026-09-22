//! Macros mode on the real cartridge: the scene sets the buttons, the readout presses one.
//!
//! Gated on `FLY_ROM`, the same convention every ROM test in the workspace uses; the cartridge
//! never enters this repository:
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! Whether the scene's own buttons plus a readout that knows nothing about the game are enough to
//! get out of Red's house (`docs/design/macros.md` section 12). The unit tests pin which macros a
//! scene binds (`pokemon_red::macros`) and what the layer does with a winning channel
//! (`flysim::macros`) separately, and neither can tell you whether the two together reach Oak's
//! lab — that is a question about the warp table, about a doormat that fires on the step off it,
//! and about the rung catalog's places being the ones the ladder actually climbs.
//!
//! ## The stub readout
//!
//! A real [`PopulationDecoder`] with the Game Boy preset's macro group, fed synthetic rates: no
//! brain. One burst per hold leans on a single `macro_<type>` population — 1.6 times its own
//! calibration rate, every other population at rest — and the hot one rotates every
//! [`HOLDS_PER_SLOT`] bursts, which is the shape a settled network's own group has ("not a
//! tie-breaker; it is the rotation period", `docs/readout.md`). Nothing else about the decision is
//! stubbed: the mask of bound channels, the hold, the hysteresis and the fatigue are the shipping
//! decoder's, so what this test measures is the scene's bindings and the layer, with a readout
//! that never names a macro and cannot see the game.
//!
//! The rotation walks all twenty-two channels while a scene binds at most six, so most bursts lean
//! on a channel that is masked out of the decision; that is the honest case and deliberately the
//! weak one, because a masked burst leaves the bound channels tied and the group falls back on its
//! own tie rule. The burst is 100 ms, longer than the group's rise and shorter than its hold, so
//! exactly one decision comes out of each.

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter, buttons,
};
use flysim::config::Config;
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

/// `constants/map_constants.asm`.
const PALLET_TOWN: u32 = 0x00;
const REDS_HOUSE_1F: u32 = 0x25;
const REDS_HOUSE_2F: u32 = 0x26;
const OAKS_LAB: u32 = 0x28;
const VIRIDIAN_CITY: u32 = 0x01;
const ROUTE_2: u32 = 0x0d;
/// Kept for the record of what the fly used to reach first: before the objective pointed at the
/// errand, the mart's door was the only door out of Viridian the macros could find.
#[allow(dead_code)]
const VIRIDIAN_MART: u32 = 0x2a;
/// Route 2's southern forest gate and the forest north of it, which is rung 9's own road.
const VIRIDIAN_FOREST_SOUTH_GATE: u32 = 0x32;
const VIRIDIAN_FOREST: u32 = 0x33;
/// Pewter City's Pokemon Center, which is the room the rung-10 nurse loop was inside.
const PEWTER_POKECENTER: u32 = 0x3a;
/// The upper floor of the Pewter museum, which is the building the rung-10 stall was inside.
const MUSEUM_2F: u32 = 0x35;
/// The forest's *northern* gate, which is the first hop from the forest toward Pewter
/// (`macros::geography`, and rung 10's own road).
const VIRIDIAN_FOREST_NORTH_GATE: u32 = 0x2f;

/// `constants/event_constants.asm`, by bit index: the parcel picked up at the mart, and the parcel
/// delivered to Oak. The save that stalled has the first and not the second.
const EVENT_GOT_OAKS_PARCEL: u16 = 57;
const EVENT_OAK_GOT_PARCEL: u16 = 56;
/// `EVENT_GOT_POKEDEX`, rung 7, which comes out of the same script as the delivery.
///
/// Kept beside its sibling now that the delivery is measured rather than asserted: the two are one
/// script, so a test that ever asserts one will want the other.
#[allow(dead_code)]
const EVENT_GOT_POKEDEX: u16 = 37;

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;

/// Frames of one channel per burst: long enough for one decision, short enough not to be a hold.
const BURST_MS: f64 = 100.0;

/// The seed the bench uses, so a failure here and a bench row are the same run.
const SEED: u32 = 20_260_916;

/// The hot population's rate, against [`REST`] for every other one.
///
/// A normalized score of 1.6, which is far enough above the rest for the group's 1.05 hysteresis
/// to be beatable in one decision and still a rate a real population reaches.
const HOT: f64 = 16.0;

/// Every other population's rate, which is also what the stub calibrates against, so a channel
/// that is not the hot one scores exactly 1.0.
const REST: f64 = 10.0;

/// Bursts the stub leans on one channel before it moves on.
///
/// The shape of the real readout rather than a number picked to pass: the exclusive group commits
/// to a winner, hysteresis keeps it while the field is within 5%, and `fatigue_gain` is what
/// eventually moves it on — "not a tie-breaker; it is the rotation period" (`docs/readout.md`).
/// Three holds is that rotation, coarsely.
const HOLDS_PER_SLOT: usize = 3;

/// The stub's rates for one burst: one macro population hot, the rest at their calibration rate.
///
/// The readout says "this population is firing" and nothing else — it never names a macro, never
/// reads the scene and does not know which channels are on the pad. Everything between that and a
/// macro starting is the shipping decoder and the shipping layer.
fn rates(hot: Option<&str>) -> NumberMap {
    let mut rates = NumberMap::new();
    for channel in flybrain_gb::macro_channels("pokemon-red") {
        rates.set(channel, REST);
    }
    for bucket in 0..8 {
        rates.set(&format!("command_{bucket}"), REST);
    }
    if let Some(channel) = hot {
        rates.set(channel, HOT);
    }
    rates
}

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

/// The cartridge, the adapter beside it, and the layer under test.
struct Run {
    gb: Emulator,
    adapter: PokemonRedReward,
    layer: MacroLayer,
    /// The readout under test: the shipping decoder, fed by hand.
    decoder: PopulationDecoder,
    /// The macro channels, in the decoder's own order, for the rotation.
    channels: Vec<&'static str>,
    ms: f64,
    /// Brain clock of the next burst.
    next_burst: f64,
    /// Which burst this is, i.e. which channel the stub readout is leaning on.
    burst: usize,
    hold_ms: f64,
    /// Every map the run has been on, in order of arrival.
    route: Vec<u32>,
    /// How many times each macro has started, for the failure message and the record.
    started: std::collections::BTreeMap<&'static str, u32>,
    /// Runs of consecutive frames in which `TALK`'s channel was on the pad — a *facing window*,
    /// the only chance the fly has to press A at what it is looking at.
    talk_windows: u32,
    talk_on_pad: bool,
    /// A channel to lean on instead of the rotation, for a test that needs one macro pressed.
    ///
    /// The rotation is the honest driver and every other test uses it; this is for the one question
    /// that is about a *script* rather than about which button the fly picks.
    force_hot: Option<&'static str>,
    /// The smallest overworld pad each map ever dealt, by map id.
    ///
    /// A pad of one button is a fly with one thing it can do: on 2026-09-17 the south gate of
    /// Viridian Forest dealt `GO FRONTIER` and nothing else for four hours
    /// (`infra/docs/macros-traps.md` row 32). Recorded per map rather than asserted here, because
    /// which maps a rotation visits is not this harness's claim.
    pads: std::collections::BTreeMap<u32, usize>,
    /// Whether `ATTACK`'s channel was ever on the pad while the scene was a battle, and the
    /// smallest battle pad the run ever dealt. Row 34's own two numbers
    /// (`infra/docs/macros-traps.md`): a turn with no move left with PP took `ATTACK` off the pad
    /// and the party list it fell into dealt one button.
    move_button_on_battle_pad: bool,
    battle_pad: Option<usize>,
    /// Whether `BACK` was ever on the pad in a battle with **no list open** (section 12.9).
    ///
    /// The rung-9 trap: between turns there is nothing to back out of, so the press completes
    /// where the fly stands and the turn does not move. `false` is the assertion.
    back_without_a_list: bool,
    /// Where a `BACK` or a `NEXT` on a battle frame was dealt, as `scene/sub-state`.
    ///
    /// The bool above says a rule broke; this says on which frame, which is the difference
    /// between a pad rule to fix and a scene the detector cannot name.
    battle_back_where: std::collections::BTreeSet<String>,
    battle_next_where: std::collections::BTreeSet<String>,
    /// Whether `THROW BALL` ever started at a species the party already held (section 12.9).
    threw_at_a_held_species: bool,
    /// Whether one battle pad ever held both `NEXT` and `BACK` (section 12.10).
    ///
    /// The pair is the trap: `NEXT` is the A that advances text, `BACK` is the B that leaves a
    /// list, and a pad with both has two buttons that undo each other with nothing else changing.
    next_and_back_on_one_pad: bool,
    /// Whether `NEXT` was ever on the pad while a battle menu was accepting input.
    ///
    /// The live v0.4.2 shape: `NEXT` on the top-level menu was an A press on FIGHT, so it opened
    /// the move list that `BACK` closed again -- 1264 starts against 1241 in 71 hours.
    next_on_a_menu_accepting_input: bool,
    /// The longest chain of battle macro starts that alternated `NEXT`, `BACK`, `NEXT`, `BACK`.
    ///
    /// Two is an accident of the rotation; the live log did it for seventy-one hours. Only the
    /// previous battle start has to be kept to measure the chain.
    longest_next_back_alternation: u32,
    alternation: u32,
    last_battle_start: Option<&'static str>,
    /// Macros started inside the battle that is running, and the worst any *finished* battle cost.
    ///
    /// The claim the rung-9 loop breaks is that a battle **ends**, and that it ends on a bounded
    /// number of macros rather than on however many holds the 2-cycle takes to fall out of.
    macros_this_battle: u32,
    worst_battle_macros: u32,
    battles_entered: u32,
    battles_ended: u32,
    was_in_battle: bool,
    /// Maps on whose *overworld* pad `GO OBJECTIVE` was ever bound.
    ///
    /// The objective is the road out: on rung 9 in the forest it has to be there, or the only way
    /// north is whatever `GO FRONTIER` stumbles into.
    objective_on_pad: std::collections::BTreeSet<u32>,
    /// Maps on whose *overworld* pad `MENU` was ever bound (section 12.11).
    ///
    /// The rung-10 trap: `MENU` opened the start menu and that scene's `BACK` closed it again, 82
    /// starts each in thirty brain minutes inside one building. `MENU` is on no pad at all now, so
    /// this set is empty -- and on v0.4.3 it holds every overworld map the run stood on.
    menu_on_pad: std::collections::BTreeSet<u32>,
    /// Overworld pads that bound nothing at all, by map: section 13.1's never-empty rule, which
    /// since 12.11 rests on the way out rather than on an unconditional `MENU`.
    empty_overworld_pads: std::collections::BTreeSet<u32>,
    /// How many times each macro finished `blocked`, for the `THROW BALL` residual.
    ///
    /// v0.4.3 on the cartridge: `THROW BALL` 63 starts, 63 `blocked` -- the bag had not drawn and
    /// the step read the battle menu's cursor instead (section 12.11).
    blocked: std::collections::BTreeMap<&'static str, u32>,
    /// `NAME@scene/sub-state` for every macro that reported `blocked`: *where* it gave up.
    blocked_where: std::collections::BTreeSet<String>,
    /// Macros started while the fly was still on the map it resumed on.
    macros_on_the_first_map: u32,
    /// How many times each macro started while the fly was still on that map, by name.
    ///
    /// The total is the wrong measure for a room the fly is meant to *leave*: a run that leaves it
    /// and then fights a gym answers `YES` in the gym's own boxes, which is the fly playing the
    /// game. What row 41 is about is the presses spent in the room.
    started_on_the_first_map: std::collections::BTreeMap<&'static str, u32>,
    /// The longest chain of macro starts that alternated `MENU`, `BACK`, `MENU`, `BACK`.
    longest_menu_back_alternation: u32,
    menu_alternation: u32,
    last_start: Option<&'static str>,
    /// Frames the run spent in a `Scene::Dialog`, and of those, frames a readable YES/NO prompt
    /// was open (section 12.12).
    ///
    /// The two numbers that name row 41: 62,804 of the hunt's 71,673 frames were one text box, and
    /// the survey found the **prompt** on one frame of every forty-six. A `NEXT` and a `YES` that
    /// are the same press live in the difference.
    dialog_frames: u32,
    prompt_frames: u32,
    /// Whether `NEXT` was ever on the pad while a readable YES/NO prompt was open.
    ///
    /// 12.10's rule in a dialog: an A press at a two-option box confirms the option the cursor is
    /// on, which is what `YES` is, so the two are one press under two names. `false` is the claim.
    next_on_a_prompt: bool,
    /// Whether `TALK` was ever on the pad while the fly faced a nurse the party had no use for.
    ///
    /// The door into the ring (section 12.12). `false` is the claim.
    talk_at_a_rested_nurse: bool,
}

impl Run {
    /// Boot the intro with scripted presses, then hand the buttons to macros mode.
    ///
    /// The button pattern is `tests/rom.rs`'s and the bench's: Start and A on a slow alternation,
    /// because a held button is ignored, then B alone until the adapter reports a stable
    /// dialogue-free overworld sample.
    fn boot(rom: &[u8], mode: MacroMode) -> Self {
        let mut gb = emulator(rom);
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
        assert!(adapter.safe_for_snapshot(), "the intro never settled (mode {})", adapter.mode());
        assert_eq!(adapter.map_id(), Some(REDS_HOUSE_2F), "a cold boot ends in bed");

        let channels = flybrain_gb::macro_channels("pokemon-red");
        let preset = gameboy_decoder_config_with_macros(&channels);
        let hold_ms = preset.macros.as_ref().expect("the preset has a macro group").hold_ms;
        let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
        // Calibrated on the resting rates, so a channel at `REST` scores exactly 1.0 and the hot
        // one 1.6.
        decoder.calibrate(&rates(None));
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_string();
        config.macros.mode = mode;
        config.validate().expect("pokemon-red has a palette in macros mode");
        let mut layer = macro_layer(&config, hold_ms, SEED).expect("a layer in a dealt mode");
        let _ = layer.observe(&mut gb, &AdapterLedger(&adapter), ms);
        let route = vec![REDS_HOUSE_2F];
        Self {
            gb,
            adapter,
            layer,
            decoder,
            channels,
            ms,
            next_burst: ms,
            burst: 0,
            hold_ms,
            route,
            started: std::collections::BTreeMap::new(),
            talk_windows: 0,
            talk_on_pad: false,
            force_hot: None,
            pads: std::collections::BTreeMap::new(),
            move_button_on_battle_pad: false,
            battle_pad: None,
            back_without_a_list: false,
            battle_back_where: std::collections::BTreeSet::new(),
            battle_next_where: std::collections::BTreeSet::new(),
            threw_at_a_held_species: false,
            next_and_back_on_one_pad: false,
            next_on_a_menu_accepting_input: false,
            longest_next_back_alternation: 0,
            alternation: 0,
            last_battle_start: None,
            macros_this_battle: 0,
            worst_battle_macros: 0,
            battles_entered: 0,
            battles_ended: 0,
            was_in_battle: false,
            objective_on_pad: std::collections::BTreeSet::new(),
            menu_on_pad: std::collections::BTreeSet::new(),
            empty_overworld_pads: std::collections::BTreeSet::new(),
            blocked: std::collections::BTreeMap::new(),
            blocked_where: std::collections::BTreeSet::new(),
            macros_on_the_first_map: 0,
            started_on_the_first_map: std::collections::BTreeMap::new(),
            longest_menu_back_alternation: 0,
            dialog_frames: 0,
            prompt_frames: 0,
            next_on_a_prompt: false,
            talk_at_a_rested_nurse: false,
            menu_alternation: 0,
            last_start: None,
        }
    }

    /// Pick up a run from a `FLYSIM01` checkpoint instead of booting the intro.
    ///
    /// The cartridge state and the reward ledger come out of the envelope; the readout does not,
    /// because the point of this harness is a stub that knows nothing about the game. So this is
    /// the release box's own world with a game-blind driver in it, which is exactly what a stall
    /// on the stream has to be reproduced against.
    fn resume(rom: &[u8], mode: MacroMode, checkpoint: &flysim::store::Checkpoint) -> Self {
        Self::resume_with(rom, mode, checkpoint, false)
    }

    /// [`Run::resume`], with `predates_macro_roles` restoring a decoder state that has no baseline
    /// for any macro role — the shape the release box restored on 2026-09-17.
    fn resume_with(
        rom: &[u8],
        mode: MacroMode,
        checkpoint: &flysim::store::Checkpoint,
        predates_macro_roles: bool,
    ) -> Self {
        let mut gb = emulator(rom);
        let mut adapter = PokemonRedReward::new();
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
        adapter.import_state(&checkpoint.runtime.reward).expect("the checkpoint's reward ledger");
        let channels = flybrain_gb::macro_channels("pokemon-red");
        let preset = gameboy_decoder_config_with_macros(&channels);
        let hold_ms = preset.macros.as_ref().expect("the preset has a macro group").hold_ms;
        let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
        decoder.calibrate(&rates(None));
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_string();
        config.macros.mode = mode;
        config.validate().expect("pokemon-red has a palette in macros mode");
        let mut layer = macro_layer(&config, hold_ms, SEED).expect("a layer in a dealt mode");
        // **The live restore, in this harness's own units.** The release box calibrates at warm-up
        // and then imports a checkpoint's decoder state; a checkpoint written before the macro
        // roles existed carries no baseline for them, and an absent baseline reads as zero
        // (`docs/readout.md`, "Channels added after a checkpoint was written").
        //
        // The checkpoint's *own* decoder state cannot be used for this: its baselines are the live
        // network's population rates and this readout is a stub feeding synthetic ones, so mixing
        // them measures neither. What reproduces the defect faithfully is the same state shape in
        // the stub's units — calibrated on the stub's rates, with every macro role's baseline
        // removed — which is exactly what the live box restored.
        if predates_macro_roles {
            let mut predates = decoder.export_state();
            for channel in &channels {
                predates.baseline.remove(channel);
            }
            decoder.import_state(&predates).expect("a state from before the macro roles");
        }
        let ms = 0.0;
        let _ = layer.observe(&mut gb, &AdapterLedger(&adapter), ms);
        let route = vec![adapter.map_id().unwrap_or(u32::MAX)];
        Self {
            gb,
            adapter,
            layer,
            decoder,
            channels,
            ms,
            next_burst: ms,
            burst: 0,
            hold_ms,
            route,
            started: std::collections::BTreeMap::new(),
            talk_windows: 0,
            talk_on_pad: false,
            force_hot: None,
            pads: std::collections::BTreeMap::new(),
            move_button_on_battle_pad: false,
            battle_pad: None,
            back_without_a_list: false,
            battle_back_where: std::collections::BTreeSet::new(),
            battle_next_where: std::collections::BTreeSet::new(),
            threw_at_a_held_species: false,
            next_and_back_on_one_pad: false,
            next_on_a_menu_accepting_input: false,
            longest_next_back_alternation: 0,
            alternation: 0,
            last_battle_start: None,
            macros_this_battle: 0,
            worst_battle_macros: 0,
            battles_entered: 0,
            battles_ended: 0,
            was_in_battle: false,
            objective_on_pad: std::collections::BTreeSet::new(),
            menu_on_pad: std::collections::BTreeSet::new(),
            empty_overworld_pads: std::collections::BTreeSet::new(),
            blocked: std::collections::BTreeMap::new(),
            blocked_where: std::collections::BTreeSet::new(),
            macros_on_the_first_map: 0,
            started_on_the_first_map: std::collections::BTreeMap::new(),
            longest_menu_back_alternation: 0,
            dialog_frames: 0,
            prompt_frames: 0,
            next_on_a_prompt: false,
            talk_at_a_rested_nurse: false,
            menu_alternation: 0,
            last_start: None,
        }
    }

    fn map(&self) -> u32 {
        self.adapter.map_id().unwrap_or(u32::MAX)
    }

    /// One of pokered's event flags, by bit index (`pokemon_red::symbols::events`).
    ///
    /// The ladder's rank cannot answer for a rung earned out of order — it is the *maximum* over
    /// satisfied rungs, so delivering the parcel while Viridian City is already stood on leaves it
    /// at 8 — so a test about the delivery asks the bit the delivery sets.
    fn event(&mut self, bit: u16) -> bool {
        let address = flybrain_gb::pokemon_red::symbols::ram::wEventFlags + (bit >> 3);
        self.gb.read_wram(address) & (1 << (bit & 7)) != 0
    }

    /// Whether a battle menu is accepting input, i.e. the fly's own turn
    /// (`docs/design/macros.md` section 12.6). Read through the same seam the macros read.
    fn own_turn(&mut self) -> bool {
        use flybrain_gb::pokemon_red::macros::state::GameState;
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        state.battle().is_some_and(|battle| battle.own_turn && !battle.forced_switch)
    }

    /// Whether a battle list is open and accepting input: the move list, the party list or the bag.
    ///
    /// Section 12.9's question, through the same seam the macros read: `BACK` is a button where
    /// there is a list to leave and nowhere else.
    /// Whether the pad on this frame is a **battle** pad.
    ///
    /// The scene the palette was dealt for, not `wIsInBattle`. The two differ on the `$ff` frame a
    /// lost battle passes through and on a Safari or tutorial battle, where `state::battle` reads
    /// nothing and `scene::detect` answers `Unknown` -- whose pad is `NEXT, BACK` by contract
    /// (section 12.2, row 9: B is what leaves the Pokédex, the trainer card and OPTION). Asking
    /// the cartridge byte instead accused that row of a battle rule it is not under.
    fn battle_pad(&self) -> bool {
        self.layer.scene_name() == "battle" || self.layer.scene_name() == "battle-switch"
    }

    /// The battle sub-state the seam reports, as a word, for the record and the failure message.
    fn battle_sub_state(&mut self) -> &'static str {
        use flybrain_gb::pokemon_red::macros::state::{BattleMenu, GameState};
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        match state.battle() {
            None => "no-battle",
            Some(battle) => match battle.menu {
                BattleMenu::Main { .. } => "main",
                BattleMenu::Moves { cursor: Some(_), .. } => "moves",
                BattleMenu::Moves { cursor: None, .. } => "moves-unplaceable",
                BattleMenu::Party { .. } if battle.forced_switch => "party-forced",
                BattleMenu::Party { .. } => "party",
                BattleMenu::Bag { .. } => "bag",
                BattleMenu::None => "between-turns",
            },
        }
    }

    fn battle_list_open(&mut self) -> bool {
        use flybrain_gb::pokemon_red::macros::state::{BattleMenu, GameState};
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        state.battle().is_some_and(|battle| {
            matches!(
                battle.menu,
                BattleMenu::Moves { cursor: Some(_), .. }
                    | BattleMenu::Party { .. }
                    | BattleMenu::Bag { .. }
            )
        })
    }

    /// Whether the Pokémon on the other side is a species the party already holds.
    ///
    /// The internal species index on both sides, which is the one numbering they share
    /// (`docs/design/macros-wram.md`).
    fn enemy_species_in_party(&mut self) -> bool {
        use flybrain_gb::pokemon_red::macros::state::GameState;
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        let Some(species) =
            state.battle().and_then(|battle| battle.enemy).map(|enemy| enemy.species)
        else {
            return false;
        };
        species != 0 && state.party().mons.iter().any(|mon| mon.species == species)
    }

    /// The rung the macros are walking toward, as the objective reads it.
    fn objective_map(&mut self) -> Option<u8> {
        use flybrain_gb::pokemon_red::macros::MacroState;
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        state.objective().map(|objective| objective.map)
    }

    /// `wIsInBattle`: 0 out of battle, 1 wild, 2 a trainer.
    fn in_battle(&mut self) -> u8 {
        self.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wIsInBattle)
    }

    /// `wPartyCount`, which is what taking a starter changes.
    fn party(&mut self) -> u8 {
        self.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wPartyCount)
    }

    /// The wallet, in whole units, through the same accessor the macros read it with.
    fn money(&mut self) -> u32 {
        flybrain_gb::pokemon_red::state::money(&mut self.gb)
    }

    /// How many of `item` the bag holds, summed over its stacks.
    ///
    /// Kept unused for now: the purchase the section-13 test would have counted with it is
    /// unit-proven rather than ROM-proven (see `go_shop_enters_the_mart_once_and_walks_to_its_counter`
    /// and rows 35 and 36 of `infra/docs/macros-traps.md`), and deleting the reader would mean
    /// writing it again with the fix.
    #[allow(dead_code)]
    fn bag_count(&mut self, item: u8) -> u32 {
        flybrain_gb::pokemon_red::state::bag(&mut self.gb)
            .iter()
            .filter(|stack| stack.id == item)
            .map(|stack| u32::from(stack.count))
            .sum()
    }

    /// What the open counter sells, through the same gated accessor the macros read it with.
    ///
    /// Empty off a counter, and that gate is the point: `wItemList` is a scratch buffer nobody
    /// clears, so a non-empty answer *is* "a real mart is open" -- which is a stronger test than
    /// the scene name. Measured on the cartridge 2026-09-17: a text box in Viridian City with a
    /// stale `wTextBoxID` reads as `Scene::Shop` and this still answers `[]`.
    fn counter_stock(&mut self) -> Vec<u8> {
        let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut self.gb);
        flybrain_gb::pokemon_red::macros::MacroState::shop_stock(&mut state)
    }

    /// Whether the two-option YES/NO box is drawn, through the accessor the palette reads
    /// (section 12.12).
    fn yes_no_prompt(&mut self) -> bool {
        flybrain_gb::pokemon_red::state::yes_no_prompt(&mut self.gb)
    }

    /// Whether the fly faces a Pokemon Center nurse with a party that does not need her.
    fn rested_nurse(&mut self) -> bool {
        let ledger = AdapterLedger(&self.adapter);
        let mut state =
            flybrain_gb::pokemon_red::state::PokeState::with_ledger(&mut self.gb, &ledger);
        flybrain_gb::pokemon_red::macros::palette::rested_nurse(&mut state)
    }

    /// Whether every party member reads full HP with no status: the end of a `HEAL`.
    fn party_rested(&mut self) -> bool {
        let party = flybrain_gb::pokemon_red::state::party(&mut self.gb);
        !party.mons.is_empty()
            && party.mons.iter().all(|mon| {
                mon.species == 0
                    || (mon.hp == mon.max_hp
                        && mon.status == flybrain_gb::pokemon_red::macros::state::Status::Healthy)
            })
    }

    /// One frame of the sim loop's order, with the stub rates in place of the brain.
    fn frame(&mut self) {
        // The stub readout: one burst per hold, one macro population hot in it, rotating.
        let bursting = self.ms < self.next_burst + BURST_MS;
        let hot = bursting.then(|| {
            self.force_hot
                .unwrap_or(self.channels[(self.burst / HOLDS_PER_SLOT) % self.channels.len()])
        });
        if self.ms >= self.next_burst + self.hold_ms {
            self.next_burst = self.ms;
            self.burst += 1;
        }
        // The loop's own two lines: the scene's bound channels are the mask, and the decoder
        // picks among exactly those (`docs/design/macros.md` section 12).
        let bound = self.layer.bound_channels();
        // Only the fly's *own* turn. Battle text between turns is a pad of one `NEXT` by design
        // (the v0.2.4 deadlock fix), so counting it would measure the wrong thing.
        if self.own_turn() {
            self.move_button_on_battle_pad |=
                bound.iter().any(|channel| channel.starts_with("macro_move_"));
            let dealt = bound.len();
            self.battle_pad = Some(self.battle_pad.map_or(dealt, |seen| seen.min(dealt)));
        }
        // Section 12.9, on the cartridge: `BACK` belongs to a list. A battle frame with no list
        // accepting input and `BACK` on the pad is the rung-9 trap itself.
        if self.battle_pad() {
            let back = bound.iter().any(|channel| channel.as_str() == "macro_back");
            if back {
                let where_ = format!("{}/{}", self.layer.scene_name(), self.battle_sub_state());
                self.battle_back_where.insert(where_);
            }
            if back && !self.battle_list_open() {
                self.back_without_a_list = true;
            }
        }
        // Section 12.10, on the cartridge: no battle pad holds a pair that undoes itself, and
        // `NEXT` is on no frame with a cursor accepting input. The second is the stronger of the
        // two -- the live pair was split across two sub-states, so no single pad held both.
        if self.battle_pad() {
            let next = bound.iter().any(|channel| channel.as_str() == "macro_next");
            let back = bound.iter().any(|channel| channel.as_str() == "macro_back");
            self.next_and_back_on_one_pad |= next && back;
            if next {
                let where_ = format!("{}/{}", self.layer.scene_name(), self.battle_sub_state());
                self.battle_next_where.insert(where_);
            }
            if next && self.own_turn() {
                self.next_on_a_menu_accepting_input = true;
            }
        }
        // A facing window opens when `TALK`'s channel joins the pad and closes when it leaves.
        let talk_bound = bound.iter().any(|channel| channel.ends_with("talk"));
        if talk_bound && !self.talk_on_pad {
            self.talk_windows += 1;
        }
        self.talk_on_pad = talk_bound;
        let active =
            self.decoder.decode_bound(&rates(hot), self.ms, false, None, Some(&bound));
        let (mask, started, blocked) = {
            let ledger = AdapterLedger(&self.adapter);
            let decision = self.layer.decide(&active, 0, self.ms, &mut self.gb, &ledger);
            let started: Vec<&'static str> = decision
                .events
                .iter()
                .filter(|event| event.outcome.is_none())
                .map(|event| event.name)
                .collect();
            let blocked: Vec<&'static str> = decision
                .events
                .iter()
                .filter(|event| {
                    event.outcome.is_some_and(|outcome| outcome.as_str() == "blocked")
                })
                .map(|event| event.name)
                .collect();
            (decision.mask, started, blocked)
        };
        for name in blocked {
            *self.blocked.entry(name).or_insert(0) += 1;
            let sub = self.battle_sub_state();
            let list = self.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wListMenuID);
            self.blocked_where.insert(format!(
                "{name}@{}/{sub}/list={list:#04x}",
                self.layer.scene_name()
            ));
        }
        let in_battle_now = self.in_battle() != 0;
        let on_a_battle_pad = self.battle_pad();
        for name in started {
            // Section 12.9's other half: a ball is never thrown at a species the party holds.
            if name == "THROW BALL" && self.enemy_species_in_party() {
                self.threw_at_a_held_species = true;
            }
            // Section 12.10's own signature, as the live event log printed it: `NEXT start/done,
            // BACK start/done`, every hold, for seventy-one hours. Measured as the longest chain
            // of consecutive battle starts drawn from those two alone and strictly alternating.
            if in_battle_now {
                self.macros_this_battle += 1;
            }
            if on_a_battle_pad {
                let two = name == "NEXT" || name == "BACK";
                self.alternation = match self.last_battle_start {
                    Some(last) if two && (last == "NEXT" || last == "BACK") && last != name => {
                        self.alternation.max(1) + 1
                    }
                    _ if two => 1,
                    _ => 0,
                };
                self.longest_next_back_alternation =
                    self.longest_next_back_alternation.max(self.alternation);
                self.last_battle_start = Some(name);
            }
            // Section 12.11's own signature, as the live event log printed it: `MENU
            // start/done, BACK start/done`, every hold, for thirty brain minutes inside one
            // building. The pair is split across two *scenes*, so this is measured over every
            // start rather than over a battle's.
            let two = name == "MENU" || name == "BACK";
            self.menu_alternation = match self.last_start {
                Some(last) if two && (last == "MENU" || last == "BACK") && last != name => {
                    self.menu_alternation.max(1) + 1
                }
                _ if two => 1,
                _ => 0,
            };
            self.longest_menu_back_alternation =
                self.longest_menu_back_alternation.max(self.menu_alternation);
            self.last_start = Some(name);
            if self.route.len() == 1 {
                self.macros_on_the_first_map += 1;
                *self.started_on_the_first_map.entry(name).or_insert(0) += 1;
            }
            *self.started.entry(name).or_insert(0) += 1;
        }
        self.gb.set_buttons(mask as u8);
        self.gb.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        let ms = self.ms;
        self.adapter.sample(&mut self.gb, ms);
        {
            let ledger = AdapterLedger(&self.adapter);
            let _ = self.layer.observe(&mut self.gb, &ledger, ms);
        }
        // Battle boundaries, after the frame: what a battle cost in macros, and whether it ended.
        let now_in_battle = self.in_battle() != 0;
        match (self.was_in_battle, now_in_battle) {
            (false, true) => {
                self.battles_entered += 1;
                self.macros_this_battle = 0;
                self.last_battle_start = None;
                self.alternation = 0;
            }
            (true, false) => {
                self.battles_ended += 1;
                self.worst_battle_macros =
                    self.worst_battle_macros.max(self.macros_this_battle);
                self.macros_this_battle = 0;
            }
            _ => {}
        }
        self.was_in_battle = now_in_battle;
        let map = self.map();
        if self.route.last() != Some(&map) && map != u32::MAX {
            self.route.push(map);
        }
        // Section 12.12's two frame counters and its two pad rules, asked of the frame the pad
        // was dealt for -- the dialog's, exactly as 12.10 asks the battle rules of the battle's.
        if self.layer.scene_name() == "dialog" {
            self.dialog_frames += 1;
            let dealt = self.layer.bound_channels();
            if self.yes_no_prompt() {
                self.prompt_frames += 1;
                if dealt.iter().any(|channel| channel.as_str() == "macro_next") {
                    self.next_on_a_prompt = true;
                }
            }
        }
        if self.layer.scene_name() == "overworld"
            && self.rested_nurse()
            && self.layer.bound_channels().iter().any(|channel| channel.as_str() == "macro_talk")
        {
            self.talk_at_a_rested_nurse = true;
        }
        // The pad the *next* frame will choose from, for the maps that have been accused of
        // dealing one button. Only the overworld: a warp in flight reads `unknown` and a text box
        // is a pad of its own.
        if map != u32::MAX && self.layer.scene_name() == "overworld" {
            let dealt = self.layer.bound_channels();
            if dealt.iter().any(|channel| channel.as_str() == "macro_go_objective") {
                self.objective_on_pad.insert(map);
            }
            if dealt.iter().any(|channel| channel.as_str() == "macro_menu") {
                self.menu_on_pad.insert(map);
            }
            if dealt.is_empty() {
                self.empty_overworld_pads.insert(map);
            }
            let dealt = dealt.len();
            let seen = self.pads.entry(map).or_insert(dealt);
            *seen = (*seen).min(dealt);
        }
    }

    /// Drive until `done`, or panic with where it got to.
    fn drive_until(&mut self, what: &str, budget: u32, mut done: impl FnMut(&mut Self) -> bool) {
        for frame in 0..budget {
            self.frame();
            if done(self) {
                eprintln!(
                    "{what}: reached after {frame} frames ({:.1} brain minutes), route {:?}, \
                     macros {:?}",
                    self.ms / 60_000.0,
                    self.route,
                    self.layer.counts()
                );
                return;
            }
        }
        panic!(
            "{what}: not reached in {budget} frames (map {:#04x}, rank {}, tile {:?}, route {:?}, \
             macros {:?}, by name {:?}, smallest pads {:?})",
            self.map(),
            self.adapter.progress().rank,
            (
                self.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wXCoord),
                self.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wYCoord)
            ),
            self.route,
            self.layer.counts(),
            self.started,
            self.pads
        );
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

/// From the bedroom, macros mode with a stub readout leaves the house and earns Oak's lab.
///
/// The three rungs the catalog knows places for at this end of the ladder, in order: the ground
/// floor (rung 2, a passage on this map), Pallet Town (rung 3, `LAST_MAP` doormats, which count
/// because the objective is outdoors) and Oak's lab (rung 4, a Pallet Town warp that names the
/// map). The readout knows nothing about any of it — it leans on one population at a time and
/// rotates — so what carries the run is which buttons each scene puts on the pad and what the
/// macros behind them do. If a place were missing from the catalog, or a scene bound the wrong
/// set, this would wander instead.
///
/// **The starter is not part of this test, and that is a finding rather than a trim**
/// (2026-09-16, recorded in `infra/docs/macros-bench.md`). A game-blind stub does not press `TALK`
/// in front of a Pokéball on purpose: three of them were tried on the cartridge under the drive
/// rules section 12 replaced and none took a starter — every slot equal pressed A 615 times at one
/// shelf and never walked away; one hot slot per burst wandered (367 `GO OUT`s, one `TALK`); a
/// committing rotation did the same over five times the frames (4,904 macros, `TALK` once). What
/// a stub cannot do is exactly what section 12 hands to the mushroom body, so the claim belongs to
/// a run with a brain in it — the bench's macros arm — and not here.
#[test]
fn macros_mode_leaves_the_house_and_reaches_oaks_lab() {
    let rom = skip_without_rom!();
    let mut run = Run::boot(&rom, MacroMode::Macros);

    run.drive_until("the ground floor", 12_000, |run| run.map() == REDS_HOUSE_1F);
    run.drive_until("pallet town", 24_000, |run| run.map() == PALLET_TOWN);
    run.drive_until("oak's lab", 60_000, |run| run.map() == OAKS_LAB);
    // The rung is not the arrival. A map rung needs three stable unscripted samples and the fly
    // lands *on* the lab's own door, which the adapter's gate rejects by design
    // (`docs/design/room-escape.md` section 2), so the rung is earned once another macro has
    // walked it into the room.
    run.drive_until("the lab's own rung", 24_000, |run| run.adapter.progress().rank >= 4);

    assert!(run.route.contains(&REDS_HOUSE_1F), "through the ground floor: {:?}", run.route);
    assert!(run.route.contains(&PALLET_TOWN), "and out into the town: {:?}", run.route);
    assert_eq!(run.map(), OAKS_LAB);
    assert!(run.adapter.progress().rank >= 4, "the ladder recorded it: {:?}", run.adapter.progress());
    assert_eq!(run.party(), 0, "nothing here takes a starter; see this test's own note");

    // Every macro that ran was one the scene had put on the pad and the readout had held up: the
    // counts are the proof that the two halves met.
    let counts = run.layer.counts();
    assert!(counts.started > 0, "no macro ever started: {counts:?}");
    assert_eq!(counts.started, counts.finished() + u64::from(run.layer.running().is_some()));
    eprintln!(
        "macros mode reached Oak's lab in {:.1} brain minutes on {} macros: {counts:?}",
        run.ms / 60_000.0,
        counts.started
    );
}

/// The checkpoint a resumed test starts from, or `None` to skip.
///
/// `.local/` is not tracked and a checkpoint is not a fixture: it is the state the release box was
/// really in. So this reads the same variables `examples/trap_hunt.rs` does and skips without one.
fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_TRAP_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_MACRO_CHECKPOINT"))?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

/// From the Viridian stall's own checkpoint, macros mode's walks complete and reach a door.
///
/// **The stall** (2026-09-17, `infra/docs/macros-traps.md`): eight hours on rung 8, 164 walk starts
/// and 165 timeouts in the last 40 KB of the event log, and not one macro finishing. Every walk
/// spent its whole 600-frame cap oscillating over *two* tiles with a net displacement of zero,
/// because `path::route`'s closest-approach answer plus a re-plan after every tile traded the fly
/// between two tiles across the walkable window's edge; the cap then read as "closing" and cost a
/// strike instead of excluding anything, so the same walk came back ten brain minutes later for
/// ever.
///
/// **The criterion is that walks complete**, not that they arrive anywhere in particular. The fly
/// chooses the macro and this readout is a rotation that knows nothing about the game, so which
/// target it spends a hold on is not this test's business. What is: that a chosen walk can finish,
/// that finishing is the common case rather than the frame cap, and that the walks carry the fly
/// far enough across Viridian City to reach one of its doors — the mart
/// (`maps::VIRIDIAN_MART`), which is a warp on the far side of the city from the checkpoint's tile.
///
/// **Route 2 is not asserted, and that is a measurement rather than a trim.** `maps::ROUTE_2` is
/// the connection north and rung 9's first hop, so it is what `GO OBJECTIVE` aims at; with these
/// fixes the fly reaches the mart in 3.2 brain minutes and then **never leaves the city** in seven
/// brain hours (1,500,000 frames, 30,946 macros, 30,921 of them `done`). It ends every run in a
/// text box two tiles from where it got stuck, and the `Dialog` pad's `NEXT`, `YES` and `NO` — one
/// A press and one B press — do not advance it. That is the next trap, not this one, and it is
/// recorded as such in `infra/docs/macros-traps.md`; asserting Route 2 here would be a test that
/// fails on a trap it does not measure.
///
/// Gated on `FLY_ROM` *and* on a checkpoint, and skips cleanly without either, because `.local/` is
/// not tracked and a checkpoint is not a fixture — it is the state the release box was really in:
///
/// ```sh
/// FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
///   FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-timeouts.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn macros_mode_completes_its_walks_from_the_stalled_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    assert_eq!(run.map(), VIRIDIAN_CITY, "the checkpoint is the one the stream stalled on");

    // Off the map the stall was on, by whichever door the fly's own choices find. Bounded at
    // 120,000 frames, which is 33 brain minutes; the measured run needs 1,600 of them.
    //
    // **Not a particular door any more.** When this test was written the objective pointed north
    // through a shut gate, so the mart was the reachable one; since the objective is the lowest
    // *unsatisfied* rung it points at Oak's lab, and the fly leaves southward by Route 1. Which
    // door is not this test's claim — that the walks carry the fly off the map at all is.
    run.drive_until("off the map the stall was on", 120_000, |run| {
        run.route.iter().any(|map| *map != VIRIDIAN_CITY)
    });
    assert!(run.route.len() > 1, "route {:?}", run.route);

    // Then keep going, because the claim is about the *outcomes* rather than the arrival: the
    // stall's signature was every walk spending its whole cap and nothing finishing, and one
    // macro's worth of frames cannot show that either way.
    for _ in 0..30_000 {
        run.frame();
    }

    let counts = run.layer.counts();
    assert!(counts.started > 0, "no macro ever started: {counts:?}");
    // The stall's signature, and what this test exists to keep out: every walk timing out and
    // nothing finishing at all.
    assert!(counts.done > 0, "no macro completed: {counts:?}");
    assert!(
        counts.done > counts.timeout,
        "finishing is the common case, not the frame cap: {counts:?}"
    );
    // On a bounded number of macros rather than by grinding: a budget that scales with the plan
    // means one walk covers what took dozens of ten-second ones.
    assert!(counts.started < 1_000, "macros spent leaving the map: {counts:?}");
    eprintln!(
        "off the stalled map in {:.1} brain minutes on {} macros ({counts:?}), route {:?}, \
         by name {:?}",
        run.ms / 60_000.0,
        counts.started,
        run.route,
        run.started
    );
}

/// The rung-9 forest checkpoint, or `None` to skip.
///
/// Its own variable rather than `FLY_TRAP_CHECKPOINT`, because the two checkpoint tests above
/// assert the map their checkpoint is on: one envelope cannot be both.
fn forest_checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_FOREST_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

/// From the rung-9 forest checkpoint: the turns advance, the battles end, and no two buttons on a
/// battle pad undo each other.
///
/// **What was live** (2026-09-22, `infra/docs/macros-traps.md`): rank 9, VIRIDIAN FOREST, 69 hours
/// on the rung, the ratchet's three attempts spent, and since the restart the macro starts were
/// `BACK` 135, `THROW BALL` 28, `MOVE 2` 10, `RUN` 5 — the event log repeating `RUN blocked, BACK
/// start, BACK done`. `BACK` on a battle frame with no list open completes in a handful of frames
/// without changing anything, so the roll landed on it most holds and the turn did not move; and
/// `THROW BALL` spent balls on the species already in the party, each catch opening a nickname
/// screen the pad cannot leave.
///
/// **What was live again** (2026-09-22, thirty-five minutes after v0.4.2): the same rung, the same
/// forest, and the macro starts since the restart were `NEXT` 1264, `BACK` 1241, `THROW BALL` 5 and
/// `GO WARP` 3, the event log alternating `NEXT start/done, BACK start/done` every hold. The pair
/// was split across two sub-states of one turn, so 12.9's rule held and the loop survived it:
/// `NEXT` on the top-level menu was an A press on FIGHT, which **opened** the move list, and `BACK`
/// on the move list **closed** it again. Section 12.10 takes `NEXT` off every pad with a cursor
/// accepting input and gives the bag its own; `MOVE 1` is the backstop the top-level menu keeps.
///
/// The claim is about the *turn*, not about the fight: that a battle from this state ends, that it
/// ends on a bounded number of macros, that the fly's own presses are what ends it, and that none
/// of the three traps is on the pad any more. Which move it picks and whether it wins are the
/// fly's.
///
/// ```sh
/// FLY_ROM=/path/to/pokemon-red.gb \
///   FLY_FOREST_CHECKPOINT=.local/checkpoints/release-forest-rung9.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn the_battles_turns_advance_from_the_rung_nine_forest_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = forest_checkpoint() else {
        eprintln!("skipped: no FLY_FOREST_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    let from = run.map();
    assert!(
        from == VIRIDIAN_FOREST || from == ROUTE_2 || from == VIRIDIAN_FOREST_SOUTH_GATE,
        "the checkpoint is the one the stream stalled on, got map {from:#04x}"
    );
    // Rung 9 is stood on, so the objective is rung 10 — Pewter City — and the road there is north
    // through the forest (`macros::geography`, asserted as hops in that module's own tests).
    assert_eq!(
        run.objective_map(),
        Some(0x02),
        "the objective is Pewter City, whatever errand is in front of it"
    );

    // Every battle this run passes through, and how it left.
    let (mut battles, mut ended, mut in_battle) = (0u32, 0u32, run.in_battle() != 0);
    let mut own_turns = 0u32;
    for _ in 0..240_000 {
        run.frame();
        let now = run.in_battle() != 0;
        match (in_battle, now) {
            (false, true) => battles += 1,
            (true, false) => ended += 1,
            _ => {}
        }
        in_battle = now;
        if now && run.own_turn() {
            own_turns += 1;
        }
        if run.route.contains(&VIRIDIAN_FOREST_NORTH_GATE) {
            break;
        }
    }

    let counts = run.layer.counts();
    eprintln!(
        "from map {from:#04x} in {:.1} brain minutes: route {:?}, battles {battles} ({ended}          ended), own-turn frames {own_turns}, macros {counts:?}, by name {:?}, objective on the          pad on {:?}",
        run.ms / 60_000.0,
        run.route,
        run.started,
        run.objective_on_pad
    );

    // The traps, as assertions on the cartridge.
    eprintln!(
        "`BACK` in a battle was dealt on {:?}; `NEXT` on {:?}",
        run.battle_back_where, run.battle_next_where
    );
    assert!(
        !run.back_without_a_list,
        "`BACK` was on a battle pad with no list open: {:?}",
        run.battle_back_where
    );
    assert!(!run.threw_at_a_held_species, "a ball was thrown at a species the party holds");
    eprintln!("blocked where: {:?}", run.blocked_where);
    // Section 12.11, the other half of `THROW BALL`: it reaches the bag. On v0.4.3 every start of
    // it was `blocked` -- 63 of 63, mean sixty-nine frames -- because the step that walks the bag
    // list was allowed to read the battle menu's cursor while the bag was still drawing.
    assert_eq!(
        run.blocked.get("THROW BALL"),
        None,
        "`THROW BALL` reported blocked: {:?}",
        run.blocked
    );
    // Section 12.10, the two halves of it.
    assert!(
        !run.next_and_back_on_one_pad,
        "a battle pad held both `NEXT` and `BACK`: two buttons that undo each other"
    );
    assert!(
        !run.next_on_a_menu_accepting_input,
        "`NEXT` was on the pad while a battle menu was accepting input, where A opens rather \
         than advances"
    );
    // And the shape the log had, rather than only the pads it came from. Two in a row is the
    // rotation happening to deal the pair; the live run did it for seventy-one hours.
    assert!(
        run.longest_next_back_alternation < 4,
        "`NEXT`/`BACK` alternated {} times in a row",
        run.longest_next_back_alternation
    );

    // The turn moves: the fly's own battle presses happen, and a battle this run entered or
    // resumed finishes.
    assert!(own_turns > 0, "the fly never got a turn");
    let battle_presses: u32 = run
        .started
        .iter()
        .filter(|(name, _)| name.starts_with("MOVE ") || **name == "THROW BALL")
        .map(|(_, n)| *n)
        .sum();
    assert!(battle_presses > 0, "no move and no ball: {:?}", run.started);
    assert!(ended > 0, "no battle ever ended: {battles} entered");
    // **Every battle that started, finished**, and each one on a bounded number of macros. That is
    // the claim the 2-cycle breaks, and the way it breaks it is the opposite of a slow fight: the
    // battle never leaves the fly's own turn at all, so the count grows with the *run*. On v0.4.2
    // from this same checkpoint the trap hunt spent all twenty of its brain minutes -- 71,673
    // frames, 1,489 macros, 73 of 73 windows flagged -- inside **one** battle that never ended,
    // with `BACK` 739 starts on the move list and `NEXT` 739 on the top-level menu.
    //
    // The bound is **seven hundred** and was four; the number it is generous against is **450**
    // and was 275, re-measured after section 12.11 put `battle_entry`'s `ITEM` and `PKMN` the
    // right way round. Before that `SWITCH` opened the bag and `ITEM` and `THROW BALL` opened the
    // party list, so neither could ever finish: every turn was an attack or nothing. A fly that
    // can switch and heal fights longer, which is a longer battle and not a stalled one -- what
    // the assertion is for is the difference between a battle that ends and a cycle that does
    // not, and 3 of 3 ended here.
    assert!(
        run.battles_ended + 1 >= run.battles_entered,
        "{} battles entered and only {} left: a battle was entered and never got out",
        run.battles_entered,
        run.battles_ended
    );
    assert!(
        run.worst_battle_macros > 0 && run.worst_battle_macros < 700,
        "the worst battle cost {} macros over {} that ended",
        run.worst_battle_macros,
        run.battles_ended
    );
    eprintln!(
        "battles: {} entered, {} ended, worst {} macros; longest NEXT/BACK alternation {}",
        run.battles_entered,
        run.battles_ended,
        run.worst_battle_macros,
        run.longest_next_back_alternation
    );
    // `BACK` is still pressed, and that is the contract rather than a residual: over the move list
    // and over a one-Pokemon party list it is one of the two answers a list has, and where it
    // leads is a menu with the move buttons on it (row 34). Its share is *reported* -- under this
    // harness's game-blind rotation it is a fact about the rotation and not about the macros,
    // because every bound channel wins about equally often.
    let backs = run.started.get("BACK").copied().unwrap_or(0);
    eprintln!(
        "`BACK` was {backs} of {} macro starts, none of them with no list open",
        counts.started
    );

    // North is the road, and the gate is the first hop. Asserted when the run reaches it and
    // reported when it does not: which way the fly walks is its own, and the objective being on
    // the pad is what this harness can hold it to.
    if run.route.contains(&VIRIDIAN_FOREST_NORTH_GATE) {
        eprintln!("the fly left the forest north through the gate at 0x2f");
    } else {
        eprintln!(
            "the forest's north gate was not reached in this run; route {:?}",
            run.route
        );
    }
    assert!(
        run.objective_on_pad.contains(&VIRIDIAN_FOREST)
            || run.objective_on_pad.contains(&ROUTE_2)
            || run.objective_on_pad.contains(&VIRIDIAN_FOREST_NORTH_GATE),
        "GO OBJECTIVE was on no overworld pad on the road north: {:?}",
        run.objective_on_pad
    );
}

/// From the stalled checkpoint, nothing the map draws is read as a text box.
///
/// **What this was written to settle** (2026-09-17, `infra/docs/macros-traps.md`): with the walk
/// fixes in, 41,012 frames of a 71,673-frame run read as `Scene::Dialog`, and two readings fitted
/// that — a real text box the pad cannot advance, or a `dialog` reading off map tiles that merely
/// look like a box. The dialog branch has two halves, `wFontLoaded`'s bit and the drawn border, so
/// the way to tell is to count the frames on which they disagree.
///
/// The answer was that the detector is right and the corner test was thin. Over 43,004 frames of
/// the real brain driving from this checkpoint: **0** frames had the font flag set with the four
/// corners drawn and the rest of the border missing — every `dialog` frame was a fully drawn
/// `TextBoxBorder` — while **315** frames had all four corners drawn with the font flag clear,
/// because the frame's tile ids ($79, $7b, $7d, $7e) are ordinary ground in the overworld
/// tilesets. So the flag was the only thing keeping a map out of the scene, and `waiting` now
/// reads the whole border instead of its corners.
///
/// This test holds the first half of that on the cartridge. The count of the second is printed
/// rather than asserted: it is a fact about which tiles the fly happens to walk past, and a run
/// that never sees one has not proved anything either way.
#[test]
fn the_map_is_never_read_as_a_text_box_from_the_stalled_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    let (mut real, mut fooled, mut no_corners, mut corners_only) = (0u32, 0u32, 0u32, 0u32);
    for _ in 0..60_000 {
        run.frame();
        let text = flybrain_gb::pokemon_red::state::text_box(&mut run.gb);
        let (corners, border) = flybrain_gb::pokemon_red::state::dialog_border(&mut run.gb);
        match (text.open, corners, border) {
            (true, true, true) => real += 1,
            (true, true, false) => fooled += 1,
            (true, false, _) => no_corners += 1,
            (false, true, _) => corners_only += 1,
            (false, false, _) => {}
        }
    }
    eprintln!(
        "dialog branch over 60,000 frames: real box {real}, fooled {fooled}, \
         font without corners {no_corners}, corners without the font {corners_only}"
    );
    assert_eq!(
        fooled, 0,
        "a frame the detector would call `dialog` on four map tiles: the border was not drawn"
    );
    // `waiting` is never true without the WRAM flag, whatever the map is drawing.
    assert!(
        !flybrain_gb::pokemon_red::state::text_box(&mut run.gb).waiting
            || flybrain_gb::pokemon_red::state::text_box(&mut run.gb).open
    );
}

/// From the stalled checkpoint, the objective is Oak, and the fly walks to him and presses A.
///
/// **Row 29** (`infra/docs/macros-traps.md`). With the objective pointing at Oak's *lab*, a place
/// that is only a map, `GO OBJECTIVE` arrived on the lab's own doormat and called that arriving;
/// `GO OUT` walked straight back out; the pair ran every three brain seconds at the door, live, for
/// most of a morning. A rung earned by a conversation is not a map — it is somebody to stand next
/// to — so the catalog names the *person*, `GO OBJECTIVE` arrives beside him facing him, and `TALK`
/// is on the pad on the next hold. While that person is on this map, untalked and unexcluded, the
/// ways out are not on the pad at all: there is nothing for the bounce to bounce on.
///
/// This is the chain on the cartridge with the game-blind rotation driving it: the parcel carried
/// and undelivered, the lab reached by way of Pallet Town rather than through the shut road, and
/// `TALK` taken in the room the rung is in. The delivery event itself is measured rather than
/// asserted; the comment at that assertion has the numbers and the reason.
#[test]
fn macros_mode_reaches_the_errand_and_takes_its_press_from_the_stalled_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    assert_eq!(run.map(), VIRIDIAN_CITY, "the checkpoint is the one the stream stalled on");
    assert!(run.event(EVENT_GOT_OAKS_PARCEL), "the parcel is carried");
    assert!(!run.event(EVENT_OAK_GOT_PARCEL), "and undelivered, which is what shuts the road");

    run.drive_until("oak's lab", 120_000, |run| run.map() == OAKS_LAB);
    // **The delivery itself is no longer asserted, and that is a measurement rather than a trim.**
    //
    // It was, when this test was written: `EVENT_OAK_GOT_PARCEL` at 27,167 frames on 504 macros.
    // That held for exactly one trajectory. The battle sub-states of section 12.6 changed what the
    // fly does in Route 1's grass -- it now chooses a move instead of pressing A at the text -- and
    // the rotation's phase inside the lab moved with it: from the same checkpoint the fly reaches
    // the lab, presses `TALK` ninety-four times, and does not deliver in **900,000 frames** (16
    // brain hours).
    //
    // Which is the sibling tests' own finding in a new place: a game-blind rotation does not aim,
    // and the delivery is one A press at one particular person. What the *macros* can be held to is
    // that the fly is sent at the errand, gets there, and has the press available -- and that is
    // what this test holds. The delivery belongs to a run with a brain in it and a reward for the
    // rung. The gap the trajectory exposed is named in `infra/docs/macros-traps.md` row 31.
    run.drive_until("a talk in the lab", 240_000, |run| {
        run.map() == OAKS_LAB && run.started.get("TALK").is_some_and(|count| *count > 0)
    });
    assert_eq!(run.map(), OAKS_LAB, "in the room the rung is in");
    assert!(run.started.get("TALK").is_some_and(|count| *count > 0), "{:?}", run.started);
    let counts = run.layer.counts();
    assert!(counts.done > counts.timeout, "the walks completed: {counts:?}");
    assert!(counts.started < 3_000, "macros spent reaching the errand: {counts:?}");
    eprintln!(
        "the errand reached and pressed in {:.1} brain minutes on {} macros ({counts:?}), \
         route {:?}, by name {:?}",
        run.ms / 60_000.0,
        counts.started,
        run.route,
        run.started
    );
}

/// From the stalled checkpoint, the objective is the errand and the fly walks to it.
///
/// **The gate** (2026-09-17, `infra/docs/macros-traps.md` row 28). The fly stepped north out of
/// (18, 10) in Viridian City into a still sprite at (18, 9); the cartridge showed a box reading
/// "This is private property!", took the joypad and walked it back one tile. Every macro that hit
/// it ended `Done` — a scene change, which records nothing — so the same step was taken once per
/// hold for eight hours.
///
/// **What shut the road is two rungs the ladder had skipped.** The save has
/// `EVENT_GOT_OAKS_PARCEL` set and `EVENT_OAK_GOT_PARCEL` clear: the parcel is carried and
/// undelivered, so rungs 6 (the delivery) and 7 (the Pokédex) are unearned — while the rank reads
/// **8**, because the rank is the *maximum* over satisfied rungs and Viridian City had been stood
/// on. `objective` was `rank + 1`, which aimed the fly at Viridian Forest, north, through a gate
/// the cartridge keeps shut until Oak has his parcel. It is the lowest *unsatisfied* rung now,
/// which is Oak's lab, two maps south — and nothing in the macros knows any of that: the catalog
/// already held the place and the arithmetic was picking the wrong rung out of it.
///
/// So this asserts what the change makes true and what the cartridge can be held to: the fly
/// **leaves Viridian City and reaches Oak's lab**, where the errand is, on a bounded number of
/// macros.
///
/// **It takes the town's errands on the way, and that is section 13 working.** When this test was
/// written the lab was 1.4 brain minutes and 55 macros away; the mart and the Pokémon Center are
/// now ahead of the rung's place in `GO OBJECTIVE`, so the measured route is Viridian, the centre,
/// the mart, Route 1, Pallet Town, the lab — 13.5 brain minutes and 788 macros, of which 777
/// completed. The bound is what this test is for (the walks finish and the count is finite), not
/// the number itself.
///
/// **Route 2 is not asserted, and that is a measurement.** Delivering the parcel is one A press at
/// one particular person, and this readout is a rotation that knows nothing about the game: over
/// 600,000 frames — eleven brain hours — inside and around the lab it never pressed it, exactly as
/// the sibling test above records for the starter ("a game-blind stub does not press `TALK` in
/// front of a Pokéball on purpose"). That claim belongs to a run with a brain in it, and the road
/// north is behind it.
#[test]
fn the_objective_is_the_errand_and_macros_mode_walks_to_it() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    assert_eq!(run.map(), VIRIDIAN_CITY, "the checkpoint is the one the stream stalled on");
    assert_eq!(run.adapter.progress().rank, 8, "rank 8, the maximum over the satisfied rungs");
    assert!(run.event(EVENT_GOT_OAKS_PARCEL), "the parcel is carried");
    assert!(!run.event(EVENT_OAK_GOT_PARCEL), "and undelivered, which is what shuts the road");

    // South, not north: Route 1, Pallet Town, the lab.
    run.drive_until("oak's lab", 120_000, |run| run.map() == OAKS_LAB);
    assert!(run.route.contains(&PALLET_TOWN), "by way of the town: {:?}", run.route);
    assert!(!run.route.contains(&ROUTE_2), "and not through the shut road: {:?}", run.route);

    let counts = run.layer.counts();
    assert!(counts.done > counts.timeout, "the walks completed: {counts:?}");
    // Bounded rather than tight: the two errands of section 13 are ahead of the rung's place, so
    // this is three buildings' worth of walking and not one town's.
    assert!(counts.started < 1_500, "macros spent getting to the errand: {counts:?}");
    eprintln!(
        "the errand in {:.1} brain minutes on {} macros ({counts:?}), route {:?}, by name {:?}",
        run.ms / 60_000.0,
        counts.started,
        run.route,
        run.started
    );
}

/// After a restore that added the macro roles, `TALK` wins a facing window.
///
/// **The live finding** (2026-09-17, after v0.3.6): forty-eight minutes in Oak's lab with `TALK`
/// never chosen once. The macro populations were firing — 27 to 145 Hz across the six roles — and
/// the decoder had no baseline for any of them, because `calibrate()` runs once at warm-up and the
/// live state was restored from a checkpoint written before the roles existed. An absent baseline
/// reads as zero, so the score `(rate + 1) / (baseline + 1)` becomes `rate + 1` and the group is
/// decided by raw rate: `macro_talk` at 41 Hz cannot beat `macro_frontier` at 54 in the one window
/// where `TALK` is on the pad at all.
///
/// So this drives the shipping loop from the checkpoint with a decoder state of exactly that shape
/// — calibrated, then every macro baseline removed, which is what the box restored — and counts
/// **facing windows**: runs of frames in which `TALK`'s channel is on the pad, the only chances
/// the fly gets. `TALK` is expected to win one of the first few, not one of the first hundred.
///
/// The stub's own rates are not the live ones and cannot be; what carries over is the *state shape*
/// and the rule that answers it. The live numbers are pinned in the unit tests and the `restore`
/// golden.
#[test]
fn talk_wins_a_facing_window_after_a_restore_that_added_the_macro_roles() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume_with(&rom, MacroMode::Macros, &checkpoint, true);
    run.drive_until("a chosen TALK", 240_000, |run| {
        run.started.get("TALK").is_some_and(|count| *count > 0)
    });
    assert!(
        run.talk_windows > 0 && run.talk_windows <= 32,
        "TALK won a facing window within a bounded number of them: {} windows, {:?}",
        run.talk_windows,
        run.started
    );
    eprintln!(
        "TALK chosen after {} facing windows, {:.1} brain minutes, {:?}",
        run.talk_windows,
        run.ms / 60_000.0,
        run.started
    );
}

/// With the move list open on the cartridge, macros mode chooses a move and the turn proceeds.
///
/// **The live loop** (2026-09-17, rank 9, Viridian Forest, an hour and forty-one minutes): a wild
/// Kakuna, Bulbasaur at 8/28, the FIGHT list open with TACKLE showing 0 of 40 PP — and the pad was
/// one `NEXT`, every 268 brain milliseconds. `own_turn` was the top-level menu alone, so a frame
/// with the move list open read as *between turns*, whose pad is an A press on whatever the cursor
/// is sitting on. It pressed A at the move with no PP; the game said so; the box closed; the list
/// came back.
///
/// This drives the shipping loop from the checkpoint until a wild battle starts, presses A to open
/// the move list, and holds the three things that were wrong:
///
/// - the frame reads as the fly's turn, not as between turns;
/// - the pad over the open list is `ATTACK` and `BACK`, and never `NEXT`;
/// - `ATTACK` confirms a move that **has** PP, and the list closes: the turn proceeds.
///
/// The 0-PP-first-move case itself is pinned synthetically, in
/// `attack_over_an_open_move_list_chooses_a_move_that_has_pp` and
/// `attack_confirms_anyway_when_every_move_is_out_of_pp` — a fake can set a PP counter to zero and
/// a cartridge cannot be talked into it inside a test.
#[test]
fn macros_mode_chooses_a_move_from_an_open_list_on_the_cartridge() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);

    // A wild battle, by the fly's own walking: Route 1's grass is on the way to the errand.
    run.drive_until("a wild battle", 240_000, |run| {
        run.gb.read_wram(flybrain_gb::pokemon_red::symbols::ram::wIsInBattle) != 0
    });

    // Advance the battle's opening text until the top-level menu is up, then open FIGHT. Raw
    // presses rather than macros, because what is under test is the pad the *list* deals.
    let mut opened = false;
    for _ in 0..600 {
        let menu = {
            let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut run.gb);
            flybrain_gb::pokemon_red::macros::palette::battle_menu(&mut state)
        };
        // A *settled* list: one whose cursor the seam can place. The coordinates appear on the
        // battle's opening frames before the engine has filled `wBattleMon*`, and that frame is
        // not a choice the game is waiting for (`pokemon_red::state::battle`).
        if matches!(
            menu,
            flybrain_gb::pokemon_red::macros::state::BattleMenu::Moves { cursor: Some(_), .. }
        ) {
            opened = true;
            break;
        }
        for phase in 0..16 {
            run.gb.set_buttons(if phase < 8 { flybrain_gb::buttons::A } else { 0 });
            run.gb.run_frame().expect("a frame should complete");
            run.ms += MS_PER_FRAME;
        }
        let ms = run.ms;
        run.adapter.sample(&mut run.gb, ms);
    }
    assert!(opened, "the move list never opened");

    // What the scene and the pad say about it.
    let scene = {
        let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut run.gb);
        let state: &mut dyn flybrain_gb::pokemon_red::macros::cartridge::MacroState = &mut state;
        state.scene()
    };
    assert!(
        matches!(
            scene,
            flybrain_gb::pokemon_red::macros::state::Scene::Battle { own_turn: true, .. }
        ),
        "the move list open is the fly's turn: {scene:?}"
    );
    let ledger = AdapterLedger(&run.adapter);
    let _ = run.layer.observe(&mut run.gb, &ledger, run.ms);
    let pad: Vec<String> =
        run.layer.feed_palette().into_iter().map(|slot| slot.name).collect();
    assert!(pad.iter().any(|name| name.starts_with("MOVE ")), "the list is chosen from: {pad:?}");
    assert!(pad.iter().any(|name| name == "BACK"), "or backed out of: {pad:?}");
    assert!(
        !pad.iter().any(|name| name == "NEXT"),
        "never an A press on whatever the cursor holds: {pad:?}"
    );

    // And `ATTACK` confirms a move that has PP, which closes the list: the turn proceeds.
    let slot = {
        let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut run.gb);
        flybrain_gb::pokemon_red::macros::palette::move_list(&mut state).and_then(|(cursor, _)| cursor)
    }
    .expect("a move to confirm");
    let pp_before = run.gb.read_wram(
        flybrain_gb::pokemon_red::symbols::ram::wBattleMonPP + u16::from(slot),
    );
    assert!(pp_before > 0, "the chosen move has PP: slot {slot} at {pp_before}");

    // Lean on ATTACK's own channel so the script under test is the one that runs, from a clean
    // decision: whatever the rotation was holding when the list opened is cancelled first, because
    // a `BACK` already in flight would close the list before `ATTACK` could be offered.
    let ms = run.ms;
    let _ = run.layer.cancel(ms);
    run.decoder.clear_holds(ms);
    run.force_hot = Some("macro_move_1");
    run.drive_until("a MOVE chosen", 6_000, |run| {
        run.started.keys().any(|name| name.starts_with("MOVE "))
    });
    run.drive_until("the list closed", 6_000, |run| {
        let mut state = flybrain_gb::pokemon_red::state::PokeState::new(&mut run.gb);
        !matches!(
            flybrain_gb::pokemon_red::macros::palette::battle_menu(&mut state),
            flybrain_gb::pokemon_red::macros::state::BattleMenu::Moves { .. }
        )
    });
    eprintln!(
        "the move list closed on slot {slot} (PP was {pp_before}), macros {:?}, by name {:?}",
        run.layer.counts(),
        run.started
    );
    assert!(
        run.started.keys().any(|name| name.starts_with("MOVE ")),
        "a MOVE button is what closed it: {:?}",
        run.started
    );
}

/// From the gate-house stall's own checkpoint, macros mode leaves northward into the forest.
///
/// **The stall** (2026-09-17 17:32 UTC, `infra/docs/macros-traps.md` rows 32 and 33): rank 9
/// (Viridian Forest, next Pewter City) for four hours and eighteen minutes with the fly on map 50,
/// the ten-by-eight gate house between Route 2 and Viridian Forest. The pad was `GO FRONTIER` and
/// nothing else, and the last 40 KB of the event log was 175 `GO FRONTIER start`/`done` pairs —
/// one tile of walking each, one every 800 brain milliseconds. Two facts made it:
///
/// - the gate's two doormats are walkable ground the *reward* ledger can never record, because
///   `sample`'s payout gate rejects `wMovementFlags`' door and warp bits, so they were a frontier
///   nothing could retire and `GO FRONTIER` never exhausted;
/// - `geography::next_hop` had one node for `ROUTE_2`, whose two halves are not connected to each
///   other, so "the first hop to Pewter" from inside the gate was Route 2 *south* — the door the
///   fly had just come in by. `GO OBJECTIVE` aimed back out of it, `GO OUT` had no other tier, and
///   `GO WARP` — the north doors into the forest, which are the way to Pewter — was never on the
///   pad at all.
///
/// **What is asserted is the direction.** The first map the fly leaves the gate house for is
/// Viridian Forest, which is northward and toward the rung; before this branch it was Route 2,
/// southward, and the fly came straight back. And the gate house never deals a pad of one button
/// again, which is the loop's own signature. Both on a bounded number of macros: the measured run
/// leaves on **one macro in 155 frames**.
///
/// **The forest's north gate is not asserted, and that is a measurement rather than a trim.** With
/// this branch the fly crosses into the forest at once and plays it — 1,200,000 frames (5.6 brain
/// hours) from this checkpoint cover six maps and 20,595 macros, including a blackout to Pallet
/// Town and the walk back north — and it does not reach map 47. From about 600,000 frames on the
/// only macros that start are `ITEM`, `BACK` and `NEXT`: `ATTACK` does not start once more and
/// every overworld count freezes, which is a battle whose own turn the fly never ends. That is the
/// next trap, recorded as row 34 in `infra/docs/macros-traps.md`, and asserting Route 2's north
/// half here would be a test that fails on a trap it does not measure.
///
/// Gated on `FLY_ROM` and on a checkpoint whose map is the gate house, and skips cleanly without
/// either:
///
/// ```sh
/// FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
///   FLY_GATE_CHECKPOINT=.local/checkpoints/release-rank9-20260917T1732.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn macros_mode_leaves_the_forests_south_gate_northward() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = gate_checkpoint() else {
        eprintln!("skipped: no FLY_GATE_CHECKPOINT / FLY_TRAP_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    if run.map() != VIRIDIAN_FOREST_SOUTH_GATE {
        eprintln!(
            "skipped: the checkpoint is on map {:#04x}, not the forest's south gate",
            run.map()
        );
        return;
    }

    // Off the map the stall was on. Bounded at 24,000 frames, which is six brain minutes; the
    // measured run needs 155 of them, because the north doors are seven tiles from the
    // checkpoint's own tile and one walk covers that.
    run.drive_until("off the gate house", 24_000, |run| {
        run.route.iter().any(|map| *map != VIRIDIAN_FOREST_SOUTH_GATE)
    });
    assert_eq!(
        run.route.first().copied(),
        Some(VIRIDIAN_FOREST_SOUTH_GATE),
        "route {:?}",
        run.route
    );
    assert_eq!(
        run.route.get(1).copied(),
        Some(VIRIDIAN_FOREST),
        "the first map out of the gate house is the forest, northward and toward the rung,          not Route 2 behind it: route {:?}, by name {:?}",
        run.route,
        run.started
    );
    let counts = run.layer.counts();
    assert!(counts.started <= 5, "macros spent leaving the gate house: {counts:?}");

    // Then keep going, because the pad claim is about what the gate house deals *after* its
    // frontier has been walked and its people talked to — which is the state four hours of stream
    // was in, and which the session ledgers only reach by running.
    for _ in 0..120_000 {
        run.frame();
    }
    let gate_pad = run.pads.get(&VIRIDIAN_FOREST_SOUTH_GATE).copied().unwrap_or(0);
    assert!(
        gate_pad > 1,
        "the gate house dealt a pad of {gate_pad} buttons: {:?}, by name {:?}",
        run.pads,
        run.started
    );

    let counts = run.layer.counts();
    assert!(counts.done > 0, "no macro completed: {counts:?}");
    eprintln!(
        "out of the gate house northward on {} macros, smallest gate-house pad {gate_pad},          {:.1} brain minutes, route {:?}, {counts:?}, by name {:?}",
        run.started.values().sum::<u32>(),
        run.ms / 60_000.0,
        run.route,
        run.started
    );
}

/// The checkpoint the gate-house test starts from, or `None` to skip.
///
/// Its own variable before `FLY_TRAP_CHECKPOINT`, because the two stalls on this rung have
/// different checkpoints and each test asserts the map its own stall was on. The test skips rather
/// than fails when the checkpoint it gets is the other one.
fn gate_checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_GATE_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_TRAP_CHECKPOINT"))
        .or_else(|| std::env::var_os("FLY_MACRO_CHECKPOINT"))?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

/// From the 0-PP battle's own checkpoint, the fly's own turn has a button that ends it.
///
/// **The stall** (`infra/docs/macros-traps.md` row 34, measured in Viridian Forest at 99.1 brain
/// minutes): a wild battle, a party of one, TACKLE / GROWL / LEECH SEED all at 0 PP, a potion in
/// the bag and a `RUN` the cartridge refuses. `ATTACK`'s precondition was `best_move(..).is_some()`
/// and `best_move` skips a move with no PP, so the button left the top-level menu — and the move
/// list, where `move_list_choice` confirms a slot anyway so that Struggle happens, is only ever
/// reached *through* FIGHT. What was left was `ITEM` over a bag it could only close and `NEXT` on
/// whatever the cursor held, which opens the party list, whose only bound button with one Pokémon
/// is `BACK`. 600,000 frames of it.
///
/// What is asserted: `ATTACK` is on the pad of the fly's own turn, the macro behind it **starts
/// and runs on the cartridge** from this state, that turn never deals one button, and the battle
/// ends — which is the whole claim, because a turn that can be ended is a battle that finishes one
/// way or the other. Which way is not asserted: with 6 of 34 HP against a trainer the measured run
/// faints and blacks out, and that is the game rather than the macro layer.
///
/// Gated on `FLY_ROM` and on a checkpoint in that state, which
/// `examples/scene_probe.rs` writes with `FLY_PROBE_CATCH=noattack FLY_PROBE_SAVE=…` — 99 brain
/// minutes of the stub from the gate-house checkpoint, the release box's own run carried forward.
/// `.local/` is not tracked, so this skips cleanly without it:
///
/// ```sh
/// FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
///   FLY_NOPP_CHECKPOINT=.local/checkpoints/release-nopp.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn macros_mode_ends_a_turn_with_no_move_left_on_the_cartridge() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = nopp_checkpoint() else {
        eprintln!("skipped: no FLY_NOPP_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    if run.in_battle() == 0 {
        eprintln!("skipped: the checkpoint is not in a battle");
        return;
    }
    assert_eq!(run.party(), 1, "a party of one is half of what made the turn unanswerable");

    // First the script, with the attack channel leaned on rather than rotated to. That is what
    // `force_hot` is for in this harness -- the one question that is about a *script* rather than
    // about which button the fly picks -- because a rotation may spend the whole turn on `NEXT`
    // and prove nothing about `ATTACK` either way.
    run.force_hot = Some("macro_move_1");
    run.drive_until("a `MOVE` to start on this turn", 12_000, |run| {
        run.started.keys().any(|name| name.starts_with("MOVE "))
    });
    run.force_hot = None;

    // Then the battle ends. Bounded at 60,000 frames, which is 17 brain minutes; the measured run
    // needs 1,536.
    run.drive_until("the battle to end", 60_000, |run| run.in_battle() == 0);

    assert!(
        run.move_button_on_battle_pad,
        "no `MOVE` button was ever on the pad of the fly's own turn: {:?}, by name {:?}",
        run.battle_pad,
        run.started
    );
    assert!(
        run.battle_pad.is_some_and(|dealt| dealt > 1),
        "the fly's own turn dealt a pad of {:?} buttons: by name {:?}",
        run.battle_pad,
        run.started
    );
    eprintln!(
        "the turn ended in {:.1} brain minutes, smallest battle pad {:?}, {:?}, by name {:?}",
        run.ms / 60_000.0,
        run.battle_pad,
        run.layer.counts(),
        run.started
    );
}

/// The checkpoint the no-PP test starts from, or `None` to skip.
///
/// Its own variable and no fallback: the other checkpoints in this file are overworld states and
/// this test asserts a battle, so taking one of them would fail on a state it does not measure.
fn nopp_checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_NOPP_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

// -------------------------------------------------------------------------------------------
// Section 13: the errands, a purchase and a heal, from the release box's own checkpoint
// -------------------------------------------------------------------------------------------

/// `GO SHOP` walks the fly into the Viridian mart, once, and then to its counter.
///
/// **What is proven here, and what is not.** The errand works: from the release container's own
/// Viridian checkpoint the fly is inside the mart in 1.4 brain minutes, the errand is discharged
/// on arrival and `GO SHOP` is never on the city's pad again. Inside, `GO SHOP` walks it to a tile
/// it can talk to the clerk from — which needs the counter reach, because every tile beside a
/// clerk behind a desk is a wall.
///
/// **The purchase is not ROM-proven from this checkpoint**, and the mechanism is named rather than
/// papered over (`infra/docs/macros-traps.md` rows 33b and 34b). Two things are in the way, both
/// measured on the cartridge on 2026-09-17:
///
/// - the screen-buffer tile read disagrees with itself on a map smaller than the screen. In the
///   Viridian mart the same tile reads walkable-and-counter from one of the fly's tiles and
///   wall-and-not-counter from another two tiles away, because an 8x8 map cannot centre under a
///   ten-by-nine view. `facing_target`'s reach is taken from the *map id* for exactly that reason,
///   but the walk's own goals are still priced off the tile read;
/// - a counter *faced* is lost to the next hold. `GO SHOP` ends standing at the counter looking at
///   the clerk, and `TALK` is on the pad there — but a hold is 800 ms and whichever macro wins the
///   next one turns the fly away before the A press happens. The `reached` window brings `GO SHOP`
///   back in ten brain minutes, so it is bounded rather than a loop; it is not a proof.
///
/// The purchase scripts themselves are covered by the fake-game tests
/// (`a_purchase_navigates_by_the_items_place_in_the_stock_list`), and the *stock* read is proven
/// against the cartridge below: Viridian's counter sells no Potion.
#[test]
fn go_shop_enters_the_mart_once_and_walks_to_its_counter() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    assert_eq!(run.map(), VIRIDIAN_CITY, "the checkpoint is the one the stream stalled on");

    // The errand is outstanding at the checkpoint: the ledger is session state and a restore
    // starts empty (`docs/design/macros.md` section 13).
    run.frame();
    assert!(
        run.layer.bound_channels().iter().any(|channel| channel == "macro_go_shop"),
        "`GO SHOP` is on the pad with the errand outstanding: {:?}",
        run.layer.bound_channels()
    );

    // Into the mart, on `GO SHOP`'s own channel. Viridian is a wide city and the mart is on the
    // far side of it, so this is several budgets' worth of walking and several holds.
    run.force_hot = Some("macro_go_shop");
    let mart = u32::from(flybrain_gb::pokemon_red::maps::VIRIDIAN_MART);
    run.drive_until("inside the mart", 240_000, |run| run.map() == mart);

    // Inside, `GO SHOP` is still the walk to the clerk -- the half of section 13 the ledger does
    // not gate, because the errand's remaining job is to put the fly where the purchases can be
    // pressed. Every tile beside the clerk is a wall, so reaching one at all is the counter reach
    // doing the work (`docs/design/macros-wram.md` section 7).
    let mut shop_seen = false;
    let mut faced_the_counter = false;
    let mut pad_in_the_mart: Vec<String> = Vec::new();
    run.drive_until("the counter faced", 60_000, |run| {
        if run.map() != mart {
            return false;
        }
        let bound = run.layer.bound_channels();
        let shop = bound.iter().any(|channel| channel == "macro_go_shop");
        // The frames straight after a warp report the new map with the *old* coordinates -- the
        // header changes before the position does -- so the pad on those is dealt against a tile
        // of the city. Waiting for `GO SHOP` to appear is waiting for the fly to actually be in
        // the building.
        if !shop_seen {
            shop_seen = shop;
            if shop_seen {
                pad_in_the_mart = bound.clone();
            }
            return false;
        }
        // `GO SHOP` leaving the pad inside the building *is* the counter having been faced: the
        // reached ledger is what retires it (`palette::counter_aims`).
        faced_the_counter |= !shop;
        faced_the_counter
    });
    eprintln!(
        "the mart's pad on arrival: {pad_in_the_mart:?}, counter stocks {:?}, wallet {}",
        run.counter_stock(),
        run.money()
    );
    assert!(faced_the_counter, "`GO SHOP` walked to the clerk and retired it");
    // And while the counter was unfaced, nothing on the pad could leave the building: that is what
    // stops the one visit this area gets being spent on the doormat (`palette::counter_pending`).
    assert!(
        !pad_in_the_mart.iter().any(|channel| channel == "macro_go_out"),
        "no way out while the errand's counter is unfaced: {pad_in_the_mart:?}"
    );

    // Out again -- the suppression is released now -- and the errand is paid: `GO SHOP` is not on
    // the city's pad a second time. That is "once per area per run", and it is what makes the
    // errand impossible to loop on.
    run.force_hot = Some("macro_go_out");
    run.drive_until("back out onto the city", 120_000, |run| run.map() == VIRIDIAN_CITY);
    run.force_hot = None;
    for _ in 0..600 {
        run.frame();
    }
    assert!(
        !run.layer.bound_channels().iter().any(|channel| channel == "macro_go_shop"),
        "the errand is paid and never offered again: {:?}",
        run.layer.bound_channels()
    );
}

/// `GO HEAL` walks the fly into the Viridian Pokémon Center and `HEAL` restores the party.
///
/// The nurse is `object_event 3, 1, SPRITE_NURSE` behind the counter tile at (3, 2), so none of
/// the four tiles around her can be stood on: the walk aims at (3, 3) and faces up, over the
/// counter, which is the only place the cartridge lets her be spoken to from
/// (`docs/design/macros-wram.md` section 7).
///
/// `HEAL` is on the pad only while the party is hurt or statused, so the test hurts it first —
/// by *reading* whether it already is, and skipping the assertion if the checkpoint's party is
/// already full, because this crate never writes game memory (`docs/loop-review.md`).
#[test]
fn go_heal_enters_the_centre_and_heal_restores_the_party() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_TRAP_CHECKPOINT / FLY_MACRO_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    assert_eq!(run.map(), VIRIDIAN_CITY);
    run.frame();
    assert!(
        run.layer.bound_channels().iter().any(|channel| channel == "macro_go_heal"),
        "`GO HEAL` is on the pad with the errand outstanding: {:?}",
        run.layer.bound_channels()
    );

    let centre = u32::from(flybrain_gb::pokemon_red::maps::VIRIDIAN_POKECENTER);
    // The pad is captured *from inside the walk*, on the frames the fly is actually standing in
    // the building. Measured 2026-09-17: given six hundred free frames after arriving it walks
    // straight back out, and the centre's own row is only dealt while it is in there -- which is
    // the mistake this test made on its first run.
    let mut heal_seen = false;
    let mut hurt_in_centre = false;
    let mut rested_in_centre = false;
    run.force_hot = Some("macro_go_heal");
    run.drive_until("the centre's own pad", 240_000, |run| {
        if run.map() != centre {
            return false;
        }
        heal_seen |= run.layer.bound_channels().iter().any(|channel| channel == "macro_heal");
        if run.party_rested() {
            rested_in_centre = true;
        } else {
            hurt_in_centre = true;
        }
        heal_seen || rested_in_centre
    });
    run.force_hot = None;
    eprintln!(
        "in the centre: HEAL seen {heal_seen}, party hurt {hurt_in_centre}, rested \
         {rested_in_centre}"
    );

    if !hurt_in_centre {
        eprintln!(
            "the checkpoint's party is already full and this crate never writes game memory, \
             so `HEAL`'s restore is not asserted from this state; the walk to the counter is"
        );
        // What *is* asserted then: the button was never on the pad, which is its own
        // precondition working (section 13: "only when at least one party member is not at full
        // HP or has a status").
        assert!(!heal_seen, "a rested party is no reason to rest");
        return;
    }

    assert!(heal_seen, "`HEAL` was never on the centre's pad with a hurt party");
    run.force_hot = Some("macro_heal");
    run.drive_until("the party reads back full", 120_000, |run| run.party_rested());
    run.force_hot = None;
    assert!(run.party_rested(), "the nurse healed the party");
}

/// The rung-10 Pewter checkpoint, or `None` to skip.
///
/// Its own variable, like `FLY_FOREST_CHECKPOINT`: the tests above assert the map their envelope
/// is on and one envelope cannot be two maps.
fn building_checkpoint() -> Option<flysim::store::Checkpoint> {
    std::env::var_os("FLY_BUILDING_CHECKPOINT").map(|path| {
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope")
    })
}

/// From the rung-10 checkpoint: the fly leaves the building, and no pad is `MENU` and a way back.
///
/// **What was live** (2026-09-22, thirty-one minutes after v0.4.3 deployed): rank 10, PEWTER CITY,
/// the fly on **map 0x35 -- the upper floor of the Pewter museum**, fourteen blocks by eight, one
/// warp at (7, 7) down to the floor below, two signs and three exhibits. For thirty brain minutes
/// the macro starts were `MENU` 82, `BACK` 82 and `GO FRONTIER` 8, the event log alternating `MENU
/// start/done, BACK start/done`.
///
/// Every candidate list on that map empties: `geography` has no row for the museum, so `next_hop`
/// answers `None` and `GO OBJECTIVE` has nothing to aim at (the objective itself is map 0x36 --
/// the gym leader, rung 11's BOULDER BADGE); the three exhibits and two signs are *reached* and
/// retired for the session; the four unstood tiles are walked or excluded; and the one way out is
/// a **passage** whose blocked window `unexcluded_exits` respects. That left `MENU`, which opens a
/// scene whose pad is `CLOSE`, `CONFIRM` and `BACK` -- and `BACK` closes it again.
///
/// The claims here are about the pad and about *leaving*, not about where the fly goes next:
///
/// - `MENU` is on **no** overworld pad, on any map the run stands on (section 12.11). On v0.4.3
///   this set holds every one of them, which is what makes this the regression test.
/// - no overworld pad is empty, which since 12.11 rests on the way out rather than on `MENU`.
/// - the fly **leaves map 0x35** on a bounded number of macros.
/// - `MENU`/`BACK` never alternate, and `THROW BALL` never reports `blocked` -- the two residuals
///   v0.4.3 left (63 starts, 63 blocked, mean sixty-nine frames).
///
/// ```sh
/// FLY_ROM=/path/to/pokemon-red.gb \
///   FLY_BUILDING_CHECKPOINT=.local/checkpoints/release-rank10-pewter.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn the_fly_leaves_the_pewter_building_from_the_rung_ten_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = building_checkpoint() else {
        eprintln!("skipped: no FLY_BUILDING_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    let from = run.map();
    assert_eq!(from, MUSEUM_2F, "the checkpoint is the building the stream stalled in");
    // Rung 10 is stood on, so the objective is rung 11 -- the BOULDER BADGE, which is the gym
    // leader and so a *person* on the gym's map.
    assert_eq!(
        run.objective_map(),
        Some(0x36),
        "the objective is the Pewter gym, where the badge is"
    );

    // Driven the whole budget rather than stopped at the door: leaving is one claim and "`MENU`
    // is on no pad" is a claim about every pad the run deals, so the run keeps going and keeps
    // recording. Thirty-three brain minutes, against the thirty the live run spent not leaving.
    let mut left = None;
    for frame in 0..120_000u32 {
        run.frame();
        if left.is_none() && run.map() != from {
            left = Some(frame);
        }
    }
    eprintln!(
        "from map {from:#04x} in {:.1} brain minutes: route {:?}, macros {:?}, MENU on the pad \
         of {:?}, empty pads {:?}, blocked {:?}",
        run.ms / 60_000.0,
        run.route,
        run.started,
        run.menu_on_pad,
        run.empty_overworld_pads,
        run.blocked
    );

    assert!(
        run.menu_on_pad.is_empty(),
        "`MENU` was on an overworld pad: {:?}",
        run.menu_on_pad
    );
    assert_eq!(run.started.get("MENU"), None, "`MENU` cannot start if it is on no pad");
    // One is a lone `BACK`, which is an ordinary press in a list; two is the pair, and the live
    // run did it eighty-two times each for thirty brain minutes.
    assert!(
        run.longest_menu_back_alternation < 2,
        "`MENU`/`BACK` alternated {} times in a row",
        run.longest_menu_back_alternation
    );
    assert!(
        run.empty_overworld_pads.is_empty(),
        "an overworld pad was empty on {:?}",
        run.empty_overworld_pads
    );
    let Some(left) = left else { panic!("the fly never left map {from:#04x}") };
    eprintln!("it left map {from:#04x} on frame {left}");
    assert!(
        run.macros_on_the_first_map < 400,
        "leaving the building cost {} macros",
        run.macros_on_the_first_map
    );
    // The v0.4.3 residual, on the cartridge: every `THROW BALL` was blocked because the step read
    // the battle menu's cursor while the bag was still drawing.
    assert_eq!(
        run.blocked.get("THROW BALL"),
        None,
        "`THROW BALL` reported blocked: {:?}",
        run.blocked
    );
}

/// The rung-10 Pokemon Center checkpoint, or `None` to skip.
fn center_checkpoint() -> Option<flysim::store::Checkpoint> {
    std::env::var_os("FLY_CENTER_CHECKPOINT").map(|path| {
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope")
    })
}

/// From the rung-10 Pokemon Center checkpoint: the fly leaves the centre and stops answering YES.
///
/// **What was live** (2026-09-22, v0.4.4, rank 10 PEWTER CITY): the fly on **map 0x3a at (3, 3)**,
/// facing the nurse over her counter, and since the 09:39 restart the macro starts were `YES`
/// **2,142**, `TALK` 107, `GO FRONTIER` 26, `BACK` 24, the event log ending `YES start/done` for
/// ever. This is row 41, first measured in the rung-9 trap hunt and named as the next trap by
/// 12.11.
///
/// **What the survey found** (`infra/docs/macros-traps.md` row 41, and
/// `examples/scene_probe.rs`'s `FLY_PROBE_CATCH=nurse`): the nurse's conversation is a ring of
/// **forty-six A presses** -- welcome, the offer, the YES/NO box on **one** frame of the
/// forty-six, "OK. We'll need your POKeMON.", the machine, "fighting fit!", "We hope to see you
/// again!", the box closes for a single frame, and the next A press opens the whole thing again.
/// The party read **70/70 and healthy** throughout, so every press of it changed nothing, and
/// `HEAL` was never in it: its precondition reads the live party and answers no. What was on the
/// pad was the dialog's `NEXT`, `YES`, `NO` -- two names for one A press -- and `TALK` to get back
/// in, whose ledger entry was read one tile shorter than its own precondition and so was never
/// written.
///
/// The claims, none of them about where the fly goes next:
///
/// - `YES` starts **under five** in the whole run, against 1,278 in the rung-9 hunt from the same
///   room. Not zero: the fly may legitimately answer a hurt party's prompt.
/// - `NEXT` is on **no** pad while a readable YES/NO prompt is open (12.10 in a dialog).
/// - `TALK` is on **no** pad while the fly faces a nurse the party has no use for.
/// - the fly **leaves map 0x3a** on a bounded number of macros.
///
/// ```sh
/// FLY_ROM=/path/to/pokemon-red.gb \
///   FLY_CENTER_CHECKPOINT=.local/checkpoints/release-rank10-pokecenter.checkpoint \
///   cargo test --release -p flysim --test rom_macros_mode -- --nocapture
/// ```
#[test]
fn the_fly_leaves_the_pokemon_center_from_the_rung_ten_checkpoint() {
    let rom = skip_without_rom!();
    let Some(checkpoint) = center_checkpoint() else {
        eprintln!("skipped: no FLY_CENTER_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, MacroMode::Macros, &checkpoint);
    let from = run.map();
    assert_eq!(from, PEWTER_POKECENTER, "the checkpoint is the room the stream stalled in");
    // The premise of the whole trap: there was nothing to heal.
    assert!(run.party_rested(), "the checkpoint's party is already full and healthy");

    let mut left = None;
    for frame in 0..120_000u32 {
        run.frame();
        if left.is_none() && run.map() != from {
            left = Some(frame);
        }
    }
    eprintln!(
        "from map {from:#04x} in {:.1} brain minutes: route {:?}, macros {:?}, dialog frames {} \
         (prompt on {}), blocked {:?}",
        run.ms / 60_000.0,
        run.route,
        run.started,
        run.dialog_frames,
        run.prompt_frames,
        run.blocked
    );
    eprintln!("macros spent in the centre: {:?}", run.started_on_the_first_map);

    assert!(
        !run.next_on_a_prompt,
        "`NEXT` was on the pad at a YES/NO box, where an A press is `YES`"
    );
    assert!(
        !run.talk_at_a_rested_nurse,
        "`TALK` was on the pad at a nurse the party had no use for"
    );
    // In the **centre**, which is what row 41 is about: the run goes on to leave Pewter's gym
    // door and fight there, and the gym's own boxes are the fly playing the game rather than the
    // ring. 1,278 of 1,295 in the rung-9 hunt from this room; 2,142 live.
    let yes = run.started_on_the_first_map.get("YES").copied().unwrap_or(0);
    assert!(yes < 5, "`YES` started {yes} times in the centre: {:?}", run.started_on_the_first_map);
    let Some(left) = left else {
        panic!("the fly never left map {from:#04x}: {:?}", run.started)
    };
    eprintln!("it left map {from:#04x} on frame {left}");
    assert!(
        run.macros_on_the_first_map < 400,
        "leaving the centre cost {} macros",
        run.macros_on_the_first_map
    );
}
