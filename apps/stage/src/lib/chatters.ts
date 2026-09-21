/**
 * "Somebody new is here": the first sighting of a display name, which the rail answers by putting
 * DESCRIBE on the slot for a few seconds (`docs/design/describe-tab.md`, 2026-09-17).
 *
 * The operator's ask, verbatim: "when new user joins chat, switch to it for a few sec, cool down timer."
 * The hold and the cooldown are the tab controller's, because they are cadence and cadence lives
 * in `src/lib/tabs.ts` (`NEW_CHATTER_HOLD_MS`, `NEW_CHATTER_COOLDOWN_MS`). This module answers the
 * one question the controller cannot: *is this name new*.
 *
 * Three ways a line is not a new chatter, and all three matter on a 24/7 broadcast:
 *
 *   - **the bridge's own replies.** `bot: true` lines are flybridgebot answering a viewer
 *     (`services/bridge/src/onscreen-chat.ts`), and the bot is not a person arriving. It is still
 *     recorded as seen, so nothing about it can trigger later either.
 *   - **history.** `header.chat` is a ring the feed re-sends every snapshot, so the lines present
 *     when the page connected were said before anyone was watching *this* page, and a socket
 *     reconnect hands the whole ring over again. A line older than the page's connect time is
 *     therefore never a trigger — which is also what keeps a recorded fixture inert: its lines
 *     carry the wall time of the recording, hours or days before the page loaded, so replaying
 *     `steady` cannot make the slot jump (and cannot move a screenshot baseline).
 *   - **a name already seen.** Every line's name is recorded whether or not it triggered, so the
 *     second sighting is never the first, including after the cooldown swallowed the first switch.
 *
 * Names are keyed case-folded: Twitch display names differ from logins only by case and
 * punctuation, and `Dendrite` arriving after `dendrite` is the same person.
 *
 * Pure and DOM-free, clock supplied by the caller, so `tests/unit/new-chatter.test.ts` drives the
 * whole thing with a scripted chat ring.
 */
import type { ChatLine } from '@/chat/types';

export class ChatterWatch {
  private readonly seen = new Set<string>();

  /**
   * Wall-clock instant the page connected. Lines older than this are history, never arrivals.
   *
   * Wall clock rather than the page's data clock on purpose: the data clock is virtual under a
   * fixture and freezes on a held seek, while `ChatLine.wallMs` is a real `Date.now()` from
   * whoever accepted the line. Comparing the two in the same units is the only version of this
   * test that is right for both a live socket and a replay.
   */
  constructor(private readonly connectedAtWallMs: number) {}

  /** A fixture seek or loop: nothing this page saw, it saw. The connect time is not a seek's to move. */
  reset(): void {
    this.seen.clear();
  }

  /**
   * Record one snapshot's chat ring. True when at least one line in it is somebody new arriving.
   *
   * Every line is recorded as seen either way, so a name that shows up while the cooldown is
   * running does not get a second chance at it later.
   */
  observe(lines: readonly ChatLine[] | undefined): boolean {
    if (!lines || lines.length === 0) return false;
    let arrived = false;
    for (const line of lines) {
      const key = line.by.toLowerCase();
      const fresh = !this.seen.has(key);
      this.seen.add(key);
      if (fresh && line.bot !== true && line.wallMs >= this.connectedAtWallMs) arrived = true;
    }
    return arrived;
  }

  /** How many distinct names this page has seen, for `window.__stage` and the tests. */
  get count(): number {
    return this.seen.size;
  }
}
