//! ENV-01's parity harness: recorded button inputs, a reference that drives the emulator directly
//! the way flysim's `LegacyFrame` does, the same inputs driven through a launched
//! [`LegacyGameboyEnvironment`] over the bus, and a record-by-record comparison.
//!
//! ```text
//! EnvScript ──> DirectEmulator (flybrain-gb Emulator in LegacyFrame order) ──> Vec<EnvRecord>
//!           ──> EnvDriver (Environment.* / State.* RPCs to a launched worker) ──> Vec<EnvRecord>
//!                                                                              └─ compare() ─┘
//! FlyTrace (FND-01 FLY_TRACE of the running service) ──> replay_trace() through the worker
//! ```
//!
//! An [`EnvRecord`] is everything both sides can observe of the world at one boundary: the
//! boundary, the engine frame and the world clock, the mask that produced it, the digest of the
//! frame on screen, of the whole 64 KiB memory image and of its WRAM window (the field FND-01's
//! trace records), the audio chunk (position, length, digest of the `f32le` bytes, discontinuity),
//! and at a slot save or a capture the digest of the exported emulator state. Equality of the
//! state digest is equality of every byte binjgb saves: CPU, memory, PPU, APU, timers, joypad.
//!
//! Three sources of recorded inputs:
//!
//! - [`toy_cart`]: a cartridge this module assembles (a few hundred bytes of our own code, no
//!   game content) that reads the joypad and folds it into WRAM, VRAM, scroll, palette and a
//!   square-wave channel, so frames, memory and audio all depend on the inputs. It runs with no
//!   ROM installed; the committed goldens are its records.
//! - a `FLYSIM01` checkpoint with the real cartridge (`FLY_ROM`, rom-env), driven by a seeded
//!   button walk with slot saves, rollbacks and a capture/restore;
//! - [`FlyTrace`]: a `FLY_TRACE` written by the running service (FND-01), whose masks and
//!   boundary actions are replayed from its start checkpoint, and whose recorded frame, WRAM and
//!   slot-state digests the worker must reproduce. This is "legacy fixtures unchanged" measured
//!   against the service itself rather than against a transcription of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fly_session_types::extensions::{
    METHOD_RESTORE_SLOT, METHOD_SAVE_SLOT, ROLLBACK_POLICY, RestoreSlotParams, RestoreSlotResult,
    SaveSlotParams, SaveSlotResult,
};
use fly_session_types::gameboy::{self, MemoryInspection};
use flybrain_gb::emulator::{Emulator, FRAMEBUFFER_LEN};
use flybus::{Client, Grants, Pattern, Policy, Router, RouterConfig};
use serde_json::{Value, json};

use crate::launcher::{
    ExecutionMode, Launcher, LegacyEnvironmentLaunch, SUPERVISOR_CLIENT, ThreadBudget, Via,
};
use crate::legacy_env::{
    self, AUDIO_STREAM_ID, BackendConfig, MEMORY_ATTACHMENT, PayloadHeader, SlotState, WorldState,
    audio_f32le, control_of, memory_image, world_time_at,
};
use crate::media::{AudioTimelines, ObservationOrigin, check_required_audio, check_required_views};
use crate::rpc::{Serials, WorkerRef, call};
use crate::types::*;

/// The WRAM window FND-01's trace digests (`crates/flysim/src/trace.rs`).
pub const WRAM: std::ops::RangeInclusive<usize> = 0xc000..=0xdfff;

// -------------------------------------------------------------------------------------------
// The toy cartridge

pub mod toy_cart {
    //! A 32 KiB cartridge assembled here: our own program, no game content, nothing committed but
    //! this code. binjgb runs it without a boot ROM from the post-boot state (LCD on, BG from
    //! tile data at `$8000`, map at `$9800`).
    //!
    //! Once per frame, at the start of vertical blank, it reads the joypad into `$C000`, counts
    //! frames in `$C001`, accumulates the pad into `$C002`, scrolls the background by both, writes
    //! the accumulator into one byte of tile 0 (which the whole background map shows), sets the
    //! palette from the pad, and triggers square channel 1 at a pitch set by the pad whenever a
    //! button is down. Frames, memory and audio therefore all depend on the recorded inputs.

    /// The title bytes at `$0134`; a variant changes the last one, so its digest differs.
    const TITLE: &[u8] = b"FLYTOYCART";

    struct Asm {
        out: Vec<u8>,
    }

    impl Asm {
        fn here(&self) -> usize {
            self.out.len()
        }
        fn emit(&mut self, bytes: &[u8]) {
            self.out.extend_from_slice(bytes);
        }
        /// `jr cc, target` with `opcode` 0x18 (always), 0x20 (nz) or 0x28 (z).
        fn jr_to(&mut self, opcode: u8, target: usize) {
            let next = self.here() as i64 + 2;
            let offset = target as i64 - next;
            assert!((-128..=127).contains(&offset), "jr out of range");
            self.emit(&[opcode, offset as i8 as u8]);
        }
        /// A forward `jr cc` whose target is patched later; returns the offset byte's index.
        fn jr_forward(&mut self, opcode: u8) -> usize {
            self.emit(&[opcode, 0]);
            self.here() - 1
        }
        fn patch_forward(&mut self, at: usize) {
            let offset = self.here() as i64 - (at as i64 + 1);
            assert!((0..=127).contains(&offset));
            self.out[at] = offset as u8;
        }
    }

    fn program() -> Vec<u8> {
        let mut a = Asm { out: Vec::new() };
        a.emit(&[0xf3]); // di
        a.emit(&[0x31, 0xfe, 0xff]); // ld sp, $fffe
        a.emit(&[0x3e, 0x80, 0xe0, 0x26]); // NR52: sound on
        a.emit(&[0x3e, 0x77, 0xe0, 0x24]); // NR50: full volume both sides
        a.emit(&[0x3e, 0xff, 0xe0, 0x25]); // NR51: every channel both sides
        let wait_vblank = a.here();
        a.emit(&[0xf0, 0x44, 0xfe, 0x90]); // ldh a,[LY]; cp 144
        a.jr_to(0x20, wait_vblank); // jr nz
        // The directions, then the buttons, active low.
        a.emit(&[0x3e, 0x20, 0xe0, 0x00, 0xf0, 0x00, 0xf0, 0x00]); // select d-pad, read twice
        a.emit(&[0x2f, 0xe6, 0x0f, 0xcb, 0x37, 0x47]); // cpl; and $0f; swap a; ld b,a
        a.emit(&[0x3e, 0x10, 0xe0, 0x00, 0xf0, 0x00, 0xf0, 0x00]); // select buttons, read twice
        a.emit(&[0x2f, 0xe6, 0x0f, 0xb0]); // cpl; and $0f; or b
        a.emit(&[0xea, 0x00, 0xc0, 0x47]); // ld [$c000],a; ld b,a
        a.emit(&[0x21, 0x01, 0xc0, 0x34]); // ld hl,$c001; inc [hl]
        a.emit(&[0xfa, 0x02, 0xc0, 0x80, 0xea, 0x02, 0xc0]); // $c002 += pad
        a.emit(&[0xe0, 0x43]); // SCX = accumulator
        a.emit(&[0xfa, 0x01, 0xc0, 0xe0, 0x42]); // SCY = frame count
        a.emit(&[0xe6, 0x0f, 0x6f, 0x26, 0x80]); // hl = $8000 + (count & 15)
        a.emit(&[0xfa, 0x02, 0xc0, 0x77]); // [hl] = accumulator
        a.emit(&[0x78, 0xee, 0xe4, 0xe0, 0x47]); // BGP = pad ^ $e4
        a.emit(&[0x78, 0xa7]); // ld a,b; and a
        let skip = a.jr_forward(0x28); // jr z, no sound
        a.emit(&[0x3e, 0x80, 0xe0, 0x11]); // NR11: 50% duty
        a.emit(&[0x3e, 0xf0, 0xe0, 0x12]); // NR12: volume 15, no envelope
        a.emit(&[0x78, 0xe0, 0x13]); // NR13: pitch low = pad
        a.emit(&[0x3e, 0x87, 0xe0, 0x14]); // NR14: trigger, pitch high 7
        a.patch_forward(skip);
        let wait_out = a.here();
        a.emit(&[0xf0, 0x44, 0xfe, 0x90]); // ldh a,[LY]; cp 144
        a.jr_to(0x28, wait_out); // jr z: leave vertical blank first
        a.jr_to(0x18, wait_vblank);
        a.out
    }

    /// The cartridge. `variant` 0 is the one the goldens run; any other is another cartridge
    /// with another digest (for the wrong-ROM refusals).
    pub fn rom(variant: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]); // nop; jp $0150
        rom[0x134..0x134 + TITLE.len()].copy_from_slice(TITLE);
        rom[0x134 + TITLE.len()] = b'0' + (variant % 10);
        rom[0x147] = 0x00; // ROM only
        rom[0x148] = 0x00; // 32 KiB
        rom[0x149] = 0x00; // no RAM
        rom[0x14a] = 0x01;
        let program = program();
        rom[0x150..0x150 + program.len()].copy_from_slice(&program);
        let mut header = 0u8;
        for byte in &rom[0x134..0x14d] {
            header = header.wrapping_sub(*byte).wrapping_sub(1);
        }
        rom[0x14d] = header;
        let global: u16 = rom
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 0x14e && *i != 0x14f)
            .fold(0u16, |sum, (_, b)| sum.wrapping_add(u16::from(*b)));
        rom[0x14e..0x150].copy_from_slice(&global.to_be_bytes());
        rom
    }
}

// -------------------------------------------------------------------------------------------
// Scripts and records

/// One operation of a script, applied at the boundary the world is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvOp {
    /// One transition with this joypad mask.
    Advance(u8),
    /// `Environment.SaveSlot` at the committed boundary.
    SaveSlot(Id),
    /// `Environment.RestoreSlot` under a new epoch (`legacy-ratchet-rollback-v1`).
    Rollback(Id),
    /// `State.Capture`, then a group restore of that payload into a fresh replacement worker
    /// under a new epoch (a restart of the service, `legacy-transient-reset`).
    CaptureRestore,
}

/// Where a script starts.
#[derive(Clone, Debug)]
pub enum EnvStart {
    /// `Environment.Initialize`: the one-frame setup scaffold.
    Fresh,
    /// A `FLYSIM01` checkpoint's world, restored into a fresh worker.
    Flysim01(Arc<Vec<u8>>),
}

#[derive(Clone, Debug)]
pub struct EnvScript {
    pub name: String,
    pub start: EnvStart,
    pub ops: Vec<EnvOp>,
}

/// One audio chunk as both sides see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRecord {
    pub first_sample: u64,
    pub frames: u64,
    pub digest: Digest,
    pub discontinuity: bool,
}

/// What both sides observe of the world after one operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvRecord {
    pub index: u64,
    pub op: &'static str,
    pub boundary: u64,
    pub engine_frame: u64,
    pub world_time: RationalNs,
    pub mask: Option<u8>,
    pub frame: Digest,
    pub memory: Digest,
    pub wram: Digest,
    pub audio: Option<AudioRecord>,
    /// The exported emulator state: at a slot save, the slot's; at a capture, the world's.
    pub state: Option<Digest>,
}

impl EnvRecord {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "op": self.op,
            "boundary": self.boundary.to_string(),
            "engineFrame": self.engine_frame.to_string(),
            "worldTime": self.world_time.to_json(),
            "mask": self.mask,
            "frame": self.frame,
            "memory": self.memory,
            "wram": self.wram,
            "audio": self.audio.as_ref().map(|a| json!({
                "firstSample": a.first_sample.to_string(),
                "frames": a.frames,
                "digest": a.digest,
                "discontinuity": a.discontinuity,
            })),
            "state": self.state,
        })
    }

    pub fn row(&self) -> String {
        format!(
            "#{} {} k={} f={} mask={:?} frame={} mem={} audio={:?} state={:?}",
            self.index,
            self.op,
            self.boundary,
            self.engine_frame,
            self.mask,
            &self.frame[..12],
            &self.memory[..12],
            self.audio.as_ref().map(|a| (
                a.first_sample,
                a.frames,
                a.digest[..12].to_owned(),
                a.discontinuity
            )),
            self.state.as_ref().map(|s| s[..12].to_owned()),
        )
    }
}

/// The first difference between two record lists, or none.
pub fn compare(left: &[EnvRecord], right: &[EnvRecord]) -> Result<(), String> {
    for (a, b) in left.iter().zip(right) {
        if a != b {
            return Err(format!(
                "records differ at #{}:\n  left:  {}\n  right: {}",
                a.index,
                a.row(),
                b.row()
            ));
        }
    }
    if left.len() != right.len() {
        return Err(format!("{} records against {}", left.len(), right.len()));
    }
    Ok(())
}

/// The golden form of a script's records.
pub fn golden_json(script: &EnvScript, records: &[EnvRecord]) -> Value {
    json!({
        "format": "fly-session/legacy-env-golden-v1",
        "script": script.name,
        "records": records.iter().map(EnvRecord::to_json).collect::<Vec<_>>(),
    })
}

async fn read_attachment(
    artifacts: &BTreeMap<String, flybus::Artifact>,
    op: &str,
    name: &str,
) -> Result<Vec<u8>, String> {
    let artifact = artifacts
        .get(name)
        .ok_or_else(|| format!("{op}: no {name} attachment"))?;
    artifact.read_all().await.map_err(|e| e.message)
}

fn memory_record(image: &[u8]) -> (Digest, Digest) {
    (digest_of_bytes(image), digest_of_bytes(&image[WRAM]))
}

fn audio_record(first_sample: u64, raw_f32: &[u8], discontinuity: bool) -> AudioRecord {
    AudioRecord {
        first_sample,
        frames: (raw_f32.len() / 4 / legacy_env::AUDIO_CHANNELS as usize) as u64,
        digest: digest_of_bytes(raw_f32),
        discontinuity,
    }
}

// -------------------------------------------------------------------------------------------
// The direct reference

/// The emulator driven directly, call for call as flysim's `LegacyFrame` drives it (`initialize`,
/// `run` + `take_frame`, the ratchet's capture, `recover_game` + `rollback`, `restore`), with the
/// session's boundary, clock and audio position kept beside it.
pub struct DirectEmulator {
    rom: Arc<Vec<u8>>,
    backend: BackendConfig,
    emulator: Emulator,
    framebuffer: Vec<u8>,
    engine_frame: u64,
    buttons: u8,
    boundary: u64,
    world_time: RationalNs,
    audio_next: u64,
    discontinuity: bool,
    slots: BTreeMap<Id, SlotState>,
    episode_id: Id,
    index: u64,
}

impl DirectEmulator {
    fn boot(rom: &[u8], backend: &BackendConfig) -> Result<Emulator, String> {
        Emulator::new(
            rom,
            backend.audio_sample_rate as u32,
            backend.audio_buffer_frames as u32,
        )
        .map_err(|e| e.to_string())
    }

    fn record(
        &mut self,
        op: &'static str,
        mask: Option<u8>,
        audio: Option<AudioRecord>,
        state: Option<Digest>,
    ) -> EnvRecord {
        let image = memory_image(&mut self.emulator).expect("the image reads");
        let (memory, wram) = memory_record(&image);
        let record = EnvRecord {
            index: self.index,
            op,
            boundary: self.boundary,
            engine_frame: self.engine_frame,
            world_time: self.world_time,
            mask,
            frame: digest_of_bytes(&self.framebuffer),
            memory,
            wram,
            audio,
            state,
        };
        self.index += 1;
        record
    }

    /// `LegacyFrame::initialize`: one frame, no button pressed.
    pub fn fresh(
        rom: Arc<Vec<u8>>,
        backend: BackendConfig,
    ) -> Result<(DirectEmulator, EnvRecord), String> {
        let mut emulator = DirectEmulator::boot(&rom, &backend)?;
        emulator.run_frame().map_err(|e| e.to_string())?;
        let framebuffer = emulator.framebuffer().to_vec();
        let _ = emulator.take_audio_u8();
        let mut direct = DirectEmulator {
            rom,
            backend,
            emulator,
            framebuffer,
            engine_frame: 1,
            buttons: 0,
            boundary: 0,
            world_time: RationalNs::ZERO,
            audio_next: 0,
            discontinuity: false,
            slots: BTreeMap::new(),
            episode_id: id("ep1"),
            index: 0,
        };
        let record = direct.record("initialize", None, None, None);
        Ok((direct, record))
    }

    /// `Sim::try_restore` + `LegacyFrame::restore`: a new emulator, `import_state`,
    /// `set_buttons(buttons)`, the saved framebuffer, and the ratchet's snapshot.
    pub fn from_state(
        rom: Arc<Vec<u8>>,
        backend: BackendConfig,
        state: WorldState,
        index: u64,
    ) -> Result<(DirectEmulator, EnvRecord), String> {
        let mut emulator = DirectEmulator::boot(&rom, &backend)?;
        emulator
            .import_state(&state.emulator)
            .map_err(|e| e.to_string())?;
        emulator.set_buttons(state.buttons);
        let mut direct = DirectEmulator {
            rom,
            backend,
            emulator,
            framebuffer: state.framebuffer,
            engine_frame: state.engine_frame,
            buttons: state.buttons,
            boundary: state.boundary,
            world_time: state.world_time,
            audio_next: state.audio_next_sample,
            discontinuity: true,
            slots: state.slots.into_iter().collect(),
            episode_id: state.episode_id,
            index,
        };
        let record = direct.record("restore", None, None, None);
        Ok((direct, record))
    }

    /// `LegacyFrame::run` + `take_frame`.
    pub fn advance(&mut self, mask: u8) -> Result<EnvRecord, String> {
        self.buttons = mask;
        self.emulator.set_buttons(mask);
        self.emulator.run_frame().map_err(|e| e.to_string())?;
        self.engine_frame += 1;
        self.framebuffer
            .copy_from_slice(self.emulator.framebuffer());
        let raw = self.emulator.take_audio_u8();
        self.boundary += 1;
        self.world_time = self
            .world_time
            .checked_add(&gameboy::step_duration())
            .map_err(|e| e.0)?;
        let samples = audio_f32le(&raw);
        let audio = audio_record(self.audio_next, &samples, self.discontinuity);
        self.audio_next += audio.frames;
        self.discontinuity = false;
        Ok(self.record("advance", Some(mask), Some(audio), None))
    }

    /// The ratchet's capture in `LegacyFrame::boundary`: `export_state` and the frame on screen.
    pub fn save_slot(&mut self, slot_id: &Id) -> Result<EnvRecord, String> {
        let game = self.emulator.export_state().map_err(|e| e.to_string())?;
        let digest = digest_of_bytes(&game);
        self.slots.insert(
            slot_id.clone(),
            SlotState {
                game,
                frame: self.framebuffer.clone(),
                boundary: self.boundary,
            },
        );
        Ok(self.record("save-slot", None, None, Some(digest)))
    }

    /// `recover_game` + `LegacyFrame::rollback`, the emulator's half.
    pub fn rollback(&mut self, slot_id: &Id) -> Result<EnvRecord, String> {
        let slot = self.slots.get(slot_id).cloned().ok_or("no such slot")?;
        self.emulator
            .import_state(&slot.game)
            .map_err(|e| e.to_string())?;
        self.emulator.set_buttons(0);
        self.framebuffer.copy_from_slice(&slot.frame);
        self.buttons = 0;
        self.emulator.set_buttons(0);
        self.discontinuity = true;
        Ok(self.record("rollback", None, None, None))
    }

    /// The world a capture holds at this boundary.
    pub fn state(&mut self) -> Result<WorldState, String> {
        Ok(WorldState {
            rom_digest: self.backend.rom_digest.clone(),
            episode_id: self.episode_id.clone(),
            boundary: self.boundary,
            world_time: self.world_time,
            engine_frame: self.engine_frame,
            buttons: self.buttons,
            emulator: self.emulator.export_state().map_err(|e| e.to_string())?,
            framebuffer: self.framebuffer.clone(),
            slots: self
                .backend
                .slots
                .iter()
                .filter_map(|s| self.slots.get(s).map(|slot| (s.clone(), slot.clone())))
                .collect(),
            audio_next_sample: self.audio_next,
        })
    }

    /// A capture and a restart of the service onto it: a new emulator restored from the state.
    pub fn capture_restore(self) -> Result<(DirectEmulator, EnvRecord, EnvRecord), String> {
        let mut this = self;
        let state = this.state()?;
        let capture = this.record("capture", None, None, Some(world_digest(&state)));
        let (restored, record) =
            DirectEmulator::from_state(this.rom.clone(), this.backend.clone(), state, this.index)?;
        Ok((restored, capture, record))
    }
}

/// The digest of a world's content: every byte array and every scalar, without the payload
/// header (which names the worker and the checkpoint, not the world).
pub fn world_digest(state: &WorldState) -> Digest {
    let slots: Vec<Value> = state
        .slots
        .iter()
        .map(|(id, slot)| {
            json!({
                "slotId": id,
                "boundary": slot.boundary.to_string(),
                "game": digest_of_bytes(&slot.game),
                "frame": digest_of_bytes(&slot.frame),
            })
        })
        .collect();
    digest_of(&json!({
        "romDigest": state.rom_digest,
        "episodeId": state.episode_id,
        "boundary": state.boundary.to_string(),
        "worldTime": state.world_time.to_json(),
        "engineFrame": state.engine_frame.to_string(),
        "buttons": state.buttons,
        "emulator": digest_of_bytes(&state.emulator),
        "framebuffer": digest_of_bytes(&state.framebuffer),
        "audioNextSample": state.audio_next_sample.to_string(),
        "slots": slots,
    }))
    .expect("a world digest canonicalizes")
}

/// Runs a script on the direct reference.
pub fn run_direct(
    rom: Arc<Vec<u8>>,
    backend: &BackendConfig,
    script: &EnvScript,
) -> Result<Vec<EnvRecord>, String> {
    let (mut direct, first) = match &script.start {
        EnvStart::Fresh => DirectEmulator::fresh(rom, backend.clone())?,
        EnvStart::Flysim01(bytes) => {
            let state = WorldState::from_flysim01(
                bytes,
                &id("ep1"),
                &backend.slots[0],
                backend.audio_sample_rate,
            )?;
            DirectEmulator::from_state(rom, backend.clone(), state, 0)?
        }
    };
    let mut records = vec![first];
    for op in &script.ops {
        match op {
            EnvOp::Advance(mask) => records.push(direct.advance(*mask)?),
            EnvOp::SaveSlot(slot) => records.push(direct.save_slot(slot)?),
            EnvOp::Rollback(slot) => records.push(direct.rollback(slot)?),
            EnvOp::CaptureRestore => {
                let (next, capture, restore) = direct.capture_restore()?;
                records.push(capture);
                records.push(restore);
                direct = next;
            }
        }
    }
    Ok(records)
}

// -------------------------------------------------------------------------------------------
// The rig: a router, a launcher in one mode, the environment it started

const DRIVER_CLIENT: &str = "env-driver";
const MAX_GENERATIONS: u32 = 8;

fn env_service(worker_id: &str) -> String {
    format!("env.{worker_id}")
}

fn env_client(worker_id: &str, generation: u32) -> String {
    format!("world-{worker_id}-r{generation}")
}

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

fn domain(e: DomainError) -> String {
    format!("{:?}: {}", e.code, e.message)
}

/// The smallest composition that runs [`LegacyGameboyEnvironment`] under `fly-session`'s own
/// supervisor, in one execution mode. The coordinator's part is played by an [`EnvDriver`].
pub struct EnvRig {
    pub launcher: Launcher,
    pub client: Client,
    pub session_id: Id,
    pub worker_id: Id,
    pub mode: ExecutionMode,
    rom_path: PathBuf,
    backend: BackendConfig,
    generation: u32,
}

impl EnvRig {
    pub async fn start(
        root: &Path,
        mode: ExecutionMode,
        rom_path: &Path,
        backend: BackendConfig,
    ) -> Result<EnvRig, String> {
        let session_id = id("legacy");
        let worker_id = id("world");
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).map_err(|e| format!("sockets: {e}"))?;
        let service = env_service(&worker_id);
        let mut policy = Policy::closed()
            .client(
                DRIVER_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("env.")]),
            )
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("env.")]),
            );
        for generation in 1..=MAX_GENERATIONS {
            policy = policy.client(
                &env_client(&worker_id, generation),
                grants(|g| g.register = vec![Pattern::exact(&service)]),
            );
        }
        let mut config = RouterConfig::new(&store_root);
        config.policy = policy;
        let router = Router::new(config).map_err(|e| format!("router: {e}"))?;
        let budget = ThreadBudget::new(3, 1).map_err(|e| e.message)?;
        let launcher = Launcher::start(router, mode, Via::Unix, &store_root, &sockets, budget)
            .await
            .map_err(|e| format!("launcher: {}", e.message))?;
        let client = launcher
            .connect(DRIVER_CLIENT)
            .await
            .map_err(|e| format!("connect: {}", e.message))?;
        let mut rig = EnvRig {
            launcher,
            client,
            session_id,
            worker_id,
            mode,
            rom_path: rom_path.to_owned(),
            backend,
            generation: 0,
        };
        rig.launch().await?;
        Ok(rig)
    }

    /// The launch specification of the next generation.
    pub fn next_spec(&self) -> LegacyEnvironmentLaunch {
        let generation = self.generation + 1;
        LegacyEnvironmentLaunch {
            session_id: self.session_id.clone(),
            worker_id: self.worker_id.clone(),
            incarnation_id: parse_id(&format!("{}-inc-{generation}", self.worker_id))
                .expect("an incarnation id"),
            worker_threads: 1,
            rom_path: self.rom_path.clone(),
            backend: self.backend.clone(),
            client_id: env_client(&self.worker_id, generation),
            service: env_service(&self.worker_id),
        }
    }

    async fn launch(&mut self) -> Result<WorkerRef, String> {
        let spec = self.next_spec();
        self.generation += 1;
        self.launcher
            .launch_legacy_environment(spec)
            .await
            .map_err(|e| e.message)?;
        Ok(self.worker_ref())
    }

    /// Launches a worker from an explicit specification (for the refusals), as the next
    /// generation.
    pub async fn launch_spec(
        &mut self,
        spec: LegacyEnvironmentLaunch,
    ) -> Result<WorkerRef, DomainError> {
        self.launcher.reap(&self.worker_id, &id("replaced")).await;
        self.generation += 1;
        self.launcher.launch_legacy_environment(spec).await?;
        Ok(self.worker_ref())
    }

    pub fn worker_ref(&self) -> WorkerRef {
        self.launcher
            .worker(&self.worker_id)
            .expect("launched")
            .worker_ref()
    }

    /// Stops the worker and starts its replacement: a new process in process mode, with a new
    /// incarnation and client identity, uninitialized.
    pub async fn replace(&mut self) -> Result<WorkerRef, String> {
        self.launcher.reap(&self.worker_id, &id("replaced")).await;
        self.launch().await
    }

    pub async fn stop(mut self) {
        self.launcher.reap_all(&id("done")).await;
    }

    pub fn driver(&self) -> EnvDriver {
        EnvDriver {
            client: self.client.clone(),
            target: self.worker_ref(),
            session_id: self.session_id.clone(),
            worker_id: self.worker_id.clone(),
            backend: self.backend.clone(),
            descriptor: None,
            epoch_serial: 1,
            boundary: 0,
            serials: Serials::default(),
            batches: 0,
            timelines: AudioTimelines::default(),
            index: 0,
            checkpoints: 0,
            last: None,
        }
    }
}

/// The coordinator's side of the environment: scopes, epochs, batch ids, the descriptor it was
/// given, and the coordinator's own media checks on every observation.
pub struct EnvDriver {
    client: Client,
    target: WorkerRef,
    session_id: Id,
    worker_id: Id,
    backend: BackendConfig,
    descriptor: Option<EnvironmentDescriptor>,
    epoch_serial: u32,
    boundary: u64,
    serials: Serials,
    batches: u64,
    timelines: AudioTimelines,
    index: u64,
    checkpoints: u64,
    /// The last observation's record, which a slot save and a capture repeat.
    last: Option<EnvRecord>,
}

/// A captured payload, held for a restore.
pub struct Captured {
    pub result: CaptureResult,
    pub artifact: flybus::Artifact,
    pub bytes: Vec<u8>,
    pub scope: Scope,
}

const WANT: [&str; 3] = ["view.lcd", MEMORY_ATTACHMENT, "audio.apu"];

impl EnvDriver {
    fn epoch(&self) -> Id {
        id(&format!("e{}", self.epoch_serial))
    }

    pub fn scope(&self) -> Scope {
        scope_at(&self.session_id, &self.epoch(), self.boundary)
    }

    pub fn boundary(&self) -> u64 {
        self.boundary
    }

    pub fn target(&self) -> &WorkerRef {
        &self.target
    }

    pub fn descriptor(&self) -> Option<&EnvironmentDescriptor> {
        self.descriptor.as_ref()
    }

    /// Moves the driver to the next epoch without telling the worker, to address it wrongly.
    pub fn bump_epoch(&mut self) {
        self.epoch_serial += 1;
    }

    pub fn set_target(&mut self, target: WorkerRef) {
        self.target = target;
    }

    async fn call(
        &mut self,
        method: &str,
        scope: Option<Scope>,
        params: Value,
        attachments: &[(&str, &flybus::Artifact)],
    ) -> Result<crate::rpc::DomainReply, String> {
        let reply = self
            .call_raw(method, scope, params, attachments)
            .await
            .map_err(|e| format!("{method}: {}", domain(e)))?;
        reply
            .result()
            .map_err(|e| format!("{method}: {}", domain(e)))?;
        Ok(reply)
    }

    /// One call with any params, its domain outcome returned as it stands.
    pub async fn call_raw(
        &mut self,
        method: &str,
        scope: Option<Scope>,
        params: Value,
        attachments: &[(&str, &flybus::Artifact)],
    ) -> Result<crate::rpc::DomainReply, DomainError> {
        let request_id = self.serials.next(&self.target.service);
        self.retry_raw(request_id, method, scope, params, attachments)
            .await
    }

    /// The same request id again: a domain retry, or a duplicate.
    pub async fn retry_raw(
        &mut self,
        request_id: DomainRequestId,
        method: &str,
        scope: Option<Scope>,
        params: Value,
        attachments: &[(&str, &flybus::Artifact)],
    ) -> Result<crate::rpc::DomainReply, DomainError> {
        let want: Vec<String> = WANT
            .iter()
            .map(|w| (*w).to_owned())
            .chain([crate::state::PAYLOAD_ATTACHMENT.to_owned()])
            .collect();
        call(
            &self.client,
            &self.target,
            method,
            scope,
            object(params),
            attachments,
            request_id,
            &want,
        )
        .await
    }

    async fn acknowledge(&mut self, request_id: DomainRequestId) -> Result<(), String> {
        let params = AcknowledgeParams {
            request_ids: vec![request_id],
        };
        self.call("Worker.Acknowledge", None, params.to_json(), &[])
            .await
            .map(|_| ())
    }

    /// The `Environment.Initialize` params for this backend.
    pub fn initialize_params(&self) -> EnvironmentInitializeParams {
        EnvironmentInitializeParams {
            backend_config: self.backend.asset_ref(),
            task_config: crate::environment::synthetic_asset("pokered-task", "pokered-macros-v1"),
            episode_id: id("ep1"),
            port_bindings: vec![(self.backend.port_id.clone(), id("fly"))],
        }
    }

    /// Reads one observation's artifacts into a record, after the coordinator's Phase C checks:
    /// the descriptor's shapes, the required view at its producing boundary, the audio rule for
    /// the observation's origin, the audio timeline, the memory image's schema and ROM.
    async fn observe(
        &mut self,
        op: &'static str,
        mask: Option<u8>,
        observation: &WorldObservation,
        artifacts: &BTreeMap<String, flybus::Artifact>,
        origin: ObservationOrigin,
        state: Option<Digest>,
    ) -> Result<EnvRecord, String> {
        let descriptor = self.descriptor.clone().ok_or("no descriptor")?;
        observation.validate_against(&descriptor).map_err(|e| e.0)?;
        check_required_views(&descriptor, observation).map_err(domain)?;
        check_required_audio(&descriptor, observation, origin).map_err(domain)?;
        self.timelines
            .accept(&descriptor, observation)
            .map_err(domain)?;
        if observation.boundary != self.boundary {
            return Err(format!(
                "{op}: observation at {} for boundary {}",
                observation.boundary, self.boundary
            ));
        }
        if observation.world_time != world_time_at(self.boundary)? {
            return Err(format!(
                "{op}: the world clock is not boundary {}'s",
                self.boundary
            ));
        }
        let engine_frame: u64 = observation
            .engine_frame
            .as_deref()
            .ok_or("no engineFrame")?
            .parse()
            .map_err(|_| "engineFrame is not a counter")?;
        let inspection = MemoryInspection::from_typed(&observation.inspection).map_err(|e| e.0)?;
        if inspection.rom_digest != descriptor.content_digest {
            return Err("the inspection names another cartridge".to_owned());
        }
        let view = &observation.sensory_views[0];
        let frame = read_attachment(artifacts, op, "view.lcd").await?;
        if frame.len() != FRAMEBUFFER_LEN || artifacts["view.lcd"].reference() != &view.pixels {
            return Err(format!("{op}: the view attachment is not the view"));
        }
        if observation.broadcast_views != observation.sensory_views {
            return Err(format!("{op}: the broadcast view is not the sensory one"));
        }
        let image = read_attachment(artifacts, op, MEMORY_ATTACHMENT).await?;
        if image.len() != gameboy::MEMORY_IMAGE_BYTES as usize
            || artifacts[MEMORY_ATTACHMENT].reference() != &inspection.memory
        {
            return Err(format!(
                "{op}: the memory attachment is not the inspection's image"
            ));
        }
        let audio = match observation.audio.first() {
            Some(chunk) => {
                let samples = read_attachment(artifacts, op, "audio.apu").await?;
                if artifacts["audio.apu"].reference() != &chunk.samples
                    || chunk.stream_id != AUDIO_STREAM_ID
                {
                    return Err(format!("{op}: the audio attachment is not the chunk"));
                }
                if samples.len() as u64 != chunk.sample_frames * legacy_env::AUDIO_CHANNELS * 4 {
                    return Err(format!("{op}: the chunk's length is not its sampleFrames"));
                }
                Some(audio_record(
                    chunk.first_sample,
                    &samples,
                    chunk.discontinuity,
                ))
            }
            None => None,
        };
        let (memory, wram) = memory_record(&image);
        let record = EnvRecord {
            index: self.index,
            op,
            boundary: self.boundary,
            engine_frame,
            world_time: observation.world_time,
            mask,
            frame: digest_of_bytes(&frame),
            memory,
            wram,
            audio,
            state,
        };
        self.index += 1;
        self.last = Some(record.clone());
        Ok(record)
    }

    /// Checks the descriptor the worker advertised against the composition's declaration.
    fn accept_descriptor(&mut self, descriptor: &EnvironmentDescriptor) -> Result<(), String> {
        descriptor.validate().map_err(|e| e.0)?;
        let expected = legacy_env::LegacyGameboyEnvironment::descriptor_for(&self.backend);
        if *descriptor != expected {
            return Err("the environment advertised another descriptor".to_owned());
        }
        if descriptor.step_duration != gameboy::step_duration()
            || descriptor.content_digest != self.backend.rom_digest
            || descriptor.configuration_digest != self.backend.asset_ref().digest
            || descriptor.inspection_schema != gameboy::MEMORY_INSPECTION.schema_ref()
        {
            return Err("the descriptor is not the legacy composition's".to_owned());
        }
        self.descriptor = Some(descriptor.clone());
        Ok(())
    }

    /// `Environment.Initialize`: O[0] after the one-frame scaffold.
    pub async fn initialize(&mut self) -> Result<EnvRecord, String> {
        let params = self.initialize_params();
        let reply = self
            .call(
                "Environment.Initialize",
                Some(self.scope()),
                params.to_json(),
                &[],
            )
            .await?;
        let result: EnvironmentInitializeResult = reply.parse().map_err(domain)?;
        self.accept_descriptor(&result.descriptor)?;
        self.timelines = AudioTimelines::fresh(&result.descriptor);
        let record = self
            .observe(
                "initialize",
                None,
                &result.observation,
                &reply.artifacts,
                ObservationOrigin::Installed,
                None,
            )
            .await?;
        self.acknowledge(reply.request_id).await?;
        Ok(record)
    }

    /// The `AdvanceParams` of the next batch.
    pub fn advance_params(&mut self, mask: u8) -> AdvanceParams {
        self.batches += 1;
        AdvanceParams {
            batch_id: id(&format!("b{}-{}", self.epoch_serial, self.batches)),
            controls: vec![control_of(&self.backend.port_id, mask)],
        }
    }

    /// `Environment.Advance` with one complete joypad batch.
    pub async fn advance(&mut self, mask: u8) -> Result<EnvRecord, String> {
        let params = self.advance_params(mask);
        let reply = self
            .call(
                "Environment.Advance",
                Some(self.scope()),
                params.to_json(),
                &[],
            )
            .await?;
        let result: StepResult = reply.parse().map_err(domain)?;
        if result.applied_from_step != self.boundary || result.next_step != self.boundary + 1 {
            return Err("Environment.Advance moved another interval".to_owned());
        }
        if result.applied_controls_digest != controls_digest(&params.controls)
            || result.batch_id != params.batch_id
        {
            return Err("Environment.Advance applied another batch".to_owned());
        }
        self.boundary += 1;
        self.observe(
            "advance",
            Some(mask),
            &result.observation,
            &reply.artifacts,
            ObservationOrigin::Transition,
            None,
        )
        .await
    }

    /// A record that changes no observation: the last one's, renumbered.
    fn repeat(&mut self, op: &'static str, state: Option<Digest>) -> Result<EnvRecord, String> {
        let last = self.last.clone().ok_or("no observation yet")?;
        let record = EnvRecord {
            index: self.index,
            op,
            mask: None,
            audio: None,
            state,
            ..last
        };
        self.index += 1;
        Ok(record)
    }

    /// `Environment.SaveSlot` at the committed boundary.
    pub async fn save_slot(&mut self, slot_id: &Id) -> Result<EnvRecord, String> {
        let params = SaveSlotParams {
            slot_id: slot_id.clone(),
        };
        let scope = self.scope();
        let reply = self
            .call(METHOD_SAVE_SLOT, Some(scope.clone()), params.to_json(), &[])
            .await?;
        let result: SaveSlotResult = reply.parse().map_err(domain)?;
        result.validate_against_scope(&scope).map_err(|e| e.0)?;
        if result.slot_id != *slot_id {
            return Err("SaveSlot saved another slot".to_owned());
        }
        self.repeat("save-slot", Some(result.state_digest))
    }

    /// The `RestoreSlotParams` of a rollback from the current epoch.
    pub fn restore_slot_params(&self, slot_id: &Id) -> RestoreSlotParams {
        RestoreSlotParams {
            slot_id: slot_id.clone(),
            prior_epoch: self.epoch(),
            policy: ROLLBACK_POLICY.to_owned(),
        }
    }

    /// `Environment.RestoreSlot` under the next epoch, same boundary.
    pub async fn rollback(&mut self, slot_id: &Id) -> Result<EnvRecord, String> {
        let params = self.restore_slot_params(slot_id);
        self.epoch_serial += 1;
        let scope = self.scope();
        let reply = self
            .call(
                METHOD_RESTORE_SLOT,
                Some(scope.clone()),
                params.to_json(),
                &[],
            )
            .await?;
        let result: RestoreSlotResult = reply.parse().map_err(domain)?;
        result.validate_against_scope(&scope).map_err(|e| e.0)?;
        let descriptor = self.descriptor.clone().ok_or("no descriptor")?;
        self.timelines =
            AudioTimelines::restored(&descriptor, &self.timelines.positions()).map_err(domain)?;
        self.observe(
            "rollback",
            None,
            &result.observation,
            &reply.artifacts,
            ObservationOrigin::Installed,
            None,
        )
        .await
    }

    /// `State.Capture` at the committed boundary, acknowledged.
    pub async fn capture(&mut self) -> Result<Captured, String> {
        self.checkpoints += 1;
        let scope = self.scope();
        let params = CaptureParams {
            checkpoint_id: id(&format!("c{}", self.checkpoints)),
        };
        let reply = self
            .call("State.Capture", Some(scope.clone()), params.to_json(), &[])
            .await?;
        let result: CaptureResult = reply.parse().map_err(domain)?;
        let artifact = reply
            .artifacts
            .get(crate::state::PAYLOAD_ATTACHMENT)
            .cloned()
            .ok_or("State.Capture carried no payload")?;
        let bytes = artifact.read_all().await.map_err(|e| e.message)?;
        if Some(digest_of_bytes(&bytes)) != result.payload.digest {
            return Err("the capture payload is not the digest it declares".to_owned());
        }
        self.acknowledge(reply.request_id).await?;
        Ok(Captured {
            result,
            artifact,
            bytes,
            scope,
        })
    }

    /// A capture's record: the last observation plus the digest of the captured world.
    pub fn capture_record(&mut self, captured: &Captured) -> Result<EnvRecord, String> {
        let (_, state) = WorldState::decode(&captured.bytes)?;
        self.repeat("capture", Some(world_digest(&state)))
    }

    /// A group restore of `captured` into `target`, a fresh replacement worker, under a new epoch.
    pub async fn restore_into(
        &mut self,
        target: WorkerRef,
        captured: &Captured,
    ) -> Result<EnvRecord, String> {
        self.target = target;
        self.epoch_serial += 1;
        let (_, state) = WorldState::decode(&captured.bytes)?;
        self.boundary = captured.scope.step;
        let params = StageRestoreParams {
            checkpoint_id: captured.result.checkpoint_id.clone(),
            source_scope: captured.scope.clone(),
            compatibility_digest: captured.result.compatibility_digest.clone(),
            payload: captured.artifact.reference().clone(),
        };
        let reply = self
            .call(
                "State.StageRestore",
                Some(self.scope()),
                params.to_json(),
                &[(crate::state::PAYLOAD_ATTACHMENT, &captured.artifact)],
            )
            .await?;
        let staged: StageRestoreResult = reply.parse().map_err(domain)?;
        self.acknowledge(reply.request_id).await?;
        let params = ActivateRestoreParams {
            restore_token: staged.restore_token,
        };
        let reply = self
            .call("State.ActivateRestore", None, params.to_json(), &[])
            .await?;
        let activated: ActivateRestoreResult = reply.parse().map_err(domain)?;
        if activated.committed_step != self.boundary {
            return Err("State.ActivateRestore landed on another boundary".to_owned());
        }
        let observation = activated
            .observation
            .ok_or("the environment restored no observation")?;
        let descriptor = legacy_env::LegacyGameboyEnvironment::descriptor_for(&self.backend);
        self.descriptor = Some(descriptor.clone());
        let positions = BTreeMap::from([(id(AUDIO_STREAM_ID), state.audio_next_sample)]);
        self.timelines = AudioTimelines::restored(&descriptor, &positions).map_err(domain)?;
        let record = self
            .observe(
                "restore",
                None,
                &observation,
                &reply.artifacts,
                ObservationOrigin::Installed,
                None,
            )
            .await?;
        self.acknowledge(reply.request_id).await?;
        Ok(record)
    }

    /// Seals `state` as a capture payload this worker would have written at `source`, for a
    /// restore that starts from a legacy checkpoint rather than from a capture.
    pub async fn payload_of(
        &mut self,
        state: &WorldState,
        source: Scope,
    ) -> Result<Captured, String> {
        self.checkpoints += 1;
        let checkpoint_id = id(&format!("c{}", self.checkpoints));
        let descriptor = legacy_env::LegacyGameboyEnvironment::descriptor_for(&self.backend);
        let header = PayloadHeader {
            worker_id: self.worker_id.clone(),
            checkpoint_id: checkpoint_id.clone(),
            source_scope: source.clone(),
            configuration_digest: descriptor.configuration_digest.clone(),
        };
        let bytes = state.encode(&header);
        let digest = digest_of_bytes(&bytes);
        let artifact = crate::state::seal_payload(&self.client, &bytes, &digest)
            .await
            .map_err(domain)?;
        Ok(Captured {
            result: CaptureResult {
                checkpoint_id,
                boundary: state.boundary,
                compatibility_digest: legacy_env::LegacyGameboyEnvironment::compatibility_digest(
                    &descriptor,
                ),
                payload: artifact.reference().clone(),
            },
            artifact,
            bytes,
            scope: source,
        })
    }

    /// Starts the world from a `FLYSIM01` checkpoint, the way the service starts after a restart:
    /// its world half becomes this environment's payload and is group-restored into the (fresh,
    /// uninitialized) worker under epoch `e2`.
    pub async fn restore_flysim01(&mut self, checkpoint: &[u8]) -> Result<EnvRecord, String> {
        let state = WorldState::from_flysim01(
            checkpoint,
            &id("ep1"),
            &self.backend.slots[0],
            self.backend.audio_sample_rate,
        )?;
        // The legacy file carries no session identity: its source scope is a legacy epoch.
        let source = scope_at(&self.session_id, "legacy", state.boundary);
        let captured = self.payload_of(&state, source).await?;
        let target = self.target.clone();
        self.restore_into(target, &captured).await
    }
}

/// Runs a script on a launched worker. `CaptureRestore` replaces the worker: a new process in
/// process mode.
pub async fn run_on_worker(rig: &mut EnvRig, script: &EnvScript) -> Result<Vec<EnvRecord>, String> {
    let mut driver = rig.driver();
    let mut records = vec![match &script.start {
        EnvStart::Fresh => driver.initialize().await?,
        EnvStart::Flysim01(bytes) => driver.restore_flysim01(bytes).await?,
    }];
    for op in &script.ops {
        match op {
            EnvOp::Advance(mask) => records.push(driver.advance(*mask).await?),
            EnvOp::SaveSlot(slot) => records.push(driver.save_slot(slot).await?),
            EnvOp::Rollback(slot) => records.push(driver.rollback(slot).await?),
            EnvOp::CaptureRestore => {
                let captured = driver.capture().await?;
                records.push(driver.capture_record(&captured)?);
                let target = rig.replace().await?;
                records.push(driver.restore_into(target, &captured).await?);
            }
        }
    }
    Ok(records)
}

// -------------------------------------------------------------------------------------------
// Scripts

/// A seeded button walk, the way the fly plays: a held direction for a while, sometimes a
/// button, sometimes nothing. xorshift32, so the script is reproducible from its seed.
pub fn button_walk(frames: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.max(1);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut masks = Vec::with_capacity(frames);
    while masks.len() < frames {
        let pick = next();
        let hold = 4 + (pick >> 8) as usize % 21;
        let mask = match pick % 10 {
            0..=5 => 1u8 << (pick >> 4 & 3), // a direction
            6 => flybrain_gb::emulator::buttons::A,
            7 => flybrain_gb::emulator::buttons::B,
            8 => 0,
            _ => (1u8 << (pick >> 4 & 3)) | flybrain_gb::emulator::buttons::A,
        };
        for _ in 0..hold {
            masks.push(mask);
        }
    }
    masks.truncate(frames);
    masks
}

/// A script over `masks` with the boundary actions a ratchet and a restart would take: a slot
/// save, two rollbacks onto it (the second after a capture/restore, so the slot crosses a
/// restore), a second save, and a rollback that lands on a boundary where a save happened too
/// (save first, then roll back: `legacy-gameboy-v1` section 11 step 1).
pub fn scripted(name: &str, start: EnvStart, masks: &[u8]) -> EnvScript {
    let slot = id(legacy_env::DEFAULT_SLOT);
    let n = masks.len();
    let mut ops = Vec::new();
    for (t, mask) in masks.iter().enumerate() {
        ops.push(EnvOp::Advance(*mask));
        if t == n / 8 {
            ops.push(EnvOp::SaveSlot(slot.clone()));
        }
        if t == n * 3 / 8 {
            ops.push(EnvOp::Rollback(slot.clone()));
        }
        if t == n / 2 {
            ops.push(EnvOp::CaptureRestore);
        }
        if t == n * 5 / 8 {
            ops.push(EnvOp::Rollback(slot.clone()));
        }
        if t == n * 3 / 4 {
            ops.push(EnvOp::SaveSlot(slot.clone()));
            ops.push(EnvOp::Rollback(slot.clone()));
        }
    }
    EnvScript {
        name: name.to_owned(),
        start,
        ops,
    }
}

/// The toy cartridge's golden script: a fresh start and 240 frames.
pub fn toy_script() -> EnvScript {
    scripted(
        "toy-cart-fresh-240",
        EnvStart::Fresh,
        &button_walk(240, 20_260_929),
    )
}

/// The committed golden of [`toy_script`].
pub fn toy_golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/legacy-env/toy-cart.golden.json")
}

// -------------------------------------------------------------------------------------------
// FND-01's trace

/// One transition of a `flysim-legacy-frame-trace-v1` trace, as the environment sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceFrame {
    /// The legacy frame counter before the transition: `engineFrame` of O[k].
    pub step: u64,
    pub mask: u8,
    pub framebuffer: Digest,
    pub wram: Digest,
    /// `save-slot` (with the slot's state digest) and `rollback`, in the order applied.
    pub actions: Vec<(String, Option<Digest>)>,
}

/// The environment's half of a `FLY_TRACE` written by the running service.
#[derive(Clone, Debug)]
pub struct FlyTrace {
    pub frames: Vec<TraceFrame>,
}

impl FlyTrace {
    pub fn read(path: &Path, limit: Option<usize>) -> Result<FlyTrace, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = text.lines();
        let header: Value =
            serde_json::from_str(lines.next().ok_or("empty trace")?).map_err(|e| e.to_string())?;
        if header.get("format").and_then(Value::as_str) != Some("flysim-legacy-frame-trace-v1") {
            return Err("not a flysim-legacy-frame-trace-v1 trace".to_owned());
        }
        let mut frames = Vec::new();
        for line in lines {
            if limit.is_some_and(|n| frames.len() >= n) {
                break;
            }
            let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let Some(b) = value.get("behaviour") else {
                continue;
            };
            let text = |key: &str| {
                b.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("trace: no {key}"))
            };
            let mut actions = Vec::new();
            for action in b
                .get("boundaryActions")
                .and_then(Value::as_array)
                .ok_or("trace: no boundaryActions")?
            {
                let kind = action
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or("trace: action kind")?
                    .to_owned();
                let digest = action
                    .get("stateDigest")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                actions.push((kind, digest));
            }
            frames.push(TraceFrame {
                step: text("step")?.parse().map_err(|_| "trace: step")?,
                mask: b
                    .get("mask")
                    .and_then(Value::as_u64)
                    .and_then(|m| u8::try_from(m).ok())
                    .ok_or("trace: mask")?,
                framebuffer: text("framebufferDigest")?,
                wram: text("wramDigest")?,
                actions,
            });
        }
        Ok(FlyTrace { frames })
    }

    /// The script this trace's environment inputs make, from its start checkpoint.
    pub fn script(&self, name: &str, checkpoint: Arc<Vec<u8>>) -> EnvScript {
        let slot = id(legacy_env::DEFAULT_SLOT);
        let mut ops = Vec::new();
        for frame in &self.frames {
            ops.push(EnvOp::Advance(frame.mask));
            for (kind, _) in &frame.actions {
                ops.push(if kind == "save-slot" {
                    EnvOp::SaveSlot(slot.clone())
                } else {
                    EnvOp::Rollback(slot.clone())
                });
            }
        }
        EnvScript {
            name: name.to_owned(),
            start: EnvStart::Flysim01(checkpoint),
            ops,
        }
    }

    /// Checks records produced by [`FlyTrace::script`] against what the service recorded: each
    /// transition's engine frame, frame digest and WRAM digest, each slot save's state digest.
    /// Returns the number of values compared.
    pub fn check(&self, records: &[EnvRecord]) -> Result<usize, String> {
        let mut at = 1; // records[0] is the restore
        let mut compared = 0;
        for (t, frame) in self.frames.iter().enumerate() {
            let before = &records[at - 1];
            let record = records
                .get(at)
                .ok_or_else(|| format!("no record for transition {t}"))?;
            if record.op != "advance" || before.engine_frame != frame.step {
                return Err(format!(
                    "transition {t}: the world was at frame {} and the service at {}",
                    before.engine_frame, frame.step
                ));
            }
            if record.frame != frame.framebuffer {
                return Err(format!(
                    "transition {t} (frame {}): the frame differs from the service's",
                    frame.step
                ));
            }
            if record.wram != frame.wram {
                return Err(format!(
                    "transition {t} (frame {}): WRAM differs from the service's",
                    frame.step
                ));
            }
            compared += 3;
            at += 1;
            for (kind, digest) in &frame.actions {
                let record = records.get(at).ok_or("a boundary action has no record")?;
                if kind == "save-slot" {
                    if record.op != "save-slot" || record.state != *digest {
                        return Err(format!(
                            "transition {t}: the slot saved is not the service's"
                        ));
                    }
                    compared += 1;
                } else if record.op != "rollback" {
                    return Err(format!(
                        "transition {t}: no rollback where the service rolled back"
                    ));
                }
                at += 1;
            }
        }
        if at != records.len() {
            return Err("records beyond the trace".to_owned());
        }
        Ok(compared)
    }
}
