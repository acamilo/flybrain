# Dataset format

A dataset is a directed, signed, aggregated connectivity graph in source-indexed CSR form, plus
anatomical role lists and a "retina": a population of input neurons with 2D column coordinates
that an image can be projected onto. Schema version 1. The type definitions are in
`packages/brain/src/dataset/format.ts`.

The shipped dataset is `data/fafb-v783`: FlyWire FAFB Codex v783, retrieved 2026-09-13,
139,255 neurons and 2,700,513 edges.

## Artifacts

`.binz` files are gzip streams of little-endian typed-array bytes. Sizes below are the
uncompressed byte lengths recorded in `meta.json`.

| File | Type | Length | Loaded into `BrainDataset` | Bytes |
| --- | --- | --- | --- | --- |
| `meta.json` | JSON | | `meta` | |
| `circuit-roles.json` | JSON | | merged into `meta.roles` | |
| `indptr.binz` | `Uint32Array` | neurons + 1 = 139,256 | `indptr` | 557,024 |
| `targets.binz` | `Uint32Array` | edges = 2,700,513 | `targets` | 10,802,052 |
| `weights.binz` | `Int16Array` | edges = 2,700,513 | `weights` | 5,401,026 |
| `visual-indices.binz` | `Uint32Array` | 1,572 | `visualIndices` | 6,288 |
| `visual-hemisphere.binz` | `Uint8Array` | 1,572 | `visualHemisphere` | 1,572 |
| `visual-xy.binz` | `Float32Array` | 3,144 (2 per column) | `visualXY` | 12,576 |
| `positions.binz` | `Float32Array` | 417,765 (xyz per neuron) | viewer only | 1,671,060 |
| `classes.binz` | `Uint8Array` | 139,255 | viewer only | 139,255 |
| `viewer-edges.binz` | `Uint32Array` | 126,172 (63,086 pairs) | viewer only | 504,688 |

`meta.json` records `bytes`, `compressedBytes` and `sha256` for every `.binz`, so a rebuild is
self-checking. The three viewer artifacts are not part of `BrainDataset` and are not hashed into
the dataset fingerprint; the activity map reads them directly.

`classes.binz` is one byte per neuron from the Codex `flow` column: 0 afferent (19,300),
2 efferent (1,491), 1 everything else (118,464). `viewer-edges.binz` is a deterministic sample of
the edge list, the pairs whose packed `(pre << 32) | post` key is divisible by 43
(`tools/build_flywire.py`).

## CSR layout

Connectivity is source-indexed compressed sparse row. Neuron `s` owns edge slots
`indptr[s] .. indptr[s+1] - 1`. For each slot `e`, `targets[e]` is the post-synaptic neuron and
`weights[e]` is the signed weight. Edges are sorted by `(pre, post)`, so each row's targets are
ascending. Neuron indices are the positions of `root_id`s sorted ascending across
`classification.csv.gz`; every array, role list and edge uses that index space.

The kernel's propagation loop walks exactly this structure (`model/lif.ts`, `stepOne`):

```
for (let edge = indptr[source]; edge < indptr[source + 1]; edge++) { ... targets[edge] ... }
```

`validateDataset()` (`dataset/format.ts`) throws unless `indptr.length === neurons + 1`,
`targets.length === weights.length === edges`, `visualIndices.length === visualHemisphere.length
=== visual.count` and `visualXY.length === visual.count * 2`.

## Weight encoding

`meta.weightEncoding`:

> signed aggregated synapse count; GABA/GLUT negative, other annotated transmitters positive

Synapse counts from `connections.csv.gz` are summed per directed neuron pair, then multiplied by
the transmitter sign and clamped to `[-32767, 32767]`. The sign table in
`tools/build_flywire.py` is `ACH +1, GABA -1, GLUT -1, OCT +1, SER +1, DA +1`, and any other or
unannotated transmitter is treated as excitatory. A pair whose rows disagree on transmitter is
marked `MIXED`, which is not in the table and therefore also excitatory. Magnitudes are measured
synapse counts; the signs are a project modeling choice.

The kernel never mutates these weights. Plasticity multiplies selected weights by a per-edge gain
in `[0.9, 1.1]`, so an excitatory edge never changes sign (see [plasticity](plasticity.md)).

## Roles

`meta.roles` maps a role name to a sorted list of neuron indices. `circuit-roles.json` carries a
second set that both loaders merge over `meta.roles` before fingerprinting
(`mergeCircuitRoles()`), so the mushroom-body populations can be regenerated without rewriting the
connectivity metadata. The merge throws if the sidecar's neuron count disagrees.

Twenty roles after the merge, with counts in the committed artifacts:

| Role | Count | Source file | Codex predicate (`tools/build_flywire.py`) |
| --- | --- | --- | --- |
| `sensory` | 17,550 | `circuit-roles.json` | `super_class` in (`sensory`, `sensory_ascending`) |
| `visual_l1` | 1,572 | `meta.json` | `column_assignment` rows of `type` L1 |
| `kenyon` | 5,177 | `circuit-roles.json` | `class` = `Kenyon_Cell` |
| `mbon` | 96 | `circuit-roles.json` | `class` = `MBON` |
| `reward_pam` | 307 | `meta.json` | `class` = `DAN` and cell type starts with `PAM` |
| `descending` | 1,305 | both | `super_class` = `descending` |
| `motor` | 110 | both | `super_class` = `motor` or `class` = `brain_motor_neuron` |
| `command_0` | 151 | `meta.json` | descending, index mod 8 = 0 |
| `command_1` | 174 | `meta.json` | descending, index mod 8 = 1 |
| `command_2` | 171 | `meta.json` | descending, index mod 8 = 2 |
| `command_3` | 141 | `meta.json` | descending, index mod 8 = 3 |
| `command_4` | 163 | `meta.json` | descending, index mod 8 = 4 |
| `command_5` | 161 | `meta.json` | descending, index mod 8 = 5 |
| `command_6` | 149 | `meta.json` | descending, index mod 8 = 6 |
| `command_7` | 195 | `meta.json` | descending, index mod 8 = 7 |
| `steer_left` | 2 | `meta.json` | cell type DNa01 or DNa02, left side |
| `steer_right` | 2 | `meta.json` | cell type DNa01 or DNa02, right side |
| `forward` | 2 | `meta.json` | cell type DNp09 |
| `backward` | 4 | `meta.json` | cell type MDN |
| `proboscis` | 24 | `meta.json` | `sub_class` = `proboscis_motor_neuron` |

`descending` and `motor` appear in both files with the same predicate and the same count, so the
merge is a no-op for them. The `command_<k>` split is a round-robin over the descending population
by neuron index: a modeling choice, not an anatomical grouping, and the bucket count is a build
option (`--command-buckets`, default 8).

Role names are anatomical labels, not inferred task functions. `command_<k>`, the transmitter
signs and the `visual_l1` retina mapping are project choices layered on top.

## Retina columns

`meta.visual` is `{ "population": "L1", "count": 1572 }`. The columns are the FlyWire L1
lamina-monopolar neurons that carry an optic-lobe column assignment. `visualIndices[i]` is the
neuron index of column `i`, `visualHemisphere[i]` is 0 for left and 1 for right, and
`visualXY[2i]`, `visualXY[2i+1]` are the column's x and y in dataset units. Hemisphere 0 columns
are mirrored on X when a frame is projected (`model/retina.ts`). How the coordinates map to pixels
is in [model](model.md).

## Fingerprinting

`fingerprintDataset()` returns seven lowercase SHA-256 hex digests joined with `:`, in this frozen
order:

1. UTF-8 `JSON.stringify(meta)`, with circuit roles already merged
2. `indptr`
3. `targets`
4. `weights`
5. `visualIndices`
6. `visualHemisphere`
7. `visualXY`

Both loaders merge circuit roles into the loaded metadata object rather than rebuilding it, which
is what keeps key order, and therefore the first digest, stable across platforms. Applications
record the string in checkpoints and refuse a restore when it does not match. The fingerprint
hashes the decoded arrays, not the compressed files, so it also catches a decode difference
between the Node and browser loaders; `packages/brain/tests/dataset.test.ts` asserts the two
agree.

## Loading

```ts
import { loadBrainDatasetFromDir } from '@flybrain/brain/node';
const dataset = await loadBrainDatasetFromDir('data/fafb-v783');
```

```ts
import { loadBrainDataset } from '@flybrain/brain/browser';
const dataset = await loadBrainDataset('/data/fafb-v783');
```

Both read `meta.json` and `circuit-roles.json`, merge, gunzip the six simulation arrays in
parallel, run `validateDataset()` and then set `dataset.fingerprint`. The loaders are subpath
exports so a bundle never pulls in `node:zlib` and a server never depends on
`DecompressionStream`.

## Regenerating

`tools/build_flywire.py` rebuilds every file in `data/fafb-v783` from the five official Codex v783
CSV exports, which it downloads into `.tools/flywire-v783/` (excluded from version control) and
checksum-verifies. Plain `python3` is blocked on this box, so invoke it through `uv`:

```sh
uv run python3 tools/build_flywire.py
```

A default run reproduces the committed artifacts byte for byte and yields 139,255 neurons,
2,700,513 edges and 1,572 L1 retina columns. Source checksums, artifact checksums, the `--output`
and `--command-buckets` options, and how to verify a rebuild are in
[`tools/README.md`](../tools/README.md).

## License and citations

Copied from `data/fafb-v783/ATTRIBUTION.md`.

The artifacts are independently generated from the FlyWire FAFB public Codex v783 exports.

- Source: https://codex.flywire.ai/
- Source files: https://storage.googleapis.com/flywire-data/codex/data/fafb/783/
- License: [Creative Commons Attribution-NonCommercial 4.0 International](https://creativecommons.org/licenses/by-nc/4.0/)
- Modifications: connectivity is aggregated by directed neuron pair, assigned stable numeric
  indices, encoded as typed sparse arrays, and joined with classification,
  representative-coordinate, cell-type, and optic-lobe column annotations. Functional role
  predicates and neural-model signs are project modeling choices.

No endorsement by FlyWire or the cited authors is implied.

Citations:

- Dorkenwald et al., "Neuronal wiring diagram of an adult brain," Nature 634 (2024),
  https://doi.org/10.1038/s41586-024-07558-y
- Schlegel et al., "Whole-brain annotation and multi-connectome cell typing," Nature 634 (2024),
  https://doi.org/10.1038/s41586-024-07686-5
- Matsliah et al., "Neuronal parts list and wiring diagram for a visual system," Nature 634
  (2024), https://doi.org/10.1038/s41586-024-07981-1

CC BY-NC 4.0 is non-commercial. A commercial demo built on these artifacts needs a different data
source or separate permission.
