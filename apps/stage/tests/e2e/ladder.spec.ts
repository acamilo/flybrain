/**
 * The milestone spine at the long ladder's length.
 *
 * `rungCount` (tests/unit/ladder.test.ts) covers how many rungs the component asks for. What is
 * left is the part only a real layout can answer: whether 38 of them still read as a spine at 1080
 * authoring, which is what `docs/design/ladder.md` requires.
 *
 * Rail layout v2 gives the spine the full width of the progress cluster (984 px against layout
 * v1's 620 px ladder cell) and the re-recorded fixtures now carry `milestone.total: 38`, so the
 * page's own spine is already 38 rungs. This still rebuilds the children by hand — same classes,
 * same `data-state`, same `--ladder-rungs` custom property — because that is what lets it measure
 * 16 and 38 in the same run, inside the real `vite build` output at the real broadcast size.
 */
import { expect, test } from '@playwright/test';

import { gotoStage } from './stage';

/** The phone width the legibility suite checks, as a fraction of the 1920 px source. */
const PHONE_DOWNSCALE = 397 / 1920;

async function spineAt(page: import('@playwright/test').Page, rungs: number, current: number) {
  return page.evaluate(
    ({ rungs, current }) => {
      const spine = document.querySelector('[data-testid="ladder"]');
      if (!(spine instanceof HTMLElement)) throw new Error('no ladder spine on the page');
      spine.style.setProperty('--ladder-rungs', String(rungs));
      spine.replaceChildren(
        ...Array.from({ length: rungs }, (_, index) => {
          const rung = document.createElement('div');
          rung.className = 'ladder-rung';
          rung.dataset.state =
            index < current ? 'reached' : index === current ? 'current' : 'future';
          rung.dataset.rung = String(index);
          return rung;
        }),
      );
      const rects = [...spine.children].map((child) => child.getBoundingClientRect());
      const box = spine.getBoundingClientRect();
      const currentRect = rects[current];
      if (currentRect === undefined) throw new Error('no current rung');
      return {
        count: rects.length,
        spineWidth: box.width,
        spineRight: box.right,
        height: box.height,
        widths: rects.map((rect) => rect.width),
        currentWidth: currentRect.width,
        // Gap as laid out, between the first two rungs.
        gap: rects.length > 1 ? (rects[1] as DOMRect).left - (rects[0] as DOMRect).right : 0,
        // The panel the spine sits in, so overflow is detectable.
        panelRight:
          spine.closest('.panel')?.getBoundingClientRect().right ?? Number.POSITIVE_INFINITY,
      };
    },
    { rungs, current },
  );
}

test('the spine stays legible at 38 rungs at 1080 authoring', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95, res: '1080' });

  const long = await spineAt(page, 38, 22);
  expect(long.count).toBe(38);
  console.log(
    `  38 rungs in ${long.spineWidth.toFixed(1)} px: gap ${long.gap.toFixed(2)} px, ` +
      `rungs ${Math.min(...long.widths).toFixed(1)}-${Math.max(...long.widths).toFixed(1)} px, ` +
      `current ${long.currentWidth.toFixed(1)} px ` +
      `(${(long.currentWidth * PHONE_DOWNSCALE).toFixed(1)} px at the phone downscale)`,
  );

  // It still fits the cell: no rung escapes the panel, at any count.
  expect(long.spineRight).toBeLessThanOrEqual(long.panelRight + 0.5);
  const laidOut = long.widths.reduce((sum, width) => sum + width, 0) + long.gap * 37;
  expect(Math.abs(laidOut - long.spineWidth)).toBeLessThan(1);

  // The gap is the theme's own 2 px at every count, not layout v1's `48 / rungs` (which gave
  // 1.25 px at 38 and 3 px at 16): `docs/design/gameboy-theme.md` asks for "a row of square cells
  // with 2 px gaps", and a 1.25 px gap is a sub-pixel one at the phone downscale anyway. At 38
  // rungs that spends 74 px of the spine on gaps rather than 46 and still leaves each cell 23.8.
  expect(long.gap).toBeCloseTo(2, 1);

  // Thin segments are fine, but they have to survive the phone downscale as something: a
  // sub-pixel rung is gone, not thin.
  const thinnest = Math.min(...long.widths);
  expect(thinnest).toBeGreaterThan(8);
  expect(thinnest * PHONE_DOWNSCALE).toBeGreaterThan(1.5);
  expect(long.height).toBeGreaterThanOrEqual(15);

  // The current-rung marker is the one segment a viewer hunts for, so it does not shrink with
  // the rest: it holds its 20 px floor and is never narrower than the others.
  //
  // In rail layout v2 the floor no longer binds at all. The spine moved out of layout v1's 620 px
  // ladder cell and across the whole 984 px progress cluster, so 38 rungs are 24.7 px each and
  // every one of them clears 20 on its own — hence `toBeCloseTo` rather than `toBe`: the widths
  // are equal to within a subpixel of flex rounding, not identical.
  expect(long.currentWidth).toBeGreaterThanOrEqual(20);
  expect(long.currentWidth).toBeCloseTo(Math.max(...long.widths), 1);
  expect(long.currentWidth * PHONE_DOWNSCALE).toBeGreaterThan(4);
});

test('the 16-rung spine takes the same 2 px gap as the long one', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95, res: '1080' });

  // One gap for every ladder length, which is the point of fixing it to the grid: at 16 rungs the
  // cells are 59 px, so the current rung's 20 px floor never binds here either.
  const short = await spineAt(page, 16, 9);
  expect(short.count).toBe(16);
  expect(short.gap).toBeCloseTo(2, 1);
  expect(Math.min(...short.widths)).toBeGreaterThan(20);
  expect(short.currentWidth).toBeCloseTo(Math.max(...short.widths), 1);
});
