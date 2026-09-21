/**
 * The DESCRIBE tab's copy must still be the copy in the doc.
 *
 * `docs/design/describe-tab.md` is what the operator approved and `src/games/describe.ts` is what the
 * build renders, and the only thing keeping those the same words twice over is this test: it
 * parses the doc's "Card (approved)" section and compares its heading and paragraph with the
 * rendered card — placeholders filled from the dataset's own `meta.json`, which is where the doc
 * says the numbers come from.
 *
 * So a copy change is a one-file edit (`src/games/describe.ts`) *and* a doc edit, and skipping
 * either one fails here rather than on air.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { DESCRIBE_CARD } from '../../src/games/describe';
import { pokemonRed } from '../../src/games/pokemon-red';
import { DESCRIBE_PLACEHOLDERS, fillDescribe, formatMillions, placeholdersIn } from '../../src/lib/describe';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '../../../..');

const meta = JSON.parse(readFileSync(resolve(repoRoot, 'data/fafb-v783/meta.json'), 'utf8')) as {
  neurons: number;
  edges: number;
  dataset: string;
};

/** The values the page would fill the card with, as the live page assembles them. */
const VALUES = {
  neurons: meta.neurons,
  synapses: meta.edges,
  game: pokemonRed.name,
  dataset: /v\d+/.exec(meta.dataset)?.[0] ?? meta.dataset,
  version: 'v0.1.1',
};

/** Collapse the doc's hard-wrapped paragraph to one line, the way the pane renders it. */
function flatten(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

/**
 * The doc's card: the one `### …` heading under "## Card", and the prose under it.
 *
 * Sliced to the next `## ` so the sections the doc grew later — the new-chatter switch, dated
 * 2026-09-17 — cannot leak into the copy this compares.
 */
function docCard(): { title: string; text: string } {
  const doc = readFileSync(resolve(repoRoot, 'docs/design/describe-tab.md'), 'utf8');
  const section = doc.slice(doc.indexOf('## Card'));
  const headings = section.split(/^### /m).slice(1);
  assert.equal(headings.length, 1, `the doc has ${headings.length} cards under "## Card", not one`);
  const card = headings[0] as string;
  const newline = card.indexOf('\n');
  return {
    title: flatten(card.slice(0, newline)),
    text: flatten(card.slice(newline).split(/^## /m)[0] as string),
  };
}

test('the doc holds exactly one card, and it is the card the build renders', () => {
  const doc = docCard();
  assert.equal(DESCRIBE_CARD.title, doc.title, 'the title drifted from docs/design/describe-tab.md');
  assert.equal(
    fillDescribe(DESCRIBE_CARD.text, VALUES),
    doc.text,
    'the paragraph drifted from docs/design/describe-tab.md',
  );
});

test('every placeholder the card uses is one the page can fill', () => {
  // The failure this catches is a card that renders `{neuron_count}` on a 24/7 broadcast.
  const known = new Set<string>(DESCRIBE_PLACEHOLDERS);
  for (const name of placeholdersIn(DESCRIBE_CARD.text)) {
    assert.ok(known.has(name), `the card uses {${name}}, which nothing resolves`);
  }
  assert.doesNotMatch(fillDescribe(DESCRIBE_CARD.text, VALUES), /[{}]/, 'the card still has braces after filling');
});

test('the numbers come from the dataset, not from a sentence', () => {
  // The doc's rule: "the neuron and synapse counts come from the dataset at runtime". A card that
  // typed them out would survive a connectome rebuild and be wrong, which is the one thing this
  // tab cannot be.
  assert.match(DESCRIBE_CARD.text, /\{neurons\}/, 'the card hardcodes its neuron count');
  assert.match(DESCRIBE_CARD.text, /\{synapses\}/, 'the card hardcodes its synapse count');
  assert.doesNotMatch(DESCRIBE_CARD.text, /\d[\d,]{4,}/, 'the card has a literal count in it');
  assert.doesNotMatch(DESCRIBE_CARD.title, /\d/, 'the title has a number in it');

  // And the rounding the copy reads in: 2,700,513 connections is "2.7 million".
  assert.equal(formatMillions(meta.edges), '2.7 million');
  assert.equal(formatMillions(4_000_000), '4 million');
  assert.equal(formatMillions(999), '999');
});

test('an unloaded dataset degrades to a dash, not to a wrong number or a hole', () => {
  const blank = { neurons: null, synapses: null, game: pokemonRed.name, dataset: null, version: 'dev' };
  const rendered = fillDescribe(DESCRIBE_CARD.text, blank);
  assert.match(rendered, /— mapped neurons/, 'an unknown count should read as the page-wide em dash');
  assert.match(rendered, /— synapses/, 'an unknown synapse count should read as the em dash too');
});

test('the card is the size the pane was measured for', () => {
  // The real check is `tests/e2e/describe.spec.ts`, which measures the rendered block against the
  // pane. This is the cheap guard next to the copy itself: the pane holds six lines of about 74
  // characters of VT323 at the 24 px floor under a 29-character Silkscreen title, and the CSS
  // comment that sets the measure is written against those numbers.
  assert.ok(DESCRIBE_CARD.title.length <= 32, `the title is ${DESCRIBE_CARD.title.length} chars, too wide for the pane`);
  const rendered = fillDescribe(DESCRIBE_CARD.text, VALUES);
  assert.ok(rendered.length <= 480, `the card is ${rendered.length} chars, over the 480 the pane holds`);
});
