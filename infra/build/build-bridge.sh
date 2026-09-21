#!/usr/bin/env bash
# infra/build/build-bridge.sh [OUT_DIR]
#
# Builds services/bridge into the directory shape infra/build/package-release.sh
# expects as its BRIDGE_DIR, i.e. the thing that becomes `bridge/` inside the
# release tarball and `/opt/fly/current/bridge/` on the container:
#
#   index.js        the whole service as one ES module (esbuild bundle)
#   package.json    "type": "module", so node treats index.js as ESM
#   node_modules/   the runtime dependencies left external by the bundle
#
# Nothing else. In particular no src/, no tests/, no tsconfig.json: a release
# ships what runs.
#
# WHY THIS SCRIPT EXISTS. It did not, and infra/units/flybridge.service's
# ExecStart has always been `node /opt/fly/current/bridge/index.js`, so the
# first release that carried a bridge at all (v0.1.0, 2026-09-16) shipped a
# hand-assembled bridge/ holding src/ + node_modules + package.json and NO
# index.js. The unit's own ConditionPathExists caught it and left the service
# cleanly inactive — which is the condition doing its job, and also why nobody
# noticed until someone went looking for the chat bridge on a live channel.
#
# WHY A BUNDLE AND NOT tsc. services/bridge is ESM TypeScript whose relative
# imports carry no file extension (`from './auth'`) and whose one workspace
# dependency, @flybrain/feed, has a .ts file as its package `main`. `tsc` would
# emit those specifiers unchanged, and Node's ESM resolver rejects both — so
# there is no `tsc` invocation that produces a runnable tree without first
# rewriting every import in the package. esbuild resolves and inlines them
# instead, which is what `npm run build -w @flybrain/bridge` does.
#
# WHAT STAYS EXTERNAL. @twurple/* and ws are left out of the bundle and come
# from node_modules: they are the only runtime deps, they are plain JavaScript
# already, and keeping them external means the bundle is 46 kB of our own code,
# which is reviewable and diffable against a release. bufferutil and
# utf-8-validate are ws's optional native accelerators — never installed here,
# required inside a try/catch by ws itself, and marked external so esbuild does
# not try to resolve them at build time.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUT_DIR="${1:-${REPO_DIR}/services/bridge/build}"

log() { echo "build-bridge: $*" >&2; }
die() { echo "build-bridge: FATAL: $*" >&2; exit 1; }

command -v npm >/dev/null 2>&1 || die "npm not found"
[ -d "${REPO_DIR}/services/bridge" ] || die "no services/bridge in ${REPO_DIR}"

# The workspace install has to exist: the bundle resolves @flybrain/feed through
# the root node_modules symlink npm workspaces creates, and the externals have
# to be there to be copied below.
[ -d "${REPO_DIR}/node_modules" ] \
    || die "no ${REPO_DIR}/node_modules — run 'npm ci' at the repo root first"

log "npm run build -w @flybrain/bridge"
npm run build --prefix "$REPO_DIR" -w @flybrain/bridge

BUNDLE="${REPO_DIR}/services/bridge/dist/index.js"
[ -f "$BUNDLE" ] || die "the build produced no ${BUNDLE}"

log "assembling ${OUT_DIR}"
rm -rf "$OUT_DIR"
mkdir -p "${OUT_DIR}/node_modules"
cp "$BUNDLE" "${OUT_DIR}/index.js"

# A package.json with "type": "module" and nothing else that matters. Copying
# services/bridge's own would carry devDependencies and a "main" pointing at
# src/index.ts, neither of which exists in the release.
cat > "${OUT_DIR}/package.json" <<'JSON'
{
  "name": "@flybrain/bridge-dist",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "main": "./index.js"
}
JSON

# The externals, and their own transitive dependencies. Resolved by asking node
# rather than by listing them by hand, so a twurple upgrade that adds a
# dependency cannot quietly ship a bridge that throws ERR_MODULE_NOT_FOUND on
# its first EventSub reconnect.
log "copying runtime dependencies"
node - "$REPO_DIR" "$OUT_DIR" <<'MJS'
import { createRequire } from 'node:module';
import { cpSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';

const [repoDir, outDir] = process.argv.slice(2);
const roots = ['@twurple/api', '@twurple/auth', '@twurple/eventsub-ws', 'ws'];
const seen = new Set();

/**
 * The directory holding a package's package.json, resolved from `fromDir`, or null when the
 * package is not installed at all.
 *
 * `require.resolve(`${name}/package.json`)` is the obvious way to ask and it is the FIRST thing
 * tried, but it cannot be the only one: a package whose `exports` map does not list
 * `./package.json` makes Node refuse the subpath with ERR_PACKAGE_PATH_NOT_EXPORTED, and every
 * @twurple package is exactly that shape (measured on @twurple/* 8.1.4 with Node 22.22.2). The
 * first version of this script had only that call, inside a try/catch meant for genuinely absent
 * optional deps, so all four roots but `ws` were silently skipped and the run died on the
 * @twurple/api assertion below — i.e. the bridge could not be built at all (found cutting
 * v0.1.1, 2026-09-16).
 *
 * The fallback is Node's own node_modules lookup — walk up from `fromDir` looking for
 * `<dir>/node_modules/<name>/package.json` — which is how a bare specifier finds its package
 * directory in the first place and which `exports` has no say over. It also keeps nested
 * (non-hoisted) copies working, which is what a transitive `@types/node` under
 * `@d-fischer/connection` needs.
 */
function packageDir(name, fromDir) {
  try {
    const req = createRequire(join(fromDir, 'index.js'));
    return dirname(req.resolve(`${name}/package.json`));
  } catch {
    // Either not installed, or an exports map that hides ./package.json. Fall through.
  }
  let dir = fromDir;
  for (;;) {
    const candidate = join(dir, 'node_modules', name);
    if (existsSync(join(candidate, 'package.json'))) return candidate;
    const parent = dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

function visit(name, fromDir) {
  // null means an optional peer or an unresolvable optional dependency (bufferutil): ws
  // requires those inside a try/catch and works without them.
  const dir = packageDir(name, fromDir);
  if (dir === null) return;
  if (seen.has(dir)) return;
  seen.add(dir);

  const manifest = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  for (const dep of Object.keys(manifest.dependencies ?? {})) visit(dep, dir);
}

const bridgeDir = join(repoDir, 'services', 'bridge');
for (const root of roots) visit(root, bridgeDir);

const nodeModulesRoot = join(repoDir, 'node_modules');
let copied = 0;
for (const dir of seen) {
  const rel = relative(nodeModulesRoot, dir);
  if (rel === '' || rel.startsWith('..')) {
    throw new Error(`refusing to copy ${dir}: not under ${nodeModulesRoot}`);
  }
  const dest = join(outDir, 'node_modules', rel);
  mkdirSync(dirname(dest), { recursive: true });
  cpSync(dir, dest, { recursive: true, dereference: true });
  copied += 1;
}
if (!existsSync(join(outDir, 'node_modules', '@twurple', 'api'))) {
  throw new Error('@twurple/api did not land in the assembled node_modules');
}
console.error(`build-bridge: copied ${copied} packages`);
MJS

# The assertion that matters, stated here rather than left to the container:
# flybridge.service's ConditionPathExists is on exactly this file, so a release
# without it produces a silently inactive service and no chat bridge.
[ -f "${OUT_DIR}/index.js" ] || die "no index.js in ${OUT_DIR}"

log "done: ${OUT_DIR} ($(du -sh "$OUT_DIR" | cut -f1))"
log "package it with: infra/build/package-release.sh VERSION <flysim> <stage-dir> ${OUT_DIR} <out-dir>"
