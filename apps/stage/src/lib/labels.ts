/**
 * The single source of human copy for the connectome itself.
 *
 * This table is **dataset-level, not game-level**: it is the same fly in both demos, so the
 * role-to-label mapping lives here and the per-game copy lives in `src/games/*`. Nothing in this
 * file may name a game.
 *
 * The authority for the role keys is `data/fafb-v783/meta.json` (`roles`) plus its
 * `circuit-roles.json` sidecar. `tests/unit/labels.test.ts` asserts every role key in those files
 * has an entry here, so a dataset rebuild cannot silently drop a bar.
 *
 * **Register** (from the 2026-09-15 copy pass): terse and instrument-like. Panel titles are
 * short nouns. No parenthetical justifications, no sentences under widgets, no captions. Every
 * explanation this page makes lives in `ROTATING_CARDS` and nowhere else — one short line each.
 *
 * Neuron counts are deliberately absent: they are read from the loaded `meta.json` at runtime and
 * rendered from there, so the count on screen cannot drift from the dataset.
 */

import { MACRO_TYPES, macroChannel, macroRateRole } from '@flybrain/feed';

/** Which colour token a bar or dot uses. */
export type CircuitTint = 'sensory' | 'motor' | 'dopamine' | 'ink-2';

/** Where a role surfaces on screen. */
export type RoleSurface = 'bar' | 'retina' | 'map';

/**
 * The 31 macro channels (`docs/design/macros.md` sections 12 to 14), labelled with their tags.
 *
 * Derived from the contract rather than typed out again: the role is `macro_` plus the type's name
 * lowercased, and the label is the tag the strip's cell already draws, so a type added to the
 * contract cannot arrive here as an unlabelled bar. Motor, because a macro is a button.
 */
const MACRO_ROLE_LABELS: Record<string, RoleLabel> = Object.fromEntries(
  MACRO_TYPES.map((name) => [
    macroRateRole(name),
    { label: macroChannel(name), surface: 'bar', tint: 'motor' } satisfies RoleLabel,
  ]),
);

/** Human copy for one dataset role. */
export interface RoleLabel {
  /** Short on-screen name. Lower case; the panel uppercases what it renders. */
  label: string;
  /** Where this role is rendered. `map` roles only tint the brain map. */
  surface: RoleSurface;
  tint: CircuitTint;
}

/**
 * Every role key in the dataset.
 *
 * `command_0..7` are the readout's eight output channels, in `GAMEBOY_BUTTON_BITS` order
 * (`packages/brain/src/readout/presets/gameboy.ts`): up, down, left, right, A, B, Start, Select.
 * `macro_*` are the 31 macro channels above.
 */
export const ROLE_LABELS: Record<string, RoleLabel> = {
  ...MACRO_ROLE_LABELS,
  command_0: { label: 'up', surface: 'bar', tint: 'motor' },
  command_1: { label: 'down', surface: 'bar', tint: 'motor' },
  command_2: { label: 'left', surface: 'bar', tint: 'motor' },
  command_3: { label: 'right', surface: 'bar', tint: 'motor' },
  command_4: { label: 'A', surface: 'bar', tint: 'motor' },
  command_5: { label: 'B', surface: 'bar', tint: 'motor' },
  command_6: { label: 'Start', surface: 'bar', tint: 'motor' },
  command_7: { label: 'Select', surface: 'bar', tint: 'motor' },

  reward_pam: { label: 'PAM', surface: 'bar', tint: 'dopamine' },
  proboscis: { label: 'proboscis', surface: 'bar', tint: 'sensory' },

  forward: { label: 'fwd', surface: 'bar', tint: 'ink-2' },
  backward: { label: 'back', surface: 'bar', tint: 'ink-2' },
  steer_left: { label: 'left', surface: 'bar', tint: 'ink-2' },
  steer_right: { label: 'right', surface: 'bar', tint: 'ink-2' },

  visual_l1: { label: 'L1', surface: 'retina', tint: 'sensory' },

  sensory: { label: 'sensory', surface: 'map', tint: 'sensory' },
  motor: { label: 'motor', surface: 'map', tint: 'motor' },
  descending: { label: 'descending', surface: 'map', tint: 'motor' },
  kenyon: { label: 'kenyon', surface: 'map', tint: 'ink-2' },
  mbon: { label: 'mbon', surface: 'map', tint: 'ink-2' },
};

/** One sub-bar inside a circuit group. */
export interface CircuitBar {
  /** Feed `rates` key. */
  role: string;
  /** Short label as rendered. */
  label: string;
}

/** One labelled group in the CIRCUITS panel. */
export interface CircuitGroup {
  id: string;
  /** Group name, uppercased on screen. A noun, with nothing after it. */
  label: string;
  tint: CircuitTint;
  /** Full-scale rate for the bars, Hz. */
  fullScaleHz: number;
  bars: readonly CircuitBar[];
}

/**
 * The six groups of the CIRCUITS panel, top to bottom (design A2: 6 groups).
 *
 * A3's table lists PRESS A and PRESS B as separate groups, which would make seven; they share one
 * group with two sub-bars here so the panel matches A2's count and pitch.
 */
export const CIRCUIT_GROUPS: readonly CircuitGroup[] = [
  {
    id: 'drive',
    label: 'drive',
    tint: 'motor',
    fullScaleHz: 30,
    bars: [
      { role: 'command_0', label: 'up' },
      { role: 'command_1', label: 'down' },
      { role: 'command_2', label: 'left' },
      { role: 'command_3', label: 'right' },
    ],
  },
  {
    id: 'press',
    label: 'A / B',
    tint: 'motor',
    fullScaleHz: 30,
    bars: [
      { role: 'command_4', label: 'A' },
      { role: 'command_5', label: 'B' },
    ],
  },
  {
    id: 'menu',
    label: 'menu',
    tint: 'motor',
    fullScaleHz: 20,
    bars: [
      { role: 'command_6', label: 'Start' },
      { role: 'command_7', label: 'Select' },
    ],
  },
  {
    id: 'dopamine',
    label: 'dopamine',
    tint: 'dopamine',
    fullScaleHz: 25,
    bars: [{ role: 'reward_pam', label: 'PAM' }],
  },
  {
    id: 'taste',
    label: 'taste',
    tint: 'sensory',
    fullScaleHz: 10,
    bars: [{ role: 'proboscis', label: 'proboscis' }],
  },
  {
    /**
     * The legs are real and drive nothing. That used to be said on a line under the panel ("legs:
     * real, wired to nothing"), which is exactly the explanatory micro-copy the copy direction
     * rejects; the fact now lives in the scaffolding card, and the row is just LEGS.
     */
    id: 'legs',
    label: 'legs',
    tint: 'ink-2',
    fullScaleHz: 12,
    bars: [
      { role: 'forward', label: 'fwd' },
      { role: 'backward', label: 'back' },
      { role: 'steer_left', label: 'left' },
      { role: 'steer_right', label: 'right' },
    ],
  },
];

/**
 * The MACROS row in the SENSES panel (`docs/design/macros.md` section 12), beside DRIVE and A/B.
 *
 * Not one of {@link CIRCUIT_GROUPS}, because its bars are not fixed: the row draws the channels the
 * scene has bound right now, in the palette's own cell order, and it draws nothing in raw mode.
 * The full scale is the direction group's, since these are decided against the same thresholds.
 */
export const MACRO_CIRCUIT = { id: 'macros', label: 'macros', tint: 'motor', fullScaleHz: 30 } as const;

/** Every macro channel's rate role, in the contract's type order. */
export const MACRO_BAR_ROLES: readonly string[] = MACRO_TYPES.map(macroRateRole);

/** Every role a circuit bar reads, in render order. */
export const BAR_ROLES: readonly string[] = CIRCUIT_GROUPS.flatMap((group) => group.bars.map((bar) => bar.role));

/**
 * Copy that describes the whole apparatus rather than any one game.
 *
 * All strings are constants: the page never generates or interpolates prose (the "no generated
 * text on a Twitch stream" rule from the Nothing, Forever precedent).
 */
export const DATASET_COPY = {
  hzUnit: 'Hz',
  retinaTitle: 'retina',
  circuitsTitle: 'circuits',
  eventsTitle: 'events',
  hereForTitle: 'here for',
  /** The whole-brain rate's label, over the Hz readout in the progress cluster. */
  brainTitle: 'brain',
  /** The persistent chat panel's title. Rendered only when there are lines to show. */
  chatTitle: 'chat',
  /** LADDER tab's stats column: the rollback budget lines and the stall meter. */
  rollbacksTitle: 'rollbacks',
  lifetimeTitle: 'lifetime',
  lastTitle: 'last',
  stallTitle: 'stall',
  agoSuffix: 'ago',
  /** The run clock's day counter, beside the clock rather than inside it. See `readouts.ts`. */
  dayPrefix: 'day',
  sugarReady: 'SUGAR READY',
  /** `{name}` is a feed-supplied display name, re-validated on render. */
  sugarBy: 'SUGAR by {name}',
  mapTitle: 'connectome',
  staleBanner: 'STALE FEED',
  feedDown: 'FEED DOWN',
  simError: 'SIM ERROR',
  emptyTicker: 'no events yet',
} as const;

/**
 * The rotating card: the one place on the page that explains anything.
 *
 * Four topics, one short line each — what is real, what is scaffolding, what sugar does, and the
 * dataset credit with its licence. One card takes the narrative lane for the last minute of every
 * four-minute cycle (`src/lib/schedule.ts`), and the next cycle shows the next card.
 *
 * Everything the page used to say in captions under widgets is in here, or nowhere.
 */
export const ROTATING_CARDS: readonly { title: string; line: string }[] = [
  {
    title: 'real',
    line: 'Measured wiring, simulated spikes, reward-modulated plasticity on real synapses.',
  },
  {
    title: 'scaffolding',
    line: 'Our mapping: eyes to the screen, eight command groups to buttons. The legs drive nothing.',
  },
  {
    title: 'sugar',
    line: 'Sugar fires one dopamine pulse. There is no path from chat to a button.',
  },
  {
    title: 'credit',
    line: 'FlyWire FAFB v783 · CC BY-NC 4.0 · 139,255 neurons, 2.7 million connections.',
  },
];
