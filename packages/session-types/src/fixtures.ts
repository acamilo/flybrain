/**
 * Loading the fixture corpus, which lives with the Rust crate:
 * `services/flysim/crates/fly-session-types/fixtures`.
 *
 * One corpus, two implementations. A case written once holds both languages to it.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { type Json, fail, parseStrict } from './canonical';

const here = fileURLToPath(new URL('.', import.meta.url));

/** The fixture directory. */
export const FIXTURE_DIR = join(
  here,
  '../../../services/flysim/crates/fly-session-types/fixtures',
);

export function loadBytes(name: string): Uint8Array {
  return new Uint8Array(readFileSync(join(FIXTURE_DIR, name)));
}

/** Reads one fixture file, parsed strictly. */
export function load(name: string): Json {
  return parseStrict(loadBytes(name));
}

export function section(file: Json, key: string): Json[] {
  const value = (file as Record<string, Json>)[key];
  if (!Array.isArray(value) || value.length === 0) {
    fail(`fixture: ${key} must be a nonempty array`);
  }
  return value;
}

export function cases(file: Json): Json[] {
  return section(file, 'cases');
}

/** A string field of one case. */
export function field(value: Json, key: string): string {
  const found = (value as Record<string, Json>)[key];
  if (typeof found !== 'string') fail(`fixture case: missing string field "${key}"`);
  return found;
}

export function optionalField(value: Json, key: string): string | undefined {
  const found = (value as Record<string, Json>)[key];
  return typeof found === 'string' ? found : undefined;
}

export function member(value: Json, key: string): Json {
  const found = (value as Record<string, Json>)[key];
  if (found === undefined) fail(`fixture case: missing field "${key}"`);
  return found;
}

export function decodeBase64(text: string): Uint8Array {
  return new Uint8Array(Buffer.from(text, 'base64'));
}
