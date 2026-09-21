/**
 * Player mode: replay a recorded `.flyfeed` with no service running (design A7).
 *
 * The timeline comes from the recording itself — every snapshot header carries `wallMs` — so one
 * affine map does all the work:
 *
 *   clock(record) = baseClock + (record.wallMs - firstRecord.wallMs)
 *
 * Seeking to `?t=95` is then not a special case: set `baseClock = now - 95_000` and pump. Every
 * snapshot up to that point is ingested against its own virtual clock value, so the ticker's
 * dwell timers, the button afterglow and the moment overlay all end up in exactly the state they
 * would have been in had the page watched those 95 seconds live. Intermediate snapshots are
 * ingested `silent`, so the catch-up neither paints 2,850 frames nor queues 95 seconds of audio.
 *
 * The same catch-up path covers a slow frame during normal playback, which is why there is no
 * separate "we fell behind" branch.
 */
import { isGzip, iterateFlyfeedRecords, readFlyfeedManifest, type FlyfeedManifest } from '@flybrain/feed';
import { decodeFeedMessage, type DecodedSnapshot } from './decode';
import type { FeedIngest } from './store';
import type { FeedSource } from './source';

export interface FixturePlayerOptions {
  /** Seconds to seek to before the first paint. */
  seekSeconds?: number | null;
  /** False to hold on the seek target instead of playing (what a screenshot wants). */
  autoplay?: boolean;
  /** Restart from the beginning when the recording ends. */
  loop?: boolean;
  /** Bytes of the spikes bitset to require, or 0 to accept any. */
  expectedSpikeBytes?: number;
}

/**
 * Slack on the "is this snapshot due yet?" comparison.
 *
 * `baseClock = nowMs - seek` and then `dueAt = baseClock + seek` is not exactly `nowMs` in
 * IEEE 754, so without slack a seek lands on snapshot 2850 or 2851 depending on the fractional
 * part of `performance.now()` — which makes a screenshot non-deterministic in a way that is
 * almost impossible to see. Half a millisecond is nothing against a 33 ms frame.
 */
const DUE_EPSILON_MS = 0.5;

export class FixturePlayer implements FeedSource {
  private readonly url: string;
  private readonly ingest: FeedIngest;
  private readonly options: Required<FixturePlayerOptions>;

  private messages: Uint8Array[] = [];
  private manifestValue: FlyfeedManifest | null = null;
  private index = 0;
  private firstWallMs = 0;
  private baseClock = 0;
  private paused = false;
  /** Virtual clock of the last ingested snapshot, used while paused. */
  private frozenClock: number | null = null;
  /** Decoded one record ahead, so `pump` can ask when the next snapshot is due. */
  private next: DecodedSnapshot | null = null;
  private started = false;

  constructor(url: string, ingest: FeedIngest, options: FixturePlayerOptions = {}) {
    this.url = url;
    this.ingest = ingest;
    this.options = {
      seekSeconds: options.seekSeconds ?? null,
      autoplay: options.autoplay ?? true,
      loop: options.loop ?? true,
      expectedSpikeBytes: options.expectedSpikeBytes ?? 0,
    };
  }

  /** The recording's manifest, once loaded. */
  manifest(): FlyfeedManifest | null {
    return this.manifestValue;
  }

  /** Length of the recording in ms, from the manifest. */
  durationMs(): number {
    return this.manifestValue?.durationMs ?? 0;
  }

  async start(): Promise<void> {
    const bytes = await fetchFixture(this.url);
    const { manifest, bodyOffset } = readFlyfeedManifest(bytes);
    this.manifestValue = manifest;
    this.messages = [...iterateFlyfeedRecords(bytes, bodyOffset)];
    if (this.messages.length === 0) throw new Error(`fixture ${this.url} holds no snapshots`);

    const first = this.firstDecodable();
    if (first === null) {
      throw new Error(`fixture ${this.url} holds no snapshot this page can decode`);
    }
    this.firstWallMs = first;
    this.started = true;
    this.rewind(performance.now());
  }

  /**
   * The clock to use while held on a seek target: the virtual time of the last snapshot
   * ingested, so nothing that depends on elapsed time keeps moving under a screenshot.
   */
  clock(nowMs: number): number {
    return this.paused && this.frozenClock !== null ? this.frozenClock : nowMs;
  }

  /** True while held on a seek target (`?t=` without `&play=1`). */
  isPaused(): boolean {
    return this.paused;
  }

  pump(nowMs: number): void {
    if (!this.started || this.paused) return;

    // Catch up: ingest everything due, painting and sounding only the last of a burst. Each
    // record is decoded exactly once, one ahead, so "is the next one also due?" is free.
    for (let guard = 0; guard < 200_000; guard++) {
      if (!this.next) {
        this.next = this.advance();
        if (!this.next) {
          // End of the recording. Looping restarts from the top *without* re-applying the seek
          // target: a `?t=` past the end would otherwise restart, run out, and restart again
          // forever (it did, until this test).
          if (!this.options.loop) return;
          this.restart(nowMs);
          this.next = this.advance();
          if (!this.next) return;
        }
      }

      const dueAt = this.dueAt(this.next);
      if (dueAt > nowMs + DUE_EPSILON_MS) return;

      const snapshot = this.next;
      this.next = this.advance();
      const followerDue = this.next !== null && this.dueAt(this.next) <= nowMs + DUE_EPSILON_MS;

      this.frozenClock = dueAt;
      this.ingest.ingest(snapshot, dueAt, { silent: followerDue });
    }
  }

  private dueAt(snapshot: DecodedSnapshot): number {
    return this.baseClock + (snapshot.header.wallMs - this.firstWallMs);
  }

  /** Decode forward to the next usable record, or null at the end of the recording. */
  private advance(): DecodedSnapshot | null {
    while (this.index < this.messages.length) {
      const decoded = this.decodeAt(this.index);
      this.index += 1;
      if (decoded) return decoded;
    }
    return null;
  }

  stop(): void {
    this.started = false;
    this.messages = [];
    this.next = null;
  }

  /** Jump to `seconds` into the recording, then hold or play per `autoplay`. */
  seek(seconds: number, nowMs = performance.now()): void {
    this.restart(nowMs - seconds * 1000);
    this.paused = false;
    this.pump(nowMs);
    this.paused = !this.options.autoplay;
  }

  /** Rewind to the top of the recording. Does not pump. */
  private restart(baseClock: number): void {
    this.index = 0;
    this.next = null;
    this.frozenClock = null;
    this.ingest.reset();
    this.baseClock = baseClock;
  }

  private rewind(nowMs: number): void {
    const seek = this.options.seekSeconds;
    if (seek !== null && seek > 0) {
      this.seek(seek, nowMs);
      return;
    }
    this.restart(nowMs);
    this.paused = !this.options.autoplay;
  }

  /**
   * `wallMs` of the first record this page can decode, or null if there is none.
   *
   * Scanned rather than assumed: a recording of a real service can start with a snapshot this
   * build rejects, and the timeline has to come from a record that actually decodes.
   */
  private firstDecodable(): number | null {
    for (let index = 0; index < this.messages.length; index++) {
      const decoded = this.decodeAt(index);
      if (decoded) return decoded.header.wallMs;
    }
    return null;
  }

  /**
   * Decode one record, counting and skipping a bad one.
   *
   * A fixture is a recording of a real service, so it can contain a message the current page
   * rejects (a dataset with a different neuron count, say). Skipping keeps the recording playable
   * and the count visible in the honesty panel.
   */
  private decodeAt(index: number): DecodedSnapshot | null {
    const message = this.messages[index];
    if (!message) return null;
    try {
      return decodeFeedMessage(message, this.options.expectedSpikeBytes);
    } catch {
      this.ingest.noteDecodeError();
      return null;
    }
  }
}

/**
 * Fetch a `.flyfeed` or `.flyfeed.gz`.
 *
 * The `.gz` path inflates with `DecompressionStream`, the same way `loadCompressed` handles the
 * dataset's `.binz` artifacts, and for the same reason: the server must serve the bytes raw
 * without `content-encoding`, so the page controls when inflation happens.
 */
export async function fetchFixture(url: string): Promise<Uint8Array> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`unable to load fixture ${url}: HTTP ${response.status}`);

  const raw = new Uint8Array(await response.arrayBuffer());
  if (!isGzip(raw)) return raw;

  const stream = new Blob([raw as BlobPart]).stream().pipeThrough(new DecompressionStream('gzip'));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}
