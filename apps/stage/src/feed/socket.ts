/**
 * Live mode: the real WebSocket client (design A6).
 *
 * Implemented and unit-tested now, but only reachable with `?mode=live`, because S3 (wiring the
 * page to a running flysim) waits on the theme being chosen from the mockups. What it does:
 *
 *   - sends exactly one message, ever: the `hello` with its `wants` list. The page is a display.
 *   - reconnects with exponential backoff and jitter, forever, because a 24/7 stream outlives
 *     every service restart underneath it.
 *   - counts, rather than hides, the things that go wrong: a decode failure increments a counter
 *     the honesty panel shows, and 2 s of silence raises the STALE FEED banner (the banner is
 *     driven by the store's clock, so it appears whether the socket noticed or not — a half-open
 *     TCP connection is exactly the case where the socket does *not* notice).
 */
import { FEED_PROTOCOL, type AttachmentKind, type ClientHello } from '@flybrain/feed';
import { decodeFeedMessage } from './decode';
import type { FeedIngest } from './store';
import type { FeedSource } from './source';
import { useStage } from './store';

export interface FeedSocketOptions {
  wants?: AttachmentKind[];
  /** First retry delay. Doubles per attempt up to `maxBackoffMs`. */
  backoffMs?: number;
  maxBackoffMs?: number;
  expectedSpikeBytes?: number;
  /** Injected for tests; defaults to the platform `WebSocket`. */
  factory?: (url: string) => WebSocket;
}

export class FeedSocket implements FeedSource {
  private readonly url: string;
  private readonly ingest: FeedIngest;
  private readonly wants: AttachmentKind[];
  private readonly backoffMs: number;
  private readonly maxBackoffMs: number;
  private readonly expectedSpikeBytes: number;
  private readonly factory: (url: string) => WebSocket;

  private socket: WebSocket | null = null;
  private retries = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;

  constructor(url: string, ingest: FeedIngest, options: FeedSocketOptions = {}) {
    this.url = url;
    this.ingest = ingest;
    this.wants = options.wants ?? ['frame', 'audio', 'spikes'];
    this.backoffMs = options.backoffMs ?? 250;
    this.maxBackoffMs = options.maxBackoffMs ?? 5000;
    this.expectedSpikeBytes = options.expectedSpikeBytes ?? 0;
    this.factory = options.factory ?? ((target) => new WebSocket(target));
  }

  async start(): Promise<void> {
    this.closed = false;
    this.open();
  }

  /** Nothing to pump: messages arrive by event. Kept so both sources share one interface. */
  pump(): void {}

  stop(): void {
    this.closed = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    const socket = this.socket;
    this.socket = null;
    if (socket) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      socket.close();
    }
  }

  /** Delay before retry `attempt` (0-based), with jitter so restarts do not synchronise. */
  backoffFor(attempt: number): number {
    const base = Math.min(this.maxBackoffMs, this.backoffMs * 2 ** attempt);
    return Math.round(base * (0.75 + Math.random() * 0.5));
  }

  private open(): void {
    if (this.closed) return;
    useStage.getState().setConnection('connecting');

    let socket: WebSocket;
    try {
      socket = this.factory(this.url);
    } catch {
      this.scheduleReconnect();
      return;
    }
    socket.binaryType = 'arraybuffer';
    this.socket = socket;

    socket.onopen = () => {
      this.retries = 0;
      useStage.getState().setConnection('open');
      const hello: ClientHello = { protocol: FEED_PROTOCOL as 1, client: 'stage', wants: this.wants };
      socket.send(JSON.stringify(hello));
    };

    socket.onmessage = (event: MessageEvent<unknown>) => {
      const data = event.data;
      if (!(data instanceof ArrayBuffer)) return;
      try {
        this.ingest.ingest(decodeFeedMessage(new Uint8Array(data), this.expectedSpikeBytes), performance.now());
      } catch {
        this.ingest.noteDecodeError();
      }
    };

    socket.onerror = () => {
      // `onclose` always follows, and that is where the retry lives.
    };

    socket.onclose = () => {
      if (this.socket === socket) this.socket = null;
      useStage.getState().setConnection('closed');
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    if (this.closed) return;
    const delay = this.backoffFor(this.retries);
    this.retries += 1;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.open();
    }, delay);
  }
}
