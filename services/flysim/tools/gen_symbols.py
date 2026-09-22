"""Regenerate the Pokemon Red RAM/event symbol table for flybrain-gb.

The prototype at ~/fly-plays-pokemon owns the extraction from the pret/pokered
disassembly (tools/build_reward_symbols.py) and is read-only to us, so this
script does not re-derive anything: it parses the prototype's already-generated
src/reward/symbols.ts and re-emits the identical addresses, flags, milestone
list and pokered commit as Rust.

Run with `uv run python3` (plain python3 is blocked on this box):

    uv run python3 services/flysim/tools/gen_symbols.py

Optional: --prototype <path> to point at a different prototype checkout.
EVENTS is emitted as an ORDERED slice, not a map: pokemon-red.ts iterates
Object.entries(EVENTS) and that order decides the order simultaneous payouts
appear in a frame, which is observable in the recent-event ticker.

## Extra flags the prototype never selected

The prototype's selection predates the 38-rung milestone ladder
(`docs/design/ladder.md`), so a ladder rung can name a flag that is simply
absent from symbols.ts. `EXTRA_EVENTS` / `EXTRA_MILESTONES` below are a small
explicit allowlist for exactly those, resolved by name against
`constants/event_constants.asm` in a pret/pokered checkout rather than
hand-numbered — the bit index of an event flag is positional, so writing one out
by hand is the one mistake that cannot be caught by review.

The checkout is found at --pokered (default: the reference tree the
fly-plays-pokemon prototype pins) and its commit MUST equal the POKERED_COMMIT
the prototype recorded, or this script refuses to run: two different revisions of
the disassembly renumber flags relative to each other. As a second check, every
flag the prototype and the .asm both define must agree on its bit index.

    uv run python3 services/flysim/tools/gen_symbols.py \
        --pokered ~/fly-plays-pokemon/.tools/pokered-reference
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

DEFAULT_PROTOTYPE = Path.home() / 'fly-plays-pokemon'
DEFAULT_POKERED = DEFAULT_PROTOTYPE / '.tools/pokered-reference'
OUTPUT = Path(__file__).resolve().parents[1] / 'crates/flybrain-gb/src/pokemon_red/symbols.rs'

#: Event flags the milestone ladder needs that the prototype's selection omits.
#: Keep this list as short as the ladder allows; anything the prototype already
#: selects must NOT be repeated here.
EXTRA_EVENTS = (
    # Ladder rung 16 ("MET BILL"): Bill hands over the S.S. Ticket once the cell
    # separator has been used on him (scripts/BillsHouse.asm). This is the gate
    # for the S.S. Anne, so it is the rung's progression condition.
    'EVENT_GOT_SS_TICKET',
)

#: Which of EXTRA_EVENTS are story beats rather than trainer wins.
EXTRA_MILESTONES = ('EVENT_GOT_SS_TICKET',)

#: WRAM addresses the ladder needs that the prototype's selection omits. Resolved
#: from the decomp's `pokered.sym`, never written out by hand.
EXTRA_RAM = (
    # Ladder rung 37 ("CHAMPION"). HallOfFame.asm resets the whole Indigo Plateau
    # event range, EVENT_BEAT_CHAMPION_RIVAL included, as the player is entered
    # into the Hall of Fame -- so no event flag survives the milestone it marks.
    # wNumHoFTeams is the counter HallOfFamePC increments and it is saved, so it
    # is the one durable "has been Champion" signal on the cartridge.
    'wNumHoFTeams',
    # Boundary rewards (`docs/design/room-escape.md` §2). The current map's warp
    # table: wNumberOfWarps counts the entries (up to MAX_WARP_EVENTS = 32,
    # constants/map_data_constants.asm) and wWarpEntries holds them as four bytes
    # each -- Y, X, destination warp id, destination map id (ram/wram.asm's own
    # comment). CheckWarpsCollision compares those Y and X against wYCoord and
    # wXCoord directly, so warp coordinates are in the same tile space the adapter
    # already samples.
    'wNumberOfWarps',
    'wWarpEntries',
    # Which map edges are exits: a bitmask over EAST 1, WEST 2, SOUTH 4, NORTH 8
    # (constants/map_data_constants.asm). The design calls this wMapConnections;
    # the symbol at this commit is wCurMapConnections, and the four per-direction
    # headers are wNorthConnectionHeader and friends, which the adapter does not
    # need: the bitmask plus the map dimensions locate every boundary tile.
    'wCurMapConnections',
    # Scene detection and the macro palette's state accessors
    # (`docs/design/macros.md` section 8, `docs/design/macros-wram.md`). Every name
    # below is resolved from pokered.sym like the rest of this list; the evidence for
    # what each one means is in macros-wram.md, one row per symbol.
    #
    # The screen's tile buffer, 20x18 background tile ids. The walkable predicate
    # reads it because it is the only WRAM-resident source of loaded map tiles:
    # _GetTileAndCoordsInFrontOfPlayer indexes it at (8, 9) for the tile the player
    # stands on and (8, 11) / (8, 7) / (6, 9) / (10, 9) for the four neighbours, which
    # pins the mapping from map coordinates to screen coordinates exactly.
    'wTileMap',
    # Sprite slots 0..15, 16 bytes each. Slot 0 is the player. Facing direction is
    # byte 9 of the wSpriteStateData1 struct (SPRITE_FACING_DOWN/UP/LEFT/RIGHT =
    # 0/4/8/12) and the map coordinates are bytes 4 and 5 of the wSpriteStateData2
    # struct, both stored +4 because `MACRO object_event` emits `db \2 + 4` then
    # `db \1 + 4` (macros/scripts/maps.asm).
    'wSpriteStateData1',
    'wSpriteStateData2',
    'wNumSprites',
    # The current map's sign (bg_event) table, for `GO ITEM`'s text-event tiles:
    # wNumSigns counts the entries, up to MAX_BG_EVENTS = 16, wSignCoords holds them
    # as Y, X pairs and wSignTextIDs the text id of each. `MACRO bg_event x, y, text`
    # emits `db \2, \1, \3` -- Y first and *no* +4 bias, unlike object_event -- and
    # IsSpriteOrSignInFrontOfPlayer compares those bytes against the coordinates
    # GetTileAndCoordsInFrontOfPlayer returns, so they are in the same tile space as
    # wXCoord / wYCoord (home/overworld.asm).
    'wNumSigns',
    'wSignCoords',
    'wSignTextIDs',
    # HandleMenuInput's state, shared by every menu in the game. The scene detector
    # reads the geometry rather than a "menu is open" flag because pokered has none:
    # the battle menu is (y=14, x=9 or 15, max=1), the move list (y=12, x=5) and the
    # party list (y=1, x=0, max=party-1); wMenuWatchedKeys distinguishes a party list
    # that can be backed out of (PAD_A | PAD_B) from a forced switch (PAD_A alone,
    # which PartyMenuInit sets from wForcePlayerToChooseMon).
    'wTopMenuItemY',
    'wTopMenuItemX',
    'wCurrentMenuItem',
    'wMaxMenuItem',
    'wMenuWatchedKeys',
    'wForcePlayerToChooseMon',
    # Which text box or menu template was drawn last: BATTLE_MENU_TEMPLATE ($0b) is
    # the battle menu and BUY_SELL_QUIT_MENU ($15) is the mart, and the mart is its
    # only user in the game (constants/menu_constants.asm, engine/events/pokemart.asm).
    'wTextBoxID',
    # Which list menu is up: PRICEDITEMLISTMENU ($02) is the mart's buy list,
    # ITEMLISTMENU ($03) the bag list (constants/list_constants.asm).
    'wListMenuID',
    # NORMAL_PARTY_MENU ($00) against BATTLE_PARTY_MENU ($02): ChooseNextMon sets the
    # latter, and it is what tells a forced switch from the player opening PKMN.
    'wPartyMenuTypeOrMessageID',
    # BIT_USING_GENERIC_PC (bit 3): ActivatePC sets it, LogOff clears it
    # (engine/menus/pc.asm), so it is the PC scene, Bill's and the player's alike.
    'wMiscFlags',
    # Set while the game is driving the player through a scripted walk; the scene is
    # not the fly's to act on then (CollisionCheckOnLand skips collision entirely).
    'wSimulatedJoypadStatesIndex',
    # The party: a count, the species list, and six 44-byte party_struct entries
    # (PARTYMON_STRUCT_LENGTH = $2c, wPartyMon2 - wPartyMon1 = $2c).
    'wPartySpecies',
    'wPartyMon1',
    # The active battler is a copy of its party entry, and it is the copy the battle
    # engine damages, so "own HP" in a battle is this one.
    'wBattleMonSpecies',
    'wBattleMonHP',
    'wBattleMonStatus',
    'wBattleMonMoves',
    'wBattleMonLevel',
    'wBattleMonMaxHP',
    'wBattleMonPP',
    'wPlayerMonNumber',
    'wNumMovesMinusOne',
    # Money is three bytes of big-endian BCD; the bag is a count, up to
    # BAG_ITEM_CAPACITY = 20 (id, quantity) pairs, and a $ff terminator.
    'wPlayerMoney',
    'wNumBagItems',
    'wBagItems',
    # The current tileset's list of passable tile ids: a pointer, little-endian, into
    # the collision tables in ROM bank 0 (Overworld_Coll and friends all resolve to
    # 00:17xx in pokered.sym), each a list of tile ids terminated by $ff.
    # CheckTilePassable walks exactly that list.
    'wTilesetCollisionPtr',
    # Shops and Pokemon Centers (`docs/design/macros.md` section 13).
    #
    # The open mart's inventory. LoadItemList (home/text_script.asm) copies the `script_mart`
    # list out of the clerk's text script into wItemList the moment the counter opens: a count
    # byte (_NARG), then the item ids, then $ff. DisplayPokemartDialogue_ points wListPointer at
    # the same buffer for PRICEDITEMLISTMENU, so an item's position in it is its cursor index in
    # the buy list -- which is what makes a purchase navigable by cursor rather than by counting
    # presses. wItemList is `ds 16`, so the buffer bounds the list as well as its terminator does.
    'wItemList',
    # The current tileset's three counter tile ids, loaded from the tileset header
    # (data/tilesets/tileset_headers.asm: `tileset Mart, $18,$19,$1E` and the same three for
    # Pokecenter; -1 means "no counter tile"). IsSpriteOrSignInFrontOfPlayer's
    # `.extendRangeOverCounter` branch walks exactly this list and doubles the talking range when
    # the tile in front of the player is one of them, which is the only reason a clerk or a nurse
    # standing behind a desk can be talked to at all: none of the four tiles around either of
    # them is walkable.
    'wTilesetTalkingOverTiles',
    # The whole-map walkability grid (docs/design/macros.md section 15).
    #
    # LoadTileBlockMap copies the loaded map out of its ROM bank into wOverworldMap
    # as one byte per 4x4-tile block, in rows of wCurMapWidth + MAP_BORDER * 2 with
    # the map itself three rows and three columns in, so the blocks of the current
    # map are a WRAM read rather than a ROM one. wCurMapTileset keys the tile-pair
    # collision lists (CheckForTilePairCollisions). wTilesetBank and
    # wTilesetBlocksPtr are the tileset header's blockset: 16 bytes per block id,
    # four rows of four tile ids, which DrawTileBlock indexes exactly that way --
    # and it is not in bank 0, which is why the memory seam grew a bank-aware ROM
    # read for it. All four are resolved the same way every other name here is;
    # services/flysim/tools/resolve_wram.py is the second reading of them, from
    # ram/wram.asm at this commit, and it re-derives 40 of the addresses this table
    # already carries before it emits one of these four.
    'wOverworldMap',
    'wCurMapTileset',
    'wTilesetBank',
    'wTilesetBlocksPtr',
    # The catch reward (`docs/rewards-learning.md`, `docs/design/macros-wram.md` section 2).
    # ram/wram.asm's own comment is "0 if no mon was captured": ItemUseBall zeroes it before
    # every throw and writes wEnemyMonSpecies into it only on the branch that keeps the
    # Pokemon, and UseBagItem zeroes it again on the way out of the battle. It is the
    # cartridge's own answer to "was this one caught", and the only signal that needs no
    # second rule to tell a catch apart from a gift, a trade or an evolution.
    # services/flysim/tools/resolve_wram.py is the second reading of it, from ram/wram.asm at
    # this commit, bracketed by wFontLoaded and wForcePlayerToChooseMon.
    'wCapturedMonSpecies',
)


def parse_ts(text: str) -> tuple[str, dict[str, int], list[tuple[str, int]], list[str]]:
    """Pull POKERED_COMMIT, RAM, EVENTS and MILESTONES out of the TS module."""

    def block(name: str) -> str:
        match = re.search(
            rf'export const {name} = (\{{.*?\}}|\[.*?\]) as const;', text, re.S
        )
        if match is None:
            raise SystemExit(f'{name} not found in symbols.ts')
        return match.group(1)

    commit_match = re.search(r'export const POKERED_COMMIT = "([0-9a-f]{40})";', text)
    if commit_match is None:
        raise SystemExit('POKERED_COMMIT not found in symbols.ts')

    ram = json.loads(block('RAM'))
    milestones = json.loads(block('MILESTONES'))
    # json.loads on an object loses nothing in Python 3.7+ (dicts keep insertion
    # order) and the TS module is machine-generated JSON, so this preserves the
    # declaration order that decides payout order.
    events = list(json.loads(block('EVENTS')).items())
    return commit_match.group(1), ram, events, milestones


def rgbds_int(text: str) -> int:
    """Evaluate an rgbds integer expression: decimals, $hex, and + - * ( )."""
    expr = re.sub(r'\$([0-9A-Fa-f]+)', lambda m: str(int(m.group(1), 16)), text.strip())
    if not re.fullmatch(r'[0-9+\-*/() ]+', expr):
        raise SystemExit(f'unsupported rgbds expression: {text!r}')
    return int(eval(expr, {'__builtins__': {}}, {}))  # noqa: S307 - grammar checked above


def parse_event_constants(text: str) -> dict[str, int]:
    """Bit index of every event flag in `constants/event_constants.asm`.

    A tiny interpreter for the four `macros/const.asm` directives the file uses:
    an event flag's bit index is its position in this enumeration, so `const_skip`
    and `const_next` have to be honoured exactly.
    """
    value, inc, out = 0, 1, {}
    for raw in text.splitlines():
        line = raw.split(';')[0].strip()
        if not line:
            continue
        head, _, rest = line.partition(' ')
        arg = rest.strip()
        if head == 'const_def':
            args = [piece.strip() for piece in arg.split(',')] if arg else []
            value = rgbds_int(args[0]) if args and args[0] else 0
            inc = rgbds_int(args[1]) if len(args) >= 2 else 1
        elif head in ('const', 'const_export'):
            out[arg.split(',')[0].strip()] = value
            value += inc
        elif head == 'const_skip':
            value += inc * (rgbds_int(arg) if arg else 1)
        elif head == 'const_next':
            value = rgbds_int(arg)
    return out


def pokered_head(checkout: Path) -> str:
    """The commit `checkout` is on, read from .git without running git."""
    head = (checkout / '.git/HEAD').read_text().strip()
    if not head.startswith('ref:'):
        return head
    ref = (checkout / '.git' / head.removeprefix('ref:').strip()).read_text().strip()
    return ref


def render(commit: str, ram: dict[str, int], events: list[tuple[str, int]], milestones: list[str]) -> str:
    out: list[str] = []
    out.append('//! Pokemon Red RAM addresses and named event bits.')
    out.append('//!')
    out.append('//! Generated by services/flysim/tools/gen_symbols.py from the')
    out.append("//! fly-plays-pokemon prototype's src/reward/symbols.ts, which")
    out.append('//! tools/build_reward_symbols.py generated from the pret/pokered')
    out.append('//! disassembly, plus that script\'s EXTRA_RAM / EXTRA_EVENTS')
    out.append('//! allowlist, resolved from the disassembly at POKERED_COMMIT.')
    out.append('//! Do not hand-edit addresses.')
    out.append('#![allow(dead_code, non_upper_case_globals)]')
    out.append('')
    out.append('/// pret/pokered revision these symbols were extracted from.')
    out.append(f'pub const POKERED_COMMIT: &str = "{commit}";')
    out.append('')
    out.append('/// Reviewed WRAM addresses, by their pokered symbol names.')
    out.append('pub mod ram {')
    width = max(len(name) for name in ram)
    for name, address in sorted(ram.items(), key=lambda item: item[1]):
        pad = ' ' * (width - len(name))
        out.append(f'    pub const {name}: u16 = 0x{address:04x};{pad}  // {address}')
    out.append('}')
    out.append('')
    out.append('/// Named event-flag bit indices.')
    out.append('pub mod events {')
    for name, bit in events:
        out.append(f'    pub const {name}: u16 = {bit};')
    out.append('}')
    out.append('')
    out.append('/// Every selected event flag, in pokered declaration order.')
    out.append('///')
    out.append('/// The order matters: the adapter walks this slice, so it decides')
    out.append('/// which of several simultaneous payouts is emitted first.')
    out.append(f'pub const EVENTS: [(&str, u16); {len(events)}] = [')
    for name, bit in events:
        out.append(f'    ("{name}", {bit}),')
    out.append('];')
    out.append('')
    out.append('/// Event flags classified as story milestones rather than trainer wins.')
    out.append(f'pub const MILESTONES: [&str; {len(milestones)}] = [')
    for name in milestones:
        out.append(f'    "{name}",')
    out.append('];')
    out.append('')
    return '\n'.join(out)


def parse_sym(text: str) -> dict[str, int]:
    """WRAM symbol addresses out of an rgblink `.sym` file (`00:d356 wName`)."""
    out: dict[str, int] = {}
    for line in text.splitlines():
        match = re.fullmatch(r'([0-9A-Fa-f]{2}):([0-9A-Fa-f]{4})\s+(\w+)', line.strip())
        if match is not None:
            out.setdefault(match.group(3), int(match.group(2), 16))
    return out


def merge_extras(
    commit: str,
    pokered: Path,
    ram: dict[str, int],
    events: list[tuple[str, int]],
    milestones: list[str],
) -> tuple[dict[str, int], list[tuple[str, int]], list[str]]:
    """Fold EXTRA_* into the prototype's selection, resolved from the decomp."""
    if not pokered.is_dir():
        raise SystemExit(
            f'pokered checkout not found: {pokered}\n'
            'It is needed to resolve EXTRA_EVENTS; pass --pokered, or clone\n'
            f'https://github.com/pret/pokered and check out {commit}.'
        )
    head = pokered_head(pokered)
    if head != commit:
        raise SystemExit(
            f'pokered checkout is at {head}, but the prototype recorded {commit}.\n'
            'Event flag bit indices are positional, so the two revisions cannot be mixed.'
        )

    decomp = parse_event_constants((pokered / 'constants/event_constants.asm').read_text())
    disagree = {
        name: (bit, decomp[name]) for name, bit in events if name in decomp and decomp[name] != bit
    }
    if disagree:
        raise SystemExit(f'prototype and decomp disagree on event bits: {disagree}')

    # The .sym file is an rgblink build artifact, which is the only machine-readable
    # source for a WRAM address: wram.asm states sizes, not addresses.
    sym_path = pokered / 'pokered.sym'
    symbols = parse_sym(sym_path.read_text()) if sym_path.is_file() else {}
    if symbols:
        wrong = {
            name: (address, symbols[name])
            for name, address in ram.items()
            if name in symbols and symbols[name] != address
        }
        if wrong:
            raise SystemExit(f'prototype RAM addresses disagree with {sym_path}: {wrong}')

    merged_ram = dict(ram)
    for name in EXTRA_RAM:
        if name in merged_ram:
            raise SystemExit(f'{name} is already in the prototype selection; drop it from EXTRA_RAM')
        if name not in symbols:
            raise SystemExit(
                f'{name} is not in {sym_path}. Build the decomp (`make` in the pokered\n'
                'checkout) so rgblink writes the symbol file, then rerun.'
            )
        merged_ram[name] = symbols[name]

    known = dict(events)
    merged_events = list(events)
    for name in EXTRA_EVENTS:
        if name in known:
            raise SystemExit(
                f'{name} is already in the prototype selection; drop it from EXTRA_EVENTS'
            )
        if name not in decomp:
            raise SystemExit(f'{name} is not an event constant at {commit}')
        merged_events.append((name, decomp[name]))
    # EVENTS is consumed in pokered declaration order, which for this file is
    # ascending bit index, so an appended flag has to be re-sorted into place.
    merged_events.sort(key=lambda item: item[1])

    merged_milestones = list(milestones)
    for name in EXTRA_MILESTONES:
        if name not in dict(merged_events):
            raise SystemExit(f'{name} is in EXTRA_MILESTONES but not in EVENTS')
        if name not in merged_milestones:
            merged_milestones.append(name)

    return merged_ram, merged_events, merged_milestones


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--prototype', type=Path, default=DEFAULT_PROTOTYPE)
    parser.add_argument('--pokered', type=Path, default=DEFAULT_POKERED)
    parser.add_argument('--output', type=Path, default=OUTPUT)
    args = parser.parse_args()

    source = args.prototype / 'src/reward/symbols.ts'
    if not source.is_file():
        raise SystemExit(f'prototype symbols.ts not found: {source}')

    commit, ram, events, milestones = parse_ts(source.read_text())
    ram, events, milestones = merge_extras(commit, args.pokered, ram, events, milestones)
    missing = [name for name in milestones if name not in dict(events)]
    if missing:
        raise SystemExit(f'milestones absent from EVENTS: {missing}')

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(render(commit, ram, events, milestones))
    print(
        f'Wrote {args.output.relative_to(Path.cwd()) if args.output.is_relative_to(Path.cwd()) else args.output}: '
        f'{len(ram)} addresses, {len(events)} event flags, {len(milestones)} milestones from {commit}'
    )


if __name__ == '__main__':
    main()
