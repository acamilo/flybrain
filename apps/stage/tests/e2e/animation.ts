import { readFile } from 'node:fs/promises';
import { gunzipSync } from 'node:zlib';
import { GAMEBOY_BUTTON_BITS } from '@flybrain/brain';
import { decodeSnapshot, iterateFlyfeedRecords, readFlyfeedManifest } from '@flybrain/feed';
import type { Page } from '@playwright/test';
import { gotoStage } from './stage';

/** Install before navigation; loading runs normally, then every rAF is test-driven. */
export async function pauseStage(page: Page): Promise<void> {
  await page.clock.install({ time: new Date('2026-01-01T00:00:00Z') });
  await gotoStage(page, { fixture: 'steady', t: 20, play: true });
  await page.clock.pauseAt(new Date('2026-01-01T01:00:00Z'));
  // Rebase the player's source clock after pauseAt's jump; seek preserves autoplay.
  await page.evaluate(() => window.__stage!.seek(20));
  await page.clock.runFor(16);
}

/** Measure isolated presses from the recording, not the decoder's nominal 85 ms hold. */
export async function recordedPresses() {
  const bytes = gunzipSync(await readFile(new URL('../../public/fixtures/steady.flyfeed.gz', import.meta.url)));
  const { bodyOffset } = readFlyfeedManifest(bytes);
  const headers = [...iterateFlyfeedRecords(bytes, bodyOffset)].map((record) => decodeSnapshot(record).header);
  const firstMs = headers[0]!.wallMs;
  return (['a', 'b'] as const).map((button) => {
    const bit = GAMEBOY_BUTTON_BITS[button];
    const edges = headers.filter((header, index) =>
      index > 0 && (header.buttons & bit) !== (headers[index - 1]!.buttons & bit),
    );
    for (let i = 0; i + 2 < edges.length; i++) {
      const down = edges[i]!;
      const up = edges[i + 1]!;
      const next = edges[i + 2]!;
      if ((down.buttons & bit) && down.wallMs - firstMs > 1000 &&
          up.wallMs - down.wallMs < 250 && next.wallMs - up.wallMs > 300) {
        return { button, downMs: down.wallMs - firstMs, upMs: up.wallMs - firstMs };
      }
    }
    throw new Error(`steady fixture has no isolated short ${button} press`);
  });
}
