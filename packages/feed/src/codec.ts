/**
 * Binary framing for one feed snapshot (docs/feed-protocol.md, "Framing").
 *
 * ```
 * u32 LE headerLength | header JSON (UTF-8) | attachments...
 * ```
 *
 * Attachments follow the header in the order listed by `header.attachments`, each as
 * `u32 LE byteLength | bytes`. Pure functions on `Uint8Array`/`DataView`/`TextEncoder`/
 * `TextDecoder` only, so both the browser stage and Node tests share one implementation.
 */
import type { AttachmentKind, FeedHeader } from './types';

const U32_BYTES = 4;

/** Thrown by `encodeSnapshot` and `decodeSnapshot` on any framing or contract violation. */
export class FeedCodecError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FeedCodecError';
  }
}

/** Encode one snapshot header plus its attachments into a single binary WebSocket message. */
export function encodeSnapshot(
  header: FeedHeader,
  attachments: Partial<Record<AttachmentKind, Uint8Array>>,
): Uint8Array {
  if (header.protocol !== 1) {
    throw new FeedCodecError(`unsupported protocol ${String(header.protocol)}, expected 1`);
  }

  const providedKinds = Object.keys(attachments) as AttachmentKind[];
  assertNoDuplicates(header.attachments, 'header.attachments');
  assertSameMembers(header.attachments, providedKinds);

  const headerJson = new TextEncoder().encode(JSON.stringify(header));

  let total = U32_BYTES + headerJson.byteLength;
  for (const kind of header.attachments) {
    const bytes = attachments[kind];
    // assertSameMembers already guarantees this is present.
    total += U32_BYTES + (bytes as Uint8Array).byteLength;
  }

  const out = new Uint8Array(total);
  const view = new DataView(out.buffer);
  let offset = 0;

  view.setUint32(offset, headerJson.byteLength, true);
  offset += U32_BYTES;

  out.set(headerJson, offset);
  offset += headerJson.byteLength;

  for (const kind of header.attachments) {
    const bytes = attachments[kind] as Uint8Array;
    view.setUint32(offset, bytes.byteLength, true);
    offset += U32_BYTES;
    out.set(bytes, offset);
    offset += bytes.byteLength;
  }

  return out;
}

/** Decode one binary WebSocket message into its header and attachment map. */
export function decodeSnapshot(bytes: Uint8Array): { header: FeedHeader; attachments: Map<AttachmentKind, Uint8Array> } {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 0;

  const headerLength = readU32(view, offset, bytes.length, 'header length');
  offset += U32_BYTES;

  if (offset + headerLength > bytes.length) {
    throw new FeedCodecError(
      `truncated message: header claims ${headerLength} bytes but only ${bytes.length - offset} remain`,
    );
  }

  const headerBytes = bytes.subarray(offset, offset + headerLength);
  offset += headerLength;

  let header: FeedHeader;
  try {
    header = JSON.parse(new TextDecoder().decode(headerBytes)) as FeedHeader;
  } catch (cause) {
    throw new FeedCodecError(`header is not valid JSON: ${(cause as Error).message}`);
  }

  if (header.protocol !== 1) {
    throw new FeedCodecError(`unsupported protocol ${String(header.protocol)}, expected 1`);
  }

  assertNoDuplicates(header.attachments ?? [], 'header.attachments');

  const attachmentMap = new Map<AttachmentKind, Uint8Array>();
  for (const kind of header.attachments ?? []) {
    const length = readU32(view, offset, bytes.length, `${kind} attachment length`);
    offset += U32_BYTES;

    if (offset + length > bytes.length) {
      throw new FeedCodecError(
        `truncated message: ${kind} attachment claims ${length} bytes but only ${bytes.length - offset} remain`,
      );
    }

    attachmentMap.set(kind, bytes.subarray(offset, offset + length));
    offset += length;
  }

  if (offset !== bytes.length) {
    throw new FeedCodecError(`trailing bytes after last attachment: ${bytes.length - offset} unexpected bytes`);
  }

  return { header, attachments: attachmentMap };
}

function readU32(view: DataView, offset: number, totalLength: number, what: string): number {
  if (offset + U32_BYTES > totalLength) {
    throw new FeedCodecError(`truncated message: not enough bytes to read ${what}`);
  }
  return view.getUint32(offset, true);
}

function assertNoDuplicates(kinds: AttachmentKind[], where: string): void {
  const seen = new Set<AttachmentKind>();
  for (const kind of kinds) {
    if (seen.has(kind)) {
      throw new FeedCodecError(`${where} lists "${kind}" more than once`);
    }
    seen.add(kind);
  }
}

function assertSameMembers(declared: AttachmentKind[], provided: AttachmentKind[]): void {
  const declaredSet = new Set(declared);
  const providedSet = new Set(provided);

  for (const kind of provided) {
    if (!declaredSet.has(kind)) {
      throw new FeedCodecError(`attachment "${kind}" was provided but is not listed in header.attachments`);
    }
  }
  for (const kind of declared) {
    if (!providedSet.has(kind)) {
      throw new FeedCodecError(`header.attachments lists "${kind}" but no such attachment was provided`);
    }
  }
}
