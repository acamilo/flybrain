import assert from 'node:assert/strict';
import test from 'node:test';
import { formatDuration } from '../src/duration';

void test('formatDuration formats seconds only', () => {
  assert.equal(formatDuration(0), '0s');
  assert.equal(formatDuration(45), '45s');
  assert.equal(formatDuration(59), '59s');
});

void test('formatDuration formats minutes and seconds', () => {
  assert.equal(formatDuration(60), '1m 00s');
  assert.equal(formatDuration(192), '3m 12s');
  assert.equal(formatDuration(3599), '59m 59s');
});

void test('formatDuration formats hours and minutes', () => {
  assert.equal(formatDuration(3600), '1h 00m');
  assert.equal(formatDuration(3900), '1h 05m');
  assert.equal(formatDuration(7384), '2h 03m');
});

void test('formatDuration clamps negative input to 0s', () => {
  assert.equal(formatDuration(-5), '0s');
});
