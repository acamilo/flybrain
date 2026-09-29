//! A FLYSIM01 checkpoint as a session's start: the legacy fly's own save, installed into the new
//! runtime through the ordinary group restore (`Coordinator::import`).
//!
//! The halves are STATE-02's (`fly_session::legacy_checkpoint`): the world as ENV-01's `FLYENV01`
//! payload at `k = emulatorFrame - 1` with the ratchet's snapshot as slot `best`; the agent as its
//! `FLYAGT01` payload with the file's reinforcement count and the file's framebuffer as the next
//! input, as `LegacyFrame::restore` installs it; the task as `{reward, ratchet, slotFilled}`. The ids
//! come from the file (`flysim01-g<generation>`, source scope `(session, legacy, k)`).
//!
//! The rest is `restore: legacy-transient-reset` (`legacy-gameboy-v1` section 14): the executor
//! starts fresh and observes the restored boundary, and the agent's readout transient starts as a
//! fresh process has it. The decision context the agent resumes with is computed here, from the
//! task's ledger and the checkpoint's own memory image. After the install the coordinator holds the
//! task to it: `Task::restored` re-observes the restored world and must derive the same context.

use std::collections::BTreeMap;

use fly_session::coordinator::{Coordinator, ImportSpec, ImportedAgent};
use fly_session::legacy_agent::legacy_remainder_to_rational;
use fly_session::legacy_checkpoint::{
    AgentImport, agent_payload, halves, import_checkpoint_id, legacy_source_scope, world_payload,
};
use fly_session::types::*;
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, MemoryImage};
use flysim::store::Checkpoint;

use crate::task::PokeredTask;

/// What an import installed.
#[derive(Clone, Debug)]
pub struct Imported {
    pub boundary: u64,
    pub epoch: Id,
    pub brain_ticks: u64,
    pub checkpoint_id: Id,
    /// The file's `rankSinceMs` and generation, for the host's rank tracking and store.
    pub rank_since_ms: f64,
    pub generation: u64,
}

/// The checkpoint's memory image at its boundary, read as the environment reads it after an
/// import: the emulator state imported into a stopped emulator, then MEM-01's one bulk read.
pub fn image_of(rom: &[u8], emulator_state: &[u8]) -> Result<MemoryImage, String> {
    let mut emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .map_err(|e| format!("the cartridge: {e}"))?;
    emulator
        .import_state(emulator_state)
        .map_err(|e| format!("the emulator state: {e}"))?;
    Ok(emulator.read_memory_image())
}

/// Installs a FLYSIM01 checkpoint as the start of a bootstrapped session that has taken no
/// transition, onto a replacement world. `task` is the composition's task object.
pub async fn import_flysim01(
    coordinator: &mut Coordinator,
    task: &PokeredTask,
    checkpoint: &Checkpoint,
    rom: &[u8],
    agent: &AgentImport,
    audio_sample_rate: u64,
    new_epoch: &Id,
) -> Result<Imported, String> {
    let descriptor = coordinator
        .descriptor()
        .cloned()
        .ok_or("the session never bootstrapped")?;
    let halves = halves(
        checkpoint,
        coordinator.episode_id(),
        &id(crate::task::SLOT),
        audio_sample_rate,
    )?;
    let boundary = halves.world.boundary;
    let checkpoint_id = import_checkpoint_id(halves.host.generation);
    let scope = legacy_source_scope(coordinator.session_id(), boundary);
    let world = world_payload(
        &halves.world,
        &coordinator.environment_ref().worker_id,
        &checkpoint_id,
        &scope,
        &descriptor.configuration_digest,
    );

    // The task's ledger, and from it and the restored image the context the agent resumes with.
    let ledger = PokeredTask::ledger_of(&halves.task, !checkpoint.runtime.ratchet_game.is_empty());
    let brain_ms = halves.agent.network.ms;
    let image = image_of(rom, &halves.world.emulator)?;
    let context = task.preview_context(&ledger, &image, brain_ms)?;
    let payload = agent_payload(
        &halves.agent,
        halves.reinforcements,
        &checkpoint.runtime.framebuffer,
        agent,
        &checkpoint_id,
        &scope,
        &context,
    )?;
    let remainder = legacy_remainder_to_rational(halves.agent.remainder)
        .map_err(|e| format!("the remainder: {e}"))?;
    let audio_positions: BTreeMap<String, u64> = descriptor
        .audio
        .iter()
        .map(|stream| (stream.stream_id.clone(), halves.world.audio_next_sample))
        .collect();
    let spec = ImportSpec {
        checkpoint_id: checkpoint_id.clone(),
        source_scope: scope,
        boundary,
        world_payload: world,
        agents: vec![ImportedAgent {
            agent_id: agent.agent_id.clone(),
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
        checkpoint_id,
        rank_since_ms: halves.host.rank_since_ms,
        generation: halves.host.generation,
    })
}
