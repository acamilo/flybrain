/**
 * Rail layout v2 geometry in 1920x1080 authoring pixels, as numbers.
 *
 * One authoring resolution: everything is laid out in these CSS pixels inside `#stage`, which is
 * the native broadcast frame (`docs/design/fly-avatar.md`, "Canvas"). `?res=720` is
 * `transform: scale(0.6667)` on that one element and exists only for thumbnails and the
 * downscale tests. So these numbers are the only layout truth in the app, the panels take their
 * absolute positions from them, and the e2e tests assert against them rather than against
 * hand-copied constants.
 *
 * The rail is the locked "Rail layout v2" of `docs/stream-mvp-plan.md`: a compact progress
 * cluster, one tabbed slot, the events ticker, and a persistent chat panel. Verified arithmetic:
 *
 *   left   40 + 720 + 4 + 220                         = 984  (title, game, gap, fly strip)
 *   rail   144 + 12 + 420 + 12 + 100 + 12 + 244       = 944  (= 720 + 4 + 220)
 *   width  800 + 12 + 1012                            = 1824
 *   rail y 88 .. 1032                                        (usable, after the 48 px insets)
 *   slot   420 = 48 tab strip + 4 frame + 368 content
 *   strip  4 + 416 + 12 + 364 + 4                     = 800  (frame, fly, gap, macro pad, frame)
 */
import { MACRO_TYPES } from '@flybrain/feed';

/** Authoring viewport. Not configurable: the encoder, the capture and every test assume it. */
export const STAGE_WIDTH = 1920;
export const STAGE_HEIGHT = 1080;

/** Safe inset on all four sides. Background art may bleed past it; nothing load-bearing may. */
export const INSET = 48;

/** Gutter between cells inside one panel. */
export const GUTTER = 4;

/**
 * Gutter between the rail's four panels.
 *
 * 12, not the 4 of layout v1: four panels instead of five, and the locked geometry puts the tab
 * slot at 244 and the chat panel at 788, which are only reachable with 12 px between rows.
 */
export const RAIL_GUTTER = 12;

/** Gutter between the two columns. */
export const COLUMN_GAP = 12;

/** Usable box after the insets. */
export const USABLE_WIDTH = STAGE_WIDTH - INSET * 2;
export const USABLE_HEIGHT = STAGE_HEIGHT - INSET * 2;

/** Integer scale of the 160x144 emulator framebuffer. 5x is 800x720 in a native 1080p frame. */
export const GAME_SCALE = 5;
export const GAME_NATIVE_WIDTH = 160;
export const GAME_NATIVE_HEIGHT = 144;
export const GAME_WIDTH = GAME_NATIVE_WIDTH * GAME_SCALE;
export const GAME_HEIGHT = GAME_NATIVE_HEIGHT * GAME_SCALE;

/** Title strip across the full usable width. */
export const TITLE_HEIGHT = 40;

/**
 * The fly strip: a plain row of eight button indicators along its top edge, then the 3D fly on one
 * side and the macro palette beside it, under the game.
 */
export const FLY_STRIP_WIDTH = GAME_WIDTH;
export const FLY_STRIP_HEIGHT = 220;

/** Height of the plain button row along the fly strip's top edge, at 1080p authoring. */
export const FLY_BUTTON_ROW_HEIGHT = 32;

/** Right rail. */
export const RAIL_WIDTH = USABLE_WIDTH - GAME_WIDTH - COLUMN_GAP;

/**
 * Panel frame width, mirroring `--border-w` in `src/theme/tokens.css`.
 *
 * 4, not layout v1's 2: `docs/design/gameboy-theme.md` makes every panel a dialogue box with "a
 * 4 px outer border with a 2 px inner line". The outer one is the real `border`, so it is the one
 * the interior arithmetic has to subtract — the inner line is a pseudo-element over the padding
 * box and takes no space. Every panel's *outer* box is unchanged (`box-sizing: border-box`), so
 * the only numbers that move are the ones derived from the inside of the tab slot.
 */
export const BORDER_WIDTH = 4;

/** The inner line of the double frame, mirroring `--border-inner-w`. Drawn, never laid out. */
export const BORDER_INNER_WIDTH = 2;

/** The row below the button row: the fly and the palette share it, and its baseline. */
export const FLY_ROW_HEIGHT = FLY_STRIP_HEIGHT - BORDER_WIDTH * 2 - FLY_BUTTON_ROW_HEIGHT - GUTTER;

/** Gap between the fly's canvas and the palette beside it. */
export const FLY_PALETTE_GAP = 12;

/**
 * The fly's own canvas, now 416 wide instead of the strip's full 792.
 *
 * The operator, 2026-09-16: "slide the fly over and put the macro palette right next to it". The number
 * is not a taste call — it is the *widest* the fly can be and still leave the palette clear of the
 * no-content zone. Twitch overlays chat and extensions over the bottom left of the player
 * ({@link NO_CONTENT_ZONE}, x < 480), the palette's cells carry text, and the palette's bottom row
 * is at y ≈ 970, inside the zone's band. So the palette starts at exactly x = 480 and the fly gets
 * everything left of it: 52 (the strip's inner edge) + 416 + 12 = 480. The fly's canvas may reach
 * into the zone, as it always has, because it carries no text at all.
 *
 * The vertical field of view is unchanged (`src/fly/camera.ts`), so the fly is the same size in
 * pixels and the narrower canvas crops the shot rather than shrinking the animal — which is what
 * "shrunk to make room" has to mean for a perspective camera with a fixed vertical framing.
 */
export const FLY_CANVAS_WIDTH = 416;
export const FLY_CANVAS_HEIGHT = FLY_ROW_HEIGHT;

/**
 * The pad beside it: the rest of the strip's inner width, and all of its inner height.
 *
 * 364 wide is not a choice — it is what is left once the fly's canvas has stopped at x = 480, the
 * no-content zone's right edge ({@link NO_CONTENT_ZONE}, and {@link FLY_CANVAS_WIDTH} has the
 * reasoning). The fly did not shrink for section 14's second column: 480 is where the pad's text
 * has to start, so a narrower fly would only move readable cells under Twitch's chat overlay.
 * What the pad took instead was the 36 px band the button row used to span, which is height
 * nothing else wanted.
 */
export const MACRO_PALETTE_WIDTH =
  FLY_STRIP_WIDTH - BORDER_WIDTH * 2 - FLY_CANVAS_WIDTH - FLY_PALETTE_GAP;
export const MACRO_PALETTE_HEIGHT = FLY_STRIP_HEIGHT - BORDER_WIDTH * 2;

/**
 * The strip's cells: the macros on the pad *now*, up to fourteen, two columns of seven.
 *
 * The layout decision at the end of `docs/design/macros.md` section 14: the strip under the game
 * shows every button on the pad now, at the 24 px floor, and the whole keyboard of thirty-one
 * types lives on the MACROS tab instead ({@link MACRO_BOARD_COLUMNS}). Fourteen is what two
 * columns of seven hold and it is comfortably above the widest pad any scene deals — ten, the
 * indoor overworld inside a centre (section 13.1) — so nothing is cut in practice, and a pad that
 * did overflow would lose its *last* types rather than its first.
 *
 * Seven rows of 28 plus six 2 px gaps is the pad's 208 px of inner height exactly, and two
 * columns of 179 plus one 2 px gap is its 360 px of inner width. The 2 px gap rather than a frame
 * per cell is the spine's treatment (`docs/design/gameboy-theme.md`: "a row of square cells with
 * 2 px gaps"), and it is also what lets a 24 px line sit in the row.
 */
export const MACRO_CELL_COLUMNS = 2;
export const MACRO_CELL_ROWS = 7;
export const MACRO_CELL_COUNT = MACRO_CELL_COLUMNS * MACRO_CELL_ROWS;
export const MACRO_CELL_GAP = 2;
/** Inset between the palette's own edge and its cells. */
export const MACRO_PALETTE_PAD = 2;
export const MACRO_CELL_HEIGHT =
  (MACRO_PALETTE_HEIGHT - MACRO_PALETTE_PAD * 2 - MACRO_CELL_GAP * (MACRO_CELL_ROWS - 1)) / MACRO_CELL_ROWS;
export const MACRO_CELL_WIDTH =
  (MACRO_PALETTE_WIDTH - MACRO_PALETTE_PAD * 2 - MACRO_CELL_GAP * (MACRO_CELL_COLUMNS - 1)) / MACRO_CELL_COLUMNS;

/**
 * Width of a cell's channel tag, and the *minimum* width of its name column.
 *
 * Both are measured in VT323 at the 24 px body floor, which is the change section 14's second
 * column paid for: the cells were Silkscreen, whose 0.75 em advance made `MB·FRONT` 135 px and
 * `GO OBJECTIVE` 195, and 330 px of one cell does not go into 179 twice. VT323 is the page's own
 * body face at 0.4 em (`src/theme/tokens.css`), so the same eight-character tag is 77 px and the
 * cell holds the tag *and* a name at the floor rather than one of them above it.
 *
 * 85 is that 77 plus the glyph chip's 8 px of padding. 87 is nine characters of VT323, which is
 * what {@link MACRO_SHORT_NAME_MAX} caps the strip's names at, and the two plus the cell's own
 * padding and gap spend 178 of the 179 a cell has. The MACROS tab's cells are 332 px and carry the
 * full name instead ({@link MACROS_TAB_CELL_WIDTH}); the gloss stays on the wire in both places.
 */
export const MACRO_CHANNEL_WIDTH = 85;
export const MACRO_NAME_WIDTH = 87;

/**
 * Longest short name the pad's cells draw, in characters.
 *
 * Nine of VT323 at the floor is 87 px ({@link MACRO_NAME_WIDTH}), which is what a 179 px cell has
 * left once the tag chip has taken its 85. Six of the contract's names are longer than that, and
 * `src/lib/macro-names.ts` is the table that shortens them — with a truncating fallback, so a type
 * this page has never heard of still draws a name that fits rather than an ellipsis.
 */
export const MACRO_SHORT_NAME_MAX = 9;

/**
 * The SENSES panel's MACROS row: one labelled bar per bound channel (section 12).
 *
 * Two columns of three rather than a stack of six, and the numbers are measured rather than
 * chosen. The groups column is 317 px and the six fixed groups take 260 of it (35 for a group
 * whose name sets its height, 60 for the two with four bars), so this row has 57 px. Six labelled
 * rows in one column need 94 — a 24 px label's cap box is 13.4 px of VT323, so the pitch cannot go
 * below 16 — and every bar on the panel gets squeezed when it overflows. Three rows of two fit in
 * 46.
 *
 * Which is what the width is spent on: this group's name column is 148 ("MACROS" in Silkscreen at
 * the label size is 142) instead of the 200 that "dopamine" needs, leaving 264 for two 126 px
 * cells — the channel tag in VT323 at the body floor (eight characters at 0.4 em is 77 px) and a
 * 38 px track.
 */
export const MACRO_BAR_ROW_HEIGHT = 14;
export const MACRO_BAR_LABEL_WIDTH = 84;
/** The MACROS row's own name column: 148, not `CIRCUIT_NAME_WIDTH`, to pay for the second column. */
export const MACRO_CIRCUIT_NAME_WIDTH = 148;

/** Rail row heights, top to bottom. Locked in `docs/stream-mvp-plan.md`, "Rail layout v2". */
export const RAIL_ROWS = {
  /** Progress cluster: rung line, 38-rung spine, counters, clock, sugar chip. */
  progress: 144,
  /** The tabbed slot: 48 px tab strip plus the pane. */
  tabs: 420,
  /** EVENTS: three ticker rows, no title (the rows say what they are). */
  events: 100,
  /** CHAT: seven lines and a title. */
  chat: 244,
} as const;

/** Total height of the right rail: four rows plus three inter-row gutters (944). */
export const RAIL_HEIGHT =
  RAIL_ROWS.progress + RAIL_ROWS.tabs + RAIL_ROWS.events + RAIL_ROWS.chat + RAIL_GUTTER * 3;

/** Height of the tab strip inside the slot. */
export const TAB_STRIP_HEIGHT = 48;

/**
 * Retina raster canvas, inside the SENSES pane's left cell.
 *
 * Both eyes, big: the pane is 1004x368 and the retina takes 560 of it, which leaves 548x316 for
 * the canvas after the cell's 6 px padding, its 30 px label row and the gap between them. 2.1x
 * the area the raster had in layout v1's 360 px rail cell, which is what the locked layout's
 * "retina raster left (both eyes, big)" asks for. The height is measured rather than derived: the
 * label's line box rounds up, and the structural test fails the panel if the sum overruns.
 */
export const SENSES_RETINA_CELL_WIDTH = 560;
export const RETINA_CANVAS_WIDTH = 548;
export const RETINA_CANVAS_HEIGHT = 316;

/**
 * Fixed width of a circuit group's name, inside the SENSES pane's narrower right cell.
 *
 * 200, because the longest of the six group labels is "dopamine" and in Silkscreen at the 33 px
 * label size that is 196 px (measured). It was 150 under VT323, whose 0.4 em advance made the same
 * word 119; the pick changed and this is one of the places that had to follow it, because the
 * alternative was a group name reading "dopamin…" on the panel that names the fly's reward
 * circuit. The 50 px comes off the bar track, which is measured at runtime and quantised against
 * whatever it turns out to be (`src/App.tsx`), so no cell arithmetic depends on this number.
 */
export const CIRCUIT_NAME_WIDTH = 200;

/**
 * Width of the LADDER pane's right-hand column: rollbacks, lifetime, last and the stall meter.
 *
 * 130, not the 236 it was when this column also held the best-snapshot thumbnail (dropped
 * 2026-09-16, the operator: give the freed width to the rung names instead — three names a column no
 * longer abbreviate to "Viridia…" at this width, see `docs/design/gameboy-theme.md` deviation 8,
 * now resolved). 130 is a compact narrow column rather than the tightest that would fit: the
 * widest readout here ("0:47 ago" style rollback age) still occasionally ellipsises at the far
 * end of an hour-plus run, which the column accepts as a rare cost.
 */
export const LADDER_STATS_WIDTH = 130;

/** Chat lines kept on screen. */
export const CHAT_LINES = 7;

/** Absolute positions of every region, in authoring pixels from the top left of `#stage`. */
export const LAYOUT = (() => {
  const left = INSET;
  const top = INSET;

  const title = { x: left, y: top, width: USABLE_WIDTH, height: TITLE_HEIGHT };
  const game = { x: left, y: title.y + title.height, width: GAME_WIDTH, height: GAME_HEIGHT };
  const flyStrip = {
    x: left,
    y: game.y + game.height + GUTTER,
    width: FLY_STRIP_WIDTH,
    height: FLY_STRIP_HEIGHT,
  };

  /** The fly's canvas and the palette: one row inside the strip's frame, one baseline. */
  const flyRowY = flyStrip.y + BORDER_WIDTH + FLY_BUTTON_ROW_HEIGHT + GUTTER;
  const flyPane = {
    x: flyStrip.x + BORDER_WIDTH,
    y: flyRowY,
    width: FLY_CANVAS_WIDTH,
    height: FLY_ROW_HEIGHT,
  };
  const macroPalette = {
    x: flyPane.x + flyPane.width + FLY_PALETTE_GAP,
    y: flyStrip.y + BORDER_WIDTH,
    width: MACRO_PALETTE_WIDTH,
    height: MACRO_PALETTE_HEIGHT,
  };

  const railX = left + GAME_WIDTH + COLUMN_GAP;
  let y = title.y + title.height;
  const row = (height: number, width = RAIL_WIDTH) => {
    const box = { x: railX, y, width, height };
    y += height + RAIL_GUTTER;
    return box;
  };

  const progress = row(RAIL_ROWS.progress);
  const tabs = row(RAIL_ROWS.tabs);
  const events = row(RAIL_ROWS.events);
  const chat = row(RAIL_ROWS.chat);

  /** The pane below the tab strip: the box every tab's content is drawn into. */
  const tabContent = {
    x: tabs.x + BORDER_WIDTH,
    y: tabs.y + TAB_STRIP_HEIGHT,
    width: tabs.width - BORDER_WIDTH * 2,
    height: tabs.height - TAB_STRIP_HEIGHT - BORDER_WIDTH,
  };

  return { title, game, flyStrip, flyPane, macroPalette, progress, tabs, tabContent, events, chat } as const;
})();

/** The rail as one box: what the border flash outlines and the particle layer is sized against. */
export const RAIL_BOX = {
  x: LAYOUT.progress.x,
  y: LAYOUT.progress.y,
  width: RAIL_WIDTH,
  height: RAIL_HEIGHT,
} as const;

/**
 * The brain map's backing store: the tab pane, exactly.
 *
 * The map is no longer an inset that promotes over the rail (layout v1) — it is the CONNECTOME
 * tab, drawn at slot size and nothing else, so the backing store is the pane's own 1004x368 and
 * there is no promotion transform at all. `docs/stream-mvp-plan.md`'s "big moments pre-empt for
 * 9 s" is now a tab focus, which is a crossfade rather than a scale.
 */
export const MAP_HERO_WIDTH = LAYOUT.tabContent.width;
export const MAP_HERO_HEIGHT = LAYOUT.tabContent.height;

/**
 * Density accumulator grid.
 *
 * 1004/4 = 251 and 368/2 = 184, so the cell is 4x2 device pixels: the two axes take different
 * divisors because the pane's height is not a multiple of 4 and a fractional grid would put the
 * sprite pass a subpixel off the cell it belongs to. `brainmap.ts` derives the per-axis scale
 * from these rather than from one shared divisor.
 */
export const MAP_GRID_WIDTH = MAP_HERO_WIDTH / 4;
export const MAP_GRID_HEIGHT = MAP_HERO_HEIGHT / 2;

/**
 * The MACROS tab's keyboard: every macro type there is, three columns, rows as needed.
 *
 * `docs/design/macros.md` section 14: "a new rail tab MACROS shows the whole keyboard, three
 * columns of eleven, bound cells lit and unbound dim, the running one bright". Eleven rows is the
 * 31-type contract; on this one it is eight, because the row count is derived from the contract's
 * own table and not written down twice — the tab grows a row when three types are added and needs
 * no edit here.
 *
 * Three columns of 332 in the pane's 1004, and rows of 43.75 (eight) down to 31.3 (eleven): every
 * one of them carries a 24 px line with room, which is the whole reason the keyboard is here and
 * not under the game.
 */
export const MACROS_TAB_COLUMNS = 3;
export const MACROS_TAB_ROWS = Math.ceil(MACRO_TYPES.length / MACROS_TAB_COLUMNS);

/**
 * One cell of the MACROS tab's keyboard, inside the pane.
 *
 * Derived here rather than beside {@link MACROS_TAB_COLUMNS} because it needs the pane's box, and
 * the pane is `LAYOUT`'s. The pad's own 2 px inset and 2 px gaps are reused, so the keyboard and
 * the pad are the same grid at two sizes and one CSS rule set draws both.
 */
export const MACROS_TAB_CELL_WIDTH =
  (LAYOUT.tabContent.width - MACRO_PALETTE_PAD * 2 - MACRO_CELL_GAP * (MACROS_TAB_COLUMNS - 1)) /
  MACROS_TAB_COLUMNS;
export const MACROS_TAB_CELL_HEIGHT =
  (LAYOUT.tabContent.height - MACRO_PALETTE_PAD * 2 - MACRO_CELL_GAP * (MACROS_TAB_ROWS - 1)) /
  MACROS_TAB_ROWS;

/**
 * The MACROS tab — same numbers as {@link MACROS_TAB_COLUMNS} and {@link MACROS_TAB_ROWS}, the
 * alias kept because the rail tab that shows the keyboard is named `macros`
 * (`src/lib/tabs.ts`) and the geometry of "the macros tab" is what callers reach for. The pad and
 * the keyboard share the 2 px inset and 2 px gap (above) so these match the tab's actual
 * dimensions exactly.
 */
export const MACRO_BOARD_COLUMNS = MACROS_TAB_COLUMNS;
export const MACRO_BOARD_ROWS = MACROS_TAB_ROWS;
export const MACRO_BOARD_PAD = MACRO_PALETTE_PAD;
export const MACRO_BOARD_GAP = MACRO_CELL_GAP;
export const MACRO_BOARD_CELL_WIDTH = MACROS_TAB_CELL_WIDTH;
export const MACRO_BOARD_CELL_HEIGHT = MACROS_TAB_CELL_HEIGHT;

/** The caption band a milestone/badge moment slides over the top of the tab slot. */
export const MAP_CAPTION_HEIGHT = 72;

/**
 * Twitch overlays chat and extensions over the bottom left of the player, so nothing
 * load-bearing goes here. At 1080p the fly strip is what reaches into this box, and it carries no
 * text below its button row.
 */
export const NO_CONTENT_ZONE = { x: 0, y: STAGE_HEIGHT - 180, width: 480, height: 180 } as const;

/** Type of one absolute box. */
export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Inline style for an absolutely positioned region. */
export function boxStyle(box: Box): {
  position: 'absolute';
  left: string;
  top: string;
  width: string;
  height: string;
} {
  return {
    position: 'absolute',
    left: `${box.x}px`,
    top: `${box.y}px`,
    width: `${box.width}px`,
    height: `${box.height}px`,
  };
}

/** True when two boxes share any area. Used by the no-content-zone assertion. */
export function intersects(a: Box, b: Box): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

/** Centre of a box, in stage coordinates. Particle emitters aim at these. */
export function centreOf(box: Box): { x: number; y: number } {
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}
