/**
 * Reading the scene's macros out of a feed header.
 *
 * The five macro fields are optional and additive (`docs/design/macros.md` section 6): an older
 * producer omits them, a raw-mode producer sends an empty palette and a null macro, and the page
 * must draw the same layout either way so nothing jumps when the mode changes. Section 12 is what
 * the fields mean now: a macro type is a button with a channel of its own, and the scene decides
 * which of those buttons are on the pad. So every consumer goes through {@link paletteView},
 * which is total: it always returns {@link MACRO_SLOTS} rows, the scene's macros sorted into the
 * contract's type order with the empty rows after them, so a macro keeps its place on screen.
 *
 * It is also the one place that knows the wire spelling of these fields, which is why it exists as
 * a function rather than as field access at each use: `game.macroMode` is a qualified name because
 * `game.mode` was taken (see `types.ts`), and a rename is this file.
 */
import {
  MACRO_CHANNELS,
  MACRO_SLOTS,
  macroChannel,
  macroTypeIndex,
  type FeedHeader,
  type GameScene,
  type MacroMode,
  type MacroOutcome,
  type PaletteEntry,
  type RunningMacro,
} from './types';

/** One row of the macro strip, filled or not. */
export interface PaletteCell {
  /** 0..5 down the strip. Fixed, because the strip's geometry is fixed. */
  row: number;
  /** Where the macro sits in the contract's type order, or -1 on an empty row. */
  type: number;
  /** The channel tag drawn in the cell, e.g. `MB·ATK`. Empty on an empty row. */
  channel: string;
  /** The macro bound to this row, or null when the scene binds fewer than six. */
  entry: PaletteEntry | null;
}

/** A header's macro state, normalized and total. */
export interface PaletteView {
  scene: GameScene;
  mode: MacroMode;
  /** Exactly `MACRO_SLOTS` rows: the bound macros in type order, then empty rows. */
  cells: PaletteCell[];
  macro: RunningMacro | null;
  outcome: MacroOutcome | null;
  /** True when the producer carries the macro fields at all. */
  supported: boolean;
}

/**
 * Normalize a header's macro fields.
 *
 * Defensive about the values as well as their absence, because this runs on the broadcast page:
 * an entry with a slot outside 0..{@link MACRO_SLOTS}, a duplicate slot, a second entry of the
 * same type or a
 * non-string name is dropped rather than drawn, and macros sent in raw mode are ignored — the
 * mode is what the page says on air, so the two cannot be allowed to disagree.
 */
export function paletteView(header: Pick<FeedHeader, 'game'> | null | undefined): PaletteView {
  const game = header?.game;
  const mode: MacroMode = game?.macroMode === 'macros' ? 'macros' : 'raw';
  const supported = game?.macroMode !== undefined || game?.scene !== undefined;

  const bound: { entry: PaletteEntry; type: number; channel: string }[] = [];
  if (mode !== 'raw') {
    const slots = new Set<number>();
    const names = new Set<string>();
    for (const entry of game?.palette ?? []) {
      const slot = entry?.slot as number;
      if (!Number.isInteger(slot) || slot < 0 || slot >= MACRO_SLOTS || slots.has(slot)) continue;
      if (typeof entry.name !== 'string' || entry.name.length === 0 || names.has(entry.name)) continue;
      slots.add(slot);
      names.add(entry.name);
      // The tag belongs to the type, so a producer that omits it or sends a non-string still gets
      // the right glyph as long as the name is one the contract knows.
      const channel =
        typeof entry.channel === 'string' && entry.channel.length > 0 ? entry.channel : macroChannel(entry.name);
      bound.push({
        entry: { slot, name: entry.name, gloss: typeof entry.gloss === 'string' ? entry.gloss : '', channel },
        type: macroTypeIndex(entry.name),
        channel,
      });
    }
    // Type order, which is the screen's cell order. A name the contract does not have sorts after
    // the ones it does, in the order the producer sent it, rather than jumping to the top.
    bound.sort((a, b) => {
      const left = a.type === -1 ? Number.MAX_SAFE_INTEGER : a.type;
      const right = b.type === -1 ? Number.MAX_SAFE_INTEGER : b.type;
      return left - right || a.entry.slot - b.entry.slot;
    });
  }

  // Section 14: **one cell per macro type, always, in the fixed order.** A cell carries its type's
  // tag whether or not this scene binds it, so the strip can draw every button dim and light the
  // ones on the pad — and a cell never moves, which is what made the old shape unreadable: cells
  // were the *bound* macros packed into the first rows, so a precondition coming and going
  // reshuffled the whole strip between two frames.
  const byType = new Map(bound.map((cell) => [cell.type, cell]));
  const cells: PaletteCell[] = [];
  for (let row = 0; row < MACRO_SLOTS; row += 1) {
    const cell = byType.get(row);
    cells.push(
      cell
        ? { row, type: row, channel: cell.channel, entry: cell.entry }
        : { row, type: row, channel: MACRO_CHANNELS[row] ?? '', entry: null },
    );
  }
  // A name the contract does not know has no cell of its own; it goes into the first free one so
  // that a producer ahead of this page still draws something rather than nothing.
  let free = 0;
  for (const cell of bound.filter((entry) => entry.type === -1)) {
    while (free < cells.length && cells[free]?.entry !== null) free += 1;
    if (free >= cells.length) break;
    cells[free] = { row: free, type: -1, channel: cell.channel, entry: cell.entry };
  }

  return {
    scene: game?.scene ?? 'unknown',
    mode,
    cells,
    macro: (mode === 'raw' ? null : game?.macro) ?? null,
    outcome: (mode === 'raw' ? null : game?.macroOutcome) ?? null,
    supported,
  };
}
