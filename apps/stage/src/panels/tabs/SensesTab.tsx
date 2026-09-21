import { macroRateRole } from '@flybrain/feed';
import type { RefObject } from 'react';

import { useStage } from '@/feed/store';
import { CIRCUIT_DECISION_THRESHOLD } from '@/lib/circuit-scale';
import {
  MACRO_BAR_LABEL_WIDTH,
  MACRO_BAR_ROW_HEIGHT,
  MACRO_CIRCUIT_NAME_WIDTH,
  RETINA_CANVAS_HEIGHT,
  RETINA_CANVAS_WIDTH,
  SENSES_RETINA_CELL_WIDTH,
} from '@/lib/geometry';
import { CIRCUIT_GROUPS, DATASET_COPY, MACRO_CIRCUIT } from '@/lib/labels';

/**
 * The SENSES tab: what the fly sees, beside what its named circuits are doing.
 *
 * Both halves are written by the paint loop, not by React: the retina raster is an `ImageData`
 * blit and each circuit bar is a `transform: scaleX()` on an element the loop looks up once by
 * its `data-bar` attribute. React draws the labels and the boxes, once.
 *
 * The raster is 548x324 here against 336x240 in layout v1 — 2.2x the area — which is the "both
 * eyes, big" of the locked layout. That matters for more than presence: the legibility test
 * measures the raster's RMS contrast at the phone downscale, and dot detail is the first thing a
 * 397 px downscale averages away.
 *
 * A bar's fill is a fraction of its own adaptive reference (`src/lib/circuit-scale.ts`), not a
 * fixed Hz ceiling, so a live feed whose rates run far above anything a fixture produced cannot
 * peg every bar at 100% (which is what the first live run did — see
 * `infra/docs/p0-local-encoded-frame.png`). The `press` and `menu` groups also carry a faint
 * `data-threshold` tick at the decoder's own decision threshold (`docs/readout.md`).
 *
 * The MACROS row is the one group whose bars are not fixed: it lists the channels the scene has
 * bound right now (`docs/design/macros.md` section 12), so React re-renders it once per scene and
 * the paint loop finds its bars the same way it finds the palette's cells.
 *
 * No captions. The retina's "one dot per L1 column" and the LEGS group's "real, wired to nothing"
 * were both sentences under widgets, which the copy direction rules out.
 */
export function SensesTab({ retinaRef }: { retinaRef: RefObject<HTMLCanvasElement | null> }) {
  return (
    <div className="senses" data-testid="senses">
      <div className="senses__cell" style={{ flex: `0 0 ${SENSES_RETINA_CELL_WIDTH}px` }}>
        <span className="panel-title">{DATASET_COPY.retinaTitle}</span>
        <canvas
          ref={retinaRef}
          className="retina-canvas"
          width={RETINA_CANVAS_WIDTH}
          height={RETINA_CANVAS_HEIGHT}
          data-testid="retina"
        />
      </div>

      <div className="senses__cell senses__cell--circuits">
        <div className="senses__head">
          <span className="panel-title">{DATASET_COPY.circuitsTitle}</span>
          {/* `spikeCount` is neurons that fired since the previous snapshot carrying the bitset
              (`docs/feed-protocol.md`), not a rate. The number is lerped by the paint loop, so it
              renders empty here.

              The unit word is gone, and it is a measured deviation from the copy this row used to
              carry. The head is 422 px: "circuits" in Press Start 2P is 229 of it and the gap 12,
              which leaves 181 for the readout — and in Silkscreen at the 27 px body floor the word
              " spikes" alone is 121 px, against 135 to 189 for the number itself. Both together are
              256 to 310 and the row overflowed its panel by 75 px.

              Of the two, the number is what cannot be paraphrased, so the unit went. It stays
              tabular (`.num`, Press Start 2P) because it is relerped every frame and this is the
              page's most restless number. */}
          <span data-role="body" className="senses__spikes whitespace-nowrap text-ink-2">
            <span className="num" data-readout="spikes" />
          </span>
        </div>

        <div className="senses__groups">
          {CIRCUIT_GROUPS.map((group) => (
            <div key={group.id} className="circuit-group" data-group={group.id}>
              <span data-role="label" className="circuit-name" style={{ color: `var(--${group.tint})` }}>
                {group.label}
              </span>
              <div className="circuit-bars">
                {group.bars.map((bar) => (
                  <div key={bar.role} className="circuit-bar" data-bar-track={bar.role}>
                    <div
                      className="circuit-bar__fill"
                      data-bar={bar.role}
                      style={{ background: `var(--${group.tint})` }}
                    />
                    <div className="circuit-bar__peak" data-peak={bar.role} />
                    {CIRCUIT_DECISION_THRESHOLD[group.id] !== undefined ? (
                      <div className="circuit-bar__threshold" data-threshold={bar.role} />
                    ) : null}
                  </div>
                ))}
              </div>
            </div>
          ))}
          <MacroGroup />
        </div>
      </div>
    </div>
  );
}

/**
 * The MACROS row: the bound channels' rates, in the strip's own cell order.
 *
 * Two columns of three, in the strip's cell order — left to right, then the next line — because
 * six labelled rows in one column do not fit the pane (`src/lib/geometry.ts` has the measurement).
 * Empty in raw mode and in any scene that binds nothing, and it renders no row at all then rather
 * than an empty one — the six fixed groups above are what holds this column's shape.
 */
function MacroGroup() {
  const cells = useStage((state) => state.palette);
  const bound = cells.filter((cell) => cell.entry !== null);
  if (bound.length === 0) return null;

  return (
    <div className="circuit-group" data-group={MACRO_CIRCUIT.id}>
      <span
        data-role="label"
        className="circuit-name"
        style={{ color: `var(--${MACRO_CIRCUIT.tint})`, flex: `0 0 ${MACRO_CIRCUIT_NAME_WIDTH}px` }}
      >
        {MACRO_CIRCUIT.label}
      </span>
      <div className="circuit-bars">
        {bound.map((cell) => {
          // A bound macro's rate lives at `macro_` + its name lowercased with spaces replaced by
          // underscores (`docs/design/macros.md` section 11): `GO OUT` is `macro_go_out`. The page
          // asks the contract for that rule rather than keeping a table of its own.
          const role = macroRateRole((cell.entry as { name: string }).name);
          return (
            <div
              key={cell.row}
              className="circuit-bar-row"
              style={{ height: MACRO_BAR_ROW_HEIGHT }}
              data-macro-bar={cell.channel}
            >
              {/* The line box is the row's own height, not the 24 px type's: a 24 px line box in
                  a 14 px row overflows the pane's groups column by 16 px and flex squeezes every
                  bar on the panel. VT323's cap box at 24 px is 13.4, so the tag still fits. */}
              <span
                className="circuit-bar-row__label"
                style={{ width: MACRO_BAR_LABEL_WIDTH, lineHeight: `${MACRO_BAR_ROW_HEIGHT}px` }}
              >
                {cell.channel}
              </span>
              <div className="circuit-bar" data-bar-track={role}>
                <div
                  className="circuit-bar__fill"
                  data-bar={role}
                  style={{ background: `var(--${MACRO_CIRCUIT.tint})` }}
                />
                <div className="circuit-bar__peak" data-peak={role} />
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
