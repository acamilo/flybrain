/**
 * Contact sheets for every moment: `docs/design/animation.md`'s catalogue, one row of frames each.
 *
 * A 20 s screen capture is not something this environment can produce, and a video is the wrong
 * review artifact anyway — the design's own verification section asks about *specific instants*
 * ("assert the DOM state at t+100 ms and at t+9.5 s"). So this shoots six of them per moment, at
 * 0, 100, 300, 600, 1200 and 9500 ms, and stitches each set into one strip a human can read in a
 * pull request:
 *
 *     mockups/motion/<moment>-0.png … -9500.png     the six frames, 1920x1080
 *     mockups/motion/<moment>-sheet.png             the six side by side, labelled
 *
 * The frames come out of the dev harness (`/motion-harness/`) driven through its virtual clock:
 * `window.__motion.shoot(type, tMs)` resets, fires at t=0 and integrates forward in 60 Hz steps, so
 * a frame is the millisecond it says it is rather than "about 300 ms after the click", and the
 * seeded particle RNG plus the reseed on reset means the six frames are six instants of *one* burst.
 * The script proves that rather than assuming it: it shoots the same instant twice and compares the
 * bytes.
 *
 * The harness is a dev-server page (`vite build` never sees it), so unlike `tools/mockup.mts` this
 * runs against `vite dev` rather than `vite preview`.
 *
 * Run it directly rather than through an npm script: the rail rework owns `apps/stage/package.json`
 * for the moment, so this pass adds no script entry to it.
 *
 * ```sh
 * cd apps/stage && npx tsx tools/motion-strip.mts
 * npx tsx tools/motion-strip.mts --only badge      # one moment
 * npx tsx tools/motion-strip.mts --keep            # leave the dev server up afterwards
 * ```
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type Page } from '@playwright/test';

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, '..');
const outDir = resolve(appDir, 'mockups/motion');
const PORT = 5298;

/**
 * The six capture times, in milliseconds after the moment starts.
 *
 * 0 is the trigger frame (nothing has moved yet: the "before"), 100 and 300 catch the arrival —
 * every moment's entry is 200-400 ms — 600 and 1200 cover the particle decay window the addendum
 * specifies, and 9500 is the design's own end-of-hold checkpoint, where a 9 s moment is on its way
 * out and a short one has been gone for eight seconds.
 */
const TIMES = [0, 100, 300, 600, 1200, 9500] as const;

/** Frame width in the stitched strip. Six of these fit a 2880 px sheet at 16:9. */
const STRIP_FRAME_WIDTH = 480;

async function waitForServer(url: string, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Not up yet.
    }
    if (Date.now() > deadline) throw new Error(`dev server never came up at ${url}`);
    await new Promise((done) => setTimeout(done, 250));
  }
}

/** Fire `moment`, integrate to `tMs` on the harness's virtual clock, and shoot the stage. */
async function frame(page: Page, moment: string, tMs: number): Promise<Buffer> {
  await page.evaluate(
    ([type, t]) => {
      const api = window.__motion;
      if (!api) throw new Error('the harness never published window.__motion');
      api.shoot(type as never, t as number);
    },
    [moment, tMs] as const,
  );
  // One real animation frame so the browser commits the styles the harness just wrote.
  await page.evaluate(() => new Promise<void>((done) => requestAnimationFrame(() => done())));
  return page.screenshot({ clip: { x: 0, y: 0, width: 1920, height: 1080 } });
}

/** Stitch one moment's six frames into a labelled strip, by screenshotting a page of images. */
async function sheet(page: Page, moment: string, frames: { tMs: number; png: Buffer }[]): Promise<Buffer> {
  const cells = frames
    .map(
      ({ tMs, png }) => `
        <figure>
          <img src="data:image/png;base64,${png.toString('base64')}" width="${String(STRIP_FRAME_WIDTH)}" />
          <figcaption>${tMs === 0 ? 't = 0 (trigger)' : `t = ${String(tMs)} ms`}</figcaption>
        </figure>`,
    )
    .join('');

  await page.setContent(`<!doctype html>
    <meta charset="utf-8" />
    <style>
      html, body { margin: 0; background: #05060a; color: #c8d0dc;
        font: 13px/1.3 ui-monospace, SFMono-Regular, Menlo, monospace; }
      main { display: inline-block; padding: 14px; }
      h1 { margin: 0 0 10px; font-size: 15px; letter-spacing: 0.16em; text-transform: uppercase; color: #ffb020; }
      .row { display: flex; gap: 6px; }
      figure { margin: 0; }
      img { display: block; border: 1px solid #232b3a; }
      figcaption { padding-top: 5px; color: #6b7688; }
    </style>
    <main>
      <h1>${moment}</h1>
      <div class="row">${cells}</div>
    </main>`);

  // The strip is six 480 px frames wide, which is wider than the 1920 px viewport the frames were
  // shot at — so the viewport grows to the content before the screenshot, or the clip would cut the
  // last two frames off.
  const strip = page.locator('main');
  const first = await strip.boundingBox();
  if (!first) throw new Error('the contact sheet has no box');
  await page.setViewportSize({ width: Math.ceil(first.width) + 8, height: Math.ceil(first.height) + 8 });
  const box = await strip.boundingBox();
  if (!box) throw new Error('the contact sheet has no box after the resize');
  const png = await page.screenshot({ clip: box });
  await page.setViewportSize({ width: 1920, height: 1080 });
  return png;
}

async function main(): Promise<void> {
  const keep = process.argv.includes('--keep');
  const onlyIndex = process.argv.indexOf('--only');
  const only = onlyIndex >= 0 ? process.argv[onlyIndex + 1] : undefined;

  await mkdir(outDir, { recursive: true });

  const dev: ChildProcess = spawn('npx', ['vite', '--port', String(PORT), '--strictPort'], {
    cwd: appDir,
    stdio: 'ignore',
  });
  const baseUrl = `http://127.0.0.1:${PORT}`;

  try {
    await waitForServer(`${baseUrl}/`);

    const browser = await chromium.launch({ args: ['--disable-lcd-text'] });
    const context = await browser.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
    const page = await context.newPage();
    page.on('console', (message) => {
      if (message.type() === 'error') console.warn(`  page error: ${message.text()}`);
    });
    page.on('pageerror', (error) => {
      throw error;
    });

    // `controls=0` hides the control bar and the region labels; `manual=1` hands the clock over.
    await page.goto(`${baseUrl}/motion-harness/?controls=0&manual=1`);
    await page.waitForSelector('html[data-motion-ready="1"]', { state: 'attached', timeout: 60_000 });

    const moments = await page.evaluate(() => [...(window.__motion?.moments ?? [])]);
    const wanted = only ? moments.filter((name) => name === only) : moments;
    if (wanted.length === 0) throw new Error(`no such moment: ${String(only)} (have ${moments.join(', ')})`);

    const written: string[] = [];
    for (const moment of wanted) {
      const frames: { tMs: number; png: Buffer }[] = [];
      for (const tMs of TIMES) {
        const png = await frame(page, moment, tMs);
        frames.push({ tMs, png });
        const path = resolve(outDir, `${moment}-${String(tMs)}.png`);
        await writeFile(path, png);
        written.push(path);
      }

      // The determinism claim, checked rather than asserted: the same instant, shot again.
      const repeat = await frame(page, moment, 300);
      const original = await readFile(resolve(outDir, `${moment}-300.png`));
      if (!repeat.equals(original)) {
        throw new Error(`${moment} is not reproducible: two shots of t=300 ms differ`);
      }

      const sheetPath = resolve(outDir, `${moment}-sheet.png`);
      await writeFile(sheetPath, await sheet(page, moment, frames));
      written.push(sheetPath);
      // The sheet page replaced the harness, so load it again for the next moment.
      await page.goto(`${baseUrl}/motion-harness/?controls=0&manual=1`);
      await page.waitForSelector('html[data-motion-ready="1"]', { state: 'attached', timeout: 60_000 });

      console.log(`  ${moment}: ${String(TIMES.length)} frames + sheet`);
    }

    // The design's budget, measured where it actually matters: a full pool on a real 2D context.
    const perf = await page.evaluate(() => window.__motion?.perf(300) ?? null);
    if (perf) {
      console.log(
        `\n${String(perf.live)}-particle tick+draw on canvas: p50 ${perf.p50Ms.toFixed(3)} ms, ` +
          `p95 ${perf.p95Ms.toFixed(3)} ms, max ${perf.maxMs.toFixed(3)} ms`,
      );
    }

    await browser.close();
    console.log(`\n${String(written.length)} files written to ${outDir}`);
  } finally {
    if (!keep) dev.kill('SIGTERM');
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
