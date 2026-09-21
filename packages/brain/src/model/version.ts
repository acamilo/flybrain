/**
 * Version-string hashing for non-default kernel configurations.
 *
 * Default configurations keep their historical version strings so old checkpoints stay
 * compatible; any other numeric configuration derives a distinct string from an FNV-1a-32 hash of
 * its numeric parameters in a fixed, documented order.
 */

/** FNV-1a-32 over the UTF-16 code units of `text`, as eight lowercase hex digits. */
export function fnv1a32Hex(text: string): string {
  let hash = 2166136261;
  for (let i = 0; i < text.length; i++) hash = Math.imul(hash ^ text.charCodeAt(i), 16777619) >>> 0;
  return hash.toString(16).padStart(8, '0');
}

/**
 * `base` when every parameter equals its default, otherwise `prefix` plus the FNV-1a-32 hash of
 * the parameters joined with ','. Parameter order is part of the version contract.
 */
export function versionFor(base: string, prefix: string, params: readonly number[], defaults: readonly number[]): string {
  if (params.length === defaults.length && params.every((value, index) => value === defaults[index])) return base;
  return `${prefix}${fnv1a32Hex(params.join(','))}`;
}
