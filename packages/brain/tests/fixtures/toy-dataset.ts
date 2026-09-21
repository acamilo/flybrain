import type { BrainDataset } from '../../src/dataset/format';

/** Four-neuron toy connectome: 0 is a Kenyon cell, 1 and 2 are MBONs, 3 is a motor/command neuron. */
export function toyDataset(): BrainDataset {
  return {
    meta: {
      schemaVersion: 1, dataset: 'test', neurons: 4, edges: 5,
      roles: { kenyon: [0], mbon: [1, 2], motor: [3], command_0: [3] },
      visual: { population: 'test', count: 0 },
    },
    indptr: Uint32Array.of(0, 3, 4, 5, 5),
    targets: Uint32Array.of(1, 2, 3, 3, 1),
    weights: Int16Array.of(10, -5, 20, 10, 5),
    visualIndices: new Uint32Array(),
    visualHemisphere: new Uint8Array(),
    visualXY: new Float32Array(),
  };
}

/** Deterministic xorshift32 for test sequences. */
export function xorshift(seed = 1): () => number {
  let state = seed | 0 || 1;
  return () => {
    state ^= state << 13; state ^= state >>> 17; state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}
