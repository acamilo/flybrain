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
| One `FATAL:` then a quiet, `chatSubscriptionHealthy: true` bridge | It healed. | Nothing. Note it in the status log if it was during a stream. |
| `active (running)`, `chatSubscriptionHealthy: false`, and NO `FATAL:` line after more than 2 min | The watchdog itself is not running — an old bundle | Check `/opt/fly/current/bridge/index.js` is from a release that has it, then `systemctl restart flybridge`. |

The manual hammer, still correct and still fast, is `pct exec <release-ctid> -- systemctl restart
flybridge`. It costs nothing but the startup notice (suppressed if one went out in the last 10
minutes) and does not touch flysim, the page or the broadcast — `fly.target` only `Wants=` this
unit, so chat can never take the fly down.

## Rotate the bot token (flybridge)

Not yet applicable until flybridge exists and is wired up (P3, `docs/design/infra.md`
section 7). Once it is: re-run `tools/authorize.mts` (`docs/design/stage-bridge.md`
section B1) on the operator box to get a fresh refresh token, which
`RefreshingAuthProvider`'s `onRefresh` persists to `/var/lib/fly/bridge/tokens.json`
automatically from then on — a one-time manual step, not a recurring one. The
`twitch-app` credential (client id/secret) only needs rotating if the app itself is
compromised; follow the same `06-secrets.sh` pattern as the stream key.

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
FLY_ACCEPT_ADAPTERS=pokered-unique8-v5 infra/05-deploy.sh <release-env> <release-tarball>
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

The flag needs **both** halves: at most 3 distinct macro names with the block repeating 20+
times, one macro at 95%+ of the window's decisions, 90%+ of 20+ decisions ending refused,
blocked or timed out (`stalled`), or decisions with no `done` among them on two probes in a row
(`zero-progress`) — **and** no growth in the exploration count. A decision is a `start` or a
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
