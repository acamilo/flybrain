/**
 * The auto-recovery splash, against the real build.
 *
 * The first two tests go through the page's real data path — the poller fetching
 * `/recovery-notice.json` — with Playwright answering the route, which is exactly what
 * `flystage-web` does with the helper's file. The rest drive the splash through
 * `window.__stage.recovery` with a pinned clock, for the geometry and the text floor.
 */
import { expect, test, type Page } from '@playwright/test';

import { stageUrl } from './stage';

const now = (): number => Math.floor(Date.now() / 1000);

function notice(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const t = now();
  return {
    v: 1,
    id: `${t}-reset`,
    phase: 'countdown',
    action: 'reset',
    fromRung: 12,
    fromLabel: 'MT. MOON',
    toRung: 11,
    toLabel: 'PEWTER CITY',
    reason: 'unrewarded',
    loop: ['GO OBJECTIVE', 'GO WARP'],
    stuckSeconds: 1800,
    announcedAt: t,
    executeAt: t + 45,
    updatedAt: t,
    ...overrides,
  };
}

async function open(page: Page): Promise<void> {
  await page.goto(`${stageUrl({ t: 95, tab: 'senses', audio: false })}&recovery=1`);
  await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });
}

test('a polled notice shows the countdown, follows the phases and clears', async ({ page }) => {
  let body: string | null = JSON.stringify(notice());
  await page.route('**/recovery-notice.json', (route) =>
    body === null
      ? route.fulfill({ status: 204 })
      : route.fulfill({ status: 200, contentType: 'application/json', body }),
  );
  await open(page);

  const splash = page.getByTestId('recovery-splash');
  await expect(splash).toBeVisible({ timeout: 10_000 });
  await expect(splash).toHaveAttribute('data-phase', 'countdown');
  await expect(splash).toHaveAttribute('data-layout', 'box');
  await expect(page.getByTestId('recovery-countdown')).toHaveText(/^0:4\d$/);
  await expect(page.getByTestId('recovery-body')).toContainText('Rewinding to PEWTER CITY');
  await expect(page.getByTestId('recovery-loop')).toContainText('GO OBJECTIVE > GO WARP');

  body = JSON.stringify(notice({ phase: 'acting' }));
  await expect(splash).toHaveAttribute('data-layout', 'cover', { timeout: 5_000 });

  body = JSON.stringify(notice({ phase: 'done' }));
  await expect(splash).toHaveAttribute('data-phase', 'done', { timeout: 5_000 });

  // The file going away is also an end: the page never holds a notice the server no longer has.
  body = null;
  await expect(splash).toHaveCount(0, { timeout: 5_000 });
});

test('stale, malformed and missing notices never show', async ({ page }) => {
  const bodies = [
    JSON.stringify(notice({ updatedAt: now() - 16 * 60 })),
    '{"v":1,"phase":"countdown"',
    JSON.stringify(notice({ v: 2 })),
    JSON.stringify(notice({ phase: 'done', updatedAt: now() - 60 })),
  ];
  let index = 0;
  await page.route('**/recovery-notice.json', (route) => {
    const body = bodies[Math.min(index, bodies.length - 1)] as string;
    index += 1;
    return route.fulfill({ status: 200, contentType: 'application/json', body });
  });
  await open(page);
  await expect.poll(() => index, { timeout: 10_000 }).toBeGreaterThan(bodies.length);
  await expect(page.getByTestId('recovery-splash')).toHaveCount(0);
});

test('player mode does not poll unless asked', async ({ page }) => {
  let hits = 0;
  await page.route('**/recovery-notice.json', (route) => {
    hits += 1;
    return route.fulfill({ status: 204 });
  });
  await page.goto(stageUrl({ t: 95, tab: 'senses', audio: false }));
  await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });
  await page.waitForTimeout(2500);
  expect(hits).toBe(0);
});

test('the box leaves the top of the game clear; the cover stays inside the game', async ({ page }) => {
  await open(page);
  const game = await page.getByTestId('game').boundingBox();
  if (!game) throw new Error('no game box');
  const t = 1_790_629_095;
  const base = notice({ announcedAt: t, executeAt: t + 60, updatedAt: t });

  await page.evaluate(([raw, nowS]) => window.__stage?.recovery(raw, nowS as number), [base, t + 18] as const);
  // Measure after the 320 ms rise, not during it.
  await page.locator('.recovery__box').evaluate((element) =>
    Promise.all(element.getAnimations().map((animation) => animation.finished)).then(() => undefined),
  );
  const box = await page.locator('.recovery__box').boundingBox();
  if (!box) throw new Error('no text box');
  expect(box.y).toBeGreaterThan(game.y + game.height / 2);
  expect(box.x).toBeGreaterThanOrEqual(game.x);
  expect(box.x + box.width).toBeLessThanOrEqual(game.x + game.width);
  expect(box.y + box.height).toBeLessThanOrEqual(game.y + game.height);
  await expect(page.getByTestId('recovery-countdown')).toHaveText('0:42');

  await page.evaluate(
    ([raw, nowS]) => window.__stage?.recovery(raw, nowS as number),
    [{ ...base, phase: 'acting', updatedAt: t + 60 }, t + 70] as const,
  );
  const cover = await page.locator('.recovery__cover').boundingBox();
  if (!cover) throw new Error('no cover');
  expect(cover).toEqual(game);

  // Every run of text on the splash sits on the page's body floor.
  const sizes = await page.evaluate(() =>
    [...document.querySelectorAll('[data-testid="recovery-splash"] *')]
      .filter((element) => [...element.childNodes].some((node) => node.nodeType === 3 && node.textContent?.trim()))
      .map((element) => Number.parseFloat(getComputedStyle(element).fontSize)),
  );
  expect(sizes.length).toBeGreaterThan(3);
  for (const size of sizes) expect(size).toBeGreaterThanOrEqual(24);

  await page.evaluate(() => window.__stage?.recovery(null));
  await expect(page.getByTestId('recovery-splash')).toHaveCount(0);
});
