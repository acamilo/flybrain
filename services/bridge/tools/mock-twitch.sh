#!/usr/bin/env bash
# Documented Twitch CLI commands for manually exercising each flybridge EventSub handler against
# a local mock transport. This script is reference documentation, not a test runner — nothing in
# `npm test` requires the Twitch CLI (`docs/design/stage-bridge.md` B4). Install it from
# https://github.com/twitchdev/twitch-cli if you don't have it.
#
# Usage: run each block by hand in a separate terminal while flybridge points SIM_CONTROL_URL at
# the fake sim (`npx tsx packages/feed/src/fake/server.ts`) and its EventSub client at the mock
# WebSocket server started below.
set -euo pipefail

cat <<'EOF'
1. Start the mock EventSub WebSocket server (leave running):

   twitch event websocket start-server

   It prints a "Started WebSocket server" line with a ws://127.0.0.1:8080/ws URL. Point
   flybridge at it instead of the real Twitch EventSub endpoint by passing that URL through
   `startEventSub`'s `url` option (services/bridge/src/eventsub.ts) — e.g. temporarily set it in
   a small script that calls `startEventSub({ ..., url: 'ws://127.0.0.1:8080/ws' })`.

2. In another terminal, trigger events. Each corresponds to one handler in src/eventsub.ts:

   # !fly / !brain / !how / !stuck / !sugar (src/eventsub.ts -> src/commands.ts)
   twitch event trigger channel.chat.message --transport=websocket

   # Edit the generated event's `message.text` field to "!sugar" / "!stuck" / etc. before
   # sending, or use --to-user / --from-user to target specific chatter identities. The CLI
   # writes the event JSON to a temp file first when run without --transport=websocket, which is
   # the easiest way to edit message.text before delivery:
   twitch event trigger channel.chat.message > /tmp/chat-event.json
   #   ... edit /tmp/chat-event.json's event.message.text ...
   twitch event trigger channel.chat.message --transport=websocket -f /tmp/chat-event.json

   # Follow thanks (src/eventsub.ts handleFollow)
   twitch event trigger channel.follow --transport=websocket

   # Raid thanks (src/eventsub.ts handleRaid)
   twitch event trigger raid --transport=websocket

   # Sugar redemption (src/redemptions.ts, behind FEATURE_REDEMPTIONS)
   twitch event trigger add-redemption --transport=websocket
   #   ... edit the event's reward.title to "Sugar" (or reward.id to match the created reward) ...

   # Revocation handling (src/eventsub.ts onRevoke)
   twitch event trigger revoke --transport=websocket

3. Watch flybridge's stdout for the chat replies it sends, and curl its own health/metrics
   endpoints to confirm state updated:

   curl -s http://127.0.0.1:7410/health | jq .
   curl -s http://127.0.0.1:7410/metrics

   chatSubscriptionHealthy tells you whether the channel.chat.message subscription was actually
   created — not the same question as eventSubConnected (src/subscription-health.ts). Note that
   the self-healing watchdog is live in a mock session too: if the mock server never confirms
   that subscription, flybridge logs one FATAL line and EXITS 75 after EVENTSUB_GRACE_MS (60 s
   by default). That is correct behaviour, not a mock-setup bug — set EVENTSUB_GRACE_MS high if
   you want to poke at a half-wired session by hand.

   # Revoking the chat subscription (step 2's `revoke`) therefore also starts that 60 s clock.
EOF
