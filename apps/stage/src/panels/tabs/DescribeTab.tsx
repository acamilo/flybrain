import { useStage } from '@/feed/store';
import { DESCRIBE_CARD } from '@/games/describe';
import type { GameConfig } from '@/games';
import { fillDescribe } from '@/lib/describe';

/**
 * The DESCRIBE tab: what this is, on one dense card (`docs/design/describe-tab.md`).
 *
 * The fourth tab on the rail, after LADDER. Everything it says lives in
 * `src/games/describe.ts` — one card, approved by the operator 2026-09-17 — and the live numbers it quotes
 * are filled here from the dataset metadata (`src/lib/describe.ts`).
 *
 * **One card, no cycling.** The eight-card cycle this pane shipped with is gone: there is no card
 * index, no `data-current` and no cell row, because a viewer who arrives mid-rotation now gets the
 * whole explanation rather than one eighth of it. Nothing about the pane is a function of the
 * clock any more, which is also why the director no longer paints it.
 *
 * The title is Silkscreen at the label size and the paragraph VT323 at the 24 px body floor, per
 * the doc. The measure is the tab's own width rather than a narrow column: at the floor the
 * approved paragraph sets in six lines across the pane and fits it with room under it
 * (`src/theme/rail.css`, and `tests/e2e/describe.spec.ts` measures it).
 */
export function DescribeTab({ game }: { game: GameConfig }) {
  const dataset = useStage((state) => state.dataset);

  const values = {
    neurons: dataset?.neurons ?? null,
    synapses: dataset?.edges ?? null,
    game: game.name,
    dataset: dataset?.name ?? null,
    version: __STAGE_VERSION__,
  };

  return (
    <div className="describe" data-testid="describe">
      <article className="describe__card">
        {/* `label` (30 px), the bottom of the label band: the title runs 29 characters and
            Silkscreen is wide, so 30 is what sets it on one line inside the pane. */}
        <h2 data-role="label" className="describe__title">
          {DESCRIBE_CARD.title}
        </h2>
        <p data-role="body" className="describe__text">
          {fillDescribe(DESCRIBE_CARD.text, values)}
        </p>
      </article>
    </div>
  );
}
