/**
 * The recovery splash's model: the notice file's contract, its validation, and what the splash
 * shows at a given wall-clock second.
 *
 * `infra/bin/fly-loop-recover` (the auto-unstick helper) writes one small JSON file while it acts
 * on a confirmed macro loop — restarting flysim, or resetting the run to an earlier rung — and
 * `flystage-web` serves it to this page at {@link RECOVERY_NOTICE_ROUTE}. Viewers otherwise see
 * the game freeze or jump for no reason; the splash says what is happening.
 *
 * Everything here is pure: the poller (`src/lib/recovery-poll.ts`) fetches, the panel
 * (`src/panels/RecoverySplash.tsx`) renders, and this file decides. The one rule it exists to
 * keep is that **the notice can never break the stream**: a missing, malformed, oversized, future
 * or stale file is no notice at all, and every string the helper wrote is clipped to a closed
 * character set before it can reach the screen.
 */

/** Where `flystage-web` (and the Vite dev/preview servers) serve the notice file. */
export const RECOVERY_NOTICE_ROUTE = '/recovery-notice.json';

/** Ignore a notice whose `updatedAt` is older than this (the contract: 15 minutes). */
export const RECOVERY_STALE_S = 15 * 60;
/** How long `done` stays up after the helper wrote it. */
export const RECOVERY_DONE_S = 8;
/** How long `failed` stays up after the helper wrote it. */
export const RECOVERY_FAILED_S = 20;
/**
 * Stop covering the game this long after an `acting` write, even if the helper never follows up.
 * A helper that died mid-recovery must not leave the game hidden behind a splash; with the game
 * uncovered again the page's own STALE FEED banner is the honest state.
 */
export const RECOVERY_ACTING_MAX_S = 10 * 60;
/** Tolerated clock skew between the helper's `updatedAt` and the page's clock. */
const FUTURE_SKEW_S = 120;

export type RecoveryPhase = 'countdown' | 'acting' | 'done' | 'failed';
export type RecoveryAction = 'restart' | 'reset';

/** A validated notice. Field names are the file's own; see `infra/docs/loop-recovery.md`. */
export interface RecoveryNotice {
  id: string;
  phase: RecoveryPhase;
  action: RecoveryAction;
  fromRung: number | null;
  fromLabel: string;
  /** Only for `reset`; null otherwise. */
  toRung: number | null;
  toLabel: string;
  reason: string;
  loop: string[];
  stuckSeconds: number | null;
  announcedAt: number;
  executeAt: number;
  updatedAt: number;
}

const PHASES: readonly RecoveryPhase[] = ['countdown', 'acting', 'done', 'failed'];
const ACTIONS: readonly RecoveryAction[] = ['restart', 'reset'];

/** Longest place name kept (the ladder's longest is 15 characters). */
const LABEL_MAX = 24;
/** Longest macro name kept (the pad's are at most 12, e.g. `GO OBJECTIVE`). */
const MACRO_MAX = 16;
/** At most this many macros of the loop are shown. */
const LOOP_MAX = 4;

/**
 * Clip a helper-written string to what the splash may draw: printable ASCII letters, digits,
 * spaces and a little punctuation (place names like `MT. MOON` or `S.S. ANNE`, `ROUTE 22`), with
 * runs of whitespace collapsed, capped at `max` characters. Anything else is dropped, so a notice
 * cannot put markup, control characters or an unbounded line on air.
 */
export function cleanText(value: unknown, max: number): string {
  if (typeof value !== 'string') return '';
  return value
    .replace(/\s+/g, ' ')
    .replace(/[^A-Za-z0-9 .,'&:#/-]/g, '')
    .replace(/ {2,}/g, ' ')
    .trim()
    .slice(0, max)
    .trim();
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function rung(value: unknown): number | null {
  const n = finite(value);
  return n !== null && Number.isInteger(n) && n >= 0 && n < 1000 ? n : null;
}

/**
 * Validate a parsed JSON value as a v1 notice, or return null.
 *
 * Required: `v === 1`, `id`, a known `phase` and `action`, and finite `announcedAt`, `executeAt`,
 * `updatedAt`. Everything else is optional and degrades the copy rather than the notice: a reset
 * without a `toLabel` still says it is rewinding, just not to where.
 */
export function parseRecoveryNotice(raw: unknown): RecoveryNotice | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  if (o.v !== 1) return null;
  const id = typeof o.id === 'string' ? o.id.slice(0, 64) : '';
  if (id === '') return null;
  if (!PHASES.includes(o.phase as RecoveryPhase)) return null;
  if (!ACTIONS.includes(o.action as RecoveryAction)) return null;
  const announcedAt = finite(o.announcedAt);
  const executeAt = finite(o.executeAt);
  const updatedAt = finite(o.updatedAt);
  if (announcedAt === null || executeAt === null || updatedAt === null) return null;

  const action = o.action as RecoveryAction;
  const loop = Array.isArray(o.loop)
    ? o.loop
        .map((step) => cleanText(step, MACRO_MAX).toUpperCase())
        .filter((step) => step !== '')
        .slice(0, LOOP_MAX)
    : [];
  const stuck = finite(o.stuckSeconds);

  return {
    id,
    phase: o.phase as RecoveryPhase,
    action,
    fromRung: rung(o.fromRung),
    fromLabel: cleanText(o.fromLabel, LABEL_MAX).toUpperCase(),
    toRung: action === 'reset' ? rung(o.toRung) : null,
    toLabel: action === 'reset' ? cleanText(o.toLabel, LABEL_MAX).toUpperCase() : '',
    reason: cleanText(o.reason, 32),
    loop,
    stuckSeconds: stuck !== null && stuck >= 0 ? stuck : null,
    announcedAt,
    executeAt,
    updatedAt,
  };
}

/** Parse the body the server returned. Never throws. */
export function parseRecoveryBody(body: string): RecoveryNotice | null {
  if (body.length === 0 || body.length > 16_384) return null;
  try {
    return parseRecoveryNotice(JSON.parse(body));
  } catch {
    return null;
  }
}

/**
 * How the splash sits on the stage: `box` is the Pokémon-style text box along the bottom of the
 * game, leaving the stuck loop visible above it; `cover` fills the game panel, which is frozen or
 * blank while flysim restarts.
 */
export type RecoveryLayout = 'box' | 'cover';

/** What the splash draws. Every string is already final copy. */
export interface RecoveryView {
  id: string;
  phase: RecoveryPhase;
  action: RecoveryAction;
  layout: RecoveryLayout;
  /** The small chip in the frame's top edge. */
  chip: string;
  /** The first line, Pokémon-dialogue style. */
  headline: string;
  /** The second line. */
  body: string;
  /** `M:SS` during the countdown, otherwise empty. */
  countdown: string;
  /** `GO OBJECTIVE > GO WARP`, or empty. */
  loop: string;
  /** `STUCK 30 MIN`, or empty. */
  stuck: string;
  /** Whether to animate the trailing dots / blinking arrow. */
  busy: boolean;
}

/** `0:42`, `1:00`, `12:05`. Negative clamps to `0:00`. */
export function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.ceil(seconds));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

/** `STUCK 30 MIN`, `STUCK 2 H`, `STUCK 1 H 5 MIN`; empty under a minute or when unknown. */
export function formatStuck(seconds: number | null): string {
  if (seconds === null || seconds < 60) return '';
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `STUCK ${minutes} MIN`;
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return m === 0 ? `STUCK ${h} H` : `STUCK ${h} H ${m} MIN`;
}

/**
 * Whether the notice is showing at `nowS` (epoch seconds), per the contract's lifetimes.
 *
 * Stateless on purpose: a page that reloads mid-recovery, or a Chromium the watchdog restarted,
 * draws exactly what a page that watched the whole thing would, from the file alone.
 */
export function recoveryVisible(notice: RecoveryNotice, nowS: number): boolean {
  const age = nowS - notice.updatedAt;
  if (age > RECOVERY_STALE_S) return false;
  if (age < -FUTURE_SKEW_S) return false;
  switch (notice.phase) {
    case 'countdown':
      return true;
    case 'acting':
      return age <= RECOVERY_ACTING_MAX_S;
    case 'done':
      return age <= RECOVERY_DONE_S;
    case 'failed':
      return age <= RECOVERY_FAILED_S;
  }
}

/**
 * Ladder rungs (`docs/design/ladder.md`) whose label is a place the fly can be "back at". The rest
 * are events (BOULDER BADGE, GOT A STARTER, HM CUT) and read as "the BOULDER BADGE milestone".
 */
const PLACE_RUNGS: ReadonlySet<string> = new Set([
  'PALLET TOWN', "OAK'S LAB", 'VIRIDIAN CITY', 'VIRIDIAN FOREST', 'PEWTER CITY', 'MT. MOON',
  'CERULEAN CITY', 'NUGGET BRIDGE', 'VERMILION CITY', 'ROCK TUNNEL', 'LAVENDER TOWN',
  'CELADON CITY', 'FUCHSIA CITY', 'CINNABAR ISLAND', 'INDIGO PLATEAU',
]);

/**
 * Where a reset goes, in words: `PEWTER CITY`, `the BOULDER BADGE milestone`, and `the start of
 * MT. MOON` when the target is the rung the fly is already on (the ladder's first reset: it goes
 * back to where it first reached that rung). Empty without a label.
 */
export function resetTarget(notice: RecoveryNotice): string {
  const label = notice.toLabel;
  if (label === '') return '';
  const named = PLACE_RUNGS.has(label) ? label : `the ${label} milestone`;
  const same = notice.toRung !== null && notice.toRung === notice.fromRung;
  return same ? `the start of ${named}` : named;
}

/** The splash at `nowS`, or null when nothing should be on screen. */
export function recoveryView(notice: RecoveryNotice | null, nowS: number): RecoveryView | null {
  if (notice === null || !recoveryVisible(notice, nowS)) return null;
  const reset = notice.action === 'reset';
  const to = resetTarget(notice);
  const base = {
    id: notice.id,
    phase: notice.phase,
    action: notice.action,
    loop: notice.loop.join(' > '),
    stuck: formatStuck(notice.stuckSeconds),
  };

  switch (notice.phase) {
    case 'countdown': {
      const remaining = notice.executeAt - nowS;
      const now = remaining <= 0;
      return {
        ...base,
        layout: 'box',
        chip: 'AUTO RECOVERY',
        headline: 'The fly is stuck in a loop!',
        body: reset
          ? now
            ? to !== '' ? `Rewinding to ${to} now` : 'Rewinding now'
            : to !== '' ? `Rewinding to ${to} in` : 'Rewinding to the last milestone in'
          : now
            ? 'Shaking it off now'
            : 'Shaking it off in',
        countdown: formatCountdown(remaining),
        busy: now,
      };
    }
    case 'acting':
      return {
        ...base,
        layout: 'cover',
        chip: 'AUTO RECOVERY',
        headline: reset ? 'Rewinding' : 'Shaking it off',
        body: reset
          ? to !== '' ? `Back to ${to}` : 'Back to the last milestone'
          : 'Same place, fresh start',
        countdown: '',
        busy: true,
      };
    case 'done':
      return {
        ...base,
        layout: 'box',
        chip: 'AUTO RECOVERY',
        headline: reset ? (to !== '' ? `Back at ${to}!` : 'Rewound!') : 'All shaken off!',
        body: "Go get 'em, little fly",
        countdown: '',
        loop: '',
        stuck: '',
        busy: false,
      };
    case 'failed':
      return {
        ...base,
        layout: 'box',
        chip: 'AUTO RECOVERY',
        headline: "That didn't work",
        body: 'Trying something else next',
        countdown: '',
        loop: '',
        stuck: '',
        busy: false,
      };
  }
}
