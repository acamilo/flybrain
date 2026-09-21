/**
 * Render the sign-off mockups.
 *
 * Twelve PNGs at 1920x1080, DPR 1, from the fixture feed, plus the two fly review crops:
 *
 *     steady-t1-senses.png       the rail's three tabs, on the steady fixture
 *     steady-t1-connectome.png
 *     steady-t1-ladder.png
 *     describe.png               the DESCRIBE tab's one card, for the copy review
 *     big-moment-t1.png          the big-moment fixture 2.5 s into its milestone
 *     chat-wrap.png              a 200-character chat line wrapped, with the name in the label face
 *     moment-milestone.png       each moment, 300 ms in: the caption band entering, the sparks
 *     moment-badge.png           in the air, the rail flash up, the wipe half across
 *     moment-sugar.png
 *     moment-rollback.png
 *     fly-webgl.png              the fly strip alone at 2x, one per renderer
 *     fly-paper.png
 *     macros-overworld.png       the macro strip's seven scene states plus section 14's
 *     macros-running.png         two screens on the wide pad: eight macros on the pad
 *     macros-outcome.png         outdoors, one running, one showing its result, a PC whose two
 *     macros-pc.png              buttons leave twelve cells empty, the indoor overworld, and
 *     macros-indoors.png         section 13's mart and Pokémon Center from their own pinned
 *     macros-shop.png            fixtures, then the whole keyboard on its own tab and the
 *     macros-center.png          wide pad with both of its columns filled
 *     macros-tab.png
 *     pad-strip.png
 *
 * T1 Instrument is the chosen theme (`docs/stream-mvp-plan.md`, decisions 2026-09-15 evening), so
 * the theme sweep the first gate needed is gone: what these images are for now is the *layout* and
 * the *motion*, which is why there are three tab shots and four moment shots instead of six themes.
 *
 * The moment shots are taken by firing the trigger by hand through `window.__stage.fire` and
 * shooting 300 ms later. Waiting for a fixture to contain one of each would make the images a
 * function of the recording, and 300 ms is the middle of every arrival in the catalogue (240 to
 * 400 ms), which is the frame a reviewer needs to see.
 *
 * ```sh
 * npm run mockups                     # build, serve, shoot every one of them
 * npm run mockups -- --only macros    # only the shots whose name contains "macros"
 * npm run mockups -- --keep           # leave the preview server running
 * ```
 *
 * `--only` exists because these are committed PNGs: re-shooting all of them to review one panel
 * rewrites sixteen binaries, and the moment shots are particle fields, so two runs of the same
 * commit are never byte-identical.
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type Page } from '@playwright/test';

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, '..');
const outDir = resolve(appDir, 'mockups');

/** 4500+, per the run instructions; `MOCKUP_PORT` overrides for a busy machine. */
const PORT = Number(process.env.MOCKUP_PORT ?? 4501);

/** The frame: the whole broadcast canvas. */
const FRAME = { x: 0, y: 0, width: 1920, height: 1080 } as const;

/** How far into a moment the shot is taken. The middle of every arrival in the catalogue. */
const MOMENT_MS = 300;

type Tab = 'senses' | 'connectome' | 'ladder' | 'describe' | 'macros';

/**
 * The held-frame shots.
 *
 * `steady` at t=95 is two thirds through a 120 s recording, with the ticker populated and chat
 * filled. `big-moment` at t=17.5 is 2.5 s after that fixture's milestone, so the caption band is
 * up and the LADDER tab has taken focus — the state the owner needs to see.
 */
/**
 * The chat ring the wrapping shot needs, longest line last.
 *
 * None of the committed fixtures holds a line near the sanitizer's 200-character limit — they were
 * recorded off a live bridge, where nobody types one — so `chat-wrap.png` replaces the ring by
 * hand through `window.__stage.chat`, the same way the moment shots fire a trigger by hand. The
 * last line is exactly 200 characters, which is the case the operator is reviewing.
 */
const CHAT_WRAP: { by: string; text: string; bot?: boolean }[] = [
  { by: 'moth_lord', text: 'he has been in there two hours' },
  { by: 'dendrite', text: 'go left! the ledge is right there' },
  { by: 'flybridgebot', text: 'sugar goes to the fly, not to the fly', bot: true },
  {
    by: 'viridian_city_enjoyer',
    text: 'the fly walked into the same ledge for twenty minutes and i have never been prouder of an insect in my whole life, somebody please teach it that the b button exists and that a ledge is not a door yet!',
  },
];

const STILLS: { name: string; fixture: string; t: number; tab?: Tab; chat?: typeof CHAT_WRAP }[] = [
  { name: 'steady-t1-senses', fixture: 'steady', t: 95, tab: 'senses' },
  { name: 'steady-t1-connectome', fixture: 'steady', t: 95, tab: 'connectome' },
  { name: 'steady-t1-ladder', fixture: 'steady', t: 95, tab: 'ladder' },
  // The DESCRIBE tab, for the copy review. One card and no cycle since 2026-09-17, so the shot is
  // the whole of the tab whatever the clock is doing; it is `describe.png` in the review folder and the same
  // frame on every machine.
  { name: 'describe', fixture: 'steady', t: 95, tab: 'describe' },
  { name: 'big-moment-t1', fixture: 'big-moment', t: 17.5 },
  // The chat panel with a wrapped line in it (the operator, 2026-09-17: "make chat messages wrap.
  // make name pop more"), which is the whole of what this shot is for: the name in the label face
  // and the accent, a 200-character message on three rows under it, the bot line still green, and
  // the oldest lines dropped off the top rather than cut in half.
  { name: 'chat-wrap', fixture: 'steady', t: 95, tab: 'senses', chat: CHAT_WRAP },
  // The macro strip and the MACROS tab (`docs/design/macros.md` sections 6, 12, 13 and 14's
  // decided layout). All three offline fixtures are generated at exactly 30 Hz, so these ten
  // seconds are the same ten states on every machine: eight macros on the pad outdoors, GO NPC
  // running, GO ITEM showing its DONE, a PC whose two buttons leave twelve of the strip's cells
  // empty, the indoor overworld with the door and the passages, the mart's purchases, a Pokémon
  // Center with HEAL running, the whole keyboard on its own tab lit where the outdoor pad binds
  // it, and the wide pad with both of its columns filled on the `bigpad` fixture. The tab is
  // pinned for the same reason every other still pins it.
  { name: 'macros-overworld', fixture: 'macros', t: 13.9, tab: 'senses' },
  { name: 'macros-running', fixture: 'macros', t: 18, tab: 'senses' },
  { name: 'macros-outcome', fixture: 'macros', t: 7.6, tab: 'senses' },
  { name: 'macros-pc', fixture: 'macros', t: 34, tab: 'senses' },
  { name: 'macros-indoors', fixture: 'macros', t: 80, tab: 'senses' },
  { name: 'macros-shop', fixture: 'shop', t: 24.6, tab: 'senses' },
  { name: 'macros-center', fixture: 'center', t: 32.5, tab: 'senses' },
  { name: 'macros-tab', fixture: 'macros', t: 18, tab: 'macros' },
  // Section 14's wide pad on the `bigpad` fixture (the fake's widest pad, nine buttons,
  // `--full-pad`, `packages/feed/src/fake/palette.ts`). This is the only shot where the strip's
  // second column has anything in it; the matching MACROS tab shot is below. They are the pair
  // the operator reviews the section 14 layout from.
  { name: 'pad-strip', fixture: 'bigpad', t: 15, tab: 'senses' },
];

/**
 * The moment shots.
 *
 * `tab` is left unpinned for the three moments that take a tab focus, because the focus is half
 * of what the shot is showing; sugar takes no focus, so its tab is pinned to keep the frame
 * deterministic.
 */
const MOMENTS: { name: string; type: string; label: string; detail: string; tab?: Tab }[] = [
  { name: 'moment-milestone', type: 'milestone', label: 'Reached: Pallet Town', detail: 'rung 3' },
  { name: 'moment-badge', type: 'badge', label: 'BOULDER BADGE', detail: '1 of 8' },
  { name: 'moment-sugar', type: 'sugar', label: 'SUGAR', detail: 'ada', tab: 'senses' },
  { name: 'moment-rollback', type: 'rollback', label: 'Rolled back to the archived frame', detail: 'try 3' },
];

function run(command: string, args: string[]): Promise<void> {
  return new Promise((done, fail) => {
    const child = spawn(command, args, { cwd: appDir, stdio: 'inherit' });
    child.on('exit', (code) => (code === 0 ? done() : fail(new Error(`${command} exited ${String(code)}`))));
    child.on('error', fail);
  });
}

async function waitForServer(url: string, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Not up yet.
    }
    if (Date.now() > deadline) throw new Error(`preview server never came up at ${url}`);
    await new Promise((done) => setTimeout(done, 250));
  }
}

function url(base: string, params: Record<string, string | number | undefined>): string {
  const query = new URLSearchParams({ mode: 'player', theme: 't1', res: '1080' });
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) query.set(key, String(value));
  }
  return `${base}/?${query.toString()}`;
}

/**
 * Wait for the page to be genuinely ready: fonts, base bitmap, a feed, and the load transitions.
 *
 * The feed is the part `data-ready` does not cover, and it is the one that matters most here.
 * `data-ready` means the fonts loaded and the brain map's base bitmap arrived; the fixture is
 * still arriving behind it, and measured on a cold browser context `steady` (23.3 MB) reports
 * ready at 1.7 s and its first accepted snapshot at 4.2 s. A fixed hold therefore photographed
 * the page's own initial zeroes — "0.0 Hz", no rung name, no chat panel — whenever the machine
 * was busy, and these PNGs are what a human signs the layout off from.
 */
async function settle(page: Page, holdMs = 900): Promise<void> {
  await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });
  await page.waitForFunction(() => (window.__stage?.health().accepted ?? 0) > 0, undefined, {
    timeout: 60_000,
  });
  await page.waitForTimeout(holdMs);
}

async function main(): Promise<void> {
  const keep = process.argv.includes('--keep');
  const onlyAt = process.argv.indexOf('--only');
  const only = onlyAt === -1 ? null : (process.argv[onlyAt + 1] ?? null);
  if (onlyAt !== -1 && only === null) throw new Error('--only needs a substring of a shot name');
  const wanted = (name: string): boolean => only === null || name.includes(only);

  console.log('building…');
  await run('npm', ['run', 'build']);

  await mkdir(outDir, { recursive: true });

  const preview: ChildProcess = spawn(
    'npx',
    ['vite', 'preview', '--host', '127.0.0.1', '--port', String(PORT), '--strictPort'],
    {
    cwd: appDir,
    stdio: 'ignore',
  });
  const baseUrl = `http://127.0.0.1:${PORT}`;

  try {
    await waitForServer(`${baseUrl}/`);

    const browser = await chromium.launch({
      args: ['--autoplay-policy=no-user-gesture-required', '--disable-lcd-text'],
    });
    const context = await browser.newContext({
      viewport: { width: 1920, height: 1080 },
      deviceScaleFactor: 1,
    });
    const page = await context.newPage();

    page.on('console', (message) => {
      if (message.type() === 'error') console.warn(`  page error: ${message.text()}`);
    });

    const written: string[] = [];

    for (const shot of STILLS) {
      if (!wanted(shot.name)) continue;
      await page.goto(url(baseUrl, { fixture: shot.fixture, t: shot.t, tab: shot.tab }));
      await settle(page);
      if (shot.chat) {
        const accepted = await page.evaluate((lines) => window.__stage?.chat(lines) ?? 0, shot.chat);
        if (accepted !== shot.chat.length) throw new Error(`the panel took ${accepted} of ${shot.chat.length} lines`);
        // Every injected line is a new React key, so all of them slide in (`src/theme/motion.css`,
        // 240 ms, from `opacity: 0`): a shot taken now is a shot of an empty panel.
        await page.waitForTimeout(400);
      }
      const path = resolve(outDir, `${shot.name}.png`);
      await page.screenshot({ path, clip: FRAME });
      written.push(path);
      console.log(`  ${path}`);
    }

    for (const moment of MOMENTS) {
      if (!wanted(moment.name)) continue;
      // `play=1`: the moment queue runs on the page's data clock, which a held seek freezes on
      // purpose, so a moment fired against a frozen fixture would never leave its first frame.
      await page.goto(url(baseUrl, { fixture: 'steady', t: 40, play: 1, tab: moment.tab }));
      await settle(page, 1500);
      await page.evaluate(
        ([type, label, detail]) => window.__stage?.fire(type as never, label, detail),
        [moment.type, moment.label, moment.detail],
      );
      await page.waitForTimeout(MOMENT_MS);
      const path = resolve(outDir, `${moment.name}.png`);
      await page.screenshot({ path, clip: FRAME });
      written.push(path);
      const state = await page.evaluate(() => window.__stage?.motion() ?? null);
      console.log(`  ${path}  (${String(state?.moment)} ${String(state?.momentPhase)}, ${String(state?.particles)} particles, tab ${String(state?.tab)})`);
    }

    // Two extra review shots: the fly strip alone, at 2x, once per renderer. The strip is 800x220
    // in a 1920x1080 frame, which is too small to judge an animal in; these are what a human looks
    // at to decide whether it reads.
    const zoom = await browser.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 2 });
    const zoomPage = await zoom.newPage();
    for (const mode of ['webgl', 'paper'] as const) {
      if (!wanted(`fly-${mode}`)) continue;
      await zoomPage.goto(url(baseUrl, { fixture: 'steady', t: 95, fly: mode, tab: 'senses' }));
      await settle(zoomPage);

      const strip = zoomPage.locator('[data-testid="fly-strip"]');
      const declared = await strip.getAttribute('data-fly');
      if (declared !== mode) throw new Error(`the page says fly=${String(declared)}, not ${mode}`);
      const box = await strip.boundingBox();
      if (!box) throw new Error('the fly strip has no box');

      const path = resolve(outDir, `fly-${mode}.png`);
      await zoomPage.screenshot({ path, clip: box });
      written.push(path);
      console.log(`  ${path}`);
    }
    await zoom.close();

    const metrics = await page.evaluate(() => window.__stage?.metrics() ?? null);
    const audio = await page.evaluate(() => window.__stage?.audio() ?? null);
    console.log('\npaint stages on the last page (ms):');
    if (metrics) {
      for (const [name, stats] of Object.entries(metrics.stages)) {
        console.log(
          `  ${name.padEnd(10)} p50 ${stats.p50Ms.toFixed(2)}  p95 ${stats.p95Ms.toFixed(2)}  max ${stats.maxMs.toFixed(2)}  n=${stats.count}`,
        );
      }
      console.log(`  frames ${metrics.frames}, long frames ${metrics.longFrames}`);
    }
    if (audio) console.log(`audio: context=${audio.context} worklet=${audio.worklet} sfx=${audio.sfxLoaded}/8`);

    await browser.close();

    console.log(`\n${written.length} mockups written to ${outDir}`);
  } finally {
    if (!keep) preview.kill('SIGTERM');
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
