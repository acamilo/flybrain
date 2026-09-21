#!/usr/bin/env bash
# infra/03-node.sh ENVFILE
#
# Install a pinned Node 22 tarball from nodejs.org into /opt/node-v${NODE_VERSION},
# verified against nodejs.org's own SHASUMS256.txt, symlinked into
# /usr/local/bin. No apt, no NodeSource repo (docs/design/infra.md section 2:
# "Hermetic, no third-party apt repo"). Idempotent: skips the download when
# the pinned version is already installed.
#
# flybridge is the only consumer today; flysim and flystage-web do not need
# Node (flysim is a native binary, flystage-web's tiny static server is
# plain Node too and uses this same install).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
require_pve_host
need pct

# VERIFY: pin the exact patch release you want before P1. This is the
# newest Node 22 release known at design time; confirm it still resolves
# at https://nodejs.org/dist/ before running against a real container.
NODE_VERSION="${NODE_VERSION:-22.14.0}"
NODE_TARBALL="node-v${NODE_VERSION}-linux-x64.tar.xz"
NODE_URL="https://nodejs.org/dist/v${NODE_VERSION}/${NODE_TARBALL}"
SHASUMS_URL="https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt"
NODE_HOME="/opt/node-v${NODE_VERSION}"

if ct_exec "$CTID" -- test -x "${NODE_HOME}/bin/node"; then
    log "03-node: ${NODE_HOME} already installed, checking symlinks only"
else
    log "03-node: fetching ${NODE_URL} inside CT $CTID"
    ct_exec "$CTID" -- sh -c "
        set -eu
        cd /tmp
        curl -fsSLO '$NODE_URL'
        curl -fsSLO '$SHASUMS_URL'
        want=\$(grep \" ${NODE_TARBALL}\$\" SHASUMS256.txt | awk '{print \$1}')
        if [ -z \"\$want\" ]; then
            echo \"03-node: ${NODE_TARBALL} not listed in SHASUMS256.txt\" >&2
            exit 1
        fi
        got=\$(sha256sum '${NODE_TARBALL}' | awk '{print \$1}')
        if [ \"\$want\" != \"\$got\" ]; then
            echo \"03-node: sha256 mismatch for ${NODE_TARBALL}: want \$want got \$got\" >&2
            exit 1
        fi
        mkdir -p '${NODE_HOME}'
        tar -xJf '${NODE_TARBALL}' -C '${NODE_HOME}' --strip-components=1
        rm -f '${NODE_TARBALL}' SHASUMS256.txt
    "
fi

log "03-node: symlinking node/npm/npx into /usr/local/bin"
for bin in node npm npx; do
    ct_exec "$CTID" -- ln -sfn "${NODE_HOME}/bin/${bin}" "/usr/local/bin/${bin}"
done

ct_exec "$CTID" -- /usr/local/bin/node --version

log "03-node: done"
