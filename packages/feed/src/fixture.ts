/**
 * `.flyfeed` fixture container: a recorded run of the feed protocol, replayable with no service.
 *
 * A fixture is the exact wire bytes of `docs/feed-protocol.md` snapshots, length-prefixed, behind
 * a small JSON manifest that records what was captured and what (if anything) the recorder
 * dropped. Shared here rather than in `apps/stage` because the recorder (Node), the player
 * (browser) and the tests all need one implementation.
 *
 * ```
 * "FLYFEED\0"        8 bytes magic
 * u32 LE version     1
 * u32 LE manifestLen
 * manifest JSON      UTF-8
 * records            repeated: u32 LE byteLength | wire message bytes
 * ```
 *
 * No per-record timestamp: every snapshot header already carries `wallMs`, so a player derives
 * its schedule from the messages themselves and a fixture of a real run needs no extra metadata.
 *
 * Pure functions on `Uint8Array` only — no `node:fs`, no `node:zlib`. A `.flyfeed.gz` is just a
 * gzipped file of this format; the caller inflates it (`DecompressionStream` in the browser,
 * `node:zlib` in tools) and passes the inflated bytes here. {@link isGzip} sniffs which it has.
 */
import type { AttachmentKind } from './types';

/** Magic bytes at the start of every `.flyfeed` file. */
export const FLYFEED_MAGIC = 'FLYFEED\0';

/** Container format version. Bumps on any breaking change to the framing or manifest shape. */
export const FLYFEED_VERSION = 1;

const U32_BYTES = 4;
const MAGIC_BYTES = 8;

/**
 * What the recorder kept for one attachment kind.
 *
 * `stride: 1` keeps every snapshot's attachment, `3` keeps every third, and `0` keeps none.
 * `seconds` (when set) additionally limits the kind to the first N seconds of the recording.
 * Attachments the recorder dropped are removed from that snapshot's `header.attachments`, which
 * is exactly what a consumer that had not asked for them would have received.
 */
export interface FlyfeedAttachmentPolicy {
  stride: number;
  seconds?: number;
}

/** The manifest at the head of a `.flyfeed` file. */
export interface FlyfeedManifest {
  /** Fixture name, e.g. `steady`. Matches the file basename by convention. */
  name: string;
  /** Feed protocol version of the recorded messages. */
  protocol: 1;
  /** ISO 8601 timestamp of the recording. */
  recordedAt: string;
  /** Human description of where the messages came from, e.g. `fake-flysim scenario=running seed=7`. */
  source: string;
  /** Number of records in the file. */
  snapshotCount: number;
  /** Wall-clock span of the recording in ms (last `wallMs` minus first). */
  durationMs: number;
  /** Nominal snapshot rate of the source feed, Hz. */
  hz: number;
  /** Per-kind record of what the recorder kept. Absent kinds were never offered by the source. */
  attachmentPolicy: Partial<Record<AttachmentKind, FlyfeedAttachmentPolicy>>;
  /** Free-form notes, rendered by tooling rather than parsed. */
  notes?: string[];
}

/** Thrown on any framing or manifest violation. */
export class FlyfeedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FlyfeedError';
  }
}

/**
 * Encode the file head (magic, version, manifest). A streaming recorder writes this once, then
 * appends {@link encodeFlyfeedRecord} per snapshot, so it never holds the whole run in memory.
 */
export function encodeFlyfeedHeader(manifest: FlyfeedManifest): Uint8Array {
  if (manifest.protocol !== 1) {
    throw new FlyfeedError(`unsupported protocol ${String(manifest.protocol)}, expected 1`);
  }
  const manifestJson = new TextEncoder().encode(JSON.stringify(manifest));
  const out = new Uint8Array(MAGIC_BYTES + U32_BYTES * 2 + manifestJson.byteLength);
  const view = new DataView(out.buffer);

  for (let i = 0; i < MAGIC_BYTES; i++) out[i] = FLYFEED_MAGIC.charCodeAt(i);
  view.setUint32(MAGIC_BYTES, FLYFEED_VERSION, true);
  view.setUint32(MAGIC_BYTES + U32_BYTES, manifestJson.byteLength, true);
  out.set(manifestJson, MAGIC_BYTES + U32_BYTES * 2);

  return out;
}

/** Encode one length-prefixed record around an already-encoded wire message. */
export function encodeFlyfeedRecord(message: Uint8Array): Uint8Array {
  const out = new Uint8Array(U32_BYTES + message.byteLength);
  new DataView(out.buffer).setUint32(0, message.byteLength, true);
  out.set(message, U32_BYTES);
  return out;
}

/** Encode a complete `.flyfeed` file. Convenience over the two streaming primitives. */
export function encodeFlyfeed(manifest: FlyfeedManifest, messages: readonly Uint8Array[]): Uint8Array {
  const head = encodeFlyfeedHeader({ ...manifest, snapshotCount: messages.length });
  let total = head.byteLength;
  for (const message of messages) total += U32_BYTES + message.byteLength;

  const out = new Uint8Array(total);
  out.set(head, 0);
  const view = new DataView(out.buffer);
  let offset = head.byteLength;
  for (const message of messages) {
    view.setUint32(offset, message.byteLength, true);
    offset += U32_BYTES;
    out.set(message, offset);
    offset += message.byteLength;
  }
  return out;
}

/** Read the manifest and return where the records begin. */
export function readFlyfeedManifest(bytes: Uint8Array): { manifest: FlyfeedManifest; bodyOffset: number } {
  if (bytes.byteLength < MAGIC_BYTES + U32_BYTES * 2) {
    throw new FlyfeedError(`truncated file: ${bytes.byteLength} bytes is shorter than the header`);
  }
  for (let i = 0; i < MAGIC_BYTES; i++) {
    if (bytes[i] !== FLYFEED_MAGIC.charCodeAt(i)) {
      throw new FlyfeedError(
        isGzip(bytes)
          ? 'file is gzipped: inflate it before decoding (see docs in src/fixture.ts)'
          : 'not a .flyfeed file: bad magic',
      );
    }
  }

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const version = view.getUint32(MAGIC_BYTES, true);
  if (version !== FLYFEED_VERSION) {
    throw new FlyfeedError(`unsupported .flyfeed version ${version}, expected ${FLYFEED_VERSION}`);
  }

  const manifestLength = view.getUint32(MAGIC_BYTES + U32_BYTES, true);
  const manifestStart = MAGIC_BYTES + U32_BYTES * 2;
  if (manifestStart + manifestLength > bytes.byteLength) {
    throw new FlyfeedError(
      `truncated file: manifest claims ${manifestLength} bytes but only ${bytes.byteLength - manifestStart} remain`,
    );
  }

  let manifest: FlyfeedManifest;
  try {
    manifest = JSON.parse(new TextDecoder().decode(bytes.subarray(manifestStart, manifestStart + manifestLength))) as FlyfeedManifest;
  } catch (cause) {
    throw new FlyfeedError(`manifest is not valid JSON: ${(cause as Error).message}`);
  }
  if (manifest.protocol !== 1) {
    throw new FlyfeedError(`unsupported protocol ${String(manifest.protocol)}, expected 1`);
  }

  return { manifest, bodyOffset: manifestStart + manifestLength };
}

/**
 * Walk the records from `bodyOffset`, yielding zero-copy views into `bytes`.
 *
 * Views, not copies: a 120 s fixture is hundreds of megabytes of attachments and the player
 * only ever reads the most recent one.
 */
export function* iterateFlyfeedRecords(bytes: Uint8Array, bodyOffset: number): Generator<Uint8Array> {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = bodyOffset;
  while (offset < bytes.byteLength) {
    if (offset + U32_BYTES > bytes.byteLength) {
      throw new FlyfeedError('truncated file: not enough bytes to read the next record length');
    }
    const length = view.getUint32(offset, true);
    offset += U32_BYTES;
    if (offset + length > bytes.byteLength) {
      throw new FlyfeedError(
        `truncated record: claims ${length} bytes but only ${bytes.byteLength - offset} remain`,
      );
    }
    yield bytes.subarray(offset, offset + length);
    offset += length;
  }
}

/** Decode a complete `.flyfeed` file into its manifest and its records (zero-copy views). */
export function decodeFlyfeed(bytes: Uint8Array): { manifest: FlyfeedManifest; messages: Uint8Array[] } {
  const { manifest, bodyOffset } = readFlyfeedManifest(bytes);
  const messages = [...iterateFlyfeedRecords(bytes, bodyOffset)];
  if (manifest.snapshotCount !== messages.length) {
    throw new FlyfeedError(
      `manifest claims ${manifest.snapshotCount} snapshots but the file holds ${messages.length}`,
    );
  }
  return { manifest, messages };
}

/** True when `bytes` starts with the gzip magic (`1f 8b`), i.e. it is a `.flyfeed.gz`. */
export function isGzip(bytes: Uint8Array): boolean {
  return bytes.byteLength >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b;
}
