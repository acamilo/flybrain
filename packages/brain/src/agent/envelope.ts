/**
 * Checkpoint envelope: a JSON manifest plus named binary chunks, checksummed.
 *
 * A network checkpoint is a few dozen scalars and a handful of large typed arrays. JSON alone
 * would quadruple the arrays and lose their exact float bits, so the scalars go in a JSON manifest
 * and each array is carried verbatim as a length-prefixed chunk.
 *
 * Layout, which is frozen (files written by earlier versions of this format must keep loading):
 *
 * ```text
 *   magic                       ASCII, caller-chosen, identifies the file kind
 *   manifestLength              u32 little-endian
 *   manifest                    UTF-8 JSON, includes schemaVersion and the chunk name list
 *   [ length, bytes ] * n       u32 little-endian length then the chunk, in manifest order
 *   checksum                    u32 little-endian CRC32 over every preceding byte
 * ```
 *
 * The envelope validates only its own structure: magic, truncation, chunk names, trailing bytes
 * and the checksum. What the manifest *means* is the caller's business — `agentToChunks` and
 * `agentFromChunks` below are the library's own use of it, and a host is free to add its
 * environment fields to the same manifest.
 */
import type { AgentState } from './agent';
import type { DecoderState } from '../readout/decoder';

/** Envelope schema; bumped only for a layout change, not for manifest fields. */
export const ENVELOPE_SCHEMA_VERSION = 2;

/** Chunk names are restricted to letters, so a manifest can never name a prototype key. */
const CHUNK_NAME = /^[a-zA-Z]+$/;

/**
 * CRC32 (IEEE 802.3, reflected polynomial 0xedb88320) over `bytes`.
 *
 * Detects accidental corruption of the manifest and every payload byte. Not a signature: a
 * checkpoint from an untrusted source is untrusted data whatever its checksum says.
 */
function checksum(bytes: Uint8Array): number {
  let crc = -1;
  for (const byte of bytes) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1)); }
  return (crc ^ -1) >>> 0;
}

/** Manifest fields the envelope itself owns; everything else belongs to the caller. */
export interface EnvelopeManifest {
  schemaVersion: number;
  /** Chunk names in payload order. */
  chunks: string[];
  [field: string]: unknown;
}

export interface EnvelopeParts {
  manifest: EnvelopeManifest;
  chunks: Record<string, Uint8Array>;
}

/**
 * Encode `manifest` and `chunks` into one checksummed buffer. `magic` must be ASCII; the chunk
 * name list and `schemaVersion` are written into the manifest, overriding any same-named fields.
 */
export function encodeEnvelope(magic: string, manifest: object, chunks: Record<string, Uint8Array>): ArrayBuffer {
  const magicBytes = new TextEncoder().encode(magic);
  if (magicBytes.length === 0 || magicBytes.length !== magic.length) throw new Error('Envelope magic must be a non-empty ASCII string');
  const names = Object.keys(chunks);
  if (names.some(name => !CHUNK_NAME.test(name))) throw new Error('Invalid envelope chunk name');
  const manifestBytes = new TextEncoder().encode(JSON.stringify({ ...manifest, schemaVersion: ENVELOPE_SCHEMA_VERSION, chunks: names }));
  const total = magicBytes.length + 8 + manifestBytes.length + names.reduce((sum, name) => sum + 4 + chunks[name]!.byteLength, 0);
  const output = new Uint8Array(total);
  const view = new DataView(output.buffer);
  let offset = 0;
  output.set(magicBytes, offset); offset += magicBytes.length;
  view.setUint32(offset, manifestBytes.length, true); offset += 4;
  output.set(manifestBytes, offset); offset += manifestBytes.length;
  for (const name of names) {
    view.setUint32(offset, chunks[name]!.byteLength, true); offset += 4;
    output.set(chunks[name]!, offset); offset += chunks[name]!.byteLength;
  }
  view.setUint32(offset, checksum(output.subarray(0, offset)), true);
  return output.buffer;
}

/**
 * Decode a buffer written by {@link encodeEnvelope}, rejecting anything that is not structurally
 * this format with `magic`. Chunks are copies, so the returned arrays own their buffers.
 */
export function decodeEnvelope(buffer: ArrayBuffer, magic: string): EnvelopeParts {
  const magicBytes = new TextEncoder().encode(magic);
  const bytes = new Uint8Array(buffer);
  if (bytes.length < magicBytes.length + 4 || !magicBytes.every((value, index) => bytes[index] === value)) throw new Error(`Not a ${magic} envelope`);
  const view = new DataView(buffer);
  let offset = magicBytes.length;
  const manifestLength = view.getUint32(offset, true); offset += 4;
  if (offset + manifestLength > bytes.length) throw new Error('Envelope manifest is truncated');
  const manifest = JSON.parse(new TextDecoder().decode(bytes.subarray(offset, offset + manifestLength))) as EnvelopeManifest;
  offset += manifestLength;
  if (manifest?.schemaVersion !== ENVELOPE_SCHEMA_VERSION) throw new Error(`Unsupported envelope schema: ${manifest?.schemaVersion}`);
  if (bytes.length < 4 || checksum(bytes.subarray(0, -4)) !== view.getUint32(bytes.length - 4, true)) throw new Error('Envelope checksum mismatch');
  if (!Array.isArray(manifest.chunks) || new Set(manifest.chunks).size !== manifest.chunks.length || manifest.chunks.some(name => typeof name !== 'string' || !CHUNK_NAME.test(name))) throw new Error('Invalid envelope chunks');
  const chunks: Record<string, Uint8Array> = Object.create(null);
  for (const name of manifest.chunks) {
    if (offset + 4 > bytes.length) throw new Error('Envelope chunk header is truncated');
    const length = view.getUint32(offset, true); offset += 4;
    if (offset + length > bytes.length) throw new Error(`Envelope chunk ${name} is truncated`);
    chunks[name] = bytes.slice(offset, offset + length);
    offset += length;
  }
  if (offset !== bytes.length - 4) throw new Error('Envelope contains trailing data');
  return { manifest, chunks };
}

/** Scalar half of an {@link AgentState}: everything that is not a typed array. */
export interface AgentManifest {
  agentVersion: 1;
  remainder: number;
  warmedUp: boolean;
  network: {
    rng: number;
    rewardRemaining: number;
    ms: number;
    populationRate: number;
    rates: Record<string, number>;
  };
  decoder: DecoderState;
  plasticity: { version: string; topology: number; enabled: boolean; updates: number; signal: number };
}

/** Chunk names of the typed arrays in an agent checkpoint, in write order. */
export const AGENT_CHUNK_NAMES = [
  'membrane', 'refractory', 'lastSpikeMs', 'visualDrive', 'plasticGains', 'plasticTraces', 'plasticTouched',
] as const;

/** Byte view of a typed array, without copying. */
function raw(array: ArrayBufferView): Uint8Array {
  return new Uint8Array(array.buffer as ArrayBuffer, array.byteOffset, array.byteLength);
}

/** Zero-offset copy, so `new Float32Array(chunk.buffer)` is aligned and exactly the right length. */
function aligned(chunk: Uint8Array, name: string, elementSize: number): ArrayBuffer {
  if (chunk.byteLength % elementSize !== 0) throw new Error(`Checkpoint chunk ${name} has a partial element`);
  return chunk.slice().buffer as ArrayBuffer;
}

/**
 * Split an agent state into the manifest and chunks {@link encodeEnvelope} takes. Chunk names are
 * the original checkpoint's, so a host that already writes those names keeps its file format.
 */
export function agentToChunks(state: AgentState): { manifest: AgentManifest; chunks: Record<string, Uint8Array> } {
  const { network } = state;
  return {
    manifest: {
      agentVersion: 1,
      remainder: state.remainder,
      warmedUp: state.warmedUp,
      network: {
        rng: network.rng,
        rewardRemaining: network.rewardRemaining,
        ms: network.ms,
        populationRate: network.populationRate,
        rates: { ...network.rates },
      },
      decoder: state.decoder,
      plasticity: {
        version: network.plasticity.version,
        topology: network.plasticity.topology,
        enabled: network.plasticity.enabled,
        updates: network.plasticity.updates,
        signal: network.plasticity.signal,
      },
    },
    chunks: {
      membrane: raw(network.membrane),
      refractory: raw(network.refractory),
      lastSpikeMs: raw(network.lastSpikeMs),
      visualDrive: raw(network.visualDrive),
      plasticGains: raw(network.plasticity.gains),
      plasticTraces: raw(network.plasticity.traces),
      plasticTouched: raw(network.plasticity.touched),
    },
  };
}

/**
 * Rebuild an agent state from a decoded envelope. Only presence, alignment and the manifest shape
 * are checked here; the values themselves are validated by `NeuralAgent.importState`, which is
 * also what makes a rejected checkpoint a no-op.
 */
export function agentFromChunks(manifest: AgentManifest, chunks: Record<string, Uint8Array>): AgentState {
  if (!manifest || manifest.agentVersion !== 1) throw new Error('Unsupported agent checkpoint version');
  if (!manifest.network || !manifest.decoder || !manifest.plasticity) throw new Error('Agent checkpoint manifest is incomplete');
  for (const name of AGENT_CHUNK_NAMES) if (!chunks[name]) throw new Error(`Checkpoint is missing ${name}`);
  return {
    version: 1,
    remainder: manifest.remainder,
    warmedUp: manifest.warmedUp,
    network: {
      membrane: new Float32Array(aligned(chunks.membrane!, 'membrane', 4)),
      refractory: new Uint8Array(aligned(chunks.refractory!, 'refractory', 1)),
      lastSpikeMs: new Float64Array(aligned(chunks.lastSpikeMs!, 'lastSpikeMs', 8)),
      visualDrive: new Float32Array(aligned(chunks.visualDrive!, 'visualDrive', 4)),
      rng: manifest.network.rng,
      rewardRemaining: manifest.network.rewardRemaining,
      ms: manifest.network.ms,
      populationRate: manifest.network.populationRate,
      rates: { ...manifest.network.rates },
      plasticity: {
        ...manifest.plasticity,
        gains: new Float32Array(aligned(chunks.plasticGains!, 'plasticGains', 4)),
        traces: new Float32Array(aligned(chunks.plasticTraces!, 'plasticTraces', 4)),
        touched: new Float64Array(aligned(chunks.plasticTouched!, 'plasticTouched', 8)),
      },
    },
    decoder: manifest.decoder,
  };
}
