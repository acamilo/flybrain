/**
 * Population-rate readout.
 *
 * A decoder turns per-role population rates into a set of active output channels. Nothing about
 * the downstream device enters this module: channels are named by the caller, and the only input
 * is a `role -> rate` record produced by the network. Presets (see `readout/presets/`) supply the
 * channel names, roles and timings for a concrete device.
 *
 * Two channel kinds are supported:
 *
 * - One optional *exclusive group*: at most one channel of the group is active at a time, re-decided
 *   every `decisionMs` with hysteresis (commitment) and bounded fatigue (habituation), so a small
 *   persistent rate bias cannot hold one channel forever.
 * - Any number of *pulse channels*: independent, threshold-triggered, fixed-width pulses with a
 *   per-channel cooldown, optionally shared across a `throttleGroup`.
 *
 * Every decision uses the normalized score `(rate + 1) / (baseline + 1)`, where the baseline comes
 * from `calibrate()`. This readout carries no learning; plasticity belongs to the network.
 */

/** At most one channel of the group is active at a time. */
export interface ExclusiveGroup {
  /** Channel name -> rate role. Channel order (insertion order) breaks argmax ties. */
  channels: Record<string, string>;
  /** Milliseconds between winner re-decisions. */
  decisionMs: number;
  /** Milliseconds the winner stays active. */
  holdMs: number;
  /** A challenger must lead the current winner by this factor to take over (1.15 = 15%). */
  hysteresis: number;
  /** Fatigue added to the winner at each decision, capped at 1. */
  fatigueGain: number;
  /** Fatigue of every other channel is multiplied by this at each decision. */
  fatigueDecay: number;
  /**
   * Fatigue forced onto a channel the caller reports blocked, or 0 for the rule off.
   *
   * The blocked-direction cooldown: a direction that produced no movement is habituated at once
   * rather than over the several decisions `fatigueGain` would take, so a wall costs one hold
   * instead of a minute. Nothing about the environment enters the score -- the caller says *which*
   * channel is blocked and this says how hard that is penalised.
   */
  blockedFatigue: number;
  /**
   * Milliseconds the caller must observe no movement before it reports the held channel blocked,
   * or 0 for the rule off.
   *
   * This class never reads it. The caller owns the clock and the position -- in `flysim` the sim
   * loop, from the adapter's sampled coordinates -- and this is where the number lives so that a
   * preset stays one object and `docs/readout.md` has one table to state it in.
   */
  blockedMs: number;
}

/** An independent, threshold-triggered pulse channel. */
export interface PulseChannel {
  channel: string;
  /** Rate role driving this channel. */
  role: string;
  /** Milliseconds the pulse stays active. */
  holdMs: number;
  /** Milliseconds before the channel may fire again. */
  cooldownMs: number;
  /** The normalized score must exceed this for the channel to fire. */
  threshold: number;
  /** Alternate threshold/cooldown used while `decode(..., boot = true)`. */
  boot?: { cooldownMs: number; threshold: number };
  /** Channels sharing a throttle group share their cooldown. */
  throttleGroup?: string;
}

export interface DecoderConfig {
  exclusive?: ExclusiveGroup;
  /**
   * A second exclusive group, the macro channels (`docs/design/macros.md` sections 11 and 12).
   *
   * Same rules as {@link DecoderConfig.exclusive} and, in the Game Boy preset, the same numbers: a
   * macro is a button, pressed by its own population, decided by hold, hysteresis and fatigue like
   * a direction. The one difference is the caller's: only the channels it names as bound compete
   * at a decision (`decode(..., bound)`), because the scene decides which buttons exist.
   */
  macros?: ExclusiveGroup;
  pulses: PulseChannel[];
  /** Cooldown applied to every channel by `clearHolds()`. */
  clearLockoutMs: number;
}

/** Serialized decoder state. */
export interface DecoderState {
  version: 4;
  calibrated: boolean;
  /** Rate role -> calibration rate. */
  baseline: Record<string, number>;
  /** Channel -> millisecond timestamp the channel stays active until. */
  heldUntil: Record<string, number>;
  /** Channel -> millisecond timestamp the channel may next fire. */
  nextAllowed: Record<string, number>;
  /** Millisecond timestamp of the next exclusive-group decision. */
  nextDecision: number;
  /** Current exclusive-group winner, or null. */
  current: string | null;
  /** Exclusive channel -> fatigue in [0, 1]. */
  fatigue: Record<string, number>;
  /**
   * Millisecond timestamp of the next macro-group decision.
   *
   * The macro group's three fields are additive and the schema version does not move with them: a
   * checkpoint written before the group existed carries none of them and the group then starts
   * rested with no winner, the same rule the network's rates follow.
   */
  macroNextDecision?: number;
  /** Current macro-group winner, or null. */
  macroCurrent?: string | null;
  /** Macro channel -> fatigue in [0, 1]. */
  macroFatigue?: Record<string, number>;
}

/**
 * Checkpoint written by the original fixed motor decoder (versions undefined, 2 and 3). Accepted by
 * `importState()` when the configured channel names match, so old checkpoints keep loading.
 */
export interface LegacyDecoderState {
  version?: number;
  calibrated: boolean;
  baseline: Record<string, number>;
  heldUntil: Record<string, number>;
  nextAllowed: Record<string, number>;
  nextDirectionDecision: number;
  direction?: string | null;
  /** Present from version 3 onwards. */
  fatigue?: Record<string, number>;
}

/** Union of every accepted checkpoint field, so validation can run before any state is touched. */
interface AnyDecoderState {
  version?: number;
  calibrated?: unknown;
  baseline?: Record<string, number>;
  heldUntil?: Record<string, number>;
  nextAllowed?: Record<string, number>;
  nextDecision?: number;
  nextDirectionDecision?: number;
  current?: string | null;
  direction?: string | null;
  fatigue?: Record<string, number>;
  macroNextDecision?: number;
  macroCurrent?: string | null;
  macroFatigue?: Record<string, number>;
}

/**
 * One exclusive group's decision, shared by the direction group and the macro group.
 *
 * Lifted verbatim out of `decode()` when the macro group arrived, so the two cannot drift: the
 * same fatigue division, the same tie rule, the same hysteresis, the same hold. `competing` is
 * the only thing that differs between them — `null` for the direction group, where every channel
 * always competes, and the bound set for the macro group — and with `null` this is statement for
 * statement the decode that shipped before it.
 */
function decideExclusive(
  group: ExclusiveGroup,
  channels: readonly string[],
  competing: readonly string[] | null,
  scores: Record<string, number>,
  state: { fatigue: Record<string, number>; current: string | null; nextDecision: number },
  heldUntil: Record<string, number>,
  nowMs: number,
): void {
  if (channels.length === 0 || nowMs < state.nextDecision) return;
  // Bounded habituation stops a small persistent rate bias from holding one channel forever.
  for (const channel of channels) scores[channel]! /= 1 + state.fatigue[channel]!;
  // The field of candidates: every channel of the group, or only the ones the caller named.
  const candidates = competing === null ? channels : channels.filter(channel => competing.includes(channel));
  // A scene that binds nothing takes no decision at all: no winner, no hold, no fatigue, and the
  // clock does not advance, so the frame a button appears on is a frame that can press it.
  if (candidates.length === 0) return;
  // Ties keep the earlier channel: only a strictly greater score displaces the running best.
  let best = candidates.reduce((a, b) => (scores[b]! > scores[a]! ? b : a));
  // The incumbent's commitment bonus, but only while it is still on the pad: a channel the scene
  // has taken away cannot hold its own seat.
  const held = state.current;
  if (held && candidates.includes(held) && scores[best]! < scores[held]! * group.hysteresis) best = held;
  for (const channel of channels) heldUntil[channel] = 0;
  state.current = best;
  for (const channel of channels) {
    state.fatigue[channel] = channel === best
      ? Math.min(1, state.fatigue[channel]! + group.fatigueGain)
      : state.fatigue[channel]! * group.fatigueDecay;
  }
  heldUntil[best] = nowMs + group.holdMs;
  state.nextDecision = nowMs + group.decisionMs;
}

export class PopulationDecoder {
  private readonly config: DecoderConfig;
  /** Exclusive channels in config order. */
  private readonly exclusiveChannels: readonly string[];
  /** Macro-group channels in config order. */
  private readonly macroChannels: readonly string[];
  /**
   * Every channel: exclusive channels first (config order), then pulses, then the macro group.
   *
   * The macro channels go last so that every index, every record order and every `active` list a
   * consumer already reads is untouched by their arrival.
   */
  private readonly channels: readonly string[];
  private readonly roles: Record<string, string>;
  private readonly throttleGroups: Map<string, readonly string[]>;
  private readonly baseline: Record<string, number> = {};
  /**
   * Roles a restored checkpoint carried no baseline for, calibrated from the rates at the first
   * `decode` after the restore.
   *
   * `docs/readout.md`, "Channels added after a checkpoint was written". A checkpoint predates any
   * channel group added since it was written -- the macro group is the case this was found on --
   * and `importState` assigns the baselines it has, which leaves the new roles absent. An absent
   * baseline reads as zero, so the score `(rate + 1) / (baseline + 1)` becomes `rate + 1`: the
   * channel competes on its **raw rate** against channels normalized to about 1, and a group whose
   * raw rates span 27 to 145 Hz is decided by which population happens to fire fastest. Live on
   * 2026-09-17, forty-eight minutes in Oak's lab: `macro_talk` at 41 Hz could not beat
   * `macro_frontier` at 54 or `macro_objective` at 65, so `TALK` was never chosen and the rung
   * that needed one A press was never earned.
   *
   * Calibrating them from the first decode's rates is what warm-up does
   * (`NeuralAgent.warmup`: settle, then `calibrate` on one snapshot of the settled rates), so this
   * is the same rule at the same fidelity -- one sample, not a window -- applied to the channels
   * warm-up never saw. Transient: it is consumed by the next decode and is not part of
   * `DecoderState`, so no checkpoint schema moves.
   */
  private pendingBaseline: string[] = [];
  /** The scores the last `decode` computed, for `lastScores`. */
  private lastScoresByChannel: Record<string, number> = {};
  private readonly heldUntil: Record<string, number>;
  private readonly nextAllowed: Record<string, number>;
  private calibrated = false;
  private nextDecision = 0;
  private current: string | null = null;
  private fatigue: Record<string, number>;
  private macroNextDecision = 0;
  private macroCurrent: string | null = null;
  private macroFatigue: Record<string, number>;

  constructor(config: DecoderConfig) {
    this.config = config;
    const exclusive = config.exclusive;
    const macros = config.macros;
    this.exclusiveChannels = exclusive ? Object.keys(exclusive.channels) : [];
    this.macroChannels = macros ? Object.keys(macros.channels) : [];
    this.channels = [...this.exclusiveChannels, ...config.pulses.map(pulse => pulse.channel), ...this.macroChannels];
    if (new Set(this.channels).size !== this.channels.length) throw new Error('Duplicate decoder channel');
    const roles: Record<string, string> = {};
    if (exclusive) for (const channel of this.exclusiveChannels) roles[channel] = exclusive.channels[channel]!;
    for (const pulse of config.pulses) roles[pulse.channel] = pulse.role;
    if (macros) for (const channel of this.macroChannels) roles[channel] = macros.channels[channel]!;
    this.roles = roles;
    this.heldUntil = Object.fromEntries(this.channels.map(channel => [channel, 0]));
    this.nextAllowed = Object.fromEntries(this.channels.map(channel => [channel, 0]));
    this.fatigue = this.zeroFatigue();
    this.macroFatigue = this.zeroMacroFatigue();
    const throttleGroups = new Map<string, string[]>();
    for (const pulse of config.pulses) {
      if (pulse.throttleGroup === undefined) continue;
      const group = throttleGroups.get(pulse.throttleGroup) ?? [];
      group.push(pulse.channel);
      throttleGroups.set(pulse.throttleGroup, group);
    }
    this.throttleGroups = throttleGroups;
  }

  /** Channel names this decoder can report, in decode order. */
  get channelNames(): readonly string[] {
    return this.channels;
  }

  /** The macro group's channels in config order, or an empty list with no group. */
  get macroChannelNames(): readonly string[] {
    return this.macroChannels;
  }

  /**
   * The macro group's current winner, or null before its first decision.
   *
   * This is the macro the game layer starts (`docs/design/macros.md` section 12: "whichever bound
   * channel wins, starts"). It is also what `decode` reports while the winner is held.
   */
  get macroWinner(): string | null {
    return this.macroCurrent;
  }

  /** Record the resting rates every score is normalized against. */
  calibrate(rates: Record<string, number>): void {
    for (const channel of this.channels) {
      const role = this.roles[channel]!;
      this.baseline[role] = rates[role] ?? 0;
    }
    this.pendingBaseline = [];
    this.calibrated = true;
  }

  /** Roles still waiting for a baseline after a restore, in channel order. For `/status`. */
  get pendingBaselineRoles(): readonly string[] {
    return this.pendingBaseline;
  }

  /** The resting rate each role is normalized against, as `calibrate` recorded it. For `/status`. */
  get baselines(): Record<string, number> {
    return { ...this.baseline };
  }

  /**
   * The normalized score of every channel as the last `decode` computed it:
   * `(rate + 1) / (baseline + 1)`, before the group's fatigue division.
   *
   * The score `docs/readout.md` defines, where 1.0 is the calibration rate — the Rust twin's
   * `last_scores()`, which the game layer already reads. Read-only, and nothing in this class reads
   * it back, so it cannot enter a decision. A decoder that has not decoded yet reports nothing.
   */
  get lastScores(): Record<string, number> {
    return { ...this.lastScoresByChannel };
  }

  /** Drop every hold, forget the exclusive winner and lock all channels out for `clearLockoutMs`. */
  clearHolds(nowMs: number): void {
    for (const channel of this.channels) {
      this.heldUntil[channel] = 0;
      this.nextAllowed[channel] = nowMs + this.config.clearLockoutMs;
    }
    this.current = null;
    for (const channel of Object.keys(this.fatigue)) this.fatigue[channel] = 0;
    this.nextDecision = nowMs;
    this.macroCurrent = null;
    for (const channel of Object.keys(this.macroFatigue)) this.macroFatigue[channel] = 0;
    this.macroNextDecision = nowMs;
  }

  /**
   * Advance the readout to `nowMs` and return the active channel names: exclusive channels first
   * (config order), then pulses (config order). `boot` selects each pulse channel's boot variant.
   */
  /**
   * @param bound - macro channels that may win this decision: the scene's own buttons
   * (`docs/design/macros.md` section 12, "unbound channels are masked from the decision"). `null`
   * lets every channel of the macro group compete, which is what the direction group always does;
   * an empty list is a scene with no macro at all and takes no decision. Masking rather than
   * penalising is the one place this differs from the blocked-direction cooldown it is modelled
   * on: a blocked channel is habituated and can still win, an unbound channel is not on the pad.
   */
  decode(
    rates: Record<string, number>,
    nowMs: number,
    boot = true,
    blocked: string | null = null,
    bound: readonly string[] | null = null,
  ): string[] {
    if (!this.calibrated) return [];
    // The blocked-direction cooldown. `blocked` names an exclusive channel the caller has observed
    // producing no movement; its only effect is to raise that channel's fatigue to
    // `blockedFatigue` straight away, so the next decision sees a habituated incumbent rather than
    // an untired one. `Math.max` rather than `+=` because the caller reports the same blockage on
    // every frame of a hold: one wall is one penalty however many times it is observed. This is
    // the only input to the readout that is not a rate, and it is deliberately the narrowest one
    // that fixes a wall bump -- it says "that button did nothing", not where the player is, where
    // it should go, or what the room looks like. `blockedFatigue = 0` ignores it entirely, and so
    // does a `blocked` naming a channel outside the group. It needs no new checkpoint field,
    // because fatigue is already in `DecoderState`.
    const blockedFatigue = this.config.exclusive?.blockedFatigue ?? 0;
    if (blocked !== null && blockedFatigue > 0 && this.fatigue[blocked] !== undefined) {
      this.fatigue[blocked] = Math.min(1, Math.max(blockedFatigue, this.fatigue[blocked]!));
    }
    // The same rule for the macro group, for the caller that one day reports a macro that moved
    // nothing. Nothing does today, so this is symmetry rather than a live path, and it is here
    // because the group's contract is "the same hold, hysteresis, fatigue and blocked rules".
    const macroBlockedFatigue = this.config.macros?.blockedFatigue ?? 0;
    if (blocked !== null && macroBlockedFatigue > 0 && this.macroFatigue[blocked] !== undefined) {
      this.macroFatigue[blocked] = Math.min(1, Math.max(macroBlockedFatigue, this.macroFatigue[blocked]!));
    }
    // A role the restored checkpoint had no baseline for is calibrated here, from this decode's
    // rates, before any score is computed: see `pendingBaseline`. Every such channel therefore
    // scores exactly 1.0 on this decision -- at rest, which is what a channel nobody has measured
    // honestly is -- instead of competing on its raw rate.
    if (this.pendingBaseline.length > 0) {
      for (const role of this.pendingBaseline) this.baseline[role] = rates[role] ?? 0;
      this.pendingBaseline = [];
    }
    const scores: Record<string, number> = {};
    for (const channel of this.channels) {
      const role = this.roles[channel]!;
      scores[channel] = ((rates[role] ?? 0) + 1) / ((this.baseline[role] ?? 0) + 1);
    }
    this.lastScoresByChannel = { ...scores };
    const exclusive = this.config.exclusive;
    if (exclusive) {
      const state = { fatigue: this.fatigue, current: this.current, nextDecision: this.nextDecision };
      decideExclusive(exclusive, this.exclusiveChannels, null, scores, state, this.heldUntil, nowMs);
      this.current = state.current;
      this.nextDecision = state.nextDecision;
    }
    // The macro group, after the buttons and on its own clock, with only the scene's bound
    // channels competing (`docs/design/macros.md` section 12).
    const macros = this.config.macros;
    if (macros) {
      const state = { fatigue: this.macroFatigue, current: this.macroCurrent, nextDecision: this.macroNextDecision };
      decideExclusive(macros, this.macroChannels, bound, scores, state, this.heldUntil, nowMs);
      this.macroCurrent = state.current;
      this.macroNextDecision = state.nextDecision;
    }
    for (const pulse of this.config.pulses) {
      const variant = boot && pulse.boot ? pulse.boot : pulse;
      if (scores[pulse.channel]! > variant.threshold && nowMs >= this.nextAllowed[pulse.channel]!) {
        this.heldUntil[pulse.channel] = nowMs + pulse.holdMs;
        const nextAllowed = nowMs + variant.cooldownMs;
        this.nextAllowed[pulse.channel] = nextAllowed;
        const group = pulse.throttleGroup === undefined ? undefined : this.throttleGroups.get(pulse.throttleGroup);
        if (group) for (const channel of group) this.nextAllowed[channel] = nextAllowed;
      }
    }
    const active: string[] = [];
    for (const channel of this.channels) if (nowMs < this.heldUntil[channel]!) active.push(channel);
    return active;
  }

  exportState(): DecoderState {
    return {
      version: 4,
      calibrated: this.calibrated,
      baseline: { ...this.baseline },
      heldUntil: { ...this.heldUntil },
      nextAllowed: { ...this.nextAllowed },
      nextDecision: this.nextDecision,
      current: this.current,
      fatigue: { ...this.fatigue },
      macroNextDecision: this.macroNextDecision,
      macroCurrent: this.macroCurrent,
      macroFatigue: { ...this.macroFatigue },
    };
  }

  /**
   * Load a version 4 checkpoint, or a legacy motor-decoder checkpoint (version undefined, 2 or 3)
   * whose channel names match this configuration. Every field is validated before anything is
   * written, so a rejected checkpoint leaves the decoder untouched.
   */
  importState(state: DecoderState | LegacyDecoderState): void {
    const raw = state as AnyDecoderState | null | undefined;
    if (!raw) throw new Error('Invalid decoder checkpoint');
    const { version } = raw;
    if (version !== undefined && version !== 2 && version !== 3 && version !== 4) throw new Error('Invalid decoder version');
    const legacy = version !== 4;
    const nextDecision = legacy ? raw.nextDirectionDecision : raw.nextDecision;
    const current = legacy ? raw.direction : raw.current;
    const { baseline, heldUntil, nextAllowed } = raw;
    if (
      typeof raw.calibrated !== 'boolean'
      || !Number.isFinite(nextDecision)
      || [baseline, heldUntil, nextAllowed].some(record => !record || Object.values(record).some(value => !Number.isFinite(value)))
      // The macro channels are exempt from "every channel must be in the checkpoint": one written
      // before the group existed carries none of them and they start at zero, which is what let
      // `docs/design/macros.md` section 11 add them without moving the schema version. A macro
      // entry that *is* present must still be finite, which the sweep above covers.
      || this.requiredChannels().some(channel => !Number.isFinite(heldUntil![channel]) || !Number.isFinite(nextAllowed![channel]))
    ) throw new Error('Invalid decoder checkpoint');
    let fatigue: Record<string, number>;
    if (version === 3 || version === 4) {
      const incoming = raw.fatigue;
      if (!incoming || Object.keys(this.fatigue).some(channel => {
        const value = incoming[channel];
        return !Number.isFinite(value) || value! < 0 || value! > 1;
      })) throw new Error('Invalid decoder fatigue');
      fatigue = { ...incoming };
    } else {
      // Version 2 and earlier predate habituation: start rested.
      fatigue = this.zeroFatigue();
    }
    // The macro group's fatigue is optional for the same reason, so an absent entry is not a
    // fault and a present one is held to the same bounds.
    const macroFatigue = raw.macroFatigue ?? {};
    if (
      this.macroChannels.some(channel => {
        const value = macroFatigue[channel];
        return value !== undefined && (!Number.isFinite(value) || value < 0 || value > 1);
      })
      || (raw.macroNextDecision !== undefined && !Number.isFinite(raw.macroNextDecision))
    ) throw new Error('Invalid decoder fatigue');
    if (current != null && !this.exclusiveChannels.includes(current)) throw new Error('Invalid decoder channel');
    const macroCurrent = raw.macroCurrent ?? null;
    if (macroCurrent != null && !this.macroChannels.includes(macroCurrent)) throw new Error('Invalid decoder channel');
    // Which roles this checkpoint does not know about, in channel order and deduplicated: the
    // first decode after the restore calibrates them. Computed from the record that came *in*
    // rather than from `this.baseline`, so a decoder reused across two restores cannot keep a
    // baseline the new checkpoint never had.
    this.pendingBaseline = [];
    for (const channel of this.channels) {
      const role = this.roles[channel]!;
      if (baseline?.[role] === undefined && !this.pendingBaseline.includes(role)) {
        this.pendingBaseline.push(role);
      }
    }
    Object.assign(this.baseline, baseline);
    Object.assign(this.heldUntil, heldUntil);
    Object.assign(this.nextAllowed, nextAllowed);
    this.calibrated = raw.calibrated;
    this.nextDecision = nextDecision!;
    this.fatigue = fatigue;
    this.current = current ?? null;
    // The macro group, channel by channel rather than by wholesale assignment, so a checkpoint
    // carrying none of them leaves every macro channel rested at zero.
    this.macroNextDecision = raw.macroNextDecision ?? 0;
    this.macroCurrent = macroCurrent;
    this.macroFatigue = Object.fromEntries(this.macroChannels.map(channel => [channel, macroFatigue[channel] ?? 0]));
  }

  private zeroFatigue(): Record<string, number> {
    return Object.fromEntries(this.exclusiveChannels.map(channel => [channel, 0]));
  }

  private zeroMacroFatigue(): Record<string, number> {
    return Object.fromEntries(this.macroChannels.map(channel => [channel, 0]));
  }

  /** Channels a version 4 checkpoint must carry: the direction group and the pulses. */
  private requiredChannels(): readonly string[] {
    return this.channels.filter(channel => !this.macroChannels.includes(channel));
  }
}
