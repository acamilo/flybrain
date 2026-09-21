/**
 * Browser dataset loader: `fetch` + `DecompressionStream('gzip')` + `crypto.subtle`.
 *
 * Ported from the original prototype loader; the error strings and the fingerprint are part of
 * the on-disk checkpoint contract and must not drift from `load-node.ts`.
 */
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
 * Fetch a gzip-compressed `.binz` artifact and wrap the inflated bytes in a typed array.
 *
 * Exported because the viewer (`@flybrain/brain/view`) loads its own artifacts the same way.
 */
export async function loadCompressed<T extends ArrayBufferView>(
  url: string,
  create: (buffer: ArrayBuffer) => T,
): Promise<T> {
  const response = await fetch(url);
  if (!response.ok || !response.body) throw new Error(`Unable to load ${url}: HTTP ${response.status}`);
  const stream = response.body.pipeThrough(new DecompressionStream('gzip'));
  return create(await new Response(stream).arrayBuffer());
}

const sha256: Sha256Hex = async (bytes) => {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
};

/** Load a dataset from a URL prefix serving the artifacts of one `data/<dataset>` directory. */
export async function loadBrainDataset(base = '/data/fafb-v783'): Promise<BrainDataset> {
  const metadataResponse = await fetch(`${base}/meta.json`);
  if (!metadataResponse.ok) throw new Error(`Unable to load brain metadata: HTTP ${metadataResponse.status}`);
  const meta = await metadataResponse.json() as BrainMetadata;
  const rolesResponse = await fetch(`${base}/circuit-roles.json`);
  if (!rolesResponse.ok) throw new Error('Unable to load anatomical circuit roles');
  const circuits = await rolesResponse.json() as CircuitRoles;
  mergeCircuitRoles(meta, circuits);
  // The macro populations, which `fingerprintDataset` deliberately leaves out of the digest: they
  // are a relabelling of neurons the dataset already carries and must not move a checkpoint's
  // compatibility string (`mergeMacroRoles`).
  mergeMacroRoles(meta, circuits);
  const [indptr, targets, weights, visualIndices, visualHemisphere, visualXY] = await Promise.all([
    loadCompressed(`${base}/indptr.binz`, (buffer) => new Uint32Array(buffer)),
    loadCompressed(`${base}/targets.binz`, (buffer) => new Uint32Array(buffer)),
    loadCompressed(`${base}/weights.binz`, (buffer) => new Int16Array(buffer)),
    loadCompressed(`${base}/visual-indices.binz`, (buffer) => new Uint32Array(buffer)),
    loadCompressed(`${base}/visual-hemisphere.binz`, (buffer) => new Uint8Array(buffer)),
    loadCompressed(`${base}/visual-xy.binz`, (buffer) => new Float32Array(buffer)),
  ]);
  const dataset: BrainDataset = { meta, indptr, targets, weights, visualIndices, visualHemisphere, visualXY };
  validateDataset(dataset);
  dataset.fingerprint = await fingerprintDataset(dataset, sha256);
  return dataset;
}
