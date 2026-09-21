import { useStage } from '@/feed/store';
import type { GameConfig } from '@/games';
import { safeDisplayName } from '@/lib/format';
import { LAYOUT } from '@/lib/geometry';
import { DATASET_COPY } from '@/lib/labels';
import { rungCount } from '@/lib/ladder';
import { Panel } from './Panel';

/** Geometry of the sugar chip's cooldown ring, in the footer row. */
const RING_RADIUS = 10;
const RING_STROKE = 3;
const RING_BOX = (RING_RADIUS + RING_STROKE) * 2;
export const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

/**
 * Rail row 1: the whole progress cluster, in one panel (locked layout v2).
 *
 * Layout v1 spread this over four panels — ladder, stuck-o-meter, run clock, sugar — which is 14
 * panel borders and four titles for eight numbers. One panel, four rows:
 *
 *     RUNG                             here for   brain    <- what the readouts are
 *     12/37 Mt. Moon  ▶ Cerulean City    2h41m  13.5 Hz
 *     ████████████░░░░░░░░░░░░░░░░░░░░░░░░░░              <- one cell per rung of the ladder
 *     1/8 badges · 214 places      try 2   06:12:33 · day 3   [◯ SUGAR READY]
 *
 * Terse register, no sentences: the rung line is one line of 30 px, and the only words are nouns
 * and the three small labels that say what a number is. The ▶ between the two rung names is drawn
 * rather than typed — see the arrow's own comment below.
 *
 * **Every number here is written by the paint loop, not by React** (`src/motion/readouts.ts`):
 * the elements carrying them render empty and their text is lerped toward the feed's value each
 * frame, per `docs/design/animation.md`'s addendum. React owns the structure and the strings that
 * are words — the rung name, the next rung, the sugar line — and commits those at 4 Hz.
 *
 * The rung spine's cell count comes from `milestone.total` when the service sends it and from the
 * game config's ladder otherwise (`src/lib/ladder.ts`), so the spine follows a service that
 * changes its ladder without a page release, and a recorded fixture that predates the field still
 * draws a full spine.
 */
export function ProgressCluster({ game }: { game: GameConfig }) {
  const milestone = useStage((state) => state.milestone);
  const sugar = useStage((state) => state.sugar);

  const total = rungCount(milestone.total, game.milestoneLadder.length);
  const lastRung = Math.max(0, total - 1);
  const rank = Math.max(0, Math.min(lastRung, milestone.rank));
  const ladder = game.milestoneLadder;
  const label = milestone.label || ladder[rank] || '';
  const next = milestone.next || ladder[Math.min(rank + 1, ladder.length - 1)] || '';
  const ready = !sugar.active && sugar.cooldownMs <= 0;

  return (
    <Panel box={LAYOUT.progress} bodyClassName="progress-cluster" critical>
      <div className="progress-cluster__grid">
        {/* The word RUNG is the label, and the count is the head of the line below it: as a label
            it costs the line nothing, and as part of the line it cost the rung name eight
            characters of ellipsis. That arithmetic matters more now than it did — the rung name is
            Press Start 2P, which is one em per character. */}
        <span className="panel-title">rung</span>
        <span className="panel-title">{DATASET_COPY.hereForTitle}</span>
        <span className="panel-title">{DATASET_COPY.brainTitle}</span>

        <span className="rung-line" data-testid="rung-line">
          <span className="rung-line__count num" data-readout="rung" />
          <span className="rung-line__name" data-testid="rank-name" title={label}>
            {label}
          </span>
          {/* The arrow is drawn, not typed. No pixel body face on Google Fonts carries U+2192 —
              none of the three candidates in `mockups/gameboy-fonts.png` does — and a missing
              glyph falls back to a proportional face and shows as tofu on air. So it is the same
              `.cursor` staircase the current rung and the open tab use. */}
          <span className="cursor rung-line__arrow" aria-hidden />
          <span className="rung-line__next" data-testid="next-rung" title={next}>
            {next}
          </span>
        </span>

        {/* The two big numbers, and both of them are Press Start 2P by way of `.num`.
            `docs/design/gameboy-theme.md` asks for the title face on big numbers *and* for tabular
            digits everywhere numbers change, and with Silkscreen as the body pick those are the
            same requirement: Silkscreen has no `tnum`, so only the monospaced face keeps a
            30 Hz-lerped Hz and a ticking HERE FOR from breathing sideways. They sit at
            `data-role="label"` — 33 px, the label band's floor, not its ceiling — because a full
            em per character at 39 px took 468 px of the cluster's 980 and left the rung line
            unreadable. `.cluster-readout` is what stops their columns thrashing. */}
        <span
          data-role="label"
          className="num cluster-readout here-for"
          data-readout="hereFor"
          data-here-for="1"
          data-warm="0"
          data-testid="here-for-time"
        />

        <span data-role="label" className="cluster-hz text-accent">
          <span className="num cluster-readout" data-readout="hz" data-testid="hero-hz" />
          <span className="cluster-hz__unit">{DATASET_COPY.hzUnit}</span>
        </span>
      </div>

      {/* The spine keeps layout v1's class names, so the e2e ladder spec that measures 38 rungs at
          the phone downscale is still measuring the same element. `--ladder-rungs` is gone with the
          gap that scaled off it: `docs/design/gameboy-theme.md` fixes the gap at the grid's 2 px
          whatever the ladder's length. */}
      <div className="ladder-spine" data-testid="ladder">
        {Array.from({ length: total }, (_, index) => (
          <div
            key={index}
            className="ladder-rung"
            data-rung={index}
            data-state={index < rank ? 'reached' : index === rank ? 'current' : 'future'}
          />
        ))}
      </div>

      <div className="progress-cluster__footer">
        {/* No `truncate`: at `--font-text` (VT323) the counters line fits its column with room to
            spare, which was not true under the Silkscreen-only pairing this class name predates. */}
        <span data-role="body" className="text-ink-2" data-readout="counters" data-testid="game-counters" />
        {/* The try count sits with the counters rather than beside HERE FOR: at 30 px mono the
            rung line needs every pixel the right-hand readouts do not take, and "try 2" is a
            counter like the other two.

            Not `.num`: the try count steps once per rollback — a few times an hour at worst — so
            it is not a number a viewer can watch jitter, and a full em per character bought 36 px
            of a footer that is over budget in Silkscreen. The clock beside it, which moves every
            second, keeps the monospaced face. */}
        <span data-role="body" className="ml-auto whitespace-nowrap text-ink-2" data-readout="try" />
        {/* The clock is two elements: eight tabular digits, then the day in words. The split is
            `readouts.ts`'s, and it buys the counters line 113 px of the footer back. */}
        <span data-role="body" className="num whitespace-nowrap text-ink-2" data-readout="clock" data-testid="run-clock" />
        <span data-role="body" className="whitespace-nowrap text-ink-2" data-readout="day" data-testid="run-day" />
        <span className="chip-cut sugar-chip" data-testid="sugar-chip" data-ready={ready ? '1' : '0'}>
          <svg width={RING_BOX} height={RING_BOX} viewBox={`0 0 ${RING_BOX} ${RING_BOX}`} aria-hidden>
            <circle
              cx={RING_BOX / 2}
              cy={RING_BOX / 2}
              r={RING_RADIUS}
              fill="none"
              stroke="var(--bg-0)"
              strokeWidth={RING_STROKE}
            />
            <circle
              cx={RING_BOX / 2}
              cy={RING_BOX / 2}
              r={RING_RADIUS}
              fill="none"
              stroke={sugar.active ? 'var(--dopamine)' : ready ? 'var(--ok)' : 'var(--ink-2)'}
              strokeWidth={RING_STROKE}
              strokeDasharray={RING_CIRCUMFERENCE}
              strokeDashoffset={0}
              transform={`rotate(-90 ${RING_BOX / 2} ${RING_BOX / 2})`}
              data-sugar-ring="1"
              data-testid="sugar-ring"
            />
          </svg>
          <span data-role="body" className="whitespace-nowrap text-dopamine" data-testid="sugar-last">
            {ready ? DATASET_COPY.sugarReady : DATASET_COPY.sugarBy.replace('{name}', safeDisplayName(sugar.lastBy))}
          </span>
        </span>
      </div>
    </Panel>
  );
}
