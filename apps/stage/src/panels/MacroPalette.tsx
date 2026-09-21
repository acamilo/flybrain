import { type PaletteCell } from '@flybrain/feed';

import { useStage } from '@/feed/store';
import {
  MACRO_CELL_COLUMNS,
  MACRO_CELL_COUNT,
  MACRO_CELL_GAP,
  MACRO_CELL_HEIGHT,
  MACRO_CELL_ROWS,
  MACRO_CELL_WIDTH,
  MACRO_PALETTE_HEIGHT,
  MACRO_PALETTE_PAD,
  MACRO_PALETTE_WIDTH,
} from '@/lib/geometry';
import { shortMacroName } from '@/lib/macro-names';
import { MacroCell } from './MacroCell';

/**
 * The pad: the macro buttons the scene has put on the pad right now.
 *
 * The operator, 2026-09-16, on where it goes: "slide the fly over and put the macro palette right
 * next to it" — so it is the right-hand part of the strip under the Game Boy screen, sharing the
 * fly's baseline and the strip's one frame (`docs/design/macros.md` section 6). Section 14,
 * decided 2026-09-17, is its shape: **two columns of seven**, one cell per button on the pad, up
 * to fourteen, each with its type's channel tag as its glyph and a short name beside it.
 *
 * It is the pad and not the keyboard, and that is the decision rather than an omission. Section
 * 14's first sketch drew every type here, lit when bound and dim when not; at the 24 px body floor
 * that is three columns of eleven in a 208 px strip, which does not exist — so the whole keyboard moved
 * to the rail's MACROS tab (`tabs/MacrosTab.tsx`), where a cell is 332 px and a row is 43, and the
 * strip kept the thing a viewer watching the game needs: what the fly can press *here*. The two
 * are the same cell (`MacroCell.tsx`) at two sizes, so the pad is a crop of the keyboard rather
 * than a second idiom.
 *
 * Fixed order by type, so a cell never moves for a reason a viewer cannot see: the cells are the
 * contract's type order (`packages/feed/src/palette.ts` sorts them), packed into the grid
 * column-major. A scene binding four macros draws four cells; the fifteenth of a scene that
 * somehow bound fifteen is on the MACROS tab and not here. Nothing on this panel ranks or weighs
 * anything: there is no plan, no prior and no cursor to draw.
 *
 * Three states, and the frame, the baseline and the fly's box are identical in all three so
 * nothing on air jumps:
 *
 *   - **macros mode.** The scene's pad, re-dealt on every scene change with the rail's own stepped
 *     arrival (`src/theme/motion.css`) — which is also how a cell appears and disappears, since a
 *     scene change is the only thing that adds or removes one — the running macro's cell bright,
 *     its outcome shown for a beat.
 *   - **raw mode.** One dim cell saying RAW, thirteen empty ones. The mode is a config knob on the
 *     service (`docs/design/macros.md` section 1) and flipping it must not re-lay out the
 *     broadcast.
 *   - **an older service.** Identical to raw mode: no macro fields means raw.
 *
 * What is *not* here is any decision. The cells are the sim's macros verbatim, the bright cell is
 * the sim's `macro`, and the page cannot select one — there is no button path from the page at all.
 */
export function MacroPalette() {
  const mode = useStage((state) => state.macroMode);
  const scene = useStage((state) => state.scene);
  const cells = useStage((state) => state.palette);

  return (
    <div
      className="macro-grid macro-palette"
      style={{
        width: MACRO_PALETTE_WIDTH,
        height: MACRO_PALETTE_HEIGHT,
        flex: `0 0 ${MACRO_PALETTE_WIDTH}px`,
        padding: MACRO_PALETTE_PAD,
        gap: MACRO_CELL_GAP,
        gridTemplateRows: `repeat(${MACRO_CELL_ROWS}, ${MACRO_CELL_HEIGHT}px)`,
        gridTemplateColumns: `repeat(${MACRO_CELL_COLUMNS}, ${MACRO_CELL_WIDTH}px)`,
      }}
      data-testid="macro-palette"
      /* Both macro grids carry this, and it is what the paint loop collects: the pad and the
         keyboard light the same running macro, and neither should have to be named in `App.tsx`
         for that to keep working. */
      data-macro-cells="pad"
      data-mode={mode}
      data-scene={scene}
      /* The sixth critical region, in macros mode only: 24 px type in 28 px cells is the tightest
         thing on the page after the ladder, and `tests/e2e/legibility.spec.ts` measures it at the
         0.31 phone downscale like the rest. In raw mode there is nothing here to read — one dim
         word holding the geometry — so it does not claim to be a readable region, and the
         contrast floor is not asked a question whose answer would be meaningless. */
      data-legible={mode === 'raw' ? undefined : 'critical'}
    >
      {/* Keyed on the scene *and* the mode, so React remounts the cells and the deal animation
          runs once per change rather than on every commit — the same trick the mode chip uses. */}
      {mode === 'raw' ? <RawCells key={`${mode}:raw`} /> : <Cells key={`${scene}:${mode}`} cells={cells} />}
    </div>
  );
}

/**
 * The pad's cells: the bound macros, in type order, capped at the grid's fourteen.
 *
 * `paletteView` packs the bound macros into its leading rows in type order
 * (`packages/feed/src/palette.ts`), so filtering is all this has to do — and it filters rather
 * than slicing the view, because the contract's own shape is free to change under it: a producer
 * that gives every type a row of its own (one cell per type, in flight for the nine new types of
 * sections 13 and 14) sends the same pad with the empty cells interleaved, and this draws the same
 * fourteen either way.
 */
function Cells({ cells }: { cells: readonly PaletteCell[] }) {
  const bound = cells.filter((cell) => cell.entry !== null).slice(0, MACRO_CELL_COUNT);
  return (
    <>
      {bound.map((cell, index) => (
        <MacroCell
          key={cell.entry?.name ?? index}
          /* `cell.row` is the macro's type index on the wire — that is what the feed carries and
             what `paintPalette` matches against, so the cell's `data-macro-row` stays the type
             index rather than the cell's linear place in this grid. */
          index={cell.row}
          channel={cell.channel}
          name={cell.entry?.name ?? ''}
          label={shortMacroName(cell.entry?.name ?? '')}
          bound
          height={MACRO_CELL_HEIGHT}
          dealMs={index * 40}
          slot={index}
        />
      ))}
    </>
  );
}

/**
 * Raw mode: the strip's frame with one dim RAW cell.
 *
 * The other thirteen are empty boxes rather than absent, because the strip's geometry is asserted
 * against `src/lib/geometry.ts` and a mode flip that moved the fly or the baseline would be a
 * different layout on air every time the operator changed a config line.
 */
function RawCells() {
  return (
    <>
      {Array.from({ length: MACRO_CELL_COUNT }, (_, row) => (
        <div
          key={row}
          className="macro-cell"
          style={{ height: MACRO_CELL_HEIGHT, ['--deal' as string]: `${row * 40}ms` }}
          data-macro-row={row}
          data-macro-name=""
          data-bound="0"
          data-raw={row === 0 ? '1' : '0'}
        >
          {row === 0 ? (
            <>
              <span className="macro-cell__glyph" />
              <span className="macro-cell__name">RAW</span>
            </>
          ) : null}
        </div>
      ))}
    </>
  );
}
