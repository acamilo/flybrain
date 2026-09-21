import assert from 'node:assert/strict';
import test from 'node:test';
import { MotorDecoder } from './legacy/decoder';
import { BUTTON_BITS } from './legacy/protocol';
import { PopulationDecoder, type DecoderConfig, type DecoderState, type LegacyDecoderState } from '../src/readout/decoder';
import { BLOCKED_FATIGUE, BLOCKED_MS, GAMEBOY_BUTTON_BITS, fromButtonMask, gameboyDecoderConfig, toButtonMask } from '../src/readout/presets/gameboy';
import { platformerDecoderConfig } from '../src/readout/presets/platformer';
import { xorshift } from './fixtures/toy-dataset';

/** The values `infra/docs/room-escape.md` chose, spelled out here so a preset edit fails a test. */
const CHOSEN_HOLD_MS = 800;
const CHOSEN_FATIGUE_GAIN = 0.08;
const CHOSEN_HYSTERESIS = 1.05;

/**
 * The Game Boy preset with the exclusive group's timings pinned to `MotorDecoder`'s own hardcoded
 * constants: a 400 ms hold, a 400 ms decision period and a 0.08 fatigue gain.
 *
 * The oracle below proves that `PopulationDecoder` reproduces the prototype's fixed decoder
 * exactly. What it cannot prove is the preset's *numbers*, because the prototype's numbers are the
 * only ones `MotorDecoder` has: `docs/design/room-escape.md` section 1 raised the preset's hold and
 * lowered its fatigue gain, and had the oracle kept reading the live preset it would have started
 * failing for a reason that has nothing to do with the decoder's logic. So the oracle is given the
 * legacy numbers explicitly, and the live numbers get their own test
 * ("the Game Boy preset pins the values the room-escape measurement chose") below.
 */
function legacyGameboyConfig(): DecoderConfig {
  const config = gameboyDecoderConfig();
  return {
    ...config,
    exclusive: {
      ...config.exclusive!,
      decisionMs: 400,
      holdMs: 400,
      fatigueGain: 0.08,
      hysteresis: 1.15,
      blockedFatigue: 0,
      blockedMs: 0,
    },
  };
}

/** Eight command rates: mostly moderate, occasionally a hard spike. */
function commandRates(rng: () => number): Record<string, number> {
  const rates: Record<string, number> = {};
  for (let index = 0; index < 8; index += 1) rates[`command_${index}`] = rng() < 0.05 ? 60 : 30 * rng();
  return rates;
}

test('ORACLE: the Game Boy preset reproduces MotorDecoder over 20000 steps', () => {
  assert.deepEqual(GAMEBOY_BUTTON_BITS, BUTTON_BITS);
  const rng = xorshift(20260915);
  const baseline = commandRates(rng);
  const legacy = new MotorDecoder();
  const decoder = new PopulationDecoder(legacyGameboyConfig());
  legacy.calibrate(baseline);
  decoder.calibrate(baseline);
  let nowMs = 0;
  for (let step = 0; step < 20000; step += 1) {
    nowMs += 1 + Math.floor(rng() * 50);
    if (step > 0 && step % 4000 === 0) {
      legacy.clearHolds(nowMs);
      decoder.clearHolds(nowMs);
    }
    const boot = Math.floor(step / 3000) % 2 === 0;
    const rates = commandRates(rng);
    const expected = legacy.decode(rates, nowMs, boot);
    const active = decoder.decode(rates, nowMs, boot);
    assert.equal(toButtonMask(active), expected, `mask differs at step ${step} (t=${nowMs}, boot=${boot})`);
    assert.deepEqual(active, fromButtonMask(expected), `channel order differs at step ${step}`);
  }
  const before = legacy.exportState();
  const after = decoder.exportState();
  assert.deepEqual(after.baseline, before.baseline);
  assert.deepEqual(after.heldUntil, before.heldUntil);
  assert.deepEqual(after.nextAllowed, before.nextAllowed);
  assert.deepEqual(after.fatigue, before.fatigue);
  assert.equal(after.current, before.direction);
  assert.equal(after.nextDecision, before.nextDirectionDecision);
  assert.equal(after.calibrated, before.calibrated);
});

const flatBaseline = Object.fromEntries(Array.from({ length: 8 }, (_, index) => [`command_${index}`, 5]));

/**
 * The random sequence above never lands exactly on a hold, cooldown or threshold boundary, so it
 * cannot tell 400 ms from 399 ms. This second oracle run quantizes both the rates (frequent exact
 * ties, plus scores just above 1, just below 1.35 and far above both) and the clock (increments
 * that land exactly on every hold and cooldown edge). Verified to catch a one-unit change in every
 * preset parameter: both thresholds, all four holds, all three cooldowns, the throttle group, the
 * channel order, decisionMs, hysteresis, both fatigue terms and clearLockoutMs.
 */
test('ORACLE: the Game Boy preset reproduces MotorDecoder on hold, cooldown and threshold edges', () => {
  const rates = [5, 5.05, 5.5, 6.2, 7, 11, 20, 60];
  const clockSteps = [1, 54, 55, 56, 84, 85, 86, 399, 400, 401, 479, 480, 481, 2499, 2500, 2501];
  const rng = xorshift(99991);
  const legacy = new MotorDecoder();
  const decoder = new PopulationDecoder(legacyGameboyConfig());
  legacy.calibrate(flatBaseline);
  decoder.calibrate(flatBaseline);
  let nowMs = 0;
  for (let step = 0; step < 20000; step += 1) {
    nowMs += clockSteps[Math.floor(rng() * clockSteps.length)]!;
    if (step > 0 && step % 4000 === 0) {
      legacy.clearHolds(nowMs);
      decoder.clearHolds(nowMs);
    }
    const boot = Math.floor(step / 3000) % 2 === 0;
    const quantized: Record<string, number> = {};
    for (let index = 0; index < 8; index += 1) quantized[`command_${index}`] = rates[Math.floor(rng() * rates.length)]!;
    const expected = legacy.decode(quantized, nowMs, boot);
    assert.equal(toButtonMask(decoder.decode(quantized, nowMs, boot)), expected, `mask differs at step ${step} (t=${nowMs}, boot=${boot})`);
  }
  const before = legacy.exportState();
  const after = decoder.exportState();
  assert.deepEqual(after.baseline, before.baseline);
  assert.deepEqual(after.heldUntil, before.heldUntil);
  assert.deepEqual(after.nextAllowed, before.nextAllowed);
  assert.deepEqual(after.fatigue, before.fatigue);
  assert.equal(after.current, before.direction);
  assert.equal(after.nextDecision, before.nextDirectionDecision);
});

// --- Ported from fly-plays-pokemon tests/unit/decoder.test.ts -------------------------------------

function gameboyDecoder(config: DecoderConfig = legacyGameboyConfig()): (rates: Record<string, number>, nowMs: number, boot?: boolean) => number {
  const decoder = new PopulationDecoder(config);
  decoder.calibrate(flatBaseline);
  return (rates, nowMs, boot) => toButtonMask(decoder.decode(rates, nowMs, boot));
}

// The clock boundaries below are `MotorDecoder`'s 400 ms ones, so this test takes the legacy
// timings from `gameboyDecoder`'s default. The live preset's commitment is checked separately.
test('commitment survives reversals, hysteresis rejects small leads, system channels share a throttle', () => {
  const d = gameboyDecoder();
  assert.ok(d({ ...flatBaseline, command_0: 20, command_6: 20 }, 0, false) & GAMEBOY_BUTTON_BITS.start);
  assert.ok(d({ ...flatBaseline, command_1: 30, command_7: 20 }, 399, false) & GAMEBOY_BUTTON_BITS.up);
  assert.equal(d({ ...flatBaseline, command_0: 20, command_1: 21 }, 400, false) & 15, GAMEBOY_BUTTON_BITS.up);
  assert.equal(d({ ...flatBaseline, command_1: 30 }, 800, false) & 15, GAMEBOY_BUTTON_BITS.down);
  assert.equal(d({ ...flatBaseline, command_6: 20, command_7: 20 }, 2500, false) & (GAMEBOY_BUTTON_BITS.start | GAMEBOY_BUTTON_BITS.select), 0);
  assert.ok(d({ ...flatBaseline, command_6: 20 }, 30000, false) & GAMEBOY_BUTTON_BITS.start);
  const boot = gameboyDecoder();
  boot({ ...flatBaseline, command_6: 20 }, 0);
  assert.ok(boot({ ...flatBaseline, command_6: 20 }, 2500) & GAMEBOY_BUTTON_BITS.start);
});

test('decoder chooses only the strongest direction', () => {
  const mask = gameboyDecoder()({ ...flatBaseline, command_2: 20, command_3: 15 }, 100);
  assert.ok(mask & GAMEBOY_BUTTON_BITS.left);
  assert.equal(mask & GAMEBOY_BUTTON_BITS.right, 0);
});

test('decoder action channels respond to activity above baseline', () => {
  const mask = gameboyDecoder()({ ...flatBaseline, command_4: 10 }, 100);
  assert.ok(mask & GAMEBOY_BUTTON_BITS.a);
});

test('the Game Boy preset pins the values the room-escape measurement chose', () => {
  // `docs/design/room-escape.md` section 1 and `infra/docs/room-escape.md`. One overworld step in
  // Pokemon Red is about 270 ms, so a 400 ms commitment was one to two steps and the walker
  // jittered; the hold and the decision period rise together and the fatigue gain halves so a
  // committed direction can survive a re-decision. Everything else is the prototype's.
  const config = gameboyDecoderConfig();
  assert.deepEqual(config.exclusive, {
    channels: { up: 'command_0', down: 'command_1', left: 'command_2', right: 'command_3' },
    decisionMs: CHOSEN_HOLD_MS,
    holdMs: CHOSEN_HOLD_MS,
    hysteresis: CHOSEN_HYSTERESIS,
    fatigueGain: CHOSEN_FATIGUE_GAIN,
    fatigueDecay: 0.8,
    blockedFatigue: BLOCKED_FATIGUE,
    blockedMs: BLOCKED_MS,
  });
  assert.equal(config.clearLockoutMs, 480);
  assert.deepEqual(
    config.pulses,
    [
      { channel: 'a', role: 'command_4', holdMs: 85, cooldownMs: 480, threshold: 1 },
      { channel: 'b', role: 'command_5', holdMs: 85, cooldownMs: 480, threshold: 1 },
      { channel: 'start', role: 'command_6', holdMs: 55, cooldownMs: 30000, threshold: 1.35, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
      { channel: 'select', role: 'command_7', holdMs: 55, cooldownMs: 30000, threshold: 1.35, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
    ],
    'the action and system channels are untouched by the room-escape change',
  );
});

test('the Game Boy preset holds a direction for the chosen period and re-decides on the same beat', () => {
  // holdMs === decisionMs, so a winner never gaps and never overstays: the re-decision lands
  // exactly as the hold expires. Sampled at every millisecond around the boundary.
  const d = gameboyDecoder(gameboyDecoderConfig());
  assert.equal(d({ ...flatBaseline, command_0: 20 }, 0, false) & 15, GAMEBOY_BUTTON_BITS.up);
  assert.equal(
    d({ ...flatBaseline, command_1: 60 }, CHOSEN_HOLD_MS - 1, false) & 15,
    GAMEBOY_BUTTON_BITS.up,
    'a clear challenger does not interrupt the commitment',
  );
  assert.equal(
    d({ ...flatBaseline, command_1: 60 }, CHOSEN_HOLD_MS, false) & 15,
    GAMEBOY_BUTTON_BITS.down,
    'and takes over at the decision, not before',
  );
});

// --- Legacy checkpoint import --------------------------------------------------------------------

test('a legacy version 3 checkpoint resumes bit-identically in the new decoder', () => {
  const rng = xorshift(4242);
  const baseline = commandRates(rng);
  const legacy = new MotorDecoder();
  legacy.calibrate(baseline);
  let nowMs = 0;
  for (let step = 0; step < 500; step += 1) {
    nowMs += 1 + Math.floor(rng() * 50);
    legacy.decode(commandRates(rng), nowMs, step % 2 === 0);
  }
  const checkpoint = legacy.exportState();
  assert.equal(checkpoint.version, 3);
  const decoder = new PopulationDecoder(legacyGameboyConfig());
  decoder.importState(checkpoint);
  const imported = decoder.exportState();
  assert.deepEqual(imported.baseline, checkpoint.baseline);
  assert.deepEqual(imported.heldUntil, checkpoint.heldUntil);
  assert.deepEqual(imported.nextAllowed, checkpoint.nextAllowed);
  assert.deepEqual(imported.fatigue, checkpoint.fatigue);
  assert.equal(imported.current, checkpoint.direction);
  assert.equal(imported.nextDecision, checkpoint.nextDirectionDecision);
  for (let step = 0; step < 2000; step += 1) {
    nowMs += 1 + Math.floor(rng() * 50);
    const rates = commandRates(rng);
    const boot = step % 3 === 0;
    assert.equal(toButtonMask(decoder.decode(rates, nowMs, boot)), legacy.decode(rates, nowMs, boot), `mask differs at step ${step}`);
  }
});

test('a version 2 checkpoint (no version, no fatigue) imports rested', () => {
  const legacy = new MotorDecoder();
  legacy.calibrate(flatBaseline);
  legacy.decode({ ...flatBaseline, command_2: 20, command_4: 20 }, 0, false);
  const v3 = legacy.exportState();
  const v2: LegacyDecoderState = {
    calibrated: v3.calibrated,
    baseline: v3.baseline,
    heldUntil: v3.heldUntil,
    nextAllowed: v3.nextAllowed,
    nextDirectionDecision: v3.nextDirectionDecision,
    direction: v3.direction,
  };
  const decoder = new PopulationDecoder(legacyGameboyConfig());
  decoder.importState(v2);
  const state = decoder.exportState();
  assert.deepEqual(state.fatigue, { up: 0, down: 0, left: 0, right: 0 });
  assert.equal(state.calibrated, true);
  assert.equal(state.current, 'left');
  assert.deepEqual(state.heldUntil, v3.heldUntil);
  assert.deepEqual(state.nextAllowed, v3.nextAllowed);
  assert.equal(state.nextDecision, v3.nextDirectionDecision);
  assert.deepEqual(state.baseline, v3.baseline);
});

// --- Generic (non-preset) configurations ---------------------------------------------------------

/** Three-way exclusive group plus one pulse channel that has no boot variant. */
function genericConfig(): DecoderConfig {
  return {
    exclusive: {
      channels: { x: 'rate_x', y: 'rate_y', z: 'rate_z' },
      decisionMs: 100,
      holdMs: 100,
      hysteresis: 1.15,
      fatigueGain: 0.08,
      fatigueDecay: 0.8,
      blockedFatigue: 0,
      blockedMs: 0,
    },
    pulses: [{ channel: 'fire', role: 'rate_fire', holdMs: 20, cooldownMs: 200, threshold: 1 }],
    clearLockoutMs: 50,
  };
}

const genericBaseline = { rate_x: 5, rate_y: 5, rate_z: 5, rate_fire: 5 };

test('generic exclusive group: hysteresis holds the incumbent until fatigue lets the runner-up win', () => {
  const decoder = new PopulationDecoder(genericConfig());
  decoder.calibrate(genericBaseline);
  const tied = { ...genericBaseline, rate_x: 20, rate_y: 20 };
  // x wins outright: score 3.5 against 1.0 for the unstimulated channels.
  assert.deepEqual(decoder.decode({ ...genericBaseline, rate_x: 20 }, 0), ['x']);
  // With 0.08 fatigue, x reads 3.240 against y's 3.5 — an 8% lead, under the 15% margin, so x stays.
  assert.deepEqual(decoder.decode(tied, 100), ['x']);
  assert.equal(decoder.exportState().current, 'x');
  // A second decision of fatigue (0.16) drops x to 3.017; y's 3.5 now clears 1.15x and takes over.
  assert.deepEqual(decoder.decode(tied, 200), ['y']);
  const state = decoder.exportState();
  assert.equal(state.current, 'y');
  assert.equal(state.fatigue.y, 0.08);
  assert.ok(state.fatigue.x! > 0.12 && state.fatigue.x! < 0.16, `x fatigue decayed: ${state.fatigue.x}`);
  assert.equal(state.fatigue.z, 0);
  // clearHolds resets the group and locks every channel out for clearLockoutMs.
  decoder.clearHolds(250);
  const cleared = decoder.exportState();
  assert.equal(cleared.current, null);
  assert.deepEqual(cleared.fatigue, { x: 0, y: 0, z: 0 });
  assert.deepEqual(cleared.heldUntil, { x: 0, y: 0, z: 0, fire: 0 });
  assert.deepEqual(cleared.nextAllowed, { x: 300, y: 300, z: 300, fire: 300 });
  assert.equal(cleared.nextDecision, 250);
});

test('generic exclusive group: exact ties keep the earlier channel', () => {
  const idle = new PopulationDecoder(genericConfig());
  idle.calibrate(genericBaseline);
  assert.deepEqual(idle.decode(genericBaseline, 0), ['x']);
  const contested = new PopulationDecoder(genericConfig());
  contested.calibrate(genericBaseline);
  assert.deepEqual(contested.decode({ ...genericBaseline, rate_y: 20, rate_z: 20 }, 0), ['y']);
});

test('generic pulse channel without a boot variant behaves the same in and out of boot', () => {
  for (const boot of [true, false]) {
    const decoder = new PopulationDecoder(genericConfig());
    decoder.calibrate(genericBaseline);
    const hot = { ...genericBaseline, rate_fire: 20 };
    // Exclusive channels come first in the returned list, then pulses.
    assert.deepEqual(decoder.decode(hot, 0, boot), ['x', 'fire'], `boot=${boot}`);
    // 200 ms cooldown regardless of the boot flag.
    assert.deepEqual(decoder.decode(hot, 100, boot), ['x'], `boot=${boot}`);
    assert.deepEqual(decoder.decode(hot, 200, boot), ['y', 'fire'], `boot=${boot}`);
  }
});

test('an uncalibrated decoder reports nothing', () => {
  const decoder = new PopulationDecoder(genericConfig());
  assert.deepEqual(decoder.decode({ ...genericBaseline, rate_x: 99, rate_fire: 99 }, 0), []);
  assert.equal(decoder.exportState().calibrated, false);
});

test('a throttle group shares one cooldown across its pulse channels', () => {
  const config: DecoderConfig = {
    pulses: [
      { channel: 'p1', role: 'rate_1', holdMs: 10, cooldownMs: 100, threshold: 1, throttleGroup: 'shared' },
      { channel: 'p2', role: 'rate_2', holdMs: 10, cooldownMs: 100, threshold: 1, throttleGroup: 'shared' },
      { channel: 'solo', role: 'rate_3', holdMs: 10, cooldownMs: 100, threshold: 1 },
    ],
    clearLockoutMs: 40,
  };
  const decoder = new PopulationDecoder(config);
  decoder.calibrate({ rate_1: 0, rate_2: 0, rate_3: 0 });
  // p1 fires and throttles p2 in the same pass; solo keeps its own cooldown.
  assert.deepEqual(decoder.decode({ rate_1: 5, rate_2: 5, rate_3: 5 }, 0), ['p1', 'solo']);
  assert.deepEqual(decoder.exportState().nextAllowed, { p1: 100, p2: 100, solo: 100 });
  assert.deepEqual(decoder.decode({ rate_1: 5, rate_2: 5, rate_3: 5 }, 50), []);
  // Once the shared cooldown lapses either member may claim it; p2 then throttles p1.
  assert.deepEqual(decoder.decode({ rate_1: 0, rate_2: 5, rate_3: 0 }, 100), ['p2']);
  assert.deepEqual(decoder.decode({ rate_1: 5, rate_2: 0, rate_3: 5 }, 150), ['solo']);
  assert.deepEqual(decoder.exportState().nextAllowed, { p1: 200, p2: 200, solo: 250 });
});

test('invalid checkpoints throw without mutating the decoder', () => {
  const decoder = new PopulationDecoder(genericConfig());
  decoder.calibrate(genericBaseline);
  decoder.decode({ ...genericBaseline, rate_x: 20, rate_fire: 20 }, 0);
  const valid = decoder.exportState();
  const before = decoder.exportState();
  const rejected: unknown[] = [
    null,
    undefined,
    { ...valid, version: 5 },
    { ...valid, version: 1 },
    { ...valid, calibrated: 'yes' },
    { ...valid, nextDecision: Number.NaN },
    { ...valid, nextDecision: Number.POSITIVE_INFINITY },
    { ...valid, nextDecision: undefined },
    { ...valid, baseline: { ...valid.baseline, rate_x: Number.NaN } },
    { ...valid, baseline: undefined },
    { ...valid, heldUntil: {} },
    { ...valid, nextAllowed: { ...valid.nextAllowed, fire: undefined } },
    { ...valid, fatigue: undefined },
    { ...valid, fatigue: { ...valid.fatigue, x: 1.5 } },
    { ...valid, fatigue: { ...valid.fatigue, x: -0.1 } },
    { ...valid, fatigue: { ...valid.fatigue, x: Number.NaN } },
    { ...valid, current: 'fire' },
    { ...valid, current: 'nope' },
    // Legacy shapes: a missing decision timestamp, and a winner that is not an exclusive channel.
    { version: 3, calibrated: true, baseline: valid.baseline, heldUntil: valid.heldUntil, nextAllowed: valid.nextAllowed, fatigue: valid.fatigue, direction: null },
    { version: 3, calibrated: true, baseline: valid.baseline, heldUntil: valid.heldUntil, nextAllowed: valid.nextAllowed, fatigue: valid.fatigue, nextDirectionDecision: 0, direction: 'fire' },
  ];
  for (const state of rejected) {
    assert.throws(() => decoder.importState(state as DecoderState), /Invalid decoder/, `accepted ${JSON.stringify(state)}`);
    assert.deepEqual(decoder.exportState(), before, `mutated on ${JSON.stringify(state)}`);
  }
  decoder.importState(valid);
  assert.deepEqual(decoder.exportState(), before);
});

// --- The platformer preset -----------------------------------------------------------------------

/**
 * `docs/design/platformer.md` §4, "Decoder tests". Scores are `(rate + 1) / (baseline + 1)` against
 * a flat baseline of 5, so a rate of 5 scores exactly 1, 5.3 is B's threshold, 5.6 is A's, 11 is
 * exactly the system threshold of 2 and 12 is above it.
 */
const platformerBaseline = Object.fromEntries(Array.from({ length: 8 }, (_, index) => [`command_${index}`, 5]));

/** A rate record at the baseline, with the named roles overridden. */
function platformerRates(overrides: Record<string, number>): Record<string, number> {
  return { ...platformerBaseline, ...overrides };
}

function platformerDecoder(): PopulationDecoder {
  const decoder = new PopulationDecoder(platformerDecoderConfig());
  decoder.calibrate(platformerBaseline);
  return decoder;
}

test('platformer preset: a held direction never gaps, because holdMs equals decisionMs', () => {
  const decoder = platformerDecoder();
  const rates = platformerRates({ command_3: 60 });
  // Sampled every 10 ms for eight seconds, which includes every decision boundary exactly.
  for (let nowMs = 0; nowMs <= 8000; nowMs += 10) {
    assert.ok(decoder.decode(rates, nowMs, false).includes('right'), `right dropped out at ${nowMs} ms`);
  }
  // Habituation is bounded, so a sustained lead survives it: fatigue caps at 1, which halves the
  // winner's score and leaves it far above every rival.
  assert.equal(decoder.exportState().fatigue.right, 1);
  assert.equal(decoder.exportState().current, 'right');
});

test('platformer preset: B is a sustained hold that releases within 600 ms of the score dropping', () => {
  const decoder = platformerDecoder();
  const holding = platformerRates({ command_5: 30 });
  const released = platformerRates({});
  for (let nowMs = 0; nowMs <= 3000; nowMs += 50) {
    assert.ok(decoder.decode(holding, nowMs, false).includes('b'), `b gapped at ${nowMs} ms`);
  }
  // The cooldown (200 ms) is shorter than the hold (600 ms), so the channel refires before the hold
  // expires; that is the whole trick, and it means the release is late, not absent.
  let releasedAt: number | undefined;
  for (let nowMs = 3050; nowMs <= 5000; nowMs += 50) {
    if (!decoder.decode(released, nowMs, false).includes('b')) {
      releasedAt = nowMs;
      break;
    }
  }
  assert.ok(releasedAt !== undefined, 'b never released');
  assert.ok(releasedAt > 3000 && releasedAt <= 3600, `b released at ${releasedAt} ms`);
});

test('platformer preset: A fires at most once per 420 ms', () => {
  const decoder = platformerDecoder();
  const rates = platformerRates({ command_4: 60 });
  const fires: number[] = [];
  let previous = 0;
  for (let nowMs = 0; nowMs <= 6000; nowMs += 10) {
    decoder.decode(rates, nowMs, false);
    const nextAllowed = decoder.exportState().nextAllowed.a!;
    if (nextAllowed !== previous) {
      fires.push(nowMs);
      previous = nextAllowed;
    }
  }
  assert.ok(fires.length > 5, `A never settled into a cadence: ${fires.length} fires`);
  for (let index = 1; index < fires.length; index += 1) {
    assert.ok(fires[index]! - fires[index - 1]! >= 420, `A refired after ${fires[index]! - fires[index - 1]!} ms`);
  }
  // The 300 ms hold inside the 420 ms cooldown leaves a grounded gap for the landing. A fresh
  // decoder, because the one above has already run its clock out to 6,000 ms.
  const fresh = platformerDecoder();
  assert.ok(fresh.decode(rates, 0, false).includes('a'));
  assert.ok(fresh.decode(rates, 299, false).includes('a'), 'the jump is held for 300 ms');
  assert.ok(!fresh.decode(rates, 300, false).includes('a'), 'then released');
  assert.ok(!fresh.decode(rates, 419, false).includes('a'), 'and grounded until the cooldown ends');
  assert.ok(fresh.decode(rates, 420, false).includes('a'));
});

test('platformer preset: Start is blocked during play at threshold 2 and permissive in boot', () => {
  const decoder = platformerDecoder();
  // Exactly at the threshold is not above it. (The exclusive group always elects a winner, and at a
  // flat baseline that is `right`, first in the config; §4 discloses that tie-break prior.)
  assert.deepEqual(decoder.decode(platformerRates({ command_6: 11 }), 0, false), ['right']);
  for (let nowMs = 0; nowMs <= 30_000; nowMs += 100) {
    assert.ok(!decoder.decode(platformerRates({ command_6: 11, command_7: 11 }), nowMs, false).includes('start'));
  }
  // Above it, once, and then not for ten minutes: pausing is dead air.
  assert.ok(decoder.decode(platformerRates({ command_6: 12 }), 40_000, false).includes('start'));
  assert.equal(decoder.exportState().nextAllowed.start, 640_000);

  // In boot the permissive variant applies, which is how a run gets started at all.
  const booting = platformerDecoder();
  assert.ok(booting.decode(platformerRates({ command_6: 6 }), 0, true).includes('start'));
  assert.equal(booting.exportState().nextAllowed.start, 2500);
});

test('platformer preset: A, B, Start and Select are never active in the same frame', () => {
  // The soft reset at bank0.asm:1075 fires when all four are held at once, so this is a safety
  // property of the preset, not a nicety: the shared throttle makes the combination unreachable.
  const decoder = platformerDecoder();
  const rng = xorshift(4242);
  let nowMs = 0;
  let sawAction = false;
  let sawSystem = false;
  for (let step = 0; step < 20000; step += 1) {
    nowMs += 1 + Math.floor(rng() * 40);
    const boot = Math.floor(step / 500) % 2 === 0;
    const rates = Object.fromEntries(Object.keys(platformerBaseline).map((role) => [role, rng() < 0.5 ? 60 : 5]));
    const mask = toButtonMask(decoder.decode(rates, nowMs, boot));
    assert.notEqual(mask & 0b1111_0000, 0b1111_0000, `soft-reset combination at ${nowMs} ms`);
    assert.notEqual(mask & 0b1100_0000, 0b1100_0000, `Start and Select together at ${nowMs} ms`);
    sawAction ||= (mask & 0b0011_0000) !== 0;
    sawSystem ||= (mask & 0b1100_0000) !== 0;
  }
  assert.ok(sawAction && sawSystem, 'the sequence never fired the channels it is guarding');
});

test('platformer preset: clearHolds locks every pulse out for 300 ms', () => {
  const decoder = platformerDecoder();
  const rates = platformerRates({ command_4: 60, command_5: 60 });
  assert.deepEqual(decoder.decode(rates, 0, false), ['right', 'a', 'b']);
  decoder.clearHolds(1000);
  const state = decoder.exportState();
  assert.equal(state.current, null);
  for (const channel of Object.keys(state.nextAllowed)) {
    assert.equal(state.nextAllowed[channel], 1300, `${channel} lockout`);
  }
  // The lockout is a pulse cooldown, so the exclusive group re-decides immediately (`clearHolds`
  // sets `nextDecision` to now) while A and B stay out until 1,300.
  assert.deepEqual(decoder.decode(rates, 1299, false), ['right'], 'the pulses are still locked out');
  assert.deepEqual(decoder.decode(rates, 1300, false), ['right', 'a', 'b'], 'and free again at 300 ms');
  assert.equal(platformerDecoderConfig().clearLockoutMs, 300);
});

// --- The blocked-direction cooldown --------------------------------------------------------------

/** `genericConfig` with the blocked-direction cooldown switched on. */
function blockedConfig(blockedFatigue: number): DecoderConfig {
  const config = genericConfig();
  config.exclusive!.blockedFatigue = blockedFatigue;
  config.exclusive!.blockedMs = config.exclusive!.holdMs;
  return config;
}

function blockedDecoder(blockedFatigue: number): PopulationDecoder {
  const decoder = new PopulationDecoder(blockedConfig(blockedFatigue));
  decoder.calibrate(genericBaseline);
  return decoder;
}

test('blocked-direction cooldown: one report hands the hold to the runner-up', () => {
  // Two channels a few percent apart, which is what the live fly's direction scores look like: x
  // scores 3.5 and y 3.333. Hysteresis needs a 15% lead, so the spread cannot decide anything and
  // only habituation can. At fatigueGain 0.08 that takes x three more decisions to lose.
  const rates = { ...genericBaseline, rate_x: 20, rate_y: 19 };

  const patient = blockedDecoder(0);
  let held = 0;
  while (patient.decode(rates, held * 100, false)[0] === 'x') held += 1;
  assert.equal(held, 3, 'with the rule off, x keeps the hold for three more decisions');

  const decoder = blockedDecoder(0.35);
  assert.deepEqual(decoder.decode(rates, 0, false), ['x']);
  // The caller has now watched a whole hold go by with no movement and says so. x's score is
  // divided by 1.35, which puts y's 3.333 over the 1.15 threshold at once.
  assert.deepEqual(decoder.decode(rates, 100, false, 'x'), ['y']);
});

test('blocked-direction cooldown: the same block every frame is one penalty, not a ramp', () => {
  const rates = { ...genericBaseline, rate_x: 20 };
  const decoder = blockedDecoder(0.35);
  decoder.decode(rates, 0, false);
  // Thirty frames inside one hold, all reporting the same wall: fatigue is set, not accumulated,
  // so it cannot saturate to the cap and cannot outlive the decision that acts on it.
  for (let frame = 1; frame < 30; frame += 1) decoder.decode(rates, frame, false, 'x');
  assert.equal(decoder.exportState().fatigue.x, 0.35, 'max, not +=');
});

test('blocked-direction cooldown: never lowers fatigue, and is capped at 1', () => {
  const rates = { ...genericBaseline, rate_x: 20 };
  const decoder = blockedDecoder(0.35);
  for (let step = 0; step < 20; step += 1) decoder.decode(rates, step * 100, false);
  const earned = decoder.exportState().fatigue.x!;
  assert.ok(earned > 0.35, `twenty wins at 0.08 exceed 0.35, got ${earned}`);
  decoder.decode(rates, 2000, false, 'x');
  assert.ok(decoder.exportState().fatigue.x! >= earned, 'a report raises fatigue or leaves it be');

  const hard = blockedDecoder(4);
  hard.decode(rates, 0, false, 'x');
  assert.equal(hard.exportState().fatigue.x, 1, 'still bounded by the cap');
});

test('blocked-direction cooldown: ignored when off, or for a channel outside the group', () => {
  const rates = { ...genericBaseline, rate_x: 20 };

  const off = blockedDecoder(0);
  off.decode(rates, 0, false, 'x');
  assert.equal(off.exportState().fatigue.x, 0.08, "the winner's ordinary gain only");

  const decoder = blockedDecoder(0.35);
  decoder.decode(rates, 0, false, 'fire');
  decoder.decode(rates, 1, false, 'nonsense');
  assert.equal(decoder.exportState().fatigue.x, 0.08, 'no fatigue entry to raise');
});

test('blocked-direction cooldown: decode with no block is byte-identical to decode without one', () => {
  const rates = { ...genericBaseline, rate_x: 20, rate_y: 19 };
  const plain = blockedDecoder(0.35);
  const explicit = blockedDecoder(0.35);
  for (let step = 0; step < 10; step += 1) {
    const ms = step * 100;
    assert.deepEqual(plain.decode(rates, ms, false), explicit.decode(rates, ms, false, null));
  }
  assert.deepEqual(plain.exportState(), explicit.exportState());
});

test('gameboy preset: the blocked-direction cooldown is the preset constants', () => {
  const group = gameboyDecoderConfig().exclusive!;
  assert.equal(group.blockedFatigue, BLOCKED_FATIGUE);
  assert.equal(group.blockedMs, BLOCKED_MS);
  // The platformer keeps the rule off: `docs/design/platformer.md` §4 has not measured it there.
  assert.equal(platformerDecoderConfig().exclusive!.blockedFatigue, 0);
  assert.equal(platformerDecoderConfig().exclusive!.blockedMs, 0);
});

// --- The macro group (`docs/design/macros.md` sections 11 and 12) --------------------------------

/** The four macro channels these tests use, named as the dataset names the populations. */
const MACROS = ['macro_go_out', 'macro_go_item', 'macro_talk', 'macro_menu'];

/** The generic config plus a macro group over {@link MACROS}, on the direction group's numbers. */
function macroConfig(): DecoderConfig {
  const config = genericConfig();
  const group = config.exclusive!;
  return {
    ...config,
    macros: { ...group, channels: Object.fromEntries(MACROS.map(name => [name, name])) },
  };
}

const macroBaseline = { ...genericBaseline, ...Object.fromEntries(MACROS.map(name => [name, 5])) };

function macroDecoder(): PopulationDecoder {
  const decoder = new PopulationDecoder(macroConfig());
  decoder.calibrate(macroBaseline);
  return decoder;
}

test('the macro group is a second exclusive group over its own channels', () => {
  const decoder = macroDecoder();
  assert.deepEqual([...decoder.macroChannelNames], MACROS);
  // The macro channels come last, so every index a consumer already reads is untouched.
  assert.deepEqual([...decoder.channelNames], ['x', 'y', 'z', 'fire', ...MACROS]);

  // One winner in each group on the same decode, never two of either.
  const active = decoder.decode({ ...macroBaseline, rate_y: 20, macro_talk: 20 }, 0, false);
  assert.deepEqual(active, ['y', 'macro_talk']);
  assert.equal(decoder.macroWinner, 'macro_talk');
});

test('only the bound macro channels compete', () => {
  const decoder = macroDecoder();
  // The loudest population by far is not on this pad, so it cannot be pressed: the scene decides
  // which buttons exist (section 12). Masking, not penalising.
  const rates = { ...macroBaseline, macro_talk: 90, macro_go_item: 20 };
  const active = decoder.decode(rates, 0, false, null, ['macro_go_out', 'macro_go_item']);
  assert.ok(active.includes('macro_go_item'));
  assert.ok(!active.includes('macro_talk'));
  assert.equal(decoder.macroWinner, 'macro_go_item');

  // A scene that binds nothing takes no decision at all, and the clock does not move: the frame a
  // button appears on is a frame that can press it.
  const fresh = macroDecoder();
  assert.deepEqual(fresh.decode(rates, 0, false, null, []), ['x']);
  assert.equal(fresh.macroWinner, null);
  assert.ok(fresh.decode(rates, 1, false, null, ['macro_talk']).includes('macro_talk'));

  // And a channel the scene has taken away cannot hold its own seat: the 1.05 commitment bonus
  // belongs to an incumbent that is still in the running.
  assert.equal(decoder.macroWinner, 'macro_go_item');
  decoder.decode(rates, 200, false, null, ['macro_talk', 'macro_menu']);
  assert.equal(decoder.macroWinner, 'macro_talk');
});

test('the macro group commits and habituates exactly as the direction group does', () => {
  const preset = gameboyDecoderConfig(MACROS);
  const directions = preset.exclusive!;
  const macros = preset.macros!;
  assert.equal(macros.holdMs, directions.holdMs);
  assert.equal(macros.decisionMs, directions.decisionMs);
  assert.equal(macros.hysteresis, directions.hysteresis);
  assert.equal(macros.fatigueGain, directions.fatigueGain);
  assert.equal(macros.fatigueDecay, directions.fatigueDecay);
  assert.equal(macros.holdMs, CHOSEN_HOLD_MS);
  assert.equal(macros.hysteresis, CHOSEN_HYSTERESIS);
  assert.equal(macros.fatigueGain, CHOSEN_FATIGUE_GAIN);
  // No group at all when the caller names no channels, which is raw mode and every other game.
  assert.equal(gameboyDecoderConfig().macros, undefined);

  const decoder = macroDecoder();
  const tied = { ...macroBaseline, macro_go_out: 20, macro_go_item: 20 };
  decoder.decode(tied, 0, false);
  assert.equal(decoder.macroWinner, 'macro_go_out', 'ties keep the earlier channel');
  let swapped = false;
  for (let step = 1; step <= 40 && !swapped; step += 1) {
    decoder.decode(tied, step * 100, false);
    // Through a local, because `assert.equal` above narrowed the getter to the winner it had.
    const winner: string | null = decoder.macroWinner;
    swapped = winner === 'macro_go_item';
  }
  assert.ok(swapped, 'bounded habituation must move a tied winner on');
});

test('the direction group decodes identically with and without a macro group', () => {
  const plain = new PopulationDecoder(genericConfig());
  plain.calibrate(genericBaseline);
  const withMacros = macroDecoder();
  for (let step = 0; step < 200; step += 1) {
    const ms = (step + 1) * 17;
    const boot = step % 3 === 0;
    const rates = { ...genericBaseline, rate_x: 5 + (step % 7), rate_y: 5 + ((step * 3) % 5) };
    const buttons = withMacros
      .decode({ ...macroBaseline, ...rates, macro_talk: 40 }, ms, boot, null, ['macro_talk'])
      .filter(channel => !channel.startsWith('macro_'));
    assert.deepEqual(plain.decode(rates, ms, boot), buttons, `step ${step}`);
  }
});

test('a checkpoint from before the macro group loads and starts it rested', () => {
  const before = new PopulationDecoder(genericConfig());
  before.calibrate(genericBaseline);
  before.decode({ ...genericBaseline, rate_x: 20 }, 0, false);
  const old = before.exportState();
  assert.equal(old.version, 4, 'the schema version does not move for an additive group');

  const after = macroDecoder();
  after.decode({ ...macroBaseline, macro_talk: 40 }, 0, false);
  after.importState(old);
  assert.equal(after.macroWinner, null);
  const rested = after.exportState();
  for (const name of MACROS) assert.equal(rested.macroFatigue![name], 0, name);
  assert.equal(rested.current, 'x', 'and the buttons restore as they always did');

  // The restored checkpoint carried no baseline for any macro role, so the first decode after the
  // restore calibrates them from its own rates -- the rule warm-up follows, applied to the channels
  // warm-up never saw (`docs/readout.md`). Every macro channel therefore scores exactly 1.0 on
  // that decision: at rest, which is the honest reading of a channel nobody has measured. Before
  // this, an absent baseline read as zero and the score became `rate + 1`, so the group was decided
  // by which population happened to fire fastest -- live on 2026-09-17 that was 145 Hz against
  // TALK's 41, and TALK was never chosen in forty-eight minutes.
  assert.deepEqual([...after.pendingBaselineRoles], MACROS, 'every macro role is waiting');
  after.decode({ ...macroBaseline, macro_talk: 40 }, 100, false);
  assert.deepEqual([...after.pendingBaselineRoles], [], 'and calibrated by the first decode');
  for (const name of MACROS) {
    assert.equal(after.lastScores[name], 1, `${name} scores at rest`);
  }
  // The buttons are untouched: their roles were in the checkpoint, so they keep the baseline the
  // restore brought and this decode's resting rate scores them at 1.0 the way it always did.
  assert.equal(after.lastScores.x, 1);
  assert.equal(after.baselines.rate_x, genericBaseline.rate_x);

  // From the next decision on, a macro above its own baseline wins on merit. `macro_talk`'s
  // baseline is the 40 the restore measured, so it takes a rate above that.
  after.decode({ ...macroBaseline, macro_talk: 80 }, 1000, false);
  const full = after.exportState();
  assert.equal(full.macroCurrent, 'macro_talk');
  assert.ok(full.macroFatigue!.macro_talk! > 0);
  after.importState(full);
  assert.deepEqual(after.exportState(), full);

  assert.throws(
    () => after.importState({ ...full, macroCurrent: 'macro_nope' }),
    new Error('Invalid decoder channel'),
  );
  assert.throws(
    () => after.importState({ ...full, macroFatigue: { ...full.macroFatigue, macro_talk: 1.5 } }),
    new Error('Invalid decoder fatigue'),
  );
});

test('a restored group with no baselines scores every channel at rest, then on merit', () => {
  // The rule on its own, without the schema assertions around it: a checkpoint that predates a
  // channel group leaves those roles with no baseline, and an absent baseline used to read as zero.
  // The live shape of that (2026-09-17, forty-eight minutes in Oak's lab): raw rates of 27 to 145 Hz
  // deciding the group, and `macro_talk` at 41 unable to beat `macro_frontier` at 54.
  const before = new PopulationDecoder(genericConfig());
  before.calibrate(genericBaseline);
  before.decode(genericBaseline, 0, false);

  const after = macroDecoder();
  after.importState(before.exportState());
  // The live rates, in the order the log reported them, as far apart as they really are.
  const live = {
    ...genericBaseline,
    macro_go_out: 145,
    macro_go_item: 107,
    macro_talk: 41,
    macro_menu: 27,
  };
  after.decode(live, 0, false);
  for (const name of MACROS) {
    assert.equal(after.lastScores[name], 1, `${name} scores at rest, not at its raw rate`);
    assert.equal(after.baselines[name], live[name as keyof typeof live], `${name} kept its rate`);
  }
  // A tie at 1.0 is decided by the group's own tie rule -- the first channel -- and not by 145 Hz.
  assert.equal(after.macroWinner, MACROS[0]);

  // And then the group works: the quietest channel of the four wins the moment it rises above the
  // rest it was measured at, which is the thing the bug made impossible.
  after.decode({ ...live, macro_menu: 60 }, 1000, false);
  assert.equal(after.macroWinner, 'macro_menu');
});

test('clearing holds rests both groups', () => {
  const decoder = macroDecoder();
  decoder.decode({ ...macroBaseline, macro_talk: 40 }, 0, false);
  assert.equal(decoder.macroWinner, 'macro_talk');
  decoder.clearHolds(500);
  const state = decoder.exportState();
  assert.equal(state.current, null);
  assert.equal(state.macroCurrent, null);
  assert.equal(state.macroNextDecision, 500);
  for (const name of MACROS) {
    assert.equal(state.macroFatigue![name], 0, name);
    assert.equal(state.heldUntil[name], 0, name);
  }
});
