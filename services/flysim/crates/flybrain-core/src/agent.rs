//! The agent loop: the environment-agnostic glue, ported from `agent/agent.ts`.
//!
//! The call order inside [`NeuralAgent::tick`] is behaviour, not style: the readout sees the rates
//! produced by this frame's ticks but the *previous* frame's image, because a real environment
//! cannot render the consequence of a button before the button is pressed.

use std::sync::Arc;

use crate::dataset::BrainDataset;
use crate::decoder::{DecoderConfig, DecoderState, PopulationDecoder};
use crate::error::{bail, Result};
use crate::lif::{LifConfig, LifNetwork, LifState, SweepPlan};
use crate::plasticity::{LearningStats, PlasticityConfig};

/// One Game Boy frame in milliseconds: 70,224 dot clocks at 4,194,304 Hz (59.7275 fps).
pub const GAMEBOY_MS_PER_FRAME: f64 = 1000.0 / (4_194_304.0 / 70_224.0);

/// Warm-up length in milliseconds: long enough for rates to settle before calibration.
pub const DEFAULT_WARMUP_MS: u64 = 2500;

/// Stimulation pulse length applied by a reward event that gives no duration.
pub const DEFAULT_STIMULATION_MS: f64 = 120.0;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Kernel overrides. Defaults are bit-exact with the prototype.
    pub lif: LifConfig,
    pub plasticity: PlasticityConfig,
    /// Readout configuration; see `decoder::gameboy` for the device preset.
    pub decoder: DecoderConfig,
    /// Size of the RGBA frames passed to `tick`. `None` defaults to the kernel's retina size.
    pub frame: Option<FrameSize>,
    /// Milliseconds stepped by [`NeuralAgent::warmup`] with plasticity disabled.
    pub warmup_ms: u64,
    /// Milliseconds of network time per environment frame.
    pub ms_per_frame: f64,
}

impl AgentConfig {
    /// Default kernel and plasticity constants with the given readout.
    pub fn with_decoder(decoder: DecoderConfig) -> Self {
        Self {
            lif: LifConfig::default(),
            plasticity: PlasticityConfig::default(),
            decoder,
            frame: None,
            warmup_ms: DEFAULT_WARMUP_MS,
            ms_per_frame: GAMEBOY_MS_PER_FRAME,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
}

/// One reward the environment detected during the frame being reported.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RewardEvent {
    /// Signed magnitude of the modulator. Values from several events in one frame are summed.
    pub value: f64,
    /// Length of the stimulation pulse; `None` defaults to [`DEFAULT_STIMULATION_MS`].
    pub stimulation_ms: Option<f64>,
}

impl RewardEvent {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            stimulation_ms: None,
        }
    }

    pub fn with_stimulation(value: f64, stimulation_ms: f64) -> Self {
        Self {
            value,
            stimulation_ms: Some(stimulation_ms),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TickOptions<'a> {
    /// Rewards the environment detected for this frame.
    pub rewards: &'a [RewardEvent],
    /// Selects each pulse channel's boot variant in the readout.
    pub boot: bool,
    /// Whether this frame may reinforce. Hosts set it false while a human is driving.
    pub learn: bool,
}

impl Default for TickOptions<'_> {
    fn default() -> Self {
        Self {
            rewards: &[],
            boot: true,
            learn: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TickResult {
    /// Active output channel names, in decoder order.
    pub active: Vec<String>,
    /// Network ticks stepped for this frame.
    pub steps: u64,
    /// Spikes emitted across those ticks.
    pub spikes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentSnapshot {
    pub ms: f64,
    pub population_rate: f64,
    pub rates: crate::ordered::NumberMap,
    pub learning: LearningStats,
    /// Last spike time per neuron, narrowed to f32 for transfer to a renderer.
    pub spike_times: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentState {
    pub version: u32,
    /// Fractional millisecond carried into the next frame; always in [0, 1).
    pub remainder: f64,
    pub warmed_up: bool,
    pub network: LifState,
    pub decoder: DecoderState,
}

/// The checkpoint schema version this agent writes and accepts.
pub const AGENT_STATE_VERSION: u32 = 1;

/// A network, its plasticity and a readout, stepped one environment frame at a time.
pub struct NeuralAgent {
    pub network: LifNetwork,
    pub decoder: PopulationDecoder,
    /// Size of the frames [`NeuralAgent::tick`] accepts.
    pub frame: FrameSize,
    pub warmup_ms: u64,
    pub ms_per_frame: f64,
    remainder: f64,
    warmed_up: bool,
}

impl std::fmt::Debug for NeuralAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NeuralAgent")
            .field("network", &self.network)
            .field("frame", &self.frame)
            .field("warmupMs", &self.warmup_ms)
            .field("msPerFrame", &self.ms_per_frame)
            .field("remainder", &self.remainder)
            .field("warmedUp", &self.warmed_up)
            .finish()
    }
}

impl NeuralAgent {
    pub fn new(data: Arc<BrainDataset>, config: AgentConfig) -> Result<Self> {
        let network = LifNetwork::new(data, config.lif, config.plasticity)?;
        let decoder = PopulationDecoder::new(config.decoder)?;
        let retina = network.config.retina;
        let frame = config.frame.unwrap_or(FrameSize {
            width: retina.width,
            height: retina.height,
        });
        if frame.width == 0 || frame.height == 0 {
            bail!("Agent frame size must be positive integers");
        }
        if !config.ms_per_frame.is_finite() || config.ms_per_frame <= 0.0 {
            bail!("Agent msPerFrame must be positive");
        }
        Ok(Self {
            network,
            decoder,
            frame,
            warmup_ms: config.warmup_ms,
            ms_per_frame: config.ms_per_frame,
            remainder: 0.0,
            warmed_up: false,
        })
    }

    /// Choose how the per-tick neuron sweep is parallelized. Results do not depend on this.
    pub fn set_sweep_plan(&mut self, sweep: SweepPlan) {
        self.network.set_sweep_plan(sweep);
    }

    /// The network's plasticity rule.
    pub fn plasticity(&self) -> &crate::plasticity::RewardModulatedStdp {
        &self.network.plasticity
    }

    /// The network's plasticity rule; `agent.plasticity_mut().enabled = false` freezes learning.
    pub fn plasticity_mut(&mut self) -> &mut crate::plasticity::RewardModulatedStdp {
        &mut self.network.plasticity
    }

    /// Whether [`NeuralAgent::warmup`] has run (or a warmed-up checkpoint was imported).
    pub fn ready(&self) -> bool {
        self.warmed_up
    }

    pub fn remainder(&self) -> f64 {
        self.remainder
    }

    /// Settle the network and calibrate the readout against the resting rates.
    ///
    /// Plasticity is disabled for the warm-up: the transient from a zeroed membrane is not
    /// experience and must not enter the eligibility traces. Calibration then happens *after*
    /// re-enabling it, on the settled rates.
    pub fn warmup(&mut self, first_frame: Option<&[u8]>) -> Result<()> {
        if self.warmed_up {
            bail!("Agent is already warmed up");
        }
        self.network.plasticity.enabled = false;
        self.network.step(self.warmup_ms);
        self.network.plasticity.enabled = true;
        let rates = self.network.rates.clone();
        self.decoder.calibrate(&rates);
        if let Some(frame) = first_frame {
            self.set_frame(frame)?;
        }
        self.warmed_up = true;
        Ok(())
    }

    /// Advance one environment frame and return the channels the environment should hold.
    ///
    /// `frame` is the image the environment produced *for* this frame; it becomes the network's
    /// visual drive for the next one.
    pub fn tick(&mut self, frame: &[u8], options: &TickOptions<'_>) -> Result<TickResult> {
        self.tick_blocked(frame, options, None)
    }

    /// [`NeuralAgent::tick`] plus the readout's blocked-direction input.
    ///
    /// `blocked` is handed straight to [`PopulationDecoder::decode_blocked`] and is the only thing
    /// the environment tells the readout besides the rates. A caller that does not track position
    /// passes `None`, which is what [`NeuralAgent::tick`] does.
    pub fn tick_blocked(
        &mut self,
        frame: &[u8],
        options: &TickOptions<'_>,
        blocked: Option<&str>,
    ) -> Result<TickResult> {
        self.tick_bound(frame, options, blocked, None)
    }

    /// [`NeuralAgent::tick_blocked`] plus the macro group's bound-channel mask.
    ///
    /// `bound` is handed straight to [`PopulationDecoder::decode_bound`]: the macro channels the
    /// scene has put on the pad (`docs/design/macros.md` section 12). A host with no macro group
    /// -- every caller that came before it -- passes `None` and decodes exactly as it always did.
    ///
    /// `flysim`'s sim loop does not come through here (it drives the network and the decoder
    /// itself, so that the macro layer can read the emulator between the two), but the bench that
    /// measures the two arms against each other does, and a bench whose macro group could win a
    /// channel the scene never bound would be measuring something the stream cannot do.
    pub fn tick_bound(
        &mut self,
        frame: &[u8],
        options: &TickOptions<'_>,
        blocked: Option<&str>,
        bound: Option<&[String]>,
    ) -> Result<TickResult> {
        if !self.warmed_up {
            bail!("Warm up the agent before ticking it");
        }
        // Checked before anything advances: a rejected frame must not leave a half-stepped network.
        self.check_frame(frame)?;
        self.remainder += self.ms_per_frame;
        let steps = self.remainder.floor();
        self.remainder -= steps;
        let spikes = self.network.step(steps as u64);
        let rates = self.network.rates.clone();
        let active =
            self.decoder
                .decode_bound(&rates, self.network.ms, options.boot, blocked, bound);
        self.set_frame(frame)?;
        let mut total = 0.0f64;
        for event in options.rewards {
            self.network
                .stimulate(event.stimulation_ms.unwrap_or(DEFAULT_STIMULATION_MS));
            total += event.value;
        }
        // One bounded modulatory pulse per frame: stacking per-event calls would make the update
        // order-dependent, and `reinforce` is a no-op for a zero sum anyway.
        if options.learn {
            let ms = self.network.ms;
            self.network.plasticity.reinforce(total, ms);
        }
        Ok(TickResult {
            active,
            steps: steps as u64,
            spikes,
        })
    }

    /// Drop everything that describes "what just happened" while keeping everything learned.
    pub fn reset_transients(&mut self, frame: &[u8]) -> Result<()> {
        self.check_frame(frame)?;
        let ms = self.network.ms;
        self.decoder.clear_holds(ms);
        self.network.plasticity.clear_eligibility(ms);
        self.set_frame(frame)
    }

    /// Cheap per-frame telemetry for a UI; allocates copies, so call it at display rate.
    pub fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            ms: self.network.ms,
            population_rate: self.network.population_rate,
            rates: self.network.rates.clone(),
            learning: self.network.plasticity.statistics(),
            spike_times: self
                .network
                .last_spike_ms
                .iter()
                .map(|value| *value as f32)
                .collect(),
        }
    }

    pub fn export_state(&self) -> AgentState {
        AgentState {
            version: AGENT_STATE_VERSION,
            remainder: self.remainder,
            warmed_up: self.warmed_up,
            network: self.network.export_state(),
            decoder: self.decoder.export_state(),
        }
    }

    /// Load a checkpoint, or leave the agent exactly as it was.
    ///
    /// The network and the readout validate themselves, but they are separate objects: a
    /// checkpoint whose network half is valid and whose readout half is not would otherwise leave
    /// a half-loaded agent running. The previous state is exported first and re-imported on any
    /// failure, so a rejected checkpoint is a no-op rather than a corrupted session.
    pub fn import_state(&mut self, state: &AgentState) -> Result<()> {
        let previous = self.export_state();
        match self.try_import(state) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.restore(&previous);
                Err(error)
            }
        }
    }

    fn try_import(&mut self, state: &AgentState) -> Result<()> {
        if state.version != AGENT_STATE_VERSION {
            bail!("Unsupported agent checkpoint version");
        }
        if !state.remainder.is_finite() || state.remainder < 0.0 || state.remainder >= 1.0 {
            bail!("Invalid agent frame remainder");
        }
        self.network.import_state(&state.network)?;
        self.decoder.import_state(&state.decoder)?;
        self.remainder = state.remainder;
        self.warmed_up = state.warmed_up;
        Ok(())
    }

    /// The library's half of a checkpoint compatibility string: kernel version, dataset identity
    /// and plasticity version. A host appends its own environment, adapter and build identifiers.
    pub fn compatibility(&self) -> String {
        format!(
            "{}/{}/{}",
            self.network.version,
            self.network
                .data
                .fingerprint
                .as_deref()
                .unwrap_or("unfingerprinted"),
            self.network.plasticity.version
        )
    }

    /// Re-import a state this agent produced itself; used only to undo a failed import.
    fn restore(&mut self, state: &AgentState) {
        // Both halves came from this very agent, so neither import can fail.
        let _ = self.network.import_state(&state.network);
        let _ = self.decoder.import_state(&state.decoder);
        self.remainder = state.remainder;
        self.warmed_up = state.warmed_up;
    }

    fn check_frame(&self, frame: &[u8]) -> Result<()> {
        let expected = self.frame.width as usize * self.frame.height as usize * 4;
        if frame.len() != expected {
            bail!("Frame must be {expected} RGBA bytes, got {}", frame.len());
        }
        Ok(())
    }

    fn set_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.check_frame(frame)?;
        let (width, height) = (self.frame.width, self.frame.height);
        self.network.set_visual_frame(frame, width, height);
        Ok(())
    }
}
