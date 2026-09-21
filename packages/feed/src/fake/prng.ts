/**
 * Deterministic PRNG for the fake flysim. Mulberry32: small, fast, good enough statistical
 * quality for synthetic dev/test data. Not used anywhere near real crypto or the real kernel's
 * `Xorshift32` (`@flybrain/brain`), which this package does not depend on.
 */
export type Rng = () => number;

export function mulberry32(seed: number): Rng {
  let a = seed >>> 0;
  return function next(): number {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Uniform float in `[min, max)`. */
export function randRange(rng: Rng, min: number, max: number): number {
  return min + rng() * (max - min);
}

/** Uniform integer in `[min, max]` inclusive. */
export function randInt(rng: Rng, min: number, max: number): number {
  return Math.floor(randRange(rng, min, max + 1));
}

/** True with probability `p` (0..1). */
export function chance(rng: Rng, p: number): boolean {
  return rng() < p;
}

/** Pick one element of a non-empty array. */
export function pick<T>(rng: Rng, items: readonly T[]): T {
  return items[randInt(rng, 0, items.length - 1)] as T;
}

/** Move `value` toward `target` by `rate` (0..1 of the gap) plus symmetric noise, clamped. */
export function wander(rng: Rng, value: number, target: number, rate: number, noise: number, min: number, max: number): number {
  const next = value + (target - value) * rate + randRange(rng, -noise, noise);
  return Math.min(max, Math.max(min, next));
}
