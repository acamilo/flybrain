//! The Super Mario Land reward adapter, `sml-progress-v1`.
//!
//! Implements `docs/design/platformer.md` (2026-09-15), which is binding: every
//! address, value, cap, gate and rank in this module is from that document, and
//! the module comments point at the section rather than restating the argument.
//!
//! Shape mirrors [`crate::pokemon_red`]: a per-sample byte cache so each address
//! crosses the FFI boundary at most once, a playable gate that pays nothing
//! unless every condition holds, baselining on the first valid sample, a
//! lifetime once-ledger that makes oscillation unprofitable, and a checkpoint
//! whose schema version gates crediting.
//!
//! Three things differ from the Pokémon adapter, all of them deliberate:
//!
//! - **The ROM pin is configuration, not a constant.** The design pins the
//!   SHA-256 of Super Mario Land (World) (Rev A), but only its SHA-1 is known
//!   (`docs/design/platformer.md` §8.5), so the hash arrives from
//!   `[game.platformer] rom_sha256` through [`PlatformerAdapter::with_rom_pin`].
//!   No pin means no semantic rewards, exactly as a wrong cartridge does.
//! - **Boot is `!playable`, not one mode string.** `BOOT`, `DEMO`, `GAME OVER`
//!   and `TRANSITION` all need the permissive Start/Select variant so the fly
//!   can begin a run after a game over ([`GameAdapter::boot`]).
//! - **Recovery has its own budget** ([`GameAdapter::recovery_policy`]): a game
//!   over discards the whole run, so it restores immediately and needs a larger
//!   lifetime budget than Pokémon's 36 (§5).
//!
//! Death pays nothing and costs nothing. The only repeatable income is capped,
//! and the only large income is ground never reached before (§2).

pub mod catalog;
pub mod symbols;

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::adapter::{
    AdapterError, DecoderPresetId, GameAdapter, MemoryReader, ProgressSnapshot, RewardEvent,
};
use crate::ordered::OrderedSet;
use crate::ratchet::RecoveryPolicy;
use catalog::{Counts, LastEvents, kind};
use symbols::{bcd, hram, wram};

/// Adapter version, pinned into the checkpoint compatibility string.
pub const ADAPTER: &str = "sml-progress-v1";

/// Schema version of [`PlatformerAdapter::export_state`]. Anything else cannot
/// be credited under the current rules and rebaselines instead.
pub const STATE_VERSION: u64 = 1;

/// The milestone ladder, ranks 0 to 15 (`docs/design/platformer.md` §3). Ranks 4
/// to 14 are `4 + highestClearedLevelIndex`; rank 15 is the game clear.
pub const RANK_LADDER: [&str; 16] = [
    "BOOTING",
    "GAME STARTED",
    "FIRST COIN",
    "HALFWAY THROUGH 1-1",
    "1-1 CLEARED",
    "1-2 CLEARED",
    "WORLD 1 CLEARED (King Totomesu)",
    "2-1 CLEARED",
    "2-2 CLEARED",
    "WORLD 2 CLEARED (Dragonzamasu, Marine Pop)",
    "3-1 CLEARED",
    "3-2 CLEARED",
    "WORLD 3 CLEARED (Hiyoihoi)",
    "4-1 CLEARED",
    "4-2 CLEARED",
    "GAME CLEARED (Tatanga)",
];

/// Column rank 3 needs: `60 + (18 - 3) * 20 / 2`, i.e. halfway through 1-1 (§3).
pub const HALFWAY_1_1_COLUMN: u32 = symbols::FIRST_COLUMN + 15 * symbols::COLUMNS_PER_SCREEN / 2;

/// Consecutive samples in the same level before anything pays, mirroring the
/// Pokémon adapter's `stable >= 3`.
pub const STABLE_SAMPLES: u64 = 3;

/// Lowest decoded `wGameTimer` value a snapshot may be taken at (§2, "Safe
/// snapshot"). The field's unit is the raw 3-byte BCD value; the *scale* of that
/// value is **UNVERIFIED**, so this threshold is the design's number applied to
/// the decoded field and nothing more.
pub const SAFE_MIN_GAME_TIMER: u32 = 100;

/// Recovery budgets for this game (§5): a game over restores immediately with a
/// 60 s cooldown, the stall window is 300 brain seconds, and the lifetime budget
/// is 48 rather than Pokémon's 36, which a platformer would spend in an hour.
/// Only the budgets are per-game: the rank bound is the ladder's own length, which
/// [`crate::Ratchet::import`] takes from the adapter.
pub const RECOVERY_POLICY: RecoveryPolicy = RecoveryPolicy {
    stall_ms: 300_000,
    cooldown_ms: 180_000,
    game_over_cooldown_ms: Some(60_000),
    unsafe_reset_ms: crate::ratchet::UNSAFE_RESET_MS,
    max_attempts: 3,
    max_recoveries: 48,
};

/// Immutable per-sample byte cache, as [`crate::pokemon_red`] uses: each address
/// requested during one sample crosses the FFI boundary at most once.
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
}

/// One playable sample's decoded game state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    level: u32,
    world: u32,
    stage: u32,
    column: u32,
    coins: u32,
    score: u32,
    lives: u32,
    super_status: u8,
    superball: u8,
    wins: u32,
    game_state: u8,
    underground: bool,
    on_ground: bool,
    jumping: bool,
    invincible: bool,
    game_timer: u32,
    timer_expiring: bool,
}

/// Per-life latches: what the previous playable sample read. All of them are
/// transient, because a rollback invalidates every delta they feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Latches {
    coins: Option<u32>,
    score: Option<u32>,
    lives: Option<u32>,
    super_status: Option<u8>,
    superball: Option<u8>,
    wins: Option<u32>,
}

pub struct PlatformerAdapter {
    /// SHA-256 semantic rewards are enabled for, lowercased. `None` disables
    /// them, which is also what a wrong cartridge does.
    rom_pin: Option<String>,

    /// Lifetime once-ledger: `band:<level>:<n>`, `powerup:super:<level>`,
    /// `powerup:ball:<level>`, `level:<index>`, `world:<n>`, `started`,
    /// `clear`, `halfway`.
    ledger: OrderedSet,
    /// Band keys in [`PlatformerAdapter::ledger`], maintained incrementally.
    bands: u64,
    /// Lifetime coin payouts per level, capped by the catalog.
    coin_payouts: BTreeMap<u32, u64>,
    /// Lifetime score payouts per level, capped by the catalog and decaying.
    score_payouts: BTreeMap<u32, u64>,
    /// Furthest column reached per level, for the hero number and the per-level
    /// ghost marker (§6). Not a reward input.
    best_column: BTreeMap<u32, u32>,
    counts: Counts,
    total: f64,
    recent: Vec<RewardEvent>,
    last: LastEvents,
    initialized: bool,
    saw_boot: bool,
    /// Highest `hLevelIndex` observed in a valid sample, ever.
    max_level: u32,
    /// Highest world nibble observed in a valid sample, ever.
    max_world: u32,
    /// Monotone rank on [`RANK_LADDER`].
    rank: u32,

    /// Transient: consecutive valid samples in the same level.
    stable: u64,
    /// Transient: level of the previous valid sample.
    level: Option<u32>,
    /// Transient: per-life latches.
    latches: Latches,
    /// Transient: last sampled column, lives and mode.
    column: u32,
    lives: u32,
    mode: String,
    safe: bool,
    game_over: bool,
    playable: bool,
}

/// What the stream page shows about reward history, as the Pokémon adapter's
/// `Statistics` does.
#[derive(Debug, Clone, PartialEq)]
pub struct Statistics {
    pub counts: BTreeMap<&'static str, u64>,
    pub total: f64,
    pub recent: Vec<RewardEvent>,
    pub last: Vec<(&'static str, RewardEvent)>,
    pub mode: String,
    /// Band keys earned, lifetime. The ratchet's coverage signal.
    pub bands: u64,
}

impl Default for PlatformerAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformerAdapter {
    /// An adapter with no ROM pin: it samples and reports, and pays nothing.
    pub fn new() -> Self {
        Self {
            rom_pin: None,
            ledger: OrderedSet::new(),
            bands: 0,
            coin_payouts: BTreeMap::new(),
            score_payouts: BTreeMap::new(),
            best_column: BTreeMap::new(),
            counts: Counts::default(),
            total: 0.0,
            recent: Vec::new(),
            last: LastEvents::default(),
            initialized: false,
            saw_boot: false,
            max_level: 0,
            max_world: 0,
            rank: 0,
            stable: 0,
            level: None,
            latches: Latches::default(),
            column: 0,
            lives: 0,
            mode: "BOOT".to_string(),
            safe: false,
            game_over: false,
            playable: false,
        }
    }

    /// Pin the cartridge semantic rewards are enabled for, from
    /// `[game.platformer] rom_sha256`. An empty or non-hex value is treated as
    /// no pin, so a misconfigured deployment runs with rewards visibly off
    /// rather than crediting an unknown ROM.
    pub fn with_rom_pin(pin: Option<&str>) -> Self {
        let mut adapter = Self::new();
        adapter.rom_pin = pin
            .map(str::trim)
            .filter(|pin| pin.len() == 64 && pin.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|pin| pin.to_ascii_lowercase());
        adapter
    }

    /// The pinned cartridge hash, if one was configured.
    pub fn rom_pin(&self) -> Option<&str> {
        self.rom_pin.as_deref()
    }

    /// Whether the last sample was a state a snapshot can be restored into.
    pub fn safe(&self) -> bool {
        self.safe
    }

    /// Whether the last sample saw a game over (§5, restore trigger A).
    pub fn game_over(&self) -> bool {
        self.game_over
    }

    /// Rank on [`RANK_LADDER`], 0 to 15.
    pub fn rank(&self) -> u32 {
        self.rank
    }

    /// Level index of the last valid sample, or `None` before one.
    pub fn level(&self) -> Option<u32> {
        self.level
    }

    /// Progress through the current level, 0.0 to 1.0, from the *camera*: the
    /// column loader runs up to one screen ahead of Mario (§8.2), and the stream
    /// must label it as camera progress.
    pub fn level_percent(&self) -> Option<f64> {
        let level = self.level?;
        let columns = symbols::level_columns(level)?;
        let ahead = self.column.saturating_sub(symbols::FIRST_COLUMN);
        Some((f64::from(ahead) / f64::from(columns)).clamp(0.0, 1.0))
    }

    /// Furthest column ever reached in `level`.
    pub fn best_column(&self, level: u32) -> Option<u32> {
        self.best_column.get(&level).copied()
    }

    pub fn statistics(&self) -> Statistics {
        Statistics {
            counts: self.counts.to_map(),
            total: self.total,
            recent: self.recent.clone(),
            last: self.last.iter().map(|(kind, event)| (kind, event.clone())).collect(),
            mode: self.mode.clone(),
            bands: self.bands,
        }
    }

    /// Forget observations a rollback invalidates. The lifetime ledger, the
    /// per-level payout counters, the rank and the totals all survive, so the
    /// ground a restore replays pays nothing on the way back (§5).
    pub fn clear_transient(&mut self) {
        self.stable = 0;
        self.level = None;
        self.latches = Latches::default();
        self.column = 0;
        self.safe = false;
        self.game_over = false;
        self.playable = false;
    }

    /// Sample WRAM and HRAM after one completed frame and return this frame's
    /// payouts.
    pub fn sample(&mut self, source: &mut dyn MemoryReader, brain_ms: f64) -> Vec<RewardEvent> {
        self.safe = false;
        self.game_over = false;
        self.playable = false;
        if self.rom_pin.is_none() {
            self.mode = "UNSUPPORTED ROM · SEMANTIC REWARDS OFF".to_string();
            self.stable = 0;
            return Vec::new();
        }
        let mut cache = SampleCache { source, bytes: HashMap::new() };
        let memory = &mut cache;

        let Some(sample) = self.gate(memory) else {
            return Vec::new();
        };
        self.playable = true;
        self.column = sample.column;
        self.lives = sample.lives;
        self.mode = format!("IN LEVEL {}-{}", sample.world, sample.stage);
        let best = self.best_column.entry(sample.level).or_insert(0);
        *best = (*best).max(sample.column);

        let mut emitted: Vec<RewardEvent> = Vec::new();
        let first = !self.initialized;
        if first {
            self.baseline(&sample, &mut emitted, brain_ms);
        }

        self.pay_bands(&sample, &mut emitted, brain_ms);
        self.pay_coins(&sample, &mut emitted, brain_ms);
        self.pay_score(&sample, &mut emitted, brain_ms);
        self.pay_powerups(&sample, &mut emitted, brain_ms);
        self.pay_lives(&sample, &mut emitted, brain_ms);
        self.pay_progress(&sample, &mut emitted, brain_ms);

        self.latches = Latches {
            coins: Some(sample.coins),
            score: Some(sample.score),
            lives: Some(sample.lives),
            super_status: Some(sample.super_status),
            superball: Some(sample.superball),
            wins: Some(sample.wins),
        };
        self.max_level = self.max_level.max(sample.level);
        self.max_world = self.max_world.max(sample.world);
        self.update_rank(&sample);
        self.safe = self.is_safe(&sample);

        // Newest first, capped at eight, as the Pokémon adapter does.
        let mut recent = emitted.clone();
        recent.append(&mut self.recent);
        recent.truncate(8);
        self.recent = recent;
        emitted
    }

    /// The playable gate (§2): every condition, every sample, before any payout.
    /// Returns the decoded sample only when all of them hold.
    fn gate(&mut self, memory: &mut impl MemoryReader) -> Option<Sample> {
        let game_state = memory.read8(hram::hGameState);
        let lives = u32::from(memory.read8(wram::wLives));
        let game_over_window = memory.read8(wram::wGameOverWindowEnabled);

        // Game over first: it is both a mode and the ratchet's restore trigger,
        // and it must not be reported as a transition.
        if game_state == symbols::GAME_STATE_GAME_OVER_TEXT
            || game_state == symbols::GAME_STATE_GAME_OVER_WAIT
            || (lives == 0 && game_over_window != 0)
        {
            self.mode = "GAME OVER".to_string();
            self.game_over = true;
            self.saw_boot = true;
            self.stable = 0;
            return None;
        }
        // Menus. `0x0E` initialises the menu, `0x0F` is the start menu.
        if game_state == 0x0e || game_state == 0x0f {
            self.mode = "BOOT".to_string();
            self.saw_boot = true;
            self.stable = 0;
            return None;
        }
        // The attract demo runs real gameplay states, so only this byte
        // distinguishes it. Without the gate the demo would farm rewards.
        if memory.read8(hram::UNNAMED_DEMO_GATE) != 0 {
            self.mode = "DEMO".to_string();
            self.saw_boot = true;
            self.stable = 0;
            return None;
        }
        let playable_state =
            game_state == symbols::GAME_STATE_NORMAL || game_state == symbols::GAME_STATE_AUTOSCROLL;
        if !playable_state
            || memory.read8(hram::hGamePaused) != 0
            || game_over_window != 0
            || lives == 0
        {
            self.mode = "TRANSITION".to_string();
            self.stable = 0;
            return None;
        }

        // Consistency: a mismatch is a mid-transition or a wrong-revision read.
        // Cheap, and it catches a wrong ROM immediately.
        let world_and_level = memory.read8(hram::hWorldAndLevel);
        let world = u32::from(world_and_level >> 4);
        let stage = u32::from(world_and_level & 0x0f);
        let level = u32::from(memory.read8(hram::hLevelIndex));
        let screen = u32::from(memory.read8(hram::hScreenIndex));
        let column_in_screen = u32::from(memory.read8(hram::hColumnIndex));
        let screens = symbols::screens(level);
        if !(1..=4).contains(&world)
            || !(1..=3).contains(&stage)
            || level >= symbols::LEVEL_COUNT
            || level != (world - 1) * 3 + (stage - 1)
            || screens.is_none_or(|screens| !(symbols::FIRST_SCREEN..=screens).contains(&screen))
            || column_in_screen >= symbols::COLUMNS_PER_SCREEN
        {
            self.mode = "TRANSITION".to_string();
            self.stable = 0;
            return None;
        }

        let score = bcd(&[
            memory.read8(wram::wScore),
            memory.read8(wram::wScore + 1),
            memory.read8(wram::wScore + 2),
        ]);
        let coins = bcd(&[memory.read8(hram::hCoins)]);
        let game_timer = bcd(&[
            memory.read8(wram::wGameTimer),
            memory.read8(wram::wGameTimer + 1),
            memory.read8(wram::wGameTimer + 2),
        ]);
        // A non-BCD nibble in any of the three is the same class of evidence as
        // an inconsistent level: something is mid-write or this is not Rev A.
        let (Some(score), Some(coins), Some(game_timer)) = (score, coins, game_timer) else {
            self.mode = "TRANSITION".to_string();
            self.stable = 0;
            return None;
        };

        self.stable = if self.level == Some(level) { self.stable + 1 } else { 1 };
        self.level = Some(level);
        if self.stable < STABLE_SAMPLES {
            self.mode = format!("IN LEVEL {world}-{stage}");
            return None;
        }

        Some(Sample {
            level,
            world,
            stage,
            column: screen * symbols::COLUMNS_PER_SCREEN + column_in_screen,
            coins,
            score,
            lives,
            super_status: memory.read8(hram::hSuperStatus),
            superball: memory.read8(hram::hSuperballMario),
            wins: u32::from(memory.read8(hram::hWinCount)),
            game_state,
            underground: memory.read8(hram::UNNAMED_UNDERGROUND) != 0,
            on_ground: memory.read8(wram::UNNAMED_ON_GROUND) == 1,
            jumping: memory.read8(wram::UNNAMED_JUMP_STATUS) != 0,
            invincible: memory.read8(wram::wInvincibilityTimer) != 0,
            game_timer,
            timer_expiring: memory.read8(wram::wGameTimerExpiringFlag) != 0,
        })
    }

    /// Record the first valid sample without paying for it, so a restored
    /// checkpoint mid-level replays nothing (§2, "Baselining").
    fn baseline(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        for band in self.band_keys(sample.level, sample.column) {
            self.remember(&band);
        }
        if sample.super_status >= 2 {
            self.remember(&format!("powerup:super:{}", sample.level));
        }
        if sample.superball != 0 {
            self.remember(&format!("powerup:ball:{}", sample.level));
        }
        for index in 0..=sample.level {
            self.remember(&format!("level:{index}"));
        }
        for world in 1..=sample.world {
            self.remember(&format!("world:{world}"));
        }
        if sample.wins > 0 {
            self.remember("clear");
        }
        if sample.column >= HALFWAY_1_1_COLUMN && sample.level == 0 {
            self.remember("halfway");
        }
        self.max_level = self.max_level.max(sample.level);
        self.max_world = self.max_world.max(sample.world);
        // `started` is the one payout a first sample can make, and only when a
        // boot state was seen first: a migrated save is not a new adventure.
        if self.saw_boot && !self.ledger.contains("started") {
            self.emit(emitted, kind::STARTED, "RUN STARTED".to_string(), 1.0, brain_ms);
        }
        self.remember("started");
        self.initialized = true;
    }

    /// Band keys from the start of the level up to `column`, clamped to the
    /// level's band cap of `2 * (screens - 3)`.
    fn band_keys(&self, level: u32, column: u32) -> Vec<String> {
        let Some(bands) = symbols::level_bands(level) else {
            return Vec::new();
        };
        let first = symbols::FIRST_COLUMN / symbols::BAND_COLUMNS;
        let reached = column / symbols::BAND_COLUMNS;
        (first..=reached.min(first + bands - 1))
            .map(|band| format!("band:{level}:{band}"))
            .collect()
    }

    /// Bands are keyed positions in a lifetime set, so walking back and forth
    /// across a boundary pays once, ever (§2).
    fn pay_bands(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        // Pipe sub-rooms reuse `hScreenIndex`, so their columns would alias onto
        // the main level's bands (§8.3). Safe v1: pay nothing underground.
        if sample.underground {
            return;
        }
        let Some(key) = self.band_keys(sample.level, sample.column).pop() else {
            return;
        };
        if self.ledger.contains(&key) {
            return;
        }
        self.remember(&key);
        let percent = symbols::level_columns(sample.level)
            .map(|columns| {
                f64::from(sample.column.saturating_sub(symbols::FIRST_COLUMN)) / f64::from(columns)
            })
            .unwrap_or(0.0);
        self.emit(
            emitted,
            kind::BAND,
            format!(
                "NEW GROUND {}-{} {}%",
                sample.world,
                sample.stage,
                (percent * 100.0).round() as u32
            ),
            1.0,
            brain_ms,
        );
    }

    fn pay_coins(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        let Some(previous) = self.latches.coins else {
            return;
        };
        // A mod-100 wrap is an increase of one, never a decrease: the counter is
        // two BCD digits and rolls over at 99.
        if sample.coins == previous {
            return;
        }
        let paid = self.coin_payouts.get(&sample.level).copied().unwrap_or(0);
        let cap = catalog::rule(kind::COIN).expect("coin is a catalog kind").per_level_cap;
        if cap.is_some_and(|cap| paid >= cap) {
            return;
        }
        self.coin_payouts.insert(sample.level, paid + 1);
        self.emit(emitted, kind::COIN, "COIN".to_string(), 1.0, brain_ms);
    }

    fn pay_score(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        let Some(previous) = self.latches.score else {
            return;
        };
        if sample.score <= previous {
            return;
        }
        let paid = self.score_payouts.get(&sample.level).copied().unwrap_or(0);
        let cap = catalog::rule(kind::SCORE).expect("score is a catalog kind").per_level_cap;
        if cap.is_some_and(|cap| paid >= cap) {
            return;
        }
        let delta = f64::from(sample.score - previous);
        let size = (delta / catalog::SCORE_FULL_DELTA).min(1.0);
        let decay = 1.0 + (paid / catalog::SCORE_DECAY_STEP) as f64;
        self.score_payouts.insert(sample.level, paid + 1);
        self.emit(
            emitted,
            kind::SCORE,
            format!("+{}", sample.score - previous),
            size / decay,
            brain_ms,
        );
    }

    fn pay_powerups(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        // `hSuperStatus` 3 and up is injury i-frames, so only "reached 2" is a
        // power-up: 2 -> 3 and 3 -> 2 pay nothing.
        if sample.super_status == 2 {
            let key = format!("powerup:super:{}", sample.level);
            if !self.ledger.contains(&key) {
                self.remember(&key);
                self.emit(
                    emitted,
                    kind::POWERUP,
                    "SUPER MARIO".to_string(),
                    1.0,
                    brain_ms,
                );
            }
        }
        if sample.superball != 0 {
            let key = format!("powerup:ball:{}", sample.level);
            if !self.ledger.contains(&key) {
                self.remember(&key);
                self.emit(emitted, kind::POWERUP, "SUPERBALL".to_string(), 1.0, brain_ms);
            }
        }
    }

    fn pay_lives(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        if self.latches.lives.is_some_and(|previous| sample.lives > previous) {
            self.emit(emitted, kind::LIFE, "1UP".to_string(), 1.0, brain_ms);
        }
    }

    /// Level, world and game clear. All three are lifetime-keyed, so replaying a
    /// level after a rollback pays nothing.
    fn pay_progress(&mut self, sample: &Sample, emitted: &mut Vec<RewardEvent>, brain_ms: f64) {
        if sample.level > self.max_level {
            let key = format!("level:{}", sample.level);
            if !self.ledger.contains(&key) {
                self.remember(&key);
                self.emit(
                    emitted,
                    kind::LEVEL,
                    format!("LEVEL CLEARED: {}-{} REACHED", sample.world, sample.stage),
                    1.0,
                    brain_ms,
                );
            }
        }
        if sample.world > self.max_world {
            let key = format!("world:{}", sample.world);
            if !self.ledger.contains(&key) {
                self.remember(&key);
                self.emit(
                    emitted,
                    kind::WORLD,
                    format!("WORLD CLEARED: WORLD {} REACHED", sample.world),
                    1.0,
                    brain_ms,
                );
            }
        }
        let rose = self.latches.wins.is_some_and(|previous| sample.wins > previous);
        if rose && !self.ledger.contains("clear") {
            self.remember("clear");
            self.emit(emitted, kind::CLEAR, "GAME CLEARED".to_string(), 1.0, brain_ms);
        }
    }

    /// Ranks are monotone: the ladder never falls back, because a rank is
    /// supposed to name a state worth archiving (§3).
    fn update_rank(&mut self, sample: &Sample) {
        let mut rank = 1;
        if self.counts.get(kind::COIN) > 0 {
            rank = 2;
        }
        if sample.level == 0 && sample.column >= HALFWAY_1_1_COLUMN {
            self.remember("halfway");
        }
        if self.ledger.contains("halfway") {
            rank = 3;
        }
        if self.max_level >= 1 {
            // 4 + highestClearedLevelIndex, and reaching level index n means
            // level n-1 was cleared.
            rank = 4 + (self.max_level - 1);
        }
        if self.ledger.contains("clear") {
            rank = (RANK_LADDER.len() - 1) as u32;
        }
        self.rank = self.rank.max(rank.min((RANK_LADDER.len() - 1) as u32));
    }

    /// The ratchet's safe-observation gate (§2, "Safe snapshot"): everything in
    /// the playable gate, plus a grounded, un-invincible, not-growing Mario in a
    /// non-autoscrolling level with time on the clock.
    fn is_safe(&self, sample: &Sample) -> bool {
        sample.game_state == symbols::GAME_STATE_NORMAL
            && !sample.underground
            && sample.on_ground
            && !sample.jumping
            && !sample.invincible
            && (sample.super_status == 0 || sample.super_status == 2)
            && sample.game_timer >= SAFE_MIN_GAME_TIMER
            && !sample.timer_expiring
            && self.stable >= STABLE_SAMPLES
    }

    /// Insert a ledger key, keeping the band counter in step.
    fn remember(&mut self, key: &str) {
        if self.ledger.insert(key) && key.starts_with("band:") {
            self.bands += 1;
        }
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
        let event = RewardEvent {
            kind,
            label,
            brain_ms,
            value: rule.value * scale,
            stimulation_ms: rule.stimulation_ms,
        };
        emitted.push(event.clone());
        self.counts.bump(kind);
        self.total += event.value;
        self.last.set(event);
    }

    pub fn export_state(&self) -> Value {
        json!({
            "version": STATE_VERSION,
            "adapter": ADAPTER,
            "ledger": self.ledger.as_slice(),
            "coinPayouts": level_object(&self.coin_payouts),
            "scorePayouts": level_object(&self.score_payouts),
            "bestColumn": level_object(&self.best_column),
            "counts": self.counts,
            "total": self.total,
            "recent": self.recent,
            "last": self.last,
            "initialized": self.initialized,
            "sawBoot": self.saw_boot,
            "maxLevel": self.max_level,
            "maxWorld": self.max_world,
            "rank": self.rank,
            "mode": self.mode,
        })
    }

    /// Restore lifetime history from [`PlatformerAdapter::export_state`].
    ///
    /// A state whose `version` is not [`STATE_VERSION`] is ignored and the
    /// adapter rebaselines at its next valid sample, because old reward
    /// semantics cannot be credited under the current rules. Malformed
    /// current-version state is an error, and nothing is assigned until every
    /// field has validated, so a failed restore leaves the adapter untouched.
    pub fn import_state(&mut self, input: &Value) -> Result<(), AdapterError> {
        if input.get("version").and_then(Value::as_u64) != Some(STATE_VERSION) {
            return Ok(());
        }
        const BAD_CHECKPOINT: AdapterError = AdapterError("Invalid reward checkpoint");
        const BAD_HISTORY: AdapterError = AdapterError("Invalid reward history");

        let ledger = string_array(input.get("ledger")).ok_or(BAD_CHECKPOINT)?;
        let total = input.get("total").and_then(Value::as_f64).ok_or(BAD_CHECKPOINT)?;
        if !total.is_finite() {
            return Err(BAD_CHECKPOINT);
        }
        let initialized =
            input.get("initialized").and_then(Value::as_bool).ok_or(BAD_CHECKPOINT)?;
        let saw_boot = input.get("sawBoot").and_then(Value::as_bool).ok_or(BAD_CHECKPOINT)?;
        let mode = input.get("mode").and_then(Value::as_str).ok_or(BAD_CHECKPOINT)?;
        let max_level = counter(input.get("maxLevel")).ok_or(BAD_CHECKPOINT)?;
        let max_world = counter(input.get("maxWorld")).ok_or(BAD_CHECKPOINT)?;
        let rank = counter(input.get("rank")).ok_or(BAD_CHECKPOINT)?;
        if max_level >= symbols::LEVEL_COUNT
            || max_world > 4
            || rank >= RANK_LADDER.len() as u32
        {
            return Err(BAD_CHECKPOINT);
        }
        let coin_payouts = level_keyed(input.get("coinPayouts")).ok_or(BAD_CHECKPOINT)?;
        let score_payouts = level_keyed(input.get("scorePayouts")).ok_or(BAD_CHECKPOINT)?;
        let best_column = level_keyed(input.get("bestColumn"))
            .ok_or(BAD_CHECKPOINT)?
            .into_iter()
            .map(|(level, column)| (level, column as u32))
            .collect();
        let counts_raw = counted_record(input.get("counts")).ok_or(BAD_CHECKPOINT)?;

        let recent = input
            .get("recent")
            .and_then(Value::as_array)
            .ok_or(BAD_CHECKPOINT)?
            .iter()
            .map(parse_event)
            .collect::<Option<Vec<_>>>()
            .ok_or(BAD_HISTORY)?;
        let mut last = LastEvents::default();
        for (key, value) in input.get("last").and_then(Value::as_object).ok_or(BAD_HISTORY)?.iter()
        {
            let event = parse_event(value).ok_or(BAD_HISTORY)?;
            if catalog::index(key).is_some() {
                last.set(event);
            }
        }

        self.ledger = ledger.iter().map(String::as_str).collect();
        self.bands = self
            .ledger
            .as_slice()
            .iter()
            .filter(|key| key.starts_with("band:"))
            .count() as u64;
        self.coin_payouts = coin_payouts;
        self.score_payouts = score_payouts;
        self.best_column = best_column;
        self.counts = Counts::default();
        for (key, count) in &counts_raw {
            self.counts.set(key, *count);
        }
        self.total = total;
        self.recent = recent;
        self.last = last;
        self.initialized = initialized;
        self.saw_boot = saw_boot;
        self.max_level = max_level;
        self.max_world = max_world;
        self.rank = rank;
        self.mode = mode.to_string();
        // Everything a rollback would invalidate starts empty: the restored run
        // re-baselines its latches at the next valid sample without paying.
        self.clear_transient();
        Ok(())
    }
}

/// A level-keyed counter map as a JSON object: JSON keys are strings, the values
/// stay numbers.
fn level_object(map: &BTreeMap<u32, impl Copy + Into<u64>>) -> BTreeMap<String, u64> {
    map.iter().map(|(level, value)| (level.to_string(), (*value).into())).collect()
}

/// An array whose every element is a string, or `None`.
fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let array = value?.as_array()?;
    array.iter().map(|item| item.as_str().map(str::to_string)).collect()
}

/// `Number.MAX_SAFE_INTEGER`, so a checkpoint no JavaScript build could have
/// written is rejected here too.
const MAX_SAFE_INTEGER: u64 = (1u64 << 53) - 1;

fn counter(value: Option<&Value>) -> Option<u32> {
    u32::try_from(value?.as_u64()?).ok()
}

/// An object whose every value is a non-negative safe integer, or `None`.
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

/// A `{"<level>": <count>}` object, where every key is a level index and every
/// value a non-negative safe integer. The inverse of [`level_object`].
fn level_keyed(value: Option<&Value>) -> Option<BTreeMap<u32, u64>> {
    let object = value?.as_object()?;
    object
        .iter()
        .map(|(key, value)| {
            let level: u32 = key.parse().ok()?;
            if level >= symbols::LEVEL_COUNT {
                return None;
            }
            let count = value.as_u64()?;
            (count <= MAX_SAFE_INTEGER).then_some((level, count))
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

impl GameAdapter for PlatformerAdapter {
    fn id(&self) -> &'static str {
        ADAPTER
    }

    /// Only the configured cartridge. With no pin nothing is allowed, so a
    /// deployment that forgot `[game.platformer] rom_sha256` runs with semantic
    /// rewards visibly off instead of crediting an unknown ROM.
    fn rom_allowed(&self, sha256: &str) -> bool {
        self.rom_pin.as_deref() == Some(sha256.to_ascii_lowercase().as_str())
    }

    fn sample(&mut self, memory: &mut dyn MemoryReader, ms: f64) -> Vec<RewardEvent> {
        PlatformerAdapter::sample(self, memory, ms)
    }

    fn mode(&self) -> &str {
        &self.mode
    }

    /// The level index, which is what `game.map` means for this game.
    fn map_id(&self) -> Option<u32> {
        self.level
    }

    /// Every non-playable state is boot, so Start and Select keep their
    /// permissive variant on the title screen, in the attract demo and after a
    /// game over — the only moments the fly needs them (§4).
    fn boot(&self) -> bool {
        !self.playable
    }

    fn progress(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            rank: self.rank,
            rank_max: RANK_LADDER.len() as u32 - 1,
            rank_label: RANK_LADDER[(self.rank as usize).min(RANK_LADDER.len() - 1)],
            counter: self.lives,
            counter_label: "LIVES",
            unique_locations: self.bands as usize,
            reward_total: self.total,
            counts: self.counts.to_map(),
        }
    }

    fn safe_for_snapshot(&self) -> bool {
        self.safe
    }

    fn game_over(&self) -> bool {
        self.game_over
    }

    fn recovery_policy(&self) -> RecoveryPolicy {
        RECOVERY_POLICY
    }

    /// The disassembly the RAM map came from, plus the pinned cartridge. Folding
    /// the hash in means a checkpoint earned on one ROM revision cannot be loaded
    /// under another's semantics (§8.5); the decoder preset needs no segment of
    /// its own, because [`ADAPTER`] selects it one to one.
    fn symbol_provenance(&self) -> String {
        format!(
            "sml:{}+rom:{}",
            symbols::SML_DISASSEMBLY,
            self.rom_pin.as_deref().unwrap_or("unpinned")
        )
    }

    fn rank_ladder(&self) -> &'static [&'static str] {
        &RANK_LADDER
    }

    fn decoder_preset(&self) -> DecoderPresetId {
        DecoderPresetId::Platformer
    }

    fn clear_transient(&mut self) {
        PlatformerAdapter::clear_transient(self);
    }

    fn export_state(&self) -> Value {
        PlatformerAdapter::export_state(self)
    }

    fn import_state(&mut self, state: &Value) -> Result<(), AdapterError> {
        PlatformerAdapter::import_state(self, state)
    }
}

#[cfg(test)]
mod tests;
