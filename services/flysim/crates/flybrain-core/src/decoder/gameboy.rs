//! Game Boy eight-button readout preset, ported from `readout/presets/gameboy.ts`.
//!
//! The only device-specific module in the library: it names the eight channels, maps them onto the
//! `command_0..7` rate roles and fixes the timings.
//!
//! The direction hold, decision period, fatigue gain, hysteresis and blocked-direction cooldown
//! are the ones measured in `infra/docs/room-escape.md`; everything else is the prototype's fixed
//! motor decoder to the digit. `flybrain-core/tests/decoder.rs` pins the numbers and
//! `packages/brain/tests/readout.test.ts` pins the same numbers on the TypeScript twin.

use super::{BootVariant, DecoderConfig, ExclusiveGroup, PulseChannel};

pub const GAMEBOY_BUTTONS: [&str; 8] = ["up", "down", "left", "right", "a", "b", "start", "select"];

/// Bit position of each button in the standard joypad mask, in `GAMEBOY_BUTTONS` order.
pub const GAMEBOY_BUTTON_BITS: [(&str, u32); 8] = [
    ("up", 1 << 0),
    ("down", 1 << 1),
    ("left", 1 << 2),
    ("right", 1 << 3),
    ("a", 1 << 4),
    ("b", 1 << 5),
    ("start", 1 << 6),
    ("select", 1 << 7),
];

/// Fatigue forced onto a direction the sim loop has reported blocked, and how long the position
/// must stand still before it does.
///
/// The blocked-direction cooldown, chosen by the v0.1.1 measurement in
/// `infra/docs/room-escape.md`. Named constants because two places quote them: this preset and
/// `docs/readout.md`.
pub const BLOCKED_FATIGUE: f64 = 0.35;
pub const BLOCKED_MS: f64 = 800.0;

/// The Game Boy eight-button decoder configuration.
///
/// `decision_ms` and `hold_ms` are equal at 800 ms, `hysteresis` is 1.05, `fatigue_gain` is 0.08
/// and the blocked-direction cooldown is on at 0.35 after one hold. All four come from the v0.1.1
/// measurement in `infra/docs/room-escape.md`, which ran the live release fly forward from its own
/// checkpoint; `docs/design/room-escape.md` sections 1 and 3 hold the argument.
///
/// The short version, because the numbers only make sense together. One overworld step in Pokémon
/// Red is about 270 ms, so an 800 ms hold is a three-tile commitment: about half a room, which is
/// what section 1 raised it for and it was right. What section 1 got wrong was halving
/// `fatigue_gain` at the same time, which doubled the number of *consecutive* holds one direction
/// wins and so committed the fly to twelve tiles in an eight-tile room; 62 to 64% of its holds
/// ended up moving it nowhere. The hysteresis was the harder problem: on a settled network the four
/// direction scores sit inside a 9% spread, so a 15% lead is unreachable and the winner could only
/// ever be changed by fatigue -- the fly orbited the walls, and Red's staircase is on that orbit
/// while the front door is not. 1.05 is a commitment bonus the spread can actually overcome. The
/// blocked-direction cooldown then stops a crossing being wasted on a wall, and it is the
/// interaction that matters: at 1.15 it made things worse (7 of 15 runs left the house against 11
/// without it) and at 1.05 it is decisive (14 of 15, and 13 of those inside five minutes, against
/// 13 and 6 without it).
///
/// Every other value -- the fatigue decay, A, B, Start, Select and the clear lockout -- is the
/// prototype's fixed motor decoder unchanged. Readout state is not part of the neural checkpoint,
/// so none of this touches the decoder version or the compatibility string: an existing checkpoint
/// loads and simply decodes on the new numbers.
pub fn gameboy_decoder_config() -> DecoderConfig {
    gameboy_decoder_config_with_macros(&[])
}

/// The Game Boy preset with a macro group over `roles` (`docs/design/macros.md` sections 11, 12).
///
/// Each entry is a `macro_<type>` rate role and is its own channel name: the population *is* the
/// button, so a second name for it would be a second thing to keep in step. The group's numbers
/// are the direction group's to the digit — 800 ms hold, 800 ms decision, 1.05 hysteresis, 0.08
/// fatigue gain, 0.8 decay, the blocked cooldown at 0.35 after 800 ms — because section 12 says a
/// macro is a button and the measurement that chose those numbers
/// (`infra/docs/room-escape.md`) was a measurement of how long a commitment should last, not of
/// what the commitment was to.
///
/// Which of the channels may win is the caller's, per decision, not the preset's
/// ([`super::PopulationDecoder::decode_bound`]): the scene decides which buttons exist. An empty
/// slice is no group at all, which is raw mode and every non-Pokémon adapter.
pub fn gameboy_decoder_config_with_macros(roles: &[&str]) -> DecoderConfig {
    let pulse =
        |channel: &str, role: &str, hold_ms: f64, cooldown_ms: f64, threshold: f64| PulseChannel {
            channel: channel.to_string(),
            role: role.to_string(),
            hold_ms,
            cooldown_ms,
            threshold,
            boot: None,
            throttle_group: None,
        };
    let system = |channel: &str, role: &str| PulseChannel {
        channel: channel.to_string(),
        role: role.to_string(),
        hold_ms: 55.0,
        cooldown_ms: 30000.0,
        threshold: 1.35,
        boot: Some(BootVariant {
            cooldown_ms: 2500.0,
            threshold: 1.0,
        }),
        throttle_group: Some("system".to_string()),
    };
    let macros = (!roles.is_empty()).then(|| ExclusiveGroup {
        channels: roles
            .iter()
            .map(|role| ((*role).to_string(), (*role).to_string()))
            .collect(),
        decision_ms: 800.0,
        hold_ms: 800.0,
        hysteresis: 1.05,
        fatigue_gain: 0.08,
        fatigue_decay: 0.8,
        blocked_fatigue: BLOCKED_FATIGUE,
        blocked_ms: BLOCKED_MS,
    });
    DecoderConfig {
        exclusive: Some(ExclusiveGroup {
            channels: vec![
                ("up".to_string(), "command_0".to_string()),
                ("down".to_string(), "command_1".to_string()),
                ("left".to_string(), "command_2".to_string()),
                ("right".to_string(), "command_3".to_string()),
            ],
            decision_ms: 800.0,
            hold_ms: 800.0,
            hysteresis: 1.05,
            fatigue_gain: 0.08,
            fatigue_decay: 0.8,
            blocked_fatigue: BLOCKED_FATIGUE,
            blocked_ms: BLOCKED_MS,
        }),
        macros,
        pulses: vec![
            pulse("a", "command_4", 85.0, 480.0, 1.0),
            pulse("b", "command_5", 85.0, 480.0, 1.0),
            system("start", "command_6"),
            system("select", "command_7"),
        ],
        clear_lockout_ms: 480.0,
    }
}

/// Pack active channel names into a joypad bit mask. Unknown names contribute nothing.
pub fn to_button_mask(active: &[String]) -> u32 {
    let mut mask = 0;
    for channel in active {
        if let Some((_, bit)) = GAMEBOY_BUTTON_BITS.iter().find(|(name, _)| name == channel) {
            mask |= bit;
        }
    }
    mask
}

/// Unpack a joypad bit mask into channel names, in `GAMEBOY_BUTTON_BITS` key order.
pub fn from_button_mask(mask: u32) -> Vec<String> {
    GAMEBOY_BUTTON_BITS
        .iter()
        .filter(|(_, bit)| mask & bit != 0)
        .map(|(name, _)| (*name).to_string())
        .collect()
}
