# flybrain

A simulated fruit-fly brain (FlyWire connectome, 139,255 neurons) that plays Game Boy games on a
24/7 stream. Monorepo: `packages/brain` (TypeScript reference core), `packages/feed` (contracts,
codec, fake sim), `services/flysim` (Rust service: brain + emulator + adapters), `services/bridge`
(Twitch), `apps/stage` (broadcast page), `infra/` (LXC provisioning and units), `docs/`.

Read `docs/architecture-tour.md` first. Then `docs/stream-mvp-plan.md` for decisions and status.

## Binding contracts

- `docs/feed-protocol.md` and `docs/control-api.md`. Where a design doc differs, the contracts win.
- `packages/brain` is the oracle. Never change its semantics to match another implementation; fix
  the other side. Default-config version strings `lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2` stay.

## Hard rules

- Never go live on Twitch without the operator's explicit approval for that run. `flypush.service` stays
  disabled; local MediaMTX demos are fine. Stream keys and tokens live in `pass`, never in git.
- No AI attribution lines in commit messages.
- ROMs are never committed, copied into the repo, shown on stream, or linked.
- Fable (the coordinator) plans, writes contracts and reviews; opus and sonnet agents build and
  test, each on its own feature branch in a worktree, merged with `--no-ff`. Run `npm test`,
  `npm run typecheck`, `cargo test --workspace` and `infra/tests/lint.sh` before merging.
- The operator reviews screens as PNGs (`apps/stage/mockups/`), never as prose. On-screen copy is terse.
- Work on the deployment host is serialised: **one agent at a time**. Claim the container before
  touching it and release it when you are done, by appending a dated line to the host's agent
  claim log — the file the operator's `AGENT_CLAIM_LOG` names (`infra/env/example.env`,
  `infra/README.md`). No claim, no host work. Never touch a guest this repo did not provision;
  other services share the host.
- This repo is public. Nothing that identifies the operator's network goes in it: no hostnames,
  LAN addresses, container ids, host paths, account ids, channel names, people's names, `pass`
  entry names or forge URLs. Say "the host", "the release container", "the dev container", "the
  channel", "the operator"; put the real values in the operator's infra repo. Real values belong
  in an env file outside the checkout — see `infra/env/README.md`. `infra/tests/lint.sh` refuses
  the patterns; the rules live in the operator's infra repo and
  `infra/tests/de-pii-allow.txt` the few legitimate mentions.
