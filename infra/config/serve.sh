#!/usr/bin/env bash
# infra/config/serve.sh — deployed to /opt/fly/current/stage/serve.sh, the
# ExecStart for flystage-web.service. Resolves its own directory so it
# serves whatever release it actually lives in, regardless of the
# `current` symlink's target at start time, and execs node so systemd
# tracks the real process (no wrapper left as an orphaned parent).
set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec node "${dir}/serve.mjs" "$dir" 127.0.0.1 7402
