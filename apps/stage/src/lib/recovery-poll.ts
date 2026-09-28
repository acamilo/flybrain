/**
 * Polls `flystage-web` for the recovery notice (`src/lib/recovery.ts`).
 *
 * Deliberately not the feed: flysim is the thing being restarted, so during `acting` there is no
 * feed and no control API to ask. `flystage-web` is the page's own static server, in the same
 * container and independent of flysim, and serves the helper's file at
 * {@link RECOVERY_NOTICE_ROUTE} (`infra/config/serve.mjs`).
 *
 * Once a second, with a short timeout, and every failure — no server, a 404 from an older
 * `serve.mjs`, a 204 for "no file", a half-written body, a hung request — is "no notice". The
 * poller never throws into the page and never retries faster than its interval.
 */
import { RECOVERY_NOTICE_ROUTE, parseRecoveryBody, type RecoveryNotice } from './recovery';

export const RECOVERY_POLL_MS = 1000;
const TIMEOUT_MS = 2500;

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

export interface RecoveryPollerOptions {
  url?: string;
  intervalMs?: number;
  fetch?: Fetch;
  onChange: (notice: RecoveryNotice | null) => void;
}

export class RecoveryPoller {
  private readonly url: string;
  private readonly intervalMs: number;
  private readonly fetchFn: Fetch;
  private readonly onChange: (notice: RecoveryNotice | null) => void;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  /** The last body seen, so an unchanged file does not re-render the panel. */
  private lastKey = '\u0000';

  constructor(options: RecoveryPollerOptions) {
    this.url = options.url ?? RECOVERY_NOTICE_ROUTE;
    this.intervalMs = options.intervalMs ?? RECOVERY_POLL_MS;
    this.fetchFn = options.fetch ?? ((input, init) => fetch(input, init));
    this.onChange = options.onChange;
  }

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    void this.tick();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  /** One poll. Public so the unit tests can drive it without timers. */
  async poll(): Promise<RecoveryNotice | null> {
    let body = '';
    const abort = new AbortController();
    const timeout = setTimeout(() => abort.abort(), TIMEOUT_MS);
    try {
      const response = await this.fetchFn(this.url, { cache: 'no-store', signal: abort.signal });
      if (response.status === 200) body = await response.text();
    } catch {
      body = '';
    } finally {
      clearTimeout(timeout);
    }
    const notice = parseRecoveryBody(body);
    const key = notice === null ? '' : JSON.stringify(notice);
    if (key !== this.lastKey) {
      this.lastKey = key;
      try {
        this.onChange(notice);
      } catch (error) {
        console.warn(`recovery splash: ${(error as Error).message}`);
      }
    }
    return notice;
  }

  private async tick(): Promise<void> {
    if (this.stopped) return;
    await this.poll();
    if (this.stopped) return;
    this.timer = setTimeout(() => void this.tick(), this.intervalMs);
  }
}
