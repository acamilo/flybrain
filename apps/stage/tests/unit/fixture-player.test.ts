/**
 * The seek semantics, driven in Node against an in-memory `.flyfeed`.
 *
 * This is the piece the mockup gate and every screenshot test stand on: seeking to t must leave
 * the page in the state it would have been in had it watched t seconds live, and must do it
 * without painting or sounding the history it skipped. Worth testing without a browser, because
 * in a browser the symptom is a screenshot that is subtly wrong.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { encodeFlyfeed, encodeSnapshot, type AttachmentKind, type FeedEvent, type FeedHeader } from '@flybrain/feed';
import { FixturePlayer } from '../../src/feed/fixture';
import { FeedIngest, hot, useStage } from '../../src/feed/store';
import { pokemonRed } from '../../src/games/pokemon-red';

const T0 = 1_700_000_000_000;
const FRAME_BYTES = 160 * 144 * 4;

function header(seq: number, overrides: Partial<FeedHeader> = {}): FeedHeader {
  return {
    protocol: 1,
    seq,
    wallMs: T0 + Math.round(((seq - 1) * 1000) / 30),
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: seq / 30,
    runSeconds: seq / 30,
    brainMs: Math.round((seq / 30) * 1000),
    frame: seq,
    buttons: 0,
    rates: { command_0: 12, reward_pam: 2 },
    populationRate: 13,
    spikeCount: 1,
    learning: { enabled: true, updates: seq, changed: 0, synapses: 100, signal: 0 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 1,
      rewardTotal: 0,
      rewardCounts: { story: 0, explore: 0, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
    },
    milestone: { rank: 0, label: 'booting', next: 'left the bedroom', sinceSeconds: 0, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
    events: [],
    attachments: ['frame', 'audio', 'spikes'],
    ...overrides,
  };
}

function snapshot(seq: number, overrides: Partial<FeedHeader> = {}): Uint8Array {
  const head = header(seq, overrides);
  const attachments: Partial<Record<AttachmentKind, Uint8Array>> = {};
  for (const kind of head.attachments) {
    if (kind === 'frame') attachments.frame = new Uint8Array(FRAME_BYTES).fill(seq % 256);
    if (kind === 'audio') attachments.audio = new Uint8Array(64);
    if (kind === 'spikes') attachments.spikes = new Uint8Array(4).fill(0b0000_0001);
  }
  return encodeSnapshot(head, attachments);
}

function reward(id: number, value: number): FeedEvent {
  return { id, wallMs: T0, brainMs: 0, kind: 'reward', label: 'raw', value, rewardKind: 'trainer' };
}

/** 300 snapshots = 10 s at 30 Hz, with a reward every second from t=1. */
function buildFixture(): Uint8Array {
  const messages: Uint8Array[] = [];
  for (let seq = 1; seq <= 300; seq++) {
    const events = seq % 30 === 0 ? [reward(seq / 30, 0.3)] : [];
    messages.push(snapshot(seq, { events }));
  }
  return encodeFlyfeed(
    {
      name: 'unit',
      protocol: 1,
      recordedAt: '2026-09-15T00:00:00.000Z',
      source: 'unit test',
      snapshotCount: 0,
      durationMs: 10_000,
      hz: 30,
      attachmentPolicy: { frame: { stride: 1 }, audio: { stride: 1 }, spikes: { stride: 1 } },
    },
    messages,
  );
}

/** Serve the fixture to `FixturePlayer.start()` without a server. */
function stubFetch(bytes: Uint8Array): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = (async () =>
    new Response(bytes as unknown as BodyInit, { status: 200 })) as typeof globalThis.fetch;
  return () => {
    globalThis.fetch = original;
  };
}

function freshIngest(): FeedIngest {
  const ingest = new FeedIngest(pokemonRed);
  ingest.reset();
  hot.frame = null;
  hot.spikes = null;
  hot.frameDirty = false;
  hot.spikesDirty = false;
  hot.accepted = 0;
  hot.decodeErrors = 0;
  return ingest;
}

test('a seek replays the whole history and lands on the right snapshot', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, { seekSeconds: 5, autoplay: false });
    await player.start();

    // 5 s at 30 Hz is snapshot 151 (the first is at t=0).
    assert.equal(hot.header?.seq, 151);
    assert.equal(hot.accepted, 151, 'every snapshot up to the target was ingested');

    ingest.commit(performance.now(), true);
    const state = useStage.getState();
    assert.ok(Math.abs(state.runSeconds - 151 / 30) < 1e-6);

    // Five rewards arrived in those five seconds, one a second from t=0.967. The 4 s dwell gate
    // promoted the first at once and the second at t=4.967; the rest are still queued. That is
    // the pacing working: a reward a second cannot flicker the panel.
    assert.equal(state.ticker.length, 2);
    assert.equal(ingest.queue().queued().length, 3);
    assert.equal(state.stale, false, 'a held seek is not stale: the clock is frozen with it');
  } finally {
    restore();
  }
});

test('the history is silent: only the landing snapshot paints and sounds', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, { seekSeconds: 5, autoplay: false });
    await player.start();

    // Only the snapshot the seek landed on is ingested audibly: 151 chunks would be five seconds
    // of fast-forwarded sound.
    assert.equal(hot.audioQueue.length, 1);
    assert.equal(hot.frameDirty, true, 'the last snapshot of the seek does paint');
    assert.ok(hot.frame, 'and it left a frame behind');
  } finally {
    restore();
  }
});

test('a held seek freezes the clock, so nothing drifts under a screenshot', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, { seekSeconds: 5, autoplay: false });
    await player.start();

    assert.equal(player.isPaused(), true);
    const frozen = player.clock(performance.now());
    const later = player.clock(performance.now() + 60_000);
    assert.equal(frozen, later, 'the clock does not advance while held');

    const before = hot.header?.seq;
    player.pump(performance.now() + 60_000);
    assert.equal(hot.header?.seq, before, 'and no further snapshots are ingested');
  } finally {
    restore();
  }
});

test('with autoplay the clock runs and later snapshots arrive', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, {
      seekSeconds: 5,
      autoplay: true,
      loop: false,
    });
    const startedAt = performance.now();
    await player.start();
    assert.equal(player.isPaused(), false);
    assert.equal(player.clock(startedAt + 1234), startedAt + 1234, 'the real clock is used');

    const seq = hot.header?.seq ?? 0;
    player.pump(performance.now() + 1000);
    assert.ok((hot.header?.seq ?? 0) > seq, 'one more second of playback arrived');
  } finally {
    restore();
  }
});

test('seeking twice from the same player is idempotent', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, { seekSeconds: 5, autoplay: false });
    await player.start();
    const first = { seq: hot.header?.seq, accepted: hot.accepted };

    player.seek(5, performance.now());
    assert.equal(hot.header?.seq, first.seq);
    assert.equal(hot.accepted, (first.accepted ?? 0) + (first.seq ?? 0), 'the replay happened again, cleanly');
  } finally {
    restore();
  }
});

test('the manifest and duration come back from the recording', async () => {
  const restore = stubFetch(buildFixture());
  try {
    const player = new FixturePlayer('/fixtures/unit.flyfeed', freshIngest(), { autoplay: false });
    await player.start();
    assert.equal(player.manifest()?.name, 'unit');
    assert.equal(player.manifest()?.snapshotCount, 300);
    assert.equal(player.durationMs(), 10_000);
  } finally {
    restore();
  }
});

test('a fixture with no decodable snapshot fails loudly rather than half-starting', async () => {
  // A bitset from a dataset with a different neuron count: the page must keep playing.
  const messages = [snapshot(1), snapshot(2), snapshot(3)];
  const file = encodeFlyfeed(
    {
      name: 'unit',
      protocol: 1,
      recordedAt: '2026-09-15T00:00:00.000Z',
      source: 'unit test',
      snapshotCount: 0,
      durationMs: 66,
      hz: 30,
      attachmentPolicy: {},
    },
    messages,
  );

  const restore = stubFetch(file);
  try {
    const ingest = freshIngest();
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, {
      seekSeconds: 0.1,
      autoplay: false,
      // The fixture's bitsets are 4 bytes; demand the real dataset's size.
      expectedSpikeBytes: 17_407,
    });

    await assert.rejects(() => player.start(), /no snapshot this page can decode/);
    assert.equal(hot.decodeErrors, 3, 'every snapshot was counted as it was rejected');
    assert.equal(hot.accepted, 0, 'and none was ingested');
  } finally {
    restore();
  }
});

test('a bad snapshot in the middle of a recording is skipped, not fatal', async () => {
  const good = snapshot(1);
  const bad = encodeSnapshot(header(2, { attachments: ['frame'] }), { frame: new Uint8Array(16) });
  const file = encodeFlyfeed(
    {
      name: 'unit',
      protocol: 1,
      recordedAt: '2026-09-15T00:00:00.000Z',
      source: 'unit test',
      snapshotCount: 0,
      durationMs: 100,
      hz: 30,
      attachmentPolicy: {},
    },
    [good, bad, snapshot(3)],
  );

  const restore = stubFetch(file);
  try {
    const ingest = freshIngest();
    // `loop: false`, because a looping player would pass the bad record again on the next lap
    // and count it again — which is correct, and would make the count here ambiguous.
    const player = new FixturePlayer('/fixtures/unit.flyfeed', ingest, {
      seekSeconds: 1,
      autoplay: false,
      loop: false,
    });
    await player.start();

    assert.equal(hot.decodeErrors, 1, 'the 16-byte frame was rejected');
    assert.equal(hot.accepted, 2, 'the two good snapshots played');
    assert.equal(hot.header?.seq, 3);
  } finally {
    restore();
  }
});
