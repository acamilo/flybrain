//! ENV-01: the Game Boy compatibility environment worker of the legacy composition.
//!
//! `legacy-gameboy-v1` sections 7 to 9, 11, 14 and 16, and the RT-01a amendments to
//! `workers-v1` (sections 3 and 7), `step-v1` (section 6) and `state-media-v1` (sections 2 to 5).
//! It is the emulator half of flysim's `LegacyFrame`, moved behind the environment methods, and it
//! makes exactly the emulator calls `LegacyFrame` makes, in the same order:
//!
//! | `LegacyFrame` (flysim `frame.rs`) | this worker |
//! | --- | --- |
//! | `initialize`: `run_frame`, framebuffer, `take_audio_u8` (the counter becomes 1) | `Environment.Initialize`: the one-frame setup scaffold, no button down; O[0] with `engineFrame` "1", its audio discarded |
//! | `run` + `take_frame`: `set_buttons(mask)`, `run_frame`, framebuffer, `take_audio_u8` | `Environment.Advance`: the joypad mask of the one port, then O[k+1] with its f32 audio chunk |
//! | `boundary`'s capture: `export_state` + the frame on screen | `Environment.SaveSlot` (`gameboy-slots-v1`) |
//! | `rollback` via `recover_game`: `import_state`, `set_buttons(0)` twice, the slot's frame | `Environment.RestoreSlot` under a new epoch, no frame run |
//! | `restore` from FLYSIM01: `import_state`, `set_buttons(buttons)`, the saved framebuffer | `State.StageRestore` + `State.ActivateRestore` |
//!
//! What the worker adds is only what the contracts ask for and the legacy loop never needed:
//! native audio as `f32le` (`sample / 255`, no filtering: the DC blocker is the edge's), one
//! memory image per boundary as the `gameboy-memory-inspection-v1` inspection, the rational world
//! clock, epochs, batch identity and the capture payload.
//!
//! The joypad is the only write into a running game. `import_state` is a write only on the
//! declared recovery paths (a slot restore and a group restore), exactly as it is in flysim.
//!
//! The memory image is read through [`memory_image`], the one seam MEM-01 replaces with the shim's
//! bulk read. Until then its body is 65,536 `fly_gb_read_mem` calls through the existing
//! `read_uncached`, which is by definition the image `legacy-gameboy-v1` section 8 specifies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fly_session_types::extensions::{
    MAX_SLOTS, METHOD_RESTORE_SLOT, METHOD_SAVE_SLOT, RestoreSlotParams, RestoreSlotResult,
    SLOTS_CAPABILITY, SaveSlotParams, SaveSlotResult,
};
use fly_session_types::gameboy::{self, MemoryInspection};
use flybrain_core::envelope::{decode_envelope, encode_envelope};
use flybrain_core::json::JsonValue;
use flybrain_gb::emulator::{DEFAULT_AUDIO_FRAMES, Emulator, FRAMEBUFFER_LEN};
use serde_json::{Value, json};

use crate::media::{self, AUDIO_CONTENT_TYPE, FRAME_CONTENT_TYPE};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::{BoxFuture, HandlerCtx, HandlerReply, StatusCell, WorkerEndpoint};

/// The one audio stream: binjgb's stereo mix at the configured rate.
pub const AUDIO_STREAM_ID: &str = "apu";
/// The attachment the boundary's memory image travels under.
pub const MEMORY_ATTACHMENT: &str = "inspection.memory";
/// The memory image's content type (`legacy-gameboy-v1` section 8).
pub const MEMORY_CONTENT_TYPE: &str = "application/octet-stream";
/// The legacy composition declares one slot.
pub const DEFAULT_SLOT: &str = "best";
/// The legacy port.
pub const DEFAULT_PORT: &str = "p1";
/// flysim's default `feed.audio_hz`.
pub const DEFAULT_AUDIO_RATE: u64 = 48_000;
/// Stereo.
pub const AUDIO_CHANNELS: u64 = 2;
/// The backend configuration document's form.
pub const BACKEND_CONFIG_FORM: &str = "gameboy-backend-config-v1";
/// The capture payload's envelope magic: flybrain-core's frozen layout, like FLYSIM01's.
pub const PAYLOAD_MAGIC: &str = "FLYENV01";
/// The capture payload's layout version.
pub const PAYLOAD_VERSION: u64 = 1;
/// The state format this environment captures under (a participant compatibility identity).
pub const STATE_FORMAT_ID: &str = "fly-gb-env-v1";
/// The FLYSIM01 magic, for the world's half of a legacy checkpoint.
pub const FLYSIM01_MAGIC: &str = "FLYSIM01";

/// The capabilities the worker advertises in `Worker.Hello`.
pub fn capabilities() -> Vec<Id> {
    vec![
        id("world-step-v1"),
        id("pixel-observation-v1"),
        id(crate::state::CHECKPOINT_CAPABILITY),
        id(SLOTS_CAPABILITY),
    ]
}

// -------------------------------------------------------------------------------------------
// The emulator seams

/// The boundary's memory image (`legacy-gameboy-v1` section 8 and its MEM-01 amendment of
/// 2026-09-29): MEM-01's one read-only bulk read, `Emulator::read_memory_image`.
///
/// Every memory byte (`flybrain_gb::captured`) is what `fly_gb_read_mem(i)` returns at this
/// boundary; the three register windows -- VRAM, OAM, I/O with the APU and wave RAM -- are not
/// captured and read `$FF` (`flybrain_gb::NOT_CAPTURED`). binjgb answers every captured address
/// straight out of an array, so the read is state-neutral by construction: nothing is saved or
/// imported around it, and `export_state` before and after is byte-identical
/// (`tests/legacy_env.rs`, `the_memory_image_leaves_the_emulator_as_it_found_it`).
///
/// (Until the ENV-01 review, 2026-09-29, R1, this body read all 65,536 addresses through
/// `read_uncached` between an `export_state` and an `import_state` of the same bytes, because a
/// read of a register window runs binjgb's lazy catch-up and moves the exported state. MEM-01's
/// rule removes the cause: a register window is never read.)
pub fn memory_image(emulator: &mut Emulator) -> Result<Vec<u8>, String> {
    Ok(emulator.read_memory_image().as_bytes().to_vec())
}

/// binjgb's unsigned 8-bit interleaved stereo as the declared `f32le-interleaved`, with binjgb's
/// own host rule `sample / 255`: unipolar, silence at exactly 0.0. No filtering; the DC blocker
/// is the edge's presentation state (`legacy-gameboy-v1` section 9).
pub fn audio_f32le(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() * 4);
    for &sample in raw {
        let value = f32::from(sample) / flybrain_gb::emulator::AUDIO_FULL_SCALE;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// The legacy mask of one complete port control: bit `i` is `GAMEBOY_BUTTONS[i]`, which is
/// `flybrain_gb::emulator::buttons`' order (`legacy-gameboy-v1` sections 6 and 7).
pub fn mask_of(control: &PortControl) -> u8 {
    control
        .buttons
        .iter()
        .enumerate()
        .filter(|(_, button)| button.down)
        .fold(0u8, |mask, (bit, _)| mask | (1u8 << bit))
}

/// The complete port control for a legacy mask.
pub fn control_of(port_id: &str, mask: u8) -> PortControl {
    PortControl {
        port_id: port_id.to_owned(),
        buttons: gameboy::GAMEBOY_BUTTONS
            .iter()
            .enumerate()
            .map(|(bit, button)| ButtonState {
                id: (*button).to_owned(),
                down: mask & (1u8 << bit) != 0,
            })
            .collect(),
        axes: Vec::new(),
    }
}

/// `ControllerSchema {schema: gameboy-joypad-v1, buttons: [up, down, left, right, a, b, start,
/// select], axes: []}` (`legacy-gameboy-v1` section 7).
pub fn joypad_schema() -> ControllerSchema {
    ControllerSchema {
        schema: gameboy::JOYPAD.schema_ref(),
        buttons: gameboy::GAMEBOY_BUTTONS
            .iter()
            .map(|b| (*b).to_owned())
            .collect(),
        axes: Vec::new(),
    }
}

/// The `lcd` view: 160 x 144 `rgba8`, row stride 640, no render delay.
pub fn view_descriptor() -> ViewDescriptor {
    ViewDescriptor {
        view_id: gameboy::VIEW_ID.to_owned(),
        width: gameboy::VIEW_WIDTH,
        height: gameboy::VIEW_HEIGHT,
        row_stride: gameboy::VIEW_WIDTH * 4,
        pixel_aspect_numerator: 1,
        pixel_aspect_denominator: 1,
        observation_delay_steps: 0,
    }
}

/// The `apu` stream at `sample_rate`, stereo.
pub fn audio_descriptor(sample_rate: u64) -> AudioDescriptor {
    AudioDescriptor {
        stream_id: id(AUDIO_STREAM_ID),
        sample_rate,
        channels: AUDIO_CHANNELS,
    }
}

/// The backend identity: the vendored binjgb core behind flybrain-gb's shim, with the size of its
/// save state on this target (an ABI-dependent layout a state is only valid under).
pub fn backend_digest() -> Digest {
    digest_of_bytes(
        format!(
            "fly-session/gameboy-backend-v1\nbinjgb+flybrain-gb-shim\nstateBytes={}\n",
            Emulator::state_size()
        )
        .as_bytes(),
    )
}

// -------------------------------------------------------------------------------------------
// The backend configuration

/// `gameboy-backend-config-v1`: everything about the backend that is identity. The cartridge is
/// named by digest, never by path; the setup scaffold, the slots and the audio configuration are
/// the composition's `environment` block. Its canonical bytes are the `backendConfig` AssetRef.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendConfig {
    /// SHA-256 of the cartridge: the environment's `contentDigest` and the executor's `rom`.
    pub rom_digest: Digest,
    pub port_id: Id,
    pub slots: Vec<Id>,
    pub audio_sample_rate: u64,
    /// binjgb's audio buffer, in stereo frames (flysim: `DEFAULT_AUDIO_FRAMES`).
    pub audio_buffer_frames: u64,
    /// The declared setup scaffold. The legacy composition declares exactly one frame.
    pub setup_frames: u64,
}

impl BackendConfig {
    /// The legacy composition's configuration for a cartridge.
    pub fn legacy(rom_digest: &str) -> BackendConfig {
        BackendConfig {
            rom_digest: rom_digest.to_owned(),
            port_id: id(DEFAULT_PORT),
            slots: vec![id(DEFAULT_SLOT)],
            audio_sample_rate: DEFAULT_AUDIO_RATE,
            audio_buffer_frames: u64::from(DEFAULT_AUDIO_FRAMES),
            setup_frames: gameboy::SETUP_FRAMES,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "form": BACKEND_CONFIG_FORM,
            "romDigest": self.rom_digest,
            "portId": self.port_id,
            "slots": self.slots,
            "audio": {
                "sampleRate": self.audio_sample_rate,
                "channels": AUDIO_CHANNELS,
                "bufferFrames": self.audio_buffer_frames,
            },
            "setupFrames": self.setup_frames,
            "view": gameboy::VIEW_ID,
            "controllerSchema": gameboy::JOYPAD.id,
            "inspectionSchema": gameboy::MEMORY_INSPECTION.id,
        })
    }

    /// The canonical bytes (RFC 8785) the AssetRef digests.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonicalize(&self.to_json())
            .expect("a backend configuration canonicalizes")
            .into_bytes()
    }

    pub fn asset_ref(&self) -> AssetRef {
        let bytes = self.canonical_bytes();
        AssetRef {
            id: id("gameboy-backend-config"),
            digest: digest_of_bytes(&bytes),
            byte_length: bytes.len() as u64,
            format: id(BACKEND_CONFIG_FORM),
        }
    }

    /// Refuses a configuration this worker cannot be: another scaffold, no slot or too many, an
    /// audio configuration binjgb cannot run, a malformed digest.
    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(&self.rom_digest) {
            return Err("romDigest must be 64 lowercase hex digits".to_owned());
        }
        if !is_id(&self.port_id) {
            return Err("portId is not an Id".to_owned());
        }
        if self.setup_frames != gameboy::SETUP_FRAMES {
            return Err(format!(
                "the legacy composition declares a setup scaffold of exactly {} frame, not {}",
                gameboy::SETUP_FRAMES,
                self.setup_frames
            ));
        }
        if self.slots.is_empty() || self.slots.len() > MAX_SLOTS {
            return Err(format!("1..={MAX_SLOTS} slots"));
        }
        let unique: BTreeSet<&Id> = self.slots.iter().collect();
        if unique.len() != self.slots.len() || self.slots.iter().any(|s| !is_id(s)) {
            return Err("slots must be unique Ids".to_owned());
        }
        if !(8_000..=192_000).contains(&self.audio_sample_rate) {
            return Err("audio sampleRate must be 8000..=192000".to_owned());
        }
        if self.audio_buffer_frames == 0 || self.audio_buffer_frames > u64::from(u32::MAX) {
            return Err("audio bufferFrames must be a positive u32".to_owned());
        }
        Ok(())
    }
}

/// What one environment worker is started with. Everything crosses a process boundary as argv.
#[derive(Clone, Debug)]
pub struct LegacyEnvironmentConfig {
    pub session_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub worker_threads: usize,
    /// Where the cartridge is. Read-only, outside the checkout; never identity.
    pub rom_path: PathBuf,
    /// The backend this worker is: `Environment.Initialize` must name exactly this document, and
    /// the cartridge at `rom_path` must be the one it names.
    pub backend: BackendConfig,
}

// -------------------------------------------------------------------------------------------
// World state: what a capture carries

/// One slot: the emulator's exported state and the frame that was on screen, as the ratchet's
/// `Snapshot` holds them, plus the boundary it was saved at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotState {
    pub game: Vec<u8>,
    pub frame: Vec<u8>,
    pub boundary: u64,
}

/// The world at one committed boundary, as data. `State.Capture` encodes it, `State.StageRestore`
/// decodes it, and [`WorldState::from_flysim01`] / [`WorldState::flysim01_chunks`] convert its
/// FLYSIM01 half.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldState {
    pub rom_digest: Digest,
    pub episode_id: Id,
    pub boundary: u64,
    pub world_time: RationalNs,
    /// The legacy frame counter: `engineFrame`, FLYSIM01's `emulatorFrame`.
    pub engine_frame: u64,
    /// The mask on the joypad (FLYSIM01's `buttons`).
    pub buttons: u8,
    pub emulator: Vec<u8>,
    /// The frame on screen: the last one produced, or a restored slot's.
    pub framebuffer: Vec<u8>,
    /// Declared slots in composition order; a slot never saved is absent.
    pub slots: Vec<(Id, SlotState)>,
    /// Where the next audio chunk starts, relative to the episode's audio origin.
    pub audio_next_sample: u64,
}

/// The header of a capture payload: whose, which checkpoint, which boundary, which backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadHeader {
    pub worker_id: Id,
    pub checkpoint_id: Id,
    pub source_scope: Scope,
    /// The backend configuration digest the world ran under.
    pub configuration_digest: Digest,
}

/// The FLYSIM01 fields the world owns: the manifest scalars and the four chunks
/// (`crates/flysim/src/store.rs`: `romHash`, `emulatorFrame`, `buttons`; `emulator`,
/// `framebuffer`, `ratchetGame`, `ratchetFrame`). The agent's half is `agent_to_chunks`; the
/// task's is the adapter and ratchet state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flysim01World {
    pub rom_hash: String,
    pub emulator_frame: u64,
    pub buttons: u32,
    pub emulator: Vec<u8>,
    pub framebuffer: Vec<u8>,
    pub ratchet_game: Vec<u8>,
    pub ratchet_frame: Vec<u8>,
}

fn slot_chunk_names(index: usize) -> (String, String) {
    let letter = (b'A' + index as u8) as char;
    (format!("slotGame{letter}"), format!("slotFrame{letter}"))
}

fn to_core_json(value: &Value) -> JsonValue {
    JsonValue::parse(&value.to_string()).expect("serde_json output is JSON")
}

fn from_core_json(value: &JsonValue) -> Result<Value, String> {
    serde_json::from_str(&value.stringify()).map_err(|e| format!("manifest: {e}"))
}

impl WorldState {
    /// The world at the boundary `k = emulatorFrame - setupFrames` of a legacy checkpoint.
    ///
    /// FLYSIM01 carries the emulator, the frame on screen, the joypad, the frame counter and the
    /// ratchet's one slot, which is the composition's slot `best`. It carries no boundary number,
    /// world clock or audio position, because the legacy loop has none; they are derived from the
    /// frame counter, which advances exactly once per transition and never across a rollback, so
    /// `engineFrame = k + 1` holds for a legacy run from its fresh start on. The audio position is
    /// the sample of `worldTime` at the configured rate, rounded down; the first chunk after the
    /// restore marks the discontinuity, so no continuity is claimed across it.
    pub fn from_flysim01(
        bytes: &[u8],
        episode_id: &Id,
        slot_id: &Id,
        audio_sample_rate: u64,
    ) -> Result<WorldState, String> {
        let world = Flysim01World::decode(bytes)?;
        world.into_state(episode_id, slot_id, audio_sample_rate)
    }

    /// The world's half of a FLYSIM01 export: the composition's first slot is the ratchet's.
    pub fn flysim01_chunks(&self, slot_id: &Id) -> Flysim01World {
        let slot = self
            .slots
            .iter()
            .find(|(id, _)| id == slot_id)
            .map(|(_, s)| s);
        Flysim01World {
            rom_hash: self.rom_digest.clone(),
            emulator_frame: self.engine_frame,
            buttons: u32::from(self.buttons),
            emulator: self.emulator.clone(),
            framebuffer: self.framebuffer.clone(),
            ratchet_game: slot.map(|s| s.game.clone()).unwrap_or_default(),
            ratchet_frame: slot.map(|s| s.frame.clone()).unwrap_or_default(),
        }
    }

    /// Encodes the capture payload: flybrain-core's envelope with [`PAYLOAD_MAGIC`], the scalars
    /// in the manifest and every byte array as a chunk.
    pub fn encode(&self, header: &PayloadHeader) -> Vec<u8> {
        let slots: Vec<Value> = self
            .slots
            .iter()
            .map(
                |(slot_id, slot)| json!({"slotId": slot_id, "boundary": slot.boundary.to_string()}),
            )
            .collect();
        let manifest = json!({
            "payloadVersion": PAYLOAD_VERSION,
            "kind": "gameboy-environment",
            "workerId": header.worker_id,
            "checkpointId": header.checkpoint_id,
            "sourceScope": header.source_scope.to_json(),
            "configurationDigest": header.configuration_digest,
            "romDigest": self.rom_digest,
            "episodeId": self.episode_id,
            "committedStep": self.boundary.to_string(),
            "worldTime": self.world_time.to_json(),
            "engineFrame": self.engine_frame.to_string(),
            "buttons": self.buttons,
            "audioNextSample": self.audio_next_sample.to_string(),
            "slots": slots,
        });
        let mut chunks = vec![
            ("emulator".to_owned(), self.emulator.clone()),
            ("framebuffer".to_owned(), self.framebuffer.clone()),
        ];
        for (index, (_, slot)) in self.slots.iter().enumerate() {
            let (game, frame) = slot_chunk_names(index);
            chunks.push((game, slot.game.clone()));
            chunks.push((frame, slot.frame.clone()));
        }
        encode_envelope(PAYLOAD_MAGIC, &to_core_json(&manifest), &chunks)
            .expect("letter-only chunk names and an ASCII magic encode")
    }

    /// Decodes a capture payload. Shapes are checked here; identities by the caller.
    pub fn decode(bytes: &[u8]) -> Result<(PayloadHeader, WorldState), String> {
        let parts = decode_envelope(bytes, PAYLOAD_MAGIC).map_err(|e| e.to_string())?;
        let manifest = from_core_json(&parts.manifest)?;
        let text = |key: &str| -> Result<String, String> {
            manifest
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("the world payload has no {key}"))
        };
        let number = |key: &str| -> Result<u64, String> {
            text(key)?
                .parse::<u64>()
                .map_err(|_| format!("the world payload's {key} is not a U64"))
        };
        if manifest.get("payloadVersion").and_then(Value::as_u64) != Some(PAYLOAD_VERSION) {
            return Err("the world payload is another payload version".to_owned());
        }
        if text("kind")? != "gameboy-environment" {
            return Err("this payload is not a Game Boy world's state".to_owned());
        }
        let chunk = |name: &str| -> Result<Vec<u8>, String> {
            parts
                .chunk(name)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| format!("the world payload has no {name} chunk"))
        };
        let source_scope = Scope::from_json(
            manifest
                .get("sourceScope")
                .ok_or("the world payload has no sourceScope")?,
        )
        .map_err(|e| format!("sourceScope: {}", e.0))?;
        let world_time = RationalNs::from_json(
            manifest
                .get("worldTime")
                .ok_or("the world payload has no worldTime")?,
        )
        .map_err(|e| format!("worldTime: {}", e.0))?;
        let buttons = manifest
            .get("buttons")
            .and_then(Value::as_u64)
            .and_then(|b| u8::try_from(b).ok())
            .ok_or("the world payload's buttons are not a mask")?;
        let slot_list = manifest
            .get("slots")
            .and_then(Value::as_array)
            .ok_or("the world payload has no slot list")?;
        if slot_list.len() > MAX_SLOTS {
            return Err(format!(
                "the world payload holds more than {MAX_SLOTS} slots"
            ));
        }
        let mut slots = Vec::with_capacity(slot_list.len());
        for (index, entry) in slot_list.iter().enumerate() {
            let slot_id = entry
                .get("slotId")
                .and_then(Value::as_str)
                .ok_or("a captured slot has no slotId")?;
            let slot_id = parse_id(slot_id)?;
            let boundary = entry
                .get("boundary")
                .and_then(Value::as_str)
                .and_then(|b| b.parse::<u64>().ok())
                .ok_or("a captured slot has no boundary")?;
            let (game, frame) = slot_chunk_names(index);
            let slot = SlotState {
                game: chunk(&game)?,
                frame: chunk(&frame)?,
                boundary,
            };
            if slot.frame.len() != FRAMEBUFFER_LEN {
                return Err(format!(
                    "slot {slot_id}'s frame is not {FRAMEBUFFER_LEN} bytes"
                ));
            }
            slots.push((slot_id, slot));
        }
        let framebuffer = chunk("framebuffer")?;
        if framebuffer.len() != FRAMEBUFFER_LEN {
            return Err(format!("the framebuffer is not {FRAMEBUFFER_LEN} bytes"));
        }
        let header = PayloadHeader {
            worker_id: parse_id(&text("workerId")?)?,
            checkpoint_id: parse_id(&text("checkpointId")?)?,
            source_scope,
            configuration_digest: text("configurationDigest")?,
        };
        let state = WorldState {
            rom_digest: text("romDigest")?,
            episode_id: parse_id(&text("episodeId")?)?,
            boundary: number("committedStep")?,
            world_time,
            engine_frame: number("engineFrame")?,
            buttons,
            emulator: chunk("emulator")?,
            framebuffer,
            slots,
            audio_next_sample: number("audioNextSample")?,
        };
        Ok((header, state))
    }
}

/// The world time of boundary `k`: `k x stepDuration`, exactly.
pub fn world_time_at(boundary: u64) -> Result<RationalNs, String> {
    let step = gameboy::step_duration();
    RationalNs::reduced(
        u128::from(step.numerator) * u128::from(boundary),
        u128::from(step.denominator),
    )
    .map_err(|e| e.0)
}

/// The sample of `world_time` at `rate`, rounded down.
pub fn sample_at(world_time: &RationalNs, rate: u64) -> u64 {
    let numerator = u128::from(world_time.numerator) * u128::from(rate);
    let denominator = u128::from(world_time.denominator) * 1_000_000_000u128;
    (numerator / denominator) as u64
}

impl Flysim01World {
    /// Reads the world's fields of a FLYSIM01 envelope. The agent's chunks and the task's manifest
    /// entries are left to their owners.
    pub fn decode(bytes: &[u8]) -> Result<Flysim01World, String> {
        let parts = decode_envelope(bytes, FLYSIM01_MAGIC).map_err(|e| e.to_string())?;
        let manifest = &parts.manifest;
        let chunk = |name: &str| -> Result<Vec<u8>, String> {
            parts
                .chunk(name)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| format!("checkpoint is missing the {name} chunk"))
        };
        let number = |key: &str| -> Result<f64, String> {
            manifest
                .get(key)
                .and_then(JsonValue::as_f64)
                .ok_or_else(|| format!("checkpoint manifest is missing {key}"))
        };
        Ok(Flysim01World {
            rom_hash: manifest
                .get("romHash")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or("checkpoint manifest is missing romHash")?,
            emulator_frame: number("emulatorFrame")? as u64,
            buttons: number("buttons")? as u32,
            emulator: chunk("emulator")?,
            framebuffer: chunk("framebuffer")?,
            ratchet_game: chunk("ratchetGame")?,
            ratchet_frame: chunk("ratchetFrame")?,
        })
    }

    /// This world as the environment's state at `k = emulatorFrame - setupFrames`.
    pub fn into_state(
        self,
        episode_id: &Id,
        slot_id: &Id,
        audio_sample_rate: u64,
    ) -> Result<WorldState, String> {
        if self.framebuffer.len() != FRAMEBUFFER_LEN {
            return Err(format!(
                "checkpoint framebuffer is {} bytes",
                self.framebuffer.len()
            ));
        }
        if self.emulator_frame < gameboy::SETUP_FRAMES {
            return Err("checkpoint frame counter is before the setup scaffold".to_owned());
        }
        let buttons =
            u8::try_from(self.buttons).map_err(|_| "checkpoint buttons are not a mask")?;
        let boundary = self.emulator_frame - gameboy::SETUP_FRAMES;
        let world_time = world_time_at(boundary)?;
        let slots = if self.ratchet_game.is_empty() {
            Vec::new()
        } else {
            if self.ratchet_frame.len() != FRAMEBUFFER_LEN {
                return Err("the ratchet snapshot has no framebuffer".to_owned());
            }
            vec![(
                slot_id.clone(),
                SlotState {
                    game: self.ratchet_game,
                    frame: self.ratchet_frame,
                    boundary,
                },
            )]
        };
        Ok(WorldState {
            rom_digest: self.rom_hash,
            episode_id: episode_id.clone(),
            boundary,
            audio_next_sample: sample_at(&world_time, audio_sample_rate),
            world_time,
            engine_frame: self.emulator_frame,
            buttons,
            emulator: self.emulator,
            framebuffer: self.framebuffer,
            slots,
        })
    }

    /// The manifest entries and chunks this world contributes to a FLYSIM01 export, in
    /// `store::encode`'s order. The caller merges them with the agent's and the task's.
    pub fn manifest_entries(&self) -> Vec<(&'static str, JsonValue)> {
        vec![
            ("romHash", self.rom_hash.as_str().into()),
            ("emulatorFrame", (self.emulator_frame as f64).into()),
            ("buttons", f64::from(self.buttons).into()),
        ]
    }

    pub fn chunks(&self) -> Vec<(String, Vec<u8>)> {
        vec![
            ("emulator".to_owned(), self.emulator.clone()),
            ("framebuffer".to_owned(), self.framebuffer.clone()),
            ("ratchetGame".to_owned(), self.ratchet_game.clone()),
            ("ratchetFrame".to_owned(), self.ratchet_frame.clone()),
        ]
    }
}

// -------------------------------------------------------------------------------------------
// The worker

/// The live world: an emulator at a committed boundary.
struct Live {
    emulator: Emulator,
    epoch: Id,
    episode_id: Id,
    descriptor: EnvironmentDescriptor,
    boundary: u64,
    world_time: RationalNs,
    engine_frame: u64,
    buttons: u8,
    framebuffer: Vec<u8>,
    slots: BTreeMap<Id, SlotState>,
    /// Batch ids applied in this epoch.
    batches: BTreeSet<Id>,
    audio_next_sample: u64,
    /// The next chunk starts a new timeline: after a slot restore or a group restore.
    audio_discontinuity: bool,
}

/// A validated replacement world the live session cannot see yet, on a stopped emulator of its
/// own (`state-media-v1` section 5: "stage a stopped replacement emulator").
struct Staged {
    token: Id,
    checkpoint_id: Id,
    scope: Scope,
    emulator: Emulator,
    state: WorldState,
    descriptor: EnvironmentDescriptor,
}

/// The Game Boy environment endpoint.
pub struct LegacyGameboyEnvironment {
    config: LegacyEnvironmentConfig,
    status: StatusCell,
    live: Option<Live>,
    staged: Option<Staged>,
    activated: BTreeSet<Id>,
}

fn incompatible(message: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, message)
}

fn backend_failure(message: impl std::fmt::Display) -> DomainError {
    DomainError::new(
        ErrorCode::BackendFailure,
        message,
        MutationCertainty::Applied,
    )
}

impl LegacyGameboyEnvironment {
    pub fn new(config: LegacyEnvironmentConfig) -> LegacyGameboyEnvironment {
        LegacyGameboyEnvironment {
            config,
            status: StatusCell::new(),
            live: None,
            staged: None,
            activated: BTreeSet::new(),
        }
    }

    pub fn status(&self) -> StatusCell {
        self.status.clone()
    }

    /// The descriptor this worker advertises: built from its launch configuration alone, so a
    /// replacement computes the same one before it has run a frame.
    pub fn descriptor_for(backend: &BackendConfig) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            backend_digest: backend_digest(),
            content_digest: backend.rom_digest.clone(),
            configuration_digest: backend.asset_ref().digest,
            step_duration: gameboy::step_duration(),
            ports: vec![PortDescriptor {
                port_id: backend.port_id.clone(),
                controls: joypad_schema(),
            }],
            inspection_schema: gameboy::MEMORY_INSPECTION.schema_ref(),
            views: vec![view_descriptor()],
            audio: vec![audio_descriptor(backend.audio_sample_rate)],
            recovery: Recovery::ExactCheckpoint,
            determinism: Determinism::FixedBuild,
        }
    }

    fn build_descriptor(&self) -> DomainResult<EnvironmentDescriptor> {
        self.config
            .backend
            .validate()
            .map_err(|e| incompatible(format!("this worker's backend configuration: {e}")))?;
        let descriptor = LegacyGameboyEnvironment::descriptor_for(&self.config.backend);
        descriptor.validate().map_err(DomainError::invalid)?;
        Ok(descriptor)
    }

    /// The participant compatibility digest of a capture: the descriptor's identities under this
    /// worker's own state format.
    pub fn compatibility_digest(descriptor: &EnvironmentDescriptor) -> Digest {
        crate::state::Compatibility {
            state_format_id: id(STATE_FORMAT_ID),
            ..crate::state::Compatibility::of(descriptor)
        }
        .digest()
    }

    /// Boots the configured cartridge, refusing one that is not the cartridge the backend
    /// configuration names. Nothing has run on the emulator it returns.
    fn boot(&self) -> DomainResult<Emulator> {
        let rom = std::fs::read(&self.config.rom_path).map_err(|e| {
            DomainError::before(
                ErrorCode::BackendFailure,
                format!("the cartridge could not be read: {e}"),
            )
        })?;
        let actual = digest_of_bytes(&rom);
        if actual != self.config.backend.rom_digest {
            return Err(incompatible(format!(
                "the cartridge is {actual}, not the {} this backend configuration names",
                self.config.backend.rom_digest
            )));
        }
        let emulator = Emulator::new(
            &rom,
            self.config.backend.audio_sample_rate as u32,
            self.config.backend.audio_buffer_frames as u32,
        )
        .map_err(|e| DomainError::before(ErrorCode::BackendFailure, format!("booting: {e}")))?;
        if emulator.audio_frequency() as u64 != self.config.backend.audio_sample_rate {
            return Err(incompatible(format!(
                "binjgb runs its audio at {} Hz, not the configured {}",
                emulator.audio_frequency(),
                self.config.backend.audio_sample_rate
            )));
        }
        Ok(emulator)
    }

    fn check_session(&self, scope: &Scope) -> DomainResult<()> {
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this environment belongs to another session",
            ));
        }
        Ok(())
    }

    /// The live world at exactly the scope's epoch and boundary.
    fn live_at(&mut self, scope: &Scope, what: &str) -> DomainResult<&mut Live> {
        self.check_session(scope)?;
        let live = self.live.as_mut().ok_or_else(|| {
            DomainError::before(ErrorCode::InvalidPhase, "this environment is uninitialized")
        })?;
        if live.epoch != scope.epoch {
            return Err(DomainError::before(
                ErrorCode::StaleEpoch,
                format!("{what} names an epoch this environment is not in"),
            ));
        }
        if scope.step != live.boundary {
            return Err(DomainError::before(
                if scope.step < live.boundary {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                format!("{what} must name the boundary the world is at"),
            ));
        }
        Ok(live)
    }

    /// The observation of the live world: the frame on screen as one immutable artifact (sensory
    /// and broadcast), the memory image read now, and the audio chunk of the interval just run,
    /// if there was one.
    async fn observation(
        live: &mut Live,
        client: &flybus::Client,
        audio: Option<Vec<u8>>,
    ) -> DomainResult<(WorldObservation, Vec<(String, flybus::Artifact)>)> {
        let image = {
            let _span = crate::profile::span("env.image");
            memory_image(&mut live.emulator).map_err(backend_failure)?
        };
        if let Some(raw) = &audio
            && raw.len() % AUDIO_CHANNELS as usize != 0
        {
            return Err(backend_failure("binjgb returned half a stereo frame"));
        }
        let audio_f32 = audio.as_deref().map(audio_f32le);
        // The three artifacts are independent: they are sealed concurrently, so the boundary
        // waits for the router's round trips once rather than three times.
        let seal_span = crate::profile::span("env.seal");
        let (frame, memory, samples) = tokio::join!(
            media::seal_copy(client, FRAME_CONTENT_TYPE.to_owned(), &live.framebuffer),
            media::seal_copy(client, MEMORY_CONTENT_TYPE.to_owned(), &image),
            async {
                match &audio_f32 {
                    Some(bytes) => media::seal_copy(client, AUDIO_CONTENT_TYPE.to_owned(), bytes)
                        .await
                        .map(Some),
                    None => Ok(None),
                }
            }
        );
        drop(seal_span);
        let (frame, memory, samples) = (frame?, memory?, samples?);
        let view = ViewRef {
            view_id: gameboy::VIEW_ID.to_owned(),
            produced_step: live.boundary,
            pixels: frame.reference().clone(),
        };
        let inspection = MemoryInspection {
            memory: memory.reference().clone(),
            rom_digest: live.descriptor.content_digest.clone(),
        }
        .to_typed();
        let mut attachments = vec![
            (media::view_attachment(gameboy::VIEW_ID), frame),
            (MEMORY_ATTACHMENT.to_owned(), memory),
        ];
        let mut chunks = Vec::new();
        if let (Some(raw), Some(samples)) = (audio, samples) {
            let frames = (raw.len() / AUDIO_CHANNELS as usize) as u64;
            chunks.push(AudioRef {
                stream_id: id(AUDIO_STREAM_ID),
                first_sample: live.audio_next_sample,
                sample_frames: frames,
                samples: samples.reference().clone(),
                discontinuity: live.audio_discontinuity,
            });
            attachments.push((media::audio_attachment(AUDIO_STREAM_ID), samples));
            live.audio_next_sample = live
                .audio_next_sample
                .checked_add(frames)
                .ok_or_else(|| backend_failure("the audio position overflows"))?;
            live.audio_discontinuity = false;
        }
        let observation = WorldObservation {
            boundary: live.boundary,
            world_time: live.world_time,
            engine_frame: Some(live.engine_frame.to_string()),
            sensory_views: vec![view.clone()],
            inspection,
            broadcast_views: vec![view],
            audio: chunks,
        };
        observation
            .validate_against(&live.descriptor)
            .map_err(|e| {
                backend_failure(format!(
                    "the observation does not fit the descriptor: {}",
                    e.0
                ))
            })?;
        Ok((observation, attachments))
    }

    // -- Environment.Initialize --------------------------------------------------------------

    async fn initialize(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_session(&scope)?;
        if self.live.is_some() || self.staged.is_some() {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment is already initialized",
            ));
        }
        if scope.step != 0 {
            return Err(DomainError::before(
                ErrorCode::FutureStep,
                "Environment.Initialize uses the new epoch at step 0",
            ));
        }
        let params: EnvironmentInitializeParams = ctx.params()?;
        let descriptor = self.build_descriptor()?;
        let own = self.config.backend.asset_ref();
        if params.backend_config != own {
            return Err(incompatible(format!(
                "Environment.Initialize names the backend configuration {}, and this worker is {}",
                params.backend_config.digest, own.digest
            )));
        }
        match params.port_bindings.as_slice() {
            [(port_id, _agent)] if *port_id == self.config.backend.port_id => {}
            _ => {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!(
                        "the legacy composition binds exactly one port, {}",
                        self.config.backend.port_id
                    ),
                ));
            }
        }
        let mut emulator = self.boot()?;
        // The declared setup scaffold: one frame with no button down, attributed to no fly
        // (`LegacyFrame::initialize`). Its audio is not published; the audio origin is the first
        // sample of transition 0 -> 1.
        emulator
            .run_frame()
            .map_err(|e| backend_failure(format!("running the setup frame: {e}")))?;
        let framebuffer = emulator.framebuffer().to_vec();
        let _setup_audio = emulator.take_audio_u8();
        let mut live = Live {
            emulator,
            epoch: scope.epoch.clone(),
            episode_id: params.episode_id.clone(),
            descriptor: descriptor.clone(),
            boundary: 0,
            world_time: RationalNs::ZERO,
            engine_frame: gameboy::SETUP_FRAMES,
            buttons: 0,
            framebuffer,
            slots: BTreeMap::new(),
            batches: BTreeSet::new(),
            audio_next_sample: 0,
            audio_discontinuity: false,
        };
        let (observation, attachments) =
            LegacyGameboyEnvironment::observation(&mut live, ctx.client, None).await?;
        self.live = Some(live);
        // The world is stopped when O[0] goes out and cannot free-run while the brain warms up.
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope));
        self.status.progress(1);
        let result = EnvironmentInitializeResult {
            descriptor,
            observation,
        };
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    // -- Environment.Advance -----------------------------------------------------------------

    async fn advance(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        let params: AdvanceParams = ctx.params()?;
        let status = self.status.clone();
        let live = self.live_at(&scope, "Environment.Advance")?;
        if live.batches.contains(&params.batch_id) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!(
                    "batch {} was already applied in this epoch",
                    params.batch_id
                ),
            ));
        }
        // One complete batch: the one port, every button in descriptor order, no axes.
        match params.controls.as_slice() {
            [control] if control.port_id == live.descriptor.ports[0].port_id => {
                control
                    .validate_against(&live.descriptor.ports[0].controls)
                    .map_err(|e| DomainError::invalid(e.0))?;
            }
            _ => {
                return Err(DomainError::invalid(format!(
                    "the batch must hold exactly one control, for port {}",
                    live.descriptor.ports[0].port_id
                )));
            }
        }
        let mask = mask_of(&params.controls[0]);
        let digest = controls_digest(&params.controls);
        let applied_from = live.boundary;

        // `LegacyFrame::run` and `take_frame`: the joypad, one frame, the frame it drew, its audio.
        let frame_span = crate::profile::span("env.frame");
        live.buttons = mask;
        live.emulator.set_buttons(mask);
        live.emulator
            .run_frame()
            .map_err(|e| backend_failure(format!("frame {}: {e}", live.engine_frame + 1)))?;
        live.engine_frame += 1;
        live.framebuffer
            .copy_from_slice(live.emulator.framebuffer());
        let audio = live.emulator.take_audio_u8();
        drop(frame_span);
        live.boundary += 1;
        live.world_time = live
            .world_time
            .checked_add(&live.descriptor.step_duration)
            .map_err(|e| backend_failure(e.0))?;
        live.batches.insert(params.batch_id.clone());
        status.set_batch(params.batch_id.clone());
        status.set_scope(Some(scope_at(
            &scope.session_id,
            &scope.epoch,
            live.boundary,
        )));
        status.progress(1);

        let (observation, attachments) =
            LegacyGameboyEnvironment::observation(live, ctx.client, Some(audio)).await?;
        let result = StepResult {
            batch_id: params.batch_id,
            applied_from_step: applied_from,
            next_step: live.boundary,
            applied_controls_digest: digest,
            observation,
        };
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    // -- gameboy-slots-v1 --------------------------------------------------------------------

    async fn save_slot(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        let params: SaveSlotParams = ctx.params()?;
        let declared = self.config.backend.slots.contains(&params.slot_id);
        let status = self.status.clone();
        let live = self.live_at(&scope, METHOD_SAVE_SLOT)?;
        if !declared {
            return Err(DomainError::invalid(format!(
                "slot {} is not a slot this composition declares",
                params.slot_id
            )));
        }
        status.set_state(WorkerState::Capturing);
        // The ratchet's capture: the exported state and the frame on screen.
        let game = live.emulator.export_state().map_err(|e| {
            status.set_state(WorkerState::Ready);
            DomainError::before(
                ErrorCode::BackendFailure,
                format!("exporting the state: {e}"),
            )
        })?;
        let result = SaveSlotResult {
            slot_id: params.slot_id.clone(),
            boundary: live.boundary,
            state_digest: digest_of_bytes(&game),
            byte_length: game.len() as u64,
        };
        live.slots.insert(
            params.slot_id,
            SlotState {
                game,
                frame: live.framebuffer.clone(),
                boundary: live.boundary,
            },
        );
        status.set_state(WorkerState::Ready);
        status.progress(1);
        result
            .validate_against_scope(&scope)
            .map_err(|e| DomainError::invalid(e.0))?;
        Ok(HandlerReply::from(&result))
    }

    async fn restore_slot(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_session(&scope)?;
        let params: RestoreSlotParams = ctx.params()?;
        params
            .validate_against_scope(&scope)
            .map_err(|e| DomainError::invalid(e.0))?;
        let status = self.status.clone();
        let prior = scope_at(&scope.session_id, &params.prior_epoch, scope.step);
        let live = self.live_at(&prior, METHOD_RESTORE_SLOT)?;
        let slot = live.slots.get(&params.slot_id).cloned().ok_or_else(|| {
            DomainError::before(
                ErrorCode::InvalidPhase,
                format!("slot {} holds nothing to restore", params.slot_id),
            )
        })?;
        if slot.frame.len() != FRAMEBUFFER_LEN {
            return Err(incompatible("the slot's frame is not a 160x144 RGBA frame"));
        }
        status.set_state(WorkerState::Restoring);
        // `recover_game` then `LegacyFrame::rollback`: import the slot, release every button
        // (both calls, as flysim makes them), and the slot's frame is the frame on screen. No
        // frame runs.
        live.emulator.import_state(&slot.game).map_err(|e| {
            DomainError::new(
                ErrorCode::BackendFailure,
                format!("restoring slot {}: {e}", params.slot_id),
                MutationCertainty::Unknown,
            )
        })?;
        live.emulator.set_buttons(0);
        live.framebuffer.copy_from_slice(&slot.frame);
        live.buttons = 0;
        live.emulator.set_buttons(0);
        live.epoch = scope.epoch.clone();
        live.batches.clear();
        live.audio_discontinuity = true;
        let (observation, attachments) =
            LegacyGameboyEnvironment::observation(live, ctx.client, None).await?;
        status.set_state(WorkerState::Ready);
        status.set_scope(Some(scope.clone()));
        status.progress(1);
        let result = RestoreSlotResult {
            slot_id: params.slot_id,
            committed_step: scope.step,
            observation,
        };
        result
            .validate_against_scope(&scope)
            .map_err(|e| DomainError::new(ErrorCode::Internal, e.0, MutationCertainty::Applied))?;
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    // -- State.Capture / StageRestore / ActivateRestore --------------------------------------

    async fn state_capture(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        let params: CaptureParams = ctx.params()?;
        let worker_id = self.config.worker_id.clone();
        let slot_order = self.config.backend.slots.clone();
        let status = self.status.clone();
        let live = self.live_at(&scope, "State.Capture")?;
        let previous = status.state();
        status.set_state(WorkerState::Capturing);
        let emulator = live.emulator.export_state().map_err(|e| {
            status.set_state(previous);
            DomainError::before(
                ErrorCode::BackendFailure,
                format!("exporting the state: {e}"),
            )
        })?;
        let state = WorldState {
            rom_digest: live.descriptor.content_digest.clone(),
            episode_id: live.episode_id.clone(),
            boundary: live.boundary,
            world_time: live.world_time,
            engine_frame: live.engine_frame,
            buttons: live.buttons,
            emulator,
            framebuffer: live.framebuffer.clone(),
            slots: slot_order
                .iter()
                .filter_map(|slot_id| {
                    live.slots
                        .get(slot_id)
                        .map(|s| (slot_id.clone(), s.clone()))
                })
                .collect(),
            audio_next_sample: live.audio_next_sample,
        };
        let header = PayloadHeader {
            worker_id,
            checkpoint_id: params.checkpoint_id.clone(),
            source_scope: scope.clone(),
            configuration_digest: live.descriptor.configuration_digest.clone(),
        };
        let bytes = state.encode(&header);
        let digest = digest_of_bytes(&bytes);
        let artifact = crate::state::seal_payload(ctx.client, &bytes, &digest).await?;
        status.set_state(previous);
        let result = CaptureResult {
            checkpoint_id: params.checkpoint_id,
            boundary: live.boundary,
            compatibility_digest: LegacyGameboyEnvironment::compatibility_digest(&live.descriptor),
            payload: artifact.reference().clone(),
        };
        Ok(HandlerReply::with_artifacts(
            object(result.to_json()),
            vec![(crate::state::PAYLOAD_ATTACHMENT.to_owned(), artifact)],
        ))
    }

    async fn state_stage_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_session(&scope)?;
        if self.live.is_some() {
            // A world already running a boundary is not a quiescent replacement: the group
            // replaces it rather than restoring over a live one.
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "State.StageRestore needs an uninitialized replacement environment",
            ));
        }
        if let Some(staged) = &self.staged {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!(
                    "this environment already holds the staged restore {} for checkpoint {}",
                    staged.token, staged.checkpoint_id
                ),
            ));
        }
        let params: StageRestoreParams = ctx.params()?;
        if params.source_scope.step != scope.step {
            return Err(DomainError::invalid(
                "State.StageRestore's scope step must be the source boundary",
            ));
        }
        if params.source_scope.epoch == scope.epoch {
            return Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "State.StageRestore proposes the epoch the checkpoint was captured in",
            ));
        }
        let artifact = ctx.artifact(crate::state::PAYLOAD_ATTACHMENT)?;
        if artifact.reference() != &params.payload {
            return Err(DomainError::before(
                ErrorCode::BufferInvalid,
                "the staged payload attachment is not the artifact the request names",
            ));
        }
        let bytes = artifact.read_all().await.map_err(|e| {
            DomainError::before(
                ErrorCode::BufferInvalid,
                format!("the staged payload could not be read: {}", e.message),
            )
        })?;
        let declared = params
            .payload
            .digest
            .clone()
            .ok_or_else(|| incompatible("a checkpoint payload must carry a content digest"))?;
        let actual = digest_of_bytes(&bytes);
        if actual != declared || bytes.len() as u64 != params.payload.byte_length {
            return Err(incompatible(
                "the staged payload is not the content the request declares",
            ));
        }
        let (header, state) = WorldState::decode(&bytes).map_err(incompatible)?;
        if header.worker_id != self.config.worker_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the staged payload belongs to another world",
            ));
        }
        if header.checkpoint_id != params.checkpoint_id {
            return Err(incompatible(
                "the staged payload belongs to another checkpoint",
            ));
        }
        if header.source_scope != params.source_scope || state.boundary != scope.step {
            return Err(incompatible(
                "the staged payload was captured at another source scope",
            ));
        }
        let descriptor = self.build_descriptor()?;
        if header.configuration_digest != descriptor.configuration_digest {
            return Err(incompatible(
                "the staged world was captured under another backend configuration",
            ));
        }
        if state.rom_digest != descriptor.content_digest {
            return Err(incompatible(format!(
                "the staged world runs the cartridge {}, not this backend's {}",
                state.rom_digest, descriptor.content_digest
            )));
        }
        let expected = LegacyGameboyEnvironment::compatibility_digest(&descriptor);
        if expected != params.compatibility_digest {
            return Err(incompatible(format!(
                "the staged world's compatibility {expected} is not the {} the restore requires",
                params.compatibility_digest
            )));
        }
        for (slot_id, _) in &state.slots {
            if !self.config.backend.slots.contains(slot_id) {
                return Err(incompatible(format!("slot {slot_id} is not declared here")));
            }
        }
        if state.world_time != world_time_at(state.boundary).map_err(incompatible)? {
            return Err(incompatible("the staged world clock is not its boundary's"));
        }
        // Validation that needs a mutation happens on a stopped replacement emulator: the
        // import either succeeds there or the live nothing is touched.
        let mut emulator = self.boot()?;
        emulator
            .import_state(&state.emulator)
            .map_err(|e| incompatible(format!("the emulator state does not load: {e}")))?;
        let token = crate::agent::restore_token(
            &params.checkpoint_id,
            &scope,
            &actual,
            &self.config.incarnation_id,
        );
        if self.activated.contains(&token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this exact restore was already activated on this environment",
            ));
        }
        self.staged = Some(Staged {
            token: token.clone(),
            checkpoint_id: params.checkpoint_id.clone(),
            scope,
            emulator,
            state,
            descriptor,
        });
        self.status.set_state(WorkerState::StagedRestore);
        let result = StageRestoreResult {
            checkpoint_id: params.checkpoint_id,
            restore_token: token,
        };
        Ok(HandlerReply::from(&result))
    }

    /// Installs the staged world and returns its observation: nothing runs, the view is the saved
    /// frame, the memory image is read after the import, and there is no audio chunk; the next
    /// one marks the discontinuity. The legacy restore's joypad write is repeated here
    /// (`LegacyFrame::restore`: `set_buttons(runtime.buttons)`).
    async fn state_activate_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let params: ActivateRestoreParams = ctx.params()?;
        if self.activated.contains(&params.restore_token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this restore token has already been activated",
            ));
        }
        let Some(staged) = self.staged.take() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment holds no staged restore",
            ));
        };
        if staged.token != params.restore_token {
            let token = staged.token.clone();
            self.staged = Some(staged);
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "this environment's staged restore is {token}, not {}",
                    params.restore_token
                ),
            ));
        }
        self.status.set_state(WorkerState::Restoring);
        let Staged {
            token,
            checkpoint_id,
            scope,
            mut emulator,
            state,
            descriptor,
        } = staged;
        emulator.set_buttons(state.buttons);
        let mut live = Live {
            emulator,
            epoch: scope.epoch.clone(),
            episode_id: state.episode_id,
            descriptor,
            boundary: state.boundary,
            world_time: state.world_time,
            engine_frame: state.engine_frame,
            buttons: state.buttons,
            framebuffer: state.framebuffer,
            slots: state.slots.into_iter().collect(),
            batches: BTreeSet::new(),
            audio_next_sample: state.audio_next_sample,
            audio_discontinuity: true,
        };
        let (observation, attachments) =
            LegacyGameboyEnvironment::observation(&mut live, ctx.client, None).await?;
        self.live = Some(live);
        self.activated.insert(token);
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope.clone()));
        let result = ActivateRestoreResult {
            committed_step: scope.step,
            checkpoint_id,
            observation: Some(observation),
        };
        result
            .validate_for_role(Role::Environment)
            .map_err(|e| DomainError::invalid(e.0))?;
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }
}

impl WorkerEndpoint for LegacyGameboyEnvironment {
    fn worker_id(&self) -> Id {
        self.config.worker_id.clone()
    }

    fn incarnation_id(&self) -> Id {
        self.config.incarnation_id.clone()
    }

    fn session_id(&self) -> Id {
        self.config.session_id.clone()
    }

    fn role(&self) -> Role {
        Role::Environment
    }

    fn capabilities(&self) -> Vec<Id> {
        capabilities()
    }

    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }

    fn worker_threads(&self) -> u64 {
        self.config.worker_threads as u64
    }

    fn methods(&self) -> Vec<&'static str> {
        vec![
            "Environment.Initialize",
            "Environment.Advance",
            METHOD_SAVE_SLOT,
            METHOD_RESTORE_SLOT,
            "State.Capture",
            "State.StageRestore",
            "State.ActivateRestore",
        ]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            match ctx.method {
                "Environment.Initialize" => self.initialize(&ctx).await,
                "Environment.Advance" => self.advance(&ctx).await,
                METHOD_SAVE_SLOT => self.save_slot(&ctx).await,
                METHOD_RESTORE_SLOT => self.restore_slot(&ctx).await,
                "State.Capture" => self.state_capture(&ctx).await,
                "State.StageRestore" => self.state_stage_restore(&ctx).await,
                "State.ActivateRestore" => self.state_activate_restore(&ctx).await,
                other => Err(DomainError::before(
                    ErrorCode::Unsupported,
                    format!("{other} is not a Game Boy environment method"),
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mask_and_the_control_are_one_order() {
        for mask in 0..=u8::MAX {
            let control = control_of("p1", mask);
            control
                .validate_against(&joypad_schema())
                .expect("complete");
            assert_eq!(mask_of(&control), mask);
        }
        // Bit order is flybrain-gb's.
        assert_eq!(
            mask_of(&control_of("p1", flybrain_gb::emulator::buttons::START)),
            1 << 6
        );
        assert_eq!(gameboy::GAMEBOY_BUTTONS[6], "start");
    }

    #[test]
    fn audio_is_sample_over_255_with_silence_at_zero() {
        let bytes = audio_f32le(&[0, 255, 51]);
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(values, vec![0.0, 1.0, 51.0 / 255.0]);
    }

    #[test]
    fn the_world_clock_is_the_boundary_times_the_frame() {
        assert_eq!(world_time_at(0).unwrap(), RationalNs::ZERO);
        let mut acc = RationalNs::ZERO;
        for k in 1..=1000u64 {
            acc = acc.checked_add(&gameboy::step_duration()).unwrap();
            assert_eq!(world_time_at(k).unwrap(), acc);
        }
        // 1000 frames at 48 kHz: 16742.68 ms -> 803,649 whole samples.
        assert_eq!(sample_at(&world_time_at(1000).unwrap(), 48_000), 803_649);
    }

    #[test]
    fn the_backend_configuration_refuses_another_scaffold_and_bad_slots() {
        let good = BackendConfig::legacy(&"ab".repeat(32));
        good.validate().unwrap();
        let mut two = good.clone();
        two.setup_frames = 2;
        assert!(two.validate().unwrap_err().contains("setup scaffold"));
        let mut none = good.clone();
        none.slots.clear();
        assert!(none.validate().is_err());
        let mut five = good.clone();
        five.slots = (0..5).map(|i| id(&format!("s{i}"))).collect();
        assert!(five.validate().is_err());
        // The digest moves with every identity field.
        let mut rate = good.clone();
        rate.audio_sample_rate = 44_100;
        assert_ne!(rate.asset_ref().digest, good.asset_ref().digest);
    }

    #[test]
    fn the_payload_round_trips_and_the_flysim01_half_is_the_legacy_layout() {
        let slot = SlotState {
            game: vec![7; 33],
            frame: vec![9; FRAMEBUFFER_LEN],
            boundary: 41,
        };
        let state = WorldState {
            rom_digest: "cd".repeat(32),
            episode_id: id("ep1"),
            boundary: 99,
            world_time: world_time_at(99).unwrap(),
            engine_frame: 100,
            buttons: 0b1001_0000,
            emulator: vec![1, 2, 3],
            framebuffer: vec![4; FRAMEBUFFER_LEN],
            slots: vec![(id("best"), slot.clone())],
            audio_next_sample: 12_345,
        };
        let header = PayloadHeader {
            worker_id: id("world"),
            checkpoint_id: id("c1"),
            source_scope: scope_at("s", "e1", 99),
            configuration_digest: "ef".repeat(32),
        };
        let bytes = state.encode(&header);
        assert_eq!(WorldState::decode(&bytes).unwrap(), (header, state.clone()));

        // The FLYSIM01 half, through flybrain-core's envelope with flysim's chunk names.
        let world = state.flysim01_chunks(&id("best"));
        let mut manifest = JsonValue::object();
        for (key, value) in world.manifest_entries() {
            manifest.set(key, value);
        }
        let envelope = encode_envelope(FLYSIM01_MAGIC, &manifest, &world.chunks()).unwrap();
        let back = WorldState::from_flysim01(&envelope, &id("ep1"), &id("best"), 48_000).unwrap();
        assert_eq!(back.boundary, 99);
        assert_eq!(back.engine_frame, 100);
        assert_eq!(back.buttons, state.buttons);
        assert_eq!(back.emulator, state.emulator);
        assert_eq!(back.framebuffer, state.framebuffer);
        assert_eq!(
            back.slots,
            vec![(
                id("best"),
                SlotState {
                    boundary: 99,
                    ..slot
                }
            )]
        );
        assert_eq!(
            back.audio_next_sample,
            sample_at(&world_time_at(99).unwrap(), 48_000)
        );
    }
}
