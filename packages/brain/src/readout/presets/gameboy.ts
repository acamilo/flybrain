/**
 * Game Boy eight-button readout preset.
 *
 * The only device-specific module in the library: it names the eight channels, maps them onto the
 * `command_0..7` rate roles and fixes the timings. The D-pad is an exclusive group (one direction at
 * a time, an 800 ms commitment with hysteresis and habituation); A and B are fast pulses; Start and
 * Select are rare pulses sharing one throttle, with a permissive boot variant so title screens and
 * new-game menus still work.
 *
 * The direction hold, decision period, fatigue gain, hysteresis and blocked-direction cooldown are
 * the ones measured in `infra/docs/room-escape.md`; everything else is the prototype's fixed motor
 * decoder to the digit.
 */
import type { DecoderConfig } from '../decoder';

export const GAMEBOY_BUTTONS = ['up', 'down', 'left', 'right', 'a', 'b', 'start', 'select'] as const;

export type GameboyButton = (typeof GAMEBOY_BUTTONS)[number];

/** Bit position of each button in the standard joypad mask. */
export const GAMEBOY_BUTTON_BITS: Record<GameboyButton, number> = {
  up: 1 << 0, down: 1 << 1, left: 1 << 2, right: 1 << 3, a: 1 << 4, b: 1 << 5, start: 1 << 6, select: 1 << 7,
};

/**
 * Fatigue forced onto a direction the caller has reported blocked, and how long the position must
 * stand still before it does.
 *
 * The blocked-direction cooldown, chosen by the v0.1.1 measurement in `infra/docs/room-escape.md`.
 * Named constants because two places quote them: this preset and `docs/readout.md`.
 */
export const BLOCKED_FATIGUE = 0.35;
export const BLOCKED_MS = 800;

/**
 * The Game Boy eight-button decoder configuration.
 *
 * `decisionMs` and `holdMs` are equal at 800 ms, `hysteresis` is 1.05, `fatigueGain` is 0.08 and
 * the blocked-direction cooldown is on at 0.35 after one hold. All four come from the v0.1.1
 * measurement in `infra/docs/room-escape.md`, which ran the live release fly forward from its own
 * checkpoint; `docs/design/room-escape.md` sections 1 and 3 hold the argument.
 *
 * The short version, because the numbers only make sense together. One overworld step in Pokemon
 * Red is about 270 ms, so an 800 ms hold is a three-tile commitment: about half a room, which is
 * what section 1 raised it for and it was right. What section 1 got wrong was halving `fatigueGain`
 * at the same time, which doubled the number of *consecutive* holds one direction wins and so
 * committed the fly to twelve tiles in an eight-tile room; 62 to 64% of its holds ended up moving
 * it nowhere. The hysteresis was the harder problem: on a settled network the four direction scores
 * sit inside a 9% spread, so a 15% lead is unreachable and the winner could only ever be changed by
 * fatigue -- the fly orbited the walls, and Red's staircase is on that orbit while the front door is
 * not. 1.05 is a commitment bonus the spread can actually overcome. The blocked-direction cooldown
 * then stops a crossing being wasted on a wall, and it is the interaction that matters: at 1.15 it
 * made things worse (7 of 15 runs left the house against 11 without it) and at 1.05 it is decisive
 * (14 of 15, and 13 of those inside five minutes, against 13 and 6 without it).
 *
 * Every other value -- the fatigue decay, A, B, Start, Select and the clear lockout -- is the
 * prototype's fixed motor decoder unchanged. Readout state is not part of the neural checkpoint, so
 * none of this touches the decoder version or the compatibility string: an existing checkpoint loads
 * and simply decodes on the new numbers.
 */
export function gameboyDecoderConfig(macroRoles: readonly string[] = []): DecoderConfig {
  return {
    ...(macroRoles.length === 0 ? {} : { macros: macroGroup(macroRoles) }),
    exclusive: {
      channels: { up: 'command_0', down: 'command_1', left: 'command_2', right: 'command_3' },
      decisionMs: 800,
      holdMs: 800,
      hysteresis: 1.05,
      fatigueGain: 0.08,
      fatigueDecay: 0.8,
      blockedFatigue: BLOCKED_FATIGUE,
      blockedMs: BLOCKED_MS,
    },
    pulses: [
      { channel: 'a', role: 'command_4', holdMs: 85, cooldownMs: 480, threshold: 1 },
      { channel: 'b', role: 'command_5', holdMs: 85, cooldownMs: 480, threshold: 1 },
      { channel: 'start', role: 'command_6', holdMs: 55, cooldownMs: 30000, threshold: 1.35, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
      { channel: 'select', role: 'command_7', holdMs: 55, cooldownMs: 30000, threshold: 1.35, boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
    ],
    clearLockoutMs: 480,
  };
}

/**
 * The macro group over `roles` (`docs/design/macros.md` sections 11 and 12).
 *
 * Each entry is a `macro_<type>` rate role and is its own channel name: the population *is* the
 * button, so a second name for it would be a second thing to keep in step. The numbers are the
 * direction group's to the digit, because section 12 says a macro is a button and the measurement
 * that chose them was a measurement of how long a commitment should last, not of what it was to.
 *
 * Which channels may win is the caller's, per decision, not the preset's: the scene decides which
 * buttons exist. No roles is no group at all, which is raw mode and every non-Pokemon adapter.
 */
function macroGroup(roles: readonly string[]): DecoderConfig['macros'] {
  return {
    channels: Object.fromEntries(roles.map(role => [role, role])),
    decisionMs: 800,
    holdMs: 800,
    hysteresis: 1.05,
    fatigueGain: 0.08,
    fatigueDecay: 0.8,
    blockedFatigue: BLOCKED_FATIGUE,
    blockedMs: BLOCKED_MS,
  };
}

/** Pack active channel names into a joypad bit mask. Unknown names contribute nothing. */
export function toButtonMask(active: string[], bits: Record<string, number> = GAMEBOY_BUTTON_BITS): number {
  let mask = 0;
  for (const channel of active) mask |= bits[channel] ?? 0;
  return mask;
}

/** Unpack a joypad bit mask into channel names, in `bits` key order. */
export function fromButtonMask(mask: number, bits: Record<string, number> = GAMEBOY_BUTTON_BITS): string[] {
  const active: string[] = [];
  for (const [channel, bit] of Object.entries(bits)) if (mask & bit) active.push(channel);
  return active;
}
