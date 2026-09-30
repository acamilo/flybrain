import importlib.machinery
import importlib.util
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

repo = Path(__file__).resolve().parents[2]
script = repo / "infra/bin/fly-loop-recover"
spec = importlib.util.spec_from_loader("recover", importlib.machinery.SourceFileLoader("recover", str(script)))
recover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recover)
RESTORABLE_RUNGS = recover.restorable_rungs
WRAPPER = repo / "infra/bin/fly-loop-reset"

T0 = 1_790_000_000


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.root = root
        for name, value in (("REPORT", "loop.json"), ("STATE", "var/state.json"),
                            ("HISTORY", "var/history.jsonl"), ("NOTICE", "run/notice.json")):
            patcher = patch.object(recover, name, root / value)
            patcher.start()
            self.addCleanup(patcher.stop)
        milestones = root / "state"
        milestones.mkdir()
        for rung in (1, 9, 10, 11, 12):
            (milestones / f"milestone-{rung}.checkpoint").write_text("x")
        patcher = patch.object(recover, "MILESTONES", milestones)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.env = patch.dict("os.environ", {}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)
        for key in ("FLY_LOOP_ROUTER_URL", "FLY_LOOP_MODELS", "FLY_LOOP_MODEL", "FLY_LOOP_ROUTER_KEY"):
            recover.os.environ.pop(key, None)
        self.acts = []
        self.unrestorable = set()
        restorable = patch.object(recover, "restorable_rungs", side_effect=lambda: {1, 9, 10, 11, 12} - self.unrestorable)
        restorable.start()
        self.addCleanup(restorable.stop)
        self.act = patch.object(recover, "act", side_effect=lambda action, target: self.acts.append((action, target)) or True)
        self.act.start()
        self.addCleanup(self.act.stop)
        self.report(suspected=1, at=T0)

    def report(self, **fields):
        base = {"suspected": 1, "action": "none", "reason": "unrewarded", "sequence": ["GO WARP"],
                "milestone": {"rank": 12, "label": "MT. MOON"}, "map": 61}
        base.update(fields)
        if "rank" in fields:
            base["milestone"] = {"rank": base.pop("rank"), "label": "X"}
        recover.REPORT.write_text(json.dumps(base))

    def tick(self, at, **fields):
        """One watchdog probe at `at` and the timer running right after it."""
        self.report(at=at, **fields)
        return recover.run(at + 5, sleep=lambda seconds: None)

    def confirm(self, start, **fields):
        """Two probes ~5 min apart after `start`; returns the second decision."""
        self.assertIn("second probe", self.tick(start, **fields))
        return self.tick(start + 300, **fields)

    def test_ladder_restart_then_current_rung_then_rung_below(self):
        self.assertIn("flysim restarted", self.confirm(T0))
        self.assertEqual(self.acts, [("restart", None)])
        self.assertIn("settling", self.tick(T0 + 600))
        self.assertIn("reset to rung 12", self.confirm(T0 + 300 + recover.SETTLE + 10))
        start = T0 + 2 * (300 + recover.SETTLE + 10)
        self.assertIn("reset to rung 11", self.confirm(start, rank=12))
        self.assertEqual([a for a in self.acts], [("restart", None), ("reset", 12), ("reset", 11)])

    def test_spent_reset_budget_holds_to_restarts_three_hours_apart(self):
        at = T0
        for _ in range(3):
            self.confirm(at)
            at += 300 + recover.SETTLE + 10
        self.assertEqual(len(self.acts), 3)
        self.assertIn("holding", self.confirm(at))
        # the trap never cleared while holding, so the first probe after the hold acts
        self.assertIn("flysim restarted", self.tick(at + recover.HOLD))
        self.assertEqual(self.acts[-1], ("restart", None))

    def test_deeper_resets_never_go_below_the_rung_under_the_best(self):
        state = {"level": 5, "bestRank": 12, "resets": [], "actedAt": 0}
        recover.write_json(recover.STATE, state)
        self.confirm(T0, rank=11)
        self.assertEqual(self.acts, [("reset", 11)])

    def test_an_unrestorable_rung_below_the_best_means_a_restart_never_a_lower_rung(self):
        self.unrestorable = {11}
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0)
        self.assertEqual(self.acts, [("restart", None)])

    def test_a_fly_already_below_the_rung_under_its_best_is_restarted_not_reset(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0, rank=10)
        self.assertEqual(self.acts, [("restart", None)])

    def test_no_restorable_archive_means_a_restart(self):
        self.unrestorable = {1, 9, 10, 11, 12}
        recover.write_json(recover.STATE, {"level": 1, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0)
        self.assertEqual(self.acts, [("restart", None)])

    def test_new_best_rung_starts_the_ladder_over(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0, rank=13)
        self.assertEqual(self.acts, [("restart", None)])

    # Row 70: the rung-11 reset replayed the run exactly (1,138 of 1,138 events), caught the Zubat
    # again and walked into the same trap; the next step reset to rung 11 again and erased the catch.
    def test_progress_after_the_last_step_starts_the_ladder_over(self):
        recover.write_json(recover.STATE, {"level": 4, "bestRank": 12, "resets": [], "actedAt": T0 - 7200})
        # an area and a species inside the windows of probes well after the step
        self.tick(T0 - 3000, suspected=0, window={"progress": {"lasting": 2, "species": 0}})
        self.assertIn("flysim restarted", self.confirm(T0))
        self.assertEqual(self.acts, [("restart", None)])
        history = [json.loads(line) for line in recover.HISTORY.read_text().splitlines()]
        self.assertIn("progress since the last step", [event.get("why") for event in history])

    def test_progress_while_a_step_settles_does_not_start_the_ladder_over(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": T0 - 1500})
        self.tick(T0 - 1400, suspected=0, window={"progress": {"lasting": 3, "species": 0}})
        self.confirm(T0)
        self.assertEqual(self.acts, [("reset", 11)])

    def test_a_species_owned_recently_is_never_reset_away(self):
        owned = T0 - 1200
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [],
                                           "actedAt": T0 - recover.HOLD - 300, "speciesAt": owned})
        self.assertIn("flysim restarted", self.confirm(T0))
        self.assertEqual(self.acts, [("restart", None)])
        history = [json.loads(line) for line in recover.HISTORY.read_text().splitlines()]
        self.assertIn("protect", [event.get("event") for event in history])
        # the hold still spaces the restarts, and says why
        self.assertIn("protection window", self.confirm(T0 + 300 + recover.SETTLE + 10))
        # the protection lasts PROTECT from the species; then the ladder resets as before
        self.assertIn("reset to rung 11", self.tick(owned + recover.PROTECT + 60))

    def test_a_probe_that_owns_a_species_starts_the_protection(self):
        self.tick(T0, suspected=0, window={"progress": {"lasting": 1, "species": 1}})
        self.assertEqual(recover.load(recover.STATE, {}).get("speciesAt"), T0)
        self.assertEqual(recover.load(recover.STATE, {}).get("progressAt"), T0)

    def test_the_last_resets_archive_is_not_restored_again(self):
        acted = T0 - recover.HOLD - 100
        recover.write_json(recover.STATE, {"level": 3, "bestRank": 12, "resets": [acted],
                                           "actedAt": acted, "lastReset": 11, "lastResetAt": acted})
        self.confirm(T0)
        self.assertEqual(self.acts, [("restart", None)])

    def test_a_protected_reset_does_not_climb_but_the_archive_just_used_does(self):
        # protected: the restart keeps the level's slot, so the reset comes at the same rung later
        owned = T0 - 600
        recover.write_json(recover.STATE, {"level": 1, "bestRank": 12, "resets": [], "actedAt": 0,
                                           "speciesAt": owned})
        self.confirm(T0)
        self.assertEqual(self.acts, [("restart", None)])
        self.assertEqual(recover.load(recover.STATE, {}).get("level"), 1)
        # the archive just restored: a restart that climbs, and the marker is forgotten after AGAIN
        self.acts.clear()
        reset_at = T0 - recover.HOLD - 100
        recover.write_json(recover.STATE, {"level": 1, "bestRank": 12, "resets": [reset_at], "actedAt": reset_at,
                                           "lastReset": 12, "lastResetAt": reset_at})
        self.confirm(T0 + 1000)
        self.assertEqual(self.acts, [("restart", None)])
        self.assertEqual(recover.load(recover.STATE, {}).get("level"), 2)
        self.acts.clear()
        self.tick(reset_at + recover.AGAIN + 60)
        self.assertNotIn("lastReset", recover.load(recover.STATE, {}))

    def test_a_reset_records_its_archive_and_a_new_best_rung_forgets_it(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0)
        self.assertEqual(recover.load(recover.STATE, {}).get("lastReset"), 11)
        self.tick(T0 + 300 + recover.SETTLE + 10, suspected=0, rank=13)
        self.assertNotIn("lastReset", recover.load(recover.STATE, {}))

    # Row 70 review r3: a reset restores the reward ledger, so its replay pays the archive's rewards
    # again. Progress is a reward name the ladder has not seen before, not a count.
    def test_a_replay_that_re_earns_seen_rewards_does_not_start_the_ladder_over(self):
        seen = ["area:AREA 59", "pokedex:OWNED #8", "trainer:BEAT ROUTE 3 TRAINER 0", "milestone:Reached MT. MOON"]
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": T0 - 7200,
                                           "progressSeen": seen})
        replay = {"progress": {"lasting": 4, "species": 1, "keys": seen}}
        self.tick(T0 - 3000, suspected=0, window=replay)
        state = recover.load(recover.STATE, {})
        self.assertNotIn("progressAt", state)
        self.assertNotIn("speciesAt", state)  # the replayed catch was protected before the reset
        self.assertEqual(state["progressSeen"], seen)
        self.confirm(T0)
        self.assertEqual(self.acts, [("reset", 11)])

    def test_a_reward_not_seen_before_starts_the_ladder_over_and_a_new_species_is_protected(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": T0 - 7200,
                                           "progressSeen": ["area:AREA 59"]})
        self.tick(T0 - 3000, suspected=0,
                  window={"progress": {"lasting": 2, "species": 1, "keys": ["area:AREA 59", "pokedex:OWNED #41"]}})
        state = recover.load(recover.STATE, {})
        self.assertEqual((state["progressAt"], state["speciesAt"]), (T0 - 3000, T0 - 3000))
        self.assertEqual(state["progressSeen"], ["area:AREA 59", "pokedex:OWNED #41"])
        self.assertIn("flysim restarted", self.confirm(T0))
        history = [json.loads(line) for line in recover.HISTORY.read_text().splitlines()]
        self.assertIn("progress since the last step", [event.get("why") for event in history])

    def test_a_catch_while_the_fly_stays_flagged_does_not_start_the_ladder_over(self):
        acted = T0 - recover.HOLD - 600
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": acted,
                                           "clearAt": acted + 60})
        catch = {"progress": {"lasting": 1, "species": 1, "keys": ["pokedex:OWNED #41"]}}
        self.assertIn("flysim restarted", self.confirm(T0, window=catch))
        self.assertEqual(self.acts, [("restart", None)])  # the rung-11 reset, held for the new species
        self.assertEqual(recover.load(recover.STATE, {}).get("level"), 2)

    def test_without_reward_names_the_counts_still_count(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": T0 - 7200,
                                           "progressSeen": ["pokedex:OWNED #8"]})
        self.tick(T0 - 3000, suspected=0, window={"progress": {"lasting": 1, "species": 1}})
        state = recover.load(recover.STATE, {})
        self.assertEqual((state["progressAt"], state["speciesAt"]), (T0 - 3000, T0 - 3000))

    def test_the_names_remembered_are_bounded(self):
        recover.write_json(recover.STATE, {"progressSeen": [f"area:AREA {n}" for n in range(recover.SEEN)]})
        self.tick(T0, suspected=0, window={"progress": {"lasting": 1, "species": 0, "keys": ["area:AREA X"]}})
        seen = recover.load(recover.STATE, {})["progressSeen"]
        self.assertEqual((len(seen), seen[0], seen[-1]), (recover.SEEN, "area:AREA 1", "area:AREA X"))
        # a malformed list is ignored item by item
        self.tick(T0 + 300, suspected=0, window={"progress": {"lasting": 1, "species": 0, "keys": [{}, ["x"], 3]}})
        self.assertEqual(recover.load(recover.STATE, {})["progressSeen"][-1], "area:AREA X")

    def test_restarts_stay_a_hold_apart_after_the_ladder_starts_over(self):
        recover.write_json(recover.STATE, {"level": 1, "bestRank": 12, "resets": [], "actedAt": T0 - 3600,
                                           "lastAction": "restart"})
        self.tick(T0 - 2000, suspected=0, window={"progress": {"lasting": 1, "species": 0, "keys": ["area:AREA 60"]}})
        self.assertIn("last restart was under three hours ago", self.confirm(T0))
        self.assertEqual(self.acts, [])
        self.assertIn("flysim restarted", self.tick(T0 - 3600 + recover.HOLD))

    def test_a_failed_reset_does_not_block_its_archive(self):
        recover.act.side_effect = lambda action, target: self.acts.append((action, target)) or False
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.assertIn("reset FAILED", self.confirm(T0))
        state = recover.load(recover.STATE, {})
        self.assertNotIn("lastReset", state)
        self.assertEqual((state["level"], len(state["resets"])), (3, 1))  # it still climbed and spent the reset

    def test_quiet_hours_start_the_ladder_over(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "actedAt": 0,
                                           "lastSuspectedAt": T0 - recover.QUIET - 1})
        self.confirm(T0)
        self.assertEqual(self.acts, [("restart", None)])

    def test_stale_clear_or_acted_reports_never_act(self):
        self.report(at=T0 - 700)
        self.assertIn("not a fresh", recover.run(T0))
        self.report(suspected=0, at=T0)
        self.assertIn("not a fresh", recover.run(T0 + 1))
        self.report(at=T0, action="restart")
        self.assertIn("not a fresh", recover.run(T0 + 2))
        self.assertEqual(self.acts, [])

    def test_a_clear_probe_breaks_the_streak(self):
        self.tick(T0)
        self.tick(T0 + 300, suspected=0)
        self.assertIn("second probe", self.tick(T0 + 600))
        self.assertEqual(self.acts, [])

    def test_state_survives_a_reboot_and_ignores_a_missing_or_corrupt_file(self):
        self.confirm(T0)
        self.assertTrue(recover.STATE.exists())
        self.assertEqual(json.loads(recover.STATE.read_text())["level"], 1)
        recover.STATE.write_text("{nope")
        self.assertIn("second probe", self.tick(T0 + 5000))

    def test_model_vetoes_delay_a_step_but_never_deny_it(self):
        with patch.object(recover, "verdict", return_value=(False, "m")):
            self.tick(T0)
            for n in range(1, recover.VETO_LIMIT + 1):
                self.assertIn(f"({n}/{recover.VETO_LIMIT})", self.tick(T0 + 300 * n))
            self.assertIn("veto limit reached", self.tick(T0 + 300 * (recover.VETO_LIMIT + 1)))
        self.assertEqual(self.acts, [("restart", None)])

    def test_no_model_answer_falls_back_to_the_watchdog(self):
        with patch.object(recover, "verdict", return_value=(None, None)):
            self.assertIn("watchdog alone", self.confirm(T0))

    def test_models_are_tried_in_order_until_one_answers(self):
        recover.os.environ.update(FLY_LOOP_ROUTER_URL="http://router/v1", FLY_LOOP_MODELS="a, b ,c")
        replies = iter([OSError("429"), {"choices": [{"message": {"content": "thinking...\n```json\n{\"stuck\": true}\n```"}}]}])
        asked = []

        class Reply:
            def __init__(self, body):
                self.body = body

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return json.dumps(self.body).encode()

        def fake(request, timeout):
            asked.append(json.loads(request.data)["model"])
            reply = next(replies)
            if isinstance(reply, Exception):
                raise reply
            return Reply(reply)

        with patch.object(recover, "urlopen", side_effect=fake):
            self.assertEqual(recover.verdict({"reason": "unrewarded"}), (True, "b"))
        self.assertEqual(asked, ["a", "b"])

    def test_parse_stuck_tolerates_prose_and_rejects_non_booleans(self):
        self.assertIs(recover.parse_stuck('{"stuck":false}'), False)
        self.assertIs(recover.parse_stuck('Answer: {"stuck": true} done'), True)
        self.assertIsNone(recover.parse_stuck('{"stuck": "yes"}'))
        self.assertIsNone(recover.parse_stuck(""))
        self.assertIsNone(recover.parse_stuck(None))

    def test_notice_goes_countdown_acting_done_for_the_splash(self):
        phases = []
        real = recover.notice
        with patch.object(recover, "notice", side_effect=lambda base, phase, now: phases.append(phase) or real(base, phase, now)):
            self.confirm(T0 + 300 + recover.SETTLE)
            recover.write_json(recover.STATE, dict(json.loads(recover.STATE.read_text()), actedAt=0))
            self.confirm(T0 + 2 * (300 + recover.SETTLE))
        notice = json.loads(recover.NOTICE.read_text())
        self.assertEqual(phases, ["countdown", "acting", "done"] * 2)
        self.assertEqual((notice["v"], notice["action"], notice["toRung"], notice["toLabel"], notice["fromRung"]),
                         (1, "reset", 12, "MT. MOON", 12))
        self.assertEqual(notice["executeAt"] - notice["announcedAt"], recover.COUNTDOWN)

    def test_failed_action_is_reported_and_still_climbs(self):
        self.act.stop()
        with patch.object(recover, "act", return_value=False):
            self.assertIn("FAILED", self.confirm(T0))
        self.act.start()
        self.assertEqual(json.loads(recover.NOTICE.read_text())["phase"], "failed")
        self.assertEqual(json.loads(recover.STATE.read_text())["level"], 1)

    def test_a_step_that_raises_or_times_out_is_a_failed_step_with_state_saved(self):
        self.act.stop()
        with patch.object(recover.subprocess, "run", side_effect=recover.subprocess.TimeoutExpired("sudo", 1)):
            self.assertIn("FAILED", self.confirm(T0))
        self.act.start()
        self.assertEqual(json.loads(recover.NOTICE.read_text())["phase"], "failed")
        self.assertEqual(json.loads(recover.STATE.read_text())["level"], 1)

    def test_state_is_saved_before_the_step_runs(self):
        seen = []
        self.act.stop()
        with patch.object(recover, "act", side_effect=lambda a, t: seen.append(json.loads(recover.STATE.read_text())) or True):
            self.confirm(T0)
        self.act.start()
        self.assertEqual(seen[0]["level"], 1)
        self.assertNotIn("observedAt", seen[0])

    def test_restorable_rungs_parses_the_list_and_is_empty_on_any_failure(self):
        ok = recover.subprocess.CompletedProcess([], 0, stdout="11\n12\n", stderr="")
        with patch.object(recover.subprocess, "run", return_value=ok):
            self.assertEqual(RESTORABLE_RUNGS(), {11, 12})
        with patch.object(recover.subprocess, "run", side_effect=OSError("no sudo")):
            self.assertEqual(RESTORABLE_RUNGS(), set())
        with patch.object(recover.subprocess, "run", return_value=recover.subprocess.CompletedProcess([], 1, "12", "")):
            self.assertEqual(RESTORABLE_RUNGS(), set())

    def test_stuck_time_counts_from_the_last_progress_across_restarts(self):
        self.report(at=T0 - 7200, suspected=0, places={"uniqueLocations": 10, "delta": 4})
        recover.run(T0 - 7195)
        recover.write_json(recover.STATE, dict(json.loads(recover.STATE.read_text()), vetoes=0))
        self.confirm(T0)
        self.assertEqual(json.loads(recover.NOTICE.read_text())["stuckSeconds"], 7505)
        self.assertEqual(json.loads(recover.STATE.read_text())["lastProgressAt"], T0 - 7200)

    def test_stuck_time_falls_back_to_the_streak_without_a_progress_mark(self):
        self.confirm(T0)
        self.assertEqual(json.loads(recover.NOTICE.read_text())["stuckSeconds"], 305 + 600)

    def test_history_records_every_step(self):
        self.confirm(T0)
        lines = [json.loads(line) for line in recover.HISTORY.read_text().splitlines()]
        self.assertEqual(lines[-1]["event"], "restart")
        self.assertTrue(lines[-1]["ok"])

    def test_ladder_labels_match_the_rust_table(self):
        source = (repo / "services/flysim/crates/flybrain-gb/src/pokemon_red/mod.rs").read_text()
        table = re.search(r"RANK_LADDER: \[&str; \d+\] = \[(.*?)\];", source, re.S).group(1)
        self.assertEqual(recover.LADDER, re.findall(r'"([^"]*)"', table))


BUILD = "lif-1ms-f64-v2/pokered-unique8-v7/abc/fly-kc-mbon-rstdp-v2"


@unittest.skipIf(recover.os.geteuid() == 0, "the wrapper ignores test overrides as root")
class ClosedLoopTests(unittest.TestCase):
    """Row 70 review L1: a trap no restart clears, every reset replaying it exactly (closed loop)."""
    M = 60
    R11 = dict(events=[(20, "pokedex"), (21, "trainer"), (35, "trainer"), (44, "area"), (46, "milestone"),
                       (52, "pokedex")], trap=58, rank_after=[(46, 12)], start_rank=11)
    R12 = dict(events=[(7, "pokedex")], trap=12, rank_after=[], start_rank=12)
    R10 = dict(events=[(30, "area"), (60, "milestone"), (90, "milestone"), (100, "pokedex")], trap=110,
               rank_after=[(60, 11), (90, 12)], start_rank=10)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        for name, value in (("REPORT", "loop.json"), ("STATE", "s.json"), ("HISTORY", "h.jsonl"),
                            ("NOTICE", "n.json")):
            patcher = patch.object(recover, name, root / value)
            patcher.start()
            self.addCleanup(patcher.stop)
        for name, value in (("restorable_rungs", lambda: {9, 10, 11, 12}), ("verdict", lambda report: (None, None))):
            patcher = patch.object(recover, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.env = patch.dict("os.environ", {}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)
        for key in ("FLY_LOOP_ROUTER_URL", "FLY_LOOP_MODELS", "FLY_LOOP_MODEL", "FLY_LOOP_ROUTER_KEY"):
            recover.os.environ.pop(key, None)

    def simulate(self, archives, hours, species_before=None, names=True):
        """The fly is trapped from T0; returns [(hours, action, target)]. A reset replays its archive.

        Every lasting reward has a name (window.progress.keys): the fly earned each archive's rewards
        once before the trap, in clear probes the ladder saw, and a replay pays the same names again.
        """
        M = self.M
        sim = dict(events=[(T0 - species_before * M, "pokedex", "pokedex:pre")] if species_before else [],
                   rank=12, rank_changes=[], trap_from=T0, now=T0, acts=[])
        history = [f"{k}:{rung}/{o}" for rung, a in sorted(archives.items()) for o, k in a["events"]]
        for i in range(0, len(history), 3) if names else ():
            recover.REPORT.write_text(json.dumps({
                "at": T0 - 86400 + i * 300, "suspected": 0, "action": "none", "milestone": {"rank": 12},
                "window": {"progress": {"lasting": 3, "species": 0, "keys": history[i:i + 3]}}}))
            recover.run(now=T0 - 86400 + i * 300 + 5, sleep=lambda s: None)
        seen = recover.load(recover.STATE, {}).get("progressSeen")

        def act(action, target):
            sim["acts"].append((sim["now"], action, target))
            if action == "reset":
                a = archives[target]
                sim["rank"] = a["start_rank"]
                sim["events"] = [(sim["now"] + o * M, k, f"{k}:{target}/{o}") for o, k in a["events"]]
                sim["rank_changes"] = [(sim["now"] + o * M, r) for o, r in a["rank_after"]]
                sim["trap_from"] = None if a["trap"] is None else sim["now"] + a["trap"] * M
            return True

        recover.write_json(recover.STATE, dict({"bestRank": 12, "rank": 12, "level": 0},
                                               **({"progressSeen": seen} if seen else {})))
        recover.HISTORY.write_text("")
        with patch.object(recover, "act", side_effect=act):
            t = T0
            while t < T0 + hours * 3600:
                for change in list(sim["rank_changes"]):
                    if change[0] <= t:
                        sim["rank"] = change[1]
                        sim["rank_changes"].remove(change)
                win = [(k, name) for at, k, name in sim["events"] if t - 600 <= at <= t]
                trapped = sim["trap_from"] is not None and t - sim["trap_from"] >= 600
                recover.REPORT.write_text(json.dumps({
                    "at": t, "suspected": 1 if trapped else 0, "action": "none", "reason": "dominant",
                    "sequence": ["NEXT"], "milestone": {"rank": sim["rank"], "label": "X"}, "map": 59,
                    "window": {"progress": dict({
                        "lasting": sum(k in ("pokedex", "area", "trainer", "badge", "milestone") for k, _ in win),
                        "species": sum(k == "pokedex" for k, _ in win)},
                        **({"keys": [name for _, name in win]} if names else {}))}}))
                sim["now"] = t + 5
                recover.run(now=t + 5, sleep=lambda s: None)
                t += 300
        return [((at - T0) / 3600, action, target) for at, action, target in sim["acts"]]

    def test_a_restart_proof_trap_is_reset_in_bounded_time_and_again(self):
        acts = self.simulate({11: self.R11, 12: self.R12, 10: self.R10, 9: self.R10}, 72, species_before=6)
        resets = [a for a in acts if a[1] == "reset"]
        # v0.7.0 resets again at 24.75 h; the ladder is no worse, and keeps resetting
        self.assertTrue(resets and resets[0][0] <= 25, acts)
        self.assertTrue(len([a for a in resets if a[0] <= 25]) >= 1, acts)
        self.assertTrue(len(resets) >= 3, acts)
        gaps = [b[0] - a[0] for a, b in zip(resets, resets[1:])]
        self.assertTrue(all(g <= 25 for g in gaps), acts)
        # the first step is a restart; the catch stays protected for 2 h
        self.assertEqual(acts[0][1], "restart")
        self.assertTrue(all(a[0] >= 2 - 0.2 for a in resets[:1]), acts)

    def test_the_held_reset_is_the_rung_12_one_not_a_deeper_rung(self):
        acts = self.simulate({11: self.R11, 12: self.R12, 10: self.R10, 9: self.R10}, 24, species_before=6)
        self.assertEqual([a[2] for a in acts if a[1] == "reset"][:1], [12], acts)

    def test_a_trap_the_rung_12_archive_escapes_ends_there(self):
        clean = dict(self.R12, trap=None)
        acts = self.simulate({11: self.R11, 12: clean, 10: self.R10, 9: self.R10}, 24, species_before=6)
        self.assertEqual([a[1:] for a in acts if a[1] == "reset"], [("reset", 12)], acts)
        self.assertEqual(len(acts), 2, acts)

    def test_without_a_catch_the_ladder_resets_early(self):
        acts = self.simulate({11: self.R11, 12: self.R12, 10: self.R10, 9: self.R10}, 24)
        self.assertLessEqual(next(a[0] for a in acts if a[1] == "reset"), 1, acts)

    def test_a_trap_only_the_rung_below_escapes_is_reached_promptly(self):
        # review r2 N1: the rung-12 replay re-earns progress, then traps; only rung 11 escapes.
        # v0.7.0 reset to 11 at 2.08 h; holding the level on the archive just used took 24.75 h, and
        # counting the replay's area and species as progress took 5.58 h (6.83 h after a catch).
        late12 = dict(events=[(30, "area"), (40, "pokedex")], trap=60, rank_after=[], start_rank=12)
        archives = {11: dict(self.R11, trap=None), 12: late12, 10: self.R10, 9: self.R10}
        for species_before, bound in ((None, 2.1), (6, 2.1 + 2)):  # a catch 6 min before the trap: +2 h at most
            with self.subTest(species_before=species_before):
                self.setUp()
                acts = self.simulate(archives, 96, species_before=species_before)
                resets = [a for a in acts if a[1] == "reset"]
                self.assertEqual([a[2] for a in resets], [12, 11], acts)
                self.assertLessEqual(resets[-1][0], bound, acts)

    def test_restarts_are_a_hold_apart_in_every_closed_loop(self):
        late12 = dict(events=[(30, "area"), (40, "pokedex")], trap=60, rank_after=[], start_rank=12)
        for archives in ({11: self.R11, 12: self.R12, 10: self.R10, 9: self.R10},
                         {11: self.R11, 12: late12, 10: self.R10, 9: self.R10}):
            for species_before in (None, 6):
                self.setUp()
                acts = self.simulate(archives, 72, species_before=species_before)
                restarts = [a[0] for a in acts if a[1] == "restart"]
                self.assertTrue(all(b - a >= 3 - 1e-6 for a, b in zip(restarts, restarts[1:])), acts)
                resets = [a[0] for a in acts if a[1] == "reset"]
                self.assertTrue(all(sum(1 for y in resets if x <= y < x + 24) <= 2 for x in resets), acts)


class WrapperTests(unittest.TestCase):
    """infra/bin/fly-loop-reset against a fake flysim, systemctl, reset tool and archives."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.root = root
        self.calls = root / "calls.log"
        (root / "state").mkdir()
        (root / "bin").mkdir()
        # The watchdog probe is mid-run ("activating") for the first two looks.
        self.script(root / "bin/systemctl", f'''echo "systemctl $*" >> {self.calls}
if [ "$1" = show ]; then
  n=$(grep -c "^systemctl show" {self.calls})
  if [ "$n" -le 2 ]; then echo activating; else echo inactive; fi
fi''')
        self.script(root / "flysim", f'[ "$1" = --print-compatibility ] && echo "{BUILD}"')
        self.script(root / "reset", f'echo "reset $* FLY_BIN=$FLY_BIN" >> {self.calls}')
        self.env_file = root / "fly.env"
        self.env_file.write_text('FLY_MACRO_MODE=macros\nGAME_TITLE=Pokemon (Red) $(touch pwned)\n')
        self.archive(12, BUILD)
        self.archive(11, BUILD.replace("-v7", "-v6"))
        self.archive(1, BUILD.replace("-v7", "-v5"))

    def script(self, path, body):
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def archive(self, rung, compat):
        (self.root / f"state/milestone-{rung}.checkpoint").write_bytes(
            b"FLYSIM01" + json.dumps({"generation": 1, "compatibility": compat}).encode() + b"\x00" * 64)

    def run_wrapper(self, *args):
        env = {"PATH": f"{self.root}/bin:/usr/bin:/bin", "FLY_LOOP_RESET_TEST_STATE_DIR": str(self.root / "state"),
               "FLY_LOOP_RESET_TEST_ENV_FILE": str(self.env_file), "FLY_LOOP_RESET_TEST_FLYSIM": str(self.root / "flysim"),
               "FLY_LOOP_RESET_TEST_RESET_BIN": str(self.root / "reset")}
        env.update(getattr(self, "extra_env", {}))
        return recover.subprocess.run([str(WRAPPER), *args], env=env, capture_output=True, text=True, timeout=60)

    def log(self):
        return self.calls.read_text() if self.calls.exists() else ""

    def test_list_names_only_rungs_this_build_restores(self):
        self.assertEqual(self.run_wrapper("--list").stdout.split(), ["12"])
        self.env_file.write_text("FLY_ACCEPT_ADAPTERS=pokered-unique8-v6\n")
        self.assertEqual(self.run_wrapper("--list").stdout.split(), ["11", "12"])
        self.assertFalse((self.root / "pwned").exists())

    def test_reset_pauses_the_watchdog_and_always_starts_flysim_again(self):
        done = self.run_wrapper("12")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual([line.split()[:3] for line in self.log().splitlines() if not line.startswith("systemctl is-active")], [
            ["systemctl", "stop", "fly-watchdog.timer"],
            ["systemctl", "show", "-p"], ["systemctl", "show", "-p"], ["systemctl", "show", "-p"], ["systemctl", "show", "-p"],
            ["systemctl", "stop", "flysim.service"],
            ["reset", "12", f"FLY_BIN={self.root}/flysim"],
            ["systemctl", "start", "flysim.service"], ["systemctl", "start", "fly-watchdog.timer"]])

    def test_a_failed_reset_still_starts_flysim_and_the_watchdog(self):
        self.script(self.root / "reset", f'echo "reset $*" >> {self.calls}; exit 7')
        self.assertEqual(self.run_wrapper("12").returncode, 7)
        self.assertIn("systemctl start flysim.service", self.log())
        self.assertIn("systemctl start fly-watchdog.timer", self.log())

    def test_an_unrestorable_or_missing_rung_touches_nothing(self):
        for rung in ("11", "5"):
            self.assertEqual(self.run_wrapper(rung).returncode, 3)
        self.assertEqual(self.log(), "")

    def test_no_build_compatibility_means_nothing_is_restorable(self):
        (self.root / "flysim").unlink()
        self.assertEqual(self.run_wrapper("--list").stdout, "")
        self.assertEqual(self.run_wrapper("12").returncode, 3)
        self.assertEqual(self.log(), "")

    def test_a_root_copy_that_is_not_the_running_build_restores_nothing(self):
        other = self.root / "live-flysim"
        self.script(other, "echo other build")
        self.extra_env = {"FLY_LOOP_RESET_TEST_LIVE_FLYSIM": str(other)}
        self.assertEqual(self.run_wrapper("--list").stdout, "")
        self.assertEqual(self.run_wrapper("12").returncode, 3)

    def test_a_watchdog_probe_that_never_finishes_blocks_the_reset(self):
        self.script(self.root / "bin/systemctl", f'echo "systemctl $*" >> {self.calls}; [ "$1" != show ] || echo activating')
        self.script(self.root / "bin/sleep", "exit 0")
        self.assertEqual(self.run_wrapper("12").returncode, 4)
        self.assertNotIn("systemctl stop flysim.service", self.log())
        self.assertIn("systemctl start fly-watchdog.timer", self.log())

    def test_bad_arguments_are_refused(self):
        for args in ((), ("--check", "12"), ("12", "13"), ("../12",), ("123",), ("-1",)):
            self.assertEqual(self.run_wrapper(*args).returncode, 2, args)
        self.assertEqual(self.log(), "")


if __name__ == "__main__":
    unittest.main()
