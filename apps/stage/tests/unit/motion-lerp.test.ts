/**
 * The motion primitives: an approach that does not care what the frame rate is, and a tween that
 * lands on its end state exactly.
 *
 * Both properties are requirements rather than niceties. The page paints at 60 Hz over a 30 Hz feed
 * and replays fixtures against virtual time (`src/paint/loop.ts`, `src/feed/fixture.ts`), so a
 * per-frame lerp constant would animate at two different speeds depending on which clock is
 * driving; and `docs/design/animation.md` requires "every animation still has a static end state so
 * a frozen frame reads correctly", which means eased(1) has to be 1 and not 0.999.
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  approach,
  BAR_STEP,
  clamp01,
  cssSteps,
  EASINGS,
  mix,
  PIXEL_STEP,
  quantize,
  quantizePixels,
  Smoothed,
  SmoothedRecord,
  stepped,
  Tween,
} from '../../src/motion/lerp';

test('a smoothed value converges on its target and reports itself settled', () => {
  const value = new Smoothed(0, { tauMs: 200 });
  value.to(1);

  // Ten time constants. The remaining error is exp(-10), about 5e-5.
  for (let i = 0; i < 120; i += 1) value.tick(16.7);

  assert.ok(Math.abs(value.value - 1) < 1e-3, `converged to ${String(value.value)}`);
  assert.equal(value.settled(), true);
  assert.equal(value.target, 1);
});

test('the same second of smoothing gives the same answer at 30 and at 60 Hz', () => {
  const at60 = new Smoothed(0, { tauMs: 250 });
  const at30 = new Smoothed(0, { tauMs: 250 });
  at60.to(100);
  at30.to(100);

  // 1000 ms of wall time, sampled twice as often on one side as the other.
  for (let i = 0; i < 60; i += 1) at60.tick(1000 / 60);
  for (let i = 0; i < 30; i += 1) at30.tick(1000 / 30);

  // Not "close enough": exp(-a/tau) * exp(-b/tau) === exp(-(a+b)/tau), so the two agree to within
  // floating-point noise, and a regression to a per-frame constant would show up as a gap of tens.
  assert.ok(Math.abs(at60.value - at30.value) < 1e-9, `60 Hz ${String(at60.value)} vs 30 Hz ${String(at30.value)}`);
  // And both actually moved most of the way, so the test is not passing on two frozen values.
  assert.ok(at60.value > 95);
});

test('two half steps of approach compose into one whole step', () => {
  const once = approach(0, 1, 33.4, 180);
  const twice = approach(approach(0, 1, 16.7, 180), 1, 16.7, 180);
  assert.ok(Math.abs(once - twice) < 1e-12, `${String(once)} vs ${String(twice)}`);
});

test('approach degenerates safely: no time passing, or no smoothing at all', () => {
  assert.equal(approach(3, 9, 0, 180), 3);
  assert.equal(approach(3, 9, -5, 180), 3);
  assert.equal(approach(3, 9, 16, 0), 9);
});

test('asymmetric time constants make a flash rise fast and fall slow', () => {
  // The catalogue's "flashes 120 ms up and 600 ms down", as one value.
  const flash = new Smoothed(0, { riseTauMs: 40, fallTauMs: 200 });

  flash.to(1);
  for (let i = 0; i < 8; i += 1) flash.tick(16.7); // ~133 ms
  const afterRise = flash.value;
  assert.ok(afterRise > 0.9, `rise reached only ${String(afterRise)}`);

  flash.to(0);
  for (let i = 0; i < 8; i += 1) flash.tick(16.7); // the same ~133 ms back down
  assert.ok(flash.value > 0.4, `fall should still be well above zero, was ${String(flash.value)}`);
  assert.ok(flash.value < afterRise);
});

test('a smoothed record starts a new role at its first value and lerps after that', () => {
  const bars = new SmoothedRecord({ tauMs: 150 });

  // A role that appears mid-run is not an event: it must not sweep up from zero.
  bars.to('command_4', 40);
  assert.equal(bars.value('command_4'), 40);
  assert.equal(bars.size, 1);

  bars.to('command_4', 0);
  bars.tick(16.7);
  assert.ok(bars.value('command_4') < 40 && bars.value('command_4') > 0);

  bars.snap('command_4', 7);
  assert.equal(bars.value('command_4'), 7);
  bars.tick(16.7);
  assert.equal(bars.value('command_4'), 7);

  bars.toAll({ command_4: 1, command_5: 2 });
  assert.equal(bars.size, 2);
  assert.equal(bars.value('steer_left'), 0);
});

test('every easing lands exactly on 0 and 1', () => {
  for (const [name, ease] of Object.entries(EASINGS)) {
    assert.equal(ease(0), 0, `${name}(0)`);
    assert.equal(ease(1), 1, `${name}(1)`);
  }
});

test('out-back overshoots past 1 before it settles, which is the badge bounce', () => {
  const peak = Math.max(...Array.from({ length: 99 }, (_, i) => EASINGS.outBack((i + 1) / 100)));
  assert.ok(peak > 1.05, `out-back peaked at ${String(peak)}`);
  assert.ok(peak < 1.2, `out-back overshot too far: ${String(peak)}`);
});

test('in-out-cubic is symmetric about its midpoint', () => {
  assert.equal(EASINGS.inOutCubic(0.5), 0.5);
  for (const x of [0.1, 0.25, 0.4]) {
    assert.ok(Math.abs(EASINGS.inOutCubic(x) + EASINGS.inOutCubic(1 - x) - 1) < 1e-12);
  }
});

test('a tween ends exactly at its end state, and stays there', () => {
  const tween = new Tween({ durationMs: 320, easing: 'outExpo' });
  tween.start(1000);

  assert.equal(tween.value(1000), 0);
  assert.equal(tween.done(1000), false);
  assert.ok(tween.value(1160) > 0.9, 'out-expo should be most of the way by half time');

  assert.equal(tween.progress(1320), 1);
  assert.equal(tween.value(1320), 1);
  assert.equal(tween.done(1320), true);
  // Past the end it does not keep going, which is what a 9 s hold relies on.
  assert.equal(tween.value(99_000), 1);
  assert.equal(tween.between(1320, 20, 60), 60);
});

test('a tween honours its delay and reports its total time', () => {
  const tween = new Tween({ durationMs: 200, delayMs: 100, easing: 'linear' });
  tween.start(0);

  assert.equal(tween.progress(50), 0);
  assert.equal(tween.progress(200), 0.5);
  assert.equal(tween.done(299), false);
  assert.equal(tween.done(300), true);
  assert.equal(tween.totalMs, 300);
});

test('a tween is inert before start and after reset, and a zero duration is already finished', () => {
  const tween = new Tween({ durationMs: 240 });
  assert.equal(tween.started, false);
  assert.equal(tween.progress(5000), 0);
  assert.equal(tween.done(5000), false);

  tween.start(0);
  tween.reset();
  assert.equal(tween.startMs, null);

  const instant = new Tween({ durationMs: 0 });
  instant.start(0);
  assert.equal(instant.value(0), 1);
  assert.equal(instant.done(0), true);
});

test('the small helpers', () => {
  assert.equal(clamp01(-1), 0);
  assert.equal(clamp01(2), 1);
  assert.equal(clamp01(0.4), 0.4);
  assert.equal(mix(10, 20, 0.25), 12.5);
});

// ---------------------------------------------------------------------------------------------
// The stepped easing (docs/design/gameboy-theme.md, Motion)
// ---------------------------------------------------------------------------------------------

test('quantize snaps to a grid and pins nothing it should not', () => {
  assert.equal(quantize(7, 4), 8);
  assert.equal(quantize(5, 4), 4);
  assert.equal(quantize(6, 4), 8);
  assert.equal(quantize(0, 4), 0);
  assert.equal(quantize(-7, 4), -8);
  // "No grid" is a legal caller, not a division by zero.
  assert.equal(quantize(7.3, 0), 7.3);
});

test('quantizePixels turns a fraction into whole pixel steps, both ends exact', () => {
  // A 96 px track on the theme's 4 px grid is 24 steps, so the quantum is 1/24 and half way — 48 px
  // — is on the grid exactly.
  assert.equal(quantizePixels(0, 96, 4), 0);
  assert.equal(quantizePixels(1, 96, 4), 1);
  assert.equal(quantizePixels(0.5, 96, 4), 0.5);
  assert.equal(quantizePixels(0.5 + 1 / 48, 96, 4) * 96, 52);
  assert.equal(quantizePixels(0.5 + 1 / 96, 96, 4) * 96, 48);

  // Every value it reports lands on a whole 8 px cell of the track it was given.
  for (let i = 0; i <= 100; i += 1) {
    const px = quantizePixels(i / 100, 264, BAR_STEP) * 264;
    assert.ok(Math.abs(px - Math.round(px)) < 1e-9, `${String(px)} is not a whole pixel`);
    assert.ok(Math.round(px) % BAR_STEP === 0, `${String(px)} is not on the 8 px grid`);
  }

  // Out of range clamps rather than extrapolating: a bar cannot be 120% full.
  assert.equal(quantizePixels(-0.2, 100, 4), 0);
  assert.equal(quantizePixels(1.2, 100, 4), 1);

  // An unmeasured track is "no quantisation" rather than a divide by zero, which is what lets the
  // paint loop fall back safely on a frame before the slot has been laid out.
  assert.equal(quantizePixels(0.37, 0, 4), 0.37);
});

test('the spine cell has exactly three poses, all whole pixels', () => {
  // `src/motion/readouts.ts` maps a rung's fill onto `scaleY` over the growing half of a 16 px
  // cell, which is 8 px — two 4 px steps, so three poses: 8, 12 and 16 px.
  const poses = new Set<number>();
  for (let i = 0; i <= 200; i += 1) {
    const grown = quantizePixels(i / 200, 16 * 0.5, PIXEL_STEP);
    poses.add((0.5 + 0.5 * grown) * 16);
  }
  assert.deepEqual([...poses].sort((a, b) => a - b), [8, 12, 16]);
});

test('the stepped easing keeps out-expo shape, its endpoints and its monotonicity', () => {
  const ease = stepped(32, PIXEL_STEP);

  assert.equal(ease(0), 0);
  assert.equal(ease(1), 1);

  // Every value is a multiple of one 4 px step of the 32 px span, i.e. of 1/8.
  let previous = 0;
  for (let i = 0; i <= 200; i += 1) {
    const value = ease(i / 200);
    assert.ok(Math.abs(value * 8 - Math.round(value * 8)) < 1e-9, `${String(value)} is off the grid`);
    assert.ok(value >= previous, 'stepped easing must not go backwards');
    previous = value;
  }

  // Fast in, soft settle, still: out-expo is three quarters of the way by a fifth of the clip, and
  // the stepped form rounds to the same side of the grid.
  assert.ok(ease(0.2) >= 0.75);

  // And the named entry in the table is the same shape over the default 32 px span.
  assert.equal(EASINGS.stepped(0), 0);
  assert.equal(EASINGS.stepped(1), 1);
  assert.equal(EASINGS.stepped(0.5), ease(0.5));
});

test('cssSteps turns a distance into the step count the CSS timing function needs', () => {
  assert.equal(cssSteps(32, 4), 8);
  assert.equal(cssSteps(36, 4), 9);
  // Never zero: a zero-length move still has to produce a legal `steps()`.
  assert.equal(cssSteps(0, 4), 1);
  assert.equal(cssSteps(1, 4), 1);
});
