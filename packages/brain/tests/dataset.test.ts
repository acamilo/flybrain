import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { after, before, describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';
import { gunzipSync, gzipSync } from 'node:zlib';
import {
  type BrainDataset,
  type Sha256Hex,
  fingerprintDataset,
  mergeCircuitRoles,
  validateDataset,
} from '../src/dataset/format';
import { loadBrainDataset } from '../src/dataset/load-browser';
import { loadBrainDatasetFromDir } from '../src/dataset/load-node';
import { toyDataset } from './fixtures/toy-dataset';

const DATA_DIR = fileURLToPath(new URL('../../../data/fafb-v783', import.meta.url));

/** FlyWire FAFB Codex v783, as built by `tools/build_flywire.py` with default options. */
const NEURONS = 139255;
const EDGES = 2700513;
const VISUAL_COLUMNS = 1572;
/** Role sizes in the committed artifacts, after `circuit-roles.json` is merged over `meta.roles`. */
const ROLE_COUNTS: Record<string, number> = {
  kenyon: 5177,
  mbon: 96,
  command_0: 151,
  command_1: 174,
  command_2: 171,
  command_3: 141,
  command_4: 163,
  command_5: 161,
  command_6: 149,
  command_7: 195,
  reward_pam: 307,
  visual_l1: VISUAL_COLUMNS,
};
const HEX_DIGEST = /^[0-9a-f]{64}$/;

const sha256: Sha256Hex = async (bytes) => createHash('sha256').update(bytes).digest('hex');

let dataset: BrainDataset;

before(async () => {
  dataset = await loadBrainDatasetFromDir(DATA_DIR);
});

describe('node loader', () => {
  it('loads the committed FlyWire artifacts', () => {
    assert.equal(dataset.meta.schemaVersion, 1);
    assert.equal(dataset.meta.dataset, 'FlyWire FAFB Codex v783');
    assert.equal(dataset.meta.neurons, NEURONS);
    assert.equal(dataset.meta.edges, EDGES);
    assert.equal(dataset.meta.visual.population, 'L1');
    assert.equal(dataset.meta.visual.count, VISUAL_COLUMNS);
  });

  it('decodes a consistent CSR graph', () => {
    assert.equal(dataset.indptr.length, NEURONS + 1);
    assert.equal(dataset.indptr[0], 0);
    assert.equal(dataset.indptr[dataset.indptr.length - 1], EDGES);
    assert.equal(dataset.targets.length, EDGES);
    assert.equal(dataset.weights.length, EDGES);
    // Row pointers are non-decreasing and every target is a real neuron index.
    for (let i = 0; i < NEURONS; i += 1) assert.ok(dataset.indptr[i]! <= dataset.indptr[i + 1]!);
    assert.ok(dataset.targets.every((target) => target < NEURONS));
  });

  it('decodes the retina column tables', () => {
    assert.equal(dataset.visualIndices.length, VISUAL_COLUMNS);
    assert.equal(dataset.visualHemisphere.length, VISUAL_COLUMNS);
    assert.equal(dataset.visualXY.length, VISUAL_COLUMNS * 2);
    assert.ok(dataset.visualHemisphere.every((side) => side === 0 || side === 1));
  });

  it('merges the anatomical circuit roles into the metadata roles', () => {
    for (const [role, count] of Object.entries(ROLE_COUNTS)) {
      assert.ok(role in dataset.meta.roles, `missing role ${role}`);
      assert.equal(dataset.meta.roles[role]!.length, count, `role ${role} changed size`);
    }
    // Merged populations keep dataset index space and sorted order.
    for (const indices of Object.values(dataset.meta.roles)) {
      assert.ok(indices.every((index) => index >= 0 && index < NEURONS));
    }
    for (const role of ['kenyon', 'mbon', 'reward_pam']) {
      const indices = dataset.meta.roles[role]!;
      assert.deepEqual(indices, [...indices].sort((a, b) => a - b));
    }
  });

  it('fingerprints as seven hex digests joined by ":"', () => {
    assert.equal(typeof dataset.fingerprint, 'string');
    const parts = dataset.fingerprint!.split(':');
    assert.equal(parts.length, 7);
    for (const part of parts) assert.match(part, HEX_DIGEST);
  });
});

describe('validateDataset', () => {
  const LENGTHS = 'FlyWire artifact lengths do not match metadata';
  const VISUAL = 'FlyWire visual artifact lengths do not match metadata';

  it('accepts the toy fixture', () => {
    assert.doesNotThrow(() => validateDataset(toyDataset()));
  });

  it('accepts the committed dataset', () => {
    assert.doesNotThrow(() => validateDataset(dataset));
  });

  it('rejects an indptr that is not neurons + 1 long', () => {
    const broken = toyDataset();
    broken.indptr = Uint32Array.of(0, 3, 4, 5);
    assert.throws(() => validateDataset(broken), new Error(LENGTHS));
  });

  it('rejects a targets length that disagrees with meta.edges', () => {
    const broken = toyDataset();
    broken.targets = Uint32Array.of(1, 2, 3, 3);
    assert.throws(() => validateDataset(broken), new Error(LENGTHS));
  });

  it('rejects a weights length that disagrees with meta.edges', () => {
    const broken = toyDataset();
    broken.weights = Int16Array.of(10, -5, 20, 10, 5, 1);
    assert.throws(() => validateDataset(broken), new Error(LENGTHS));
  });

  it('rejects a neuron count that disagrees with the arrays', () => {
    const broken = toyDataset();
    broken.meta.neurons = 5;
    assert.throws(() => validateDataset(broken), new Error(LENGTHS));
  });

  it('rejects a visual index count that disagrees with meta.visual.count', () => {
    const broken = toyDataset();
    broken.visualIndices = Uint32Array.of(0);
    assert.throws(() => validateDataset(broken), new Error(VISUAL));
  });

  it('rejects a visual hemisphere count that disagrees with meta.visual.count', () => {
    const broken = toyDataset();
    broken.visualHemisphere = Uint8Array.of(1);
    assert.throws(() => validateDataset(broken), new Error(VISUAL));
  });

  it('rejects visual coordinates that are not two per column', () => {
    const broken = toyDataset();
    broken.meta.visual.count = 2;
    broken.visualIndices = Uint32Array.of(0, 1);
    broken.visualHemisphere = Uint8Array.of(0, 1);
    broken.visualXY = Float32Array.of(1, 2, 3);
    assert.throws(() => validateDataset(broken), new Error(VISUAL));
  });

  it('rejects circuit roles built against a different connectome', () => {
    const { meta } = toyDataset();
    assert.throws(
      () => mergeCircuitRoles(meta, { neurons: 5, roles: { kenyon: [0] } }),
      new Error('Circuit roles do not match connectome'),
    );
    assert.equal('sensory' in meta.roles, false);
  });
});

describe('fingerprintDataset', () => {
  it('is deterministic for the committed dataset', async () => {
    const first = await fingerprintDataset(dataset, sha256);
    const second = await fingerprintDataset(dataset, sha256);
    assert.equal(first, second);
    assert.equal(first, dataset.fingerprint);
  });

  it('is deterministic for the toy fixture', async () => {
    const first = await fingerprintDataset(toyDataset(), sha256);
    const second = await fingerprintDataset(toyDataset(), sha256);
    assert.equal(first, second);
    assert.equal(first.split(':').length, 7);
  });

  it('changes when any hashed part changes', async () => {
    const base = await fingerprintDataset(toyDataset(), sha256);
    const renamed = toyDataset();
    renamed.meta.dataset = 'other';
    assert.notEqual(await fingerprintDataset(renamed, sha256), base);
    const reweighted = toyDataset();
    reweighted.weights[0] = 11;
    assert.notEqual(await fingerprintDataset(reweighted, sha256), base);
  });
});

describe('browser loader', () => {
  const BASE = 'https://artifacts.test/data/fafb-v783';
  const realFetch = globalThis.fetch;

  /** Detach bytes into a standalone ArrayBuffer so they are a valid response body. */
  function body(bytes: Uint8Array): ArrayBuffer {
    const buffer = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buffer).set(bytes);
    return buffer;
  }

  async function serve(name: string): Promise<Response> {
    const path = join(DATA_DIR, name);
    if (name.endsWith('.json')) return new Response(await readFile(path, 'utf8'), { status: 200 });
    // Recompress at a different level: the response bytes deliberately differ from the committed
    // file, so an equal fingerprint proves the digest covers decompressed content only.
    return new Response(body(gzipSync(gunzipSync(await readFile(path)), { level: 1 })), { status: 200 });
  }

  before(() => {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = input instanceof URL ? input.href : typeof input === 'string' ? input : input.url;
      if (!url.startsWith(`${BASE}/`)) return new Response(null, { status: 404 });
      return serve(url.slice(BASE.length + 1));
    }) as typeof fetch;
  });

  after(() => {
    globalThis.fetch = realFetch;
  });

  it('fingerprints identically to the node loader', async () => {
    const fetched = await loadBrainDataset(BASE);
    assert.equal(fetched.fingerprint, dataset.fingerprint);
  });

  it('decodes the same arrays as the node loader', async () => {
    const fetched = await loadBrainDataset(BASE);
    assert.deepEqual(fetched.meta, dataset.meta);
    assert.deepEqual(Object.keys(fetched.meta.roles), Object.keys(dataset.meta.roles));
    assert.deepEqual(fetched.indptr, dataset.indptr);
    assert.deepEqual(fetched.targets, dataset.targets);
    assert.deepEqual(fetched.weights, dataset.weights);
    assert.deepEqual(fetched.visualIndices, dataset.visualIndices);
    assert.deepEqual(fetched.visualHemisphere, dataset.visualHemisphere);
    assert.deepEqual(fetched.visualXY, dataset.visualXY);
  });

  it('reports a missing dataset instead of hanging', async () => {
    await assert.rejects(
      () => loadBrainDataset('https://artifacts.test/data/missing'),
      /Unable to load brain metadata: HTTP 404/,
    );
  });
});
