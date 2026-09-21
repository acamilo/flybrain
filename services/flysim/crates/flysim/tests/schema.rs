//! The produced header must validate against `packages/feed/src/schema.json`.
//!
//! That file is the one shape pinned for both languages (`docs/feed-protocol.md`: "defined in
//! TypeScript in `packages/feed` and in Rust with serde in `services/flysim/crates/flysim`. A
//! JSON schema test in each side pins the shape"). It is `additionalProperties: false`
//! throughout, so a renamed or extra Rust field fails here rather than in the browser.
//!
//! Validation is done in-process with the `jsonschema` crate rather than by shelling out to
//! `npx tsx`, so `cargo test` needs no Node toolchain.

mod common;

use flysim::simloop::booting_snapshot;
use flysim::snapshot::{
    AttachmentKind, ChatLine, FeedMacroOutcome, FeedPaletteSlot, FeedStatus, MacroMode,
    MacroOutcome, Wants,
};

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&common::header_schema()).expect("the feed header schema compiles")
}

fn assert_valid(validator: &jsonschema::Validator, header: &serde_json::Value) {
    let errors: Vec<String> = validator
        .iter_errors(header)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(
        errors.is_empty(),
        "header does not validate:\n  {}\n\n{}",
        errors.join("\n  "),
        serde_json::to_string_pretty(header).unwrap_or_default()
    );
}

#[test]
fn a_fully_populated_header_validates() {
    let snapshot = common::populated_snapshot(139_255);
    let header = serde_json::to_value(&snapshot.header).unwrap();
    assert_valid(&validator(), &header);
}

#[test]
fn the_boot_window_header_validates() {
    // The header published before the dataset is even loaded: no attachments, no events, and
    // every numeric field at its floor. `required` covers all of them, so a missing field here
    // is a schema failure rather than a silent `undefined` on the page.
    let header = serde_json::to_value(booting_snapshot(1, 1_757_000_000_000, MacroMode::Raw).header).unwrap();
    assert_valid(&validator(), &header);
    assert_eq!(header["status"], "booting");
    assert_eq!(header["attachments"], serde_json::json!([]));
}

#[test]
fn a_header_encoded_for_a_client_that_wants_nothing_still_validates() {
    // The bridge asks for no attachments, which zeroes `spikeCount` as well.
    let snapshot = common::populated_snapshot(4_096);
    let bytes = snapshot.encode(Wants::none());
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&bytes[4..4 + length]).unwrap();
    assert_valid(&validator(), &header);
    assert_eq!(header["attachments"], serde_json::json!([]));
    assert_eq!(header["spikeCount"], 0);
}

#[test]
fn the_schema_rejects_the_mistakes_it_exists_to_catch() {
    let validator = validator();
    let snapshot = common::populated_snapshot(4_096);
    let valid = serde_json::to_value(&snapshot.header).unwrap();
    assert!(validator.is_valid(&valid));

    // An unknown header field: `additionalProperties: false` at the top level.
    let mut extra = valid.clone();
    extra["manualButtons"] = serde_json::json!(3);
    assert!(!validator.is_valid(&extra), "an extra field must not validate");

    // A misspelled camelCase name is the same failure from the other side.
    let mut renamed = valid.clone();
    let object = renamed.as_object_mut().unwrap();
    let value = object.remove("realtimeFactor").unwrap();
    object.insert("realtime_factor".to_string(), value);
    assert!(!validator.is_valid(&renamed), "snake_case must not validate");

    // A non-finite number would serialize as `null`.
    let mut nulled = valid.clone();
    nulled["populationRate"] = serde_json::Value::Null;
    assert!(!validator.is_valid(&nulled));

    // Out-of-range values the protocol bounds. `rank` has no upper bound any more --
    // the ladder belongs to the adapter and Pokémon Red's is 38 rungs
    // (`docs/design/ladder.md`) -- so what is bounded is the sign, and `total`, which
    // is a count.
    let mut bad_rank = valid.clone();
    bad_rank["milestone"]["rank"] = serde_json::json!(-1);
    assert!(!validator.is_valid(&bad_rank));
    let mut deep_rank = valid.clone();
    deep_rank["milestone"]["rank"] = serde_json::json!(37);
    assert!(validator.is_valid(&deep_rank), "rung 37 is on the ladder");
    let mut bad_total = valid.clone();
    bad_total["milestone"]["total"] = serde_json::json!(0);
    assert!(!validator.is_valid(&bad_total), "a ladder has at least one rung");
    let mut bad_buttons = valid.clone();
    bad_buttons["buttons"] = serde_json::json!(256);
    assert!(!validator.is_valid(&bad_buttons));
    let mut bad_badges = valid.clone();
    bad_badges["game"]["badges"] = serde_json::json!(9);
    assert!(!validator.is_valid(&bad_badges));

    // An unknown enum member.
    let mut bad_mode = valid.clone();
    bad_mode["game"]["mode"] = serde_json::json!("UNSUPPORTED ROM");
    assert!(!validator.is_valid(&bad_mode));
    let mut bad_status = valid;
    bad_status["status"] = serde_json::json!("stopped");
    assert!(!validator.is_valid(&bad_status));
}

#[test]
fn the_chat_ring_validates_full_empty_and_absent() {
    let validator = validator();
    let mut snapshot = common::populated_snapshot(4_096);
    // The populated header already carries a viewer line and a bot line.
    let header = serde_json::to_value(&snapshot.header).unwrap();
    assert_valid(&validator, &header);
    assert_eq!(header["chat"].as_array().map(Vec::len), Some(2));
    assert_eq!(header["chat"][1]["bot"], serde_json::json!(true));
    assert!(header["chat"][0].get("bot").is_none(), "a viewer line omits bot");

    // A full ring of the maximum size.
    let line = snapshot.header.chat.as_ref().unwrap()[0].clone();
    snapshot.header.chat = Some(
        (0..flysim::chat::RING_MAX)
            .map(|index| ChatLine { id: 1_000 + index as u64, ..line.clone() })
            .collect(),
    );
    assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());

    // An empty ring (chat on, nobody has said anything yet) and no ring at all (kill switch).
    snapshot.header.chat = Some(Vec::new());
    assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    snapshot.header.chat = None;
    let header = serde_json::to_value(&snapshot.header).unwrap();
    assert_valid(&validator, &header);
    assert!(header.get("chat").is_none(), "the kill switch omits the field entirely");
}

#[test]
fn the_schema_rejects_the_chat_lines_the_sanitizer_would_never_have_produced() {
    let validator = validator();
    let snapshot = common::populated_snapshot(4_096);
    let valid = serde_json::to_value(&snapshot.header).unwrap();
    let line = valid["chat"][0].clone();

    // One line past the contract's ceiling.
    let mut too_many = valid.clone();
    too_many["chat"] = serde_json::Value::Array(vec![line.clone(); flysim::chat::RING_MAX + 1]);
    assert!(!validator.is_valid(&too_many), "13 lines must not validate");

    // Text past the sanitizer's cap, an empty name, and an unknown field on a line.
    let mut too_long = valid.clone();
    too_long["chat"][0]["text"] = serde_json::json!("x".repeat(flysim::chat::MAX_TEXT_LENGTH + 1));
    assert!(!validator.is_valid(&too_long));
    let mut no_name = valid.clone();
    no_name["chat"][0]["by"] = serde_json::json!("");
    assert!(!validator.is_valid(&no_name));
    let mut extra = valid.clone();
    extra["chat"][0]["colour"] = serde_json::json!("#ff0000");
    assert!(!validator.is_valid(&extra));
    let mut missing = valid;
    missing["chat"][0] = serde_json::json!({ "id": 1, "wallMs": 1, "by": "alex" });
    assert!(!validator.is_valid(&missing), "a line without text must not validate");
}

#[test]
fn every_status_mode_reward_kind_and_attachment_the_service_can_emit_is_in_the_schema() {
    // The service picks these from closed Rust enums; the schema lists them as string enums.
    // Round-tripping each one through the validator proves the two lists agree, which a
    // hand-written mapping test would not.
    let validator = validator();
    let mut snapshot = common::populated_snapshot(4_096);
    for status in [
        FeedStatus::Booting,
        FeedStatus::Running,
        FeedStatus::Paused,
        FeedStatus::Recovering,
        FeedStatus::Error,
    ] {
        snapshot.header.status = status;
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    for mode in [
        "BOOT",
        "OVERWORLD",
        "BATTLE",
        "TRANSITION",
        "DEMO / SAFARI",
        "UNSUPPORTED ROM · SEMANTIC REWARDS OFF",
        "",
    ] {
        snapshot.header.game.mode = flysim::snapshot::GameMode::from_adapter(mode);
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    for rule in flybrain_gb::pokemon_red::catalog::REWARDS {
        let kind = flysim::snapshot::RewardKind::from_adapter(rule.kind).expect(rule.kind);
        snapshot.header.events[0].reward_kind = Some(kind);
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    for kinds in [
        vec![],
        vec![AttachmentKind::Frame],
        vec![AttachmentKind::Audio],
        vec![AttachmentKind::Spikes],
        AttachmentKind::ALL.to_vec(),
    ] {
        snapshot.header.attachments = kinds;
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    // The macro palette's three closed sets, from the producer's own enums: every scene the
    // adapter can detect, both modes, every outcome, and every `macro` event label.
    for scene in flybrain_gb::SceneId::ALL {
        snapshot.header.game.scene = flysim::snapshot::FeedScene::from_adapter(scene);
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    for mode in [MacroMode::Raw, MacroMode::Macros] {
        snapshot.header.game.macro_mode = mode;
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    for outcome in [
        flybrain_gb::Outcome::Done,
        flybrain_gb::Outcome::Blocked,
        flybrain_gb::Outcome::Timeout,
        flybrain_gb::Outcome::Refused,
    ] {
        let outcome = MacroOutcome::from_adapter(outcome);
        snapshot.header.game.macro_outcome = Some(FeedMacroOutcome {
            slot: 5,
            name: "WANDER".to_string(),
            outcome,
            at_ms: 12.0,
        });
        snapshot.header.events[3].label = format!("WANDER {}", outcome.as_str());
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    // Every macro the Pokémon palette can bind: its name fits the schema's fourteen characters,
    // its gloss fits the cell and its channel tag fits the glyph column, which are the three
    // things a page cannot work around.
    snapshot.header.game.macro_mode = MacroMode::Macros;
    for kind in flybrain_gb::pokemon_red::macros::MacroKind::ALL {
        snapshot.header.game.palette = vec![FeedPaletteSlot {
            slot: 3,
            name: kind.name().to_string(),
            gloss: kind.gloss().to_string(),
            channel: kind.channel_tag().to_string(),
        }];
        assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    }
    // A full palette: six bound slots, each with its own type's channel (section 12).
    snapshot.header.game.palette = (0..6)
        .map(|slot| {
            let kind = flybrain_gb::pokemon_red::macros::MacroKind::BY_CHANNEL[usize::from(slot)];
            FeedPaletteSlot {
                slot,
                name: kind.name().to_string(),
                gloss: kind.gloss().to_string(),
                channel: kind.channel_tag().to_string(),
            }
        })
        .collect();
    assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
    // Raw mode: no scene, no palette, no macro. The other half of the additive contract.
    snapshot.header.game.scene = flysim::snapshot::FeedScene::Unknown;
    snapshot.header.game.macro_mode = MacroMode::Raw;
    snapshot.header.game.palette = Vec::new();
    snapshot.header.game.running_macro = None;
    snapshot.header.game.macro_outcome = None;
    assert_valid(&validator, &serde_json::to_value(&snapshot.header).unwrap());
}
