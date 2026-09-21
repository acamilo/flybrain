/**
 * The DESCRIBE tab's copy. **This file is the whole of it.**
 *
 * `docs/design/describe-tab.md` is the review surface and this is the build's copy of it: one
 * card, and nothing else in the repo carries a word of it. A copy change after the operator's review is
 * an edit here and a rebuild — no component, no CSS, no test holds a sentence of its own
 * (`tests/unit/describe.test.ts` asserts this file still matches the doc, title and paragraph, so
 * the two cannot drift).
 *
 * The copy is **approved by the operator (2026-09-17)**: one densely packed card, no cycling. Do not
 * paraphrase it here; change the doc, have it reviewed, then bring it across.
 *
 * `{…}` placeholders are the one thing that is not static: the doc's rule is that "names and
 * numbers come from the feed where they exist", so the card writes a placeholder and the renderer
 * fills it from the live page (`src/lib/describe.ts` resolves them, and the numbers are formatted
 * there rather than typed out here, so the count on screen cannot drift from the dataset):
 *
 *     {neurons}    the loaded dataset's neuron count, e.g. `139,255`
 *     {synapses}   its connection count, rounded, e.g. `2.7 million`
 *     {game}       the game config's human name — the only place a game may be named
 *     {dataset}    the dataset version, e.g. `v783`
 *     {version}    the release version the page was built at
 *
 * All five resolve; the approved card uses two. A future card that wants the build's version
 * writes `{version}` and gets it, which is why the list is longer than the copy needs.
 */

/** The card: the Silkscreen title, and the VT323 paragraph under it. */
export interface DescribeCard {
  /** The card's title, in the caps the doc gives it (`### …` in `docs/design/describe-tab.md`). */
  title: string;
  /** The paragraph. Placeholders as above. */
  text: string;
}

/**
 * The card, as approved.
 *
 * One, not eight. The operator, 2026-09-17: "keep it to a densely packed card" — so there is no cycle, no
 * card index and no cell row anywhere downstream of this file, and a reader who arrives at any
 * moment gets the whole explanation rather than one eighth of it.
 */
export const DESCRIBE_CARD: DescribeCard = {
  title: 'A CONNECTOME MEETS A GAME BOY',
  text:
    "This is a real fly's brain, {neurons} mapped neurons and {synapses} synapses, running live. " +
    'The screen is its eye. Its motor neurons press the buttons. Each scene offers a few actions, ' +
    'walk to a door, talk, attack; the fly picks one. When the game rewards it, a few thousand ' +
    'synapses shift, and what worked gets likelier. !sugar sends it a small reward pulse, no ' +
    'buttons. FlyWire connectome.',
};
