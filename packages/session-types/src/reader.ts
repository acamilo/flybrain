/**
 * Reading one JSON object field by field, then refusing any field that was not read.
 *
 * The Rust crate's `flybus::wire::Fields` does the same job; keeping the two shaped alike is
 * what lets the fixture corpus hold both languages to the same rules.
 */
import { ContractError, canonicalize, fail } from './canonical';
import {
  type Digest,
  type Id,
  type U64,
  type ArtifactRef,
  isDigest,
  isId,
  parseU64,
  requireU64,
} from './scalar';

export class Reader {
  private readonly map: Record<string, unknown>;
  private readonly seen = new Set<string>();

  constructor(
    value: unknown,
    private readonly what: string,
  ) {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      fail(`${what} must be an object`);
    }
    this.map = value as Record<string, unknown>;
  }

  value(key: string): unknown {
    this.seen.add(key);
    if (!Object.prototype.hasOwnProperty.call(this.map, key)) {
      fail(`${this.what}: missing field "${key}"`);
    }
    return this.map[key];
  }

  string(key: string): string {
    const value = this.value(key);
    if (typeof value !== 'string') fail(`${this.what}: ${key} must be a string`);
    return value;
  }

  id(key: string): Id {
    const value = this.string(key);
    if (!isId(value)) fail(`${this.what}: ${key} is not a valid id`);
    return value;
  }

  nullableId(key: string): Id | null {
    const value = this.value(key);
    if (value === null) return null;
    return this.id(key);
  }

  digest(key: string): Digest {
    const value = this.string(key);
    if (!isDigest(value)) fail(`${this.what}: ${key} must be 64 lowercase hex digits`);
    return value;
  }

  u64(key: string): U64 {
    return requireU64(this.value(key), `${this.what}: ${key}`);
  }

  int(key: string, low: number, high: number): number {
    const value = this.value(key);
    if (typeof value !== 'number' || !Number.isInteger(value) || value < low || value > high) {
      fail(`${this.what}: ${key} must be an integer in ${low}..=${high}`);
    }
    return value;
  }

  finite(key: string): number {
    const value = this.value(key);
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      fail(`${this.what}: ${key} must be a finite JSON number`);
    }
    if (Number.isInteger(value) && Math.abs(value) > Number.MAX_SAFE_INTEGER) {
      fail(`${this.what}: ${key} is outside the exact double range`);
    }
    return value;
  }

  finiteIn(key: string, low: number, high: number): number {
    const value = this.finite(key);
    if (value < low || value > high) fail(`${this.what}: ${key} must be in [${low}, ${high}]`);
    return value;
  }

  boolean(key: string): boolean {
    const value = this.value(key);
    if (typeof value !== 'boolean') fail(`${this.what}: ${key} must be a boolean`);
    return value;
  }

  constantTrue(key: string): true {
    if (!this.boolean(key)) fail(`${this.what}: ${key} must be true`);
    return true;
  }

  object(key: string): Record<string, unknown> {
    const value = this.value(key);
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      fail(`${this.what}: ${key} must be an object`);
    }
    return value as Record<string, unknown>;
  }

  array(key: string, low: number, high: number): unknown[] {
    const value = this.value(key);
    if (!Array.isArray(value) || value.length < low || value.length > high) {
      fail(`${this.what}: ${key} must be an array of ${low}..=${high} items`);
    }
    return value;
  }

  list<T>(key: string, low: number, high: number, read: (item: unknown) => T): T[] {
    return this.array(key, low, high).map((item, index) => {
      try {
        return read(item);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new ContractError(`${this.what}: ${key}[${index}]: ${message}`);
      }
    });
  }

  idList(key: string, low: number, high: number): Id[] {
    return this.list(key, low, high, (item) => {
      if (!isId(item)) fail('every entry must be an id');
      return item;
    });
  }

  enumeration<T extends string>(key: string, allowed: readonly T[]): T {
    const value = this.string(key);
    if (!(allowed as readonly string[]).includes(value)) {
      fail(`${this.what}: ${key} must be one of ${allowed.join(', ')}`);
    }
    return value as T;
  }

  constant<T extends string>(key: string, expected: T): T {
    const value = this.string(key);
    if (value !== expected) fail(`${this.what}: ${key} must be "${expected}"`);
    return expected;
  }

  boundedString(key: string, maxCodePoints: number): string {
    const value = this.string(key);
    if ([...value].length > maxCodePoints) {
      fail(`${this.what}: ${key} must be at most ${maxCodePoints} code points`);
    }
    return value;
  }

  nullableBoundedString(key: string, maxCodePoints: number): string | null {
    return this.value(key) === null ? null : this.boundedString(key, maxCodePoints);
  }

  /** Refuses fields that were not read. */
  finish(): void {
    for (const key of Object.keys(this.map)) {
      if (!this.seen.has(key)) fail(`${this.what}: unknown field "${key}"`);
    }
  }
}

/** Fails on the first repeated key, naming it. */
export function requireUnique(keys: readonly string[], what: string): void {
  const seen = new Set<string>();
  for (const key of keys) {
    if (seen.has(key)) fail(`${what}: duplicate "${key}"`);
    seen.add(key);
  }
}

/** Fails unless `actual` is exactly `expected`, in that order. */
export function requireSameOrder(
  actual: readonly string[],
  expected: readonly string[],
  what: string,
): void {
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    fail(
      `${what}: must list [${expected.join(', ')}] in that order, found [${actual.join(', ')}]`,
    );
  }
}

/** A bus `ArtifactRef`, read with the bus's own rules. */
export function readArtifactRef(value: unknown): ArtifactRef {
  const reader = new Reader(value, 'ArtifactRef');
  const storeId = reader.id('storeId');
  const artifactId = reader.id('artifactId');
  const generation = reader.u64('generation');
  const byteLength = reader.u64('byteLength');
  const contentType = reader.string('contentType');
  const digestValue = reader.value('digest');
  reader.finish();
  if (contentType.length < 1 || contentType.length > 127) {
    fail('ArtifactRef: contentType must be 1..=127 printable ASCII characters');
  }
  if (digestValue !== null && !isDigest(digestValue)) {
    fail('ArtifactRef: digest must be null or 64 lowercase hex digits');
  }
  return {
    storeId,
    artifactId,
    generation,
    byteLength,
    contentType,
    digest: digestValue as ArtifactRef['digest'],
  };
}

/** The canonical JSON byte length of a value. */
export function canonicalLength(value: unknown): number {
  return canonicalize(value).length;
}

/** `parseU64` that throws, for places that have already validated the string. */
export function u64(value: U64): bigint {
  const parsed = parseU64(value);
  if (parsed === undefined) fail(`${value} is not a canonical U64 string`);
  return parsed;
}
