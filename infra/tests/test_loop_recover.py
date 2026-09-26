import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

script = Path(__file__).resolve().parents[1] / "bin/fly-loop-recover"
spec = importlib.util.spec_from_loader("recover", importlib.machinery.SourceFileLoader("recover", str(script)))
recover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recover)


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        recover.REPORT = root / "loop.json"
        recover.STATE = root / "recovery.json"
        self.report = {"suspected": 1, "at": 1000, "action": "none", "reason": "unrewarded"}
        self.save()

    def save(self):
        recover.REPORT.write_text(json.dumps(self.report))

    def test_two_probes_and_cooldown(self):
        with patch.object(recover, "verdict", return_value=True), patch.object(recover.subprocess, "run") as restart:
            self.assertIn("second probe", recover.run(1001))
            self.assertIn("next probe", recover.run(1002))
            self.report["at"] = 1300
            self.save()
            self.assertIn("restarted", recover.run(1301))
            restart.assert_called_once_with(["sudo", "-n", "systemctl", "restart", "flysim.service"], check=True)
            self.assertIn("cooldown", recover.run(1302))

    def test_stale_and_clear_never_restart(self):
        with patch.object(recover.subprocess, "run") as restart:
            self.assertIn("not a fresh", recover.run(1700))
            self.report.update(at=1700, suspected=0)
            self.save()
            self.assertIn("not a fresh", recover.run(1700))
            restart.assert_not_called()

    def test_router_failure_does_not_act(self):
        recover.run(1000)
        self.report["at"] = 1300
        self.save()
        with patch.object(recover, "verdict", side_effect=ValueError("bad response")), patch.object(recover.subprocess, "run") as restart:
            self.assertEqual(recover.run(1300), "router unavailable")
            restart.assert_not_called()

    def test_cleared_probe_preserves_cooldown(self):
        with patch.object(recover, "verdict", return_value=True), patch.object(recover.subprocess, "run") as restart:
            recover.run(1000)
            self.report["at"] = 1300
            self.save()
            recover.run(1300)
            self.report.update(at=1600, suspected=0)
            self.save()
            recover.run(1600)
            self.report.update(at=1900, suspected=1)
            self.save()
            recover.run(1900)
            self.report["at"] = 2200
            self.save()
            self.assertIn("cooldown", recover.run(2200))
            restart.assert_called_once()

    def test_router_rejection_does_not_restart(self):
        recover.run(1000)
        self.report["at"] = 1300
        self.save()
        with patch.object(recover, "verdict", return_value=False), patch.object(recover.subprocess, "run") as restart:
            self.assertIn("did not confirm", recover.run(1300))
            restart.assert_not_called()

    def test_partial_router_configuration_cannot_bypass_veto(self):
        with patch.dict("os.environ", {"FLY_LOOP_ROUTER_URL": "http://router/v1"}, clear=True):
            self.assertFalse(recover.verdict(self.report))
        with patch.dict("os.environ", {"FLY_LOOP_MODEL": "free"}, clear=True):
            self.assertFalse(recover.verdict(self.report))

    def test_unconfigured_router_uses_deterministic_confirmation(self):
        with patch.dict("os.environ", {}, clear=True):
            self.assertTrue(recover.verdict(self.report))


if __name__ == "__main__":
    unittest.main()
