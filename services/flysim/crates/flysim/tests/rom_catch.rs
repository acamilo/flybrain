//! The catch reward against the real cartridge.
//!
//! Gated on `FLY_ROM` *and* on a checkpoint, the way every ROM test in this workspace is, and
//! skips cleanly without either — the cartridge never enters this repository and a checkpoint is
//! not a fixture, it is the state the release box was really in:
//!
//! ```sh
//! FLY_ROM="$HOME/roms/pokemon-red.gb" \
//!   FLY_CATCH_CHECKPOINT=.local/checkpoints/<a rung-9 forest checkpoint> \
//!   cargo test --release -p flysim --test rom_catch -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! The synthetic trace in `pokemon_red/tests.rs` writes `wCapturedMonSpecies`, `wBattleResult`
//! and the Pokédex bit itself, from the disassembly. It cannot say that those are the bytes
//! *this* cartridge writes when a ball keeps a Pokémon, in that order, on frames an adapter
//! sampling once a frame actually sees. That is this test, and it is the "survey" half of
//! `docs/design/macros-wram.md`'s evidence for the row: a real battle, real button presses, and
//! the byte read out of the running game rather than written into a fake one.
//!
//! ## How the catch is produced
//!
//! No steering and no scripted button sequence: the shipping macro palette, the shipping macro
//! layer and the shipping decoder, with a stub readout that leans on one macro population at a
//! time — the same driver `tests/rom_macros_mode.rs` uses and for the same reason. The one thing
//! this harness does that the rotation does not is lean on `THROW BALL`'s channel while a wild
//! battle is up, because the question here is what the adapter reads from a catch, not whether a
//! game-blind readout finds its way to one.
//!
//! The checkpoint must hold at least one ball in the bag. The macro palette can buy one
//! (`BUY BALL`, `MB·PBALL`, inside a mart), but that is a walk across a city and back and it is a
//! different test's question; this one says out loud that it skipped.

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::RewardEvent;
use flybrain_gb::pokemon_red::state;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, catalog};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter,
};
use flysim::config::Config;
use flysim::frame::LegacyFrame;
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const SEED: u32 = 20_260_922;
/// The hot population's rate against every other one's, which is also the stub's calibration
/// rate — so a channel that is not the hot one scores exactly 1.0.
const HOT: f64 = 16.0;
const REST: f64 = 10.0;
/// `THROW BALL`'s channel (`pokemon_red::macros::palette`).
///
/// Row 66's note: this is the button's *tag*; its channel is `macro_throw_ball`
/// ([`THROW_BALL_CHANNEL`]), so the lean below sets a rate the decoder does not read and the drive
/// in a wild battle is the calibrated tie. It is kept as it is because that drive is the one that
/// catches from the forest checkpoint (with the real channel hot the checkpoint's one ball misses
/// on this frame timing), and the row 66 test reuses it up to the catch.
const BALL: &str = "MB·BALL";
/// `THROW BALL`'s channel as the macro layer binds it.
const THROW_BALL_CHANNEL: &str = "macro_throw_ball";
/// Frames the stub leans on one channel before the rotation moves on, the shape of the real
/// group's hysteresis-then-fatigue rotation.
const BURST_FRAMES: u32 = 24;

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

fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_CATCH_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

struct Run {
    gb: Emulator,
    adapter: PokemonRedReward,
    layer: MacroLayer,
    /// The stream's frame (`flysim::frame::LegacyFrame`), behind the stub readout.
    legacy: LegacyFrame,
    decoder: PopulationDecoder,
    channels: Vec<&'static str>,
    ms: f64,
    frame: u32,
    /// Every payout the adapter has made since the run started.
    payouts: Vec<RewardEvent>,
}

impl Run {
    fn resume(rom: &[u8], checkpoint: &flysim::store::Checkpoint) -> Self {
        let mut gb = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the cartridge");
        let mut adapter = PokemonRedReward::new();
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
        // A checkpoint written by an earlier adapter rebaselines rather than failing, which is
        // exactly the `v5` -> `v6` case this rule ships with.
        adapter.import_state(&checkpoint.runtime.reward).expect("the checkpoint's reward ledger");
        let channels = flybrain_gb::macro_channels("pokemon-red");
        let preset = gameboy_decoder_config_with_macros(&channels);
        let hold_ms = preset.macros.as_ref().expect("the preset has a macro group").hold_ms;
        let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
        decoder.calibrate(&rates(None));
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_string();
        config.macros.mode = MacroMode::Macros;
        config.validate().expect("pokemon-red has a palette in macros mode");
        let mut layer = macro_layer(&config, hold_ms, SEED).expect("a layer in macros mode");
        let _ = layer.observe(&mut gb, &AdapterLedger(&adapter), 0.0);
        Self {
            gb,
            adapter,
            layer,
            legacy: LegacyFrame::new(),
            decoder,
            channels,
            ms: 0.0,
            frame: 0,
            payouts: Vec::new(),
        }
    }

    fn byte(&mut self, address: u16) -> u8 {
        self.gb.read_wram(address)
    }

    fn in_wild_battle(&mut self) -> bool {
        self.byte(ram::wIsInBattle) == 1
    }

    /// Balls in the bag, of any kind (`constants/item_constants.asm`: MASTER_BALL 1,
    /// ULTRA_BALL 2, GREAT_BALL 3, POKE_BALL 4 — the same four `THROW BALL` looks for).
    fn balls(&mut self) -> usize {
        state::bag(&mut self.gb)
            .iter()
            .filter(|item| (0x01..=0x04).contains(&item.id) && item.count > 0)
            .map(|item| usize::from(item.count))
            .sum()
    }

    fn step(&mut self) {
        // Lean on `THROW BALL` while a wild battle is up; otherwise rotate, which is what gets
        // the fly into the grass in the first place.
        let hot = if self.in_wild_battle() {
            Some(BALL)
        } else {
            let slot = (self.frame / BURST_FRAMES) as usize % self.channels.len();
            Some(self.channels[slot])
        };
        let bound = self.layer.bound_channels();
        let active = self.decoder.decode_bound(&rates(hot), self.ms, false, None, Some(&bound));
        self.legacy.execute(Some(&mut self.layer), &active, 0, self.ms, &mut self.gb, &self.adapter);
        self.ms += MS_PER_FRAME;
        self.frame += 1;
        let evaluated = self
            .legacy
            .stub_advance(Some(&mut self.layer), &mut self.gb, &mut self.adapter, self.ms)
            .expect("a frame should complete");
        self.payouts.extend(evaluated.rewards);
    }

    fn catches(&self) -> Vec<&RewardEvent> {
        self.payouts.iter().filter(|event| event.kind == catalog::kind::CATCH).collect()
    }
}

#[test]
fn a_catch_on_the_cartridge_pays_the_catch_rule_once_with_the_species_in_its_label() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_CATCH_CHECKPOINT");
        return;
    };
    let balls = Run::resume(&rom, &checkpoint).balls();
    if balls == 0 {
        let run = Run::resume(&rom, &checkpoint);
        eprintln!(
            "skipped: the checkpoint's bag holds no ball (map {:#04x}). `BUY BALL` can buy one \
             inside a mart; point FLY_CATCH_CHECKPOINT at a state that already has one.",
            run.adapter.map_id().unwrap_or(u32::MAX)
        );
        return;
    }
    eprintln!("bag holds {balls} balls");

    // Five brain minutes per attempt is generous for a forest checkpoint: the live run threw 28
    // balls in its first Viridian Forest session (`pokemon_red::macros::palette`). An attempt
    // starts after `5 * attempt` frames with nothing pressed, so each one meets the wild battle,
    // and the ball meets the random number, on a different frame (row 66): one ball misses about
    // two throws in three, and which throw a checkpoint's one ball is depends on frame timing
    // that any change to the pad moves.
    let budget = 5 * 60 * 60;
    let mut battles;
    let mut attempt = 0u32;
    let mut run = loop {
        let mut run = Run::resume(&rom, &checkpoint);
        for _ in 0..attempt * 5 {
            run.ms += MS_PER_FRAME;
            run.frame += 1;
            let evaluated = run
                .legacy
                .stub_advance(Some(&mut run.layer), &mut run.gb, &mut run.adapter, run.ms)
                .expect("a frame should complete");
            run.payouts.extend(evaluated.rewards);
        }
        battles = 0;
        let mut was_in_battle = false;
        for _ in 0..budget {
            run.step();
            let now = run.in_wild_battle();
            if now && !was_in_battle {
                battles += 1;
            }
            was_in_battle = now;
            if !run.catches().is_empty() {
                break;
            }
        }
        if !run.catches().is_empty() || attempt == 23 {
            break run;
        }
        eprintln!("attempt {attempt}: no catch ({battles} wild battles, {} balls left)", run.balls());
        attempt += 1;
    };

    let catches = run.catches();
    assert!(
        !catches.is_empty(),
        "no catch in {:.1} brain minutes: {battles} wild battles, {} balls left, map {:#04x}, \
         macros {:?}",
        run.ms / 60_000.0,
        run.balls(),
        run.adapter.map_id().unwrap_or(u32::MAX),
        run.layer.counts()
    );

    let caught = catches[0];
    eprintln!(
        "caught after {:.1} brain minutes and {battles} wild battles: {} for {}",
        run.ms / 60_000.0,
        caught.label,
        caught.value
    );
    assert!(caught.label.starts_with("CAUGHT #"), "{}", caught.label);
    assert!(
        (caught.value - 0.30).abs() < 1e-12 || caught.value == catalog::CATCH_REPEAT_VALUE,
        "a catch pays one of the rule's two amounts, not {}",
        caught.value
    );
    // The cartridge's own flag is clear again by the time the payout lands, which is what makes
    // the payout a battle-exit event rather than a per-frame one.
    assert_eq!(run.byte(ram::wCapturedMonSpecies), 0);
    assert_eq!(
        run.adapter.progress().counts[catalog::kind::CATCH],
        1,
        "one battle, one payout"
    );
    // And a species the run is paid for catching is a species the Pokédex knows: the same event
    // sets the bit the `species` rule reads, whether or not it was new to this run.
    assert!(run.balls() < balls, "a ball was spent");
}

/// What one drive from the checkpoint saw after its catch.
struct AfterCatch {
    caught_at: u32,
    ended_at: u32,
    /// Frames between the catch and the battle's end on which `THROW BALL` was on the pad.
    ball_dealt: u32,
    /// Every pad the fly could decide on in that stretch.
    pads: std::collections::BTreeSet<Vec<String>>,
    /// Row 69: the naming screen, as this test reads it off the cartridge ([`keyboard`]).
    keyboard: Keyboard,
}

/// Row 69: what the naming screen saw, read by this test itself rather than by the palette.
#[derive(Debug, Default)]
struct Keyboard {
    /// Frames the keyboard was up.
    frames: u32,
    /// Frames on it with no macro running on which the cartridge was given a button: the fly's
    /// own presses.
    raw_frames: u32,
    /// Macros started while it was up, by name.
    starts: Vec<&'static str>,
    /// Pads dealt on it, while nothing ran.
    pads: std::collections::BTreeSet<Vec<String>>,
    /// The longest the name got, and the name on its last frame (`wStringBuffer`, decoded).
    longest: u8,
    name: String,
    /// Frames from its first frame to its last.
    span: u32,
    closed: bool,
}

/// `wStringBuffer`, from `CalcStringLength`'s `ld hl, wStringBuffer` on the cartridge (row 69).
const STRING_BUFFER: u16 = 0xcf4b;
/// `wNamingScreenNameLength` (row 69, `pokemon_red::state::poke::NAMING_LENGTH`).
const NAMING_LENGTH: u16 = 0xcee9;

/// The naming screen, as this test reads it: the keyboard's menu bytes, its box's corner at
/// (0, 4), and an underscore under the name at (10, 3). The letters typed, or `None`.
fn keyboard(run: &mut Run) -> Option<u8> {
    let tile = |run: &mut Run, x: u16, y: u16| run.byte(ram::wTileMap + y * 20 + x);
    let menu = run.byte(ram::wTopMenuItemY) == 3
        && run.byte(ram::wMaxMenuItem) == 7
        && run.byte(ram::wMenuWatchedKeys) == 0xff;
    (menu && tile(run, 0, 4) == 0x79 && tile(run, 19, 14) == 0x7e
        && matches!(tile(run, 10, 3), 0x76 | 0x77))
        .then(|| run.byte(NAMING_LENGTH))
}

/// The name in `wStringBuffer`, in Red's charmap: capitals from `$80`, small letters from `$a0`.
fn typed(run: &mut Run) -> String {
    (0..11u16)
        .map(|index| run.byte(STRING_BUFFER + index))
        .take_while(|byte| *byte != 0x50)
        .map(|byte| match byte {
            0x80..=0x99 => char::from(b'A' + (byte - 0x80)),
            0xa0..=0xb9 => char::from(b'a' + (byte - 0xa0)),
            0x7f => ' ',
            _ => '?',
        })
        .collect()
}

/// One drive: `idle` frames with nothing pressed first (so each attempt reaches the wild battle
/// on a different frame, and the ball meets a different random number), then the first test's
/// drive up to the catch, then the pad's own buttons in turn. `None` when no ball kept anything
/// within the budget: one ball in the bag misses about two throws in three.
fn drive_past_a_catch(rom: &[u8], checkpoint: &flysim::store::Checkpoint, idle: u32) -> Option<AfterCatch> {
    drive_past_a_catch_typing(rom, checkpoint, idle, true)
}

/// [`drive_past_a_catch`], and on the keyboard lean on A and B (`type_keys`) or on nothing, so
/// only the D-pad group's own rotation reaches it and the row 69 bound is what ends it.
fn drive_past_a_catch_typing(
    rom: &[u8],
    checkpoint: &flysim::store::Checkpoint,
    idle: u32,
    type_keys: bool,
) -> Option<AfterCatch> {
    let mut run = Run::resume(rom, checkpoint);
    for _ in 0..idle {
        run.ms += MS_PER_FRAME;
        run.frame += 1;
        let evaluated = run
            .legacy
            .stub_advance(Some(&mut run.layer), &mut run.gb, &mut run.adapter, run.ms)
            .expect("a frame should complete");
        run.payouts.extend(evaluated.rewards);
    }
    let budget = 5 * 60 * 60;
    let mut caught_at: Option<u32> = None;
    let mut ball_dealt = 0u32;
    let mut pads = std::collections::BTreeSet::new();
    let mut hold = 0usize;
    let mut board = Keyboard::default();
    let mut board_from: Option<u32> = None;
    for frame in 0..budget {
        let bound = run.layer.bound_channels();
        let on_keyboard = keyboard(&mut run);
        let hot = if caught_at.is_some() && on_keyboard.is_some() && bound.is_empty() && !type_keys {
            None
        } else if caught_at.is_some() && on_keyboard.is_some() && bound.is_empty() {
            // Row 69: the keyboard's pad is the fly's own buttons. The stub leans on A (a letter)
            // mostly and on B (a deletion) now and then; the D-pad is the direction group's own
            // rotation. What the name comes out as is the stub's, as it will be the brain's.
            if frame % BURST_FRAMES == 0 {
                hold += 1;
            }
            Some(if hold % 4 == 3 { "command_5".to_string() } else { "command_4".to_string() })
        } else if caught_at.is_none() {
            if run.in_wild_battle() {
                Some(BALL.to_string())
            } else {
                let slot = (run.frame / BURST_FRAMES) as usize % run.channels.len();
                Some(run.channels[slot].to_string())
            }
        } else if bound.is_empty() {
            None
        } else {
            if frame % BURST_FRAMES == 0 {
                hold += 1;
            }
            Some(bound[hold % bound.len()].clone())
        };
        if caught_at.is_some() {
            if bound.iter().any(|channel| channel == THROW_BALL_CHANNEL) {
                ball_dealt += 1;
            }
            if run.layer.running().is_none() && !bound.is_empty() {
                pads.insert(bound.clone());
            }
        }
        let active = run.decoder.decode_bound(&rates(hot.as_deref()), run.ms, false, None, Some(&bound));
        // The readout's own buttons as raw mode would press them: the layer passes them to the
        // cartridge only where a scene says so (the title, and since row 69 the keyboard).
        let raw = to_button_mask(&active);
        let idle = run.layer.running().is_none();
        let executed =
            run.legacy.execute(Some(&mut run.layer), &active, raw, run.ms, &mut run.gb, &run.adapter);
        if let Some(length) = on_keyboard {
            board.frames += 1;
            board_from.get_or_insert(frame);
            board.span = frame - board_from.unwrap_or(frame);
            board.longest = board.longest.max(length);
            board.name = typed(&mut run);
            if idle {
                if !bound.is_empty() {
                    board.pads.insert(bound.clone());
                }
                if executed.mask != 0 && executed.events.is_empty() {
                    board.raw_frames += 1;
                }
            }
            for event in &executed.events {
                if event.outcome.is_none() {
                    board.starts.push(event.name);
                }
            }
        } else if board.frames > 0 {
            board.closed = true;
        }
        run.ms += MS_PER_FRAME;
        run.frame += 1;
        let evaluated = run
            .legacy
            .stub_advance(Some(&mut run.layer), &mut run.gb, &mut run.adapter, run.ms)
            .expect("a frame should complete");
        run.payouts.extend(evaluated.rewards);
        if caught_at.is_none() && run.byte(ram::wCapturedMonSpecies) != 0 {
            caught_at = Some(frame);
        }
        if let Some(caught_at) = caught_at
            && run.byte(ram::wIsInBattle) == 0
        {
            assert!(!run.catches().is_empty(), "the catch pays at the battle's end");
            return Some(AfterCatch { caught_at, ended_at: frame, ball_dealt, pads, keyboard: board });
        }
    }
    assert!(caught_at.is_none(), "a battle still running {budget} frames after its catch");
    None
}

/// Row 66: once a ball has kept a Pokémon, nothing to the end of the battle is the bag.
///
/// After the throw `wListMenuID` still says `ITEMLISTMENU` -- it is written when a list opens and
/// never cleared -- so on v0.6.5 every frame from "All right! … was caught!" through the Pokédex
/// page, the nickname offer and the naming screen read as an open battle bag. The pad was `BACK`
/// and `THROW BALL`, a `THROW BALL` that threw nothing and whose A presses typed the nickname;
/// driven from the rung-8 survey checkpoint the battle took 6,900 frames to end after the catch.
///
/// The drive after the catch leans on the pad's own buttons in turn (not the whole channel
/// list), so what it measures is what the pad offers, not how often a rotation over every channel
/// lands on it.
#[test]
fn row66_after_a_catch_the_pad_is_not_the_bag_and_the_battle_ends() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_CATCH_CHECKPOINT");
        return;
    };
    if Run::resume(&rom, &checkpoint).balls() == 0 {
        eprintln!("skipped: the checkpoint's bag holds no ball");
        return;
    }
    let after = (0..24)
        .find_map(|attempt| {
            let after = drive_past_a_catch(&rom, &checkpoint, attempt * 5);
            if after.is_none() {
                eprintln!("attempt {attempt}: the ball missed");
            }
            after
        })
        .expect("one of 24 attempts should keep a Pokémon");
    eprintln!(
        "caught at f{}, battle over at f{} ({} frames); THROW BALL dealt on {} of them; pads {:?}",
        after.caught_at,
        after.ended_at,
        after.ended_at - after.caught_at,
        after.ball_dealt,
        after.pads
    );
    assert_eq!(after.ball_dealt, 0, "THROW BALL on the pad after the catch: {:?}", after.pads);
    assert!(
        after.pads.iter().all(|pad| !pad.iter().any(|channel| channel == "macro_back")),
        "BACK on the pad after the catch, with no list up: {:?}",
        after.pads
    );
    assert!(
        after.ended_at - after.caught_at < 3_000,
        "the battle took {} frames to end after the catch",
        after.ended_at - after.caught_at
    );
}

/// Row 69 ("let the fly name it"): after a catch the nickname offer is answered YES as before,
/// and the keyboard's pad is the fly's own buttons -- the D-pad, A, B, START -- with no macro on
/// it, until the name is full (or the bound runs), when `CONFIRM` alone hands the name back. The
/// keyboard closes and the battle ends.
///
/// On a69a258 (row 66) the keyboard read as the battle's between-turns frame and its pad was
/// `NEXT`, whose A presses typed the name.
#[test]
fn row69_after_a_catch_the_keyboard_is_the_flys_own_buttons_and_it_ends() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_CATCH_CHECKPOINT");
        return;
    };
    if Run::resume(&rom, &checkpoint).balls() == 0 {
        eprintln!("skipped: the checkpoint's bag holds no ball");
        return;
    }
    let after = (0..24)
        .find_map(|attempt| drive_past_a_catch(&rom, &checkpoint, attempt * 5))
        .expect("one of 24 attempts should keep a Pokémon");
    let board = &after.keyboard;
    eprintln!(
        "caught at f{}, battle over at f{}; keyboard {} frames over {} (raw {}), longest {}, \
         name {:?}, starts {:?}, pads {:?}, closed {}",
        after.caught_at,
        after.ended_at,
        board.frames,
        board.span,
        board.raw_frames,
        board.longest,
        board.name,
        board.starts,
        board.pads,
        board.closed
    );
    assert!(board.frames > 0, "a catch goes to the keyboard (the nickname offer answered YES)");
    assert!(board.raw_frames > 0, "the fly's own buttons reached the keyboard");
    assert!(board.longest > 0, "the fly typed a letter");
    assert!(
        board.starts.iter().all(|name| *name == "CONFIRM"),
        "no macro types on the keyboard: {:?}",
        board.starts
    );
    assert!(
        board.pads.iter().all(|pad| pad == &vec!["macro_confirm".to_string()]),
        "the keyboard deals CONFIRM alone or nothing: {:?}",
        board.pads
    );
    assert_ne!(board.name, "AAAAAAAAAA", "not the name NEXT's A presses typed on a69a258");
    assert!(board.closed, "the keyboard closed");
    // The bound is sixty brain seconds, about 3,600 frames; the stub fills the name long before.
    assert!(board.span < 3_700, "the keyboard was up for {} frames", board.span);
    assert!(after.ended_at - after.caught_at < 6_000, "the battle ended");
}

/// `ItemUseBall`'s HP test, this file's own reading of it (`engine/items/item_effects.asm`): for
/// the first ball in the bag, `(MaxHP * 255 / BallFactor) / max(HP / 4, 1) >= 255`, a factor of 8
/// for a Great Ball and 12 for the rest.
fn enemy_low(run: &mut Run) -> bool {
    let word = |run: &mut Run, address: u16| {
        u16::from(run.byte(address)) * 256 + u16::from(run.byte(address + 1))
    };
    let hp = word(run, ram::wEnemyMonHP);
    let max = word(run, ram::wEnemyMonMaxHP);
    let Some(ball) = state::bag(&mut run.gb)
        .iter()
        .find(|item| (0x01..=0x04).contains(&item.id) && item.count > 0)
        .map(|item| item.id)
    else {
        return false;
    };
    if hp == 0 || max == 0 || hp > max {
        return false;
    }
    let factor: u32 = if ball == 0x03 { 8 } else { 12 };
    let quarter = u32::from((hp >> 2) as u8).max(1);
    (u32::from(max) * 255 / factor) / quarter >= 255
}

/// Row 69's checkpoint for the low-HP drive, or the catch checkpoint when it is not set.
///
/// The catch checkpoint's lead is a level-twenty Wartortle that takes every forest Pokémon from
/// full HP to nothing in one hit, so no wild Pokémon is ever *low* there. `FLY_ROW69_CHECKPOINT`
/// names a rung-9 state whose lead is weak enough for a wild Pokémon's HP to pass through a third
/// on its way down, with a ball in the bag.
fn row69_checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_ROW69_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

/// Row 69 ("throw on low HP only"): in a wild battle, with a ball in the bag and the wild
/// Pokémon's HP where the ball's HP factor is at its best, `THROW BALL` is the only attack-side
/// button on the pad -- no `MOVE n`, no `SWITCH`, no `RUN` -- and the throw happens.
///
/// The stub leans on the pad's own buttons in turn, leaving `THROW BALL` alone while HP is high
/// so the checkpoint's one ball is not spent before the HP is low. On a69a258 the low-HP pads
/// hold the moves beside the ball.
#[test]
fn row69_at_low_hp_a_wild_battles_pad_is_the_ball() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = row69_checkpoint() else {
        eprintln!("skipped: no FLY_ROW69_CHECKPOINT");
        return;
    };
    if Run::resume(&rom, &checkpoint).balls() == 0 {
        eprintln!("skipped: the checkpoint's bag holds no ball");
        return;
    }
    let mut low_pads: std::collections::BTreeSet<Vec<String>> = Default::default();
    let mut high_ball_pads: std::collections::BTreeSet<Vec<String>> = Default::default();
    let mut low_frames = 0u32;
    let mut thrown_low = 0u32;
    let mut battles = 0u32;
    let mut hp_log: Vec<(u8, u16, u16, u16)> = Vec::new();
    for attempt in 0..16u32 {
        let mut run = Run::resume(&rom, &checkpoint);
        for _ in 0..attempt * 7 {
            run.ms += MS_PER_FRAME;
            run.frame += 1;
            let _ = run
                .legacy
                .stub_advance(Some(&mut run.layer), &mut run.gb, &mut run.adapter, run.ms)
                .expect("a frame should complete");
        }
        let mut hold = 0usize;
        let mut was_wild = false;
        for frame in 0..3 * 60 * 60u32 {
            let bound = run.layer.bound_channels();
            let wild = run.in_wild_battle();
            if wild && !was_wild {
                battles += 1;
            }
            was_wild = wild;
            let low = wild && enemy_low(&mut run);
            if wild {
                let hp = u16::from(run.byte(ram::wEnemyMonHP)) * 256
                    + u16::from(run.byte(ram::wEnemyMonHP + 1));
                let max = u16::from(run.byte(ram::wEnemyMonMaxHP)) * 256
                    + u16::from(run.byte(ram::wEnemyMonMaxHP + 1));
                let own = u16::from(run.byte(ram::wBattleMonHP)) * 256
                    + u16::from(run.byte(ram::wBattleMonHP + 1));
                let seen = (run.byte(ram::wEnemyMonSpecies), hp, max, own);
                if hp_log.last() != Some(&seen) {
                    hp_log.push(seen);
                }
            }
            let idle = run.layer.running().is_none();
            if idle && wild && bound.iter().any(|channel| channel == THROW_BALL_CHANNEL) {
                if low {
                    low_frames += 1;
                    low_pads.insert(bound.clone());
                } else {
                    high_ball_pads.insert(bound.clone());
                }
            }
            let hot = if wild && !bound.is_empty() {
                if frame % BURST_FRAMES == 0 {
                    hold += 1;
                }
                // Not `RUN` either: a fled battle never reaches low HP.
                let choices: Vec<&String> = bound
                    .iter()
                    .filter(|channel| low || *channel != THROW_BALL_CHANNEL)
                    .filter(|channel| *channel != "macro_run")
                    .collect();
                choices.get(hold % choices.len().max(1)).map(|channel| channel.to_string())
            } else {
                let slot = (run.frame / BURST_FRAMES) as usize % run.channels.len();
                Some(run.channels[slot].to_string())
            };
            let balls = run.balls();
            let active =
                run.decoder.decode_bound(&rates(hot.as_deref()), run.ms, false, None, Some(&bound));
            run.legacy.execute(Some(&mut run.layer), &active, 0, run.ms, &mut run.gb, &run.adapter);
            run.ms += MS_PER_FRAME;
            run.frame += 1;
            let evaluated = run
                .legacy
                .stub_advance(Some(&mut run.layer), &mut run.gb, &mut run.adapter, run.ms)
                .expect("a frame should complete");
            run.payouts.extend(evaluated.rewards);
            if run.balls() < balls {
                if low {
                    thrown_low += 1;
                }
                break;
            }
        }
        if thrown_low > 0 && low_frames > 0 {
            break;
        }
    }
    eprintln!(
        "{battles} wild battles; {low_frames} low-HP frames with the ball dealt; thrown at low HP \
         {thrown_low}; low pads {low_pads:?}; high pads with the ball {high_ball_pads:?}"
    );
    eprintln!("(species, enemy hp, max, own hp) as it changed: {:?}", &hp_log[..hp_log.len().min(120)]);
    assert!(low_frames > 0, "no wild battle reached low HP with a ball dealt");
    let allowed = [THROW_BALL_CHANNEL, "macro_item"];
    for pad in &low_pads {
        assert!(
            pad.iter().all(|channel| allowed.contains(&channel.as_str())),
            "a low-HP pad with more than the ball on its attack side: {pad:?}"
        );
    }
    assert!(thrown_low > 0, "the ball was thrown at low HP");
}

/// Row 69's bound: a fly that types nothing is not left on the keyboard. After sixty brain seconds
/// (`palette::NAMING_BOUND_MS`) `CONFIRM` alone is dealt, hands the name back empty, and the
/// cartridge keeps the species' own name. The stub presses no letter: only the D-pad group's own
/// rotation reaches the keyboard.
#[test]
fn row69_a_fly_that_types_nothing_is_ended_by_the_bound() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_CATCH_CHECKPOINT");
        return;
    };
    if Run::resume(&rom, &checkpoint).balls() == 0 {
        eprintln!("skipped: the checkpoint's bag holds no ball");
        return;
    }
    let after = (0..24)
        .find_map(|attempt| drive_past_a_catch_typing(&rom, &checkpoint, attempt * 5, false))
        .expect("one of 24 attempts should keep a Pokémon");
    let board = &after.keyboard;
    eprintln!(
        "keyboard {} frames over {} (raw {}), longest {}, name {:?}, starts {:?}, pads {:?}, \
         closed {}",
        board.frames,
        board.span,
        board.raw_frames,
        board.longest,
        board.name,
        board.starts,
        board.pads,
        board.closed
    );
    assert_eq!(board.longest, 0, "nothing typed");
    assert_eq!(board.starts, vec!["CONFIRM"], "the bound's one button, once");
    assert_eq!(
        board.pads.iter().cloned().collect::<Vec<_>>(),
        vec![vec!["macro_confirm".to_string()]]
    );
    // Sixty brain seconds at 59.7275 frames a second is 3,584 frames, and a little for the press.
    assert!((3_500..3_800).contains(&board.span), "the bound ran: {} frames", board.span);
    assert!(board.closed);
}

/// Row 69: the two naming-screen bytes the palette reads are where the cartridge's own
/// instructions store them. `PrintNicknameAndUnderscores`: `ld a, c / ld [wNamingScreenNameLength],
/// a / hlcoord 10, 2` (`$79 $EA lo hi $21 $D2 $C3`); `DisplayNamingScreen.pressedStart`:
/// `ld a, 1 / ld [wNamingScreenSubmitName], a / ret` (`$3E $01 $EA lo hi $C9`).
#[test]
fn row69_the_naming_bytes_are_where_the_cartridge_stores_them() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    use flybrain_gb::pokemon_red::state::poke;
    let [lo, hi] = poke::NAMING_LENGTH.to_le_bytes();
    let length = [0x79, 0xea, lo, hi, 0x21, 0xd2, 0xc3];
    assert!(rom.windows(length.len()).any(|window| window == length), "wNamingScreenNameLength");
    let [lo, hi] = poke::NAMING_SUBMIT.to_le_bytes();
    let submit = [0x3e, 0x01, 0xea, lo, hi, 0xc9];
    assert!(rom.windows(submit.len()).any(|window| window == submit), "wNamingScreenSubmitName");
    assert_eq!(poke::NAMING_SUBMIT, poke::NAMING_LENGTH + 1, "consecutive in the UNION");
}
