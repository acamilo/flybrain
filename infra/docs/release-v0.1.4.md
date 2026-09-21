# Release v0.1.4 — the chat bridge learns to heal itself, method

> **The record lives in the operator's infra repo** (`services/flybrain/release-v0.1.4.md`), verbatim and dated.
> This file keeps only what the run established.

The fifth tagged release, and the fourth deployed to a container that was already
broadcasting. Read `infra/docs/runbook.md`'s "Cutting a release" for the procedure, its
"Chat dead, bridge active" section for what this release ships, and
`docs/design/stage-bridge.md` section B5 for the design.

**v0.1.4 is the first release that changes `flybridge` and the first that restarts
`flypush`**, so it is the first one where the deploy could take chat or the push down
rather than just the sim.

What it shipped, and why: a connected EventSub websocket is **not** a working
subscription. Both sockets can drop, Twitch can refuse the re-created
`channel.chat.message` subscription with "number of websocket transports limit exceeded"
(429), and the bridge then sits `active (running)` with no chat subscription at all —
which is exactly what happened, for an hour. The bridge now exits 75 when that
subscription has been unconfirmed for `EVENTSUB_GRACE_MS`, and systemd restarts it with
a fresh transport; `NOTICE_MIN_INTERVAL_MS` keeps the self-healing restarts from turning
into chat spam. `chatSubscriptionHealthy` is the field to read — never
`eventSubConnected` on its own.

Two auth lessons from the same period, both now in the code: refresh **before** validating
a stored token (a broadcaster token that expired while a previous process held a fresher
one in memory fails startup with 401 in a crash loop), and persist refreshed tokens for
**both** roles when one account holds both.
