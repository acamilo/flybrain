//! One run's frames in, one [`RunReport`] out.
//!
//! Everything the scorecard measures about play is computed here from [`Frame`]s, with no
//! emulator, brain or runtime in sight, so every definition is unit-tested on synthetic frames.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::obs::{Facts, Frame};
use crate::watchdog::{Ev, Probe, Rules, Watchdog};

/// One Game Boy frame of game time, ms (`70,224` cycles at `4,194,304` Hz).
pub const FRAME_MS: f64 = 1000.0 * 70_224.0 / 4_194_304.0;
const MINUTE_MS: f64 = 60_000.0;

/// The trap hunt's window rules (`examples/trap_hunt.rs` in flysim), kept as it has them: two
/// brain minutes, stepped by fifteen seconds, fewer than four tiles or one macro sequence
/// repeated more than ten times (any period up to eight).
pub const HUNT_WINDOW_MS: f64 = 2.0 * MINUTE_MS;
pub const HUNT_STEP_MS: f64 = 15_000.0;
pub const HUNT_MIN_TILES: usize = 4;
pub const HUNT_MAX_REPEATS: usize = 10;
pub const HUNT_MAX_PERIOD: usize = 8;

/// How a run is measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TallyConfig {
    pub rules: Rules,
    /// Brain seconds between watchdog probes.
    pub probe_s: f64,
    /// Brain seconds before the first probe (a window needs some life in it).
    pub first_probe_s: f64,
}

impl Default for TallyConfig {
    fn default() -> Self {
        Self { rules: Rules::default(), probe_s: 120.0, first_probe_s: 240.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RungReport {
    pub start: u32,
    pub end: u32,
    pub gained: u32,
    /// `[rung, brain minute]` for every climb.
    pub climbs: Vec<(u32, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PadReport {
    /// Frames with the macro layer on, nothing running and no button dealt.
    pub empty_frames: u64,
    pub empty_seconds: f64,
    pub empty_fraction: f64,
    pub longest_empty_seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchdogReport {
    pub probes: usize,
    pub suspected: usize,
    /// Probes that were suspected with the one before also suspected (the loop recovery's
    /// "confirmed": two fresh suspected reports in a row).
    pub confirmed: usize,
    /// Runs of two or more suspected probes: each is one trap the recovery ladder would act on.
    pub ladder_events: usize,
    pub reasons: BTreeMap<String, usize>,
    pub flagged: Vec<Probe>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HuntWindow {
    pub at_min: f64,
    pub tiles: usize,
    pub macros: usize,
    pub cycle: Option<(String, usize)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HuntReport {
    pub windows: usize,
    pub flagged: usize,
    /// The first twenty flagged windows.
    pub first_flagged: Vec<HuntWindow>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RatchetReport {
    pub rollbacks: u32,
    pub game_over: u32,
    pub stall: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroCounts {
    pub start: u64,
    pub done: u64,
    pub blocked: u64,
    pub timeout: u64,
    pub refused: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroReport {
    pub starts: u64,
    pub done: u64,
    pub blocked: u64,
    pub timeout: u64,
    pub refused: u64,
    pub by_macro: BTreeMap<String, MacroCounts>,
}

impl MacroReport {
    /// Finishes that were not `done`, which is what "blocked macro counts" asks for.
    pub fn failed(&self) -> u64 {
        self.blocked + self.timeout + self.refused
    }
}

/// GO SHOP, BUY BALL, THROW BALL, catch, and the bag around them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FunnelReport {
    pub go_shop_start: u64,
    pub go_shop_done: u64,
    pub buy_ball_start: u64,
    pub buy_ball_done: u64,
    pub throw_ball_start: u64,
    pub throw_ball_done: u64,
    /// `catch` rewards: a wild Pokemon kept.
    pub catches: u64,
    /// New species owned (Pokedex bits that went up).
    pub species_gained: u64,
    pub balls_gained: u64,
    pub balls_spent: u64,
    /// How far down GO SHOP done, BUY BALL done, THROW BALL start, catch the run got, in order of
    /// first occurrence (0 to 4).
    pub depth: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HealReport {
    pub go_heal_start: u64,
    pub go_heal_done: u64,
    pub heal_start: u64,
    /// The nurse's `HEAL` finished: a party healed.
    pub heal_done: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BattleReport {
    pub started: u64,
    pub ended: u64,
    pub wild: u64,
    pub trainer: u64,
    /// Ended with a battle or trainer payout.
    pub won: u64,
    /// Ended with the whole party fainted.
    pub lost: u64,
    /// A wild Pokemon kept.
    pub caught: u64,
    /// Ended otherwise (run away, a ball thrown away from the kept Pokemon, ...).
    pub other: u64,
    pub longest_frames: u64,
    pub still_running: bool,
    pub macros_in_longest: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RewardTotals {
    pub n: u64,
    pub total: f64,
}

/// One run: a checkpoint, a seed, a runtime, some brain minutes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub checkpoint: String,
    pub seed: u32,
    pub runtime: String,
    pub mode: String,
    /// Brain minutes measured (after the seed's idle frames).
    pub minutes: f64,
    pub frames: u64,
    pub idle_frames: u32,
    pub wall_seconds: f64,
    pub rungs: RungReport,
    pub places_start: u32,
    pub places_end: u32,
    pub pad: PadReport,
    pub watchdog: WatchdogReport,
    pub hunt: HuntReport,
    pub ratchet: RatchetReport,
    pub macros: MacroReport,
    pub funnel: FunnelReport,
    pub heals: HealReport,
    pub whiteouts: u64,
    pub battles: BattleReport,
    pub rewards: BTreeMap<String, RewardTotals>,
    pub scenes: BTreeMap<String, u64>,
    pub ended_map: u32,
}

/// The battle in progress.
#[derive(Clone, Debug, Default)]
struct Battle {
    frames: u64,
    macros: u64,
    won: bool,
    caught: bool,
    fainted: bool,
}

pub struct Tally {
    config: TallyConfig,
    began_ms: f64,
    last_ms: f64,
    frames: u64,
    start_rank: u32,
    rank: u32,
    start_places: u32,
    places: u32,
    climbs: Vec<(u32, f64)>,
    macros_on: bool,
    empty_frames: u64,
    empty_run: u64,
    longest_empty: u64,
    events: Vec<Ev>,
    watchdog: Watchdog,
    next_probe_ms: f64,
    probes: Vec<Probe>,
    steps: Vec<(f64, u32, u32, u32)>,
    starts: Vec<(f64, &'static str)>,
    ratchet: RatchetReport,
    macro_report: MacroReport,
    funnel: FunnelReport,
    funnel_order: [Option<f64>; 4],
    heals: HealReport,
    whiteouts: u64,
    was_fainted: bool,
    battles: BattleReport,
    battle: Option<Battle>,
    rewards: BTreeMap<String, RewardTotals>,
    scenes: BTreeMap<String, u64>,
    last_facts: Option<Facts>,
    ended_map: u32,
}

/// The feed's kind for an adapter reward kind (`flysim::snapshot::RewardKind::from_adapter`), or
/// an empty string where the feed has no counter. The watchdog reads the feed's names.
pub fn feed_kind(adapter_kind: &str) -> &'static str {
    flysim::snapshot::RewardKind::from_adapter(adapter_kind).map_or("", |kind| kind.as_str())
}

impl Tally {
    /// The first frame's brain clock, rung and exploration count are the run's start. Nothing
    /// before it is measured.
    pub fn new(config: TallyConfig, began_ms: f64, rank: u32, places: u32) -> Self {
        let watchdog = Watchdog::new(config.rules.clone(), places);
        let next_probe_ms = began_ms + config.first_probe_s * 1000.0;
        Self {
            config,
            began_ms,
            last_ms: began_ms,
            frames: 0,
            start_rank: rank,
            rank,
            start_places: places,
            places,
            climbs: Vec::new(),
            macros_on: false,
            empty_frames: 0,
            empty_run: 0,
            longest_empty: 0,
            events: Vec::new(),
            watchdog,
            next_probe_ms,
            probes: Vec::new(),
            steps: Vec::new(),
            starts: Vec::new(),
            ratchet: RatchetReport::default(),
            macro_report: MacroReport::default(),
            funnel: FunnelReport::default(),
            funnel_order: [None; 4],
            heals: HealReport::default(),
            whiteouts: 0,
            was_fainted: false,
            battles: BattleReport::default(),
            battle: None,
            rewards: BTreeMap::new(),
            scenes: BTreeMap::new(),
            last_facts: None,
            ended_map: 0xff,
        }
    }

    /// Brain minutes measured so far.
    pub fn minutes(&self) -> f64 {
        (self.last_ms - self.began_ms) / MINUTE_MS
    }

    pub fn push(&mut self, frame: &Frame) {
        let ms = frame.ms;
        self.frames += 1;
        self.last_ms = ms;
        self.macros_on |= frame.macros_on;
        if frame.rank != self.rank {
            if frame.rank > self.rank {
                self.climbs.push((frame.rank, (ms - self.began_ms) / MINUTE_MS));
                self.events.push(Ev::Milestone { ms });
            }
            self.rank = frame.rank;
        }
        self.places = frame.places;
        if let Some((map, x, y)) = frame.location {
            self.steps.push((ms, map, x, y));
        }
        *self.scenes.entry(frame.scene.to_owned()).or_insert(0) += 1;

        // The pad.
        if frame.macros_on && frame.pad_empty {
            self.empty_frames += 1;
            self.empty_run += 1;
            self.longest_empty = self.longest_empty.max(self.empty_run);
        } else {
            self.empty_run = 0;
        }

        // Macros.
        for event in &frame.events {
            let counts = self.macro_report.by_macro.entry(event.name.to_owned()).or_default();
            match event.outcome {
                None => {
                    counts.start += 1;
                    self.macro_report.starts += 1;
                    self.starts.push((ms, event.name));
                    if let Some(battle) = self.battle.as_mut() {
                        battle.macros += 1;
                    }
                }
                Some("done") => {
                    counts.done += 1;
                    self.macro_report.done += 1;
                }
                Some("blocked") => {
                    counts.blocked += 1;
                    self.macro_report.blocked += 1;
                }
                Some("timeout") => {
                    counts.timeout += 1;
                    self.macro_report.timeout += 1;
                }
                Some("refused") => {
                    counts.refused += 1;
                    self.macro_report.refused += 1;
                }
                Some(_) => {}
            }
            self.events.push(Ev::Macro { ms, name: event.name, outcome: event.outcome });
            self.funnel_and_heals(ms, event.name, event.outcome);
        }

        // Rewards.
        let mut won_now = false;
        let mut caught_now = false;
        for (kind, value) in &frame.rewards {
            let totals = self.rewards.entry((*kind).to_owned()).or_default();
            totals.n += 1;
            totals.total += value;
            self.events.push(Ev::Reward { ms, kind: feed_kind(kind) });
            match *kind {
                "battle" | "trainer" => won_now = true,
                "catch" => {
                    caught_now = true;
                    self.funnel.catches += 1;
                    self.note_stage(3, ms);
                }
                _ => {}
            }
        }

        // The ratchet.
        if let Some(rolled) = frame.rollback {
            self.ratchet.rollbacks += 1;
            if rolled.game_over {
                self.ratchet.game_over += 1;
            } else {
                self.ratchet.stall += 1;
            }
        }

        // Memory: bag, party, battles.
        let facts = frame.facts;
        if let Some(last) = self.last_facts {
            if facts.balls > last.balls {
                self.funnel.balls_gained += u64::from(facts.balls - last.balls);
            }
            if facts.balls < last.balls {
                self.funnel.balls_spent += u64::from(last.balls - facts.balls);
            }
            if facts.owned > last.owned {
                self.funnel.species_gained += u64::from(facts.owned - last.owned);
            }
        }
        self.last_facts = Some(facts);
        self.ended_map = facts.map;
        let fainted = facts.all_fainted();
        if fainted && !self.was_fainted {
            self.whiteouts += 1;
        }
        self.was_fainted = fainted;
        match (facts.in_battle(), self.battle.as_mut()) {
            (true, None) => {
                self.battles.started += 1;
                if facts.battle == 2 {
                    self.battles.trainer += 1;
                } else {
                    self.battles.wild += 1;
                }
                self.battle = Some(Battle {
                    frames: 1,
                    won: won_now,
                    caught: caught_now,
                    fainted,
                    ..Battle::default()
                });
            }
            (true, Some(battle)) => {
                battle.frames += 1;
                battle.won |= won_now;
                battle.caught |= caught_now;
                battle.fainted |= fainted;
            }
            (false, Some(battle)) => {
                battle.won |= won_now;
                battle.caught |= caught_now;
                battle.fainted |= fainted;
                let battle = self.battle.take().expect("a battle");
                self.end_battle(battle);
            }
            (false, None) => {}
        }

        // Probes.
        while ms >= self.next_probe_ms {
            let at_min = (self.next_probe_ms - self.began_ms) / MINUTE_MS;
            let probe = self.watchdog.probe(&self.events, self.places, at_min);
            self.probes.push(probe);
            self.next_probe_ms += self.config.probe_s * 1000.0;
        }
    }

    fn end_battle(&mut self, battle: Battle) {
        self.battles.ended += 1;
        if battle.frames > self.battles.longest_frames {
            self.battles.longest_frames = battle.frames;
            self.battles.macros_in_longest = battle.macros;
        }
        if battle.won {
            self.battles.won += 1;
        } else if battle.fainted {
            self.battles.lost += 1;
        } else if battle.caught {
            self.battles.caught += 1;
        } else {
            self.battles.other += 1;
        }
        if battle.caught {
            // A catch that is also a win payout counts once as a win and is still a catch.
            if battle.won {
                self.battles.caught += 1;
            }
        }
    }

    fn funnel_and_heals(&mut self, ms: f64, name: &str, outcome: Option<&str>) {
        let f = &mut self.funnel;
        let h = &mut self.heals;
        let mut stage = None;
        match (name, outcome) {
            ("GO SHOP", None) => f.go_shop_start += 1,
            ("GO SHOP", Some("done")) => {
                f.go_shop_done += 1;
                stage = Some(0);
            }
            ("BUY BALL", None) => f.buy_ball_start += 1,
            ("BUY BALL", Some("done")) => {
                f.buy_ball_done += 1;
                stage = Some(1);
            }
            ("THROW BALL", None) => {
                f.throw_ball_start += 1;
                stage = Some(2);
            }
            ("THROW BALL", Some("done")) => f.throw_ball_done += 1,
            ("GO HEAL", None) => h.go_heal_start += 1,
            ("GO HEAL", Some("done")) => h.go_heal_done += 1,
            ("HEAL", None) => h.heal_start += 1,
            ("HEAL", Some("done")) => h.heal_done += 1,
            _ => {}
        }
        if let Some(stage) = stage {
            self.note_stage(stage, ms);
        }
    }

    fn note_stage(&mut self, stage: usize, ms: f64) {
        self.funnel_order[stage].get_or_insert(ms);
    }

    /// The run's report. `checkpoint`, `seed`, `runtime` and the rest of the identity are the
    /// caller's.
    pub fn finish(mut self, identity: RunReport) -> RunReport {
        // The funnel's depth: stages in order of first occurrence, each after the last one.
        let mut depth = 0;
        let mut after = f64::NEG_INFINITY;
        for first in self.funnel_order {
            match first {
                Some(ms) if ms >= after => {
                    depth += 1;
                    after = ms;
                }
                _ => break,
            }
        }
        self.funnel.depth = depth;
        if let Some(battle) = self.battle.take() {
            self.battles.still_running = true;
            self.battles.longest_frames = self.battles.longest_frames.max(battle.frames);
        }
        let minutes = self.minutes();
        let seconds = |frames: u64| frames as f64 * FRAME_MS / 1000.0;

        // The watchdog.
        let mut watchdog = WatchdogReport { probes: self.probes.len(), ..Default::default() };
        let mut run = 0usize;
        for probe in &self.probes {
            if probe.suspected {
                watchdog.suspected += 1;
                run += 1;
                if run >= 2 {
                    watchdog.confirmed += 1;
                }
                if run == 2 {
                    watchdog.ladder_events += 1;
                }
                if let Some(reason) = &probe.reason {
                    *watchdog.reasons.entry(reason.clone()).or_insert(0) += 1;
                }
                if watchdog.flagged.len() < 20 {
                    watchdog.flagged.push(probe.clone());
                }
            } else {
                run = 0;
            }
        }

        let hunt = hunt(&self.steps, &self.starts, self.began_ms, self.last_ms);
        let macro_report = std::mem::take(&mut self.macro_report);
        RunReport {
            minutes,
            frames: self.frames,
            rungs: RungReport {
                start: self.start_rank,
                end: self.rank,
                gained: self.rank.saturating_sub(self.start_rank),
                climbs: self.climbs,
            },
            places_start: self.start_places,
            places_end: self.places,
            pad: PadReport {
                empty_frames: self.empty_frames,
                empty_seconds: seconds(self.empty_frames),
                empty_fraction: if self.frames == 0 || !self.macros_on {
                    0.0
                } else {
                    self.empty_frames as f64 / self.frames as f64
                },
                longest_empty_seconds: seconds(self.longest_empty),
            },
            watchdog,
            hunt,
            ratchet: self.ratchet,
            macros: macro_report,
            funnel: self.funnel,
            heals: self.heals,
            whiteouts: self.whiteouts,
            battles: self.battles,
            rewards: self.rewards,
            scenes: self.scenes,
            ended_map: self.ended_map,
            ..identity
        }
    }
}

/// The longest run of one repeated block in `names`, as `(block, repeats)`
/// (`examples/trap_hunt.rs`: every period up to eight, every offset).
pub fn longest_cycle(names: &[&'static str]) -> Option<(String, usize)> {
    let mut best: Option<(String, usize)> = None;
    for period in 1..=HUNT_MAX_PERIOD.min(names.len()) {
        let mut start = 0;
        while start + period <= names.len() {
            let block = &names[start..start + period];
            let mut repeats = 1;
            while start + period * (repeats + 1) <= names.len()
                && &names[start + period * repeats..start + period * (repeats + 1)] == block
            {
                repeats += 1;
            }
            if best.as_ref().is_none_or(|(_, known)| repeats > *known) {
                best = Some((block.join(", "), repeats));
            }
            start += period * repeats;
        }
    }
    best
}

/// Every window of the run that is a trap by the trap hunt's rules: it did things (a window with
/// no macro in it is the fly choosing nothing, which is the doctrine working) and either stood on
/// fewer than four distinct tiles or repeated one macro sequence more than ten times.
pub fn hunt(
    steps: &[(f64, u32, u32, u32)],
    starts: &[(f64, &'static str)],
    began_ms: f64,
    ended_ms: f64,
) -> HuntReport {
    let mut report = HuntReport::default();
    let mut at = began_ms;
    while at + HUNT_WINDOW_MS <= ended_ms {
        let until = at + HUNT_WINDOW_MS;
        report.windows += 1;
        let tiles: BTreeSet<(u32, u32, u32)> = steps
            .iter()
            .filter(|(ms, ..)| *ms >= at && *ms < until)
            .map(|(_, map, x, y)| (*map, *x, *y))
            .collect();
        let names: Vec<&'static str> = starts
            .iter()
            .filter(|(ms, _)| *ms >= at && *ms < until)
            .map(|(_, name)| *name)
            .collect();
        let cycle = longest_cycle(&names).filter(|(_, repeats)| *repeats > HUNT_MAX_REPEATS);
        let stuck = tiles.len() < HUNT_MIN_TILES && !names.is_empty();
        if stuck || cycle.is_some() {
            report.flagged += 1;
            if report.first_flagged.len() < 20 {
                report.first_flagged.push(HuntWindow {
                    at_min: (at - began_ms) / MINUTE_MS,
                    tiles: tiles.len(),
                    macros: names.len(),
                    cycle,
                });
            }
        }
        at += HUNT_STEP_MS;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::{MacroEv, Rolled};

    fn frame(ms: f64) -> Frame {
        Frame {
            ms,
            events: Vec::new(),
            rewards: Vec::new(),
            rank: 8,
            places: 100,
            location: Some((1, (ms / 1000.0) as u32 % 50, 3)),
            macros_on: true,
            pad_empty: false,
            scene: "overworld",
            rollback: None,
            facts: Facts { party: 2, alive: 2, balls: 3, money: 500, owned: 10, map: 1, battle: 0 },
        }
    }

    fn ev(name: &'static str, outcome: Option<&'static str>) -> MacroEv {
        MacroEv { name, outcome }
    }

    fn run(frames: &[Frame]) -> RunReport {
        let mut tally = Tally::new(TallyConfig::default(), 0.0, 8, 100);
        for frame in frames {
            tally.push(frame);
        }
        tally.finish(RunReport { checkpoint: "t".into(), ..RunReport::default() })
    }

    #[test]
    fn rungs_climb_and_are_timed() {
        let mut a = frame(1000.0);
        a.rank = 9;
        let mut b = frame(61_000.0);
        b.rank = 10;
        let report = run(&[a, b]);
        assert_eq!((report.rungs.start, report.rungs.end, report.rungs.gained), (8, 10, 2));
        assert_eq!(report.rungs.climbs, vec![(9, 1000.0 / 60_000.0), (10, 61_000.0 / 60_000.0)]);
    }

    #[test]
    fn the_empty_pad_is_counted_in_seconds_and_in_its_longest_stretch() {
        let mut frames = Vec::new();
        for i in 0..10u64 {
            let mut f = frame(i as f64 * FRAME_MS);
            f.pad_empty = (2..7).contains(&i) || i == 9;
            frames.push(f);
        }
        let report = run(&frames);
        assert_eq!(report.pad.empty_frames, 6);
        assert!((report.pad.empty_fraction - 0.6).abs() < 1e-9);
        assert!((report.pad.longest_empty_seconds - 5.0 * FRAME_MS / 1000.0).abs() < 1e-9);
    }

    #[test]
    fn raw_mode_has_no_pad_to_be_empty() {
        let mut f = frame(0.0);
        f.macros_on = false;
        f.pad_empty = true;
        assert_eq!(run(&[f]).pad.empty_fraction, 0.0);
    }

    #[test]
    fn the_shop_to_catch_funnel_counts_each_stage_and_its_depth() {
        let mut frames = Vec::new();
        let script: [(&'static str, Option<&'static str>); 6] = [
            ("GO SHOP", None),
            ("GO SHOP", Some("done")),
            ("BUY BALL", None),
            ("BUY BALL", Some("done")),
            ("THROW BALL", None),
            ("THROW BALL", Some("done")),
        ];
        for (i, (name, outcome)) in script.iter().enumerate() {
            let mut f = frame(i as f64 * 1000.0);
            f.events = vec![ev(name, *outcome)];
            frames.push(f);
        }
        let mut f = frame(7000.0);
        f.rewards = vec![("catch", 5.0), ("species", 1.0)];
        frames.push(f);
        let report = run(&frames);
        let funnel = &report.funnel;
        assert_eq!(
            (funnel.go_shop_done, funnel.buy_ball_done, funnel.throw_ball_start, funnel.catches),
            (1, 1, 1, 1)
        );
        assert_eq!(funnel.depth, 4);
        assert_eq!(report.macros.starts, 3);
        assert_eq!(report.macros.done, 3);
        assert_eq!(report.rewards["catch"].n, 1);
    }

    #[test]
    fn a_funnel_that_skips_the_shop_stops_at_depth_zero() {
        let mut f = frame(0.0);
        f.events = vec![ev("BUY BALL", Some("done"))];
        // Stage 0 never occurred: depth counts the unbroken prefix only.
        assert_eq!(run(&[f]).funnel.depth, 0);
    }

    #[test]
    fn blocked_refused_and_timed_out_macros_are_counted_by_macro() {
        let mut f = frame(0.0);
        f.events = vec![
            ev("GO ROUTE", Some("refused")),
            ev("GO ROUTE", Some("refused")),
            ev("GO OUT", Some("blocked")),
            ev("GO NPC", Some("timeout")),
            ev("NEXT", Some("done")),
        ];
        let report = run(&[f]);
        assert_eq!(report.macros.failed(), 4);
        assert_eq!(report.macros.by_macro["GO ROUTE"].refused, 2);
        assert_eq!(report.macros.done, 1);
    }

    #[test]
    fn heals_are_the_nurses_heal_and_the_walk_to_her() {
        let mut f = frame(0.0);
        f.events = vec![
            ev("GO HEAL", None),
            ev("GO HEAL", Some("done")),
            ev("HEAL", None),
            ev("HEAL", Some("done")),
        ];
        let heals = run(&[f]).heals;
        assert_eq!((heals.go_heal_done, heals.heal_done), (1, 1));
    }

    fn battle_frames(kind: u8, n: usize, from_ms: f64) -> Vec<Frame> {
        (0..n)
            .map(|i| {
                let mut f = frame(from_ms + i as f64 * FRAME_MS);
                f.facts.battle = kind;
                f
            })
            .collect()
    }

    #[test]
    fn a_battle_is_won_lost_or_caught_by_what_ended_it() {
        let mut frames = Vec::new();
        // A won trainer battle.
        frames.extend(battle_frames(2, 5, 0.0));
        let mut win = frame(100.0);
        win.rewards = vec![("trainer", 1.0)];
        frames.push(win);
        // A wild battle lost: the party faints inside it, then a whiteout.
        frames.extend(battle_frames(1, 3, 200.0));
        let mut down = battle_frames(1, 3, 300.0);
        for f in &mut down {
            f.facts.alive = 0;
        }
        frames.extend(down);
        let mut after = frame(400.0);
        after.facts.alive = 0;
        frames.push(after);
        frames.push(frame(500.0));
        // A wild battle caught.
        frames.extend(battle_frames(1, 4, 600.0));
        let mut catch = battle_frames(1, 1, 700.0);
        catch[0].rewards = vec![("catch", 3.0)];
        frames.extend(catch);
        frames.push(frame(800.0));
        // One run away from.
        frames.extend(battle_frames(1, 2, 900.0));
        frames.push(frame(1000.0));
        // One still running at the end.
        frames.extend(battle_frames(1, 2, 1100.0));
        let report = run(&frames);
        let b = &report.battles;
        assert_eq!((b.started, b.ended), (5, 4));
        assert_eq!((b.trainer, b.wild), (1, 4));
        assert_eq!((b.won, b.lost, b.caught, b.other), (1, 1, 1, 1));
        assert!(b.still_running);
        assert_eq!(report.whiteouts, 1);
        assert_eq!(report.funnel.catches, 1);
    }

    #[test]
    fn bag_and_dex_changes_become_ball_and_species_counts() {
        let mut a = frame(0.0);
        a.facts.balls = 5;
        let mut b = frame(1000.0);
        b.facts.balls = 3;
        let mut c = frame(2000.0);
        c.facts.balls = 8;
        c.facts.owned = 12;
        let report = run(&[a, b, c]);
        assert_eq!((report.funnel.balls_spent, report.funnel.balls_gained), (2, 5));
        assert_eq!(report.funnel.species_gained, 2);
    }

    #[test]
    fn ratchet_rollbacks_are_split_by_trigger() {
        let mut a = frame(0.0);
        a.rollback = Some(Rolled { game_over: true });
        let mut b = frame(1.0);
        b.rollback = Some(Rolled { game_over: false });
        let report = run(&[a, b]);
        assert_eq!((report.ratchet.rollbacks, report.ratchet.game_over, report.ratchet.stall), (2, 1, 1));
    }

    /// The Viridian cycle on three tiles for twelve minutes: the watchdog confirms it and so does
    /// the trap hunt.
    #[test]
    fn a_ring_is_caught_by_both_instruments() {
        let names = ["GO NPC", "GO OUT", "NEXT", "GO FRONTIER"];
        let mut frames = Vec::new();
        let mut i = 0usize;
        let mut ms = 0.0;
        while ms < 12.0 * MINUTE_MS {
            let mut f = frame(ms);
            f.location = Some((1, (i % 3) as u32, 3));
            if ((ms / FRAME_MS) as u64).is_multiple_of(180) {
                f.events = vec![ev(names[i % 4], None), ev(names[i % 4], Some("done"))];
                i += 1;
            }
            frames.push(f);
            ms += FRAME_MS;
        }
        let report = run(&frames);
        assert!(report.watchdog.suspected >= 2, "{:?}", report.watchdog);
        assert!(report.watchdog.ladder_events >= 1);
        assert!(report.watchdog.confirmed >= 1);
        assert!(report.watchdog.reasons.get("sequence").copied().unwrap_or(0) > 0);
        assert!(report.hunt.flagged > 0 && report.hunt.windows > 0, "{:?}", report.hunt);
        // The run explored nothing: the count never moved.
        assert_eq!(report.places_end, report.places_start);
    }

    /// The same cadence of macros on a fly that keeps finding new tiles is not a trap.
    #[test]
    fn a_walk_that_finds_places_is_not_flagged() {
        let names = ["GO NPC", "GO OUT", "NEXT", "GO FRONTIER"];
        let mut frames = Vec::new();
        let mut i = 0usize;
        let mut ms = 0.0;
        while ms < 12.0 * MINUTE_MS {
            let mut f = frame(ms);
            f.location = Some((1, (ms / 1000.0) as u32, 3));
            f.places = 100 + (ms / 10_000.0) as u32;
            if ((ms / FRAME_MS) as u64).is_multiple_of(180) {
                f.events = vec![ev(names[i % 4], None), ev(names[i % 4], Some("done"))];
                i += 1;
            }
            frames.push(f);
            ms += FRAME_MS;
        }
        let report = run(&frames);
        assert_eq!(report.watchdog.suspected, 0, "{:?}", report.watchdog);
        assert_eq!(report.hunt.flagged, 0);
        assert!(report.watchdog.probes >= 4);
    }

    #[test]
    fn the_hunt_ignores_a_window_with_no_macro_in_it() {
        let steps: Vec<(f64, u32, u32, u32)> = (0..9000).map(|i| (i as f64 * 20.0, 1, 1, 1)).collect();
        let report = hunt(&steps, &[], 0.0, 180_000.0);
        assert!(report.windows > 0);
        assert_eq!(report.flagged, 0);
        // The same ground with a macro in each window is a stand-still.
        let starts: Vec<(f64, &'static str)> = (0..18).map(|i| (i as f64 * 10_000.0, "NEXT")).collect();
        assert!(hunt(&steps, &starts, 0.0, 180_000.0).flagged > 0);
    }

    #[test]
    fn the_longest_cycle_finds_the_shortest_block_and_its_repeats() {
        let names = ["A", "B", "A", "B", "A", "B", "C"];
        assert_eq!(longest_cycle(&names), Some(("A, B".to_string(), 3)));
        assert_eq!(longest_cycle(&[]), None);
    }
}
