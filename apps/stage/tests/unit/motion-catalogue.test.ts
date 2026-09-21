/**
 * The catalogue as data, and the engine that reads it.
 *
 * `docs/design/animation.md` asks for the catalogue to "stay reviewable", which in code means two
 * things a test can actually hold: every moment type has exactly one row, and every row only names
 * regions, anchors, colours and sound tiers that exist. The region arithmetic is checked too,
 * because rail layout v2's four rows have to close on the same 944 the left column fixes — an off-by-
 * 24 there would aim every particle at the wrong panel.
 *
 * The engine tests are the seam the later rail wiring will use: a trigger in, particles and one
 * sound cue out, with the reward's value tier scaling the burst.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { SFX_NAMES } from '../../src/audio/sfx';
import {
  MOMENT_CATALOGUE,
  MOTION_ANCHORS,
  MOTION_COLOURS,
  MOTION_REGIONS,
  recipeFor,
  regionsUnion,
  resolveMotionColours,
  type ColourToken,
  type MotionAnchor,
  type MotionRegion,
} from '../../src/motion/catalogue';
import { MotionEngine } from '../../src/motion/engine';
import { MOMENT_TIMING, MOMENT_TYPES, momentTotalMs, type MomentTrigger, type MomentType } from '../../src/motion/moments';
import { NEEDS_SAMPLE, SFX_TIERS, sfxCue, type SfxTier } from '../../src/motion/sfx-tiers';

function trigger(type: MomentType, intensity = 1): MomentTrigger {
  return { type, label: type, intensity, source: 'header' };
}

// ---------------------------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------------------------

test('every moment type has exactly one recipe, keyed by itself', () => {
  assert.deepEqual(Object.keys(MOMENT_CATALOGUE).sort(), [...MOMENT_TYPES].sort());
  for (const type of MOMENT_TYPES) {
    const recipe = recipeFor(type);
    assert.equal(recipe.type, type);
    assert.equal(recipe.timing, MOMENT_TIMING[type], 'the queue and the renderer share one timing row');
    assert.ok(recipe.regions.length > 0, `${type} animates nothing`);
    assert.ok(recipe.note.length > 10, `${type} has no catalogue citation`);
  }
});

test('every recipe names only regions, anchors and colours that exist', () => {
  const regions = new Set(Object.keys(MOTION_REGIONS) as MotionRegion[]);
  const anchors = new Set(Object.keys(MOTION_ANCHORS) as MotionAnchor[]);
  const colours = new Set(Object.keys(MOTION_COLOURS) as ColourToken[]);

  for (const type of MOMENT_TYPES) {
    const recipe = recipeFor(type);
    for (const region of recipe.regions) assert.ok(regions.has(region), `${type}: region ${region}`);
    for (const spec of recipe.emitters) {
      assert.ok(colours.has(spec.colour), `${type}: colour ${spec.colour}`);
      if ('at' in spec) assert.ok(anchors.has(spec.at), `${type}: anchor ${spec.at}`);
      if ('over' in spec) assert.ok(regions.has(spec.over), `${type}: region ${spec.over}`);
      if ('from' in spec) {
        assert.ok(anchors.has(spec.from), `${type}: anchor ${spec.from}`);
        assert.ok(anchors.has(spec.to), `${type}: anchor ${spec.to}`);
      }
      // An emitter cannot fire after the moment it belongs to has left the stage.
      assert.ok((spec.delayMs ?? 0) < momentTotalMs(type), `${type}: an emitter is delayed past the exit`);
    }
  }
});

test('the two 9 s moments are the two the design says, and nothing else holds that long', () => {
  // 9 s of total stage time, which is what the design's verification checks; see
  // `MOMENT_TIMING`'s comment on why the hold itself is 8360.
  const long = MOMENT_TYPES.filter((type) => momentTotalMs(type) >= 9000);
  assert.deepEqual(long.sort(), ['badge', 'milestone']);
  assert.equal(momentTotalMs('badge'), 9000);
  assert.equal(momentTotalMs('milestone'), 9000);
  // Arrivals stay inside the design's 240-320 ms band, except the rollback's own 400 ms wipe.
  for (const type of MOMENT_TYPES) {
    const { enterMs } = MOMENT_TIMING[type];
    assert.ok(enterMs <= 400, `${type} arrives over ${String(enterMs)} ms`);
  }
});

test('only the badge is allowed to fill the frame; everything else stays modest', () => {
  const particles = (type: MomentType) =>
    recipeFor(type).emitters.reduce((sum, spec) => sum + ('count' in spec ? spec.count : 1), 0);

  assert.ok(particles('badge') > particles('milestone'));
  for (const type of MOMENT_TYPES) {
    if (type === 'badge') continue;
    assert.ok(particles(type) <= 140, `${type} asks for ${String(particles(type))} particles`);
  }
});

test('the LADDER tab takes focus for exactly the moments the catalogue says', () => {
  const focused = MOMENT_TYPES.filter((type) => recipeFor(type).focusTab !== undefined);
  assert.deepEqual(focused.sort(), ['badge', 'milestone', 'rollback']);
  for (const type of focused) assert.equal(recipeFor(type).focusTab, 'LADDER');
});

test('rail layout v2 closes: four rows on 944, level with the game and the fly strip', () => {
  const rail = MOTION_REGIONS.rail;
  const rows: MotionRegion[] = ['progressCluster', 'tabSlot', 'ticker', 'chat'];

  // The rail spans the game plus the gutter plus the fly strip, exactly.
  assert.equal(rail.height, 944);
  assert.equal(rail.y, MOTION_REGIONS.game.y, 'the rail starts level with the top of the game');
  assert.equal(rail.y + rail.height, MOTION_REGIONS.flyStrip.y + MOTION_REGIONS.flyStrip.height);
  // And the usable box is filled to the 48 px insets on both sides.
  assert.equal(MOTION_REGIONS.titleStrip.x, 48);
  assert.equal(rail.x + rail.width, 1920 - 48);
  assert.equal(MOTION_REGIONS.flyStrip.y + MOTION_REGIONS.flyStrip.height, 1080 - 48);

  // The four rows tile the rail top to bottom with no overlap and no hole.
  let cursor = rail.y;
  for (const name of rows) {
    const row = MOTION_REGIONS[name];
    assert.equal(row.x, rail.x, `${name} x`);
    assert.equal(row.width, rail.width, `${name} width`);
    assert.ok(row.y >= cursor, `${name} overlaps the row above`);
    cursor = row.y + row.height;
  }
  assert.equal(cursor, rail.y + rail.height, 'the rows do not close on the rail height');
});

test('every anchor lands inside the stage, and inside the region it belongs to', () => {
  for (const [name, point] of Object.entries(MOTION_ANCHORS)) {
    assert.ok(point.x > 0 && point.x < 1920, `${name} x ${String(point.x)}`);
    assert.ok(point.y > 0 && point.y < 1080, `${name} y ${String(point.y)}`);
  }
  const inside = (point: { x: number; y: number }, region: MotionRegion) => {
    const box = MOTION_REGIONS[region];
    return point.x >= box.x && point.x <= box.x + box.width && point.y >= box.y && point.y <= box.y + box.height;
  };
  assert.ok(inside(MOTION_ANCHORS.flyHead, 'flyStrip'));
  assert.ok(inside(MOTION_ANCHORS.sugarChip, 'progressCluster'));
  assert.ok(inside(MOTION_ANCHORS.badgeCount, 'progressCluster'));
  assert.ok(inside(MOTION_ANCHORS.spineRung, 'progressCluster'));
  assert.ok(inside(MOTION_ANCHORS.tickerLine, 'ticker'));
});

test('regions union covers every region any moment touches', () => {
  const union = new Set(regionsUnion());
  for (const type of MOMENT_TYPES) for (const region of recipeFor(type).regions) assert.ok(union.has(region));
});

test('colours resolve to the token fallbacks with no DOM', () => {
  const palette = resolveMotionColours();
  for (const token of Object.keys(MOTION_COLOURS) as ColourToken[]) {
    assert.equal(palette[token], MOTION_COLOURS[token].fallback);
  }
});

// ---------------------------------------------------------------------------------------------
// Sound tiers
// ---------------------------------------------------------------------------------------------

test('every recipe\'s sound tier exists, and every tier plays something in the real bank', () => {
  for (const type of MOMENT_TYPES) {
    const tier = recipeFor(type).sfx;
    assert.ok(tier in SFX_TIERS, `${type}: tier ${tier}`);
  }
  for (const [tier, cue] of Object.entries(SFX_TIERS)) {
    if (cue.sfx === null) {
      assert.equal(tier, 'silent');
      continue;
    }
    assert.ok(SFX_NAMES.includes(cue.sfx), `${tier} plays ${cue.sfx}, which is not in the bank`);
    assert.ok(cue.gain > 0 && cue.gain <= 1);
  }
});

test('the design\'s five sound tiers map to the five moments that carry sound', () => {
  assert.equal(recipeFor('reward').sfx, 'tick');
  assert.equal(recipeFor('sugar').sfx, 'tone');
  assert.equal(recipeFor('milestone').sfx, 'chime');
  assert.equal(recipeFor('badge').sfx, 'fanfare');
  assert.equal(recipeFor('rollback').sfx, 'rewind');
  // The catalogue's "none" rows really are silent.
  assert.equal(recipeFor('modeChange').sfx, 'silent');
  assert.equal(sfxCue('silent').sfx, null);
});

test('every tier resolves to a real bank entry, and any stand-in is declared rather than hidden', () => {
  // The bank grew a rewind sweep and a stinger when the rail wiring landed, so `NEEDS_SAMPLE` is
  // empty and every sounding tier plays its own sample at full level. The loop still runs: it is
  // the check that the *next* gap is declared and pulled back in level rather than passing for the
  // sample it borrows.
  for (const tier of NEEDS_SAMPLE) {
    const cue = sfxCue(tier as SfxTier);
    assert.ok(cue.sfx !== null);
    assert.ok(cue.gain < 1, `${tier} borrows ${String(cue.sfx)} at full level`);
  }
  for (const tier of Object.keys(SFX_TIERS) as SfxTier[]) {
    const cue = sfxCue(tier);
    if (tier === 'silent') {
      assert.equal(cue.sfx, null);
      continue;
    }
    assert.ok(cue.sfx !== null, `${tier} plays nothing`);
    assert.ok(SFX_NAMES.includes(cue.sfx), `${tier} plays ${String(cue.sfx)}, which is not in the bank`);
  }
});

// ---------------------------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------------------------

test('a moment fires its emitters and queues exactly one sound cue', () => {
  let nowMs = 0;
  const engine = new MotionEngine({ clock: () => nowMs, seed: 42 });

  engine.enqueue(trigger('milestone'), 0);
  engine.tick(0);

  assert.ok(engine.field.live > 0, 'no particles');
  assert.equal(engine.cueCount, 1);
  const cue = engine.takeCue();
  assert.equal(cue?.tier, 'chime');
  assert.equal(cue?.cue.sfx, 'milestone');
  assert.equal(engine.takeCue(), null, 'a cue is taken once');

  // The delayed second burst has not fired yet, and does once its delay is up.
  const before = engine.field.live;
  nowMs = 100;
  engine.tick(100);
  assert.equal(engine.field.live, before, 'the delayed emitter fired early');
  nowMs = 300;
  engine.tick(300);
  assert.ok(engine.field.live > before, 'the delayed emitter never fired');
});

test('a reward\'s value tier scales the burst', () => {
  const run = (value: number) => {
    const engine = new MotionEngine({ clock: () => 0, seed: 7 });
    engine.enqueue(
      { type: 'reward', label: 'reward', value, intensity: value >= 0.3 ? 1 : 0.35, source: 'event' },
      0,
    );
    engine.tick(0);
    return engine.field.live;
  };

  const quiet = run(0.05);
  const loud = run(0.5);
  assert.ok(quiet > 0);
  assert.ok(loud > quiet, `low tier ${String(quiet)} was not quieter than high tier ${String(loud)}`);
});

test('a coalesced moment re-fires its burst without changing identity', () => {
  let nowMs = 0;
  const engine = new MotionEngine({ clock: () => nowMs, seed: 8 });
  engine.enqueue(trigger('sugar'), 0);
  engine.tick(0);
  const first = engine.field.live;
  const id = engine.active(0)?.id;

  nowMs = 600;
  engine.enqueue(trigger('sugar'), 600);
  engine.tick(600);
  assert.equal(engine.active(600)?.id, id);
  assert.equal(engine.active(600)?.count, 2);
  assert.ok(engine.field.live > first - 10, 'the second sugar produced no new sparks');
  assert.equal(engine.cueCount, 2, 'and it gets its own tone');
});

test('a silent moment queues no cue and emits nothing', () => {
  const engine = new MotionEngine({ clock: () => 0, seed: 9 });
  engine.enqueue(trigger('modeChange'), 0);
  engine.tick(0);
  assert.equal(engine.cueCount, 0);
  assert.equal(engine.field.live, 0);
  assert.equal(engine.active(0)?.type, 'modeChange');
});

test('the engine is only dirty while there is something to paint', () => {
  let nowMs = 0;
  const engine = new MotionEngine({ clock: () => nowMs, seed: 10 });
  assert.equal(engine.dirty, false);

  engine.enqueue(trigger('reward'), 0);
  engine.tick(0);
  assert.equal(engine.dirty, true);

  // Past the moment and past the longest particle decay.
  for (nowMs = 16.7; nowMs < 4000; nowMs += 16.7) engine.tick(nowMs);
  assert.equal(engine.dirty, false);
  assert.equal(engine.field.live, 0);
});

test('the paint-loop surface ticks and draws in one call', () => {
  let calls = 0;
  const painter = {
    globalAlpha: 1,
    globalCompositeOperation: 'source-over',
    fillStyle: '#000',
    strokeStyle: '#000',
    lineWidth: 1,
    save: () => {
      calls += 1;
    },
    restore: () => {},
    beginPath: () => {},
    closePath: () => {},
    moveTo: () => {},
    lineTo: () => {},
    arc: () => {},
    fill: () => {},
    stroke: () => {},
  };

  const engine = new MotionEngine({ clock: () => 0, seed: 11 });
  const surface = engine.surface(painter);
  assert.equal(surface.name, 'motion');

  engine.enqueue(trigger('badge'), 0);
  surface.draw(0, 16.7);
  assert.ok(engine.field.live > 0);
  assert.equal(calls, 1, 'the surface painted');
});

test('reset clears moments, particles and cues', () => {
  const engine = new MotionEngine({ clock: () => 0, seed: 12 });
  engine.enqueue(trigger('badge'), 0);
  engine.tick(0);
  engine.reset();
  assert.equal(engine.field.live, 0);
  assert.equal(engine.cueCount, 0);
  assert.equal(engine.active(0), null);
  assert.equal(engine.dirty, false);
});

test('a region lookup returns the v2 box', () => {
  const engine = new MotionEngine({ clock: () => 0 });
  assert.deepEqual(engine.region('ticker'), MOTION_REGIONS.ticker);
});
