"""Resolve WRAM symbol addresses out of a pret/pokered checkout, and verify the pinned ones.

`gen_symbols.py` owns `symbols.rs` and takes its addresses from the prototype's
already-generated table; it refuses a hand-written address. Section 15 of
`docs/design/macros.md` needs four symbols that table does not carry
(`wOverworldMap`, `wCurMapTileset`, `wTilesetBank`, `wTilesetBlocksPtr`), and a
hand-computed address is exactly what neither script will take. So this tool
does the same job the disassembly's own `.sym` would, from `ram/wram.asm`:

* it walks the file in order, keeping a byte cursor;
* the cursor is **only ever live while it is anchored**: it is set from a symbol
  `symbols.rs` already pins, and any declaration form this tool cannot evaluate
  exactly kills it until the next pinned symbol revives it. An unanchored region
  can therefore not produce a number at all, rather than producing a wrong one;
* every pinned symbol it reaches while live is checked against `symbols.rs`, and
  a single disagreement is a failure with no output. A resolved symbol is only
  reported when the run also re-derived the *next* pinned symbol after it, so
  each answer is bracketed by two addresses the table already carries.

Usage (read-only; `--emit` rewrites the `ram` block of symbols.rs in place):

    python3 services/flysim/tools/resolve_wram.py --pokered <checkout> [--emit]
"""

from __future__ import annotations

import argparse
import math
import re
from pathlib import Path

SYMBOLS = (
    Path(__file__).resolve().parents[1] / 'crates/flybrain-gb/src/pokemon_red/symbols.rs'
)

#: The symbols this run is for, with why the macro layer needs each one. Every
#: one is checked to be bracketed by two pinned addresses before it is emitted.
WANTED = {
    # The current map's block ids, as LoadTileBlockMap copies them out of the
    # map's ROM bank: rows of (width + MAP_BORDER * 2) bytes, the map itself
    # offset by three rows and three columns. This is what makes a whole-map
    # walkability grid a WRAM read rather than a ROM one.
    'wOverworldMap': 'the loaded map, one byte per 4x4-tile block',
    # Which tileset the loaded map uses: the tile-pair collision lists are keyed
    # by it (CheckForTilePairCollisions).
    'wCurMapTileset': 'the loaded map tileset id',
    # The tileset header's blockset: a bank byte and a little-endian pointer at
    # 16 bytes per block, four rows of four tile ids (DrawTileBlock). The bank is
    # not bank 0, so this is the read the memory seam grew a bank for.
    'wTilesetBank': 'the ROM bank the blockset lives in',
    'wTilesetBlocksPtr': 'blocks to tiles, 16 bytes per block',
}


def pinned(text: str) -> dict[str, int]:
    """Every address `symbols.rs` carries today, by symbol name."""
    return {
        name: int(value, 16)
        for name, value in re.findall(r'pub const (w\w+): u16 = 0x([0-9a-f]{4});', text)
    }


def constants(root: Path) -> dict[str, int]:
    """Every `DEF NAME EQU <expression>` the declarations below need.

    Resolved by repeated passes rather than in one, because the decomp defines
    constants in terms of each other (`SURROUNDING_WIDTH EQU SCREEN_BLOCK_WIDTH *
    BLOCK_WIDTH`). A name whose expression never becomes evaluable is simply left
    out, which kills the cursor at any declaration that uses it.
    """
    pending: dict[str, str] = {}
    sources = sorted((root / 'constants').glob('*.asm')) + sorted(
        (root / 'constants').glob('*.inc')
    )
    for path in sources:
        for name, value in re.findall(
            r'^\s*(?:DEF|def)\s+(\w+)\s+(?:EQU|equ)\s+([^;\n]+)', path.read_text(), re.M
        ):
            pending.setdefault(name, value.strip())
    out: dict[str, int] = {}
    while pending:
        progressed = False
        for name in list(pending):
            try:
                out[name] = size_of(pending[name], out)
            except Unevaluable:
                continue
            del pending[name]
            progressed = True
        if not progressed:
            break
    return out


def number(token: str) -> int:
    token = token.strip()
    if token.startswith('$'):
        return int(token[1:], 16)
    if token.startswith('%'):
        return int(token[1:], 2)
    return int(token, 10)


class Unevaluable(Exception):
    """A declaration this tool will not guess the size of."""


def size_of(expression: str, known: dict[str, int]) -> int:
    """Bytes in a `ds`/`EQU` expression: the decomp's own arithmetic, nothing else.

    `$`/`%` literals and the constants resolved so far are substituted, the
    `tiles` unit is a factor of sixteen, and what is left must be plain
    arithmetic over integers -- so a name this tool has not resolved, a function
    call or anything else raises [`Unevaluable`] instead of becoming a guess.
    """
    expression = expression.split(';')[0].strip()
    if not expression:
        raise Unevaluable('empty')
    scale = 1
    if expression.endswith('tiles'):
        expression = expression[: -len('tiles')].strip()
        scale = 16  # one 8x8 2bpp tile is 16 bytes
    def substitute(match: re.Match[str]) -> str:
        token = match.group(0)
        if token[0] in '$%':
            return str(number(token))
        if token in known:
            return str(known[token])
        raise Unevaluable(token)
    substituted = re.sub(r'\$[0-9A-Fa-f_]+|%[01_]+|[A-Za-z_]\w*', substitute, expression)
    if not re.fullmatch(r'[\d\s()+\-*/]+', substituted):
        raise Unevaluable(expression)
    try:
        value = eval(substituted, {'__builtins__': {}}, {})  # arithmetic only, checked above
    except (SyntaxError, ZeroDivisionError, TypeError) as error:
        raise Unevaluable(expression) from error
    if not isinstance(value, int):
        raise Unevaluable(expression)
    return value * scale


def macro_sizes(root: Path, known: dict[str, int]) -> dict[str, int]:
    """Sizes of the RAM struct macros, counted from their own declarations."""
    out: dict[str, int] = {}
    for path in sorted((root / 'macros').glob('*.asm')):
        text = path.read_text()
        for match in re.finditer(r'^MACRO\??\s+(\w+)\n(.*?)^ENDM', text, re.M | re.S):
            name, body = match.group(1), match.group(2)
            total = 0
            for line in body.splitlines():
                line = line.split(';')[0].strip()
                # A struct macro labels each field with its argument
                # (`\\1YCoord:: db`), so the label is stripped and the
                # declaration after it is what reserves the bytes.
                line = re.sub(r'^[\\\w{}:.\d]+::\s*', '', line)
                if not line or line.startswith(('IF', 'ELSE', 'ENDC', 'ASSERT')):
                    continue
                if re.match(r'^\w+::?$', line):
                    continue
                if line.startswith('db'):
                    total += max(1, len([part for part in line[2:].split(',') if part.strip()]))
                elif line.startswith('dw'):
                    total += 2 * max(1, len([p for p in line[2:].split(',') if p.strip()]))
                elif line.startswith('ds '):
                    try:
                        total += size_of(line[3:], known)
                    except Unevaluable:
                        total = None
                        break
                else:
                    total = None
                    break
            if total is not None:
                out[name] = total
    return out


def walk(
    root: Path, table: dict[str, int], known: dict[str, int], verbose: bool = False
) -> tuple[dict[str, int], list[str], int]:
    """Resolve every symbol of wram.asm the anchored cursor can reach exactly."""
    macros = macro_sizes(root, known)
    lines = (root / 'ram/wram.asm').read_text().splitlines()
    cursor: int | None = None
    resolved: dict[str, int] = {}
    # Symbols counted since the last pinned anchor, held back until a pinned
    # address after them agrees.
    pending_run: dict[str, int] = {}
    order: list[str] = []
    checked = 0
    problems: list[str] = []
    lost: list[str] = []
    # UNION frames: (start address, widest branch so far, whether a branch was
    # unevaluable). A frame whose start is unknown, or any one of whose branches this
    # tool could not size, poisons the whole union: what the section advances by is
    # the widest branch, so one branch it cannot measure means it cannot measure any.
    unions: list[tuple[int | None, int, bool]] = []
    index = 0
    while index < len(lines):
        raw = lines[index]
        index += 1
        line = raw.split(';')[0].strip()
        if not line:
            continue
        if line.startswith('SECTION'):
            # A section's address comes from the linker, not the source. WRAM0
            # sections are packed in declaration order, so the cursor carries on
            # -- and the next address `symbols.rs` pins is what tests that: any
            # padding the linker inserted would land as a MISMATCH and this tool
            # would emit nothing.
            continue
        if line == 'UNION':
            unions.append((cursor, 0, False))
            continue
        if line == 'NEXTU':
            if not unions:
                cursor = None
                continue
            start, widest, poisoned = unions.pop()
            poisoned = poisoned or start is None or cursor is None
            if not poisoned:
                widest = max(widest, cursor - start)
            unions.append((start, widest, poisoned))
            cursor = start
            continue
        if line == 'ENDU':
            if not unions:
                cursor = None
                continue
            start, widest, poisoned = unions.pop()
            if poisoned or start is None or cursor is None:
                cursor = None
                continue
            cursor = start + max(widest, cursor - start)
            continue
        if line.startswith(('FOR ', 'REPT ')):
            # Evaluate the body only when every line of it has a known size.
            head = line.split(None, 1)[1]
            # `REPT n`, `FOR v, stop` and `FOR v, start, stop`: rgbasm's own
            # three forms, and the third iterates stop - start times.
            arguments = [part.strip() for part in head.split(',')]
            count_token = arguments[-1]
            start_token = arguments[-2] if line.startswith('FOR ') and len(arguments) == 3 else None
            body: list[str] = []
            depth = 1
            while index < len(lines):
                inner = lines[index].split(';')[0].strip()
                index += 1
                if inner.startswith(('FOR ', 'REPT ')):
                    depth += 1
                if inner == 'ENDR':
                    depth -= 1
                    if depth == 0:
                        break
                body.append(inner)
            try:
                count = size_of(count_token, known)
                if start_token is not None:
                    count -= size_of(start_token, known)
                per = 0
                for inner in body:
                    per += declaration_size(inner, known, macros)
                if cursor is not None:
                    cursor += count * per
            except Unevaluable:
                if cursor is not None:
                    lost.append(f'line {index}: {line}')
                cursor = None
                pending_run.clear()
            continue
        label = re.match(r'^(w\w+)::', line)
        if label is not None:
            name = label.group(1)
            if name in table:
                if cursor is not None and cursor != table[name]:
                    problems.append(
                        f'{name}: wram.asm gives ${cursor:04x}, symbols.rs pins ${table[name]:04x}'
                    )
                    pending_run.clear()
                elif cursor is not None:
                    checked += 1
                    # Everything counted since the last anchor is now bracketed
                    # by two addresses the table already carries.
                    resolved.update(pending_run)
                    order.extend(pending_run)
                    pending_run.clear()
                else:
                    if verbose:
                        lost.append(f'cold anchor at {name} (line {index})')
                    pending_run.clear()
                cursor = table[name]
            elif cursor is not None:
                pending_run[name] = cursor
            line = line[label.end() :].strip()
            if not line:
                continue
        if line.endswith('::') or re.fullmatch(r'\.\w+', line):
            continue
        try:
            if cursor is not None:
                cursor += declaration_size(line, known, macros)
        except Unevaluable:
            if cursor is not None:
                lost.append(f'line {index}: {line}')
            cursor = None
            pending_run.clear()
    if verbose:
        for entry in lost:
            print(f'UNEVALUABLE {entry}')
    return resolved, problems, checked


def declaration_size(line: str, known: dict[str, int], macros: dict[str, int]) -> int:
    """Bytes one declaration line reserves, or [`Unevaluable`]."""
    line = line.split(';')[0].strip()
    if not line or line.endswith('::') or line.startswith(('ENDSECTION', 'ASSERT', 'ENDR')):
        return 0
    # Any label, including the `{02d:n}` interpolations a FOR body labels its
    # iterations with; what reserves the bytes is the declaration after it.
    line = re.sub(r'^[\\\w{}:.\d]+::\s*', '', line)
    if not line:
        return 0
    if line == 'db':
        return 1
    if line == 'dw':
        return 2
    if line.startswith('db '):
        return max(1, len([part for part in line[3:].split(',') if part.strip()]))
    if line.startswith('dw '):
        return 2 * max(1, len([part for part in line[3:].split(',') if part.strip()]))
    if line.startswith('ds '):
        return size_of(line[3:], known)
    if line.startswith('flag_array '):
        return math.ceil(size_of(line[len('flag_array ') :], known) / 8)
    head = line.split(None, 1)[0]
    if head in macros:
        return macros[head]
    raise Unevaluable(line)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pokered', type=Path, required=True)
    parser.add_argument('--verbose', action='store_true', help='name every declaration it will not size')
    parser.add_argument('--emit', action='store_true', help='write the new addresses into symbols.rs')
    args = parser.parse_args()

    text = SYMBOLS.read_text()
    commit = re.search(r'POKERED_COMMIT: &str = "([0-9a-f]{40})"', text)
    if commit is None:
        raise SystemExit('symbols.rs carries no POKERED_COMMIT')
    head = (args.pokered / '.git/HEAD').read_text().strip()
    if head.startswith('ref:'):
        head = (args.pokered / '.git' / head.split()[1]).read_text().strip()
    if head != commit.group(1):
        raise SystemExit(
            f'checkout is at {head}, symbols.rs pins {commit.group(1)}: two revisions of the '
            'disassembly renumber RAM relative to each other'
        )

    table = pinned(text)
    known = constants(args.pokered)
    resolved, problems, checked = walk(args.pokered, table, known, args.verbose)
    if problems:
        for problem in problems:
            print(f'MISMATCH {problem}')
        raise SystemExit('the walk disagrees with symbols.rs; nothing emitted')
    print(f'{checked} of {len(table)} pinned addresses re-derived from wram.asm, no disagreement')

    missing = [name for name in WANTED if name not in resolved]
    if missing:
        raise SystemExit(f'unanchored, so not resolved: {", ".join(missing)}')
    for name in WANTED:
        print(f'{name} = ${resolved[name]:04x}  ({WANTED[name]})')

    if not args.emit:
        return
    block = re.search(r'(pub mod ram \{\n)(.*?)(\n\}\n)', text, re.S)
    if block is None:
        raise SystemExit('no ram block in symbols.rs')
    rows = []
    for row in block.group(2).splitlines():
        name = re.match(r'\s*pub const (w\w+): u16 = 0x([0-9a-f]{4});', row)
        if name is None:
            continue
        rows.append((int(name.group(2), 16), name.group(1)))
    for name in WANTED:
        if name not in table:
            rows.append((resolved[name], name))
    rows = sorted(set(rows))
    width = max(len(name) for _, name in rows)
    body = '\n'.join(
        f'    pub const {name}: u16 = 0x{address:04x};'.ljust(width + 31)
        + f'// {address}'
        for address, name in rows
    )
    SYMBOLS.write_text(text[: block.start(2)] + body + text[block.end(2) :])
    print(f'symbols.rs: {len(rows)} addresses written')


if __name__ == '__main__':
    main()
