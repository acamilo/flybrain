//! Macros mode: the layer between the readout and the button register.
//!
//! `docs/design/macros.md` sections 1, 4 and 12 are the contract. In raw mode this module is not
//! constructed at all — [`macro_layer`] returns `None` — and the sim loop's button path is the
//! one it has always been: decode, `to_button_mask`, `set_buttons`. In macros mode the same
//! decode happens, plus one more exclusive group: the macro channels, one population per macro
//! type, of which only the scene's own bound channels compete. Whichever of them wins, starts.
//!
//! Section 12 is what this module lost, and the loss is the point: there is no plan, no rank, no
//! prior, no scene weight, no cursor and no blend. The layer deals the scene's buttons, hands
//! their channels to the decoder, and starts the one the decoder held up.
//!
//! Two calls per frame, at the two points in the loop's order where they belong
//! (`docs/design/flysim.md` section 4):
//!
//! | loop step | call | what it does |
//! | --- | --- | --- |
//! | 4, apply the buttons | [`MacroLayer::decide`] | steps a running macro, or starts one |
//! | after 7, sample rewards | [`MacroLayer::observe`] | detects the scene and deals the palette |
//!
//! And twice outside a frame: once at the end of boot, so the first frame is decided on a real
//! palette rather than an empty one, and once in the ratchet's recovery path, where
//! [`MacroLayer::cancel`] abandons the running macro and an `observe` re-reads the scene from the
//! restored game before the next frame decides anything.
//!
//! The rules, each with the line that implements it:
//!
//! - **a running macro owns the buttons.** [`MacroLayer::decide`] steps it before it looks at a
//!   single channel, and its mask is what goes to the emulator.
//! - **the fly chooses, or nothing happens.** With no macro running, the macro group's winner
//!   names the slot it is bound to; a group that held nothing this frame, a scene that binds
//!   nothing and a refused start all press nothing. There is no default macro and no fallback,
//!   and nothing in this module ever prefers one bound macro to another: the scene says which
//!   buttons exist and the readout says which one is pressed.
//! - **at most one decision per `holdMs`.** The exclusive group's own hold, taken from the
//!   decoder preset the adapter chose, so macros mode commits for as long as raw mode does.
//!   A slot that is unbound in this scene spends nothing, because nothing happened.
//! - **the title screen is raw.** `Scene::Title` has no palette (section 2), so the readout's own
//!   button mask reaches the cartridge there and the boot variant can still fire Start.
//! - **a scene change aborts.** Enforced inside the executor, at the next `step`, which is why
//!   [`MacroLayer::decide`] does not re-check it.
//!
//! What is *not* here: a checkpoint. A running macro is transient state and is deliberately not
//! persisted (`docs/design/flysim.md` section 8) — a restore starts with no macro running and the
//! fly is consulted again on the restored frame's palette.

use flybrain_gb::adapter::MemoryReader;
use flybrain_gb::{RunLedger, MacroPalette, SceneId, SlotBinding, Started};

use crate::config::Config;
use crate::snapshot::{
    FeedMacro, FeedMacroOutcome, FeedPaletteSlot, FeedScene, MacroOutcome, finite,
};

/// The macro layer for a configuration, or `None` in raw mode.
///
/// The one place the mode is turned into a presence or an absence. Raw mode returns `None`, so
/// every macro call site in the sim loop is inside an `if let Some(layer)` and raw mode cannot
/// execute a line of this module; `Config::validate` has already refused macros mode for a game
/// with no palette, so a `None` here in macros mode would be a bug and says so.
pub fn macro_layer(config: &Config, hold_ms: f64, seed: u32) -> Option<MacroLayer> {
    let flavour = config.macros.mode.palette_mode()?;
    let palette = flybrain_gb::palette_for(&config.loop_.game, seed, flavour)?;
    Some(MacroLayer::new(palette, hold_ms))
}

/// What one frame of macros mode decided.
#[derive(Debug, Default)]
pub struct Decision {
    /// The mask to hand the emulator. 0 means nothing is pressed, which is what an unbound slot,
    /// a refused start and a silent readout all produce.
    pub mask: u32,
    /// `macro` feed events this frame produced, in order.
    pub events: Vec<MacroEvent>,
    /// Why nothing was pressed, on a frame where nothing was. `None` exactly when `mask != 0`.
    ///
    /// Not published and not logged: it exists so that a measurement can say *which* of the
    /// doctrine's several ways of doing nothing a run spent its silent frames in
    /// (`examples/palette_bench.rs`). The sim loop ignores it.
    pub silence: Option<Silence>,
}

/// Why macros mode pressed nothing on a frame.
///
/// `docs/design/macros.md` section 1 has three deliberate ways for a frame to press nothing — an
/// unbound slot, a refused macro, a silent readout — and section 4 adds two more that are the
/// machinery working: the decision cooldown inside `holdMs`, and a running macro whose script is
/// between presses. A silent frame is not a fault, but 63% of them being one cause rather than
/// another is the difference between a palette that is too small and a palette that is never
/// reached, so they are counted apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Silence {
    /// A macro owned the buttons and its script held nothing this frame: a settle, the gap
    /// between two pulses, a wait for a menu cursor, or the frame it finished on.
    Macro,
    /// The readout asked for nothing: the macro group held no channel this scene binds.
    Channels,
    /// A channel was active but its slot is unbound in this scene: "no action". Or a scene that
    /// binds nothing at all, which is the same nothing arrived at from the other side.
    Unbound,
    /// A channel was active and its slot is bound, but the last decision is less than one
    /// `holdMs` old.
    Cooldown,
    /// A bound macro refused to start: its precondition had lapsed, or it found no route.
    Refused,
}

impl Silence {
    /// Every cause, for a histogram that names them all whether or not a run hit them.
    pub const ALL: [Self; 5] =
        [Self::Macro, Self::Channels, Self::Unbound, Self::Cooldown, Self::Refused];

    /// Short name for a table.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Macro => "macro pressing nothing",
            Self::Channels => "readout silent",
            Self::Unbound => "no bound slot",
            Self::Cooldown => "decision cooldown",
            Self::Refused => "macro refused",
        }
    }
}

/// One `macro` feed event (`docs/design/macros.md` section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroEvent {
    pub slot: u8,
    pub name: &'static str,
    /// `None` for a start; the outcome for a finish.
    pub outcome: Option<MacroOutcome>,
}

impl MacroEvent {
    /// `<NAME> start` or `<NAME> done|blocked|timeout|refused`.
    pub fn label(&self) -> String {
        match self.outcome {
            None => format!("{} start", self.name),
            Some(outcome) => format!("{} {}", self.name, outcome.as_str()),
        }
    }
}

/// What one frame's channels ask for, in the mode's own terms.
///
/// Palette mode reads a slot off the active channel set. Plan mode blends
/// (`docs/design/macros.md` section 10), so it has two ways of asking for nothing that a set of
/// active channels cannot express: a readout under the activity floor, and a scene with no bound
/// slot to blend over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Start this slot's macro, if the hold allows.
    Slot(u8),
    /// The readout is under the activity floor, or silent: the game waits.
    Asleep,
    /// This scene binds nothing, so there is nothing the channels could mean.
    Unbound,
}

/// The macro that owns the buttons.
#[derive(Debug, Clone, Copy)]
struct Running {
    slot: u8,
    name: &'static str,
    started_ms: f64,
}

/// The macro that finished most recently.
#[derive(Debug, Clone, Copy)]
struct Finished {
    slot: u8,
    name: &'static str,
    outcome: MacroOutcome,
    at_ms: f64,
}

/// Palette mode's own state: the scene, the palette, and which macro is running.
pub struct MacroLayer {
    palette: Box<dyn MacroPalette>,
    /// The macro group's hold, from the decoder preset the adapter picked. One decision per hold,
    /// so a macro is chosen exactly as often as raw mode changes direction.
    hold_ms: f64,
    /// Brain clock of the last decision taken, or `None` before the first one.
    last_decision_ms: Option<f64>,
    scene: SceneId,
    bindings: Vec<SlotBinding>,
    running: Option<Running>,
    finished: Option<Finished>,
    /// The brain millisecond the pad became empty in a playable scene, or `None` while it has
    /// something on it.
    ///
    /// The operator, on stream 2026-09-17: "sometimes the macro buttons disappear and everything just
    /// hangs there." An empty pad is the one state the doctrine cannot recover from on its own --
    /// nothing presses for the fly, so a scene with no buttons waits, and waiting is *correct*
    /// behaviour that looks exactly like a hang. So it is measured and published
    /// (`game.padEmptyMs`) rather than acted on: the watchdog exports it and the fix, where there
    /// is one, goes inside the macros (section 13.1's audit).
    ///
    /// A running macro counts as *not* empty: it owns the pad, which is the honest reading of what
    /// the audience is looking at.
    pad_empty_since_ms: Option<f64>,
    /// Lifetime outcome counts, for the measurement harness and the log line.
    counts: OutcomeCounts,
}

/// Macro outcomes over a run, which is what `docs/design/macros.md` section 7 asks a measurement
/// to report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    pub started: u64,
    pub done: u64,
    pub blocked: u64,
    pub timeout: u64,
    pub refused: u64,
}

impl OutcomeCounts {
    fn record(&mut self, outcome: MacroOutcome) {
        let counter = match outcome {
            MacroOutcome::Done => &mut self.done,
            MacroOutcome::Blocked => &mut self.blocked,
            MacroOutcome::Timeout => &mut self.timeout,
            MacroOutcome::Refused => &mut self.refused,
        };
        *counter += 1;
    }

    /// Outcomes that ended a macro that really ran, i.e. everything but a refusal.
    pub fn finished(&self) -> u64 {
        self.done + self.blocked + self.timeout
    }
}

impl MacroLayer {
    pub fn new(palette: Box<dyn MacroPalette>, hold_ms: f64) -> Self {
        Self {
            palette,
            hold_ms: if hold_ms.is_finite() && hold_ms > 0.0 { hold_ms } else { 0.0 },
            last_decision_ms: None,
            // Nothing has been observed yet. `Unknown` rather than `Title` because the first
            // `observe` has not happened and guessing the intro would be a guess.
            scene: SceneId::Unknown,
            bindings: Vec::new(),
            running: None,
            finished: None,
            pad_empty_since_ms: None,
            counts: OutcomeCounts::default(),
        }
    }

    /// Loop step 4: the mask this frame, given the channels the decoder just produced.
    ///
    /// `raw_mask` is what raw mode would have pressed, and it is used for exactly one thing: the
    /// title screen, where section 2 says the readout's boot variant applies and no palette is
    /// dealt.
    ///
    /// `active` is the decode's own channel list, which in macros mode carries the winner of the
    /// macro group (`docs/design/macros.md` section 12) alongside whatever buttons are held. The
    /// layer reads only the macro half of it; the buttons are still decoded, still published and
    /// still drive the on-screen afterglow, they simply do not reach the cartridge while this
    /// layer is in the loop.
    pub fn decide(
        &mut self,
        active: &[String],
        raw_mask: u32,
        ms: f64,
        memory: &mut dyn MemoryReader,
        ledger: &dyn RunLedger,
    ) -> Decision {
        // The brain clock, before anything reads it: the palette's blocked-target ledger is a
        // window in brain minutes (`docs/design/macros.md` section 12).
        self.palette.clock(ms);
        // Section 2: the title screen has no palette, so the fly's raw buttons reach the
        // cartridge there. This is the only path in macros mode that presses a decoded mask, and
        // it is what keeps the intro identical to raw mode's.
        //
        // Nothing of the fly's reaches the cartridge here but its raw buttons: a macro that has
        // run into this scene was abandoned by `observe`, which is what keeps a header from
        // saying `scene: title` beside a running macro.
        if !self.scene.playable() {
            return Decision {
                mask: raw_mask,
                events: Vec::new(),
                silence: (raw_mask == 0).then_some(Silence::Channels),
            };
        }

        // Rule one: a running macro owns the buttons, whatever the channels are doing. Its own
        // `step` ends it on a scene change or at the frame cap.
        if let Some(running) = self.running {
            if let Some(mask) = self.palette.step(memory, ledger) {
                return Decision {
                    mask: u32::from(mask),
                    events: Vec::new(),
                    silence: (mask == 0).then_some(Silence::Macro),
                };
            }
            let mut decision = Decision { silence: Some(Silence::Macro), ..Decision::default() };
            self.finish(running, ms, &mut decision);
            return decision;
        }

        // The drive (section 12): the macro group's winner, if it is one of this scene's
        // buttons.
        let slot = match self.asked(active) {
            Ask::Slot(slot) => slot,
            Ask::Asleep => {
                return Decision { silence: Some(Silence::Channels), ..Decision::default() };
            }
            Ask::Unbound => {
                return Decision { silence: Some(Silence::Unbound), ..Decision::default() };
            }
        };
        if self.last_decision_ms.is_some_and(|last| ms - last < self.hold_ms) {
            // Inside the hold, but only a slot that *would* have done something is waiting for
            // one: an unbound channel is "no action" whatever the clock says, and counting it as
            // a cooldown would hide a scene with an empty palette behind the cooldown's number.
            let silence = if self.bound(slot) { Silence::Cooldown } else { Silence::Unbound };
            return Decision { silence: Some(silence), ..Decision::default() };
        }

        let mut decision = Decision::default();
        match self.palette.start(slot, memory, ledger) {
            Started::Running(name) => {
                self.last_decision_ms = Some(ms);
                self.running = Some(Running { slot, name, started_ms: ms });
                self.finished = None;
                self.counts.started += 1;
                decision.events.push(MacroEvent { slot, name, outcome: None });
                // The macro owns the buttons from `start`, so this frame is already its first.
                let running = self.running.expect("just set");
                match self.palette.step(memory, ledger) {
                    Some(mask) => decision.mask = u32::from(mask),
                    None => self.finish(running, ms, &mut decision),
                }
                if decision.mask == 0 {
                    decision.silence = Some(Silence::Macro);
                }
            }
            // A bound macro that refused is an outcome the feed reports, and it spends the
            // decision: the precondition that failed will not have changed inside one hold, and
            // one refusal per hold is a log line rather than a flood.
            Started::Refused { name: Some(name), reason } => {
                self.last_decision_ms = Some(ms);
                tracing::debug!(slot, name, reason, "macro refused");
                let _ = self.palette.take_finished();
                self.record_finish(slot, name, MacroOutcome::Refused, ms, &mut decision);
                decision.silence = Some(Silence::Refused);
            }
            // An unbound slot spends nothing, because nothing happened. The fly pointed a
            // channel at a scene that has no action there, and a later pulse inside the same
            // hold — A, while a direction whose slot is unbound is still held down — must still
            // be able to start its own macro rather than be swallowed by a non-event.
            Started::Refused { name: None, reason } => {
                tracing::trace!(slot, reason, "no macro on that channel");
                let _ = self.palette.take_finished();
                decision.silence = Some(Silence::Unbound);
            }
        }
        decision
    }

    /// Loop step 7 and a half: detect the scene and deal its palette, once per frame, after the
    /// frame (`docs/design/macros.md` section 2).
    ///
    /// Returns the `macro` events the observation itself produced, which is at most one: a macro
    /// that has run into a scene with no palette is abandoned here rather than one frame later,
    /// so that the header this observation feeds cannot say `scene: title` beside a running macro
    /// (`tests/integration.rs`). `ms` is the brain clock the caller is about to publish.
    pub fn observe(
        &mut self,
        memory: &mut dyn MemoryReader,
        ledger: &dyn RunLedger,
        ms: f64,
    ) -> Vec<MacroEvent> {
        self.palette.clock(ms);
        let observed = self.palette.observe(memory, ledger);
        self.scene = observed.scene;
        self.bindings = observed.bindings;
        self.watch_pad(ms);
        let mut decision = Decision::default();
        if !self.scene.playable()
            && let Some(running) = self.running.take()
        {
            self.palette.cancel();
            let _ = self.palette.take_finished();
            self.record_finish(running.slot, running.name, MacroOutcome::Blocked, ms, &mut decision);
        }
        // Nothing else to carry across a scene change: section 12 left no cursor, no last step
        // and no order to re-derive. The bindings are the scene's buttons and the decoder is
        // handed them again on the very next frame, which is what made the cartridge's 615 A
        // presses at one shelf impossible to write.
        decision.events
    }

    /// Give up on whatever is running, because the loop is rolling the game back.
    ///
    /// A rollback restores an emulator state the running macro's plan was never made for — a
    /// route over a map the fly is no longer on — so the macro is abandoned rather than carried
    /// across, and the fly is consulted again on the restored frame's palette. The decision
    /// budget is cleared too: the fly should not have to wait out a hold it did not spend.
    pub fn cancel(&mut self, ms: f64) -> Vec<MacroEvent> {
        self.palette.cancel();
        let _ = self.palette.take_finished();
        self.last_decision_ms = None;
        let mut decision = Decision::default();
        if let Some(running) = self.running.take() {
            self.record_finish(running.slot, running.name, MacroOutcome::Blocked, ms, &mut decision);
        }
        decision.events
    }

    /// Start, continue or clear the empty-pad clock for this frame.
    ///
    /// Only a *playable* scene counts: the title screen deals no palette by contract (section 2),
    /// and raw mode has no palette at all, so neither is an empty pad in the sense that matters.
    fn watch_pad(&mut self, ms: f64) {
        let empty = self.scene.playable() && self.bindings.is_empty() && self.running.is_none();
        match (empty, self.pad_empty_since_ms) {
            (false, _) => self.pad_empty_since_ms = None,
            (true, None) => self.pad_empty_since_ms = Some(ms),
            (true, Some(_)) => {}
        }
    }

    /// How long the pad has been empty in a playable scene, in brain milliseconds; 0 otherwise.
    ///
    /// The feed's `game.padEmptyMs`, and report-only: nothing in the loop reads it back and
    /// nothing presses a button because of it (`docs/loop-review.md`: "nothing presses for the
    /// fly"). A clock that has gone backwards -- which only a rollback does, and a rollback
    /// cancels the running macro -- reads 0 rather than a negative age.
    pub fn pad_empty_ms(&self, ms: f64) -> f64 {
        match self.pad_empty_since_ms {
            Some(since) if ms > since => ms - since,
            _ => 0.0,
        }
    }

    /// The scene the last [`MacroLayer::observe`] found.
    pub fn scene(&self) -> FeedScene {
        FeedScene::from_adapter(self.scene)
    }

    /// The same scene as the name the feed publishes, for a histogram key or a log line.
    pub fn scene_name(&self) -> &'static str {
        self.scene.feed_name()
    }

    /// The header's `game.palette`: bound slots only, ascending by slot.
    ///
    /// Sorted here rather than trusted from the game. `docs/feed-protocol.md` promises ascending
    /// order, the Pokémon palette happens to bind in that order anyway, and a published contract
    /// that holds only because of how one implementation iterates is not a contract.
    pub fn feed_palette(&self) -> Vec<FeedPaletteSlot> {
        let mut published: Vec<FeedPaletteSlot> = self
            .bindings
            .iter()
            .map(|binding| FeedPaletteSlot {
                slot: binding.slot,
                name: binding.name.to_string(),
                gloss: binding.gloss.to_string(),
                channel: binding.tag.to_string(),
            })
            .collect();
        published.sort_by_key(|slot| slot.slot);
        published
    }

    /// The header's `game.macro`.
    pub fn feed_macro(&self, ms: f64) -> Option<FeedMacro> {
        self.running.map(|running| FeedMacro {
            slot: running.slot,
            name: running.name.to_string(),
            since_ms: finite(ms - running.started_ms).max(0.0),
        })
    }

    /// The header's `game.macroOutcome`.
    pub fn feed_outcome(&self) -> Option<FeedMacroOutcome> {
        self.finished.map(|finished| FeedMacroOutcome {
            slot: finished.slot,
            name: finished.name.to_string(),
            outcome: finished.outcome,
            at_ms: finite(finished.at_ms).max(0.0),
        })
    }

    pub fn counts(&self) -> OutcomeCounts {
        self.counts
    }

    /// The name of the running macro, for a log line.
    pub fn running(&self) -> Option<&'static str> {
        self.running.map(|running| running.name)
    }

    /// The macro channels this scene binds, for the decoder's per-decision mask
    /// (`docs/design/macros.md` section 12: "unbound channels are masked from the decision").
    ///
    /// The sim loop passes this to [`flybrain_core::decoder::PopulationDecoder::decode_bound`] on
    /// the same frame, which is why it is the layer's to answer: the bindings are what the last
    /// `observe` dealt, and nothing else in the loop knows them.
    /// Whether this frame saw the fly nearer its objective than the run has managed before.
    ///
    /// Straight through from the palette ([`MacroPalette::nearer_the_objective`]), for the
    /// ratchet's stall window and for nothing else: the macro layer neither reads it back nor
    /// presses anything because of it.
    pub fn nearer_the_objective(&self) -> bool {
        self.palette.nearer_the_objective()
    }

    pub fn bound_channels(&self) -> Vec<String> {
        self.bindings
            .iter()
            .map(|binding| binding.channel.to_string())
            .collect()
    }

    /// Whether `slot` is bound in the palette the last `observe` dealt.
    fn bound(&self, slot: u8) -> bool {
        self.bindings.iter().any(|binding| binding.slot == slot)
    }

    /// What the readout is asking for this frame.
    ///
    /// The macro group holds at most one channel at a time and the sim loop has already told the
    /// decoder which channels may win, so this is a lookup rather than a decision: the bound
    /// binding whose channel is active. It is deliberately not a search for the *best* active
    /// channel — there is no better and worse here, and a layer that ranked them would be
    /// section 10 growing back.
    fn asked(&self, active: &[String]) -> Ask {
        if self.bindings.is_empty() {
            return Ask::Unbound;
        }
        match self
            .bindings
            .iter()
            .find(|binding| active.iter().any(|name| name == binding.channel))
        {
            Some(binding) => Ask::Slot(binding.slot),
            None => Ask::Asleep,
        }
    }

    /// A macro whose `step` returned `None`: take its outcome and report it.
    fn finish(&mut self, running: Running, ms: f64, decision: &mut Decision) {
        self.running = None;
        let taken = self.palette.take_finished();
        debug_assert!(
            taken.is_some(),
            "a macro that stopped stepping must have recorded how it ended"
        );
        // `step` returning `None` and the palette recording no outcome cannot both happen, but
        // `done` is the honest reading if they ever do: the presses happened.
        let outcome = taken.map_or(MacroOutcome::Done, |(_, outcome)| {
            MacroOutcome::from_adapter(outcome)
        });
        self.record_finish(running.slot, running.name, outcome, ms, decision);
    }

    fn record_finish(
        &mut self,
        slot: u8,
        name: &'static str,
        outcome: MacroOutcome,
        ms: f64,
        decision: &mut Decision,
    ) {
        self.counts.record(outcome);
        self.finished = Some(Finished { slot, name, outcome, at_ms: ms });
        decision.events.push(MacroEvent { slot, name, outcome: Some(outcome) });
    }
}

impl std::fmt::Debug for MacroLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacroLayer")
            .field("scene", &self.scene)
            .field("bound", &self.bindings.len())
            .field("running", &self.running.map(|running| running.name))
            .field("hold_ms", &self.hold_ms)
            .field("counts", &self.counts)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use flybrain_gb::{NoLedger, Observed, Outcome};

    use super::*;

    /// A memory that reads as all zeroes: enough for a layer test, which never looks at it.
    struct Zeroes;
    impl MemoryReader for Zeroes {
        fn read8(&mut self, _address: u16) -> u8 {
            0
        }
    }

    /// A scripted palette: the layer under test is the decision rule, not a cartridge.
    struct Fake {
        scene: SceneId,
        /// Scenes the next `observe`s report instead, oldest first: the fake's own scene walk,
        /// for the re-plan rule.
        then: Vec<SceneId>,
        /// Slot sets the next `observe`s bind instead, oldest first: a plan that gets shorter
        /// without the scene changing.
        rebind: Vec<Vec<u8>>,
        /// Names for the bound slots, in slot order, so a test can give a plan a *head*.
        names: Vec<&'static str>,
        /// Name sets the next `observe`s use instead, oldest first: a plan whose head changes.
        rename: Vec<Vec<&'static str>>,
        bound: Vec<u8>,
        /// Masks `step` returns, in order; a `None` ends the macro.
        plan: Vec<Option<u8>>,
        running: Option<&'static str>,
        finished: Option<(&'static str, Outcome)>,
        starts: Vec<u8>,
        cancels: u32,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                scene: SceneId::Overworld,
                then: Vec::new(),
                rebind: Vec::new(),
                names: Vec::new(),
                rename: Vec::new(),
                bound: Vec::new(),
                plan: Vec::new(),
                running: None,
                finished: None,
                starts: Vec::new(),
                cancels: 0,
            }
        }
    }

    impl MacroPalette for Fake {
        fn observe(&mut self, _memory: &mut dyn MemoryReader, _exits: &dyn RunLedger)
        -> Observed {
            if !self.then.is_empty() {
                self.scene = self.then.remove(0);
            }
            if !self.rebind.is_empty() {
                self.bound = self.rebind.remove(0);
            }
            if !self.rename.is_empty() {
                self.names = self.rename.remove(0);
            }
            Observed {
                scene: self.scene,
                bindings: self
                    .bound
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| SlotBinding {
                        slot: *slot,
                        name: self.names.get(index).copied().unwrap_or("GO EXIT"),
                        gloss: "nearest door",
                        channel: channel_of(*slot),
                        tag: tag_of(*slot),
                    })
                    .collect(),
            }
        }

        fn start(
            &mut self,
            slot: u8,
            _memory: &mut dyn MemoryReader,
            _exits: &dyn RunLedger,
        ) -> Started {
            self.starts.push(slot);
            if !self.bound.contains(&slot) {
                return Started::Refused { name: None, reason: "unbound" };
            }
            if self.plan.is_empty() {
                self.finished = Some(("GO EXIT", Outcome::Refused));
                return Started::Refused { name: Some("GO EXIT"), reason: "no route" };
            }
            let name = self
                .names
                .get(self.bound.iter().position(|bound| *bound == slot).unwrap_or(0))
                .copied()
                .unwrap_or("GO EXIT");
            self.running = Some(name);
            Started::Running(name)
        }

        fn step(&mut self, _memory: &mut dyn MemoryReader, _exits: &dyn RunLedger)
        -> Option<u8> {
            match self.plan.remove(0) {
                Some(mask) => Some(mask),
                None => {
                    let name = self.running.take().unwrap_or("GO EXIT");
                    self.finished = Some((name, Outcome::Done));
                    None
                }
            }
        }

        fn running(&self) -> Option<&'static str> {
            self.running
        }

        fn take_finished(&mut self) -> Option<(&'static str, Outcome)> {
            self.finished.take()
        }

        fn cancel(&mut self) {
            self.cancels += 1;
            self.running = None;
        }
    }

    fn layer(fake: Fake) -> MacroLayer {
        let mut layer = MacroLayer::new(Box::new(fake), 800.0);
        layer.observe(&mut Zeroes, &NoLedger, 0.0);
        layer
    }

    fn channels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    /// Real macro channels, one per slot the fake can bind.
    ///
    /// Real ones rather than invented strings, because the names are a contract the dataset, the
    /// decoder and the page all spell the same way (`MacroKind::channel`).
    const CHANNELS: [&str; 6] = [
        "macro_go_objective",
        "macro_go_out",
        "macro_go_warp",
        "macro_go_route",
        "macro_go_item",
        "macro_go_npc",
    ];
    const TAGS: [&str; 6] =
        ["MB·GOAL", "MB·OUT", "MB·WARP", "MB·ROUTE", "MB·ITEM", "MB·NPC"];

    fn channel_of(slot: u8) -> &'static str {
        CHANNELS[usize::from(slot)]
    }

    fn tag_of(slot: u8) -> &'static str {
        TAGS[usize::from(slot)]
    }

    /// The decode's channel list when the macro group is holding `slot`'s channel.
    fn holding(slot: u8) -> Vec<String> {
        channels(&[channel_of(slot)])
    }

    /// One `decide` on the layer, with the memory and ledger every test here uses.
    fn decide(layer: &mut MacroLayer, active: &[String], raw_mask: u32, ms: f64) -> Decision {
        layer.decide(active, raw_mask, ms, &mut Zeroes, &NoLedger)
    }

    /// Section 13.1's `game.padEmptyMs`: how long a playable scene has had nothing on the pad.
    ///
    /// The operator, on stream 2026-09-17: "sometimes the macro buttons disappear and everything just
    /// hangs there." Report only -- nothing in the loop reads it back and nothing presses a button
    /// because of it -- so what is under test is the reading, and the four states it distinguishes.
    #[test]
    fn the_empty_pad_clock_runs_only_while_a_playable_scene_binds_nothing() {
        // A scene with buttons: zero, whatever the clock says.
        let bound = layer(Fake { bound: vec![0], ..Fake::default() });
        assert_eq!(bound.pad_empty_ms(5_000.0), 0.0);

        // A playable scene with nothing bound: the clock starts at the frame it was observed on
        // and runs.
        let mut empty = MacroLayer::new(Box::new(Fake::default()), 800.0);
        empty.observe(&mut Zeroes, &NoLedger, 1_000.0);
        assert_eq!(empty.pad_empty_ms(1_000.0), 0.0, "no time has passed yet");
        assert_eq!(empty.pad_empty_ms(4_000.0), 3_000.0);
        // It keeps running across further empty frames rather than restarting on each one.
        empty.observe(&mut Zeroes, &NoLedger, 2_000.0);
        assert_eq!(empty.pad_empty_ms(4_000.0), 3_000.0);
        // It is the *first* empty frame that starts it, so a scene that keeps binding nothing
        // keeps one clock rather than restarting it every frame.
        empty.observe(&mut Zeroes, &NoLedger, 5_000.0);
        assert_eq!(empty.pad_empty_ms(9_000.0), 8_000.0);
        // And a scene with something on the pad reads zero.
        let mut refilled = MacroLayer::new(
            Box::new(Fake { bound: vec![1], ..Fake::default() }),
            800.0,
        );
        refilled.observe(&mut Zeroes, &NoLedger, 1_000.0);
        assert_eq!(refilled.pad_empty_ms(9_000.0), 0.0);

        // The title screen binds nothing by contract (section 2), so it is not an empty pad.
        let mut title = MacroLayer::new(
            Box::new(Fake { scene: SceneId::Title, ..Fake::default() }),
            800.0,
        );
        title.observe(&mut Zeroes, &NoLedger, 1_000.0);
        assert_eq!(title.pad_empty_ms(9_000.0), 0.0);

        // A clock that has gone backwards reads 0 rather than a negative age.
        assert_eq!(empty.pad_empty_ms(0.0), 0.0);
    }

    /// A macro that owns the pad is not an empty pad, even where the scene binds nothing else.
    #[test]
    fn a_running_macro_is_not_an_empty_pad() {
        let mut layer = layer(Fake {
            bound: vec![0],
            plan: vec![Some(0b0001), Some(0b0001), None],
            // The first `observe` is the one `layer()` makes and keeps the pad; the second, below,
            // empties it under the running macro.
            rebind: vec![vec![0], Vec::new()],
            ..Fake::default()
        });
        decide(&mut layer, &holding(0), 0, 0.0);
        assert!(layer.running().is_some(), "a macro owns the pad");
        // A scene that binds nothing under a running macro is not an empty pad: the macro owns it,
        // which is the honest reading of what the audience is looking at.
        layer.observe(&mut Zeroes, &NoLedger, 16.0);
        assert_eq!(layer.pad_empty_ms(5_000.0), 0.0);
        // When it gives the buttons back and the scene still binds nothing, the clock starts.
        decide(&mut layer, &[], 0, 32.0);
        decide(&mut layer, &[], 0, 48.0);
        layer.observe(&mut Zeroes, &NoLedger, 64.0);
        assert!(layer.pad_empty_ms(5_000.0) > 0.0);
    }

    #[test]
    fn a_silent_readout_presses_nothing() {
        let mut layer = layer(Fake { bound: vec![0], ..Fake::default() });
        let decision = decide(&mut layer, &[], 0, 0.0);
        assert_eq!(decision.mask, 0);
        assert!(decision.events.is_empty());
        assert_eq!(layer.feed_macro(0.0), None);
    }

    #[test]
    fn a_channel_this_scene_does_not_bind_presses_nothing_and_says_nothing() {
        // The decoder is told which channels may win, so this is the belt-and-braces half: a
        // macro channel that is active anyway -- held over from the scene before, say -- is not
        // a button on this pad and means nothing here.
        let mut layer = layer(Fake { bound: vec![3], ..Fake::default() });
        let decision = decide(&mut layer, &holding(0), 0b0000_0001, 0.0);
        assert_eq!(decision.mask, 0, "an unbound channel presses nothing, not the raw mask");
        assert!(decision.events.is_empty(), "{:?}", decision.events);
        assert_eq!(layer.feed_outcome(), None);
    }

    #[test]
    fn the_bound_channels_are_what_the_decoder_is_allowed_to_choose_among() {
        // The mask the sim loop hands `decode_bound`: this scene's buttons and nothing else
        // (`docs/design/macros.md` section 12).
        let bound = layer(Fake { bound: vec![4, 0, 2], ..Fake::default() });
        assert_eq!(
            bound.bound_channels(),
            channels(&["macro_go_item", "macro_go_objective", "macro_go_warp"]),
            "in the palette's own order; the feed is what sorts"
        );
        let empty = super::tests::layer(Fake { bound: Vec::new(), ..Fake::default() });
        assert!(empty.bound_channels().is_empty(), "a scene with no buttons masks the whole group");
    }

    #[test]
    fn a_refusal_spends_the_hold_but_an_unbound_channel_does_not() {
        // Bound and refusing: one refusal per hold, not one per frame.
        let mut layer = layer(Fake { bound: vec![0], ..Fake::default() });
        assert_eq!(decide(&mut layer, &holding(0), 1, 0.0).events.len(), 1);
        assert!(decide(&mut layer, &holding(0), 1, 16.0).events.is_empty());
        assert_eq!(decide(&mut layer, &holding(0), 1, 1_000.0).events.len(), 1);
        assert_eq!(layer.counts().refused, 2);

        // Not on this pad: the channel means nothing here, so it costs nothing either, and a
        // bound one on the very next frame still starts.
        let mut layer = super::tests::layer(Fake {
            bound: vec![1],
            plan: vec![Some(0b0000_0010), None],
            ..Fake::default()
        });
        assert!(decide(&mut layer, &holding(0), 1, 0.0).events.is_empty());
        let next = decide(&mut layer, &holding(1), 0b10, 16.0);
        assert_eq!(next.events.len(), 1, "the channel this scene does not bind spent nothing");
        assert_eq!(next.events[0].slot, 1);
    }

    #[test]
    fn a_held_macro_channel_starts_its_macro_once_per_hold() {
        let mut layer = layer(Fake {
            bound: vec![0],
            plan: vec![Some(1), Some(1), None],
            ..Fake::default()
        });
        let up = holding(0);

        // Frame 1: the decision, the start event, and the macro's own first mask.
        let first = decide(&mut layer, &up, 0b0000_0001, 1_000.0);
        assert_eq!(first.mask, 1);
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].label(), "GO EXIT start");
        assert_eq!(layer.feed_macro(1_000.0).unwrap().since_ms, 0.0);

        // Frame 2: the running macro owns the buttons, no new decision.
        let second = decide(&mut layer, &up, 0b0000_0001, 1_016.0);
        assert_eq!(second.mask, 1);
        assert!(second.events.is_empty());
        assert_eq!(layer.feed_macro(1_016.0).unwrap().since_ms, 16.0);

        // Frame 3: it finishes. Nothing is pressed on the frame it ends.
        let third = decide(&mut layer, &up, 0b0000_0001, 1_032.0);
        assert_eq!(third.mask, 0);
        assert_eq!(third.events.len(), 1);
        assert_eq!(third.events[0].label(), "GO EXIT done");
        assert_eq!(layer.feed_macro(1_032.0), None);
        let outcome = layer.feed_outcome().expect("the finish is kept");
        assert_eq!(outcome.outcome, MacroOutcome::Done);
        assert_eq!(outcome.at_ms, 1_032.0);

        // Frame 4: inside the hold, so the same channel starts nothing.
        let fourth = decide(&mut layer, &up, 0b0000_0001, 1_048.0);
        assert_eq!(fourth.mask, 0);
        assert!(fourth.events.is_empty());
        assert_eq!(layer.counts().started, 1);
        assert_eq!(layer.counts().done, 1);
    }

    #[test]
    fn the_title_screen_presses_the_raw_mask_and_runs_no_macro() {
        let mut layer = layer(Fake { scene: SceneId::Title, ..Fake::default() });
        let decision = decide(&mut layer, &channels(&["start"]), 0b0100_0000, 0.0);
        assert_eq!(decision.mask, 0b0100_0000, "the boot variant still fires Start");
        assert!(decision.events.is_empty());
        assert_eq!(layer.scene(), FeedScene::Title);
        assert!(layer.feed_palette().is_empty());

        // A macro decided on the previous frame's scene does not survive into the title screen:
        // the header would otherwise say `scene: title` beside a running macro.
        let mut layer = super::tests::layer(Fake {
            bound: vec![0],
            then: vec![SceneId::Overworld, SceneId::Title],
            plan: vec![Some(1); 8],
            ..Fake::default()
        });
        assert!(decide(&mut layer, &holding(0), 1, 0.0).events[0].outcome.is_none());
        assert!(layer.running().is_some());
        // The observation that finds the title is where it ends, so the header that observation
        // feeds already says no macro is running.
        let events = layer.observe(&mut Zeroes, &NoLedger, 16.0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].label(), "GO EXIT blocked");
        assert_eq!(layer.running(), None);
        assert_eq!(layer.feed_macro(16.0), None);
        let decision = decide(&mut layer, &holding(0), 0b0100_0000, 16.0);
        assert_eq!(decision.mask, 0b0100_0000, "the raw mask, not the macro's");
        assert!(decision.events.is_empty());
    }

    #[test]
    fn a_refused_macro_is_an_outcome_and_presses_nothing() {
        // Bound but with an empty plan, which the fake refuses as "no route".
        let mut layer = layer(Fake { bound: vec![0], ..Fake::default() });
        let decision = decide(&mut layer, &holding(0), 0b0000_0001, 0.0);
        assert_eq!(decision.mask, 0);
        assert_eq!(decision.events.len(), 1);
        assert_eq!(decision.events[0].label(), "GO EXIT refused");
        assert_eq!(layer.counts().refused, 1);
        assert_eq!(layer.counts().started, 0);
    }

    #[test]
    fn a_rollback_abandons_the_running_macro_and_clears_the_hold() {
        let mut layer = layer(Fake {
            bound: vec![0],
            plan: vec![Some(1); 8],
            ..Fake::default()
        });
        decide(&mut layer, &holding(0), 1, 0.0);
        assert!(layer.running().is_some());

        let events = layer.cancel(500.0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].label(), "GO EXIT blocked");
        assert_eq!(layer.running(), None);
        assert_eq!(layer.feed_macro(500.0), None);
        // The next frame may decide again rather than waiting out a hold it did not spend.
        let decision = decide(&mut layer, &holding(0), 1, 516.0);
        assert_eq!(decision.events.len(), 1);
        assert_eq!(decision.events[0].label(), "GO EXIT start");
    }

    #[test]
    fn the_palette_the_feed_publishes_is_the_bound_slots_ascending() {
        // Whatever order the game hands its bindings over in, the feed's is ascending by slot,
        // because that is what `docs/feed-protocol.md` promises a page.
        let layer = layer(Fake { bound: vec![4, 0, 2], ..Fake::default() });
        let published = layer.feed_palette();
        assert_eq!(
            published.iter().map(|slot| slot.slot).collect::<Vec<_>>(),
            [0, 2, 4]
        );
        assert!(published.iter().all(|slot| !slot.gloss.is_empty()));
    }

    #[test]
    fn every_silent_frame_is_attributed_to_one_cause() {
        // The doctrine has several ways for a frame to press nothing, and a measurement that
        // cannot tell them apart cannot say whether the palette is too small or never reached.
        let mut layer = layer(Fake { bound: vec![0], ..Fake::default() });
        // Nothing active at all.
        assert_eq!(
            decide(&mut layer, &[], 0, 0.0).silence,
            Some(Silence::Channels)
        );
        // A channel this scene does not bind, which is the same nothing as a silent group: the
        // fly is not asking for anything that is on this pad.
        assert_eq!(
            decide(&mut layer, &holding(1), 0b10, 16.0).silence,
            Some(Silence::Channels)
        );
        // A bound macro that refused, which also spends the hold...
        assert_eq!(
            decide(&mut layer, &holding(0), 1, 32.0).silence,
            Some(Silence::Refused)
        );
        // ...so the next frame on the same channel is the cooldown.
        assert_eq!(
            decide(&mut layer, &holding(0), 1, 48.0).silence,
            Some(Silence::Cooldown)
        );
        // A scene that binds nothing at all is the other silence, and it is not the fly's: the
        // bench has to be able to tell "no button on this pad" from "the fly pressed none".
        let mut empty = super::tests::layer(Fake { bound: Vec::new(), ..Fake::default() });
        assert_eq!(decide(&mut empty, &holding(0), 0, 0.0).silence, Some(Silence::Unbound));

        // A running macro whose script holds nothing this frame, and the frame it ends on.
        let mut layer = super::tests::layer(Fake {
            bound: vec![0],
            plan: vec![Some(1), Some(0), None],
            ..Fake::default()
        });
        let up = holding(0);
        let first = decide(&mut layer, &up, 1, 0.0);
        assert_eq!(first.mask, 1);
        assert_eq!(first.silence, None, "a frame that pressed something has no cause");
        assert_eq!(
            decide(&mut layer, &up, 1, 16.0).silence,
            Some(Silence::Macro),
            "a gap between two pulses is the macro's own frame"
        );
        assert_eq!(decide(&mut layer, &up, 1, 32.0).silence, Some(Silence::Macro));
    }

    #[test]
    fn every_silence_cause_has_a_name_for_the_table() {
        for cause in Silence::ALL {
            assert!(!cause.label().is_empty());
        }
        assert_eq!(Silence::ALL.len(), 5, "a new cause needs a column in the bench table");
    }

    #[test]
    fn raw_mode_builds_no_layer_at_all() {
        use crate::snapshot::MacroMode;

        let mut config = Config::default();
        assert_eq!(config.macros.mode, MacroMode::Raw);
        assert!(macro_layer(&config, 800.0, 1).is_none());
        config.macros.mode = MacroMode::Macros;
        assert!(macro_layer(&config, 800.0, 1).is_some(), "{}", config.macros.mode.as_str());
        // A game with no palette: `Config::validate` refuses this combination, and the layer
        // would refuse it too.
        config.loop_.game = "platformer".to_string();
        assert!(macro_layer(&config, 800.0, 1).is_none());
    }


}
