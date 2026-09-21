/**
 * Shared helpers for the e2e suite: the URL builder and the "the page is genuinely settled" wait.
 */
import type { Page } from '@playwright/test';

export type Theme = 't1' | 't2' | 't3';
/**
 * The committed fixtures. `shop` and `center` are the two scene-pinned ones
 * (`docs/design/macros.md` section 13): the mart and the Pokémon Center are pads the fake's
 * random walk reaches rarely and never for long, so each gets a recording that never leaves it.
 * `bigpad` is the fake's widest pad — every overworld walk at once (`--full-pad`,
 * `packages/feed/src/fake/palette.ts`) — used by the section-14 wide-pad shots.
 */
export type Fixture = 'cold-open' | 'steady' | 'big-moment' | 'macros' | 'shop' | 'center' | 'bigpad';
export type Tab = 'senses' | 'connectome' | 'ladder' | 'describe' | 'macros';

export interface StageUrlOptions {
  fixture?: Fixture;
  /** Seek target in seconds. Without `play`, the page holds here and the clock freezes. */
  t?: number;
  theme?: Theme;
  res?: '720' | '1080';
  play?: boolean;
  game?: string;
  audio?: boolean;
  loop?: boolean;
  /** Fly renderer: `webgl` (default), the 2D `paper` fallback, or `off`. */
  fly?: 'off' | 'webgl' | 'paper';
  /**
   * Pin the tab slot. Every visual test does, because the slot cycles every 45 to 60 s and a
   * screenshot of an unpinned slot is a screenshot of whatever the clock happened to be doing.
   */
  tab?: Tab;
  /** `off` is the chat kill switch: no panel at all, whatever the feed carries. */
  chat?: 'off';
}

/** Build a player-mode URL. Every knob the page has is a query parameter by design. */
export function stageUrl(options: StageUrlOptions = {}): string {
  const params = new URLSearchParams({
    mode: 'player',
    fixture: options.fixture ?? 'steady',
    theme: options.theme ?? 't1',
    res: options.res ?? '1080',
  });
  if (options.t !== undefined) params.set('t', String(options.t));
  if (options.play) params.set('play', '1');
  if (options.game) params.set('game', options.game);
  if (options.audio === false) params.set('audio', '0');
  if (options.loop === false) params.set('loop', '0');
  if (options.fly) params.set('fly', options.fly);
  if (options.tab) params.set('tab', options.tab);
  if (options.chat) params.set('chat', options.chat);
  return `/?${params.toString()}`;
}

/**
 * Wait for `data-ready="1"`, then for the feed to have actually arrived, then for the rail's own
 * transitions to finish.
 *
 * `data-ready` means fonts are loaded and the brain map's base bitmap has arrived. It deliberately
 * does *not* mean the fixture has been fetched and decoded, and those are not the same moment:
 * measured on a cold browser context, `steady` (10.8 MB) reports ready at 1.7 s and its first
 * accepted snapshot at 4.2 s. A fixed settle of 800 ms therefore screenshotted the page's own
 * initial zeroes — an empty chat panel, "0.0 Hz", no milestone label — whenever the machine was
 * busy enough to widen that gap, which is the flake that produced twenty-two failures in one run
 * and, worse, would have baked those zeroes into the screenshot baselines.
 *
 * So the wait is on the condition rather than the clock: at least one accepted snapshot, which is
 * what every assertion past this point is really waiting for. `__stage.health()` is the page's own
 * operator surface, so this asks the page rather than guessing.
 */
export async function gotoStage(page: Page, options: StageUrlOptions = {}): Promise<void> {
  await page.goto(stageUrl(options));
  await page.waitForSelector('html[data-ready="1"]', { timeout: 60_000 });
  await page.waitForFunction(() => (window.__stage?.health().accepted ?? 0) > 0, undefined, {
    timeout: 60_000,
  });
  await page.waitForTimeout(800);
}

/** The page's own test surface (`window.__stage`). */
export async function stageHandle(page: Page) {
  return page.evaluateHandle(() => window.__stage);
}

/** True when the page went ready without its brain map. */
export async function isDegraded(page: Page): Promise<boolean> {
  return page.evaluate(() => document.documentElement.dataset.degraded === '1');
}

/** Which tab panes the page says are visible. Exactly one, always. */
export async function visibleTabs(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll('[data-tab-pane]')]
      .filter((pane) => pane instanceof HTMLElement && pane.dataset.visible === '1')
      .map((pane) => (pane as HTMLElement).dataset.tabPane ?? '?'),
  );
}
