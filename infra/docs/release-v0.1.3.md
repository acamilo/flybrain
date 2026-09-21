# Release v0.1.3 — method

> **The record lives in the operator's infra repo** (`services/flybrain/release-v0.1.3.md`), verbatim and dated: the tag, the
> commit, the box, the timings, the journal excerpts and the post-deploy checks.

The procedure this run followed is `infra/docs/runbook.md`'s "Cutting a release". Nothing
about the method is specific to this release; what is specific is the artifact and the
numbers, and those moved.

The two habits these runs put into the runbook:

- **Two `fly_lag_seconds` samples, one either side of the single `05-deploy.sh` call,
  before any restart.** The metric only grows, so a single reading after a change says
  nothing about the change.
- A release container takes **one** `05-deploy.sh` call and then the minimum restart:
  restarting units that did not change is how a deploy drops a live stream for no reason.
