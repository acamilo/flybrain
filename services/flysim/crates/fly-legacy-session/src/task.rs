//! `pokered-macros-v1`: the Pokémon Red task and its action executor, one object
//! (`legacy-gameboy-v1` section 10, `workers-v1` section 4 amendment of 2026-09-23).
//!
//! Nothing here is a port of a rule. The reward adapter (`PokemonRedReward`), the ratchet, and the
//! macro layer (`flysim::macros::MacroLayer` over `flybrain-gb`'s palette, executor and `PokeState`)
//! are the very types the legacy loop runs, called in `flysim::frame::LegacyFrame`'s order. What
//! changed is what they read: a boundary's 64 KiB memory image and the cartridge, through
//! `flybrain_gb::ImageReader` (MEM-01), instead of a live emulator.
//!
//! | legacy frame | here |
//! | --- | --- |
//! | `execute`: `MacroLayer::decide` over the emulator | Phase B, [`ActionExecutor::apply`] over `ImageReader(O[k])` |
//! | `evaluate`: `adapter.sample`, `MacroLayer::observe`, location, progress | Phase C, [`Task::evaluate_transition`] over `ImageReader(O[k+1])` |
//! | `boundary`: the ratchet's capture and decision | Phase C too: a slot save and a rollback request, applied by the coordinator at `Ready(k+1)` |
//! | `rollback`: `recover_game`'s `clear_transient`, `cancel`, `observe` | [`Task::rollback`] over `ImageReader(O'[k+1])` |
//!
//! The ratchet keeps its ledger here; its *slot* lives in the environment (`gameboy-slots-v1`).
//! The ratchet only needs to know a slot exists (its budget test is `snapshot.is_none()`), so it
//! holds a marker [`Snapshot`] in place of the bytes.
//!
//! `restore: legacy-transient-reset` (section 14): a restore installs the adapter and the ratchet,
//! and the executor starts as a fresh process has it -- a new macro layer with empty ledgers and
//! nothing running, which observes the restored boundary before it decides anything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use fly_session::fly_session_types::gameboy::{
    self, ChannelsDecision, Location, MemoryInspection, ReadoutContext, RollbackRequest,
};
use fly_session::legacy_checkpoint::TaskHalf;
use fly_session::task::{
    ActionExecutor, Bootstrap, Evaluation, Inspection, RollbackEvaluation, Task,
};
use fly_session::types::*;
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::emulator::FRAMEBUFFER_LEN;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::{Ratchet, Snapshot};
use flybrain_gb::{AdapterLedger, Cartridge, ImageReader, MemoryImage};
use flysim::config::Config;
use flysim::macros::{MacroEvent, MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

/// The attachment the image travels under (ENV-01's `legacy_env::MEMORY_ATTACHMENT`).
pub const MEMORY_ATTACHMENT: &str = fly_session::legacy_env::MEMORY_ATTACHMENT;
/// The composition's one slot, which is the ratchet's.
pub const SLOT: &str = fly_session::legacy_env::DEFAULT_SLOT;
/// The game id the adapter and the palette are chosen by.
pub const GAME: &str = "pokemon-red";
/// The palette seed of a fresh start. The legacy loop seeds the palette from the brain's RNG
/// state after the warm-up (`Sim::boot`: `rng_state() as u32`), which the task cannot read: the
/// agent does not publish it. It is also never read: `MacroMachine`'s generator is written and
/// never consulted (no macro has a random step), which `tests/palette_seed.rs` holds on the
/// cartridge, so a fresh start's constant presses exactly what the legacy seed presses. A
/// restore uses the legacy seed itself, the imported agent state's RNG
/// ([`PokeredTask::set_palette_seed`]).
pub const PALETTE_SEED: u32 = 0;

fn schema(name: &str) -> SchemaRef {
    let digest = digest_of_bytes(format!("fly-legacy-session-schema-v1\n{name}\n1\n").as_bytes());
    SchemaRef::new(name, 1, &digest).expect("a task-local schema reference is valid")
}

/// The task ledger's schema: STATE-02's `TaskHalf` ledger `{reward, ratchet, slotFilled}` --
/// FLYSIM01's `reward` and `ratchet`, and whether the environment's slot `best` holds the
/// ratchet's snapshot. Nothing else: the executor's ledgers and the adapter's transient
/// observations are not captured (`legacy-transient-reset`).
pub fn ledger_schema() -> SchemaRef {
    schema("pokered-task-ledger-v1")
}
/// The attachment the task ledger's adapter state travels as (its JSON).
pub const REWARD_ATTACHMENT: &str = "reward";

/// The executor's capture: a marker only, because its state is transient (section 10, "Capture").
pub fn executor_schema() -> SchemaRef {
    schema("pokered.executor.v1")
}
pub fn progress_schema() -> SchemaRef {
    schema("pokered.progress.v1")
}
pub fn event_schema() -> SchemaRef {
    schema("pokered.event.v1")
}

fn state_error(message: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, message)
}

/// The ratchet's stand-in for a slot it knows the environment holds.
fn slot_marker() -> Snapshot {
    Snapshot {
        game: format!("slot:{SLOT}").into_bytes(),
        frame: vec![0; FRAMEBUFFER_LEN],
    }
}

/// The legacy trace's JSON of one macro event (`flysim::trace::macro_json`).
pub fn macro_json(phase: &str, event: &MacroEvent) -> Value {
    json!({
        "phase": phase,
        "slot": event.slot,
        "name": event.name,
        "outcome": event.outcome.map(|outcome| outcome.as_str()),
    })
}

/// What the object did in one transition, for the trace and the ledger comparison: the fields of
/// `flysim-legacy-frame-trace-v1` that are the task's and the executor's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskRecord {
    /// The boundary the transition started from.
    pub boundary: u64,
    /// The decision as the executor was given it, as the legacy active set (buttons, then the
    /// macro channel).
    pub decision: Vec<String>,
    /// The bound channels the decision was masked to (the context of this transition's Prepare).
    pub bound: Vec<String>,
    pub mask: u32,
    /// `macroEvents`, phases `execute`, `evaluate` and `rollback`, in order.
    pub macro_events: Vec<Value>,
    /// SHA-256 of work RAM, `$C000-$DFFF`, of O[k+1].
    pub wram_digest: String,
    /// `rewards`: `{kind, value, stimulationMs}` in adapter order.
    pub rewards: Vec<Value>,
    pub rank: u32,
    /// The ratchet asked for a slot save at `Ready(k+1)`.
    pub saved: bool,
    /// The ratchet asked for a rollback at `Ready(k+1)`, and it ran.
    pub rolled_back: bool,
    /// The ledgers after the boundary: adapter, ratchet and executor, as one canonical string.
    pub ledgers: String,
}

/// The ledgers as one canonical string: the adapter's export (its lifetime ledgers), the
/// ratchet's state and whether it holds a slot, and the executor's observable state -- scene,
/// bound channels, running macro, outcome counts, "nearer the objective". A legacy loop computes
/// the same string from its own parts, which is how a parity run compares ledgers frame by frame.
pub fn ledgers_of(
    adapter: &PokemonRedReward,
    ratchet: &Ratchet,
    macros: Option<&MacroLayer>,
) -> String {
    let executor = match macros {
        Some(layer) => json!({
            "scene": layer.scene_name(),
            "bound": layer.bound_channels(),
            "running": layer.running(),
            "counts": format!("{:?}", layer.counts()),
            "nearer": layer.nearer_the_objective(),
        }),
        None => Value::Null,
    };
    json!({
        "adapter": adapter.export_state(),
        "ratchet": serde_json::to_value(ratchet.state).expect("serializes"),
        "slot": ratchet.snapshot.is_some(),
        "executor": executor,
    })
    .to_string()
}

/// How the object is configured. Everything here is composition, not state.
#[derive(Clone, Debug)]
pub struct PokeredConfig {
    pub agent_id: Id,
    pub port_id: Id,
    pub mode: MacroMode,
    /// `executor.macroChannels`, composition order; empty in raw mode.
    pub macro_channels: Vec<String>,
    /// The macro group's `holdMs` from the decoder configuration.
    pub hold_ms: f64,
    /// Record a [`TaskRecord`] per transition (a parity run); off in service.
    pub record: bool,
}

struct Inner {
    config: PokeredConfig,
    cartridge: Cartridge,
    adapter: PokemonRedReward,
    ratchet: Ratchet,
    macros: Option<MacroLayer>,
    epoch: Id,
    /// O[k]: the image the executor reads in Phase B and the task reads as "old" in Phase C.
    current: Option<(u64, MemoryImage)>,
    /// The brain clock of the transition in flight (Phase B's clock), in ms.
    ms: f64,
    evaluations: u64,
    issued: u64,
    last_source_step: u64,
    open: Option<TaskRecord>,
    records: Vec<TaskRecord>,
    /// A parity run's stub readout ([`crate::driver`]); never set in service.
    driver: Option<Box<dyn crate::driver::DecisionDriver>>,
    /// Addresses the engine read that the image does not capture ([`Watched`]).
    uncaptured: std::collections::BTreeSet<u16>,
    /// The seed the next fresh macro layer is built with ([`PALETTE_SEED`], or a restored
    /// agent's RNG state as the legacy loop seeds it).
    palette_seed: u32,
}

/// The one object. [`PokeredTask::task`] and [`PokeredTask::executor`] are its two faces; the
/// coordinator holds each where it expects a task and an executor, and both reach the same state.
#[derive(Clone)]
pub struct PokeredTask {
    inner: Arc<Mutex<Inner>>,
}

impl PokeredTask {
    /// A fresh object over the cartridge. `rom_digest` is the environment's `contentDigest`: a
    /// cartridge that is not it is refused (`legacy-gameboy-v1` section 8).
    pub fn new(
        config: PokeredConfig,
        rom: &[u8],
        rom_digest: &str,
        epoch: &Id,
    ) -> Result<Self, String> {
        let cartridge = Cartridge::verified(rom, rom_digest).map_err(|e| e.to_string())?;
        let adapter = PokemonRedReward::new();
        let ratchet = Ratchet::with_policy(adapter.recovery_policy());
        let macros = fresh_layer(&config, PALETTE_SEED);
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                cartridge,
                adapter,
                ratchet,
                macros,
                epoch: epoch.clone(),
                current: None,
                ms: 0.0,
                evaluations: 0,
                issued: 0,
                last_source_step: 0,
                open: None,
                records: Vec::new(),
                driver: None,
                uncaptured: std::collections::BTreeSet::new(),
                palette_seed: PALETTE_SEED,
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The task face.
    pub fn task(&self) -> Box<dyn Task> {
        Box::new(TaskFace(self.clone()))
    }

    /// The executor face.
    pub fn executor(&self) -> Box<dyn ActionExecutor> {
        Box::new(ExecutorFace(self.clone()))
    }

    /// Replace every decision the executor is handed by `driver`'s, as a legacy harness does
    /// through `FrameObserver::readout`. A parity run's tool; the service never sets one.
    pub fn set_driver(&self, driver: Box<dyn crate::driver::DecisionDriver>) {
        self.lock().driver = Some(driver);
    }

    /// Every address the engine has read that the image does not capture. Empty is the contract
    /// (`legacy-gameboy-v1` section 8): a register window reads `$FF` in the image.
    pub fn uncaptured_reads(&self) -> Vec<u16> {
        self.lock().uncaptured.iter().copied().collect()
    }

    /// Seeds the next restore's fresh macro layer as the legacy loop does after a restore: with
    /// the restored brain's RNG state (`rng_state() as u32`).
    pub fn set_palette_seed(&self, seed: u32) {
        self.lock().palette_seed = seed;
    }

    /// The records since the last call.
    pub fn take_records(&self) -> Vec<TaskRecord> {
        std::mem::take(&mut self.lock().records)
    }

    /// The ledgers as one canonical string: the adapter's export, the ratchet's state, and the
    /// executor's observable state (scene, bound channels, running macro, outcome counts).
    pub fn ledgers(&self) -> String {
        self.lock().ledgers()
    }

    /// The ledger of a task half (STATE-02's `TaskHalf`, FLYSIM01's `reward` and `ratchet`), with
    /// whether the ratchet holds a snapshot (which the environment's slot `best` now holds).
    ///
    /// The adapter's lifetime ledger grows with play and is past the 32 KiB `TypedValue` bound on
    /// the live fly (42-46 KB at rank 12-15), so it is artifact-backed (`workers-v1` section 1):
    /// the typed value is `{reward: {digest, byteLength}, ratchet, slotFilled}` and the reward's
    /// JSON travels as the attachment [`REWARD_ATTACHMENT`].
    pub fn ledger_of(
        half: &TaskHalf,
        slot_filled: bool,
    ) -> DomainResult<(TypedValue, BTreeMap<String, Vec<u8>>)> {
        // The adapter's own JSON, exactly as FLYSIM01's `reward` member holds it.
        let reward = serde_json::to_vec(&half.reward)
            .map_err(|e| state_error(format!("the adapter ledger: {e}")))?;
        let value = json!({
            "reward": {"digest": digest_of_bytes(&reward), "byteLength": reward.len().to_string()},
            "ratchet": serde_json::to_value(half.ratchet)
                .map_err(|e| state_error(format!("the ratchet: {e}")))?,
            "slotFilled": slot_filled,
        });
        let typed = TypedValue::new(ledger_schema(), value).map_err(|e| state_error(e.0))?;
        Ok((
            typed,
            BTreeMap::from([(REWARD_ATTACHMENT.to_owned(), reward)]),
        ))
    }

    /// The task's half of a FLYSIM01 export at the committed boundary, and whether the slot is
    /// filled.
    pub fn task_half(&self) -> (TaskHalf, bool) {
        let inner = self.lock();
        (
            TaskHalf {
                reward: inner.adapter.export_state(),
                ratchet: inner.ratchet.state,
            },
            inner.ratchet.snapshot.is_some(),
        )
    }

    /// The brain time of the last transition (its Prepare's clock), in ms: `network.ms`.
    pub fn brain_ms(&self) -> f64 {
        self.lock().ms
    }

    /// The adapter's ladder rank now (`Sim::track_rank` reads it after each transition).
    pub fn rank(&self) -> u32 {
        self.lock().adapter.progress().rank
    }

    /// The context a restore of `ledger` onto the boundary `image` shows at brain time `ms`: what
    /// [`Task::restored`] will compute after the install, computed before it so the agent's
    /// payload can carry it. Changes nothing.
    pub fn preview_context(
        &self,
        ledger: &TypedValue,
        attachments: &BTreeMap<String, Vec<u8>>,
        image: &MemoryImage,
        ms: f64,
    ) -> Result<TypedValue, String> {
        let inner = self.lock();
        let (adapter, _ratchet) =
            parse_ledger(&inner, ledger, attachments).map_err(|e| e.message)?;
        let mut layer = fresh_layer(&inner.config, inner.palette_seed);
        if let Some(layer) = layer.as_mut() {
            let mut reader = ImageReader::new(image, &inner.cartridge);
            let _ = layer.observe(&mut reader, &AdapterLedger(&adapter), ms);
        }
        Ok(ReadoutContext {
            boot: adapter.boot(),
            bound: layer
                .as_ref()
                .map(MacroLayer::bound_channels)
                .unwrap_or_default(),
            location: adapter
                .location()
                .map(|(area, x, y)| Location { area, x, y }),
        }
        .to_typed())
    }

    /// The readout context the task would hand the decoder now.
    pub fn context(&self) -> ReadoutContext {
        self.lock().context()
    }
}

/// `Sim::boot`'s layer: the configuration's palette at the macro group's hold.
fn fresh_layer(config: &PokeredConfig, seed: u32) -> Option<MacroLayer> {
    let mut service = Config::default();
    service.loop_.game = GAME.to_owned();
    service.macros.mode = config.mode;
    macro_layer(&service, config.hold_ms, seed)
}

/// The image reader the engine is handed: `ImageReader`, plus a record of any address it was asked
/// for that the image does not capture (`legacy-gameboy-v1` section 8, MEM-01 amendment). Such a
/// read answers `$FF` where the live emulator would answer the register, so the engine and the
/// legacy loop could disagree; the record makes that visible rather than silent.
struct Watched<'a> {
    reader: ImageReader<'a>,
    uncaptured: &'a mut std::collections::BTreeSet<u16>,
}

impl flybrain_gb::adapter::MemoryReader for Watched<'_> {
    fn read8(&mut self, address: u16) -> u8 {
        if !flybrain_gb::captured(address) {
            self.uncaptured.insert(address);
        }
        self.reader.read8(address)
    }

    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        self.reader.read_rom(bank, address)
    }
}

fn decision_active(decision: &ChannelsDecision) -> Vec<String> {
    let mut active: Vec<String> = gameboy::GAMEBOY_BUTTONS
        .iter()
        .zip(decision.buttons)
        .filter(|(_, down)| *down)
        .map(|(name, _)| (*name).to_owned())
        .collect();
    if let Some(channel) = &decision.macro_channel {
        active.push(channel.clone());
    }
    active
}

fn intent_of(mask: u32) -> ControllerIntent {
    let control = fly_session::legacy_env::control_of("p1", mask as u8);
    ControllerIntent {
        buttons: control.buttons,
        axes: control.axes,
    }
}

impl Inner {
    fn image_of(&self, inspection: &Inspection) -> DomainResult<MemoryImage> {
        let typed = MemoryInspection::from_typed(&inspection.value)
            .map_err(|e| DomainError::before(ErrorCode::IdentityMismatch, e.0))?;
        if typed.rom_digest != self.cartridge.sha256_hex() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the inspection's romDigest is not the executor's cartridge",
            ));
        }
        let bytes = inspection
            .attachments
            .get(MEMORY_ATTACHMENT)
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::BufferInvalid,
                    format!(
                        "boundary {} has no {MEMORY_ATTACHMENT}",
                        inspection.boundary
                    ),
                    MutationCertainty::Unknown,
                )
            })?;
        MemoryImage::from_bytes(bytes).map_err(|e| {
            DomainError::new(
                ErrorCode::BufferInvalid,
                e.to_string(),
                MutationCertainty::Unknown,
            )
        })
    }

    fn context(&self) -> ReadoutContext {
        ReadoutContext {
            boot: self.adapter.boot(),
            bound: self
                .macros
                .as_ref()
                .map(MacroLayer::bound_channels)
                .unwrap_or_default(),
            location: self
                .adapter
                .location()
                .map(|(area, x, y)| Location { area, x, y }),
        }
    }

    fn contexts(&self) -> BTreeMap<Id, TypedValue> {
        BTreeMap::from([(self.config.agent_id.clone(), self.context().to_typed())])
    }

    fn progress_value(&self) -> TypedValue {
        let progress = self.adapter.progress();
        TypedValue::new(
            progress_schema(),
            json!({
                "rank": progress.rank,
                "uniqueLocations": progress.unique_locations,
                "rewardTotal": progress.reward_total,
                "best": self.ratchet.state.best,
                "recoveries": self.ratchet.state.recoveries,
            }),
        )
        .expect("progress fits the contract")
    }

    fn ledgers(&self) -> String {
        ledgers_of(&self.adapter, &self.ratchet, self.macros.as_ref())
    }

    fn observe(&mut self, image: &MemoryImage, ms: f64) -> Vec<MacroEvent> {
        let cartridge = self.cartridge.clone();
        let Inner {
            macros,
            adapter,
            uncaptured,
            ..
        } = self;
        let mut reader = Watched {
            reader: ImageReader::new(image, &cartridge),
            uncaptured,
        };
        match macros.as_mut() {
            Some(layer) => layer.observe(&mut reader, &AdapterLedger(&*adapter), ms),
            None => Vec::new(),
        }
    }

    fn event(
        &mut self,
        source_step: u64,
        rule: &str,
        ordinal: u32,
        kind: &str,
        payload: Value,
    ) -> TaskEvent {
        self.issued += 1;
        TaskEvent {
            id: event_id(&self.epoch, source_step, rule, ordinal),
            kind_id: id(kind),
            source_step,
            agent_id: Some(self.config.agent_id.clone()),
            payload: TypedValue::new(event_schema(), payload).expect("an event payload fits"),
        }
    }

    fn close(&mut self) {
        if let Some(mut record) = self.open.take() {
            record.ledgers = self.ledgers();
            self.records.push(record);
        }
    }
}

struct TaskFace(PokeredTask);
struct ExecutorFace(PokeredTask);

impl Task for TaskFace {
    fn schema(&self) -> SchemaRef {
        progress_schema()
    }

    fn inspection_attachments(&self) -> Vec<String> {
        vec![MEMORY_ATTACHMENT.to_owned()]
    }

    fn bootstrap(
        &mut self,
        initial: &Inspection,
        bindings: &[PortBinding],
    ) -> DomainResult<Bootstrap> {
        let mut inner = self.0.lock();
        if bindings.len() != 1
            || bindings[0].agent_id != inner.config.agent_id
            || bindings[0].port_id != inner.config.port_id
        {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "pokered-macros-v1 serves exactly one agent on its one port",
            ));
        }
        let image = inner.image_of(initial)?;
        // `Sim::boot` on a fresh start: the layer observes O[0] once, at the warm-up's end. The
        // agent is initialized after this, but its warm-up is the profile's constant, so the
        // brain time the legacy loop observes at is known: exactly `warmupMs`.
        let ms = gameboy::WARMUP_MS as f64;
        inner.ms = ms;
        inner.observe(&image, ms);
        inner.current = Some((initial.boundary, image));
        Ok(Bootstrap {
            contexts: inner.contexts(),
            progress: inner.progress_value(),
            events: Vec::new(),
        })
    }

    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old: &Inspection,
        new: &Inspection,
        _applied_controls: &[PortControl],
    ) -> DomainResult<Evaluation> {
        let mut inner = self.0.lock();
        match &inner.current {
            Some((boundary, _)) if *boundary == old.boundary && old.boundary == scope.step => {}
            _ => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the task evaluates the transition from the boundary it last observed",
                ));
            }
        }
        let span = fly_session::profile::span("task.image");
        let image = inner.image_of(new)?;
        drop(span);
        let ms = inner.ms;
        let k1 = scope.step + 1;
        inner.evaluations += 1;
        inner.last_source_step = k1;

        // `LegacyFrame::evaluate`: rewards from the frame just produced, then the scene, then the
        // location (the agent's, from the context), then the rank.
        let span = fly_session::profile::span("task.sample");
        let cartridge = inner.cartridge.clone();
        let rewards = {
            let Inner {
                adapter,
                uncaptured,
                ..
            } = &mut *inner;
            let mut reader = Watched {
                reader: ImageReader::new(&image, &cartridge),
                uncaptured,
            };
            adapter.sample(&mut reader, ms)
        };
        drop(span);
        let span = fly_session::profile::span("task.observe");
        let abandoned = inner.observe(&image, ms);
        drop(span);
        let span = fly_session::profile::span("task.ratchet");
        let progress = inner.adapter.progress();

        // `LegacyFrame::boundary`, the decision half: the ratchet captures on a safe frame above
        // its best -- a slot save at `Ready(k+1)` -- and says whether to roll back.
        let safe = inner.adapter.safe_for_snapshot();
        let nearer = inner
            .macros
            .as_ref()
            .is_some_and(MacroLayer::nearer_the_objective);
        let game_over = inner.adapter.game_over();
        let mut saved = false;
        let recover = inner.ratchet.observe_with_progress(
            safe,
            u64::from(progress.rank),
            progress.unique_locations as u64,
            ms as u64,
            game_over,
            nearer,
            || {
                saved = true;
                slot_marker()
            },
        );

        drop(span);
        let span = fly_session::profile::span("task.outcome");
        let mut outcome = AgentOutcome::default();
        let mut events = Vec::new();
        for (n, event) in rewards.iter().enumerate() {
            let ordinal = n as u32;
            let task_event = inner.event(
                k1,
                "reward",
                ordinal,
                "pokered.reward",
                json!({"kind": event.kind, "label": event.label, "value": event.value}),
            );
            outcome.rewards.push(Reward {
                event_id: task_event.id.clone(),
                rule_id: id(event.kind),
                value: event.value,
            });
            // `Stimulus.durationMs` is positive: an event that drives no pulse is a Reward alone,
            // as the legacy `stimulate(0)` changes nothing.
            if event.stimulation_ms > 0 {
                outcome.stimulations.push(Stimulus {
                    id: task_event.id.clone(),
                    kind_id: gameboy::STIMULUS_REWARD_PULSE.to_owned(),
                    duration_ms: f64::from(event.stimulation_ms),
                });
            }
            events.push(task_event);
        }
        let episode = recover.then(|| EpisodeRequest {
            kind: EpisodeRequestKind::Rollback,
            reason: id(if game_over { "game-over" } else { "stall" }),
            outcome: RollbackRequest {
                slot_id: SLOT.to_owned(),
                trigger: if game_over { "game-over" } else { "stall" }.to_owned(),
            }
            .to_typed(),
        });

        if let Some(record) = inner.open.as_mut() {
            record
                .macro_events
                .extend(abandoned.iter().map(|e| macro_json("evaluate", e)));
            record.wram_digest = sha256_hex(&image.as_bytes()[0xc000..=0xdfff]);
            record.rewards = rewards
                .iter()
                .map(|e| json!({"kind": e.kind, "value": e.value, "stimulationMs": e.stimulation_ms}))
                .collect();
            record.rank = progress.rank;
            record.saved = saved;
        }
        drop(span);
        let span = fly_session::profile::span("task.contexts");
        let next_contexts = inner.contexts();
        let progress_value = inner.progress_value();
        drop(span);
        inner.current = Some((new.boundary, image));
        if !recover {
            inner.close();
        }
        Ok(Evaluation {
            outcomes: BTreeMap::from([(inner.config.agent_id.clone(), outcome)]),
            next_contexts,
            progress: progress_value,
            events,
            episode,
            slot_saves: if saved { vec![id(SLOT)] } else { Vec::new() },
        })
    }

    fn rollback(
        &mut self,
        scope: &Scope,
        restored: &Inspection,
    ) -> DomainResult<RollbackEvaluation> {
        let mut inner = self.0.lock();
        let image = inner.image_of(restored)?;
        let ms = inner.ms;
        inner.epoch = scope.epoch.clone();
        // `recover_game`'s task half: the adapter's transient observations; then the executor
        // cancels and observes the restored game (`LegacyFrame::rollback`).
        inner.adapter.clear_transient();
        let mut events = match inner.macros.as_mut() {
            Some(layer) => layer.cancel(ms),
            None => Vec::new(),
        };
        events.extend(inner.observe(&image, ms));
        if let Some(record) = inner.open.as_mut() {
            record.rolled_back = true;
            record
                .macro_events
                .extend(events.iter().map(|e| macro_json("rollback", e)));
        }
        inner.current = Some((restored.boundary, image));
        inner.close();
        Ok(RollbackEvaluation {
            next_contexts: inner.contexts(),
            events: Vec::new(),
        })
    }

    fn restored(
        &mut self,
        scope: &Scope,
        restored: &Inspection,
        clock: &RationalNs,
    ) -> DomainResult<Option<BTreeMap<Id, TypedValue>>> {
        let mut inner = self.0.lock();
        let image = inner.image_of(restored)?;
        let ms = clock_ms(clock)?;
        inner.epoch = scope.epoch.clone();
        inner.ms = ms;
        // `Sim::boot` after a restore: a fresh layer (installed by `install_restore`) observes the
        // restored game once, at the restored brain time, before the first frame decides.
        inner.observe(&image, ms);
        inner.current = Some((restored.boundary, image));
        Ok(Some(inner.contexts()))
    }

    fn progress(&self) -> TypedValue {
        self.0.lock().progress_value()
    }

    fn evaluations(&self) -> u64 {
        self.0.lock().evaluations
    }

    fn capture(&self) -> DomainResult<TypedValue> {
        let (half, slot_filled) = self.0.task_half();
        PokeredTask::ledger_of(&half, slot_filled).map(|(typed, _)| typed)
    }

    fn capture_attachments(&self) -> DomainResult<BTreeMap<String, Vec<u8>>> {
        let (half, slot_filled) = self.0.task_half();
        PokeredTask::ledger_of(&half, slot_filled).map(|(_, attachments)| attachments)
    }

    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()> {
        self.validate_restore_with(state, &BTreeMap::new())
    }

    fn validate_restore_with(
        &self,
        state: &TypedValue,
        attachments: &BTreeMap<String, Vec<u8>>,
    ) -> DomainResult<()> {
        let inner = self.0.lock();
        parse_ledger(&inner, state, attachments).map(|_| ())
    }

    fn install_restore(&mut self, epoch: &Id, state: &TypedValue) -> DomainResult<()> {
        self.install_restore_with(epoch, state, &BTreeMap::new())
    }

    fn install_restore_with(
        &mut self,
        epoch: &Id,
        state: &TypedValue,
        attachments: &BTreeMap<String, Vec<u8>>,
    ) -> DomainResult<()> {
        let mut inner = self.0.lock();
        let (adapter, ratchet) = parse_ledger(&inner, state, attachments)?;
        inner.adapter = adapter;
        inner.ratchet = ratchet;
        // `legacy-transient-reset`: a fresh executor, nothing running, empty ledgers.
        inner.macros = fresh_layer(&inner.config, inner.palette_seed);
        inner.current = None;
        inner.open = None;
        inner.epoch = epoch.clone();
        inner.evaluations = 0;
        inner.issued = 0;
        inner.last_source_step = 0;
        Ok(())
    }

    fn rebase_ids(&self, _to_epoch: &Id) -> DomainResult<BTreeMap<Id, Id>> {
        Err(DomainError::before(
            ErrorCode::Unsupported,
            "pokered-macros-v1 compares runs by FLY_TRACE, not by rebased event ids",
        ))
    }

    fn event_watermarks(&self) -> (u64, u64) {
        let inner = self.0.lock();
        (inner.last_source_step, inner.issued)
    }
}

fn parse_ledger(
    inner: &Inner,
    state: &TypedValue,
    attachments: &BTreeMap<String, Vec<u8>>,
) -> DomainResult<(PokemonRedReward, Ratchet)> {
    if state.schema != ledger_schema() {
        return Err(state_error(
            "the captured ledger is not a pokered-macros-v1 ledger",
        ));
    }
    let bytes = attachments
        .get(REWARD_ATTACHMENT)
        .ok_or_else(|| state_error("the ledger's reward attachment is missing"))?;
    let declared = &state.value["reward"];
    if declared["digest"].as_str() != Some(digest_of_bytes(bytes).as_str())
        || declared["byteLength"].as_str() != Some(bytes.len().to_string().as_str())
    {
        return Err(state_error(
            "the reward attachment is not the one the ledger names",
        ));
    }
    let reward: Value = serde_json::from_slice(bytes)
        .map_err(|e| state_error(format!("the reward attachment is not JSON: {e}")))?;
    let mut value = state.value.clone();
    value["reward"] = reward;
    let half = TaskHalf::from_ledger(&value).map_err(state_error)?;
    let slot_filled = state
        .value
        .get("slotFilled")
        .and_then(Value::as_bool)
        .ok_or_else(|| state_error("the ledger has no slotFilled"))?;
    // `LegacyFrame::restore`'s order: the adapter (unless the file has none), then the ratchet
    // bounded by the adapter's ladder.
    let mut adapter = PokemonRedReward::new();
    if !half.reward.is_null() {
        adapter
            .import_state(&half.reward)
            .map_err(|e| state_error(format!("adapter: {e}")))?;
    }
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    ratchet
        .import(
            Some(half.ratchet),
            slot_filled.then(slot_marker),
            adapter.rank_ladder().len(),
        )
        .map_err(|e| state_error(format!("ratchet: {e}")))?;
    let _ = inner;
    Ok((adapter, ratchet))
}

impl ActionExecutor for ExecutorFace {
    fn apply(
        &mut self,
        scope: &Scope,
        decision: &TypedValue,
        current: &Inspection,
        _progress: &TypedValue,
        clock: &RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)> {
        let mut inner = self.0.lock();
        let decision = ChannelsDecision::from_typed(decision)
            .map_err(|e| DomainError::before(ErrorCode::IdentityMismatch, e.0))?;
        let image = match &inner.current {
            Some((boundary, image)) if *boundary == current.boundary && *boundary == scope.step => {
                image.clone()
            }
            _ => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the executor reads the boundary the task last observed",
                ));
            }
        };
        // The clock is the brain time after Prepare, whole milliseconds (the legacy
        // `network.ms`), which is what the macro layer's windows read.
        let ms = clock_ms(clock)?;
        inner.ms = ms;
        let mut active = decision_active(&decision);
        let bound = inner
            .macros
            .as_ref()
            .map(MacroLayer::bound_channels)
            .unwrap_or_default();
        if let Some(driver) = inner.driver.as_mut() {
            driver.readout(ms, &bound, &mut active);
        }
        let raw = to_button_mask(&active);
        let cartridge = inner.cartridge.clone();
        let (mask, events) = {
            let Inner {
                macros,
                adapter,
                uncaptured,
                ..
            } = &mut *inner;
            match macros.as_mut() {
                Some(layer) => {
                    let mut reader = Watched {
                        reader: ImageReader::new(&image, &cartridge),
                        uncaptured,
                    };
                    let decided =
                        layer.decide(&active, raw, ms, &mut reader, &AdapterLedger(&*adapter));
                    (decided.mask, decided.events)
                }
                None => (raw, Vec::new()),
            }
        };
        if inner.config.record {
            inner.open = Some(TaskRecord {
                boundary: scope.step,
                decision: active,
                bound,
                mask,
                macro_events: events.iter().map(|e| macro_json("execute", e)).collect(),
                ..TaskRecord::default()
            });
        }
        Ok((intent_of(mask), Vec::new()))
    }

    fn capture(&self) -> DomainResult<TypedValue> {
        TypedValue::new(executor_schema(), json!({"executor": gameboy::EXECUTOR_ID}))
            .map_err(|e| DomainError::invalid(e.0))
    }

    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()> {
        if state.schema != executor_schema()
            || state.value.get("executor").and_then(Value::as_str) != Some(gameboy::EXECUTOR_ID)
        {
            return Err(state_error(
                "the captured executor is not pokered-macros-v1",
            ));
        }
        Ok(())
    }

    /// Nothing to install: the executor's state is transient (`legacy-transient-reset`), and
    /// the task's install already gave it a fresh layer.
    fn install_restore(&mut self, state: &TypedValue) -> DomainResult<()> {
        self.validate_restore(state)
    }
}

/// Brain time in ms from the executor's clock: exact whole nanoseconds of whole milliseconds.
pub fn clock_ms(clock: &RationalNs) -> DomainResult<f64> {
    if clock.denominator != 1 || !clock.numerator.is_multiple_of(1_000_000) {
        return Err(DomainError::invalid(
            "the executor clock is not a whole number of milliseconds",
        ));
    }
    Ok((clock.numerator / 1_000_000) as f64)
}
