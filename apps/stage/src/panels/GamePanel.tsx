import type { RefObject } from 'react';

import { boxStyle, LAYOUT } from '@/lib/geometry';

/**
 * The game canvas: 800x720, exactly 5x the 160x144 framebuffer.
 *
 * The canvas itself carries no border, because a border on a `box-sizing: border-box` canvas
 * shrinks its content box under the backing store — which is the fractional scale, and therefore
 * the row-thickness jitter, the audit found at 2.62x. The dialogue-box frame
 * (`docs/design/gameboy-theme.md`) is a sibling overlay instead: the game box abuts the title
 * strip with no gutter, so there is nowhere outside it to draw one, and 6 px over the edge of the
 * image is the price of keeping the scale exactly 5.
 */
export function GamePanel({ canvasRef }: { canvasRef: RefObject<HTMLCanvasElement | null> }) {
  return (
    <div style={boxStyle(LAYOUT.game)} data-testid="game" data-legible="critical">
      <canvas
        ref={canvasRef}
        className="game-canvas"
        width={LAYOUT.game.width}
        height={LAYOUT.game.height}
      />
      <div className="game-frame" aria-hidden />
    </div>
  );
}
