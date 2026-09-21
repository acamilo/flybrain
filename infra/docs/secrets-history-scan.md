# Secrets scan over the whole git history

Run 2026-09-17 against the local checkout at `main` = `c9858cc`, before any public push.
Scope: **every commit on every ref**, not just the working tree.

No secret value appears anywhere in this document. Findings are described by *shape* and
*location* only.

## What was run

| tool | version | command | coverage |
| --- | --- | --- | --- |
| gitleaks | 8.30.0 | `gitleaks git . --log-opts="--all" --redact` | 460 non-merge commits, ~8.27 MB of diffs |
| gitleaks | 8.30.0 | `gitleaks dir .` | the checked-out tree, 2.02 GB incl. `target/` and `node_modules/` |
| trufflehog | 3.90.10 | `trufflehog git file:///<checkout>` | 322,604 chunks, 4.23 GB, all branches |
| grep (own pattern set) | — | every blob in the object DB, see below | 5,791 objects incl. dangling blobs |

Both binaries were installed as prebuilt releases under `~/.local/bin` (no root, no package
manager). Repository size for reference: 563 commits, 87 refs, one author.

The grep pass exists because gitleaks and trufflehog know generic provider shapes but not this
project's own ones. It enumerates `git rev-list --objects --all` plus the dangling blobs from
`git fsck --lost-found`, pipes each blob through `git cat-file blob`, and matches:

- `-----BEGIN [A-Z ]*PRIVATE KEY-----`
- Twitch IRC tokens: `oauth:[A-Za-z0-9]{20,}`
- Twitch/RTMP stream keys: `live_[0-9]{6,}_[A-Za-z0-9]{20,}`
- `Authorization: (Bearer|Basic|OAuth) <12+ chars>`
- `client_secret` / `clientSecret` / `CLIENT_SECRET` assigned a 20+ char value
- `stream_key` / `STREAM_KEY` / `streamKey` assigned a 12+ char value
- `access_token` / `refresh_token` / `accessToken` / `refreshToken` assigned a 16+ char value
- `api_key` / `API_KEY` / `apiKey` assigned a 16+ char value
- `password` / `passwd` / `PASSWORD` assigned an 8+ char literal
- `pass show` / `pass ls` (this repo's credential store is `pass`, so captured output would
  look like a shell pipeline in a script or a log)
- AWS `AKIA…`, GitHub `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`, Slack `xox[baprs]-`, OpenAI `sk-`
- a separate broad sweep for any 32+ char high-entropy token not already explained by a
  checksum, `integrity` field or base64 blob

## Findings

gitleaks: **no leaks found**, on history and on the tree.
trufflehog: **0 verified, 0 unverified secrets** (one non-critical decode error on
`data/fafb-v783/weights.binz`, which is a gzip stream trufflehog tried to read as brotli — not a
finding).

The grep pass matched 14 blobs across 5 paths. Every one is a *reference to* a credential, never
a credential. Full list:

| # | shape matched | path | commits (first → last touching) | verdict |
| --- | --- | --- | --- | --- |
| 1 | `pass show "$PASS_KEY"` shell pipelines, 4 blobs | `infra/06-secrets.sh` | `0d1a762` → `69816e6` | **False positive.** The script's whole job is to pipe `pass show` into `systemd-creds` on the container; the value never lands in a file and never in git. `PASS_KEY` holds a pass *entry name* (`twitch/fly-pokemon-key`), not a secret. |
| 2 | `pass show … \| head -c8` in a verify helper, 6 blobs | `infra/verify.sh` | `0d1a762` → `57276d4` | **False positive.** Reads the first 8 bytes of the stored key to compare a prefix against what the container holds. The prefix is computed at run time and printed nowhere that is committed. |
| 3 | `accessToken:` / `refreshToken:` struct fields, 3 blobs | `services/bridge/src/auth.ts` | `d9f00b6` → `48cd40a` | **False positive.** Twurple `AccessToken` field names in code (`accessToken: token.accessToken`). No literal values. |
| 4 | `accessToken:` / `refreshToken:` struct fields, 1 blob | `services/bridge/tools/authorize.mts` | `77f5892` | **False positive.** Same: the interactive authorize tool writes the token it just fetched to a path outside the repo. |
| 5 | the words `pass show \| systemd-creds` in prose | `infra/docs/release-ct-provision.md` | `6598ad0` | **False positive.** A provisioning log describing the mechanism. |

Broad high-entropy sweep: 1,101 blobs carried a 32+ char token line, all accounted for by
`package-lock.json` integrity hashes, the FlyWire `meta.json` / `tools/artifact-checksums.txt`
sha256 digests, and vendored `binjgb` tables. Nothing credential-shaped.

## Negative results worth recording

- **No `.env` was ever committed.** The only `*.env` paths in history are
  `infra/env/fly-pokemon.env`, `infra/env/fly-platformer.env` and `infra/env/spike.env`, which
  are *deployment profiles*, not credential files. Dumping every `KEY=value` line those three
  files have ever carried, across every commit, yields only CT sizing (`CORES`, `CPUSET`,
  `MEMORY_MB`), network (`IP`, `HOSTNAME`), stream geometry (`STREAM_WIDTH`, `STREAM_KBPS`),
  feature flags, `PASS_KEY` (an entry *name*), `TWITCH_CHANNEL`/`TWITCH_BOT_USER` (public
  handles) and `PUSH_TARGET`. `.gitignore` covers `.env` and `.env.*` with a
  `!.env.example` exception.
- **No key material of any kind.** Zero matches for `BEGIN … PRIVATE KEY`, `id_rsa`,
  `id_ed25519`, `*.pem`, `*.key`, `*.p12`, `*.pfx` as paths or as content.
- **No stream key, ever.** Zero matches for the `live_…` RTMP shape or for a `STREAM_KEY=`
  assignment. Stream keys reach the containers only through `pass` → `systemd-creds`.
- **The remote carries no credential.** the operator's git remote — SSH, no
  token in the URL.
- **One advisory, not a secret:** `ROM_SHA256=0e85…219f` is committed in
  `infra/env/spike.env` and `infra/env/fly-pokemon.env`. It is a cartridge fingerprint, so it
  is not a credential and needs no rotation, but it does identify a specific commercial ROM
  dump. Flagging it for whoever is handling the licence/redaction pass, not for this scan.

## Verdict

**Clean. Nothing to rotate, nothing to rewrite history for.** Three independent engines
(gitleaks, trufflehog, a project-specific grep set over every blob including dangling objects)
found zero real credentials across all 563 commits. All 14 grep hits are code and prose that
*name* credentials handled by `pass` at run time. The hard rule in `CLAUDE.md` — "Stream keys and
tokens live in `pass`, never in git" — holds in the history as well as in the tree.

## Re-running this

```sh
gitleaks git . --log-opts="--all" --redact --no-banner
trufflehog git "file://$PWD" --no-update
```

Neither is wired into `.forgejo/workflows/ci.yml` or `.github/workflows/ci.yml`: CI runs no
secret-bearing step, and a full-history scan is a pre-publish gate rather than a per-push one.
Run it again before any new public push, and after any commit that touches
`infra/06-secrets.sh`, `infra/env/*.env` or `services/bridge/src/auth.ts`.
