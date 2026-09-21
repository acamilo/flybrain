/**
 * Behavioural tests (design A10.5, and `docs/design/animation.md`'s own verification list): the
 * things that are about time, not pixels.
 *
 * None of them is visible in a screenshot, and every one was either broken or absent on the old
 * page: an invisible A press, a fly that moved when the feed did not, a ticker that flickered, a
 * moment that never left, a page that froze and said nothing.
 */
import { expect, test } from '@playwright/test';

import { intersects, LAYOUT } from '../../src/lib/geometry';
import { gotoStage, visibleTabs } from './stage';
import { pauseStage, recordedPresses } from './animation';

/** Round a Playwright bounding box to whole authoring pixels, for exact comparison against geometry.ts. */
function roundBox(box: { x: number; y: number; width: number; height: number } | null) {
  if (!box) return null;
  return { x: Math.round(box.x), y: Math.round(box.y), width: Math.round(box.width), height: Math.round(box.height) };
}

test('the A/B afterglow lasts 250 ms after the recorded falling edge', async ({ page }) => {
  const presses = await recordedPresses();
  await pauseStage(page);

  for (const press of presses) {
    // FixturePlayer timestamps edges at the recording's dueAt, not at the later DOM paint.
    // Align a paint with the exact 250 ms expiry (166 + 250 = 26 frames).
    const startMs = press.upMs - 166;
    await page.evaluate((seconds) => window.__stage!.seek(seconds), startMs / 1000);
    const samples: { elapsed: number; down: boolean; glow: boolean }[] = [];
    for (let elapsed = 16; elapsed <= press.upMs - startMs + 272; elapsed += 16) {
      await page.clock.runFor(16);
      const sample = await page.locator(`[data-button="${press.button}"]`).evaluate((cell) => ({
        down: (cell as HTMLElement).dataset.down === '1',
        glow: (cell as HTMLElement).dataset.glow === '1',
      }));
      samples.push({ elapsed, ...sample });
      const sourceMs = startMs + elapsed;
      const down = sourceMs >= press.downMs && sourceMs < press.upMs;
      expect(sample.down, `${press.button} down at fixture ${sourceMs} ms`).toBe(down);
      expect(sample.glow, `${press.button} glow at falling edge + ${sourceMs - press.upMs} ms`)
        .toBe(down || (sourceMs >= press.upMs && sourceMs < press.upMs + 250));
    }
    const rising = samples.find((sample) => sample.down)!;
    expect(rising, `${press.button} rising edge was not painted`).toBeDefined();
    const falling = samples.find((sample) => sample.elapsed > rising.elapsed && !sample.down)!;
    expect(falling, `${press.button} falling edge was not painted`).toBeDefined();
    const off = samples.find((sample) => sample.elapsed >= falling.elapsed && !sample.glow)!;
    expect(off, `${press.button} afterglow never ended`).toBeDefined();
    // Every frame before expiry is lit; the first frame at/after expiry is dark.
    expect(startMs + off.elapsed - press.upMs).toBe(250);
    console.log(`  ${press.button}: recorded press ${press.upMs - press.downMs} ms; ` +
      `painted press ${falling.elapsed - rising.elapsed} ms; falling-to-off ` +
      `${startMs + off.elapsed - press.upMs} ms`);
  }
});

test('a sugar event extends the proboscis', async ({ page }) => {
  // `big-moment` carries a sugar redemption a few seconds in, and the sugar moment holds while
  // the pulse does.
  await gotoStage(page, { fixture: 'big-moment', t: 8.5 });
  const during = await page.evaluate(() => window.__stage?.fly().proboscis ?? 0);

  await gotoStage(page, { fixture: 'big-moment', t: 30 });
  const after = await page.evaluate(() => window.__stage?.fly().proboscis ?? 0);

  expect(during, 'the proboscis did not extend during the sugar pulse').toBeGreaterThan(after + 0.2);
});

test('the fly is still when the feed is', async ({ page }) => {
  // Idle is breathing only: with the clock frozen the gait phase is zero and stays there, which is
  // also what makes the screenshots reproducible.
  await gotoStage(page, { fixture: 'steady', t: 95 });
  const first = await page.evaluate(() => window.__stage?.fly() ?? null);
  await page.waitForTimeout(1200);
  const second = await page.evaluate(() => window.__stage?.fly() ?? null);

  expect(first?.gaitPhase).toBe(0);
  expect(second?.gaitPhase).toBe(0);
  expect(second?.legTips).toEqual(first?.legTips);
});

test('the ticker holds a row for at least four seconds', async ({ page }) => {
  // The dwell gate, observed on the running page. Moment-tier rows (a badge, a sugar redemption,
  // a milestone) deliberately jump the gate, so only the quiet and notable promotions are timed.
  await gotoStage(page, { fixture: 'big-moment', t: 0.5, play: true });

  const promotions = await page.evaluate(async () => {
    const seen: { id: string; tier: string; at: number }[] = [];
    const container = document.querySelector('[data-testid="ticker"]');

    const sample = () => {
      const head = container?.querySelector('.ticker-row');
      if (!head) return;
      const id = head.getAttribute('data-ticker-id') ?? '';
      const tier = head.getAttribute('data-tier') ?? '';
      if (seen.length === 0 || seen[seen.length - 1]?.id !== id) {
        seen.push({ id, tier, at: performance.now() });
      }
    };

    const timer = setInterval(sample, 100);
    await new Promise((done) => setTimeout(done, 26_000));
    clearInterval(timer);
    return seen;
  });

  expect(promotions.length, 'the ticker never changed').toBeGreaterThan(1);

  const gated = promotions.filter((entry) => entry.tier !== 'moment');
  for (let i = 1; i < gated.length; i++) {
    const gap = (gated[i]?.at ?? 0) - (gated[i - 1]?.at ?? 0);
    expect(gap, `two gated rows promoted ${gap.toFixed(0)} ms apart`).toBeGreaterThanOrEqual(3900);
  }
});

test('the tab slot crossfades, and exactly one tab is ever visible', async ({ page }) => {
  await pauseStage(page);
  const before = await page.evaluate(() => window.__stage!.motion().tab);
  expect(before).not.toBe('ladder');
  expect(await visibleTabs(page)).toEqual([before]);

  // Fire while paused; the next frame starts the fade, not a wall-time sleep after fire().
  await page.evaluate(() => window.__stage!.fire('milestone', 'Reached: Pallet Town'));
  await page.clock.runFor(16);
  for (let elapsed = 0; elapsed <= 304; elapsed += 16) {
    if (elapsed > 0) await page.clock.runFor(16);
    const frame = await page.evaluate((outgoing) => ({
      visible: [...document.querySelectorAll<HTMLElement>('[data-tab-pane]')]
        .filter((pane) => pane.dataset.visible === '1').map((pane) => pane.dataset.tabPane),
      incoming: Number(getComputedStyle(document.querySelector('[data-tab-pane="ladder"]')!).opacity),
      outgoing: Number(getComputedStyle(document.querySelector(`[data-tab-pane="${outgoing}"]`)!).opacity),
    }), before);
    const progress = Math.min(elapsed / 300, 1);
    const opacity = progress < 0.5 ? 4 * progress ** 3 : 1 - (-2 * progress + 2) ** 3 / 2;
    expect(frame.visible, `semantic visibility at ${elapsed} ms`).toEqual(['ladder']);
    // The director publishes opacity to three decimals; pin the whole 300 ms cubic fade.
    expect(frame.incoming, `incoming at ${elapsed} ms`).toBe(Number(opacity.toFixed(3)));
    expect(frame.outgoing, `outgoing at ${elapsed} ms`).toBe(Number((1 - opacity).toFixed(3)));
  }

  // Settled: one pane visible, one pane painted, and the strip agrees with it.
  const settled = await page.evaluate(() => ({
    visible: [...document.querySelectorAll('[data-tab-pane]')]
      .filter((pane) => (pane as HTMLElement).dataset.visible === '1')
      .map((pane) => (pane as HTMLElement).dataset.tabPane),
    painted: [...document.querySelectorAll('[data-tab-pane]')].filter(
      (pane) => Number.parseFloat(getComputedStyle(pane).opacity) > 0.01,
    ).length,
    active: [...document.querySelectorAll('[data-tab]')]
      .filter((tab) => (tab as HTMLElement).dataset.active === '1')
      .map((tab) => (tab as HTMLElement).dataset.tab),
    underline: document.querySelector('[data-tab-underline]')?.getBoundingClientRect().width ?? 0,
  }));
  expect(settled.visible).toEqual(['ladder']);
  expect(settled.painted).toBe(1);
  expect(settled.active).toEqual(['ladder']);
  expect(settled.underline, 'the underline never sized itself').toBeGreaterThan(40);

  // And the focus hands the slot back when the moment is over: 9 s of total stage time.
  await page.clock.runFor(9000);
  expect(await page.evaluate(() => window.__stage!.motion().focused)).toBe(false);
  expect(await visibleTabs(page)).toHaveLength(1);
});

test('two moments at once are ordered by priority, and the loser does not come back', async ({ page }) => {
  // `docs/design/animation.md`: "A lower-priority moment waits; equal priority coalesces", and the
  // queue ordering with two simultaneous events is on its verification list explicitly.
  //
  // Two cases, because the answer differs and both are deliberate
  // (`src/motion/moments.ts`): a *lower* priority moment that arrives while something holds
  // waits its turn, and one that is *already on stage* when something bigger arrives is spent —
  // its exit is cut short and it is not replayed, because "a sugar pulse replayed nine seconds
  // after the badge that interrupted it is a lie about when it happened".
  await gotoStage(page, { fixture: 'steady', t: 20, play: true });

  // Case one: sugar first, then a badge on the same frame. The badge takes the stage.
  const preempted = await page.evaluate(async () => {
    const seen: { type: string | null; phase: string | null; pending: number }[] = [];
    window.__stage?.fire('sugar', 'SUGAR', 'ada');
    window.__stage?.fire('badge', 'BOULDER BADGE', '1 of 8');

    for (let i = 0; i < 40; i++) {
      const motion = window.__stage?.motion();
      const last = seen[seen.length - 1];
      if (!last || last.type !== (motion?.moment ?? null) || last.phase !== (motion?.momentPhase ?? null)) {
        seen.push({
          type: motion?.moment ?? null,
          phase: motion?.momentPhase ?? null,
          pending: motion?.pending ?? 0,
        });
      }
      await new Promise((done) => setTimeout(done, 250));
    }
    return seen;
  });

  // eslint-disable-next-line no-console
  console.log(`  pre-empted: ${preempted.map((entry) => `${String(entry.type)}/${String(entry.phase)}`).join(' -> ')}`);

  const types = preempted.map((entry) => entry.type);
  expect(types[0], 'the sugar did not take the stage first').toBe('sugar');
  expect(preempted[0]?.phase, 'the sugar was not sent off by the badge').toBe('leaving');
  expect(preempted[0]?.pending, 'the badge should be waiting for the sugar to leave').toBeGreaterThan(0);
  expect(types, 'the badge never reached the stage').toContain('badge');
  // Spent, not requeued: sugar appears once, at the start, on its way out.
  expect(types.filter((type) => type === 'sugar')).toHaveLength(1);
  expect(types.at(-1), 'the stage never emptied').toBeNull();

  // Case two: a badge holding, then sugar. The sugar waits nine seconds and then plays.
  await gotoStage(page, { fixture: 'steady', t: 20, play: true });
  const queued = await page.evaluate(async () => {
    const seen: (string | null)[] = [];
    window.__stage?.fire('badge', 'BOULDER BADGE', '1 of 8');
    await new Promise((done) => setTimeout(done, 400));
    window.__stage?.fire('sugar', 'SUGAR', 'ada');

    for (let i = 0; i < 48; i++) {
      const type = window.__stage?.motion().moment ?? null;
      if (seen.length === 0 || seen[seen.length - 1] !== type) seen.push(type);
      await new Promise((done) => setTimeout(done, 250));
    }
    return seen;
  });

  // eslint-disable-next-line no-console
  console.log(`  queued: ${queued.map((type) => String(type)).join(' -> ')}`);
  expect(queued[0]).toBe('badge');
  expect(queued, 'the queued sugar never got its turn').toContain('sugar');
  expect(queued.indexOf('sugar')).toBeGreaterThan(queued.indexOf('badge'));
});

test('a rollback wipes the game canvas', async ({ page }) => {
  // The catalogue's one exception to "moments never touch the game canvas". 400 ms of wipe, so the
  // canvas has to be measurably different mid-effect and identical in kind afterwards.
  //
  // This used to also assert the LADDER tab's best-snapshot thumbnail blinked twice; that
  // thumbnail was dropped 2026-09-16 (its 160 px went back to the rung names instead), and with it
  // the one behaviour this test pinned on `[data-testid="best-thumb"]`.
  await gotoStage(page, { fixture: 'steady', t: 20, play: true, tab: 'ladder' });

  const sample = async () =>
    page.evaluate(() => {
      const canvas = document.querySelector<HTMLCanvasElement>('[data-testid="game"] canvas');
      const ctx = canvas?.getContext('2d');
      if (!canvas || !ctx) return null;
      // One row across the middle: the wipe is horizontal, so this is where it shows.
      const row = ctx.getImageData(0, Math.floor(canvas.height / 2), canvas.width, 1).data;
      let sum = 0;
      for (let i = 0; i < row.length; i += 4) sum += row[i] as number;
      return sum;
    });

  await page.evaluate(() => window.__stage?.fire('rollback', 'REWIND', 'try 3'));
  await page.waitForTimeout(80);

  const during = await sample();
  expect(during).not.toBeNull();

  // The wipe writes a bright seam and a scanline field over the frame, so the row's luminance sum
  // moves; afterwards the canvas is just the game again.
  await page.waitForTimeout(700);
  const after = await sample();
  expect(after).not.toBeNull();
  expect(Math.abs((during as number) - (after as number)), 'the wipe left no trace on the canvas').toBeGreaterThan(
    0,
  );
});

test('a new chat line slides in, and the panel keeps only the last seven', async ({ page }) => {
  // The chat ring is bounded in the header (<= 12) and on screen (7). The slide is a CSS
  // animation keyed on the line's own id, so a new line animates and the six above it do not.
  await gotoStage(page, { fixture: 'steady', t: 20, play: true });

  const panel = page.locator('[data-testid="chat-panel"]');
  await expect(panel).toHaveCount(1);

  const first = await page.evaluate(() =>
    [...document.querySelectorAll('.chat-line')].map((line) => (line as HTMLElement).dataset.chatId ?? ''),
  );
  expect(first.length, 'the fixture carries no chat').toBeGreaterThan(0);
  expect(first.length).toBeLessThanOrEqual(7);

  // Wait for the ring to move: the fake simulator's chatter is a few seconds apart.
  await expect
    .poll(
      async () =>
        page.evaluate(
          () => [...document.querySelectorAll('.chat-line')].at(-1)?.getAttribute('data-chat-id') ?? '',
        ),
      { timeout: 30_000 },
    )
    .not.toBe(first.at(-1));

  const after = await page.evaluate(() => {
    const lines = [...document.querySelectorAll('.chat-line')];
    const newest = lines.at(-1) as HTMLElement | undefined;
    return {
      count: lines.length,
      ids: lines.map((line) => (line as HTMLElement).dataset.chatId ?? ''),
      animation: newest ? getComputedStyle(newest).animationName : '',
      duration: newest ? getComputedStyle(newest).animationDuration : '',
    };
  });

  expect(after.count).toBeLessThanOrEqual(7);
  expect(after.ids, 'the ring did not advance').not.toEqual(first);
  expect(after.animation, 'a new chat line does not slide in').toBe('slide-up');
  expect(after.duration).toBe('0.24s');
});

/**
 * The longest line the protocol allows, at exactly the sanitizer's limit.
 *
 * 200 code points of ordinary words, which is what makes this the wrapping case: at VT323's
 * 0.4 em advance and the 24 px floor it is 1920 px of text in a 972 px measure, so it takes three
 * rows or it is not all there.
 */
const LONG_LINE =
  'the fly walked into the same ledge for twenty minutes and i have never been prouder of an insect in my whole life, somebody please teach it that the b button exists and that a ledge is not a door yet!';

test('a 200-character chat line wraps and stays whole in the panel', async ({ page }) => {
  // the operator, 2026-09-17: "make chat messages wrap." The committed fixtures were recorded off a live
  // bridge and none of them holds a line anywhere near the limit, so the ring is replaced by hand
  // through the page's own test surface (`window.__stage.chat`, which writes the *feed header* and
  // therefore goes through the same sanitizer a real line does).
  await gotoStage(page, { fixture: 'steady', t: 95, tab: 'senses' });
  expect(LONG_LINE.length, 'the long line is not at the sanitizer limit').toBe(200);

  const accepted = await page.evaluate(
    (long) =>
      window.__stage?.chat([
        { by: 'moth_lord', text: 'he has been in there two hours' },
        { by: 'dendrite', text: 'go left! the ledge is right there' },
        { by: 'flybridgebot', text: 'sugar goes to the fly, not to the fly', bot: true },
        { by: 'viridian_city_enjoyer', text: long },
      ]) ?? 0,
    LONG_LINE,
  );
  expect(accepted, 'the panel refused the injected ring').toBe(4);
  // Every injected line is a new React key, so all four slide in (`src/theme/motion.css`, 240 ms)
  // and a row measured mid-arrival is still 14 px below where it lands.
  await page.waitForTimeout(400);

  const shape = await page.evaluate(() => {
    const box = document.querySelector('[data-testid="chat-lines"]') as HTMLElement;
    const rows = [...box.querySelectorAll('.chat-line')] as HTMLElement[];
    const outer = box.getBoundingClientRect();
    const newest = rows.at(-1) as HTMLElement;
    const lineHeight = Number.parseFloat(getComputedStyle(newest).lineHeight);
    return {
      // The text as rendered, so a CSS ellipsis or a clipped span shows up as missing characters.
      newestText: newest.querySelector('.chat-line__text')?.textContent ?? '',
      newestRows: Math.round(newest.getBoundingClientRect().height / lineHeight),
      // Every row the panel says it is showing, and whether it is inside the panel whole.
      shown: rows
        .filter((row) => row.dataset.fits === '1')
        .map((row) => {
          const rect = row.getBoundingClientRect();
          return {
            id: row.dataset.chatId ?? '',
            inside: rect.top >= outer.top - 0.5 && rect.bottom <= outer.bottom + 0.5,
            // Nothing runs off the side of a row either: the hanging indent and `overflow-wrap`
            // have to be enough for a 200-character line with no break in it.
            clipped: row.scrollWidth > row.clientWidth + 0.5,
          };
        }),
      // No horizontal overflow and no scrollbar anywhere in the panel: the frame is fixed and
      // nobody on a broadcast can reach a scrollbar.
      overflowX: box.scrollWidth - box.clientWidth,
      scrollbar: box.offsetWidth - box.clientWidth,
      overflow: getComputedStyle(box).overflowY,
    };
  });

  // Wrapped, whole, and all 200 characters of it.
  expect(shape.newestText).toBe(LONG_LINE);
  expect(shape.newestRows, 'the longest line did not wrap').toBeGreaterThan(1);
  expect(shape.shown.length, 'the panel hid every line it had').toBeGreaterThan(0);
  expect(
    shape.shown.filter((row) => !row.inside).map((row) => row.id),
    'a line the panel is showing is not inside it',
  ).toEqual([]);
  expect(
    shape.shown.filter((row) => row.clipped).map((row) => row.id),
    'a line the panel is showing is clipped',
  ).toEqual([]);
  expect(shape.overflowX, 'the panel overflows sideways').toBe(0);
  expect(shape.scrollbar, 'the panel grew a scrollbar').toBe(0);
  expect(shape.overflow).toBe('hidden');

  // And the oldest lines are the ones that go: eleven long lines cannot all fit 244 px, so the
  // panel drops them off the top rather than shrinking, clipping or ellipsising anything.
  await page.evaluate(
    (long) => window.__stage?.chat(Array.from({ length: 11 }, (_, index) => ({ by: `chatter_${index}`, text: long }))),
    LONG_LINE,
  );
  await page.waitForTimeout(400);
  const dropped = await page.evaluate(() => {
    const rows = [...document.querySelectorAll('.chat-line')] as HTMLElement[];
    const shown = rows.filter((row) => row.dataset.fits === '1');
    return {
      rows: rows.length,
      shown: shown.length,
      // The newest is always shown, and the ones hidden are a prefix: the oldest.
      newestShown: shown.at(-1) === rows.at(-1),
      hiddenAreOldest: rows.findIndex((row) => row.dataset.fits === '1') === rows.length - shown.length,
    };
  });

  // Seven is the ring (`CHAT_LINES`); fewer than seven are shown because each one is three rows.
  expect(dropped.rows).toBe(7);
  expect(dropped.shown).toBeGreaterThan(0);
  expect(dropped.shown).toBeLessThan(7);
  expect(dropped.newestShown, 'the newest line is not on screen').toBe(true);
  expect(dropped.hiddenAreOldest, 'the panel dropped lines from somewhere other than the top').toBe(true);
});

test('the stale banner appears two seconds after the feed stops', async ({ page }) => {
  // The old page froze its numbers and said nothing, so a wedged worker looked like a live
  // stream. This is driven by the store's own clock, not by a socket event, because the failure
  // that matters (a half-open connection to a hung service) produces no event at all.
  await gotoStage(page, { fixture: 'steady', t: 10, play: true });

  const banner = page.locator('[data-testid="stale-banner"]');
  await expect(banner).toHaveCount(0);

  await page.evaluate(() => window.__stage?.stopFeed());

  await page.waitForTimeout(1500);
  expect(await banner.count(), 'the banner came up before 2 s of silence').toBe(0);

  await expect(banner).toBeVisible({ timeout: 3000 });
  await expect(banner).toContainText('STALE FEED');
});

test('the audio engine comes up and never throws, whatever the context does', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 10, play: true });
  await page.waitForTimeout(2000);

  const audio = await page.evaluate(() => window.__stage?.audio() ?? null);
  expect(audio).not.toBeNull();
  expect(audio?.lastError, 'the audio engine reported an error').toBeNull();
  expect(['running', 'suspended'], `context state was ${String(audio?.context)}`).toContain(audio?.context);
  // Eight, not six: the rail wiring added the rollback's rewind sweep and the day stinger, which
  // the moment catalogue's sound tiers had been borrowing other samples for.
  expect(audio?.sfxLoaded, 'the synthesised SFX bank did not render').toBe(8);

  // With a running context the ring buffer must actually be receiving the feed's PCM.
  if (audio?.context === 'running') {
    expect(audio.worklet).toBe('ready');
    expect(audio.pushedFrames, 'no audio frames reached the worklet').toBeGreaterThan(0);
  }
});

test('the paint loop keeps its budget, moment and all', async ({ page }) => {
  // `docs/design/animation.md`'s budget: "All of the above must keep the page's whole-frame paint
  // p95 under 4 ms on the laptop". Measured with a moment running, because an idle stage is not
  // the case the budget is about — the badge is the most expensive frame the page ever draws.
  await gotoStage(page, { fixture: 'steady', t: 10, play: true });
  await page.waitForTimeout(3000);

  await page.evaluate(() => {
    window.__stage?.fire('badge', 'BOULDER BADGE', '1 of 8');
  });
  await page.waitForTimeout(5000);

  const metrics = await page.evaluate(() => window.__stage?.metrics() ?? null);
  expect(metrics).not.toBeNull();

  const frame = metrics?.stages.frame;
  expect(frame?.count ?? 0, 'the loop did not run').toBeGreaterThan(100);
  // eslint-disable-next-line no-console
  console.log(
    `  frame p50 ${frame?.p50Ms.toFixed(2)} p95 ${frame?.p95Ms.toFixed(2)} max ${frame?.maxMs.toFixed(2)} ` +
      `(n=${frame?.count}), motion p95 ${metrics?.stages.motion?.p95Ms.toFixed(2)}, ` +
      `brainmap p95 ${metrics?.stages.brainmap?.p95Ms.toFixed(2)}`,
  );

  // The design's own figure is 4 ms; the gate here is 16 because this is a laptop under a test
  // runner with a video capture attached, not the capture VM. The reported number is what the
  // milestone asks for; this assertion is the order-of-magnitude regression catch.
  expect(frame?.p95Ms ?? 99, `frame p95 was ${frame?.p95Ms}`).toBeLessThan(16);

  const map = metrics?.stages.brainmap;
  expect(map?.p95Ms ?? 99, `brain map p95 was ${map?.p95Ms}`).toBeLessThan(12);

  const sprites = await page.evaluate(() => window.__stage?.sprites() ?? -1);
  expect(sprites, 'the sprite pass is meant to be capped at 256').toBeLessThanOrEqual(256);

  const particles = await page.evaluate(() => window.__stage?.motion().particles ?? -1);
  expect(particles, 'the particle pool is meant to be capped at 400').toBeLessThanOrEqual(400);
});

test('the feed plays without gaps or decode errors', async ({ page }) => {
  await gotoStage(page, { fixture: 'steady', t: 10, play: true });
  await page.waitForTimeout(4000);

  const health = await page.evaluate(() => window.__stage?.health() ?? null);
  expect(health?.decodeErrors).toBe(0);
  expect(health?.gaps, 'the fixture has a seq gap in it').toBe(0);
  expect(health?.accepted ?? 0).toBeGreaterThan(300);
});

test('a moment never covers the game or the fly', async ({ page }) => {
  // Layout v1 promoted the brain map over the rail for nine seconds and this test guarded the one
  // thing a broadcast overlay must never do. Layout v2 replaced the promotion with a tab focus,
  // so the only thing that now reaches outside the rail is the caption band — and it is over the
  // tab slot, which is inside it.
  await gotoStage(page, { fixture: 'steady', t: 20, play: true });
  await page.evaluate(() => window.__stage?.fire('badge', 'BOULDER BADGE', '1 of 8'));
  await page.waitForTimeout(400);

  const caption = page.locator('[data-testid="moment-caption"]');
  await expect(caption).toBeVisible();

  const captionBox = roundBox(await caption.boundingBox());
  expect(captionBox).not.toBeNull();
  if (captionBox) {
    expect(intersects(captionBox, LAYOUT.game), 'the caption band covers the game').toBe(false);
    expect(intersects(captionBox, LAYOUT.flyStrip), 'the caption band covers the fly strip').toBe(false);
    expect(intersects(captionBox, LAYOUT.title), 'the caption band covers the title strip').toBe(false);
    expect(intersects(captionBox, LAYOUT.tabs), 'the caption band is not over the tab slot').toBe(true);
  }

  // The game canvas has not moved, and it does not move when the moment ends either.
  expect(roundBox(await page.locator('[data-testid="game"] canvas').boundingBox())).toEqual({
    x: LAYOUT.game.x,
    y: LAYOUT.game.y,
    width: LAYOUT.game.width,
    height: LAYOUT.game.height,
  });

  await expect(caption).toHaveCount(0, { timeout: 12_000 });
  expect(roundBox(await page.locator('[data-testid="game"] canvas').boundingBox())).toEqual({
    x: LAYOUT.game.x,
    y: LAYOUT.game.y,
    width: LAYOUT.game.width,
    height: LAYOUT.game.height,
  });
});
