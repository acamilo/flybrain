/**
 * The fake flysim's scene and macro machine.
 *
 * `docs/design/macros.md` sections 3 and 12 are the tables this file implements, row for row: the
 * macros each scene puts on the pad, with a two or three word gloss added for the screen
 * (section 6: 'each with the macro's name and a two or three word gloss of what it does in this
 * scene ("GO EXIT · nearest door")'). The glosses are this repository's copy, not the contract's,
 * and they are capped at ten characters, which is what the macro cell's gloss column holds once
 * the longest name — `GO OBJECTIVE`, twelve characters of Silkscreen — has taken its share of the
 * row. `apps/stage/src/lib/geometry.ts` has the rest of that arithmetic.
 *
 * Two modes, because the service has two (`packages/feed/src/types.ts`). In `raw` nothing is bound
 * at all. In `macros` each macro type is a button with a channel of its own and the scene decides
 * which of them exist: the rows below are those sets, in the order the wire's six slots are filled
 * from, and section 9.1's indoor/outdoor split decides which overworld set is dealt.
 *
 * What it does *not* do is decide anything for the fly in a way the real sim would not: a macro is
 * chosen from the *bound* macros of the current scene and nothing else, a scene change aborts the
 * macro running under it, and a macro whose precondition fails is simply absent. The one thing it
 * cannot be faithful about is the button script, because there is no emulator here: the button
 * mask keeps coming from the raw generator while a macro runs, which is why `simulator.ts` says so
 * where it wires this in.
 */
import type { GameScene, MacroMode, MacroOutcomeKind, PaletteEntry } from '../types';
import { macroChannel, macroTypeIndex } from '../types';
import { chance, pick, randInt, randRange, type Rng } from './prng';

/** One macro a scene can bind. */
interface Slot {
  name: string;
  gloss: string;
  /**
   * A macro the contract binds only when a precondition holds: `ITEM` ("potion if own HP < 50%
   * and one is held"), `RUN` ("wild only"), `BUY POTION` / `BUY BALL` ("if money allows"), `TALK`
   * ("only when facing something untalked") and `GO OBJECTIVE`, which is on the pad only where the
   * rung catalog knows a place for the next rung (section 12). The fake rolls each one once per
   * scene entry, so the screen shows real absent cells and real scenes of different depths.
   */
  conditional?: boolean;
}

const slot = (name: string, gloss: string, conditional = false): Slot =>
  conditional ? { name, gloss, conditional } : { name, gloss };

/** The macros, named once so no two scene sets can drift apart. */
const GO_OBJECTIVE = slot('GO OBJECTIVE', 'next rung', true);
const GO_OUT = slot('GO OUT', 'leave here');
const GO_WARP = slot('GO WARP', 'stairs/door');
const GO_ROUTE = slot('GO ROUTE', 'next area');
const GO_ITEM = slot('GO ITEM', 'an object');
const GO_NPC = slot('GO NPC', 'a person');
const GO_FRONTIER = slot('GO FRONTIER', 'new ground');
const TALK = slot('TALK', 'press A', true);
const MENU = slot('MENU', 'open menu');
const NEXT = slot('NEXT', 'advance it');
const YES = slot('YES', 'answer yes');
const NO = slot('NO', 'answer no');
const CLOSE = slot('CLOSE', 'to the map');
const CONFIRM = slot('CONFIRM', 'take it');
const BACK = slot('BACK', 'step back');
/** Section 14: one button per move slot, each bound only when that slot has a move with PP. */
const MOVE_1 = slot('MOVE 1', 'first move');
const MOVE_2 = slot('MOVE 2', 'second', true);
const MOVE_3 = slot('MOVE 3', 'third', true);
const MOVE_4 = slot('MOVE 4', 'fourth', true);
const SWITCH = slot('SWITCH', 'healthiest');
const ITEM = slot('ITEM', 'a potion', true);
const THROW_BALL = slot('THROW BALL', 'a ball', true);
const RUN = slot('RUN', 'losing wild', true);
/** Section 13's errands, each on the pad only while this area's is outstanding. */
const GO_SHOP = slot('GO SHOP', 'the mart', true);
const GO_HEAL = slot('GO HEAL', 'the centre', true);
/** Section 13's centre: the conversation at the nurse's counter. */
const HEAL = slot('HEAL', 'rest party', true);
const BUY_POTION = slot('BUY POTION', 'one potion', true);
const BUY_BALL = slot('BUY BALL', 'one ball', true);
const BUY_ANTIDOTE = slot('BUY ANTIDOTE', 'one cure', true);
const BUY_REPEL = slot('BUY REPEL', 'one repel', true);
const LEAVE_SHOP = slot('LEAVE', 'out of it');
const LEAVE_PC = slot('LEAVE', 'close it');

/**
 * `docs/design/macros.md` section 12, scene by scene: the macros on the pad, in the order the
 * wire's slots are filled from.
 *
 * The order is the fake's, not the contract's — the contract names a set, and the display sorts by
 * type anyway — but it has to exist, because the wire carries six macros (`slot` is 0..5) and the
 * overworld can want eight. A scene that overflows binds the first six of its order, and what goes
 * last is `GO FRONTIER`: it is the fallback for a map with nothing else to do, so the only scenes
 * that need it are the ones with room for it.
 *
 * The overworld row here is the *outdoor* one; {@link FAKE_INDOOR_OVERWORLD} is the other half of
 * section 9.1's split, and the fake alternates between them as the scene walk re-enters the
 * overworld so that both shapes reach the screen.
 */
export const FAKE_MACRO_TABLE: Record<GameScene, readonly Slot[]> = {
  // Nothing is bound on the title screen: the boot variant of the readout applies.
  title: [],
  overworld: [GO_OBJECTIVE, GO_ROUTE, GO_SHOP, GO_HEAL, TALK, GO_ITEM, GO_NPC, GO_FRONTIER, MENU],
  // `YES` and `NO` are added when a choice is open; see `FakePalette.choice`.
  dialog: [NEXT],
  menu: [CLOSE, CONFIRM, BACK],
  // Section 14's own turn: four move buttons where `ATTACK` was, plus section 14's `THROW BALL`
  // and section 13.1's `RUN`, which is on the pad only in a wild battle the fly is losing.
  battle: [MOVE_1, MOVE_2, MOVE_3, MOVE_4, SWITCH, ITEM, THROW_BALL, RUN, NEXT],
  'battle-switch': [SWITCH, NEXT],
  shop: [BUY_POTION, BUY_BALL, BUY_ANTIDOTE, BUY_REPEL, CONFIRM, LEAVE_SHOP],
  pc: [CONFIRM, LEAVE_PC],
  // Detection failed: treated like Dialog, advance only, plus the B that leaves the three screens
  // that land here (section 13.1, row 9).
  unknown: [NEXT, BACK],
};

/** Section 9.1's indoor overworld: the door and the passages instead of the route. */
export const FAKE_INDOOR_OVERWORLD: readonly Slot[] = [
  GO_OBJECTIVE,
  GO_OUT,
  GO_WARP,
  GO_SHOP,
  GO_HEAL,
  TALK,
  GO_ITEM,
  GO_NPC,
  GO_FRONTIER,
  MENU,
];

/**
 * Section 13's centre: the indoor overworld inside a Pokémon Center, where `HEAL` is on the pad.
 *
 * A centre is a sub-state of the overworld rather than a scene of its own, exactly as it is in the
 * real palette: pokered has no "a Pokémon Center is open" byte, so the only honest observable is
 * the map id. `GO WARP` is absent because a centre is one floor with nothing to warp to, and
 * `GO HEAL` is absent because the fly is standing in the building that errand asks for.
 */
export const FAKE_CENTER_OVERWORLD: readonly Slot[] = [
  GO_OBJECTIVE,
  GO_OUT,
  HEAL,
  GO_SHOP,
  TALK,
  GO_ITEM,
  GO_NPC,
  GO_FRONTIER,
  MENU,
];

/**
 * A scene the `shop` and `center` scenarios pin the walk to.
 *
 * `center` is not a `GameScene`: section 13's centre is a sub-state of the overworld, on the wire
 * as on the cartridge, so pinning it means pinning `overworld` *and* the place inside it.
 */
export type PinnedScene = 'shop' | 'center';

/** Which of section 9.1's and 13's three overworlds the fly is standing in. */
export type OverworldPlace = 'outdoors' | 'indoors' | 'center';

const OVERWORLD_SETS: Record<OverworldPlace, readonly Slot[]> = {
  outdoors: FAKE_MACRO_TABLE.overworld ?? [],
  indoors: FAKE_INDOOR_OVERWORLD,
  center: FAKE_CENTER_OVERWORLD,
};

/**
 * The fake's widest outdoor pad: every macro the outdoors ever binds at once.
 *
 * `--full-pad` records with this so the section-14 strip's second column and the MACROS tab's lit
 * cells have something to regress; without it the random walk is three to nine and never the same
 * twice. The `bigpad` fixture is the one recording.
 */
export const OUTDOOR_MACROS: readonly Slot[] = FAKE_MACRO_TABLE.overworld ?? [];

/** The cycle the fake walks them in, so all three shapes reach the screen. */
const NEXT_PLACE: Record<OverworldPlace, OverworldPlace> = {
  outdoors: 'indoors',
  indoors: 'center',
  center: 'outdoors',
};

/** The two macros a dialog adds while a choice is open (section 12). */
export const FAKE_DIALOG_CHOICE: readonly Slot[] = [YES, NO];

/**
 * Where the scene walk can go next, by scene.
 *
 * A plausible loop rather than a uniform choice: the overworld is where a fly spends its time and
 * everything else returns to it, which is also what makes the pad on screen change *shape* (six
 * cells, then four, then one) rather than just changing words.
 */
const SCENE_NEXT: Record<GameScene, readonly GameScene[]> = {
  title: ['overworld'],
  // `unknown` is in the table above and in the schema, but deliberately not in the walk: it maps
  // to `game.mode: UNKNOWN`, which the page reads as "no honest word for this state" and hides the
  // mode chip for — not something to put in a review mockup on purpose.
  overworld: ['dialog', 'battle', 'menu', 'shop', 'pc', 'dialog', 'battle', 'menu'],
  dialog: ['overworld', 'overworld', 'battle'],
  menu: ['overworld'],
  battle: ['battle-switch', 'overworld', 'overworld', 'dialog'],
  'battle-switch': ['battle'],
  shop: ['overworld'],
  pc: ['overworld'],
  unknown: ['overworld'],
};

/** Scene dwell, per `docs/design/macros.md`'s stage note: a re-deal every 20 to 60 s. */
const SCENE_MS = { min: 20_000, max: 60_000 } as const;

/** How the outcomes are distributed. `done` dominates; the rest have to be visible on screen. */
const OUTCOMES: readonly { outcome: MacroOutcomeKind; weight: number }[] = [
  { outcome: 'done', weight: 70 },
  { outcome: 'blocked', weight: 14 },
  { outcome: 'refused', weight: 10 },
  { outcome: 'timeout', weight: 6 },
];

/** The hard cap from section 4: 600 frames. A `timeout` runs the whole way to it. */
export const MACRO_FRAME_CAP_MS = 10_000;

export interface MacroEvent {
  label: string;
  /** Slot the event is about, carried as the event's `value` so the log can group by it. */
  slot: number;
}

export interface PaletteState {
  scene: GameScene;
  entries: PaletteEntry[];
  macro: { slot: number; name: string; sinceMs: number } | null;
  outcome: { slot: number; name: string; outcome: MacroOutcomeKind; atMs: number } | null;
}

/**
 * Scene walk plus macro decisions, driven by the simulator's own clock and RNG.
 *
 * Stepped every tick whatever the mode is, because scene detection does not depend on the mode —
 * but it only ever *binds* a macro or starts one in macros mode, so a raw-mode run reports an
 * empty palette and a null macro exactly as the contract says.
 */
export class FakePalette {
  private scene: GameScene = 'overworld';
  private sceneUntilMs: number;
  private bound: PaletteEntry[] = [];
  /**
   * Which side of a door the fly is on, flipped on every entry into the overworld.
   *
   * There is no map here, so this is the one thing the fake invents rather than derives — and it
   * has to invent something, because section 9.1's overworld differs between an interior and a
   * town and a screen that only ever showed one of them would leave half the pad unreviewed.
   * Section 13 adds the third: a Pokémon Center, which is where `HEAL` is on the pad.
   */
  private place: OverworldPlace = 'outdoors';
  /** Whether the dialog on screen has a choice open, flipped on every entry into a dialog. */
  private choice = false;

  private running: { slot: number; name: string; startedMs: number; runMs: number; outcome: MacroOutcomeKind } | null =
    null;
  private outcome: { slot: number; name: string; outcome: MacroOutcomeKind; atMs: number } | null = null;
  private nextDecisionMs: number;

  /**
   * A scene the walk never leaves, for the `shop` and `center` scenarios.
   *
   * `docs/design/macros.md` section 13's screens are two of the three the random walk reaches
   * rarely and never for long, and the operator reviews screens as PNGs: a pinned scene is what makes a
   * mockup and a Playwright baseline of a mart counter or a nurse's pad reproducible at a fixed
   * seek time. Nothing else about the fake changes — the macro decisions, the outcomes and the
   * rates are the same code on the same RNG.
   */
  private readonly pin: GameScene | null;
  /** Which overworld a pinned `center` scenario claims. */
  private readonly pinnedPlace: OverworldPlace | null;

  constructor(
    private readonly rng: Rng,
    private readonly mode: MacroMode,
    startMs = 0,
    pin: PinnedScene | null = null,
    /**
     * Deal the fake's widest pad in every scene.
     *
     * The walk without it ranges from three to nine macros across an evening; with it, every
     * overworld binds all nine of the outdoor macros (`docs/design/macros.md` section 14), which
     * is what the `bigpad` fixture records and what the section-14 mockups regress.
     */
    private readonly fullPad = false,
  ) {
    this.pin = pin === null ? null : pin === 'center' ? 'overworld' : pin;
    this.pinnedPlace = pin === 'center' ? 'center' : null;
    if (this.pin !== null) this.scene = this.pin;
    if (this.pinnedPlace !== null) this.place = this.pinnedPlace;
    this.sceneUntilMs = startMs + randRange(this.rng, SCENE_MS.min, SCENE_MS.max);
    this.nextDecisionMs = startMs + randRange(this.rng, 800, 2_500);
    this.deal();
  }

  /**
   * The five header fields, as one object.
   *
   * Raw mode answers all five emptily, the scene included: `docs/feed-protocol.md` has it
   * `unknown` there, because "nothing is bound, so no scene is claimed". The walk below keeps
   * running anyway, so the mode is the only thing that changes when an operator flips it.
   */
  state(elapsedMs: number): PaletteState {
    if (this.mode === 'raw') {
      return { scene: 'unknown', entries: [], macro: null, outcome: null };
    }

    return {
      scene: this.scene,
      entries: this.bound.map((entry) => ({ ...entry })),
      macro: this.running
        ? {
            slot: this.running.slot,
            name: this.running.name,
            sinceMs: Math.max(0, Math.round(elapsedMs - this.running.startedMs)),
          }
        : null,
      outcome: this.outcome,
    };
  }

  /** The names bound right now, for the simulator's macro-channel rates. */
  boundNames(): string[] {
    return this.mode === 'raw' ? [] : this.bound.map((entry) => entry.name);
  }

  /** The name of the macro running right now, or null. */
  get runningName(): string | null {
    return this.running?.name ?? null;
  }

  /**
   * One tick. Returns the `macro` events this tick produced, oldest first.
   *
   * Order matters and is the sim's: a scene change aborts whatever was running *before* the new
   * scene's macros are bound, and a decision is only taken when nothing is running.
   */
  step(elapsedMs: number): MacroEvent[] {
    const events: MacroEvent[] = [];

    if (elapsedMs >= this.sceneUntilMs) {
      // "A scene change mid-macro (a wild battle starting during GO OUT) aborts the macro at the
      // next frame; the fly is consulted on the new scene's macros."
      if (this.running) events.push(this.finish(elapsedMs, 'blocked'));
      // A pinned scene stays put: the dwell expires, the macros are re-dealt, and the scene is
      // the one the scenario asked for. The RNG draw still happens, so a pinned run and a walking
      // run diverge in their macro choices rather than in their scene sequences.
      const next = pick(this.rng, [...(SCENE_NEXT[this.scene] ?? ['overworld'])]);
      this.scene = this.pin ?? next;
      if (this.scene === 'overworld') {
        this.place = this.pinnedPlace ?? NEXT_PLACE[this.place];
      }
      if (this.scene === 'dialog') this.choice = !this.choice;
      this.sceneUntilMs = elapsedMs + randRange(this.rng, SCENE_MS.min, SCENE_MS.max);
      this.deal();
      this.nextDecisionMs = elapsedMs + randRange(this.rng, 600, 1_800);
    }

    if (this.mode === 'raw') return events;

    if (this.running) {
      if (elapsedMs - this.running.startedMs >= this.running.runMs) {
        events.push(this.finish(elapsedMs, this.running.outcome));
        this.nextDecisionMs = elapsedMs + randRange(this.rng, 900, 2_600);
      }
      return events;
    }

    if (elapsedMs >= this.nextDecisionMs) {
      if (this.bound.length === 0) {
        // "If the fly's channels are silent, the game waits." So does a scene with no macros.
        this.nextDecisionMs = elapsedMs + randRange(this.rng, 1_000, 2_000);
        return events;
      }
      const entry = this.bound[randInt(this.rng, 0, this.bound.length - 1)] as PaletteEntry;
      const outcome = this.rollOutcome();

      if (outcome === 'refused') {
        // A refusal presses nothing, so there is no start event and no running macro: the fly
        // chose a channel whose precondition had gone stale, and the sim said no. It is still the
        // outcome of the fly's last choice, so it replaces the one on screen.
        this.outcome = { slot: entry.slot, name: entry.name, outcome, atMs: elapsedMs };
        this.nextDecisionMs = elapsedMs + randRange(this.rng, 700, 1_800);
        events.push({ label: `${entry.name} refused`, slot: entry.slot });
        return events;
      }

      const runMs =
        outcome === 'timeout' ? MACRO_FRAME_CAP_MS : randRange(this.rng, 700, outcome === 'blocked' ? 2_200 : 3_600);
      // `docs/feed-protocol.md`: the outcome is "kept until the next one starts", so the page can
      // light a cell's result for a beat rather than racing a single frame.
      this.outcome = null;
      this.running = { slot: entry.slot, name: entry.name, startedMs: elapsedMs, runMs, outcome };
      events.push({ label: `${entry.name} start`, slot: entry.slot });
    }

    return events;
  }

  /** Slot the running macro occupies, for the event log. */
  get runningSlot(): number | null {
    return this.running?.slot ?? null;
  }

  private finish(elapsedMs: number, outcome: MacroOutcomeKind): MacroEvent {
    const running = this.running as { slot: number; name: string };
    this.running = null;
    this.outcome = { slot: running.slot, name: running.name, outcome, atMs: elapsedMs };
    return { label: `${running.name} ${outcome}`, slot: running.slot };
  }

  /**
   * Bind the scene's macros, rolling each conditional one once.
   *
   * A macro whose precondition fails is simply not on the pad (section 12) and is absent from
   * `palette` entirely; the ones that hold keep their type index as `slot` (section 14), so a
   * cell's place on screen does not depend on which of its neighbours happen to be bound.
   */
  private deal(): void {
    this.bound = [];
    const set =
      this.fullPad && this.scene === 'overworld'
        ? OUTDOOR_MACROS
        : this.scene === 'overworld'
          ? OVERWORLD_SETS[this.place]
          : this.scene === 'dialog' && this.choice
            ? [...(FAKE_MACRO_TABLE.dialog ?? []), ...FAKE_DIALOG_CHOICE]
            : (FAKE_MACRO_TABLE[this.scene] ?? []);

    for (const cell of set) {
      if (cell.conditional && !chance(this.rng, 0.55)) continue;
      this.bound.push({
        // Section 14: a slot is the macro type's own index, so a cell never moves and nothing is
        // truncated off a pad. It was the position among the bound ones until then, which is why
        // the strip's cells used to shuffle as preconditions came and went.
        slot: macroTypeIndex(cell.name),
        name: cell.name,
        gloss: cell.gloss,
        channel: macroChannel(cell.name),
      });
    }
    this.bound.sort((a, b) => a.slot - b.slot);
  }

  private rollOutcome(): MacroOutcomeKind {
    const total = OUTCOMES.reduce((sum, row) => sum + row.weight, 0);
    let roll = this.rng() * total;
    for (const row of OUTCOMES) {
      if (roll < row.weight) return row.outcome;
      roll -= row.weight;
    }
    return 'done';
  }
}

/** `game.mode` for a scene, so the closed set and the scene never disagree on screen. */
export function gameModeForScene(scene: GameScene): 'BOOT' | 'OVERWORLD' | 'BATTLE' | 'UNKNOWN' {
  if (scene === 'title') return 'BOOT';
  if (scene === 'battle' || scene === 'battle-switch') return 'BATTLE';
  if (scene === 'unknown') return 'UNKNOWN';
  return 'OVERWORLD';
}
