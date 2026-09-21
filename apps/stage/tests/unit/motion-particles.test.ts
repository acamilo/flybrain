/**
 * The particle layer's contract, which is mostly about what it must *not* do.
 *
 * `docs/design/animation.md`'s addendum caps it at 400 live particles with "decay 600 to 1200 ms",
 * and the budget section caps the whole page's frame paint at a p95 of 4 ms. A particle system is
 * the classic way to lose that budget, so the tests here are the cap, the decay, the "no draw calls
 * while nothing is alive" rule, and a measurement of a full pool's tick+draw against a 1 ms bar.
 *
 * `draw` is checked through a recording stub rather than a real canvas: the point is which calls the
 * layer makes and in what state (`lighter`, balanced save/restore), which is exactly what a headless
 * test can see and a screenshot cannot.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import { DECAY_MS, DIR, MAX_PARTICLES, ParticleField, type ParticlePainter } from '../../src/motion/particles';

/** A `ParticlePainter` that records what it was asked to draw. */
class RecordingPainter implements ParticlePainter {
  globalAlpha = 1;
  globalCompositeOperation = 'source-over';
  fillStyle: string | CanvasGradient | CanvasPattern = '#000';
  strokeStyle: string | CanvasGradient | CanvasPattern = '#000';
  lineWidth = 1;

  saves = 0;
  restores = 0;
  fills = 0;
  strokes = 0;
  paths = 0;
  readonly arcs: { x: number; y: number; radius: number }[] = [];
  readonly lines: { x: number; y: number }[] = [];
  readonly composites: string[] = [];
  readonly alphas: number[] = [];

  get calls(): number {
    return this.saves + this.restores + this.fills + this.strokes + this.paths + this.arcs.length + this.lines.length;
  }

  save(): void {
    this.saves += 1;
  }
  restore(): void {
    this.restores += 1;
  }
  beginPath(): void {
    this.paths += 1;
  }
  closePath(): void {}
  moveTo(): void {}
  lineTo(x: number, y: number): void {
    this.lines.push({ x, y });
  }
  arc(x: number, y: number, radius: number): void {
    this.arcs.push({ x, y, radius });
  }
  fill(): void {
    this.fills += 1;
    this.alphas.push(this.globalAlpha);
    // Recorded at paint time, not at `save()`: what matters is the state each batch is drawn in.
    this.composites.push(this.globalCompositeOperation);
  }
  stroke(): void {
    this.strokes += 1;
    this.composites.push(this.globalCompositeOperation);
  }
}

/** A painter that does nothing, for the performance measurement. */
class NullPainter implements ParticlePainter {
  globalAlpha = 1;
  globalCompositeOperation = 'source-over';
  fillStyle: string | CanvasGradient | CanvasPattern = '#000';
  strokeStyle: string | CanvasGradient | CanvasPattern = '#000';
  lineWidth = 1;
  save(): void {}
  restore(): void {}
  beginPath(): void {}
  closePath(): void {}
  moveTo(): void {}
  lineTo(): void {}
  arc(): void {}
  fill(): void {}
  stroke(): void {}
}

test('the pool is capped at 400 and refuses the overflow instead of growing', () => {
  const field = new ParticleField();
  assert.equal(field.capacity, MAX_PARTICLES);

  const spawned = field.sparks(100, 100, 1000, '#ffb020');
  assert.equal(spawned, MAX_PARTICLES);
  assert.equal(field.live, MAX_PARTICLES);
  assert.equal(field.dropped, 600, 'every refused emission is counted, not silently lost');

  // And a further burst on a full pool adds nothing at all.
  field.sparks(200, 200, 50, '#f472a8');
  assert.equal(field.live, MAX_PARTICLES);
});

test('a smaller capacity is honoured, and the cap is never above the design\'s 400', () => {
  assert.equal(new ParticleField({ capacity: 25 }).capacity, 25);
  assert.equal(new ParticleField({ capacity: 4000 }).capacity, MAX_PARTICLES);
});

test('particles decay inside the design\'s 600-1200 ms window and the pool empties', () => {
  const field = new ParticleField({ seed: 1 });
  field.sparks(100, 100, 40, '#ffb020');
  assert.equal(field.live, 40);

  // Nothing may die before the minimum decay.
  field.tick(DECAY_MS.min - 1);
  assert.equal(field.live, 40, 'a spark died before 600 ms');

  // And nothing may outlive the maximum.
  field.tick(DECAY_MS.max);
  assert.equal(field.live, 0, 'a spark outlived 1200 ms');
  assert.equal(field.idle, true);
});

test('a long stall does not teleport particles: the field integrates what it is given', () => {
  const field = new ParticleField({ seed: 2 });
  field.sparks(100, 100, 10, '#ffb020');
  // A negative or zero step is a no-op rather than a reversal (a fixture seek can hand one over).
  field.tick(0);
  field.tick(-50);
  assert.equal(field.live, 10);
});

test('draw touches the context only while something is alive', () => {
  const field = new ParticleField({ seed: 3 });
  const idle = new RecordingPainter();

  assert.equal(field.draw(idle), 0);
  assert.equal(idle.calls, 0, 'an empty field must not even save/restore the context');

  field.sparks(400, 300, 20, '#ffb020');
  const busy = new RecordingPainter();
  assert.ok(field.draw(busy) > 0);
  assert.ok(busy.arcs.length > 0);
  assert.equal(busy.saves, 1);
  assert.equal(busy.restores, 1, 'every context change is restored');
  assert.ok(busy.composites.length > 0);
  assert.ok(
    busy.composites.every((mode) => mode === 'lighter'),
    'every batch is painted additively, per the addendum',
  );

  // Once everything has decayed, the layer goes quiet again.
  field.tick(DECAY_MS.max + 1);
  const after = new RecordingPainter();
  assert.equal(field.draw(after), 0);
  assert.equal(after.calls, 0);
});

test('fills are batched: far fewer draw calls than particles', () => {
  const field = new ParticleField({ seed: 4 });
  field.sparks(400, 300, MAX_PARTICLES, '#ffb020');
  const painter = new RecordingPainter();
  const calls = field.draw(painter);

  assert.equal(painter.arcs.length, MAX_PARTICLES, 'every particle is drawn');
  assert.ok(calls <= 24, `${String(calls)} draw calls for 400 particles is not batched`);
  assert.equal(painter.fills, calls);
});

test('a particle dims as it ages', () => {
  const field = new ParticleField({ seed: 5 });
  field.sparks(400, 300, 30, '#ffb020');

  const young = new RecordingPainter();
  field.draw(young);
  field.tick(500);
  const old = new RecordingPainter();
  field.draw(old);

  const brightest = (painter: RecordingPainter) => Math.max(...painter.alphas);
  assert.ok(brightest(old) < brightest(young), 'the brightest batch did not dim with age');
});

test('the same seed draws the same burst twice, and a different seed does not', () => {
  const shoot = (seed: number) => {
    const field = new ParticleField({ seed });
    field.sparks(400, 300, 12, '#ffb020', DIR.up, Math.PI / 3);
    field.tick(100);
    const painter = new RecordingPainter();
    field.draw(painter);
    return painter.arcs.map((arc) => `${arc.x.toFixed(4)},${arc.y.toFixed(4)}`);
  };

  assert.deepEqual(shoot(11), shoot(11));
  assert.notDeepEqual(shoot(11), shoot(12));
});

test('the fountain rises from the bottom of its rect and falls back', () => {
  const rect = { x: 860, y: 88, width: 1012, height: 944 };
  const field = new ParticleField({ seed: 6 });
  field.fountain(rect, 60, '#ffb020');

  const at = (dtMs: number) => {
    field.tick(dtMs);
    const painter = new RecordingPainter();
    field.draw(painter);
    return painter.arcs.reduce((sum, arc) => sum + arc.y, 0) / Math.max(1, painter.arcs.length);
  };

  const start = at(1);
  const mid = at(300);
  assert.ok(mid < start, 'the fountain should be rising at 300 ms');
  const late = at(600);
  assert.ok(late > mid, 'and falling back by 900 ms');
  // It stays over the rail rather than escaping the frame.
  assert.ok(mid > rect.y - 40, `the fountain overshot the rail top by ${String(rect.y - mid)} px`);
});

test('drift converges on its target instead of merely being aimed at it', () => {
  const from = { x: 348, y: 882 };
  const to = { x: 1752, y: 198 };
  const field = new ParticleField({ seed: 7 });
  field.drift(from, to, 20, '#f472a8');

  const distance = () => {
    const painter = new RecordingPainter();
    field.draw(painter);
    const mean = painter.arcs.reduce(
      (sum, arc) => ({ x: sum.x + arc.x / painter.arcs.length, y: sum.y + arc.y / painter.arcs.length }),
      { x: 0, y: 0 },
    );
    return Math.hypot(to.x - mean.x, to.y - mean.y);
  };

  const before = distance();
  for (let i = 0; i < 30; i += 1) field.tick(16.7);
  const after = distance();
  assert.ok(after < before * 0.6, `the swarm closed only ${String(Math.round(before - after))} of ${String(Math.round(before))} px`);
});

test('the shockwave is one expanding, thinning stroke', () => {
  const field = new ParticleField({ seed: 8 });
  assert.equal(field.shockwave(1500, 176, '#ffb020', { radius: 300 }), 1);

  const radiusAt = (dtMs: number) => {
    field.tick(dtMs);
    const painter = new RecordingPainter();
    field.draw(painter);
    return { radius: painter.arcs[0]?.radius ?? 0, width: painter.lineWidth, strokes: painter.strokes };
  };

  const early = radiusAt(60);
  const later = radiusAt(300);
  assert.equal(early.strokes, 1);
  assert.ok(later.radius > early.radius, 'the ring should expand');
  assert.ok(later.radius <= 300, 'and never past its end radius');
  assert.ok(later.width < early.width, 'and thin as it goes');
});

test('the rewind field streams across the game box as strokes, not dots', () => {
  const game = { x: 48, y: 88, width: 800, height: 720 };
  const field = new ParticleField({ seed: 9 });
  field.streamField(game, DIR.left, 60, '#4fc3f7');

  const painter = new RecordingPainter();
  field.draw(painter);
  assert.equal(painter.arcs.length, 0, 'a line field draws no discs');
  assert.ok(painter.lines.length >= 60);
  assert.ok(painter.strokes > 0);

  // Every streak starts inside the game box.
  assert.ok(painter.lines.every((point) => point.y >= game.y - 40 && point.y <= game.y + game.height + 40));
});

test('clear empties the pool without reallocating it', () => {
  const field = new ParticleField({ seed: 10 });
  field.sparks(100, 100, 500, '#ffb020');
  assert.ok(field.dropped > 0);
  field.clear();
  assert.equal(field.live, 0);
  assert.equal(field.dropped, 0);
  assert.equal(field.capacity, MAX_PARTICLES);
  // The pool is reusable: a second burst fills it again.
  assert.equal(field.sparks(100, 100, 400, '#ffb020'), MAX_PARTICLES);
});

test('slots are recycled: a thousand bursts over a simulated minute stay inside the cap', () => {
  const field = new ParticleField({ seed: 11 });
  const painter = new NullPainter();
  let peak = 0;
  for (let frame = 0; frame < 3600; frame += 1) {
    if (frame % 12 === 0) field.sparks(400, 300, 8, '#ffb020');
    field.tick(16.7);
    field.draw(painter);
    peak = Math.max(peak, field.live);
  }
  assert.ok(peak <= MAX_PARTICLES);
  // A leak would show as a pool pinned at the cap with everything being dropped.
  assert.ok(field.live < MAX_PARTICLES, `steady state pinned at ${String(field.live)} live`);
});

test('400 particles tick and draw in well under a millisecond', () => {
  const field = new ParticleField({ seed: 12 });
  const painter = new NullPainter();

  // A full pool of the most expensive mix: discs, streaks and a ring, in two colours.
  const refill = () => {
    field.clear();
    field.sparks(400, 300, 200, '#ffb020', DIR.up, Math.PI / 2, { lifeMs: 1200 });
    field.streamField({ x: 48, y: 88, width: 800, height: 720 }, DIR.left, 150, '#4fc3f7', { lifeMs: 1200 });
    field.fountain({ x: 860, y: 88, width: 1012, height: 944 }, 49, '#ffb020', { lifeMs: 1200 });
    field.shockwave(1500, 176, '#ffb020', { lifeMs: 1200 });
    assert.equal(field.live, 400);
  };

  // Warm up, then measure 200 frames of a full pool.
  refill();
  for (let i = 0; i < 200; i += 1) {
    field.tick(1);
    field.draw(painter);
  }

  refill();
  const samples: number[] = [];
  for (let i = 0; i < 200; i += 1) {
    if (field.live < 400) refill();
    const started = performance.now();
    field.tick(1);
    field.draw(painter);
    samples.push(performance.now() - started);
  }
  samples.sort((a, b) => a - b);
  const p50 = samples[Math.floor(samples.length * 0.5)] as number;
  const p95 = samples[Math.floor(samples.length * 0.95)] as number;
  console.log(
    `  400-particle tick+draw: p50 ${p50.toFixed(3)} ms, p95 ${p95.toFixed(3)} ms, max ${(samples[samples.length - 1] as number).toFixed(3)} ms`,
  );

  assert.ok(p95 < 1, `p95 of ${p95.toFixed(3)} ms exceeds the 1 ms bar`);
});
