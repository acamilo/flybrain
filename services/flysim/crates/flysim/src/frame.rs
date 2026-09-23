//! The legacy frame: one Game Boy frame of the live loop, in its phases, in the one order.
//!
//! This is the order `simloop.rs` runs on the stream, and the order every harness that claims to
//! measure the stream runs: the trap hunt, the palette bench, the room-escape bench, and the
//! stub-readout drivers of the ROM tests and the scene probe. It used to be written out in each of
//! them, and the copies had drifted: the benches ticked the brain through `NeuralAgent::tick`, which
//! installs the previous frame and its rewards *after* the next ticks, so every bench ran the
//! brain one frame behind the stream. There is one copy now, and the parity oracle is this file.
//!
//! The phases are named for the lockstep transaction they become in the session framework
//! (`docs/design/session-framework/legacy-gameboy-v1.md` section 4):
//!
//! | phase | what it does | lockstep |
//! | --- | --- | --- |
//! | (host) | drains commands: sugar and operator pulses are applied here, before the ticks | admission at `Ready(k)` |
//! | [`LegacyFrame::prepare`] | 16 or 17 brain ticks, the remainder carried; decode with the scene's bound channels and the blocked direction | A: `Agent.Prepare` |
//! | [`LegacyFrame::execute`] | the raw mask, then the macro layer decides the mask | B: the executor |
//! | [`LegacyFrame::advance`] | the joypad, one emulator frame, the framebuffer, the audio | B: `Environment.Advance` |
//! | [`LegacyFrame::evaluate`] | reward events, the macro layer's observation, the location, the rank | C: the task |
//! | [`LegacyFrame::commit`] | install the frame, one stimulation per event, one reinforcement | D: `Agent.Commit` |
//! | (host) | the milestone archive | a capture at `Ready(k+1)` |
//! | [`LegacyFrame::boundary`] | the ratchet's capture and decision, and the rollback when it fires | C decides; slot save and rollback at `Ready(k+1)` |
//!
//! Two moves from the order `simloop.rs` used to spell out, both between operations that touch
//! disjoint state, so neither changes a byte: the visual frame is installed in `commit` rather
//! than straight after the emulator frame (nothing reads the network in between), and the
//! stimulation and reinforcement come after the macro layer's observation and the location
//! (which read the emulator and the adapter, never the network). The milestone archive stays
//! where the stream has it -- after the reinforcement, before the ratchet captures -- which is
//! why the ratchet is its own call after `transition`: the host takes its archive between the
//! two. `FLY_TRACE` (`crate::trace`) records every phase, and a trace of the stream from one
//! checkpoint is byte-identical before and after this extraction.

use anyhow::{Result, anyhow};
use flybrain_core::agent::NeuralAgent;
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_gb::adapter::{GameAdapter, ProgressSnapshot};
use flybrain_gb::emulator::{Emulator, FRAMEBUFFER_LEN};
use flybrain_gb::macros::AdapterLedger;
use flybrain_gb::ratchet::{Ratchet, Snapshot};
use flybrain_gb::recovery::{NeuralRecovery, recover_game};
use flybrain_gb::RewardEvent;

use crate::macros::{MacroEvent, MacroLayer, Silence};
use crate::trace::FrameTrace;

/// Everything one frame reads and writes besides the frame's own state: the parts the loop owns.
pub struct Parts<'a> {
    pub agent: &'a mut NeuralAgent,
    pub emulator: &'a mut Emulator,
    pub adapter: &'a mut dyn GameAdapter,
    pub ratchet: &'a mut Ratchet,
    /// `None` in raw mode, where not one line of the macro layer runs.
    pub macros: Option<&'a mut MacroLayer>,
}

/// Where a host may look in, or time a phase. Every method defaults to nothing.
///
/// The stream's loop uses [`FrameObserver::after`] for its per-phase profile and nothing else. A
/// harness may read the emulator between phases to measure the run, and a stub-readout harness may
/// replace the decision in [`FrameObserver::readout`]; nothing else about the frame is open.
pub trait FrameObserver {
    /// A phase has finished. `agent` is lent for the profiler's kernel timings.
    fn after(&mut self, _phase: FramePhase, _agent: &mut NeuralAgent) {}

    /// The decoded decision, before the executor sees it. The stream never replaces it; the trap
    /// hunt's `FLY_TRAP_STUB` does, and the brain still ticks exactly as it would.
    fn readout(&mut self, _ms: f64, _bound: Option<&[String]>, _active: &mut Vec<String>) {}

    /// Just before the executor decides: O[k] is on the emulator, the palette is the one dealt
    /// for it.
    fn before_execute(&mut self, _frame: &LegacyFrame, _parts: &mut Parts<'_>, _active: &[String]) {
    }

    /// The executor has decided and the mask is not yet on the joypad.
    fn executed(&mut self, _frame: &LegacyFrame, _parts: &mut Parts<'_>, _executed: &Executed) {}
}

/// The observer that observes nothing.
impl FrameObserver for () {}

/// The points [`FrameObserver::after`] is called at, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePhase {
    /// The brain ticks are done.
    Ticked,
    /// The decode and the executor's decision are done.
    Executed,
    /// The emulator frame has run.
    Emulated,
    /// The framebuffer and the audio are taken.
    Advanced,
    /// Rewards are sampled, the scene observed and the transition committed to the brain.
    Committed,
}

/// Phase B's result.
#[derive(Debug, Default)]
pub struct Executed {
    /// The mask the emulator is given.
    pub mask: u32,
    /// The macro layer's start and finish events, in order.
    pub events: Vec<MacroEvent>,
    /// Why nothing was pressed, when nothing was (`crate::macros::Decision::silence`).
    pub silence: Option<Silence>,
}

/// Phase C's result.
#[derive(Debug)]
pub struct Evaluated {
    /// Reward events from the frame just produced, in adapter order.
    pub rewards: Vec<RewardEvent>,
    /// The macro layer's own events from observing that frame (at most one abandonment).
    pub abandoned: Vec<MacroEvent>,
    pub progress: ProgressSnapshot,
}

/// One transition `k -> k+1`, up to and including its commit.
#[derive(Debug)]
pub struct Transition {
    /// Brain ticks this frame advanced.
    pub ticks: u64,
    /// The brain clock after them, which is the clock of every phase that follows.
    pub ms: f64,
    /// The scene's bound macro channels the decode was masked to; `None` in raw mode.
    pub bound: Option<Vec<String>>,
    /// The decision the executor was given.
    pub active: Vec<String>,
    pub executed: Executed,
    /// The frame's audio, binjgb's unsigned 8-bit interleaved stereo.
    pub audio: Vec<u8>,
    pub evaluated: Evaluated,
}

/// Why the ratchet rolled the game back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackTrigger {
    GameOver,
    Stall,
}

/// A rollback at the boundary.
#[derive(Debug)]
pub struct Rollback {
    pub trigger: RollbackTrigger,
    /// The running macro's abandonment and the restored scene's observation, in order.
    pub events: Vec<MacroEvent>,
}

/// What happened at `Ready(k+1)`.
#[derive(Debug, Default)]
pub struct Boundary {
    /// The ratchet captured a slot this boundary.
    pub captured: bool,
    pub rollback: Option<Rollback>,
}

/// The neural half of a ratchet recovery, wired to `flybrain-core`.
struct AgentRecovery<'a> {
    agent: &'a mut NeuralAgent,
}

impl NeuralRecovery for AgentRecovery<'_> {
    fn clear_decoder_holds(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.decoder.clear_holds(ms);
    }

    fn clear_eligibility(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.network.plasticity.clear_eligibility(ms);
    }

    fn set_visual_frame(&mut self, frame: &[u8]) {
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent.network.set_visual_frame(frame, width, height);
    }
}

/// The frame's own state: the clock remainder, the frame counter, the frame on screen, the mask,
/// and the readout's blocked-direction window.
///
/// The window (`docs/readout.md`) is the player's area and tile as of the last frame the adapter
/// reported one, the channel the group is holding, and the brain clock at which *either* of those
/// last changed. A direction is only blamed once it has been held for a whole `blocked_ms` with no
/// movement, so a direction that has just won is never blamed for a wall the previous one hit.
/// All three are transient and never checkpointed: one hold of a wall after a restart is cheaper
/// than a stale position surviving a restore (`restore: legacy-transient-reset`).
pub struct LegacyFrame {
    /// Fractional millisecond carried into the next frame; checkpointed.
    pub remainder: f64,
    /// Frames the emulator has run in this fly's life; checkpointed.
    pub frame_counter: u64,
    /// The frame on screen: the last one produced, or a restored slot's.
    pub frame_buffer: Vec<u8>,
    /// The mask on the joypad; checkpointed.
    pub buttons: u32,
    pub location: Option<(u32, u32, u32)>,
    pub held_channel: Option<String>,
    pub blocked_since_ms: f64,
    trace: Option<FrameTrace>,
}

impl Default for LegacyFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl LegacyFrame {
    /// The state of a fresh process: nothing held, no location, the blocked window starting at
    /// brain time 0 (legacy-gameboy-v1 section 14), and a black frame.
    pub fn new() -> Self {
        Self {
            remainder: 0.0,
            frame_counter: 0,
            frame_buffer: vec![0u8; FRAMEBUFFER_LEN],
            buttons: 0,
            location: None,
            held_channel: None,
            blocked_since_ms: 0.0,
            trace: None,
        }
    }

    /// Record every phase into `trace` (`FLY_TRACE`).
    pub fn with_trace(mut self, trace: Option<FrameTrace>) -> Self {
        self.trace = trace;
        self
    }

    /// The trace, when one is on: a host records its admissions and captures through it.
    pub fn trace_mut(&mut self) -> Option<&mut FrameTrace> {
        self.trace.as_mut()
    }

    // -- setup -------------------------------------------------------------------------------

    /// A fresh start: one frame with no button down, then the brain's warm-up on it
    /// (`Environment.Initialize`, legacy-gameboy-v1 section 9). Returns that frame's audio.
    pub fn initialize(&mut self, emulator: &mut Emulator, agent: &mut NeuralAgent) -> Result<Vec<u8>> {
        emulator
            .run_frame()
            .map_err(|error| anyhow!("running the first frame: {error}"))?;
        self.frame_buffer.copy_from_slice(emulator.framebuffer());
        self.frame_counter = 1;
        let audio = emulator.take_audio_u8();
        agent.warmup(Some(&self.frame_buffer)).map_err(|error| anyhow!("{error}"))?;
        Ok(audio)
    }

    /// Everything a `FLYSIM01` checkpoint restores into the parts and the frame, in the order the
    /// stream restores it; the host checks the cartridge and the compatibility string first.
    ///
    /// The readout transient is left as a fresh process has it (`legacy-transient-reset`), which
    /// is what a restart of the service gives the fly. `import_state` of the agent is
    /// self-validating, so a refused checkpoint leaves the agent as it was.
    pub fn restore(&mut self, parts: &mut Parts<'_>, checkpoint: &crate::store::Checkpoint) -> Result<()> {
        let runtime = &checkpoint.runtime;
        if runtime.framebuffer.len() != FRAMEBUFFER_LEN {
            anyhow::bail!("checkpoint framebuffer is {} bytes", runtime.framebuffer.len());
        }
        parts.agent.import_state(&checkpoint.agent).map_err(|error| anyhow!("{error}"))?;
        parts.emulator.import_state(&runtime.emulator).map_err(|error| anyhow!("{error}"))?;
        if !runtime.reward.is_null() {
            parts.adapter.import_state(&runtime.reward).map_err(|error| anyhow!("{error}"))?;
        }
        let snapshot = if runtime.ratchet_game.is_empty() {
            None
        } else {
            Some(Snapshot { game: runtime.ratchet_game.clone(), frame: runtime.ratchet_frame.clone() })
        };
        parts
            .ratchet
            .import(Some(runtime.ratchet), snapshot, parts.adapter.rank_ladder().len())
            .map_err(|error| anyhow!("{error}"))?;

        self.remainder = checkpoint.agent.remainder;
        self.frame_counter = runtime.emulator_frame;
        self.buttons = runtime.buttons;
        self.frame_buffer.copy_from_slice(&runtime.framebuffer);
        let (width, height) = (parts.agent.frame.width, parts.agent.frame.height);
        parts.agent.network.set_visual_frame(&self.frame_buffer, width, height);
        parts.emulator.set_buttons(self.buttons as u8);
        Ok(())
    }

    // -- the transition ----------------------------------------------------------------------

    /// Transition `k -> k+1`, prepare through commit. The host takes its milestone archive after
    /// this and then calls [`LegacyFrame::boundary`].
    pub fn transition(
        &mut self,
        parts: &mut Parts<'_>,
        observer: &mut dyn FrameObserver,
    ) -> Result<Transition> {
        let ticks = self.tick(parts.agent);
        observer.after(FramePhase::Ticked, parts.agent);

        // Not the mode string: the adapter decides what counts as boot, because a platformer
        // needs the permissive Start variant in four of its five modes (`GameAdapter::boot`).
        let boot = parts.adapter.boot();
        // The scene's own macro buttons, for the macro group's per-decision mask
        // (`docs/design/macros.md` section 12: "unbound channels are masked from the decision").
        // They are the bindings the previous frame's `observe` dealt, which is the palette the
        // page is showing, so the fly is choosing among exactly the buttons the audience can see.
        let bound = parts.macros.as_deref().map(MacroLayer::bound_channels);
        let mut active = self.decode(parts.agent, boot, bound.as_deref());
        let ms = parts.agent.network.ms;
        observer.readout(ms, bound.as_deref(), &mut active);

        observer.before_execute(self, parts, &active);
        let raw = to_button_mask(&active);
        let executed = self.execute(
            parts.macros.as_deref_mut(),
            &active,
            raw,
            ms,
            parts.emulator,
            &*parts.adapter,
        );
        observer.executed(self, parts, &executed);
        observer.after(FramePhase::Executed, parts.agent);

        let audio = self.advance(parts.emulator, parts.agent, observer)?;
        observer.after(FramePhase::Advanced, parts.agent);

        let evaluated = self.evaluate(parts.emulator, parts.adapter, parts.macros.as_deref_mut(), ms);
        self.commit(parts.agent, &evaluated.rewards, ms);
        observer.after(FramePhase::Committed, parts.agent);

        Ok(Transition { ticks, ms, bound, active, executed, audio, evaluated })
    }

    /// The rest of phase B and phase C behind a stub readout, for the drivers that measure the
    /// macros without a brain (the ROM tests, the scene probe). The driver decodes its stub,
    /// calls [`LegacyFrame::execute`] with no raw mask, reads what it measures, and then this runs
    /// the frame and evaluates it. There is no commit and no ratchet.
    ///
    /// A stub has no phase A to advance its clock in, so it keeps its own and advances it with the
    /// emulator frame: it decides at its clock and evaluates at `evaluate_ms`, one frame later.
    pub fn stub_advance(
        &mut self,
        macros: Option<&mut MacroLayer>,
        emulator: &mut Emulator,
        adapter: &mut dyn GameAdapter,
        evaluate_ms: f64,
    ) -> Result<Evaluated> {
        self.run(emulator)?;
        let _ = self.take_frame(emulator);
        Ok(self.evaluate(emulator, adapter, macros, evaluate_ms))
    }

    /// Phase A: brain ticks and the decode, masked to `bound`.
    pub fn prepare(
        &mut self,
        agent: &mut NeuralAgent,
        boot: bool,
        bound: Option<&[String]>,
    ) -> (u64, Vec<String>) {
        let ticks = self.tick(agent);
        (ticks, self.decode(agent, boot, bound))
    }

    /// Phase A, the ticks: 16 or 17 whole milliseconds, the fraction carried to the next frame.
    pub fn tick(&mut self, agent: &mut NeuralAgent) -> u64 {
        if let Some(trace) = self.trace.as_mut() {
            trace.begin(self.frame_counter, agent.network.ms);
        }
        self.remainder += agent.ms_per_frame;
        let steps = self.remainder.floor();
        self.remainder -= steps;
        agent.network.step(steps as u64);
        if let Some(trace) = self.trace.as_mut() {
            trace.ticked(steps as u64, self.remainder, &agent.network);
        }
        steps as u64
    }

    /// Phase A, the readout: decode the rates with the blocked direction and the bound channels,
    /// and restart the blocked window when the held channel changes.
    pub fn decode(&mut self, agent: &mut NeuralAgent, boot: bool, bound: Option<&[String]>) -> Vec<String> {
        let ms = agent.network.ms;
        let rates = agent.network.rates.clone();
        // The readout's blocked-direction cooldown (`docs/readout.md`): the direction the group
        // is holding, once the adapter's position has stood still for a whole `blocked_ms`. The
        // loop owns the clock and the position; the decoder only learns *which* channel did
        // nothing. `blocked_ms == 0` -- the platformer preset, and the Game Boy preset before
        // v0.1.1 -- switches the rule off here, before the decoder is asked.
        let blocked_ms = agent.decoder.blocked_ms();
        let blocked = (blocked_ms > 0.0 && ms - self.blocked_since_ms >= blocked_ms)
            .then(|| agent.decoder.current())
            .flatten()
            .map(str::to_string);
        let active = agent.decoder.decode_bound(&rates, ms, boot, blocked.as_deref(), bound);
        // A new winner starts its own window: it has not had a hold to move in yet.
        let held = agent.decoder.current().map(str::to_string);
        if held != self.held_channel {
            self.held_channel = held;
            self.blocked_since_ms = ms;
        }
        active
    }

    /// Phase B: the mask. `raw_mask` is the decision's own buttons (`to_button_mask`); in macros
    /// mode the mask that reaches the emulator is the running macro's, or nothing, or -- on the
    /// title screen alone -- the raw mask (`docs/design/macros.md` sections 4 and 12). In raw mode
    /// it is the raw mask.
    pub fn execute(
        &mut self,
        macros: Option<&mut MacroLayer>,
        active: &[String],
        raw_mask: u32,
        ms: f64,
        emulator: &mut Emulator,
        adapter: &dyn GameAdapter,
    ) -> Executed {
        self.buttons = raw_mask;
        let executed = match macros {
            // The adapter's exploration ledger answers the ways out' "unvisited" -- read-only, by
            // `&dyn`, and the only thing the palette is told about the reward side.
            Some(layer) => {
                let ledger = AdapterLedger(adapter);
                let decision = layer.decide(active, self.buttons, ms, emulator, &ledger);
                self.buttons = decision.mask;
                Executed { mask: decision.mask, events: decision.events, silence: decision.silence }
            }
            None => Executed { mask: self.buttons, ..Executed::default() },
        };
        if let Some(trace) = self.trace.as_mut() {
            trace.decided(active);
            trace.executed(self.buttons, &executed.events);
        }
        executed
    }

    /// Phase B, the environment: the joypad, one emulator frame, the frame it drew and its audio.
    pub fn advance(
        &mut self,
        emulator: &mut Emulator,
        agent: &mut NeuralAgent,
        observer: &mut dyn FrameObserver,
    ) -> Result<Vec<u8>> {
        self.run(emulator)?;
        observer.after(FramePhase::Emulated, agent);
        Ok(self.take_frame(emulator))
    }

    fn run(&mut self, emulator: &mut Emulator) -> Result<()> {
        emulator.set_buttons(self.buttons as u8);
        emulator
            .run_frame()
            .map_err(|error| anyhow!("frame {}: {error}", self.frame_counter + 1))?;
        self.frame_counter += 1;
        Ok(())
    }

    fn take_frame(&mut self, emulator: &mut Emulator) -> Vec<u8> {
        self.frame_buffer.copy_from_slice(emulator.framebuffer());
        if let Some(trace) = self.trace.as_mut() {
            trace.advanced(&self.frame_buffer, emulator);
        }
        emulator.take_audio_u8()
    }

    /// Phase C: rewards from the frame just produced, then the scene, then the location.
    ///
    /// `docs/design/macros.md` section 2: the scene is sampled once per game frame, after the
    /// frame, so the palette the fly is offered on the next frame is the one for the frame it can
    /// actually see.
    pub fn evaluate(
        &mut self,
        emulator: &mut Emulator,
        adapter: &mut dyn GameAdapter,
        macros: Option<&mut MacroLayer>,
        ms: f64,
    ) -> Evaluated {
        let rewards = adapter.sample(emulator, ms);
        // At most one: a macro that has run into a scene with no palette.
        let abandoned = match macros {
            Some(layer) => {
                let ledger = AdapterLedger(&*adapter);
                layer.observe(emulator, &ledger, ms)
            }
            None => Vec::new(),
        };
        // The cooldown's other reset: the player actually moved. `None` -- a battle, a script, a
        // map transition -- is no information rather than "still", so the rule cannot fire while
        // the fly has no control anyway.
        let location = adapter.location();
        if location.is_some() && location != self.location {
            self.location = location;
            self.blocked_since_ms = ms;
        }
        let progress = adapter.progress();
        if let Some(trace) = self.trace.as_mut() {
            trace.evaluated(&rewards, &abandoned, progress.rank);
        }
        Evaluated { rewards, abandoned, progress }
    }

    /// Phase D: the frame just produced becomes the next ticks' visual drive, each reward event
    /// stimulates once, and the summed value reinforces once.
    pub fn commit(&mut self, agent: &mut NeuralAgent, rewards: &[RewardEvent], ms: f64) {
        let (width, height) = (agent.frame.width, agent.frame.height);
        agent.network.set_visual_frame(&self.frame_buffer, width, height);
        let mut total = 0.0;
        for event in rewards {
            agent.network.stimulate(f64::from(event.stimulation_ms));
            total += event.value;
        }
        if agent.network.plasticity.enabled {
            agent.network.plasticity.reinforce(total, ms);
        }
    }

    // -- the boundary ------------------------------------------------------------------------

    /// `Ready(k+1)`: the ratchet captures on a safe frame above its best, observes, and rolls the
    /// game back when it says so.
    pub fn boundary(
        &mut self,
        parts: &mut Parts<'_>,
        progress: &ProgressSnapshot,
        ms: f64,
    ) -> Result<Boundary> {
        let safe = parts.adapter.safe_for_snapshot();
        let capture_due = safe && u64::from(progress.rank) > parts.ratchet.state.best;
        let captured = if capture_due {
            Some(Snapshot {
                game: parts
                    .emulator
                    .export_state()
                    .map_err(|error| anyhow!("capturing a ratchet snapshot: {error}"))?,
                frame: self.frame_buffer.clone(),
            })
        } else {
            None
        };
        // The stall window's second progress signal (`docs/design/ladder.md`, the 2026-09-17
        // rule as amended 2026-09-22): the macro layer answers "nearer the objective" with the map
        // graph it already walks (`docs/design/macros.md` section 12.15); in raw mode there is no
        // layer and no objective, and the answer is false.
        let nearer = parts.macros.as_deref().is_some_and(MacroLayer::nearer_the_objective);
        let trace = &mut self.trace;
        let mut saved = false;
        let recover = parts.ratchet.observe_with_progress(
            safe,
            u64::from(progress.rank),
            progress.unique_locations as u64,
            ms as u64,
            parts.adapter.game_over(),
            nearer,
            || {
                let snapshot =
                    captured.expect("the ratchet only captures when a snapshot was prepared");
                if let Some(trace) = trace.as_mut() {
                    trace.slot_saved(&snapshot.game);
                }
                saved = true;
                snapshot
            },
        );
        let rollback = if recover {
            // Two triggers, two stories on the ticker: a game over ended the run, a stall did not.
            let trigger =
                if parts.adapter.game_over() { RollbackTrigger::GameOver } else { RollbackTrigger::Stall };
            let events = self.rollback(parts)?;
            Some(Rollback { trigger, events })
        } else {
            None
        };
        Ok(Boundary { captured: saved, rollback })
    }

    /// The ratchet's game-only rollback (`legacy-ratchet-rollback-v1`): the slot is restored, the
    /// brain's holds and eligibility are cleared and it is shown the slot's frame, the buttons are
    /// released, the blocked window restarts, and a running macro is abandoned and the restored
    /// scene observed. The brain clock, its learning and the ratchet's ledger carry on.
    pub fn rollback(&mut self, parts: &mut Parts<'_>) -> Result<Vec<MacroEvent>> {
        let snapshot = Snapshot {
            game: parts
                .ratchet
                .game()
                .ok_or_else(|| anyhow!("the ratchet asked to recover with no snapshot"))?
                .to_vec(),
            frame: parts
                .ratchet
                .frame()
                .ok_or_else(|| anyhow!("the ratchet snapshot has no framebuffer"))?
                .to_vec(),
        };
        let frame = {
            let mut neural = AgentRecovery { agent: parts.agent };
            recover_game(parts.emulator, parts.adapter, &mut neural, &snapshot)
                .map_err(|error| anyhow!("recovering the game: {error}"))?
        };
        self.frame_buffer.copy_from_slice(&frame);
        self.buttons = 0;
        parts.emulator.set_buttons(0);
        let ms = parts.agent.network.ms;
        self.location = parts.adapter.location();
        self.held_channel = None;
        self.blocked_since_ms = ms;
        // A rollback restores a game the running macro's plan was never made for, so the macro is
        // abandoned rather than carried over a map change it cannot see. The frame's `observe` ran
        // before the ratchet decided, so the scene describes the run just thrown away: re-detect
        // on the restored game rather than decide the next frame against a map the fly is no
        // longer standing on.
        let events = match parts.macros.as_deref_mut() {
            Some(layer) => {
                let mut events = layer.cancel(ms);
                let ledger = AdapterLedger(&*parts.adapter);
                events.extend(layer.observe(parts.emulator, &ledger, ms));
                events
            }
            None => Vec::new(),
        };
        if let Some(trace) = self.trace.as_mut() {
            trace.rolled_back(&events);
        }
        Ok(events)
    }

    /// Write the open transition of the trace, if one is on.
    pub fn finish_trace(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            trace.finish();
        }
    }
}
