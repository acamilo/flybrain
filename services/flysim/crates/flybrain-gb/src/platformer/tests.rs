//! Synthetic-trace tests for `sml-progress-v1`, one per rule.
//!
//! The list is `docs/design/platformer.md` §7, "Synthetic traces", in order. Every
//! trace is a byte map over a fake 64 KiB address space, grounded in the
//! disassembly through `symbols.rs` rather than in a running game — no ROM is
//! needed and none is read here.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

/// A cartridge hash that is not any real cartridge: 64 hex digits, so it passes
/// the pin's format check.
const TEST_PIN: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// A flat 64 KiB address space plus a per-address read counter, the same shape
/// [`crate::Emulator`]'s frame cache presents.
struct FakeMemory {
    bytes: Vec<u8>,
    reads: BTreeMap<u16, u32>,
}

impl MemoryReader for FakeMemory {
    fn read8(&mut self, address: u16) -> u8 {
        *self.reads.entry(address).or_insert(0) += 1;
        self.bytes[address as usize]
    }
}

impl FakeMemory {
    fn new() -> Self {
        Self { bytes: vec![0; 65536], reads: BTreeMap::new() }
    }

    fn set(&mut self, address: u16, value: u8) {
        self.bytes[address as usize] = value;
    }
}

/// `n` as one BCD byte.
fn bcd_byte(n: u32) -> u8 {
    (((n / 10) % 10) << 4) as u8 | (n % 10) as u8
}

struct Fixture {
    memory: FakeMemory,
    adapter: PlatformerAdapter,
    ms: f64,
}

impl Fixture {
    /// A fixture parked in 1-1 at the level's first column, in a state that
    /// passes both the playable gate and the safe-snapshot gate.
    fn new() -> Self {
        let mut memory = FakeMemory::new();
        memory.set(hram::hGameState, symbols::GAME_STATE_NORMAL);
        memory.set(hram::hGamePaused, 0);
        memory.set(hram::UNNAMED_DEMO_GATE, 0);
        memory.set(hram::UNNAMED_UNDERGROUND, 0);
        memory.set(wram::wGameOverWindowEnabled, 0);
        memory.set(hram::hWorldAndLevel, 0x11);
        memory.set(hram::hLevelIndex, 0);
        memory.set(wram::wLives, 3);
        memory.set(wram::UNNAMED_ON_GROUND, 1);
        memory.set(wram::UNNAMED_JUMP_STATUS, 0);
        memory.set(wram::wInvincibilityTimer, 0);
        // 3-byte BCD "400", comfortably above the safe-snapshot floor.
        memory.set(wram::wGameTimer, 0x00);
        memory.set(wram::wGameTimer + 1, 0x04);
        memory.set(wram::wGameTimer + 2, 0x00);
        memory.set(wram::wGameTimerExpiringFlag, 0);
        let mut fixture =
            Self { memory, adapter: PlatformerAdapter::with_rom_pin(Some(TEST_PIN)), ms: 0.0 };
        fixture.column(symbols::FIRST_COLUMN);
        fixture
    }

    fn sample(&mut self) -> Vec<RewardEvent> {
        self.memory.reads.clear();
        self.ms += 17.0;
        let ms = self.ms;
        self.adapter.sample(&mut self.memory, ms)
    }

    /// Put the camera at an absolute column.
    fn column(&mut self, column: u32) {
        self.memory
            .set(hram::hScreenIndex, (column / symbols::COLUMNS_PER_SCREEN) as u8);
        self.memory
            .set(hram::hColumnIndex, (column % symbols::COLUMNS_PER_SCREEN) as u8);
    }

    /// Move to `column` and take one sample.
    fn at(&mut self, column: u32) -> Vec<RewardEvent> {
        self.column(column);
        self.sample()
    }

    /// The three consecutive same-level samples the gate needs before anything
    /// pays, which is also where baselining happens.
    fn settle(&mut self) -> Vec<RewardEvent> {
        let mut events = self.sample();
        events.extend(self.sample());
        events.extend(self.sample());
        events
    }

    /// Boot the fixture properly: a start-menu sample, then settle in 1-1. Used
    /// wherever `started` has to be observable.
    fn boot_then_play(&mut self) -> Vec<RewardEvent> {
        self.memory.set(hram::hGameState, 0x0f);
        assert!(self.sample().is_empty(), "a menu pays nothing");
        self.memory.set(hram::hGameState, symbols::GAME_STATE_NORMAL);
        self.settle()
    }

    fn set_coins(&mut self, coins: u32) {
        self.memory.set(hram::hCoins, bcd_byte(coins));
    }

    fn set_score(&mut self, score: u32) {
        self.memory.set(wram::wScore, bcd_byte(score / 10_000));
        self.memory.set(wram::wScore + 1, bcd_byte((score / 100) % 100));
        self.memory.set(wram::wScore + 2, bcd_byte(score % 100));
    }

    /// Enter `level` (0-based), reporting a consistent `hWorldAndLevel`, and
    /// settle there.
    fn enter_level(&mut self, level: u32) -> Vec<RewardEvent> {
        let (world, stage) = (level / 3 + 1, level % 3 + 1);
        self.memory.set(hram::hLevelIndex, level as u8);
        self.memory
            .set(hram::hWorldAndLevel, ((world as u8) << 4) | stage as u8);
        self.column(symbols::FIRST_COLUMN);
        self.settle()
    }
}

fn kinds(events: &[RewardEvent]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind).collect()
}

fn labels(events: &[RewardEvent]) -> Vec<&str> {
    events.iter().map(|event| event.label.as_str()).collect()
}

fn of_kind<'a>(events: &'a [RewardEvent], kind: &str) -> Vec<&'a RewardEvent> {
    events.iter().filter(|event| event.kind == kind).collect()
}

// --- Gates and baselining ------------------------------------------------------

#[test]
fn boot_then_playable_pays_started_once_and_reads_each_address_once() {
    let mut f = Fixture::new();
    f.memory.set(hram::hGameState, 0x0f);
    assert!(f.sample().is_empty());
    assert_eq!(f.adapter.mode(), "BOOT");
    assert!(f.adapter.boot());

    f.memory.set(hram::hGameState, symbols::GAME_STATE_NORMAL);
    // The first two samples are the stability gate; the third baselines and pays.
    assert!(f.sample().is_empty());
    assert!(f.sample().is_empty());
    let events = f.sample();
    assert_eq!(labels(&events), ["RUN STARTED"]);
    assert_eq!(events[0].value, 1.0);
    assert_eq!(events[0].stimulation_ms, 250);
    assert!(
        f.memory.reads.values().all(|count| *count == 1),
        "a sample must read each address at most once: {:?}",
        f.memory.reads.iter().filter(|(_, count)| **count != 1).collect::<Vec<_>>()
    );
    assert_eq!(f.adapter.mode(), "IN LEVEL 1-1");
    assert!(!f.adapter.boot(), "a playable sample is not boot");
    assert!(f.sample().is_empty(), "started pays once per lifetime");
}

#[test]
fn a_migrated_save_that_never_saw_boot_pays_no_started() {
    let mut f = Fixture::new();
    let events = f.settle();
    assert!(events.is_empty(), "no boot state was observed: {:?}", labels(&events));
    assert!(f.adapter.rank() >= 1);
}

#[test]
fn the_attract_demo_gate_pays_nothing() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::UNNAMED_DEMO_GATE, 28);
    assert!(f.at(150).is_empty());
    assert_eq!(f.adapter.mode(), "DEMO");
    assert!(!f.adapter.safe_for_snapshot());
    // And the ground the demo covered is not banked: it pays when the fly gets there.
    f.memory.set(hram::UNNAMED_DEMO_GATE, 0);
    f.column(150);
    let events = f.settle();
    assert_eq!(kinds(&events), [kind::BAND]);
}

#[test]
fn the_pause_gate_pays_nothing() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::hGamePaused, 1);
    assert!(f.at(150).is_empty());
    assert_eq!(f.adapter.mode(), "TRANSITION");
    assert!(!f.adapter.safe_for_snapshot());
}

#[test]
fn an_autoscroll_level_is_playable_but_never_safe() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::hGameState, symbols::GAME_STATE_AUTOSCROLL);
    let events = f.at(150);
    assert_eq!(kinds(&events), [kind::BAND], "autoscroll still pays for ground");
    assert!(
        !f.adapter.safe_for_snapshot(),
        "restoring a vehicle level mid-flight is fragile, so it is never archived"
    );
}

#[test]
fn the_game_over_states_report_game_over_and_pay_nothing() {
    for state in [symbols::GAME_STATE_GAME_OVER_TEXT, symbols::GAME_STATE_GAME_OVER_WAIT] {
        let mut f = Fixture::new();
        f.boot_then_play();
        f.memory.set(hram::hGameState, state);
        assert!(f.sample().is_empty(), "{state:#04x}");
        assert_eq!(f.adapter.mode(), "GAME OVER");
        assert!(f.adapter.game_over(), "{state:#04x}");
        assert!(f.adapter.boot(), "Start must work again after a game over");
        assert!(!f.adapter.safe_for_snapshot());
    }
}

#[test]
fn no_lives_with_the_game_over_window_up_is_a_game_over() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(wram::wLives, 0);
    f.memory.set(wram::wGameOverWindowEnabled, 1);
    assert!(f.sample().is_empty());
    assert!(f.adapter.game_over());
    assert_eq!(f.adapter.mode(), "GAME OVER");
}

#[test]
fn an_inconsistent_world_level_and_index_reports_transition() {
    // Every disagreement the design lists, plus a screen and column out of range.
    let cases: [(&str, u8, u8, u8, u8); 6] = [
        ("world 0", 0x01, 0, 3, 0),
        ("stage 4", 0x14, 0, 3, 0),
        ("index disagrees", 0x11, 4, 3, 0),
        ("index out of range", 0x43, 12, 3, 0),
        ("screen before the first", 0x11, 0, 2, 0),
        ("column past the screen", 0x11, 0, 3, 20),
    ];
    for (name, world_and_level, level, screen, column) in cases {
        let mut f = Fixture::new();
        f.boot_then_play();
        f.memory.set(hram::hWorldAndLevel, world_and_level);
        f.memory.set(hram::hLevelIndex, level);
        f.memory.set(hram::hScreenIndex, screen);
        f.memory.set(hram::hColumnIndex, column);
        assert!(f.sample().is_empty(), "{name}");
        assert_eq!(f.adapter.mode(), "TRANSITION", "{name}");
    }
}

#[test]
fn a_non_bcd_counter_reports_transition_rather_than_a_huge_delta() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::hCoins, 0xaa);
    assert!(f.sample().is_empty());
    assert_eq!(f.adapter.mode(), "TRANSITION");
}

#[test]
fn an_unpinned_or_wrong_cartridge_samples_without_paying() {
    let mut f = Fixture::new();
    f.adapter = PlatformerAdapter::new();
    assert!(f.boot_then_play().is_empty());
    assert!(f.adapter.mode().contains("SEMANTIC REWARDS OFF"));
    assert!(!f.adapter.rom_allowed(TEST_PIN));
    assert_eq!(f.adapter.progress().reward_total, 0.0);

    let pinned = PlatformerAdapter::with_rom_pin(Some(TEST_PIN));
    assert!(pinned.rom_allowed(TEST_PIN));
    assert!(pinned.rom_allowed(&TEST_PIN.to_ascii_uppercase()), "the pin is case-insensitive");
    assert!(!pinned.rom_allowed(crate::pokemon_red::SUPPORTED_ROM));
    assert_eq!(PlatformerAdapter::with_rom_pin(Some("nonsense")).rom_pin(), None);
}

// --- Bands ---------------------------------------------------------------------

#[test]
fn a_new_band_pays_and_the_first_sample_baselines_the_ground_behind_it() {
    let mut f = Fixture::new();
    f.column(150);
    assert!(f.settle().is_empty(), "ground already behind the camera is a baseline");
    let events = f.at(160);
    assert_eq!(kinds(&events), [kind::BAND]);
    assert_eq!(events[0].value, 0.05);
    assert_eq!(events[0].stimulation_ms, 80);
    assert_eq!(labels(&events), ["NEW GROUND 1-1 33%"]);
    // Coverage counts every band key in the ledger, baselined ground included:
    // bands 6 to 15 were behind the camera at the first sample, and band 16 is
    // the one just paid for.
    assert_eq!(f.adapter.progress().unique_locations, 11);
}

#[test]
fn oscillating_across_a_band_boundary_pays_once_ever() {
    let mut f = Fixture::new();
    f.boot_then_play();
    assert_eq!(kinds(&f.at(70)), [kind::BAND]);
    for _ in 0..20 {
        assert!(f.at(69).is_empty());
        assert!(f.at(70).is_empty());
        assert!(f.at(75).is_empty());
    }
}

#[test]
fn band_payouts_stop_at_two_per_screen_of_level_length() {
    let mut f = Fixture::new();
    f.boot_then_play();
    let mut paid = 0;
    // Walk the whole of 1-1 and then past its last band.
    let mut column = symbols::FIRST_COLUMN;
    while column < symbols::LEVEL_SCREENS[0] * symbols::COLUMNS_PER_SCREEN + 19 {
        column += symbols::BAND_COLUMNS;
        paid += of_kind(&f.at(column), kind::BAND).len();
    }
    // The band the level starts in was baselined by the first sample, so 29 of
    // the level's 30 bands are payable and the ledger still holds exactly 30.
    assert_eq!(paid as u32, symbols::level_bands(0).unwrap() - 1);
    assert_eq!(paid, 29);
    assert_eq!(
        f.adapter.progress().unique_locations,
        30,
        "2 * (18 - 3), the design's cap for 1-1"
    );
}

#[test]
fn underground_pays_no_bands_because_its_columns_alias_the_level() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::UNNAMED_UNDERGROUND, 1);
    assert!(f.at(200).is_empty(), "a pipe sub-room reuses hScreenIndex");
    assert!(!f.adapter.safe_for_snapshot(), "and is never archived");
    f.memory.set(hram::UNNAMED_UNDERGROUND, 0);
    assert_eq!(kinds(&f.at(200)), [kind::BAND], "the same ground pays above ground");
}

#[test]
fn death_resets_the_position_pays_nothing_and_cannot_re_pay_its_ground() {
    let mut f = Fixture::new();
    f.boot_then_play();
    let mut paid = 0;
    for column in [70, 80, 90, 100, 110, 120] {
        paid += of_kind(&f.at(column), kind::BAND).len();
    }
    assert_eq!(paid, 6);
    let total = f.adapter.statistics().total;

    // Dying: pre-dying, dying, then the checkpoint restore at screen 3.
    for state in [0x03u8, 0x04, 0x02] {
        f.memory.set(hram::hGameState, state);
        assert!(f.sample().is_empty(), "dying pays nothing and costs nothing");
    }
    f.memory.set(hram::hGameState, symbols::GAME_STATE_NORMAL);
    f.column(symbols::FIRST_COLUMN);
    assert!(f.settle().is_empty());
    for column in [70, 80, 90, 100, 110, 120] {
        assert!(f.at(column).is_empty(), "ground already paid never pays again");
    }
    assert_eq!(f.adapter.statistics().total, total, "death is free in both directions");
}

// --- Coins, score, power-ups, lives -------------------------------------------

#[test]
fn a_coin_pays_on_every_increase_up_to_forty_per_level() {
    let mut f = Fixture::new();
    f.boot_then_play();
    let mut coins = 0;
    let mut paid = 0;
    for _ in 0..50 {
        coins += 1;
        f.set_coins(coins % 100);
        paid += of_kind(&f.sample(), kind::COIN).len();
    }
    assert_eq!(paid, 40, "40 payouts per level, lifetime");
    let event = f.adapter.statistics().last.iter().find(|(kind, _)| *kind == kind::COIN).unwrap().1.clone();
    assert_eq!(event.value, 0.02);
    assert_eq!(event.stimulation_ms, 60);
    assert_eq!(event.label, "COIN");
}

#[test]
fn a_coin_counter_wrapping_from_ninety_nine_to_zero_is_an_increase() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.set_coins(99);
    assert_eq!(kinds(&f.sample()), [kind::COIN]);
    f.set_coins(0);
    assert_eq!(
        kinds(&f.sample()),
        [kind::COIN],
        "0x99 -> 0x00 is the hundredth coin, never a decrease"
    );
}

#[test]
fn score_pays_by_delta_size_then_decays_and_caps() {
    let mut f = Fixture::new();
    f.boot_then_play();

    // A full-size delta at n = 0.
    f.set_score(400);
    let events = f.sample();
    assert_eq!(kinds(&events), [kind::SCORE]);
    assert_eq!(events[0].value, 0.05);
    assert_eq!(labels(&events), ["+400"]);

    // A quarter-size delta.
    f.set_score(500);
    let events = f.sample();
    assert!((events[0].value - 0.05 * 0.25).abs() < 1e-12, "{}", events[0].value);

    // n = 1, 2, 3 keep the same divisor; n = 4 halves it.
    let mut score = 500;
    let mut values = Vec::new();
    for _ in 0..6 {
        score += 400;
        f.set_score(score);
        values.push(f.sample()[0].value);
    }
    assert_eq!(values[0], 0.05, "n = 2");
    assert_eq!(values[1], 0.05, "n = 3");
    assert_eq!(values[2], 0.025, "n = 4 halves the payout");
    assert_eq!(values[5], 0.025, "n = 7");

    // The cap: twenty payouts in this level, ever.
    let mut paid = 8;
    for _ in 0..30 {
        score += 400;
        f.set_score(score);
        paid += of_kind(&f.sample(), kind::SCORE).len();
    }
    assert_eq!(paid, 20);

    // A decrease never pays.
    f.set_score(0);
    assert!(f.sample().is_empty());
}

#[test]
fn a_power_up_pays_once_per_level_and_injury_pays_nothing() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::hSuperStatus, 1);
    assert!(f.sample().is_empty(), "growing is not yet super");
    f.memory.set(hram::hSuperStatus, 2);
    let events = f.sample();
    assert_eq!(labels(&events), ["SUPER MARIO"]);
    assert_eq!(events[0].value, 0.5);
    assert_eq!(events[0].stimulation_ms, 200);
    for status in [3u8, 4, 2, 0, 2] {
        f.memory.set(hram::hSuperStatus, status);
        assert!(f.sample().is_empty(), "status {status} in the same level pays nothing");
    }

    f.memory.set(hram::hSuperballMario, 1);
    assert_eq!(labels(&f.sample()), ["SUPERBALL"]);
    assert!(f.sample().is_empty(), "superball pays once per level");

    // A new level is a new pair of keys.
    let events = f.enter_level(1);
    assert_eq!(of_kind(&events, kind::POWERUP).len(), 2, "{:?}", labels(&events));
}

#[test]
fn a_1up_pays_whenever_lives_rise() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(wram::wLives, 4);
    let events = f.sample();
    assert_eq!(labels(&events), ["1UP"]);
    assert_eq!(events[0].value, 1.0);
    assert_eq!(events[0].stimulation_ms, 250);
    assert!(f.sample().is_empty());
    // Dying costs nothing, and the next 1UP still pays: the kind is uncapped.
    f.memory.set(wram::wLives, 3);
    assert!(f.sample().is_empty());
    f.memory.set(wram::wLives, 4);
    assert_eq!(kinds(&f.sample()), [kind::LIFE]);
    assert_eq!(f.adapter.progress().counter, 4);
    assert_eq!(f.adapter.progress().counter_label, "LIVES");
}

// --- Level, world and game clear ----------------------------------------------

#[test]
fn clearing_1_1_pays_a_level_and_moves_the_ladder() {
    let mut f = Fixture::new();
    f.boot_then_play();
    let events = f.enter_level(1);
    // The new level's first band has never been walked either, so it pays too:
    // baselining happens once per lifetime, not once per level.
    assert_eq!(kinds(&events), [kind::BAND, kind::LEVEL]);
    let level = of_kind(&events, kind::LEVEL)[0];
    assert_eq!(level.value, 3.0);
    assert_eq!(level.stimulation_ms, 400);
    assert_eq!(level.label, "LEVEL CLEARED: 1-2 REACHED");
    assert_eq!(f.adapter.rank(), 4);
    assert_eq!(f.adapter.map_id(), Some(1));
    assert!(f.enter_level(1).is_empty(), "a level pays once per lifetime");
}

#[test]
fn clearing_world_1_pays_both_a_level_and_a_world() {
    let mut f = Fixture::new();
    f.boot_then_play();
    for level in 1..=2 {
        f.enter_level(level);
    }
    // 1-3 (0x13) to 2-1 (0x21).
    let events = f.enter_level(3);
    assert_eq!(kinds(&events), [kind::BAND, kind::LEVEL, kind::WORLD]);
    assert_eq!(of_kind(&events, kind::WORLD)[0].value, 5.0);
    assert_eq!(of_kind(&events, kind::WORLD)[0].stimulation_ms, 500);
    assert_eq!(f.adapter.rank(), 6, "4 + highestClearedLevelIndex, 1-3 being index 2");
    assert_eq!(f.adapter.progress().rank_label, "WORLD 1 CLEARED (King Totomesu)");
    assert!(f.enter_level(3).is_empty());
}

#[test]
fn a_game_clear_pays_once_and_tops_the_ladder() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.memory.set(hram::hWinCount, 1);
    let events = f.sample();
    assert_eq!(labels(&events), ["GAME CLEARED"]);
    assert_eq!(events[0].value, 10.0);
    assert_eq!(events[0].stimulation_ms, 800);
    assert_eq!(f.adapter.rank(), 15);
    assert_eq!(f.adapter.progress().rank_label, "GAME CLEARED (Tatanga)");
    f.memory.set(hram::hWinCount, 2);
    assert!(f.sample().is_empty(), "clear pays once per lifetime");
}

// --- Ladder --------------------------------------------------------------------

#[test]
fn the_ladder_is_monotone_and_ranks_four_up_are_four_plus_cleared_levels() {
    let mut f = Fixture::new();
    f.boot_then_play();
    assert_eq!(f.adapter.rank(), 1, "the first playable sample");

    f.set_coins(1);
    f.sample();
    assert_eq!(f.adapter.rank(), 2, "any coin");

    f.at(HALFWAY_1_1_COLUMN - 10);
    assert_eq!(f.adapter.rank(), 2, "not halfway yet");
    f.at(HALFWAY_1_1_COLUMN);
    assert_eq!(f.adapter.rank(), 3);
    assert_eq!(HALFWAY_1_1_COLUMN, 210, "60 + (18 - 3) * 20 / 2");

    let mut ranks = vec![f.adapter.rank()];
    for level in 1..symbols::LEVEL_COUNT {
        f.enter_level(level);
        assert_eq!(f.adapter.rank(), 4 + (level - 1), "rank = 4 + highestClearedLevelIndex");
        ranks.push(f.adapter.rank());
    }
    assert_eq!(*ranks.last().unwrap(), 14, "4-2 cleared");
    assert!(ranks.windows(2).all(|pair| pair[1] > pair[0]), "{ranks:?}");

    // Falling back to 1-1 keeps the rank: the ladder is monotone.
    f.enter_level(0);
    assert_eq!(f.adapter.rank(), 14);
    let progress = f.adapter.progress();
    assert_eq!(progress.rank_max, 15);
    assert_eq!(progress.rank_label, "4-2 CLEARED");
    assert_eq!(RANK_LADDER.len(), 16);
}

// --- Safe snapshot -------------------------------------------------------------

#[test]
fn only_a_grounded_untouched_mario_with_time_on_the_clock_is_safe() {
    let mut f = Fixture::new();
    f.boot_then_play();
    assert!(f.adapter.safe_for_snapshot(), "the fixture is deliberately safe");

    let unsafe_cases: [(&str, u16, u8); 7] = [
        ("airborne", wram::UNNAMED_ON_GROUND, 0),
        ("jumping", wram::UNNAMED_JUMP_STATUS, 1),
        ("falling", wram::UNNAMED_JUMP_STATUS, 2),
        ("invincible", wram::wInvincibilityTimer, 30),
        ("growing", hram::hSuperStatus, 1),
        ("injury i-frames", hram::hSuperStatus, 3),
        ("timer running out", wram::wGameTimerExpiringFlag, 1),
    ];
    for (name, address, value) in unsafe_cases {
        let previous = f.memory.bytes[address as usize];
        f.memory.set(address, value);
        f.sample();
        assert!(!f.adapter.safe_for_snapshot(), "{name}");
        f.memory.set(address, previous);
        f.sample();
        assert!(f.adapter.safe_for_snapshot(), "{name}: restored");
    }

    // A game timer below the floor.
    f.memory.set(wram::wGameTimer + 1, 0x00);
    f.memory.set(wram::wGameTimer + 2, 0x99);
    f.sample();
    assert!(!f.adapter.safe_for_snapshot(), "99 is below the 100-unit floor");
    f.memory.set(wram::wGameTimer + 2, bcd_byte(0));
    f.memory.set(wram::wGameTimer + 1, 0x01);
    f.sample();
    assert!(f.adapter.safe_for_snapshot(), "100 units clears it");
}

#[test]
fn the_first_two_samples_in_a_level_are_never_safe() {
    let mut f = Fixture::new();
    f.memory.set(hram::hLevelIndex, 0);
    assert!(f.sample().is_empty());
    assert!(!f.adapter.safe_for_snapshot(), "one sample is not stability");
    assert!(f.sample().is_empty());
    assert!(!f.adapter.safe_for_snapshot(), "two are not either");
    f.sample();
    assert!(f.adapter.safe_for_snapshot());
}

// --- Checkpoints ---------------------------------------------------------------

#[test]
fn a_mid_level_import_baselines_and_pays_nothing() {
    let mut source = Fixture::new();
    source.boot_then_play();
    for column in [70, 80, 90] {
        source.at(column);
    }
    source.set_coins(5);
    source.sample();
    source.enter_level(1);
    let state = source.adapter.export_state();
    let statistics = source.adapter.statistics();

    let mut f = Fixture::new();
    f.adapter.import_state(&state).unwrap();
    assert_eq!(f.adapter.statistics().total, statistics.total);
    assert_eq!(f.adapter.statistics().counts, statistics.counts);
    assert_eq!(f.adapter.rank(), source.adapter.rank());
    assert_eq!(f.adapter.progress().unique_locations, statistics.bands as usize);

    // Landing back in 1-1 mid-level replays nothing.
    f.memory.set(hram::hLevelIndex, 0);
    f.memory.set(hram::hWorldAndLevel, 0x11);
    f.column(90);
    assert!(f.settle().is_empty());
    assert!(f.at(80).is_empty());
    assert_eq!(f.adapter.statistics().total, statistics.total);
    // Ground never covered still pays.
    assert_eq!(kinds(&f.at(100)), [kind::BAND]);
}

#[test]
fn an_invalid_checkpoint_is_rejected_without_mutating_the_adapter() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.at(70);
    let good = f.adapter.export_state();
    let before = f.adapter.statistics();

    let cases: [(&str, Value); 7] = [
        ("no ledger", json!({ "version": STATE_VERSION })),
        ("ledger is not strings", {
            let mut state = good.clone();
            state["ledger"] = json!([1, 2]);
            state
        }),
        ("total is not finite", {
            let mut state = good.clone();
            state["total"] = json!("nan");
            state
        }),
        ("rank past the ladder", {
            let mut state = good.clone();
            state["rank"] = json!(16);
            state
        }),
        ("level past the game", {
            let mut state = good.clone();
            state["maxLevel"] = json!(12);
            state
        }),
        ("a payout counter on an impossible level", {
            let mut state = good.clone();
            state["coinPayouts"] = json!({ "99": 1 });
            state
        }),
        ("an event of an unknown kind", {
            let mut state = good.clone();
            state["recent"] = json!([{ "kind": "badge", "label": "x", "brainMs": 1, "value": 1 }]);
            state
        }),
    ];
    for (name, state) in cases {
        let mut adapter = PlatformerAdapter::with_rom_pin(Some(TEST_PIN));
        adapter.import_state(&good).unwrap();
        let error = adapter.import_state(&state).unwrap_err();
        assert!(
            error.0.starts_with("Invalid reward"),
            "{name}: unexpected message {}",
            error.0
        );
        assert_eq!(adapter.statistics().total, before.total, "{name}: state was mutated");
        assert_eq!(adapter.rank(), f.adapter.rank(), "{name}: state was mutated");
    }
}

#[test]
fn a_checkpoint_from_another_schema_version_rebaselines_silently() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.at(70);
    let mut state = f.adapter.export_state();
    state["version"] = json!(STATE_VERSION + 1);

    let mut adapter = PlatformerAdapter::with_rom_pin(Some(TEST_PIN));
    adapter.import_state(&state).unwrap();
    assert_eq!(adapter.statistics().total, 0.0, "old semantics are not credited");
    assert_eq!(adapter.rank(), 0);
}

#[test]
fn the_exported_state_names_the_adapter_and_round_trips() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.at(70);
    f.set_coins(1);
    f.sample();
    let state = f.adapter.export_state();
    assert_eq!(state["adapter"], json!(ADAPTER));
    assert_eq!(state["version"], json!(STATE_VERSION));
    assert_eq!(ADAPTER, "sml-progress-v1");

    let mut adapter = PlatformerAdapter::with_rom_pin(Some(TEST_PIN));
    adapter.import_state(&state).unwrap();
    assert_eq!(adapter.export_state(), state);
}

#[test]
fn clearing_transients_keeps_the_ledger_and_forgets_the_latches() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.at(70);
    f.set_coins(4);
    f.sample();
    let total = f.adapter.statistics().total;
    let rank = f.adapter.rank();

    f.adapter.clear_transient();
    assert!(!f.adapter.safe_for_snapshot());
    assert!(!f.adapter.game_over());
    assert_eq!(f.adapter.level(), None);
    assert_eq!(f.adapter.statistics().total, total, "lifetime history survives a rollback");
    assert_eq!(f.adapter.rank(), rank);

    // The restored run re-baselines its coin latch without paying for the gap.
    f.set_coins(0);
    assert!(f.settle().is_empty());
    assert_eq!(f.adapter.statistics().total, total);
    f.set_coins(1);
    assert_eq!(kinds(&f.sample()), [kind::COIN]);
}

// --- Trait surface -------------------------------------------------------------

#[test]
fn the_adapter_asks_for_the_platformer_preset_and_its_own_recovery_policy() {
    let adapter = PlatformerAdapter::new();
    assert_eq!(adapter.decoder_preset(), DecoderPresetId::Platformer);
    let policy = adapter.recovery_policy();
    assert_eq!(policy.stall_ms, 300_000, "300 brain seconds of stall");
    assert_eq!(policy.cooldown_ms, 180_000);
    assert_eq!(policy.game_over_cooldown_ms, Some(60_000));
    assert_eq!(policy.max_attempts, 3);
    assert_eq!(policy.max_recoveries, 48);
    assert_eq!(adapter.rank_ladder().len(), 16);
    assert_eq!(adapter.id(), ADAPTER);
}

#[test]
fn the_hero_number_is_camera_progress_through_the_level() {
    let mut f = Fixture::new();
    f.boot_then_play();
    f.at(210);
    assert_eq!(f.adapter.level_percent(), Some(0.5), "halfway through 1-1");
    assert_eq!(f.adapter.best_column(0), Some(210));
    f.at(120);
    assert_eq!(f.adapter.best_column(0), Some(210), "the ghost marker keeps the best");
    assert_eq!(f.adapter.level_percent(), Some(0.2));
}
