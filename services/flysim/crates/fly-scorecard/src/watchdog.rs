//! `fly-watchdog` check 10 (loop suspected), as a function of a run's events.
//!
//! The live check reads the event log's tail, the exploration count from `/status.json` and its
//! own state files; this reads the same three things from a run. The rules, their order and every
//! default are the shell's (`infra/bin/fly-watchdog`, `loop_analyze` and `check_loop`), so a run
//! that would have raised the flag on the stream raises it here. Two differences, both about time:
//!
//! - the probe cadence is a parameter (the stream's is every five wall minutes; a run is short,
//!   so the scorecard probes more often), and a window is the last `window_ms` of brain time or
//!   the whole run when it is shorter;
//! - the exploration count the first probe compares with is the run's starting count, which the
//!   live check does not have for its first probe after a boot.
//!
//! Like the shell, this reads and reports. It never acts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The thresholds of `fly-watchdog`'s `WD_LOOP_*` variables.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    pub window_ms: f64,
    pub max_period: usize,
    pub max_distinct: usize,
    pub min_repeats: usize,
    pub dominance_pct: usize,
    pub min_events: usize,
    pub stall_pct: usize,
    pub busy_min: usize,
    pub fight_min: usize,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            window_ms: 600_000.0,
            max_period: 8,
            max_distinct: 4,
            min_repeats: 20,
            dominance_pct: 95,
            min_events: 20,
            stall_pct: 90,
            busy_min: 100,
            fight_min: 24,
        }
    }
}

/// One line of the event log the check reads: a macro event, a reward or a rung.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    /// `<NAME> start` is `outcome: None`; a finish carries its word (done, blocked, timeout,
    /// refused).
    Macro { ms: f64, name: &'static str, outcome: Option<&'static str> },
    /// A reward, with the feed's kind (`wildwin`, `trainer`, `badge`, `pokedex`, `area`, ...), or
    /// an empty string for an adapter kind the feed has no counter for.
    Reward { ms: f64, kind: &'static str },
    Milestone { ms: f64 },
}

/// `loop_analyze`'s result for one window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub starts: usize,
    pub decisions: usize,
    pub distinct: usize,
    pub top_name: String,
    pub top_count: usize,
    pub period: usize,
    pub repeats: usize,
    pub refused: usize,
    pub blocked: usize,
    pub timeout: usize,
    pub done: usize,
    pub rewards: usize,
    pub fights: usize,
    pub wins: usize,
    pub progress: usize,
    pub window_from: f64,
    pub window_to: f64,
}

fn is_lasting(kind: &str) -> bool {
    matches!(kind, "milestone" | "pokedex" | "area" | "trainer" | "badge")
}

/// `loop_analyze`: the window ends at the newest macro event.
pub fn analyze(events: &[Ev], rules: &Rules) -> Analysis {
    let last_macro = events.iter().rev().find_map(|event| match event {
        Ev::Macro { ms, .. } => Some(*ms),
        _ => None,
    });
    let Some(to) = last_macro else { return Analysis::default() };
    let from = to - rules.window_ms;
    let mut a = Analysis { window_from: from, window_to: to, ..Analysis::default() };
    for event in events {
        let (ms, kind) = match event {
            Ev::Reward { ms, kind } => (*ms, *kind),
            Ev::Milestone { ms } => (*ms, "milestone"),
            Ev::Macro { .. } => continue,
        };
        if ms < from {
            continue;
        }
        if is_lasting(kind) {
            a.progress += 1;
        }
        if kind == "milestone" {
            continue;
        }
        a.rewards += 1;
        if matches!(kind, "wildwin" | "trainer" | "badge") {
            a.wins += 1;
        }
    }
    let mut seq: Vec<&'static str> = Vec::new();
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for event in events {
        let Ev::Macro { ms, name, outcome } = event else { continue };
        if *ms < from {
            continue;
        }
        match outcome {
            Some("blocked") => {
                a.blocked += 1;
                continue;
            }
            Some("timeout") => {
                a.timeout += 1;
                continue;
            }
            Some("done") => {
                a.done += 1;
                continue;
            }
            Some("refused") => a.refused += 1,
            None => a.starts += 1,
            Some(_) => continue,
        }
        seq.push(name);
        if matches!(*name, "MOVE 1" | "MOVE 2" | "MOVE 3" | "MOVE 4") {
            a.fights += 1;
        }
        let n = counts.entry(name).or_insert(0);
        *n += 1;
        if *n > a.top_count {
            a.top_count = *n;
            a.top_name = (*name).to_owned();
        }
    }
    a.decisions = seq.len();
    a.distinct = counts.len();
    let k = seq.len();
    let mut p = 1;
    while p <= rules.max_period && p * 2 <= k {
        let mut r = 1;
        while (r + 1) * p <= k {
            let same = (0..p).all(|j| seq[k - 1 - j] == seq[k - 1 - r * p - j]);
            if !same {
                break;
            }
            r += 1;
        }
        if r >= 2 {
            a.period = p;
            a.repeats = r;
            break;
        }
        p += 1;
    }
    a
}

/// One probe's verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// Brain minutes into the run.
    pub at_min: f64,
    pub suspected: bool,
    pub reason: Option<String>,
    pub decisions: usize,
    pub distinct: usize,
    pub period: usize,
    pub repeats: usize,
    pub done: usize,
    pub failed: usize,
    pub rewards: usize,
    pub fights: usize,
    pub wins: usize,
    pub places_delta: i64,
}

/// `check_loop`'s memory between probes.
#[derive(Clone, Debug)]
pub struct Watchdog {
    pub rules: Rules,
    prev_places: i64,
    prev_idle: bool,
    prev_busy: bool,
    prev_unwon: bool,
}

impl Watchdog {
    /// `places` is the exploration count the run starts with.
    pub fn new(rules: Rules, places: u32) -> Self {
        Self {
            rules,
            prev_places: i64::from(places),
            prev_idle: false,
            prev_busy: false,
            prev_unwon: false,
        }
    }

    /// One probe over every event so far.
    pub fn probe(&mut self, events: &[Ev], places: u32, at_min: f64) -> Probe {
        let rules = &self.rules;
        let a = analyze(events, rules);
        let delta = i64::from(places) - self.prev_places;
        self.prev_places = i64::from(places);
        // `grown` is 1 unless the count is known not to have grown.
        let grown = delta > 0;
        let idle = a.decisions > 0 && a.done == 0 && !grown;
        let busy = a.decisions >= rules.busy_min && a.rewards == 0 && !grown;
        let unwon = a.fights >= rules.fight_min && a.wins == 0 && !grown;
        let failed = a.refused + a.blocked + a.timeout;
        let mut reason = None;
        if a.decisions > 0 && !grown {
            reason = if a.decisions >= rules.min_events
                && failed * 100 >= a.decisions * rules.stall_pct
            {
                Some("stalled")
            } else if idle && self.prev_idle {
                Some("zero-progress")
            } else if a.distinct <= rules.max_distinct && a.repeats >= rules.min_repeats {
                Some("sequence")
            } else if a.decisions >= rules.min_events
                && a.top_count * 100 >= a.decisions * rules.dominance_pct
            {
                Some("dominant")
            } else if busy && self.prev_busy {
                Some("unrewarded")
            } else if unwon && self.prev_unwon {
                Some("unwon-battles")
            } else {
                None
            };
        }
        self.prev_idle = idle;
        self.prev_busy = busy;
        self.prev_unwon = unwon;
        Probe {
            at_min,
            suspected: reason.is_some(),
            reason: reason.map(str::to_owned),
            decisions: a.decisions,
            distinct: a.distinct,
            period: a.period,
            repeats: a.repeats,
            done: a.done,
            failed,
            rewards: a.rewards,
            fights: a.fights,
            wins: a.wins,
            places_delta: delta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(ms: f64, name: &'static str) -> [Ev; 2] {
        [
            Ev::Macro { ms, name, outcome: None },
            Ev::Macro { ms: ms + 1.0, name, outcome: Some("done") },
        ]
    }

    fn cycle(names: &[&'static str], times: usize) -> Vec<Ev> {
        let mut events = Vec::new();
        let mut ms = 0.0;
        for _ in 0..times {
            for name in names {
                events.extend(start(ms, name));
                ms += 3000.0;
            }
        }
        events
    }

    #[test]
    fn the_viridian_cycle_is_a_sequence_when_nothing_is_explored() {
        // GO NPC, GO OUT, NEXT, GO FRONTIER, every three brain seconds, 25 times.
        let events = cycle(&["GO NPC", "GO OUT", "NEXT", "GO FRONTIER"], 25);
        let a = analyze(&events, &Rules::default());
        assert_eq!((a.period, a.repeats, a.distinct), (4, 25, 4));
        let mut watchdog = Watchdog::new(Rules::default(), 100);
        let probe = watchdog.probe(&events, 100, 5.0);
        assert!(probe.suspected);
        assert_eq!(probe.reason.as_deref(), Some("sequence"));
        // The same sequence on a fly that is still finding places is a long walk, not a loop.
        let mut watchdog = Watchdog::new(Rules::default(), 100);
        assert!(!watchdog.probe(&events, 101, 5.0).suspected);
    }

    #[test]
    fn a_pad_of_one_refusing_button_is_a_stall() {
        let mut events = Vec::new();
        for i in 0..30 {
            events.push(Ev::Macro {
                ms: 1000.0 * i as f64,
                name: "GO ROUTE",
                outcome: Some("refused"),
            });
        }
        let mut watchdog = Watchdog::new(Rules::default(), 7);
        let probe = watchdog.probe(&events, 7, 1.0);
        assert_eq!(probe.reason.as_deref(), Some("stalled"));
        assert_eq!(probe.failed, 30);
    }

    #[test]
    fn unrewarded_needs_two_probes() {
        // Ten distinct names (a period past the cap), all done, no reward, 120 decisions, no new
        // ground: row 58's shape. The first probe remembers it and only the second flags.
        let names = ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"];
        let mut events = Vec::new();
        let mut ms = 0.0;
        for _ in 0..12 {
            for name in names {
                events.extend(start(ms, name));
                ms += 1000.0;
            }
        }
        let mut watchdog = Watchdog::new(Rules::default(), 5);
        let first = watchdog.probe(&events, 5, 3.0);
        assert!(!first.suspected, "{first:?}");
        let second = watchdog.probe(&events, 5, 5.0);
        assert_eq!(second.reason.as_deref(), Some("unrewarded"), "{second:?}");
        // One new place clears it.
        let third = watchdog.probe(&events, 6, 7.0);
        assert!(!third.suspected);
    }

    #[test]
    fn rewards_and_rungs_inside_the_window_are_counted() {
        let mut events = cycle(&["GO FRONTIER", "NEXT"], 3);
        events.push(Ev::Reward { ms: 100.0, kind: "wildwin" });
        events.push(Ev::Reward { ms: 200.0, kind: "" });
        events.push(Ev::Milestone { ms: 300.0 });
        let a = analyze(&events, &Rules::default());
        assert_eq!((a.rewards, a.wins, a.progress), (2, 1, 1));
        // A reward before the window's start is not in it.
        let rules = Rules { window_ms: 1000.0, ..Rules::default() };
        let late = analyze(&events, &rules);
        assert_eq!(late.rewards, 0);
    }

    #[test]
    fn a_window_with_no_macro_is_not_a_loop() {
        let mut watchdog = Watchdog::new(Rules::default(), 5);
        let probe = watchdog.probe(&[Ev::Reward { ms: 5.0, kind: "wildwin" }], 5, 5.0);
        assert!(!probe.suspected);
        assert_eq!(probe.decisions, 0);
    }

    #[test]
    fn the_shortest_period_wins() {
        let events = cycle(&["X", "Y"], 40);
        let a = analyze(&events, &Rules::default());
        assert_eq!((a.period, a.repeats), (2, 40));
    }

    #[test]
    fn fights_without_a_win_flag_on_the_second_probe_only() {
        let mut events = Vec::new();
        let moves = ["MOVE 1", "MOVE 2"];
        let mut ms = 0.0;
        // Alternate MOVE n with a varying filler so no short cycle or dominant name forms.
        let fillers = ["NEXT", "TALK", "YES", "NO", "BACK", "MENU", "CLOSE"];
        for i in 0..30usize {
            events.extend(start(ms, moves[i % 2]));
            ms += 1000.0;
            events.extend(start(ms, fillers[(i * 3 + i / 4) % fillers.len()]));
            ms += 1000.0;
        }
        let rules = Rules { min_repeats: 1000, ..Rules::default() };
        let mut watchdog = Watchdog::new(rules, 9);
        let first = watchdog.probe(&events, 9, 4.0);
        assert!(!first.suspected, "{first:?}");
        let second = watchdog.probe(&events, 9, 6.0);
        assert_eq!(second.reason.as_deref(), Some("unwon-battles"), "{second:?}");
        // A win in the window clears it.
        events.push(Ev::Reward { ms: 100.0, kind: "wildwin" });
        let third = watchdog.probe(&events, 9, 8.0);
        assert!(!third.suspected);
    }
}
