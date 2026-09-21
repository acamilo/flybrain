/**
 * Per-game config for the second demo, Super Mario Land (`sml-progress-v1`).
 *
 * This is the only file in the app that names this game. The ladder is the adapter's 16 ranks from
 * `docs/design/platformer.md` §3; the feed header stays authoritative for the *current* rank's
 * label, so a ladder change in the service shows up on screen without a page release. The same goes
 * for the mode and the reward counters: this file supplies words, never values.
 */
import type { GameConfig } from './types';

/**
 * Ticker words for the adapter's nine reward kinds.
 *
 * Seven of them reach the page as one of the feed's published `RewardKind` counters and are worded
 * in `rewardCopy` below; the mapping is in `docs/feed-protocol.md`. `started` and `clear` map to no
 * counter, because each pays once in a lifetime, so the ticker shows the adapter's own event label
 * for them ("RUN STARTED", "GAME CLEARED"). This record is the one place all nine words live, so a
 * reader can see the catalogue in one glance; the two unmapped entries are documentation until the
 * feed grows a counter for them.
 */
export const PLATFORMER_REWARD_WORDS: Record<string, string> = {
  started: 'run started',
  band: 'new ground',
  coin: 'coin',
  score: 'points',
  powerup: 'power-up',
  life: '1UP',
  level: 'level cleared',
  world: 'world cleared',
  clear: 'game clear',
};

export const platformer: GameConfig = {
  id: 'platformer',
  // 34 characters, four over the ~30 the title strip fits at 18 px (see `GameConfig.wordmark`), so
  // the strip measures this one at 16 px. The game's name is not negotiable and abbreviating it
  // ("SUPER MARIO") would read as a different game.
  wordmark: 'A FLY BRAIN PLAYS SUPER MARIO LAND',
  name: 'Super Mario Land',

  // The adapter's ladder, in the page's lower case. Rungs 4 to 14 are "4 + highest cleared level",
  // and the boss names come from the design, which took them from Mario Wiki.
  milestoneLadder: [
    'booting',
    'started a run',
    'found a coin',
    'halfway through 1-1',
    'cleared 1-1',
    'cleared 1-2',
    'cleared world 1',
    'cleared 2-1',
    'cleared 2-2',
    'cleared world 2',
    'cleared 3-1',
    'cleared 3-2',
    'cleared world 3',
    'cleared 4-1',
    'cleared 4-2',
    'finished the game',
  ],

  // Short state words, not sentences: these read as a badge in the title strip. The adapter's five
  // modes fold onto the feed's closed set (`docs/feed-protocol.md`): `IN LEVEL <world>-<stage>` is
  // OVERWORLD, and GAME OVER is TRANSITION -- which is why the TRANSITION word has to cover a level
  // load, a death, a pause and a game over at once. BATTLE and SAFARI are unreachable for this
  // adapter; they are here because the mode set is closed.
  modeLabels: {
    BOOT: 'booting',
    OVERWORLD: 'in the level',
    BATTLE: 'boss',
    TRANSITION: 'not in play',
    DEMO: 'attract demo',
    SAFARI: 'bonus game',
    UNKNOWN: 'unknown',
  },

  // Ticker rows are noun phrases, so a row reads as a log line rather than as narration. The keys
  // are the feed's counters; the adapter kind each one carries is named in the comment.
  rewardCopy: {
    explore: { label: 'new ground', tier: 'quiet', dedupeMs: 20_000, collapsedNoun: 'new ground' }, // band
    wildwin: { label: 'coin', tier: 'quiet', dedupeMs: 15_000, collapsedNoun: 'coins' }, // coin
    area: { label: 'points', tier: 'quiet', dedupeMs: 10_000, collapsedNoun: 'points' }, // score
    pokedex: { label: 'power-up', tier: 'notable', dedupeMs: 0 }, // powerup
    trainer: { label: '1UP', tier: 'notable', dedupeMs: 0 }, // life
    story: { label: 'level cleared', tier: 'notable', dedupeMs: 0 }, // level
    badge: { label: 'world cleared', tier: 'moment', dedupeMs: 0 }, // world
  },

  // `badges` carries the adapter's headline counter, which for this game is lives, so there is no
  // denominator: a 1UP can push it past three. `uniqueLocations` is the band ledger, i.e. ten-column
  // stretches of ground the fly has been paid for.
  counters: [
    { field: 'badges', label: 'lives' },
    { field: 'uniqueLocations', label: 'ground' },
  ],

  // Ranks are hours apart here -- a level clear is a rare event for a fly at 250 ms per decision --
  // so the alarm sits where the Pokémon demo's does in spirit rather than in value. The design's
  // 5-minute "time since the last new band" dial is a different measurement, and the feed carries no
  // field for it yet; `milestone.sinceSeconds` is what this threshold reads.
  stuckAlarmSeconds: 2 * 3600,
};

export default platformer;
