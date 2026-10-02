# flybrain infra runbook

Operational procedures for the two fly demo containers (`fly-pokemon` the release container,
`fly-platformer` the platformer container) plus the throwaway spike (`fly-spike` the dev container). Every command below
runs on **the host** (`<host-ip>`) as root, unless marked "operator box". There is no
sshd in any fly container — everything goes through `pct exec`/`pct push`/`pct pull`.

Read `docs/design/infra.md` first if you have not; this runbook assumes its vocabulary
(flysim, flystage, flystage-web, flycast, flypush, flybridge, fly.target) without
re-explaining it.

## Do not touch the neighbouring production container

The neighbouring production container is another service's production workload. Nothing in `infra/` targets it, references it, or shares a
dataset/pool path with it beyond the same `rpool` SSD mirror both live on. If a disk-full
or write-storm incident on a fly container ever threatens `rpool`, the priority order is:
stop the fly container's writes first (`pct stop <ctid>` if needed), never touch the neighbouring production container.
See `docs/design/infra.md` section 0 and section 8 ("SSD write endurance") for why the
checkpoint cadence design specifically exists to protect this mirror.

## Start / stop

Single verb, `fly.target`:

```
pct exec <ctid> -- systemctl start fly.target   # xvfb, pulse, mediamtx, flysim,
                                                  # flystage-web, flystage, flycast
pct exec <ctid> -- systemctl stop  fly.target
pct exec <ctid> -- systemctl status fly.target
```

`flypush` is deliberately **not** part of `fly.target` (see "Local-to-Twitch flip"
below) — stopping/starting the target never touches it, and it is not affected by a
`fly.target` restart. `flybridge` **is** in `fly.target` but as `Wants=`, so a
missing/broken bridge never blocks target start/stop.

Expect `flycast` to take up to about two minutes longer than everything else on a cold
start or after an `xvfb` restart: it is `After=flystage.service` and waits (up to 120 s,
then starts anyway with a warning) for the page to be painted and its audio client
attached, which is the capture-freeze ordering gate — see "Capture freeze" below.

To restart just one unit after a config change: `pct exec <ctid> -- systemctl restart
<unit>.service`. `05-deploy.sh` already does this for you when it pushes a changed unit
file and detects a `daemon-reload` is needed, but a config file inside `/etc/fly/`
(`chromium-flags`, `pulse.pa`, `mediamtx.yml`) needs a manual restart of the unit that
reads it.

## Local-to-Twitch flip

Local test mode (`PUSH_TARGET=local` in the env file) leaves `flypush.service` disabled;
everything else is identical to production. Flipping to Twitch:

```
# 1. Install the stream key (from the operator box, where `pass` lives):
FORCE_SECRETS=1 infra/06-secrets.sh <release-env>

# 2. Enable flypush:
pct exec <release-ctid> -- systemctl enable --now flypush.service
```

Flip back: `pct exec <release-ctid> -- systemctl disable --now flypush.service`. Neither direction
restarts the sim, the page, the encoder, or the recording — that is the entire reason
`flycast` and `flypush` are split into two units (`docs/design/infra.md` section 3).

After flipping to Twitch, also enable the 23h restart guard:
`pct exec <release-ctid> -- systemctl enable --now flypush-restart.timer` (or re-run
`07-enable.sh` with `PUSH_TARGET=twitch` set in the env file, which does both).

## Rotate the stream key

1. Generate a new key on Twitch's dashboard for the channel.
2. Update the `pass` entry (`twitch/<channel>-key` or `twitch/<platformer-channel>-key`) on
   The operator box.
3. Re-run secrets install: `FORCE_SECRETS=1 infra/06-secrets.sh <release-env>`.
   This overwrites `/etc/fly/creds/twitch-key.cred` with the new encrypted credential.
4. Restart flypush to pick it up: `pct exec <release-ctid> -- systemctl restart flypush.service`.
5. Confirm: `pct exec <release-ctid> -- journalctl -u flypush -n 50` shows a clean reconnect, and
   `infra/verify.sh <release-env>`'s key-prefix check passes against the new key.

**Rotate immediately** if a key ever appears in a log, a commit, or chat — the design's
threat model (`docs/design/infra.md` section 3) is explicit that the key's presence in
`/proc/<pid>/cmdline` is an accepted, contained residual risk, not a reason to be
casual about rotation.

## On-screen chat: ban a phrase, or kill the panel

The right rail carries a live CHAT panel. Chat is AutoMod-passed, sanitized by flybridge,
sanitized again and deny-list filtered by flysim, and carried in a 12-line ring in the feed
header (`docs/control-api.md`, `POST /chat`). **Chat never reaches the simulation**, so nothing
here can affect the fly — only what the audience sees on the broadcast.

Ban a phrase or a name, live, without dropping a frame:

```sh
# On the host. The file is operator-owned: 05-deploy.sh installed it once and never
# overwrites it. One pattern per line, matched case-insensitively anywhere in the
# name or the sanitized text; `#` comments.
pct exec <release-ctid> -- sh -c 'printf "%s\n" "some phrase" >> /srv/fly/chat-deny.txt'
pct exec <release-ctid> -- systemctl kill -s HUP flysim.service
pct exec <release-ctid> -- journalctl -u flysim -n 5   # "chat deny list reloaded patterns=N ... forced=true"
```

SIGHUP re-reads that file **and does nothing else** — no restart, no reload of anything else, no
interruption. flysim also re-reads it once a minute by itself, so a forgotten signal costs at most
60 seconds.

Turn the whole panel off (the kill switch):

```sh
# Source end, no Twitch credentials needed. Survives a flysim restart because
# 05-deploy.sh writes it from the env file.
#   1. set FLY_CHAT_ENABLED=0 in the release env file
#   2. infra/05-deploy.sh <release-env>
#   3. pct exec <release-ctid> -- systemctl restart flysim.service
# In an emergency, without a deploy:
pct exec <release-ctid> -- sh -c 'sed -i s/FLY_CHAT_ENABLED=1/FLY_CHAT_ENABLED=0/ /etc/fly/fly.env'
pct exec <release-ctid> -- systemctl restart flysim.service
```

`POST /chat` then answers 403 and the feed header omits `chat` entirely, so the page blanks the
panel rather than freezing the last seven lines. Put the env file back afterwards, or the next
deploy will turn it on again. flybridge has its own flag (`FEATURE_ONSCREEN_CHAT`) but the flysim
switch is the one to reach for: it is the source end, and it also stops anything else that might
be posting to the endpoint.

Check what the filters are doing:

```sh
curl -s 127.0.0.1:9101/metrics | grep fly_chat_
# fly_chat_accepted_total, fly_chat_ring_lines, and one
# fly_chat_rejected_total{reason="..."} series per rule (url, charset, deny_list, ...).
```

## Chat dead, bridge active (flybridge EventSub)

The symptom: nothing the bot says appears in chat, `!fly` and friends do nothing, the on-screen
CHAT panel stops filling — and `systemctl status flybridge` says `active (running)` with
`NRestarts=0`. Measured once for real: the release container, 2026-09-16 11:45:34 to 12:47:13 UTC, an hour of it,
ended by a hand-typed restart.

**What the bridge now does by itself.** It exits. If the `channel.chat.message` EventSub
subscription has not been confirmed created for `EVENTSUB_GRACE_MS` (default 60 s) after any
socket disconnect, create failure or revocation, it logs one `FATAL:` line and exits 75;
`flybridge.service` has `Restart=always`, `RestartSec=15` and no start limit, so it comes back
15 s later with a FRESH EventSub websocket transport — which is the only thing that clears
Twitch's "number of websocket transports limit exceeded". Expect roughly one attempt per 75 s for
as long as the cause lasts, one `FATAL:` line each, and at most one notice in chat per 10 minutes.
So in the normal case there is nothing to do but read the journal and find out why.

**First question: is it actually dead, or did it already heal?**

```sh
ssh the host pct exec <release-ctid> -- curl -s 127.0.0.1:7410/health | python3 -m json.tool
# chatSubscriptionHealthy  false => chat IS dead right now. THIS is the field to read.
# eventSubConnected        was TRUE for the whole hour on 2026-09-16 — a connected socket is
#                          not a working subscription. Never conclude anything from it alone.
# eventSubListenerCount    1 when the bot and broadcaster roles are one Twitch account (today),
#                          2 once a separate bot account exists. A 2 on a one-account channel
#                          means the shared auth provider did not get wired: two transports for
#                          one account is the shape that caused the outage.
# lastSelfHealAtIso        non-null => this process is a restart after a self-heal.
```

**Then the journal. Three lines matter:**

```sh
ssh the host pct exec <release-ctid> -- journalctl -u flybridge -n 200 --no-pager
```

- `flybridge: FATAL: the channel.chat.message EventSub subscription has not been confirmed for
  60000 ms` — the self-heal fired. The same line names every loss reason it saw and, when it was
  Twitch's transport limit, says so. This is the line to grep for.
- `flybridge: EventSub ... socket for <id> disconnected: ...` — the trigger (code 1006 on
  2026-09-16).
- `flybridge: EventSub subscription create failure for channel.chat.message...: Encountered HTTP
  status code 429 ... number of websocket transports limit exceeded` — Twitch refusing the
  re-create because stale transports from this account have not been reaped. Time fixes this, and
  `RestartSec=15` is the wait. Do NOT shorten it, and do not add a retry loop.

```sh
ssh the host pct exec <release-ctid> -- curl -s 127.0.0.1:7410/metrics | grep flybridge_eventsub
# ..._chat_subscription_confirmed_total  climbs once per successful (re-)create
# ..._chat_subscription_lost_total       climbs on every disconnect/failure/revocation
# ..._transport_limit_total              > 0 => the 429 above
# ..._subscription_create_failures_total all subscriptions, not just chat
# ..._reconnects_total                   socket drops
```

**When to intervene, and with what.**

| What the journal says | What it means | Do |
| --- | --- | --- |
| Repeating `FATAL:` + `transport_limit` for more than ~10 min | Something else is holding this account's websocket transports (a second bridge, a dev box, a stale `tools/mock-twitch.sh`) | Find and stop the other client. `GET https://api.twitch.tv/helix/eventsub/subscriptions` with the broadcaster token lists them; delete the strays. |
| Repeating `FATAL:` + `create failure ... missing scope` | The token lost a scope (re-authorized with the wrong `--role`, or Twitch revoked it) | Re-run `tools/authorize.mts` — see "Rotate the bot token" below. The bridge will NOT recover from this on its own; restarting cannot add a scope. |
| Repeating `FATAL:` + `revoked (authorization_revoked)` | The channel or the user revoked the app | Re-authorize. Same as above. |
| Repeating `FATAL: Twitch refused to refresh the <role> token ... (HTTP 400 "Invalid refresh token")` at startup | That role's refresh token is revoked or invalid (password change, app disconnected, re-authorized elsewhere) | Re-authorize that `--role` — "Rotate the bot token" below. Restarting cannot help; systemd keeps retrying every 15 s with one refresh request per role until the file is fixed. |
| `the stored <role> access token has expired; refreshing it` or `rejected the stored <role> access token (401); refreshing it once`, then a normal start | The bridge was down longer than an access token lives (~4 h), or Twitch invalidated it early | Nothing: it refreshed and persisted the new token itself. |
| One `FATAL:` then a quiet, `chatSubscriptionHealthy: true` bridge | It healed. | Nothing. Note it in the status log if it was during a stream. |
| `active (running)`, `chatSubscriptionHealthy: false`, and NO `FATAL:` line after more than 2 min | The watchdog itself is not running — an old bundle | Check `/opt/fly/current/bridge/index.js` is from a release that has it, then `systemctl restart flybridge`. |

The manual hammer, still correct and still fast, is `pct exec <release-ctid> -- systemctl restart
flybridge`. It costs nothing but the startup notice (suppressed if one went out in the last 10
minutes) and does not touch flysim, the page or the broadcast — `fly.target` only `Wants=` this
unit, so chat can never take the fly down.

## Rotate the bot token (flybridge)

Access tokens live about 4 h; refresh tokens until revoked. The bridge refreshes on its own:
at runtime through `RefreshingAuthProvider`, and ON LOAD (`prepareStartupTokens` in
`services/bridge/src/auth.ts`) — a stored access token that has expired, or that Twitch's
validate endpoint rejects with 401, is refreshed with its refresh token before the bridge gives
up, for both roles, and each refreshed token is written back to `/var/lib/flybridge/tokens.json`
under its own role (tmp file + rename, mode 0600), also when the bot and the broadcaster are one
account. So an outage longer than an access token's life no longer needs a hand refresh
(2026-09-28: it used to crash-loop on 401 at `validate`).

Re-authorizing is needed only when the journal says
`flybridge: FATAL: Twitch refused to refresh the <role> token for user <id> (HTTP 400 "Invalid refresh token")`,
or a scope is missing (the table in "Chat dead" above). Then, on the operator box:

1. `npx tsx tools/authorize.mts --role <role> --tokens-file <local copy of tokens.json>`
   (`services/bridge`, `docs/design/stage-bridge.md` section B1). It rewrites only that role's
   entry and keeps the other.
2. Push the file to the release container as `/var/lib/flybridge/tokens.json`, owned by the
   service user, mode 0600 (it is a secret; never in a release artifact or in git).
3. `systemctl restart flybridge` and read the journal for the startup notice line.

Never paste a token or the client secret into a log, a ticket or chat: the bridge itself prints
neither (Twitch errors are logged by status and message only, URLs redacted). The `twitch-app`
credential (client id/secret) only needs rotating if the app itself is compromised; follow the
same `06-secrets.sh` pattern as the stream key.

## Restore a checkpoint locally

flysim restores automatically on startup from `manifest.json` -> `latest` -> `previous`
-> highest archived generation -> best milestone, in that order
(`docs/design/flysim.md` section 8). To force a specific generation by hand:

```
pct exec <ctid> -- systemctl stop flysim.service
pct exec <ctid> -- cat /srv/fly/state/manifest.json   # note the generation you want
# Edit manifest.json's "latest" field to point at that generation (systemd-creds/pct
# push a corrected manifest.json, or edit in place with pct exec ... -- sh -c '...').
pct exec <ctid> -- systemctl start flysim.service
pct exec <ctid> -- curl -s http://127.0.0.1:7401/status | jq .checkpoint
```

If every candidate checkpoint fails validation, flysim exits non-zero and does **not**
auto-reset (`docs/design/flysim.md` section 8: "no automatic fresh start, ever"). That
is deliberate — a silent reset would be indistinguishable from real progress on stream.
A deliberate reset means moving `/srv/fly/state` aside by hand.

## Restart the run from a rung

When the run has to go back to an earlier milestone rather than start over — the operator's
decision of 2026-09-22 was "restart the live run from an early checkpoint instead of from
scratch". `FLY_RESET_STATE=1` is the wrong tool: it archives the durable state and the next start
warms up a fresh fly, losing everything the brain has learned.

`infra/bin/fly-reset-to-milestone <N>` promotes `milestone-<N>.checkpoint` to being what both
stores restore, with the ratchet's attempts and recoveries back at zero. It copies every file in
both stores to `/srv/fly/state.reset-<UTC>` first, so it is reversible by hand. It refuses while
flysim is running, and refuses a rung this run never reached.

The whole sequence, in order. Claim the container in the host's agent claim log first, like any
other work on it.

```
CTID=<release-ctid>
N=9                       # the rung to restart from

# 1. what rungs exist at all
pct exec $CTID -- ls -1 /srv/fly/state/milestone-*.checkpoint

# 2. stop flysim (it owns both stores; a reset underneath it is overwritten within the minute)
pct exec $CTID -- systemctl stop flysim.service

# 3. the reset. Prints what it did, one line per step.
pct exec $CTID -- /opt/fly/bin/fly-reset-to-milestone $N

# 4. deploy. Two cases:
#    (a) the running release already wrote that checkpoint -> nothing to deploy, skip to 5.
#    (b) the new build bumps the ADAPTER VERSION and nothing else -> name the checkpoint's
#        adapter so the gate and flysim both migrate instead of refusing:
FLY_ACCEPT_ADAPTERS=pokered-unique8-v7 infra/05-deploy.sh <release-env> <release-tarball>
#    The gate logs "the adapter version is the only difference, and it is named; the run is KEPT
#    and migrated", and writes FLY_ACCEPT_ADAPTERS into /etc/fly/fly.env so flysim applies the
#    same rule at restore. Anything else about the string differing is still a refusal.

# 5. start
pct exec $CTID -- systemctl start flysim.service

# 6. verify: the rank is the rung, and the restore came from the generation the tool wrote
pct exec $CTID -- curl -s http://127.0.0.1:7401/status | jq '.milestone.rank, .game.badges, .checkpoint'
pct exec $CTID -- journalctl -u flysim -n 40 --no-pager | grep -E 'restored|migration|compatibility'
```

Step 6 is the one that must be read rather than assumed. The rank is recomputed by the adapter
from the restored game state, not taken from the ratchet, so a rank that is *not* N means the
milestone archive was taken somewhere other than where its name says — stop and look before
starting a stream on it.

To undo: stop flysim, move the contents of `/srv/fly/state.reset-<UTC>/durable` back into
`/srv/fly/state`, delete the generation the tool wrote, and start again.

## Restore from the backup host

```
# On the host:
DATE=20260101   # the backup date you want
CTID=<release-ctid>
scp -r <backup-user>@<backup-host>:<backup-path>/fly-pokemon/$DATE/* <host-stage>/fly-restore/
pct exec $CTID -- systemctl stop flysim.service
pct push $CTID <host-stage>/fly-restore/manifest.json /srv/fly/state/manifest.json
pct push $CTID <host-stage>/fly-restore/<generation>.checkpoint /srv/fly/state/<generation>.checkpoint
pct exec $CTID -- systemctl start flysim.service
```

To restore into the **spike CT** instead (the P1 restore drill in
`docs/design/infra.md` section 7, drill 6: "Restore from the backup host into the dev container and boot
flysim on it. Assert rank and badges match."), point the `pct push` calls at the dev container and
provision the spike with the corresponding `<dev-env>` `GAME` set to match, then
compare `GET /status`'s `milestone.rank` and `game.badges` against the source
container's before the restore.

## Cutting a release

The release container `fly-pokemon` is the **release** container (`ROLE=release` in
`<release-env>`) — docs/stream-mvp-plan.md, "Release container" (the operator,
2026-09-16): "the release container runs TAGGED commits only". The dev container `fly-spike`
(`ROLE=dev` in `<dev-env>`) is where everything else happens: builds,
measurements, deploy trials, the VirtualGL experiment. Never develop or measure on
The release container; never deploy an untagged or dirty tree there.

1. **Tag**, on a clean `main`, with every test suite green:

   ```sh
   # operator box
   infra/build/tag-release.sh v0.2.0
   ```

   `tag-release.sh` refuses to create the tag — and never pushes anything — if you are
   not on `main`, the tree is dirty, the tag already exists (locally or on the
   remote), or any of `npm test`, `npm run typecheck`, `cargo test --workspace` (in
   `services/flysim`), or `infra/tests/lint.sh` fails.

2. **Package**, from that exact tagged commit (fly-build CT or the operator box):

   ```sh
   infra/build/build-flysim.sh
   infra/build/package-release.sh v0.2.0 ./flysim path/to/stage/dist path/to/bridge/dist /path/to/out
   ```

   To build the flysim binary with the **CUDA LIF backend** compiled in, set
   `FLY_CARGO_FEATURES` (default empty — every release through `v0.1.3` was built
   with it unset, and an unset build is byte-for-byte the command those used):

   ```sh
   FLY_CARGO_FEATURES=cuda infra/build/build-flysim.sh ./flysim
   ```

   That is the only supported value today. It needs no CUDA toolkit on the build box
   (the PTX is committed and embedded), and `ldd` on the result lists **no** CUDA
   library — `cudarc` is built with `dynamic-loading`, so `libcuda` is `dlopen`ed on
   first use and a run without `FLY_LIF_CUDA=1` never opens the driver. The same
   binary is therefore deployable to a GPU-less container, and `build-flysim.sh`
   fails the build if a CUDA library ever shows up in the link. Because the GPU tick
   is bit-exact with the CPU one, `--print-compatibility` is byte-identical to a
   CPU-only build of the same tree, so a checkpoint carries over in both directions
   (measured: `infra/docs/cuda-on-dev.md`). Turning the backend **on** is a separate,
   per-container switch — `FLY_LIF_CUDA=1` in the env file, see "CPU partition
   (cpuset)" below.

   The tarball's `MANIFEST` records provenance as `#`-prefixed lines ahead of the
   sha256 checksums (GNU `sha256sum -c` skips comment lines, so
   `05-deploy.sh`'s post-extraction verification still passes):

   ```
   # flybrain release MANIFEST
   # version=v0.2.0
   # git_tag=v0.2.0
   # git_commit=<full sha>
   <sha256>  flysim
   ...
   ```

3. **Deploy to the release container**, from a checkout that is ITSELF at that same clean tag — this
   is the check that matters, not the tarball's own MANIFEST:

   ```sh
   # on the host, from a checkout/worktree at v0.2.0:
   infra/05-deploy.sh <release-env> /path/to/out/flybrain-v0.2.0.tar.gz
   ```

   `<release-env>` has `ROLE=release`, so before touching the container at all
   `05-deploy.sh` runs `git describe --exact-match --tags` and `git status
   --porcelain` against the tree it is itself running from
   (`infra/lib/common.sh`'s `require_release_tag`) and refuses the **entire** deploy —
   not just the release-artifact step — if either check fails. The refusal (verified
   against a throwaway git repo by `infra/tests/lint.sh`) reads:

   ```
   05-deploy: ROLE=release refuses to deploy: source tree at <path> is not exactly at an annotated tag (git describe --exact-match --tags failed)
   ```

   or, for a tagged-but-dirty tree:

   ```
   05-deploy: ROLE=release refuses to deploy: source tree at <path> is not clean (git status --porcelain is non-empty)
   ```

   (both printed after `die()`'s own `[<UTC timestamp>] FATAL: ` prefix). A tarball
   whose own version does not match the tree's tag is refused too, with a message
   naming both versions and the `package-release.sh` command to rebuild it correctly.
   On success, the release directory is named after the tag
   (`/opt/fly/releases/v0.2.0/`), and the last line printed is an claim-log-style
   deploy line — copy it into `$AGENT_CLAIM_LOG` by hand (the same the LAN convention
   as "claim the container ID" in `infra/README.md`):

   ```
   05-deploy: 2026-0X-XX: deployed fly-pokemon (the release container) role=release tag=v0.2.0 release=v0.2.0
   ```

   `ROLE=dev` (the dev container, `<dev-env>`) skips all of the above — any commit deploys,
   and the release directory is named after a short sha + timestamp instead of a tag,
   the historical behaviour, unchanged.

4. **Verify**: `infra/verify.sh <release-env>`.

5. **Rollback**: see "Roll back a release" immediately below. Because the release container's release
   directories are named after tags, "the previous release" and "the previous tag" are
   the same `/opt/fly/releases/<tag>/` directory — `ln -sfn` it back and restart.

## Roll back a release

```
pct exec <ctid> -- ls /opt/fly/releases/           # find the previous version
pct exec <ctid> -- sh -c 'ln -sfn /opt/fly/releases/<previous-version> /opt/fly/current.tmp && mv -T /opt/fly/current.tmp /opt/fly/current'
pct exec <ctid> -- systemctl restart flysim.service flystage-web.service flystage.service
```

One `ln -sfn` plus a restart, per `docs/design/infra.md` section 2. Nothing else needs
touching: `flycast`/`flypush`/`mediamtx` do not read anything under `/opt/fly/current`.
On the release container, `<previous-version>` is a tag (e.g. `v0.1.0`) — "Cutting a release" above.

## Switch the runtime (legacy loop or session runtime)

Two binaries in every release from SERVE-01 on run the same fly: `flysim`, the legacy loop, and
`flysim-session`, the same composition on the session framework (agent, Game Boy environment and
macro task as participants). They read the same `/etc/fly/fly.env`, write the same checkpoint
stores, event log, sugar journal and chat sidecar, and serve the same feed, control API and
metrics through the same listener code, so nothing else on the container can tell them apart.
`flysim.service` is the fly's one unit name either way; which binary it runs is one drop-in,
written and removed by `fly-runtime` (`infra/bin/fly-runtime`, installed to `/opt/fly/bin`).

```
pct exec $CTID -- /opt/fly/bin/fly-runtime status     # legacy | session
pct exec $CTID -- /opt/fly/bin/fly-runtime session    # CUT-01, after the shadow's verdict (starts the speed probation)
pct exec $CTID -- /opt/fly/bin/fly-runtime legacy     # the one-command rollback
```

- **What it does.** `session` writes `/etc/systemd/system/flysim.service.d/10-runtime.conf`
  (`ExecStart=/opt/fly/current/flysim-session`, `FLY_SESSION_DIR=/run/fly/session`),
  daemon-reloads and restarts `flysim.service`; `legacy` deletes the file and does the same. No
  data moves: the store is shared, and each runtime restores the other's checkpoints.
- **What it checks.** `session` refuses a release without `flysim-session`, and refuses when
  `flysim --print-compatibility` and `flysim-session --print-compatibility` differ. After the
  restart both wait for `/healthz` and a `/status` frame that advances
  (`FLY_RUNTIME_HEALTH_TIMEOUT`, 300 s). If the session runtime does not come up healthy, it
  switches back to legacy by itself and exits 1 (`--no-fallback` leaves it to the operator).
  Every switch is logged (`journalctl -t fly-runtime`, `/var/lib/fly/runtime.log`).
- **The fallback outlives the switch (C1).** While the session runtime is selected, the drop-in
  also sets `OnFailure=fly-runtime-fallback.service`, `StartLimitBurst=3`,
  `StartLimitIntervalSec=600` and `RestartMode=direct`, and `MALLOC_ARENA_MAX=2` (glibc arena
  fragmentation was the whole of the 5-8 MB/h RSS growth). `RestartMode=direct` is what makes the
  limit real: since systemd 254 the default (`normal`) sends the unit through the `failed` state
  before every `Restart=always` restart, so `OnFailure=` would fire on the first crash, watchdog
  kill or start timeout (seen on systemd 257, the container's). With `direct` the unit goes from
  one start to the next and only hitting the start limit fails it. Three starts of `flysim.service` within ten minutes, from any
  cause and any caller (a reboot, the watchdog or `fly-loop-recover` restarting it, a deploy, a
  binary that crashes, hangs before READY or is killed by `WatchdogSec`) make systemd run
  `fly-runtime fallback`: it writes `/run/fly/runtime-fellback.json` (time, reason, unit result),
  deletes the drop-in, daemon-reloads and restarts `flysim.service` on the legacy binary, and logs
  a `fly-runtime` journal line. It is idempotent, and it does nothing if the release has no
  executable `flysim`. Look with `fly-runtime status` (it prints the reason) or
  `cat /run/fly/runtime-fellback.json`; the container is then on legacy until an operator runs
  `fly-runtime session` again, which clears the reason file. Restarts you make yourself count
  too, and once the limit is reached systemd refuses the next start and runs the fallback. Three legitimate restarts in ten
  minutes count the same as three crashes: after that many, prefer `fly-runtime legacy`, do the
  work, then switch back.
- **Interrupted switches roll back.** `fly-runtime` traps INT, TERM and HUP (an ssh that drops
  mid-switch): an unfinished `session` removes the drop-in again (or restores the one that was
  there) and restarts `flysim.service` on that state, so a drop-in that never passed its health
  check is never left behind. `session`, `legacy`, `fallback` and that rollback all take one lock
  (`/run/fly/runtime.lock`); a switch holds it while it changes the drop-in and queues the restart
  (`systemctl restart --no-block`, so a unit that never becomes READY cannot hold it) and releases
  it for the health wait. The rollback does nothing when a fallback finished after the switch began (the
  reason file exists), so it can never put back an older drop-in over a fallback.
- **What stays the same.** Everything that names `flysim.service` keeps working unchanged:
  `fly.target`, `flyedge.service`'s `Requires=`, the watchdog, `fly-loop-recover`'s restart and
  its sudoers line, `fly-loop-reset`, `fly-reset-to-milestone` (run with the service stopped, on
  the shared store), the unstick rule's `systemctl restart flysim.service`, `journalctl -u
  flysim`, the cpuset drop-in. The choice survives a reboot and a deploy: `05-deploy.sh`
  converges unit files but never removes a drop-in (it rewrites it in place from the current
  template, without a restart), and it refuses a release without `flysim-session` or with a
  different compatibility string while the drop-in is present (`fly-runtime legacy` first to
  deploy one). **Policy (C2, operator decision 2026-09-29, "Session stays, trust gates"):** a
  deploy keeps the chosen runtime on the new release's `flysim-session`, without re-running the
  shadow for it. The trust gates before the release vouch for the binary, and the persistent
  fallback above protects a bad one.
- **What differs, by declaration.** `POST /reward` is always 403 on the session runtime (the
  operator pulse has no session-framework counterpart, `legacy-gameboy-v1` section 15); a
  fresh start publishes no audio for its one setup frame; `FLY_TRACE` and
  `FLY_PROFILE_SECONDS` are legacy-loop tools (the session runtime logs a `session profile`
  line of per-phase timings every minute instead). The sugar journal's boot header says
  `runtime: fly-session`.
- **The standalone unit.** `flysim-session.service` is the same service as a unit of its own
  (flysim's limits, cpuset and environment; `Conflicts=flysim.service`; no `[Install]`, so it
  cannot be enabled). It is for a rehearsal or a soak on a container whose `flysim.service` is
  stopped, not for switching the stream.
- **The session on the bus (BUS-01).** `fly-runtime session --bus` (or `session-bus`) runs the
  session's participants on the flybus router over Unix sockets: `flysim-session` (the router, the
  coordinator, the listeners) starts `fly-session legacy-agent` and `fly-session legacy-environment`
  as children of the same unit, in `flysim.slice`, on flysim's CPUs. The drop-in gains
  `Environment=FLY_SESSION_TRANSPORT=bus`; `fly-runtime status` prints `session` and then
  `  transport: bus`. `fly-runtime session` without a flag (05-deploy's refresh) keeps the transport
  the drop-in has; `fly-runtime session --local` goes back to the in-process lane, and
  `fly-runtime legacy` to legacy. The fallback, the probation, the watchdog and the unstick rule's
  restart are the same on either transport: one unit, restarted as a whole, children included
  (`ps -o pid,args --ppid $(systemctl show -p MainPID --value flysim)` lists them). A deploy
  refuses a release without `fly-session` while the drop-in says `bus`. **Before switching onto the
  bus, run a shadow of it** (Shadow run below): with the live fly on legacy (`fly-runtime legacy`),
  `fly-shadow-run start --bus`, three hours, then `fly-shadow-run check --bus && fly-runtime
  session --bus`. Watch `fly_frame_work_mean_ms` and `fly_frame_work_p99_ms` on `/metrics` (the
  loop's compute time per frame, without its pacing sleep, over the last 3,600 frames; the budget
  at real time is 16.74 ms): they show the headroom that a realtime factor of 1.0 hides.
- **Per-frame compute time, both runtimes.** `/metrics` exports `fly_frame_work_mean_ms`,
  `fly_frame_work_p99_ms`, `fly_frame_work_max_ms` and `fly_frame_work_frames` for the legacy loop
  and the session runtime alike: everything one running iteration does except the pacing sleep.
  Headroom is `1 - mean / 16.74` at real time.

## Feed over the bus (EDGE-02)

`ws://127.0.0.1:7400/feed` is served either by `flysim` itself (`direct`, how every release before
EDGE-01 did it, and the default) or by `fly-edge` from flysim's embedded feed bus (`bus`,
`docs/design/flybus.md` "Feed over the bus"). The WebSocket contract is the same bytes either way
(`docs/feed-protocol.md`); nothing about the fly, the readout or the compatibility string changes,
on either runtime (`flysim` or `flysim-session`, `fly-runtime`). The stage, the bridge and the
capture never see the difference.

**One switch, one line.** `FLY_FEED_VIA` in `/etc/fly/fly.env` (written by every deploy from the
env file's `FLY_FEED_VIA`, default `direct`). flysim reads it, `flyedge.service` runs only when it
says `bus` (`ExecCondition=`; in direct mode the unit starts as "skipped", inactive and not
failed), and `flysim.service` has `Wants=flyedge.service`, so **every** start of flysim brings the
edge up in bus mode, whoever starts it: a reboot, the unstick rule's restart, `fly-loop-reset`'s
stop and start, `fly-reset-to-milestone`, a crash-restart. There is nothing to enable or start by
hand, and flysim and the edge cannot disagree.

```
pct exec $CTID -- /opt/fly/bin/fly-feed status          # configured / running / the edge and its counters
pct exec $CTID -- /opt/fly/bin/fly-feed bus              # switch (restarts flysim; rolls itself back if unhealthy)
pct exec $CTID -- /opt/fly/bin/fly-feed direct           # the way back
```

### The live procedure

Preconditions: the release has `fly-edge` (v0.7.6 and later; `ls /opt/fly/current/fly-edge`),
`fly-feed` is installed (`/opt/fly/bin/fly-feed`, converged by the deploy), the container is
claimed in the host log as for any host work, and `fly-feed status` says `configured: direct`,
`running: direct`. Pick a moment you would also pick for the unstick rule's restart: **the switch
restarts flysim**, so the stream shows the stage's reconnect screen for about a minute (the
restore, the warm-up and the page's reconnect; the rehearsal measured it, below), and the run
continues from the last checkpoint. It counts as one start against `flysim.service`'s start limit
(3 in 10 minutes on the session runtime), two if it rolls back: do not switch within ten minutes
of two other restarts.

```
# 1. Before: the baseline to compare with (keep the output).
pct exec $CTID -- /opt/fly/bin/fly-feed status
pct exec $CTID -- sh -c 'curl -s 127.0.0.1:9101/metrics | grep -E "^fly_(realtime_factor|lag_seconds|feed_clients|frames_sent_total) "'
pct exec $CTID -- fly-cpu-confine status                 # PERF-02: the sim's cpus are the sim's
# 2. Switch. Blocks until the page is on the feed again (FLY_FEED_HEALTH_TIMEOUT, 300 s) and prints
#    "healthy (bus): the page is on the feed and frames are moving", exit 0.
pct exec $CTID -- /opt/fly/bin/fly-feed bus
# 3. Make it permanent for the next deploy: FLY_FEED_VIA=bus in the operator's env file for this
#    container. (A deploy that finds fly.env and the env file disagreeing warns loudly and writes the
#    env file's value; flysim applies it at its next restart.)
# 4. Look at it (below) for a few minutes, then at the end of the first hour and the first day.
pct exec $CTID -- /opt/fly/bin/fly-feed status
```

What `fly-feed bus` does, in order: refuses unless the release has `fly-edge`, takes
`/run/fly/feed.lock`, rewrites the one `FLY_FEED_VIA` line (every other line, the mode and the owner
kept), restarts `flysim.service` (`--no-block`, then waits for a **new** invocation's `/healthz`),
waits for the edge's `/healthz` (subscribed to the bus, a snapshot within 15 s) and, while
`flystage.service` is active, for the page: `fly_feed_clients >= 1` on the edge's counters with
`fly_frames_sent_total` advancing. Anything short of that within the timeout puts `FLY_FEED_VIA`
back to `direct`, restarts flysim again, checks it the same way and exits 1 (`--no-rollback`
leaves the failed switch in place for the operator; an interrupted switch, ssh dropped or INT, TERM
or HUP, rolls back too). Every switch is in the journal (`journalctl -t fly-feed`) and
`/var/lib/fly/feed.log`.

### What to watch

| Where | Healthy in bus mode | Wrong looks like |
| --- | --- | --- |
| `fly-feed status` | `configured: bus`, `running: bus`, `edge healthz: ok`, snapshot age under a second, `bus lost` and `bind failures` not growing | `running` differs from `configured` (a deploy wrote a new value; the next flysim start applies it); `edge healthz: NOT OK` |
| edge `:9102/metrics` (loopback for the watchdog; the same address answers on the container's network for the dashboard) | `fly_edge_bus_connected 1`, `fly_edge_snapshot_age_seconds` under 1, `fly_feed_clients 1` (the page; the bridge or a test adds more), `fly_frames_sent_total` rising about 30 a second, `fly_edge_decode_failures_total 0`, `fly_edge_bind_failures_total 0` | `bus_connected 0` (flysim down, or restarting), `bind_failures_total` rising (something else holds :7400: a flysim still in direct mode, a stale process), `decode_failures_total` rising (a mismatched flysim and fly-edge: deploy one release) |
| flysim `:9101/metrics` | `fly_bus_published_total` rising at the snapshot rate, `fly_bus_publish_failures_total` flat 0; `fly_feed_clients` and `fly_frames_sent_total` read **0 here in bus mode** (the counters belong to the edge) | publish failures rising: the store is full or the edge's seat is stuck; read `journalctl -u flysim -g "feed bus"` |
| `fly_realtime_factor`, `fly_lag_seconds` | the same as the baseline from step 1, within the usual noise: the publisher is one 6% thread on the host CPU, and the websocket writes it replaces were on that CPU in direct mode | a drop that persists for the hour: switch back (`fly-feed direct`) and compare |
| `ps -L -o pid,psr,comm -p $(pidof fly-edge)` / `grep Cpus_allowed_list /proc/$(pidof fly-edge)/status` | the page CPUs (`flyedge.service.d/cpuset.conf`, written by the deploy), never the sim's | an edge on the sim's CPUs: the cpuset drop-in is missing; re-run the deploy |
| `ls -R /run/fly/bus/store | wc -l`, `du -sk /run/fly/bus` | a few files, under 1 MB, flat for days (the router keeps one retained snapshot and what the edge is still reading) | growth: report it, restart flysim, switch back |
| `journalctl -u flyedge -u flysim -g "bus|feed|edge"`, `journalctl -t fly-watchdog -g flyedge` | `serving the feed from the bus` once per flysim start | `the feed bus went away` outside a flysim restart; a watchdog line `flyedge: ... failures` |
| the stream | the page is up, frames moving | the page on its reconnect screen: `fly-feed status`; if the edge is down `systemctl status flyedge`; `fly-feed direct` is the one-command answer |

The watchdog follows the **running** mode (the running flysim's own environment, so a deploy that
changed `FLY_FEED_VIA` but has not restarted flysim is not an outage). Check 2 reads the feed
counters from the edge in bus mode; a new check 2a judges the edge itself, only while flysim answers
`/healthz` (a down flysim is check 1's): active and `/healthz` 200, one failed pass let go, then
the second pass restarts `flyedge`, the third restarts the page and the encoder as well, and the
fourth **puts the feed back on flysim** (`fly-feed direct --no-wait`, granted in sudoers with exactly
those arguments): one more flysim restart, and `FLY_FEED_VIA=direct` stays until an operator
switches again (put it in the env file too). Check 2 in bus mode leaves a silent edge to 2a rather
than restarting a page that has nothing to connect to, and its chain ends with the edge and the
encoder.

### Rolling back

- `fly-feed direct`: writes `direct`, restarts flysim, waits until the page is on flysim's own
  feed, stops a lingering edge. It needs nothing from the release (it works on one without
  `fly-edge`), and it is the way back if anything above looks wrong. It restarts flysim again, so
  count it against the start limit.
- Automatic: a switch that does not become healthy (above), the watchdog's fourth failed pass, and
  `fly-runtime fallback` (to legacy) all leave `FLY_FEED_VIA` alone: the setting is the same on both
  runtimes.
- A deploy of a release **without** `fly-edge` is refused (before `/opt/fly/current` moves) while
  the env file says `bus` or the container is in bus mode: `fly-feed direct` and set
  `FLY_FEED_VIA=direct` in the env file first, then roll back or deploy.
- A reboot keeps whatever `fly.env` says.

### What the rehearsal measured

Before the first live switch (EDGE-02, 2026-10-02): the real fly (session runtime, real connectome,
macros mode, sim on four CPUs and the page and edge on two others, as on the container), the real
stage page in headless Chromium consuming through `fly-edge`, plus the repo's own units, `fly-feed`
and watchdog under a real systemd. The build box was shared and loaded (load average 8 to 30), so
**real-time factor is not comparable between the two runs**; the numbers below are the ones load does
not move much. The live switch is the real A/B: take the step 1 baseline and compare.

| Measure | direct | bus |
| --- | --- | --- |
| WebSocket content, speed 0.25 so every frame is published, 4,261 and 5,406 frames compared | reference | frame, audio and spike attachments byte-equal on every frame; headers equal except a wall-clock "checkpoint saved" event that lands on different frames in any two runs |
| Latency, snapshot stamped by the sim to received by a local client (median / p95) | 1 / 3 ms | 7 / 14 ms (two hops: seal into the store, read out) |
| CPU per snapshot | 0.24 ms (flysim's websocket thread, on the host CPU) | 2.4 ms in flysim (`flysim-bus` publisher, on the host CPU, about 7% of a CPU at 30 Hz) and 1.1 ms in the edge (page CPUs) |
| Edge CPU and placement | n/a | 1.3% mean, 2.8% p95, only on the page CPUs |
| Router store | n/a | 116 KB steady (median), 238 KB peak, 6 to 9 files, flat over the run |
| Memory over 35 min plus the restarts | flysim 72 to 78 MB | flysim 65 to 69 MB, edge 8.3 to 9.6 MB; the page grows the same in both |
| Snapshots dropped on the way (heavy load) | none seen by a local client | 0.28% sequence gaps at the recorder (the edge sits on CPUs Chromium saturates); none on a quiet box |
| Page | 0 decode errors | 0 decode errors, gaps only under that load |

Recovery, with the page (a reconnecting stage) as the witness: `kill -9 fly-edge`, a SIGTERM
restart of it, and two kills 0.1 s apart (the router's one seat for the edge is released at once)
each had the page streaming again in 0.7 to 0.9 s; a `kill -9` of flysim (router, publisher and
sim) had the edge serving the new router 3 s later and the page 3.4 s later; a SIGTERM restart of
flysim (the unstick rule) 4 and 5.3 s (the page's retries are 0.3 to 1 s apart, the sim restores
from its hot ring). Under the real systemd: `fly-feed bus` took 6 s from command to "page on the
feed", `fly-feed direct` 6 s, a `fly-feed bus` with a broken `fly-edge` rolled itself back after
the 40 s test timeout and left the page on flysim, a stop and start of flysim brought the edge
back by itself, and the watchdog's fourth failed pass switched a permanently broken edge back to
direct with the page connected again. The live restart is longer than these 6 s by the real
restore and warm-up; use the unstick rule's usual time.

### Notes

- **Dashboards.** In bus mode flysim's `:9101` reports `fly_feed_clients` and
  `fly_frames_sent_total` as 0. Scrape the edge's `:9102` too (same names, plus the `fly_edge_*`
  series); the metrics address is on every interface, as `:9101` is, and exposes nothing but
  counters.
- **CPUs.** `flyedge.service` is confined to the page CPUs by the deploy's drop-in; the router and
  the publisher run inside flysim, on its `flysim-bus` runtime (two threads on the host CPU, with
  the listeners, not on a pinned sweep CPU).
- **The store** is `/run/fly/bus` (tmpfs, `fly` 0700, 32 MiB cap, in practice under 0.5 MB). A
  reboot empties it; a crashed flysim's directory is removed by the next one.
- **Control (`:7401`)** stays in flysim and is not part of this switch.

## Shadow run (SHADOW-01)

The session runtime, run beside the live fly as the gate for the automatic cutover (CUT-01). The
contract is `docs/design/session-framework/legacy-gameboy-v1.md` section 18. The shadow is
report-only: it presses no button, serves no port, and only reads flysim's stores and trace. Claim
the release container in the host log first, as for any host work.

```
pct exec <ctid> -- /opt/fly/bin/fly-shadow-run start     # baseline, trace on, remote shadow + guard, one flysim restart
pct exec <ctid> -- /opt/fly/bin/fly-shadow-run status    # the verdict, the relay (and a guard trip) in one screen
pct exec <ctid> -- /opt/fly/bin/fly-shadow-run check     # CUT-01's hook: exit 0 = cutover allowed
pct exec <ctid> -- /opt/fly/bin/fly-shadow-run stop --restart-flysim   # shadow, guard and trace off
```

- **A shadow of the bus topology (BUS-01).** `start --bus` runs the box's shadow session with its
  agent and world as child processes on its router's sockets (`FLY_SHADOW_MODE=bus/process`, which
  the relay forwards); `check --bus` also requires that the verdict ran that topology
  (`candidate.transport` `bus`, `executionMode` `process`). The live fly must be on the legacy
  runtime for any shadow: only the legacy loop writes the trace, and `start` refuses otherwise.

- **Where it runs (SHADOW-02).** On a build box, not on the release container: there it cost the
  live fly real time (2026-09-30: the realtime factor fell from 0.9998 to 0.93-0.98 and came back
  to 0.999-1.0 the moment the shadow stopped), and at about 27 ms a frame on shared CPUs it fell
  further behind every hour. `start` refuses without the remote configuration below; `start --local`
  runs it on the container anyway. In remote mode `flyshadow.service` on the container is the
  *relay* (`fly-shadow relay`, from a drop-in `start` writes): normal scheduling off flysim's CPUs,
  a few file reads and one outbound ssh stream that carries the trace, the new saves and the sugar
  journal to the box and brings the box shadow's verdict back to `/srv/fly/shadow/verdict.json`.
  The guard, `check` and CUT-01 see the unit and the files they always did. Set-up below
  ("Remote shadow set-up").
- **Start** first records the live fly's baseline for 10 minutes, before the shadow exists: sixty
  10-s means of `fly_realtime_factor` (their mean and spread) and the `fly_lag_seconds` growth
  rate. It refuses unless flysim ran normally for most of that time. Then it:
  1. installs `flysim.service.d/shadow-trace.conf` (`FLY_TRACE_DIR=/srv/fly/shadow/trace`,
     `FLY_TRACE_LEDGERS=60`) and, remote, `flyshadow.service.d/remote.conf` (the relay, and this
     run's id: the start's Unix ms);
  2. starts `flyshadow.service` and waits for its heartbeat (remote: up to 5 minutes, while the
     box resets its mirror, restarts its shadow for the new run and reports it alive);
  3. restarts `flysim.service` once;
  4. starts `flyshadow-guard.timer`.

  `FLY_SHADOW_BASELINE_SECONDS` shortens the baseline, for tests only. The restart is the same one
  the unstick rule uses: the rung is kept and the ledgers are cleared. A previous `verdict.json`
  is kept beside the new one, renamed with its time.
- **The guard** runs every 60 s, automatically, and judges the live fly against its own baseline,
  not against real time. A fly that already ran at 0.66 is fine at 0.66. It trips only on a
  sustained degradation: 3 checks in a row in which the last 5 samples of the same flysim process
  fall below the baseline, or grow lag faster than it, by more than the margin. The margin is
  max(0.05, 4 x the baseline spread / sqrt 5). A trip is at the earliest about 7 minutes after a
  real degradation begins. A tripped guard stops the shadow and the guard, removes the drop-in and
  restarts flysim without the trace. It records why in `/srv/fly/shadow/guard-tripped.json`, and
  `status` shows it. A paused fly is not judged, and a restarted one starts its window again.
  Once the shadow is down for good (`ActiveState` inactive or failed: diverged, stopped, given up),
  the guard stops itself and changes nothing. While the shadow waits out its 10-s crash-restart delay
  (`activating`) the guard keeps running. A trip is not a divergence: fix the resources, then `start` again.
- **While it runs**, every later flysim restart (the unstick rule, the watchdog,
  `fly-loop-recover`, `fly-reset-to-milestone`) starts a new trace file. The shadow follows it on
  its own; nothing needs doing. The 3 h window is live *brain* time summed over those processes.
  Stopping or restarting `flyshadow.service` itself starts the window again.
- **Pass**: `status` shows `verdict pass` once both of these hold with zero divergence:
  - 10,800 brain seconds are compared;
  - at least one live save per 10 brain minutes has been compared byte for byte.

  The shadow keeps following, and the verdict stays `pass` only while nothing diverges. CUT-01
  runs `check` at the moment it cuts over. `check` also requires all of these:
  - at least 10,800 brain seconds, whatever window the shadow was started with;
  - the guard has not tripped, `flyshadow.service` is running, `flyshadow-guard.timer` is active and
    `baseline.json` exists (a run no guard watched is never cut over);
  - remote: `/srv/fly/shadow/relay.json` is less than a minute old and `healthy` (connected, the
    box's shadow of this run alive, the sync caught up), and the verdict is this run's (`runId`);
    its `lagTransitions` counts the live trace not yet on the box as well;
  - the verdict is for the release `/opt/fly/current` points to, with `flysim-session` and every
    other shadowed binary unchanged;
  - the shadow is caught up (at most about a minute behind);
  - remote: `relay.json` is this run's (its `runId`, the verdict's and the drop-in's are one), has
    no `coverageLost`, and its `traceAgeSeconds` (the newest live trace file's age) is at most 90 s:
    a trace that is not being written while the shadow claims to follow refuses the check (a paused
    flysim refuses too, until it runs);
  - remote, the two hosts' clocks agree: the verdict's `updatedAt` is the box's clock and the check
    bounds its age to within 60 s, so clock skew between the container and the box makes `check`
    refuse (safe, not silent). Run NTP on both;
  - the verdict's `updatedAt` is neither older than the max age nor more than 60 s in the future
    (a clock stepped back must not keep an old pass fresh);
  - no segment was skipped for a reason other than the live side's own.
- **Divergence**: the shadow stops itself (exit status 3; the unit does not restart it). A failure
  of the session runtime itself counts: it did not start, the restore gate refused the live save,
  or a step failed. The shadow leaves `verdict.json` at `diverged` and
  `/srv/fly/shadow/divergence.json` with the field, both values and the 30 transitions before it.
  On a checkpoint difference it also leaves both checkpoint files. It removes its heartbeat, so
  flysim stops tracing within a minute. The live fly is untouched. Run `stop --keep`, pull the
  files for the review, and do not cut over.
- **The trace cannot outlive the shadow.** flysim starts no trace without the shadow's fresh
  heartbeat (`/srv/fly/shadow/trace/consumer`), and stops a running trace within a minute once the
  heartbeat is more than 10 minutes old. That happens when the shadow crashed, diverged, was
  stopped, or the guard tripped. flysim keeps the trace directory under 8 GiB, and the shadow
  deletes every file it has compared (remote: the box deletes its copy, and the relay then deletes
  the container's). Remote, the heartbeat is the relay's, and it is written only while the relay is
  healthy: a dead box, a dead box shadow, a broken link or a sync more than a minute behind all stop
  it, and flysim then stops its trace as for a dead local shadow. A trace that stops that way while
  the shadow follows it is a `coverage` divergence: that process can no longer be compared. A
  network blip shorter than the 10 minutes costs nothing for the trace: the relay resumes where
  the box's copy ends. The hot saves queued in the relay (400 MiB, about 12 minutes) cover the same
  window; beyond it a save may be missed, which the unavailable-checkpoint allowance absorbs.
  **A stop that did happen is never forgotten** (review B1): after an outage of more than 10 minutes
  flysim stops its trace (`no-consumer`) and runs on untraced. When the link heals the box is alive
  and, counted in trace lines, soon caught up, long before its shadow reaches that stop. The relay
  therefore scans the trace bytes it forwards for the stop and sets `coverageLost` in
  `/srv/fly/shadow/relay.json`; it reports itself not healthy (no heartbeat) and `check` refuses
  until the box's shadow reaches the stop and diverges (`coverage`), or ends its window at that
  stop. **A stop ends the window** (B2): a trace that stopped (no consumer, byte cap) is a coverage
  gap, never a skip. The shadow discards the brain time and saves compared before it
  (`window.ends` in the verdict says how much), and a `pass` needs the whole 10,800 s after the
  last gap. A box shadow that restarts after a gap replays the stopped trace, counts it, and drops
  the count at its stop, so the pass still comes 10,800 s after the gap. `check` refuses while
  `coverageLost` is set (it closes only when the verdict's `window.lastStopTrace` is that trace or
  a later one) and for a verdict without a `window`. Catching up and restarting never clear it.
  The relay flags a byte-cap stop exactly like a no-consumer one. A reward pulse (a declared skip)
  does not hide a later stop in the same trace: the shadow reads on for it and the stop still ends
  the window. After a window end the verdict's `status` is `running` again (not `pass`) until the
  new window has compared the required time.
  **After a long outage (more than the 10 minutes) the run cannot recover in practice**: the shadow
  would have to restart while the stopped trace is still the newest file and reach its stop before
  any flysim restart, and flysim's next boot is untraced (no heartbeat) so the shadow diverges
  (`coverage`). Do not try to nurse it. The operator starts a new run (as root in the release
  container, `pct exec <release-ctid> --` in front): `/opt/fly/bin/fly-shadow-run stop
  --restart-flysim && /opt/fly/bin/fly-shadow-run start`. The old verdict is kept beside the new
  one.
  **Restarting the relay (`systemctl restart flyshadow`) mid-run** removes the heartbeat. If the
  restart is faster than flysim's next minute check nothing is lost, otherwise flysim stops tracing
  and the run is lost (safe: start a new run as above); do not rely on the fast case.
  `coverageLost` is synced to disk before the chunk with the stop is forwarded.
- **Resources**: the shadow is a second whole brain, at about the live fly's CPU per frame.
  Remote, it runs on the box's own cores at normal priority (`flyshadow-remote.service`), and the
  container pays the relay (about 0.5 % of a core, measured; about 2.1 GB an hour over the network:
  the hot save every 5 s is 2.7 MB, the trace about 140 MB an hour) and the trace writer inside
  flysim: about 1.3 ms of the sim thread a frame (measured unpaced on a build box's Haswell, 3
  threads: 11.1 to 12.5 ms). The guard's baseline is taken before the trace is on, so that cost
  counts against it; if the guard trips on a remote run, the trace alone is too much for the live
  fly's headroom. `--local` runs it `SCHED_IDLE` on the page and encoder CPUs, never on flysim's (05-deploy.sh
  writes its `cpuset.conf`), pausing for a minute while flysim's lag grows faster than its
  baseline rate. Disk on the container: the trace until the box has compared it (under 8 GiB);
  on the box: the mirrored saves (under 3 GiB) and a spool of at most 4 GiB.
- **Start** refuses, and starts nothing, unless `flyshadow.service.d/cpuset.conf` exists and the
  unit's `AllowedCPUs` (`systemctl show`) is disjoint from flysim.service's (05-deploy skips every
  cpuset drop-in when the CT conf and CPUSET disagree).
- **Stop** stops the unit and the guard, removes the drop-ins, and deletes the trace and the spool
  (`--keep` keeps them). Without `--restart-flysim`, the running flysim stops its current trace
  within a minute, because its consumer is gone. Remote, the box's shadow keeps running idle until
  the next `start`, which resets its mirror and restarts it for the new run.
- **Remote divergence**: the box's shadow stops (exit 3); the relay brings `divergence.json` and
  both checkpoint files back to `/srv/fly/shadow/`, writes the diverged verdict and exits with
  status 3 (the unit stays stopped, the guard stops itself). Pull the files from the container as
  for a local divergence.

### Remote shadow set-up (SHADOW-02)

Once per build box, and again for every release the container runs. The real values (the box's
address, the key's path) go in the operator's private files, never in this repository. Nothing
on the build box can reach the release container: the container dials out.

1. **The relay's key**, on the release container (as root, claimed):
   `install -d -o root -g fly -m 0750 /etc/fly/shadow-remote`, then
   `ssh-keygen -t ed25519 -N '' -C fly-shadow-relay -f /etc/fly/shadow-remote/id_ed25519` and
   `chown fly:fly /etc/fly/shadow-remote/id_ed25519*`. The private half never leaves the container.
   The container needs an ssh client (`openssh-client`; `02-base.sh` installs it) and no sshd.
2. **The box** (as root on the box): copy the release tarball the container runs, the cartridge,
   `infra/box/` and the public key there, then
   `infra/box/fly-shadow-remote-setup flybrain-<version>.tar.gz <rom> id_ed25519.pub --from <container address> --cpus <its cores> --threads <one per core>`.
   It installs the release at the same `/opt/fly/releases/<version>` path (MANIFEST-verified,
   root-owned), a `flyshadow` user whose home is a root-owned `/srv/fly-shadow-remote-home` (so the
   user cannot rename `.ssh`; the mirror `/srv/fly-shadow-remote` is the user's and is never
   followed through a symlink on a re-run), a root-owned `authorized_keys` whose only line is
   `restrict,command="/opt/fly/current/fly-shadow ingest --root /srv/fly-shadow-remote",from="…" <key>`,
   `/etc/fly/fly-shadow-remote.env` (the box's `FLY_ROM`, `FLY_DATASET`), and
   `flyshadow-remote.service` + `.path` (enabled). Put the box's host key into
   `/etc/fly/shadow-remote/known_hosts` on the container (`ssh-keyscan` from the host, checked by
   hand).
3. **The container's remote file**, `/etc/fly/shadow-remote.env` (root, 0600):
   `FLY_SHADOW_REMOTE=flyshadow@<box address>`,
   `FLY_SHADOW_REMOTE_KEY=/etc/fly/shadow-remote/id_ed25519`,
   `FLY_SHADOW_REMOTE_KNOWN_HOSTS=/etc/fly/shadow-remote/known_hosts`
   (and `FLY_SHADOW_REMOTE_PORT` if the box's sshd is not on 22). `fly-shadow-run start` finds
   it and runs in remote mode.
4. `fly-shadow-run start` as above. **Then watch the live fly's realtime factor by hand for the
   first hour** (`fly_realtime_factor` on the metrics endpoint): stop the shadow if its 30-minute
   median is below 0.99. The guard's floor is a 5 % margin on the baseline and cannot see the
   trace's cost (about 1.3 ms a frame, 2 to 3 %); on the rehearsal it tripped only by chance,
   after an hour. The ingest refuses a relay whose release directory or binaries
   differ from the box's (another release on the container: re-run step 2 with its tarball);
   `journalctl -u flyshadow` on the container shows it.

Which box: never one on the live fly's NUMA node (flysim is memory-bandwidth-bound: in the
SHADOW-02 rehearsal an unpaced flysim on a neighbouring box of the same node took a paced one from
1.0 to 0.68-0.88). The shadow must also run clearly faster than real time (16.7 ms a frame) to
catch up after a restart; measured per frame (mean, unthrottled replay): 11.8 ms on the second
host's box with 4 threads, 11.9-12.2 ms on the build boxes of the release host's other node with
6 threads (13-14 ms with 3-4). Give it 4 cores (second host) or all 6 (same host, other node)
with `--cpus` and `--threads`; keep other heavy work (bx) off that box
for the 3 h (the shadow runs at `CPUWeight=1000`, `Nice=-5`).

## Cutover to the session runtime (CUT-01)

Operator decision (2026-09-30): the stream moves to the session runtime only if that runtime
sustains at least 1.0x real time on the release container, otherwise it stays on legacy. Only the
release container can measure that, so the switch is guarded by a **probation**. Claim the
container in the host log first, as for any host work.

```
# 1. Deploy v0.7.0 with flysim-session installed but NOT selected (no drop-in): the ordinary deploy.
#    The container keeps running legacy; nothing else changes. (fly-runtime status: legacy)
# 2. Shadow, 3 h of brain time, zero divergence, guarded against live impact; on a build box
#    (Remote shadow set-up above, once per box and release):
pct exec $CTID -- /opt/fly/bin/fly-shadow-run start
pct exec $CTID -- /opt/fly/bin/fly-shadow-run status       # until: verdict pass
# 3. Cutover: the shadow's gate, then the switch. One command line:
pct exec $CTID -- sh -c '/opt/fly/bin/fly-shadow-run check && /opt/fly/bin/fly-runtime session'
# 4. Watch the probation (30 minutes after the switch):
pct exec $CTID -- /opt/fly/bin/fly-runtime-probation status
pct exec $CTID -- /opt/fly/bin/fly-runtime status
# 5. Pass, or automatic fallback (below). Nothing to stop: step 3 already stopped the shadow.
# Manual rollback, any time (cancels the probation too):
pct exec $CTID -- /opt/fly/bin/fly-runtime legacy
```

- **Step 3 in detail.** `check` refuses unless the verdict, the guard and the release all line
  up (Shadow run above). `session` then does what "Switch the runtime" describes (compatibility
  check, drop-in, restart, health wait) and, once flysim is healthy on the session runtime, starts
  the probation. `session` returns 0 at that point: the probation is judged later, by the timer.
  `session` also stops the shadow and its guard (`fly-shadow-run stop`, never
  `--restart-flysim`) as soon as flysim is healthy on the session runtime, before the probation
  starts. A guard left running would judge the session runtime against the legacy baseline and
  restart flysim on a trip (a second start inside the 600 s start-limit window, and a reset
  warm-up), and the session writes no trace, so the shadow has nothing to follow. If you stop the
  shadow by hand at any time during the probation, do it without `--restart-flysim`.
- **The probation** (`infra/bin/fly-runtime-probation`, `fly-runtime-probation.timer` every 60 s,
  root). Each tick takes one sample of flysim's `/metrics` (`fly_realtime_factor` averaged over 10
  one-second reads, `fly_lag_seconds`, `fly_uptime_seconds`; the same sampling as the shadow
  guard). It judges the session runtime only:
  - *Warm-up.* A flysim process is not judged before it has been up 300 s (connectome load,
    restore, first minutes). A restart, a pause or a fly that is not `running` starts the window
    again; it is never counted as slow.
  - *Failure*, sustained and not one sample: over the judged samples of the last 10 minutes (at
    least 8 of them) the **median** realtime factor is below **0.97**, or the lag grew by more than
    **30 s** from the window's start to its end (median of the first and of the last three
    samples). The median is the robust statistic the shadow guard's baseline uses: one or two
    stalled minutes do not fail it, a steady 0.85x does, at the earliest about 13 minutes after the
    switch (5 min warm-up, 8 samples). A lag step of more than 30 s inside the window counts as
    lag growth.
  - *Pass*: 30 minutes since the switch, and the latest window is full and healthy. A probation
    that cannot fill a window within 2 h (a fly that never runs) fails as inconclusive: no proof
    of 1.0x, no cutover. `FLY_PROBATION_*` variables tune every number (tests and rehearsals only).
  - **On failure** it runs `fly-runtime fallback "speed: ..."`, exactly the fallback of "Switch
    the runtime": under the same lock, `/run/fly/runtime-fellback.json` with the reason
    (`"speed: median realtime factor 0.850 < 0.97 ..."`), the drop-in removed, flysim restarted on
    legacy, a `fly-runtime` journal line (`journalctl -t fly-runtime`,
    `/var/lib/fly/runtime.log`), and the probation ended (`/var/lib/fly/probation/last.json`
    says `passed: false`). If that fallback cannot complete (no executable `flysim`) it is retried
    every minute. The container is then on legacy and the decision is "no cutover": do not retry
    `session` without a fix (or a new measurement) for the cause.
  - **On pass** it writes `/run/fly/runtime-probation.json` (`{"passed": true, "at", "startedAt",
    "medianRtf", "lagGrowth", "samples", "detail"}`; also kept in
    `/var/lib/fly/probation/last.json` because `/run` is cleared by a reboot) and disables its
    timer. **Nothing judges the speed afterwards.** A continuous guard would make a passing
    cutover fall back at 03:00 on one noisy hour, on a stream that has proven itself, and the
    watchdog already covers a stuck or dead loop. The start-limit fallback also stays as it is.
- **Cancel and resume.** `fly-runtime legacy` cancels the probation, as does every fallback
  (including the start-limit one), and a tick that finds the legacy runtime selected ends it
  without a verdict. The state is `/var/lib/fly/probation/state.json` and the timer is enabled while
  a probation runs (`[Install] WantedBy=timers.target`; nothing else enables it), so a reboot
  during the probation resumes it: `OnBootSec=90s` runs the first tick, flysim's new process
  starts a new warm-up and window, and the 30 minutes keep counting from the original switch.
  `fly-runtime session --no-restart` (05-deploy's refresh of the drop-in) starts no probation;
  `fly-runtime session --no-probation` is for rehearsals. A `session` that cannot start the
  probation (helper or timer unit missing) falls back: an unguarded cutover is not allowed.
- **Reading the result honestly.** The probation is absolute, as decided: a container on which the
  legacy loop itself runs below 0.97x (the release container has been measured at 0.66, and 0.77-0.99
  after its cpuset rebalance) will fail it, and the session runtime was measured at 1.25-1.35x
  legacy's time per frame on the dev box. A fallback with reason `speed` therefore means "this box
  cannot hold real time on the session runtime", not necessarily "the session runtime is
  broken"; compare with `fly-shadow-run`'s baseline (`/srv/fly/shadow/baseline.json`) before
  anything else. The `check` before it measures the live fly against itself; this measures it
  against the clock.
- **Rehearsal** without a host: `fly-runtime-probation simulate CHECKS.jsonl` runs the rule over
  recorded samples (`{t, status, rtMean, lag, uptime}`, one a minute); `infra/tests/lint.sh` drives
  the real `fly-runtime` and the probation over synthetic traces (healthy, 0.85x, a stall, lag
  growth, a restore, a restart, legacy cancel), and the unit wiring has been run under a real
  systemd 257 user manager (state, timer enable/disable, `OnFailure`-style fallback, resume).

## Control over the bus (CTRL-01)

`FLY_CONTROL_VIA` picks who serves `http://127.0.0.1:7401`: `direct` (the default; flysim binds
it) or `bus`. In bus mode flysim registers the control API as RPC services on its embedded flybus
router, and `flycontrol-edge.service` serves the same HTTP bytes on `:7401` by calling them
(`docs/design/flybus.md`, "Control over the bus"). Clients do not change: the bridge, the stage's
health wait, the watchdog, `fly-runtime`, the probation and shadow guards, `fly-loop-recover`,
the stream check and `curl` still talk to `:7401`. It works under either runtime, independently
of `FLY_FEED_VIA`. Claim the container in the host log first, as for any host work.

**Prerequisites.** The release ships `fly-control-edge` (`ls /opt/fly/current/fly-control-edge`).
The env file has `FLY_CONTROL_VIA=bus`, deployed with `05-deploy.sh`. The deploy only rewrites
`/etc/fly/fly.env` and pushes the unit; it neither enables the edge nor restarts flysim, so the
box keeps serving `:7401` directly until step 2.

```
# 1. Before: what the switch must preserve.
pct exec $CTID -- grep -E '^FLY_(CONTROL|FEED)_VIA=' /etc/fly/fly.env   # FLY_CONTROL_VIA=bus
pct exec $CTID -- curl -s 127.0.0.1:7401/status | jq '.frame, .status, .checkpoint'
pct exec $CTID -- curl -s -X POST 127.0.0.1:7401/checkpoint            # a fresh generation to restart from
# 2. The switch: flysim stops binding :7401 and registers the services; the edge (Wants= of
#    flysim.service, started with it because fly.env says bus) binds :7401 once they answer.
#    Between the two, :7401 refuses connections, exactly as during any flysim restart.
#    There is nothing to enable: the unit has no [Install] section.
pct exec $CTID -- systemctl restart flysim.service
# 3. Verify.
pct exec $CTID -- ss -ltnp | grep ':7401'                            # users:(("fly-control-edg"...
pct exec $CTID -- curl -s 127.0.0.1:7401/healthz                     # {"status":"ok"}
pct exec $CTID -- curl -s 127.0.0.1:7401/status | jq '.frame, .status'   # the frame moves
pct exec $CTID -- curl -s 127.0.0.1:9103/metrics | grep -E '^fly_control_edge_(bus_connected|calls_total|call_failures_total)'
pct exec $CTID -- journalctl -u flysim -b --since -5min | grep -E 'control services registered|bus listening'
pct exec $CTID -- ls /run/fly/bus/control-edge.sock /run/fly/bus/control/
pct exec $CTID -- curl -s 127.0.0.1:7410/health | jq .               # the bridge: sim reachable
```

- **Healthy looks like:** `fly_control_edge_bus_connected 1`, `calls_total` rising (the bridge and
  the watchdog poll it), `call_failures_total` flat, no `fly-watchdog:` restart lines, and the next
  real channel-points sugar fulfilled as before. Do not fire a test sugar or chat line on the live
  stream to prove it; both are visible on screen.
- **Watchdog.** With `FLY_CONTROL_VIA=bus`, check 1 reads flysim's own `/healthz` on `:9101`, so a
  dead edge never restarts the fly. Check 1b restarts `flycontrol-edge` alone when `:7401` fails
  while flysim is healthy (the sudoers file grants exactly that restart).
- **Restarts.** `flysim.service` has `Wants=flycontrol-edge.service` and the edge has `Requires=`
  on flysim plus an `ExecCondition=` on `FLY_CONTROL_VIA`, so the edge follows flysim in bus mode
  and stays off ("skipped", not failed) in direct mode. A `restart`, and a `stop` then `start`
  (`fly-loop-reset`, `fly-reset-to-milestone`, the milestone-reset rung of the unstick rule) all
  bring `:7401` back with flysim; nothing needs a separate `start` of the edge. A crash-restart of flysim needs nothing: the edge
  unbinds `:7401`, reconnects every 500 ms and binds again once the new services answer.

**Rollback** (any time, no release change):

```
pct exec $CTID -- sed -i 's/^FLY_CONTROL_VIA=.*/FLY_CONTROL_VIA=direct/' /etc/fly/fly.env
pct exec $CTID -- systemctl restart flysim.service                   # binds :7401 itself; the edge is skipped
pct exec $CTID -- curl -s 127.0.0.1:7401/healthz
# then set FLY_CONTROL_VIA=direct in the env file too, or the next 05-deploy.sh writes bus back.
```

- **Rolling back to a release from before CTRL-01 while in bus mode** is safe. The older flysim
  does not know `FLY_CONTROL_VIA`, so it binds `:7401` itself. The edge unit's
  `ConditionPathExists=` keeps it inactive on a release without the binary, and the older
  watchdog reads `:7401` as before. Switch to direct first anyway, so the env file says what
  runs.
- **If the edge cannot bind** (`fly_control_edge_bind_failures_total` rising, "the control port
  cannot be bound" in its journal), flysim is still in direct mode. `fly.env` says bus but flysim
  was not restarted after the deploy. Restart flysim (step 2).
- **Upgrading a container that had the edge enabled** (before this wiring): an
  old `fly.target.wants/flycontrol-edge.service` symlink is harmless (the condition decides), but
  run `systemctl disable flycontrol-edge.service` once so `systemctl is-enabled` says what is true.

## CPU partition (cpuset)

`05-deploy.sh` derives the in-guest `AllowedCPUs=` drop-ins for every app unit
(`flysim`, `xvfb`, `flystage`, `flystage-web`, `flycast`, `pulse`, `mediamtx`) from two
env-file values, via `lib/common.sh`'s `cpuset_partition`: `CPUSET` (whole physical
cores reserved for the container, `01-create-ct.sh`'s `lxc.cgroup2.cpuset.cpus`) and
`RAYON_THREADS` (flysim's Rayon pool size — also written as `RAYON_NUM_THREADS` into
`/etc/fly/fly.env`, from the same `RAYON_THREADS_EFFECTIVE` value, so the two can never
drift apart).

The split is **three groups**, not two:

1. flysim gets the **first** `RAYON_THREADS` cpus of `CPUSET`.
2. flycast gets the **last** `ENCODER_CORES` cpus (env var, default 2) of what remains.
3. Everything else — `xvfb`/`flystage`/`flystage-web`/`pulse`/`mediamtx` — shares
   whatever is left over in between.

flycast is split off into its own group, separate from the page/capture group, because
of a live finding on the release container (2026-09-16): Chromium's compositor starved when it shared
cores with the x264 encoder — 63% of captured frames came back unchanged across two
`fly-watchdog` passes, against 2% on the NVENC dev box, where the encoder is off-CPU
entirely. **That 63% is the `73/115` half of a pair taken with the wrong instrument** (a
second `x11grab` of the display rather than the encoder output) and has not been
re-measured — see "Capture freeze" above and the correction note in
`infra/docs/p0-measurements.md`. The split itself stays: it is cheap and live on both
containers, but do not cite the number as evidence until someone re-runs it. Before this
fix, only `flysim`/`xvfb`/`flystage`/`flycast` got the drop-in at
all (a two-way split); `flystage-web`/`pulse`/`mediamtx` ran on the container's full,
unpartitioned cpuset.

`X264_PRESET` (env var, written into `/etc/fly/fly.env` as `FLY_X264_PRESET`, read by
`bin/flycast-launch`'s `encoder_x264()`) is the matching encoder-side knob: `veryfast`
is the wire default, but the release container (2026-09-16, release box, x264 at 6000k) measured it
contending within flycast's own (then two-cpu) `ENCODER_CORES` group and moved to
`superfast` for headroom.

**"Extra cores" (2026-09-16, later the same day):** the first fix above still ran
`CPUSET` at eight cpus total (`RAYON_THREADS=4` / `ENCODER_CORES=2`, the function's
default), and that eight-cpu set could not hold x264 + page/browser + sim all at once —
the same Chromium-compositor starvation the three-way split exists to fix, just not
fully fixed by three thin groups on eight cores. The operator chose to widen `CPUSET` to all
**ten** of node 1's whole physical cores rather than shrink flysim or flycast, and set
`ENCODER_CORES=3` explicitly (`<release-env>`, `CORES=10` to match). The release container's live
values — which cpus, how many threads, how many encoder cores — are a record of one box,
not a constant, and live in the operator's infra repo
(`services/flybrain/runbook-host.md`). `cpuset_partition CPUSET RAYON_THREADS
ENCODER_CORES` reproduces the split from those three numbers; `infra/tests/lint.sh`
checks it against both the current shape and the earlier, narrower one.

### The sim's CPUs are exclusive to the sim (PERF-02 review N1)

The session runtime pins its sweep workers one per CPU. A pinned worker cannot leave its CPU, so
any sustained CPU-bound process that is allowed on a worker's CPU stalls every barrier: measured
**0.14x real time** with a plain busy loop (0.68x at nice 19; `SCHED_IDLE` was harmless; the
floating, unpinned runtimes held 0.86-0.94x under the same hog). Priorities cannot fix that, so
the partition above is completed by making the sim's CPUs exclusive:

- `flysim.service` and `flysim-session.service` run in **`flysim.slice`** (`units/flysim.slice`),
  whose `AllowedCPUs=` is the sim's set (`flysim.slice.d/cpuset.conf`, written by `05-deploy.sh` from
  `lib/common.sh`'s `cpuset_dropin_plan`, the same call that writes every other drop-in).
- `bin/fly-cpu-confine apply` sets `AllowedCPUs=` the **non-sim** CPUs on `system.slice` (every
  other service, cron, apt, sshd), `user.slice` (every login session) and `init.scope`, and writes
  the same list to `/sys/fs/cgroup/.lxc/cpuset.cpus` (where `pct exec` / `pct enter` processes land,
  see below). It runs as
  both sim units' `ExecStartPre` (root, failure never blocks the start), so it is re-applied after
  every boot and every restart. The lists come from `/etc/fly/cpuset.env`, which the deploy writes.
- The slice is named without a dash on purpose: systemd nests `a-b.slice` under `a.slice`, and a
  child's cpuset is bounded by its parent's (cgroup v2: effective cpus = own cpus intersected with
  the parent's, and the parent's when that is empty). `flysim.slice` sits beside `system.slice`
  under the root slice, so nothing above it excludes its CPUs.
- **Ordering rule.** Shrinking `system.slice` under a flysim that still sits in `system.slice` (a
  unit started before this change, not yet restarted) would leave its own `AllowedCPUs=` outside the
  parent's set and silently make it float over the page CPUs. So `apply` refuses (exit 3, nothing
  changed) while a sim unit's cgroup is not under `/flysim.slice/`. A deploy therefore changes
  nothing on a running sim; the confinement starts with the next `flysim` (re)start, which is the
  first moment it is safe. `fly-cpu-confine status` shows the result.
- `fly-recap` and `fly-retention` also run at `CPUSchedulingPolicy=idle`, which the review measured
  harmless beside the pinned workers (defence in depth for the batch jobs).
- **No partition, no pinning.** With `CPUSET` unset (or the container conf not matching it) there
  is no confinement, so `05-deploy.sh` writes `FLY_SESSION_PIN=0` to `/etc/fly/fly.env` and the
  session runtime's threads float, as before PERF-02. With the partition in force it writes
  `FLY_SESSION_PIN=1`. (The runtime treats exactly `0` as off.)
- **`pct exec` / `pct enter`:** `lxc-attach` puts its processes in the cgroup `/.lxc` (or
  `/.lxc-N`), a sibling of `system.slice` that systemd does not manage. `apply` writes the non-sim
  list into the `cpuset.cpus` of every such cgroup that already exists (it never creates one; lxc
  owns it). Where that file is not writable it logs a warning and carries on: `apply` never fails
  the sim's start. In that case run long operator jobs through
  `pct exec <ctid> -- systemd-run --scope --slice=system.slice -- <command>`. A `.lxc-N` created
  after `apply` is not covered until the next sim start. `fly-cpu-confine check` lists every
  process outside `flysim.slice` that may run on a sim CPU (exit 1 if there is one); its own
  process and the shell chain it was started from are printed as a note and not counted, so it
  exits 0 on a healthy container when run through `pct exec`.
- **After the first deploy (once):** restart the sim so the confinement is applied, then read it.
  `apply` is refused while flysim still runs outside `flysim.slice`, so nothing changes until then.
  ```
  pct exec <ctid> -- systemctl restart flysim.service
  pct exec <ctid> -- /opt/fly/bin/fly-cpu-confine status    # system.slice, user.slice, init.scope and /.lxc = the non-sim cpus; flysim.slice = the sim's; flysim ControlGroup under /flysim.slice/
  pct exec <ctid> -- /opt/fly/bin/fly-cpu-confine check     # exit 0: "ok: only flysim.slice can reach ..."
  pct exec <ctid> -- systemctl show -p AllowedCPUs flycast.service xvfb.service   # still the drop-in values
  ```
  Then watch the probation RTF. A `check` that names a process is a hog to move or stop.
- **If lag grows on the session runtime**: `fly-cpu-confine check`, then `top -H` for a stray hog on
  the sim's CPUs. `FLY_SESSION_PIN=0` (a drop-in) restores floating.
- `fly-cpu-confine release` undoes the runtime confinement (until the next sim start); it sets
  `system.slice`, `user.slice`, `init.scope` and `/.lxc` back to the container's whole cpuset
  explicitly, because systemd keeps the old mask for an empty `AllowedCPUs=` while the cpuset
  controller stays on for another unit. (Tested in `tests/lint.sh` against a fake cgroup tree.)
- **Rollback by re-running an older infra tree's `05-deploy.sh`** (rather than the symlink flip in
  "Roll back a release", which is fine): an infra tree older than this change has no
  `Slice=flysim.slice`, so flysim restarts into `system.slice`, which is still confined to the
  non-sim CPUs. Its `AllowedCPUs=` then does not intersect that set and it floats over the page and
  encoder CPUs while the sim's own CPUs sit idle (the slow shape again, not a black stream). After
  such a rollback run `pct exec <ctid> -- /opt/fly/bin/fly-cpu-confine release` (or reboot), then
  restart flysim.

Verified on a real-systemd box (4 CPUs, the sim as a unit in `flysim.slice` on 3 of them, 3 sweep
threads, paced at 0.55x): confined or not, with no hog the session holds 0.55; five busy loops (three
as system services, two from a user shell) unconfined sit on the sim's CPUs and the session drops to
0.37; confined by the unit's own `ExecStartPre`, every loop samples on CPU 0 only, the sim's threads
keep their CPUs and the session holds 0.54-0.55.

### `FLY_LIF_CUDA` — the LIF tick on the GPU, and what it does to the partition

`FLY_LIF_CUDA=1` in an env file makes `05-deploy.sh` write `FLY_LIF_CUDA=1` into
`/etc/fly/fly.env`, which is what flysim reads at startup to attach the CUDA LIF
backend. **Default 0**, written for every container either way so the file says out
loud which backend is configured. Two separate switches, deliberately:

| | switch | where | effect |
|---|---|---|---|
| build | `FLY_CARGO_FEATURES=cuda` | `build-flysim.sh` | the backend is *compiled in*, `libcuda` still only `dlopen`ed |
| run | `FLY_LIF_CUDA=1` | the env file → `/etc/fly/fly.env` | the backend is *attached* |

A binary without the feature ignores `FLY_LIF_CUDA=1` **silently**, so confirm the
backend from flysim's own log, not from the env file:

```sh
pct exec <ctid> -- journalctl -u flysim -b --no-pager | grep -i 'cuda\|lif backend'
```

It needs `GPU=1` (the `/dev/nvidia*` device block plus the userspace driver in the
container — `docs/design/gpu.md` sections 1 and 2) and the card the committed PTX
targets, `sm_75`.

**It changes `RAYON_THREADS`.** With the sweep and the propagation on the card, flysim
holds real time on about one host core, so the dev container (`<dev-env>`) runs
`RAYON_THREADS=2` instead of 3 — two rather than one because the pool still shards
`plasticity.observe`. The freed cpu goes to the page group, which is the whole reason
to do this: `CPUSET=0,2,4,6,8,10,12,14` with `RAYON_THREADS=2` gives flysim `0,2`,
`xvfb`/`flystage`/`flystage-web`/`pulse`/`mediamtx` `4,6,8,10` and flycast `12,14`
(`infra/tests/lint.sh` checks that shape too). **Set it back to 3 if you set
`FLY_LIF_CUDA=0`** — two cpus is below real time for the CPU kernel.

Flipping backends does **not** cost the checkpoint: the GPU tick is bit-exact, so the
compatibility string `05-deploy.sh` gates on is unchanged and a container can move
between the two across restarts in either direction. Measurements, the golden-suite
run on the card, the 3 h soak and the restore drill: `infra/docs/cuda-on-dev.md`.

## Capture freeze

The symptom: the stream looks alive — audio perfect, `frame=` advancing at 30 fps in
`/run/fly/flycast.progress`, 0 dup / 0 drop, page healthy, display updating — and the
**picture only changes about once a second**. It lasts for the life of the `flycast`
ffmpeg process. Full findings, including the ten dev-box experiments and what is *not*
established, in `infra/docs/capture-freeze.md`.

**Measure it on the encoder output. Never on a second `x11grab` of `:99`** — the display
is fine during this failure, so a display-side grab reports PASS on the very thing you are
looking for (see that doc's section 4, and the correction note in
`infra/docs/p0-measurements.md`).

```sh
# On the host. Newest segment, last 3 s: count frames identical to the one before them.
# 80+ of ~90 is frozen; a healthy stream reads 0-6.
pct exec <ctid> -- sh -c '
  seg=$(ls -1t /srv/fly/media/rec/*.ts | head -n1); echo "$seg";
  ffmpeg -hide_banner -sseof -4 -i "$seg" -t 3 \
    -vf "tblend=all_mode=difference,blackframe=amount=99.5:threshold=8" \
    -an -f null - 2>&1 | grep -c "Parsed_blackframe.*] frame:"'

# Or read what the watchdog already measured (every 5 minutes, check 9):
pct exec <ctid> -- grep fly_capture /var/lib/node_exporter/textfile/fly_watchdog.prom
pct exec <ctid> -- journalctl -t fly-watchdog -g 'capture freeze' --since -2h
```

`fly_capture_identical_frames` is the last probe's count (`-1` before the first probe) and
`fly_capture_freeze_restarts_total` counts the restarts check 9 issued.

**Restart procedure.** Restarting `flycast` alone clears it, and nothing else needs to move:

```sh
pct exec <ctid> -- systemctl restart flycast.service
# Under 10 s of stream gap (the wait-for-stage settle is 5 s of it, and it should pass
# immediately because the page is already up); the recording rolls to a new segment;
# flysim, the page and flypush are untouched. Then confirm with the probe (0-6 identical):
pct exec <ctid> -- journalctl -u flycast -n 30
```

The watchdog does this itself after **two consecutive** bad probes, at most **once per 30
minutes**, so if the counter is climbing do not also restart by hand — read
`journalctl -t fly-watchdog -g 'capture freeze'` first.

If it comes back immediately after a restart, the ordering gate did not help and the freeze
has a path this repo has not seen: check `journalctl -u flycast -g wait-for-stage` for what
`wait-for-stage` decided, run `pct exec <ctid> -- runuser -u fly -- env
XDG_RUNTIME_DIR=/run/fly PULSE_SERVER=unix:/run/fly/pulse/native
/opt/fly/bin/wait-for-stage --once` for the three readiness checks by hand, and record it
against `infra/docs/capture-freeze.md` section 5 rather than restarting in a loop.

**Why the ordering gate exists.** `flycast.service` is `After=flystage.service` and runs
`ExecStartPre=/opt/fly/bin/wait-for-stage 120`, which returns only once the X display
answers, a Chromium kiosk window is mapped on `:99`, the pulse sink `stream` has at least
one sink-input, and all three have held for 5 s. A new pulse client attaching during
ffmpeg's first one to three seconds is what triggers the freeze, and Chromium's audio
stream at page load is exactly such a client. After 120 s the wait warns and starts the
encoder anyway (a black-but-running stream beats no stream), so a `flycast` that starts
while the page is missing is expected behaviour, not a bug — but it *is* the state the
freeze likes, so check the probe afterwards.

## Loop suspected

The symptom: everything is healthy and nothing is happening. The sim advances, the page
draws, the stream encodes, the macros all report `done` — and the fly is pressing the same
short cycle over the same two tiles, hour after hour, with the stuck-o-meter climbing. The
live case was Viridian on 2026-09-17: `GO NPC, GO OUT, NEXT, GO FRONTIER` every ~3 brain
seconds for 2 h 17 min on rung 8. Mechanism and audit in `infra/docs/macros-traps.md`.

**The watchdog never acts on this.** Check 10 detects and reports; a human or a review agent
decides. It does not restart `flysim`, it does not press anything, it does not touch the game
— a loop is a bug in what a macro's target choice considers, and a bounce would only restore
the same loop with the stream interrupted for nothing. Do not "fix" it by restarting either.

```sh
# What check 10 measured, every 5 minutes.
pct exec <ctid> -- grep -E 'fly_loop|fly_places' /var/lib/node_exporter/textfile/fly_watchdog.prom
pct exec <ctid> -- journalctl -t fly-watchdog -g 'loop ' --since -6h
# The report, written on every probe, for a human or a review agent:
pct exec <ctid> -- cat /run/fly/wd/loop.json | jq .
```

| series | reading |
| --- | --- |
| `fly_loop_suspected` | `1` = a short cycle is repeating and no new ground was covered. `0` = looked, clear |
| `fly_loop_period` | length in macro labels of the repeating block (`0` = nothing repeats, `-1` = no probe yet) |
| `fly_loop_repeats` | how many times that block repeats at the end of the 10-brain-minute window |
| `fly_loop_distinct_macros` | distinct macro names started in the window |
| `fly_places_delta` | growth in `game.uniqueLocations` since the previous probe (`-1` = no previous probe) |
| `fly_loop_refused` | macro presses refused in the window: a bound button pressed, nothing run |
| `fly_loop_blocked` | macros that ended `blocked` or `timeout` in the window |
| `fly_loop_done` | macros that ended `done` in the window |
| `fly_loop_rewards` | reward events in the window |
| `fly_loop_fights` | `MOVE n` decisions (a move chosen in a battle) in the window |
| `fly_loop_wins` | battle-won reward events (`wildwin`, `trainer`, `badge`) in the window |

The flag needs **both** halves: at most 3 distinct macro names with the block repeating 20+
times, one macro at 95%+ of the window's decisions, 90%+ of 20+ decisions ending refused,
blocked or timed out (`stalled`), or decisions with no `done` among them on two probes in a row
(`zero-progress`), or 100+ decisions with no reward event among them on two probes in a row
(`unrewarded`, row 58: `GO OBJECTIVE` in and `GO OUT` out of one door, diluted by eight other
names, every macro `done`), or 24+ `MOVE n` decisions and no battle won on two probes in a row
(`unwon-battles`, row 67: one Pewter Gym trainer lost thirty times, whiteout and back, with a
stray tile reward now and then keeping `unrewarded` quiet) — **and** no growth in the exploration count. A decision is a `start` or a
`refused`: a refused press starts nothing, which is why counting starts alone read row 57's
pad (`GO ROUTE refused` ~740 times in ten brain minutes, `macros-traps.md`) as one start and
one name. A
repeating macro over ground that keeps growing is a walk longer than the 600-frame cap, not a
trap (`macros-traps.md`: `GO FRONTIER` x19 across 93 tiles), and the watchdog is deliberately
quiet about it. The thresholds are `WD_LOOP_*` in `infra/bin/fly-watchdog`; the 3-name ceiling
is narrower than the 4-macro cycle that was actually live, so a cycle of four or more names
reports through `fly_loop_period`/`fly_loop_repeats` without raising the flag. Read the gauges,
not just the flag.

**What to do with it.** Reproduce it off the stream, from the box's own state, and fix the
macro — never the stream:

```sh
# 1. Pull the checkpoint the loop is in, before it rolls out of the manifest.
pct exec <ctid> -- cat /srv/fly/state/manifest.json | jq .        # note "latest"
pct pull <ctid> /srv/fly/state/<generation>.checkpoint \
    <host-stage>/<ctid>-loop-$(date -u +%Y%m%d%H%M).checkpoint
pct pull <ctid> /run/fly/wd/loop.json <host-stage>/<ctid>-loop.json
pct pull <ctid> /srv/fly/state/events.jsonl <host-stage>/<ctid>-events.jsonl
# ... then copy both onto the dev box, into .local/checkpoints/ (never committed).

# 2. Run the trap hunt over it on the dev box, which flags every two-brain-minute
#    window by tiles and by repeated sequence (infra/docs/macros-traps.md, "The trap hunt").
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/release-loop-<stamp>.checkpoint \
  FLY_TRAP_MINUTES=20 cargo run --release -p flysim --example trap_hunt
```

The report's `sequence` names which macros to read first, `milestone.rank`/`label` says which
rung's goals they were aiming at, and `map` says where. Add what you find as a row in
`infra/docs/macros-traps.md`'s audit table — including "left, and why" — and fix it as a change
to a macro's target choice, never to a ranking, a prior or a fallback action
(`docs/design/macros.md` section 12).

Check 10 goes quiet on its own, with one `loop cleared` line, as soon as the exploration count
moves again. That line is how a deployed fix is confirmed from the outside.

## Disk full

The three independent brakes, in the order they trigger
(`docs/design/infra.md` section 8):

1. **ZFS quota** on `<bulk-pool>/subvol-<ctid>-disk-1` (600G by default) — the
   filesystem itself refuses new writes once hit. This is "the one that actually
   protects the neighbours" (the neighbouring GPU container, the metrics container, another container on the host, another guest on the host on the same pool).
2. **`fly-retention.timer`**, hourly, prunes segments > 7 days, highlights > 90 days,
   orphan checkpoints.
3. **`fly-watchdog`'s disk guard** (`bin/fly-watchdog`'s `check_disk`): >85% triggers an
   immediate extra retention pass; >95% sets the `fly_watchdog_disk_critical` alarm
   metric and runs retention in `--aggressive` mode (1-day segment window instead of 7).
   **Known gap:** at 95% the design calls for also dropping the recording leg of
   `flycast` while keeping the stream up; this infra pass does not implement a
   no-recording variant of the fixed `flycast.service` command (see that unit's own
   header comment). If retention alone cannot get ahead of a runaway recorder, the
   manual fallback is `pct exec <ctid> -- systemctl stop flycast.service` (this also
   stops the stream — accept the outage over risking the pool) followed by manual
   cleanup of `/srv/fly/media/rec`, then `systemctl start flycast.service`.

Check current usage: `pct exec <ctid> -- df -h /srv/fly/media /srv/fly/state`.

## GPU driver version lockstep

`docs/design/gpu.md` sections 2 and 8. The single permanent operational coupling the GPU
work adds: **the host's NVIDIA kernel modules and each container's NVIDIA userspace must
be the same version string**, today `580.76.05`. The host's modules are a hand-patched
build against `7.0.14-11-pve`; the patch repo is the operator's own kernel-patch repo
(per the operator's own GPU-driver notes, the stock sources do not build against 7.x).
The container userspace comes from the **stock**
`<host-stage>/NVIDIA-Linux-x86_64-580.76.05.run`, installed by `02-base.sh` with
`--no-kernel-modules --silent --no-x-check`.

`NVIDIA_VERSION` in the env file is the declared version, and `verify.sh` asserts three
things agree: that value, the container's `nvidia-smi`, and the **host's** `nvidia-smi`.

### What breaks, in what order, after a host driver or kernel upgrade

1. The guest's `libcuda`/`libnvidia-encode` refuse to talk to a mismatched kernel module.
   `nvidia-smi` inside the CT prints **"Failed to initialize NVML: Driver/library version
   mismatch"** and `flycast` cannot open an NVENC session. The automatic fallback in
   `bin/flycast-launch` catches this: the stream continues on `libx264` at about 2 cores,
   `fly_encoder_backend{backend="x264"}` goes to 1, and `fly-watchdog` logs
   `encoder degraded:` and raises `fly_watchdog_encoder_degraded`. Without that fallback
   `flycast` would crash-loop and the stream would go black.
2. A kernel update rebuilds the modules via DKMS, but the stock sources do not build
   against 7.x, so **fresh patches may be needed**. Always check `nvidia-smi` on the host
   **and** in every guest after a kernel update — including the neighbouring GPU container, which shares this card.
3. A driver **upgrade** is an ordered, three-step operation. Do the spike CT first:

   ```sh
   # 1. on the host: patch + install the new kernel module (nvidia-kernel-patches)
   # 2. reboot the host, so fly-nvidia-majors.service rewrites the majors for the
   #    newly loaded module (the uvm major moves on essentially every boot)
   # 3. for each CT: bump NVIDIA_VERSION in env/<name>.env, then
   infra/02-base.sh <release-env>      # installs the matching userspace
   infra/verify.sh  <release-env>      # lockstep + char devices + majors
   ```

   **Never upgrade the host driver without a window in which the streams may be on the
   x264 path.** Two containers at ~2 cores each on a 2015 Haswell box is survivable but
   not free.

### Symptom-to-cause, fastest first

| Symptom | Look at |
|---|---|
| `nvidia-smi` in the CT: "Driver/library version mismatch" | host driver moved; re-run `02-base.sh` (step 3 above) |
| `nvidia-smi` in the CT: "No devices were found" | passthrough, not the driver. `ls -l /dev/nvidia*` in the CT — a **regular file** where a character device should be means the bind's source was missing at container start |
| `/dev/nvidia-uvm` present but NVENC fails | stale majors. `/usr/local/sbin/fly-nvidia-majors.sh <ctid>`, then restart the container |
| `fly_encoder_backend{backend="x264"} 1` with `FLY_ENCODER=nvenc` | the fallback fired. `nvidia-smi --query-gpu=memory.used,utilization.encoder --format=csv` — the GPU workload in the neighbouring container can hold 6-7 of the 8 GB |
| Everything fine, `flycast` still crash-looping | not the GPU. `journalctl -u flycast -n 50`; a non-session error is deliberately **not** faulted over to x264 |

The card is shared with **the neighbouring GPU container (`ml`, the GPU workload in the neighbouring container)**, which belongs to the LLM work. Compute
mode stays `Default`; never set `nvidia-smi -c EXCLUSIVE_PROCESS`, which would let
whichever container got there first lock the card out from under the other.

## The host reboot order

Fly containers have no special reboot ordering requirement relative to each other
(`onboot=1`, `startup order=4,up=60`, per `01-create-ct.sh` — they start after
the neighbouring production service/order-lower services, with a 60s stagger). After any the host reboot:

There is one host-side ordering requirement, added by the GPU work
(`docs/design/gpu.md` section 1). Two units must complete **before**
`pve-guests.service`, because `lxc.*` keys are read only at container start and the
`nvidia-uvm` major is allocated dynamically and moves on essentially every boot:

```
nvidia-devnodes.service   (already on the host: materialises /dev/nvidia*)
    -> fly-nvidia-majors.service   (rewrites the majors into /etc/pve/lxc/<id>.conf, one per fly guest plus the neighbouring GPU container)
        -> pve-guests.service      (starts the guests, which read those configs)
```

`fly-nvidia-majors.service` is `Before=pve-guests.service` and the fly CTs'
`startup order=4,up=60` keeps them behind it. If that unit is missing or disabled, the
containers come up with a **stale** uvm major and no announcement: `verify.sh`'s GPU
section is what catches it. Install per `infra/host/README.md`.

**Do not be alarmed by the shape of `/etc/pve/lxc/<id>.conf` after a boot.** PVE re-emits
that file on every lifecycle operation with its own keys sorted alphabetically, every
comment hoisted to the top of the file and every raw `lxc.*` key moved to the end, so the
`# BEGIN fly-nvidia` / `# END fly-nvidia` pair will be sitting at the top with nothing
between it while the eight lines it generated sit at the bottom (measured on PVE 9.2.10,
2026-09-16). The passthrough is fine — the `lxc.*` lines are what LXC reads, and they are
all there. `fly-nvidia-majors.sh` converges lines rather than bytes and still reports
`unchanged` in that state. What *would* be a fault is the same script reporting `changed`
on two consecutive runs.

After any the host reboot:

1. Confirm the neighbouring production container (the neighbouring production service) is healthy first — it is the priority guest on this host.
2. `systemctl status fly-nvidia-majors.service` on the host — want `active (exited)`,
   `status=0/SUCCESS`. This is the check the operator's own GPU-driver notes call the
   silent one.
3. Confirm the fly containers came back: `pct list`, then `infra/verify.sh
   <release-env>` and `<platformer-env>`. The GPU section asserts every
   `/dev/nvidia*` is a character device, that the conf's majors still match
   `/proc/devices`, and that the driver versions are in lockstep.
4. `flysim` restores from its checkpoint automatically; expect a brief gap while
   `wait-for-health` gates `flystage`'s Chromium start (up to 120s per
   `flystage.service`'s `ExecStartPre` timeout).
5. If `flypush` was enabled before the reboot, confirm it reconnects to Twitch within a
   couple of minutes; if not, check `journalctl -u flypush` for an RTMP-level error
   before assuming a key problem.

## Known gaps (recorded, not smuggled in as "done")

- **No alerting path.** Everything degrades to a Prometheus textfile metric and a
  journal line at `err` with the `fly-watchdog:` prefix. There is no page/SMS/Slack
  path from this host (`docs/design/infra.md` section 0 and section 8).
- **95% disk guard does not stop `flycast`'s recording leg**, only prunes harder — see
  "Disk full" above.
- **`fly-recap`'s event/segment schema is inferred, not contract-specified.** Its
  header comment explains exactly which fields are assumed; update it once
  `services/flysim` actually ships `events.jsonl`/`segments.csv`.
- **`flybridge.service`'s `ExecStart` path is a placeholder** (`services/bridge` does
  not exist yet). It will fail to start until that package ships, which is expected
  under `Wants=` semantics and does not block `fly.target`.
- **`/dev/shm` resize via a `dev-shm.mount` drop-in is inert — confirmed on the P0 spike
  (2026-09-15), no longer a maybe.** An unprivileged Debian 13 LXC on PVE 9 has no
  `dev-shm.mount` unit at all (LXC mounts `/dev/shm` itself before systemd starts), so
  the drop-in does nothing. Two consequences. First, `/dev/shm` comes up at **94.4G**
  (half the host's RAM), not the small tmpfs the design feared, so Chromium will not run
  out of space there; the residual risk is the reverse, a runaway charging 94G of tmpfs
  against an 8G container limit and being OOM-killed rather than getting `ENOSPC`.
  Second, it cannot be fixed from inside the guest either: the mount carries
  `uid=100000` from the unprivileged id-map and `mount -o remount,size=1G /dev/shm` as
  root in the guest fails with `tmpfs: Invalid uid '100000'`. The only real fix is a
  host-side `lxc.mount.entry` in `/etc/pve/lxc/<ctid>.conf` — an the operator by-hand step,
  which nothing reachable through `pct exec` can do. `verify.sh`'s size check should be
  read as "is it at least 1G", not "is it exactly 1G".

## P0 spike checklist

Run the ten measurements in `docs/design/infra.md` section 4 against the dev container
(`<dev-env>`) and fill in `infra/docs/p0-measurements.md`. Do not skip ahead to P1
provisioning of the real containers until that file's go/no-go line is checked GO.

```
infra/provision.sh <dev-env>
# ... run the measurements by hand against the dev container (P0 is manual instrumentation,
# not scripted — see docs/design/infra.md section 4 for exactly what to run) ...
# when done and no longer needed:
pct stop <dev-ctid> && pct destroy <dev-ctid>
```
