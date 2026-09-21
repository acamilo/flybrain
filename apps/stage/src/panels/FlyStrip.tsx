import type { RefObject } from 'react';

import { GAMEBOY_BUTTONS } from '@flybrain/brain';

import type { FlyMode } from '@/fly';
import {
  boxStyle,
  FLY_BUTTON_ROW_HEIGHT,
  FLY_CANVAS_HEIGHT,
  FLY_CANVAS_WIDTH,
  FLY_ROW_HEIGHT,
  LAYOUT,
} from '@/lib/geometry';
import { MacroPalette } from './MacroPalette';

/**
 * Two-character glyphs for the eight indicators, in `GAMEBOY_BUTTONS` order.
 *
 * Fixed here rather than in a per-game config: this row is the physical Game Boy's own buttons,
 * the same eight regardless of which game is loaded (`src/games/*` supplies vocabulary for the
 * game, not for the device it runs on). The macro strip beside it labels its cells with channel
 * tags instead (`docs/design/macros.md` section 12): a macro is a button of its own now, so no
 * cell hangs off one of these eight.
 */
export const BUTTON_GLYPHS: Record<string, string> = {
  up: 'UP',
  down: 'DN',
  left: 'LF',
  right: 'RT',
  a: 'A',
  b: 'B',
  start: 'ST',
  select: 'SE',
};

/**
 * The fly strip: the 3D fly with its plain row of eight button indicators above it on one side,
 * and the macro pad beside them.
 *
 * The buttons used to be the Game Boy's own caps, tapped by the fly's front legs; per review the
 * fly's limbs and wings are wired to real motor neurons only, so the indicators are a plain row
 * again — the same 250 ms afterglow semantics as before, painted by `src/App.tsx` straight onto
 * these cells' `data-down`/`data-glow` attributes. The afterglow stays under macros mode too: a
 * macro's presses are real presses (`docs/design/macros.md` section 1), made by its script rather
 * than by the channel that chose it.
 *
 * The fly shares the strip with the macro pad (the operator, 2026-09-16: "slide the fly over and
 * put the macro palette right next to it"), and they share one baseline and the strip's one frame
 * so the two read as a single instrument. The fly's own canvas still carries no text and has no
 * caption (`docs/design/fly-avatar.md`, "Copy: none"); the text in the strip is the button row's
 * glyphs and the macro cells.
 *
 * Two columns since `docs/design/macros.md` section 14: the button row is the *fly's* row now,
 * stacked over its canvas, because the pad's two columns of seven needed the strip's full inner
 * height and that band was the only place to find it (`src/lib/geometry.ts`). The row keeps its
 * eight chips, its glyphs and its afterglow; what it gives up is width it was only spacing them
 * across.
 */
export function FlyStrip({ mode, canvasRef }: { mode: FlyMode; canvasRef: RefObject<HTMLCanvasElement | null> }) {
  return (
    <div className="fly-strip" style={boxStyle(LAYOUT.flyStrip)} data-testid="fly-strip" data-fly={mode}>
      {/* The fly's column: its button row, then its canvas. Both are the canvas's width, which is
          what puts the pad's text clear of the no-content zone (`src/lib/geometry.ts`). */}
      <div
        className="fly-strip__col"
        style={{ width: FLY_CANVAS_WIDTH, flex: `0 0 ${FLY_CANVAS_WIDTH}px` }}
      >
        <div
          className="grid grid-cols-8 gap-1.5"
          style={{ height: FLY_BUTTON_ROW_HEIGHT, flex: `0 0 ${FLY_BUTTON_ROW_HEIGHT}px` }}
          data-testid="button-row"
          aria-hidden
        >
          {GAMEBOY_BUTTONS.map((button) => (
            <div key={button} data-button={button} className="button-cell" title={button}>
              <span className="label-face" style={{ fontSize: 'var(--fs-glyph)', lineHeight: 1 }}>
                {BUTTON_GLYPHS[button] ?? button.slice(0, 2).toUpperCase()}
              </span>
            </div>
          ))}
        </div>

        <div
          className="fly-pane"
          style={{ height: FLY_ROW_HEIGHT, flex: `0 0 ${FLY_ROW_HEIGHT}px` }}
          data-testid="fly-pane"
          aria-hidden
        >
          {mode === 'off' ? null : (
            <canvas
              ref={canvasRef}
              width={FLY_CANVAS_WIDTH}
              height={FLY_CANVAS_HEIGHT}
              data-testid="fly-canvas"
              className="fly-canvas"
            />
          )}
        </div>
      </div>

      <MacroPalette />
    </div>
  );
}
