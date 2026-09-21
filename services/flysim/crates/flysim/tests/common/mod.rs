#![allow(dead_code)]

//! Fixtures shared by the integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use flysim::eventlog::EventRing;
use flysim::simloop::{Command, Shared, booting_snapshot};
use flysim::snapshot::{
    AttachmentKind, ChatLine, FRAME_BYTES, FeedEvent, FeedEventKind, FeedMacro, FeedMacroOutcome,
    FeedPaletteSlot, FeedScene, FeedStatus, GameMode, MacroMode, MacroOutcome, RewardCounts,
    RewardKind, Snapshot,
};
use tokio::sync::{mpsc, watch};

/// The repository root, from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("the repository root is above services/flysim/crates/flysim")
}

/// `packages/feed/src/schema.json`, the header schema both languages are pinned to.
pub fn header_schema() -> serde_json::Value {
    let path = repo_root().join("packages/feed/src/schema.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    serde_json::from_str(&text).expect("the feed header schema is valid JSON")
}

/// A snapshot with every header field populated and all three attachments at their contract
/// sizes: 160x144 RGBA, interleaved stereo f32, and a `ceil(n / 8)`-byte spike bitset.
///
/// Built by filling in the loop's own boot snapshot through the same public types the publisher
/// uses, so a field that changes type has to be changed here too.
pub fn populated_snapshot(neurons: usize) -> Snapshot {
    let mut snapshot = booting_snapshot(4_242, 1_757_000_000_000, MacroMode::Raw);
    let header = &mut snapshot.header;
    header.status = FeedStatus::Running;
    header.realtime_factor = 1.0312;
    header.uptime_seconds = 612.5;
    header.run_seconds = 3_600.25;
    header.brain_ms = 3_600_250.0;
    header.frame = 215_089;
    header.buttons = 0b0001_0001; // up + a
    for (index, role) in [
        "command_0",
        "command_1",
        "command_2",
        "command_3",
        "command_4",
        "command_5",
        "command_6",
        "command_7",
        "steer_left",
        "steer_right",
        "forward",
        "backward",
        "proboscis",
        "reward_pam",
    ]
    .into_iter()
    .enumerate()
    {
        header
            .rates
            .insert(role.to_string(), serde_json::json!(index as f64 * 0.75));
    }
    header.population_rate = 12.375;
    header.learning.updates = 91;
    header.learning.changed = 1_204;
    header.learning.synapses = 16_384;
    header.learning.signal = -0.125;
    header.game.mode = GameMode::Overworld;
    header.game.semantic_rewards = true;
    header.game.map = Some(37);
    header.game.badges = 2;
    header.game.unique_locations = 148;
    header.game.reward_total = 9.55;
    header.game.reward_counts = RewardCounts {
        story: 1,
        explore: 18,
        area: 7,
        pokedex: 2,
        trainer: 3,
        wildwin: 11,
        badge: 2,
    };
    // Macros mode with a macro running and the previous one's outcome still on screen: the
    // fullest the `game` block ever is (`docs/design/macros.md` sections 6 and 12).
    header.game.scene = FeedScene::Overworld;
    header.game.macro_mode = MacroMode::Macros;
    header.game.palette = vec![
        FeedPaletteSlot {
            slot: 0,
            name: "GO OUT".to_string(),
            gloss: "nearest door".to_string(),
            channel: "MB·OUT".to_string(),
        },
        FeedPaletteSlot {
            slot: 4,
            name: "TALK".to_string(),
            gloss: "press A".to_string(),
            channel: "MB·TALK".to_string(),
        },
    ];
    header.game.running_macro = Some(FeedMacro {
        slot: 0,
        name: "GO OUT".to_string(),
        since_ms: 1_216.0,
    });
    header.game.macro_outcome = Some(FeedMacroOutcome {
        slot: 4,
        name: "TALK".to_string(),
        outcome: MacroOutcome::Done,
        at_ms: 3_598_900.0,
    });
    // A rank well up the 38-rung Pokémon ladder (`docs/design/ladder.md`), so the
    // fixture exercises a rung the old 16-rung bound would have rejected.
    header.milestone.rank = 22;
    header.milestone.label = "CELADON CITY".to_string();
    header.milestone.next = "SILPH SCOPE".to_string();
    header.milestone.since_seconds = 431.5;
    header.milestone.attempts = 1;
    header.milestone.total = 38;
    header.sugar.active = true;
    header.sugar.remaining_ms = 250.0;
    header.sugar.cooldown_ms = 12_500.0;
    header.sugar.last_by = Some("alex".to_string());
    header.sugar.today_count = 4;
    header.events = vec![
        FeedEvent {
            id: 900,
            wall_ms: 1_757_000_000_000,
            brain_ms: 3_600_100.0,
            kind: FeedEventKind::Reward,
            label: "AREA 37".to_string(),
            value: Some(0.2),
            reward_kind: Some(RewardKind::Area),
            by: None,
        },
        FeedEvent {
            id: 901,
            wall_ms: 1_757_000_000_100,
            brain_ms: 3_600_200.0,
            kind: FeedEventKind::Sugar,
            label: "alex fed the fly sugar".to_string(),
            value: Some(400.0),
            reward_kind: None,
            by: Some("alex".to_string()),
        },
        FeedEvent {
            id: 902,
            wall_ms: 1_757_000_000_200,
            brain_ms: 3_600_250.0,
            kind: FeedEventKind::System,
            label: "Checkpoint 12 saved".to_string(),
            value: None,
            reward_kind: None,
            by: None,
        },
        FeedEvent {
            id: 903,
            wall_ms: 1_757_000_000_220,
            brain_ms: 3_600_250.0,
            kind: FeedEventKind::Macro,
            label: "GO EXIT start".to_string(),
            value: Some(0.0),
            reward_kind: None,
            by: None,
        },
    ];
    header.chat = Some(vec![
        ChatLine {
            id: 904,
            wall_ms: 1_757_000_000_050,
            by: "mothra_fan".to_string(),
            text: "the ledge is RIGHT there".to_string(),
            bot: None,
        },
        ChatLine {
            id: 905,
            wall_ms: 1_757_000_000_100,
            by: "flybridgebot".to_string(),
            text: "Sugar from alex! The fly gets a brief PAM reward pulse.".to_string(),
            bot: Some(true),
        },
    ]);
    header.attachments = AttachmentKind::ALL.to_vec();

    // 1,600 stereo frames is one 30 Hz snapshot's worth at 48 kHz.
    let audio: Vec<f32> = (0..3_200).map(|index| (index as f32 / 3_200.0) - 0.5).collect();
    let mut spikes = vec![0u8; neurons.div_ceil(8)];
    let mut count = 0;
    for (index, byte) in spikes.iter_mut().enumerate() {
        if index % 3 == 0 {
            *byte = 0b1010_1010;
            count += 4;
        }
    }
    header.spike_count = count;

    snapshot.frame = Arc::new((0..FRAME_BYTES).map(|index| (index % 251) as u8).collect());
    snapshot.audio = Arc::new(flysim::snapshot::f32_bytes(&audio));
    snapshot.spikes = Arc::new(spikes);
    snapshot
}

/// An `AppState` with no simulation behind it, plus the command receiver, so a test can drive the
/// HTTP surface and answer commands itself.
pub fn test_state(
    configure: impl FnOnce(&mut flysim::config::Config),
) -> (flysim::AppState, mpsc::Receiver<Command>, watch::Sender<Arc<Snapshot>>) {
    let mut config = flysim::config::Config::default();
    configure(&mut config);
    let shared = Arc::new(Shared::new(config, EventRing::new()));
    let (commands, command_rx) = mpsc::channel(flysim::simloop::COMMAND_QUEUE);
    let (snapshots_tx, snapshots) = watch::channel(Arc::new(populated_snapshot(139_255)));
    (
        flysim::AppState { shared, commands, snapshots },
        command_rx,
        snapshots_tx,
    )
}
