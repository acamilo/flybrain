/**
 * The macro cells: the pad under the game, and the keyboard on the rail.
 *
 * `docs/design/macros.md` section 14's decided layout split them in two, and the split is what
 * most of this file is about. The strip under the Game Boy screen is **the pad now** — the scene's
 * bound macros, up to fourteen, two columns of seven at the 24 px body floor, packed from the top
 * in the contract's type order, the running one bright and the outcome shown for a beat. The
 * MACROS tab is **the whole keyboard** — all thirty-one types, three columns of eleven, in the
 * fixed order, bound lit and unbound dim, so a cell never moves and a button the scene has taken
 * away is still on screen.
 *
 * What a viewer has to be able to trust, as tests: the strip is the pad and nothing else; the
 * board is every button there is; a name is never truncated and never under the floor; the bright
 * cell is the macro the sim says is running, in both places at once; and the mode on the chip is
 * the mode the cells are drawn from. Plus the one thing an operator has to be able to trust:
 * flipping the mode does not re-lay out the broadcast.
 *
 * Seek times are the fixtures' own. All three are generated offline at exactly 30 Hz and are
 * therefore reproducible from their seed (`tools/record-fixture.mts --offline --mode macros
 * --seed 777`), and the scenes below are what the fake's scene walk lands on with that seed:
 *
 *     macros, t=7.6    an outdoor overworld with `GO ITEM` showing its DONE
 *     macros, t=13.9   the same pad of eight, nothing running and no outcome showing
 *     macros, t=18     the same pad with `GO NPC` running
 *     macros, t=34     a PC: two buttons on the pad, twelve of the strip's cells empty
 *     macros, t=80     an indoor overworld of nine: `GO OUT`, `GO WARP` and `GO SHOP` instead of
 *                      the route, with `GO OBJECTIVE` running
 *     shop,   t=24.6   the mart's five purchase buttons, nothing running
 *     center, t=32.5   a Pokémon Center with `HEAL` on the pad and running
 *
 * `shop` and `center` pin the scene (`packages/feed/src/fake/palette.ts`), because the random walk
 * reaches those two rarely and never for long.
 */
import { MACRO_CHANNELS, MACRO_TYPES, macroChannel } from '@flybrain/feed';
import { expect, test } from '@playwright/test';

import {
  LAYOUT,
  MACRO_BOARD_CELL_HEIGHT,
  MACRO_BOARD_CELL_WIDTH,
  MACRO_BOARD_GAP,
  MACRO_BOARD_PAD,
  MACRO_BOARD_ROWS,
  MACRO_CELL_COUNT,
  MACRO_CELL_GAP,
  MACRO_CELL_HEIGHT,
  MACRO_CELL_ROWS,
  MACRO_CELL_WIDTH,
  MACRO_PALETTE_PAD,
  MACRO_PALETTE_WIDTH,
} from '../../src/lib/geometry';
import { shortMacroName } from '../../src/lib/macro-names';
import { gotoStage } from './stage';

const STRIP = '[data-testid="macro-palette"]';
const BOARD = '[data-testid="macros-tab"]';

/** Every cell one of the two draws, in the order it draws them. */
async function cellsOf(page: import('@playwright/test').Page, root: string) {
  return page.evaluate((selector) => {
    const container = document.querySelector(selector);
    if (container === null) return [];
    return [...container.querySelectorAll<HTMLElement>('[data-macro-row]')].map((cell) => {
      const rect = cell.getBoundingClientRect();
      const name = cell.querySelector<HTMLElement>('.macro-cell__name');
      return {
        row: Number(cell.dataset.macroRow),
        slot: cell.dataset.macroSlot ?? '',
        bound: cell.dataset.bound,
        live: cell.dataset.live ?? '',
        outcome: cell.dataset.outcome ?? '',
        macroName: cell.dataset.macroName ?? '',
        glyph: cell.querySelector('.macro-cell__glyph')?.textContent?.trim() ?? '',
        name: name?.textContent?.trim() ?? '',
        fontPx: name === null ? 0 : Number.parseFloat(getComputedStyle(name).fontSize),
        outcomeText: cell.querySelector('.macro-cell__outcome')?.textContent?.trim() ?? '',
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    });
  }, root);
}

/** Which names the page's own store says are on the pad, in the contract's type order. */
async function boundNames(page: import('@playwright/test').Page) {
  return page.evaluate(
    () =>
      window.__stage
        ?.state()
        .palette.filter((cell) => cell.entry !== null)
        .map((cell) => cell.entry?.name ?? '') ?? [],
  );
}

test('the strip is the scene\'s pad, in two columns of seven at the body floor', async ({ page }) => {
  await gotoStage(page, { fixture: 'macros', t: 13.9, tab: 'senses' });

  await expect(page.locator(STRIP)).toHaveAttribute('data-mode', 'macros');
  const cells = await cellsOf(page, STRIP);

  // The pad, and only the pad: an outdoor overworld binds eight of the thirty-one types, so the
  // strip draws eight cells and leaves six of its fourteen empty.
  const pad = await boundNames(page);
  expect(pad.length).toBe(8);
  expect(cells.map((cell) => cell.macroName)).toEqual(pad);
  expect(cells.map((cell) => cell.name)).toEqual(pad.map(shortMacroName));
  expect(cells.map((cell) => cell.slot)).toEqual(pad.map((_, index) => String(index)));
  // `data-macro-row` stays the *type* index, because that is what the feed carries.
  expect(cells.map((cell) => cell.row)).toEqual(pad.map((name) => MACRO_TYPES.indexOf(name)));

  // At the floor, which is the whole reason the strip is the pad rather than the keyboard.
  expect(cells.map((cell) => cell.fontPx)).toEqual(pad.map(() => 24));

  // Column-major, 2 by 7: cell 7 starts the second column beside cell 0, and cell 1 is under cell
  // 0. Every cell is checked against the arithmetic rather than against its neighbour, because the
  // row pitch is 24.857 and a test that rounded twice would be asserting the rounding.
  const placed = (slot: number) => ({
    x: Math.round(
      LAYOUT.macroPalette.x +
        MACRO_PALETTE_PAD +
        Math.floor(slot / MACRO_CELL_ROWS) * (MACRO_CELL_WIDTH + MACRO_CELL_GAP),
    ),
    y: Math.round(
      LAYOUT.macroPalette.y +
        MACRO_PALETTE_PAD +
        (slot % MACRO_CELL_ROWS) * (MACRO_CELL_HEIGHT + MACRO_CELL_GAP),
    ),
    width: MACRO_CELL_WIDTH,
    height: Math.round(MACRO_CELL_HEIGHT),
  });
  expect(cells.map((cell) => ({ x: cell.x, y: cell.y, width: cell.width, height: cell.height }))).toEqual(
    cells.map((_, slot) => placed(slot)),
  );
  expect(placed(MACRO_CELL_ROWS).y).toBe(placed(0).y);
  expect(placed(MACRO_CELL_ROWS).x).toBe(placed(0).x + MACRO_CELL_WIDTH + MACRO_CELL_GAP);

  // Nothing is running and no outcome is showing at this moment.
  expect(cells.filter((cell) => cell.live === '1')).toEqual([]);
  expect(cells.filter((cell) => cell.outcome !== '')).toEqual([]);
});

test('a long name is shortened by the table, and the full name stays on the element', async ({ page }) => {
  // The strip's cell is 179 px and 24 px VT323 sets nine characters in 87 of them
  // (`src/lib/geometry.ts`), so five of the thirty-one names take the table entries
  // (`src/lib/macro-names.ts`). What must not be shortened is the name the page *matches* on: the
  // paint loop, the ticker and the event log all use the contract's own name.
  await gotoStage(page, { fixture: 'macros', t: 13.9, tab: 'senses' });

  const cells = await cellsOf(page, STRIP);
  const objective = cells.find((cell) => cell.macroName === 'GO OBJECTIVE');
  expect(objective?.name).toBe('GO GOAL');
  const frontier = cells.find((cell) => cell.macroName === 'GO FRONTIER');
  expect(frontier?.name).toBe('GO FRONT');
  // And a name that fits is untouched, even when its first two words look like a verb.
  expect(cells.find((cell) => cell.macroName === 'GO ROUTE')?.name).toBe('GO ROUTE');
});

test('a shallow pad draws fewer cells, and the strip is the same box', async ({ page }) => {
  // A PC is two buttons. The other twelve cells are not drawn at all — no ground, no ink, no em
  // dash standing in for an action that does not exist — and the box the fly sits beside does not
  // move, which is what the geometry is asserted to the pixel for.
  await gotoStage(page, { fixture: 'macros', t: 34, tab: 'senses' });
  const pc = await cellsOf(page, STRIP);
  expect(pc.map((cell) => cell.macroName)).toEqual(['CONFIRM', 'LEAVE']);
  expect(pc.every((cell) => cell.bound === '1')).toBe(true);

  const box = async () =>
    page.evaluate((selector) => {
      const rect = document.querySelector(selector)?.getBoundingClientRect();
      return rect
        ? {
            x: Math.round(rect.x),
            y: Math.round(rect.y),
            w: Math.round(rect.width),
            h: Math.round(rect.height),
          }
        : null;
    }, STRIP);
  const shallow = await box();

  // An indoor overworld binds nine, which fills the first column and two of the second.
  await gotoStage(page, { fixture: 'macros', t: 80, tab: 'senses' });
  const indoors = await cellsOf(page, STRIP);
  expect(indoors.length).toBe(9);
  expect(indoors.length).toBeLessThanOrEqual(MACRO_CELL_COUNT);
  expect(indoors.map((cell) => cell.macroName)).toEqual(await boundNames(page));
  expect(await box()).toEqual(shallow);
});

test('the MACROS tab is the whole keyboard, three columns of eleven, in the contract\'s order', async ({
  page,
}) => {
  await gotoStage(page, { fixture: 'macros', t: 18, tab: 'macros' });

  const cells = await cellsOf(page, BOARD);
  expect(cells.length).toBe(MACRO_TYPES.length);
  expect(cells.map((cell) => cell.row)).toEqual([...MACRO_TYPES.keys()]);
  expect(cells.map((cell) => cell.macroName)).toEqual([...MACRO_TYPES]);
  // The tab has room for the full name (`src/lib/geometry.ts`), so what is drawn is the
  // contract's name, not the verb-dropped form the strip draws (`src/lib/macro-names.ts`).
  expect(cells.map((cell) => cell.name)).toEqual([...MACRO_TYPES]);
  // Here a cell has room for its channel tag, which the strip's 179 px does not.
  expect(cells.map((cell) => cell.glyph)).toEqual([...MACRO_CHANNELS]);
  for (const cell of cells) expect(cell.glyph).toBe(macroChannel(cell.macroName));
  expect(cells.map((cell) => cell.fontPx)).toEqual(MACRO_TYPES.map(() => 24));

  // Column-major, 3 by 11: type 11 starts the second column and type 22 the third, so the last
  // column is nine deep and the board's two spare slots are at the bottom of it.
  const placed = (type: number) => ({
    x: Math.round(
      LAYOUT.tabContent.x +
        MACRO_BOARD_PAD +
        Math.floor(type / MACRO_BOARD_ROWS) * (MACRO_BOARD_CELL_WIDTH + MACRO_BOARD_GAP),
    ),
    y: Math.round(
      LAYOUT.tabContent.y +
        MACRO_BOARD_PAD +
        (type % MACRO_BOARD_ROWS) * (MACRO_BOARD_CELL_HEIGHT + MACRO_BOARD_GAP),
    ),
  });
  expect(cells.map((cell) => ({ x: cell.x, y: cell.y }))).toEqual(cells.map((_, type) => placed(type)));
  expect(placed(MACRO_BOARD_ROWS).y).toBe(placed(0).y);

  // And which of them the scene has bound is the feed's answer, not the page's.
  const pad = await boundNames(page);
  expect(cells.filter((cell) => cell.bound === '1').map((cell) => cell.macroName)).toEqual(pad);
  expect(cells.filter((cell) => cell.bound === '0').length).toBe(MACRO_TYPES.length - pad.length);
});

test('an unbound cell on the board is dim, not gone', async ({ page }) => {
  // The board's whole reason: a button the scene has taken away is still a button the fly has, and
  // it says what it is while it sits there switched off.
  await gotoStage(page, { fixture: 'macros', t: 34, tab: 'macros' });
  const cells = await cellsOf(page, BOARD);
  expect(cells.filter((cell) => cell.bound === '1').map((cell) => cell.macroName)).toEqual([
    'CONFIRM',
    'LEAVE',
  ]);
  expect(cells.every((cell) => cell.name !== '' && cell.glyph !== '')).toBe(true);

  const drawn = await page.evaluate((selector) => {
    const read = (name: string) => {
      const cell = [...document.querySelectorAll<HTMLElement>(`${selector} [data-macro-row]`)].find(
        (element) => element.dataset.macroName === name,
      );
      if (!cell) return null;
      return {
        background: getComputedStyle(cell).backgroundColor,
        ink: getComputedStyle(cell).color,
      };
    };
    return { lit: read('CONFIRM'), dim: read('MOVE 1') };
  }, BOARD);
  expect(drawn.dim?.background).not.toBe('rgba(0, 0, 0, 0)');
  expect(drawn.dim?.background).not.toBe(drawn.lit?.background);
  expect(drawn.dim?.ink).not.toBe('rgba(0, 0, 0, 0)');
  expect(drawn.dim?.ink).not.toBe(drawn.lit?.ink);
});

test('no name is truncated in any scene the fixtures visit', async ({ page }) => {
  // The typographic risk, and it is now two of them: a strip cell is 179 px with 175 inside its
  // padding, and a board cell spends 144 + 6 + 168 of its 325 (`src/lib/geometry.ts`). An ellipsis
  // in either would be a macro name a viewer cannot read, so it is a failure rather than a
  // cosmetic note — and the board is measured whether or not its tab is the one showing, because a
  // hidden pane still has a layout.
  for (const seek of [
    { fixture: 'macros' as const, t: 7.6 },
    { fixture: 'macros' as const, t: 13.9 },
    { fixture: 'macros' as const, t: 18 },
    { fixture: 'macros' as const, t: 34 },
    { fixture: 'macros' as const, t: 80 },
    { fixture: 'shop' as const, t: 24.6 },
    { fixture: 'center' as const, t: 32.5 },
  ]) {
    await gotoStage(page, { ...seek, tab: 'senses' });
    const clipped = await page.evaluate(() =>
      [...document.querySelectorAll('.macro-cell__glyph, .macro-cell__name, .macro-cell__outcome')]
        .filter((element) => element.scrollWidth - element.clientWidth > 1)
        .map((element) => `${element.className}: ${element.textContent ?? ''}`),
    );
    expect(clipped, `${seek.fixture} t=${seek.t}`).toEqual([]);
  }
});

test('the running macro lights its own cell in both places, and only that one', async ({ page }) => {
  await gotoStage(page, { fixture: 'macros', t: 18, tab: 'senses' });

  const running = await page.evaluate(() => {
    const state = window.__stage?.state();
    return state ? { scene: state.scene, mode: state.macroMode } : null;
  });
  expect(running).toEqual({ scene: 'overworld', mode: 'macros' });

  const strip = await cellsOf(page, STRIP);
  const board = await cellsOf(page, BOARD);
  expect(strip.filter((cell) => cell.live === '1').map((cell) => cell.macroName)).toEqual(['GO NPC']);
  expect(board.filter((cell) => cell.live === '1').map((cell) => cell.macroName)).toEqual(['GO NPC']);
  expect(strip.every((cell) => cell.outcome === '')).toBe(true);

  // And indoors, where the bright cell is a different type in a different place.
  await gotoStage(page, { fixture: 'macros', t: 80, tab: 'senses' });
  expect((await cellsOf(page, STRIP)).filter((cell) => cell.live === '1').map((cell) => cell.macroName)).toEqual([
    'GO OBJECTIVE',
  ]);
  expect((await cellsOf(page, BOARD)).filter((cell) => cell.live === '1').map((cell) => cell.macroName)).toEqual([
    'GO OBJECTIVE',
  ]);
});

test('a finished macro shows its outcome for a beat, in the outcome\'s own colour', async ({ page }) => {
  await gotoStage(page, { fixture: 'macros', t: 7.6, tab: 'senses' });

  const cells = await cellsOf(page, STRIP);
  const shown = cells.filter((cell) => cell.outcome !== '');
  expect(shown.length).toBe(1);
  expect(shown[0]?.macroName).toBe('GO ITEM');
  expect(shown[0]?.outcome).toBe('done');
  expect(shown[0]?.outcomeText).toBe('DONE');
  expect(cells.every((cell) => cell.live !== '1')).toBe(true);

  // The outcome takes the name's place rather than squeezing beside it.
  const hidden = await page.evaluate((selector) => {
    const cell = [...document.querySelectorAll<HTMLElement>(`${selector} [data-macro-row]`)].find(
      (element) => (element.dataset.outcome ?? '') !== '',
    );
    const name = cell?.querySelector('.macro-cell__name');
    return name ? getComputedStyle(name).display : null;
  }, STRIP);
  expect(hidden).toBe('none');
});

test('the mart deals purchases, and the centre deals HEAL', async ({ page }) => {
  // Section 13's two screens, each on a fixture that pins it. What the test is for is that the two
  // scenes reach the screen at all — they are the pads a random walk almost never shows — and that
  // what they deal is what the contract says: purchases at the counter, the nurse's HEAL inside a
  // centre, with every other button dim on the board.
  await gotoStage(page, { fixture: 'shop', t: 24.6, tab: 'senses' });
  await expect(page.locator(STRIP)).toHaveAttribute('data-scene', 'shop');
  expect(await boundNames(page)).toEqual(['CONFIRM', 'BUY POTION', 'BUY BALL', 'BUY ANTIDOTE', 'LEAVE']);
  const mart = await cellsOf(page, BOARD);
  // The walks are on the board and off the pad: a mart's own buttons are the only ones bound.
  expect(mart.filter((cell) => cell.macroName.startsWith('GO ')).every((cell) => cell.bound === '0')).toBe(true);
  expect(mart.find((cell) => cell.macroName === 'BUY REPEL')?.bound).toBe('0');
  // `BUY ANTIDOTE` is the name the table shortens, and the mart is where it is drawn.
  expect((await cellsOf(page, STRIP)).map((cell) => cell.name)).toContain('BUY ANTI');

  await gotoStage(page, { fixture: 'center', t: 32.5, tab: 'senses' });
  // A centre is a sub-state of the overworld, on the wire as on the cartridge: the scene says
  // `overworld` and the pad is what says where the fly is standing.
  await expect(page.locator(STRIP)).toHaveAttribute('data-scene', 'overworld');
  const strip = await cellsOf(page, STRIP);
  const board = await cellsOf(page, BOARD);
  // `HEAL` is type 29 of 31 and a centre binds eight, so the strip reaches it — which is what the
  // fourteen cells bought: the six-row strip could not, and the audience saw the fly heal in the
  // ticker without ever seeing the button.
  expect(strip.find((cell) => cell.macroName === 'HEAL')?.live).toBe('1');
  expect(board.find((cell) => cell.macroName === 'HEAL')?.bound).toBe('1');
  expect(board.find((cell) => cell.macroName === 'HEAL')?.live).toBe('1');
  // `GO HEAL` is the walk to the door and the fly is through it; `GO WARP` is a floor change and a
  // centre is one floor. Neither is on the pad, so neither is on the strip, and both are dim on
  // the board.
  expect(strip.map((cell) => cell.macroName)).not.toContain('GO HEAL');
  expect(strip.map((cell) => cell.macroName)).not.toContain('GO WARP');
  expect(board.find((cell) => cell.macroName === 'GO HEAL')?.bound).toBe('0');
  expect(board.find((cell) => cell.macroName === 'GO WARP')?.bound).toBe('0');
});

test('the MODE chip says which mode the cells are drawn from', async ({ page }) => {
  await gotoStage(page, { fixture: 'macros', t: 13.9, tab: 'senses' });
  await expect(page.locator('[data-testid="macro-mode-chip"]')).toHaveText(/macros/i);

  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'senses' });
  await expect(page.locator('[data-testid="macro-mode-chip"]')).toHaveText(/raw/i);
});

test('raw mode keeps the layout: same frame, same baseline, one dim RAW cell', async ({ page }) => {
  // The mode is a config knob on the service (`docs/design/macros.md` section 1), so flipping it
  // must not move the fly or the strip. `steady` was recorded before the macros existed, so it is
  // also the "an older service" case: no macro fields at all, and the same raw layout.
  const boxes = async () =>
    page.evaluate(() => {
      const box = (selector: string) => {
        const rect = document.querySelector(selector)?.getBoundingClientRect();
        return rect
          ? {
              x: Math.round(rect.x),
              y: Math.round(rect.y),
              w: Math.round(rect.width),
              h: Math.round(rect.height),
            }
          : null;
      };
      return { pane: box('[data-testid="fly-pane"]'), palette: box('[data-testid="macro-palette"]') };
    });

  await gotoStage(page, { fixture: 'macros', t: 13.9, tab: 'senses' });
  const withMacros = await boxes();

  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'senses' });
  const raw = await boxes();

  expect(raw).toEqual(withMacros);
  expect(raw.palette?.w).toBe(MACRO_PALETTE_WIDTH);

  await expect(page.locator(STRIP)).toHaveAttribute('data-mode', 'raw');
  const cells = await cellsOf(page, STRIP);
  // Raw mode fills all fourteen slots so the strip's box stays asserted to the pixel
  // (`src/lib/geometry.ts`); one of them says RAW and the rest are empty.
  expect(cells.length).toBe(MACRO_CELL_COUNT);
  expect(cells.filter((cell) => cell.name !== '').map((cell) => cell.name)).toEqual(['RAW']);
  expect(cells.every((cell) => cell.live !== '1')).toBe(true);
  // The board is the whole keyboard whatever the mode is, with nothing bound in raw mode: the
  // buttons exist, the service is simply not letting the fly press them.
  const board = await cellsOf(page, BOARD);
  expect(board.length).toBe(MACRO_TYPES.length);
  expect(board.every((cell) => cell.bound === '0')).toBe(true);
});

test('the SENSES panel gains a MACROS row of the bound channels\' rates', async ({ page }) => {
  // Section 12: the MACROS row beside DRIVE and A/B, one bar per channel the scene has bound,
  // labelled with the tag and driven from `brain.rates[macro_<type>]` by the paint loop. The
  // *bound* ones, unlike the board: a rate bar for a channel that cannot fire is a flat bar.
  await gotoStage(page, { fixture: 'shop', t: 24.6, tab: 'senses' });

  const bars = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>('[data-group="macros"] [data-macro-bar]')].map((row) => ({
      label: row.querySelector('.circuit-bar-row__label')?.textContent?.trim() ?? '',
      role: row.querySelector<HTMLElement>('[data-bar]')?.dataset.bar ?? '',
      scale: row.querySelector<HTMLElement>('[data-bar]')?.style.transform ?? '',
    })),
  );

  // The mart binds five, so the row has five bars, in the strip's own order.
  expect(bars.map((bar) => bar.label)).toEqual([
    'MB·CONF',
    'MB·POTN',
    'MB·PBALL',
    'MB·ANTI',
    'MB·LEAVE',
  ]);
  expect(bars.map((bar) => bar.role)).toEqual([
    'macro_confirm',
    'macro_buy_potion',
    'macro_buy_ball',
    'macro_buy_antidote',
    'macro_leave',
  ]);
  // The paint loop has written each of them, and at least one is off the floor: these are real
  // populations in the dataset, so a flat row would mean the page is reading the wrong key.
  expect(bars.every((bar) => /scaleX/.test(bar.scale))).toBe(true);
  const widths = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>('[data-group="macros"] [data-bar]')].map(
      (fill) => fill.getBoundingClientRect().width,
    ),
  );
  expect(Math.max(...widths)).toBeGreaterThan(0);

  // The six fixed groups are still there, and still six.
  const groups = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>('.circuit-group')].map((group) => group.dataset.group),
  );
  expect(groups).toEqual(['drive', 'press', 'menu', 'dopamine', 'taste', 'legs', 'macros']);

  // Raw mode binds nothing, so the row is not drawn at all.
  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'senses' });
  expect(await page.locator('[data-group="macros"]').count()).toBe(0);
});

test('macro events reach the ticker', async ({ page }) => {
  await gotoStage(page, { fixture: 'macros', t: 18, tab: 'senses' });

  const rowsText = await page.locator('[data-testid="ticker"]').innerText();
  expect(rowsText).toMatch(/\b(start|done|blocked|timeout|refused)\b/);

  const kinds = await page.evaluate(() => window.__stage?.state().ticker.map((item) => item.kind) ?? []);
  expect(kinds, 'no macro row in the ticker').toContain('macro');
});

/**
 * WCAG relative luminance and contrast, over whatever `getComputedStyle` hands back.
 *
 * Every colour on this page resolves to `rgb()` or `color(srgb …)` once the browser has flattened
 * `color-mix`, so both spellings are parsed rather than assumed: `color(srgb …)` carries the
 * channels already normalised, `rgb()` in eighths of a byte.
 */
function contrast(a: string, b: string): number {
  const channels = (value: string): [number, number, number] => {
    const numbers = value.match(/[\d.]+/g)?.map(Number) ?? [];
    if (value.startsWith('color(')) return [numbers[0], numbers[1], numbers[2]];
    return [numbers[0] / 255, numbers[1] / 255, numbers[2] / 255];
  };
  const luminance = (value: string): number => {
    const [r, g, bl] = channels(value).map((c) =>
      c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05);
}

/**
 * The bright cell's name is readable *on the bright cell* (2026-09-16, off the release box: "the
 * lit cell hides the macro name").
 *
 * The running macro is the one cell a viewer most needs to read, and it was the one cell whose
 * type shared its ground's colour. Section 12.1 fixed that by moving the state's colour onto the
 * cell's glyph chip; the strip has no chip since section 14's decided layout, so there the whole
 * cell inverts instead and this is what holds both arrangements to the same floor — the 4.5:1 the
 * legibility work uses, for the running cell and for each outcome state.
 */
test('the bright cell\'s name is legible against the bright cell', async ({ page }) => {
  const paint = async (t: number, root: string) => {
    await gotoStage(page, { fixture: 'macros', t, tab: 'senses' });
    return page.evaluate(
      (selector) =>
        [...document.querySelectorAll<HTMLElement>(`${selector} [data-macro-row]`)]
          .filter((cell) => cell.dataset.live === '1' || (cell.dataset.outcome ?? '') !== '')
          .map((cell) => {
            const shown = cell.querySelector<HTMLElement>(
              (cell.dataset.outcome ?? '') === '' ? '.macro-cell__name' : '.macro-cell__outcome',
            );
            return {
              state: cell.dataset.live === '1' ? 'live' : (cell.dataset.outcome ?? ''),
              text: shown?.textContent?.trim() ?? '',
              background: getComputedStyle(cell).backgroundColor,
              ink: shown ? getComputedStyle(shown).color : '',
            };
          }),
      root,
    );
  };

  // t=18 is `GO NPC` running; t=7.6 is `GO ITEM` showing its DONE. Both places draw both states.
  const seen = [
    ...(await paint(18, STRIP)),
    ...(await paint(7.6, STRIP)),
    ...(await paint(18, BOARD)),
    ...(await paint(7.6, BOARD)),
  ];
  expect(seen.map((cell) => cell.state).sort()).toEqual(['done', 'done', 'live', 'live']);

  for (const cell of seen) {
    expect(cell.text, `the ${cell.state} cell has words in it`).not.toBe('');
    expect(cell.ink, `the ${cell.state} cell's text colour is not its background`).not.toBe(
      cell.background,
    );
    expect(
      contrast(cell.ink, cell.background),
      `${cell.state} cell: "${cell.text}" on its own ground`,
    ).toBeGreaterThan(4.5);
  }
});
