/**
 * `seed-derivation-v1`: independent per-agent seeds from one recorded master seed.
 *
 * The specification is `docs/design/session-framework/seed-derivation-v1.md`, and
 * `fixtures/seed-vectors.json` its test vectors, which the Rust crate reproduces.
 */
import { createHash } from 'node:crypto';

import { fail } from './canonical';
import { requireUnique } from './reader';
import { isId, parseU64 } from './scalar';

export const ALGORITHM = 'seed-derivation-v1';
export const PREFIX = 'flybrain/seed-derivation-v1';

/** The exact bytes hashed: prefix, master seed and agent id, each followed by one newline. */
export function material(masterSeed: bigint, agentId: string): Uint8Array {
  if (!isId(agentId)) fail('seed derivation: agentId is not a valid id');
  if (masterSeed < 0n || masterSeed > 18446744073709551615n) {
    fail('seed derivation: the master seed is a U64');
  }
  return new TextEncoder().encode(`${PREFIX}\n${masterSeed.toString()}\n${agentId}\n`);
}

export function materialDigest(masterSeed: bigint, agentId: string): string {
  return createHash('sha256').update(material(masterSeed, agentId)).digest('hex');
}

/**
 * The signed 32-bit seed `Agent.Initialize` takes for `agentId`: the first nonzero big-endian
 * `u32` lane of the digest, as a two's-complement `i32`.
 */
export function agentSeed(masterSeed: bigint, agentId: string): number {
  let bytes = Buffer.from(material(masterSeed, agentId));
  for (let round = 0; round < 4; round += 1) {
    if (round > 0) bytes = Buffer.concat([bytes, Buffer.from(`${round}\n`, 'utf8')]);
    const digest = createHash('sha256').update(bytes).digest();
    for (let offset = 0; offset < digest.length; offset += 4) {
      const word = digest.readUInt32BE(offset);
      if (word !== 0) return word | 0;
    }
  }
  return fail('seed derivation: every lane of four digests was zero');
}

/** The seeds of a whole composition, in the order the agent ids are given. */
export function compositionSeeds(masterSeed: bigint, agentIds: readonly string[]): number[] {
  requireUnique(agentIds, 'seed derivation: agentIds');
  return agentIds.map((agentId) => agentSeed(masterSeed, agentId));
}

/** Parses a master seed from its `U64` decimal string. */
export function masterSeed(text: string): bigint {
  const parsed = parseU64(text);
  if (parsed === undefined) fail('seed derivation: the master seed is a canonical U64 string');
  return parsed;
}
