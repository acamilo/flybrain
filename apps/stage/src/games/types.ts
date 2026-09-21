/**
 * The per-game config contract.
 *
 * There will be a second demo (a Game Boy platformer), so the page is game-agnostic by
 * construction: the *live values* always come from the feed header (`milestone.label`,
 * `milestone.next`, `game.mode`, `game.rewardCounts` keys, `game.badges`), and a game config
 * supplies only the human copy for those values plus the wordmark. The config is selected by
 * `?game=` and resolved in `src/games/index.ts`.
 *
 * The rule this encodes: no game's name, ladder or vocabulary appears anywhere outside its own
 * file in `src/games/`. Grep for the game's name and you should find exactly one file.
 */
import type { GameMode, RewardKind } from '@flybrain/feed';

/** How loudly a reward is presented in the ticker (design A3: value tiers drive presentation). */
export type RewardTier = 'quiet' | 'notable' | 'moment';

/** Human copy for one reward kind the adapter can report. */
export interface RewardCopy {
  /** Short ticker text. Constant; never interpolated with feed values beyond the amount. */
  label: string;
  tier: RewardTier;
  /**
   * Identical events within this window collapse into one ticker row with a count
   * ("+3 new places"). 0 disables collapsing for the kind.
   */
  dedupeMs: number;
  /** Plural noun for a collapsed row, e.g. `new places`. */
  collapsedNoun?: string;
}

/** A counter shown in the progress row beside the milestone ladder. */
export interface GameCounter {
  /** Which `FeedGame` field to read. */
  field: 'badges' | 'uniqueLocations' | 'rewardTotal';
  label: string;
  /** Denominator for a "3 of 8" readout, when the total is known and fixed. */
  outOf?: number;
}

export interface GameConfig {
  /** Matches the file name and the `?game=` value. */
  id: string;
  /**
   * Title-strip wordmark, rendered in Press Start 2P.
   *
   * Press Start 2P is one em per character and the strip is 1216 px wide, so at the 18 px the
   * strip uses this has to stay at or under about 30 characters to leave room for the subtitle
   * and the status badges. Measured, not guessed.
   */
  wordmark: string;
  /** Human name used in the sugar honesty line and the explainer cards. */
  name: string;
  /**
   * Fallback rung labels, index = `milestone.rank`.
   *
   * The feed is authoritative for both the current rung's label and the number of rungs
   * (`milestone.total`); this is what the panel falls back to when a header carries neither, which
   * is a recorded fixture or a flysim older than `total`. So its length no longer defines how many
   * rungs exist, and it does not have to match the service's ladder rung for rung.
   */
  milestoneLadder: readonly string[];
  /** Human copy for each `game.mode` the adapter can report. */
  modeLabels: Record<GameMode, string>;
  /** Human copy and presentation tier for each reward kind. */
  rewardCopy: Record<RewardKind, RewardCopy>;
  /**
   * Counters shown on the progress cluster's footer line (`ProgressCluster`), e.g.
   * "3/8 badges · 214 places". Per-game *data*, not copy: the values come
   * from the matching `FeedGame` field, and this config only supplies the label and denominator.
   */
  counters: readonly GameCounter[];
  /** Stuck-o-meter threshold in simulated seconds past which the panel changes colour. */
  stuckAlarmSeconds: number;
  /** True while the config is a placeholder for a demo that does not exist yet. */
  stub?: boolean;
}
