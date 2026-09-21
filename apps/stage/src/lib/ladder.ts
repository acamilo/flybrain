/**
 * How many rungs the milestone spine draws.
 *
 * The feed is authoritative. `milestone.total` is the running game adapter's ladder length
 * (`docs/feed-protocol.md`), so the service can change its ladder — Pokémon Red's went from 16
 * rungs to 38 in `docs/design/ladder.md` — and the spine follows without a page release.
 *
 * The per-game config's `milestoneLadder` is the fallback, for a feed that predates `total`: a
 * recorded fixture, or an older flysim. It is also still the source of the rung *labels* the panel
 * falls back to when the header's own label is empty, which is why it does not simply go away.
 */
export const DEFAULT_RUNG_COUNT = 16;

/**
 * Resolve the spine's rung count from the feed's `total`, falling back to the config's ladder.
 *
 * A `total` of 0, a negative, a fraction or a non-number is ignored rather than trusted: this
 * value sizes a render loop on a page that has to keep painting on air, and the feed is a network
 * input. The fallback chain ends at {@link DEFAULT_RUNG_COUNT} so the count is never 0, which
 * would erase the panel.
 */
export function rungCount(total: number | undefined, ladderLength: number): number {
  if (typeof total === 'number' && Number.isInteger(total) && total >= 1) {
    return total;
  }
  if (Number.isInteger(ladderLength) && ladderLength >= 1) {
    return ladderLength;
  }
  return DEFAULT_RUNG_COUNT;
}

/**
 * The labels the LADDER tab spells out, padded or trimmed to `total`.
 *
 * The header carries `rank`, `label`, `next` and `total` but not the ladder's other names
 * (`docs/feed-protocol.md`), so the list comes from the per-game config and the *count* from the
 * feed. Padded rather than truncated to the config's length, because a service reporting more
 * rungs than this build knows the names of must not silently shorten the ladder on screen: the
 * unnamed rungs exist, and the tab shows them as numbered blanks.
 */
export function rungLabels(total: number, labels: readonly string[]): string[] {
  const out: string[] = [];
  for (let index = 0; index < total; index++) out.push(labels[index] ?? '');
  return out;
}

/**
 * Deal `items` into `columns` top-to-bottom columns of equal height.
 *
 * Column-major, not row-major: the ladder is a sequence, and a reader following 0, 1, 2 down the
 * first column and on to the next is following the fly's own path. Row-major would put rung 1 next
 * to rung 14.
 */
export function ladderColumns<T>(items: readonly T[], columns: number): T[][] {
  const count = Math.max(1, Math.floor(columns));
  const perColumn = Math.ceil(items.length / count);
  const out: T[][] = [];
  for (let column = 0; column < count; column++) {
    out.push(items.slice(column * perColumn, (column + 1) * perColumn));
  }
  return out;
}
