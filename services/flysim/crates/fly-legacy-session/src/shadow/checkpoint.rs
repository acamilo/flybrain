//! Checkpoint bytes at a live save: the shadow's own export of the same boundary, byte for byte
//! against the file the live service wrote, after the named substitutions.
//!
//! The shadow never writes to the live stores. At a boundary where the live trace records a
//! capture `g<N>`, the shadow takes its own participant-coherent capture of that boundary (in the
//! position the live one had: before or after that boundary's rollback), exports it as FLYSIM01
//! through the one encoder both runtimes share (STATE-02), and compares the bytes with the live
//! file. The host fields the shadow cannot own are the live file's: `generation`, `wallMs` and
//! `lastEventId` (declared `host-fields`). `compatibility` and `speed` are the shadow's own
//! configuration and `rankSinceMs` its own rank tracking, so a difference in any of them is a
//! divergence. A legacy milestone archive taken before that boundary's slot save
//! (`afterActions` 0 ahead of a `save-slot`) holds the pre-capture ratchet and slot: the live
//! file's `ratchet`, `ratchetGame` and `ratchetFrame` stand in for the shadow's there (declared
//! `archive-order`) and everything else must still be identical.

use fly_session::legacy_checkpoint::{self, HostHalf, TaskHalf};
use fly_session::types::id;
use flysim::store;

/// The shadow's capture of one boundary.
#[derive(Clone, Debug)]
pub struct ShadowCapture {
    pub boundary: u64,
    pub world_payload: Vec<u8>,
    pub agent_payload: Vec<u8>,
    pub task: TaskHalf,
    /// The shadow's `rankSinceMs` at the capture.
    pub rank_since_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Identical,
    /// Identical after a declared difference.
    Declared,
}

/// A checkpoint that differs: the explanation and the shadow's export, kept for the review.
#[derive(Debug)]
pub struct Mismatch {
    pub detail: String,
    pub shadow_bytes: Vec<u8>,
}

/// Compares the live file's bytes with the shadow's export of the same boundary.
pub fn compare(
    live: &[u8],
    capture: &ShadowCapture,
    compatibility: &str,
    speed: f64,
    archive_order: bool,
) -> Result<Outcome, Box<Mismatch>> {
    let fail = |detail: String| {
        Box::new(Mismatch {
            detail,
            shadow_bytes: Vec::new(),
        })
    };
    let live_checkpoint = store::decode(live).map_err(|e| fail(format!("the live file: {e:#}")))?;
    let host = HostHalf {
        generation: live_checkpoint.runtime.generation,
        wall_ms: live_checkpoint.runtime.wall_ms,
        compatibility: compatibility.to_owned(),
        speed,
        rank_since_ms: capture.rank_since_ms,
        last_event_id: live_checkpoint.runtime.last_event_id,
    };
    let mut bytes = legacy_checkpoint::export(
        &capture.agent_payload,
        &capture.world_payload,
        &id(crate::task::SLOT),
        &capture.task,
        &host,
    )
    .map_err(fail)?;
    if archive_order {
        let mut shadow =
            store::decode(&bytes).map_err(|e| fail(format!("the shadow export: {e:#}")))?;
        shadow.runtime.ratchet = live_checkpoint.runtime.ratchet;
        shadow.runtime.ratchet_game = live_checkpoint.runtime.ratchet_game.clone();
        shadow.runtime.ratchet_frame = live_checkpoint.runtime.ratchet_frame.clone();
        bytes =
            store::encode(&shadow.agent, &shadow.runtime).map_err(|e| fail(format!("{e:#}")))?;
    }
    if bytes == live {
        return Ok(if archive_order {
            Outcome::Declared
        } else {
            Outcome::Identical
        });
    }
    Err(Box::new(Mismatch {
        detail: explain(&live_checkpoint, &bytes),
        shadow_bytes: bytes,
    }))
}

/// Which members differ, for the divergence report.
fn explain(live: &store::Checkpoint, shadow_bytes: &[u8]) -> String {
    let shadow = match store::decode(shadow_bytes) {
        Ok(shadow) => shadow,
        Err(e) => return format!("the shadow export does not decode: {e:#}"),
    };
    let (l, s) = (&live.runtime, &shadow.runtime);
    let mut fields = Vec::new();
    macro_rules! field {
        ($name:literal, $a:expr, $b:expr) => {
            if $a != $b {
                fields.push($name);
            }
        };
    }
    field!("romSha256", l.rom_sha256, s.rom_sha256);
    field!("emulatorFrame", l.emulator_frame, s.emulator_frame);
    field!("compatibility", l.compatibility, s.compatibility);
    field!("speed", l.speed, s.speed);
    field!("buttons", l.buttons, s.buttons);
    field!("rankSinceMs", l.rank_since_ms, s.rank_since_ms);
    field!("reward", l.reward, s.reward);
    field!("ratchet", l.ratchet, s.ratchet);
    field!("reinforcements", l.reinforcements, s.reinforcements);
    field!("emulator", l.emulator, s.emulator);
    field!("framebuffer", l.framebuffer, s.framebuffer);
    field!("ratchetGame", l.ratchet_game, s.ratchet_game);
    field!("ratchetFrame", l.ratchet_frame, s.ratchet_frame);
    let agent_same = store::encode(&shadow.agent, &live.runtime)
        .ok()
        .zip(store::encode(&live.agent, &live.runtime).ok())
        .is_some_and(|(a, b)| a == b);
    if !agent_same {
        fields.push("agent");
    }
    let mut detail = format!(
        "generation {}: the shadow's export differs from the live file in {}",
        live.runtime.generation,
        if fields.is_empty() {
            "the encoding only".to_owned()
        } else {
            fields.join(", ")
        }
    );
    if fields.contains(&"rankSinceMs") {
        detail.push_str(&format!(
            " (rankSinceMs live {} shadow {})",
            l.rank_since_ms, s.rank_since_ms
        ));
    }
    if fields.contains(&"emulator") {
        let first = l.emulator.iter().zip(&s.emulator).position(|(a, b)| a != b);
        let count = l
            .emulator
            .iter()
            .zip(&s.emulator)
            .filter(|(a, b)| a != b)
            .count();
        detail.push_str(&format!(
            " (emulator: {} vs {} bytes, {count} differ, first at {first:?})",
            l.emulator.len(),
            s.emulator.len()
        ));
    }
    if fields.contains(&"ratchet") {
        detail.push_str(&format!(
            " (ratchet live {:?} shadow {:?})",
            l.ratchet, s.ratchet
        ));
    }
    detail
}
