//! A FLYSIM01 checkpoint as a session's start: the legacy fly's own save, installed into the new
//! runtime through the ordinary group restore (`Coordinator::import`).
//!
//! Each participant gets its half of the file in its own capture format and validates it as one of
//! its own captures:
//!
//! - **the world**: `WorldState::from_flysim01` (ENV-01) at `k = emulatorFrame - 1`, the ratchet's
//!   snapshot as the slot `best`, as a `FLYENV01` payload;
//! - **the agent**: the checkpoint's agent state in the worker's own `FLYAGT01` capture, the way
//!   AGENT-01's parity harness seeds a worker (`legacy_parity::legacy_state_payload`), with
//!   `learning.updates` started at `plasticity.updates` (legacy-gameboy-v1 section 13 amendment of
//!   2026-09-29) and the checkpoint's identity and boundary written in;
//! - **the task**: the adapter state and the ratchet state (FLYSIM01's `reward` and `ratchet`).
//!
//! The rest is `restore: legacy-transient-reset` (section 14): the executor starts fresh and
//! observes the restored boundary, the agent's readout transient starts as a fresh process has it.
//! The decision context the agent is given is computed here from the restored world -- the task's
//! own boot, bound channels and location on the checkpoint's memory image -- and the coordinator
//! holds the task to it after the install.

use std::collections::BTreeMap;

use serde_json::json;

use fly_session::coordinator::{Coordinator, ImportSpec, ImportedAgent};
use fly_session::legacy_env::{PayloadHeader, WorldState};
use fly_session::legacy_parity::legacy_state_payload;
use fly_session::types::*;
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, MemoryImage};

use crate::task::PokeredTask;

/// What an import installed.
#[derive(Clone, Debug)]
pub struct Imported {
    pub boundary: u64,
    pub epoch: Id,
    pub brain_ticks: u64,
}

/// The checkpoint's memory image at its boundary, read the way the environment reads it after an
/// import: the emulator state imported into a stopped emulator, then one bulk read.
pub fn image_of(rom: &[u8], emulator_state: &[u8]) -> Result<MemoryImage, String> {
    let mut emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .map_err(|e| format!("the cartridge: {e}"))?;
    emulator
        .import_state(emulator_state)
        .map_err(|e| format!("the emulator state: {e}"))?;
    let mut image = MemoryImage::zeroed();
    emulator.read_memory_image_into(&mut image);
    Ok(image)
}

/// Installs a FLYSIM01 checkpoint as the start of a bootstrapped session that has taken no
/// transition. `task` is the composition's task object (the coordinator holds its two faces).
#[allow(clippy::too_many_arguments)]
pub async fn import_flysim01(
    coordinator: &mut Coordinator,
    task: &PokeredTask,
    bytes: &[u8],
    rom: &[u8],
    agent_id: &Id,
    checkpoint_id: &Id,
    audio_sample_rate: u64,
    new_epoch: &Id,
) -> Result<Imported, String> {
    let checkpoint = flysim::store::decode(bytes).map_err(|e| format!("FLYSIM01: {e}"))?;
    let descriptor = coordinator
        .descriptor()
        .cloned()
        .ok_or("the session never bootstrapped")?;
    let environment_id = coordinator.environment_ref().worker_id.clone();
    let world = WorldState::from_flysim01(
        bytes,
        coordinator.episode_id(),
        &id(crate::task::SLOT),
        audio_sample_rate,
    )?;
    let boundary = world.boundary;
    let scope = coordinator.import_scope(boundary);
    let world_payload = world.encode(&PayloadHeader {
        worker_id: environment_id,
        checkpoint_id: checkpoint_id.clone(),
        source_scope: scope.clone(),
        configuration_digest: descriptor.configuration_digest.clone(),
    });

    // The task's ledger, and from it and the restored image the context the agent resumes with.
    let ledger = PokeredTask::ledger_from_flysim01(
        &checkpoint.runtime.reward,
        &checkpoint.runtime.ratchet,
        !checkpoint.runtime.ratchet_game.is_empty(),
    );
    let brain_ms = checkpoint.agent.network.ms;
    let context = task.preview_context(
        &ledger,
        &image_of(rom, &checkpoint.runtime.emulator)?,
        brain_ms,
    )?;

    // The agent: its own capture as the template, the legacy state written into it.
    let template = coordinator
        .capture_participant(agent_id)
        .await
        .map_err(|e| format!("the agent template: {}", e.error.message))?;
    let payload = legacy_state_payload(&template, &checkpoint.agent, &context)?;
    let payload = restamp_agent_payload(&payload, checkpoint_id, &scope, boundary)?;
    let remainder =
        fly_session::legacy_agent::legacy_remainder_to_rational(checkpoint.agent.remainder)
            .map_err(|e| format!("the remainder: {e}"))?;

    let audio_positions: BTreeMap<String, u64> = descriptor
        .audio
        .iter()
        .map(|stream| (stream.stream_id.clone(), world.audio_next_sample))
        .collect();
    let spec = ImportSpec {
        checkpoint_id: checkpoint_id.clone(),
        boundary,
        world_payload,
        agents: vec![ImportedAgent {
            agent_id: agent_id.clone(),
            payload,
            brain_ticks: brain_ms as u64,
            remainder,
            context,
        }],
        task_ledger: ledger,
        audio_positions,
    };
    coordinator
        .import(spec, new_epoch)
        .await
        .map_err(|e| format!("the import ({}): {}", e.detail, e.error.message))?;
    coordinator
        .resume()
        .map_err(|e| format!("resuming the import: {}", e.error.message))?;
    Ok(Imported {
        boundary,
        epoch: new_epoch.clone(),
        brain_ticks: brain_ms as u64,
    })
}

/// Writes the checkpoint's identity and boundary into an agent payload's session member: the
/// template was captured at boundary 0 under another checkpoint id.
fn restamp_agent_payload(
    payload: &[u8],
    checkpoint_id: &Id,
    scope: &Scope,
    boundary: u64,
) -> Result<Vec<u8>, String> {
    let magic = fly_session::legacy_agent::PAYLOAD_MAGIC;
    let parts =
        flybrain_core::envelope::decode_envelope(payload, magic).map_err(|e| e.to_string())?;
    let mut manifest = parts.manifest.clone();
    let session = manifest.get("session").ok_or("no session member")?;
    let mut session: serde_json::Value =
        serde_json::from_str(&session.stringify()).map_err(|e| e.to_string())?;
    session["checkpointId"] = json!(checkpoint_id.as_str());
    session["sourceScope"] = scope.to_json();
    session["committedStep"] = json!(boundary.to_string());
    manifest.set(
        "session",
        flybrain_core::json::JsonValue::parse(&session.to_string()).map_err(|e| e.to_string())?,
    );
    flybrain_core::envelope::encode_envelope(magic, &manifest, &parts.chunks)
        .map_err(|e| e.to_string())
}
