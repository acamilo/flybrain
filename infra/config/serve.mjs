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
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { join, normalize, extname } from 'node:path';

const root = process.argv[2] ?? new URL('.', import.meta.url).pathname;
const host = process.argv[3] ?? '127.0.0.1';
const port = Number(process.argv[4] ?? 7402);

const TYPES = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff2': 'font/woff2',
  '.ico': 'image/x-icon', '.wasm': 'application/wasm',
};

const server = createServer(async (req, res) => {
  try {
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
  console.log(`flystage-web: serving ${root} on http://${host}:${port}/`);
});
