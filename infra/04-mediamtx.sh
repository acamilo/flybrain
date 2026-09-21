#!/usr/bin/env bash
# infra/04-mediamtx.sh ENVFILE
#
# Install a pinned MediaMTX release tarball, verified against the release's
# own checksums.txt, into /opt/mediamtx, plus config/mediamtx.yml. See
# docs/design/infra.md section 4 for the config shape and its RTMP-loopback
# / HLS-LAN / API-loopback split.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
require_pve_host
need pct

# Pinned to a release that exists and was actually installed and run by the P0
# spike on 2026-09-15. The previous pin (v1.9.3) 404s: that asset is not
# published, so this step failed outright.
MEDIAMTX_VERSION="${MEDIAMTX_VERSION:-v1.21.0}"
ASSET="mediamtx_${MEDIAMTX_VERSION}_linux_amd64.tar.gz"
# The checksum file is named checksums.sha256, not checksums.txt.
CHECKSUMS="checksums.sha256"
BASE_URL="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}"
MEDIAMTX_HOME="/opt/mediamtx-${MEDIAMTX_VERSION}"

if ct_exec "$CTID" -- test -x "${MEDIAMTX_HOME}/mediamtx"; then
    log "04-mediamtx: ${MEDIAMTX_HOME} already installed"
else
    log "04-mediamtx: fetching ${ASSET} inside CT $CTID"
    # The checksum lines are "<sha256> *<asset>" (sha256sum's binary-mode
    # marker), so the old `grep " ${ASSET}$"` never matched and the comparison
    # silently had nothing to compare. Hand the one relevant line to
    # `sha256sum --check` instead, which parses that format natively and fails
    # loudly on a mismatch or a missing file.
    ct_exec "$CTID" -- sh -c "
        set -eu
        cd /tmp
        curl -fsSLO '${BASE_URL}/${ASSET}'
        curl -fsSLO '${BASE_URL}/${CHECKSUMS}'
        line=\$(grep -F ' *${ASSET}' '${CHECKSUMS}')
        if [ -z \"\$line\" ]; then
            echo '04-mediamtx: ${ASSET} not listed in ${CHECKSUMS}' >&2
            exit 1
        fi
        printf '%s\n' \"\$line\" | sha256sum --check --strict -
        mkdir -p '${MEDIAMTX_HOME}'
        tar -xzf '${ASSET}' -C '${MEDIAMTX_HOME}'
        rm -f '${ASSET}' '${CHECKSUMS}'
    "
fi

ct_exec "$CTID" -- ln -sfn "${MEDIAMTX_HOME}/mediamtx" /usr/local/bin/mediamtx

log "04-mediamtx: converging /etc/mediamtx.yml"
INFRA_DIR="$(infra_root)"
CHANGED="$(converge_file "$CTID" "$INFRA_DIR/config/mediamtx.yml" /etc/mediamtx.yml 0644 root:root)"
if [ "$CHANGED" = changed ]; then
    log "04-mediamtx: config changed; mediamtx.service restart will be handled by 07-enable.sh / a subsequent provision run"
fi

log "04-mediamtx: done"
