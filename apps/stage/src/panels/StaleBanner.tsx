import { useStage } from '@/feed/store';
import { INSET, STAGE_WIDTH, boxStyle } from '@/lib/geometry';
import { DATASET_COPY } from '@/lib/labels';

/**
 * The STALE FEED banner (design A6).
 *
 * The old page froze its numbers when the worker stopped and said nothing, so the stream looked
 * alive and was not. Two seconds of silence and the page says so, across the top, over the title
 * strip. Honest beats tidy: a viewer who can see the feed is stale knows the fly is not stuck,
 * the plumbing is. Three words at most — STALE FEED, FEED DOWN, SIM ERROR — because an alarm that
 * explains itself in a clause is an alarm nobody reads.
 *
 * Driven by the store's own clock rather than by a socket event, because the failure that matters
 * — a half-open TCP connection to a wedged service — produces no event at all.
 */
export function StaleBanner() {
  const stale = useStage((state) => state.stale);
  const status = useStage((state) => state.status);
  const connection = useStage((state) => state.connection);

  if (!stale && status !== 'error' && connection !== 'closed') return null;

  const text =
    status === 'error'
      ? DATASET_COPY.simError
      : connection === 'closed' && !stale
        ? DATASET_COPY.feedDown
        : DATASET_COPY.staleBanner;

  return (
    <div
      className="stale-banner"
      data-testid="stale-banner"
      style={{
        ...boxStyle({ x: INSET, y: INSET, width: STAGE_WIDTH - INSET * 2, height: 40 }),
        zIndex: 60,
      }}
    >
      {/* A three-word alarm, not a sentence and not a name from an open vocabulary: it takes the
          chip/title face rather than `--font-text`, the same as the panel titles. */}
      <span className="label-face" style={{ fontSize: 'var(--fs-label)' }}>
        {text}
      </span>
    </div>
  );
}
