//! Game Boy platformer readout preset, ported from `readout/presets/platformer.ts`.
//!
//! Same eight channels, bits and rate roles as [`super::gameboy`]; only the
//! `DecoderConfig` numbers differ. Every one of them is from
//! `docs/design/platformer.md` §4, which argues each change against the Pokémon
//! preset from the Super Mario Land disassembly:
//!
//! - `holdMs == decisionMs` (250/250, against 400/400 with an implicit gap),
//!   because a gap in the right-hold decays the momentum byte and momentum is
//!   what clears gaps.
//! - All four directions stay: `down` enters pipes and crouches, `up` is needed
//!   in the two vehicle levels.
//! - `hysteresis` 1.25, because a reversal resets momentum, so a challenger
//!   should need a 25% lead.
//! - `fatigueGain` 0.04 and `fatigueDecay` 0.85: weaker habituation, because the
//!   game's own timer breaks a stuck fly.
//! - `a` is a 300 ms hold on a 420 ms cooldown: jump height is variable, so an
//!   85 ms tap can never clear a two-block gap.
//! - `b` is a 600 ms hold on a 200 ms cooldown. Because the cooldown is shorter
//!   than the hold, a channel whose score stays up refires the instant the hold
//!   expires, which is a gapless sustained hold with no decoder change.
//! - `start` and `select` are boot-only: 10-minute cooldowns at threshold 2.0
//!   during play, permissive on a title screen. Pausing is dead air.
//!
//! Keeping both system channels in one throttle group is a safety property, not
//! only a throttle: Super Mario Land soft resets when A, B, Select and Start are
//! held in the same frame (`and a, $0F / cp a, $0F / jp Init`, bank0.asm:1075),
//! and a throttle-group fire writes `nextAllowed` for every member, so Start and
//! Select can never be held together. `tests/decoder.rs` asserts it.
//!
//! One disclosed prior: `right` is first in `channels`, and insertion order
//! breaks argmax ties (`docs/readout.md`, "Ties keep the earlier channel"), so a
//! tie favours rightward travel. Accepted and disclosed rather than hidden
//! (§4, "Doctrine check").

use super::{BootVariant, DecoderConfig, ExclusiveGroup, PulseChannel};

/// Decoder configuration for the platformer demo.
pub fn platformer_decoder_config() -> DecoderConfig {
    let hold = |channel: &str, role: &str, hold_ms: f64, cooldown_ms: f64, threshold: f64| {
        PulseChannel {
            channel: channel.to_string(),
            role: role.to_string(),
            hold_ms,
            cooldown_ms,
            threshold,
            boot: None,
            throttle_group: None,
        }
    };
    let system = |channel: &str, role: &str| PulseChannel {
        channel: channel.to_string(),
        role: role.to_string(),
        hold_ms: 55.0,
        cooldown_ms: 600_000.0,
        threshold: 2.0,
        boot: Some(BootVariant {
            cooldown_ms: 2500.0,
            threshold: 1.0,
        }),
        throttle_group: Some("system".to_string()),
    };
    DecoderConfig {
        exclusive: Some(ExclusiveGroup {
            channels: vec![
                ("right".to_string(), "command_3".to_string()),
                ("left".to_string(), "command_2".to_string()),
                ("down".to_string(), "command_1".to_string()),
                ("up".to_string(), "command_0".to_string()),
            ],
            decision_ms: 250.0,
            hold_ms: 250.0,
            hysteresis: 1.25,
            fatigue_gain: 0.04,
            fatigue_decay: 0.85,
            // No blocked-direction cooldown: a platformer that is not moving is usually falling,
            // standing on a ledge or being carried, not bumping a wall, and `docs/design/
            // platformer.md` §4 chose its own commitment. The rule is off until measured there.
            blocked_fatigue: 0.0,
            blocked_ms: 0.0,
        }),
        // No macro group: the platformer has no scene palette
        // (`docs/design/macros.md` section 12 is Pokémon Red's).
        macros: None,
        pulses: vec![
            hold("a", "command_4", 300.0, 420.0, 1.10),
            hold("b", "command_5", 600.0, 200.0, 1.05),
            system("start", "command_6"),
            system("select", "command_7"),
        ],
        clear_lockout_ms: 300.0,
    }
}
