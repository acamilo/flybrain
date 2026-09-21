import { useEffect, useRef } from 'react';

import { useStage } from '@/feed/store';
import type { GameConfig } from '@/games';
import { LADDER_STATS_WIDTH } from '@/lib/geometry';
import { DATASET_COPY } from '@/lib/labels';
import { ladderColumns, rungCount, rungLabels } from '@/lib/ladder';
import { marqueeCycleMs, marqueeDistance } from '@/lib/marquee';

/** Columns the rungs are dealt into. Three fits 38 rungs at the 24 px floor in a 370 px pane. */
export const LADDER_COLUMNS = 3;

/**
 * Measure every `.ladder__label` against its column and mark the ones that overflow for the CSS
 * marquee (`src/theme/rail.css`) to animate, instead of ellipsising a name a viewer is meant to be
 * able to read in full — `docs/design/gameboy-theme.md`'s "Selection cursor marks the current
 * rung" makes the name the one thing on this pane a viewer actually hunts for.
 *
 * One effect over the whole column container rather than a hook per row: React's rules forbid a
 * hook inside `.map()`, and a `ResizeObserver` per label would be 38 of them doing the same job
 * one does. Re-measures on resize (`?res=` and a stray DPI change) and whenever the rendered rung
 * set changes.
 */
function useLadderMarquee(containerRef: React.RefObject<HTMLDivElement | null>, dependency: unknown): void {
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const measure = () => {
      for (const box of container.querySelectorAll<HTMLElement>('.ladder__label')) {
        const inner = box.firstElementChild as HTMLElement | null;
        if (!inner) continue;
        const distance = marqueeDistance(inner.scrollWidth, box.clientWidth);
        if (distance <= 0) {
          if (box.dataset.marquee) delete box.dataset.marquee;
          box.style.removeProperty('--marquee-distance');
          box.style.removeProperty('--marquee-duration');
          continue;
        }
        box.dataset.marquee = '1';
        box.style.setProperty('--marquee-distance', `${distance}px`);
        box.style.setProperty('--marquee-duration', `${marqueeCycleMs(distance)}ms`);
      }
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dependency]);
}

/**
 * The LADDER tab: the whole ratchet, spelled out.
 *
 * The spine in the progress cluster says how far the fly has got; this says what the rungs *are*,
 * which is the visible long-horizon goal the research says holds an audience for months. Three
 * column-major columns of rungs (reached amber, current marked with a caret, future dim), and one
 * narrow stats column: the rollback budget and the stall meter.
 *
 * The count comes from `milestone.total` and the *names* from the per-game config: the header
 * carries `rank`, `label`, `next` and `total` but not the other 37 names
 * (`docs/feed-protocol.md`), so a service that grows its ladder gets the right number of rows
 * without a page release and the names it does not send are shown as numbered blanks
 * (`src/lib/ladder.ts`).
 *
 * **No thumbnail** (dropped 2026-09-16, the operator: the best-snapshot canvas cost 160 of the stats
 * column's 236 px for a picture, not the ratchet this tab is about; the width goes to the rung
 * names instead, which is what let them stop abbreviating — README deviation 8, now resolved). The
 * "best" age readout went with it; rollbacks, lifetime, the last rollback's age and the stall
 * meter stay, in a `LADDER_STATS_WIDTH`-wide column.
 *
 * Every number in the stats column is written by the paint loop (`src/motion/readouts.ts`): they
 * are all functions of elapsed time, and a "41m ago" that only updates on the next 4 Hz commit
 * would be visibly wrong for a quarter of a second every minute.
 */
export function LadderTab({ game }: { game: GameConfig }) {
  const milestone = useStage((state) => state.milestone);
  const columnsRef = useRef<HTMLDivElement | null>(null);

  const total = rungCount(milestone.total, game.milestoneLadder.length);
  const rank = Math.max(0, Math.min(total - 1, milestone.rank));
  const labels = rungLabels(total, game.milestoneLadder);
  const columns = ladderColumns(
    labels.map((label, index) => ({ label, index })),
    LADDER_COLUMNS,
  );

  useLadderMarquee(columnsRef, labels.join('|'));

  return (
    <div className="ladder" data-testid="ladder-tab">
      <div className="ladder__columns" ref={columnsRef}>
        {columns.map((column, columnIndex) => (
          <ol className="ladder__column" key={columnIndex}>
            {column.map((rung) => (
              <li
                className="ladder__rung"
                key={rung.index}
                data-state={rung.index < rank ? 'reached' : rung.index === rank ? 'current' : 'future'}
                data-rung-row={rung.index}
              >
                <span className="ladder__mark" aria-hidden />
                {/* Not `.num`: a rung's ordinal is fixed for the life of the ladder, so it is not
                    a number that changes and has no claim on the monospaced face. */}
                <span className="ladder__index">{rung.index}</span>
                <span className="ladder__label" data-testid="ladder-label">
                  {/* The marquee pans this inner span, never the outer clipping box: `useLadderMarquee`
                      measures `firstElementChild.scrollWidth` against the box's own `clientWidth`. */}
                  <span>{rung.label}</span>
                </span>
              </li>
            ))}
          </ol>
        ))}
      </div>

      <div className="ladder__stats" style={{ flex: `0 0 ${LADDER_STATS_WIDTH}px` }}>
        <span data-role="body" className="ladder__line">
          <span className="text-ink-2">{DATASET_COPY.rollbacksTitle}</span>
          <span className="num ml-auto" data-readout="rollbacks" />
        </span>
        <span data-role="body" className="ladder__line">
          <span className="text-ink-2">{DATASET_COPY.lifetimeTitle}</span>
          <span className="num ml-auto" data-readout="lifetime" />
        </span>
        <span data-role="body" className="ladder__line">
          <span className="text-ink-2">{DATASET_COPY.lastTitle}</span>
          <span className="num ml-auto" data-readout="rollbackAge" />
        </span>
        <span data-role="body" className="ladder__line" data-stall-line="1">
          <span className="text-ink-2">{DATASET_COPY.stallTitle}</span>
          <span className="num ml-auto" data-readout="stall" />
        </span>
        <div className="stall-bar" data-testid="stall-bar">
          <div className="stall-bar__fill" data-stall-fill="1" />
        </div>
      </div>
    </div>
  );
}
