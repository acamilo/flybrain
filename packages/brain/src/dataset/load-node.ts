/**
 * Node dataset loader: `node:fs/promises` + `node:zlib` + `node:crypto`.
 *
 * Produces exactly the same {@link BrainDataset}, and exactly the same fingerprint string, as
 * `load-browser.ts` does for the same artifact directory. Tests assert that equivalence.
 */
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { gunzipSync } from 'node:zlib';
import {
  type BrainDataset,
  type BrainMetadata,
  type CircuitRoles,
  type Sha256Hex,
  fingerprintDataset,
  mergeCircuitRoles,
  mergeMacroRoles,
  validateDataset,
} from './format';

/**
 * Copy gunzipped bytes into a standalone, zero-offset `ArrayBuffer`.
 *
 * `gunzipSync` can return a `Buffer` that is a view into a pooled allocation, which typed array
 * constructors would either reject (alignment) or read past (length). The copy makes
 * `new Uint32Array(buffer)` over the whole buffer mean the same thing it means in the browser.
 */
function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  return buffer;
}

async function loadCompressed<T extends ArrayBufferView>(
  path: string,
  create: (buffer: ArrayBuffer) => T,
): Promise<T> {
  let compressed: Uint8Array;
  try {
    compressed = await readFile(path);
  } catch (error) {
    throw new Error(`Unable to load ${path}: ${(error as Error).message}`);
  }
  return create(toArrayBuffer(gunzipSync(compressed)));
}

async function loadJson<T>(path: string, missing: (error: Error) => string): Promise<T> {
  try {
    return JSON.parse(await readFile(path, 'utf8')) as T;
  } catch (error) {
    throw new Error(missing(error as Error));
  }
}

const sha256: Sha256Hex = async (bytes) => createHash('sha256').update(bytes).digest('hex');

/** Load a dataset from a local `data/<dataset>` directory. */
export async function loadBrainDatasetFromDir(dir: string): Promise<BrainDataset> {
  const meta = await loadJson<BrainMetadata>(
    join(dir, 'meta.json'),
    (error) => `Unable to load brain metadata: ${error.message}`,
  );
  const circuits = await loadJson<CircuitRoles>(
    join(dir, 'circuit-roles.json'),
    () => 'Unable to load anatomical circuit roles',
  );
  mergeCircuitRoles(meta, circuits);
  // The macro populations, which `fingerprintDataset` deliberately leaves out of the digest: they
  // are a relabelling of neurons the dataset already carries and must not move a checkpoint's
  // compatibility string (`mergeMacroRoles`).
  mergeMacroRoles(meta, circuits);
  const [indptr, targets, weights, visualIndices, visualHemisphere, visualXY] = await Promise.all([
    loadCompressed(join(dir, 'indptr.binz'), (buffer) => new Uint32Array(buffer)),
    loadCompressed(join(dir, 'targets.binz'), (buffer) => new Uint32Array(buffer)),
    loadCompressed(join(dir, 'weights.binz'), (buffer) => new Int16Array(buffer)),
    loadCompressed(join(dir, 'visual-indices.binz'), (buffer) => new Uint32Array(buffer)),
    loadCompressed(join(dir, 'visual-hemisphere.binz'), (buffer) => new Uint8Array(buffer)),
    loadCompressed(join(dir, 'visual-xy.binz'), (buffer) => new Float32Array(buffer)),
  ]);
  const dataset: BrainDataset = { meta, indptr, targets, weights, visualIndices, visualHemisphere, visualXY };
  validateDataset(dataset);
  dataset.fingerprint = await fingerprintDataset(dataset, sha256);
  return dataset;
}
