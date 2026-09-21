/**
 * One macro cell, drawn the same way in the two places that draw macros.
 *
 * `docs/design/macros.md` section 14 split the macro screen in two: the pad under the game (the
 * buttons on the pad *now*, up to fourteen, `MacroPalette.tsx`) and the whole keyboard on the
 * rail's MACROS tab (every type there is, bound or not, `tabs/MacrosTab.tsx`). They are two grids
 * of one cell, and this is that cell — so the tag chip, the lit and outcome states, the deal
 * animation and the attributes the paint loop writes exist once and cannot drift into two
 * dialects of the same button.
 *
 * The cell is a channel tag and a name. The tag is the type's own
 * (`packages/feed/src/types.ts`, `game.palette[i].channel`) and is the glyph the paint loop
 * colours; the name is the contract's on the tab, where 332 px holds it, and
 * `shortMacroName`'s on the pad, where 179 px does not (`src/lib/macro-names.ts`).
 *
 * React owns the text, because it changes once per scene, and the paint loop owns `data-live` and
 * `data-outcome` (`paintPalette` in `src/App.tsx`) — the same split as the button row's afterglow:
 * at 30 Hz an attribute flip is free and a re-render is not.
 */
import { MACRO_CHANNEL_WIDTH, MACRO_NAME_WIDTH } from '@/lib/geometry';

export interface MacroCellProps {
  /**
   * The cell's index in its own grid, column-major.
   *
   * What it is *not* is the wire's `slot`: the pad packs the scene's bound macros into its
   * fourteen cells and the keyboard gives every type a cell of its own, so the index means "where
   * on this grid" and nothing else. The paint loop matches cells by name.
   */
  index: number;
  /** The type's channel tag, e.g. `MB·GOAL`. */
  channel: string;
  /** The contract's name, which is what the paint loop matches on. */
  name: string;
  /** What the cell draws: the name, or its short form on the pad. */
  label: string;
  /** True when this scene has the button on the pad. */
  bound: boolean;
  /** Cell height in authoring pixels: the pad's 28, or the keyboard's row. */
  height: number;
  /** Stagger for the deal animation, in ms (`src/theme/motion.css`). */
  dealMs: number;
  /**
   * The cell's place in its grid's linear draw order, used by `paintPalette` to aim the running
   * macro's spark burst. Pad cells carry it (a bound macro's slot is the cell's place); tab cells
   * do not (the burst belongs to the strip's geometry and the tab may not even be on screen).
   * Optional because the keyboard has no slot.
   */
  slot?: number;
}

export function MacroCell({ index, channel, name, label, bound, height, dealMs, slot }: MacroCellProps) {
  return (
    <div
      className="macro-cell"
      style={{ height, ['--deal' as string]: `${dealMs}ms` }}
      data-macro-row={index}
      /* The pad carries `data-macro-slot` so `paintPalette` can target the spark at this cell; the
         keyboard omits it, and the absence is what tells the paint loop the burst does not belong
         here. */
      {...(slot === undefined ? {} : { 'data-macro-slot': slot })}
      /* The macro's name, which is how the paint loop finds the cell to light: a type is a channel
         (`docs/design/macros.md` section 12), so the name identifies it and a stale outcome from
         the scene before cannot light whichever cell inherited its place. */
      data-macro-name={name}
      data-bound={bound ? '1' : '0'}
      data-live="0"
      data-outcome=""
    >
      {/* The channel tag: `MB·GOAL`, the button this cell is. Fixed width, because it is the
          cell's glyph column and every name in the grid has to start at the same x. */}
      <span className="macro-cell__glyph" style={{ width: MACRO_CHANNEL_WIDTH }}>
        {channel}
      </span>
      {/* `minWidth`, not `width`: the pad's names are capped to fit this column
          (`src/lib/macro-names.ts`) and the keyboard's cells have 145 px of slack for the long
          ones to grow into rather than ellipsising. */}
      <span className="macro-cell__name" style={{ minWidth: MACRO_NAME_WIDTH }}>
        {label}
      </span>
      {/* The outcome word, written by the paint loop and shown for about a second in the name's
          place. Always in the DOM so a frozen screenshot cannot catch it mid-mount. */}
      <span className="macro-cell__outcome" data-outcome-row={index} />
    </div>
  );
}
