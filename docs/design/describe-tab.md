# DESCRIBE tab

The operator, 2026-09-17: fourth tab on the rail after SENSES / CONNECTOME / LADDER. One densely packed
card, no cycling ("keep it to a densely packed card"). Copy approved by the operator 2026-09-17 ("good.
style it send it"). The tab renders exactly this; the copy lives in
`apps/stage/src/games/describe.ts` as one entry, so a copy change is a one-file edit and a rebuild.
Silkscreen for the title, VT323 for the text at the body floor; the neuron and synapse counts
come from the dataset at runtime; nothing else is dynamic.

## Card (approved)

### A CONNECTOME MEETS A GAME BOY
This is a real fly's brain, 139,255 mapped neurons and 2.7 million synapses, running live. The
screen is its eye. Its motor neurons press the buttons. Each scene offers a few actions, walk to
a door, talk, attack; the fly picks one. When the game rewards it, a few thousand synapses shift,
and what worked gets likelier. !sugar sends it a small reward pulse, no buttons. FlyWire connectome.

## The new-chatter switch (the operator, 2026-09-17)

The operator, same day, after the card was approved: "when new user joins chat, switch to it for a few
sec, cool down timer."

A chat line from a display name this page has not seen before takes the slot to DESCRIBE for
8 seconds, then hands it back to whatever was up, on the rail's existing focus-and-return — the
same tab-change motion a big moment uses, not a second kind of switch. For 120 seconds after a
switch, further new names do not trigger one; they still count as seen, so the switch means
"somebody new turned up recently" rather than being a queue of arrivals to work through.

Both numbers live in `apps/stage/src/lib/tabs.ts` as `NEW_CHATTER_HOLD_MS` and
`NEW_CHATTER_COOLDOWN_MS`, because that file owns the rail's cadence; tune them there.

What never triggers it (`apps/stage/src/lib/chatters.ts`):

- the bridge's own replies (`bot: true` lines from flybridgebot) — the bot is not a person arriving;
- any line older than the page's connect time. `header.chat` is a ring the feed re-sends every
  snapshot and a reconnect hands the whole ring back, so history must not replay as arrivals. This
  is also what keeps a recorded fixture inert: its lines carry the wall time of the recording;
- a name already seen, whether or not its first sighting actually switched the tab;
- a moment holding the slot, and the cooldown is not spent in that case.

The pinned slot (`?tab=`) disables it along with the rest of the cycle, so the mockup and the
screenshot baselines are unaffected.
