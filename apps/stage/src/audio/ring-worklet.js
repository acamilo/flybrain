/**
 * AudioWorklet processor: a ring buffer with a fractional read index (design A8).
 *
 * Plain JavaScript on purpose. An AudioWorklet module is loaded by URL into its own global scope,
 * so it is not part of the bundle graph; keeping it as hand-written ES module JS avoids relying on
 * static imports inside worklet scope, which is not a portable thing to depend on.
 *
 * It owns no policy: `targetFrames`, `maxDrift`, `gain` and the capacity all arrive in
 * `processorOptions` from `src/audio/drift.ts`, which is where they are documented and tested.
 * This file only applies them.
 *
 * Audio arrives as transferred `Float32Array`s over `port.postMessage` — deliberately not a
 * SharedArrayBuffer, which would need COOP/COEP headers on the page, while 30 messages a second
 * costs nothing.
 */

class RingPlayer extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const config = (options && options.processorOptions) || {};

    this.channels = config.channels || 2;
    this.capacity = Math.max(2048, config.capacityFrames || 72000);
    this.targetFrames = config.targetFrames || 12000;
    this.maxDrift = config.maxDrift || 0.003;
    this.gain = config.gain || 0.5;
    this.reportEvery = config.reportEveryFrames || 4800;

    // One interleaved ring, so a chunk is a single copy in.
    this.ring = new Float32Array(this.capacity * this.channels);
    this.writeIndex = 0;
    this.readIndex = 0; // fractional, in frames
    this.fill = 0; // frames available

    this.underruns = 0;
    this.drops = 0;
    this.pushed = 0;
    this.rate = 1;
    this.framesSinceReport = 0;
    this.closed = false;

    this.port.onmessage = (event) => {
      const data = event.data;
      if (data instanceof Float32Array) {
        this.push(data);
        return;
      }
      if (data && data.type === 'flush') {
        this.writeIndex = 0;
        this.readIndex = 0;
        this.fill = 0;
        return;
      }
      if (data && data.type === 'close') {
        this.closed = true;
      }
    };
  }

  /** Append interleaved frames, dropping the oldest if the producer has run away. */
  push(interleaved) {
    const frames = Math.floor(interleaved.length / this.channels);
    if (frames <= 0) return;
    this.pushed += frames;

    for (let frame = 0; frame < frames; frame++) {
      const slot = (this.writeIndex % this.capacity) * this.channels;
      for (let channel = 0; channel < this.channels; channel++) {
        this.ring[slot + channel] = interleaved[frame * this.channels + channel];
      }
      this.writeIndex += 1;
    }

    this.fill += frames;
    if (this.fill > this.capacity) {
      // Hard overflow: keep the newest audio, count it, carry on.
      const excess = this.fill - this.capacity;
      this.readIndex += excess;
      this.fill = this.capacity;
      this.drops += excess;
    }
  }

  /** One interpolated frame at the fractional read index. */
  sample(channel, position) {
    const base = Math.floor(position);
    const fraction = position - base;
    const a = this.ring[(base % this.capacity) * this.channels + channel];
    const b = this.ring[((base + 1) % this.capacity) * this.channels + channel];
    return a + (b - a) * fraction;
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length === 0) return !this.closed;
    const blockFrames = output[0].length;

    // Gross excursion first: an over-full ring is trimmed to the target rather than played out.
    if (this.fill > this.targetFrames * 2) {
      const excess = this.fill - this.targetFrames;
      this.readIndex += excess;
      this.fill -= excess;
      this.drops += excess;
    }

    // Varispeed: pull the read rate towards whatever keeps the fill at the target.
    const error = this.targetFrames > 0 ? (this.fill - this.targetFrames) / this.targetFrames : 0;
    const correction = Math.max(-this.maxDrift, Math.min(this.maxDrift, error * this.gain));
    this.rate = 1 + correction;

    // Needing more than the ring holds is an underrun: emit silence and say so.
    const needed = Math.ceil(blockFrames * this.rate) + 2;
    if (this.fill < needed) {
      this.underruns += 1;
      for (let channel = 0; channel < output.length; channel++) output[channel].fill(0);
      this.report(blockFrames);
      return !this.closed;
    }

    for (let frame = 0; frame < blockFrames; frame++) {
      const position = this.readIndex + frame * this.rate;
      for (let channel = 0; channel < output.length; channel++) {
        output[channel][frame] = this.sample(Math.min(channel, this.channels - 1), position);
      }
    }

    const consumed = blockFrames * this.rate;
    this.readIndex += consumed;
    this.fill -= consumed;

    this.report(blockFrames);
    return !this.closed;
  }

  report(blockFrames) {
    this.framesSinceReport += blockFrames;
    if (this.framesSinceReport < this.reportEvery) return;
    this.framesSinceReport = 0;
    this.port.postMessage({
      type: 'stats',
      fillFrames: this.fill,
      rate: this.rate,
      underruns: this.underruns,
      drops: this.drops,
      pushed: this.pushed,
    });
  }
}

registerProcessor('ring-player', RingPlayer);
