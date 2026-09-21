/**
 * The audit's structural findings, as regressions that cannot come back (design A10.4).
 *
 * Each of these was a real bug on the old page: a footer grid that wrapped and left an empty
 * band, a 2.62x game scale that gave the encoder row-thickness jitter, a mobile media query that
 * escaped its block, and a WebGL context nobody meant to create.
 */
import { GAMEBOY_BUTTONS } from '@flybrain/brain';
import { MACRO_CHANNELS, MACRO_TYPES } from '@flybrain/feed';
import { expect, test } from '@playwright/test';

import {
  FLY_BUTTON_ROW_HEIGHT,
  FLY_CANVAS_WIDTH,
  LAYOUT,
  MACRO_CELL_COLUMNS,
  MACRO_CELL_COUNT,
  MACRO_CELL_HEIGHT,
  MACRO_CELL_ROWS,
  MACRO_CELL_WIDTH,
  MACROS_TAB_COLUMNS,
  MACROS_TAB_ROWS,
  NO_CONTENT_ZONE,
  RAIL_BOX,
  TAB_STRIP_HEIGHT,
} from '../../src/lib/geometry';
import { DESCRIBE_CARD } from '../../src/games/describe';
import { TABS } from '../../src/lib/tabs';
import { BUTTON_GLYPHS } from '../../src/panels/FlyStrip';
import { gotoStage, stageUrl, visibleTabs } from './stage';

test('nothing scrolls, in either axis', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const scroll = await page.evaluate(() => ({
    height: document.documentElement.scrollHeight,
    clientHeight: document.documentElement.clientHeight,
    width: document.documentElement.scrollWidth,
    clientWidth: document.documentElement.clientWidth,
  }));
  expect(scroll.height).toBeLessThanOrEqual(scroll.clientHeight);
  expect(scroll.width).toBeLessThanOrEqual(scroll.clientWidth);
});

for (const tab of TABS) {
  test(`no panel clips its own content, ${tab} tab`, async ({ page }) => {
    // The panels are `overflow: hidden` at exact sizes, so a panel whose content is taller than
    // its box silently eats a line — which is how three of them shipped their first render. Run
    // once per tab, because the slot's four panes are four layouts in one 370 px box: the 38-rung
    // ladder is the tightest of them and DESCRIBE's paragraph the one that moves when the copy does.
    await gotoStage(page, { fixture: 'steady', t: 95, tab });

  const clipped = await page.evaluate(() =>
    [...document.querySelectorAll('.panel__body')]
      .map((element) => ({
        text: (element.textContent ?? '').trim().slice(0, 40),
        overflowY: element.scrollHeight - element.clientHeight,
        overflowX: element.scrollWidth - element.clientWidth,
      }))
      .filter((entry) => entry.overflowY > 1 || entry.overflowX > 1),
    );
    expect(clipped).toEqual([]);
  });
}

test('the game canvas is an exact integer 5x of 160x144 at the broadcast size', async ({ page }) => {
  // One authoring resolution, and it is the one that goes on air: 800x720 is exactly 5x the
  // 160x144 framebuffer, in both the backing store and the on-screen box, so no source row lands
  // across two device rows (the audit's 2.62x finding).
  await gotoStage(page, { fixture: 'steady', t: 95, res: '1080' });

  const game = await page.evaluate(() => {
    const canvas = document.querySelector<HTMLCanvasElement>('[data-testid="game"] canvas');
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    return {
      backingWidth: canvas.width,
      backingHeight: canvas.height,
      renderedWidth: rect.width,
      renderedHeight: rect.height,
      imageRendering: getComputedStyle(canvas).imageRendering,
      reportedScale: window.__stage?.gameScale() ?? 0,
    };
  });

  expect(game).not.toBeNull();
  expect((game as { backingWidth: number }).backingWidth / 160, 'backing store scale').toBe(5);
  expect((game as { backingHeight: number }).backingHeight / 144, 'backing store scale').toBe(5);
  expect((game as { renderedWidth: number }).renderedWidth).toBe(800);
  expect((game as { renderedHeight: number }).renderedHeight).toBe(720);
  expect(game?.reportedScale).toBe(5);
  expect(game?.imageRendering).toBe('pixelated');
});

test('the 720p thumbnail mode is a pure 2/3 downscale of the same 5x canvas', async ({ page }) => {
  // `?res=720` is `transform: scale(0.6667)` on the one stage, for thumbnails and the downscale
  // tests. The rendered box is therefore *not* an integer multiple (800 x 0.6667 = 533.4) and is
  // not meant to be: what must stay integer is the backing store, which is the same build's.
  await gotoStage(page, { fixture: 'steady', t: 95, res: '720' });

  const view = await page.evaluate(() => {
    const canvas = document.querySelector<HTMLCanvasElement>('[data-testid="game"] canvas');
    const stage = document.querySelector('#stage');
    if (!canvas || !stage) return null;
    return {
      backingScale: canvas.width / 160,
      stageWidth: Math.round(stage.getBoundingClientRect().width),
      stageHeight: Math.round(stage.getBoundingClientRect().height),
    };
  });

  expect(view?.backingScale).toBe(5);
  expect(view?.stageWidth).toBe(1280);
  expect(view?.stageHeight).toBe(720);
});

test('the fly is the only WebGL context, and there is none without it', async ({ page }) => {
  // Asked, not observed: calling getContext('webgl') to look would create the very thing being
  // counted. The brain map is 2D canvas by decision 3, and an accidental context is a silent
  // one-to-two core regression on the capture VM — so the count is exactly one with the fly in
  // WebGL mode (`docs/design/fly-avatar.md`) and exactly zero in either fallback.
  await page.addInitScript(() => {
    const requested: string[] = [];
    (window as unknown as { __glRequests: string[] }).__glRequests = requested;
    const original = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function patched(
      this: HTMLCanvasElement,
      kind: string,
      ...rest: unknown[]
    ) {
      if (kind.includes('webgl') || kind.includes('experimental')) requested.push(kind);
      return (original as (...args: unknown[]) => unknown).call(this, kind, ...rest);
    } as typeof HTMLCanvasElement.prototype.getContext;
  });

  const count = async () => page.evaluate(() => (window as unknown as { __glRequests: string[] }).__glRequests.length);

  await gotoStage(page, { fixture: 'big-moment', t: 17.5, fly: 'webgl' });
  expect(await count(), 'the fly should hold exactly one GL context').toBe(1);
  expect(await page.evaluate(() => window.__stage?.fly().rendering)).toBe(true);

  await gotoStage(page, { fixture: 'big-moment', t: 17.5, fly: 'paper' });
  expect(await count(), 'the paper fly must not touch WebGL').toBe(0);
  expect(await page.evaluate(() => window.__stage?.fly().rendering)).toBe(true);

  await gotoStage(page, { fixture: 'big-moment', t: 17.5, fly: 'off' });
  expect(await count(), 'fly=off must create no context at all').toBe(0);
  expect(await page.evaluate(() => window.__stage?.fly().rendering)).toBe(false);
});

for (const mode of ['webgl', 'paper'] as const) {
  test(`the ${mode} fly canvas paints within 2 s of data-ready, and carries no text`, async ({ page }) => {
    // The design's verification list: "strip renders within 2 s of `data-ready`". "Renders" is
    // measured rather than asserted — the canvas's pixels must actually vary, which a canvas
    // painted flat with the panel colour would fail. The fly's own canvas carries no text at all;
    // the strip's only text is the button row along its top edge, checked separately below.
    await page.goto(stageUrl({ fixture: 'steady', t: 95, fly: mode }));
    await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });

    await expect
      .poll(async () => page.evaluate(() => window.__stage?.fly().rendering ?? false), { timeout: 2000 })
      .toBe(true);

    const strip = page.locator('[data-testid="fly-strip"]');
    const canvas = page.locator('[data-testid="fly-canvas"]');
    // The fly's half of the strip carries no text at all: no caption, and nothing its canvas draws
    // is DOM text. The strip's text is the button row's glyphs and the macro strip's cells, both
    // of which are their own elements and are checked in `macros.spec.ts` and below.
    expect((await page.locator('[data-testid="fly-pane"]').innerText()).trim()).toBe('');
    const rowText = (await page.locator('[data-testid="button-row"]').innerText()).trim().split(/\s+/).join(' ');
    expect(rowText, 'the button row is not the eight glyphs').toBe(
      GAMEBOY_BUTTONS.map((button) => BUTTON_GLYPHS[button] ?? button.slice(0, 2).toUpperCase()).join(' '),
    );
    expect(await strip.getAttribute('data-fly')).toBe(mode);

    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    const shot = await page.screenshot({
      clip: { x: box?.x ?? 0, y: box?.y ?? 0, width: box?.width ?? 1, height: box?.height ?? 1 },
    });

    // Measure the strip's own pixels the way the legibility test does: decode it in a blank page
    // and take the luminance standard deviation. A blank strip scores near zero.
    const viewer = await page.context().newPage();
    await viewer.setContent('<body style="margin:0"></body>');
    const rms = await viewer.evaluate(async (url) => {
      const image = new Image();
      image.src = url;
      await image.decode();
      const canvas = document.createElement('canvas');
      canvas.width = image.width;
      canvas.height = image.height;
      const ctx = canvas.getContext('2d');
      if (!ctx) throw new Error('no 2d context');
      ctx.drawImage(image, 0, 0);
      const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
      let sum = 0;
      let squares = 0;
      const count = data.length / 4;
      for (let i = 0; i < data.length; i += 4) {
        const luminance =
          ((data[i] as number) * 0.2126 + (data[i + 1] as number) * 0.7152 + (data[i + 2] as number) * 0.0722) / 255;
        sum += luminance;
        squares += luminance * luminance;
      }
      const mean = sum / count;
      return Math.sqrt(Math.max(0, squares / count - mean * mean));
    }, `data:image/png;base64,${shot.toString('base64')}`);
    await viewer.close();

    expect(rms, `the ${mode} fly canvas looks blank (RMS ${rms.toFixed(4)})`).toBeGreaterThan(0.02);
  });
}

test('the button row is a plain row of eight chips that respects the afterglow', async ({ page }) => {
  // Restored per review: the fly no longer taps a Game Boy, so the eight indicators are their own
  // plain row again, along the fly strip's top edge, 32 px tall — the fly's own column wide since
  // `docs/design/macros.md` section 14 gave the band beside it to the macro pad.
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const row = page.locator('[data-testid="button-row"]');
  await expect(row).toHaveCount(1);

  const rowBox = await row.boundingBox();
  const stripBox = await page.locator('[data-testid="fly-strip"]').boundingBox();
  expect(rowBox).not.toBeNull();
  expect(stripBox).not.toBeNull();
  if (rowBox && stripBox) {
    // Along the top edge, over the fly's canvas, and the documented height. The tolerance is for
    // the strip's own `--border-w`, which insets the row from the strip's outer edge on every
    // side: the Game Boy pass took that frame from 2 px to 4 (`docs/design/gameboy-theme.md`, "a
    // 4 px outer border with a 2 px inner line"), so the row is 8 px narrower than its column
    // rather than 4, and a tolerance of 6 failed on the frame it was supposed to allow for.
    const tolerance = 10;
    expect(Math.abs(rowBox.x - stripBox.x), 'the row should hug the strip\'s left edge').toBeLessThan(tolerance);
    expect(Math.abs(rowBox.y - stripBox.y), 'the row should sit on the strip\'s top edge').toBeLessThan(tolerance);
    // The fly's column, not the strip: the pad takes the rest of the band (`src/lib/geometry.ts`).
    expect(Math.round(rowBox.width), 'the row should span the fly\'s canvas').toBe(FLY_CANVAS_WIDTH);
    expect(rowBox.x + rowBox.width, 'the row must stay left of the pad').toBeLessThanOrEqual(
      LAYOUT.macroPalette.x,
    );
    expect(Math.round(rowBox.height)).toBe(FLY_BUTTON_ROW_HEIGHT);
  }

  const cells = await row.locator('[data-button]').all();
  expect(cells.length, 'the row must hold exactly eight chips').toBe(8);

  const order = await row.locator('[data-button]').evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-button')));
  expect(order).toEqual([...GAMEBOY_BUTTONS]);

  const glyphs = await row.locator('[data-button]').allInnerTexts();
  expect(glyphs).toEqual(GAMEBOY_BUTTONS.map((button) => BUTTON_GLYPHS[button] ?? button.slice(0, 2).toUpperCase()));

  // Each cell carries its own `data-down`/`data-glow` afterglow attributes — the paint loop's
  // `paintButtons` (`src/App.tsx`) — rather than the removed single `data-caps` string on the
  // strip. The exact timing of the 250 ms afterglow is `behaviour.spec.ts`'s job; this just checks
  // the plumbing landed on each chip.
  for (const cell of cells) {
    expect(['0', '1'], 'data-down must be a written afterglow flag').toContain(await cell.getAttribute('data-down'));
    expect(['0', '1'], 'data-glow must be a written afterglow flag').toContain(await cell.getAttribute('data-glow'));
  }
});

test('no console errors, and no unhandled page errors', async ({ page }) => {
  const problems: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') problems.push(`console: ${message.text()}`);
  });
  page.on('pageerror', (error) => problems.push(`pageerror: ${error.message}`));

  await gotoStage(page, { fixture: 'big-moment', t: 10, play: true });
  await page.waitForTimeout(4000);
  expect(problems).toEqual([]);
});

test('no media query changes the type scale at 1920x1080', async ({ page }) => {
  // The exact audit bug: a mobile type scale that escaped its `@media` block and applied
  // everywhere. Any matching media rule that sets a font size is the failure.
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const offenders = await page.evaluate(() => {
    const found: string[] = [];
    for (const sheet of [...document.styleSheets]) {
      let rules: CSSRuleList;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      for (const rule of [...rules]) {
        if (!(rule instanceof CSSMediaRule)) continue;
        if (!window.matchMedia(rule.conditionText).matches) continue;
        for (const inner of [...rule.cssRules]) {
          const text = inner.cssText;
          if (/font-size|--fs-/.test(text)) found.push(`@media ${rule.conditionText} { ${text.slice(0, 80)} }`);
        }
      }
    }
    return found;
  });
  expect(offenders).toEqual([]);
});

test('no text or readout lands in the bottom-left no-content zone', async ({ page }) => {
  // Twitch overlays chat and extensions here. The fly strip's canvas reaches into the box at
  // 1920x1080, and that is fine — the canvas carries no DOM text at all — but nothing readable may
  // be in it. The button row does carry text, but it sits along the strip's top edge, well clear
  // of this zone (`docs/design/fly-avatar.md`); the macro strip's cells do too, and they are the
  // reason the fly's canvas is 416 px wide rather than the strip's full width, so this runs on the
  // macros fixture as well as the steady one.
  for (const fixture of ['steady', 'macros'] as const) {
    await gotoStage(page, { fixture, t: fixture === 'macros' ? 15.2 : 95 });

    const intruders = await page.evaluate(() => {
      const zoneElement = document.querySelector('[data-nocontent="1"]');
      if (!zoneElement) return ['the no-content zone is not declared in the layout'];
      const zone = zoneElement.getBoundingClientRect();

      const hits: string[] = [];
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        const text = (node.textContent ?? '').trim();
        if (!text) continue;
        const element = node.parentElement;
        if (!element) continue;
        const rect = element.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;
        const overlaps =
          rect.left < zone.right && zone.left < rect.right && rect.top < zone.bottom && zone.top < rect.bottom;
        if (overlaps) hits.push(`"${text.slice(0, 30)}" at ${Math.round(rect.left)},${Math.round(rect.top)}`);
      }
      return hits;
    });
    expect(intruders, fixture).toEqual([]);
  }
});

test('the layout matches geometry.ts, so the tests and the page agree', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const boxes = await page.evaluate(() => {
    const read = (selector: string) => {
      const element = document.querySelector(selector);
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return {
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        w: Math.round(rect.width),
        h: Math.round(rect.height),
      };
    };
    return {
      stage: read('#stage'),
      game: read('[data-testid="game"]'),
      flyStrip: read('[data-testid="fly-strip"]'),
      flyPane: read('[data-testid="fly-pane"]'),
      macroPalette: read('[data-testid="macro-palette"]'),
      tabSlot: read('[data-testid="tab-slot"]'),
      tabStrip: read('[data-testid="tab-strip"]'),
      pane: read('[data-tab-pane="connectome"]'),
      ticker: read('[data-testid="events-panel"]'),
      chat: read('[data-testid="chat-panel"]'),
      map: read('[data-testid="brain-map"]'),
    };
  });

  expect(boxes.stage).toEqual({ x: 0, y: 0, w: 1920, h: 1080 });
  expect(boxes.game).toEqual({ x: 48, y: 88, w: 800, h: 720 });
  // The button row lives along the fly strip's own top edge, under the game.
  expect(boxes.flyStrip).toEqual({ x: 48, y: 812, w: 800, h: 220 });

  // Inside it, the fly's column and the macro pad, and the pad starts at x = 480 — the no-content
  // zone's right edge, which is what sizes the fly's canvas (`src/lib/geometry.ts`). The pad spans
  // the strip's whole inner height since `docs/design/macros.md` section 14; both it and the fly's
  // canvas end on 1028, the strip's inner bottom.
  expect(boxes.flyPane).toEqual({
    x: LAYOUT.flyPane.x,
    y: LAYOUT.flyPane.y,
    w: LAYOUT.flyPane.width,
    h: LAYOUT.flyPane.height,
  });
  expect(boxes.macroPalette).toEqual({
    x: LAYOUT.macroPalette.x,
    y: LAYOUT.macroPalette.y,
    w: LAYOUT.macroPalette.width,
    h: LAYOUT.macroPalette.height,
  });
  expect(boxes.macroPalette?.x).toBe(NO_CONTENT_ZONE.x + NO_CONTENT_ZONE.width);
  expect((boxes.flyPane?.y ?? 0) + (boxes.flyPane?.h ?? 0)).toBe(
    (boxes.macroPalette?.y ?? 0) + (boxes.macroPalette?.h ?? 0),
  );

  // Rail layout v2, locked: the slot at 244 for 420 with a 48 px strip, the ticker at 676 for
  // 100, chat at 788 for 244, and the rail closing on 1032.
  expect(boxes.tabSlot).toEqual({ x: 860, y: 244, w: 1012, h: 420 });
  expect(boxes.ticker).toEqual({ x: 860, y: 676, w: 1012, h: 100 });
  expect(boxes.chat).toEqual({ x: 860, y: 788, w: 1012, h: 244 });
  expect(RAIL_BOX.y + RAIL_BOX.height).toBe(1032);

  // The strip is 48 px from the panel's own top edge, border included, which is what puts the
  // pane on 292 and makes it exactly the 1008x370 the brain map's backing store is allocated at.
  expect((boxes.tabStrip?.y ?? 0) + (boxes.tabStrip?.h ?? 0) - (boxes.tabSlot?.y ?? 0)).toBe(TAB_STRIP_HEIGHT);
  const pane = { x: LAYOUT.tabContent.x, y: LAYOUT.tabContent.y, w: LAYOUT.tabContent.width, h: LAYOUT.tabContent.height };
  expect(boxes.pane).toEqual(pane);
  // And the map is drawn at that size rather than scaled into it.
  expect(boxes.map).toEqual(pane);
});

test('exactly one tab is visible, whichever tab that is', async ({ page }) => {
  // The one invariant of a five-pane slot whose panes all stay mounted (the connectome's base
  // bitmap is a worker's 139,255-point raster and must not be re-rastered on every cycle).
  for (const tab of TABS) {
    await gotoStage(page, { fixture: 'steady', t: 95, tab });
    expect(await visibleTabs(page)).toEqual([tab]);

    const painted = await page.evaluate(
      () =>
        [...document.querySelectorAll('[data-tab-pane]')].filter(
          (node) => getComputedStyle(node).visibility !== 'hidden',
        ).length,
    );
    expect(painted, `${tab}: more than one pane is painted`).toBe(1);
  }

  // Unpinned, the slot still shows exactly one: the cycle is a crossfade, never a cut.
  await gotoStage(page, { fixture: 'steady', t: 95 });
  expect(await visibleTabs(page)).toHaveLength(1);
});

test('the chat panel is absent with no chat in the feed, and with the kill switch on', async ({ page }) => {
  // The header omits `chat` entirely when the service has chat disabled
  // (`docs/feed-protocol.md`), and the cold-open fixture was recorded before the field existed, so
  // it is the real "no chat" case. What must not happen is an empty bordered box: a viewer reads
  // that as a broken stream.
  await gotoStage(page, { fixture: 'cold-open', t: 8 });
  await expect(page.locator('[data-testid="chat-panel"]')).toHaveCount(0);
  expect(await page.evaluate(() => window.__stage?.state().chat.length ?? -1)).toBe(0);

  // With chat in the feed it is there, and bounded at seven lines.
  await gotoStage(page, { fixture: 'steady', t: 95 });
  await expect(page.locator('[data-testid="chat-panel"]')).toHaveCount(1);
  const lines = await page.locator('.chat-line').count();
  expect(lines).toBeGreaterThan(0);
  expect(lines).toBeLessThanOrEqual(7);

  // Unless the kill switch is on, which is one query parameter and no panel.
  await gotoStage(page, { fixture: 'steady', t: 95, chat: 'off' });
  await expect(page.locator('[data-testid="chat-panel"]')).toHaveCount(0);
});

test('chat renders text and nothing else: no links, no markup, no images', async ({ page }) => {
  // The page re-validates every line through the shared sanitizer on the way to the DOM
  // (`src/chat/sanitize.ts`), so this asserts the outcome: the panel's subtree is two spans of
  // text per line, and nothing in it can navigate, load or execute.
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const shape = await page.evaluate(() => {
    const panel = document.querySelector('[data-testid="chat-panel"]');
    if (!panel) return null;
    const urlish = /(https?:)|(www\.)|(\.(com|net|org|tv|gg)\b)/i;
    return {
      tags: [...panel.querySelectorAll('*')].map((node) => node.tagName),
      urls: [...panel.querySelectorAll('.chat-line__text')].filter((node) =>
        urlish.test(node.textContent ?? ''),
      ).length,
    };
  });

  expect(shape).not.toBeNull();
  expect(shape?.urls, 'a chat line advertises a link').toBe(0);
  const allowed = new Set(['DIV', 'SPAN']);
  expect(shape?.tags.filter((tag) => !allowed.has(tag)), 'the chat panel renders something but text').toEqual([]);
});

test('the page reports ready only once the fonts and the brain map are in', async ({ page }) => {
  await page.goto(stageUrl({ fixture: 'steady', t: 95 }));
  await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });

  const state = await page.evaluate(() => ({
    degraded: document.documentElement.dataset.degraded ?? null,
    fontsReady: document.fonts.status,
    loaded: [...document.fonts].filter((face) => face.status === 'loaded').map((face) => face.family),
  }));

  expect(state.degraded, 'the page went ready without its brain map').toBeNull();
  expect(state.fontsReady).toBe('loaded');
  // Three faces on the page since the 2026-09-16 pairing (`src/theme/tokens.css`): Silkscreen for
  // `--font-label`, VT323 for `--font-text`, Press Start 2P for `.num` and the wordmark. Inter and
  // IBM Plex Mono are gone, files and all; Pixelify Sans stays committed and declared because
  // `tools/font-compare.mts` still renders the three-way comparison frame from it, and nothing on
  // the live page references it, so it is never fetched — which is what this asserts, since a
  // fourth loaded face would mean it had crept into use.
  expect(state.loaded).toContain('Silkscreen');
  expect(state.loaded).toContain('VT323');
  expect(state.loaded).toContain('Press Start 2P');
  expect(state.loaded).not.toContain('Pixelify Sans');
});

test('every fixture renders feed values, not the initial state', async ({ page }) => {
  // The failure this guards against is silent and total: the 4 Hz commit gate stayed shut after a
  // seek froze the clock, so the page rendered its own zeroes over a feed that had already
  // delivered sixteen snapshots. The cold open is where it showed, because it is the only fixture
  // with no events to force a commit of their own.
  for (const fixture of ['cold-open', 'steady', 'big-moment'] as const) {
    await gotoStage(page, { fixture, t: fixture === 'cold-open' ? 8 : 20 });

    const state = await page.evaluate(() => {
      const snapshot = window.__stage?.state();
      return {
        label: snapshot?.milestone.label ?? '',
        next: snapshot?.milestone.next ?? '',
        populationRate: snapshot?.populationRate ?? 0,
        accepted: window.__stage?.health().accepted ?? 0,
      };
    });

    expect(state.accepted, `${fixture}: no snapshots ingested`).toBeGreaterThan(5);
    expect(state.label, `${fixture}: the milestone label never reached the DOM state`).not.toBe('');
    expect(state.next, `${fixture}: the next milestone never reached the DOM state`).not.toBe('');
    expect(state.populationRate, `${fixture}: the population rate is still the initial zero`).toBeGreaterThan(0);

    // And it is on screen, not just in the store.
    await expect(page.locator('[data-testid="rank-name"]')).toHaveText(state.label);
  }
});

test('the platformer config drives the same page with its own ladder', async ({ page }) => {
  // The game-agnostic claim, tested rather than asserted: a different config, a different ladder,
  // different mode words and different reward copy, with no code change. The rung *count* is the
  // feed's for every game — the re-recorded fixtures carry `milestone.total: 38`, which is the
  // Pokemon service's ladder — so what differs here is the rung *text*: Super Mario Land knows
  // sixteen names and the rest of the spine's rows are numbered blanks rather than the other
  // game's rungs. The LADDER tab is pinned because that is the pane the names are in.
  await gotoStage(page, { fixture: 'steady', t: 95, game: 'platformer', tab: 'ladder' });

  const view = await page.evaluate(() => ({
    game: document.documentElement.dataset.game,
    wordmark: document.querySelector('[data-testid="wordmark"]')?.textContent ?? '',
    rungs: document.querySelectorAll('[data-testid="ladder"] .ladder-rung').length,
    total: window.__stage?.state().milestone.total ?? null,
    named: [...document.querySelectorAll('.ladder__label')]
      .map((node) => node.textContent ?? '')
      .filter((text) => text.length > 0),
    describe: document.querySelector('[data-testid="describe"]')?.textContent ?? '',
    body: document.body.textContent ?? '',
  }));

  expect(view.game).toBe('platformer');
  expect(view.wordmark).toContain('SUPER MARIO LAND');
  expect(view.rungs).toBe(view.total ?? 16);
  expect(view.named.length).toBeLessThanOrEqual(16);
  expect(view.body.toLowerCase()).toContain('cleared 1-1');
  expect(view.body.toLowerCase(), 'the other game leaked into the frame').not.toContain('pokemon');
  // Including the DESCRIBE tab. The approved card names no game at all (it explains the
  // connectome, not the cartridge), so what this asserts is that it is *there* under the other
  // config and that it did not bring the Pokemon words with it — the body check above covers the
  // leak, and `{game}` still resolves from the config for any card that takes it.
  expect(view.describe).toContain(DESCRIBE_CARD.title);
});

test('the title strip carries a non-empty release version', async ({ page }) => {
  // `vite.config.ts` injects `__STAGE_VERSION__` from `git describe --tags --always --dirty` at
  // build time, so this is one of: a tag (`v0.1.0`, optionally `-dirty` or `-N-g<sha>`), a bare
  // short sha when the checkout has no reachable tag, or `dev` from the dev server.
  await gotoStage(page, { fixture: 'steady', t: 95 });

  const text = await page.locator('[data-testid="stage-version"]').textContent();
  expect(text, 'the version chip is empty').toBeTruthy();
  expect((text ?? '').trim()).toMatch(/^(v\d+\.\d+\.\d+|[0-9a-f]{7,}|dev)/);
});

test('the pad under the game is two columns of seven, and never more than fourteen cells', async ({
  page,
}) => {
  // `docs/design/macros.md` section 14's first half: the strip shows every button on the pad now,
  // up to fourteen, as two columns of seven at the 24 px floor. The fixture is `bigpad`, whose
  // nine-button pad is the only one that reaches the second column
  // (`packages/feed/src/fake/palette.ts`).
  await gotoStage(page, { fixture: 'bigpad', t: 15 });

  const pad = await page.evaluate(() => {
    const root = document.querySelector<HTMLElement>('[data-testid="macro-palette"]');
    if (!root) return null;
    const style = getComputedStyle(root);
    const cells = [...root.querySelectorAll<HTMLElement>('[data-macro-row]')].map((cell) => {
      const rect = cell.getBoundingClientRect();
      return {
        name: cell.dataset.macroName ?? '',
        bound: cell.dataset.bound ?? '',
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    });
    return {
      flow: style.gridAutoFlow,
      columns: style.gridTemplateColumns.split(' ').length,
      rows: style.gridTemplateRows.split(' ').length,
      cells,
    };
  });

  expect(pad, 'the pad is not in the page').not.toBeNull();
  expect(pad?.columns).toBe(MACRO_CELL_COLUMNS);
  expect(pad?.rows).toBe(MACRO_CELL_ROWS);
  // Column-major: a pad reads down its first column and then down the second, which is the order
  // the contract's type list was written in.
  expect(pad?.flow).toContain('column');

  const cells = pad?.cells ?? [];
  expect(cells.length, 'the pad drew nothing').toBeGreaterThan(7);
  expect(cells.length, 'the pad drew more cells than the strip holds').toBeLessThanOrEqual(MACRO_CELL_COUNT);
  // Every cell the pad draws is a button that is *on* the pad: the dim ones are the MACROS tab's
  // job, and a pad that drew them would be the layout section 14 rejected.
  expect(cells.map((cell) => cell.bound)).toEqual(cells.map(() => '1'));
  expect(new Set(cells.map((cell) => cell.name)).size, 'a macro is on the pad twice').toBe(cells.length);

  for (const cell of cells) {
    expect(cell.width, `${cell.name} is the wrong width`).toBe(Math.round(MACRO_CELL_WIDTH));
    expect(cell.height, `${cell.name} is the wrong height`).toBe(Math.round(MACRO_CELL_HEIGHT));
    // And nothing readable crosses back into Twitch's overlay band.
    expect(cell.x, `${cell.name} is inside the no-content zone`).toBeGreaterThanOrEqual(
      NO_CONTENT_ZONE.x + NO_CONTENT_ZONE.width,
    );
  }

  // Two columns means two distinct x positions, and seven rows at most per column.
  const columns = [...new Set(cells.map((cell) => cell.x))].sort((a, b) => a - b);
  expect(columns.length, 'the second column is empty on a nine-button pad').toBe(2);
  for (const x of columns) {
    expect(cells.filter((cell) => cell.x === x).length).toBeLessThanOrEqual(MACRO_CELL_ROWS);
  }
});

test('the MACROS tab is the whole keyboard, three columns, bound lit and unbound dim', async ({ page }) => {
  // Section 14's second half. Every type in the contract has a cell here whatever the scene is
  // doing, which is what the tab is *for*: the dim cells are the buttons this scene has taken
  // away. The rows come from `MACRO_TYPES`, so this grows with the contract and the test does not
  // name a count of its own.
  await gotoStage(page, { fixture: 'bigpad', t: 15, tab: 'macros' });

  const grid = await page.evaluate(() => {
    const root = document.querySelector<HTMLElement>('[data-testid="macros-tab"]');
    if (!root) return null;
    const style = getComputedStyle(root);
    const cells = [...root.querySelectorAll<HTMLElement>('[data-macro-row]')].map((cell) => ({
      name: cell.dataset.macroName ?? '',
      bound: cell.dataset.bound ?? '',
      tag: cell.querySelector('.macro-cell__glyph')?.textContent?.trim() ?? '',
      label: cell.querySelector('.macro-cell__name')?.textContent?.trim() ?? '',
      x: Math.round(cell.getBoundingClientRect().x),
    }));
    return {
      columns: style.gridTemplateColumns.split(' ').length,
      rows: style.gridTemplateRows.split(' ').length,
      cells,
    };
  });

  expect(grid, 'the MACROS pane is not in the page').not.toBeNull();
  expect(grid?.columns).toBe(MACROS_TAB_COLUMNS);
  expect(grid?.rows).toBe(MACROS_TAB_ROWS);

  const cells = grid?.cells ?? [];
  expect(cells.map((cell) => cell.name)).toEqual([...MACRO_TYPES]);
  // Each cell carries its type's own tag and the contract's own name, not the pad's short form.
  expect(cells.map((cell) => cell.tag)).toEqual([...MACRO_CHANNELS]);
  expect(cells.map((cell) => cell.label)).toEqual([...MACRO_TYPES]);
  expect([...new Set(cells.map((cell) => cell.x))].length).toBe(MACROS_TAB_COLUMNS);

  // Lit and dim, and both kinds are on screen: the pad's nine buttons against the rest of the
  // keyboard. A run where every cell was one or the other would pass an assertion that only
  // checked the attribute exists.
  const bound = cells.filter((cell) => cell.bound === '1');
  expect(bound.length, 'nothing is lit on the keyboard').toBeGreaterThan(7);
  expect(bound.length, 'everything is lit, so the dim state is unregressed').toBeLessThan(cells.length);

  // The pad under the game is the same buttons, drawn short: the two screens cannot disagree.
  const padNames = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>('[data-macro-cells="pad"] [data-macro-row]')].map(
      (cell) => cell.dataset.macroName ?? '',
    ),
  );
  expect(bound.map((cell) => cell.name)).toEqual(padNames);
});
