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

    def test_new_best_rung_starts_the_ladder_over(self):
        recover.write_json(recover.STATE, {"level": 2, "bestRank": 12, "resets": [], "actedAt": 0})
        self.confirm(T0, rank=13)
        self.assertEqual(self.acts, [("restart", None)])

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

    def test_history_records_every_step(self):
        self.confirm(T0)
        lines = [json.loads(line) for line in recover.HISTORY.read_text().splitlines()]
        self.assertEqual(lines[-1]["event"], "restart")
        self.assertTrue(lines[-1]["ok"])

    def test_ladder_labels_match_the_rust_table(self):
        source = (repo / "services/flysim/crates/flybrain-gb/src/pokemon_red/mod.rs").read_text()
        table = re.search(r"RANK_LADDER: \[&str; \d+\] = \[(.*?)\];", source, re.S).group(1)
        self.assertEqual(recover.LADDER, re.findall(r'"([^"]*)"', table))


if __name__ == "__main__":
    unittest.main()
