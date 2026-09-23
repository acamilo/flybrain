/**
 * Ticker pacing: the audit's "a badge and a +0.05 explore tick got identical treatment" finding,
 * as tests.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import type { FeedEvent, RewardKind } from '@flybrain/feed';
import { pokemonRed } from '../../src/games/pokemon-red';
import { TickerQueue } from '../../src/lib/ticker';

let nextId = 1;

function reward(kind: RewardKind, value = 0.05): FeedEvent {
  return { id: nextId++, wallMs: 0, brainMs: 0, kind: 'reward', label: `raw ${kind}`, value, rewardKind: kind };
}

function event(kind: FeedEvent['kind'], label: string, extra: Partial<FeedEvent> = {}): FeedEvent {
  return { id: nextId++, wallMs: 0, brainMs: 0, kind, label, ...extra };
}

test('a promoted row holds the panel for the minimum dwell', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 4000, visibleRows: 3 });

  queue.push(reward('trainer', 0.3), 0);
  assert.equal(queue.items().length, 1, 'the first row appears at once');

  // Three more arrive immediately; none may displace anything before the dwell elapses.
  queue.push(reward('story', 0.4), 100);
  queue.push(reward('pokedex', 0.2), 200);
  queue.push(reward('trainer', 0.3), 300);
  assert.equal(queue.items().length, 1, 'still one row inside the dwell window');
  assert.equal(queue.queued().length, 3);

  queue.tick(3999);
  assert.equal(queue.items().length, 1, 'one millisecond short still holds');

  queue.tick(4000);
  assert.equal(queue.items().length, 2, 'the gate opens exactly at the dwell');

  queue.tick(4100);
  assert.equal(queue.items().length, 2, 'and closes again immediately');

  queue.tick(8000);
  assert.equal(queue.items().length, 3);
});

test('the visible list is capped and newest-first', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0, visibleRows: 3 });
  const first = reward('trainer', 0.3);
  const second = reward('story', 0.4);
  const third = reward('pokedex', 0.2);
  const fourth = reward('trainer', 0.35);

  for (const [index, item] of [first, second, third, fourth].entries()) queue.push(item, index * 10);

  const items = queue.items();
  assert.equal(items.length, 3);
  assert.equal(items[0]?.id, fourth.id, 'newest is first');
  assert.ok(!items.some((item) => item.id === first.id), 'the oldest fell off');
});

test('exploration ticks collapse into one counted row inside the dedupe window', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0 });

  queue.push(reward('explore', 0.05), 0);
  queue.push(reward('explore', 0.05), 5000);
  queue.push(reward('explore', 0.04), 10_000);

  assert.equal(queue.items().length, 1, 'three ticks, one row');
  const row = queue.items()[0];
  assert.equal(row?.count, 3);
  assert.equal(row?.label, '3 new finds');
  assert.ok(Math.abs((row?.amount ?? 0) - 0.14) < 1e-9, 'values sum');
});

test('a tick past the dedupe window starts a new row', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0 });
  queue.push(reward('explore'), 0);
  queue.push(reward('explore'), 20_001);
  assert.equal(queue.items().length, 2);
});

test('kinds with dedupeMs 0 never collapse', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0 });
  queue.push(reward('trainer', 0.3), 0);
  queue.push(reward('trainer', 0.3), 10);
  assert.equal(queue.items().length, 2);
});

test('a moment-tier event jumps the dwell gate', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 4000 });
  queue.push(reward('trainer', 0.3), 0);
  queue.push(reward('badge', 3), 500);

  const items = queue.items();
  assert.equal(items.length, 2, 'the badge did not wait');
  assert.equal(items[0]?.tier, 'moment');
  assert.equal(items[0]?.label, pokemonRed.rewardCopy.badge.label);
});

test('reward rows use the config copy, not the service text', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0 });
  queue.push(reward('area', 0.15), 0);
  assert.equal(queue.items()[0]?.label, pokemonRed.rewardCopy.area.label);
});

test('non-reward rows keep the feed label, and checkpoints never show at all', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 0 });

  assert.equal(queue.push(event('checkpoint', 'Checkpoint 12 saved'), 0), false);
  assert.equal(queue.items().length, 0, 'a 5 s heartbeat is not news');

  queue.push(event('sugar', 'alex fed the fly sugar', { by: 'alex' }), 10);
  assert.equal(queue.items()[0]?.label, 'alex fed the fly sugar');
  assert.equal(queue.items()[0]?.by, 'alex');
  assert.equal(queue.items()[0]?.tier, 'moment');

  queue.push(event('recovery', 'Rolled back'), 20);
  assert.equal(queue.items()[0]?.tier, 'notable');
});

test('the queue is bounded, so a burst cannot grow without limit', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 4000, maxQueue: 5 });
  for (let i = 0; i < 50; i++) queue.push(reward('trainer', 0.3), 1 + i);
  assert.ok(queue.queued().length <= 5);
});

test('the version number changes only when the visible list changes', () => {
  const queue = new TickerQueue(pokemonRed, { minDwellMs: 4000 });
  const before = queue.version();
  queue.push(reward('trainer', 0.3), 0);
  const afterPromotion = queue.version();
  assert.notEqual(before, afterPromotion);

  queue.push(reward('story', 0.4), 10);
  assert.equal(queue.version(), afterPromotion, 'queued but not visible: no change');

  queue.tick(5000);
  assert.notEqual(queue.version(), afterPromotion);
});

test('the platformer config paces the same way with different copy', async () => {
  const { platformer } = await import('../../src/games/platformer');
  const queue = new TickerQueue(platformer, { minDwellMs: 0 });
  queue.push(reward('explore'), 0);
  queue.push(reward('explore'), 100);
  assert.equal(queue.items()[0]?.label, '2 new ground');
});
