import assert from 'node:assert/strict';
import { mkdtemp, readFile, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { atomicWriteJson } from '../src/atomic-file';

void test('atomicWriteJson writes readable JSON at mode 0600', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'flybridge-atomic-'));
  const path = join(dir, 'tokens.json');
  await atomicWriteJson(path, { hello: 'world' });

  const contents = JSON.parse(await readFile(path, 'utf8')) as { hello: string };
  assert.equal(contents.hello, 'world');

  const info = await stat(path);
  assert.equal(info.mode & 0o777, 0o600);
});

void test('atomicWriteJson creates missing parent directories', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'flybridge-atomic-'));
  const path = join(dir, 'nested', 'deeper', 'state.json');
  await atomicWriteJson(path, [1, 2, 3]);
  const contents = JSON.parse(await readFile(path, 'utf8')) as number[];
  assert.deepEqual(contents, [1, 2, 3]);
});

void test('atomicWriteJson overwrites an existing file', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'flybridge-atomic-'));
  const path = join(dir, 'state.json');
  await atomicWriteJson(path, { version: 1 });
  await atomicWriteJson(path, { version: 2 });
  const contents = JSON.parse(await readFile(path, 'utf8')) as { version: number };
  assert.equal(contents.version, 2);
});
