/**
 * The DESCRIBE tab's placeholder resolver: the half of the copy that is not copy.
 *
 * `src/games/describe.ts` holds every word the tab says and nothing else, so the live numbers it
 * quotes arrive as `{…}` placeholders and are filled here from the page's own sources — the loaded
 * dataset metadata (`store.dataset`), the game config's name, and the build's version string. The
 * doc's rule, verbatim: "names and numbers come from the feed where they exist (neuron count,
 * version); the rest is static copy".
 *
 * Pure and DOM-free, so `tests/unit/describe.test.ts` can render every card against the dataset's
 * own `meta.json` and compare the result with the doc.
 *
 * An unknown value renders as the page's own em dash rather than a guess or a hardcoded default: a
 * card that quoted a baked-in 139,255 while the dataset said something else would be the one thing
 * this tab cannot afford, which is a sentence that is not true.
 */
import { formatCount } from './format';

/** Everything a card may interpolate. Keys are the placeholder names, minus the braces. */
export interface DescribeValues {
  /** Dataset neuron count, or null until `meta.json` has landed. */
  neurons: number | null;
  /** Dataset connection count, or null. */
  synapses: number | null;
  /** The game config's human name: the only place a game is named. */
  game: string;
  /** Dataset version, e.g. `v783`, or null. */
  dataset: string | null;
  /** Release version from the build (`__STAGE_VERSION__`). */
  version: string;
}

/** The placeholders `fill` resolves. A copy edit may use any of these and no others. */
export const DESCRIBE_PLACEHOLDERS: readonly (keyof DescribeValues)[] = [
  'neurons',
  'synapses',
  'game',
  'dataset',
  'version',
];

/** What an unfilled number reads as, matching `src/lib/format.ts`. */
const UNKNOWN = '—';

/**
 * A synapse count as the copy says it: `2.7 million`, not `2,700,513`.
 *
 * The doc's register is "every sentence a fact", and 2,700,513 in the middle of a sentence is a
 * number a viewer stops to parse. One decimal, trailing `.0` dropped, and anything under a million
 * falls back to plain separators — a smaller connectome would be a different dataset, not a
 * rounding problem.
 */
export function formatMillions(value: number): string {
  if (!Number.isFinite(value)) return UNKNOWN;
  if (Math.abs(value) < 1_000_000) return formatCount(value);
  const millions = (value / 1_000_000).toFixed(1).replace(/\.0$/, '');
  return `${millions} million`;
}

/** Placeholder name -> rendered string. */
function resolve(values: DescribeValues): Record<string, string> {
  return {
    neurons: values.neurons === null ? UNKNOWN : formatCount(values.neurons),
    synapses: values.synapses === null ? UNKNOWN : formatMillions(values.synapses),
    game: values.game,
    dataset: values.dataset ?? UNKNOWN,
    version: values.version,
  };
}

/**
 * Fill one card's text.
 *
 * An unknown placeholder is left exactly as it was written, braces and all, so a typo shows up on
 * the mockup a reviewer is looking at instead of silently deleting half a sentence on air. The
 * unit test fails on one, which is where it is meant to be caught.
 */
export function fillDescribe(text: string, values: DescribeValues): string {
  const table = resolve(values);
  return text.replace(/\{(\w+)\}/g, (whole, key: string) => table[key] ?? whole);
}

/** Every placeholder a card's text uses, in order of appearance. */
export function placeholdersIn(text: string): string[] {
  return [...text.matchAll(/\{(\w+)\}/g)].map((match) => match[1] as string);
}
