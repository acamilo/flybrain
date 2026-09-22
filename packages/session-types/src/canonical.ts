/**
 * Canonical JSON (RFC 8785), strict parsing and canonical digests.
 *
 * The Rust crate `services/flysim/crates/fly-session-types` is the other half of this
 * contract; `fixtures/valid.json` records the canonical bytes and digest of every accepted
 * payload, and both languages assert against it.
 *
 * Three rules make the two agree:
 *
 * - object keys sort by UTF-16 code unit, which is what comparing JavaScript strings does;
 * - numbers print with `String(number)`, the ECMAScript algorithm RFC 8785 requires;
 * - a number is canonicalizable when it is finite and, if integral, no larger in magnitude
 *   than `Number.MAX_SAFE_INTEGER`. Larger integers are refused rather than rounded: every
 *   counter and clock in these contracts is a `U64` decimal string. The rule is on the value,
 *   not on how it was written, because `JSON.parse` cannot tell `1e21` from the same digits
 *   written out.
 */
import { createHash } from 'node:crypto';

/** The largest JSON envelope, in bytes (bus-v1 section 4). */
export const MAX_ENVELOPE_BYTES = 65_536;

/** Thrown by everything in this package. One error type, like the bus's `WireError`. */
export class ContractError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ContractError';
  }
}

export function fail(message: string): never {
  throw new ContractError(message);
}

/** A JSON value, as strictly parsed. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** `String(number)` for a canonicalizable number. */
function numberToString(value: number): string {
  if (!Number.isFinite(value)) {
    fail(`canonical JSON: ${String(value)} is not a finite number`);
  }
  if (Number.isInteger(value) && Math.abs(value) > Number.MAX_SAFE_INTEGER) {
    fail(`canonical JSON: ${String(value)} is an integral value outside the exact double range`);
  }
  // String(-0) is already "0".
  return String(value);
}

function writeString(out: string[], value: string): void {
  out.push('"');
  for (const character of value) {
    switch (character) {
      case '"':
        out.push('\\"');
        break;
      case '\\':
        out.push('\\\\');
        break;
      case '\b':
        out.push('\\b');
        break;
      case '\t':
        out.push('\\t');
        break;
      case '\n':
        out.push('\\n');
        break;
      case '\f':
        out.push('\\f');
        break;
      case '\r':
        out.push('\\r');
        break;
      default: {
        const point = character.codePointAt(0) ?? 0;
        if (point < 0x20) {
          out.push(`\\u${point.toString(16).padStart(4, '0')}`);
        } else {
          out.push(character);
        }
      }
    }
  }
  out.push('"');
}

function write(out: string[], value: unknown): void {
  if (value === null) {
    out.push('null');
    return;
  }
  switch (typeof value) {
    case 'boolean':
      out.push(value ? 'true' : 'false');
      return;
    case 'number':
      out.push(numberToString(value));
      return;
    case 'string':
      writeString(out, value);
      return;
    case 'object':
      break;
    default:
      fail(`canonical JSON: ${typeof value} is not a JSON value`);
  }
  if (Array.isArray(value)) {
    out.push('[');
    value.forEach((item, index) => {
      if (index > 0) out.push(',');
      write(out, item);
    });
    out.push(']');
    return;
  }
  const entries = Object.entries(value as Record<string, unknown>);
  for (const [key, item] of entries) {
    if (item === undefined) fail(`canonical JSON: ${key} is undefined, which is not a JSON value`);
  }
  // Comparing JavaScript strings compares UTF-16 code units, which is the order RFC 8785
  // section 3.2.3 specifies.
  entries.sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  out.push('{');
  entries.forEach(([key, item], index) => {
    if (index > 0) out.push(',');
    writeString(out, key);
    out.push(':');
    write(out, item);
  });
  out.push('}');
}

/** The canonical JSON text of `value`. */
export function canonicalize(value: unknown): string {
  const out: string[] = [];
  write(out, value);
  return out.join('');
}

/** Lowercase hex SHA-256 of `bytes`. */
export function sha256Hex(bytes: Uint8Array | string): string {
  return createHash('sha256')
    .update(typeof bytes === 'string' ? Buffer.from(bytes, 'utf8') : bytes)
    .digest('hex');
}

/** The canonical digest of a JSON value: SHA-256 over its canonical JSON bytes. */
export function digestOf(value: unknown): string {
  return sha256Hex(canonicalize(value));
}

/**
 * Parses JSON strictly: duplicate keys at any depth, invalid UTF-8, `NaN`/`Infinity`,
 * trailing bytes and control characters inside strings are all refused.
 *
 * `JSON.parse` keeps the last of two duplicate keys instead of failing, so this is a small
 * recursive-descent parser rather than a wrapper around it.
 */
export function parseStrict(input: Uint8Array | string): Json {
  let text: string;
  if (typeof input === 'string') {
    text = input;
  } else {
    try {
      text = new TextDecoder('utf-8', { fatal: true }).decode(input);
    } catch {
      return fail('invalid UTF-8');
    }
  }
  const parser = new Parser(text);
  const value = parser.value();
  parser.skipWhitespace();
  if (!parser.atEnd()) fail('invalid JSON: trailing data');
  return value;
}

class Parser {
  private index = 0;

  constructor(private readonly text: string) {}

  atEnd(): boolean {
    return this.index >= this.text.length;
  }

  skipWhitespace(): void {
    while (this.index < this.text.length && ' \t\n\r'.includes(this.text[this.index] as string)) {
      this.index += 1;
    }
  }

  value(): Json {
    this.skipWhitespace();
    const character = this.text[this.index];
    if (character === undefined) fail('invalid JSON: unexpected end of input');
    switch (character) {
      case '{':
        return this.object();
      case '[':
        return this.array();
      case '"':
        return this.string();
      case 't':
        this.literal('true');
        return true;
      case 'f':
        this.literal('false');
        return false;
      case 'n':
        this.literal('null');
        return null;
      default:
        return this.number();
    }
  }

  private literal(word: string): void {
    if (!this.text.startsWith(word, this.index)) fail(`invalid JSON: expected ${word}`);
    this.index += word.length;
  }

  private object(): Json {
    this.index += 1;
    const out: { [key: string]: Json } = {};
    this.skipWhitespace();
    if (this.text[this.index] === '}') {
      this.index += 1;
      return out;
    }
    for (;;) {
      this.skipWhitespace();
      if (this.text[this.index] !== '"') fail('invalid JSON: expected a key');
      const key = this.string();
      if (Object.prototype.hasOwnProperty.call(out, key)) {
        fail(`invalid JSON: duplicate key ${JSON.stringify(key)}`);
      }
      this.skipWhitespace();
      if (this.text[this.index] !== ':') fail('invalid JSON: expected :');
      this.index += 1;
      out[key] = this.value();
      this.skipWhitespace();
      const next = this.text[this.index];
      if (next === ',') {
        this.index += 1;
        continue;
      }
      if (next === '}') {
        this.index += 1;
        return out;
      }
      fail('invalid JSON: expected , or }');
    }
  }

  private array(): Json {
    this.index += 1;
    const out: Json[] = [];
    this.skipWhitespace();
    if (this.text[this.index] === ']') {
      this.index += 1;
      return out;
    }
    for (;;) {
      out.push(this.value());
      this.skipWhitespace();
      const next = this.text[this.index];
      if (next === ',') {
        this.index += 1;
        continue;
      }
      if (next === ']') {
        this.index += 1;
        return out;
      }
      fail('invalid JSON: expected , or ]');
    }
  }

  private string(): string {
    this.index += 1;
    let out = '';
    for (;;) {
      const character = this.text[this.index];
      if (character === undefined) fail('invalid JSON: unterminated string');
      this.index += 1;
      if (character === '"') return out;
      if (character === '\\') {
        const escape = this.text[this.index];
        this.index += 1;
        switch (escape) {
          case '"':
          case '\\':
          case '/':
            out += escape;
            break;
          case 'b':
            out += '\b';
            break;
          case 'f':
            out += '\f';
            break;
          case 'n':
            out += '\n';
            break;
          case 'r':
            out += '\r';
            break;
          case 't':
            out += '\t';
            break;
          case 'u': {
            const hex = this.text.slice(this.index, this.index + 4);
            if (!/^[0-9a-fA-F]{4}$/.test(hex)) fail('invalid JSON: bad \\u escape');
            out += String.fromCharCode(Number.parseInt(hex, 16));
            this.index += 4;
            break;
          }
          default:
            fail('invalid JSON: bad escape');
        }
        continue;
      }
      if (character.charCodeAt(0) < 0x20) fail('invalid JSON: control character in a string');
      out += character;
    }
  }

  private number(): number {
    const match = /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?/.exec(
      this.text.slice(this.index),
    );
    if (!match) fail('invalid JSON: expected a value');
    this.index += match[0].length;
    const value = Number(match[0]);
    if (!Number.isFinite(value)) fail('invalid JSON: non-finite number');
    return value;
  }
}

/**
 * Refuses a domain payload that does not fit the bus envelope ceiling. `envelopeOverhead` is
 * what the surrounding envelope adds, so a payload that only fits without its envelope fails.
 */
export function requireEnvelopeFit(value: unknown, envelopeOverhead: number): number {
  const total = canonicalize(value).length + envelopeOverhead;
  if (total > MAX_ENVELOPE_BYTES) {
    fail(`envelope: ${total} bytes exceeds the ${MAX_ENVELOPE_BYTES}-byte maximum`);
  }
  return total;
}

/** Keys that belong to the bus and never to a domain body (ipc-v1 section 5). */
export const BUS_ONLY_KEYS = [
  'callId',
  'deliveryId',
  'ownerId',
  'ownerIds',
  'deliveryIds',
  'requestDeliveryId',
  'expectedIncarnation',
  'serviceIncarnation',
  'connectionId',
  'topicSequence',
  'subscriptionId',
] as const;

/** Fails if any bus-only key appears anywhere in `value`. */
export function rejectBusIdentities(value: unknown): void {
  if (Array.isArray(value)) {
    for (const item of value) rejectBusIdentities(item);
    return;
  }
  if (value === null || typeof value !== 'object') return;
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    if ((BUS_ONLY_KEYS as readonly string[]).includes(key)) {
      fail(`canonical body: "${key}" is a bus identity and never part of a domain body`);
    }
    rejectBusIdentities(item);
  }
}
