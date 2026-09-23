/**
 * Per-game config for the first demo.
 *
 * This is the only file in the app that names this game. The feed header stays authoritative for
 * the *current* rank's label and for how many rungs exist (`milestone.total`), so a ratchet change
 * in the service shows up on screen without a page release.
 *
 * The full 38 labels are here anyway, and that is a decision rather than an oversight: the header
 * carries `rank`, `label`, `next` and `total` but **not** the ladder's other 37 names, and the
 * LADDER tab's whole job is to spell the ladder out — the visible long-horizon goal the research
 * says holds an audience for months. So the list is transcribed from `docs/design/ladder.md`'s
 * table, index for index with the service's own ratchet, and `tests/unit/rungs.test.ts` pins its
 * length at the header's `total` for this game. The drift risk is real and is paid for with a test
 * rather than by leaving the tab blank.
 */
import type { GameConfig } from './types';

export const pokemonRed: GameConfig = {
  id: 'pokemon-red',
  // Unaccented on purpose: Press Start 2P draws É at x-height, so "POKÉMON" reads as "POKéMON" —
  // smaller and off next to the surrounding caps. `apps/stage/README.md` records the glyph check.
  wordmark: 'A FLY BRAIN PLAYS POKEMON RED',
  name: 'Pokémon Red',

  // 38 rungs, 0..37, transcribed from `docs/design/ladder.md`'s table. Short on purpose: they
  // render three to a row in a 1008 px pane, and the current one also shares one 30 px mono line
  // with the next rung in the progress cluster.
  milestoneLadder: [
    'Boot screen',
    'Bedroom',
    'Downstairs',
    'Pallet Town',
    "Oak's lab",
    'Got a starter',
    "Oak's parcel",
    'Pokédex',
    'Viridian City',
    'Viridian Forest',
    'Pewter City',
    'Boulder Badge',
    'Mt. Moon',
    'Cerulean City',
    'Cascade Badge',
    'Nugget Bridge',
    'Met Bill',
    'Vermilion City',
    'HM Cut',
    'Thunder Badge',
    'Rock Tunnel',
    'Lavender Town',
    'Celadon City',
    'Silph Scope',
    'Rainbow Badge',
    'Poké Flute',
    'Fuchsia City',
    'Soul Badge',
    'Silph Co. freed',
    'Marsh Badge',
    'Cinnabar Island',
    'Volcano Badge',
    'Earth Badge',
    'Indigo Plateau',
    'Beat Lorelei',
    'Beat Bruno',
    'Beat Agatha',
    'Champion',
  ],

  // Short state words, not sentences: these read as a badge in the title strip. `UNKNOWN` has no
  // real word — `TitleStrip` hides the chip entirely rather than show a placeholder.
  modeLabels: {
    BOOT: 'boot',
    OVERWORLD: 'walking',
    BATTLE: 'battle',
    TRANSITION: 'menu',
    DEMO: 'demo',
    SAFARI: 'safari zone',
    UNKNOWN: 'unknown',
  },

  // Ticker rows are noun phrases, so a row reads as a log line rather than as narration.
  rewardCopy: {
    explore: { label: 'new place', tier: 'quiet', dedupeMs: 20_000, collapsedNoun: 'new places' },
    area: { label: 'new area', tier: 'notable', dedupeMs: 20_000, collapsedNoun: 'new areas' },
    wildwin: { label: 'wild win', tier: 'quiet', dedupeMs: 20_000, collapsedNoun: 'wild wins' },
    trainer: { label: 'trainer beaten', tier: 'notable', dedupeMs: 0 },
    pokedex: { label: 'new Pokédex entry', tier: 'notable', dedupeMs: 0 },
    story: { label: 'story', tier: 'notable', dedupeMs: 0 },
    badge: { label: 'gym badge', tier: 'moment', dedupeMs: 0 },
  },

  counters: [
    { field: 'badges', label: 'badges', outOf: 8 },
    { field: 'uniqueLocations', label: 'places' },
  ],

  stuckAlarmSeconds: 6 * 3600,
};

export default pokemonRed;
