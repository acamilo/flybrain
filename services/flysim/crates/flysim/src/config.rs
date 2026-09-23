//! Configuration: `flysim.toml` plus environment overrides.
//!
//! The file is optional. `infra/units/flysim.service` starts the binary with no `--config`
//! argument at all and supplies everything through the environment, so every field has a default
//! and every field an environment override.
//!
//! Two override families are accepted, and the environment always wins over the file:
//!
//! - the `FLY_*` names the systemd units already set (`FLY_GAME`, `FLY_ROM`, `FLY_DATASET`,
//!   `FLY_STATE`, `FLY_STATE_HOT`, `FLY_FEED_BIND`, `FLY_CONTROL_BIND`, `FLY_METRICS_ADDR`,
//!   `FLY_ROM_SHA256`, `FLY_ROM_PLATFORMER_SHA256`, `FLY_CHAT_ENABLED`, `FLY_CHAT_DENY_LIST`,
//!   `FLY_MACRO_MODE`, `FLY_FEED_VIA`, `FLY_BUS_DIR`, `RAYON_NUM_THREADS`);
//! - `FLYSIM_<SECTION>_<KEY>` for everything, e.g. `FLYSIM_LOOP_SPEED=0`.
//!
//! Nothing here is secret (`docs/control-api.md`: "No secrets live in this service or its
//! config"), so the resolved configuration is logged in full at INFO.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub use crate::snapshot::MacroMode;

/// Milliseconds of network time per emulator frame, from `flybrain-core`.
pub use flybrain_core::agent::GAMEBOY_MS_PER_FRAME;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Config {
    pub paths: Paths,
    #[serde(rename = "loop")]
    pub loop_: Loop,
    pub feed: Feed,
    pub control: Control,
    pub chat: Chat,
    pub macros: Macros,
    pub game: Games,
}

/// Per-game settings, keyed by the adapter id `loop.game` selects.
///
/// Pokémon Red needs none: its ROM pin is a build constant in `flybrain-gb`. The platformer's is
/// not, because `docs/design/platformer.md` §8.5 records only the SHA-1 of the revision its RAM map
/// describes, so the SHA-256 has to come from the deployment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Games {
    pub platformer: Platformer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Platformer {
    /// SHA-256 of the one cartridge the platformer adapter enables semantic rewards for. Unset, or
    /// anything that is not 64 hex digits, leaves them off: the stream still runs, with
    /// `SEMANTIC REWARDS OFF` on screen, rather than crediting an unknown ROM.
    pub rom_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// Game Boy cartridge image.
    pub rom: PathBuf,
    /// Connectome directory (`meta.json` plus the `.binz` artifacts).
    pub dataset: PathBuf,
    /// Durable checkpoints, the event log and `manifest.json`.
    pub save_dir: PathBuf,
    /// tmpfs copy of the same store, written far more often (SSD endurance).
    pub hot_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Loop {
    /// Adapter id: `pokemon-red` or `platformer`.
    pub game: String,
    /// Target realtime factor. `0` means unthrottled (soak tests); otherwise 0.25..=8.
    pub speed: f64,
    /// Neuron-sweep worker threads. `0` keeps the sweep sequential, which is the fastest
    /// setting measured so far (see `crates/flybrain-core/README.md`, "Performance").
    pub threads: usize,
    /// Snapshot publish rate while running.
    pub snapshot_hz: f64,
    /// Snapshot publish rate while paused, booting or recovering.
    pub idle_snapshot_hz: f64,
    /// Durable checkpoint interval, seconds.
    pub checkpoint_seconds: f64,
    /// tmpfs checkpoint interval, seconds.
    pub hot_seconds: f64,
    /// Warm-up length on a fresh start. Never applied after a restore.
    pub warmup_ms: u64,
    /// Durable generations kept besides the milestone archives.
    pub keep_generations: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Feed {
    pub bind: SocketAddr,
    /// Audio attachment sample rate. The page wants Web Audio's native 48 kHz.
    pub audio_hz: u32,
    /// Who serves `:7400/feed` (`docs/design/flybus.md`, "Feed over the bus").
    pub via: FeedVia,
    /// The bus runtime directory in `bus` mode: the router's socket and its artifact store.
    /// Belongs on tmpfs; a store here holds a few snapshots, never history.
    pub bus_dir: PathBuf,
}

/// Where the feed WebSocket is served from.
///
/// `direct` is the default and is the behaviour that predates the bus, byte for byte: flysim
/// binds `feed.bind` itself. `bus` starts an embedded flybus router, publishes every snapshot
/// on it, and leaves `feed.bind` to the `fly-edge` process. The control API stays in flysim
/// either way. Nothing about the fly changes with this knob: it is outside the simulation loop
/// and outside the compatibility string.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedVia {
    #[default]
    Direct,
    Bus,
}

impl FeedVia {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Bus => "bus",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Control {
    pub bind: SocketAddr,
    /// Optional extra read-only listener carrying `/metrics`, `/status`, `/status.json` and
    /// `/healthz` only (`infra/units/flysim.service` sets `FLY_METRICS_ADDR=0.0.0.0:9101`).
    pub metrics_bind: Option<SocketAddr>,
    /// `POST /reward` returns 403 while this is false (`docs/control-api.md`).
    pub allow_reward: bool,
    pub sugar_default_ms: f64,
    pub sugar_max_ms: f64,
    pub sugar_per_minute: usize,
    /// SHA-256 the ROM is expected to have. Only logged; the adapter decides whether semantic
    /// rewards are enabled, and an unexpected cartridge runs with them off rather than failing.
    pub expect_rom_sha256: Option<String>,
}

/// `[macros]` (`docs/design/macros.md` sections 1 and 4): what the fly's channels mean.
///
/// One knob, and deliberately only one. `raw` is the default and stays the default until the
/// measurement in section 7 says otherwise; macros mode is a config and environment setting,
/// never a chat command and never a control-API call, because "the fly chooses the action" is a
/// property of the deployment rather than something a viewer can flip.
///
/// A game with no palette (`flybrain_gb::palette_for`) cannot run in macros mode: the
/// configuration is refused at startup rather than silently downgraded, so a unit that asks for
/// macros over the platformer fails loudly instead of streaming raw under a `PALETTE` chip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Macros {
    pub mode: MacroMode,
}

/// `[chat]` (`docs/control-api.md`): the on-screen chat ring and its kill switch.
///
/// The per-name (1 per 2 s) and global (5 per s) admission limits are not configurable: they are
/// constants in `crate::chat`, because the contract states them as numbers rather than as knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chat {
    /// False makes `POST /chat` answer 403 and the feed header omit `chat` entirely.
    pub enabled: bool,
    /// Lines carried in the header, 1 to `crate::chat::RING_MAX`.
    pub ring: usize,
    /// Operator-maintained deny list: one pattern per line, `#` comments. Reloaded on SIGHUP and
    /// at most once a minute anyway. A missing file is an empty list, not a startup failure.
    pub deny_list: Option<PathBuf>,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            enabled: true,
            ring: crate::chat::RING_MAX,
            deny_list: Some(PathBuf::from("/srv/fly/chat-deny.txt")),
        }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            rom: PathBuf::from("/srv/fly/rom/pokemon-red.gb"),
            dataset: PathBuf::from("/srv/fly/data/fafb-v783"),
            save_dir: PathBuf::from("/srv/fly/state"),
            hot_dir: PathBuf::from("/run/fly/state"),
        }
    }
}

impl Default for Loop {
    fn default() -> Self {
        Self {
            game: "pokemon-red".to_string(),
            speed: 1.0,
            threads: 0,
            snapshot_hz: 30.0,
            idle_snapshot_hz: 2.0,
            checkpoint_seconds: 300.0,
            hot_seconds: 5.0,
            warmup_ms: flybrain_core::agent::DEFAULT_WARMUP_MS,
            keep_generations: 2,
        }
    }
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7400".parse().expect("literal address"),
            audio_hz: 48_000,
            via: FeedVia::Direct,
            bus_dir: PathBuf::from("/run/fly/bus"),
        }
    }
}

impl Default for Control {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7401".parse().expect("literal address"),
            metrics_bind: None,
            allow_reward: false,
            sugar_default_ms: 400.0,
            sugar_max_ms: 1000.0,
            sugar_per_minute: 6,
            expect_rom_sha256: None,
        }
    }
}

impl Config {
    /// Read `path` if given, then apply environment overrides, then validate.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading config {}", path.display()))?;
                toml::from_str(&text)
                    .with_context(|| format!("parsing config {}", path.display()))?
            }
            None => Self::default(),
        };
        config.apply_env(&std::env::vars().collect())?;
        config.validate()?;
        Ok(config)
    }

    /// Environment overrides, taken from an explicit map so this is testable.
    pub fn apply_env(&mut self, env: &BTreeMap<String, String>) -> Result<()> {
        let get = |key: &str| env.get(key).map(String::as_str).filter(|v| !v.is_empty());

        // The names the systemd units already use.
        if let Some(value) = get("FLY_GAME") {
            self.loop_.game = value.to_string();
        }
        if let Some(value) = get("FLY_ROM") {
            self.paths.rom = PathBuf::from(value);
        }
        if let Some(value) = get("FLY_DATASET") {
            self.paths.dataset = PathBuf::from(value);
        }
        if let Some(value) = get("FLY_STATE") {
            self.paths.save_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLY_STATE_HOT") {
            self.paths.hot_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLY_FEED_BIND") {
            self.feed.bind = parse_addr("FLY_FEED_BIND", value)?;
        }
        if let Some(value) = get("FLY_FEED_VIA") {
            self.feed.via = parse_feed_via("FLY_FEED_VIA", value)?;
        }
        if let Some(value) = get("FLY_BUS_DIR") {
            self.feed.bus_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLY_CONTROL_BIND") {
            self.control.bind = parse_addr("FLY_CONTROL_BIND", value)?;
        }
        if let Some(value) = get("FLY_METRICS_ADDR") {
            self.control.metrics_bind = Some(parse_addr("FLY_METRICS_ADDR", value)?);
        }
        if let Some(value) = get("FLY_ROM_SHA256") {
            self.control.expect_rom_sha256 = Some(value.to_ascii_lowercase());
        }
        if let Some(value) = get("FLY_ROM_PLATFORMER_SHA256") {
            self.game.platformer.rom_sha256 = Some(value.to_ascii_lowercase());
        }
        if let Some(value) = get("RAYON_NUM_THREADS") {
            self.loop_.threads = parse("RAYON_NUM_THREADS", value)?;
        }
        // The chat kill switch and the deny-list path are what `infra/env/example.env` sets, so they
        // get FLY_* names alongside the FLYSIM_CHAT_* family below.
        if let Some(value) = get("FLY_CHAT_ENABLED") {
            self.chat.enabled = parse_bool("FLY_CHAT_ENABLED", value)?;
        }
        if let Some(value) = get("FLY_CHAT_DENY_LIST") {
            self.chat.deny_list = Some(PathBuf::from(value));
        }
        // The macro mode gets a FLY_* name because `infra/env/example.env` is where a box's mode is
        // set, and the dev box has to be able to differ from the release box without a file edit.
        if let Some(value) = get("FLY_MACRO_MODE") {
            self.macros.mode = parse_macro_mode("FLY_MACRO_MODE", value)?;
        }

        // FLYSIM_<SECTION>_<KEY>, one per field.
        if let Some(value) = get("FLYSIM_PATHS_ROM") {
            self.paths.rom = PathBuf::from(value);
        }
        if let Some(value) = get("FLYSIM_PATHS_DATASET") {
            self.paths.dataset = PathBuf::from(value);
        }
        if let Some(value) = get("FLYSIM_PATHS_SAVE_DIR") {
            self.paths.save_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLYSIM_PATHS_HOT_DIR") {
            self.paths.hot_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLYSIM_LOOP_GAME") {
            self.loop_.game = value.to_string();
        }
        if let Some(value) = get("FLYSIM_LOOP_SPEED") {
            self.loop_.speed = parse("FLYSIM_LOOP_SPEED", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_THREADS") {
            self.loop_.threads = parse("FLYSIM_LOOP_THREADS", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_SNAPSHOT_HZ") {
            self.loop_.snapshot_hz = parse("FLYSIM_LOOP_SNAPSHOT_HZ", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_IDLE_SNAPSHOT_HZ") {
            self.loop_.idle_snapshot_hz = parse("FLYSIM_LOOP_IDLE_SNAPSHOT_HZ", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_CHECKPOINT_SECONDS") {
            self.loop_.checkpoint_seconds = parse("FLYSIM_LOOP_CHECKPOINT_SECONDS", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_HOT_SECONDS") {
            self.loop_.hot_seconds = parse("FLYSIM_LOOP_HOT_SECONDS", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_WARMUP_MS") {
            self.loop_.warmup_ms = parse("FLYSIM_LOOP_WARMUP_MS", value)?;
        }
        if let Some(value) = get("FLYSIM_LOOP_KEEP_GENERATIONS") {
            self.loop_.keep_generations = parse("FLYSIM_LOOP_KEEP_GENERATIONS", value)?;
        }
        if let Some(value) = get("FLYSIM_FEED_BIND") {
            self.feed.bind = parse_addr("FLYSIM_FEED_BIND", value)?;
        }
        if let Some(value) = get("FLYSIM_FEED_AUDIO_HZ") {
            self.feed.audio_hz = parse("FLYSIM_FEED_AUDIO_HZ", value)?;
        }
        if let Some(value) = get("FLYSIM_FEED_VIA") {
            self.feed.via = parse_feed_via("FLYSIM_FEED_VIA", value)?;
        }
        if let Some(value) = get("FLYSIM_FEED_BUS_DIR") {
            self.feed.bus_dir = PathBuf::from(value);
        }
        if let Some(value) = get("FLYSIM_CONTROL_BIND") {
            self.control.bind = parse_addr("FLYSIM_CONTROL_BIND", value)?;
        }
        if let Some(value) = get("FLYSIM_CONTROL_METRICS_BIND") {
            self.control.metrics_bind = Some(parse_addr("FLYSIM_CONTROL_METRICS_BIND", value)?);
        }
        if let Some(value) = get("FLYSIM_CONTROL_ALLOW_REWARD") {
            self.control.allow_reward = parse_bool("FLYSIM_CONTROL_ALLOW_REWARD", value)?;
        }
        if let Some(value) = get("FLYSIM_CONTROL_SUGAR_DEFAULT_MS") {
            self.control.sugar_default_ms = parse("FLYSIM_CONTROL_SUGAR_DEFAULT_MS", value)?;
        }
        if let Some(value) = get("FLYSIM_CONTROL_SUGAR_MAX_MS") {
            self.control.sugar_max_ms = parse("FLYSIM_CONTROL_SUGAR_MAX_MS", value)?;
        }
        if let Some(value) = get("FLYSIM_CONTROL_SUGAR_PER_MINUTE") {
            self.control.sugar_per_minute = parse("FLYSIM_CONTROL_SUGAR_PER_MINUTE", value)?;
        }
        if let Some(value) = get("FLYSIM_CHAT_ENABLED") {
            self.chat.enabled = parse_bool("FLYSIM_CHAT_ENABLED", value)?;
        }
        if let Some(value) = get("FLYSIM_CHAT_RING") {
            self.chat.ring = parse("FLYSIM_CHAT_RING", value)?;
        }
        if let Some(value) = get("FLYSIM_CHAT_DENY_LIST") {
            // The empty string is already filtered out by `get`, so "none" is the way to say
            // "run with no deny list at all" from the environment.
            self.chat.deny_list = if value.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(PathBuf::from(value))
            };
        }
        if let Some(value) = get("FLYSIM_MACROS_MODE") {
            self.macros.mode = parse_macro_mode("FLYSIM_MACROS_MODE", value)?;
        }
        if let Some(value) = get("FLYSIM_GAME_PLATFORMER_ROM_SHA256") {
            self.game.platformer.rom_sha256 = Some(value.to_ascii_lowercase());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if flybrain_gb::adapter_for(&self.loop_.game).is_none() {
            bail!(
                "unknown game {:?}; expected one of {:?}",
                self.loop_.game,
                flybrain_gb::adapter::GAME_IDS
            );
        }
        let speed = self.loop_.speed;
        if !speed.is_finite() || speed < 0.0 || (speed != 0.0 && !(0.25..=8.0).contains(&speed)) {
            bail!("loop.speed must be 0 (unthrottled) or between 0.25 and 8, got {speed}");
        }
        for (name, hz) in [
            ("loop.snapshot_hz", self.loop_.snapshot_hz),
            ("loop.idle_snapshot_hz", self.loop_.idle_snapshot_hz),
        ] {
            if !hz.is_finite() || hz <= 0.0 || hz > 240.0 {
                bail!("{name} must be in (0, 240], got {hz}");
            }
        }
        for (name, seconds) in [
            ("loop.checkpoint_seconds", self.loop_.checkpoint_seconds),
            ("loop.hot_seconds", self.loop_.hot_seconds),
        ] {
            if !seconds.is_finite() || seconds <= 0.0 {
                bail!("{name} must be positive, got {seconds}");
            }
        }
        if self.loop_.keep_generations < 1 {
            bail!("loop.keep_generations must be at least 1");
        }
        if self.feed.audio_hz < 8_000 || self.feed.audio_hz > 192_000 {
            bail!("feed.audio_hz must be between 8000 and 192000");
        }
        let (default_ms, max_ms) = (self.control.sugar_default_ms, self.control.sugar_max_ms);
        if !default_ms.is_finite() || default_ms <= 0.0 {
            bail!("control.sugar_default_ms must be positive");
        }
        if !max_ms.is_finite() || max_ms < default_ms {
            bail!("control.sugar_max_ms must be at least control.sugar_default_ms");
        }
        if self.control.sugar_per_minute == 0 {
            bail!("control.sugar_per_minute must be at least 1");
        }
        // The router's socket and store, and the edge's way to them. A relative path would
        // resolve against whichever working directory each process happens to have, so the two
        // could silently disagree; an empty one is a typo. Checked in either mode, so a bad
        // value is found before the day a box is switched to the bus.
        if self.feed.bus_dir.as_os_str().is_empty() || !self.feed.bus_dir.is_absolute() {
            bail!(
                "feed.bus_dir (FLY_BUS_DIR) must be an absolute path, got {:?}",
                self.feed.bus_dir
            );
        }
        if self.feed.bind == self.control.bind {
            bail!("feed.bind and control.bind must differ (7400 and 7401)");
        }
        if !(1..=crate::chat::RING_MAX).contains(&self.chat.ring) {
            bail!(
                "chat.ring must be between 1 and {} (the feed header schema's own ceiling), got {}",
                crate::chat::RING_MAX,
                self.chat.ring
            );
        }
        if let Some(mode) = self.macros.mode.palette_mode()
            && flybrain_gb::palette_for(&self.loop_.game, 1, mode).is_none()
        {
            bail!(
                "macros.mode = {:?} but {:?} has no macro palette; \
                 only pokemon-red does (docs/design/macros.md section 2)",
                self.macros.mode.as_str(),
                self.loop_.game
            );
        }
        // A malformed pin is a typo, not a policy: an unset pin means "no semantic rewards", which
        // is a deliberate state, while `rom_sha256 = "abc"` is a mistake worth failing on.
        if let Some(pin) = &self.game.platformer.rom_sha256
            && (pin.len() != 64 || !pin.chars().all(|c| c.is_ascii_hexdigit()))
        {
            bail!("game.platformer.rom_sha256 must be 64 hex digits, got {pin:?}");
        }
        Ok(())
    }

    /// The configured ROM pin for the selected game, or `None` for a game whose pin is a build
    /// constant (Pokémon Red) or that has none configured.
    pub fn rom_pin(&self) -> Option<&str> {
        match self.loop_.game.as_str() {
            "platformer" => self.game.platformer.rom_sha256.as_deref(),
            _ => None,
        }
    }

    /// Publish period while running, and while not.
    pub fn publish_periods(&self) -> (std::time::Duration, std::time::Duration) {
        (
            std::time::Duration::from_secs_f64(1.0 / self.loop_.snapshot_hz),
            std::time::Duration::from_secs_f64(1.0 / self.loop_.idle_snapshot_hz),
        )
    }
}

fn parse_feed_via(name: &str, value: &str) -> Result<FeedVia> {
    match value.to_ascii_lowercase().as_str() {
        "direct" => Ok(FeedVia::Direct),
        "bus" => Ok(FeedVia::Bus),
        _ => bail!("{name}: {value:?} is not a feed path; expected \"direct\" or \"bus\""),
    }
}

fn parse_addr(name: &str, value: &str) -> Result<SocketAddr> {
    value
        .parse()
        .with_context(|| format!("{name}: {value:?} is not a host:port address"))
}

fn parse<T: std::str::FromStr>(name: &str, value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("{name}: {value:?} is not valid ({error})"))
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => bail!("{name}: {other:?} is not a boolean"),
    }
}

/// `raw` or `macros`, and nothing else.
///
/// Deliberately not `parse_bool`-shaped: a typo must not read as "off". An unrecognised value
/// fails startup rather than falling back to raw, because a box configured for macros that
/// quietly streams raw is the harder failure to notice.
///
/// `palette` and `plan` are the two modes `docs/design/macros.md` section 12 replaced. They are
/// accepted for one release and mean `macros`, with a warning, because the env files on the dev
/// box and in `infra/env/` carry them and a deploy that failed to start on an old value would be
/// a black stream for a renamed knob.
fn parse_macro_mode(name: &str, value: &str) -> Result<MacroMode> {
    match value.to_ascii_lowercase().as_str() {
        "raw" => Ok(MacroMode::Raw),
        "macros" => Ok(MacroMode::Macros),
        legacy @ ("palette" | "plan") => {
            tracing::warn!(
                name,
                value = legacy,
                "macro mode {legacy:?} was replaced by \"macros\" \
                 (docs/design/macros.md section 12); reading it as \"macros\" for this release"
            );
            Ok(MacroMode::Macros)
        }
        other => bail!("{name}: {other:?} is not a macro mode (raw or macros)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn the_defaults_are_the_documented_ports_and_paths() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.feed.bind.to_string(), "127.0.0.1:7400");
        assert_eq!(config.control.bind.to_string(), "127.0.0.1:7401");
        assert_eq!(config.loop_.checkpoint_seconds, 300.0);
        assert_eq!(config.loop_.hot_seconds, 5.0);
        assert!(!config.control.allow_reward);
        assert_eq!(config.loop_.warmup_ms, 2500);
        assert!(config.chat.enabled, "chat is on by default; the kill switch is opt-in");
        assert_eq!(config.chat.ring, crate::chat::RING_MAX);
        assert_eq!(config.chat.deny_list, Some(PathBuf::from("/srv/fly/chat-deny.txt")));
        assert_eq!(
            config.macros.mode,
            MacroMode::Raw,
            "raw mode stays the default until the measurement says otherwise \
             (docs/design/macros.md section 1)"
        );
    }

    #[test]
    fn the_macro_mode_comes_from_the_file_or_either_environment_name() {
        let config: Config = toml::from_str("[macros]\nmode = \"macros\"\n").unwrap();
        config.validate().unwrap();
        assert_eq!(config.macros.mode, MacroMode::Macros);

        // The two names section 12 replaced still parse, for one release, and mean `macros`.
        for legacy in ["palette", "plan"] {
            let config: Config =
                toml::from_str(&format!("[macros]\nmode = \"{legacy}\"\n")).unwrap();
            config.validate().unwrap();
            assert_eq!(config.macros.mode, MacroMode::Macros, "{legacy}");
        }

        for name in ["FLY_MACRO_MODE", "FLYSIM_MACROS_MODE"] {
            let mut config = Config::default();
            config.apply_env(&env(&[(name, "macros")])).unwrap();
            assert_eq!(config.macros.mode, MacroMode::Macros, "{name}");
            config.apply_env(&env(&[(name, "RAW")])).unwrap();
            assert_eq!(config.macros.mode, MacroMode::Raw, "{name} is case-insensitive");
            // The two names section 12 replaced still start a box, for one release, and mean
            // `macros` (the warning is a log line, not a failure).
            for legacy in ["palette", "PLAN"] {
                config.apply_env(&env(&[(name, legacy)])).unwrap();
                assert_eq!(config.macros.mode, MacroMode::Macros, "{name} = {legacy}");
                config.apply_env(&env(&[(name, "raw")])).unwrap();
            }
            // A typo fails startup rather than quietly streaming raw under a MACROS chip.
            assert!(config.apply_env(&env(&[(name, "on")])).is_err(), "{name}");
        }
    }

    #[test]
    fn palette_mode_over_a_game_with_no_palette_is_refused() {
        let mut config = Config::default();
        config.loop_.game = "platformer".to_string();
        config.game.platformer.rom_sha256 = None;
        config.validate().expect("the platformer runs fine in raw mode");
        config.macros.mode = MacroMode::Macros;
        let error = config.validate().expect_err("the platformer has no macro palette");
        assert!(error.to_string().contains("no macro palette"), "{error}");
    }

    #[test]
    fn the_chat_kill_switch_and_deny_list_path_come_from_the_environment() {
        let mut config = Config::default();
        config
            .apply_env(&env(&[
                ("FLY_CHAT_ENABLED", "0"),
                ("FLY_CHAT_DENY_LIST", "/srv/fly/chat-deny.txt"),
            ]))
            .unwrap();
        assert!(!config.chat.enabled);
        assert_eq!(config.chat.deny_list, Some(PathBuf::from("/srv/fly/chat-deny.txt")));
        config.validate().unwrap();

        let mut config = Config::default();
        config
            .apply_env(&env(&[
                ("FLYSIM_CHAT_ENABLED", "true"),
                ("FLYSIM_CHAT_RING", "7"),
                ("FLYSIM_CHAT_DENY_LIST", "none"),
            ]))
            .unwrap();
        assert!(config.chat.enabled);
        assert_eq!(config.chat.ring, 7);
        assert_eq!(config.chat.deny_list, None, "\"none\" means run with no deny list");
        config.validate().unwrap();
    }

    #[test]
    fn the_unit_files_env_names_override_the_file() {
        let mut config = Config::default();
        config
            .apply_env(&env(&[
                ("FLY_GAME", "platformer"),
                ("FLY_ROM", "/srv/fly/rom/abc.gb"),
                ("FLY_STATE", "/srv/fly/state2"),
                ("FLY_STATE_HOT", "/run/fly/state2"),
                ("FLY_FEED_BIND", "127.0.0.1:1"),
                ("FLY_CONTROL_BIND", "127.0.0.1:2"),
                ("FLY_METRICS_ADDR", "0.0.0.0:9101"),
                ("RAYON_NUM_THREADS", "6"),
                ("FLY_ROM_SHA256", "ABCDEF"),
            ]))
            .unwrap();
        assert_eq!(config.loop_.game, "platformer");
        assert_eq!(config.paths.rom, PathBuf::from("/srv/fly/rom/abc.gb"));
        assert_eq!(config.paths.save_dir, PathBuf::from("/srv/fly/state2"));
        assert_eq!(config.paths.hot_dir, PathBuf::from("/run/fly/state2"));
        assert_eq!(config.feed.bind.port(), 1);
        assert_eq!(config.control.bind.port(), 2);
        assert_eq!(config.control.metrics_bind.unwrap().port(), 9101);
        assert_eq!(config.loop_.threads, 6);
        assert_eq!(config.control.expect_rom_sha256.as_deref(), Some("abcdef"));
        config.validate().unwrap();
    }

    /// A hash-shaped value that is not any real cartridge.
    const PIN: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn the_platformer_rom_pin_comes_from_the_game_section_or_the_environment() {
        let mut config = Config::default();
        assert_eq!(config.rom_pin(), None, "Pokémon pins its ROM in the adapter");

        config.loop_.game = "platformer".to_string();
        assert_eq!(config.rom_pin(), None, "unset means semantic rewards off");
        config
            .apply_env(&env(&[("FLY_ROM_PLATFORMER_SHA256", &PIN.to_ascii_uppercase())]))
            .unwrap();
        assert_eq!(config.rom_pin(), Some(PIN), "and it is lowercased");
        config.validate().unwrap();

        // The file spelling, and the generic override, reach the same field.
        let from_file: Config =
            toml::from_str(&format!("[game.platformer]\nrom_sha256 = \"{PIN}\"\n")).unwrap();
        assert_eq!(from_file.game.platformer.rom_sha256.as_deref(), Some(PIN));
        let mut config = Config::default();
        config.apply_env(&env(&[("FLYSIM_GAME_PLATFORMER_ROM_SHA256", PIN)])).unwrap();
        assert_eq!(config.game.platformer.rom_sha256.as_deref(), Some(PIN));
    }

    #[test]
    fn a_malformed_rom_pin_is_a_config_error_rather_than_a_silent_no_pin() {
        for value in ["abc", "", &"z".repeat(64)] {
            let mut config = Config::default();
            config.game.platformer.rom_sha256 = Some(value.to_string());
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains("game.platformer.rom_sha256"), "{value:?}: {error}");
        }
    }

    #[test]
    fn an_empty_env_value_is_not_an_override() {
        // 05-deploy.sh writes `FLY_REWARD_ADAPTER=` and friends when the source env is blank.
        let mut config = Config::default();
        config.apply_env(&env(&[("FLY_GAME", ""), ("FLY_ROM", "")])).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn the_feed_path_is_direct_unless_the_environment_says_bus() {
        let config = Config::default();
        assert_eq!(config.feed.via, FeedVia::Direct);
        assert_eq!(config.feed.bus_dir, PathBuf::from("/run/fly/bus"));

        let mut config = Config::default();
        config
            .apply_env(&env(&[("FLY_FEED_VIA", "bus"), ("FLY_BUS_DIR", "/tmp/fly-bus")]))
            .unwrap();
        assert_eq!(config.feed.via, FeedVia::Bus);
        assert_eq!(config.feed.bus_dir, PathBuf::from("/tmp/fly-bus"));

        let mut config = Config::default();
        config.apply_env(&env(&[("FLYSIM_FEED_VIA", "DIRECT")])).unwrap();
        assert_eq!(config.feed.via, FeedVia::Direct);

        // A typo is a refusal, not a silent fallback to one of the two.
        let error = Config::default().apply_env(&env(&[("FLY_FEED_VIA", "buss")])).unwrap_err();
        assert!(error.to_string().contains("FLY_FEED_VIA"), "{error}");
        assert_eq!(toml::from_str::<Config>("[feed]\nvia = \"bus\"\n").unwrap().feed.via, FeedVia::Bus);
    }

    #[test]
    fn the_bus_dir_must_be_absolute_and_not_empty() {
        Config::default().validate().unwrap();
        for bad in ["", "run/fly/bus", "./bus"] {
            let mut config = Config::default();
            config.feed.bus_dir = PathBuf::from(bad);
            let error = config.validate().unwrap_err();
            assert!(error.to_string().contains("FLY_BUS_DIR"), "{bad:?}: {error}");
        }
        // Through the environment too.
        let mut config = Config::default();
        config.apply_env(&env(&[("FLY_BUS_DIR", "relative/bus")])).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn the_example_file_parses_and_validates() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../flysim.toml.example");
        let text = std::fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&text).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_a_silent_default() {
        let error = toml::from_str::<Config>("[loop]\nspeeed = 2.0\n").unwrap_err();
        assert!(error.to_string().contains("speeed"), "{error}");
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        for (key, value) in [
            ("FLYSIM_LOOP_SPEED", "0.1"),
            ("FLYSIM_LOOP_SNAPSHOT_HZ", "0"),
            ("FLYSIM_LOOP_HOT_SECONDS", "0"),
            ("FLYSIM_LOOP_GAME", "sonic"),
            ("FLYSIM_CONTROL_SUGAR_PER_MINUTE", "0"),
            ("FLYSIM_FEED_AUDIO_HZ", "10"),
            ("FLYSIM_CHAT_RING", "0"),
            ("FLYSIM_CHAT_RING", "13"),
        ] {
            let mut config = Config::default();
            config.apply_env(&env(&[(key, value)])).unwrap();
            assert!(config.validate().is_err(), "{key}={value}");
        }
        // Zero speed is the documented "unthrottled" sentinel, not an error.
        let mut config = Config::default();
        config.apply_env(&env(&[("FLYSIM_LOOP_SPEED", "0")])).unwrap();
        config.validate().unwrap();
    }
}
