/**
 * The domain scalars of ipc-v1 section 2, and the four identities that must never be confused.
 *
 * `Id`, `U64` and `Digest` are the bus encodings (bus-v1 section 3 defers to ipc-v1 for them),
 * and `tests/encodings.test.ts` pins the same edge cases the Rust crate pins.
 */
import { fail } from './canonical';

/** `^[a-z0-9][a-z0-9._-]{0,63}$`. */
export type Id = string;
/** `"0"` or `[1-9][0-9]*`, at most 18446744073709551615. A counter, never a JSON number. */
export type U64 = string;
/** 64 lowercase hexadecimal digits (SHA-256). */
export type Digest = string;

const ID = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const U64_TEXT = /^(0|[1-9][0-9]*)$/;
const DIGEST = /^[0-9a-f]{64}$/;
/** The largest U64, as a bigint. */
export const U64_MAX = 18446744073709551615n;

export function isId(value: unknown): value is Id {
  return typeof value === 'string' && ID.test(value);
}

export function isDigest(value: unknown): value is Digest {
  return typeof value === 'string' && DIGEST.test(value);
}

/** The value a `U64` string denotes, or `undefined` if it is not canonical. */
export function parseU64(value: unknown): bigint | undefined {
  if (typeof value !== 'string' || !U64_TEXT.test(value)) return undefined;
  const parsed = BigInt(value);
  return parsed <= U64_MAX ? parsed : undefined;
}

export function requireU64(value: unknown, what: string): U64 {
  if (parseU64(value) === undefined) fail(`${what} is not a canonical U64 string`);
  return value as U64;
}

/** RPC method: 1..=128 printable ASCII characters (bus-v1 section 5). */
export function isMethod(value: unknown): boolean {
  return (
    typeof value === 'string' &&
    value.length >= 1 &&
    value.length <= 128 &&
    [...value].every((character) => {
      const point = character.codePointAt(0) ?? 0;
      return point >= 0x20 && point <= 0x7e;
    })
  );
}

// ---------------------------------------------------------------------------------------------
// The four identities

declare const brand: unique symbol;
/** A branded string: assignable only through its own parser. */
type Branded<Name extends string> = string & { readonly [brand]: Name };

/** A bus RPC correlation id, `call-<U64>`. Not a domain operation id. */
export type BusCallId = Branded<'BusCallId'>;
/** A domain operation id, `req-<U64>`. A safe retry keeps it and gets a new `BusCallId`. */
export type DomainRequestId = Branded<'DomainRequestId'>;
/** A delivery (`dlv-<U64>`) or explicit-hold (`own-<U64>`) owner token. Connection-private. */
export type OwnerToken = Branded<'OwnerToken'>;

export type OwnerKind = 'delivery' | 'hold';

function serial(prefix: string, value: unknown): bigint | undefined {
  if (typeof value !== 'string' || !value.startsWith(`${prefix}-`)) return undefined;
  return parseU64(value.slice(prefix.length + 1));
}

export function isBusCallId(value: unknown): value is BusCallId {
  return serial('call', value) !== undefined;
}

export function isDomainRequestId(value: unknown): value is DomainRequestId {
  return serial('req', value) !== undefined;
}

export function ownerTokenKind(value: unknown): OwnerKind | undefined {
  if (serial('dlv', value) !== undefined) return 'delivery';
  if (serial('own', value) !== undefined) return 'hold';
  return undefined;
}

export function busCallId(value: unknown): BusCallId {
  if (!isBusCallId(value)) fail('a bus callId must be canonical call-<U64>');
  return value;
}

export function domainRequestId(value: unknown): DomainRequestId {
  if (!isDomainRequestId(value)) fail('a domain requestId must be canonical req-<U64>');
  return value;
}

export function ownerToken(value: unknown): OwnerToken {
  if (ownerTokenKind(value) === undefined) {
    fail('an owner token must be canonical dlv-<U64> or own-<U64>');
  }
  return value as OwnerToken;
}

/** The naming half of a bus `ArtifactRef`: what identifies the bytes. */
export interface ArtifactIdentity {
  storeId: Id;
  artifactId: Id;
  generation: U64;
}

/** A transient bus artifact reference (bus-v1 section 4). Never an `AssetRef`. */
export interface ArtifactRef {
  storeId: Id;
  artifactId: Id;
  generation: U64;
  byteLength: U64;
  contentType: string;
  digest: Digest | null;
}

export function artifactIdentity(reference: ArtifactRef): ArtifactIdentity {
  return {
    storeId: reference.storeId,
    artifactId: reference.artifactId,
    generation: reference.generation,
  };
}

// ---------------------------------------------------------------------------------------------
// RationalNs

/** A nanosecond rational: reduced, positive denominator, zero encoded `0/1`. */
export interface RationalNs {
  numerator: U64;
  denominator: U64;
}

export const RATIONAL_ZERO: RationalNs = { numerator: '0', denominator: '1' };

function gcd(a: bigint, b: bigint): bigint {
  let left = a;
  let right = b;
  while (right !== 0n) {
    const rest = left % right;
    left = right;
    right = rest;
  }
  return left;
}

function parts(value: RationalNs, what: string): [bigint, bigint] {
  const numerator = parseU64(value.numerator);
  const denominator = parseU64(value.denominator);
  if (numerator === undefined || denominator === undefined) {
    fail(`${what}: numerator and denominator are U64 strings`);
  }
  return [numerator, denominator];
}

/** The canonical-form rules: positive denominator, `0/1` zero, reduced fraction. */
export function validateRational(value: RationalNs, what = 'RationalNs'): void {
  const [numerator, denominator] = parts(value, what);
  if (denominator === 0n) fail(`${what}: denominator must be positive`);
  if (numerator === 0n && denominator !== 1n) fail(`${what}: zero is encoded 0/1`);
  if (numerator !== 0n && gcd(numerator, denominator) !== 1n) {
    fail(`${what}: fraction must be reduced`);
  }
}

/** Reduces, then validates: the constructor for arithmetic results. */
export function reduced(numerator: bigint, denominator: bigint): RationalNs {
  if (denominator <= 0n) fail('RationalNs: denominator must be positive');
  let n = numerator;
  let d = denominator;
  if (n === 0n) {
    d = 1n;
  } else {
    const divisor = gcd(n, d);
    n /= divisor;
    d /= divisor;
  }
  if (n > U64_MAX || d > U64_MAX) fail('RationalNs: reduced value does not fit U64');
  return { numerator: n.toString(), denominator: d.toString() };
}

export function isRationalZero(value: RationalNs): boolean {
  return parseU64(value.numerator) === 0n;
}

export function requirePositiveRational(value: RationalNs, what: string): void {
  if (isRationalZero(value)) fail(`${what}: duration must be positive`);
}

export function addRational(left: RationalNs, right: RationalNs): RationalNs {
  const [ln, ld] = parts(left, 'RationalNs');
  const [rn, rd] = parts(right, 'RationalNs');
  return reduced(ln * rd + rn * ld, ld * rd);
}

export function subtractRational(left: RationalNs, right: RationalNs): RationalNs {
  const [ln, ld] = parts(left, 'RationalNs');
  const [rn, rd] = parts(right, 'RationalNs');
  const a = ln * rd;
  const b = rn * ld;
  if (b > a) fail('RationalNs: subtraction would be negative');
  return reduced(a - b, ld * rd);
}

export function multiplyRational(value: RationalNs, factor: bigint): RationalNs {
  const [numerator, denominator] = parts(value, 'RationalNs');
  return reduced(numerator * factor, denominator);
}

export function compareRational(left: RationalNs, right: RationalNs): -1 | 0 | 1 {
  const [ln, ld] = parts(left, 'RationalNs');
  const [rn, rd] = parts(right, 'RationalNs');
  const a = ln * rd;
  const b = rn * ld;
  return a < b ? -1 : a > b ? 1 : 0;
}

/**
 * The step-v1 section 5 accumulator: `ticks = floor(value / tick)` and the remainder
 * `value - ticks * tick`, which is always `>= 0` and `< tick`.
 */
export function divideFloor(value: RationalNs, tick: RationalNs): { ticks: U64; remainder: RationalNs } {
  requirePositiveRational(tick, 'RationalNs divideFloor tick');
  const [vn, vd] = parts(value, 'RationalNs');
  const [tn, td] = parts(tick, 'RationalNs');
  const ticks = (vn * td) / (vd * tn);
  if (ticks > U64_MAX) fail('RationalNs: tick count does not fit U64');
  const remainder = subtractRational(value, multiplyRational(tick, ticks));
  return { ticks: ticks.toString(), remainder };
}

// ---------------------------------------------------------------------------------------------
// Scope, SchemaRef, TypedValue

/** The simulation timeline identity. Never a bus route or store incarnation. */
export interface Scope {
  sessionId: Id;
  epoch: Id;
  step: U64;
}

export interface SchemaRef {
  id: Id;
  version: number;
  digest: Digest;
}

/** A schema identity plus an object, capped at 32 KiB of canonical JSON. */
export interface TypedValue {
  schema: SchemaRef;
  value: Record<string, unknown>;
}

/** The canonical-JSON size limit of one `TypedValue`. */
export const MAX_TYPED_VALUE_BYTES = 32 * 1024;
