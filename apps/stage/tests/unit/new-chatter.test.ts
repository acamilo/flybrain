/**
 * The new-chatter switch (`docs/design/describe-tab.md`, 2026-09-17).
 *
 * The operator: "when new user joins chat, switch to it for a few sec, cool down timer." Two halves, and
 * this drives both against a scripted chat ring shaped exactly like the feed's — the fake
 * simulator's own scenario, names and bot line included
 * (`packages/feed/src/fake/simulator.ts`'s `CHAT_SCRIPT`), because that is what the page sees in
 * dev and what a fixture recorded from it carries:
 *
 *   - `ChatterWatch` (`src/lib/chatters.ts`): is this name new, and are the bot's lines and the
 *     ring's history correctly not arrivals;
 *   - `TabController` (`src/lib/tabs.ts`): the 8 s hold, the return to the tab that was up, and
 *     the 120 s cooldown.
 *
 * Wired together the way the page wires them, through `RailSignals.observe`, so the test fails if
 * either half is connected to the other wrongly rather than only if one of them is wrong.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import type { ChatLine, FeedHeader } from '@flybrain/feed';
import { ChatterWatch } from '../../src/lib/chatters';
import {
  NEW_CHATTER_COOLDOWN_MS,
  NEW_CHATTER_HOLD_MS,
  STEER_DWELL_MS,
  TabController,
  type TabSignals,
} from '../../src/lib/tabs';
import { RailSignals } from '../../src/motion/rail-signals';

/** The page's connect time, as `ChatterWatch` is given it. */
const CONNECTED_AT = 1_700_000_000_000;

/** One accepted line. `at` is an offset from the connect time, so a negative `at` is history. */
function line(id: number, by: string, at: number, bot = false): ChatLine {
  return { id, wallMs: CONNECTED_AT + at, by, text: 'hello little fly', ...(bot ? { bot: true } : {}) };
}

/**
 * The fake simulator's chatter script, as a ring: three viewers, then the bridge's own reply.
 *
 * Copied in shape, not in words — the words are the fake's and the test only needs the names and
 * the `bot` flag.
 */
const SCENARIO: readonly ChatLine[] = [
  line(1, 'mothra_fan', 1_000),
  line(2, 'kc_gamma', 9_000),
  line(3, 'ari_9', 18_000),
  line(4, 'flybridgebot', 19_000, true),
];

/** A header carrying `chat`, which is all `RailSignals` reads here. */
function header(seq: number, chat: readonly ChatLine[]): FeedHeader {
  return {
    protocol: 1,
    seq,
    wallMs: CONNECTED_AT + seq * 1000,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: seq,
    runSeconds: seq,
    brainMs: seq * 1000,
    frame: seq,
    buttons: 0,
    rates: {},
    populationRate: 10,
    spikeCount: 0,
    learning: { enabled: true, updates: 0, changed: 0, synapses: 0, signal: 0 },
    game: { mode: 'OVERWORLD', semanticRewards: true, rewardTotal: 0, badges: 0, uniqueLocations: 0 },
    milestone: { rank: 3, label: 'a', next: 'b', sinceSeconds: 10, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
    events: [],
    chat: [...chat],
    attachments: [],
  } as unknown as FeedHeader;
}

const QUIET: TabSignals = { walking: false, commandActivity: 0, reward: false, rankChanged: false };

test('a name nobody has seen is an arrival; the bot and the ring history are not', () => {
  const watch = new ChatterWatch(CONNECTED_AT);

  // The ring as it stood when the page connected: said before anyone was watching this page.
  assert.equal(watch.observe([line(1, 'norbert', -30_000), line(2, 'lena_k', -12_000)]), false);
  assert.equal(watch.count, 2, 'history still counts as seen');

  // A live line from somebody new.
  assert.equal(watch.observe([line(3, 'mothra_fan', 4_000)]), true);
  // The same ring again, which is what every subsequent snapshot carries.
  assert.equal(watch.observe([line(3, 'mothra_fan', 4_000)]), false, 'the same line switched twice');
  // A name from the history, live this time: already seen, so not an arrival.
  assert.equal(watch.observe([line(4, 'norbert', 6_000)]), false, 'a returning viewer is not an arrival');
  // The bridge's own reply.
  assert.equal(watch.observe([line(5, 'flybridgebot', 7_000, true)]), false, 'the bot switched the tab');
  // And case is not identity.
  assert.equal(watch.observe([line(6, 'Mothra_Fan', 8_000)]), false, 'the same name in caps switched again');
});

test('a reconnect replaying the whole ring is not four arrivals', () => {
  const watch = new ChatterWatch(CONNECTED_AT);
  assert.equal(watch.observe(SCENARIO), true, 'the first live line is an arrival');
  for (let again = 0; again < 5; again += 1) {
    assert.equal(watch.observe(SCENARIO), false, 'the replayed ring switched the tab');
  }
  assert.equal(watch.count, 4);
});

test('the switch holds DESCRIBE for 8 s and hands the slot back', () => {
  const tabs = new TabController({ initial: 'senses' });
  assert.equal(tabs.update(0, QUIET), 'senses');

  assert.equal(tabs.update(1_000, { ...QUIET, newChatter: true }), 'describe', 'a new chatter did not switch');
  assert.ok(tabs.focused, 'the switch is a focus, so the slot returns on its own');
  assert.equal(tabs.update(1_000 + NEW_CHATTER_HOLD_MS - 1, QUIET), 'describe', 'the hold ended early');
  assert.equal(tabs.update(1_000 + NEW_CHATTER_HOLD_MS, QUIET), 'senses', 'the slot did not go back');
  assert.equal(tabs.reason, 'return');
  assert.equal(NEW_CHATTER_HOLD_MS, 8_000, 'the documented hold is 8 s');
});

test('the cooldown blocks a second switch, and lets the one after it through', () => {
  // A 10-minute dwell, because the cooldown is longer than the real 45-to-60 s one: on the shipped
  // cadence the slot would cycle twice inside the window and the tab this asserts on would be the
  // rotation's, not the switch's.
  const tabs = new TabController({ initial: 'ladder', minMs: 600_000, maxMs: 600_000 });
  tabs.update(0, QUIET);
  assert.equal(tabs.update(1_000, { ...QUIET, newChatter: true }), 'describe');
  const first = tabs.lastChatterSwitch;
  assert.equal(first, 1_000);

  // Out of the hold, inside the cooldown: another new name changes nothing.
  const inside = 1_000 + NEW_CHATTER_COOLDOWN_MS - 1;
  assert.equal(tabs.update(20_000, QUIET), 'ladder', 'the slot should be back on LADDER');
  assert.equal(tabs.update(inside, { ...QUIET, newChatter: true }), 'ladder', 'the cooldown let a switch through');
  assert.equal(tabs.lastChatterSwitch, first, 'a blocked switch reset the cooldown');

  // And once it has run out.
  assert.equal(tabs.update(1_000 + NEW_CHATTER_COOLDOWN_MS, { ...QUIET, newChatter: true }), 'describe');
  assert.equal(NEW_CHATTER_COOLDOWN_MS, 120_000, 'the documented cooldown is 120 s');
});

test('a moment keeps the slot, and does not spend the cooldown doing it', () => {
  const tabs = new TabController({ initial: 'senses' });
  tabs.update(0, QUIET);
  tabs.focusOn('connectome', 9_000, 0);
  assert.equal(tabs.update(1_000, { ...QUIET, newChatter: true }), 'connectome', 'a new chatter stole a moment');
  assert.equal(tabs.lastChatterSwitch, null, 'the cooldown was spent on a switch that did not happen');

  // The moment ends, and the next new name gets its switch.
  assert.equal(tabs.update(9_000, QUIET), 'senses');
  assert.equal(tabs.update(9_100, { ...QUIET, newChatter: true }), 'describe');
});

test('a pinned slot ignores it entirely', () => {
  const tabs = new TabController({ forced: 'senses' });
  tabs.update(0, QUIET);
  assert.equal(tabs.update(1_000, { ...QUIET, newChatter: true }), 'senses', '?tab= was overridden');
  assert.equal(tabs.lastChatterSwitch, null);
});

test('the scenario, end to end: RailSignals to the slot', () => {
  const signals = new RailSignals(CONNECTED_AT);
  const tabs = new TabController({ initial: 'senses' });
  tabs.update(0, QUIET);

  // Snapshot one: the ring as it stood at connect. Nothing arrived.
  signals.observe(header(1, [line(1, 'norbert', -40_000)]), 1_000);
  assert.equal(signals.takeSteering().newChatter, false);
  assert.equal(tabs.update(1_000, { ...QUIET, newChatter: false }), 'senses');

  // Snapshot two: the fake's scripted chatter starts. mothra_fan is new.
  signals.observe(header(2, SCENARIO), 2_000);
  const steering = signals.takeSteering();
  assert.equal(steering.newChatter, true, 'the scenario did not report an arrival');
  assert.equal(tabs.update(2_000, { ...QUIET, newChatter: steering.newChatter }), 'describe');

  // The flag is one frame's, not sticky.
  assert.equal(signals.takeSteering().newChatter, false);

  // The rest of the run: the ring keeps being re-sent, and the bot keeps answering.
  signals.observe(header(3, SCENARIO), 3_000);
  assert.equal(signals.takeSteering().newChatter, false, 'the ring re-triggered');
  assert.equal(tabs.update(2_000 + NEW_CHATTER_HOLD_MS, QUIET), 'senses', 'the slot did not return');
  assert.equal(signals.chatterCount(), 5, 'norbert plus the scenario is five names');

  // A seek replays it all: nothing this page saw, it saw — and the replayed lines are history.
  signals.reset();
  assert.equal(signals.chatterCount(), 0);

  // Sanity on the steer this shares its channel with: still gated by the dwell, unchanged.
  assert.ok(STEER_DWELL_MS > 0);
});
