import assert from 'node:assert/strict';
import test from 'node:test';

import { canonicalize, digestOf, sha256Hex } from '../src/canonical';
import {
  readAgentRollbackParams,
  readAgentRollbackResult,
  readRestoreSlotParams,
  validateAgentRollbackAgainstScope,
  validateAgentRollbackResultAgainstScope,
  validateRestoreSlotAgainstScope,
} from '../src/extensions';
import * as fixtures from '../src/fixtures';
import * as gameboy from '../src/gameboy';
import { addRational, divideFloor, RATIONAL_ZERO, type Scope } from '../src/scalar';
import { EPISODE_REQUEST_KINDS, readEpisodeRequest } from '../src/workers';

const legacy = () => fixtures.load('gameboy-legacy.json') as Record<string, any>;

test('every registered schema reference is the digest of its declaration, in this language', () => {
  const file = legacy();
  const declared = new Map<string, unknown>(
    (file.extensionSet.payloadSchemas as Record<string, any>[]).map((entry) => [
      entry.declaration.id as string,
      entry.declaration,
    ]),
  );
  for (const schema of gameboy.PAYLOAD_SCHEMAS) {
    assert.deepEqual(file.schemaRefs[schema.id], schema, `${schema.id}: the constant is the fixture`);
    assert.equal(digestOf(declared.get(schema.id)), schema.digest, `${schema.id}: recomputed here`);
  }
  assert.equal(digestOf(file.extensionSet), file.extensionSetDigest);
});

test('the legacy profile document is the one profile and its digest is the constant', () => {
  const file = legacy();
  const document = file.profile.document;
  const profile = gameboy.readLegacyGameboyProfile(document);
  assert.equal(canonicalize(profile), file.profile.canonical, 'reading keeps every field');
  assert.equal(sha256Hex(file.profile.canonical), gameboy.PROFILE_DIGEST);
  assert.equal(file.profile.assetRef.digest, gameboy.PROFILE_DIGEST);
  assert.equal(file.profile.assetRef.byteLength, String(file.profile.canonical.length));
  assert.equal(profile.kernelVersion, 'lif-1ms-f64-v2');
  assert.equal(profile.plasticityVersion, 'fly-kc-mbon-rstdp-v2');
  assert.equal(profile.datasetFingerprint.split(':').length, 7);
});

test('the frame clock is exact and matches the legacy f64 accumulator', () => {
  const legacyMsPerFrame = 1000 / (4_194_304 / 70_224);
  assert.equal(legacyMsPerFrame, 548_625 / 32_768, 'the legacy constant is dyadic, so exact');
  const frames = legacy().clock.frames as { ticks: string; remainder: unknown }[];
  let exact = RATIONAL_ZERO;
  let float = 0;
  for (let frame = 0; frame < 20_000; frame += 1) {
    exact = addRational(exact, gameboy.STEP_DURATION);
    const { ticks, remainder } = divideFloor(exact, gameboy.TICK_DURATION);
    exact = remainder;
    float += legacyMsPerFrame;
    const steps = Math.floor(float);
    float -= steps;
    assert.equal(ticks, String(steps), `frame ${frame}: tick counts agree`);
    // The f64 remainder is a multiple of 2^-15 ms; compare it as an exact fraction of ns.
    const scaled = float * 32_768;
    assert.equal(scaled, Math.floor(scaled), `frame ${frame}: dyadic`);
    assert.equal(
      BigInt(remainder.numerator) * 32_768n,
      BigInt(scaled) * 1_000_000n * BigInt(remainder.denominator),
      `frame ${frame}: remainders agree`,
    );
    if (frame < frames.length) {
      assert.equal(frames[frame]!.ticks, ticks);
      assert.deepEqual(frames[frame]!.remainder, remainder);
    }
  }
});

test('the example composition digest is the recorded one, and the decoder moves it', () => {
  const file = legacy();
  const composition = gameboy.readLegacyGameboyComposition(file.composition.example);
  assert.equal(gameboy.compositionDeclarationDigest(composition), file.composition.digest);
  const other = structuredClone(composition);
  other.decoderConfigDigest = sha256Hex('another decoder');
  assert.notEqual(gameboy.compositionDeclarationDigest(other), file.composition.digest);
  assert.equal(other.flysimCompatibility, composition.flysimCompatibility);
});

test('bound is an ordered subset, and the macro winner is always bound', () => {
  const channels = ['macro_go_objective', 'macro_talk', 'macro_next'];
  const context = gameboy.readGameboyReadoutContext({
    boot: false,
    bound: ['macro_go_objective', 'macro_next'],
    location: null,
  });
  gameboy.validateReadoutContextAgainst(context, channels);
  assert.throws(() =>
    gameboy.validateReadoutContextAgainst({ ...context, bound: ['macro_next', 'macro_go_objective'] }, channels),
  );
  const decision = gameboy.readGameboyChannelsDecision({
    buttons: gameboy.GAMEBOY_BUTTONS.map((id) => ({ id, down: id === 'up' || id === 'a' })),
    macro: 'macro_next',
  });
  assert.equal(gameboy.channelsMask(decision), 0x11);
  gameboy.validateDecisionAgainst(decision, context);
  assert.throws(() => gameboy.validateDecisionAgainst({ ...decision, macro: 'macro_talk' }, context));
});

test('typed values are read only under their registered schema', () => {
  const value = { boot: true, bound: [], location: null };
  gameboy.readGameboyReadoutContextTyped({ schema: gameboy.READOUT_CONTEXT_SCHEMA, value });
  assert.throws(() => gameboy.readGameboyReadoutContextTyped({ schema: gameboy.CHANNELS_SCHEMA, value }));
  assert.throws(() =>
    gameboy.readGameboyReadoutContextTyped({
      schema: { ...gameboy.READOUT_CONTEXT_SCHEMA, digest: sha256Hex('x') },
      value,
    }),
  );
});

test('a rollback request and the extension methods check what they must', () => {
  assert.deepEqual([...EPISODE_REQUEST_KINDS], ['terminal', 'rollback']);
  const request = readEpisodeRequest({
    kind: 'rollback',
    reason: 'stall',
    outcome: { schema: gameboy.ROLLBACK_REQUEST_SCHEMA, value: { slotId: 'best', trigger: 'stall' } },
  });
  assert.equal(gameboy.readLegacyRatchetRollbackRequestTyped(request.outcome).slotId, 'best');

  const newEpoch: Scope = { sessionId: 'live', epoch: 'epoch-8', step: '4101' };
  const sameEpoch: Scope = { sessionId: 'live', epoch: 'epoch-7', step: '4101' };
  const restore = readRestoreSlotParams({
    slotId: 'best',
    priorEpoch: 'epoch-7',
    policy: 'legacy-ratchet-rollback-v1',
  });
  validateRestoreSlotAgainstScope(restore, newEpoch);
  assert.throws(() => validateRestoreSlotAgainstScope(restore, sameEpoch));

  const cases = fixtures.cases(fixtures.load('valid.json')) as Record<string, any>[];
  const find = (name: string) => cases.find((item) => item.name === name)!.value;
  const rollback = readAgentRollbackParams(find('agent rollback params'));
  validateAgentRollbackAgainstScope(rollback, newEpoch);
  assert.throws(() => validateAgentRollbackAgainstScope(rollback, { ...newEpoch, step: '4102' }));
  const result = readAgentRollbackResult(find('agent rollback result'));
  validateAgentRollbackResultAgainstScope(result, newEpoch);
  assert.throws(() => validateAgentRollbackResultAgainstScope(result, { ...newEpoch, step: '4100' }));
});

test('the decoderConfigDigest vectors are what the TypeScript oracle preset computes', async () => {
  const { gameboyDecoderConfig } = await import('@flybrain/brain');
  const file = fixtures.load('gameboy-decoder-config.json') as Record<string, any>;
  const cases = file.cases as Record<string, any>[];
  assert.deepEqual(
    cases.map((item) => item.name),
    ['raw', 'macros'],
  );
  for (const item of cases) {
    const form = gameboy.decoderConfigForm(gameboyDecoderConfig(item.macroChannels as string[]));
    assert.deepEqual(form, item.form, `${item.name}: the oracle's form is the Rust twin's`);
    assert.equal(canonicalize(form), item.canonical, `${item.name}: canonical bytes`);
    assert.equal(
      gameboy.decoderConfigDigest(gameboyDecoderConfig(item.macroChannels as string[])),
      item.digest,
      `${item.name}: digest`,
    );
  }
  const reversed = [...(cases[1]!.macroChannels as string[])].reverse();
  assert.notEqual(
    gameboy.decoderConfigDigest(gameboyDecoderConfig(reversed)),
    cases[1]!.digest,
    'channel order is identity',
  );
  // The example composition declares the real macros-mode digest and channel set.
  const example = legacy().composition.example;
  assert.equal(example.decoderConfigDigest, cases[1]!.digest);
  assert.deepEqual(example.executor.macroChannels, cases[1]!.macroChannels);
});
