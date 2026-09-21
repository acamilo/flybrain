/**
 * The DESCRIBE tab, on the real page (`docs/design/describe-tab.md`).
 *
 * What is worth asserting here is not the words — `tests/unit/describe.test.ts` holds those
 * against the doc — but that the page renders the *one* card the doc approved, in the two faces it
 * names, with the dataset's own numbers in it, inside the pane and without scrolling. Nothing in
 * this file types a sentence of copy: it imports `src/games/describe.ts` and fills it exactly as
 * the pane does, so a reviewed copy change needs no edit here.
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { DESCRIBE_CARD } from '../../src/games/describe';
import { pokemonRed } from '../../src/games/pokemon-red';
import { fillDescribe } from '../../src/lib/describe';
import { gotoStage } from './stage';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');
const meta = JSON.parse(readFileSync(resolve(repoRoot, 'data/fafb-v783/meta.json'), 'utf8')) as {
  neurons: number;
  edges: number;
  dataset: string;
};

test('the tab is one card, in Silkscreen and VT323', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'describe' });

  const pane = page.locator('[data-testid="describe"]');
  await expect(pane).toHaveCount(1);
  // One card, and none of the cycle's machinery: no card index, no `data-current`, no cell row.
  await expect(pane.locator('.describe__card')).toHaveCount(1);
  await expect(pane.locator('[data-describe-card], [data-current], .describe__cell')).toHaveCount(0);

  const shown = await pane.evaluate((element) => {
    const title = element.querySelector('.describe__title') as HTMLElement;
    const text = element.querySelector('.describe__text') as HTMLElement;
    const measure = (node: HTMLElement) => {
      const style = getComputedStyle(node);
      return { face: style.fontFamily, size: Number.parseFloat(style.fontSize), text: node.innerText.trim() };
    };
    return { title: measure(title), text: measure(text) };
  });

  expect(shown.title.text).toBe(DESCRIBE_CARD.title);
  expect(shown.title.face, 'the title is not in the label face').toContain('Silkscreen');
  expect(shown.title.size, 'the title is outside the documented label band').toBeGreaterThanOrEqual(30);
  expect(shown.title.size).toBeLessThanOrEqual(36);
  expect(shown.text.face, 'the card text is not in VT323').toContain('VT323');
  expect(shown.text.size, 'the text is off the body floor').toBe(24);

  // And the words are the copy file's, with the dataset's own counts in them. The version comes
  // from the title strip's own chip rather than a literal, so this holds on a tagged build, an
  // untagged sha and the dev server alike — and it keeps holding if a reviewed card takes `{version}`.
  const version = ((await page.locator('[data-testid="stage-version"]').textContent()) ?? '').trim();
  expect(shown.text.text).toBe(
    fillDescribe(DESCRIBE_CARD.text, {
      neurons: meta.neurons,
      synapses: meta.edges,
      game: pokemonRed.name,
      dataset: /v\d+/.exec(meta.dataset)?.[0] ?? meta.dataset,
      version,
    }),
  );
});

test('the card fills the width, fits the pane and does not scroll at 1920x1080', async ({ page }) => {
  // The doc: one densely packed card, "no scrolling at 1920x1080". So: the block the card wants is
  // no taller than the pane, neither element is scrollable, and the paragraph actually uses the
  // tab's width rather than sitting in a narrow column — a card that fitted by being short would
  // pass the first two on its own.
  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'describe' });

  const fit = await page.evaluate(() => {
    const pane = document.querySelector('[data-testid="describe"]') as HTMLElement;
    const card = pane.querySelector('.describe__card') as HTMLElement;
    const text = pane.querySelector('.describe__text') as HTMLElement;
    const title = pane.querySelector('.describe__title') as HTMLElement;
    return {
      paneHeight: pane.getBoundingClientRect().height,
      paneWidth: pane.getBoundingClientRect().width,
      wanted: card.scrollHeight,
      overflowY: card.scrollHeight - card.clientHeight,
      overflowX: card.scrollWidth - card.clientWidth,
      textWidth: text.getBoundingClientRect().width,
      titleWidth: title.getBoundingClientRect().width,
      titleHeight: title.getBoundingClientRect().height,
      lineHeight: Number.parseFloat(getComputedStyle(text).lineHeight),
      textHeight: text.getBoundingClientRect().height,
    };
  });

  expect(fit.wanted, 'the card is taller than the pane').toBeLessThanOrEqual(fit.paneHeight);
  expect(fit.overflowY, 'the card scrolls vertically').toBeLessThanOrEqual(0);
  expect(fit.overflowX, 'the card scrolls sideways').toBeLessThanOrEqual(0);
  // A comfortable measure that is still the tab's width: at least three quarters of the pane.
  expect(fit.textWidth / fit.paneWidth).toBeGreaterThan(0.75);
  // The title sets on one line, which is what the 30 px size is for.
  expect(fit.titleHeight).toBeLessThan(48);
  expect(fit.titleWidth).toBeLessThan(fit.paneWidth);
  // And the paragraph is a paragraph: more than three lines, fewer than the pane holds.
  const lines = Math.round(fit.textHeight / fit.lineHeight);
  expect(lines, `the paragraph set in ${lines} lines`).toBeGreaterThanOrEqual(4);
  expect(lines).toBeLessThanOrEqual(8);
});
