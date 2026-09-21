/**
 * Thin adapter over `@flybrain/feed`'s `decodeSnapshot`: one wire message in, typed views out.
 *
 * Two things this layer exists for, both of them sharp edges:
 *
 * 1. **Alignment.** The wire format is `u32 headerLength | header JSON | attachments...`, and the
 *    header JSON length is arbitrary, so an attachment's byte offset is arbitrary too. A
 *    `Float32Array` view needs a 4-byte-aligned offset, so the audio attachment is copied when it
 *    is not aligned. Skipping this check gives you a `RangeError` at some random snapshot hours
 *    into a broadcast.
 * 2. **Shape.** A snapshot whose `frame` is not 160x144 RGBA, or whose `spikes` bitset is not the
 *    dataset's size, is dropped as a decode error rather than painted as garbage. The page shows
 *    the stale banner instead, which is the honest failure.
 */
import { FRAME_HEIGHT, FRAME_WIDTH, decodeSnapshot, type FeedHeader } from '@flybrain/feed';

/** Bytes in a `frame` attachment: 160 x 144 RGBA. */
export const FRAME_BYTES = FRAME_WIDTH * FRAME_HEIGHT * 4;

/** One decoded snapshot: the header plus whichever attachments came with it. */
export interface DecodedSnapshot {
  header: FeedHeader;
  /** 160x144 RGBA, or null when the snapshot carried no frame. */
  frame: Uint8Array | null;
  /** Interleaved stereo f32 at 48 kHz, or null. Always a copy, so it can be transferred. */
  audio: Float32Array | null;
  /** Spike bitset, bit `i` set when neuron `i` fired, or null. A zero-copy view. */
  spikes: Uint8Array | null;
}

/** Thrown when a message decodes but does not describe a snapshot this page can paint. */
export class SnapshotShapeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'SnapshotShapeError';
  }
}

/**
 * Decode one binary feed message.
 *
 * `expectedSpikeBytes` is `ceil(neurons / 8)` once the dataset is known; pass 0 before then to
 * accept any size (the page paints the game and the readouts long before the brain map's worker
 * has finished loading positions).
 */
export function decodeFeedMessage(bytes: Uint8Array, expectedSpikeBytes = 0): DecodedSnapshot {
  const { header, attachments } = decodeSnapshot(bytes);

  const frameBytes = attachments.get('frame') ?? null;
  if (frameBytes && frameBytes.byteLength !== FRAME_BYTES) {
    throw new SnapshotShapeError(
      `frame attachment is ${frameBytes.byteLength} bytes, expected ${FRAME_BYTES} (${FRAME_WIDTH}x${FRAME_HEIGHT} RGBA)`,
    );
  }

  const spikeBytes = attachments.get('spikes') ?? null;
  if (spikeBytes && expectedSpikeBytes > 0 && spikeBytes.byteLength !== expectedSpikeBytes) {
    throw new SnapshotShapeError(
      `spikes bitset is ${spikeBytes.byteLength} bytes, expected ${expectedSpikeBytes}`,
    );
  }

  return {
    header,
    frame: frameBytes,
    audio: toFloat32(attachments.get('audio')),
    spikes: spikeBytes,
  };
}

/**
 * Copy an audio attachment into a `Float32Array`.
 *
 * Always a copy: the caller transfers it to the AudioWorklet, and a view into a shared message
 * buffer cannot be transferred without taking the frame and spikes with it.
 */
function toFloat32(bytes: Uint8Array | undefined): Float32Array | null {
  if (!bytes) return null;
  if (bytes.byteLength % 4 !== 0) {
    throw new SnapshotShapeError(`audio attachment is ${bytes.byteLength} bytes, not a whole number of f32 samples`);
  }
  const copy = new Float32Array(bytes.byteLength / 4);
  new Uint8Array(copy.buffer).set(bytes);
  return copy;
}
