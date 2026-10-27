#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "stop_vrf", Path(__file__).with_name("stop-vrf-service.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class StopTests(unittest.TestCase):
    def runner(self, loaded="loaded"):
        calls = []

        def run(command, **_):
            calls.append(command)
            return SimpleNamespace(stdout=loaded + "\n", returncode=0)

        return calls, run

    def test_absent_unit_is_noop(self):
        calls, run = self.runner("not-found")
        module.stop(run, lambda: self.fail("no journal exists"))
        self.assertEqual(len(calls), 1)

    def test_clean_stop_disables_boot_start(self):
        calls, run = self.runner()
        module.stop(run, lambda: None)
        self.assertEqual(
            [c[1] for c in calls], ["show", "is-active", "stop", "disable"]
        )

    def test_unknown_outcomes_restore_worker_and_block_cd(self):
        calls, run = self.runner()

        def blocked():
            raise RuntimeError("unresolved")

        with self.assertRaisesRegex(RuntimeError, "unresolved"):
            module.stop(run, blocked)
        self.assertEqual(calls[-1][1], "start")
        self.assertFalse(any(c[1] == "disable" for c in calls))
        self.assertFalse(any("northstar-validator" in " ".join(c) for c in calls))

    def test_snapshot_binding_and_status_checks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "journals"
            binding = "a" * 64
            directory = state / binding
            directory.mkdir(parents=True)
            config = root / "config.json"
            config.write_text(json.dumps({"state_dir": str(directory)}))
            snapshot = directory / "public-state.json"
            for status in ("pending", "retryable", "unresolved", "unknown"):
                snapshot.write_text(
                    json.dumps(
                        {"binding": binding, "records": {"one": {"status": status}}}
                    )
                )
                with self.assertRaises(RuntimeError):
                    module.inspect_journal(config, state)
            snapshot.write_text(
                json.dumps(
                    {"binding": binding, "records": {"one": {"status": "confirmed"}}}
                )
            )
            module.inspect_journal(config, state)
            snapshot.unlink()
            with self.assertRaises(FileNotFoundError):
                module.inspect_journal(config, state)

    def test_deploy_order_and_trust_boundary(self):
        source = Path(__file__).parents[1].joinpath("workflows/ci.yml").read_text()
        stop = source.index("Stop managed VRF before deployment")
        self.assertLess(stop, source.index("Generate new portal program address"))
        self.assertLess(
            source.index(
                "Run SPL deposit, resume, withdrawal, and settlement smoke test"
            ),
            source.index("Reconcile CI-owned VRF stack and enable service"),
        )
        deploy = source[source.index("  deploy:") :]
        self.assertIn("runs-on: [devnet]", deploy)
        self.assertIn("github.ref == 'refs/heads/release'", deploy)
        self.assertIn("persist-credentials: false", deploy)
        self.assertIn("NORTHSTAR_VRF_ENABLED == 'true'", deploy)


if __name__ == "__main__":
    unittest.main()
