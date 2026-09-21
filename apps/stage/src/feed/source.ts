/**
 * What the page needs from a feed, whichever end it comes from.
 *
 * Both implementations are pumped by the one rAF loop rather than owning a timer, so the whole
 * page runs on a single clock: seeking a fixture, catching up after a slow frame and painting all
 * read the same `nowMs`.
 */

export interface FeedSource {
  /** Open the socket or load the fixture. Resolves once the first snapshot could arrive. */
  start(): Promise<void>;
  /** Called once per animation frame with the paint clock. */
  pump(nowMs: number): void;
  /** Close everything. Safe to call twice. */
  stop(): void;
  /**
   * The clock the rest of the page should use this frame.
   *
   * Normally `nowMs`. A fixture held on a seek target (`?t=` without `&play=1`) returns the
   * virtual time of the snapshot it stopped on, which freezes the ticker's dwell timers, the
   * button afterglow, the moment overlay and the stale check — everything whose state is a
   * function of elapsed time. That is what makes a screenshot of a seek reproducible instead of
   * depending on how long the test took to get around to taking it.
   */
  clock?(nowMs: number): number;
}
