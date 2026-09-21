/**
 * The CONNECTOME tab against a *live* feed at the *live* spike rate.
 *
 * Until 2026-09-16 the fake simulator defaulted to 2,000 spikes a snapshot, so every e2e test that
 * replays a committed `.flyfeed` fixture exercised only that count. The real flysim sends around
 * 30,000 — the live page reports `spikeCount` near 30,000 all day — and 2,000 versus 30,000 is not
 * a matter of degree for this panel: at 2,000 about 25 accumulator cells clear the sprite threshold
 * and the 256-sprite cap never binds, while at 30,000 nearly 10,000 cells clear it and the cap
 * binds on every frame. The bug that shipped — all 256 sprites drawn as a bar across the top of the
 * brain — lived entirely in the saturated path, which is why the whole suite stayed green over it.
 * The fake simulator's default (and the committed fixtures) now match the live rate too, so this
 * test's own pinned rate is a belt-and-suspenders regression guard rather than the only place that
 * exercises the saturated path.
 *
 * So this test brings its own feed: `FakeFlysim` at the live rate, on an ephemeral port, over the
 * same protocol `fake/server.ts` speaks (`docs/feed-protocol.md`). It is structural, not a
 * screenshot: it samples the canvas's own pixels the way an operator samples them over CDP, and
 * asserts what a viewer would notice — the brain is not a bar, and the edges are empty.
 */
import { createServer, type Server } from 'node:http';
import { expect, test } from '@playwright/test';
import { WebSocket, WebSocketServer, type RawData } from 'ws';

import { encodeSnapshot } from '@flybrain/feed';
import { FakeFlysim } from '@flybrain/feed/fake';
import type { AttachmentKind, ClientHello } from '@flybrain/feed';

import { MAP_GRID_HEIGHT, MAP_GRID_WIDTH, MAP_HERO_HEIGHT, MAP_HERO_WIDTH } from '../../src/lib/geometry';
import { MAX_SPRITES } from '../../src/paint/brainmap';

/**
 * What the real flysim sends, measured off the live page — also the fake simulator's own default
 * since 2026-09-16, but pinned here explicitly so this test does not depend on that default.
 */
const LIVE_SPIKES_PER_TICK = 30_000;
const TICK_MS = 1000 / 30;

interface Feed {
  port: number;
  close: () => Promise<void>;
}

/** A minimal feed server: the fake simulator at the live spike rate, nothing else. */
async function startLiveFeed(): Promise<Feed> {
  const simulator = new FakeFlysim({ scenario: 'running', seed: 20_260_916, spikesPerTick: LIVE_SPIKES_PER_TICK });
  const http: Server = createServer((_req, res) => {
    res.writeHead(404).end('not found');
  });
  const wss = new WebSocketServer({ server: http, path: '/feed' });
  const wants = new Map<WebSocket, Set<AttachmentKind>>();

  wss.on('connection', (socket) => {
    const onMessage = (data: RawData): void => {
      const hello = JSON.parse(data.toString()) as ClientHello;
      wants.set(socket, new Set(hello.wants ?? []));
      socket.off('message', onMessage);
    };
    socket.on('message', onMessage);
    socket.on('close', () => wants.delete(socket));
    socket.on('error', () => wants.delete(socket));
  });

  let last = Date.now();
  const timer = setInterval(() => {
    const now = Date.now();
    const snapshot = simulator.tick(Math.max(1, now - last));
    last = now;
    for (const [socket, kinds] of wants) {
      if (socket.readyState !== WebSocket.OPEN) continue;
      const attachments: Partial<Record<AttachmentKind, Uint8Array>> = {};
      const sent: AttachmentKind[] = [];
      for (const kind of snapshot.header.attachments) {
        if (!kinds.has(kind)) continue;
        sent.push(kind);
        attachments[kind] = snapshot.attachments[kind];
      }
      socket.send(encodeSnapshot({ ...snapshot.header, attachments: sent }, attachments));
    }
  }, TICK_MS);

  await new Promise<void>((resolve) => http.listen(0, '127.0.0.1', () => resolve()));
  const { port } = http.address() as { port: number };

  return {
    port,
    close: async () => {
      clearInterval(timer);
      for (const socket of wants.keys()) socket.terminate();
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await new Promise<void>((resolve) => http.close(() => resolve()));
    },
  };
}

/**
 * Bright pixels per canvas row, and where they sit.
 *
 * Brightness rather than total luminance, because the static base raster dominates every row sum
 * and the question is where the *glow* went: the sprite pass and the hottest accumulator cells are
 * what clear this, the faint point cloud underneath is what does not.
 */
async function sampleRows(page: import('@playwright/test').Page) {
  return page.evaluate(() => {
    const canvas = document.querySelector('[data-testid="brain-map"]') as HTMLCanvasElement;
    const ctx = canvas.getContext('2d') as CanvasRenderingContext2D;
    const { width, height } = canvas;
    const pixels = ctx.getImageData(0, 0, width, height).data;
    const bright: number[] = [];
    for (let y = 0; y < height; y++) {
      let count = 0;
      for (let x = 0; x < width; x++) {
        const p = (y * width + x) * 4;
        if ((pixels[p] as number) + (pixels[p + 1] as number) + (pixels[p + 2] as number) > 360) count += 1;
      }
      bright.push(count);
    }
    return { width, height, bright };
  });
}

test.describe('connectome at the live spike rate', () => {
  let feed: Feed;

  test.beforeAll(async () => {
    feed = await startLiveFeed();
  });

  test.afterAll(async () => {
    await feed?.close();
  });

  test('the map fills the pane, saturates the sprite cap, and does not band', async ({ page }) => {
    const params = new URLSearchParams({
      mode: 'live',
      feed: `ws://127.0.0.1:${feed.port}/feed`,
      theme: 't1',
      tab: 'connectome',
      fly: 'off',
      audio: '0',
      chat: 'off',
    });
    await page.goto(`/?${params.toString()}`);
    await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });
    await page.waitForFunction(() => (window.__stage?.health().accepted ?? 0) > 0, undefined, { timeout: 60_000 });
    // The accumulator needs a few time constants of spikes before the cap binds. Wait for it to be
    // binding by a wide margin — four times the budget — so the sample is taken well inside the
    // saturated regime the live page lives in rather than at the moment it first crosses into it.
    await page.waitForFunction(
      (floor) => (window.__stage?.brainmap().cellsOverThreshold ?? 0) > floor,
      4 * MAX_SPRITES,
      { timeout: 30_000 },
    );

    const stats = await page.evaluate(() => window.__stage?.brainmap() ?? null);
    expect(stats).not.toBeNull();
    if (!stats) return;

    // 1. The pane, the backing store and the fit the LUT was built for are one size.
    expect(stats.canvas).toMatchObject({ width: MAP_HERO_WIDTH, height: MAP_HERO_HEIGHT, dpr: 1 });
    expect(stats.canvas.cssWidth).toBe(MAP_HERO_WIDTH);
    expect(stats.canvas.cssHeight).toBe(MAP_HERO_HEIGHT);
    expect(stats.grid).toMatchObject({ width: MAP_GRID_WIDTH, height: MAP_GRID_HEIGHT });
    expect(stats.fit).not.toBeNull();
    expect(stats.fit).toMatchObject({
      width: MAP_HERO_WIDTH,
      height: MAP_HERO_HEIGHT,
      gridWidth: MAP_GRID_WIDTH,
      gridHeight: MAP_GRID_HEIGHT,
      // The fit comes from these neurons' own extent, so not one of them may fall outside it.
      outOfBounds: 0,
    });

    // 2. The cap is genuinely binding — otherwise the rest of this test proves nothing.
    expect(stats.cellsOverThreshold).toBeGreaterThan(MAX_SPRITES);
    expect(stats.sprites).toBe(MAX_SPRITES);

    // 3. The sprites are spread over the map rather than pooled in a band of rows near the top.
    // Measured both ways round on this same page and feed: scan-order selection put all 256 into
    // 21 to 30 of the 184 grid rows (span 0.11 to 0.17), brightest-first spreads them over about
    // 105 (span 0.57 to 0.60). The bound sits between the two, nearer the bug.
    expect(stats.hotRows).not.toBeNull();
    expect(stats.hotRows?.span ?? 0, 'the sprites are a band of rows, not the brain').toBeGreaterThan(0.4);

    // 4. And the same thing again from the pixels, which is what the operator is actually looking at.
    const { height, bright } = await sampleRows(page);
    expect(height).toBe(MAP_HERO_HEIGHT);

    // The 0.97 fit margin keeps the extreme neurons just inside the edges, so the outermost rows
    // hold nothing at all. A clamp is what used to put a line of glow here.
    for (const row of [0, 1, 2, height - 3, height - 2, height - 1]) {
      expect(bright[row], `edge row ${row} must be empty`).toBe(0);
    }

    const active = bright.filter((count) => count > 0);
    expect(active.length).toBeGreaterThan(height / 3);
    const total = active.reduce((sum, count) => sum + count, 0);
    const peakRatio = Math.max(...bright) / (total / active.length);
    test.info().annotations.push({
      type: 'connectome',
      description:
        `active rows ${active.length}, peak/mean ${peakRatio.toFixed(2)}, ` +
        `hot rows span ${(stats.hotRows?.span ?? 0).toFixed(3)}, cells over threshold ${stats.cellsOverThreshold}`,
    });

    // A bar of 256 sprites makes one row several times the brain's own mean row. Measured both
    // ways round on this page and feed: 3.2 to 3.5 with scan-order selection, 2.0 to 2.2 with
    // brightest-first.
    expect(peakRatio, 'the brightest row is a row of the brain, not a bar across it').toBeLessThan(2.7);
  });
});
