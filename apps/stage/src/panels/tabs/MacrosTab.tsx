import { MACRO_CHANNELS, MACRO_TYPES } from '@flybrain/feed';

import { useStage } from '@/feed/store';
import {
  MACRO_CELL_GAP,
  MACRO_PALETTE_PAD,
  MACROS_TAB_CELL_HEIGHT,
  MACROS_TAB_CELL_WIDTH,
  MACROS_TAB_COLUMNS,
  MACROS_TAB_ROWS,
} from '@/lib/geometry';
import { MacroCell } from '../MacroCell';

/**
 * The MACROS tab: the whole macro keyboard, three columns, every type there is.
 *
 * `docs/design/macros.md` section 14, decided 2026-09-17: "a new rail tab MACROS shows the whole
 * keyboard, three columns of eleven, bound cells lit and unbound dim, the running one bright".
 * Fifth in the rotation, after DESCRIBE.
 *
 * It exists because the pad under the game could not be both things at once. A viewer watching the
 * game needs to know what the fly can press *now*, which is the strip (`../MacroPalette.tsx`,
 * fourteen cells at most); a viewer working out what this stream *is* needs to know what the fly
 * can press at all, which is 31 buttons — three columns of eleven at the 24 px body floor, 366 px
 * of rows in a strip that has 208. Three columns in the slot's 1004x368 pane is the same grid with
 * room: a cell is 332 px and carries the contract's own name in full, not the pad's short form.
 *
 * **Every type, in the contract's order, whether or not anything is bound.** That is the
 * difference from the pad, and it is the point of the tab: the dim cells are the buttons this
 * scene has taken away, which is information the pad cannot show without spending the room its
 * bound cells need. The rows come from `MACRO_TYPES` rather than from the feed, so the keyboard is
 * complete on a cold open, in raw mode and on a producer that has never sent a palette — and the
 * layout grows a row on its own when the contract grows three types ({@link MACROS_TAB_ROWS}).
 *
 * Like DESCRIBE, nothing steers to this tab and no moment focuses it (`src/lib/tabs.ts`): the
 * keyboard is never what just happened.
 */
export function MacrosTab() {
  const mode = useStage((state) => state.macroMode);
  const scene = useStage((state) => state.scene);
  const cells = useStage((state) => state.palette);

  /**
   * Which types the scene has on the pad, by name.
   *
   * By name and not by row, because the row is the *feed's* shape and this grid's row is the
   * contract's: `paletteView` is free to pack the bound macros into its leading rows or to give
   * every type a row of its own, and the answer to "is this button on the pad" is the same either
   * way (`packages/feed/src/palette.ts`).
   */
  const bound = new Set(
    cells.flatMap((cell) => (cell.entry === null ? [] : [cell.entry.name])),
  );

  return (
    <div
      className="macro-grid macros-tab"
      style={{
        padding: MACRO_PALETTE_PAD,
        gap: MACRO_CELL_GAP,
        gridTemplateRows: `repeat(${MACROS_TAB_ROWS}, ${MACROS_TAB_CELL_HEIGHT}px)`,
        gridTemplateColumns: `repeat(${MACROS_TAB_COLUMNS}, ${MACROS_TAB_CELL_WIDTH}px)`,
      }}
      data-testid="macros-tab"
      /* Collected by `paintPalette` (`src/App.tsx`) exactly as the pad's grid is: one running
         macro lights its cell in both places, painted once. */
      data-macro-cells="keyboard"
      data-mode={mode}
      data-scene={scene}
    >
      {MACRO_TYPES.map((name, index) => (
        <MacroCell
          key={name}
          index={index}
          channel={MACRO_CHANNELS[index] ?? ''}
          name={name}
          label={name}
          bound={mode !== 'raw' && bound.has(name)}
          height={MACROS_TAB_CELL_HEIGHT}
          /* The deal is staggered down each column rather than over the whole keyboard: 31 cells
             at the pad's 40 ms would take 1240 ms to land, which is far longer than the rail's own
             arrival. A column's worth is 440. */
          dealMs={(index % MACROS_TAB_ROWS) * 40}
        />
      ))}
    </div>
  );
}
