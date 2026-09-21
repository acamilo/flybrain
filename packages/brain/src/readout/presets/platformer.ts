/**
 * Game Boy platformer readout preset.
 *
 * Same eight channels, bits and `command_*` rate roles as the Game Boy preset — this module
 * re-exports them rather than restating them — with only the `DecoderConfig` numbers changed. No
 * decoder code changes: every value below is a parameter the decoder already reads.
 *
 * Each change is argued in `docs/design/platformer.md` §4 from the Super Mario Land disassembly:
 *
 * - `holdMs === decisionMs` (250/250, against the Game Boy preset's 400/400 with an implicit gap).
 *   Momentum builds in `0xC20C` up to 6 and decays one per frame with no direction held, so a gap
 *   in the right-hold erases running speed, and running speed is what clears gaps.
 * - All four directions stay: `down` enters pipes and crouches as Super Mario, `up` is needed in
 *   the two vehicle levels, so dropping either would make 2-3 and 4-3 unplayable.
 * - `hysteresis` 1.15 -> 1.25: a reversal triggers a reverse animation and resets momentum, so a
 *   challenger should need a 25% lead.
 * - `fatigueGain` 0.08 -> 0.04, `fatigueDecay` 0.8 -> 0.85. Habituation exists to break a stuck
 *   corner; here the game's own timer breaks a stuck fly, so the network should be able to hold
 *   right for many seconds.
 * - `a` 85 -> 300 ms hold, 480 -> 420 ms cooldown: jump height is variable, so an 85 ms tap is a
 *   minimum-height hop that can never clear a two-block gap. The hold-to-height curve is not
 *   source-verified (the physics is still `INCBIN`-ed in the disassembly), so 300 ms is a measured
 *   starting estimate, not a derived constant.
 * - `b` is a 600 ms hold on a 200 ms cooldown. B is run-faster and Superball fire, which needs a
 *   hold. The decoder has no hold primitive outside the exclusive group, but because the cooldown
 *   is shorter than the hold, a channel whose score stays above threshold refires the instant the
 *   hold expires: a gapless sustained hold that releases within 600 ms of the score dropping.
 * - `start` and `select` are boot-only. The adapter reports boot for every non-playable state, so
 *   the permissive variant applies on the title screen and after a game over, while a 10-minute
 *   cooldown at threshold 2.0 makes pausing effectively impossible during play. Pausing is dead
 *   air on a 24/7 stream.
 *
 * Keeping both system channels in one throttle group is a safety property, not only a throttle:
 * Super Mario Land soft resets when A, B, Select and Start are held in the same frame
 * (`and a, $0F / cp a, $0F / jp Init`, bank0.asm:1075), and a throttle-group fire writes
 * `nextAllowed` for every member, so Start and Select can never be held together — the reset
 * combination is structurally unreachable. `tests/readout.test.ts` asserts it.
 *
 * One disclosed prior: `right` is first in `channels`, and insertion order breaks argmax ties
 * (`docs/readout.md`, "Ties keep the earlier channel"), so a tie favours rightward travel. A tie
 * is measure-zero in practice; the prior is accepted and disclosed on the honesty panel rather
 * than hidden (§4, "Doctrine check").
 *
 * These are readout parameters: fixed once, identical for every run, carrying no learning.
 */
import type { DecoderConfig } from '../decoder';

export {
  GAMEBOY_BUTTONS,
  GAMEBOY_BUTTON_BITS,
  fromButtonMask,
  toButtonMask,
} from './gameboy';
export type { GameboyButton } from './gameboy';

/** Decoder configuration for the platformer demo. */
export function platformerDecoderConfig(): DecoderConfig {
  return {
    exclusive: {
      channels: { right: 'command_3', left: 'command_2', down: 'command_1', up: 'command_0' },
      decisionMs: 250,
      holdMs: 250,
      hysteresis: 1.25,
      fatigueGain: 0.04,
      fatigueDecay: 0.85,
      // No blocked-direction cooldown: a platformer that is not moving is usually falling,
      // standing on a ledge or being carried, not bumping a wall, and `docs/design/platformer.md`
      // §4 chose its own commitment. The rule stays off until it is measured there.
      blockedFatigue: 0,
      blockedMs: 0,
    },
    pulses: [
      { channel: 'a', role: 'command_4', holdMs: 300, cooldownMs: 420, threshold: 1.1 },
      { channel: 'b', role: 'command_5', holdMs: 600, cooldownMs: 200, threshold: 1.05 },
      { channel: 'start', role: 'command_6', holdMs: 55, cooldownMs: 600_000, threshold: 2, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
      { channel: 'select', role: 'command_7', holdMs: 55, cooldownMs: 600_000, threshold: 2, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
    ],
    clearLockoutMs: 300,
  };
}
