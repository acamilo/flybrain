/**
 * The text-size lint (design A10.2), which is the audit's headline bug as a test.
 *
 * `fly-plays-pokemon/src/style.css:41-43` sat outside its closing `@media` block, so the mobile
 * type scale applied everywhere and the live page rendered body text at 7 to 11 px. At Twitch's
 * player scales (0.88 theater, 0.70 desktop with chat, 0.31 phone) nothing reached 14 px
 * effective. Nobody noticed, because nobody was measuring.
 *
 * So: walk every element with visible text, read its computed font size, multiply by the
 * accumulated scale of its ancestors (`#stage` is `scale(0.6667)` in the 720p thumbnail mode), and
 * fail below the floor for that mode. At the 1080p authoring size the floors are 2026-09-16's revised
 * ones: body >= 24, `data-role="label"` in 30-36, `data-role="hero"` at or above 72. The 720p
 * thumbnail lands them back on 16 / 20-24 / 48.
 */
import { expect, test } from '@playwright/test';

import { gotoStage, type Tab } from './stage';

/**
 * Every tab, because two thirds of the rail's readable area is inside the slot — and DESCRIBE and
 * MACROS most of all: one is a pane of nothing but text and the other is 22 cells of it, so the
 * floor is the whole of both designs.
 */
const TABS: Tab[] = ['senses', 'connectome', 'ladder', 'describe', 'macros'];

interface Measured {
  text: string;
  tag: string;
  role: string | null;
  fontPx: number;
  effectivePx: number;
  cls: string;
}

/** Read every rendered text run and its effective size, in one page evaluation. */
async function measure(page: import('@playwright/test').Page): Promise<Measured[]> {
  return page.evaluate(() => {
    /** Product of the x-scales of an element's transformed ancestors. */
    const scaleOf = (element: Element): number => {
      let scale = 1;
      let node: Element | null = element;
      while (node) {
        const transform = getComputedStyle(node).transform;
        if (transform && transform !== 'none') {
          const match = /matrix\(([^)]+)\)/.exec(transform);
          if (match) {
            const parts = (match[1] as string).split(',').map((part) => Number.parseFloat(part));
            if (Number.isFinite(parts[0])) scale *= parts[0] as number;
          }
        }
        node = node.parentElement;
      }
      return scale;
    };

    const out: Measured[] = [];
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const seen = new Set<Element>();

    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = (node.textContent ?? '').trim();
      if (text.length === 0) continue;

      const element = node.parentElement;
      if (!element || seen.has(element)) continue;
      seen.add(element);

      const rect = element.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) continue;
      const style = getComputedStyle(element);
      if (style.visibility === 'hidden' || style.display === 'none' || Number.parseFloat(style.opacity) === 0) {
        continue;
      }

      const fontPx = Number.parseFloat(style.fontSize);
      out.push({
        text: text.slice(0, 48),
        tag: element.tagName,
        role: element.getAttribute('data-role'),
        fontPx,
        effectivePx: fontPx * scaleOf(element),
        cls: element.getAttribute('class') ?? '',
      });
    }
    return out;
  });
}

for (const res of ['720', '1080'] as const) {
  for (const tab of TABS) {
    test(`no rendered text is under the floor at ${res}p, ${tab} tab`, async ({ page }) => {
      await gotoStage(page, { fixture: 'steady', t: 95, res, tab });

      const runs = await measure(page);
      expect(runs.length, 'the lint found no text at all, which means it is not working').toBeGreaterThan(15);

      const floor = res === '1080' ? 24 : 16;
      const tooSmall = runs.filter((run) => run.effectivePx < floor - 0.01);
      expect(
        tooSmall.map((run) => `${run.effectivePx.toFixed(1)}px "${run.text}" (${run.tag}.${run.cls})`),
        `text below the ${floor} px floor`,
      ).toEqual([]);
    });
  }
}

test('the type roles land in their documented bands', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'senses' });
  const runs = await measure(page);

  const labels = runs.filter((run) => run.role === 'label' || run.role === 'label-lg');
  expect(labels.length, 'no labelled text found').toBeGreaterThan(3);
  for (const label of labels) {
    expect(label.effectivePx, `label "${label.text}"`).toBeGreaterThanOrEqual(30);
    expect(label.effectivePx, `label "${label.text}"`).toBeLessThanOrEqual(36);
  }

  // Layout v2 has no 72 px hero: the compact progress cluster replaced the four panels that used
  // to carry one (the run clock's Hz, the stuck-o-meter's time) with one line of 27 px mono plus
  // two 36 px `label-lg` readouts, because 72 px of anything does not fit a 144 px cluster that
  // also carries a 38-rung spine. `data-role="hero"` therefore appears nowhere, and the floor
  // that matters is the body floor every other run is measured against.
  expect(runs.filter((run) => run.role === 'hero')).toEqual([]);

  // The rung line is the cluster's headline, and it is deliberately 27 px: between the 24 px body
  // floor and the 30 px label band, which is the locked layout's own number.
  const rungLine = runs.find((run) => run.cls.includes('rung-line__name'));
  expect(rungLine, 'the rung name is not rendered').toBeDefined();
  expect(rungLine?.fontPx).toBe(27);
});

test('every fixture holds the floor, including the cold open', async ({ page }) => {
  // The boot state is the one the audit says looks broken, and the one most likely to render an
  // empty panel with a stray small label in it. The LADDER tab is the one pinned here: 38 rung
  // rows at the 24 px floor in a 370 px pane is the tightest type on the page.
  // `bigpad` is here for the macro pad: nine buttons in two columns of seven under the game, which
  // is the tightest type on the page after the ladder's rungs and the one region the three older
  // fixtures cannot show at all (`docs/design/macros.md` section 14).
  for (const fixture of ['cold-open', 'steady', 'big-moment', 'macros', 'bigpad'] as const) {
    await gotoStage(page, { fixture, t: fixture === 'cold-open' ? 8 : 20, tab: 'ladder' });
    const tooSmall = (await measure(page)).filter((run) => run.effectivePx < 23.99);
    expect(tooSmall.map((run) => `${fixture}: ${run.effectivePx}px "${run.text}"`)).toEqual([]);
  }
});
