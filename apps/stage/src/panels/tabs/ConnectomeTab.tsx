import type { RefObject } from 'react';

import { MAP_HERO_HEIGHT, MAP_HERO_WIDTH } from '@/lib/geometry';

/**
 * The CONNECTOME tab: the 2D brain map at slot size, and nothing else.
 *
 * In layout v1 this was a 380x270 inset that animated to hero size over the rail on a big moment.
 * The locked layout v2 deletes that promotion: the map is a tab, a moment gives a tab focus
 * instead, and the backing store is simply the pane — 1008x370, allocated once, never rescaled.
 * That removes the whole class of promotion bugs (a transform racing a repaint, a caption band
 * over a canvas whose backing store was the wrong size) and it removes the 520 ms transform from
 * the broadcast's critical path.
 *
 * No title inside the pane: the tab strip above it already says CONNECTOME, and a plate over the
 * map would sit on top of the one thing the tab exists to show.
 *
 * Three tiers of 2D canvas, no WebGL (`docs/design/stage-bridge.md` A5, decision 3): a base
 * bitmap of all 139,255 neurons rasterised once in a worker, a density accumulator over the spike
 * bitset with a 110 ms decay, and at most 256 pre-rendered sprites on the brightest cells. The
 * reward flare spreads from the PAM cluster's own centroid, which the worker computes from the
 * dataset's roles and positions rather than from a hand-placed coordinate.
 */
export function ConnectomeTab({ canvasRef }: { canvasRef: RefObject<HTMLCanvasElement | null> }) {
  return (
    <div className="connectome" data-testid="connectome">
      <canvas
        ref={canvasRef}
        width={MAP_HERO_WIDTH}
        height={MAP_HERO_HEIGHT}
        className="connectome__canvas"
        data-testid="brain-map"
      />
    </div>
  );
}
