#!/usr/bin/env python3
"""Build every browser artifact from the official FlyWire Codex v783 exports.

One run downloads (and checksum-verifies) the Codex CSV exports, then writes the sparse
connectivity arrays, viewer geometry, optic-lobe column tables, `meta.json` and
`circuit-roles.json` into the output directory. Rerunning with default options reproduces the
artifacts committed in `data/fafb-v783` byte for byte.
"""

from __future__ import annotations

import argparse
import csv
import gzip
import hashlib
import json
import struct
import urllib.request
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / ".tools" / "flywire-v783"
OUTPUT = ROOT / "data" / "fafb-v783"
BASE_URL = "https://storage.googleapis.com/flywire-data/codex/data/fafb/783"
FILES = {
    "classification.csv.gz": "e946b552f4056dfc977707be0674609832c3f64332a22d69dc0d9615e7aae663",
    "connections.csv.gz": "d49dd692e59e153aa3c83f5257bfc0eff51247b86d7bb183386c6d1622c70fc9",
    "consolidated_cell_types.csv.gz": "8aba246d71dc40361677493629972ce3883048c3d02010adc42bda22962a1a2d",
    "coordinates.csv.gz": "14337121f451f98c2576cee72c24409ada5aaf7948b7c7ca8de9040296840e05",
    "column_assignment.csv.gz": "bdf4ce7f62cc63493d53eefad3816ff2dfd08b190e97b35a492e0e453df2f0f6",
}
DATASET = "FlyWire FAFB Codex v783"
RETRIEVED = "2026-09-13"
DEFAULT_COMMAND_BUCKETS = 8
# Emitted into circuit-roles.json in this order, empty lists included.
CIRCUIT_ROLES = ("sensory", "motor", "descending", "kenyon", "mbon")
# The macro types of `docs/design/macros.md` section 11, in the order the contract lists them.
# One population per type, `macro_<type>`, drawn round-robin from the mushroom body output
# neurons plus the brain motor neurons -- the neurons whose input synapses the reward rule
# changes, which is what makes a learned preference for a macro the mushroom body doing what it
# does in the real fly. The order is the contract's and fixes which neurons land in which
# population, so it is never re-sorted.
MACRO_TYPES = (
    "go_objective",
    "go_out",
    "go_warp",
    "go_route",
    "go_item",
    "go_npc",
    "go_frontier",
    # `docs/design/macros.md` section 13's two errands, beside the other walks.
    "go_shop",
    "go_heal",
    "talk",
    "menu",
    "next",
    "yes",
    "no",
    "close",
    "confirm",
    "back",
    # Section 14: one population per move slot, where `attack` was. No "best move" knowledge
    # remains anywhere, so which move is used is the fly's choice and these four are what it
    # learns on.
    "move_1",
    "move_2",
    "move_3",
    "move_4",
    "switch",
    "item",
    "throw_ball",
    "run",
    "buy_potion",
    "buy_ball",
    "buy_antidote",
    "buy_repel",
    # The conversation at the Pokemon Center counter, which is section 13's `HEAL`.
    "heal",
    "leave",
)
# Neural-model signs for annotated transmitters; anything else is treated as excitatory.
NT_SIGN = {"ACH": 1, "GABA": -1, "GLUT": -1, "OCT": 1, "SER": 1, "DA": 1}


def macro_roles(mbon: list[int], motor: list[int]) -> dict[str, list[int]]:
    """`macro_<type>` populations, round-robin over the MBON and brain-motor pool.

    A pure function of the two anatomical role lists, so it says the same thing whether it is
    called from a full build or from `--macro-roles` over the committed artifact. The pool is the
    union of the two lists sorted ascending -- neuron indices are already the sorted `root_id`
    positions, so this is the dataset's own order and not a second opinion about it -- and
    neuron `pool[i]` joins `MACRO_TYPES[i % len(MACRO_TYPES)]`. With 96 MBONs and 110 brain motor
    neurons that is 206 neurons: six or seven per type at the thirty-one of section 14, seven or
    eight at the twenty-seven of section 13, nine or ten at the twenty-two section 11 shipped.

    The partition is a pure function of the type list, so **adding a type re-deals every
    population** -- 206 neurons dealt twenty-seven ways is not twenty-two ways plus five. That is
    what the earlier addition did too, and it is why section 11 says what it says about the
    checkpoint: rates are restored by role *name*, new roles start at zero, and the neuron ids,
    the edges and the kernel are untouched, so the compatibility string does not move. What a run
    loses across the change is the mushroom body's learned preference for particular macros, which
    is the cost of the button existing at all.
    """
    pool = sorted(set(mbon) | set(motor))
    roles: dict[str, list[int]] = {f"macro_{name}": [] for name in MACRO_TYPES}
    for position, neuron in enumerate(pool):
        roles[f"macro_{MACRO_TYPES[position % len(MACRO_TYPES)]}"].append(neuron)
    return roles


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--command-buckets",
        type=int,
        default=DEFAULT_COMMAND_BUCKETS,
        metavar="N",
        help="split descending neurons into N round-robin `command_<k>` roles (default: %(default)s)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=OUTPUT,
        metavar="DIR",
        help="directory to write the artifacts into (default: %(default)s)",
    )
    parser.add_argument(
        "--macro-roles",
        action="store_true",
        help=(
            "rewrite only the `macro_<type>` roles of an existing circuit-roles.json in --output, "
            "without downloading the Codex exports; the macro populations are a pure function of "
            "the mbon and motor lists already in that file"
        ),
    )
    args = parser.parse_args(argv)
    if args.command_buckets < 1:
        parser.error("--command-buckets must be at least 1")
    return args


def rewrite_macro_roles(output: Path) -> None:
    """`--macro-roles`: recompute the macro populations over the committed circuit roles.

    The full build needs the five Codex exports (several GB) and rewrites every artifact; the
    macro populations need neither, because they are derived from `mbon` and `motor` and nothing
    else. This path rewrites `circuit-roles.json` in place: the five anatomical roles keep their
    values and their positions, every `macro_*` role is dropped and re-derived, and the file is
    written with the same separators, so the diff against the previous copy is exactly the macro
    roles. `tests/test_build_flywire.py` pins that it agrees with a full run.
    """
    path = output / "circuit-roles.json"
    circuits = json.loads(path.read_text())
    roles = {name: indices for name, indices in circuits["roles"].items() if not name.startswith("macro_")}
    roles.update(macro_roles(roles["mbon"], roles["motor"]))
    circuits["roles"] = roles
    path.write_text(json.dumps(circuits, separators=(",", ":")))
    print({name: len(indices) for name, indices in roles.items() if name.startswith("macro_")}, flush=True)


def fetch_sources() -> None:
    SOURCE.mkdir(parents=True, exist_ok=True)
    for name, expected in FILES.items():
        path = SOURCE / name
        if not path.exists():
            print(f"downloading {name}", flush=True)
            urllib.request.urlretrieve(f"{BASE_URL}/{name}", path)
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError(f"checksum mismatch for {name}: {actual}")


def write_gzip(output: Path, name: str, data: bytes) -> dict[str, int | str]:
    path = output / name
    with gzip.GzipFile(filename="", mode="wb", fileobj=path.open("wb"), mtime=0) as stream:
        stream.write(data)
    return {
        "bytes": len(data),
        "compressedBytes": path.stat().st_size,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def pack_array(code: str, values: list[int] | list[float]) -> bytes:
    return struct.pack(f"<{len(values)}{code}", *values)


def main(argv: list[str] | None = None) -> None:
    args = parse_args(argv)
    output = args.output
    command_buckets = args.command_buckets

    if args.macro_roles:
        rewrite_macro_roles(output)
        return

    fetch_sources()
    output.mkdir(parents=True, exist_ok=True)

    classifications: dict[int, dict[str, str]] = {}
    with gzip.open(SOURCE / "classification.csv.gz", "rt", newline="") as stream:
        for row in csv.DictReader(stream):
            classifications[int(row["root_id"])] = row

    roots = sorted(classifications)
    index = {root: i for i, root in enumerate(roots)}
    n = len(roots)
    print(f"indexed {n:,} neurons", flush=True)

    primary: dict[int, str] = {}
    with gzip.open(SOURCE / "consolidated_cell_types.csv.gz", "rt", newline="") as stream:
        for row in csv.DictReader(stream):
            primary[int(row["root_id"])] = row["primary_type"]

    positions_sum = [[0.0, 0.0, 0.0, 0] for _ in range(n)]
    with gzip.open(SOURCE / "coordinates.csv.gz", "rt", newline="") as stream:
        for row in csv.DictReader(stream):
            i = index.get(int(row["root_id"]))
            if i is None:
                continue
            xyz = [float(value) for value in row["position"].strip("()[]").replace(",", " ").split()]
            positions_sum[i][0] += xyz[0]
            positions_sum[i][1] += xyz[1]
            positions_sum[i][2] += xyz[2]
            positions_sum[i][3] += 1

    positions: list[float] = []
    classes: list[int] = []
    roles: dict[str, list[int]] = defaultdict(list)
    circuit_roles: dict[str, list[int]] = {name: [] for name in CIRCUIT_ROLES}
    for i, root in enumerate(roots):
        sx, sy, sz, count = positions_sum[i]
        positions.extend((sx / count, sy / count, sz / count) if count else (0.0, 0.0, 0.0))
        row = classifications[root]
        flow = row["flow"]
        super_class = row["super_class"]
        cell_class = row["class"]
        subtype = row["sub_class"]
        side = row["side"].lower()
        cell_type = primary.get(root, "")
        category = 0 if flow == "afferent" else 2 if flow == "efferent" else 1
        classes.append(category)
        if cell_type in {"DNa01", "DNa02"}:
            roles[f"steer_{'left' if side.startswith('left') else 'right'}"].append(i)
        if cell_type == "DNp09":
            roles["forward"].append(i)
        if cell_type == "MDN":
            roles["backward"].append(i)
        if subtype == "proboscis_motor_neuron":
            roles["proboscis"].append(i)
        if super_class == "descending":
            roles["descending"].append(i)
            roles[f"command_{i % command_buckets}"].append(i)
        if super_class == "motor" or cell_class == "brain_motor_neuron":
            roles["motor"].append(i)
        if cell_class == "DAN" and cell_type.startswith("PAM"):
            roles["reward_pam"].append(i)
        # Anatomical circuit roles, written separately so the mushroom-body populations can be
        # loaded and merged over meta.roles without reloading the whole metadata blob.
        if super_class in ("sensory", "sensory_ascending"):
            circuit_roles["sensory"].append(i)
        if super_class == "motor" or cell_class == "brain_motor_neuron":
            circuit_roles["motor"].append(i)
        if super_class == "descending":
            circuit_roles["descending"].append(i)
        if cell_class == "Kenyon_Cell":
            circuit_roles["kenyon"].append(i)
        if cell_class == "MBON":
            circuit_roles["mbon"].append(i)

    visual_rows: list[tuple[int, int, float, float]] = []
    with gzip.open(SOURCE / "column_assignment.csv.gz", "rt", newline="") as stream:
        for row in csv.DictReader(stream):
            if row["type"] != "L1":
                continue
            i = index.get(int(row["root_id"]))
            if i is None:
                continue
            hemisphere = 0 if row["hemisphere"].lower().startswith("left") else 1
            visual_rows.append((i, hemisphere, float(row["x"]), float(row["y"])))
            roles["visual_l1"].append(i)

    edge_map: dict[int, list[int | str]] = {}
    with gzip.open(SOURCE / "connections.csv.gz", "rt", newline="") as stream:
        for row_number, row in enumerate(csv.DictReader(stream), 1):
            pre = index.get(int(row["pre_root_id"]))
            post = index.get(int(row["post_root_id"]))
            if pre is None or post is None:
                continue
            key = (pre << 32) | post
            count = int(row["syn_count"])
            nt = row["nt_type"].upper()
            if key in edge_map:
                edge_map[key][0] = int(edge_map[key][0]) + count
                if edge_map[key][1] != nt:
                    edge_map[key][1] = "MIXED"
            else:
                edge_map[key] = [count, nt]
            if row_number % 500_000 == 0:
                print(f"parsed {row_number:,} connection rows", flush=True)

    indptr = [0] * (n + 1)
    targets: list[int] = []
    weights: list[int] = []
    viewer_edges: list[int] = []
    for key, (count_value, nt_value) in sorted(edge_map.items()):
        pre = key >> 32
        post = key & 0xFFFFFFFF
        count = int(count_value)
        sign = NT_SIGN.get(str(nt_value), 1)
        indptr[pre + 1] += 1
        targets.append(post)
        weights.append(max(-32767, min(32767, count * sign)))
        if key % 43 == 0:
            viewer_edges.extend((pre, post))
    for i in range(n):
        indptr[i + 1] += indptr[i]

    visual_indices = [row[0] for row in visual_rows]
    visual_hemisphere = [row[1] for row in visual_rows]
    visual_xy = [value for row in visual_rows for value in row[2:]]
    artifacts = {
        "indptr.binz": write_gzip(output, "indptr.binz", pack_array("I", indptr)),
        "targets.binz": write_gzip(output, "targets.binz", pack_array("I", targets)),
        "weights.binz": write_gzip(output, "weights.binz", pack_array("h", weights)),
        "positions.binz": write_gzip(output, "positions.binz", pack_array("f", positions)),
        "classes.binz": write_gzip(output, "classes.binz", bytes(classes)),
        "viewer-edges.binz": write_gzip(output, "viewer-edges.binz", pack_array("I", viewer_edges)),
        "visual-indices.binz": write_gzip(output, "visual-indices.binz", pack_array("I", visual_indices)),
        "visual-hemisphere.binz": write_gzip(output, "visual-hemisphere.binz", bytes(visual_hemisphere)),
        "visual-xy.binz": write_gzip(output, "visual-xy.binz", pack_array("f", visual_xy)),
    }
    metadata = {
        "schemaVersion": 1,
        "dataset": DATASET,
        "retrieved": RETRIEVED,
        "neurons": n,
        "edges": len(targets),
        "sourceFiles": FILES,
        "roles": dict(sorted(roles.items())),
        "visual": {"population": "L1", "count": len(visual_rows)},
        "weightEncoding": "signed aggregated synapse count; GABA/GLUT negative, other annotated transmitters positive",
        "positionPolicy": "arithmetic mean of representative coordinate rows",
        "artifacts": artifacts,
    }
    (output / "meta.json").write_text(json.dumps(metadata, separators=(",", ":")))
    # Section 11's macro populations, appended after the five anatomical roles so that an
    # existing consumer's key order is untouched and the diff against the previous artifact is
    # additive.
    circuit_roles.update(macro_roles(circuit_roles["mbon"], circuit_roles["motor"]))
    circuits = {"dataset": DATASET, "neurons": n, "roles": circuit_roles}
    (output / "circuit-roles.json").write_text(json.dumps(circuits, separators=(",", ":")))
    print(f"wrote {len(targets):,} edges and {len(visual_rows):,} L1 inputs", flush=True)
    print({name: len(indices) for name, indices in circuit_roles.items()}, flush=True)


if __name__ == "__main__":
    main()
