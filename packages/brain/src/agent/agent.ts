/**
 * The agent loop: the environment-agnostic glue that composes a {@link LifNetwork}, its
 * reward-modulated plasticity and a {@link PopulationDecoder} into something an environment can
 * drive one frame at a time.
 *
 * Nothing about a concrete environment enters this module. An environment supplies three things
 * per frame: an RGBA image, a list of reward events, and two flags (`boot`, `learn`). It gets back
 * the active output channel names. Wall-clock pacing, input devices, save states and reward
 * detection all belong to the host.
 *
 * The frame-to-millisecond relation is the one detail worth spelling out. Environments advance in
 * frames, the network in 1-ms ticks, and `msPerFrame` is generally not an integer (a Game Boy
 * frame is ~16.7427 ms). `tick()` therefore accumulates a fractional remainder and steps
 * `floor(remainder)` ticks, so the network neither drifts ahead of nor behind the environment over
 * a long session. The remainder is part of the checkpoint for exactly that reason.
 */
import type { BrainDataset } from '../dataset/format';
import { LifNetwork, type LifConfig, type LifState } from '../model/lif';
import type { LearningStats, PlasticityConfig, RewardModulatedStdp } from '../model/plasticity';
import { PopulationDecoder, type DecoderConfig, type DecoderState } from '../readout/decoder';

/**
 * One Game Boy frame in milliseconds: 70,224 dot clocks at 4,194,304 Hz, i.e. ~16.7427 ms
 * (59.7275 fps). The historical default of the loop, kept here so the agent layer has a sensible
 * default without importing a device preset.
 */
export const GAMEBOY_MS_PER_FRAME = 1000 / (4_194_304 / 70_224);

/** Warm-up length in milliseconds: long enough for rates to settle before calibration. */
export const DEFAULT_WARMUP_MS = 2500;

/** Stimulation pulse length in milliseconds applied by a reward event that gives no duration. */
export const DEFAULT_STIMULATION_MS = 120;

export interface AgentConfig {
  /** Kernel overrides, plus plasticity overrides under `plasticity`. Defaults are bit-exact. */
  lif?: Partial<LifConfig> & { plasticity?: Partial<PlasticityConfig> };
  /** Readout configuration; see `readout/presets/` for device presets. */
  decoder: DecoderConfig;
  /** Size of the RGBA frames passed to `tick`. Defaults to the kernel's retina size. */
  frame?: { width: number; height: number };
  /** Milliseconds stepped by {@link NeuralAgent.warmup} with plasticity disabled. Default 2500. */
  warmupMs?: number;
  /** Milliseconds of network time per environment frame. Default {@link GAMEBOY_MS_PER_FRAME}. */
  msPerFrame?: number;
}

/** One reward the environment detected during the frame being reported. */
export interface RewardEvent {
  /** Signed magnitude of the modulator. Values from several events in one frame are summed. */
  value: number;
  /** Length of the stimulation pulse; defaults to {@link DEFAULT_STIMULATION_MS}. */
  stimulationMs?: number;
}

export interface TickOptions {
  /** Rewards the environment detected for this frame. */
  rewards?: RewardEvent[];
  /** Selects each pulse channel's boot variant in the readout. Default true. */
  boot?: boolean;
  /**
   * Whether this frame may reinforce. Default true. Hosts set it false while a human is driving,
   * so plasticity never credits the network for an action it did not take.
   */
  learn?: boolean;
}

export interface TickResult {
  /** Active output channel names, in decoder order. */
  active: string[];
  /** Network ticks stepped for this frame. */
  steps: number;
  /** Spikes emitted across those ticks. */
  spikes: number;
}

export interface AgentSnapshot {
  ms: number;
  populationRate: number;
  rates: Record<string, number>;
  learning: LearningStats;
  /** Last spike time per neuron, narrowed to Float32 for transfer to a renderer. */
  spikeTimes: Float32Array;
}

export interface AgentState {
  version: 1;
  /** Fractional millisecond carried into the next frame; always in [0, 1). */
  remainder: number;
  warmedUp: boolean;
  network: LifState;
  decoder: DecoderState;
}

/**
 * A network, its plasticity and a readout, stepped one environment frame at a time.
 *
 * The call order inside {@link NeuralAgent.tick} is behaviour, not style: the readout sees the
 * rates produced by this frame's ticks but the *previous* frame's image, because a real environment
 * cannot render the consequence of a button before the button is pressed. Reordering it changes
 * the trajectory.
 */
export class NeuralAgent {
  readonly network: LifNetwork;
  readonly decoder: PopulationDecoder;
  /** Size of the frames {@link NeuralAgent.tick} accepts. */
  readonly frame: { width: number; height: number };
  readonly warmupMs: number;
  readonly msPerFrame: number;
  private remainder = 0;
  private warmedUp = false;

  constructor(dataset: BrainDataset, config: AgentConfig) {
    this.network = new LifNetwork(dataset, config.lif ?? {});
    this.decoder = new PopulationDecoder(config.decoder);
    const retina = this.network.config.retina;
    this.frame = config.frame ? { ...config.frame } : { width: retina.width, height: retina.height };
    this.warmupMs = config.warmupMs ?? DEFAULT_WARMUP_MS;
    this.msPerFrame = config.msPerFrame ?? GAMEBOY_MS_PER_FRAME;
    if (!Number.isInteger(this.frame.width) || !Number.isInteger(this.frame.height) || this.frame.width <= 0 || this.frame.height <= 0) {
      throw new Error('Agent frame size must be positive integers');
    }
    if (!Number.isSafeInteger(this.warmupMs) || this.warmupMs < 0) throw new Error('Agent warmupMs must be a non-negative integer');
    if (!Number.isFinite(this.msPerFrame) || this.msPerFrame <= 0) throw new Error('Agent msPerFrame must be positive');
  }

  /** The network's plasticity rule; `agent.plasticity.enabled = false` freezes learning. */
  get plasticity(): RewardModulatedStdp {
    return this.network.plasticity;
  }

  /** Whether {@link NeuralAgent.warmup} has run (or a warmed-up checkpoint was imported). */
  get ready(): boolean {
    return this.warmedUp;
  }

  /**
   * Settle the network and calibrate the readout against the resting rates.
   *
   * Plasticity is disabled for the warm-up: the transient from a zeroed membrane is not experience
   * and must not enter the eligibility traces. Calibration then happens *after* re-enabling it, on
   * the settled rates, so every later score is normalized against rest rather than the transient.
   */
  warmup(firstFrame?: Uint8Array): void {
    if (this.warmedUp) throw new Error('Agent is already warmed up');
    this.network.plasticity.enabled = false;
    this.network.step(this.warmupMs);
    this.network.plasticity.enabled = true;
    this.decoder.calibrate(this.network.rates);
    if (firstFrame) this.setFrame(firstFrame);
    this.warmedUp = true;
  }

  /**
   * Advance one environment frame and return the channels the environment should hold.
   *
   * `frame` is the image the environment produced *for* this frame; it becomes the network's
   * visual drive for the next one.
   */
  tick(frame: Uint8Array, options: TickOptions = {}): TickResult {
    if (!this.warmedUp) throw new Error('Warm up the agent before ticking it');
    // Checked before anything advances: a rejected frame must not leave a half-stepped network.
    this.checkFrame(frame);
    const boot = options.boot ?? true;
    const learn = options.learn ?? true;
    this.remainder += this.msPerFrame;
    const steps = Math.floor(this.remainder);
    this.remainder -= steps;
    const spikes = this.network.step(steps);
    const active = this.decoder.decode(this.network.rates, this.network.ms, boot);
    this.setFrame(frame);
    let total = 0;
    for (const event of options.rewards ?? []) {
      this.network.stimulate(event.stimulationMs ?? DEFAULT_STIMULATION_MS);
      total += event.value;
    }
    // One bounded modulatory pulse per frame: stacking per-event calls would make the update
    // order-dependent, and `reinforce` is a no-op for a zero sum anyway.
    if (learn) this.network.plasticity.reinforce(total, this.network.ms);
    return { active, steps, spikes };
  }

  /**
   * Drop everything that describes "what just happened" while keeping everything learned.
   *
   * Hosts call this when the environment jumps discontinuously (a save-state rollback, a level
   * reset): held outputs and eligibility traces describe a timeline that no longer exists, while
   * synaptic gains, the RNG stream and the clock do. Not doing it credits the next reward to
   * spike pairs from the abandoned branch.
   */
  resetTransients(frame: Uint8Array): void {
    this.checkFrame(frame);
    this.decoder.clearHolds(this.network.ms);
    this.network.plasticity.clearEligibility(this.network.ms);
    this.setFrame(frame);
  }

  /** Cheap per-frame telemetry for a UI; allocates copies, so call it at display rate. */
  snapshot(): AgentSnapshot {
    return {
      ms: this.network.ms,
      populationRate: this.network.populationRate,
      rates: { ...this.network.rates },
      learning: this.network.plasticity.statistics(),
      spikeTimes: Float32Array.from(this.network.lastSpikeMs),
    };
  }

  exportState(): AgentState {
    return {
      version: 1,
      remainder: this.remainder,
      warmedUp: this.warmedUp,
      network: this.network.exportState(),
      decoder: this.decoder.exportState(),
    };
  }

  /**
   * Load a checkpoint, or leave the agent exactly as it was.
   *
   * The network and the readout validate themselves, but they are separate objects: a checkpoint
   * whose network half is valid and whose readout half is not would otherwise leave a half-loaded
   * agent running. The previous state is exported first and re-imported on any failure, so a
   * rejected checkpoint is a no-op rather than a corrupted session.
   */
  importState(state: AgentState): void {
    const previous = this.exportState();
    try {
      if (!state || state.version !== 1) throw new Error('Unsupported agent checkpoint version');
      if (typeof state.warmedUp !== 'boolean') throw new Error('Invalid agent warm-up flag');
      if (!Number.isFinite(state.remainder) || state.remainder < 0 || state.remainder >= 1) throw new Error('Invalid agent frame remainder');
      this.network.importState(state.network);
      this.decoder.importState(state.decoder);
      this.remainder = state.remainder;
      this.warmedUp = state.warmedUp;
    } catch (error) {
      this.restore(previous);
      throw error;
    }
  }

  /**
   * The library's half of a checkpoint compatibility string: kernel version, dataset identity and
   * plasticity version. A host appends its own environment, adapter and build identifiers.
   */
  compatibility(): string {
    return `${this.network.version}/${this.network.data.fingerprint ?? 'unfingerprinted'}/${this.network.plasticity.version}`;
  }

  /** Re-import a state this agent produced itself; used only to undo a failed import. */
  private restore(state: AgentState): void {
    this.network.importState(state.network);
    this.decoder.importState(state.decoder);
    this.remainder = state.remainder;
    this.warmedUp = state.warmedUp;
  }

  private checkFrame(frame: Uint8Array): void {
    const expected = this.frame.width * this.frame.height * 4;
    if (frame.length !== expected) throw new Error(`Frame must be ${expected} RGBA bytes, got ${frame.length}`);
  }

  private setFrame(frame: Uint8Array): void {
    this.checkFrame(frame);
    this.network.setVisualFrame(frame, this.frame.width, this.frame.height);
  }
}
