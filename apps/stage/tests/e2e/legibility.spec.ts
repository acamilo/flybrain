/**
 * The legibility check (design A10.3): a true downscale of the encoded-size frame.
 *
 * Not a re-render at low DPI, which is the easy mistake — a browser re-rendering at a fifth scale
 * lays text out again with hinting and subpixel positioning and looks fine. What a phone viewer
 * actually gets is the 1920x1080 frame, encoded, then scaled down by the video element. So this
 * takes the real screenshot, draws it into a canvas at 397 px (a phone in portrait) and 896 px
 * (desktop with chat open) — the *player* widths, unchanged by the source going 1080p — and
 * measures it there.
 *
 * The measurement is RMS contrast of the luminance in each region marked
 * `data-legible="critical"`: the standard deviation of pixel luminance about its own mean, which
 * is low when detail has been averaged away and high when edges survive. The floor is calibrated
 * from a measured run and set well below it, so this fails on a real regression (a thinner font, a
 * lower-contrast theme token, a hairline border) rather than on noise.
 *
 * Both downscales are written to `tests/e2e/artifacts/` and attached to the report, because the
 * design asks for them as review artifacts for a human to look at.
 */
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { gotoStage } from './stage';

const here = dirname(fileURLToPath(import.meta.url));
const artifactDir = resolve(here, 'artifacts');

/** The source frame this measures: the whole broadcast canvas. */
const SOURCE_WIDTH = 1920;
const SOURCE_HEIGHT = 1080;

/** Twitch player widths from the research: a phone in portrait, and desktop with chat open. */
const SCALES = [
  { name: 'phone-397', width: 397 },
  { name: 'desktop-896', width: 896 },
] as const;

/**
 * RMS contrast floor at the phone width.
 *
 * Measured on this build at 397 px, the five regions layout v2 marks critical: game 0.195, rung
 * 0.177 to 0.179, the slot 0.097 to 0.125 by tab, events 0.126, chat 0.104. A region of flat
 * colour would score near zero, which is what makes this a real check rather than a formality, and
 * the floor sits well below every one of those — 2.8x under the worst — so it fails on a real
 * regression (a thinner font, a lower-contrast theme token, a hairline border) rather than on
 * noise.
 *
 * The slot's worst reading is the tab with the least ink in it: LADDER at 0.098 and DESCRIBE at
 * 0.097, the latter being one label, a paragraph and a row of cells in a 1004x368 box. Landing
 * where the 38-rung ladder lands is the reading the DESCRIBE pane had to make: it is the one pane
 * with no raster and no bars to carry its contrast, so its ink is a label in the accent colour and
 * a 560 px measure that keeps the paragraph together instead of thinning it across the box.
 *
 * Two notes on where those numbers came from.
 *
 * The retina raster is the one that needed watching through the 1080p move: its dots are the
 * finest detail on the page, and at the old 2 px they measured 0.037 here once the canvas grew.
 * `src/paint/retina.ts` scales the dot with the canvas, which is what put it back above 0.1.
 *
 * And the Game Boy pass *raised* every type region rather than risking them. The body face is
 * Silkscreen, a boxier and heavier pixel face than the Inter it replaced and than VT323 beside
 * it, so the panels carrying text went up by a lot: chat and events were 0.078 under layout v1's
 * region set and are 0.137 now. The 0.31 downscale was the check the pixel faces were most likely
 * to fail, and it is the one they pass most comfortably — nothing on the page needed its size
 * raised to clear this floor.
 */
const RMS_FLOOR_PHONE = 0.035;

/**
 * Every tab, because the slot is 420 of the rail's 944 px and each pane is its own type problem.
 *
 * DESCRIBE is the type problem: a Silkscreen label and a paragraph of VT323 at the 24 px floor, with
 * no raster and no bars to carry the region's contrast for it. MACROS is the other one: thirty-one
 * cells of it, two thirds of them dim, and nothing else in the pane.
 */
const TABS = ['senses', 'connectome', 'ladder', 'describe', 'macros'] as const;

/**
 * The macro strip is a sixth critical region, and it only exists in macros mode.
 *
 * The three committed fixtures above predate it, so they draw the raw layout — one dim word in the
 * strip, which claims no readable region and is not measured. This run is the strip itself: the
 * pad's macros in two columns of seven, 24 px type in 22.857 px cells, which is the tightest type
 * on the page after the ladder's rungs.
 */
const MACRO_FIXTURE = { fixture: 'macros', t: 18 } as const;

interface Region {
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

const RUNS = [
  ...TABS.map((tab) => ({ name: `${tab} tab`, fixture: 'steady' as const, t: 95, tab, regions: 4 })),
  { name: 'macros mode', fixture: MACRO_FIXTURE.fixture, t: MACRO_FIXTURE.t, tab: 'senses' as const, regions: 5 },
];

for (const run of RUNS) {
  test(`the critical regions survive the phone and desktop-with-chat downscales, ${run.name}`, async ({
    page,
  }, testInfo) => {
    const tab = run.tab;
    await gotoStage(page, { fixture: run.fixture, t: run.t, tab });

    const regions: Region[] = await page.evaluate(() =>
    [...document.querySelectorAll('[data-legible="critical"]')].map((element, index) => {
      const rect = element.getBoundingClientRect();
      const title = element.querySelector('.panel-title')?.textContent?.trim();
      return {
        name: title || element.getAttribute('data-testid') || `region-${index}`,
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    }),
  );
    // Five: the game, the progress cluster, the tab slot, the events ticker and chat — six in
    // macros mode, where the strip's rows are readable text too. The slot counts as one region
    // whichever pane is up, which is the point of pinning the tab.
    expect(regions.length, 'nothing is marked data-legible="critical"').toBeGreaterThan(run.regions);

    const shot = await page.screenshot({ clip: { x: 0, y: 0, width: SOURCE_WIDTH, height: SOURCE_HEIGHT } });
    const dataUrl = `data:image/png;base64,${shot.toString('base64')}`;

    await mkdir(artifactDir, { recursive: true });

    for (const scale of SCALES) {
    // A second page holding only the image, downscaled by the browser the way a video element
    // would, then measured in the canvas rather than re-rendered.
    const viewer = await page.context().newPage();
    await viewer.setContent('<body style="margin:0;background:#000"></body>');

    const result = await viewer.evaluate(
      async ({ url, targetWidth, regionList, sourceWidth }) => {
        const image = new Image();
        image.src = url;
        await image.decode();

        const targetHeight = Math.round((image.height * targetWidth) / image.width);
        const canvas = document.createElement('canvas');
        canvas.width = targetWidth;
        canvas.height = targetHeight;
        const ctx = canvas.getContext('2d');
        if (!ctx) throw new Error('no 2d context in the viewer page');
        ctx.imageSmoothingEnabled = true;
        ctx.imageSmoothingQuality = 'high';
        ctx.drawImage(image, 0, 0, targetWidth, targetHeight);

        const factor = targetWidth / sourceWidth;
        const measured = regionList.map((region) => {
          const x = Math.max(0, Math.floor(region.x * factor));
          const y = Math.max(0, Math.floor(region.y * factor));
          const width = Math.max(1, Math.min(targetWidth - x, Math.round(region.width * factor)));
          const height = Math.max(1, Math.min(targetHeight - y, Math.round(region.height * factor)));

          const { data } = ctx.getImageData(x, y, width, height);
          let sum = 0;
          let sumSquares = 0;
          const count = data.length / 4;
          for (let i = 0; i < data.length; i += 4) {
            // Rec. 709 luminance, 0..1, the same weights the retina kernel uses.
            const luminance =
              ((data[i] as number) * 0.2126 + (data[i + 1] as number) * 0.7152 + (data[i + 2] as number) * 0.0722) /
              255;
            sum += luminance;
            sumSquares += luminance * luminance;
          }
          const mean = sum / count;
          const rms = Math.sqrt(Math.max(0, sumSquares / count - mean * mean));
          return { name: region.name, rms, mean, pixels: count };
        });

        return { png: canvas.toDataURL('image/png'), width: targetWidth, height: targetHeight, measured };
      },
      { url: dataUrl, targetWidth: scale.width, regionList: regions, sourceWidth: SOURCE_WIDTH },
    );

    await viewer.close();

    const path = resolve(artifactDir, `${run.fixture}-${tab}-downscale-${scale.name}.png`);
    await writeFile(path, Buffer.from(result.png.split(',')[1] as string, 'base64'));
    await testInfo.attach(`${run.name} downscale ${scale.name} (${result.width}x${result.height})`, {
      path,
      contentType: 'image/png',
    });

    // eslint-disable-next-line no-console
    console.log(
      `  ${run.name} ${scale.name} (${result.width}x${result.height}): ` +
        result.measured.map((region) => `${region.name}=${region.rms.toFixed(3)}`).join(' '),
    );

    if (scale.name === 'phone-397') {
      for (const region of result.measured) {
        expect(region.pixels, `${region.name} measured no pixels`).toBeGreaterThan(100);
        expect(region.rms, `${region.name} RMS contrast at the phone width`).toBeGreaterThan(RMS_FLOOR_PHONE);
      }
    }
    }
  });
}
