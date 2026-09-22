import assert from 'node:assert/strict';
import test from 'node:test';

import * as fixtures from '../src/fixtures';
import { readCommittedSnapshot, readSessionDescriptor, validateSnapshotAgainst } from '../src/publishing';
import {
  findPort,
  readEnvironmentDescriptor,
  readPortControl,
  readSensoryInput,
  readStepResult,
  readWorldObservation,
  validateBatch,
  validateObservationAgainst,
  validatePortControlAgainst,
  validateSensoryInputAgainst,
  validateStepResultAgainst,
} from '../src/workers';
import { requiredProducedStep, frameBytes } from '../src/media';

test('every descriptor check lands the way the fixture says', () => {
  const file = fixtures.load('descriptor-checks.json');
  const descriptor = readEnvironmentDescriptor(fixtures.member(file, 'descriptor'));
  const delayed = readEnvironmentDescriptor(fixtures.member(file, 'delayedDescriptor'));
  const session = readSessionDescriptor(fixtures.member(file, 'sessionDescriptor'));
  const previous = readWorldObservation(fixtures.member(file, 'stepResultPrevious'));

  for (const item of fixtures.cases(file)) {
    const name = fixtures.field(item, 'name');
    const kind = fixtures.field(item, 'kind');
    const reason = fixtures.optionalField(item, 'reason') ?? '';
    const expectAccept = fixtures.field(item, 'expect') === 'accept';
    const value = fixtures.member(item, 'value');
    const attempt = () => {
      switch (kind) {
        case 'portControl': {
          const control = readPortControl(value);
          const port = findPort(descriptor, control.portId);
          if (!port) throw new Error('no such port');
          validatePortControlAgainst(control, port.controls);
          return;
        }
        case 'advanceControls': {
          const controls = (value as unknown[]).map(readPortControl);
          validateBatch(descriptor, controls);
          return;
        }
        case 'sensoryInput':
          validateSensoryInputAgainst(readSensoryInput(value), descriptor.views);
          return;
        case 'sensoryInputDelayed':
          validateSensoryInputAgainst(readSensoryInput(value), delayed.views);
          return;
        case 'worldObservation':
          validateObservationAgainst(readWorldObservation(value), descriptor);
          return;
        case 'stepResult':
          validateStepResultAgainst(readStepResult(value), descriptor, previous);
          return;
        case 'snapshot':
          validateSnapshotAgainst(readCommittedSnapshot(value), session);
          return;
        default:
          throw new Error(`unknown descriptor check kind "${kind}"`);
      }
    };
    if (expectAccept) {
      assert.doesNotThrow(attempt, `${name} must be accepted. ${reason}`);
    } else {
      assert.throws(attempt, `${name} must be refused. ${reason}`);
    }
  }
});

test('the required producing boundary saturates at zero', () => {
  const file = fixtures.load('descriptor-checks.json');
  const delayed = readEnvironmentDescriptor(fixtures.member(file, 'delayedDescriptor'));
  const view = delayed.views[0]!;
  assert.equal(view.observationDelaySteps, 2);
  assert.equal(requiredProducedStep(view, 0n), 0n);
  assert.equal(requiredProducedStep(view, 1n), 0n);
  assert.equal(requiredProducedStep(view, 2n), 0n);
  assert.equal(requiredProducedStep(view, 3n), 1n);
  assert.equal(frameBytes(view), 160 * 4 * 144);
});
