#!/usr/bin/env python3
"""Synthetic live-fly RTF traces for fly-shadow-run's guard (SHADOW-01 review round 2, G1).

    shadow_guard_traces.py OUTDIR

Writes <case>/baseline.jsonl and <case>/checks.jsonl in fly-shadow-run's sample format
({t, status, rtMean, lag, uptime}) and prints one line per case: `<case> <expect>`, where
<expect> is `pass` (the guard must not trip) or `trip` (it must). The baseline is 60 samples 10 s
apart (start's 10 minutes); the checks are one sample a minute for the given length.

The lag follows flysim's pacer: a fly behind real time owes (1 - rtf) seconds per second, and the
pacer books it as lag only in chunks of more than a second (`pacing::MAX_CATCHUP`).

The cases come from the release container's own numbers (the coordinator's samples, agents.log
2026-09-30 00:08Z): a flat 0.66 before the cpuset rebalance, 0.77-0.99 noisy after it, plus a fly
at real time; each alone (must pass), and each with a genuine degradation after the start (must
trip).
"""
import json
import os
import random
import sys


class Fly:
    def __init__(self, seed):
        self.rng = random.Random(seed)
        self.t, self.uptime, self.lag, self.debt = 1_790_000_000.0, 3600.0, 12.0, 0.0

    def run(self, seconds, rtf_of):
        """Advances `seconds` of wall time at the realtime factors `rtf_of()` gives per second."""
        rtfs = []
        for _ in range(int(seconds)):
            rtf = rtf_of(self.rng)
            rtfs.append(rtf)
            self.debt += max(0.0, 1.0 - rtf)
            if self.debt > 1.0:
                self.lag += self.debt
                self.debt = 0.0
            self.t += 1
            self.uptime += 1
        return rtfs

    def sample(self, rtf_of, status="running"):
        rtfs = self.run(10, rtf_of)
        return {"t": self.t, "status": status, "rtMean": sum(rtfs) / len(rtfs),
                "lag": round(self.lag, 3), "uptime": self.uptime}

    def restart(self):
        self.uptime, self.lag, self.debt = 5.0, 0.0, 0.0


def flat(v, jitter=0.01):
    return lambda rng: v + rng.uniform(-jitter, jitter)


def noisy(lo, hi):
    return lambda rng: rng.uniform(lo, hi)


def noisy_windows(lo, hi):
    """Noise at the sample level: each 10 s holds one value in [lo, hi], so the 10-s means the
    guard reads spread over the whole range (the coordinator's samples)."""
    state = {"n": 0, "v": None}

    def pace(rng):
        if state["n"] % 10 == 0:
            state["v"] = rng.uniform(lo, hi)
        state["n"] += 1
        return state["v"]

    return pace


def realtime(rng):
    return min(1.0, 1.0 + rng.uniform(-0.004, 0.002))


def case(seed, before, after, checks=180, degrade_at=None, events=None):
    fly = Fly(seed)
    baseline = [fly.sample(before) for _ in range(60)]
    fly.restart()  # start's own flysim restart
    rows = []
    for n in range(checks):
        pace = after if degrade_at is not None and n >= degrade_at else before
        status = "running"
        if events and n in events:
            kind = events[n]
            if kind == "restart":
                fly.restart()
            elif kind == "paused":
                status = "paused"
            elif kind == "hiccup":
                fly.lag += 1.3  # one pacing shortfall of 1.3 s, nothing more
        rows.append(fly.sample(pace, status))
        fly.run(50, pace)
    return baseline, rows


CASES = {
    # The release CT before the rebalance: a flat 0.66, 3 hours, with a restart and a pause.
    "ct-flat-0.66": (case(1, flat(0.66), None, events={40: "restart", 90: "paused", 91: "paused"}), "pass"),
    # After the rebalance: 0.77-0.99 noisy, 3 hours, a restart.
    "ct-noisy-0.77-0.99": (case(2, noisy(0.77, 0.99), None, events={120: "restart"}), "pass"),
    "ct-noisy-0.77-0.99-seed3": (case(3, noisy(0.77, 0.99), None), "pass"),
    # The same spread at the level of the 10-s samples themselves, the harder case.
    "ct-samples-0.77-0.99": (case(8, noisy_windows(0.77, 0.99), None, events={60: "restart"}), "pass"),
    "ct-samples-0.77-0.99-seed9": (case(9, noisy_windows(0.77, 0.99), None), "pass"),
    "ct-samples-0.77-0.99-seed10": (case(10, noisy_windows(0.77, 0.99), None), "pass"),
    # At real time, with single pacing hiccups: never a trip.
    "realtime-hiccups": (case(4, realtime, None, events={30: "hiccup", 100: "hiccup", 150: "hiccup"}), "pass"),
    # A shadow that costs the fly real time.
    "ct-flat-0.66-then-0.55": (case(5, flat(0.66), flat(0.55), checks=40, degrade_at=15), "trip"),
    "ct-noisy-then-0.60-0.75": (case(6, noisy(0.77, 0.99), noisy(0.60, 0.75), checks=40, degrade_at=15), "trip"),
    "realtime-then-0.90": (case(7, realtime, flat(0.90), checks=40, degrade_at=15), "trip"),
    "ct-samples-then-0.55-0.75": (
        case(11, noisy_windows(0.77, 0.99), noisy_windows(0.55, 0.75), checks=40, degrade_at=15),
        "trip",
    ),
}


def main():
    out = sys.argv[1]
    for name, ((baseline, checks), expect) in CASES.items():
        d = os.path.join(out, name)
        os.makedirs(d, exist_ok=True)
        for fname, rows in (("baseline.jsonl", baseline), ("checks.jsonl", checks)):
            with open(os.path.join(d, fname), "w") as f:
                for r in rows:
                    f.write(json.dumps(r) + "\n")
        print(name, expect)


if __name__ == "__main__":
    main()
