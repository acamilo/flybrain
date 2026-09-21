# FlyWire artifact builder

`build_flywire.py` regenerates everything in `data/fafb-v783` from the official FlyWire Codex
v783 CSV exports. The committed artifacts are the output of one default run, so the build is the
provenance record for the data: nothing in `data/` is hand-edited.

## Provenance

- Codex portal: <https://codex.flywire.ai/>
- Export bucket: <https://storage.googleapis.com/flywire-data/codex/data/fafb/783/>
- License: CC BY-NC 4.0 (see `data/fafb-v783/ATTRIBUTION.md` for the license, citations and the
  list of modifications).

Five exports are read. `fetch_sources()` downloads each one into `.tools/flywire-v783/`
(gitignored, several GB unpacked) and aborts on a checksum mismatch:

| source file | sha256 |
| --- | --- |
| `classification.csv.gz` | `e946b552f4056dfc977707be0674609832c3f64332a22d69dc0d9615e7aae663` |
| `connections.csv.gz` | `d49dd692e59e153aa3c83f5257bfc0eff51247b86d7bb183386c6d1622c70fc9` |
| `consolidated_cell_types.csv.gz` | `8aba246d71dc40361677493629972ce3883048c3d02010adc42bda22962a1a2d` |
| `coordinates.csv.gz` | `14337121f451f98c2576cee72c24409ada5aaf7948b7c7ca8de9040296840e05` |
| `column_assignment.csv.gz` | `bdf4ce7f62cc63493d53eefad3816ff2dfd08b190e97b35a492e0e453df2f0f6` |

Neuron indices are the positions of `root_id`s sorted ascending across
`classification.csv.gz`; every array, role list and edge in the dataset is expressed in that
index space. Roles are anatomical labels from the Codex annotations, not inferred task
functions; the `command_<k>` buckets and the transmitter sign table are project modeling
choices.

## Rerunning

The box enforces `uv`, so invoke Python through it:

```sh
uv run python3 tools/build_flywire.py
```

Options:

- `--output DIR` — write artifacts somewhere other than `data/fafb-v783` (useful for diffing a
  rebuild against the committed copies before overwriting them).
- `--command-buckets N` — split the descending population into `N` round-robin
  `command_<i % N>` roles. Default `8`, which is what the committed `meta.json` contains.
  Changing it changes `meta.json` only; the connectivity arrays are unaffected. Readout presets
  expect `command_0`..`command_7`, so a non-default value needs a matching decoder config.

A default rerun is reproducible: gzip streams are written with `mtime=0` and an empty filename
field, JSON is emitted with `separators=(",", ":")`, `meta.roles` is sorted by role name and
`circuit-roles.json` keeps its fixed `sensory, motor, descending, kenyon, mbon` order, with the
twenty-two `macro_<type>` populations appended after them in the order
`docs/design/macros.md` section 11 lists the types. The run prints the edge count, the L1 input
count and the circuit-role sizes.

- `--macro-roles` — rewrite only the `macro_<type>` roles of an existing `circuit-roles.json`
  in `--output`, with no downloads. The macro populations are a pure function of the `mbon` and
  `motor` lists already in that file (`macro_roles()`), so this path and a full run agree by
  construction; `services/flysim/crates/flybrain-core/tests/macro_roles.rs` checks the committed
  artifact against the same rule. It is how the committed copy gained the roles without
  re-deriving 2.7 M edges, and the diff it produced was additive to the byte: the file's first
  150,440 bytes are unchanged and the twenty-two roles are appended before the closing braces.

The macro populations name no new neuron, edge or weight -- they are a relabelling of neurons the
dataset already carries -- and they are deliberately *outside* the dataset fingerprint, so a
checkpoint written before they existed still loads and `flysim --print-compatibility` is
unchanged. Both loaders put them in `meta.roles` like any other role, and both fingerprint
functions hash the metadata with the `macro_*` roles removed, which is the one place that
exclusion lives.

## Verifying a rebuild

`meta.json` carries the byte length, compressed length and sha256 of every `.binz` it describes,
so a rebuild is self-checking. To confirm the committed tree instead:

```sh
cd data/fafb-v783 && sha256sum -c ../../tools/artifact-checksums.txt
```

Expected digests of the committed artifacts:

| artifact | sha256 |
| --- | --- |
| `indptr.binz` | `d6d42d174e02ca33351a76596fcf69ff1202705cfe2d6bcd227f6c7905c4a8dd` |
| `targets.binz` | `2af5c1eaa58b8cf0bfa95eaed0b7473c7eb8f6a65693822021ec6b76879f4cc2` |
| `weights.binz` | `5babc87800879821d1399de4fc423c3ff29e490b485ad0f194dfb85216900847` |
| `positions.binz` | `3c45c0abc0523344b4359f2c03d00408758850803ea8d0e23aa59513f3e02879` |
| `classes.binz` | `806dcf394ef7dc341d19636703813d9581bf60eae9006c5d1d94425aebdb724b` |
| `viewer-edges.binz` | `1e78af6ee31866440a18f2124bdc2f5cbe9cbe6e7f8e2e352c6467426740439b` |
| `visual-indices.binz` | `35e689285e7c2edcd7646bddf891cea2d9163a378641f962d87c0016879c8531` |
| `visual-hemisphere.binz` | `261e49d942c6e6d2cadd0b03e06fe7a1cec801bf30b37e6deacf15ebfb117dea` |
| `visual-xy.binz` | `1212951beeb2496a37c83541e8b425b9f90a57774c6d43c62de9d23df43e8ea9` |
| `meta.json` | `d2c2cddfea686cf7691fe30cf2df8f95bc0fa9eef3b4f2ede62c759b28ea88f5` |
| `circuit-roles.json` | `0994d1346df1a06c7c3f268f5ab179fb2646e21bf5672afb6e695eb04ceeb8a9` |

A default build yields 139,255 neurons, 2,700,513 edges and 1,572 L1 retina columns.

The library's own `fingerprintDataset()` hashes the loaded arrays rather than the compressed
files, so `packages/brain/tests/dataset.test.ts` is the end-to-end check that the artifacts in
`data/` still decode to the connectome the model was tuned against.

## History

`build_flywire.py` and a separate `build_circuit_roles.py` were merged into this single script
during extraction from the `fly-plays-pokemon` prototype. The circuit roles are now accumulated
in the same pass over the sorted `root_id` list that builds `meta.json`, which is what made the
two scripts agree on neuron indices in the first place.
