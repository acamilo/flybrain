import type { RefObject } from 'react';

import type { GameConfig } from '@/games';
import { BORDER_WIDTH, LAYOUT, TAB_STRIP_HEIGHT } from '@/lib/geometry';
import { TABS, TAB_LABELS } from '@/lib/tabs';
import { ConnectomeTab } from './tabs/ConnectomeTab';
import { DescribeTab } from './tabs/DescribeTab';
import { LadderTab } from './tabs/LadderTab';
import { MacrosTab } from './tabs/MacrosTab';
import { SensesTab } from './tabs/SensesTab';
import { Panel } from './Panel';

/**
 * The strip's own height inside the panel's frame, so the pane starts at exactly 292.
 *
 * `TAB_STRIP_HEIGHT` is measured from the panel's outer edge (the structural test asserts that),
 * and `.panel__body` starts inside the 4 px frame, so the strip is the difference.
 */
const STRIP_CONTENT_HEIGHT = TAB_STRIP_HEIGHT - BORDER_WIDTH;

/**
 * Rail row 2: the tabbed slot (locked layout v2).
 *
 * One 420 px box, a 48 px tab strip, and five panes that occupy exactly `LAYOUT.tabContent`
 * (1008x370). The strip's amber underline slides to the active tab and the panes crossfade over
 * 300 ms; `src/motion/director.ts` drives both, because the cadence is a function of the clock
 * and the feed, not of a React state transition.
 *
 * **All five panes stay mounted.** Only one is *visible* — `data-visible="1"`, which is what the
 * structural test counts — and the rest are `visibility: hidden`, which takes them out of paint
 * entirely. They stay mounted because two of them own canvases with expensive contents: the
 * connectome's base bitmap is a worker's 139,255-point raster handed over once as an
 * `ImageBitmap`, and unmounting the canvas would throw it away and re-raster it on every tab
 * cycle, 20 times an hour, for ever.
 *
 * The slot is the panel the legibility test measures (`data-legible="critical"`): whichever tab is
 * up, the downscale has to survive it, which is the check that catches a pane whose type is fine
 * at 1080p and mud at 397 px — DESCRIBE, whose pane is nothing but type, is the one that check was
 * waiting for.
 */
export function TabSlot({
  game,
  retinaRef,
  mapRef,
}: {
  game: GameConfig;
  retinaRef: RefObject<HTMLCanvasElement | null>;
  mapRef: RefObject<HTMLCanvasElement | null>;
}) {
  return (
    <Panel box={LAYOUT.tabs} bodyClassName="tab-slot" testId="tab-slot" critical>
      <div
        className="tab-strip"
        style={{ flex: `0 0 ${STRIP_CONTENT_HEIGHT}px`, height: STRIP_CONTENT_HEIGHT }}
        data-testid="tab-strip"
      >
        {/* Each tab is a box joined to its neighbour and to the slot's own frame, and the open one
            opens into the pane (`src/theme/rail.css`). The pixel cursor marks it, per
            `docs/design/gameboy-theme.md`: "Selection cursor '▶' … marks the current rung and the
            active tab". It is rendered on every tab and hidden on the closed ones, so the strip's
            widths do not shift when the slot cycles — a moving underline is the motion here, not a
            reflowing strip. */}
        {TABS.map((id) => (
          <div className="tab" key={id} data-tab={id} data-active="0">
            <span className="cursor" data-tab-cursor="1" aria-hidden />
            <span data-role="body" className="tab__label">
              {TAB_LABELS[id]}
            </span>
          </div>
        ))}
        <div className="tab-underline" data-tab-underline="1" />
      </div>

      <div className="tab-panes">
        <div className="tab-pane" data-tab-pane="senses" data-visible="0">
          <SensesTab retinaRef={retinaRef} />
        </div>
        <div className="tab-pane" data-tab-pane="connectome" data-visible="0">
          <ConnectomeTab canvasRef={mapRef} />
        </div>
        <div className="tab-pane" data-tab-pane="ladder" data-visible="0">
          <LadderTab game={game} />
        </div>
        <div className="tab-pane" data-tab-pane="describe" data-visible="0">
          <DescribeTab game={game} />
        </div>
        {/* MACROS stays mounted for a reason of its own: the paint loop writes `data-live` and
            `data-outcome` onto its cells on every frame (`paintPalette` in `src/App.tsx`), so a
            pane that unmounted between cycles would hand the loop a stale node list every time the
            slot came back round. */}
        <div className="tab-pane" data-tab-pane="macros" data-visible="0">
          <MacrosTab />
        </div>
      </div>
    </Panel>
  );
}
