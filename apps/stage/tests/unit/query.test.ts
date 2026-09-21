/**
 * The query string is the page's entire configuration surface, and the two things that launch it
 * are a systemd kiosk line and a Playwright test. A wrong default here is a broadcast-wide bug.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { resolveGame } from '../../src/games';
import { parseStageOptions } from '../../src/lib/query';

test('the defaults are player mode, the steady fixture, T1 and native 1080p', () => {
  const options = parseStageOptions('');
  assert.equal(options.mode, 'player');
  assert.equal(options.fixture, 'steady');
  assert.equal(options.theme, 't1');
  assert.equal(options.res, '1080');
  assert.equal(options.game, 'pokemon-red');
  assert.equal(options.seekSeconds, null);
  assert.equal(options.autoplay, true, 'with no seek the page plays');
  assert.equal(options.loop, true);
  assert.equal(options.audio, true);
});

test('a seek without &play=1 holds, which is what makes a screenshot deterministic', () => {
  const held = parseStageOptions('?t=95');
  assert.equal(held.seekSeconds, 95);
  assert.equal(held.autoplay, false);

  const playing = parseStageOptions('?t=95&play=1');
  assert.equal(playing.seekSeconds, 95);
  assert.equal(playing.autoplay, true);
});

test('a seek accepts the design document\'s "95s" spelling as well as "95"', () => {
  assert.equal(parseStageOptions('?t=95s').seekSeconds, 95);
  assert.equal(parseStageOptions('?t=17.5').seekSeconds, 17.5);
});

test('garbage never takes the page down, it falls back', () => {
  assert.equal(parseStageOptions('?t=banana').seekSeconds, null);
  assert.equal(parseStageOptions('?theme=t9').theme, 't1');
  assert.equal(parseStageOptions('?res=4k').res, '1080');
  assert.equal(parseStageOptions('?mode=nonsense').mode, 'player');
  assert.equal(parseStageOptions('?gain=abc').gains.master, 0.9);
});

test('live mode and a feed override', () => {
  const options = parseStageOptions('?mode=live&feed=ws://10.0.0.5:7400/feed');
  assert.equal(options.mode, 'live');
  assert.equal(options.feedUrl, 'ws://10.0.0.5:7400/feed');
  assert.equal(parseStageOptions('?mode=live').feedUrl, 'ws://127.0.0.1:7400/feed');
});

test('gains come from the query with the documented defaults', () => {
  assert.deepEqual(parseStageOptions('').gains, { master: 0.9, game: 0.8, sfx: 0.5 });
  assert.deepEqual(parseStageOptions('?gain=0.5&gamegain=0.2&sfxgain=0').gains, {
    master: 0.5,
    game: 0.2,
    sfx: 0,
  });
});

test('flags accept 0 and false as off', () => {
  assert.equal(parseStageOptions('?audio=0').audio, false);
  assert.equal(parseStageOptions('?audio=false').audio, false);
  assert.equal(parseStageOptions('?audio=1').audio, true);
  assert.equal(parseStageOptions('?loop=0').loop, false);
});

test('an unknown ?game= falls back rather than throwing on air', () => {
  assert.equal(resolveGame('pokemon-red').id, 'pokemon-red');
  assert.equal(resolveGame('platformer').id, 'platformer');
  assert.equal(resolveGame('does-not-exist').id, 'pokemon-red');
  assert.equal(resolveGame(null).id, 'pokemon-red');
  assert.equal(resolveGame(undefined).id, 'pokemon-red');
});
