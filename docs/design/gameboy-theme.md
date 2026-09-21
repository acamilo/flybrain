# Game Boy theme pass (T1 revision)

Requested by the operator 2026-09-15 night after the first live demo: "more gameboyey, with the mono fonts
and structure." This revises the T1 Instrument theme in place; it is not a fourth theme.

## Type

- Titles, rung names, big numbers, tab labels, button chips: Press Start 2P (already bundled, OFL).
- Body, ticker lines, chat, counters, captions: a pixel-style monospace with real legibility at the
  floors. Candidates, all OFL and on Google Fonts: VT323 (tall, narrow, very readable), Silkscreen
  (bolder, boxier), Pixelify Sans (rounder). Pick by mockup; self-host the chosen woff2 in
  public/fonts with its licence. Drop Inter and IBM Plex Mono from the page entirely.
- Tabular digits everywhere numbers change (monospace makes this automatic).
- Text floors unchanged (body >= 27 px, labels 33 to 39, hero >= 72 at 1080). Pixel fonts render
  at integer multiples of their native size where possible (VT323 native 8 px grid: use 32/40/48).

## Structure

- Every panel is a Game Boy dialogue box: square corners, a 4 px outer border with a 2 px inner
  line (the double frame), no drop shadows, no rounded radius. Tab strip drawn as boxes joined to the
  slot frame; the active tab's bottom border opens into the content like a folder tab.
- Bars are chunky: 12 px tall, hard edges, filled in 8 px steps (quantized), no gradients.
- The spine is a row of square cells with 2 px gaps.
- Chips (buttons, mode) are boxes with the same double frame, 4 px corner cut instead of radius.
- Selection cursor "▶" (as a small pixel triangle) marks the current rung and the active tab.

## Palette

Keep T1's amber accent and near-black ground, but shade everything else on four steps, in the
spirit of the DMG's four-tone LCD: ground #0b0e12, dark #1e232a, mid #4a5460, light #c9d1c8, plus
amber #f0a72e and the two role colours (dopamine pink, taste cyan) unchanged for the bars. No
Nintendo green-screen pastiche: the game panel is the only thing that looks like a Game Boy screen.
No Nintendo assets, fonts, sprites or logos anywhere.

## Motion

Unchanged catalogue, but easing on pixel elements snaps to 4 px steps (a "stepped" easing variant
in motion/lerp.ts) so movement reads as sprite motion rather than smooth interpolation, except for
particles and the fly, which stay smooth.

## Gate

Two mockups (SENSES tab and LADDER tab) before wiring; the operator picks the body font from three
candidates rendered in the same frame.
