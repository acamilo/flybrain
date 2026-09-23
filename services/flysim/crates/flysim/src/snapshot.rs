//! Feed protocol v1: the header types and the binary framing.
//!
//! Binding contract: `docs/feed-protocol.md`. The TypeScript side of the same wire format is
//! `packages/feed/src/{types,codec}.ts`, and `packages/feed/src/schema.json` pins the header for
//! both languages (`tests/schema.rs` validates a produced header against it).
//!
//! ```text
//! u32 LE headerLength | header JSON (UTF-8) | (u32 LE byteLength | bytes) * n
//! ```
//!
//! Attachments follow the header in the order `header.attachments` lists them.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Feed and control API protocol version.
pub const PROTOCOL: u32 = 1;

/// Emulator framebuffer geometry (`docs/feed-protocol.md`, "Attachments").
pub const FRAME_WIDTH: usize = 160;
pub const FRAME_HEIGHT: usize = 144;
/// RGBA bytes in one `frame` attachment: 92,160.
pub const FRAME_BYTES: usize = FRAME_WIDTH * FRAME_HEIGHT * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedStatus {
    Booting,
    Running,
    Paused,
    Recovering,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GameMode {
    Boot,
    Overworld,
    Battle,
    Transition,
    Demo,
    Safari,
    Unknown,
}

impl GameMode {
    /// Map an adapter's free-form mode string onto the protocol's closed set.
    ///
    /// `flybrain-gb`'s Pokémon adapter reports `BOOT`, `OVERWORLD`, `BATTLE`, `TRANSITION`,
    /// `DEMO / SAFARI` and `UNSUPPORTED ROM · SEMANTIC REWARDS OFF`; the last one is a mode the
    /// protocol has no name for, and `game.semanticRewards` already carries that fact.
    ///
    /// The platformer adapter reports `BOOT`, `IN LEVEL <world>-<stage>`, `TRANSITION`,
    /// `GAME OVER` and `DEMO`. The set stays closed rather than growing an `IN_LEVEL` and a
    /// `GAME_OVER` — `docs/feed-protocol.md` is a published schema and every consumer switches
    /// exhaustively on it — so two of them map onto existing names:
    ///
    /// - `IN LEVEL 1-1` -> `OVERWORLD`: both mean "the player is controllable in the world", which
    ///   is what the page uses the mode for. The *level* is carried separately as `game.map`, so
    ///   nothing is lost; the per-game config supplies the word ("running", not "overworld").
    /// - `GAME OVER` -> `TRANSITION`: the run is over and the fly is not controllable, which is
    ///   exactly how `TRANSITION` already reads on screen. The ratchet's own recovery event says
    ///   what happened, and the ticker shows it.
    pub fn from_adapter(mode: &str) -> Self {
        match mode {
            "BOOT" => Self::Boot,
            "OVERWORLD" => Self::Overworld,
            "BATTLE" => Self::Battle,
            "TRANSITION" | "GAME OVER" => Self::Transition,
            "DEMO / SAFARI" | "DEMO" => Self::Demo,
            "SAFARI" => Self::Safari,
            mode if mode.starts_with("IN LEVEL") => Self::Overworld,
            _ => Self::Unknown,
        }
    }
}

/// Which scene the game is in (`docs/feed-protocol.md`, `game.scene`).
///
/// A closed set, like [`GameMode`]: the stage switches exhaustively on it to title its palette
/// strip. An adapter's own scene enum folds onto these names — `flybrain_gb::SceneId` is the
/// producer side and is the same nine words — and a game with no palette at all reports
/// [`FeedScene::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeedScene {
    Title,
    Overworld,
    Dialog,
    Menu,
    Battle,
    BattleSwitch,
    Shop,
    Pc,
    Unknown,
}

impl FeedScene {
    pub fn from_adapter(scene: flybrain_gb::SceneId) -> Self {
        use flybrain_gb::SceneId;
        match scene {
            SceneId::Title => Self::Title,
            SceneId::Overworld => Self::Overworld,
            SceneId::Dialog => Self::Dialog,
            SceneId::Menu => Self::Menu,
            SceneId::Battle => Self::Battle,
            SceneId::BattleSwitch => Self::BattleSwitch,
            SceneId::Shop => Self::Shop,
            SceneId::Pc => Self::Pc,
            SceneId::Unknown => Self::Unknown,
        }
    }
}

/// Whether the fly's channels press buttons or choose macros (`docs/design/macros.md` section 1).
///
/// One type for the config knob (`flysim.toml` `[macros] mode`) and the published field, so the
/// two cannot disagree about the spelling or about which one is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacroMode {
    /// The decoder's button mask goes to the emulator, exactly as it always has.
    #[default]
    Raw,
    /// The scene's macros are buttons of their own, each pressed by its own population and
    /// decided by the readout's `macros` exclusive group (`docs/design/macros.md` section 12).
    ///
    /// The eight real buttons keep working: a macro that is running owns the pad, and nothing
    /// else changes about the decode. `palette` and `plan`, the two modes this one replaces, are
    /// accepted as configuration values for one release and mean this -- the aliases are what
    /// keeps a `flysim.toml` or an `infra/env/example.env` from the release before this one starting
    /// the service at all. They are read, never written: the feed publishes `macros`.
    #[serde(alias = "palette", alias = "plan")]
    Macros,
}

impl MacroMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Macros => "macros",
        }
    }

    /// Whether this mode deals a palette at all, i.e. everything but [`MacroMode::Raw`].
    pub const fn dealt(self) -> bool {
        !matches!(self, Self::Raw)
    }

    /// The palette flavour this mode asks [`flybrain_gb::palette_for`] for.
    ///
    /// `None` in raw mode, where no palette is built at all.
    pub const fn palette_mode(self) -> Option<flybrain_gb::PaletteMode> {
        match self {
            Self::Raw => None,
            // `PaletteMode::Plan` deals the *set* section 12 asks for: it is the dealer that
            // knows `GO OBJECTIVE`'s rung, `GO WARP` indoors and `TALK` in front of an untalked
            // thing, which the fixed table never did. Its order no longer means anything --
            // nothing ranks a macro any more -- and the screen sorts the cells by type.
            Self::Macros => Some(flybrain_gb::PaletteMode::Plan),
        }
    }
}

/// How a macro ended (`docs/design/macros.md` section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacroOutcome {
    Done,
    Blocked,
    Timeout,
    Refused,
}

impl MacroOutcome {
    pub fn from_adapter(outcome: flybrain_gb::Outcome) -> Self {
        use flybrain_gb::Outcome;
        match outcome {
            Outcome::Done => Self::Done,
            Outcome::Blocked => Self::Blocked,
            Outcome::Timeout => Self::Timeout,
            Outcome::Refused => Self::Refused,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
        }
    }
}

/// One bound slot of the current scene's palette.
///
/// Unbound slots are omitted entirely rather than sent as nulls, so `palette` is 0 to 6 entries
/// in ascending slot order and the page draws a missing slot dim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPaletteSlot {
    /// 0..5 in the readout's order: UP, DOWN, LEFT, RIGHT, A, B.
    pub slot: u8,
    /// The macro's name, at most fourteen characters.
    pub name: String,
    /// Two or three words of what it does in this scene ("nearest door").
    pub gloss: String,
    /// The macro's channel tag, `MB·GO`, `MB·ATK` and so on
    /// (`docs/design/macros.md` section 12).
    ///
    /// The glyph on the cell, where the button arrow used to be, and the label on the SENSES
    /// tab's MACROS row. The population behind it is `macro_` plus the macro's name lowercased
    /// with spaces as underscores, which is how the page finds this macro's rate in
    /// `brain.rates`.
    pub channel: String,
}

/// The macro that owns the buttons right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedMacro {
    pub slot: u8,
    pub name: String,
    /// Brain milliseconds since it started.
    pub since_ms: f64,
}

/// The macro that finished most recently, kept until the next one starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedMacroOutcome {
    pub slot: u8,
    pub name: String,
    pub outcome: MacroOutcome,
    /// The brain clock at which it finished.
    pub at_ms: f64,
}

/// Reward categories the feed reports counts for. The adapter's own interned kinds
/// (`milestone`, `exploration`, `map`, `species`, `trainer`, `battle`, `badge`, `boundary`,
/// `catch`, `talk`, `item`) map onto these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RewardKind {
    Story,
    Explore,
    Area,
    Pokedex,
    Trainer,
    Wildwin,
    Badge,
}

impl RewardKind {
    /// Map an adapter's interned kind onto the protocol's closed set, or `None` for a kind the
    /// protocol has no counter for. A `None` reward still reaches the page as an event with the
    /// adapter's own label; it is only left out of `game.rewardCounts` and out of the ticker's
    /// per-kind copy.
    ///
    /// The platformer's nine kinds share the same seven slots, because these are the counters the
    /// feed schema publishes and the per-game config re-words them (`apps/stage/src/games/`):
    /// `band` -> `explore`, `coin` -> `wildwin`, `score` -> `area`, `powerup` -> `pokedex`,
    /// `life` -> `trainer`, `level` -> `story`, `world` -> `badge`. `started` and `clear` are
    /// deliberately unmapped: each pays once in a lifetime, a counter for them is noise, and the
    /// ticker shows their own labels ("RUN STARTED", "GAME CLEARED") which is what should be on
    /// screen at that moment anyway.
    pub fn from_adapter(kind: &str) -> Option<Self> {
        Some(match kind {
            // Pokémon Red.
            "milestone" => Self::Story,
            "exploration" => Self::Explore,
            "map" => Self::Area,
            "species" => Self::Pokedex,
            "trainer" => Self::Trainer,
            "battle" => Self::Wildwin,
            "badge" => Self::Badge,
            // `boundary` (`docs/design/room-escape.md` section 2) is exploration as far as the
            // protocol is concerned: finding a door is finding somewhere new, and the design asks
            // for no new feed kind.
            "boundary" => Self::Explore,
            // `catch` is a wild battle the fly won by keeping the Pokémon, so it publishes on
            // the same counter a wild KO does. The feed's kinds are a closed set
            // (`docs/feed-protocol.md`) and this rule asked for no new one.
            //
            // Deliberately *not* `pokedex`: on a catch of a species this run has never owned,
            // the cartridge sets the Pokédex bit and the adapter's existing `species` rule pays
            // for it on the same frame, so the `pokedex` counter already moves. Mapping `catch`
            // there as well would count one event twice.
            "catch" => Self::Wildwin,
            // `talk` and `item` (the operator, 2026-09-23) are the fly finding what is in a place:
            // a person or a sign it opened, an item it picked up. That is the family `explore`
            // already counts -- new ground, a door found -- at the same quiet scale (0.05 to
            // 0.15), so both publish there and the feed's closed kind set does not move. Not
            // `area`, which counts maps and is a notable row; not `story`, which is the plot;
            // not `wildwin`, which is a battle. The Pokémon Red ticker's copy for `explore` says
            // "new find" so that the row is true of all four (`apps/stage/src/games/pokemon-red.ts`).
            "talk" => Self::Explore,
            "item" => Self::Explore,
            // The platformer.
            "band" => Self::Explore,
            "coin" => Self::Wildwin,
            "score" => Self::Area,
            "powerup" => Self::Pokedex,
            "life" => Self::Trainer,
            "level" => Self::Story,
            "world" => Self::Badge,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Story => "story",
            Self::Explore => "explore",
            Self::Area => "area",
            Self::Pokedex => "pokedex",
            Self::Trainer => "trainer",
            Self::Wildwin => "wildwin",
            Self::Badge => "badge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedEventKind {
    Reward,
    Sugar,
    Milestone,
    Recovery,
    Checkpoint,
    Viewer,
    System,
    /// A macro started or finished (`docs/design/macros.md` section 5). Label is
    /// `<NAME> start` or `<NAME> done|blocked|timeout|refused`.
    Macro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Frame,
    Audio,
    Spikes,
}

impl AttachmentKind {
    pub const ALL: [Self; 3] = [Self::Frame, Self::Audio, Self::Spikes];
}

/// One entry in `FeedHeader.events` and one line of the event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedEvent {
    pub id: u64,
    pub wall_ms: u64,
    pub brain_ms: f64,
    pub kind: FeedEventKind,
    /// Short human text, template-generated, never raw chat.
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward_kind: Option<RewardKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedLearning {
    pub enabled: bool,
    pub updates: u64,
    pub changed: u64,
    pub synapses: u64,
    pub signal: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedGame {
    pub mode: GameMode,
    /// False when the ROM hash is not the audited one.
    pub semantic_rewards: bool,
    pub map: Option<u32>,
    pub badges: u32,
    pub unique_locations: u64,
    pub reward_total: f64,
    pub reward_counts: RewardCounts,
    /// The scene the macro palette is dealt for. `unknown` in raw mode over a game with no
    /// palette, and `title` through the intro, where the readout's boot variant applies instead.
    pub scene: FeedScene,
    /// Whether the fly's channels press buttons (`raw`) or choose macros (`palette`).
    pub macro_mode: MacroMode,
    /// The bound slots of the current scene's palette, ascending, unbound slots omitted. Always
    /// empty in raw mode: no palette is dealt, so there is nothing honest to draw.
    pub palette: Vec<FeedPaletteSlot>,
    /// The macro that owns the buttons, or `null`.
    #[serde(rename = "macro")]
    pub running_macro: Option<FeedMacro>,
    /// The macro that finished most recently, kept until the next one starts.
    pub macro_outcome: Option<FeedMacroOutcome>,
    /// Brain milliseconds the pad has been empty in a playable scene; 0 otherwise.
    ///
    /// The operator, 2026-09-17: "sometimes the macro buttons disappear and everything just hangs there."
    /// An empty pad *is* the doctrine working -- nothing presses for the fly, so a scene with no
    /// button waits -- and it is indistinguishable on screen from a hang, so it is measured.
    /// Report-only: the watchdog exports it as `fly_pad_empty_seconds` and nothing acts on it.
    ///
    /// Additive and always present, like every other field of this struct; 0 in raw mode, where no
    /// palette is dealt at all.
    #[serde(default)]
    pub pad_empty_ms: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewardCounts {
    pub story: u64,
    pub explore: u64,
    pub area: u64,
    pub pokedex: u64,
    pub trainer: u64,
    pub wildwin: u64,
    pub badge: u64,
}

impl RewardCounts {
    /// Add `value` to one counter.
    ///
    /// Adding rather than assigning, because the mapping is many-to-one: Pokémon Red's
    /// `exploration` and `boundary` both land on `explore`, so the published counter is their sum.
    /// Assigning would have let whichever adapter kind came last in the map silently win.
    pub fn add(&mut self, kind: RewardKind, value: u64) {
        let counter = match kind {
            RewardKind::Story => &mut self.story,
            RewardKind::Explore => &mut self.explore,
            RewardKind::Area => &mut self.area,
            RewardKind::Pokedex => &mut self.pokedex,
            RewardKind::Trainer => &mut self.trainer,
            RewardKind::Wildwin => &mut self.wildwin,
            RewardKind::Badge => &mut self.badge,
        };
        *counter = counter.saturating_add(value);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedMilestone {
    /// Position on the adapter's ladder, `0..total - 1`.
    pub rank: u32,
    pub label: String,
    /// Label of rank + 1, or the current one at the top of the ladder.
    pub next: String,
    /// Simulated seconds since the rank last changed (the stuck-o-meter).
    pub since_seconds: f64,
    /// Game rollbacks since reaching this rank.
    pub attempts: u64,
    /// Rungs the adapter's ladder has, so a page can draw the right number of them
    /// without knowing the game. Optional in the protocol (a producer that predates
    /// it omits it); this producer always sends it.
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSugar {
    pub active: bool,
    pub remaining_ms: f64,
    pub cooldown_ms: f64,
    pub last_by: Option<String>,
    pub today_count: u64,
}

/// One accepted chat line in the header's bounded `chat` ring.
///
/// `by` passed `chat::is_valid_display_name` and `text` passed `chat::sanitize_chat_text` before
/// this existed — see `docs/control-api.md` (`POST /chat`). Raw chat never reaches the page, and
/// nothing here is ever fed back into the simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLine {
    /// The event id of the accepted line.
    pub id: u64,
    pub wall_ms: u64,
    pub by: String,
    pub text: String,
    /// `Some(true)` for the bridge's own template replies; omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<bool>,
}

/// The header of one feed snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedHeader {
    pub protocol: u32,
    pub seq: u64,
    pub wall_ms: u64,
    pub status: FeedStatus,
    pub realtime_factor: f64,
    pub uptime_seconds: f64,
    pub run_seconds: f64,
    pub brain_ms: f64,
    pub frame: u64,
    pub buttons: u32,
    /// Hz per tracked role, in the network's own role order.
    pub rates: Map<String, Value>,
    pub population_rate: f64,
    /// Set bits in the `spikes` attachment; 0 when it is omitted.
    pub spike_count: u64,
    pub learning: FeedLearning,
    pub game: FeedGame,
    pub milestone: FeedMilestone,
    pub sugar: FeedSugar,
    pub events: Vec<FeedEvent>,
    /// The accepted chat ring, oldest first. `None` — the field absent from the JSON entirely —
    /// while `[chat] enabled = false`. That is the kill switch: a page can tell "chat is off"
    /// from "nobody has said anything yet".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<Vec<ChatLine>>,
    pub attachments: Vec<AttachmentKind>,
}

/// Which attachments one client asked for in its `hello`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wants {
    pub frame: bool,
    pub audio: bool,
    pub spikes: bool,
}

impl Wants {
    pub fn all() -> Self {
        Self { frame: true, audio: true, spikes: true }
    }

    pub fn none() -> Self {
        Self::default()
    }

    pub fn from_kinds(kinds: &[AttachmentKind]) -> Self {
        let mut wants = Self::none();
        for kind in kinds {
            match kind {
                AttachmentKind::Frame => wants.frame = true,
                AttachmentKind::Audio => wants.audio = true,
                AttachmentKind::Spikes => wants.spikes = true,
            }
        }
        wants
    }

    pub fn has(&self, kind: AttachmentKind) -> bool {
        match kind {
            AttachmentKind::Frame => self.frame,
            AttachmentKind::Audio => self.audio,
            AttachmentKind::Spikes => self.spikes,
        }
    }
}

/// One published snapshot: the full header plus every attachment the sim produced.
///
/// Encoding is per client, because `header.attachments` has to list exactly what that client is
/// sent and `spikeCount` is 0 when the bitset is withheld.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub header: FeedHeader,
    pub frame: Arc<Vec<u8>>,
    pub audio: Arc<Vec<u8>>,
    pub spikes: Arc<Vec<u8>>,
}

impl Snapshot {
    /// Frame the snapshot for one client, honouring its `wants`.
    pub fn encode(&self, wants: Wants) -> Vec<u8> {
        let mut header = self.header.clone();
        let mut payloads: Vec<&[u8]> = Vec::with_capacity(3);
        header.attachments = Vec::with_capacity(3);
        for kind in self.header.attachments.iter().copied() {
            if !wants.has(kind) {
                continue;
            }
            header.attachments.push(kind);
            payloads.push(match kind {
                AttachmentKind::Frame => self.frame.as_slice(),
                AttachmentKind::Audio => self.audio.as_slice(),
                AttachmentKind::Spikes => self.spikes.as_slice(),
            });
        }
        if !header.attachments.contains(&AttachmentKind::Spikes) {
            header.spike_count = 0;
        }
        encode_message(&header, &payloads)
    }
}

/// `u32 LE headerLength | header JSON | (u32 LE length | bytes)*`, exactly as
/// `packages/feed/src/codec.ts` encodes it.
pub fn encode_message(header: &FeedHeader, attachments: &[&[u8]]) -> Vec<u8> {
    debug_assert_eq!(header.attachments.len(), attachments.len());
    let json = serde_json::to_vec(header).expect("a FeedHeader always serializes");
    let total = 4 + json.len() + attachments.iter().map(|bytes| 4 + bytes.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for bytes in attachments {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Bit `i` set when neuron `i` spiked at least once since the previous snapshot, plus the number
/// of set bits.
///
/// `lastSpikeMs[i]` holds the brain clock of that neuron's last spike, and a spike emitted during
/// tick `t` is stamped `t`, so the window `[since_ms, now)` is exactly `lastSpikeMs >= since_ms`.
/// A snapshot published without any ticks in between (paused) reports nothing rather than
/// re-reporting the last tick's spikes.
pub fn spike_bitset(last_spike_ms: &[f64], since_ms: f64, now_ms: f64) -> (Vec<u8>, u64) {
    let mut bytes = vec![0u8; last_spike_ms.len().div_ceil(8)];
    if now_ms <= since_ms {
        return (bytes, 0);
    }
    let mut count = 0u64;
    for (index, last) in last_spike_ms.iter().enumerate() {
        if *last >= since_ms {
            bytes[index >> 3] |= 1 << (index & 7);
            count += 1;
        }
    }
    (bytes, count)
}

/// binjgb's unipolar u8 samples to the bipolar f32 PCM the feed contract requires.
///
/// `docs/feed-protocol.md`, "Audio source note": convert with `v / 255` and then apply a
/// DC-blocking one-pole high-pass per channel, `y[n] = x[n] - x[n-1] + 0.995 * y[n-1]`. The
/// filter state has to survive across frames, or every frame boundary is a step discontinuity.
#[derive(Debug, Clone, Copy, Default)]
pub struct DcBlocker {
    previous_in: [f32; 2],
    previous_out: [f32; 2],
}

/// Pole of the DC-blocking high-pass.
pub const DC_BLOCK_POLE: f32 = 0.995;

impl DcBlocker {
    /// Convert and filter `raw` (interleaved stereo u8) onto the end of `out`.
    pub fn process_into(&mut self, raw: &[u8], out: &mut Vec<f32>) {
        out.reserve(raw.len());
        for (index, sample) in raw.iter().enumerate() {
            let channel = index & 1;
            let input = f32::from(*sample) / 255.0;
            let output =
                input - self.previous_in[channel] + DC_BLOCK_POLE * self.previous_out[channel];
            self.previous_in[channel] = input;
            self.previous_out[channel] = output;
            out.push(output);
        }
    }
}

/// Little-endian bytes of an f32 slice, as a `Float32Array` holds them.
pub fn f32_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Replace a non-finite number with 0: JSON has no NaN or Infinity, so `serde_json` would write
/// `null` and the header would stop validating against the schema.
pub fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A memory that reads as all zeroes, enough to drive an adapter's mode string.
    struct Zeroes;
    impl flybrain_gb::adapter::MemoryReader for Zeroes {
        fn read8(&mut self, _address: u16) -> u8 {
            0
        }
    }

    #[test]
    fn a_bitset_covers_every_neuron_and_counts_its_own_bits() {
        let last = vec![-1_000_000.0, 5.0, 4.0, 6.0, -1_000_000.0, 7.0, 7.0, 7.0, 7.0];
        let (bytes, count) = spike_bitset(&last, 5.0, 8.0);
        assert_eq!(bytes.len(), 2, "ceil(9 / 8)");
        assert_eq!(count, 6);
        assert_eq!(bytes[0], 0b1110_1010);
        assert_eq!(bytes[1], 0b0000_0001);
    }

    #[test]
    fn a_snapshot_with_no_elapsed_ticks_reports_no_spikes() {
        let last = vec![5.0; 32];
        let (bytes, count) = spike_bitset(&last, 5.0, 5.0);
        assert_eq!(count, 0);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn silence_stays_silent_and_the_blocker_removes_a_constant_offset() {
        let mut blocker = DcBlocker::default();
        let mut out = Vec::new();
        blocker.process_into(&[0u8; 64], &mut out);
        assert!(out.iter().all(|sample| *sample == 0.0), "{out:?}");

        // A step to a constant level decays back to zero rather than sitting at an offset.
        let mut blocker = DcBlocker::default();
        let mut out = Vec::new();
        blocker.process_into(&[40u8; 4096], &mut out);
        let first = out[0];
        let last = *out.last().unwrap();
        assert!(first > 0.1, "the step itself is audible: {first}");
        assert!(last.abs() < 1e-3, "the offset is gone by the end: {last}");
        assert!(out.iter().all(|sample| (-1.0..=1.0).contains(sample)));
    }

    #[test]
    fn the_two_channels_are_filtered_independently() {
        let mut blocker = DcBlocker::default();
        let mut out = Vec::new();
        // Left silent, right at full scale.
        let raw: Vec<u8> = (0..64).map(|index| if index % 2 == 0 { 0 } else { 255 }).collect();
        blocker.process_into(&raw, &mut out);
        assert!(out.iter().step_by(2).all(|sample| *sample == 0.0));
        assert!(out[1] > 0.9);
    }

    #[test]
    fn adapter_modes_and_reward_kinds_map_onto_the_protocol_names() {
        assert_eq!(GameMode::from_adapter("OVERWORLD"), GameMode::Overworld);
        assert_eq!(GameMode::from_adapter("DEMO / SAFARI"), GameMode::Demo);
        assert_eq!(
            GameMode::from_adapter("UNSUPPORTED ROM · SEMANTIC REWARDS OFF"),
            GameMode::Unknown
        );
        for rule in flybrain_gb::pokemon_red::catalog::REWARDS {
            assert!(RewardKind::from_adapter(rule.kind).is_some(), "{}", rule.kind);
        }
        assert_eq!(RewardKind::from_adapter("nonsense"), None);
        assert_eq!(RewardKind::from_adapter("boundary"), Some(RewardKind::Explore));
        assert_eq!(RewardKind::from_adapter("catch"), Some(RewardKind::Wildwin));
        assert_eq!(RewardKind::from_adapter("talk"), Some(RewardKind::Explore));
        assert_eq!(RewardKind::from_adapter("item"), Some(RewardKind::Explore));
    }

    #[test]
    fn two_adapter_kinds_on_one_protocol_counter_are_summed() {
        // `exploration` and `boundary` both publish as `explore`, and `progress().counts` is a
        // BTreeMap, so an assigning setter would have let whichever key sorted last win and
        // silently dropped the other. This is the whole reason `add` is not `set`.
        let counts: std::collections::BTreeMap<&str, u64> =
            [("exploration", 7), ("boundary", 3), ("badge", 2)].into_iter().collect();
        let mut published = RewardCounts::default();
        for (kind, count) in &counts {
            if let Some(kind) = RewardKind::from_adapter(kind) {
                published.add(kind, *count);
            }
        }
        assert_eq!(published.explore, 10);
        assert_eq!(published.badge, 2);
        assert_eq!(published.area, 0);
    }

    #[test]
    fn the_platformer_modes_fold_onto_the_closed_set() {
        assert_eq!(GameMode::from_adapter("IN LEVEL 1-1"), GameMode::Overworld);
        assert_eq!(GameMode::from_adapter("IN LEVEL 4-3"), GameMode::Overworld);
        assert_eq!(GameMode::from_adapter("GAME OVER"), GameMode::Transition);
        assert_eq!(GameMode::from_adapter("DEMO"), GameMode::Demo);
        assert_eq!(GameMode::from_adapter("BOOT"), GameMode::Boot);
        assert_eq!(GameMode::from_adapter("TRANSITION"), GameMode::Transition);
        // Every mode the adapter can report has a home, and none of them is Unknown.
        let mut adapter = flybrain_gb::adapter_for("platformer").unwrap();
        assert_eq!(GameMode::from_adapter(adapter.mode()), GameMode::Boot);
        // With no ROM pin the adapter reports rewards-off, which is the one Unknown.
        adapter.sample(&mut Zeroes, 0.0);
        assert_eq!(GameMode::from_adapter(adapter.mode()), GameMode::Unknown);
    }

    #[test]
    fn the_platformer_reward_kinds_share_the_seven_published_counters() {
        use flybrain_gb::platformer::catalog::{REWARDS, kind};
        let mapped: Vec<_> = REWARDS
            .iter()
            .filter_map(|rule| RewardKind::from_adapter(rule.kind).map(|k| (rule.kind, k)))
            .collect();
        assert_eq!(mapped.len(), 7, "{mapped:?}");
        assert_eq!(RewardKind::from_adapter(kind::BAND), Some(RewardKind::Explore));
        assert_eq!(RewardKind::from_adapter(kind::COIN), Some(RewardKind::Wildwin));
        assert_eq!(RewardKind::from_adapter(kind::SCORE), Some(RewardKind::Area));
        assert_eq!(RewardKind::from_adapter(kind::POWERUP), Some(RewardKind::Pokedex));
        assert_eq!(RewardKind::from_adapter(kind::LIFE), Some(RewardKind::Trainer));
        assert_eq!(RewardKind::from_adapter(kind::LEVEL), Some(RewardKind::Story));
        assert_eq!(RewardKind::from_adapter(kind::WORLD), Some(RewardKind::Badge));
        // Once-per-lifetime payouts carry their own label instead of a counter.
        assert_eq!(RewardKind::from_adapter(kind::STARTED), None);
        assert_eq!(RewardKind::from_adapter(kind::CLEAR), None);
    }

    #[test]
    fn the_status_and_kind_names_serialize_exactly_as_the_protocol_spells_them() {
        assert_eq!(serde_json::to_string(&FeedStatus::Recovering).unwrap(), "\"recovering\"");
        assert_eq!(serde_json::to_string(&GameMode::Overworld).unwrap(), "\"OVERWORLD\"");
        assert_eq!(serde_json::to_string(&RewardKind::Wildwin).unwrap(), "\"wildwin\"");
        assert_eq!(serde_json::to_string(&AttachmentKind::Spikes).unwrap(), "\"spikes\"");
        assert_eq!(serde_json::to_string(&FeedEventKind::Milestone).unwrap(), "\"milestone\"");
    }
}
