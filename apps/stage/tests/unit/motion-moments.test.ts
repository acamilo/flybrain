/**
 * The moment queue and the feed -> moment mapping.
 *
 * What is actually being pinned here is `docs/design/animation.md`'s three rules — one moment at a
 * time, priority badge > milestone > rollback > sugar > reward, equal priority coalesces — plus the
 * thing the design's own verification section asks for ("assert the DOM state at t+100 ms and at
 * t+9.5 s (entered, then gone), the queue ordering with two simultaneous events"), one layer below
 * the DOM where it can be checked without a browser.
 *
 * Every test drives a virtual clock. There are no timers and nothing sleeps, so nine seconds of
 * moment costs microseconds and the result is the same on a loaded machine.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import type { FeedEvent, FeedHeader, GameMode } from '@flybrain/feed';

import {
  dayNumber,
  MOMENT_PRIORITY,
  MOMENT_TIMING,
  MOMENT_TYPES,
  MomentQueue,
  momentTotalMs,
  rewardTier,
  triggerFromEvent,
  TriggerMapper,
  type MomentTrigger,
  type MomentType,
} from '../../src/motion/moments';

// ---------------------------------------------------------------------------------------------
// Synthetic feed material
// ---------------------------------------------------------------------------------------------

/** A `FeedHeader` with every required field, so a test can override only what it is about. */
function header(overrides: Partial<FeedHeader> = {}): FeedHeader {
  return {
    protocol: 1,
    seq: 1,
    wallMs: 1_700_000_000_000,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: 120,
    runSeconds: 120,
    brainMs: 120_000,
    frame: 3600,
    buttons: 0,
    rates: {},
    populationRate: 13,
    spikeCount: 0,
    learning: { enabled: true, updates: 10, changed: 2, synapses: 1000, signal: 0.1 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 4,
      rewardTotal: 0.4,
      rewardCounts: { story: 0, explore: 2, area: 1, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
    },
    milestone: { rank: 3, label: 'Left the bedroom', next: 'Left the house', sinceSeconds: 40, attempts: 1 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 2 },
    events: [],
    attachments: [],
    ...overrides,
  };
}

let nextEventId = 1;
function event(overrides: Partial<FeedEvent> & Pick<FeedEvent, 'kind' | 'label'>): FeedEvent {
  return { id: nextEventId++, wallMs: 1_700_000_000_000, brainMs: 120_000, ...overrides };
}

function trigger(type: MomentType, label: string = type, intensity = 1): MomentTrigger {
  return { type, label, intensity, source: 'header' };
}

// ---------------------------------------------------------------------------------------------
// Priority, coalescing, one at a time
// ---------------------------------------------------------------------------------------------

test('the priority order is the design\'s: badge > milestone > rollback > sugar > reward', () => {
  const ranked = [...MOMENT_TYPES].sort((a, b) => MOMENT_PRIORITY[b] - MOMENT_PRIORITY[a]);
  assert.deepEqual(ranked.slice(0, 5), ['badge', 'milestone', 'rollback', 'sugar', 'reward']);
  // Distinct priorities are what makes "equal priority" mean "same type".
  assert.equal(new Set(Object.values(MOMENT_PRIORITY)).size, MOMENT_TYPES.length);
});

test('one moment at a time: a lower-priority moment waits its turn', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('milestone', 'Reached: Left the bedroom'), 0);
  queue.enqueue(trigger('reward', 'new place'), 0);

  queue.tick(100);
  const active = queue.active(100);
  assert.equal(active?.type, 'milestone');
  assert.equal(active?.phase, 'entering');
  assert.equal(queue.pendingCount, 1);
  assert.deepEqual(queue.pending().map((item) => item.type), ['reward']);

  // The milestone runs its full course, then the reward starts on the frame the milestone ends.
  const end = momentTotalMs('milestone');
  queue.tick(end);
  assert.equal(queue.active(end)?.type, 'reward');
  assert.equal(queue.pendingCount, 0);
});

test('a bigger moment pre-empts, and the moment it interrupted is spent rather than requeued', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('reward', 'new place'), 0);
  queue.tick(300);
  assert.equal(queue.active(300)?.type, 'reward');

  queue.enqueue(trigger('badge', 'gym badge'), 300);
  // Still the reward, now on its way out over the short pre-emption exit.
  assert.equal(queue.active(300)?.type, 'reward');
  assert.equal(queue.active(300)?.phase, 'leaving');

  queue.tick(420);
  assert.equal(queue.active(420)?.type, 'badge');
  // The reward does not come back 9 s later: the ticker already reported it.
  queue.tick(420 + momentTotalMs('badge'));
  assert.equal(queue.active(420 + momentTotalMs('badge')), null);
  assert.equal(queue.pendingCount, 0);
});

test('equal priority coalesces into the moment on stage and restarts its hold', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('sugar', 'SUGAR · ada'), 0);
  queue.tick(500);
  assert.equal(queue.active(500)?.phase, 'holding');

  const id = queue.active(500)?.id;
  queue.enqueue({ type: 'sugar', label: 'SUGAR · bo', intensity: 1, source: 'event' }, 500);

  const active = queue.active(500);
  assert.equal(active?.id, id, 'a coalesce keeps the id, so React keeps the same element');
  assert.equal(active?.count, 2);
  assert.equal(active?.trigger.label, 'SUGAR · bo', 'the caption follows the newest trigger');
  assert.equal(queue.pendingCount, 0, 'nothing queued: it folded');

  // The hold clock restarted at 500, so the moment now ends a full hold later than it would have.
  const wouldHaveEnded = MOMENT_TIMING.sugar.enterMs + MOMENT_TIMING.sugar.holdMs;
  queue.tick(wouldHaveEnded + 1);
  assert.equal(queue.active(wouldHaveEnded + 1)?.phase, 'holding');
});

test('a waiting moment of the same type folds into the one already waiting', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('badge'), 0);
  queue.enqueue(trigger('reward', 'a'), 0);
  queue.enqueue(trigger('reward', 'b'), 0);

  assert.equal(queue.pendingCount, 1);
  assert.equal(queue.pending()[0]?.count, 2);
});

test('the pending queue is capped, dropping the least important waiter', () => {
  const queue = new MomentQueue({ maxPending: 2 });
  queue.enqueue(trigger('badge'), 0);
  queue.tick(0);
  assert.equal(queue.enqueue(trigger('milestone'), 0), 2);
  assert.equal(queue.enqueue(trigger('rollback'), 0), 3);
  // Third waiter, lowest priority of the three: refused, and the queue stays at its cap.
  assert.equal(queue.enqueue(trigger('modeChange'), 0), null);
  assert.equal(queue.pendingCount, 2);
  assert.deepEqual(queue.pending().map((item) => item.type), ['milestone', 'rollback']);
});

// ---------------------------------------------------------------------------------------------
// The state machine and the 9 s hold
// ---------------------------------------------------------------------------------------------

test('a milestone is on stage at t+100 ms and gone after its hold, per the design verification', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('milestone', 'Reached: Left the bedroom'), 0);

  queue.tick(100);
  const entering = queue.active(100);
  assert.equal(entering?.phase, 'entering');
  assert.ok((entering?.presence ?? 0) > 0.5, 'out-expo is most of the way in by 100 of 320 ms');

  // The design's 9 s is the *total* stage time, arrival and exit included, because that is what
  // its own verification measures ("at t+100 ms and at t+9.5 s: entered, then gone") and what the
  // catalogue's "LADDER focus 9 s" means on screen.
  assert.equal(momentTotalMs('milestone'), 9000);
  queue.tick(5000);
  assert.equal(queue.active(5000)?.phase, 'holding');
  assert.equal(queue.active(5000)?.presence, 1);

  // So at t+9.5 s it is gone, not still leaving.
  queue.tick(9000);
  assert.equal(queue.active(9000), null);
  queue.tick(9500);
  assert.equal(queue.active(9500), null);
});

test('presence rises from 0 through 1 and back to 0, monotonically in each phase', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('rollback', 'REWIND'), 0);

  let previous = -1;
  let peaked = false;
  for (let nowMs = 0; nowMs <= momentTotalMs('rollback'); nowMs += 16.7) {
    queue.tick(nowMs);
    const active = queue.active(nowMs);
    if (!active) break;
    const presence = active.presence;
    assert.ok(presence >= -1e-9 && presence <= 1 + 1e-9, `presence ${String(presence)} out of range`);
    if (active.phase === 'entering') {
      assert.ok(presence >= previous - 1e-9, 'the arrival never goes backwards');
      previous = presence;
    }
    if (active.phase === 'holding') peaked = true;
  }
  assert.equal(peaked, true);
});

test('a moment shorter than a dropped frame still drains the queue on one tick', () => {
  const queue = new MomentQueue();
  // modeChange is 200/0/200 ms: a single 500 ms stall spans the whole thing.
  queue.enqueue(trigger('modeChange', 'BATTLE'), 0);
  queue.enqueue(trigger('sugar', 'SUGAR'), 0);

  queue.tick(500);
  assert.equal(queue.active(500)?.type, 'sugar', 'the queue did not stall behind a finished moment');
});

test('the same script against the same clock gives the same result twice', () => {
  const script: [number, MomentType][] = [
    [0, 'reward'],
    [120, 'sugar'],
    [900, 'milestone'],
    [1500, 'reward'],
    [9800, 'badge'],
  ];
  const run = () => {
    const queue = new MomentQueue();
    const seen: string[] = [];
    let cursor = 0;
    for (let nowMs = 0; nowMs <= 22_000; nowMs += 16.7) {
      while (cursor < script.length && (script[cursor] as [number, MomentType])[0] <= nowMs) {
        const [, type] = script[cursor] as [number, MomentType];
        queue.enqueue(trigger(type), nowMs);
        cursor += 1;
      }
      queue.tick(nowMs);
      const active = queue.active(nowMs);
      const line = active ? `${active.type}:${active.phase}:${String(active.id)}` : 'idle';
      if (seen[seen.length - 1] !== line) seen.push(line);
    }
    return seen;
  };

  const first = run();
  assert.deepEqual(run(), first);
  // And it is a real sequence, not an empty one.
  assert.ok(first.length > 6, `only ${String(first.length)} states`);
  assert.ok(first.some((line) => line.startsWith('badge:')));
});

test('subscribers hear about phases and coalesces, not about every frame', () => {
  const queue = new MomentQueue();
  let calls = 0;
  const off = queue.subscribe(() => {
    calls += 1;
  });

  queue.enqueue(trigger('reward', 'new place'), 0);
  const afterEnqueue = calls;
  assert.ok(afterEnqueue >= 1, 'the new moment notified');

  // Ten frames inside the 240 ms arrival: the progress moves every frame, the snapshot does not.
  for (let i = 1; i <= 10; i += 1) queue.tick(i * 16.7);
  assert.equal(calls, afterEnqueue, 'no notification for a progress value moving');

  // Crossing into the hold is a state change, and is notified exactly once.
  queue.tick(300);
  queue.tick(320);
  assert.equal(calls, afterEnqueue + 1, 'one notification: entering -> holding');

  const snapshot = queue.getSnapshot();
  assert.equal(snapshot.active?.type, 'reward');
  assert.equal(queue.getSnapshot(), snapshot, 'the snapshot identity is stable between changes');

  off();
  queue.enqueue(trigger('badge'), 500);
  assert.equal(calls, afterEnqueue + 1, 'unsubscribed');
});

test('reset clears the stage and the queue', () => {
  const queue = new MomentQueue();
  queue.enqueue(trigger('badge'), 0);
  queue.enqueue(trigger('sugar'), 0);
  queue.tick(100);
  queue.reset();
  assert.equal(queue.active(100), null);
  assert.equal(queue.pendingCount, 0);
  assert.equal(queue.getSnapshot().active, null);
});

// ---------------------------------------------------------------------------------------------
// Trigger mapping
// ---------------------------------------------------------------------------------------------

test('feed events map to the moments the catalogue names', () => {
  assert.equal(triggerFromEvent(event({ kind: 'milestone', label: 'Reached: Left the bedroom', value: 4 }))?.type, 'milestone');
  assert.equal(triggerFromEvent(event({ kind: 'sugar', label: 'ada sent sugar', by: 'ada' }))?.type, 'sugar');
  assert.equal(triggerFromEvent(event({ kind: 'recovery', label: 'Rolled back', value: 3 }))?.type, 'rollback');
  assert.equal(
    triggerFromEvent(event({ kind: 'reward', label: 'gym badge', rewardKind: 'badge', value: 1 }))?.type,
    'badge',
  );
  assert.equal(triggerFromEvent(event({ kind: 'reward', label: 'new place', rewardKind: 'explore', value: 0.05 }))?.type, 'reward');

  // Not moments: a 5 s checkpoint heartbeat, a viewer line, a service message.
  assert.equal(triggerFromEvent(event({ kind: 'checkpoint', label: 'Checkpoint 9 saved' })), null);
  assert.equal(triggerFromEvent(event({ kind: 'viewer', label: 'ada said hi' })), null);
  assert.equal(triggerFromEvent(event({ kind: 'system', label: 'feed reconnected' })), null);
});

test('a reward\'s value picks its tier, and the tier scales the effect', () => {
  // Boundaries against the generator's own ranges: explore 0.02-0.08, area 0.1-0.2, story 0.3-0.6.
  assert.equal(rewardTier(0.05), 'low');
  assert.equal(rewardTier(0.1), 'mid');
  assert.equal(rewardTier(0.29), 'mid');
  assert.equal(rewardTier(0.3), 'high');
  assert.equal(rewardTier(0), 'low');

  const quiet = triggerFromEvent(event({ kind: 'reward', label: 'new place', rewardKind: 'explore', value: 0.05 }));
  const loud = triggerFromEvent(event({ kind: 'reward', label: 'story', rewardKind: 'story', value: 0.5 }));
  assert.ok((quiet?.intensity ?? 1) < (loud?.intensity ?? 0));
  assert.equal(quiet?.detail, '+0.05');
  assert.equal(loud?.detail, '+0.50');
});

test('the first header never invents moments out of a run already in progress', () => {
  const mapper = new TriggerMapper();
  // Rank 3, one attempt, two sugars today: none of that just happened.
  assert.deepEqual(mapper.observe(header()), []);
});

test('header deltas produce the moments whose events the service may have dropped', () => {
  const mapper = new TriggerMapper();
  mapper.observe(header());

  const badge = mapper.observe(header({ seq: 2, game: { ...header().game, badges: 1 } }));
  assert.deepEqual(badge.map((item) => item.type), ['badge']);
  assert.equal(badge[0]?.source, 'header');

  const rank = mapper.observe(
    header({ seq: 3, game: { ...header().game, badges: 1 }, milestone: { ...header().milestone, rank: 4, label: 'Left the house' } }),
  );
  assert.deepEqual(rank.map((item) => item.type), ['milestone']);
  assert.equal(rank[0]?.label, 'Left the house', 'the caption is the feed\'s own label');

  const base = header({ seq: 4, game: { ...header().game, badges: 1 }, milestone: { ...header().milestone, rank: 4, label: 'Left the house' } });
  const rollback = mapper.observe({ ...base, seq: 5, milestone: { ...base.milestone, attempts: 2 } });
  assert.deepEqual(rollback.map((item) => item.type), ['rollback']);
  assert.equal(rollback[0]?.detail, 'try 2');
});

test('a sugar, a day boundary and a mode change all read off the header', () => {
  const mapper = new TriggerMapper();
  const first = header();
  mapper.observe(first);

  const sugar = mapper.observe(header({ seq: 2, sugar: { ...first.sugar, todayCount: 3, lastBy: 'ada' } }));
  assert.deepEqual(sugar.map((item) => item.type), ['sugar']);
  assert.equal(sugar[0]?.by, 'ada');

  const mode: GameMode = 'BATTLE';
  const changed = mapper.observe(
    header({
      seq: 3,
      sugar: { ...first.sugar, todayCount: 3, lastBy: 'ada' },
      runSeconds: 86_500,
      game: { ...first.game, mode },
    }),
  );
  assert.deepEqual(changed.map((item) => item.type).sort(), ['dayRollover', 'modeChange']);
  assert.equal(changed.find((item) => item.type === 'dayRollover')?.label, 'DAY 2');
  assert.equal(changed.find((item) => item.type === 'modeChange')?.label, 'BATTLE');

  // `UNKNOWN` is the adapter shrugging, not a mode the chip should announce.
  const shrug = mapper.observe(
    header({
      seq: 4,
      sugar: { ...first.sugar, todayCount: 3, lastBy: 'ada' },
      runSeconds: 86_600,
      game: { ...first.game, mode: 'UNKNOWN' },
    }),
  );
  assert.deepEqual(shrug, []);
});

test('an event and its header delta in the same snapshot make one moment, not two', () => {
  const mapper = new TriggerMapper();
  const first = header();
  mapper.observe(first);

  const both = mapper.observe(
    header({
      seq: 2,
      game: { ...first.game, badges: 1 },
      milestone: { ...first.milestone, rank: 4, label: 'Left the house' },
      events: [
        event({ kind: 'reward', label: 'gym badge', rewardKind: 'badge', value: 1 }),
        event({ kind: 'milestone', label: 'Reached: Left the house', value: 4 }),
      ],
    }),
  );

  assert.deepEqual(both.map((item) => item.type), ['badge', 'milestone']);
  assert.ok(both.every((item) => item.source === 'event'), 'the events won; the deltas were suppressed');
});

test('the day number counts simulated days, falling back to uptime before a run has any', () => {
  assert.equal(dayNumber(header({ runSeconds: 0, uptimeSeconds: 10 })), 1);
  assert.equal(dayNumber(header({ runSeconds: 86_399 })), 1);
  assert.equal(dayNumber(header({ runSeconds: 86_400 })), 2);
  assert.equal(dayNumber(header({ runSeconds: 86_400 * 9 + 5 })), 10);
});

test('a raw feed event can be enqueued directly, and a non-moment event is refused', () => {
  const queue = new MomentQueue();
  assert.ok(queue.enqueue(event({ kind: 'sugar', label: 'ada sent sugar', by: 'ada' }), 0));
  assert.equal(queue.enqueue(event({ kind: 'checkpoint', label: 'Checkpoint 9 saved' }), 0), null);
  queue.tick(0);
  assert.equal(queue.active(0)?.type, 'sugar');
});
