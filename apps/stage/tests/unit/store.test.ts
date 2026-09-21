/**
 * The cold store's commit gate.
 *
 * The gate coalesces React renders to 4 Hz, and it has to do that against an *injected* clock
 * that can jump backwards: a fixture held on a seek target freezes its clock at the virtual time
 * of the snapshot it landed on, which is earlier than the real clock the paint loop was using
 * while the fixture was still loading. A plain `elapsed >= 250` gate stays shut forever after
 * that, and the page renders its initial state over a feed that has already delivered snapshots.
 *
 * That shipped, briefly, and the cold-open fixture is what caught it: every readout zero while
 * sixteen snapshots had been ingested. Hence this test.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { MACRO_CHANNELS, MACRO_SLOTS, macroTypeIndex, type FeedHeader } from '@flybrain/feed';
import { COMMIT_MS, FeedIngest, STALE_AFTER_MS, hot, useStage } from '../../src/feed/store';
import { pokemonRed } from '../../src/games/pokemon-red';
import { MotionEngine } from '../../src/motion/engine';
import { RailSignals } from '../../src/motion/rail-signals';

function snapshot(seq: number, overrides: Partial<FeedHeader> = {}) {
  const header: FeedHeader = {
    protocol: 1,
    seq,
    wallMs: 1_700_000_000_000 + seq * 33,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: seq,
    runSeconds: seq,
    brainMs: seq * 1000,
    frame: seq,
    buttons: 0,
    rates: { command_0: 20 },
    populationRate: 13,
    spikeCount: 1956,
    learning: { enabled: true, updates: 3, changed: 1, synapses: 42, signal: 0.1 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 4,
      badges: 1,
      uniqueLocations: 7,
      rewardTotal: 1.25,
      rewardCounts: { story: 1, explore: 4, area: 2, pokedex: 0, trainer: 1, wildwin: 0, badge: 1 },
    },
    milestone: { rank: 4, label: "reached Oak's lab", next: 'got a starter', sinceSeconds: 120, attempts: 2 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 3000, lastBy: 'alex', todayCount: 4 },
    events: [],
    attachments: ['spikes'],
    ...overrides,
  };
  return { header, frame: null, audio: null, spikes: new Uint8Array([1]) };
}

/**
 * A store wired to a motion engine, as the page wires it.
 *
 * The moment machinery moved out of this file's subject and into `src/motion/`
 * (`MotionEngine` + `RailSignals`), so the store's part is now exactly the seam: hand over the
 * snapshot and its clock, and publish whatever the engine says is on stage.
 */
function fresh(): { ingest: FeedIngest; motion: MotionEngine; signals: RailSignals } {
  const motion = new MotionEngine();
  const signals = new RailSignals();
  const ingest = new FeedIngest(pokemonRed, motion, signals);
  ingest.reset();
  useStage.setState({
    populationRate: 0,
    milestone: { rank: 0, label: '', next: '', sinceSeconds: 0, attempts: 0 },
    uniqueLocations: 0,
    stale: false,
  });
  return { ingest, motion, signals };
}

test('one snapshot reaches the DOM state on the very next commit', () => {
  const { ingest } = fresh();
  ingest.ingest(snapshot(1), 1000);
  ingest.commit(1000);

  const state = useStage.getState();
  assert.equal(state.populationRate, 13);
  assert.equal(state.milestone.label, "reached Oak's lab");
  assert.equal(state.uniqueLocations, 7);
  assert.equal(state.spikeCount, 1956);
  assert.equal(state.badges, 1);
  assert.equal(state.sugar.lastBy, 'alex');
});

test('commits coalesce to 4 Hz', () => {
  const { ingest } = fresh();
  ingest.ingest(snapshot(1), 1000);
  ingest.commit(1000);

  ingest.ingest(snapshot(2, { populationRate: 99 }), 1050);
  ingest.commit(1050);
  assert.equal(useStage.getState().populationRate, 13, 'inside the window, nothing is written');

  ingest.commit(1000 + COMMIT_MS);
  assert.equal(useStage.getState().populationRate, 99, 'and the window opens on time');
});

test('a clock that jumps backwards commits immediately', () => {
  const { ingest } = fresh();
  // The paint loop's real clock, before the fixture finished loading.
  ingest.commit(50_000);

  // The seek lands, and every clock value from here is the recording's virtual time.
  ingest.ingest(snapshot(1), 42_000);
  ingest.commit(42_000);

  assert.equal(useStage.getState().populationRate, 13, 'the gate reopened on the backwards jump');
});

test('reset reopens the gate, so a seek always repaints', () => {
  const { ingest } = fresh();
  ingest.ingest(snapshot(1), 1000);
  ingest.commit(1000);

  ingest.reset();
  ingest.ingest(snapshot(2, { populationRate: 77 }), 1010);
  ingest.commit(1010);
  assert.equal(useStage.getState().populationRate, 77);
});

test('a ticker change commits even inside the coalescing window', () => {
  const { ingest } = fresh();
  ingest.commit(1000);

  ingest.ingest(
    snapshot(1, {
      events: [
        { id: 1, wallMs: 0, brainMs: 0, kind: 'reward', label: 'raw', value: 3, rewardKind: 'badge' },
      ],
    }),
    1010,
  );
  ingest.commit(1010);

  const state = useStage.getState();
  assert.equal(state.ticker.length, 1, 'the event did not wait 250 ms');
  assert.equal(state.ticker[0]?.tier, 'moment');
  assert.ok(state.moment, 'and the moment reached the DOM state');
  assert.equal(state.moment?.type, 'badge');
});

test('the stale flag follows the injected clock, not the wall clock', () => {
  const { ingest } = fresh();
  ingest.ingest(snapshot(1), 1000);
  ingest.commit(1000, true);
  assert.equal(useStage.getState().stale, false);

  ingest.commit(1000 + STALE_AFTER_MS - 1, true);
  assert.equal(useStage.getState().stale, false, 'just inside the window');

  ingest.commit(1000 + STALE_AFTER_MS + 1, true);
  assert.equal(useStage.getState().stale, true);
});

test('a moment reaches the DOM state the frame it starts, and leaves on its own', () => {
  // The commit gate coalesces React to 4 Hz, but a moment must not wait up to 250 ms for it: the
  // caption band, the rail flash and the tab focus all key off the same state. A sugar's total
  // stage time is 2.3 s (`MOMENT_TIMING`), so it is gone well before the next 4 Hz tick would
  // have noticed.
  const { ingest, motion } = fresh();
  ingest.ingest(
    snapshot(1, {
      events: [{ id: 1, wallMs: 0, brainMs: 0, kind: 'sugar', label: 'alex fed the fly sugar', by: 'alex', value: 400 }],
    }),
    1000,
  );
  motion.tick(1000);
  ingest.commit(1000, true);
  assert.equal(useStage.getState().moment?.type, 'sugar');

  motion.tick(1000 + 2000);
  ingest.commit(1000 + 2000, true);
  assert.ok(useStage.getState().moment, 'still on stage at 2 s');

  motion.tick(1000 + 4000);
  ingest.commit(1000 + 4000, true);
  assert.equal(useStage.getState().moment, null);
});

test('a milestone rank that climbs without an event still becomes a moment', () => {
  // The contract allows the service to drop snapshots rather than queue them, so an event can be
  // lost while the counter it belongs to moves. Watching both is the difference between a
  // milestone with a chime and a silent one.
  const { ingest, motion, signals } = fresh();
  ingest.ingest(snapshot(1, { milestone: { rank: 4, label: 'a', next: 'b', sinceSeconds: 0, attempts: 0 } }), 1000);
  motion.tick(1000);
  ingest.commit(1000, true);
  assert.equal(useStage.getState().moment, null, 'the first snapshot is not a change');

  ingest.ingest(snapshot(2, { milestone: { rank: 5, label: 'b', next: 'c', sinceSeconds: 0, attempts: 0 } }), 1100);
  motion.tick(1100);
  ingest.commit(1100, true);
  assert.equal(useStage.getState().moment?.type, 'milestone');
  // And the rail's own signals saw it too, which is what steers the tab slot to LADDER.
  assert.equal(signals.takeSteering().rankChanged, true);
});

test('button edges are recorded against the injected clock', () => {
  const { ingest } = fresh();
  ingest.ingest(snapshot(1, { buttons: 0 }), 1000);
  ingest.ingest(snapshot(2, { buttons: 1 << 4 }), 1100); // A down
  assert.equal(hot.buttonStates.a?.down, true);
  assert.equal(hot.buttonStates.a?.downAtMs, 1100);

  ingest.ingest(snapshot(3, { buttons: 0 }), 1185); // A up, 85 ms later
  assert.equal(hot.buttonStates.a?.down, false);
  assert.equal(hot.buttonStates.a?.upAtMs, 1185);
  assert.equal((hot.buttonStates.a?.upAtMs ?? 0) - (hot.buttonStates.a?.downAtMs ?? 0), 85);
});

test('a seq gap is counted rather than hidden', () => {
  const { ingest } = fresh();
  hot.gaps = 0;
  ingest.ingest(snapshot(10), 1000);
  ingest.ingest(snapshot(14), 1100);
  assert.equal(hot.gaps, 3, 'three snapshots the service dropped');
});

test('spikeCount holds its last real value when the attachment is strided out', () => {
  // The committed fixtures record spikes at 10 Hz, and the contract says `spikeCount` is 0 when
  // the attachment is absent. The readout must not blink to zero two frames out of three.
  const { ingest } = fresh();
  ingest.ingest(snapshot(1), 1000);
  ingest.commit(1000, true);
  assert.equal(useStage.getState().spikeCount, 1956);

  const withoutSpikes = snapshot(2, { spikeCount: 0, attachments: [] });
  ingest.ingest({ ...withoutSpikes, spikes: null }, 1033);
  ingest.commit(1033, true);
  assert.equal(useStage.getState().spikeCount, 1956);
});

test('the scene\'s macros commit as a cell per type, and a raw-mode header commits none', () => {
  // The cold store is where the strip's *text* lives (`src/panels/MacroPalette.tsx` draws it);
  // which cell is lit is the paint loop's, off `hot.header`. So what this pins is that the commit
  // is total: one cell per macro type either way (section 14), so the strip cannot render a
  // two-cell grid for a frame, and that a cell holds its own type whatever slot the producer
  // filled.
  const { ingest } = fresh();

  const macros = snapshot(1, {
    game: {
      ...snapshot(1).header.game,
      scene: 'battle',
      macroMode: 'macros',
      palette: [
        { slot: 21, name: 'SWITCH', gloss: 'healthiest', channel: 'MB\u00b7SWAP' },
        { slot: 17, name: 'MOVE 1', gloss: 'first move', channel: 'MB\u00b7MV1' },
      ],
      macro: { slot: 17, name: 'MOVE 1', sinceMs: 120 },
      macroOutcome: null,
    },
  });
  ingest.ingest(macros, 1000);
  ingest.commit(1000, true);

  const state = useStage.getState();
  assert.equal(state.scene, 'battle');
  assert.equal(state.macroMode, 'macros');
  assert.equal(state.palette.length, MACRO_SLOTS);
  assert.deepEqual(
    state.palette.filter((cell) => cell.entry !== null).map((cell) => [cell.row, cell.entry?.name]),
    [
      [macroTypeIndex('MOVE 1'), 'MOVE 1'],
      [macroTypeIndex('SWITCH'), 'SWITCH'],
    ],
  );
  // Every cell carries its type's tag, bound or not: that is what lets the strip draw the whole
  // pad and dim the part of it this scene has taken away.
  assert.deepEqual(state.palette.map((cell) => cell.channel), [...MACRO_CHANNELS]);
  // The wire slot rides along, because it is what `macro` and `macroOutcome` name.
  assert.deepEqual(
    state.palette.filter((cell) => cell.entry !== null).map((cell) => cell.entry?.slot),
    [17, 21],
  );

  // An older service — no macro fields at all — is raw mode with an empty cell per type.
  ingest.ingest(snapshot(2), 1300);
  ingest.commit(1300, true);
  const raw = useStage.getState();
  assert.equal(raw.macroMode, 'raw');
  assert.equal(raw.palette.length, MACRO_SLOTS);
  assert.deepEqual(
    raw.palette.filter((cell) => cell.entry !== null),
    [],
  );
});

