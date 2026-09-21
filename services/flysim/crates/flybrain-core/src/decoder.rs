//! Population-rate readout, bit-exact with `readout/decoder.ts`.
//!
//! A decoder turns per-role population rates into a set of active output channels. Nothing about
//! the downstream device enters this module; presets (see [`gameboy`] and [`platformer`]) supply
//! the channel names, roles and timings for a concrete device and game.

pub mod gameboy;
pub mod platformer;

use std::collections::HashMap;

use crate::error::{bail, Result};
use crate::jsmath::{js_max, js_min};
use crate::ordered::NumberMap;

/// At most one channel of the group is active at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct ExclusiveGroup {
    /// Channel name -> rate role. Channel order breaks argmax ties.
    pub channels: Vec<(String, String)>,
    /// Milliseconds between winner re-decisions.
    pub decision_ms: f64,
    /// Milliseconds the winner stays active.
    pub hold_ms: f64,
    /// A challenger must lead the current winner by this factor to take over (1.15 = 15%).
    pub hysteresis: f64,
    /// Fatigue added to the winner at each decision, capped at 1.
    pub fatigue_gain: f64,
    /// Fatigue of every other channel is multiplied by this at each decision.
    pub fatigue_decay: f64,
    /// Fatigue forced onto a channel the environment reports blocked, or 0 for the rule off.
    ///
    /// The blocked-direction cooldown (`docs/readout.md`): a direction that produced no movement
    /// is habituated at once rather than over the several decisions `fatigue_gain` would take, so
    /// a wall costs one hold instead of a minute. Nothing about the environment enters the score:
    /// the caller says *which* channel is blocked and this says how hard that is penalised.
    pub blocked_fatigue: f64,
    /// Milliseconds the environment must observe no movement before it calls the held channel
    /// blocked, or 0 for the rule off.
    ///
    /// The decoder never reads this. The caller owns the clock and the position -- in `flysim` the
    /// sim loop, from the adapter's sampled coordinates -- and this is where the number lives so
    /// that a preset stays one object and `docs/readout.md` has one table to state it in.
    pub blocked_ms: f64,
}

/// The alternate threshold and cooldown used while `decode(.., boot = true)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BootVariant {
    pub cooldown_ms: f64,
    pub threshold: f64,
}

/// An independent, threshold-triggered pulse channel.
#[derive(Debug, Clone, PartialEq)]
pub struct PulseChannel {
    pub channel: String,
    /// Rate role driving this channel.
    pub role: String,
    /// Milliseconds the pulse stays active.
    pub hold_ms: f64,
    /// Milliseconds before the channel may fire again.
    pub cooldown_ms: f64,
    /// The normalized score must exceed this for the channel to fire.
    pub threshold: f64,
    pub boot: Option<BootVariant>,
    /// Channels sharing a throttle group share their cooldown.
    pub throttle_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecoderConfig {
    pub exclusive: Option<ExclusiveGroup>,
    /// A second exclusive group, the macro channels (`docs/design/macros.md` sections 11 and 12).
    ///
    /// Same rules as [`DecoderConfig::exclusive`] and, in the Game Boy preset, the same numbers:
    /// a macro is a button, pressed by its own population, decided by hold, hysteresis and
    /// fatigue like a direction. Two differences, both of them the caller's:
    ///
    /// - only the channels the caller names as *bound* compete at a decision
    ///   ([`PopulationDecoder::decode_bound`]); the scene decides which buttons exist, so an
    ///   unbound macro is masked out of the argmax rather than beaten in it;
    /// - nothing reports a macro channel blocked, so `blocked_fatigue` is carried for symmetry
    ///   and is untouched in practice.
    ///
    /// `None` -- every preset written before section 11, and the platformer -- leaves the decoder
    /// exactly as it was: no channels, no state, no decision.
    pub macros: Option<ExclusiveGroup>,
    pub pulses: Vec<PulseChannel>,
    /// Cooldown applied to every channel by `clear_holds`.
    pub clear_lockout_ms: f64,
}

/// Serialized decoder state (version 4).
#[derive(Debug, Clone, PartialEq)]
pub struct DecoderState {
    pub version: u32,
    pub calibrated: bool,
    /// Rate role -> calibration rate.
    pub baseline: NumberMap,
    /// Channel -> millisecond timestamp the channel stays active until.
    pub held_until: NumberMap,
    /// Channel -> millisecond timestamp the channel may next fire.
    pub next_allowed: NumberMap,
    /// Millisecond timestamp of the next exclusive-group decision.
    pub next_decision: f64,
    /// Current exclusive-group winner, or `None`.
    pub current: Option<String>,
    /// Exclusive channel -> fatigue in [0, 1].
    pub fatigue: NumberMap,
    /// Millisecond timestamp of the next macro-group decision.
    ///
    /// The macro group's three fields are additive and the schema version does not move with
    /// them: a checkpoint written before the group existed carries none of them, and the group
    /// then starts rested with no winner -- the same rule the network's rates follow ("missing
    /// rate roles import as 0"). That is what keeps a live checkpoint loadable across
    /// `docs/design/macros.md` section 11, and a build without the group ignores the fields.
    pub macro_next_decision: f64,
    /// Current macro-group winner, or `None`.
    pub macro_current: Option<String>,
    /// Macro channel -> fatigue in [0, 1].
    pub macro_fatigue: NumberMap,
}

/// The checkpoint schema version this decoder writes and accepts.
pub const DECODER_STATE_VERSION: u32 = 4;

#[derive(Debug, Clone)]
pub struct PopulationDecoder {
    config: DecoderConfig,
    /// Exclusive channels in config order.
    exclusive_channels: Vec<String>,
    /// Macro-group channels in config order.
    macro_channels: Vec<String>,
    /// Every channel: exclusive channels first (config order), then pulses, then the macro group.
    ///
    /// The macro channels go last so that every index, every map order and every `active` list a
    /// consumer already reads is untouched by their arrival.
    channels: Vec<String>,
    roles: HashMap<String, String>,
    throttle_groups: HashMap<String, Vec<String>>,
    baseline: NumberMap,
    /// Roles a restored checkpoint carried no baseline for, calibrated from the rates at the first
    /// [`PopulationDecoder::decode`] after the restore.
    ///
    /// `docs/readout.md`, "Channels added after a checkpoint was written". A checkpoint predates
    /// any channel group added since it was written — the macro group is the case this was found
    /// on — and [`PopulationDecoder::import_state`] assigns the baselines it has, which leaves the
    /// new roles absent. An absent baseline reads as zero, so the score
    /// `(rate + 1) / (baseline + 1)` becomes `rate + 1`: the channel competes on its **raw rate**
    /// against channels normalized to about 1, and a group whose raw rates span 27 to 145 Hz is
    /// decided by which population happens to fire fastest. Live on 2026-09-17, forty-eight
    /// minutes in Oak's lab: `macro_talk` at 41 Hz could not beat `macro_frontier` at 54 or
    /// `macro_objective` at 65, so `TALK` was never chosen and the rung that needed one A press
    /// was never earned.
    ///
    /// Calibrating them from the first decode's rates is what warm-up does
    /// (`NeuralAgent::warmup`: settle, then `calibrate` on one snapshot of the settled rates), so
    /// this is the same rule at the same fidelity — one sample, not a window — applied to the
    /// channels warm-up never saw. Transient: consumed by the next decode and not part of
    /// [`DecoderState`], so no checkpoint schema moves.
    pending_baseline: Vec<String>,

    held_until: NumberMap,
    next_allowed: NumberMap,
    /// The normalized score of every channel as of the last `decode`, before fatigue.
    ///
    /// Written once per `decode`, read by nobody in this module: see
    /// [`PopulationDecoder::last_scores`]. Not in [`DecoderState`], because it is derived from the
    /// rates a decode is given rather than state a decode carries, so the checkpoint schema and
    /// the compatibility string are untouched.
    last_scores: NumberMap,
    calibrated: bool,
    next_decision: f64,
    current: Option<String>,
    fatigue: NumberMap,
    macro_next_decision: f64,
    macro_current: Option<String>,
    macro_fatigue: NumberMap,
}

impl PopulationDecoder {
    pub fn new(config: DecoderConfig) -> Result<Self> {
        let exclusive_channels: Vec<String> = config
            .exclusive
            .as_ref()
            .map(|group| {
                group
                    .channels
                    .iter()
                    .map(|(channel, _)| channel.clone())
                    .collect()
            })
            .unwrap_or_default();
        let macro_channels: Vec<String> = config
            .macros
            .as_ref()
            .map(|group| {
                group
                    .channels
                    .iter()
                    .map(|(channel, _)| channel.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut channels = exclusive_channels.clone();
        channels.extend(config.pulses.iter().map(|pulse| pulse.channel.clone()));
        channels.extend(macro_channels.iter().cloned());
        let unique: std::collections::HashSet<&String> = channels.iter().collect();
        if unique.len() != channels.len() {
            bail!("Duplicate decoder channel");
        }

        let mut roles = HashMap::new();
        if let Some(group) = &config.exclusive {
            for (channel, role) in &group.channels {
                roles.insert(channel.clone(), role.clone());
            }
        }
        for pulse in &config.pulses {
            roles.insert(pulse.channel.clone(), pulse.role.clone());
        }
        if let Some(group) = &config.macros {
            for (channel, role) in &group.channels {
                roles.insert(channel.clone(), role.clone());
            }
        }

        let held_until = NumberMap::from_pairs(channels.iter().map(|channel| (channel, 0.0)));
        let next_allowed = NumberMap::from_pairs(channels.iter().map(|channel| (channel, 0.0)));
        // 1.0 is "at rest": a decoder that has never decoded reports every channel at its
        // calibration rate rather than at zero, which would read as "far below baseline".
        let last_scores = NumberMap::from_pairs(channels.iter().map(|channel| (channel, 1.0)));
        let fatigue =
            NumberMap::from_pairs(exclusive_channels.iter().map(|channel| (channel, 0.0)));
        let macro_fatigue =
            NumberMap::from_pairs(macro_channels.iter().map(|channel| (channel, 0.0)));

        let mut throttle_groups: HashMap<String, Vec<String>> = HashMap::new();
        for pulse in &config.pulses {
            if let Some(group) = &pulse.throttle_group {
                throttle_groups
                    .entry(group.clone())
                    .or_default()
                    .push(pulse.channel.clone());
            }
        }

        Ok(Self {
            config,
            exclusive_channels,
            macro_channels,
            channels,
            roles,
            throttle_groups,
            baseline: NumberMap::new(),
            pending_baseline: Vec::new(),
            held_until,
            next_allowed,
            last_scores,
            calibrated: false,
            next_decision: 0.0,
            current: None,
            fatigue,
            macro_next_decision: 0.0,
            macro_current: None,
            macro_fatigue,
        })
    }

    /// Channel names this decoder can report, in decode order.
    pub fn channel_names(&self) -> &[String] {
        &self.channels
    }

    pub fn calibrated(&self) -> bool {
        self.calibrated
    }

    /// How long the caller must see no movement before it reports a channel blocked, or 0 for the
    /// rule off. The decoder never uses it itself: see [`ExclusiveGroup::blocked_ms`].
    pub fn blocked_ms(&self) -> f64 {
        self.config.exclusive.as_ref().map_or(0.0, |group| group.blocked_ms)
    }

    /// The exclusive group's current winner, or `None` before the first decision.
    ///
    /// Read-only. `flysim`'s sim loop needs it to name the channel it is about to report blocked,
    /// which is the one it is holding.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The macro group's channels in config order, or an empty slice with no group.
    pub fn macro_channel_names(&self) -> &[String] {
        &self.macro_channels
    }

    /// The macro group's current winner, or `None` before its first decision.
    ///
    /// This is the macro the game layer starts (`docs/design/macros.md` section 12: "whichever
    /// bound channel wins, starts"). It is also `active`'s own answer while the winner is held,
    /// which is what the layer actually reads; this accessor is for a log line and a test.
    pub fn macro_current(&self) -> Option<&str> {
        self.macro_current.as_deref()
    }

    /// The normalized score of every channel as the last [`PopulationDecoder::decode`] computed
    /// it: `(rate[role] + 1) / (baseline[role] + 1)`, one entry per exclusive channel and one per
    /// pulse channel, in `channel_names` order.
    ///
    /// Read-only, and deliberately *before* the exclusive group's fatigue division: this is the
    /// score `docs/readout.md` defines, where 1.0 is the calibration rate and above 1.0 is above
    /// it, not the habituated number one decision happens to argmax over. Nothing in this module
    /// reads it back, so it cannot enter a decision: the winner is still the argmax of the same
    /// scores over the same rates, and `flybrain-core/tests/decoder.rs` pins that a run that
    /// reads it decodes identically to one that does not.
    ///
    /// Who wants it: the game layer in palette and plan modes
    /// (`docs/design/macros.md` section 10, "the game layer reads its per-channel normalized
    /// scores"). A decoder that has not decoded yet — uncalibrated, or freshly constructed —
    /// reports every channel at 1.0, which is "at rest".
    pub fn last_scores(&self) -> &NumberMap {
        &self.last_scores
    }

    /// Record the resting rates every score is normalized against.
    pub fn calibrate(&mut self, rates: &NumberMap) {
        for channel in &self.channels {
            let role = &self.roles[channel];
            self.baseline.set(role, rates.get_or_zero(role));
        }
        self.pending_baseline.clear();
        self.calibrated = true;
    }

    /// Roles still waiting for a baseline after a restore, in channel order. For `/status`.
    pub fn pending_baseline_roles(&self) -> &[String] {
        &self.pending_baseline
    }

    /// The rate role each channel reads, in channel order. For `/status`.
    pub fn channel_roles(&self) -> Vec<(&str, &str)> {
        self.channels
            .iter()
            .map(|channel| (channel.as_str(), self.roles[channel].as_str()))
            .collect()
    }

    /// The resting rate each role is normalized against, as [`PopulationDecoder::calibrate`]
    /// recorded it. For `/status`.
    pub fn baselines(&self) -> &NumberMap {
        &self.baseline
    }

    /// Drop every hold, forget the exclusive winner and lock all channels out for
    /// `clear_lockout_ms`.
    pub fn clear_holds(&mut self, now_ms: f64) {
        for channel in &self.channels {
            self.held_until.set(channel, 0.0);
            self.next_allowed
                .set(channel, now_ms + self.config.clear_lockout_ms);
        }
        self.current = None;
        let names: Vec<String> = self.fatigue.keys().cloned().collect();
        for channel in names {
            self.fatigue.set(&channel, 0.0);
        }
        self.next_decision = now_ms;
        self.macro_current = None;
        let names: Vec<String> = self.macro_fatigue.keys().cloned().collect();
        for channel in names {
            self.macro_fatigue.set(&channel, 0.0);
        }
        self.macro_next_decision = now_ms;
    }

    /// Advance the readout to `now_ms` and return the active channel names: exclusive channels
    /// first (config order), then pulses (config order).
    pub fn decode(&mut self, rates: &NumberMap, now_ms: f64, boot: bool) -> Vec<String> {
        self.decode_blocked(rates, now_ms, boot, None)
    }

    /// [`PopulationDecoder::decode`] plus the blocked-direction cooldown.
    ///
    /// `blocked` names an exclusive channel the environment has observed producing no movement.
    /// Its only effect is to raise that channel's fatigue to [`ExclusiveGroup::blocked_fatigue`]
    /// straight away, so the next decision sees a habituated incumbent rather than an untired one.
    /// `max` rather than `+=` because the caller reports the same blockage on every frame of a
    /// hold: one wall is one penalty however many times it is observed.
    ///
    /// This is the only input to the readout that is not a rate, and it is deliberately the
    /// narrowest one that fixes a wall bump: it says "that button did nothing", not where the fly
    /// is, where it should go, or what the room looks like. `blocked_fatigue = 0` -- every preset
    /// written before the rule existed -- ignores it entirely, and so does a `blocked` naming a
    /// channel outside the group. It needs no new checkpoint field, because fatigue is already in
    /// [`DecoderState`].
    pub fn decode_blocked(
        &mut self,
        rates: &NumberMap,
        now_ms: f64,
        boot: bool,
        blocked: Option<&str>,
    ) -> Vec<String> {
        self.decode_bound(rates, now_ms, boot, blocked, None)
    }

    /// [`PopulationDecoder::decode_blocked`] plus the macro group's bound-channel mask.
    ///
    /// `bound` is the set of macro channels that may win this decision: the scene's own buttons
    /// (`docs/design/macros.md` section 12, "the scene decides which buttons exist ... unbound
    /// channels are masked from the decision"). `None` -- and every caller that has no macro
    /// group -- lets every channel of the group compete, which is what the direction group always
    /// does; `Some(&[])` is a scene with no macro at all and takes no decision.
    ///
    /// Masking rather than penalising, and that is the one place where it differs from the
    /// blocked-direction cooldown it is modelled on: a blocked channel is habituated and can
    /// still win, while an unbound channel is not on the pad and must not be pressable at any
    /// score. Its fatigue relaxes meanwhile, exactly as a channel that lost a decision does, so a
    /// button coming back onto the pad is neither penalised nor freshened for having been off it.
    /// A masked channel cannot stay `current` either: the winner is always one of `bound`.
    pub fn decode_bound(
        &mut self,
        rates: &NumberMap,
        now_ms: f64,
        boot: bool,
        blocked: Option<&str>,
        bound: Option<&[String]>,
    ) -> Vec<String> {
        if !self.calibrated {
            return Vec::new();
        }
        let blocked_fatigue =
            self.config.exclusive.as_ref().map_or(0.0, |group| group.blocked_fatigue);
        if let Some(channel) = blocked.filter(|channel| {
            blocked_fatigue > 0.0 && self.fatigue.get(channel).is_some()
        }) {
            // `js_max`/`js_min`, not the inherent ones: `Math.max` propagates NaN and Rust's
            // `f64::max` swallows it, and this module is bit-exact with `readout/decoder.ts`.
            let raised =
                js_min(1.0, js_max(blocked_fatigue, self.fatigue.get_or_zero(channel)));
            self.fatigue.set(channel, raised);
        }
        // The same rule for the macro group, for the caller that one day reports a macro that
        // moved nothing. Nothing in the tree does today, so this is symmetry rather than a live
        // path, and it is here rather than absent because the group's contract says "the same
        // hold, hysteresis, fatigue and blocked rules as the direction group".
        let macro_blocked_fatigue =
            self.config.macros.as_ref().map_or(0.0, |group| group.blocked_fatigue);
        if let Some(channel) = blocked.filter(|channel| {
            macro_blocked_fatigue > 0.0 && self.macro_fatigue.get(channel).is_some()
        }) {
            let raised = js_min(
                1.0,
                js_max(macro_blocked_fatigue, self.macro_fatigue.get_or_zero(channel)),
            );
            self.macro_fatigue.set(channel, raised);
        }
        let mut scores = NumberMap::new();
        // A role the restored checkpoint had no baseline for is calibrated here, from this decode's
        // rates, before any score is computed: see `pending_baseline`. Every such channel therefore
        // scores exactly 1.0 on this decision -- at rest, which is what a channel nobody has
        // measured honestly is -- instead of competing on its raw rate.
        if !self.pending_baseline.is_empty() {
            for role in std::mem::take(&mut self.pending_baseline) {
                self.baseline.set(&role, rates.get_or_zero(&role));
            }
        }
        for channel in &self.channels {
            let role = &self.roles[channel];
            scores.set(
                channel,
                (rates.get_or_zero(role) + 1.0) / (self.baseline.get_or_zero(role) + 1.0),
            );
        }
        // The one write of the read-only accessor, here rather than at the end of the decode so
        // that it is the score of `docs/readout.md` and not the fatigue-divided copy the exclusive
        // group's argmax works on. `assign` over the same key set, so the map keeps its order and
        // allocates nothing.
        self.last_scores.assign(&scores);

        if let Some(group) = self.config.exclusive.clone() {
            decide_exclusive(
                &group,
                &self.exclusive_channels,
                None,
                &mut scores,
                &mut self.fatigue,
                &mut self.current,
                &mut self.next_decision,
                &mut self.held_until,
                now_ms,
            );
        }
        // The macro group, after the buttons and on its own clock, with only the scene's bound
        // channels competing (`docs/design/macros.md` section 12).
        if let Some(group) = self.config.macros.clone() {
            decide_exclusive(
                &group,
                &self.macro_channels,
                bound,
                &mut scores,
                &mut self.macro_fatigue,
                &mut self.macro_current,
                &mut self.macro_next_decision,
                &mut self.held_until,
                now_ms,
            );
        }

        for pulse in &self.config.pulses {
            let variant = match (boot, pulse.boot) {
                (true, Some(boot)) => boot,
                _ => BootVariant {
                    cooldown_ms: pulse.cooldown_ms,
                    threshold: pulse.threshold,
                },
            };
            if scores.get_or_zero(&pulse.channel) > variant.threshold
                && now_ms >= self.next_allowed.get_or_zero(&pulse.channel)
            {
                self.held_until.set(&pulse.channel, now_ms + pulse.hold_ms);
                let next_allowed = now_ms + variant.cooldown_ms;
                self.next_allowed.set(&pulse.channel, next_allowed);
                if let Some(name) = &pulse.throttle_group {
                    if let Some(group) = self.throttle_groups.get(name) {
                        for channel in group.clone() {
                            self.next_allowed.set(&channel, next_allowed);
                        }
                    }
                }
            }
        }

        self.channels
            .iter()
            .filter(|channel| now_ms < self.held_until.get_or_zero(channel))
            .cloned()
            .collect()
    }

    pub fn export_state(&self) -> DecoderState {
        DecoderState {
            version: DECODER_STATE_VERSION,
            calibrated: self.calibrated,
            baseline: self.baseline.clone(),
            held_until: self.held_until.clone(),
            next_allowed: self.next_allowed.clone(),
            next_decision: self.next_decision,
            current: self.current.clone(),
            fatigue: self.fatigue.clone(),
            macro_next_decision: self.macro_next_decision,
            macro_current: self.macro_current.clone(),
            macro_fatigue: self.macro_fatigue.clone(),
        }
    }

    /// Load a version 4 checkpoint. Every field is validated before anything is written, so a
    /// rejected checkpoint leaves the decoder untouched.
    ///
    /// The prototype `MotorDecoder` checkpoints (versions `undefined`, 2 and 3) that
    /// `readout/decoder.ts` also accepts are deliberately out of scope for this port: nothing on
    /// the Rust side has ever written one.
    pub fn import_state(&mut self, state: &DecoderState) -> Result<()> {
        if state.version != DECODER_STATE_VERSION {
            bail!("Invalid decoder version");
        }
        let records = [&state.baseline, &state.held_until, &state.next_allowed];
        if !state.next_decision.is_finite()
            || records
                .iter()
                .any(|record| record.values().any(|value| !value.is_finite()))
            // The macro channels are exempt from "every channel must be in the checkpoint":
            // a checkpoint written before the group existed carries none of them and they start
            // at zero, which is the whole reason `docs/design/macros.md` section 11 could add
            // them without moving the schema version. A macro entry that *is* present must still
            // be finite, which the `records` sweep above covers.
            || self.required_channels().any(|channel| {
                !state
                    .held_until
                    .get(channel)
                    .is_some_and(|value| value.is_finite())
                    || !state
                        .next_allowed
                        .get(channel)
                        .is_some_and(|value| value.is_finite())
            })
        {
            bail!("Invalid decoder checkpoint");
        }
        if self.fatigue.keys().any(|channel| {
            !state
                .fatigue
                .get(channel)
                .is_some_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        }) {
            bail!("Invalid decoder fatigue");
        }
        // The macro group's fatigue is optional for the same reason, so an absent entry is not a
        // fault and a present one is held to the same bounds.
        if self.macro_fatigue.keys().any(|channel| {
            state
                .macro_fatigue
                .get(channel)
                .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        }) || !state.macro_next_decision.is_finite()
        {
            bail!("Invalid decoder fatigue");
        }
        if let Some(current) = &state.current {
            if !self.exclusive_channels.contains(current) {
                bail!("Invalid decoder channel");
            }
        }
        if let Some(current) = &state.macro_current {
            if !self.macro_channels.contains(current) {
                bail!("Invalid decoder channel");
            }
        }
        // Which roles this checkpoint does not know about, in channel order and deduplicated: the
        // first decode after the restore calibrates them. Read off the record that came *in*
        // rather than off `self.baseline`, so a decoder reused across two restores cannot keep a
        // baseline the new checkpoint never had.
        self.pending_baseline.clear();
        for channel in &self.channels {
            let role = &self.roles[channel];
            if !state.baseline.contains(role) && !self.pending_baseline.iter().any(|held| held == role)
            {
                self.pending_baseline.push(role.clone());
            }
        }
        self.baseline.assign(&state.baseline);
        self.held_until.assign(&state.held_until);
        self.next_allowed.assign(&state.next_allowed);
        self.calibrated = state.calibrated;
        self.next_decision = state.next_decision;
        self.fatigue = state.fatigue.clone();
        self.current = state.current.clone();
        // The macro group, channel by channel rather than by wholesale assignment, so that a
        // checkpoint carrying none of them leaves every macro channel rested at zero.
        self.macro_next_decision = state.macro_next_decision;
        self.macro_current = state.macro_current.clone();
        for channel in self.macro_channels.clone() {
            let value = state.macro_fatigue.get(&channel).unwrap_or(0.0);
            self.macro_fatigue.set(&channel, value);
        }
        Ok(())
    }

    /// Channels a version 4 checkpoint must carry: the direction group and the pulses.
    fn required_channels(&self) -> impl Iterator<Item = &String> {
        self.channels
            .iter()
            .filter(|channel| !self.macro_channels.contains(channel))
    }
}

/// One exclusive group's decision, shared by the direction group and the macro group.
///
/// Lifted verbatim out of [`PopulationDecoder::decode_bound`] when the macro group arrived, so
/// the two groups cannot drift: the same fatigue division, the same tie rule, the same hysteresis,
/// the same hold. `competing` is the only thing that differs between them -- `None` for the
/// direction group, where every channel always competes, and the bound set for the macro group --
/// and with `None` this is statement for statement the decode that shipped in v0.1.x.
///
/// A free function, and every piece of state it touches is a parameter, because the alternative is
/// a method that borrows `self` mutably while reading `self.config`.
#[allow(clippy::too_many_arguments)]
fn decide_exclusive(
    group: &ExclusiveGroup,
    channels: &[String],
    competing: Option<&[String]>,
    scores: &mut NumberMap,
    fatigue: &mut NumberMap,
    current: &mut Option<String>,
    next_decision: &mut f64,
    held_until: &mut NumberMap,
    now_ms: f64,
) {
    if channels.is_empty() || now_ms < *next_decision {
        return;
    }
    // Bounded habituation stops a small persistent rate bias from holding one channel forever.
    for channel in channels {
        let score = scores.get_or_zero(channel);
        scores.set(channel, score / (1.0 + fatigue.get_or_zero(channel)));
    }
    // The field of candidates: every channel of the group, or only the ones the caller named.
    let candidates: Vec<&String> = match competing {
        None => channels.iter().collect(),
        Some(bound) => channels
            .iter()
            .filter(|channel| bound.iter().any(|name| name == *channel))
            .collect(),
    };
    // A scene that binds nothing takes no decision at all: no winner, no hold, no fatigue, and
    // the clock does not advance, so the frame a button appears on is a frame that can press it.
    let Some((first, rest)) = candidates.split_first() else {
        return;
    };
    // Ties keep the earlier channel: only a strictly greater score displaces the running best.
    let mut best = (*first).clone();
    for channel in rest {
        if scores.get_or_zero(channel) > scores.get_or_zero(&best) {
            best = (*channel).clone();
        }
    }
    if let Some(held) = current.as_ref() {
        // The incumbent's commitment bonus, but only while it is still on the pad: a channel the
        // scene has taken away cannot hold its own seat.
        if candidates.contains(&held)
            && scores.get_or_zero(&best) < scores.get_or_zero(held) * group.hysteresis
        {
            best = held.clone();
        }
    }
    for channel in channels {
        held_until.set(channel, 0.0);
    }
    *current = Some(best.clone());
    for channel in channels {
        let value = fatigue.get_or_zero(channel);
        let next = if *channel == best {
            js_min(1.0, value + group.fatigue_gain)
        } else {
            value * group.fatigue_decay
        };
        fatigue.set(channel, next);
    }
    held_until.set(&best, now_ms + group.hold_ms);
    *next_decision = now_ms + group.decision_ms;
}
