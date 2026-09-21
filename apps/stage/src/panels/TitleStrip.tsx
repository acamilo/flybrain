import { Badge } from '@/components/ui/badge';
import { useStage } from '@/feed/store';
import type { GameConfig } from '@/games';
import { boxStyle, LAYOUT } from '@/lib/geometry';

/**
 * The title strip: one line, in the pixel face.
 *
 *     A FLY BRAIN PLAYS POKEMON RED
 *
 * The wordmark comes from the game config, the only place a game is named. The neuron count and
 * the learning/frozen chip that used to sit beside it are gone (2026-09-15 review): the count
 * duplicated the credit line in the rotating card, and "learning" read as one more piece of
 * operator status the audit already complained about.
 *
 * What is left besides the wordmark is the mode chip, which the current `game.mode` needs to
 * mean something on air (`data-ready`'s "the game is doing X" question) — hidden entirely when
 * the adapter reports `UNKNOWN`, since there is no honest plain word for that state — and, last
 * and quietest, the release version (2026-09-16), for whoever is on call rather than a viewer.
 */
export function TitleStrip({ game }: { game: GameConfig }) {
  const mode = useStage((state) => state.mode);
  const macroMode = useStage((state) => state.macroMode);
  const semanticRewards = useStage((state) => state.semanticRewards);

  return (
    <div style={boxStyle(LAYOUT.title)} className="flex items-center gap-4 overflow-hidden">
      <span
        className="pixel shrink-0 truncate text-accent"
        style={{ fontSize: 'var(--fs-wordmark)' }}
        data-testid="wordmark"
      >
        {game.wordmark}
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-2">
        {semanticRewards ? null : (
          <Badge variant="warn" style={{ fontSize: 'var(--fs-body)' }}>
            unaudited rom
          </Badge>
        )}
        {mode === 'UNKNOWN' ? null : (
          /* The catalogue's mode change: "chip text swaps with a 200 ms vertical roll". Keyed on
             the mode, so React remounts the chip and the CSS animation runs once per change —
             the mode is a value React already renders, and a second driver would fight it. */
          <Badge
            key={mode}
            className="mode-chip"
            variant="secondary"
            style={{ fontSize: 'var(--fs-body)' }}
            data-testid="mode-chip"
          >
            {game.modeLabels[mode]}
          </Badge>
        )}
        {/* What is on the pad: RAW buttons only, or buttons and the scene's MACROS
            (`docs/design/macros.md` section 12, "MODE chip: RAW / MACROS"). The word is the mode's
            own name from the feed rather than a mapping kept here, so a third mode cannot show up
            as `raw`. Always on screen, in both modes, because it is the disclosure that makes the
            macro strip honest — and keyed on the mode so the chip rolls once when it changes. */}
        <Badge
          key={macroMode}
          className="mode-chip"
          variant={macroMode === 'raw' ? 'outline' : 'default'}
          style={{ fontSize: 'var(--fs-body)' }}
          data-testid="macro-mode-chip"
        >
          {macroMode}
        </Badge>

        {/* The release version, quiet and last: `v0.1.0` on a tagged build, a short sha untagged,
            `dev` from the dev server (`vite.config.ts`). Plain text, not a chip — it is for
            whoever is on call, not a viewer's read of the game. */}
        <span
          className="shrink-0 text-ink-2"
          style={{ fontFamily: 'var(--font-label)', fontSize: 'var(--fs-label)' }}
          data-testid="stage-version"
        >
          {__STAGE_VERSION__}
        </span>
      </div>
    </div>
  );
}
