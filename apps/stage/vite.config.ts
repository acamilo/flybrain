/**
 * Vite config for the broadcast page.
 *
 * Two things here are not boilerplate:
 *
 * 1. The dataset artifacts in the repo's `data/fafb-v783` are served at `/data/fafb-v783` in dev
 *    by a small middleware and copied into `dist/` on build. They are not copied into
 *    `public/`: they are 11 MB of generated artifacts that belong to the repo root, and a second
 *    copy in git is exactly what the task forbids. They are `.binz` (gzip) files that the
 *    browser inflates itself with `DecompressionStream`, so the middleware must NOT set
 *    `content-encoding: gzip` — that would make the browser inflate them first and
 *    `loadCompressed` would then fail on already-inflated bytes.
 * 2. `?res=720` scales the whole page down with a CSS transform, so nothing here changes per
 *    resolution. There is one authoring resolution (1920x1080, the native broadcast frame) and one
 *    build.
 */
import { execFileSync } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { cp, readFile, stat } from 'node:fs/promises';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { dirname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '../..');
const datasetDir = resolve(repoRoot, 'data/fafb-v783');
const datasetRoute = '/data/fafb-v783';

/**
 * The version chip's string (`src/panels/TitleStrip.tsx`, `data-testid="stage-version"`).
 *
 * `git describe --tags --always --dirty` from the repo root: a tagged, clean checkout gives
 * `v0.1.0`; an untagged one falls back to the short sha on its own (`--always`); a dirty tree
 * appends `-dirty`. `apps/stage/README.md` names the one thing this depends on that Vite cannot
 * enforce — the build has to run from the tagged checkout for the string to be right.
 *
 * The dev server never shells out for it: HMR does not represent a build, so `npm run dev` always
 * reads `dev` rather than a sha that would go stale on the next commit without a restart.
 */
function stageVersion(command: 'build' | 'serve'): string {
  if (command === 'serve') return 'dev';
  try {
    return execFileSync('git', ['describe', '--tags', '--always', '--dirty'], { cwd: repoRoot })
      .toString()
      .trim();
  } catch {
    return 'dev';
  }
}

/** Serve `data/fafb-v783` at `/data/fafb-v783` in dev, and copy it into `dist/` on build. */
function datasetArtifacts(): Plugin {
  return {
    name: 'flystage-dataset-artifacts',
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = req.url ?? '';
        if (!url.startsWith(`${datasetRoute}/`)) return next();

        const relative = normalize(decodeURIComponent(url.slice(datasetRoute.length + 1)).split('?')[0] ?? '');
        if (!relative || relative.startsWith('..')) {
          res.statusCode = 403;
          res.end('forbidden');
          return;
        }

        const file = join(datasetDir, relative);
        try {
          const info = await stat(file);
          if (!info.isFile()) throw new Error('not a file');
          res.setHeader('content-type', file.endsWith('.json') ? 'application/json' : 'application/octet-stream');
          res.setHeader('content-length', String(info.size));
          res.setHeader('cache-control', 'no-cache');
          createReadStream(file).pipe(res);
        } catch {
          res.statusCode = 404;
          res.end('not found');
        }
      });
    },
    async closeBundle() {
      // Build-time copy. `dist/data/fafb-v783` mirrors the dev route exactly.
      await cp(datasetDir, resolve(here, 'dist/data/fafb-v783'), { recursive: true });
    },
  };
}

/**
 * `GET /recovery-notice.json` in dev and preview, the same route `infra/config/serve.mjs` gives
 * the deployed page: the auto-recovery helper's notice file (`FLY_RECOVERY_NOTICE`, default
 * `/run/fly/wd/recovery-notice.json`), or 204 when there is none. What makes
 * `npm run recovery-notice` (`tools/recovery-notice.mts`) work against a local page.
 */
function recoveryNotice(): Plugin {
  const serve = async (req: IncomingMessage, res: ServerResponse, next: () => void): Promise<void> => {
    if ((req.url ?? '').split('?')[0] !== '/recovery-notice.json') return next();
    const path = process.env.FLY_RECOVERY_NOTICE || '/run/fly/wd/recovery-notice.json';
    res.setHeader('cache-control', 'no-store');
    try {
      const info = await stat(path);
      if (!info.isFile() || info.size === 0 || info.size > 16_384) throw new Error('no notice');
      const body = await readFile(path);
      res.setHeader('content-type', 'application/json; charset=utf-8');
      res.end(body);
    } catch {
      res.statusCode = 204;
      res.end();
    }
  };
  return {
    name: 'flystage-recovery-notice',
    configureServer(server) {
      server.middlewares.use((req, res, next) => void serve(req, res, next));
    },
    configurePreviewServer(server) {
      server.middlewares.use((req, res, next) => void serve(req, res, next));
    },
  };
}

export default defineConfig(({ command }) => ({
  plugins: [react(), tailwindcss(), datasetArtifacts(), recoveryNotice()],
  define: {
    __STAGE_VERSION__: JSON.stringify(stageVersion(command)),
  },
  resolve: {
    alias: { '@': resolve(here, 'src') },
  },
  server: {
    port: 5273,
    strictPort: true,
    // The dataset middleware reads from the repo root, which is outside the app root.
    fs: { allow: [repoRoot] },
  },
  preview: {
    port: 4300,
    strictPort: true,
  },
  worker: {
    format: 'es',
  },
  build: {
    target: 'es2023',
    // One long-lived page in one pinned Chromium: inlining nothing keeps the waterfall honest,
    // and a broadcast page has no cold-start budget worth optimising.
    assetsInlineLimit: 0,
    sourcemap: true,
  },
}));
