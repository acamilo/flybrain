# Fly avatar and the 1080p canvas

Decided 2026-09-15. A small 3D fly sits under the game screen, facing it as if playing. Every
motion is driven by a real population rate from the feed; nothing is scripted or random except
idle breathing. This is the one place the stream shows the body the connectome belongs to.

Revised 2026-09-15 (review): the fly briefly tapped a drawn Game Boy with its front legs. The operator's
call — "the fly isn't really pressing buttons; just have his limbs and wings wired up to the motor
neurons" — removed that: there is no Game Boy or button-tapping in either renderer any more, the
legs and wings answer only to real motor and steering rates, and the eight button indicators are
their own plain row again, above the fly's own canvas.

## Canvas

The broadcast canvas becomes native 1920x1080 (`#stage` authored at 1920x1080; the `?res=720`
mode becomes a 2/3 downscale for tests and thumbnails). Twitch target 1080p30, 6000 kbps CBR,
2 s keyframes; the ffmpeg command in `infra/units/flycast.service` and `infra/config` change
accordingly, and the Xvfb screen becomes 1920x1080x24. Safe inset 48 px.

Left column, 800 wide, 984 usable height: title strip 40; game 800x720 at exactly 5x; 4 px gap;
fly strip 800x220 (a plain 32 px button row along its top edge, full width, and the fly's own
canvas filling the rest). 40 + 720 + 4 + 220 = 984 exactly. Right rail: 1920 - 96 - 800 - 12 =
1012 wide, same five rows as today scaled to the new height; the connectome inset grows to about
380x270 and still promotes over the rail on big moments.

## The fly

Procedural low-poly Drosophila built from primitives in three.js (already an optional peer
dependency of `@flybrain/brain`; becomes a real dependency of `apps/stage`). No external model
files. Parts: head with two large compound eyes (red, faceted by a normal map or vertex noise),
antennae, thorax, abdomen with segment banding, six legs with three joints each, two translucent
wings, halteres, proboscis that can extend. Body length about 120 px on screen (a little larger
than the first cut, now that the strip is fly-only). Camera behind and above the fly at roughly
30 degrees, looking over its head toward the game screen, same framing as before; there is nothing
else in the 3D scene to look at.

Rendering: WebGL through three.js if the SwiftShader measurement from the P0 spike (measurement
k) shows under 0.5 core at 30 fps for a 3,000-triangle scene at 800x220; otherwise the same scene
through a CPU fallback (three.js with a software renderer is not maintained, so the fallback is a
2D canvas "paper fly": the same rig projected by hand with flat shading). Decide once, from the
measurement, and record it here.

## Neuron mapping (feed `rates`, Hz per role)

| Role(s) | Neurons | Drives |
|---|---|---|
| `forward` (DNp09) | 2 | tripod gait speed; above resting it walks in place toward the screen |
| `backward` (MDN) | 4 | reverse gait; wins over forward when its normalized score is higher |
| `steer_left`, `steer_right` (DNa01/02) | 2 + 2 | body yaw a few degrees toward the stronger side, and asymmetric leg stride (the outer leg reaches further, the same shape a tank uses to turn) |
| `command_0..7` (sum) | 1,305, all of `descending` | wing/flight drive: wing beat amplitude and frequency, and haltere jitter. Stand-in for a `motor` role rate (110 neurons) — TODO once `rates.motor` exists in the feed, that is the honest source; `WING_DRIVE_ROLES` in `src/fly/drives.ts` is the one table to change |
| `proboscis` | 24 | proboscis extension length; sugar pulses visibly extend it |
| `reward_pam` | 307 | warm glow inside the head and thorax, brightness by rate |
| `populationRate` | all | breathing amplitude of the abdomen |

Every value is normalized the same way the circuits panel does (running reference per role), never
hard-coded Hz. The wing/flight drive sums `command_0..7`'s raw rates and running references
*before* scoring, so the group reads as one drive against the sum of its own recent normal — the
same formula a single role uses, which is what makes the `motor` swap above a one-line change.

## Copy

None on the fly's own canvas — no caption, it is self-explanatory. The button row's eight glyphs
(`UP DN LF RT A B ST SE`) are the strip's only text, and they are indicators, not an explanation.

## Verification

Playwright: strip renders within 2 s of `data-ready`; its only text is the button row's glyphs;
WebGL context count is exactly one (the fly) and zero when the fallback is active; the button row
is a plain row of eight chips that respects the 250 ms afterglow; proboscis extends on a sugar
event. Unit: gait speed rises with the forward drive, and stride asymmetry follows differential
steering (`tests/unit/fly-rig.test.ts`) — this replaced an earlier Playwright check that the leg
landed on a button cap, which no longer applies now that the legs do not tap anything. Performance:
fly render under 4 ms per frame on the laptop and the measured budget on the host.
