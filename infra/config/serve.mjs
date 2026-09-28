#!/usr/bin/env node
// infra/config/serve.mjs — deployed to /opt/fly/current/stage/serve.mjs.
//
// A tiny static file server for apps/stage's build output, no
// dependencies. flystage's Chromium kiosk loads it at
// http://127.0.0.1:7402/ (docs/control-api.md's contract fixes flysim's
// own control API at :7401, so the built page gets its own tiny server
// rather than being served by flysim — see the infra task's clarification
// on flystage-web.service). Deliberately not a general-purpose server:
// no directory listing, no symlink following outside root, no range
// requests (the page is a handful of static assets, not video).
//
// One route is not a file under root: GET /recovery-notice.json returns the
// auto-recovery helper's notice (infra/bin/fly-loop-recover writes it to
// /run/fly/wd/recovery-notice.json, FLY_RECOVERY_NOTICE overrides), which the
// page's recovery splash polls once a second. It is served from here and not
// from flysim because flysim is the thing being restarted. 200 with the file's
// bytes, or 204 when there is no (readable, small, regular) file; the page does
// all of the validation (apps/stage/src/lib/recovery.ts).
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { join, normalize, extname } from 'node:path';

const root = process.argv[2] ?? new URL('.', import.meta.url).pathname;
const host = process.argv[3] ?? '127.0.0.1';
const port = Number(process.argv[4] ?? 7402);

const RECOVERY_ROUTE = '/recovery-notice.json';
const RECOVERY_PATH = process.env.FLY_RECOVERY_NOTICE || '/run/fly/wd/recovery-notice.json';
const RECOVERY_MAX_BYTES = 16384;

async function serveRecoveryNotice(res) {
  const headers = { 'cache-control': 'no-store' };
  const st = await stat(RECOVERY_PATH).catch(() => null);
  if (!st?.isFile() || st.size === 0 || st.size > RECOVERY_MAX_BYTES) {
    res.writeHead(204, headers).end();
    return;
  }
  const body = await readFile(RECOVERY_PATH).catch(() => null);
  if (!body) { res.writeHead(204, headers).end(); return; }
  res.writeHead(200, {
    ...headers,
    'content-type': 'application/json; charset=utf-8',
    'content-length': body.length,
  });
  res.end(body);
}

const TYPES = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff2': 'font/woff2',
  '.ico': 'image/x-icon', '.wasm': 'application/wasm',
};

const server = createServer(async (req, res) => {
  try {
    if (req.url.split('?')[0] === RECOVERY_ROUTE) { await serveRecoveryNotice(res); return; }
    const urlPath = normalize(decodeURIComponent(req.url.split('?')[0]));
    if (urlPath.includes('..')) { res.writeHead(400).end('bad path'); return; }
    let path = join(root, urlPath === '/' ? 'index.html' : urlPath);
    let st = await stat(path).catch(() => null);
    if (st?.isDirectory()) { path = join(path, 'index.html'); st = await stat(path).catch(() => null); }
    if (!st) { res.writeHead(404).end('not found'); return; }
    const body = await readFile(path);
    res.writeHead(200, {
      'content-type': TYPES[extname(path)] ?? 'application/octet-stream',
      'content-length': body.length,
      'cache-control': 'no-cache',
    });
    res.end(body);
  } catch (err) {
    res.writeHead(500).end('internal error');
    console.error('flystage-web:', err);
  }
});

server.listen(port, host, () => {
  console.log(`flystage-web: serving ${root} on http://${host}:${server.address().port}/`);
});
