/**
 * The scene's macros on the wire: the header fields, the reader, and the fake sim that produces
 * them (`docs/design/macros.md` sections 6 and 12).
 *
 * What is being pinned here is mostly what *stays* true when the fields are absent or wrong,
 * because the page these feed is a broadcast that runs for weeks: a raw-mode producer, an older
 * producer with none of the five fields, and a producer that contradicts itself all have to leave
 * the display with six rows and nothing lit.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import Ajv2020 from 'ajv/dist/2020.js';

import { decodeSnapshot, encodeSnapshot } from '../src/codec';
import { paletteView } from '../src/palette';
import {
  MACRO_CHANNELS,
  MACRO_SLOTS,
  MACRO_TYPES,
  macroChannel,
  macroRateRole,
  macroTypeIndex,
  type FeedHeader,
  type MacroOutcomeKind,
} from '../src/types';
import {
  FAKE_CENTER_OVERWORLD,
  FAKE_DIALOG_CHOICE,
  FAKE_INDOOR_OVERWORLD,
  FAKE_MACRO_TABLE,
  MACRO_FRAME_CAP_MS,
} from '../src/fake/palette';
import { FakeFlysim } from '../src/fake/simulator';

const here = fileURLToPath(new URL('.', import.meta.url));
const schema = JSON.parse(readFileSync(join(here, '../src/schema.json'), 'utf8')) as object;
const validate = new Ajv2020({ strict: true }).compile(schema);

/** A minimal macros-mode header, hand-built so the shape is visible in the test. */
function macroHeader(): FeedHeader {
  return {
    protocol: 1,
    seq: 7,
    wallMs: 1_700_000_000_000,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: 12,
    runSeconds: 12,
    brainMs: 12_000,
    frame: 360,
    buttons: 1,
    rates: { command_0: 42, macro_go_out: 31 },
    populationRate: 13,
    spikeCount: 0,
    learning: { enabled: true, updates: 1, changed: 0, synapses: 4_200_000, signal: 0 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 3,
      rewardTotal: 0.1,
      rewardCounts: { story: 0, explore: 1, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
      scene: 'overworld',
      macroMode: 'macros',
      palette: [
        { slot: 0, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' },
        { slot: 1, name: 'TALK', gloss: 'press A', channel: 'MB·TALK' },
      ],
      macro: { slot: 0, name: 'GO OUT', sinceMs: 420 },
      macroOutcome: { slot: 1, name: 'TALK', outcome: 'done', atMs: 11_000 },
    },
    milestone: { rank: 2, label: 'Left the house', next: 'Reached Pallet Town', sinceSeconds: 30, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
    events: [
      { id: 9, wallMs: 1_700_000_000_000, brainMs: 11_580, kind: 'macro', label: 'GO OUT start', value: 0 },
      { id: 8, wallMs: 1_699_999_999_000, brainMs: 11_000, kind: 'macro', label: 'TALK done', value: 1 },
    ],
    attachments: [],
  };
}

test('a macros-mode header round trips through the codec and validates', () => {
  const header = macroHeader();
  const decoded = decodeSnapshot(encodeSnapshot(header, {}));

  assert.deepEqual(decoded.header, header);
  assert.ok(validate(header), `macros header failed the schema: ${JSON.stringify(validate.errors)}`);
});

test('the 31 types and their channels are one table, in the contract\'s order', () => {
  // Sections 12, 13 and 14's list, which is also the order a display lays the cells out in and,
  // since section 14, the `slot` index itself.
  assert.equal(MACRO_TYPES.length, 31);
  assert.equal(MACRO_TYPES.length, MACRO_SLOTS, 'a slot per type');
  assert.equal(MACRO_CHANNELS.length, MACRO_TYPES.length);
  assert.deepEqual(MACRO_TYPES.slice(0, 3), ['GO OBJECTIVE', 'GO OUT', 'GO WARP']);
  assert.deepEqual(MACRO_CHANNELS.slice(0, 3), ['MB·GOAL', 'MB·OUT', 'MB·WARP']);
  assert.equal(MACRO_TYPES.at(-1), 'LEAVE');
  assert.equal(MACRO_CHANNELS.at(-1), 'MB·LEAVE');

  for (const [index, name] of MACRO_TYPES.entries()) {
    assert.equal(macroTypeIndex(name), index, `${name} is not at ${index}`);
    assert.equal(macroChannel(name), MACRO_CHANNELS[index]);
    // A tag has to fit a cell glyph and a bar label, and it is written with an interpunct.
    assert.ok((MACRO_CHANNELS[index] as string).length <= 8, `${MACRO_CHANNELS[index]} is too long for a cell`);
    assert.match(MACRO_CHANNELS[index] as string, /^MB·[A-Z0-9]+$/);
  }
  assert.equal(new Set(MACRO_CHANNELS).size, MACRO_CHANNELS.length, 'two types share a channel');
  assert.equal(macroTypeIndex('SOMERSAULT'), -1);
  assert.equal(macroChannel('SOMERSAULT'), '');
});

test('a type\'s rate role is its name lowercased, spaces to underscores', () => {
  // The rule the page finds a bound macro's rate in `brain.rates` by, and the rule the dataset
  // generated the roles with (`tools/build_flywire.py`).
  assert.equal(macroRateRole('GO OUT'), 'macro_go_out');
  assert.equal(macroRateRole('BUY POTION'), 'macro_buy_potion');
  assert.equal(macroRateRole('MOVE 1'), 'macro_move_1');
  assert.equal(macroRateRole('THROW BALL'), 'macro_throw_ball');
  const roles = MACRO_TYPES.map(macroRateRole);
  assert.equal(new Set(roles).size, roles.length, 'two types share a rate role');
  for (const role of roles) assert.match(role, /^macro_[a-z0-9_]+$/);
});

test('the five macro fields are optional, so an older producer and every old fixture stay valid', () => {
  const header = macroHeader();
  const { scene, macroMode, palette, macro, macroOutcome, ...game } = header.game;
  void scene;
  void macroMode;
  void palette;
  void macro;
  void macroOutcome;

  const older: FeedHeader = { ...header, game, events: [] };
  assert.ok(validate(older), `a header without the macro fields failed: ${JSON.stringify(validate.errors)}`);

  // And the reader is total over it: one row per type, nothing bound, nothing running, raw mode.
  // Section 14: a dim cell still carries its type's tag, because the strip draws every button.
  const view = paletteView(older);
  assert.equal(view.mode, 'raw');
  assert.equal(view.supported, false);
  assert.equal(view.cells.length, MACRO_SLOTS);
  assert.deepEqual(
    view.cells.map((cell) => cell.row),
    MACRO_TYPES.map((_, index) => index),
  );
  assert.ok(
    view.cells.every((cell) => cell.entry === null),
    'nothing is bound',
  );
  assert.deepEqual(
    view.cells.map((cell) => cell.channel),
    [...MACRO_CHANNELS],
  );
  assert.equal(view.macro, null);
  assert.equal(view.outcome, null);
});

test('the schema refuses macros the display could not draw', () => {
  const header = macroHeader();
  const bad: unknown[] = [
    { slot: MACRO_SLOTS, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' },
    { slot: -1, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' },
    { slot: 1.5, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' },
    { slot: 0, name: '', gloss: 'leave here', channel: 'MB·OUT' },
    { slot: 0, name: 'A NAME THAT IS FAR TOO LONG', gloss: '', channel: 'MB·OUT' },
    { slot: 0, name: 'GO OUT', channel: 'MB·OUT' },
    // The channel is not optional: a cell with no tag has no glyph to draw.
    { slot: 0, name: 'GO OUT', gloss: 'leave here' },
    { slot: 0, name: 'GO OUT', gloss: 'leave here', channel: '' },
    { slot: 0, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT', extra: 1 },
  ];
  for (const entry of bad) {
    assert.equal(
      validate({ ...header, game: { ...header.game, palette: [entry] } }),
      false,
      `schema accepted ${JSON.stringify(entry)}`,
    );
  }

  // One entry past the type list cannot happen: the wire carries a slot per type and no more.
  const overflow = Array.from({ length: MACRO_SLOTS + 1 }, (_, slot) => ({
    slot: slot % MACRO_SLOTS,
    name: 'NEXT',
    gloss: 'advance it',
    channel: 'MB·NEXT',
  }));
  assert.equal(validate({ ...header, game: { ...header.game, palette: overflow } }), false);

  for (const outcome of ['ok', 'aborted', '', null]) {
    assert.equal(
      validate({ ...header, game: { ...header.game, macroOutcome: { slot: 0, name: 'GO OUT', outcome, atMs: 1 } } }),
      false,
      `schema accepted outcome ${JSON.stringify(outcome)}`,
    );
  }
  for (const scene of ['battle_switch', 'Overworld', 'fishing']) {
    assert.equal(validate({ ...header, game: { ...header.game, scene } }), false, `schema accepted scene ${scene}`);
  }
  // The two modes section 12 removed are not modes any more.
  for (const mode of ['palette', 'plan']) {
    assert.equal(validate({ ...header, game: { ...header.game, macroMode: mode } }), false, `schema accepted ${mode}`);
  }
});

test('the reader ignores macros sent in raw mode, so the screen and the mode cannot disagree', () => {
  const header = macroHeader();
  const view = paletteView({ ...header, game: { ...header.game, macroMode: 'raw' } });

  assert.equal(view.mode, 'raw');
  assert.equal(view.supported, true, 'the producer does carry the fields');
  assert.ok(view.cells.every((cell) => cell.entry === null), 'nothing is drawn as bound');
  assert.equal(view.macro, null);
  assert.equal(view.outcome, null);
});

test('the rows are the scene\'s macros in type order, whatever order they arrived in', () => {
  // The screen's cell order is the contract's type order (section 12), so a macro is in the same
  // place in every scene that binds it — which is the whole point of a channel per type.
  const view = paletteView({
    game: {
      ...macroHeader().game,
      palette: [
        { slot: 0, name: 'MENU', gloss: 'open menu', channel: 'MB·MENU' },
        { slot: 1, name: 'GO ITEM', gloss: 'an object', channel: 'MB·ITEM' },
        { slot: 2, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' },
      ],
    },
  });
  // Section 14: a cell per type, at its own index, lit when bound and dim when not.
  const bound = view.cells.filter((cell) => cell.entry !== null);
  assert.deepEqual(
    bound.map((cell) => cell.entry?.name),
    ['GO OUT', 'GO ITEM', 'MENU'],
  );
  assert.deepEqual(
    bound.map((cell) => cell.row),
    [macroTypeIndex('GO OUT'), macroTypeIndex('GO ITEM'), macroTypeIndex('MENU')],
  );
  // Every cell knows its own tag, bound or not, because the strip draws all of them.
  assert.deepEqual(
    view.cells.map((cell) => cell.channel),
    [...MACRO_CHANNELS],
  );
  assert.deepEqual(
    view.cells.map((cell) => cell.type),
    MACRO_TYPES.map((_, index) => index),
  );
  // The wire slot is carried through untouched: it is what `macro` and `macroOutcome` name.
  assert.deepEqual(
    bound.map((cell) => cell.entry?.slot),
    [2, 1, 0],
  );
});

test('the reader drops a duplicate slot, a repeated type and a nameless entry', () => {
  const view = paletteView({
    game: {
      ...macroHeader().game,
      palette: [
        { slot: 1, name: 'GO NPC', gloss: 'a person', channel: 'MB·NPC' },
        { slot: 1, name: 'SHOULD NOT', gloss: 'win', channel: 'MB·NO' },
        // One population per type, so the same macro cannot be on the pad twice.
        { slot: 2, name: 'GO NPC', gloss: 'a person', channel: 'MB·NPC' },
        { slot: 3, name: '', gloss: 'tile ahead', channel: 'MB·ITEM' },
      ],
    },
  });

  assert.deepEqual(
    view.cells.filter((cell) => cell.entry !== null).map((cell) => cell.entry?.name),
    ['GO NPC'],
  );
});

test('a missing channel is filled in from the name, and an unknown name still draws', () => {
  const view = paletteView({
    game: {
      ...macroHeader().game,
      palette: [
        { slot: 0, name: 'SOMERSAULT', gloss: 'not a type', channel: 'MB·FLIP' },
        { slot: 1, name: 'MOVE 2', gloss: 'second' } as never,
      ],
    },
  });
  // The tag belongs to the type, so the known name gets its own back, in its own cell...
  const known = view.cells[macroTypeIndex('MOVE 2')];
  assert.equal(known?.entry?.name, 'MOVE 2');
  assert.equal(known?.channel, 'MB·MV2');
  // ...and a name the contract does not have has no cell of its own, so it takes the first free
  // one rather than being dropped, keeping its own tag.
  const unknown = view.cells.find((cell) => cell.entry?.name === 'SOMERSAULT');
  assert.equal(unknown?.channel, 'MB·FLIP');
  assert.equal(unknown?.type, -1);
});

test('an unknown macro mode reads as raw rather than as itself', () => {
  // The page says the mode on air, so a mode it does not know must not be published as one. The
  // two removed modes are unknown modes now.
  for (const mode of ['autopilot', 'palette', 'plan']) {
    const view = paletteView({
      game: {
        ...macroHeader().game,
        macroMode: mode as never,
        palette: [{ slot: 0, name: 'GO OUT', gloss: 'leave here', channel: 'MB·OUT' }],
        macro: { slot: 0, name: 'GO OUT', sinceMs: 5 },
      },
    });
    assert.equal(view.mode, 'raw', `${mode} came through as itself`);
    assert.ok(view.cells.every((cell) => cell.entry === null));
    assert.equal(view.macro, null);
  }
});

test('every scene\'s macros are distinct types, inside the cell\'s limits', () => {
  const sets: [string, readonly { name: string; gloss: string }[]][] = [
    ...Object.entries(FAKE_MACRO_TABLE),
    ['overworld indoors', FAKE_INDOOR_OVERWORLD],
    ['overworld in a centre', FAKE_CENTER_OVERWORLD],
    ['dialog with a choice', [...(FAKE_MACRO_TABLE.dialog ?? []), ...FAKE_DIALOG_CHOICE]],
  ];
  for (const [scene, set] of sets) {
    const names = set.map((cell) => cell.name);
    assert.equal(new Set(names).size, names.length, `${scene} binds a type twice`);
    for (const cell of set) {
      assert.ok(MACRO_TYPES.includes(cell.name), `${scene}: "${cell.name}" is not one of the 31 types`);
      assert.ok(cell.name.length <= 14, `${scene}: "${cell.name}" is longer than 14 characters`);
      // Eleven, not twelve: the longest names are `GO OBJECTIVE` and `BUY ANTIDOTE`, and a row
      // that gives twelve characters of Silkscreen to the name has eleven glyphs of VT323 left
      // for the gloss in section 14's two-column strip (`apps/stage/src/lib/geometry.ts`).
      assert.ok(cell.gloss.length <= 11, `${scene}: gloss "${cell.gloss}" is longer than the cell's column`);
      assert.ok(cell.gloss.length > 0, `${scene}: "${cell.name}" has no gloss`);
    }
  }
});

test('the raw-mode fake claims no scene, no macros and no running macro', () => {
  const sim = new FakeFlysim({ scenario: 'running', seed: 11 });
  for (let i = 0; i < 400; i += 1) {
    const { header } = sim.tick(100);
    assert.ok(validate(header), `raw snapshot ${i} failed the schema: ${JSON.stringify(validate.errors)}`);
    assert.equal(header.game.macroMode, 'raw');
    assert.deepEqual(header.game.palette, []);
    assert.equal(header.game.macro, null);
    assert.equal(header.game.macroOutcome, null);
    // `docs/feed-protocol.md`: the scene is `unknown` in raw mode, because nothing is bound and
    // so no scene is claimed.
    assert.equal(header.game.scene, 'unknown');
    // And `game.mode` does not follow the scene into `UNKNOWN` just because raw mode claims none:
    // it stays the word the fake has always published for a running game.
    assert.equal(header.game.mode, header.status === 'booting' ? 'BOOT' : 'OVERWORLD');
    assert.equal(
      header.events.filter((event) => event.kind === 'macro').length,
      0,
      'raw mode produced a macro event',
    );
  }
});

test('the macros-mode fake deals scenes, runs macros and logs both ends of each one', () => {
  const sim = new FakeFlysim({ scenario: 'running', seed: 0xbee, macroMode: 'macros' });

  const scenes = new Set<string>();
  const outcomes = new Map<MacroOutcomeKind, number>();
  const labels: string[] = [];
  let starts = 0;
  let longestRunMs = 0;
  let widestPad = 0;
  let sawShortPad = false;

  // 20 simulated minutes at 100 ms a step: about twenty scene changes and a few hundred macros.
  for (let i = 0; i < 12_000; i += 1) {
    const { header } = sim.tick(100);
    assert.ok(validate(header), `macros snapshot ${i} failed the schema: ${JSON.stringify(validate.errors)}`);

    const view = paletteView(header);
    assert.equal(view.mode, 'macros');
    scenes.add(view.scene);
    const bound = view.cells.filter((cell) => cell.entry !== null).length;
    widestPad = Math.max(widestPad, bound);
    if (bound > 0 && bound < 4) sawShortPad = true;

    // The rows are in type order, and each carries its type's own tag.
    const types = view.cells.filter((cell) => cell.entry).map((cell) => cell.type);
    assert.deepEqual(types, [...types].sort((a, b) => a - b), `snapshot ${i} is out of type order`);
    for (const cell of view.cells) {
      if (!cell.entry) continue;
      assert.equal(cell.channel, macroChannel(cell.entry.name));
      // And the population behind that channel is reporting a rate the page can draw.
      assert.equal(typeof header.rates[macroRateRole(cell.entry.name)], 'number');
    }

    if (view.macro) {
      longestRunMs = Math.max(longestRunMs, view.macro.sinceMs);
      // A running macro is always one of the macros the scene bound.
      const cell = view.cells.find((candidate) => candidate.entry?.slot === view.macro?.slot);
      assert.ok(cell?.entry, `macro on unbound slot ${view.macro.slot} in scene ${view.scene}`);
      assert.equal(cell?.entry?.name, view.macro.name);
    }
    if (view.outcome) outcomes.set(view.outcome.outcome, (outcomes.get(view.outcome.outcome) ?? 0) + 1);

    for (const event of header.events) {
      if (event.kind !== 'macro') continue;
      labels.push(event.label);
      if (event.label.endsWith(' start')) starts += 1;
      assert.ok(
        typeof event.value === 'number' && event.value >= 0 && event.value < MACRO_SLOTS,
        event.label,
      );
    }
  }

  assert.ok(scenes.size >= 4, `only saw scenes ${[...scenes].join(', ')}`);
  assert.ok(starts > 20, `only ${starts} macro starts in 20 minutes`);
  // Section 14's own turn is nine buttons and the overworld is up to ten, so a pad wider than
  // the six the wire used to carry is what the fake must be able to deal at all.
  assert.ok(widestPad >= 7, `the widest pad was ${widestPad} macros`);
  assert.ok(sawShortPad, 'never saw a scene with only a button or two');
  assert.ok(longestRunMs <= MACRO_FRAME_CAP_MS + 200, `a macro ran ${longestRunMs} ms, past the 600-frame cap`);

  // Every outcome the contract names shows up, and every label is `<NAME> <word>`.
  for (const outcome of ['done', 'blocked', 'timeout', 'refused'] as MacroOutcomeKind[]) {
    assert.ok((outcomes.get(outcome) ?? 0) > 0, `never saw outcome ${outcome}`);
  }
  const words = new Set(labels.map((label) => label.slice(label.lastIndexOf(' ') + 1)));
  assert.deepEqual([...words].sort(), ['blocked', 'done', 'refused', 'start', 'timeout']);
});

test('the fake publishes a rate for all 22 channels, bound or not', () => {
  // They are populations of real neurons (section 11), so they fire whatever the scene binds; the
  // bound ones simply run higher. The SENSES panel's MACROS row reads exactly these.
  const sim = new FakeFlysim({ scenario: 'running', seed: 7, macroMode: 'macros' });
  let checked = 0;
  for (let i = 0; i < 600; i += 1) {
    const { header } = sim.tick(100);
    for (const name of MACRO_TYPES) {
      const rate = header.rates[macroRateRole(name)];
      assert.equal(typeof rate, 'number', `${name} has no rate`);
      assert.ok((rate as number) >= 0, `${name} reported ${String(rate)} Hz`);
    }
    const running = header.game.macro?.name;
    if (running) {
      checked += 1;
      const unbound = MACRO_TYPES.filter(
        (name) => !(header.game.palette ?? []).some((entry) => entry.name === name),
      );
      const quietest = Math.min(...unbound.map((name) => header.rates[macroRateRole(name)] as number));
      assert.ok(quietest >= 0, 'a masked channel reported a negative rate');
    }
  }
  assert.ok(checked > 0, 'no macro ran in a minute of simulation');
});

test('an outcome is kept until the next macro starts, and no longer', () => {
  // `docs/feed-protocol.md`: "kept until the next one starts, so the page can light a cell's
  // result for a beat rather than racing a single frame". The page shows it for a second of brain
  // time (`apps/stage/src/App.tsx`); what the feed owes it is that the field is still there.
  const sim = new FakeFlysim({ scenario: 'running', seed: 99, macroMode: 'macros' });
  let held = false;
  let previous = paletteView(sim.tick(50).header);

  for (let i = 0; i < 6_000; i += 1) {
    const view = paletteView(sim.tick(50).header);

    // A macro that just started clears the previous outcome...
    if (view.macro && !previous.macro) {
      assert.equal(view.outcome, null, `a macro started with ${view.outcome?.name ?? ''} still on screen`);
    }
    // ...and one that just ended leaves its own there for as long as nothing else starts.
    if (!view.macro && previous.macro) {
      assert.ok(view.outcome, 'a macro ended and reported no outcome');
      assert.equal(view.outcome?.name, previous.macro.name);
      held = true;
    }
    previous = view;
  }

  assert.ok(held, 'no macro ended in five minutes');
});

test('a scene change ends the macro that was running under it', () => {
  // Deterministic by construction rather than by seed: step until a snapshot shows a running macro
  // and the next one shows a different scene, then the same tick must have logged its end.
  const sim = new FakeFlysim({ scenario: 'running', seed: 4242, macroMode: 'macros' });
  let previous = paletteView(sim.tick(50).header);
  let checked = 0;

  for (let i = 0; i < 40_000 && checked < 2; i += 1) {
    const { header } = sim.tick(50);
    const view = paletteView(header);
    if (previous.macro && view.scene !== previous.scene) {
      const ended = header.events.some(
        (event) =>
          event.kind === 'macro' &&
          event.label.startsWith(`${previous.macro?.name ?? ''} `) &&
          !event.label.endsWith(' start'),
      );
      assert.ok(ended, `scene went ${previous.scene} -> ${view.scene} with ${previous.macro.name} running and no end event`);
      assert.equal(view.macro, null, 'a macro survived the scene change');
      checked += 1;
    }
    previous = view;
  }

  assert.ok(checked > 0, 'never caught a scene change with a macro running');
});

test('the indoor and outdoor overworld differ by the door and the stairs', () => {
  const outdoors = (FAKE_MACRO_TABLE.overworld ?? []).map((cell) => cell.name);
  const indoors = FAKE_INDOOR_OVERWORLD.map((cell) => cell.name);

  assert.ok(indoors.includes('GO OUT') && indoors.includes('GO WARP'), 'indoors has no way out');
  assert.ok(!outdoors.includes('GO OUT') && !outdoors.includes('GO WARP'), 'outdoors has a door to leave by');
  assert.ok(outdoors.includes('GO ROUTE'), 'outdoors has no route to the next area');
  assert.ok(!indoors.includes('GO ROUTE'), 'indoors has a route out of the building');

  // Everything else is the same set: the split is section 9.1's, not a different scene.
  const rest = (names: string[]) => names.filter((name) => !['GO OUT', 'GO WARP', 'GO ROUTE'].includes(name));
  assert.deepEqual(rest(indoors), rest(outdoors));
});
