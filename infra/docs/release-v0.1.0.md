# Release v0.1.0 — first tagged deploy, method

> **The record lives in the operator's infra repo** (`services/flybrain/release-v0.1.0.md`), verbatim and dated.
> This file keeps only what the run established about the procedure.

The first tagged release deployed to the release container: annotated tag, built off that
tag, deployed from a checkout at that tag, CPU-only (`GPU=0`, `FLY_ENCODER=x264`).

**The run deployed and verified the release. It did not prove the container holds real
time** — and that is the lesson, not a footnote. The box held real time while the encoder
was feeding only the container's own MediaMTX, and stopped holding it the moment
`flypush` was also remuxing to Twitch: the realtime factor fell just below 1 and
`fly_lag_seconds` grew and **never came back** (it is an accumulated pacing shortfall —
the loop never skips a frame, so it only ever grows). Do not read "ALL CHECKS PASSED" as
"ready to leave alone".

What the recovery established, and what the scripts now encode:

- `RAYON_THREADS` and `X264_PRESET` are **load-bearing** and belong in the env file,
  not in a hand-edit on the container: `05-deploy.sh` regenerates `/etc/fly/fly.env`
  and the `AllowedCPUs=` drop-ins from the env file on **every** run, so a hand-edited
  container is silently reverted by the next tagged deploy, mid-stream.
- The encoder needs its own cpu group, separate from the page: Chromium's compositor
  starves when it shares a physical core with x264.
- `ENV_LABEL` is derived from the env file's basename rather than the path as typed,
  because the same file reached by two spellings made `converge_file` see a content
  change and re-push every generated file for nothing.
- A release deploy runs from a checkout staged on the host; unpacking one as root leaves
  it owned by the archive's uid, and then every `git` call fails with a dubious-ownership
  refusal that `require_release_tag` used to report as "not at an annotated tag".
