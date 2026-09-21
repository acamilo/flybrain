/**
 * Screenshot tests: every fixture, every tab (design A10.1, rail layout v2).
 *
 * Deterministic because the fixture, the seek time *and the tab* are. A held seek freezes the
 * page's clock, so the ticker, the afterglow, the moment queue, the lerped readouts and the brain
 * map's decay all stop with it; `?tab=` pins the slot, which otherwise cycles every 45 to 60 s and
 * would make every one of these a coin toss between three layouts.
 *
 * Fifteen shots at the broadcast size rather than three, because the tab slot is 420 of the rail's
 * 944 px: a suite that only ever shot whichever tab the clock happened to land on would leave two
 * thirds of the rail's area unregressed. The DESCRIBE shots are also the copy's regression — the
 * tab's words are a one-file edit pending the operator's review (`docs/design/describe-tab.md`), and these
 * are where a change to them shows up as a picture.
 *
 * Plus eight of the macro cells (`docs/design/macros.md` sections 6, 12, 13 and 14) — seven of
 * the strip under the game and one of the MACROS tab — which are the one region the three fixtures
 * above cannot regress: they were recorded before the macros existed, so they only ever show the
 * raw layout. The seek times are the macro fixtures' own, and they are stable because all three
 * are generated offline at exactly 30 Hz (`tools/record-fixture.mts --offline`) rather than
 * recorded over a socket.
 *
 * And two of section 14's own two screens, on the `bigpad` fixture, whose pad is the fake's widest
 * (nine buttons, `--full-pad`): the pad with both of its columns filled, and the MACROS tab with
 * nine cells lit and the rest dim. Between them they are the only shots where a regression in the
 * second column or in the lit-versus-dim split of the keyboard shows up as a picture.
 *
 * ## Why the version chip is hidden
 *
 * Every shot injects `screenshot.css`, which takes the title strip's version string out of the
 * picture at a fixed width. It is `git describe` at build time, so it changed with every commit,
 * and because Silkscreen is proportional a different sha is a different *width* — which moved the
 * two chips right-aligned beside it and invalidated all thirteen committed baselines on every
 * commit. That file has the rest of the reasoning; `structure.spec.ts` still asserts the chip's
 * own text.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { gotoStage, type Fixture, type Tab } from './stage';

/** Injected into every shot: see `screenshot.css`. */
const stylePath = resolve(dirname(fileURLToPath(import.meta.url)), 'screenshot.css');

const SHOTS: { fixture: Fixture; t: number }[] = [
  { fixture: 'cold-open', t: 8 },
  { fixture: 'steady', t: 95 },
  { fixture: 'big-moment', t: 17.5 },
];

const TABS: Tab[] = ['senses', 'connectome', 'ladder', 'describe', 'macros'];

/** The strip's states, and the board's. */
const MACRO_SHOTS: { name: string; fixture: Fixture; t: number; tab: Tab }[] = [
  // Eight macros on the pad outdoors; the same pad with one running and with one showing its
  // result.
  { name: 'overworld', fixture: 'macros', t: 13.9, tab: 'senses' },
  { name: 'running', fixture: 'macros', t: 18, tab: 'senses' },
  { name: 'outcome', fixture: 'macros', t: 7.6, tab: 'senses' },
  // A PC binds two, so the strip draws two cells and leaves twelve empty; indoors the door and the
  // stairs replace the route (`docs/design/macros.md` section 9.1).
  { name: 'pc', fixture: 'macros', t: 34, tab: 'senses' },
  { name: 'indoors', fixture: 'macros', t: 80, tab: 'senses' },
  // The MACROS tab: the whole keyboard, with the outdoor pad's eight lit and `GO NPC` bright.
  { name: 'board', fixture: 'macros', t: 18, tab: 'macros' },
  // Section 13's two, each on the fixture that pins its scene.
  { name: 'shop', fixture: 'shop', t: 24.6, tab: 'senses' },
  { name: 'center', fixture: 'center', t: 32.5, tab: 'senses' },
];

for (const shot of MACRO_SHOTS) {
  test(`macros ${shot.name} at 1080p`, async ({ page }) => {
    await gotoStage(page, { fixture: shot.fixture, t: shot.t, res: '1080', tab: shot.tab });
    await expect(page).toHaveScreenshot(`macros-${shot.name}-1080.png`, {
      clip: { x: 0, y: 0, width: 1920, height: 1080 },
      stylePath,
    });
  });
}

/**
 * Section 14's two screens, both on the wide pad.
 *
 * The tab is pinned away from MACROS for the pad shot and onto it for the keyboard, so each
 * picture is one region's regression and not both at once — and the seek is the same 15 s in both,
 * which is what makes the pair readable side by side in a review.
 */
const SECTION_14_SHOTS: { name: string; tab: Tab }[] = [
  { name: 'pad-strip', tab: 'senses' },
  { name: 'macros-tab', tab: 'macros' },
];

for (const shot of SECTION_14_SHOTS) {
  test(`the wide pad, ${shot.name} at 1080p`, async ({ page }) => {
    await gotoStage(page, { fixture: 'bigpad', t: 15, res: '1080', tab: shot.tab });
    await expect(page).toHaveScreenshot(`${shot.name}-1080.png`, {
      clip: { x: 0, y: 0, width: 1920, height: 1080 },
      stylePath,
    });
  });
}

for (const shot of SHOTS) {
  for (const tab of TABS) {
    test(`${shot.fixture} at 1080p, ${tab} tab`, async ({ page }) => {
      // The authoring size and the broadcast size: no transform on the stage at all.
      await gotoStage(page, { fixture: shot.fixture, t: shot.t, res: '1080', tab });
      await expect(page).toHaveScreenshot(`${shot.fixture}-${tab}-1080.png`, {
        clip: { x: 0, y: 0, width: 1920, height: 1080 },
        stylePath,
      });
    });
  }

  test(`${shot.fixture} at 720p`, async ({ page }) => {
    // `?res=720` is `transform: scale(0.6667)` on the one 1920x1080 stage: the thumbnail mode.
    // One tab is enough here — this shot exists to regress the downscale, not the slot.
    await gotoStage(page, { fixture: shot.fixture, t: shot.t, res: '720', tab: 'senses' });
    await expect(page).toHaveScreenshot(`${shot.fixture}-720.png`, {
      clip: { x: 0, y: 0, width: 1280, height: 720 },
      stylePath,
    });
  });
}
