/**
 * The audio engine: game PCM from the feed through a worklet ring buffer, plus the SFX bank
 * (design A8).
 *
 * The page is the stream's audio source: `flycast` captures the same X display and the same Pulse
 * sink, so A/V sync is the browser's problem and not ffmpeg's. That makes three failure modes
 * worth designing against, all of which look like success:
 *
 *   - the AudioContext never leaves `suspended` (no `--autoplay-policy=no-user-gesture-required`)
 *   - the worklet module fails to load
 *   - the sink exists but nothing is written to it
 *
 * So every step is guarded and reported rather than thrown, `state()` exposes what actually
 * happened, and nothing here can take the page down. In tests the context is usually suspended
 * and `start()` must still resolve without throwing — `tests/unit` covers the policy maths and
 * the e2e "no console errors" check covers the rest.
 */
import { AUDIO_RATE } from '@flybrain/feed';
import { CHANNELS, DEFAULT_DRIFT_POLICY, framesFor, type DriftPolicy } from './drift';
import { SFX_NAMES, renderSfxBank, type SfxName } from './sfx';
import workletUrl from './ring-worklet.js?url';

export interface AudioGains {
  master: number;
  game: number;
  sfx: number;
}

export interface AudioEngineState {
  /** `unavailable` when the browser has no Web Audio at all. */
  context: AudioContextState | 'unavailable';
  worklet: 'idle' | 'ready' | 'failed';
  fillMs: number;
  rate: number;
  underruns: number;
  drops: number;
  pushedFrames: number;
  sfxLoaded: number;
  lastError: string | null;
}

interface WorkletStats {
  type: 'stats';
  fillFrames: number;
  rate: number;
  underruns: number;
  drops: number;
  pushed: number;
}

export class AudioEngine {
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private masterGain: GainNode | null = null;
  private gameGain: GainNode | null = null;
  private sfxGain: GainNode | null = null;
  private bank = new Map<SfxName, AudioBuffer>();
  private stats: WorkletStats | null = null;
  private workletState: 'idle' | 'ready' | 'failed' = 'idle';
  private lastError: string | null = null;
  private readonly policy: DriftPolicy;

  constructor(
    private gains: AudioGains,
    policy: DriftPolicy = DEFAULT_DRIFT_POLICY,
  ) {
    this.policy = policy;
  }

  /**
   * Bring up the context, the worklet and the SFX bank.
   *
   * Never throws. A failure is recorded in `state().lastError` and the page keeps running silent,
   * which is exactly what `flycast`'s pre-flight check is for: it refuses to go live when
   * `state().context !== 'running'`.
   */
  async start(): Promise<void> {
    if (typeof AudioContext === 'undefined') {
      this.lastError = 'this browser has no AudioContext';
      return;
    }

    try {
      // 48 kHz is the feed's rate and Web Audio's native rate on Linux, so nothing resamples.
      this.context = new AudioContext({ sampleRate: AUDIO_RATE, latencyHint: 'playback' });
    } catch (error) {
      this.lastError = `AudioContext: ${(error as Error).message}`;
      return;
    }

    const context = this.context;
    this.masterGain = context.createGain();
    this.gameGain = context.createGain();
    this.sfxGain = context.createGain();
    this.applyGains();
    this.gameGain.connect(this.masterGain);
    this.sfxGain.connect(this.masterGain);
    this.masterGain.connect(context.destination);

    try {
      await context.audioWorklet.addModule(workletUrl);
      const node = new AudioWorkletNode(context, 'ring-player', {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        outputChannelCount: [CHANNELS],
        processorOptions: {
          channels: CHANNELS,
          capacityFrames: framesFor(this.policy.capacityMs, context.sampleRate),
          targetFrames: framesFor(this.policy.targetMs, context.sampleRate),
          maxDrift: this.policy.maxDrift,
          gain: this.policy.gain,
          reportEveryFrames: framesFor(100, context.sampleRate),
        },
      });
      node.port.onmessage = (event: MessageEvent<WorkletStats>) => {
        if (event.data?.type === 'stats') this.stats = event.data;
      };
      node.connect(this.gameGain);
      this.node = node;
      this.workletState = 'ready';
    } catch (error) {
      this.workletState = 'failed';
      this.lastError = `audio worklet: ${(error as Error).message}`;
    }

    try {
      this.bank = await renderSfxBank(context.sampleRate);
    } catch (error) {
      this.lastError = `sfx: ${(error as Error).message}`;
    }

    // A kiosk Chromium launched with `--autoplay-policy=no-user-gesture-required` starts running;
    // anywhere else this is a no-op that leaves the context suspended, which is not an error.
    try {
      if (context.state === 'suspended') await context.resume();
    } catch {
      // Suspended is a legitimate state in a test browser. Nothing to do.
    }
  }

  /** Queue one snapshot's PCM. Transfers the buffer, so the caller must not reuse it. */
  push(chunk: Float32Array): void {
    const node = this.node;
    if (!node || chunk.length === 0) return;
    try {
      node.port.postMessage(chunk, [chunk.buffer]);
    } catch {
      // A detached buffer or a torn-down node: drop the chunk rather than fail the frame.
    }
  }

  /** Drop everything buffered. Used when a fixture loops or the feed reconnects. */
  flush(): void {
    try {
      this.node?.port.postMessage({ type: 'flush' });
    } catch {
      // Nothing to flush.
    }
  }

  /**
   * Fire one effect. Silent and harmless when the bank or the context is unavailable.
   *
   * `gain` is the per-cue level the moment catalogue's sound tier asks for
   * (`src/motion/sfx-tiers.ts`), applied through its own one-shot `GainNode` under the SFX bus, so
   * a tier that deliberately under-plays its sample cannot change the bus level for everything
   * after it. 1 is the common case and allocates nothing extra.
   */
  playSfx(name: SfxName, gain = 1): void {
    const context = this.context;
    const buffer = this.bank.get(name);
    const target = this.sfxGain;
    if (!context || !buffer || !target || context.state !== 'running') return;
    try {
      const source = context.createBufferSource();
      source.buffer = buffer;
      if (gain >= 1) {
        source.connect(target);
      } else {
        const trim = context.createGain();
        trim.gain.value = Math.max(0, gain);
        source.connect(trim).connect(target);
      }
      source.start();
    } catch {
      // Same rule: a sound effect never breaks the page.
    }
  }

  setGains(gains: AudioGains): void {
    this.gains = gains;
    this.applyGains();
  }

  state(): AudioEngineState {
    const sampleRate = this.context?.sampleRate ?? AUDIO_RATE;
    return {
      context: this.context ? this.context.state : 'unavailable',
      worklet: this.workletState,
      fillMs: this.stats ? (this.stats.fillFrames / sampleRate) * 1000 : 0,
      rate: this.stats?.rate ?? 1,
      underruns: this.stats?.underruns ?? 0,
      drops: this.stats?.drops ?? 0,
      pushedFrames: this.stats?.pushed ?? 0,
      sfxLoaded: SFX_NAMES.filter((name) => this.bank.has(name)).length,
      lastError: this.lastError,
    };
  }

  async stop(): Promise<void> {
    try {
      this.node?.port.postMessage({ type: 'close' });
      this.node?.disconnect();
      await this.context?.close();
    } catch {
      // Shutting down is best-effort.
    }
    this.node = null;
    this.context = null;
  }

  private applyGains(): void {
    if (this.masterGain) this.masterGain.gain.value = clamp01(this.gains.master);
    if (this.gameGain) this.gameGain.gain.value = clamp01(this.gains.game);
    if (this.sfxGain) this.sfxGain.gain.value = clamp01(this.gains.sfx);
  }
}

function clamp01(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(1, value));
}
