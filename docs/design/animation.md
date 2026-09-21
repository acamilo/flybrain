# Animation and moments

Locked 2026-09-15 night. Intensity: halfway between instrument and arcade. Animated, satisfying,
"endorphin-hitting", never busy. Every motion is caused by data; nothing loops for decoration.

## Rules

- Compositor-only properties (`transform`, `opacity`, canvas draws). No layout-triggering
  animation, no `filter: blur`, no `box-shadow` on moving elements.
- Easing: out-expo for arrivals (fast in, soft settle), in-out for tab crossfades. Durations:
  arrivals 240 to 320 ms, flashes 120 ms up and 600 ms down, holds 9 s for moments.
- One moment at a time. A queue with priority: badge > milestone > rollback > sugar > small
  reward. A lower-priority moment waits; equal priority coalesces.
- Sound is tiered with the visual: small reward tick, sugar tone, milestone chime, badge fanfare,
  rollback rewind. Synthesized in the page, master gain from config.
- Reduced motion is not a viewer concern here (it is a broadcast), but every animation still has a
  static end state so a frozen frame reads correctly.

## Catalogue

| Trigger | Where | Motion | Sound |
|---|---|---|---|
| Tab cycle (timer or steering) | tab slot | 300 ms crossfade of content, underline slides to the tab, dot row advances | none |
| Small reward (explore, area, wild win) | ticker | line slides up 240 ms, value flashes amber 120/600 ms | tick |
| Sugar | fly, progress cluster, ticker | proboscis extends over 400 ms and holds for the pulse, head glow blooms then decays with the PAM rate, sugar ring fills instantly and drains over the cooldown, ticker line with the name | tone |
| Milestone (rung up) | tab slot, spine, ticker | caption band enters from the left over the tab slot (out-expo), spine rung pulses twice then fills amber, LADDER tab takes focus for 9 s, then returns | chime |
| Badge | whole rail | rail border flashes amber 120/600 ms, badge count ticks up with a 1.15x bounce, connectome flare (all recent spikes at full glow for 500 ms then decay), caption band, LADDER focus 9 s | fanfare |
| Rollback (ratchet restore) | game, ladder | game canvas does a 400 ms horizontal "rewind" wipe to the archived frame with a scanline flicker, best-snapshot thumbnail blinks twice, try count ticks, caption "REWIND · try 3" | rewind sweep |
| Mode change (walking, battle, menu) | title chip | chip text swaps with a 200 ms vertical roll | none |
| Day rollover | title strip | "DAY N" slides across the strip once, 2 s | soft stinger |
| New chat line | chat | slides up 240 ms; bot lines green | none |
| Idle fly | fly strip | breathing, wing tremor, antennae twitch, all scaled by population rate | none |
| Connectome | tab | spike glow (existing), plus reward flare on any reward event scaled by value | none |
| HERE FOR crossing 1 h, 3 h, 6 h | progress cluster | the number does a single pulse and its colour steps warmer at each threshold | none |

## Budget

All of the above must keep the page's whole-frame paint p95 under 4 ms on the laptop and under the
measured budget on the host. Moments never touch the game or fly canvases except the rollback wipe.
Implement in one module (`src/moments.ts`) with the queue, and one CSS file for keyframes, so the
catalogue above stays reviewable.

## Verification

Playwright with the fixture: drive each trigger from the big-moment fixture and assert the DOM
state at t+100 ms and at t+9.5 s (entered, then gone), the queue ordering with two simultaneous
events, the tab crossfade leaves exactly one tab visible, and the frame-paint budget from the
performance hook.

## Addendum (the operator): smooth transitions, particles, lerp everything

- Every displayed number and bar is lerped toward its target each frame (rates, Hz, counters,
  spine fill, sugar ring), never snapped; time constants 120 to 300 ms so motion is continuous
  between 30 Hz feed updates.
- Tab and moment transitions are interpolated (position, opacity, scale), not cut.
- Particles, on a 2D canvas layer over the rail (no WebGL): sugar spawns a burst of small
  warm sparks that drift from the fly's head toward the sugar chip; milestone spawns amber
  sparks from the new rung; badge spawns a larger fountain over the rail plus a shockwave ring
  from the badge count; rewards spawn a few sparks from the ticker line scaled by value;
  rollback spawns a backward-streaming line field over the game. Budget: at most 400 live
  particles, pooled, additive blending, decay 600 to 1200 ms.
- Connectome: reward flare spreads outward from the PAM cluster position over 400 ms.
