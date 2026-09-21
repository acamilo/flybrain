/**
 * Number and duration formatting for the readouts.
 *
 * Everything here is tabular-safe: fixed decimal places and fixed-width unit words, so a value
 * changing 30 times a second never reflows the layout (the jitter the audit found on the old
 * page). The face does the rest — `.num` in `src/index.css`, which is Press Start 2P at exactly
 * one em per character, because the body face (Silkscreen) is proportional and has no `tnum`.
 */

/** `3 h 41 m`, `12 m 04 s`, `48 s`. Coarse on purpose: a stream clock is not a stopwatch. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;

  if (hours > 0) return `${hours} h ${String(minutes).padStart(2, '0')} m`;
  if (minutes > 0) return `${minutes} m ${String(secs).padStart(2, '0')} s`;
  return `${secs} s`;
}

/**
 * `3h41m`, `12m04s`, `48s`: the same information as {@link formatDuration} with no spaces.
 *
 * Used for HERE FOR and the two "ago" readouts. Layout v2 has no hero (README deviation 5), but
 * the six-character ceiling this form guarantees matters more than it did: HERE FOR is Press
 * Start 2P at 33 px, a full em per character, in a column that starts at five characters. The
 * spaced form would be nine and take the width out of the rung line beside it.
 */
export function formatDurationCompact(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;

  if (hours > 0) return `${hours}h${String(minutes).padStart(2, '0')}m`;
  if (minutes > 0) return `${minutes}m${String(secs).padStart(2, '0')}s`;
  return `${secs}s`;
}

/**
 * `0:47`, `2:00`: the stall meter, which is the one readout on the page measured in seconds.
 *
 * Its window is 120 s (`docs/design/ladder.md`), so `formatDurationCompact`'s "48s" then "1m00s"
 * changes width halfway through and the meter jumps. Minutes and seconds, always, fixed width.
 */
export function formatMinutesSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

/** `104:12:33`, for the run clock where every digit is wanted. */
export function formatClock(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '--:--:--';
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
}

/** Day 1 is the first 24 h of simulated run time. */
export function dayNumber(runSeconds: number): number {
  if (!Number.isFinite(runSeconds) || runSeconds < 0) return 1;
  return Math.floor(runSeconds / 86_400) + 1;
}

/** One decimal, always. `13.2`. */
export function formatHz(hz: number): string {
  if (!Number.isFinite(hz)) return '—';
  return hz.toFixed(1);
}

/** Thousands separators with a thin space, which survives the encoder better than a comma. */
export function formatCount(value: number): string {
  if (!Number.isFinite(value)) return '—';
  return Math.round(value).toLocaleString('en-US');
}

/** `+0.05`, `+3.0`. Reward values are always shown signed so a tick reads as a gain. */
export function formatReward(value: number): string {
  if (!Number.isFinite(value)) return '—';
  const decimals = Math.abs(value) >= 1 ? 1 : 2;
  return `${value >= 0 ? '+' : '−'}${Math.abs(value).toFixed(decimals)}`;
}

/** `0.98x`, the realtime factor. */
export function formatRealtime(factor: number): string {
  if (!Number.isFinite(factor)) return '—';
  return `${factor.toFixed(2)}x`;
}

/** Seconds remaining on a cooldown, rounded up so it never shows 0 while still blocking. */
export function formatCooldown(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return 'ready';
  return `${Math.ceil(ms / 1000)} s`;
}

/** One `game.counters` entry, formatted: `3/8 badges` when `outOf` is set, else `214 places`. */
export function formatCounter(counter: { field: string; label: string; outOf?: number }, value: number): string {
  const shown = formatCount(value);
  return counter.outOf !== undefined ? `${shown}/${counter.outOf} ${counter.label}` : `${shown} ${counter.label}`;
}

/**
 * The ladder panel's terse game-counters line: `3/8 badges · 214 places`. No sentence, no label
 * beyond the counters themselves.
 */
export function formatCounters(
  counters: readonly { field: string; label: string; outOf?: number }[],
  values: Record<string, number>,
): string {
  return counters.map((counter) => formatCounter(counter, values[counter.field] ?? 0)).join(' · ');
}

/**
 * Only display names the feed carried, and only in the shape the bridge promises
 * (`^[\p{L}\p{N}_]{1,25}$`). Defence in depth: the bridge validates before the sim call, and the
 * page validates again on render, so no path exists from chat text to the video frame.
 */
const DISPLAY_NAME = /^[\p{L}\p{N}_]{1,25}$/u;

/** A safe display name, or the literal fallback the bridge uses. */
export function safeDisplayName(name: string | null | undefined): string {
  if (typeof name !== 'string') return 'a viewer';
  return DISPLAY_NAME.test(name) ? name : 'a viewer';
}
