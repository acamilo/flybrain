/**
 * `FLYSESS1`: the envelope layout of
 * `docs/design/session-framework/checkpoint-envelope-v1.md`.
 *
 * The layout half of the specification, not the store: writing generations, fsyncing and
 * committing a manifest belong to the STATE-01 slice. `FLYSIM01` is a different format with a
 * different magic and is not touched by any of this.
 */
import { createHash } from 'node:crypto';

import { type Json, canonicalize, fail, parseStrict } from './canonical';
import { requireUnique } from './reader';
import { isId } from './scalar';

export const MAGIC = 'FLYSESS1';
export const FOOTER_MAGIC = 'FLYSESSF';
export const VERSION = 1;
export const HEADER_BYTES = 32;
export const TABLE_ENTRY_BYTES = 112;
export const NAME_BYTES = 64;
export const FOOTER_BYTES = 48;
export const ALIGNMENT = 8;
export const MAX_PAYLOADS = 64;

export interface PayloadEntry {
  name: string;
  offset: number;
  byteLength: number;
  digest: string;
}

export interface Layout {
  manifestOffset: number;
  manifestBytes: number;
  tableOffset: number;
  entries: PayloadEntry[];
  footerOffset: number;
  totalBytes: number;
}

export interface Envelope {
  manifest: Json;
  payloads: { name: string; bytes: Uint8Array }[];
  layout: Layout;
}

function alignUp(value: number): number {
  return Math.ceil(value / ALIGNMENT) * ALIGNMENT;
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

export function layoutOf(
  manifest: unknown,
  payloads: readonly { name: string; bytes: Uint8Array }[],
): Layout {
  if (payloads.length > MAX_PAYLOADS) fail('checkpoint envelope: at most 64 payloads');
  requireUnique(
    payloads.map((payload) => payload.name),
    'checkpoint envelope: payload names',
  );
  for (const payload of payloads) {
    if (!isId(payload.name)) {
      fail(`checkpoint envelope: payload name "${payload.name}" is not an Id`);
    }
  }
  const manifestBytes = new TextEncoder().encode(canonicalize(manifest)).length;
  const manifestOffset = HEADER_BYTES;
  const tableOffset = alignUp(manifestOffset + manifestBytes);
  let offset = alignUp(tableOffset + payloads.length * TABLE_ENTRY_BYTES);
  const entries: PayloadEntry[] = [];
  for (const payload of payloads) {
    entries.push({
      name: payload.name,
      offset,
      byteLength: payload.bytes.length,
      digest: sha256(payload.bytes),
    });
    offset = alignUp(offset + payload.bytes.length);
  }
  return {
    manifestOffset,
    manifestBytes,
    tableOffset,
    entries,
    footerOffset: offset,
    totalBytes: offset + FOOTER_BYTES,
  };
}

export function encode(
  manifest: unknown,
  payloads: readonly { name: string; bytes: Uint8Array }[],
): Uint8Array {
  const layout = layoutOf(manifest, payloads);
  const manifestText = new TextEncoder().encode(canonicalize(manifest));
  const out = Buffer.alloc(layout.footerOffset);
  out.write(MAGIC, 0, 'ascii');
  out.writeUInt32LE(VERSION, 8);
  out.writeUInt32LE(HEADER_BYTES, 12);
  out.writeUInt32LE(layout.manifestBytes, 16);
  out.writeUInt32LE(payloads.length, 20);
  out.writeUInt32LE(layout.tableOffset, 24);
  out.writeUInt32LE(0, 28);
  Buffer.from(manifestText).copy(out, layout.manifestOffset);
  layout.entries.forEach((entry, index) => {
    const base = layout.tableOffset + index * TABLE_ENTRY_BYTES;
    out.write(entry.name, base, 'ascii');
    out.writeBigUInt64LE(BigInt(entry.offset), base + NAME_BYTES);
    out.writeBigUInt64LE(BigInt(entry.byteLength), base + NAME_BYTES + 8);
    Buffer.from(entry.digest, 'hex').copy(out, base + NAME_BYTES + 16);
  });
  layout.entries.forEach((entry, index) => {
    Buffer.from((payloads[index] as { bytes: Uint8Array }).bytes).copy(out, entry.offset);
  });
  const footer = Buffer.alloc(FOOTER_BYTES);
  footer.writeBigUInt64LE(BigInt(layout.totalBytes), 0);
  Buffer.from(sha256(out), 'hex').copy(footer, 8);
  footer.write(FOOTER_MAGIC, 40, 'ascii');
  return Buffer.concat([out, footer]);
}

/** Reads and fully validates one envelope. */
export function decode(input: Uint8Array): Envelope {
  const bytes = Buffer.from(input);
  if (bytes.length < HEADER_BYTES + FOOTER_BYTES) {
    fail('checkpoint envelope: shorter than a header plus a footer');
  }
  if (bytes.subarray(0, 8).toString('ascii') !== MAGIC) {
    fail('checkpoint envelope: wrong magic (FLYSIM01 is a different format)');
  }
  if (bytes.readUInt32LE(8) !== VERSION) fail('checkpoint envelope: unsupported version');
  if (bytes.readUInt32LE(12) !== HEADER_BYTES) {
    fail('checkpoint envelope: headerBytes must be 32');
  }
  if (bytes.readUInt32LE(28) !== 0) {
    fail('checkpoint envelope: reserved header word must be zero');
  }
  const manifestBytes = bytes.readUInt32LE(16);
  const payloadCount = bytes.readUInt32LE(20);
  const tableOffset = bytes.readUInt32LE(24);
  if (payloadCount > MAX_PAYLOADS) fail('checkpoint envelope: at most 64 payloads');
  const footerOffset = bytes.length - FOOTER_BYTES;
  if (bytes.subarray(footerOffset + 40).toString('ascii') !== FOOTER_MAGIC) {
    fail('checkpoint envelope: missing footer magic');
  }
  if (bytes.readBigUInt64LE(footerOffset) !== BigInt(bytes.length)) {
    fail('checkpoint envelope: footer length does not match the file');
  }
  const recorded = bytes.subarray(footerOffset + 8, footerOffset + 40).toString('hex');
  if (recorded !== sha256(bytes.subarray(0, footerOffset))) {
    fail('checkpoint envelope: footer digest does not match the contents');
  }
  const manifestEnd = HEADER_BYTES + manifestBytes;
  if (manifestEnd > footerOffset) {
    fail('checkpoint envelope: manifest runs past the payload area');
  }
  const manifestSlice = bytes.subarray(HEADER_BYTES, manifestEnd);
  const manifest = parseStrict(manifestSlice);
  if (canonicalize(manifest) !== manifestSlice.toString('utf8')) {
    fail('checkpoint envelope: the manifest is not canonical JSON');
  }
  if (tableOffset !== alignUp(manifestEnd)) {
    fail('checkpoint envelope: the payload table is not at its laid-out offset');
  }
  const tableEnd = tableOffset + payloadCount * TABLE_ENTRY_BYTES;
  if (tableEnd > footerOffset) {
    fail('checkpoint envelope: the payload table runs past the payload area');
  }
  const entries: PayloadEntry[] = [];
  const payloads: { name: string; bytes: Uint8Array }[] = [];
  let previousEnd = alignUp(tableEnd);
  for (let index = 0; index < payloadCount; index += 1) {
    const base = tableOffset + index * TABLE_ENTRY_BYTES;
    const nameField = bytes.subarray(base, base + NAME_BYTES);
    const terminator = nameField.indexOf(0);
    const length = terminator === -1 ? NAME_BYTES : terminator;
    if (nameField.subarray(length).some((byte) => byte !== 0)) {
      fail('checkpoint envelope: a payload name has bytes after its terminator');
    }
    const name = nameField.subarray(0, length).toString('utf8');
    if (!isId(name)) fail(`checkpoint envelope: payload name "${name}" is not an Id`);
    const offset = Number(bytes.readBigUInt64LE(base + NAME_BYTES));
    const byteLength = Number(bytes.readBigUInt64LE(base + NAME_BYTES + 8));
    const digest = bytes.subarray(base + NAME_BYTES + 16, base + NAME_BYTES + 48).toString('hex');
    if (offset !== previousEnd) {
      fail(
        `checkpoint envelope: payload "${name}" starts at ${offset}, not at its aligned ${previousEnd}`,
      );
    }
    const end = offset + byteLength;
    if (end > footerOffset) {
      fail(`checkpoint envelope: payload "${name}" runs past the payload area`);
    }
    const payload = bytes.subarray(offset, end);
    if (sha256(payload) !== digest) {
      fail(`checkpoint envelope: payload "${name}" fails its digest`);
    }
    previousEnd = alignUp(end);
    entries.push({ name, offset, byteLength, digest });
    payloads.push({ name, bytes: Uint8Array.from(payload) });
  }
  requireUnique(
    entries.map((entry) => entry.name),
    'checkpoint envelope: payload names',
  );
  if (previousEnd !== footerOffset) {
    fail('checkpoint envelope: padding between the last payload and the footer');
  }
  return {
    manifest,
    payloads,
    layout: {
      manifestOffset: HEADER_BYTES,
      manifestBytes,
      tableOffset,
      entries,
      footerOffset,
      totalBytes: bytes.length,
    },
  };
}

/** The manifest fields state-media-v1 section 4 requires. */
export const REQUIRED_MANIFEST_FIELDS = [
  'envelopeVersion',
  'checkpointId',
  'sourceScope',
  'episodeId',
  'worldTime',
  'schedulerId',
  'compositionDigest',
  'portMap',
  'compatibility',
  'agents',
  'coordinator',
  'payloads',
] as const;

/** Checks the required field set and that the manifest's payload table mirrors the envelope's. */
export function validateManifest(envelope: Envelope): void {
  const manifest = envelope.manifest;
  if (manifest === null || typeof manifest !== 'object' || Array.isArray(manifest)) {
    fail('checkpoint manifest: must be an object');
  }
  const map = manifest as Record<string, Json>;
  for (const field of REQUIRED_MANIFEST_FIELDS) {
    if (!Object.prototype.hasOwnProperty.call(map, field)) {
      fail(`checkpoint manifest: missing "${field}"`);
    }
  }
  if (map.envelopeVersion !== VERSION) fail('checkpoint manifest: envelopeVersion must be 1');
  const listed = map.payloads;
  if (!Array.isArray(listed)) fail('checkpoint manifest: payloads must be an array');
  if (listed.length !== envelope.layout.entries.length) {
    fail('checkpoint manifest: payloads does not match the payload table');
  }
  listed.forEach((declared, index) => {
    const entry = envelope.layout.entries[index] as PayloadEntry;
    const record = declared as Record<string, Json>;
    if (record.name !== entry.name) {
      fail('checkpoint manifest: payload name does not match the table');
    }
    if (record.byteLength !== String(entry.byteLength)) {
      fail(`checkpoint manifest: payload "${entry.name}" byteLength does not match the table`);
    }
    if (record.digest !== entry.digest) {
      fail(`checkpoint manifest: payload "${entry.name}" digest does not match the table`);
    }
  });
}
