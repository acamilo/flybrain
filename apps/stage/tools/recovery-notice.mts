/**
 * Write a fake auto-recovery notice, for looking at the recovery splash by hand.
 *
 * The file is the one `infra/bin/fly-loop-recover` writes (contract: `infra/docs/loop-recovery.md`,
 * "Stream notice"), written the same way — a temporary file renamed over the target, mode 0644 —
 * to `--out`, else `$FLY_RECOVERY_NOTICE`. Point the page's server at the same path:
 *
 * ```sh
 * export FLY_RECOVERY_NOTICE=$PWD/.recovery-notice.json   # any writable path
 * npm run dev -w @flybrain/stage                           # serves it at /recovery-notice.json
 * #   http://127.0.0.1:5273/?mode=player&recovery=1
 *
 * npm run recovery-notice -w @flybrain/stage -- --phase countdown --action reset --in 45
 * npm run recovery-notice -w @flybrain/stage -- --phase acting --action restart
 * npm run recovery-notice -w @flybrain/stage -- --demo reset   # countdown 20 s, acting 15 s, done
 * npm run recovery-notice -w @flybrain/stage -- --clear
 * ```
 *
 * On the release container the deployed `serve.mjs` reads `/run/fly/wd/recovery-notice.json`
 * unless its unit sets `FLY_RECOVERY_NOTICE`; this tool is for a dev machine, not for faking a
 * recovery on air.
 */
import { chmod, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';

type Phase = 'countdown' | 'acting' | 'done' | 'failed';
type Action = 'restart' | 'reset';

function arg(name: string): string | null {
  const at = process.argv.indexOf(`--${name}`);
  return at === -1 ? null : (process.argv[at + 1] ?? null);
}

const out = arg('out') ?? process.env.FLY_RECOVERY_NOTICE ?? '';
if (out === '') {
  console.error('recovery-notice: pass --out <path> or set FLY_RECOVERY_NOTICE');
  process.exit(2);
}

async function write(phase: Phase, action: Action, id: string, announcedAt: number, executeAt: number): Promise<void> {
  const notice = {
    v: 1,
    id,
    phase,
    action,
    fromRung: Number(arg('from-rung') ?? 12),
    fromLabel: arg('from-label') ?? 'MT. MOON',
    ...(action === 'reset' ? { toRung: Number(arg('to-rung') ?? 11), toLabel: arg('to-label') ?? 'PEWTER CITY' } : {}),
    reason: arg('reason') ?? 'unrewarded',
    loop: (arg('loop') ?? 'GO OBJECTIVE,GO WARP').split(',').map((step) => step.trim()),
    stuckSeconds: Number(arg('stuck') ?? 1800),
    announcedAt,
    executeAt,
    updatedAt: Math.floor(Date.now() / 1000),
  };
  const tmp = join(dirname(out), `.recovery-notice.${process.pid}.tmp`);
  await writeFile(tmp, `${JSON.stringify(notice, null, 2)}\n`);
  await chmod(tmp, 0o644);
  await rename(tmp, out);
  console.log(`${phase} ${action} -> ${out}`);
}

const sleep = (s: number): Promise<void> => new Promise((done) => setTimeout(done, s * 1000));

async function main(): Promise<void> {
  if (process.argv.includes('--clear')) {
    await rm(out, { force: true });
    console.log(`removed ${out}`);
    return;
  }
  const now = Math.floor(Date.now() / 1000);
  const demo = arg('demo');
  if (demo !== null) {
    const action = (demo === 'restart' ? 'restart' : 'reset') as Action;
    const id = `${now}-${action}`;
    await write('countdown', action, id, now, now + 20);
    await sleep(20);
    await write('acting', action, id, now, now + 20);
    await sleep(15);
    await write(process.argv.includes('--fail') ? 'failed' : 'done', action, id, now, now + 20);
    return;
  }
  const phase = (arg('phase') ?? 'countdown') as Phase;
  const action = (arg('action') ?? 'reset') as Action;
  const lead = Number(arg('in') ?? 60);
  await write(phase, action, `${now}-${action}`, now, now + lead);
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
