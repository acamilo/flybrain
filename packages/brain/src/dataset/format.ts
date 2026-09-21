/**
 * Connectome artifact format (schema 1).
 *
 * A dataset is a directed, signed, aggregated connectivity graph in source-indexed CSR form plus
 * anatomical role lists and an optional "retina": a population of input neurons with 2D column
 * coordinates that an image can be projected onto.
 */
export interface BrainMetadata {
  schemaVersion: number;
  dataset: string;
  neurons: number;
  edges: number;
  /** Anatomical role name -> sorted neuron indices. Labels, not inferred task functions. */
  roles: Record<string, number[]>;
  visual: { population: string; count: number };
}

export interface BrainDataset {
  /** SHA-256 hex digests of metadata and every array, joined with ':'; used for checkpoint compatibility. */
  fingerprint?: string;
  meta: BrainMetadata;
  /** CSR row pointers, length neurons + 1. */
  indptr: Uint32Array;
  /** CSR column indices (post-synaptic neuron per edge). */
  targets: Uint32Array;
  /** Signed aggregated synapse counts per edge (inhibitory transmitters negative). */
  weights: Int16Array;
  /** Neuron index of each retina column. */
  visualIndices: Uint32Array;
  /** 0 = left, 1 = right; left columns are mirrored on X when projecting an image. */
  visualHemisphere: Uint8Array;
  /** Interleaved x,y column coordinates in dataset units. */
  visualXY: Float32Array;
}

/**
 * Sidecar role lists shipped next to the metadata (`circuit-roles.json`). Populations that the
 * plasticity rule needs (kenyon, mbon) live here so they can be regenerated without rewriting
 * the connectivity metadata.
 */
export interface CircuitRoles {
  neurons: number;
  roles: Record<string, number[]>;
}

/** Hashes bytes to a lowercase hex SHA-256 digest; supplied by the platform-specific loader. */
export type Sha256Hex = (bytes: Uint8Array<ArrayBuffer>) => Promise<string>;

/**
 * The metadata the fingerprint is taken over: everything, minus the `macro_*` populations.
 *
 * Key order is what a digest is made of, so this is a shallow copy with `roles` rebuilt in its own
 * place and in its own order — the macro roles are appended last by the artifact, so dropping them
 * leaves exactly the object the fingerprint was defined over before they existed. See
 * {@link mergeMacroRoles} for why they are outside it at all.
 */
function fingerprintedMetadata(meta: BrainMetadata): BrainMetadata {
  const roles: Record<string, number[]> = {};
  let dropped = false;
  for (const [name, indices] of Object.entries(meta.roles)) {
    if (name.startsWith(MACRO_ROLE_PREFIX)) dropped = true;
    else roles[name] = indices;
  }
  return dropped ? { ...meta, roles } : meta;
}

/**
 * Prefix of the macro-type populations (`docs/design/macros.md` section 11).
 *
 * The name rule that splits `circuit-roles.json` in two at load time: an anatomical role is
 * merged before the fingerprint, a `macro_*` role after it. See {@link mergeMacroRoles}.
 */
export const MACRO_ROLE_PREFIX = 'macro_';

/**
 * Merge the sidecar's anatomical circuit roles into loaded metadata, in place.
 *
 * Both loaders must call this before fingerprinting: the fingerprint hashes `JSON.stringify(meta)`
 * and the merge is what puts the kenyon/mbon/sensory keys into the object. Merging (rather than
 * rebuilding the object) is also what keeps metadata key order — and therefore the digest —
 * stable across platforms.
 *
 * The `macro_*` populations are deliberately left out: {@link mergeMacroRoles} adds them after
 * the fingerprint has been taken.
 */
export function mergeCircuitRoles(meta: BrainMetadata, circuits: CircuitRoles): void {
  if (circuits.neurons !== meta.neurons) throw new Error('Circuit roles do not match connectome');
  for (const [name, indices] of Object.entries(circuits.roles)) {
    if (name.startsWith(MACRO_ROLE_PREFIX)) continue;
    meta.roles[name] = indices;
  }
}

/**
 * Merge the sidecar's `macro_<type>` populations into loaded metadata, in place, *after* the
 * fingerprint.
 *
 * A contract rather than a convenience. Checkpoints record the fingerprint, and the macro
 * populations name no new neuron, edge or weight — they are the mushroom body output neurons and
 * the brain motor neurons the artifact already listed, relabelled twenty-two ways
 * (`docs/design/macros.md` section 11: "the neuron ids, edges and kernel are untouched, so the
 * compatibility string must not move"). Hashing them would move every checkpoint's compatibility
 * string for a relabelling, so a loader fingerprints the anatomical roles and calls this
 * afterwards; a checkpoint written before the roles existed loads and starts them at zero.
 *
 * The cost, stated rather than hidden: re-cutting the populations differently would not
 * invalidate a checkpoint, and the rates restored by name would then belong to different
 * neurons. `tools/build_flywire.py` owns that partition.
 */
export function mergeMacroRoles(meta: BrainMetadata, circuits: CircuitRoles): void {
  for (const [name, indices] of Object.entries(circuits.roles)) {
    if (name.startsWith(MACRO_ROLE_PREFIX)) meta.roles[name] = indices;
  }
}

/**
 * Throw if the arrays do not describe the connectome the metadata claims.
 *
 * The circuit-role neuron count is checked separately, by {@link mergeCircuitRoles}, because it
 * compares the sidecar file against the metadata before the two are merged into one object.
 */
export function validateDataset(dataset: BrainDataset): void {
  const { meta, indptr, targets, weights, visualIndices, visualHemisphere, visualXY } = dataset;
  if (indptr.length !== meta.neurons + 1 || targets.length !== meta.edges || weights.length !== meta.edges) {
    throw new Error('FlyWire artifact lengths do not match metadata');
  }
  if (
    visualIndices.length !== meta.visual.count ||
    visualHemisphere.length !== meta.visual.count ||
    visualXY.length !== meta.visual.count * 2
  ) {
    throw new Error('FlyWire visual artifact lengths do not match metadata');
  }
}

/**
 * Digest of the metadata and every array, joined with ':'.
 *
 * Checkpoints record this string, so the order of the seven parts and the hashed byte ranges are
 * frozen: metadata as UTF-8 `JSON.stringify(meta)` (with circuit roles already merged), then
 * indptr, targets, weights, visualIndices, visualHemisphere, visualXY. Any loader on any
 * platform must produce the same string for the same artifacts.
 */
export async function fingerprintDataset(dataset: BrainDataset, sha256: Sha256Hex): Promise<string> {
  const { meta, indptr, targets, weights, visualIndices, visualHemisphere, visualXY } = dataset;
  const parts: ArrayBufferView[] = [
    new TextEncoder().encode(JSON.stringify(fingerprintedMetadata(meta))),
    indptr,
    targets,
    weights,
    visualIndices,
    visualHemisphere,
    visualXY,
  ];
  const hashes = await Promise.all(parts.map((part) =>
    sha256(new Uint8Array(part.buffer as ArrayBuffer, part.byteOffset, part.byteLength))));
  return hashes.join(':');
}
