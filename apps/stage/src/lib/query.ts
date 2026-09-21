/**
 * The page's entire configuration surface is the query string, because the only two things that
 * ever launch it are a systemd `chromium --kiosk <url>` line and a Playwright test.
 *
 * `?mode=player` (the default) replays a recorded `.flyfeed`; `?mode=live` opens the real socket.
 * `?t=` seeks the fixture and, unless `&play=1` is given, holds there — that is what makes a
 * screenshot deterministic.
 */

import { parseTab, type TabId } from './tabs';

export type StageMode = 'player' | 'live';
export type StageTheme = 't1' | 't2' | 't3';
export type StageRes = '720' | '1080';
export type StageFly = 'off' | 'webgl' | 'paper';

/**
 * The chat panel's source.
 *
 * `feed` renders `header.chat` and disappears when the header does not carry it — a service with
 * chat disabled omits the key entirely (`docs/feed-protocol.md`), which is the same on-screen
 * outcome as this page's own kill switch. `off` is that kill switch: no panel at all, whatever
 * the feed says, which is what a moderation incident needs to be one query parameter away.
 */
export type StageChat = 'feed' | 'off';

export interface StageOptions {
  mode: StageMode;
  fixture: string;
  /** Seek target in seconds, or null to play from the start. */
  seekSeconds: number | null;
  /** False when `?t=` was given without `&play=1`: seek, then hold for the screenshot. */
  autoplay: boolean;
  loop: boolean;
  theme: StageTheme;
  res: StageRes;
  /**
   * Which fly renderer draws the strip: `webgl` (three.js, one GL context), `paper` (the 2D
   * fallback for a host with no usable WebGL), or `off` (no fly, no context).
   */
  fly: StageFly;
  /**
   * Pin the tab slot to one tab and stop the cycle (`?tab=senses|connectome|ladder|describe|macros`).
   *
   * A screenshot of a slot that cycles every 45 to 60 s is a screenshot of whatever the clock
   * happened to be doing, so every visual test and every mockup pins the tab.
   */
  tab: TabId | null;
  /** Where the chat panel reads its lines from. */
  chat: StageChat;
  /** Per-game config id, resolved against `src/games`. */
  game: string;
  /** Feed WebSocket URL for `mode=live`. */
  feedUrl: string;
  gains: { master: number; game: number; sfx: number };
  /** `?audio=0` disables the AudioContext entirely (default in tests). */
  audio: boolean;
  /** `?metrics=1` keeps the paint-stage histogram and prints it on demand. */
  metrics: boolean;
}

const THEMES: readonly StageTheme[] = ['t1', 't2', 't3'];
const FLIES: readonly StageFly[] = ['off', 'webgl', 'paper'];

function num(params: URLSearchParams, key: string, fallback: number): number {
  const raw = params.get(key);
  if (raw === null) return fallback;
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) ? value : fallback;
}

function flag(params: URLSearchParams, key: string, fallback: boolean): boolean {
  const raw = params.get(key);
  if (raw === null) return fallback;
  return raw !== '0' && raw !== 'false';
}

/** Parse the options out of a URL query string. Pure, so the unit tests can drive it. */
export function parseStageOptions(search: string, defaultFeedUrl = 'ws://127.0.0.1:7400/feed'): StageOptions {
  const params = new URLSearchParams(search);

  const mode: StageMode = params.get('mode') === 'live' ? 'live' : 'player';
  const themeParam = params.get('theme');
  const theme: StageTheme = THEMES.includes(themeParam as StageTheme) ? (themeParam as StageTheme) : 't1';
  // 1080 is the authoring and broadcast size and therefore the default; 720 is the 2/3 downscale.
  const res: StageRes = params.get('res') === '720' ? '720' : '1080';

  const flyParam = params.get('fly');
  const fly: StageFly = FLIES.includes(flyParam as StageFly) ? (flyParam as StageFly) : 'webgl';

  const tRaw = params.get('t');
  const seekSeconds = tRaw === null ? null : Number.parseFloat(tRaw.replace(/s$/, ''));

  const chatParam = params.get('chat');
  const chat: StageChat = chatParam === '0' || chatParam === 'off' ? 'off' : 'feed';

  return {
    mode,
    fixture: params.get('fixture') ?? 'steady',
    seekSeconds: seekSeconds !== null && Number.isFinite(seekSeconds) ? seekSeconds : null,
    autoplay: flag(params, 'play', seekSeconds === null),
    loop: flag(params, 'loop', true),
    theme,
    res,
    fly,
    tab: parseTab(params.get('tab')),
    chat,
    game: params.get('game') ?? 'pokemon-red',
    feedUrl: params.get('feed') ?? defaultFeedUrl,
    gains: {
      master: num(params, 'gain', 0.9),
      game: num(params, 'gamegain', 0.8),
      sfx: num(params, 'sfxgain', 0.5),
    },
    audio: flag(params, 'audio', true),
    metrics: flag(params, 'metrics', false),
  };
}

/** Read the options from `window.location`. */
export function stageOptions(): StageOptions {
  return parseStageOptions(window.location.search);
}
