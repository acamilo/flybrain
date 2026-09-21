import type { RefObject } from 'react';

import { useStage } from '@/feed/store';
import {
  boxStyle,
  LAYOUT,
  MAP_CAPTION_HEIGHT,
  RAIL_BOX,
  STAGE_HEIGHT,
  STAGE_WIDTH,
} from '@/lib/geometry';
import { recipeFor } from '@/motion/catalogue';
import { CAPTION_HEADLINE } from '@/motion/rail-motion';

/** The caption band: over the top of the tab slot's pane, full pane width. */
const CAPTION_BOX = {
  x: LAYOUT.tabContent.x,
  y: LAYOUT.tabContent.y,
  width: LAYOUT.tabContent.width,
  height: MAP_CAPTION_HEIGHT,
};

/**
 * Everything a moment draws that is not inside a panel: the caption band, the rail border flash,
 * the particle layer and the day-rollover slide.
 *
 * One component, so `App.tsx` gains one element rather than five, and so the z-order of the four
 * overlays is decided in one place:
 *
 *     10  panels
 *     40  rail flash (outlines the rail, never fills it)
 *     41  caption band (over the tab slot only, never over the game)
 *     45  particles
 *     50  day slide (across the title strip)
 *     60  stale banner (which outranks every celebration)
 *
 * **The particle canvas spans the whole stage, not just the rail.** The design puts particles "on
 * a 2D canvas layer over the rail", and then asks for sugar sparks that "drift from the fly's head
 * toward the sugar chip" — a path that starts at x=448 in the left column and ends at x=1790 in
 * the rail. A rail-sized canvas cannot draw it. The cost of the wider canvas is nil, because the
 * layer touches no pixels at all on a frame with no live particles and clears only its own dirty
 * rectangle otherwise (`src/motion/particles.ts`).
 *
 * The caption band is keyed on the moment id so its entrance animation runs once per moment, and
 * it is only rendered for kinds the catalogue says carry a caption — a sugar pulse gets the ring,
 * the sparks and the ticker row, not a band across the tab slot.
 */
export function MomentLayer({ particleRef }: { particleRef: RefObject<HTMLCanvasElement | null> }) {
  const moment = useStage((state) => state.moment);
  const caption = moment && recipeFor(moment.type).caption ? moment : null;

  return (
    <>
      <div className="rail-flash" data-rail-flash="0" style={{ ...boxStyle(RAIL_BOX), zIndex: 40 }} aria-hidden />

      {caption ? (
        <div
          className="moment-caption"
          key={caption.id}
          data-testid="moment-caption"
          data-caption-kind={caption.type}
          style={{ ...boxStyle(CAPTION_BOX), zIndex: 41 }}
        >
          {/* The headline word (MILESTONE, BADGE, …) is a closed vocabulary, like a panel title;
              the label beside it is the feed's own rung/reward text, `--font-text`'s job. */}
          <span className="label-face shrink-0 text-accent" style={{ fontSize: 'var(--fs-body)' }}>
            {CAPTION_HEADLINE[caption.type]}
          </span>
          <span data-role="label" className="truncate text-ink-0">
            {caption.label}
          </span>
          {/* The detail is the catalogue's own second half — "REWIND · try 3", "BADGE · 1 of 8" —
              and it is the part that says *which* one, so it never truncates. */}
          {caption.detail ? (
            <span data-role="body" className="ml-auto shrink-0 whitespace-nowrap text-ink-2">
              · {caption.detail}
            </span>
          ) : null}
        </div>
      ) : null}

      <canvas
        ref={particleRef}
        className="particles"
        width={STAGE_WIDTH}
        height={STAGE_HEIGHT}
        style={{ ...boxStyle({ x: 0, y: 0, width: STAGE_WIDTH, height: STAGE_HEIGHT }), zIndex: 45 }}
        aria-hidden
      />

      <div
        className="day-slide"
        data-day-slide="0"
        style={{ ...boxStyle(LAYOUT.title), zIndex: 50 }}
        aria-hidden
      >
        {/* "DAY N" is a day word like the run clock's own (`ProgressCluster.tsx`), so it takes the
            page's default `--font-text` rather than the title face — no class override needed. */}
        <span className="text-accent" data-day-text style={{ fontSize: 'var(--fs-label)' }} />
      </div>
    </>
  );
}
