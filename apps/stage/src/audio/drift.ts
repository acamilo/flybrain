/**
 * Ring-buffer policy and the varispeed drift servo (design A8).
 *
 * The emulator produces audio in 30 Hz chunks off the simulation clock; the AudioContext consumes
 * it off the sound card's clock. Those two clocks are never the same, so over an hour the buffer
 * either fills until it overflows or drains until it clicks. A fractional read index resampling at
 * 1 ± 0.3 percent absorbs the difference inaudibly, and only a gross excursion (double the target
 * fill, or an empty buffer) is corrected by dropping or inserting samples.
 *
 * The policy lives here, in TypeScript, and is handed to the AudioWorklet as `processorOptions`:
 * the worklet applies the formula but owns none of the numbers, so there is exactly one place to
 * change them and `tests/unit/drift.test.ts` can check them without an AudioContext.
 */

/** Interleaved stereo. */
export const CHANNELS = 2;

export interface DriftPolicy {
  /** Target buffered audio, ms. */
  targetMs: number;
  /** Above this, the servo is pulling the read rate up. */
  highMs: number;
  /** Below this, the servo is pulling the read rate down. */
  lowMs: number;
  /** Maximum fractional rate deviation, e.g. 0.003 for ±0.3 percent. */
  maxDrift: number;
  /** Proportional gain on the normalised fill error. */
  gain: number;
  /** Ring capacity, ms. Must comfortably exceed `highMs`. */
  capacityMs: number;
}

export const DEFAULT_DRIFT_POLICY: DriftPolicy = {
  targetMs: 250,
  highMs: 400,
  lowMs: 120,
  maxDrift: 0.003,
  gain: 0.5,
  capacityMs: 1500,
};

/** Frames (per channel) for a duration at a sample rate. */
export function framesFor(ms: number, sampleRate: number): number {
  return Math.round((ms / 1000) * sampleRate);
}

/**
 * Playback rate for the current fill level.
 *
 * Above the target the rate goes slightly *up* (consume faster, drain the excess); below, down.
 * Clamped to `maxDrift` in both directions, which is the whole point: the correction must be
 * inaudible, so it is never allowed to be fast.
 */
export function varispeedRate(fillFrames: number, policy: DriftPolicy, sampleRate: number): number {
  const target = framesFor(policy.targetMs, sampleRate);
  if (target <= 0) return 1;
  const error = (fillFrames - target) / target;
  const correction = Math.max(-policy.maxDrift, Math.min(policy.maxDrift, error * policy.gain));
  return 1 + correction;
}

/** What the servo cannot fix on its own. */
export type HardCorrection = 'drop' | 'insert' | null;

/**
 * Gross excursions.
 *
 * `drop` when the buffer holds more than twice the target (the producer ran ahead, e.g. after the
 * page was throttled): discard down to the target rather than play a growing delay for the rest of
 * the broadcast. `insert` when it is empty: emit silence and count an underrun.
 */
export function hardCorrection(fillFrames: number, policy: DriftPolicy, sampleRate: number): HardCorrection {
  const target = framesFor(policy.targetMs, sampleRate);
  if (fillFrames <= 0) return 'insert';
  if (fillFrames > target * 2) return 'drop';
  return null;
}

/** Frames to discard to bring an over-full buffer back to the target. */
export function dropCount(fillFrames: number, policy: DriftPolicy, sampleRate: number): number {
  const target = framesFor(policy.targetMs, sampleRate);
  return Math.max(0, fillFrames - target);
}

/** Where the servo currently is, for the health readout. */
export function fillZone(fillFrames: number, policy: DriftPolicy, sampleRate: number): 'low' | 'ok' | 'high' {
  const ms = (fillFrames / sampleRate) * 1000;
  if (ms < policy.lowMs) return 'low';
  if (ms > policy.highMs) return 'high';
  return 'ok';
}
