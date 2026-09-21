/**
 * Atomic, mode-0600 JSON file writes, shared by `tools/authorize.mts` (tokens.json) and
 * `src/redemptions.ts` (the redemption intent log). Writes to a temp file in the same directory
 * and renames over the target, so a crash mid-write never leaves a truncated file behind.
 */
import { mkdir, rename, unlink, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';

export async function atomicWriteJson(path: string, data: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const tempPath = `${path}.${process.pid}.${Date.now()}.tmp`;
  const json = JSON.stringify(data, null, 2);
  try {
    await writeFile(tempPath, json, { mode: 0o600 });
    await rename(tempPath, path);
  } catch (cause) {
    await unlink(tempPath).catch(() => undefined);
    throw cause;
  }
}
