/**
 * Pure geometry and classification helpers for the connectome viewer.
 *
 * Nothing here touches the DOM, WebGL or `three`, so it is unit-testable in Node and is safe to
 * import from any environment. The rendering half lives in `./connectome.ts`.
 */

/**
 * Centre a neuron point cloud on the origin, scale it so the furthest neuron sits at radius 1.3,
 * and flatten it onto z = 0 (the viewer draws an orthographic projection of the brain).
 *
 * `source` is a flat xyz triple array; the result has the same length.
 */
export function normalizePositions(source: Float32Array): Float32Array {
  const result = new Float32Array(source.length);
  const center = [0, 0, 0];
  const count = source.length / 3;
  for (let i = 0; i < source.length; i += 3) {
    center[0] += source[i]; center[1] += source[i + 1]; center[2] += source[i + 2];
  }
  center[0] /= count; center[1] /= count; center[2] /= count;
  let maxRadius = 0;
  for (let i = 0; i < source.length; i += 3) {
    const x = source[i] - center[0], y = source[i + 1] - center[1], z = source[i + 2] - center[2];
    maxRadius = Math.max(maxRadius, Math.hypot(x, y, z));
  }
  for (let i = 0; i < source.length; i += 3) {
    result[i] = (source[i] - center[0]) / maxRadius * 1.3;
    result[i + 1] = (source[i + 1] - center[1]) / maxRadius * 1.3;
    result[i + 2] = 0;
  }
  return result;
}

/** Which circuit roles count as brain input and brain output for {@link classifyByRoles}. */
export interface RoleClassification {
  /** Roles drawn as sensory (class 0). Default: `['sensory']`. */
  sensory?: string[];
  /** Roles drawn as output (class 2), applied after the sensory roles. Default: `['motor', 'descending']`. */
  output?: string[];
}

/**
 * Label every neuron 0 (sensory), 1 (internal) or 2 (output) from the dataset's circuit roles.
 *
 * Everything starts internal; the sensory roles are then marked, and the output roles last, so a
 * neuron listed under both is drawn as an output — the rule the original viewer used.
 */
export function classifyByRoles(
  roles: Record<string, number[]>,
  count: number,
  { sensory = ['sensory'], output = ['motor', 'descending'] }: RoleClassification = {},
): Uint8Array {
  const classes = new Uint8Array(count).fill(1);
  for (const role of sensory) for (const neuron of roles[role] ?? []) classes[neuron] = 0;
  for (const role of output) for (const neuron of roles[role] ?? []) classes[neuron] = 2;
  return classes;
}
