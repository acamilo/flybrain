//! The task ledger is not bounded by the 32 KiB `TypedValue` limit (TASK-01 review B1).
//!
//! The adapter's lifetime ledger grows with play (42-46 KB on the live fly at rank 12-15). The
//! pokered-macros-v1 ledger keeps it as an attachment beside a small typed value, so a ledger of
//! any size captures, validates and installs, and a malformed one is an error, never a panic.
//! ROM-free: the cartridge is ENV-01's toy one, which the task never reads here.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use fly_legacy_session::task::{PokeredConfig, PokeredTask, REWARD_ATTACHMENT};
use fly_session::fly_session_types::gameboy::ReadoutContext;
use fly_session::legacy_checkpoint::TaskHalf;
use fly_session::legacy_env_parity::toy_cart;
use fly_session::types::*;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flysim::snapshot::MacroMode;

fn task() -> PokeredTask {
    let rom = toy_cart::rom(0);
    PokeredTask::new(
        PokeredConfig {
            agent_id: id("fly"),
            port_id: id("p1"),
            mode: MacroMode::Raw,
            macro_channels: Vec::new(),
            hold_ms: 0.0,
            record: false,
        },
        &rom,
        &digest_of_bytes(&rom),
        &id("e1"),
    )
    .expect("the object")
}

/// A real adapter state grown past 64 KB: thousands of walked tiles, as a long life has.
fn big_reward() -> Value {
    let mut reward = PokemonRedReward::new().export_state();
    let tiles: Vec<Value> = (0..12_000)
        .map(|n| json!(format!("{}:{}:{}", n % 200, n / 200 % 64, n / 12_800)))
        .collect();
    reward["tiles"] = Value::Array(tiles);
    reward
}

#[test]
fn a_ledger_past_32_kib_captures_validates_and_installs() {
    let reward = big_reward();
    let bytes = serde_json::to_vec(&reward).unwrap();
    assert!(bytes.len() > 64 * 1024, "{} bytes", bytes.len());
    // The adapter itself takes it, so it is a real ledger and not just a large JSON value.
    PokemonRedReward::new()
        .import_state(&reward)
        .expect("an adapter state");

    let half = TaskHalf {
        reward,
        ratchet: Default::default(),
    };
    let (typed, attachments) = PokeredTask::ledger_of(&half, false).expect("any size");
    assert!(
        typed.to_json().to_string().len() < 1024,
        "the typed value stays small"
    );
    assert_eq!(attachments[REWARD_ATTACHMENT], bytes);

    let object = task();
    let mut face = object.task();
    face.validate_restore_with(&typed, &attachments)
        .expect("validates");
    face.install_restore_with(&id("e2"), &typed, &attachments)
        .expect("installs");
    let (restored, slot) = object.task_half();
    assert!(!slot);
    let mut direct = PokemonRedReward::new();
    direct.import_state(&half.reward).unwrap();
    assert_eq!(
        restored.reward,
        direct.export_state(),
        "the installed adapter is the imported one"
    );
    // And what it captures back is a ledger of the same kind, which installs again.
    let (again, again_attachments) = (face.capture().unwrap(), face.capture_attachments().unwrap());
    assert!(again_attachments[REWARD_ATTACHMENT].len() > 32 * 1024);
    face.validate_restore_with(&again, &again_attachments)
        .expect("re-validates");

    // The context preview reads it too.
    let image = flybrain_gb::MemoryImage::zeroed();
    let context = object
        .preview_context(&typed, &attachments, &image, 1_000.0)
        .expect("previews");
    assert!(ReadoutContext::from_typed(&context).is_ok());
}

#[test]
fn a_bad_ledger_is_an_error_not_a_panic() {
    let half = TaskHalf {
        reward: big_reward(),
        ratchet: Default::default(),
    };
    let (typed, attachments) = PokeredTask::ledger_of(&half, false).unwrap();
    let face = task().task();
    for (what, attachments) in [
        ("missing", BTreeMap::new()),
        (
            "another",
            BTreeMap::from([(REWARD_ATTACHMENT.to_owned(), b"{}".to_vec())]),
        ),
        (
            "truncated",
            BTreeMap::from([(
                REWARD_ATTACHMENT.to_owned(),
                attachments[REWARD_ATTACHMENT][..100].to_vec(),
            )]),
        ),
    ] {
        let err = face
            .validate_restore_with(&typed, &attachments)
            .unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::IncompatibleState,
            "{what}: {}",
            err.message
        );
    }
}
