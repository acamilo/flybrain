import { useStage } from '@/feed/store';
import { formatReward } from '@/lib/format';
import { LAYOUT } from '@/lib/geometry';
import { DATASET_COPY } from '@/lib/labels';
import { Panel } from './Panel';

/**
 * Rail row 3: EVENTS. Three rows, unchanged in content from layout v1.
 *
 * The pacing is still `src/lib/ticker.ts`'s: a 4 s minimum dwell, exploration ticks deduped into
 * one counted row, and tiers so a gym badge and a +0.05 exploration tick do not get identical
 * treatment — which was the audit's finding about the old page's footer.
 *
 * Two things changed, both from the locked layout:
 *
 *   - **no title.** The panel is 100 px for three 28 px rows; a title row would cost one of them,
 *     and three lines of "+0.20 new area" do not need to be told they are events.
 *   - **no rotating card.** Layout v1 gave this lane to an explainer card for the last minute of
 *     every four-minute cycle. Layout v2 is explicit: "No explainer card anywhere."
 *
 * The motion is the catalogue's: a new row slides up over 240 ms and its value flashes amber
 * 120 ms up, 600 ms down. Both are CSS animations keyed on the row's feed event id, so they run
 * once per row on the compositor and leave a correct static end state in a frozen frame.
 */
export function EventsTicker() {
  const ticker = useStage((state) => state.ticker);

  return (
    <Panel box={LAYOUT.events} bodyClassName="events" testId="events-panel" critical>
      <div className="events__rows" data-testid="ticker">
        {ticker.length === 0 ? (
          <span data-role="body" className="text-ink-2">
            {DATASET_COPY.emptyTicker}
          </span>
        ) : null}
        {ticker.map((item) => (
          <div key={item.id} className="ticker-row" data-tier={item.tier} data-ticker-id={item.id}>
            <span className="ticker-row__dot" />
            <span data-role="body" className="ticker-row__label truncate">
              {item.label}
            </span>
            {item.kind === 'reward' && item.amount !== undefined ? (
              <span data-role="body" className="ticker-row__value num ml-auto shrink-0">
                {formatReward(item.amount)}
              </span>
            ) : null}
          </div>
        ))}
      </div>
    </Panel>
  );
}
